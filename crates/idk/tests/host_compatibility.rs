#[path = "common/launcher.rs"]
mod launcher;

use idk_workspace::client::{compatible_host_client, Client};
use idk_workspace::package::{VerifiedBundle, BINARY};
use idk_workspace::protocol::SnapshotReply;
use idk_workspace::store::Store;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
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
fn invoke(binary: &Path, data: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(binary)
        .arg("--data-dir")
        .arg(data)
        .args(args)
        .env("HOME", home)
        .env_remove("IDK_HOME")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}
fn json(binary: &Path, data: &Path, home: &Path, args: &[&str]) -> Value {
    let output = invoke(binary, data, home, args);
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
#[ignore = "requires freshly verified v0.5.0 public binary/archive in IDK_TEST_COMPAT_* variables"]
fn actual_v05_host_guidance_preserves_schema_sessions_and_verifies_the_original_generation() {
    let binary =
        PathBuf::from(std::env::var_os("IDK_TEST_COMPAT_BINARY").expect("verified v0.5 binary"));
    let archive =
        PathBuf::from(std::env::var_os("IDK_TEST_COMPAT_BUNDLE").expect("verified v0.5 archive"));
    let digest =
        std::env::var("IDK_TEST_COMPAT_SHA256").expect("independently recorded archive digest");
    let bundle = VerifiedBundle::read(&archive, &digest).unwrap();
    assert_eq!(bundle.review().manifest.version, "0.5.0");
    assert_eq!(
        idk_workspace::protocol::executable_build_id(&binary).unwrap(),
        bundle.review().manifest.sha256
    );
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let data = root.join("data with 'quote");
    let home = root.join("home");
    let source = root.join("project");
    let prefix = root.join("installation with spaces");
    for path in [&home, &source] {
        fs::create_dir(path).unwrap();
    }
    fs::write(home.join(".bashrc"),"printf 'startup\\n' >> \"$HOME/startup-count\"\nPS1='OLD_READY> '\nexport KEPT_STATE=original\n").unwrap();
    json(
        &binary,
        &data,
        &home,
        &[
            "package",
            "install",
            archive.to_str().unwrap(),
            "--sha256",
            &digest,
            "--prefix",
            prefix.to_str().unwrap(),
            "--yes",
        ],
    );
    let original = prefix
        .join("generations")
        .join(&bundle.review().generation)
        .join(BINARY);
    let project = json(
        &original,
        &data,
        &home,
        &[
            "project",
            "connect",
            "compatibility",
            source.to_str().unwrap(),
            "--shell",
            "/bin/bash",
            "--shell-arg",
            "-i",
        ],
    );
    let project_id = project["id"].as_str().unwrap();
    let terminal_id = project["terminals"][0]["id"].as_str().unwrap();
    let store = Store::open(Some(&data)).unwrap();
    let mut host = OwnedHost(
        Command::new(&original)
            .args(["__host", "--config-dir"])
            .arg(&store.config_dir)
            .arg("--state-dir")
            .arg(&store.state_dir)
            .arg("--runtime-dir")
            .arg(&store.runtime_dir)
            .env("HOME", &home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if invoke(&original, &data, &home, &["host", "status"])
            .status
            .success()
        {
            break;
        }
        assert!(host.0.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline, "old host startup timeout");
        std::thread::sleep(Duration::from_millis(20));
    }
    let started = json(
        &original,
        &data,
        &home,
        &["session", "start", project_id, terminal_id],
    );
    let session = started["session_id"].as_str().unwrap();
    let shell_pid = loop {
        let snapshot: SnapshotReply = serde_json::from_value(json(
            &original,
            &data,
            &home,
            &["session", "snapshot", session],
        ))
        .unwrap();
        if snapshot
            .screen
            .is_some_and(|screen| screen.text().contains("OLD_READY>"))
        {
            break snapshot.session.child_pid.unwrap();
        }
        assert!(Instant::now() < deadline, "old shell startup timeout");
        std::thread::sleep(Duration::from_millis(20));
    };
    let config_before = fs::read(store.config_path()).unwrap();
    let inventory_before = fs::read(prefix.join("installation.json")).unwrap();
    let guidance = compatible_host_client(&store, None)
        .unwrap()
        .expect("exact verified old generation");
    assert_eq!(guidance.host_pid, host.0.id());
    assert_eq!(guidance.client, original);
    assert_eq!(guidance.binary_sha256, bundle.review().manifest.sha256);
    assert!(guidance.inspect_command.contains("--data-dir"));
    let listed = Command::new("/bin/sh")
        .args(["-c", &guidance.inspect_command])
        .env("HOME", &home)
        .output()
        .unwrap();
    assert!(
        listed.status.success(),
        "{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let sessions: Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(sessions[0]["child_pid"], shell_pid);
    assert_eq!(sessions[0]["session_id"], session);
    let error = Client::connect_with_launcher(&store, launcher::path())
        .err()
        .expect("new protocol must reject old host");
    let message = format!("{error:#}");
    assert!(
        message.contains("verified matching client")
            && message.contains("session list")
            && message.contains("host protocol changed"),
        "{message}"
    );
    // Path entry is a TUI operation: use a real PTY so the compatibility preflight is exercised.
    let mut command = portable_pty::CommandBuilder::new(launcher::path());
    command.arg("--data-dir");
    command.arg(&data);
    command.arg(&source);
    command.env_clear();
    command.env("HOME", &home);
    command.env("PATH", "/usr/bin:/bin");
    command.env("TERM", "xterm");
    let mut rejected =
        idk_workspace::terminal::TerminalSession::spawn(command, 40, 200, 200).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(exit) = rejected.try_wait().unwrap() {
            assert_ne!(exit.code, 0);
            if rejected.status().unwrap().reader_closed {
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "path-entry preflight did not exit"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let screen = rejected.snapshot().unwrap().text();
    assert!(
        screen.replace('\n', "").contains("session list"),
        "{screen}"
    );
    assert_eq!(
        fs::read(store.config_path()).unwrap(),
        config_before,
        "path entry must reject before schema 3 migration"
    );
    assert_eq!(
        fs::read(prefix.join("installation.json")).unwrap(),
        inventory_before
    );
    assert_eq!(
        fs::read_to_string(home.join("startup-count")).unwrap(),
        "startup\n"
    );
    let snapshot: SnapshotReply = serde_json::from_value(json(
        &original,
        &data,
        &home,
        &["session", "snapshot", session],
    ))
    .unwrap();
    assert_eq!(snapshot.session.child_pid, Some(shell_pid));
    assert!(host.0.try_wait().unwrap().is_none());
    // A stale install inventory is not enough to recommend an executable: all generation bytes are checked.
    let notices = original
        .parent()
        .unwrap()
        .join("idk-THIRD-PARTY-NOTICES.txt");
    let original_notices = fs::read(&notices).unwrap();
    fs::write(&notices, b"changed").unwrap();
    assert!(compatible_host_client(&store, Some(&prefix)).is_err());
    fs::write(&notices, original_notices).unwrap();
    json(
        &original,
        &data,
        &home,
        &["host", "stop", "--yes", "--force"],
    );
    let deadline = Instant::now() + Duration::from_secs(8);
    while host.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!Path::new(&format!("/proc/{shell_pid}")).exists());
}
