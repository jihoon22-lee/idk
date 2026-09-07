use idk_workspace::client::Client;
use idk_workspace::git_wire::*;
use idk_workspace::model::{new_id, Project, ShellConfig};
use idk_workspace::project::{ConnectDraft, LaunchEnvironment, ProjectService, TerminalDraft};
use idk_workspace::store::Store;
use std::collections::BTreeMap;
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
        let root = tmp.path().join("fixtures");
        let home = tmp.path().join("home");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&home).unwrap();
        let store = Store::open(Some(&tmp.path().join("data"))).unwrap();
        let shell = std::env::var_os("IDK_TEST_SHELL")
            .map(PathBuf::from)
            .unwrap_or_else(|| "/usr/bin/tcsh".into());
        assert!(shell.is_file(), "provide actual tcsh via IDK_TEST_SHELL");
        let env = BTreeMap::from([
            ("HOME".into(), home.to_str().unwrap().into()),
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("TERM".into(), "xterm-256color".into()),
            ("LANG".into(), "C.UTF-8".into()),
        ]);
        Self {
            _tmp: tmp,
            store,
            root,
            env,
            shell,
        }
    }
    fn git(&self, root: &Path, args: &[&str]) -> Output {
        let output = Command::new("/usr/bin/git")
            .args(args)
            .current_dir(root)
            .env_clear()
            .envs(&self.env)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "Git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }
    fn project(&self, name: &str) -> Project {
        let root = self.root.join(name);
        let external = self.root.join(format!("{name}-external-test"));
        fs::create_dir(&root).unwrap();
        fs::create_dir(&external).unwrap();
        self.git(&root, &["init", "--quiet"]);
        self.git(&root, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        self.git(&root, &["config", "user.name", "Synthetic Author"]);
        self.git(&root, &["config", "user.email", "fixture@example.invalid"]);
        fs::write(root.join("base"), "base\n").unwrap();
        self.git(&root, &["add", "--", "base"]);
        self.git(&root, &["commit", "--quiet", "-m", "initial"]);
        let service = ProjectService { store: &self.store };
        let preview = service
            .preview_connect(ConnectDraft {
                name: name.into(),
                root,
                shell: ShellConfig {
                    executable: self.shell.clone(),
                    login: false,
                    init_cwd: external.clone(),
                    sources: Vec::new(),
                    trusted_digest: None,
                },
                terminals: vec![TerminalDraft {
                    name: "External test shell".into(),
                    cwd: external,
                    sources: Vec::new(),
                    persistent: true,
                }],
            })
            .unwrap();
        let project = service.create(preview.revision, preview).unwrap();
        let review = service
            .review_initialization(
                &project.id,
                &LaunchEnvironment::from_variables(self.env.clone()).unwrap(),
            )
            .unwrap();
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
    fn host(&self) -> (OwnedHost, Client) {
        let mut host = OwnedHost(
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
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Ok(client) = Client::connect_with_launcher(&self.store, binary()) {
                return (host, client);
            }
            assert!(
                host.0.try_wait().unwrap().is_none(),
                "host exited during startup"
            );
            assert!(Instant::now() < deadline, "host startup deadline");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    fn open(
        &self,
        client: &mut Client,
        project: &Project,
        env: BTreeMap<String, String>,
    ) -> GitContextInfo {
        match job(
            client,
            GitTask::Open {
                project: project.id.clone(),
                repository: None,
                env,
            },
        ) {
            GitValue::Open(value) => value,
            other => panic!("unexpected {other:?}"),
        }
    }
}
fn binary() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_idk"))
}
fn wait_job(client: &mut Client, id: &str) -> GitJobInfo {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let info = client.git_job(id).unwrap();
        if matches!(info.state, GitJobState::Ready | GitJobState::Failed) {
            return info;
        }
        assert!(Instant::now() < deadline, "Git job timeout {info:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn job(client: &mut Client, task: GitTask) -> GitValue {
    let queued = client.git_submit(task).unwrap();
    let info = wait_job(client, &queued.id);
    assert_eq!(info.state, GitJobState::Ready, "{info:?}");
    info.result.unwrap()
}
fn status(client: &mut Client, context: &str) -> GitStatusReply {
    match job(
        client,
        GitTask::Status {
            context: context.into(),
            refresh: true,
        },
    ) {
        GitValue::Status(value) => value,
        other => panic!("unexpected {other:?}"),
    }
}
fn plan(client: &mut Client, task: GitTask) -> GitOperationPreview {
    match job(client, task) {
        GitValue::Plan(value) => value,
        other => panic!("unexpected {other:?}"),
    }
}
fn complete(client: &mut Client, id: &str) -> GitOperationInfo {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let info = client
            .git_operations(None)
            .unwrap()
            .into_iter()
            .find(|info| info.id == id)
            .unwrap();
        if matches!(
            info.state,
            GitOperationState::Complete | GitOperationState::Unknown
        ) {
            return info;
        }
        assert!(Instant::now() < deadline, "Git operation timeout {info:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn execute(client: &mut Client, plan: &GitOperationPreview) -> GitOperationInfo {
    let started = client.git_execute(&plan.id, 24, 100).unwrap();
    complete(client, &started.id)
}
fn wait_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !path.exists() {
        assert!(Instant::now() < deadline, "missing {}", path.display());
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn hook(root: &Path, contents: &str) {
    let path = root.join(".git/hooks/pre-commit");
    fs::write(&path, contents).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
fn shutdown(client: &mut Client, host: &mut OwnedHost) {
    let ids: Vec<_> = client
        .preview_close(None)
        .unwrap()
        .targets
        .into_iter()
        .map(|info| info.session_id)
        .collect();
    client.shutdown(&ids, true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    while host.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "host did not shut down");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn repository_tokens_reject_cross_context_selection_and_refresh_authentication_without_rerouting_git(
) {
    let fixture = Fixture::new();
    let first = fixture.project("first");
    let second = fixture.project("second");
    fs::write(first.root.join("same.txt"), "first change\n").unwrap();
    fs::write(second.root.join("same.txt"), "second change\n").unwrap();
    let (mut host, mut client) = fixture.host();
    let mut environment = fixture.env.clone();
    environment.insert(
        "SSH_AUTH_SOCK".into(),
        "/synthetic/old-agent-reference".into(),
    );
    environment.insert(
        "GIT_DIR".into(),
        second.root.join(".git").to_str().unwrap().into(),
    );
    environment.insert(
        "GIT_INDEX_FILE".into(),
        second.root.join(".git/index").to_str().unwrap().into(),
    );
    let a = fixture.open(&mut client, &first, environment.clone());
    let b = fixture.open(&mut client, &second, fixture.env.clone());
    assert_eq!(a.repository.root, first.root);
    assert!(a.source_use.provider_ready);
    let snapshot = status(&mut client, &a.id);
    assert!(client
        .git_submit(GitTask::Stage {
            context: b.id.clone(),
            snapshot: snapshot.snapshot_id.clone(),
            entries: vec![0]
        })
        .is_err());
    let stale = plan(
        &mut client,
        GitTask::Stage {
            context: a.id.clone(),
            snapshot: snapshot.snapshot_id,
            entries: vec![0],
        },
    );
    environment.insert(
        "SSH_AUTH_SOCK".into(),
        "/synthetic/new-agent-reference".into(),
    );
    let current = fixture.open(&mut client, &first, environment);
    assert!(client.git_execute(&stale.id, 24, 100).is_err());
    assert!(client
        .git_submit(GitTask::Status {
            context: a.id,
            refresh: true
        })
        .is_err());
    let snapshot = status(&mut client, &current.id);
    let stage = plan(
        &mut client,
        GitTask::Stage {
            context: current.id.clone(),
            snapshot: snapshot.snapshot_id,
            entries: vec![0],
        },
    );
    assert_eq!(
        execute(&mut client, &stage).result.unwrap().outcome,
        GitOutcome::Succeeded
    );
    assert_eq!(
        String::from_utf8(
            fixture
                .git(&first.root, &["diff", "--cached", "--name-only"])
                .stdout
        )
        .unwrap()
        .trim(),
        "same.txt"
    );
    assert!(fixture
        .git(&second.root, &["diff", "--cached", "--name-only"])
        .stdout
        .is_empty());
    hook(
        &first.root,
        "#!/bin/sh\nprintf '%s' \"$SSH_AUTH_SOCK\" > agent-reference-used\n",
    );
    let review = match job(
        &mut client,
        GitTask::CommitReview {
            context: current.id.clone(),
        },
    ) {
        GitValue::CommitReview(value) => value,
        _ => unreachable!(),
    };
    let commit = plan(
        &mut client,
        GitTask::Commit {
            context: current.id,
            review: review.review_id,
            message: "use current frontend references".into(),
        },
    );
    let result = execute(&mut client, &commit);
    assert_eq!(result.result.unwrap().outcome, GitOutcome::Succeeded);
    assert_eq!(
        fs::read_to_string(first.root.join("agent-reference-used")).unwrap(),
        "/synthetic/new-agent-reference"
    );
    let ledger = fs::read_to_string(fixture.store.state_dir.join("git-operations.json")).unwrap();
    assert!(
        !ledger.contains("old-agent-reference")
            && !ledger.contains("new-agent-reference")
            && !ledger.contains("SSH_AUTH_SOCK")
    );
    shutdown(&mut client, &mut host);
}

#[test]
fn immutable_plan_executes_once_and_durable_failure_prevents_spawning_before_the_record() {
    let fixture = Fixture::new();
    let project = fixture.project("durable");
    fs::write(project.root.join("changed"), "changed\n").unwrap();
    let (mut host, mut client) = fixture.host();
    let context = fixture.open(&mut client, &project, fixture.env.clone());
    let snapshot = status(&mut client, &context.id);
    let stage = plan(
        &mut client,
        GitTask::Stage {
            context: context.id.clone(),
            snapshot: snapshot.snapshot_id,
            entries: vec![0],
        },
    );
    let saved_state = fixture.root.join("saved-state");
    fs::rename(&fixture.store.state_dir, &saved_state).unwrap();
    fs::write(
        &fixture.store.state_dir,
        "synthetic unavailable state directory",
    )
    .unwrap();
    let refused = client.git_execute(&stage.id, 24, 100);
    fs::remove_file(&fixture.store.state_dir).unwrap();
    fs::rename(saved_state, &fixture.store.state_dir).unwrap();
    assert!(refused.is_err());
    assert!(fixture
        .git(&project.root, &["diff", "--cached", "--name-only"])
        .stdout
        .is_empty());
    assert_eq!(
        execute(&mut client, &stage).result.unwrap().outcome,
        GitOutcome::Succeeded
    );
    let review = match job(
        &mut client,
        GitTask::CommitReview {
            context: context.id.clone(),
        },
    ) {
        GitValue::CommitReview(value) => value,
        _ => unreachable!(),
    };
    let commit = plan(
        &mut client,
        GitTask::Commit {
            context: context.id,
            review: review.review_id,
            message: "exactly one explicit commit".into(),
        },
    );
    let accepted = client.git_execute(&commit.id, 24, 100).unwrap();
    assert_eq!(
        client.git_execute(&commit.id, 24, 100).unwrap().id,
        accepted.id
    );
    assert_eq!(
        complete(&mut client, &accepted.id).result.unwrap().outcome,
        GitOutcome::Succeeded
    );
    assert_eq!(
        client.git_execute(&commit.id, 24, 100).unwrap().id,
        accepted.id
    );
    assert_eq!(
        String::from_utf8(
            fixture
                .git(&project.root, &["rev-list", "--count", "HEAD"])
                .stdout
        )
        .unwrap()
        .trim(),
        "2"
    );
    assert!(client.git_execute(&new_id(), 24, 100).is_err());
    shutdown(&mut client, &mut host);
    let (mut replacement, mut client) = fixture.host();
    let historical = client.git_execute(&commit.id, 24, 100).unwrap();
    assert_eq!(historical.state, GitOperationState::Complete);
    assert_eq!(
        String::from_utf8(
            fixture
                .git(&project.root, &["rev-list", "--count", "HEAD"])
                .stdout
        )
        .unwrap()
        .trim(),
        "2"
    );
    shutdown(&mut client, &mut replacement);
}

#[test]
fn hook_cancellation_retains_repository_single_flight_and_stale_owners_cannot_signal_it() {
    let fixture = Fixture::new();
    let project = fixture.project("cancel");
    let other = fixture.project("independent");
    fs::write(project.root.join("changed"), "changed\n").unwrap();
    fixture.git(&project.root, &["add", "--", "changed"]);
    hook(
        &project.root,
        "#!/bin/sh\ntrap '' HUP TERM\nprintf ready > hook-ready\nsleep 30\n",
    );
    let (mut host, mut client) = fixture.host();
    let context = fixture.open(&mut client, &project, fixture.env.clone());
    let other_context = fixture.open(&mut client, &other, fixture.env.clone());
    let review = match job(
        &mut client,
        GitTask::CommitReview {
            context: context.id.clone(),
        },
    ) {
        GitValue::CommitReview(value) => value,
        _ => unreachable!(),
    };
    let commit = plan(
        &mut client,
        GitTask::Commit {
            context: context.id.clone(),
            review: review.review_id,
            message: "cancel waiting hook".into(),
        },
    );
    let operation = client.git_execute(&commit.id, 24, 100).unwrap();
    let owned = client.git_operation_attach(&operation.id, false).unwrap();
    wait_file(&project.root.join("hook-ready"));
    assert!(client.preview_close(None).is_err());
    assert!(client.shutdown(&[], true).is_err());
    let queued = client
        .git_submit(GitTask::Status {
            context: context.id,
            refresh: true,
        })
        .unwrap();
    assert_eq!(
        client.git_job(&queued.id).unwrap().state,
        GitJobState::Pending
    );
    assert!(
        status(&mut client, &other_context.id).snapshot.clean(),
        "a different repository read was blocked by the hook"
    );
    let mut takeover = Client::connect_with_launcher(&fixture.store, binary()).unwrap();
    assert!(takeover.git_operation_attach(&operation.id, false).is_err());
    let new_owner = takeover.git_operation_attach(&operation.id, true).unwrap();
    assert!(new_owner.input_epoch > owned.input_epoch);
    assert!(client
        .git_operation_cancel(&operation.id, owned.input_epoch, true)
        .is_err());
    assert!(client
        .git_operation_input(&operation.id, owned.input_epoch, b"stale input\n")
        .is_err());
    takeover
        .git_operation_cancel(&operation.id, new_owner.input_epoch, false)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let info = takeover
            .git_operations(None)
            .unwrap()
            .into_iter()
            .find(|info| info.id == operation.id)
            .unwrap();
        if info.state == GitOperationState::Cancelling
            && info
                .error
                .as_ref()
                .is_some_and(|error| error.contains("same-session processes remain"))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "hook cleanup was not pending: {info:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        client.git_job(&queued.id).unwrap().state,
        GitJobState::Pending,
        "repository read ran before owned descendants completed"
    );
    takeover
        .git_operation_cancel(&operation.id, new_owner.input_epoch, true)
        .unwrap();
    let completed = complete(&mut takeover, &operation.id);
    assert_eq!(completed.result.unwrap().outcome, GitOutcome::Cancelled);
    assert_eq!(wait_job(&mut client, &queued.id).state, GitJobState::Ready);
    assert_eq!(
        String::from_utf8(
            fixture
                .git(&project.root, &["rev-list", "--count", "HEAD"])
                .stdout
        )
        .unwrap()
        .trim(),
        "1"
    );
    shutdown(&mut takeover, &mut host);
}

#[test]
fn host_crash_restores_unknown_without_replaying_git_or_persisting_terminal_content() {
    let fixture = Fixture::new();
    let project = fixture.project("crash");
    fs::write(project.root.join("changed"), "changed\n").unwrap();
    fixture.git(&project.root, &["add", "--", "changed"]);
    // A finite failing hook makes the crash fixture self-cleaning even if it outlives
    // its host. The replacement host must neither adopt its PID nor rerun it.
    hook(
        &project.root,
        "#!/bin/sh\nprintf x >> hook-invocations\nprintf 'synthetic-authentication-transcript\\n'\nprintf ready > hook-ready\nsleep 2\nexit 1\n",
    );
    let (mut host, mut client) = fixture.host();
    let context = fixture.open(&mut client, &project, fixture.env.clone());
    let review = match job(
        &mut client,
        GitTask::CommitReview {
            context: context.id.clone(),
        },
    ) {
        GitValue::CommitReview(value) => value,
        _ => unreachable!(),
    };
    let commit = plan(
        &mut client,
        GitTask::Commit {
            context: context.id,
            review: review.review_id,
            message: "synthetic-private-commit-draft".into(),
        },
    );
    let operation = client.git_execute(&commit.id, 24, 100).unwrap();
    wait_file(&project.root.join("hook-ready"));
    host.0.kill().unwrap();
    host.0.wait().unwrap();
    drop(client);
    let (mut replacement, mut client) = fixture.host();
    let recovered = client.git_execute(&commit.id, 24, 100).unwrap();
    assert_eq!(recovered.id, operation.id);
    assert_eq!(recovered.state, GitOperationState::Unknown);
    assert!(!recovered.terminal_available);
    assert!(recovered.result.is_none());
    let historical = client.git_operation_attach(&operation.id, true).unwrap();
    assert!(historical.owner.is_none());
    assert!(!historical.terminal_available);
    assert!(client
        .git_operation_input(&operation.id, historical.input_epoch, b"no replay\n")
        .is_err());
    assert_eq!(
        client
            .git_operation_cancel(&operation.id, recovered.input_epoch, true)
            .unwrap()
            .state,
        GitOperationState::Unknown
    );
    let context = fixture.open(&mut client, &project, fixture.env.clone());
    assert!(!context.source_use.provider_ready);
    std::thread::sleep(Duration::from_millis(2200));
    assert_eq!(
        fs::read_to_string(project.root.join("hook-invocations")).unwrap(),
        "x"
    );
    assert_eq!(
        String::from_utf8(
            fixture
                .git(&project.root, &["rev-list", "--count", "HEAD"])
                .stdout
        )
        .unwrap()
        .trim(),
        "1"
    );
    let ledger = fs::read_to_string(fixture.store.state_dir.join("git-operations.json")).unwrap();
    assert!(!ledger.contains("synthetic-authentication-transcript"));
    assert!(!ledger.contains("synthetic-private-commit-draft"));
    assert!(!ledger.contains("hook-invocations"));
    shutdown(&mut client, &mut replacement);
}
