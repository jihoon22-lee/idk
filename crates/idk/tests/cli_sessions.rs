use idk_workspace::client::Client;
use idk_workspace::store::Store;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

struct OwnedHost(Child);

impl Drop for OwnedHost {
    fn drop(&mut self) {
        // Only this fixture's spawned child handle is eligible for cleanup.
        // Never trust a saved host PID as authority to signal another process.
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

fn invoke(data: &Path, home: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_idk"))
        .arg("--data-dir")
        .arg(data)
        .args(arguments)
        .env("HOME", home)
        .env("CLI_ENV_MARK", "original")
        .env_remove("IDK_HOME")
        .output()
        .unwrap()
}

fn success(data: &Path, home: &Path, arguments: &[&str]) -> Value {
    let output = invoke(data, home, arguments);
    assert!(
        output.status.success(),
        "{arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn wait_for_screen(client: &mut Client, id: &str, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let snapshot = client.snapshot(id, None).unwrap();
        if snapshot
            .screen
            .as_ref()
            .is_some_and(|screen| screen.text().contains(expected))
        {
            return;
        }
        assert!(Instant::now() < deadline, "missing {expected}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn cli_reuses_the_live_shell_requires_explicit_reopen_and_preserves_unrelated_processes() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let home = temp.path().join("home");
    let source = temp.path().join("source 한글");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&source).unwrap();
    let setup = source.join("setup.csh");
    std::fs::write(
        &setup,
        "set cli_local = retained\necho initialized >> initialization-count\necho CLI_INITIALIZED\n",
    )
    .unwrap();
    let shell = std::env::var_os("IDK_TEST_SHELL")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/usr/bin/tcsh"));
    assert!(
        shell.is_file(),
        "provide an actual csh/tcsh via IDK_TEST_SHELL"
    );
    let project = success(
        &data,
        &home,
        &[
            "project",
            "connect",
            "CLI runtime",
            source.to_str().unwrap(),
            "--shell",
            shell.to_str().unwrap(),
            "--source",
            setup.to_str().unwrap(),
        ],
    );
    let project_id = project["id"].as_str().unwrap();
    let terminal_id = project["terminals"][0]["id"].as_str().unwrap();
    success(&data, &home, &["project", "trust", project_id, "--yes"]);
    let store = Store::open(Some(&data)).unwrap();
    assert!(!invoke(&data, &home, &["host", "status"]).status.success());
    assert!(
        !store.socket_path().exists(),
        "status must not launch a host"
    );
    let mut host = OwnedHost(
        Command::new(env!("CARGO_BIN_EXE_idk"))
            .arg("__host")
            .arg("--config-dir")
            .arg(&store.config_dir)
            .arg("--state-dir")
            .arg(&store.state_dir)
            .arg("--runtime-dir")
            .arg(&store.runtime_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut client = loop {
        if let Ok(client) =
            Client::connect_with_launcher(&store, Path::new(env!("CARGO_BIN_EXE_idk")))
        {
            break client;
        }
        assert!(
            host.0.try_wait().unwrap().is_none(),
            "host failed during startup"
        );
        assert!(Instant::now() < deadline, "host startup deadline");
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut unrelated = OwnedHost(Command::new("sleep").arg("60").spawn().unwrap());
    let first = success(&data, &home, &["session", "start", project_id, terminal_id]);
    let session = first["session_id"].as_str().unwrap();
    wait_for_screen(&mut client, session, "CLI_INITIALIZED");
    let attached = client.attach(session, false).unwrap();
    client
        .input(session, attached.input_epoch, b"set cli_local = changed\n")
        .unwrap();
    client.detach(session, attached.input_epoch).unwrap();
    let second = success(&data, &home, &["session", "start", project_id, terminal_id]);
    assert_eq!(first["session_id"], second["session_id"]);
    let attached = client.attach(session, false).unwrap();
    client
        .input(session, attached.input_epoch, b"echo STATE:$cli_local\n")
        .unwrap();
    wait_for_screen(&mut client, session, "STATE:changed");
    assert_eq!(
        std::fs::read_to_string(source.join("initialization-count"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    let preview = invoke(&data, &home, &["session", "close", session]);
    assert!(!preview.status.success());
    assert!(host.0.try_wait().unwrap().is_none());
    success(
        &data,
        &home,
        &["session", "close", session, "--yes", "--takeover"],
    );
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let info = serde_json::to_value(client.list(None).unwrap()).unwrap();
        let state = info
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["session_id"] == session)
            .unwrap()["state"]
            .as_str()
            .unwrap();
        if matches!(state, "closed" | "exited") {
            break;
        }
        assert!(Instant::now() < deadline, "close remained pending: {info}");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !invoke(&data, &home, &["session", "start", project_id, terminal_id])
            .status
            .success()
    );
    let reopened = success(
        &data,
        &home,
        &["session", "start", project_id, terminal_id, "--reopen"],
    );
    let new_session = reopened["session_id"].as_str().unwrap();
    assert_ne!(session, new_session);
    wait_for_screen(&mut client, new_session, "CLI_INITIALIZED");
    assert_eq!(
        std::fs::read_to_string(source.join("initialization-count"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    success(&data, &home, &["project", "remove", project_id, "--yes"]);
    let orphan = client.attach(new_session, false).unwrap();
    client
        .input(
            new_session,
            orphan.input_epoch,
            b"echo ORPHAN_\"SURVIVES\"\n",
        )
        .unwrap();
    wait_for_screen(&mut client, new_session, "ORPHAN_SURVIVES");
    assert_eq!(client.definition(new_session).unwrap().id, terminal_id);
    assert!(success(&data, &home, &["session", "list"])
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["session_id"] == new_session));
    success(&data, &home, &["host", "stop", "--yes"]);
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if let Some(status) = host.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline, "host shutdown remained pending");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(unrelated.0.try_wait().unwrap().is_none());
    assert!(source.is_dir() && setup.is_file());
    assert!(!source.join(".git").exists());
}
