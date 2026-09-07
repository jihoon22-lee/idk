use super::*;
use crate::package::{sha256, VerifiedBundle, BINARY, CHECKSUMS, INVENTORY, MANIFEST, NOTICES};
use flate2::{Compression, GzBuilder};
use serde_json::json;
use std::collections::BTreeMap;

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

fn next_version() -> String {
    let (major, minor, patch) = version_tuple(CURRENT_VERSION).unwrap();
    format!("{major}.{minor}.{}", patch.checked_add(1).unwrap())
}

// Static ELF structure fixture only; runnable candidate health is a separate
// installer/packaged test. These bytes are never executed by the verifier.
fn static_elf() -> Vec<u8> {
    let mut bytes = vec![0; 120];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    bytes[16..18].copy_from_slice(&2u16.to_le_bytes());
    bytes[18..20].copy_from_slice(&62u16.to_le_bytes());
    bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
    bytes[32..40].copy_from_slice(&64u64.to_le_bytes());
    bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
    bytes[54..56].copy_from_slice(&56u16.to_le_bytes());
    bytes[56..58].copy_from_slice(&1u16.to_le_bytes());
    bytes
}

fn files(binary: Vec<u8>) -> BTreeMap<String, Vec<u8>> {
    let mut notices = Vec::new();
    let mut components = Vec::new();
    for id in [
        "fixture",
        "rust-standard-library",
        "compiler-builtins",
        "musl",
        "llvm-libunwind",
        "compiler-rt-crt",
    ] {
        let text =
            format!("Synthetic {id} notice; not distribution license evidence.\n").into_bytes();
        let offset = notices.len();
        notices.extend_from_slice(&text);
        components.push(json!({"id":id,"name":id,"documents":[{"source_size":text.len(),"source_sha256":sha256(&text),
            "notice_offset":offset,"notice_size":text.len(),"notice_sha256":sha256(&text)}]}));
    }
    let inventory = json!({"schema_version":1,"notices_complete":true,"target":"x86_64-unknown-linux-musl","toolchain":"1.97.1",
        "cargo_lock_sha256":"3".repeat(64),"runtime_notice_policy_sha256":"4".repeat(64),"notices_sha256":sha256(&notices),
        "packages":[components.remove(0)],"runtime_components":components});
    let manifest = json!({
        "schema_version": 1, "artifact": BINARY, "version": CURRENT_VERSION, "target": "x86_64-unknown-linux-musl", "toolchain": "1.97.1",
        "source": {"sha":"1".repeat(40), "dirty":false}, "candidate_kind":"clean-source", "main_sha_verified":false,
        "input_tree_sha256":"2".repeat(64), "cargo_lock_sha256":"3".repeat(64), "size":binary.len(), "sha256":sha256(&binary),
        "elf":{"interpreter":null,"needed":[]}, "license_inventory":INVENTORY, "notices":NOTICES, "distribution_notices_complete":true,
        "runtime_notice_policy_sha256":"4".repeat(64)
    });
    let mut files = BTreeMap::from([
        (BINARY.into(), binary),
        (MANIFEST.into(), serde_json::to_vec(&manifest).unwrap()),
        (INVENTORY.into(), serde_json::to_vec(&inventory).unwrap()),
        (NOTICES.into(), notices),
    ]);
    checksums(&mut files);
    files
}

fn checksums(files: &mut BTreeMap<String, Vec<u8>>) {
    let checksums = files
        .iter()
        .filter(|(name, _)| name.as_str() != CHECKSUMS)
        .map(|(name, bytes)| format!("{}  {name}\n", sha256(bytes)))
        .collect::<String>();
    files.insert(CHECKSUMS.into(), checksums.into_bytes());
}

fn raw_header(name: &str, length: u64, entry_type: tar::EntryType) -> tar::Header {
    let mut header = tar::Header::new_ustar();
    let raw = header.as_mut_bytes();
    assert!(name.len() < 100);
    raw[..name.len()].copy_from_slice(name.as_bytes());
    header.set_size(length);
    header.set_mode(if name == BINARY { 0o755 } else { 0o644 });
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_entry_type(entry_type);
    header.set_cksum();
    header
}

fn archive(files: &BTreeMap<String, Vec<u8>>, extra: Option<(tar::Header, Vec<u8>)>) -> Vec<u8> {
    let gzip = GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), Compression::fast());
    let mut archive = tar::Builder::new(gzip);
    for (name, bytes) in files {
        archive
            .append(
                &raw_header(name, bytes.len() as u64, tar::EntryType::Regular),
                bytes.as_slice(),
            )
            .unwrap();
    }
    if let Some((header, bytes)) = extra {
        archive.append(&header, bytes.as_slice()).unwrap();
    }
    archive.into_inner().unwrap().finish().unwrap()
}

fn verify(bytes: &[u8]) -> anyhow::Result<VerifiedBundle> {
    VerifiedBundle::from_bytes(bytes, &sha256(bytes))
}

fn bundle(version: &str, nonce: u8) -> VerifiedBundle {
    let mut binary = static_elf();
    binary[119] = nonce;
    let mut entries = files(binary);
    let mut manifest: serde_json::Value = serde_json::from_slice(&entries[MANIFEST]).unwrap();
    manifest["version"] = json!(version);
    entries.insert(MANIFEST.into(), serde_json::to_vec(&manifest).unwrap());
    checksums(&mut entries);
    verify(&archive(&entries, None)).unwrap()
}

#[test]
fn interruption_at_every_activation_boundary_recovers_without_restoring_live_data() {
    for stop in [
        Checkpoint::Staged,
        Checkpoint::Journaled,
        Checkpoint::Activated,
        Checkpoint::Healthy,
        Checkpoint::Committed,
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let installer = Installer::open(&temporary.path().join("install")).unwrap();
        let store = Store::open(Some(&temporary.path().join("data"))).unwrap();
        let original = bundle(CURRENT_VERSION, 0);
        let replacement = bundle(&next_version(), 1);
        installer
            .install_with(&original, |_| Ok(()), |_| Ok(()))
            .unwrap();
        let outcome = installer.install_with(
            &replacement,
            |_| Ok(()),
            |point| {
                if point == stop {
                    bail!("simulated process interruption");
                }
                Ok(())
            },
        );
        assert!(outcome.is_err());
        let review = installer.review_recovery().unwrap();
        assert_eq!(
            review.finish_committed_activation,
            stop == Checkpoint::Committed
        );
        // The live host or another client may write data after the interruption.
        let later = b"later live run result; must survive recovery";
        atomic_write(&store.state_dir.join("live-result.txt"), later).unwrap();
        let recovered = installer.recover().unwrap();
        let expected = if stop == Checkpoint::Committed {
            &replacement
        } else {
            &original
        };
        assert_eq!(
            recovered.active.as_deref(),
            Some(expected.review().generation.as_str())
        );
        assert_eq!(
            recovered.committed_version_floor.as_deref(),
            Some(expected.review().manifest.version.as_str())
        );
        assert_eq!(
            fs::read(store.state_dir.join("live-result.txt")).unwrap(),
            later
        );
        assert!(installer
            .generation_path(&original.review().generation)
            .is_dir());
        assert!(installer
            .generation_path(&replacement.review().generation)
            .is_dir());
        assert_eq!(installer.status().unwrap(), recovered);
        assert_eq!(
            installer.recover().unwrap(),
            recovered,
            "recovery must be idempotent"
        );
    }
}

#[test]
fn health_failure_before_or_after_activation_preserves_original_generation() {
    for fail_at in [0, 1] {
        let temporary = tempfile::tempdir().unwrap();
        let installer = Installer::open(&temporary.path().join("install")).unwrap();
        let original = bundle(CURRENT_VERSION, 2);
        let replacement = bundle(&next_version(), 3);
        installer
            .install_with(&original, |_| Ok(()), |_| Ok(()))
            .unwrap();
        let mut checks = 0;
        let result = installer.install_with(
            &replacement,
            |_| {
                let this = checks;
                checks += 1;
                if this == fail_at {
                    bail!("synthetic health rejection");
                }
                Ok(())
            },
            |_| Ok(()),
        );
        assert!(result.is_err());
        let state = installer.status().unwrap();
        assert_eq!(
            state.active.as_deref(),
            Some(original.review().generation.as_str())
        );
        installer.verify(&original.review().generation).unwrap();
        assert!(!installer.root.join(JOURNAL).exists());
        assert_eq!(
            state.committed_version_floor.as_deref(),
            Some(CURRENT_VERSION)
        );
        // Merely staging a higher version must not impose a compatibility floor.
        installer
            .install_with(&original, |_| Ok(()), |_| Ok(()))
            .unwrap();
    }
}

#[test]
fn interrupted_staging_can_only_adopt_the_exact_sealed_original() {
    let temporary = tempfile::tempdir().unwrap();
    let installer = Installer::open(&temporary.path().join("install")).unwrap();
    let candidate = bundle(CURRENT_VERSION, 4);
    let directory = installer.generation_path(&candidate.review().generation);
    candidate.stage(&directory).unwrap();
    atomic_write(&directory.join(ARCHIVE), candidate.archive_bytes()).unwrap();
    // This represents rename to the final directory before the inventory write.
    installer
        .install_with(&candidate, |_| Ok(()), |_| Ok(()))
        .unwrap();
    let state = installer.status().unwrap();
    assert_eq!(
        state.active.as_deref(),
        Some(candidate.review().generation.as_str())
    );
    fs::write(directory.join(BINARY), "corrupted installed component").unwrap();
    assert!(installer
        .install_with(&candidate, |_| Ok(()), |_| Ok(()))
        .is_err());
    assert!(installer.verify(&candidate.review().generation).is_err());
    assert_eq!(
        fs::read(directory.join(BINARY)).unwrap(),
        b"corrupted installed component"
    );
}

#[test]
fn uninstall_removes_only_managed_links_and_recovery_restores_interrupted_links() {
    let temporary = tempfile::tempdir().unwrap();
    let installer = Installer::open(&temporary.path().join("install")).unwrap();
    let candidate = bundle(CURRENT_VERSION, 5);
    installer
        .install_with(&candidate, |_| Ok(()), |_| Ok(()))
        .unwrap();
    let before = installer.status().unwrap();
    let journal = Journal {
        schema: 1,
        before: before.clone(),
        target: None,
    };
    atomic_write(
        &installer.root.join(JOURNAL),
        &serde_json::to_vec(&journal).unwrap(),
    )
    .unwrap();
    fs::remove_file(installer.root.join("idk")).unwrap();
    assert_eq!(installer.recover().unwrap(), before);
    assert!(installer.root.join("idk").is_file());
    let preserved = installer
        .generation_path(&candidate.review().generation)
        .join(BINARY);
    installer.uninstall_entrypoints().unwrap();
    assert!(installer.status().unwrap().active.is_none());
    assert!(!installer.root.join("idk").exists());
    assert!(preserved.is_file());
    installer.verify(&candidate.review().generation).unwrap();
}

#[test]
fn unrelated_entrypoint_future_schema_and_committed_downgrade_are_preserved() {
    let temporary = tempfile::tempdir().unwrap();
    let installer = Installer::open(&temporary.path().join("install")).unwrap();
    fs::write(installer.root.join("idk"), "user launcher").unwrap();
    let old = bundle(CURRENT_VERSION, 6);
    assert!(installer
        .install_with(&old, |_| Ok(()), |_| Ok(()))
        .is_err());
    assert_eq!(
        fs::read(installer.root.join("idk")).unwrap(),
        b"user launcher"
    );
    fs::remove_file(installer.root.join("idk")).unwrap();
    let newer = bundle(&next_version(), 7);
    installer
        .install_with(&newer, |_| Ok(()), |_| Ok(()))
        .unwrap();
    assert!(installer
        .install_with(&old, |_| Ok(()), |_| Ok(()))
        .is_err());
    assert_eq!(
        installer.status().unwrap().active.as_deref(),
        Some(newer.review().generation.as_str())
    );
    let future = br#"{"schema":99,"active":null,"generations":{}}"#;
    atomic_write(&installer.root.join(STATE), future).unwrap();
    assert!(installer.status().is_err());
    assert!(installer.recover().is_err());
    assert_eq!(fs::read(installer.root.join(STATE)).unwrap(), future);
}

#[test]
fn schema_health_rejects_corruption_without_changing_existing_data() {
    let temporary = tempfile::tempdir().unwrap();
    let store = Store::open(Some(temporary.path())).unwrap();
    let future = br#"{"schema":99,"sessions":[]}"#;
    atomic_write(&store.state_dir.join("host-sessions.json"), future).unwrap();
    assert!(health(&store).is_err());
    assert_eq!(
        fs::read(store.state_dir.join("host-sessions.json")).unwrap(),
        future
    );
    fs::remove_file(store.state_dir.join("host-sessions.json")).unwrap();
    let report = health(&store).unwrap();
    assert_eq!(report.configuration_schema, SCHEMA);
    assert!(
        !store.config_path().exists(),
        "health must not synthesize configuration"
    );
}

#[test]
fn uninstall_preserves_the_committed_floor_when_an_older_installer_is_used() {
    let temporary = tempfile::tempdir().unwrap();
    let installer = Installer::open(&temporary.path().join("install")).unwrap();
    let newer = bundle(&next_version(), 8);
    let older = bundle(CURRENT_VERSION, 9);
    installer
        .install_with(&newer, |_| Ok(()), |_| Ok(()))
        .unwrap();
    let uninstalled = installer.uninstall_entrypoints().unwrap();
    assert!(uninstalled.active.is_none());
    assert_eq!(
        uninstalled.committed_version_floor.as_deref(),
        Some(newer.review().manifest.version.as_str())
    );
    let state_bytes = fs::read(installer.root.join(STATE)).unwrap();
    let result = installer.install_with(
        &older,
        |_| panic!("downgrade must be rejected before executing health"),
        |_| Ok(()),
    );
    assert!(result.is_err());
    assert_eq!(fs::read(installer.root.join(STATE)).unwrap(), state_bytes);
    assert_eq!(installer.status().unwrap(), uninstalled);
    installer.verify(&newer.review().generation).unwrap();
    installer
        .install_with(&newer, |_| Ok(()), |_| Ok(()))
        .unwrap();
}

#[test]
fn previous_installation_schema_is_preserved_without_an_inferred_floor_or_migration() {
    let temporary = tempfile::tempdir().unwrap();
    let installer = Installer::open(&temporary.path().join("install")).unwrap();
    let original = br#"{"schema":1,"active":null,"generations":{}}"#;
    atomic_write(&installer.root.join(STATE), original).unwrap();
    let candidate = bundle(CURRENT_VERSION, 10);
    assert!(installer.status().is_err());
    assert!(installer.review_recovery().is_err());
    assert!(installer.recover().is_err());
    assert!(installer
        .install_with(&candidate, |_| Ok(()), |_| Ok(()))
        .is_err());
    assert_eq!(fs::read(installer.root.join(STATE)).unwrap(), original);
}
