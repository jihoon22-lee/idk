use base64::Engine;
use idk_workspace::{client::Client, model::new_id, store::Store};
use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
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
fn invoke(data: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_idk"))
        .arg("--data-dir")
        .arg(data)
        .args(args)
        .env("HOME", home)
        .env("IDK_CLI_EDITOR_ARGS", home.join("editor-args"))
        .env("CLI_PRIVATE_MARKER", "never-in-run-ledger")
        .env_remove("IDK_HOME")
        .output()
        .unwrap()
}
fn success(data: &Path, home: &Path, args: &[&str]) -> Value {
    let output = invoke(data, home, args);
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
#[test]
fn cli_definitions_do_not_execute_and_actual_failed_run_links_log_problem_editor_and_replay_key() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let data = root.join("data");
    let home = root.join("home");
    let source = root.join("source");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&source).unwrap();
    fs::write(source.join("main.cpp"), "bad source kept\n").unwrap();
    let setup = source.join("setup.csh");
    fs::write(
        &setup,
        "set cli_run_local = retained\necho init >> init-count\n",
    )
    .unwrap();
    let shell = std::env::var_os("IDK_TEST_SHELL")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/usr/bin/tcsh".into());
    let project = success(
        &data,
        &home,
        &[
            "project",
            "connect",
            "CLI tasks",
            source.to_str().unwrap(),
            "--shell",
            shell.to_str().unwrap(),
            "--source",
            setup.to_str().unwrap(),
        ],
    );
    let project = project["id"].as_str().unwrap();
    let task = success(
        &data,
        &home,
        &[
            "task",
            "add",
            project,
            "compile",
            "--command",
            "echo TASK_$cli_run_local; echo 'main.cpp:1:1: error: synthetic CLI failure'; exit 7",
        ],
    );
    let task = task["id"].as_str().unwrap();
    assert!(!source.join("init-count").exists());
    let review = success(&data, &home, &["task", "review", project, task]);
    assert_eq!(review["approved"], false);
    assert!(
        !invoke(&data, &home, &["task", "approve", project, task, "--yes"])
            .status
            .success()
    );
    success(&data, &home, &["project", "trust", project, "--yes"]);
    success(&data, &home, &["task", "approve", project, task, "--yes"]);
    assert!(
        !source.join("init-count").exists(),
        "approval executed initialization"
    );
    let store = Store::open(Some(&data)).unwrap();
    let mut host = OwnedHost(
        Command::new(env!("CARGO_BIN_EXE_idk"))
            .arg("__host")
            .arg("--config-dir")
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
    while Client::connect_with_launcher(&store, Path::new(env!("CARGO_BIN_EXE_idk"))).is_err() {
        assert!(host.0.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    let operation = new_id();
    let output = invoke(
        &data,
        &home,
        &[
            "run",
            "start",
            project,
            task,
            "--operation-id",
            &operation,
            "--wait",
        ],
    );
    assert_eq!(
        output.status.code(),
        Some(7),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let failed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(failed["state"], "Failed");
    let run = failed["run_id"].as_str().unwrap();
    assert_eq!(failed["exit_code"], 7);
    assert_eq!(
        fs::read_to_string(source.join("init-count"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    let replay = success(
        &data,
        &home,
        &["run", "start", project, task, "--operation-id", &operation],
    );
    assert_eq!(replay["existing"], true);
    assert_eq!(replay["run"]["run_id"], run);
    assert_eq!(
        fs::read_to_string(source.join("init-count"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    let log = success(&data, &home, &["run", "log", run]);
    let raw = base64::engine::general_purpose::STANDARD
        .decode(log["value"]["data_base64"].as_str().unwrap())
        .unwrap();
    assert!(String::from_utf8_lossy(&raw).contains("TASK_retained"));
    let problems = success(&data, &home, &["run", "problems", run]);
    assert_eq!(problems["value"]["problems"].as_array().unwrap().len(), 1);
    let problem = problems["value"]["problems"][0]["id"].as_str().unwrap();
    let editor = root.join("editor-fixture");
    fs::write(
        &editor,
        "#!/bin/sh\nprintf '%s\\000' \"$@\" > \"$IDK_CLI_EDITOR_ARGS\"\n",
    )
    .unwrap();
    fs::set_permissions(&editor, fs::Permissions::from_mode(0o700)).unwrap();
    success(
        &data,
        &home,
        &[
            "task",
            "editor",
            project,
            editor.to_str().unwrap(),
            "--arg=+{line}",
            "--arg=--",
            "--arg={file}",
        ],
    );
    success(&data, &home, &["run", "editor", run, problem]);
    assert!(!home.join("editor-args").exists());
    success(&data, &home, &["run", "editor", run, problem, "--yes"]);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !home.join("editor-args").exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        fs::read(home.join("editor-args")).unwrap(),
        format!("+1\0--\0{}\0", source.join("main.cpp").display()).as_bytes()
    );
    assert_eq!(
        fs::read_to_string(source.join("init-count"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    let slow = success(
        &data,
        &home,
        &["task", "add", project, "slow", "--command", "sleep 30"],
    );
    let slow = slow["id"].as_str().unwrap();
    success(&data, &home, &["task", "approve", project, slow, "--yes"]);
    let running = success(&data, &home, &["run", "start", project, slow]);
    let running = running["run"]["run_id"].as_str().unwrap();
    let preview = success(&data, &home, &["run", "cancel", running]);
    assert_eq!(preview["performed"], false);
    success(
        &data,
        &home,
        &["run", "cancel", running, "--yes", "--force"],
    );
    let cancelled = invoke(&data, &home, &["run", "wait", running]);
    assert_eq!(cancelled.status.code(), Some(130));
    success(&data, &home, &["task", "remove", project, slow, "--yes"]);
    assert!(
        success(&data, &home, &["run", "status", running])["cleanup_confirmed"]
            .as_bool()
            .unwrap()
    );
    let ledger = fs::read_to_string(store.state_dir.join("runs.json")).unwrap();
    assert!(!ledger.contains("never-in-run-ledger"));
    assert_eq!(
        fs::read(source.join("main.cpp")).unwrap(),
        b"bad source kept\n"
    );
    success(&data, &home, &["host", "stop", "--yes", "--force"]);
    let deadline = Instant::now() + Duration::from_secs(8);
    while host.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
}
