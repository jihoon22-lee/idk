use base64::Engine;
use idk_workspace::client::Client;
use idk_workspace::model::{new_id, Project, ShellConfig};
use idk_workspace::project::{ConnectDraft, LaunchEnvironment, ProjectService, TerminalDraft};
use idk_workspace::protocol::{BatchState, Request, SessionInfo, SessionState};
use idk_workspace::shell::InitializationState;
use idk_workspace::store::Store;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}
struct Fixture {
    _tmp: tempfile::TempDir,
    store: Store,
    root: PathBuf,
    env: BTreeMap<String, String>,
    shell: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("source 한글");
        fs::create_dir(&root).unwrap();
        let home = tmp.path().join("home");
        fs::create_dir(&home).unwrap();
        let store = Store::open(Some(&tmp.path().join("data"))).unwrap();
        let shell = std::env::var_os("IDK_TEST_SHELL")
            .or_else(|| std::env::var_os("IDK_TEST_TCSH"))
            .map(PathBuf::from)
            .unwrap_or_else(|| "/usr/bin/tcsh".into());
        assert!(shell.is_file(), "provide a real tcsh using IDK_TEST_SHELL");
        let env = BTreeMap::from([
            ("HOME".into(), home.to_str().unwrap().into()),
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("TERM".into(), "xterm-256color".into()),
            ("LANG".into(), "C.UTF-8".into()),
            (
                "HOST_PRIVATE_TOKEN".into(),
                "must-never-enter-ledger".into(),
            ),
        ]);
        Self {
            _tmp: tmp,
            store,
            root,
            env,
            shell,
        }
    }
    fn project(&self, name: &str, sources: &[&str]) -> Project {
        let service = ProjectService { store: &self.store };
        let mut terminals = Vec::new();
        for (index, source) in sources.iter().enumerate() {
            let path = self.root.join(format!("{name}-{index}.csh"));
            fs::write(&path, source).unwrap();
            terminals.push(TerminalDraft {
                name: format!("Terminal {index}"),
                cwd: self.root.clone(),
                sources: vec![path.into()],
                persistent: true,
            });
        }
        let mut preview = service
            .preview_connect(ConnectDraft {
                name: name.into(),
                root: self.root.clone(),
                shell: ShellConfig {
                    executable: self.shell.clone(),
                    login: false,
                    init_cwd: self.root.clone(),
                    sources: Vec::new(),
                    trusted_digest: None,
                },
                terminals,
            })
            .unwrap();
        preview.allow_duplicate_root = true;
        let project = service.create(preview.revision, preview).unwrap();
        let environment = LaunchEnvironment::from_variables(self.env.clone()).unwrap();
        let review = service
            .review_initialization(&project.id, &environment)
            .unwrap();
        assert!(review.common.error.is_none(), "{:?}", review.common.error);
        service
            .approve_initialization(review.revision, review)
            .unwrap();
        self.store
            .load()
            .unwrap()
            .project(&project.id)
            .unwrap()
            .clone()
    }
    fn host(&self) -> (OwnedChild, Client) {
        let mut host = OwnedChild(
            Command::new(binary())
                .arg("__host")
                .arg("--config-dir")
                .arg(&self.store.config_dir)
                .arg("--state-dir")
                .arg(&self.store.state_dir)
                .arg("--runtime-dir")
                .arg(&self.store.runtime_dir)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(client) = Client::connect_with_launcher(&self.store, binary()) {
                return (host, client);
            }
            assert!(
                host.0.try_wait().unwrap().is_none(),
                "host failed at startup"
            );
            assert!(Instant::now() < deadline, "host startup timed out");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    fn client(&self) -> Client {
        Client::connect_with_launcher(&self.store, binary()).unwrap()
    }
}
fn binary() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_idk"))
}
fn wait(
    client: &mut Client,
    session: &str,
    predicate: impl Fn(&SessionInfo) -> bool,
) -> SessionInfo {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let info = client
            .list(None)
            .unwrap()
            .into_iter()
            .find(|info| info.session_id == session)
            .unwrap();
        if predicate(&info) {
            return info;
        }
        assert!(
            Instant::now() < deadline,
            "terminal wait timed out: {info:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn ready(client: &mut Client, session: &str) -> SessionInfo {
    wait(client, session, |info| {
        info.initialization == Some(InitializationState::Ready)
    })
}
fn screen(client: &mut Client, session: &str, needle: &str) {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if client
            .snapshot(session, None)
            .unwrap()
            .screen
            .is_some_and(|screen| screen.text().contains(needle))
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "terminal did not display {needle}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn stop(client: &mut Client, host: &mut OwnedChild) {
    let targets: Vec<_> = client
        .preview_close(None)
        .unwrap()
        .targets
        .into_iter()
        .map(|info| info.session_id)
        .collect();
    client.shutdown(&targets, true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = host.0.try_wait().unwrap() {
            assert!(status.success());
            return;
        }
        assert!(Instant::now() < deadline, "host shutdown did not finish");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn two_projects_six_shells_keep_state_and_enforce_epochs_and_exactly_once_input() {
    let fixture = Fixture::new();
    let source = "set local_state = original\nalias retained_alias 'echo ALIAS_RETAINED'\necho initialized >> init-count\n";
    let first = fixture.project("first", &[source, source, source]);
    let second = fixture.project("second", &[source, source, source]);
    let (mut host, mut client) = fixture.host();
    let mut sessions = Vec::new();
    for project in [&first, &second] {
        for terminal in &project.terminals {
            let created = client
                .start(
                    &project.id,
                    &terminal.id,
                    fixture.env.clone(),
                    24,
                    100,
                    false,
                )
                .unwrap();
            let info = ready(&mut client, &created.session_id);
            assert!(info.child_pid.is_some());
            sessions.push(info);
        }
    }
    let mut pids: Vec<_> = sessions.iter().map(|info| info.child_pid).collect();
    pids.sort();
    pids.dedup();
    assert_eq!(pids.len(), 6);
    assert_eq!(
        fs::read_to_string(fixture.root.join("init-count"))
            .unwrap()
            .lines()
            .count(),
        6
    );
    let session = &sessions[0].session_id;
    let owned = client.attach(session, false).unwrap();
    let input = Request::Input {
        session: session.clone(),
        epoch: owned.input_epoch,
        data: base64::engine::general_purpose::STANDARD
            .encode(b"echo once >> input-count\nset local_state = changed\n"),
    };
    let envelope = client.envelope(input);
    client
        .send_envelope(&envelope)
        .unwrap()
        .decode::<()>()
        .unwrap();
    client
        .send_envelope(&envelope)
        .unwrap()
        .decode::<()>()
        .unwrap();
    let mut changed = envelope.clone();
    changed.request = Request::Input {
        session: session.clone(),
        epoch: owned.input_epoch,
        data: base64::engine::general_purpose::STANDARD.encode(b"echo forbidden >> input-count\n"),
    };
    assert!(client
        .send_envelope(&changed)
        .unwrap()
        .error
        .unwrap()
        .contains("different content"));
    let mut expired = envelope.clone();
    expired.deadline_ms = 1;
    assert!(client.send_envelope(&expired).is_err());
    client
        .input(
            session,
            owned.input_epoch,
            b"echo LOCAL:$local_state; retained_alias\n",
        )
        .unwrap();
    screen(&mut client, session, "LOCAL:changed");
    screen(&mut client, session, "ALIAS_RETAINED");
    assert_eq!(
        fs::read_to_string(fixture.root.join("input-count")).unwrap(),
        "once\n"
    );
    let before = client.snapshot(session, None).unwrap().screen.unwrap();
    assert!(client
        .snapshot(session, Some(before.generation))
        .unwrap()
        .screen
        .is_none());
    let mut other = fixture.client();
    assert!(other.attach(session, false).is_err());
    let taken = other.attach(session, true).unwrap();
    assert!(taken.input_epoch > owned.input_epoch);
    assert!(client
        .input(session, owned.input_epoch, b"echo stale\n")
        .is_err());
    assert!(client.detach(session, owned.input_epoch).is_err());
    assert!(client.resize(session, owned.input_epoch, 25, 80).is_err());
    other.detach(session, taken.input_epoch).unwrap();
    drop(other);
    let mut attached = fixture.client();
    let current = attached.attach(session, false).unwrap();
    attached
        .input(
            session,
            current.input_epoch,
            b"echo RECONNECT:$local_state\n",
        )
        .unwrap();
    screen(&mut attached, session, "RECONNECT:changed");
    let same = attached
        .start(
            &first.id,
            &first.terminals[0].id,
            fixture.env.clone(),
            30,
            90,
            false,
        )
        .unwrap();
    assert_eq!(same.session_id, *session);
    assert_eq!(same.child_pid, sessions[0].child_pid);
    let ledger = fs::read_to_string(fixture.store.state_dir.join("host-sessions.json")).unwrap();
    assert!(
        !ledger.contains("must-never-enter-ledger")
            && !ledger.contains("local_state")
            && !ledger.contains("child_pid")
            && !ledger.contains("HOST_PRIVATE_TOKEN")
    );
    stop(&mut attached, &mut host);
}

#[test]
fn default_open_pauses_for_interactive_initialization_and_cancel_preserves_shell() {
    let fixture = Fixture::new();
    let project = fixture.project(
        "interactive",
        &[
            "echo WAITING_FOR_ANSWER\nset answer = $<\necho ANSWER:$answer\n",
            "echo NEXT_OPENED >> next-count\n",
        ],
    );
    let (mut host, mut client) = fixture.host();
    let batch = client
        .start_defaults(&project.id, fixture.env.clone(), 24, 100)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let paused = loop {
        let current = client.batch(&batch.batch_id).unwrap();
        if current.state == BatchState::Paused {
            break current;
        }
        assert!(
            Instant::now() < deadline,
            "batch did not pause: {current:?}"
        );
        std::thread::sleep(Duration::from_millis(30));
    };
    assert_eq!(paused.remaining.len(), 1);
    assert!(!fixture.root.join("next-count").exists());
    let session = paused.waiting_session.unwrap();
    let duplicate = client
        .start_defaults(&project.id, fixture.env.clone(), 30, 90)
        .unwrap();
    assert_eq!(duplicate.batch_id, batch.batch_id);
    let attached = client.attach(&session, false).unwrap();
    assert_eq!(
        attached.initialization,
        Some(InitializationState::Initializing)
    );
    client.cancel_defaults(&batch.batch_id).unwrap();
    client
        .input(&session, attached.input_epoch, b"retained-answer\n")
        .unwrap();
    ready(&mut client, &session);
    screen(&mut client, &session, "ANSWER:retained-answer");
    assert_eq!(
        client.batch(&batch.batch_id).unwrap().state,
        BatchState::Cancelled
    );
    assert!(!fixture.root.join("next-count").exists());
    // A fresh accepted batch may reuse the same ready shell and continue defaults.
    let fresh = client
        .start_defaults(&project.id, fixture.env.clone(), 24, 100)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let current = client.batch(&fresh.batch_id).unwrap();
        if current.state == BatchState::Complete {
            break;
        }
        assert!(Instant::now() < deadline, "fresh batch did not complete");
        std::thread::sleep(Duration::from_millis(30));
    }
    assert_eq!(
        fs::read_to_string(fixture.root.join("next-count"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    stop(&mut client, &mut host);
}

#[test]
fn close_preview_is_atomic_and_closed_tombstones_survive_replacement() {
    let fixture = Fixture::new();
    let project = fixture.project(
        "close",
        &["echo first >> init-count\n", "echo second >> init-count\n"],
    );
    let (mut host, mut client) = fixture.host();
    let first = client
        .start(
            &project.id,
            &project.terminals[0].id,
            fixture.env.clone(),
            24,
            80,
            false,
        )
        .unwrap();
    ready(&mut client, &first.session_id);
    let preview = client.preview_close(Some(&project.id)).unwrap();
    let second = client
        .start(
            &project.id,
            &project.terminals[1].id,
            fixture.env.clone(),
            24,
            80,
            false,
        )
        .unwrap();
    ready(&mut client, &second.session_id);
    let targets: Vec<_> = preview
        .targets
        .into_iter()
        .map(|info| info.session_id)
        .collect();
    assert!(client.close_project(&project.id, &targets, true).is_err());
    assert!(client
        .list(None)
        .unwrap()
        .iter()
        .all(|info| info.state == SessionState::Running));
    let attached = client.attach(&first.session_id, false).unwrap();
    client
        .close(&first.session_id, attached.input_epoch, false)
        .unwrap();
    wait(&mut client, &first.session_id, |info| {
        info.state == SessionState::Closed
    });
    assert!(client
        .start(
            &project.id,
            &project.terminals[0].id,
            fixture.env.clone(),
            24,
            80,
            false
        )
        .is_err());
    stop(&mut client, &mut host);
    let (mut replacement, mut other) = fixture.host();
    let list = other.list(None).unwrap();
    assert_eq!(list.len(), 2);
    assert!(list.iter().all(|info| info.state == SessionState::Closed));
    assert!(other
        .start(
            &project.id,
            &project.terminals[0].id,
            fixture.env.clone(),
            24,
            80,
            false
        )
        .is_err());
    let reopened = other
        .start(
            &project.id,
            &project.terminals[0].id,
            fixture.env.clone(),
            24,
            80,
            true,
        )
        .unwrap();
    assert_ne!(reopened.session_id, first.session_id);
    ready(&mut other, &reopened.session_id);
    assert_eq!(
        fs::read_to_string(fixture.root.join("init-count"))
            .unwrap()
            .lines()
            .count(),
        3
    );
    stop(&mut other, &mut replacement);
}

#[test]
fn abrupt_host_loss_restores_unknown_without_replaying_initialization_or_adopting_pids() {
    let fixture = Fixture::new();
    let project = fixture.project("lost", &["echo source >> source-count\n"]);
    let (mut host, mut client) = fixture.host();
    let old_host = client.info().host_instance.clone();
    let created = client
        .start(
            &project.id,
            &project.terminals[0].id,
            fixture.env.clone(),
            24,
            80,
            false,
        )
        .unwrap();
    ready(&mut client, &created.session_id);
    let unrelated = OwnedChild(Command::new("sleep").arg("60").spawn().unwrap());
    host.0.kill().unwrap();
    host.0.wait().unwrap();
    let (mut replacement, mut other) = fixture.host();
    assert_ne!(other.info().host_instance, old_host);
    let restored = other.list(None).unwrap();
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].state, SessionState::Unknown);
    assert_eq!(restored[0].child_pid, None);
    assert!(restored[0].owner.is_none());
    assert!(other
        .start(
            &project.id,
            &project.terminals[0].id,
            fixture.env.clone(),
            24,
            80,
            false
        )
        .is_err());
    assert!(client.list(None).is_err());
    assert_eq!(
        fs::read_to_string(fixture.root.join("source-count"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert_eq!(unsafe { libc::kill(unrelated.0.id() as i32, 0) }, 0);
    stop(&mut other, &mut replacement);
}

#[test]
fn scrollback_search_resize_and_force_close_report_observed_results() {
    let fixture = Fixture::new();
    let project = fixture.project("force", &["echo READY_FOR_SEARCH\n"]);
    let (mut host, mut client) = fixture.host();
    let created = client
        .start(
            &project.id,
            &project.terminals[0].id,
            fixture.env.clone(),
            12,
            80,
            false,
        )
        .unwrap();
    ready(&mut client, &created.session_id);
    let owned = client.attach(&created.session_id, false).unwrap();
    let session = &created.session_id;
    client.input(session, owned.input_epoch, b"python3 -c 'print(\"history_needle\\n\" + \"normal_line\\n\" * 80 + \"SEARCH_\" + \"DONE\")'\n").unwrap();
    screen(&mut client, session, "SEARCH_DONE");
    let result = client
        .search(session, owned.input_epoch, "history_needle", true)
        .unwrap();
    assert!(result.found.is_some_and(|point| point.line < 0));
    assert!(result.searched_rows > 12);
    let searched = client.snapshot(session, None).unwrap().screen.unwrap();
    assert!(searched.display_offset > 0);
    assert!(searched.text().contains("history_needle"));
    client.scroll(session, owned.input_epoch, i32::MIN).unwrap();
    assert_eq!(
        client
            .snapshot(session, None)
            .unwrap()
            .screen
            .unwrap()
            .display_offset,
        0
    );
    client.resize(session, owned.input_epoch, 20, 100).unwrap();
    let resized = client.snapshot(session, None).unwrap().screen.unwrap();
    assert_eq!((resized.rows, resized.cols), (20, 100));
    assert!(resized.generation > searched.generation);
    fs::write(fixture.root.join("stress.py"), "import time\ntime.sleep(0.3)\nfor i in range(10000): print('bounded-background-output-' * 3)\nopen('stress-done', 'w').write('done')\n").unwrap();
    client
        .input(session, owned.input_epoch, b"python3 stress.py\n")
        .unwrap();
    // Four stalled frame readers occupy every IPC worker. The host's actor and
    // PTY reader must still drain a hidden terminal without depending on them.
    let slow: Vec<_> = (0..4)
        .map(|_| {
            let mut stream = UnixStream::connect(fixture.store.socket_path()).unwrap();
            stream.write_all(&[0]).unwrap();
            stream
        })
        .collect();
    let deadline = Instant::now() + Duration::from_secs(4);
    while !fixture.root.join("stress-done").exists() {
        assert!(
            Instant::now() < deadline,
            "stalled IPC peers blocked terminal output"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(slow);
    client.input(session, owned.input_epoch, b"exec python3 -c 'import signal,time; signal.signal(signal.SIGHUP, signal.SIG_IGN); print(\"IGNORE_\" + \"HUP_READY\", flush=True); time.sleep(60)'\n").unwrap();
    screen(&mut client, session, "IGNORE_HUP_READY");
    let closing = client.close(session, owned.input_epoch, false).unwrap();
    assert_eq!(closing.state, SessionState::Closing);
    let pending = wait(&mut client, session, |info| {
        info.error
            .as_ref()
            .is_some_and(|error| error.contains("explicit force"))
    });
    assert_eq!(pending.state, SessionState::Closing);
    assert!(pending.exit.is_none());
    client.close(session, owned.input_epoch, true).unwrap();
    let closed = wait(&mut client, session, |info| {
        info.state == SessionState::Closed
    });
    assert!(closed.exit.is_some());
    assert!(closed.child_pid.is_none());
    stop(&mut client, &mut host);
}

#[test]
fn failed_default_is_retained_and_next_terminal_opens_without_pinning_owner() {
    let fixture = Fixture::new();
    let project = fixture.project(
        "partial",
        &["echo will-be-invalid\n", "echo SECOND_READY\n"],
    );
    // Change the already reviewed first source: failure belongs to that item,
    // and must not silently approve the new contents or stop unrelated defaults.
    fs::write(
        &project.terminals[0].sources[0].path,
        "echo unapproved >> must-not-execute\n",
    )
    .unwrap();
    let (mut host, mut client) = fixture.host();
    let batch = client
        .start_defaults(&project.id, fixture.env.clone(), 24, 80)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    let completed = loop {
        let current = client.batch(&batch.batch_id).unwrap();
        if current.state == BatchState::Complete {
            break current;
        }
        assert!(
            Instant::now() < deadline,
            "partial default batch did not complete: {current:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(completed.failures.len(), 1);
    assert!(completed.failures.contains_key(&project.terminals[0].id));
    let sessions = client.list(None).unwrap();
    assert_eq!(sessions.len(), 2);
    let failed = sessions
        .iter()
        .find(|info| info.terminal_id == project.terminals[0].id)
        .unwrap();
    assert_eq!(failed.state, SessionState::Failed);
    assert!(failed.owner.is_none());
    let success = sessions
        .iter()
        .find(|info| info.terminal_id == project.terminals[1].id)
        .unwrap();
    assert_eq!(success.initialization, Some(InitializationState::Ready));
    assert!(!fixture.root.join("must-not-execute").exists());
    stop(&mut client, &mut host);
}

#[test]
fn transient_history_does_not_exhaust_live_capacity_and_explicit_reopen_sources_once() {
    let fixture = Fixture::new();
    let project = fixture.project("transient", &["echo source >> transient-count\n"]);
    // A history fixture represents confirmed old closures, not live PTYs. The
    // following launch/reopen are actual tcsh processes using reviewed sources.
    let history: Vec<_> = (0..65)
        .map(|index| {
            serde_json::json!({
                "session_id": new_id(), "project_id": project.id, "terminal_id": new_id(),
                "name": format!("Old transient {index}"), "persistent": false, "state": "closed",
                "definition_revision": 0, "launch_digest": null, "exit": null,
            })
        })
        .collect();
    fixture
        .store
        .write_state(
            "host-sessions.json",
            &serde_json::json!({"schema": 1, "sessions": history}),
        )
        .unwrap();
    let mut definition = project.terminals[0].clone();
    definition.id = new_id();
    definition.name = "Temporary".into();
    definition.persistent = false;
    definition.trusted_digest = None;
    let service = ProjectService {
        store: &fixture.store,
    };
    let environment = LaunchEnvironment::from_variables(fixture.env.clone()).unwrap();
    let review = service
        .review_terminal(&project.id, &definition, &environment)
        .unwrap();
    service
        .approve_transient(
            fixture.store.load().unwrap().revision,
            &project.id,
            &mut definition,
            &review,
            &environment,
        )
        .unwrap();
    let (mut host, mut client) = fixture.host();
    let mut collision = project.terminals[0].clone();
    collision.persistent = false;
    assert!(client
        .start_transient(&project.id, collision, fixture.env.clone(), 24, 80)
        .is_err());
    let first = client
        .start_transient(&project.id, definition.clone(), fixture.env.clone(), 24, 80)
        .unwrap();
    ready(&mut client, &first.session_id);
    let attached = client.attach(&first.session_id, false).unwrap();
    client
        .close(&first.session_id, attached.input_epoch, true)
        .unwrap();
    wait(&mut client, &first.session_id, |info| {
        info.state == SessionState::Closed
    });
    assert!(client
        .start_transient(&project.id, definition.clone(), fixture.env.clone(), 24, 80)
        .is_err());
    assert_eq!(client.definition(&first.session_id).unwrap(), definition);
    let reopened = client
        .reopen_transient(&project.id, definition.clone(), fixture.env.clone(), 24, 80)
        .unwrap();
    assert_ne!(first.session_id, reopened.session_id);
    assert_eq!(reopened.terminal_id, definition.id);
    ready(&mut client, &reopened.session_id);
    assert_eq!(
        fs::read_to_string(fixture.root.join("transient-count"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    assert_eq!(client.list(None).unwrap().len(), 66);
    assert!(!fixture
        .store
        .load()
        .unwrap()
        .project(&project.id)
        .unwrap()
        .terminals
        .iter()
        .any(|terminal| terminal.id == definition.id));
    // Saving the reviewed temporary definition changes future launches. The
    // existing live runtime keeps its original identity until explicit reopen.
    fixture
        .store
        .update(|workspace| {
            let mut saved = definition.clone();
            saved.persistent = true;
            workspace.project_mut(&project.id)?.terminals.push(saved);
            Ok(())
        })
        .unwrap();
    let same = client
        .start(
            &project.id,
            &definition.id,
            fixture.env.clone(),
            24,
            80,
            false,
        )
        .unwrap();
    assert_eq!(same.session_id, reopened.session_id);
    let attached = client.attach(&reopened.session_id, false).unwrap();
    client
        .close(&reopened.session_id, attached.input_epoch, true)
        .unwrap();
    wait(&mut client, &reopened.session_id, |info| {
        info.state == SessionState::Closed
    });
    assert!(client
        .reopen_transient(&project.id, definition.clone(), fixture.env.clone(), 24, 80)
        .is_err());
    let saved = client
        .start(
            &project.id,
            &definition.id,
            fixture.env.clone(),
            24,
            80,
            true,
        )
        .unwrap();
    assert!(saved.persistent);
    assert_ne!(saved.session_id, reopened.session_id);
    ready(&mut client, &saved.session_id);
    assert_eq!(
        fs::read_to_string(fixture.root.join("transient-count"))
            .unwrap()
            .lines()
            .count(),
        3
    );
    stop(&mut client, &mut host);
}

#[test]
fn final_output_cutoff_reports_loss_to_clients_with_an_unchanged_screen_revision() {
    let fixture = Fixture::new();
    let project = fixture.project("final-output", &["echo READY_FOR_FINAL_OUTPUT\n"]);
    let (mut host, mut client) = fixture.host();
    let session = client
        .start(
            &project.id,
            &project.terminals[0].id,
            fixture.env.clone(),
            24,
            80,
            false,
        )
        .unwrap();
    ready(&mut client, &session.session_id);
    let attached = client.attach(&session.session_id, false).unwrap();
    // The exec'd shell leader exits after its child installs SIG_IGN. This
    // controlled descendant holds the original PTY beyond the host's two-second
    // collection limit, then exits itself. No external PID is adopted/signalled.
    fs::write(
        fixture.root.join("retained-pty.py"),
        r#"import os, signal, time
r, w = os.pipe()
pid = os.fork()
if pid:
    os.close(w)
    os.read(r, 1)
    os._exit(0)
os.close(r)
signal.signal(signal.SIGHUP, signal.SIG_IGN)
os.write(1, b'EARLY_DESCENDANT_OUTPUT\n')
release_deadline = time.monotonic() + 10
while not os.path.exists('release-parent') and time.monotonic() < release_deadline:
    time.sleep(0.01)
os.write(w, b'1')
os.close(w)
time.sleep(5)
try:
    os.write(1, b'LATE_DESCENDANT_OUTPUT\n')
except OSError:
    pass
open('descendant-done', 'w').write('done')
os._exit(0)
"#,
    )
    .unwrap();
    client
        .input(
            &session.session_id,
            attached.input_epoch,
            b"exec python3 retained-pty.py\n",
        )
        .unwrap();
    screen(&mut client, &session.session_id, "EARLY_DESCENDANT_OUTPUT");
    let before = client
        .snapshot(&session.session_id, None)
        .unwrap()
        .screen
        .unwrap();
    // Begin the exit/drain boundary only after this client has cached its screen.
    fs::write(fixture.root.join("release-parent"), b"release").unwrap();
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut limited = None;
    while Instant::now() < deadline {
        let reply = client
            .snapshot(&session.session_id, Some(before.generation))
            .unwrap();
        if reply
            .screen
            .as_ref()
            .is_some_and(|screen| screen.output_limited && screen.reader_closed)
        {
            limited = Some(reply);
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // Finish the owned fixture before asserting the regression result, including
    // on the old implementation where unchanged-since never returns a warning.
    let deadline = Instant::now() + Duration::from_secs(8);
    while !fixture.root.join("descendant-done").exists() {
        assert!(
            Instant::now() < deadline,
            "controlled PTY descendant did not finish"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    wait(&mut client, &session.session_id, |info| {
        info.state == SessionState::Exited
    });
    let final_reply = client.snapshot(&session.session_id, None).unwrap();
    stop(&mut client, &mut host);
    assert!(!before.reader_closed && !before.output_limited);
    let warning = limited.expect("final collection cutoff was hidden from unchanged-since client");
    assert_eq!(
        warning.session.state,
        SessionState::Closing,
        "known background descendants must keep cleanup pending"
    );
    let screen = warning.screen.unwrap();
    assert_ne!(screen.generation, before.generation);
    assert_eq!(warning.session.generation, screen.generation);
    assert!(warning
        .session
        .error
        .is_some_and(|error| error.contains("before PTY EOF")));
    let screen = final_reply.screen.unwrap();
    assert!(screen.output_limited && screen.reader_closed);
    assert!(screen.text().contains("EARLY_DESCENDANT_OUTPUT"));
    assert!(!screen.text().contains("LATE_DESCENDANT_OUTPUT"));
    assert_eq!(final_reply.session.state, SessionState::Exited);
}

fn fixture_pid(path: &Path) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(text) = fs::read_to_string(path) {
            if let Ok(pid) = text.trim().parse() {
                return pid;
            }
        }
        assert!(
            Instant::now() < deadline,
            "fixture did not publish {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn process_start(pid: u32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let tail = stat.rsplit_once(") ")?.1;
    tail.split_whitespace().nth(19)?.parse().ok()
}

#[test]
fn force_close_reaps_ordinary_background_jobs_and_preserves_other_projects_and_external_processes()
{
    let fixture = Fixture::new();
    let first = fixture.project("background-first", &["echo FIRST_READY\n"]);
    let second = fixture.project("background-second", &["echo SECOND_READY\n"]);
    let (mut host, mut client) = fixture.host();
    let mut unrelated = OwnedChild(Command::new("sleep").arg("60").spawn().unwrap());
    let mut sessions = Vec::new();
    let mut jobs = Vec::new();
    for (index, project) in [&first, &second].into_iter().enumerate() {
        let session = client
            .start(
                &project.id,
                &project.terminals[0].id,
                fixture.env.clone(),
                24,
                100,
                false,
            )
            .unwrap();
        let info = ready(&mut client, &session.session_id);
        let attached = client.attach(&session.session_id, false).unwrap();
        client
            .input(
                &session.session_id,
                attached.input_epoch,
                format!("sleep 30 &\necho $! > ordinary-bg-{index}.pid\n").as_bytes(),
            )
            .unwrap();
        let pid = fixture_pid(&fixture.root.join(format!("ordinary-bg-{index}.pid")));
        assert_eq!(
            unsafe { libc::getsid(pid as i32) },
            info.child_pid.unwrap() as i32
        );
        jobs.push((pid, process_start(pid).unwrap()));
        sessions.push(attached);
    }
    client
        .close(&sessions[0].session_id, sessions[0].input_epoch, true)
        .unwrap();
    wait(&mut client, &sessions[0].session_id, |info| {
        info.state == SessionState::Closed
    });
    assert_ne!(
        process_start(jobs[0].0),
        Some(jobs[0].1),
        "Closed was returned while an ordinary same-session background job remained"
    );
    assert_eq!(process_start(jobs[1].0), Some(jobs[1].1));
    client
        .input(
            &sessions[1].session_id,
            sessions[1].input_epoch,
            b"echo OTHER_\"PROJECT_ALIVE\"\n",
        )
        .unwrap();
    screen(&mut client, &sessions[1].session_id, "OTHER_PROJECT_ALIVE");
    assert!(unrelated.0.try_wait().unwrap().is_none());
    stop(&mut client, &mut host);
    assert_ne!(process_start(jobs[1].0), Some(jobs[1].1));
    assert!(unrelated.0.try_wait().unwrap().is_none());
}

#[test]
fn nested_same_session_jobs_ignoring_hangup_stay_pending_until_explicit_force_and_actual_reaping() {
    let fixture = Fixture::new();
    let project = fixture.project("nested-background", &["echo READY\n"]);
    fs::write(
        fixture.root.join("nested-background.py"),
        r#"import os, signal, time
signal.signal(signal.SIGHUP, signal.SIG_IGN)
signal.signal(signal.SIGTERM, signal.SIG_IGN)
child = os.fork()
if child:
    role = 'parent'
else:
    child = os.fork()
    role = 'middle' if child else 'leaf'
open('nested-' + role + '.pid', 'w').write(str(os.getpid()))
time.sleep(30)
"#,
    )
    .unwrap();
    let (mut host, mut client) = fixture.host();
    let created = client
        .start(
            &project.id,
            &project.terminals[0].id,
            fixture.env.clone(),
            24,
            100,
            false,
        )
        .unwrap();
    let initialized = ready(&mut client, &created.session_id);
    let attached = client.attach(&created.session_id, false).unwrap();
    client
        .input(
            &created.session_id,
            attached.input_epoch,
            b"python3 nested-background.py &\n",
        )
        .unwrap();
    let jobs: Vec<_> = ["parent", "middle", "leaf"]
        .into_iter()
        .map(|role| {
            let pid = fixture_pid(&fixture.root.join(format!("nested-{role}.pid")));
            assert_eq!(
                unsafe { libc::getsid(pid as i32) },
                initialized.child_pid.unwrap() as i32
            );
            (pid, process_start(pid).unwrap())
        })
        .collect();
    client
        .close(&created.session_id, attached.input_epoch, false)
        .unwrap();
    let pending = wait(&mut client, &created.session_id, |info| {
        info.state == SessionState::Closing
            && info.exit.is_some()
            && info
                .error
                .as_ref()
                .is_some_and(|error| error.contains("same-session processes remain"))
    });
    assert!(
        pending.child_pid.is_none(),
        "shell exit is observed separately from job cleanup"
    );
    for (pid, start) in &jobs {
        assert_eq!(process_start(*pid), Some(*start));
    }
    assert!(
        client
            .start(
                &project.id,
                &project.terminals[0].id,
                fixture.env.clone(),
                24,
                100,
                true
            )
            .is_err(),
        "pending descendants must prevent replacement/release"
    );
    client
        .close(&created.session_id, attached.input_epoch, true)
        .unwrap();
    wait(&mut client, &created.session_id, |info| {
        info.state == SessionState::Closed
    });
    for (pid, start) in &jobs {
        assert_ne!(
            process_start(*pid),
            Some(*start),
            "nested PID {pid} was not reaped before Closed"
        );
    }
    stop(&mut client, &mut host);
}
