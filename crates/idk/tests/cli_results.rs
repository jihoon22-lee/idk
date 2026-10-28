#[path = "common/launcher.rs"]
mod launcher;
use idk_workspace::{
    client::Client, model::EditorConfig, portable, project::LaunchEnvironment, store::Store,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant},
};
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
    Command::new(launcher::path())
        .arg("--data-dir")
        .arg(data)
        .args(args)
        .env("HOME", home)
        .env("SHELL", "/bin/sh")
        .env("PATH", "/usr/bin:/bin")
        .env_remove("IDK_HOME")
        .output()
        .unwrap()
}
fn json_success(data: &Path, home: &Path, args: &[&str]) -> Value {
    let output = invoke(data, home, args);
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
#[test]
fn native_cli_approve_result_pages_baseline_export_progress_and_structured_editor() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    let home = tmp.path().join("home");
    let root = tmp.path().join("source");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&root).unwrap();
    fs::write(root.join("main.c"), "int main(void) {}\n").unwrap();
    let script="cp report.json \"$IDK_RESULT_PATH\"; printf '{cut' > \"$IDK_EVENTS_PATH\"; printf done > executed; exit 7";
    let declaration = format!(
        "schema_version=1\n[tasks.report]\nprogram='/bin/sh'\nargs={}\n",
        toml::Value::Array(vec![
            toml::Value::String("-c".into()),
            toml::Value::String(script.into())
        ])
    );
    fs::write(root.join("idk.toml"), declaration).unwrap();
    fs::write(root.join("report.json"),serde_json::to_vec(&json!({"schema_id":"lens.result","schema_version":1,"producer":{"name":"lens","version":"0.10.0"},"run_id":null,"execution":{"cwd":root,"exit_code":7},"source":{"kind":"command","identity":["fixture","test"]},"complete":true,"findings":[{"severity":"fail","code":"test.failed","message":"actual reported failure","evidence":{"path":"main.c","object":"main"}}]})).unwrap()).unwrap();
    let store = Store::open(Some(&data)).unwrap();
    let env = LaunchEnvironment::from_variables(BTreeMap::from([
        ("HOME".into(), home.to_string_lossy().into()),
        ("SHELL".into(), "/bin/sh".into()),
        ("PATH".into(), "/usr/bin:/bin".into()),
    ]))
    .unwrap();
    let project = portable::apply(&store, portable::resolve(&store, &root, &env).unwrap()).unwrap();
    let mut host = OwnedHost(
        Command::new(launcher::path())
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
    while Client::connect_with_launcher(&store, launcher::path()).is_err() {
        assert!(host.0.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !invoke(&data, &home, &["run", "start", &project.id, "report"])
            .status
            .success()
    );
    assert!(!root.join("executed").exists());
    let output = invoke(
        &data,
        &home,
        &["run", "start", &project.id, "report", "--approve", "--wait"],
    );
    assert_eq!(
        output.status.code(),
        Some(7),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let run: Value = serde_json::from_slice(&output.stdout).unwrap();
    let run_id = run["run_id"].as_str().unwrap();
    assert_eq!(run["structured_result"]["state"], "stored");
    assert!(String::from_utf8_lossy(&output.stderr).contains("\"digest\""));
    let findings = json_success(&data, &home, &["run", "result", run_id, "--json"]);
    assert_eq!(findings["total"], 1);
    let sha = findings["findings"][0]["body_sha256"].as_str().unwrap();
    let readable = invoke(&data, &home, &["run", "result", run_id]);
    assert!(readable.status.success());
    assert!(String::from_utf8_lossy(&readable.stdout).contains("actual reported failure"));
    let raw = json_success(
        &data,
        &home,
        &["run", "result", run_id, "--raw", "--limit", "10", "--json"],
    );
    assert_eq!(raw["next_offset"], 10);
    assert_eq!(raw["eof"], false);
    assert_eq!(
        json_success(&data, &home, &["run", "pin", run_id, "--json"])["pinned"],
        true
    );
    assert_eq!(
        json_success(&data, &home, &["run", "compare", run_id, run_id, "--json"])
            ["unchanged_count"],
        1
    );
    assert_eq!(
        json_success(&data, &home, &["run", "events", run_id, "--json"])["truncated_tail"],
        true
    );
    let destination = root.join("export.json");
    json_success(
        &data,
        &home,
        &[
            "run",
            "export",
            run_id,
            destination.to_str().unwrap(),
            "--json",
        ],
    );
    let original = fs::read(&destination).unwrap();
    assert_eq!(original, fs::read(root.join("report.json")).unwrap());
    assert!(!invoke(
        &data,
        &home,
        &["run", "export", run_id, destination.to_str().unwrap()]
    )
    .status
    .success());
    assert_eq!(fs::read(&destination).unwrap(), original);
    (idk_workspace::editor::EditorService { store: &store })
        .save(
            store.load().unwrap().revision,
            &project.id,
            EditorConfig {
                executable: "/bin/true".into(),
                args: vec!["--".into(), "{file}".into()],
                external_gui: false,
            },
        )
        .unwrap();
    let review = json_success(
        &data,
        &home,
        &["run", "finding-editor", run_id, "0", "--body-sha256", sha],
    );
    assert_eq!(review["value"]["review"]["finding"]["body_sha256"], sha);
    assert_eq!(
        review["value"]["review"]["file"],
        root.join("main.c").to_str().unwrap()
    );
    json_success(
        &data,
        &home,
        &[
            "run",
            "finding-editor",
            run_id,
            "0",
            "--body-sha256",
            sha,
            "--yes",
        ],
    );
    assert_eq!(
        json_success(&data, &home, &["run", "unpin", run_id, "--json"])["pinned"],
        false
    );
    json_success(&data, &home, &["host", "stop", "--yes", "--force"]);
    let deadline = Instant::now() + Duration::from_secs(10);
    while host.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
}
