//! Explicit candidate tests. Run after the archive is built, never against a
//! substituted debug binary or a fixture with only an ELF-shaped header.
use idk_workspace::client::Client;
use idk_workspace::install::Installer;
use idk_workspace::model::ShellConfig;
use idk_workspace::package::{sha256, VerifiedBundle, BINARY, CHECKSUMS, MANIFEST};
use idk_workspace::project::{ConnectDraft, LaunchEnvironment, ProjectService, TerminalDraft};
use idk_workspace::store::{atomic_write, Store};
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct OwnedHost(Child);
impl Drop for OwnedHost {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}
fn supplied_bundle() -> VerifiedBundle {
    let path = PathBuf::from(
        std::env::var_os("IDK_TEST_BUNDLE")
            .expect("set IDK_TEST_BUNDLE to the actual candidate archive"),
    );
    let digest = std::env::var("IDK_TEST_BUNDLE_SHA256")
        .expect("set IDK_TEST_BUNDLE_SHA256 to the verified archive digest");
    VerifiedBundle::read(&path, &digest).unwrap()
}
fn screen_contains(client: &mut Client, session: &str, text: &str) {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if client
            .snapshot(session, None)
            .unwrap()
            .screen
            .is_some_and(|screen| screen.text().contains(text))
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "candidate screen did not contain {text}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

// Only the appended non-loadable identity marker differs. This exercises a
// real executable with a different build SHA, not a second release candidate.
fn changed_identity_bundle(directory: &std::path::Path) -> VerifiedBundle {
    let mut files = BTreeMap::new();
    for name in [
        BINARY,
        MANIFEST,
        CHECKSUMS,
        "idk-third-party-licenses.json",
        "idk-THIRD-PARTY-NOTICES.txt",
    ] {
        files.insert(name, fs::read(directory.join(name)).unwrap());
    }
    files
        .get_mut(BINARY)
        .unwrap()
        .extend_from_slice(b"\nIDK_PACKAGE_TEST_ONLY_IDENTITY_VARIANT\n");
    let mut manifest: serde_json::Value = serde_json::from_slice(&files[MANIFEST]).unwrap();
    manifest["size"] = serde_json::json!(files[BINARY].len());
    manifest["sha256"] = serde_json::json!(sha256(&files[BINARY]));
    manifest["source"]["dirty"] = serde_json::json!(true);
    manifest["main_sha_verified"] = serde_json::json!(false);
    manifest["candidate_kind"] = serde_json::json!("dirty-development");
    manifest["fixture"] =
        serde_json::json!("test-only appended binary identity; never a release candidate");
    files.insert(MANIFEST, serde_json::to_vec(&manifest).unwrap());
    let checksums = files
        .iter()
        .filter(|(name, _)| **name != CHECKSUMS)
        .map(|(name, bytes)| format!("{}  {name}\n", sha256(bytes)))
        .collect::<String>();
    files.insert(CHECKSUMS, checksums.into_bytes());
    let gzip = flate2::GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), flate2::Compression::fast());
    let mut tar = tar::Builder::new(gzip);
    for (name, bytes) in files {
        let mut header = tar::Header::new_ustar();
        header.set_path(name).unwrap();
        header.set_size(bytes.len() as u64);
        header.set_mode(if name == BINARY { 0o755 } else { 0o644 });
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_cksum();
        tar.append(&header, bytes.as_slice()).unwrap();
    }
    let bytes = tar.into_inner().unwrap().finish().unwrap();
    VerifiedBundle::from_bytes(&bytes, &sha256(&bytes)).unwrap()
}

#[test]
#[ignore = "requires the actual built bundle and an installed csh/tcsh"]
fn actual_packaged_install_update_and_uninstall_preserve_a_live_original_shell() {
    let bundle = supplied_bundle();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let store = Store::open(Some(&root.join("data"))).unwrap();
    let installer = Installer::open(&root.join("installation")).unwrap();
    let initial = installer.install(&bundle, &store).unwrap();
    let original_binary = root
        .join("installation/generations")
        .join(&initial.active.name)
        .join(BINARY);
    let original_directory = original_binary.parent().unwrap();
    let shell = std::env::var_os("IDK_TEST_SHELL")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/usr/bin/tcsh".into());
    assert!(shell.is_file(), "a real csh/tcsh is required");
    let source = root.join("source 한글");
    fs::create_dir(&source).unwrap();
    let home = root.join("home");
    fs::create_dir(&home).unwrap();
    let setup = source.join("setup.csh");
    fs::write(
        &setup,
        "set package_local = retained\necho initialized >> source-count\necho PACKAGE_READY\n",
    )
    .unwrap();
    let env = BTreeMap::from([
        ("HOME".into(), home.to_str().unwrap().into()),
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("TERM".into(), "xterm-256color".into()),
        ("LANG".into(), "C.UTF-8".into()),
    ]);
    let service = ProjectService { store: &store };
    let preview = service
        .preview_connect(ConnectDraft {
            name: "Package fixture".into(),
            root: source.clone(),
            shell: ShellConfig {
                executable: shell,
                login: false,
                init_cwd: source.clone(),
                sources: vec![setup.into()],
                trusted_digest: None,
            },
            terminals: vec![TerminalDraft {
                name: "Build shell".into(),
                cwd: source.clone(),
                sources: Vec::new(),
                persistent: true,
            }],
        })
        .unwrap();
    let project = service.create(preview.revision, preview).unwrap();
    let environment = LaunchEnvironment::from_variables(env.clone()).unwrap();
    let review = service
        .review_initialization(&project.id, &environment)
        .unwrap();
    service
        .approve_initialization(review.revision, review)
        .unwrap();
    let project = store.load().unwrap().project(&project.id).unwrap().clone();
    let mut host = OwnedHost(
        Command::new(&original_binary)
            .arg("__host")
            .arg("--config-dir")
            .arg(&store.config_dir)
            .arg("--state-dir")
            .arg(&store.state_dir)
            .arg("--runtime-dir")
            .arg(&store.runtime_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut client = loop {
        if let Ok(client) = Client::connect_with_launcher(&store, &original_binary) {
            break client;
        }
        assert!(host.0.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    };
    let session = client
        .start(&project.id, &project.terminals[0].id, env, 24, 80, false)
        .unwrap();
    screen_contains(&mut client, &session.session_id, "PACKAGE_READY");
    let attached = client.attach(&session.session_id, false).unwrap();
    let pid = attached.child_pid;
    client
        .input(
            &session.session_id,
            attached.input_epoch,
            b"set package_local = changed\n",
        )
        .unwrap();
    client
        .detach(&session.session_id, attached.input_epoch)
        .unwrap();
    let second = changed_identity_bundle(original_directory);
    let updated = installer.install(&second, &store).unwrap();
    let new_binary = root.join("installation/idk");
    assert!(
        Client::connect_with_launcher(&store, &new_binary).is_err(),
        "new build must not silently replace the old host"
    );
    assert!(host.0.try_wait().unwrap().is_none());
    let mut reattached = Client::connect_with_launcher(&store, &original_binary).unwrap();
    let live = reattached.attach(&session.session_id, false).unwrap();
    assert_eq!(live.child_pid, pid);
    reattached
        .input(
            &session.session_id,
            live.input_epoch,
            b"echo PACKAGE_VALUE_$package_local\n",
        )
        .unwrap();
    screen_contains(
        &mut reattached,
        &session.session_id,
        "PACKAGE_VALUE_changed",
    );
    assert_eq!(
        fs::read_to_string(source.join("source-count"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    let later = b"new run metadata created after update";
    atomic_write(&store.state_dir.join("later-result.txt"), later).unwrap();
    installer.uninstall_entrypoints().unwrap();
    assert!(original_binary.is_file());
    assert!(host.0.try_wait().unwrap().is_none());
    assert_eq!(
        fs::read(store.state_dir.join("later-result.txt")).unwrap(),
        later
    );
    assert!(root
        .join("installation/generations")
        .join(updated.active.name)
        .is_dir());
    reattached
        .input(
            &session.session_id,
            live.input_epoch,
            b"echo AFTER_UNINSTALL\n",
        )
        .unwrap();
    screen_contains(&mut reattached, &session.session_id, "AFTER_UNINSTALL");
    reattached
        .detach(&session.session_id, live.input_epoch)
        .unwrap();
    let preview = reattached.preview_close(None).unwrap();
    let targets = preview
        .targets
        .iter()
        .map(|session| session.session_id.clone())
        .collect::<Vec<_>>();
    reattached.shutdown(&targets, true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    while host.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut original = fs::File::open(original_binary).unwrap();
    let mut bytes = Vec::new();
    original.read_to_end(&mut bytes).unwrap();
    assert_eq!(sha256(&bytes), bundle.review().manifest.sha256);
}
