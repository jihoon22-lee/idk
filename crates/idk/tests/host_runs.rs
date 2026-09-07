#[path = "common/launcher.rs"]
mod launcher;

use base64::Engine;
use idk_workspace::client::Client;
use idk_workspace::model::*;
use idk_workspace::project::{LaunchEnvironment, ProjectService};
use idk_workspace::run_wire::*;
use idk_workspace::store::Store;
use idk_workspace::task::TaskService;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
struct Host(Child);
impl Drop for Host {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}
struct Fixture {
    tmp: tempfile::TempDir,
    store: Store,
    project: String,
    task: String,
    env: BTreeMap<String, String>,
    root: PathBuf,
}
impl Fixture {
    fn new(command: &str, interactive: bool) -> Self {
        Self::with_timeout(command, interactive, None)
    }
    fn with_timeout(command: &str, interactive: bool, timeout_seconds: Option<u64>) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("source");
        std::fs::create_dir(&root).unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let store = Store::open(Some(&tmp.path().join("data"))).unwrap();
        let env = BTreeMap::from([
            ("HOME".into(), home.to_str().unwrap().into()),
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("TERM".into(), "xterm-256color".into()),
            ("PRIVATE_TOKEN".into(), "NEVER_PERSIST_ENV".into()),
        ]);
        let shell = std::env::var_os("IDK_TEST_SHELL")
            .map(PathBuf::from)
            .unwrap_or_else(|| "/usr/bin/tcsh".into());
        assert!(shell.is_file());
        let task = TaskDefinition {
            id: new_id(),
            name: "Build".into(),
            command: command.into(),
            cwd: root.clone(),
            sources: vec![],
            artifact: None,
            approved_digest: None,
            steps: vec![],
            failure_policy: FailurePolicy::Stop,
            logging: if interactive {
                TaskLogging::Disabled
            } else {
                TaskLogging::Raw
            },
            interactive,
            build_outputs: vec![],
            artifact_from_task: None,
            timeout_seconds,
        };
        let project = Project {
            id: new_id(),
            name: "Project".into(),
            root: root.clone(),
            repository: None,
            repository_binding: None,
            related_repositories: vec![],
            default_terminal: None,
            shell: ShellConfig {
                executable: shell,
                login: false,
                init_cwd: root.clone(),
                sources: vec![],
                trusted_digest: None,
            },
            terminals: vec![],
            tasks: vec![task.clone()],
            editor: None,
        };
        let mut workspace = Workspace {
            projects: vec![project.clone()],
            ..Default::default()
        };
        store.save(&mut workspace, 0).unwrap();
        let service = ProjectService { store: &store };
        let environment = LaunchEnvironment::from_variables(env.clone()).unwrap();
        let review = service
            .review_initialization(&project.id, &environment)
            .unwrap();
        service
            .approve_initialization(review.revision, review)
            .unwrap();
        let fixture = Self {
            tmp,
            store,
            project: project.id,
            task: task.id,
            env,
            root,
        };
        fixture.approve();
        fixture
    }
    fn approve(&self) {
        let service = TaskService { store: &self.store };
        let environment = LaunchEnvironment::from_variables(self.env.clone()).unwrap();
        let review = service
            .review(&self.project, &self.task, &environment)
            .unwrap();
        service
            .approve(
                review.revision,
                &self.project,
                &self.task,
                &review.digest,
                &environment,
            )
            .unwrap();
    }
    fn host(&self) -> (Host, Client) {
        let mut host = Host(
            Command::new(launcher::path())
                .args(["__host", "--config-dir"])
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
            if let Ok(client) = Client::connect_with_launcher(&self.store, launcher::path()) {
                return (host, client);
            }
            assert!(host.0.try_wait().unwrap().is_none());
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn start(&self, client: &mut Client, op: &str) -> RunJob {
        client
            .run(RunRequest::Start {
                project_id: self.project.clone(),
                task_id: self.task.clone(),
                operation_id: op.into(),
                environment: self.env.clone(),
                parallel: false,
                rows: 24,
                cols: 100,
            })
            .unwrap()
    }
}
fn result(client: &mut Client, job: RunJob) -> RunResult {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut job = job;
    loop {
        if job.state != RunJobState::Pending {
            assert_eq!(job.state, RunJobState::Complete, "{job:?}");
            return job.result.unwrap();
        }
        assert!(Instant::now() < deadline, "{job:?}");
        std::thread::sleep(Duration::from_millis(10));
        job = client.run(RunRequest::Job { job_id: job.job_id }).unwrap();
    }
}
fn request(client: &mut Client, request: RunRequest) -> RunResult {
    let job = client.run(request).unwrap();
    result(client, job)
}
fn finished(client: &mut Client, id: &str) -> RunInfo {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let RunResult::Run(run) = request(client, RunRequest::Info { run_id: id.into() }) else {
            panic!()
        };
        if !run.state.is_live() && run.cleanup_confirmed {
            return run;
        }
        assert!(Instant::now() < deadline, "{run:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn shutdown(client: &mut Client, host: &mut Host) {
    let preview = client.preview_close(None).unwrap();
    client
        .shutdown(
            &preview
                .targets
                .iter()
                .map(|info| info.session_id.clone())
                .collect::<Vec<_>>(),
            true,
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while host.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn host_run_log_problem_editor_and_repeat_share_actual_result_and_owned_sessions() {
    let f = Fixture::new(
        "echo 'main.cpp:3:2: error: synthetic failure'\n/bin/sh -c 'exit 7'",
        false,
    );
    std::fs::write(f.root.join("main.cpp"), "first\nsecond\nthird\n").unwrap();
    let (mut host, mut client) = f.host();
    let started = f.start(&mut client, &new_id());
    assert_eq!(started.state, RunJobState::Pending);
    assert!(started.session_id.is_some());
    let RunResult::Started(started) = result(&mut client, started) else {
        panic!()
    };
    let run = finished(&mut client, &started.run.run_id);
    assert_eq!(run.state, RunState::Failed);
    assert_eq!(run.exit_code, Some(7));
    assert!(client.list(None).unwrap().is_empty());
    let RunResult::Log(log) = request(
        &mut client,
        RunRequest::Log {
            run_id: run.run_id.clone(),
            generation: run.log.generation,
            offset: 0,
            limit: 65536,
        },
    ) else {
        panic!()
    };
    let text = String::from_utf8(
        base64::engine::general_purpose::STANDARD
            .decode(log.data_base64)
            .unwrap(),
    )
    .unwrap();
    assert!(text.contains("synthetic failure"));
    let RunResult::Problems(problems) = request(
        &mut client,
        RunRequest::Problems {
            run_id: run.run_id.clone(),
        },
    ) else {
        panic!()
    };
    assert_eq!(problems.problems.len(), 1);
    use std::os::unix::fs::PermissionsExt;
    let editor = f.tmp.path().join("editor");
    let marker = f.tmp.path().join("opened");
    std::fs::write(
        &editor,
        format!(
            "#!/usr/bin/python3\nimport json,sys\nopen({:?},'w').write(json.dumps(sys.argv[1:]))\n",
            marker.to_str().unwrap()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&editor, std::fs::Permissions::from_mode(0o700)).unwrap();
    let revision = f.store.load().unwrap().revision;
    request(
        &mut client,
        RunRequest::SaveEditor {
            revision,
            project_id: f.project.clone(),
            config: EditorConfig {
                executable: editor,
                args: vec!["--".into(), "{file}".into(), "{line}".into()],
                external_gui: false,
            },
        },
    );
    let RunResult::EditorReview { review_id, review } = request(
        &mut client,
        RunRequest::EditorReview {
            run_id: run.run_id.clone(),
            problem_id: problems.problems[0].id.clone(),
            log_generation: run.log.generation,
        },
    ) else {
        panic!()
    };
    assert_eq!(review.line, Some(3));
    let RunResult::EditorOpened { session_id } = request(
        &mut client,
        RunRequest::EditorOpen {
            review_id,
            environment: f.env.clone(),
            rows: 24,
            cols: 100,
        },
    ) else {
        panic!()
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let snapshot = client.snapshot(&session_id, None).unwrap();
        if !snapshot.session.state.is_live() {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(std::fs::read_to_string(marker)
        .unwrap()
        .contains("main.cpp"));
    let again = f.start(&mut client, &new_id());
    let RunResult::Started(again) = result(&mut client, again) else {
        panic!()
    };
    assert_ne!(again.run.run_id, run.run_id);
    assert_eq!(finished(&mut client, &again.run.run_id).exit_code, Some(7));
    for name in ["runs.json", "host-sessions.json"] {
        assert!(!std::fs::read_to_string(f.store.state_dir.join(name))
            .unwrap()
            .contains("NEVER_PERSIST_ENV"));
    }
    shutdown(&mut client, &mut host);
}
#[test]
fn duplicate_start_and_generic_close_cancel_the_same_owned_run() {
    let f = Fixture::new("echo ONCE >> starts\nsleep 20", false);
    let (mut host, mut client) = f.host();
    let operation = new_id();
    let job = f.start(&mut client, &operation);
    let RunResult::Started(started) = result(&mut client, job) else {
        panic!()
    };
    let job = f.start(&mut client, &operation);
    let RunResult::Started(duplicate) = result(&mut client, job) else {
        panic!()
    };
    assert!(duplicate.existing);
    assert_eq!(started.run.run_id, duplicate.run.run_id);
    assert_eq!(started.run.session_id, duplicate.run.session_id);
    let session = started.run.session_id.unwrap();
    let attached = client.attach(&session, false).unwrap();
    let preview = client.preview_close(Some(&f.project)).unwrap();
    assert_eq!(preview.run_relations.len(), 1);
    client.close(&session, attached.input_epoch, true).unwrap();
    let run = finished(&mut client, &started.run.run_id);
    assert_eq!(run.state, RunState::Cancelled);
    assert!(run.cancel_requested);
    assert!(run.cleanup_confirmed);
    assert_eq!(
        std::fs::read_to_string(f.root.join("starts")).unwrap(),
        "ONCE\n"
    );
    shutdown(&mut client, &mut host);
}
fn git_binding(f: &Fixture) -> idk_workspace::git::Repository {
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.name", "Fixture"],
        vec!["config", "user.email", "fixture@example.invalid"],
    ] {
        assert!(Command::new("git")
            .current_dir(&f.root)
            .args(args)
            .status()
            .unwrap()
            .success());
    }
    std::fs::write(f.root.join("tracked.cpp"), "source\n").unwrap();
    for args in [
        vec!["add", "."],
        vec!["commit", "-qm", "initial"],
        vec!["branch", "other"],
    ] {
        assert!(Command::new("git")
            .current_dir(&f.root)
            .args(args)
            .status()
            .unwrap()
            .success());
    }
    let repo = idk_workspace::git::Repository::discover(&f.root).unwrap();
    f.store
        .update(|workspace| {
            let project = workspace.project_mut(&f.project)?;
            project.repository = Some(f.root.clone());
            project.repository_binding = Some(repo.clone());
            Ok(())
        })
        .unwrap();
    f.approve();
    repo
}
fn git_job(
    client: &mut Client,
    task: idk_workspace::git_wire::GitTask,
) -> idk_workspace::git_wire::GitJobInfo {
    let mut job = client.git_submit(task).unwrap();
    let deadline = Instant::now() + Duration::from_secs(12);
    while matches!(
        job.state,
        idk_workspace::git_wire::GitJobState::Pending
            | idk_workspace::git_wire::GitJobState::Running
    ) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
        job = client.git_job(&job.id).unwrap();
    }
    job
}
#[test]
fn actual_run_lease_blocks_git_switch_until_owned_cleanup_is_confirmed() {
    use idk_workspace::git_wire::*;
    let f = Fixture::new("sleep 20", false);
    git_binding(&f);
    let (mut host, mut client) = f.host();
    let started = f.start(&mut client, &new_id());
    let RunResult::Started(started) = result(&mut client, started) else {
        panic!()
    };
    let opened = git_job(
        &mut client,
        GitTask::Open {
            project: f.project.clone(),
            repository: None,
            env: f.env.clone(),
        },
    );
    let Some(GitValue::Open(context)) = opened.result else {
        panic!("{opened:?}")
    };
    assert!(context.source_use.provider_ready);
    assert_eq!(context.source_use.runs.len(), 1);
    let status = git_job(
        &mut client,
        GitTask::Status {
            context: context.id.clone(),
            refresh: true,
        },
    );
    let Some(GitValue::Status(status)) = status.result else {
        panic!()
    };
    let planned = git_job(
        &mut client,
        GitTask::Switch {
            context: context.id.clone(),
            snapshot: status.snapshot_id,
            name: "other".into(),
        },
    );
    let Some(GitValue::Plan(plan)) = planned.result else {
        panic!("{planned:?}")
    };
    let operation = client.git_execute(&plan.id, 24, 100).unwrap();
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        let info = client
            .git_operations(None)
            .unwrap()
            .into_iter()
            .find(|info| info.id == operation.id)
            .unwrap();
        if info.state == GitOperationState::Complete {
            assert_ne!(
                info.result.map(|result| result.outcome),
                Some(GitOutcome::Succeeded)
            );
            break;
        }
        assert!(Instant::now() < deadline, "{info:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
    request(
        &mut client,
        RunRequest::Cancel {
            run_id: started.run.run_id.clone(),
            force: true,
        },
    );
    assert_eq!(
        finished(&mut client, &started.run.run_id).state,
        RunState::Cancelled
    );
    let status = git_job(
        &mut client,
        GitTask::Status {
            context: context.id.clone(),
            refresh: true,
        },
    );
    let Some(GitValue::Status(status)) = status.result else {
        panic!()
    };
    assert!(status.source_use.runs.is_empty());
    let planned = git_job(
        &mut client,
        GitTask::Switch {
            context: context.id,
            snapshot: status.snapshot_id,
            name: "other".into(),
        },
    );
    let Some(GitValue::Plan(plan)) = planned.result else {
        panic!("{planned:?}")
    };
    let operation = client.git_execute(&plan.id, 24, 100).unwrap();
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        let info = client
            .git_operations(None)
            .unwrap()
            .into_iter()
            .find(|info| info.id == operation.id)
            .unwrap();
        if info.state == GitOperationState::Complete {
            assert_eq!(info.result.unwrap().outcome, GitOutcome::Succeeded);
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    shutdown(&mut client, &mut host);
}
#[test]
fn unknown_git_blocks_run_until_exact_durable_reconciliation_without_claiming_success() {
    use idk_workspace::git_wire::*;
    let f = Fixture::new("echo reviewed", false);
    let repository = git_binding(&f);
    let operation = new_id();
    f.store.write_state("git-operations.json",&serde_json::json!({"schema":1,"operations":[{"id":operation,"context_id":new_id(),"project_id":f.project,"repository":repository,"kind":"switch_branch","state":"running","outcome":null,"exit_code":null,"commit":null}]})).unwrap();
    let (mut host, mut client) = f.host();
    let mut job = f.start(&mut client, &new_id());
    let deadline = Instant::now() + Duration::from_secs(10);
    while job.state == RunJobState::Pending {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
        job = client.run(RunRequest::Job { job_id: job.job_id }).unwrap();
    }
    assert_eq!(job.state, RunJobState::Failed);
    assert!(client
        .git_operations(None)
        .unwrap()
        .iter()
        .any(|info| info.id == operation
            && info.state == GitOperationState::Unknown
            && !info.cleanup_acknowledged));
    let mut wrong = repository.clone();
    wrong.root = f.tmp.path().into();
    assert!(client.git_reconcile(&operation, wrong).is_err());
    let reconciled = client.git_reconcile(&operation, repository).unwrap();
    assert_eq!(reconciled.state, GitOperationState::Unknown);
    assert!(reconciled.cleanup_acknowledged);
    assert!(reconciled.result.is_none());
    let job = f.start(&mut client, &new_id());
    let RunResult::Started(started) = result(&mut client, job) else {
        panic!()
    };
    assert_eq!(
        finished(&mut client, &started.run.run_id).state,
        RunState::Succeeded
    );
    shutdown(&mut client, &mut host);
}
#[test]
fn interactive_task_keeps_input_private_across_client_reconnect() {
    let f = Fixture::new(
        "echo INPUT_READY\nset response = $<\necho RECEIVED\nsleep 1",
        true,
    );
    let (mut host, mut client) = f.host();
    let job = f.start(&mut client, &new_id());
    let RunResult::Started(started) = result(&mut client, job) else {
        panic!()
    };
    let session = started.run.session_id.clone().unwrap();
    let first = client.attach(&session, false).unwrap();
    client.detach(&session, first.input_epoch).unwrap();
    let mut other = Client::connect_with_launcher(&f.store, launcher::path()).unwrap();
    let attached = other.attach(&session, false).unwrap();
    assert!(client
        .input(&session, first.input_epoch, b"WRONG\n")
        .is_err());
    other
        .input(&session, attached.input_epoch, b"SECRET_RESPONSE\n")
        .unwrap();
    let run = finished(&mut other, &started.run.run_id);
    assert_eq!(run.state, RunState::Succeeded);
    assert_eq!(run.log.state, LogState::Disabled);
    for name in ["runs.json", "host-sessions.json"] {
        let data = std::fs::read_to_string(f.store.state_dir.join(name)).unwrap();
        assert!(!data.contains("SECRET_RESPONSE"));
    }
    assert!(!f
        .store
        .state_dir
        .join("run-logs")
        .join(format!("{}.log", run.run_id))
        .exists());
    shutdown(&mut other, &mut host);
}
#[test]
fn pending_start_cancel_never_executes_the_registered_command() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new("echo SHOULD_NOT_EXIST > executed", false);
    let real = f
        .store
        .load()
        .unwrap()
        .project(&f.project)
        .unwrap()
        .shell
        .executable
        .clone();
    let wrapper = f.tmp.path().join("slow-shell");
    std::fs::write(
        &wrapper,
        format!("#!/bin/sh\nsleep 0.3\nexec '{}' \"$@\"\n", real.display()),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    f.store
        .update(|workspace| {
            workspace.project_mut(&f.project)?.shell.executable = wrapper.clone();
            Ok(())
        })
        .unwrap();
    let service = ProjectService { store: &f.store };
    let review = service
        .review_initialization(
            &f.project,
            &LaunchEnvironment::from_variables(f.env.clone()).unwrap(),
        )
        .unwrap();
    service
        .approve_initialization(review.revision, review)
        .unwrap();
    f.approve();
    let (mut host, mut client) = f.host();
    let review = client
        .run(RunRequest::ReviewTask {
            project_id: f.project.clone(),
            task_id: f.task.clone(),
            environment: f.env.clone(),
        })
        .unwrap();
    let start = f.start(&mut client, &new_id());
    client
        .run(RunRequest::CancelJob {
            job_id: start.job_id.clone(),
        })
        .unwrap();
    result(&mut client, review);
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let job = client
            .run(RunRequest::Job {
                job_id: start.job_id.clone(),
            })
            .unwrap();
        if job.state != RunJobState::Pending {
            assert_eq!(job.state, RunJobState::Cancelled, "{job:?}");
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!f.root.join("executed").exists());
    shutdown(&mut client, &mut host);
}
#[test]
fn failed_cancel_persistence_cannot_signal_owned_descendants_before_retry_succeeds() {
    let f = Fixture::new(
        "python3 child.py &\nwhile (! -e child-ready)\n sleep 0.02\nend\nexit 0",
        false,
    );
    std::fs::write(f.root.join("child.py"),"import signal,time,pathlib\nsignal.signal(signal.SIGHUP,signal.SIG_IGN)\npathlib.Path('child-ready').write_text('ready')\nfor i in range(1000):\n pathlib.Path('heartbeat').write_text(str(i))\n time.sleep(.02)\n").unwrap();
    let (mut host, mut client) = f.host();
    let job = f.start(&mut client, &new_id());
    let RunResult::Started(started) = result(&mut client, job) else {
        panic!()
    };
    let session = started.run.session_id.clone().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !f.root.join("heartbeat").exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    let ledger = f.store.state_dir.join("runs.json");
    let backup = f.store.state_dir.join("runs.saved");
    std::fs::rename(&ledger, &backup).unwrap();
    std::fs::create_dir(&ledger).unwrap();
    let attached = client.attach(&session, false).unwrap();
    client.close(&session, attached.input_epoch, true).unwrap();
    let before = std::fs::read_to_string(f.root.join("heartbeat")).unwrap();
    std::thread::sleep(Duration::from_millis(650));
    let after = std::fs::read_to_string(f.root.join("heartbeat")).unwrap();
    assert_ne!(
        before, after,
        "descendant was signalled despite failed durable cancellation"
    );
    let screen = client.snapshot(&session, None).unwrap();
    assert!(screen.session.state.is_live());
    std::fs::remove_dir(&ledger).unwrap();
    std::fs::rename(&backup, &ledger).unwrap();
    let run = finished(&mut client, &started.run.run_id);
    assert_eq!(run.state, RunState::Cancelled);
    assert!(run.cleanup_confirmed);
    shutdown(&mut client, &mut host);
}
#[test]
fn timeout_cancels_owned_run_and_persists_cleanup() {
    timeout_cleanup(false);
}
#[test]
fn failed_timeout_persistence_retries_automatically_after_storage_recovers() {
    timeout_cleanup(true);
}
fn timeout_cleanup(storage_fault: bool) {
    // Bounded even if the host/test fails; heartbeat proves the task continues
    // until cancellation is durable, rather than merely trusting a Run label.
    let f = Fixture::with_timeout(
        "set tick = 0\nwhile ($tick < 600)\n echo tick >> heartbeat\n @ tick++\n sleep 0.05\nend",
        false,
        Some(3),
    );
    let (mut host, mut client) = f.host();
    let job = f.start(&mut client, &new_id());
    let RunResult::Started(started) = result(&mut client, job) else {
        panic!()
    };
    let session = started.run.session_id.clone().unwrap();
    let ledger = f.store.state_dir.join("runs.json");
    if storage_fault {
        let backup = f.store.state_dir.join("runs.saved");
        std::fs::rename(&ledger, &backup).unwrap();
        std::fs::create_dir(&ledger).unwrap();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&backup).unwrap()).unwrap();
        assert_eq!(saved["runs"][0]["state"], "Running");
        assert_eq!(saved["runs"][0]["cancel_requested"], false);
        // Wait for the actual failed timeout write, not an assumed timer delay.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let snapshot = client.snapshot(&session, None).unwrap();
            if snapshot.session.error.as_deref() == Some("destination must be a regular owned file")
            {
                assert!(snapshot.session.state.is_live());
                break;
            }
            assert!(
                Instant::now() < deadline,
                "timeout fault not observed: {snapshot:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let heartbeat = f.root.join("heartbeat");
        let before = std::fs::metadata(&heartbeat).unwrap().len();
        let deadline = Instant::now() + Duration::from_secs(2);
        while std::fs::metadata(&heartbeat).unwrap().len() == before {
            assert!(
                Instant::now() < deadline,
                "task stopped before durable cancellation"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        std::fs::remove_dir(&ledger).unwrap();
        std::fs::rename(&backup, &ledger).unwrap();
    }
    // No explicit cancel/close request: timeout recovery must finish on its own.
    let run = finished(&mut client, &started.run.run_id);
    assert_eq!(run.state, RunState::Cancelled);
    assert!(run.timeout_requested && run.cancel_requested && run.cleanup_confirmed);
    assert!(!client
        .snapshot(&session, None)
        .unwrap()
        .session
        .state
        .is_live());
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&ledger).unwrap()).unwrap();
    assert_eq!(saved["runs"][0]["state"], "Cancelled");
    assert_eq!(saved["runs"][0]["timeout_requested"], true);
    assert_eq!(saved["runs"][0]["cleanup_confirmed"], true);
    shutdown(&mut client, &mut host);
}
#[test]
fn failed_finalization_keeps_shutdown_pending_and_retries_after_storage_recovers() {
    // The task cannot finish until the storage fault is installed, even if the
    // test thread is descheduled after observing the start result.
    let f = Fixture::new(
        "while (! -e allow-finish)\n sleep 0.02\nend\necho DONE",
        false,
    );
    let (mut host, mut client) = f.host();
    let job = f.start(&mut client, &new_id());
    let RunResult::Started(started) = result(&mut client, job) else {
        panic!()
    };
    let ledger = f.store.state_dir.join("runs.json");
    let backup = f.store.state_dir.join("runs.saved");
    std::fs::rename(&ledger, &backup).unwrap();
    std::fs::create_dir(&ledger).unwrap();
    let before: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&backup).unwrap()).unwrap();
    assert_eq!(before["runs"][0]["state"], "Running");
    assert_eq!(before["runs"][0]["cleanup_confirmed"], false);
    std::fs::write(f.root.join("allow-finish"), "ready").unwrap();
    let session = started.run.session_id.unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let snapshot = client.snapshot(&session, None).unwrap();
        if !snapshot.session.state.is_live() && snapshot.session.error.is_some() {
            assert_eq!(
                snapshot.session.error.as_deref(),
                Some("destination must be a regular owned file")
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "finalization fault was not observed: {snapshot:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let preview = client.preview_close(None).unwrap();
    assert!(preview.targets.is_empty());
    client.shutdown(&[], false).unwrap();
    std::thread::sleep(Duration::from_millis(400));
    assert!(
        host.0.try_wait().unwrap().is_none(),
        "host left before durable finalization recovered"
    );
    std::fs::remove_dir(&ledger).unwrap();
    std::fs::rename(&backup, &ledger).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while host.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    let ledger: serde_json::Value =
        serde_json::from_slice(&std::fs::read(ledger).unwrap()).unwrap();
    assert_eq!(ledger["runs"][0]["state"], "Succeeded");
    assert_eq!(ledger["runs"][0]["cleanup_confirmed"], true);
    assert_eq!(ledger["runs"][0]["exit_code"], 0);
}

#[test]
fn repository_registered_after_host_start_becomes_ready_without_starting_a_task() {
    use idk_workspace::git_wire::*;
    let f = Fixture::new("echo task", false);
    let (mut host, mut client) = f.host();
    git_binding(&f);
    let opened = git_job(
        &mut client,
        GitTask::Open {
            project: f.project.clone(),
            repository: None,
            env: f.env.clone(),
        },
    );
    let Some(GitValue::Open(context)) = opened.result else {
        panic!("{opened:?}")
    };
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        let status = git_job(
            &mut client,
            GitTask::Status {
                context: context.id.clone(),
                refresh: true,
            },
        );
        let Some(GitValue::Status(status)) = status.result else {
            panic!("{status:?}")
        };
        if status.source_use.provider_ready {
            assert!(status.source_use.runs.is_empty());
            break;
        }
        assert!(
            Instant::now() < deadline,
            "newly registered repository remained unknown despite a healthy run provider"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let RunResult::Runs(runs) = request(
        &mut client,
        RunRequest::List {
            project_id: Some(f.project.clone()),
        },
    ) else {
        panic!()
    };
    assert!(runs.is_empty());
    shutdown(&mut client, &mut host);
}

#[test]
fn captured_raw_run_rejects_typed_secret_without_changing_input_or_log() {
    let f = Fixture::new(
        "echo WAITING_RAW_INPUT\nset response = $<\necho \"$response\" > received",
        false,
    );
    let (mut host, mut client) = f.host();
    let job = f.start(&mut client, &new_id());
    let RunResult::Started(started) = result(&mut client, job) else {
        panic!()
    };
    let session = started.run.session_id.clone().unwrap();
    let attached = client.attach(&session, false).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let before = loop {
        let RunResult::Log(log) = request(
            &mut client,
            RunRequest::Log {
                run_id: started.run.run_id.clone(),
                generation: 1,
                offset: 0,
                limit: 65536,
            },
        ) else {
            panic!()
        };
        let raw = base64::engine::general_purpose::STANDARD
            .decode(log.data_base64)
            .unwrap();
        if String::from_utf8_lossy(&raw).contains("WAITING_RAW_INPUT") {
            break raw;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    };
    let error = client
        .input(&session, attached.input_epoch, b"RAW_SECRET_SENTINEL\n")
        .expect_err("captured Run accepted keyboard input");
    assert!(error.to_string().contains("read-only"));
    std::thread::sleep(Duration::from_millis(100));
    assert!(!f.root.join("received").exists());
    let screen = client.snapshot(&session, None).unwrap().screen.unwrap();
    assert!(!screen.text().contains("RAW_SECRET_SENTINEL"));
    let RunResult::Log(after) = request(
        &mut client,
        RunRequest::Log {
            run_id: started.run.run_id.clone(),
            generation: 1,
            offset: 0,
            limit: 65536,
        },
    ) else {
        panic!()
    };
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(after.data_base64)
            .unwrap(),
        before
    );
    request(
        &mut client,
        RunRequest::Cancel {
            run_id: started.run.run_id.clone(),
            force: true,
        },
    );
    assert_eq!(
        finished(&mut client, &started.run.run_id).state,
        RunState::Cancelled
    );
    shutdown(&mut client, &mut host);
}
