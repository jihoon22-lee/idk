#[path = "common/launcher.rs"]
mod launcher;

use idk_workspace::client::Client;
use idk_workspace::model::new_id;
use idk_workspace::store::{atomic_write, Store};
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

// Serialize subprocess fixtures so a concurrent fork cannot inherit the copied
// host's writable FD before CLOEXEC runs and keep that executable busy.
static PROCESS_FIXTURE_LOCK: Mutex<()> = Mutex::new(());

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

fn doctor(data: &Path, strict: bool) -> (Output, Value) {
    let mut command = Command::new(launcher::path());
    command
        .arg("--data-dir")
        .arg(data)
        .args(["doctor", "--json"]);
    if strict {
        command.arg("--strict");
    }
    let output = command.output().unwrap();
    let report = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("doctor did not return JSON: {error}; {output:?}"));
    (output, report)
}

fn finding<'a>(report: &'a Value, area: &str) -> &'a Value {
    report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|finding| finding["area"] == area)
        .unwrap_or_else(|| panic!("missing {area} finding: {report}"))
}

fn running_record(pid: u32) -> Value {
    json!({"schema":1,"host_instance":new_id(),"pid":pid,"started_at_ms":1,
        "stopped_at_ms":null,"error":null})
}

fn host(store: &Store, binary: &Path) -> (OwnedChild, Client) {
    let mut child = OwnedChild(
        Command::new(binary)
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
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(client) = Client::connect_with_launcher(store, binary) {
            return (child, client);
        }
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "host exited at startup"
        );
        assert!(Instant::now() < deadline, "host startup timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn shutdown(host: &mut OwnedChild, client: &mut Client) {
    client.shutdown(&[], false).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = host.0.try_wait().unwrap() {
            assert!(status.success());
            return;
        }
        assert!(Instant::now() < deadline, "host shutdown timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn strict_doctor_rejects_an_unrelated_live_pid_without_changing_the_record_or_process() {
    let _serial = PROCESS_FIXTURE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let store = Store::open(Some(&data)).unwrap();
    let mut unrelated = OwnedChild(Command::new("sleep").arg("60").spawn().unwrap());
    // A stale record after a crash/reboot can refer to a PID now owned by an
    // unrelated process. It is deliberately live, so kill(pid, 0) would succeed.
    store
        .write_state("host-exit.json", &running_record(unrelated.0.id()))
        .unwrap();
    let path = store.state_dir.join("host-exit.json");
    let before = fs::read(&path).unwrap();
    let (output, report) = doctor(&data, true);
    assert!(
        !output.status.success(),
        "strict doctor accepted stale identity: {report}"
    );
    assert_eq!(finding(&report, "host-exit")["status"], "warning");
    assert!(finding(&report, "host-exit")["detail"]
        .as_str()
        .unwrap()
        .contains("not confirmed"));
    assert_eq!(fs::read(path).unwrap(), before);
    assert!(unrelated.0.try_wait().unwrap().is_none());
    assert!(!store.socket_path().exists());
    // The informational command keeps its documented default exit status.
    let (output, report) = doctor(&data, false);
    assert!(output.status.success());
    assert_eq!(finding(&report, "host-exit")["status"], "warning");
}

#[test]
fn strict_doctor_requires_the_live_handshake_to_match_both_recorded_identity_fields() {
    let _serial = PROCESS_FIXTURE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let store = Store::open(Some(&data)).unwrap();
    let (mut host, mut client) = host(&store, launcher::path());
    let path = store.state_dir.join("host-exit.json");
    let before = fs::read(&path).unwrap();
    let (output, report) = doctor(&data, true);
    assert!(output.status.success(), "{report}");
    assert_eq!(finding(&report, "host-exit")["status"], "ok");

    fs::remove_file(&path).unwrap();
    let (output, report) = doctor(&data, true);
    assert!(output.status.success(), "{report}");
    assert_eq!(finding(&report, "host")["status"], "ok");
    assert!(
        !path.exists(),
        "diagnosis must not synthesize an exit record"
    );
    atomic_write(&path, &before).unwrap();

    let original: Value = serde_json::from_slice(&before).unwrap();
    let mut other_instance = original.clone();
    other_instance["host_instance"] = json!(new_id());
    let mut unrelated = OwnedChild(Command::new("sleep").arg("60").spawn().unwrap());
    let mut other_pid = original;
    other_pid["pid"] = json!(unrelated.0.id());
    for record in [other_instance, other_pid] {
        store.write_state("host-exit.json", &record).unwrap();
        let bytes = fs::read(&path).unwrap();
        let (output, report) = doctor(&data, true);
        assert!(
            !output.status.success(),
            "accepted a mismatched identity: {report}"
        );
        assert_eq!(finding(&report, "host")["status"], "ok");
        assert_eq!(finding(&report, "host-exit")["status"], "warning");
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
    assert!(unrelated.0.try_wait().unwrap().is_none());
    atomic_write(&path, &before).unwrap();
    shutdown(&mut host, &mut client);
    let (output, report) = doctor(&data, true);
    assert!(output.status.success(), "{report}");
    assert!(finding(&report, "host-exit")["detail"]
        .as_str()
        .unwrap()
        .contains("clean shutdown"));
}

#[test]
fn strict_doctor_keeps_a_different_build_host_unconfirmed_and_preserves_it() {
    let _serial = PROCESS_FIXTURE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let store = Store::open(Some(&data)).unwrap();
    let other_binary = temp.path().join("other-idk");
    fs::copy(launcher::path(), &other_binary).unwrap();
    // Appending a non-loadable marker changes executable identity while
    // retaining the actual host implementation and protocol.
    fs::OpenOptions::new()
        .append(true)
        .open(&other_binary)
        .unwrap()
        .write_all(b"\nIDK_DOCTOR_TEST_OTHER_BUILD\n")
        .unwrap();
    fs::set_permissions(&other_binary, fs::Permissions::from_mode(0o700)).unwrap();
    let (mut host, mut client) = host(&store, &other_binary);
    let path = store.state_dir.join("host-exit.json");
    let before = fs::read(&path).unwrap();
    let (output, report) = doctor(&data, true);
    assert!(!output.status.success());
    assert_eq!(finding(&report, "host")["status"], "warning");
    assert_eq!(finding(&report, "host-exit")["status"], "warning");
    assert_eq!(fs::read(path).unwrap(), before);
    assert!(host.0.try_wait().unwrap().is_none());
    shutdown(&mut host, &mut client);
}

#[test]
fn doctor_preserves_missing_stopped_and_unreadable_exit_record_semantics() {
    let _serial = PROCESS_FIXTURE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let store = Store::open(Some(&data)).unwrap();
    let path = store.state_dir.join("host-exit.json");
    let (output, report) = doctor(&data, true);
    assert!(output.status.success(), "{report}");
    assert!(!path.exists());
    assert!(!store.socket_path().exists());

    let mut stopped = running_record(std::process::id());
    stopped["stopped_at_ms"] = json!(2);
    store.write_state("host-exit.json", &stopped).unwrap();
    let (output, report) = doctor(&data, true);
    assert!(output.status.success(), "{report}");
    assert_eq!(finding(&report, "host-exit")["status"], "ok");
    stopped["error"] = json!("host serve loop failed");
    let mut future = running_record(std::process::id());
    future["schema"] = json!(99);
    for bytes in [
        serde_json::to_vec(&stopped).unwrap(),
        serde_json::to_vec(&future).unwrap(),
        b"{broken host exit record".to_vec(),
    ] {
        atomic_write(&path, &bytes).unwrap();
        let (output, report) = doctor(&data, true);
        assert!(!output.status.success(), "{report}");
        assert_eq!(finding(&report, "host-exit")["status"], "warning");
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
}
