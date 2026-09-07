use flate2::{Compression, GzBuilder};
use idk_workspace::package::{
    sha256, VerifiedBundle, BINARY, CHECKSUMS, INVENTORY, MANIFEST, NOTICES,
};
use serde_json::json;
use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;

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
        "schema_version": 1, "artifact": BINARY, "version": "0.4.0", "target": "x86_64-unknown-linux-musl", "toolchain": "1.97.1",
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

#[test]
fn reviewed_bytes_remain_sealed_when_source_archive_is_replaced_before_staging() {
    let root = tempfile::tempdir().unwrap();
    let bytes = archive(&files(static_elf()), None);
    let path = root.path().join("bundle.tar.gz");
    std::fs::write(&path, &bytes).unwrap();
    let bundle = VerifiedBundle::read(&path, &sha256(&bytes)).unwrap();
    std::fs::write(&path, b"changed after review").unwrap();
    let stage = root.path().join("stage");
    bundle.stage(&stage).unwrap();
    assert_eq!(std::fs::read(stage.join(BINARY)).unwrap(), static_elf());
    assert_eq!(
        std::fs::metadata(stage.join(BINARY))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    assert_eq!(bundle.review().manifest.version, "0.4.0");
    assert_eq!(bundle.review().bundle_sha256, sha256(&bytes));
}

#[test]
fn staging_never_overwrites_existing_user_directories_or_files() {
    let root = tempfile::tempdir().unwrap();
    let bundle = verify(&archive(&files(static_elf()), None)).unwrap();
    let source = root.path().join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("user.csh"), b"preserved").unwrap();
    assert!(bundle.stage(&source).is_err());
    assert_eq!(
        std::fs::read(source.join("user.csh")).unwrap(),
        b"preserved"
    );
    assert!(!source.join(BINARY).exists());
    let target = root.path().join("alias");
    std::os::unix::fs::symlink(&source, &target).unwrap();
    assert!(bundle.stage(&target).is_err());
    assert_eq!(
        std::fs::read(source.join("user.csh")).unwrap(),
        b"preserved"
    );
}

#[test]
fn archive_paths_links_duplicates_and_hidden_members_are_rejected_even_with_a_matching_digest() {
    let base = files(static_elf());
    for name in [
        "../outside",
        "/tmp/outside",
        "nested/file",
        "postinstall.sh",
        BINARY,
    ] {
        let bytes = archive(
            &base,
            Some((raw_header(name, 1, tar::EntryType::Regular), b"x".to_vec())),
        );
        assert!(
            verify(&bytes).is_err(),
            "unexpected entry {name:?} accepted"
        );
    }
    let mut without_binary = base.clone();
    without_binary.remove(BINARY);
    for kind in [
        tar::EntryType::Symlink,
        tar::EntryType::Link,
        tar::EntryType::Fifo,
        tar::EntryType::Directory,
        tar::EntryType::XHeader,
    ] {
        let mut header = raw_header(BINARY, 0, kind);
        if matches!(kind, tar::EntryType::Symlink | tar::EntryType::Link) {
            header.set_link_name("../outside").unwrap();
        }
        header.set_cksum();
        assert!(verify(&archive(&without_binary, Some((header, Vec::new())))).is_err());
    }
    let mut bytes = archive(&base, None);
    bytes.extend_from_slice(&archive(&base, None));
    assert!(verify(&bytes).is_err(), "second gzip member accepted");
}

#[test]
fn source_integrity_manifest_and_all_member_hashes_must_agree() {
    let mut entries = files(static_elf());
    let bytes = archive(&entries, None);
    assert!(VerifiedBundle::from_bytes(&bytes, &"0".repeat(64)).is_err());
    entries.get_mut(NOTICES).unwrap().push(b'!');
    assert!(
        verify(&archive(&entries, None)).is_err(),
        "changed notice passed its old checksum"
    );
    checksums(&mut entries);
    let mut manifest: serde_json::Value = serde_json::from_slice(&entries[MANIFEST]).unwrap();
    manifest["size"] = json!(999);
    entries.insert(MANIFEST.into(), serde_json::to_vec(&manifest).unwrap());
    checksums(&mut entries);
    assert!(
        verify(&archive(&entries, None)).is_err(),
        "manifest/binary mismatch accepted"
    );
    manifest["size"] = json!(static_elf().len());
    manifest["schema_version"] = json!(2);
    entries.insert(MANIFEST.into(), serde_json::to_vec(&manifest).unwrap());
    checksums(&mut entries);
    assert!(
        verify(&archive(&entries, None)).is_err(),
        "future schema accepted"
    );
}

#[test]
fn a_manifest_cannot_disguise_dynamic_or_wrong_architecture_executables() {
    let mut dynamic = static_elf();
    dynamic[64..68].copy_from_slice(&3u32.to_le_bytes());
    assert!(verify(&archive(&files(dynamic), None)).is_err());
    let mut other_arch = static_elf();
    other_arch[18..20].copy_from_slice(&183u16.to_le_bytes());
    assert!(verify(&archive(&files(other_arch), None)).is_err());
    let mut entries = files(static_elf());
    let mut manifest: serde_json::Value = serde_json::from_slice(&entries[MANIFEST]).unwrap();
    manifest["distribution_notices_complete"] = json!(false);
    entries.insert(MANIFEST.into(), serde_json::to_vec(&manifest).unwrap());
    checksums(&mut entries);
    assert!(verify(&archive(&entries, None)).is_err());
}

#[test]
fn oversized_headers_and_truncated_compression_fail_before_staging() {
    let mut entries = files(static_elf());
    entries.remove(BINARY);
    // Build the oversized declaration directly; it must be refused before the
    // parser allocates or tries to read the advertised 64 MiB payload.
    let mut gzip = GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), Compression::fast());
    gzip.write_all(raw_header(BINARY, 64 * 1024 * 1024 + 1, tar::EntryType::Regular).as_bytes())
        .unwrap();
    let bytes = gzip.finish().unwrap();
    let error = verify(&bytes).err().unwrap().to_string();
    assert!(error.contains("size limit"), "{error}");
    let mut bytes = archive(&files(static_elf()), None);
    bytes.truncate(bytes.len() - 4);
    assert!(verify(&bytes).is_err());
}

#[test]
fn fifo_bundle_input_is_rejected_without_waiting_for_a_writer() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("bundle");
    let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let start = std::time::Instant::now();
    assert!(VerifiedBundle::read(&path, &"0".repeat(64)).is_err());
    assert!(start.elapsed() < std::time::Duration::from_millis(500));
}

#[test]
fn internally_rehashed_notice_contradictions_are_rejected() {
    for mutation in 0..7 {
        let mut entries = files(static_elf());
        let mut value: serde_json::Value = serde_json::from_slice(&entries[INVENTORY]).unwrap();
        match mutation {
            0 => value["notices_complete"] = json!(false),
            1 => value["toolchain"] = json!("1.96.0"),
            2 => value["notices_sha256"] = json!("0".repeat(64)),
            3 => {
                value["runtime_components"].as_array_mut().unwrap().pop();
            }
            4 => value["packages"][0]["documents"][0]["notice_size"] = json!(u64::MAX),
            5 => {
                value["runtime_components"][0]["documents"][0] =
                    value["packages"][0]["documents"][0].clone()
            }
            _ => value["runtime_notice_policy_sha256"] = json!("0".repeat(64)),
        }
        entries.insert(INVENTORY.into(), serde_json::to_vec(&value).unwrap());
        checksums(&mut entries);
        assert!(
            verify(&archive(&entries, None)).is_err(),
            "accepted inconsistent notice metadata {mutation}"
        );
    }
}
