use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use idk_workspace::{
    client::Client,
    git_wire::{GitOperationKind, GitOperationState},
    model::ShellConfig,
    project::{ConnectDraft, LaunchEnvironment, ProjectService, TerminalDraft},
    store::Store,
    ui::{App, UiOutcome},
};
use ratatui::{backend::TestBackend, text::Line, Terminal};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

struct Fixture {
    temporary: tempfile::TempDir,
    store: Store,
    root: PathBuf,
    related: PathBuf,
    project: String,
    environment: LaunchEnvironment,
}
impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("primary");
        let related = temporary.path().join("related");
        let home = temporary.path().join("home");
        for path in [&root, &related, &home] {
            fs::create_dir(path).unwrap();
        }
        for path in [&root, &related] {
            git(path, &["init", "-b", "main"]);
            git(path, &["config", "user.name", "UI Fixture"]);
            git(path, &["config", "user.email", "ui@example.invalid"]);
            fs::write(path.join("base"), "base\n").unwrap();
            git(path, &["add", "base"]);
            git(path, &["commit", "-m", "initial"]);
        }
        let environment = LaunchEnvironment::from_variables(BTreeMap::from([
            ("HOME".into(), home.to_string_lossy().into()),
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("TERM".into(), "xterm-256color".into()),
            ("LANG".into(), "C.UTF-8".into()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ]))
        .unwrap();
        let shell = std::env::var_os("IDK_TEST_SHELL")
            .map(PathBuf::from)
            .unwrap_or_else(|| "/usr/bin/tcsh".into());
        assert!(shell.is_file(), "set IDK_TEST_SHELL to a real tcsh");
        let store = Store::open(Some(&temporary.path().join("data"))).unwrap();
        let service = ProjectService { store: &store };
        let preview = service
            .preview_connect(ConnectDraft {
                name: "Git UI fixture".into(),
                root: root.clone(),
                shell: ShellConfig {
                    executable: shell,
                    login: false,
                    init_cwd: root.clone(),
                    sources: vec![],
                    trusted_digest: None,
                },
                terminals: vec![TerminalDraft {
                    name: "Outside tests".into(),
                    cwd: related.clone(),
                    sources: vec![],
                    persistent: true,
                }],
            })
            .unwrap();
        let project = service.create(preview.revision, preview).unwrap().id;
        let workspace = store.load().unwrap();
        service
            .bind_repository(workspace.revision, &project, related.clone(), false)
            .unwrap();
        Self {
            temporary,
            store,
            root,
            related,
            project,
            environment,
        }
    }
    fn app(&self) -> App<'_> {
        let mut app = App::with_environment(&self.store, self.environment.clone()).unwrap();
        app.enable_terminal_runtime(PathBuf::from(env!("CARGO_BIN_EXE_idk")))
            .unwrap();
        key(&mut app, KeyCode::Char('2'));
        wait(&mut app, "Primary Git");
        wait(&mut app, "main ·");
        app
    }
    fn client(&self) -> Client {
        Client::connect_with_launcher(&self.store, Path::new(env!("CARGO_BIN_EXE_idk"))).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Ok(mut client) =
            Client::connect_with_launcher(&self.store, Path::new(env!("CARGO_BIN_EXE_idk")))
        {
            if let Ok(operations) = client.git_operations(None) {
                for operation in operations {
                    if matches!(
                        operation.state,
                        GitOperationState::Pending
                            | GitOperationState::Running
                            | GitOperationState::Cancelling
                    ) {
                        if let Ok(owned) = client.git_operation_attach(&operation.id, true) {
                            let _ =
                                client.git_operation_cancel(&operation.id, owned.input_epoch, true);
                        }
                    }
                }
            }
            if let Ok(preview) = client.preview_close(None) {
                let ids: Vec<_> = preview
                    .targets
                    .into_iter()
                    .map(|session| session.session_id)
                    .collect();
                let _ = client.shutdown(&ids, true);
            }
        }
    }
}
fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("/usr/bin/git")
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "Git fixture command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}
fn key(app: &mut App<'_>, code: KeyCode) {
    event(app, Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
}
fn control(app: &mut App<'_>, character: char) {
    event(
        app,
        Event::Key(KeyEvent::new(
            KeyCode::Char(character),
            KeyModifiers::CONTROL,
        )),
    );
}
fn event(app: &mut App<'_>, event: Event) {
    let outcome = app.handle_event(event).unwrap();
    if matches!(
        outcome,
        UiOutcome::ForwardTerminalKey(_) | UiOutcome::ForwardTerminalPaste(_)
    ) {
        app.forward_terminal(outcome).unwrap();
    }
}
fn screen(app: &mut App<'_>) -> String {
    let mut terminal = Terminal::new(TestBackend::new(120, 34)).unwrap();
    terminal.draw(|frame| app.render(frame)).unwrap();
    let buffer = terminal.backend().buffer();
    (0..34)
        .map(|row| {
            let mut line = String::new();
            let mut col = 0;
            while col < 120 {
                let text = buffer[(col, row)].symbol();
                line.push_str(text);
                col += Line::from(text).width().max(1) as u16;
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}
fn wait(app: &mut App<'_>, needle: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        app.tick();
        let text = screen(app);
        if text.contains(needle) {
            return;
        }
        assert!(Instant::now() < deadline, "missing {needle:?}: {text}");
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn quiet(app: &mut App<'_>) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        app.tick();
        let text = screen(app);
        if !text.contains("working…") && !text.contains("Reading Git status…") {
            return;
        }
        assert!(Instant::now() < deadline, "Git did not settle: {text}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn done(app: &mut App<'_>, kind: &str) {
    wait(app, &format!("Git {kind}"));
    wait(app, "Complete");
    control(app, 'g');
    key(app, KeyCode::Char('z'));
    wait(app, "Changes");
    quiet(app); // Returning from an operation requests a fresh HEAD/index snapshot.
}

#[test]
fn git_ui_keeps_primary_target_stages_literal_names_and_commits_entire_actual_index() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("--한글\nfile"), "literal\n").unwrap();
    fs::write(fixture.root.join("base"), "pre-staged elsewhere\n").unwrap();
    git(&fixture.root, &["add", "base"]);
    fs::write(fixture.related.join("unrelated"), "must remain unstaged\n").unwrap();
    let mut app = fixture.app();
    assert_eq!(
        app.selected_terminal_definition().unwrap().cwd,
        fixture.related
    );
    let text = screen(&mut app);
    assert!(text.contains("Primary Git"));
    assert!(text.contains("--한글\\nfile"));
    assert!(!text.contains("unrelated"));
    key(&mut app, KeyCode::End);
    key(&mut app, KeyCode::Char('s'));
    done(&mut app, "Stage");
    let staged = git(&fixture.root, &["diff", "--cached", "--name-only", "-z"]);
    assert!(staged.contains("--한글\nfile\0"));
    assert!(staged.contains("base\0"));
    assert!(git(&fixture.related, &["diff", "--cached", "--name-only"]).is_empty());
    quiet(&mut app);
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Char('t'));
    wait(&mut app, "literal");
    key(&mut app, KeyCode::Char('z'));
    quiet(&mut app);
    key(&mut app, KeyCode::Home);
    key(&mut app, KeyCode::Char('u'));
    done(&mut app, "Unstage");
    assert!(
        !git(&fixture.root, &["diff", "--cached", "--name-only", "-z"]).contains("--한글\nfile\0")
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("--한글\nfile")).unwrap(),
        "literal\n"
    );
    quiet(&mut app);
    key(&mut app, KeyCode::End);
    key(&mut app, KeyCode::Char('s'));
    done(&mut app, "Stage");
    key(&mut app, KeyCode::Char('c'));
    event(
        &mut app,
        Event::Paste("한글 commit\n\nBody stays literal".into()),
    );
    key(&mut app, KeyCode::F(2));
    wait(&mut app, "Commit all staged changes");
    let review = screen(&mut app);
    assert!(review.contains("--한글\\nfile"));
    assert!(review.contains("base"));
    assert!(review.contains("Body stays literal"));
    key(&mut app, KeyCode::F(2));
    done(&mut app, "Commit");
    assert_eq!(
        git(&fixture.root, &["log", "-1", "--format=%B"]).trim(),
        "한글 commit\n\nBody stays literal"
    );
    key(&mut app, KeyCode::Char('h'));
    wait(&mut app, "한글 commit");
    key(&mut app, KeyCode::Enter);
    wait(&mut app, "--한글\\nfile");
    key(&mut app, KeyCode::Char('v'));
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);
    wait(&mut app, "Related Git");
    wait(&mut app, "unrelated");
    assert_eq!(
        fixture.store.load().unwrap().projects[0].repository,
        Some(fixture.root.clone())
    );
}

#[test]
fn changed_index_invalidates_commit_review_without_losing_multiline_draft() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("base"), "reviewed\n").unwrap();
    git(&fixture.root, &["add", "base"]);
    let mut app = fixture.app();
    key(&mut app, KeyCode::Char('c'));
    event(
        &mut app,
        Event::Paste("Keep this draft\n\nafter stale review".into()),
    );
    key(&mut app, KeyCode::F(2));
    wait(&mut app, "Commit all staged changes");
    fs::write(fixture.root.join("external"), "external stage\n").unwrap();
    git(&fixture.root, &["add", "external"]);
    key(&mut app, KeyCode::F(2));
    wait(&mut app, "changed");
    assert_eq!(
        git(&fixture.root, &["rev-list", "--count", "HEAD"]).trim(),
        "1"
    );
    key(&mut app, KeyCode::Char('c'));
    assert!(screen(&mut app).contains("after stale review"));
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Char('c'));
    assert!(screen(&mut app).contains("Keep this draft"));
}

fn run_request(
    client: &mut Client,
    request: idk_workspace::run_wire::RunRequest,
) -> idk_workspace::run_wire::RunResult {
    use idk_workspace::run_wire::*;
    let mut job = client.run(request).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while job.state == RunJobState::Pending {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
        job = client.run(RunRequest::Job { job_id: job.job_id }).unwrap();
    }
    assert_eq!(job.state, RunJobState::Complete, "{:?}", job.error);
    job.result.unwrap()
}
fn start_source_user(fixture: &Fixture) -> String {
    use idk_workspace::{model::*, run_wire::*, task::TaskService};
    let service = ProjectService {
        store: &fixture.store,
    };
    let review = service
        .review_initialization(&fixture.project, &fixture.environment)
        .unwrap();
    service
        .approve_initialization(review.revision, review)
        .unwrap();
    let task = TaskDefinition {
        id: new_id(),
        name: "Registered source user".into(),
        command: "sleep 120".into(),
        cwd: fixture.root.clone(),
        sources: vec![],
        artifact: None,
        approved_digest: None,
        steps: vec![],
        failure_policy: FailurePolicy::Stop,
        logging: TaskLogging::Raw,
        interactive: false,
        build_outputs: vec![],
        artifact_from_task: None,
        timeout_seconds: None,
    };
    let service = TaskService {
        store: &fixture.store,
    };
    service
        .save(
            fixture.store.load().unwrap().revision,
            &fixture.project,
            task.clone(),
        )
        .unwrap();
    let review = service
        .review(&fixture.project, &task.id, &fixture.environment)
        .unwrap();
    service
        .approve(
            review.revision,
            &fixture.project,
            &task.id,
            &review.digest,
            &fixture.environment,
        )
        .unwrap();
    let mut client = fixture.client();
    match run_request(
        &mut client,
        RunRequest::Start {
            project_id: fixture.project.clone(),
            task_id: task.id,
            operation_id: new_id(),
            environment: fixture.environment.variables().clone(),
            parallel: false,
            rows: 24,
            cols: 100,
        },
    ) {
        RunResult::Started(reply) => reply.run.run_id,
        _ => panic!("expected registered Run"),
    }
}
fn stop_source_user(fixture: &Fixture, run_id: &str) {
    use idk_workspace::run_wire::*;
    let mut client = fixture.client();
    run_request(
        &mut client,
        RunRequest::Cancel {
            run_id: run_id.into(),
            force: true,
        },
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let RunResult::Run(run) = run_request(
            &mut client,
            RunRequest::Info {
                run_id: run_id.into(),
            },
        ) else {
            panic!("expected Run")
        };
        if run.cleanup_confirmed {
            assert_eq!(run.state, RunState::Cancelled);
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn fetch_and_push_stay_explicit_while_registered_run_blocks_pull_and_switch_until_cleanup() {
    let fixture = Fixture::new();
    let remote = fixture.temporary.path().join("remote.git");
    fs::create_dir(&remote).unwrap();
    git(&remote, &["init", "--bare"]);
    git(
        &fixture.root,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&fixture.root, &["push", "origin", "main"]);
    let mut app = fixture.app();
    assert!(!fixture.root.join(".git/FETCH_HEAD").exists());
    let run_id = start_source_user(&fixture);
    key(&mut app, KeyCode::F(5));
    wait(&mut app, "builds using worktree");
    key(&mut app, KeyCode::Char('r'));
    wait(&mut app, "origin");
    key(&mut app, KeyCode::Char('f'));
    wait(&mut app, "Review Git action");
    assert!(!fixture.root.join(".git/FETCH_HEAD").exists());
    key(&mut app, KeyCode::Esc);
    assert!(!fixture.root.join(".git/FETCH_HEAD").exists());
    key(&mut app, KeyCode::Char('f'));
    wait(&mut app, "Review Git action");
    key(&mut app, KeyCode::F(2));
    done(&mut app, "Fetch");
    assert!(fixture.root.join(".git/FETCH_HEAD").is_file());
    let remote_before = git(&remote, &["rev-parse", "refs/heads/main"]);
    fs::write(fixture.root.join("base"), "push new commit\n").unwrap();
    git(&fixture.root, &["add", "base"]);
    git(&fixture.root, &["commit", "-m", "explicit push"]);
    key(&mut app, KeyCode::F(5));
    quiet(&mut app);
    key(&mut app, KeyCode::Char('r'));
    wait(&mut app, "origin");
    key(&mut app, KeyCode::Char('P'));
    event(&mut app, Event::Paste("main".into()));
    key(&mut app, KeyCode::F(2));
    wait(&mut app, "Review Git action");
    assert_eq!(
        git(&remote, &["rev-parse", "refs/heads/main"]),
        remote_before
    );
    key(&mut app, KeyCode::F(2));
    done(&mut app, "Push");
    assert_eq!(
        git(&remote, &["rev-parse", "refs/heads/main"]),
        git(&fixture.root, &["rev-parse", "HEAD"])
    );
    let remote_work = fixture.temporary.path().join("remote-work");
    git(
        fixture.temporary.path(),
        &[
            "clone",
            "-b",
            "main",
            remote.to_str().unwrap(),
            remote_work.to_str().unwrap(),
        ],
    );
    git(&remote_work, &["config", "user.name", "Remote Fixture"]);
    git(
        &remote_work,
        &["config", "user.email", "remote@example.invalid"],
    );
    fs::write(remote_work.join("remote-change"), "new remote bytes\n").unwrap();
    git(&remote_work, &["add", "remote-change"]);
    git(&remote_work, &["commit", "-m", "remote advance"]);
    git(&remote_work, &["push", "origin", "main"]);
    quiet(&mut app);
    key(&mut app, KeyCode::Char('r'));
    wait(&mut app, "origin");
    key(&mut app, KeyCode::Char('p'));
    event(&mut app, Event::Paste("main".into()));
    key(&mut app, KeyCode::F(2));
    wait(&mut app, "Review Git action");
    let fetched_before = fs::read(fixture.root.join(".git/FETCH_HEAD")).unwrap();
    key(&mut app, KeyCode::F(2));
    done(&mut app, "Fast-forward pull");
    assert_eq!(
        fs::read(fixture.root.join(".git/FETCH_HEAD")).unwrap(),
        fetched_before,
        "a registered Run must block pull before remote contact"
    );
    key(&mut app, KeyCode::Char('b'));
    wait(&mut app, "Branches");
    key(&mut app, KeyCode::Char('n'));
    event(&mut app, Event::Paste("topic".into()));
    key(&mut app, KeyCode::F(2));
    done(&mut app, "Create branch");
    assert_eq!(
        git(&fixture.root, &["branch", "--show-current"]).trim(),
        "main"
    );
    assert!(git(&fixture.root, &["branch", "--list", "topic"]).contains("topic"));
    key(&mut app, KeyCode::Char('b'));
    wait(&mut app, "topic");
    key(&mut app, KeyCode::End);
    key(&mut app, KeyCode::Enter);
    wait(&mut app, "Review Git action");
    key(&mut app, KeyCode::F(2));
    done(&mut app, "Switch branch");
    assert_eq!(
        git(&fixture.root, &["branch", "--show-current"]).trim(),
        "main"
    );
    stop_source_user(&fixture, &run_id);
    key(&mut app, KeyCode::F(5));
    quiet(&mut app);
    wait(&mut app, "shared worktree");
    key(&mut app, KeyCode::Char('b'));
    wait(&mut app, "topic");
    key(&mut app, KeyCode::End);
    key(&mut app, KeyCode::Enter);
    wait(&mut app, "Review Git action");
    key(&mut app, KeyCode::F(2));
    done(&mut app, "Switch branch");
    assert_eq!(
        git(&fixture.root, &["branch", "--show-current"]).trim(),
        "topic"
    );
    key(&mut app, KeyCode::Char('r'));
    wait(&mut app, "origin");
    key(&mut app, KeyCode::Char('p'));
    event(&mut app, Event::Paste("main".into()));
    key(&mut app, KeyCode::F(2));
    wait(&mut app, "Review Git action");
    key(&mut app, KeyCode::F(2));
    done(&mut app, "Fast-forward pull");
    assert_eq!(
        git(&fixture.root, &["rev-parse", "HEAD"]),
        git(&remote, &["rev-parse", "refs/heads/main"])
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("remote-change")).unwrap(),
        "new remote bytes\n"
    );
    let mut client = fixture.client();
    let operations = client.git_operations(Some(&fixture.project)).unwrap();
    assert!(operations
        .iter()
        .any(|operation| operation.kind == GitOperationKind::Fetch));
}

#[test]
fn actual_ui_uses_dedicated_hook_tty_keeps_secrets_out_of_results_and_preserves_busy_shell() {
    use idk_workspace::terminal::TerminalSession;
    use portable_pty::CommandBuilder;
    use std::os::unix::fs::PermissionsExt;
    fn wait_tty(terminal: &TerminalSession, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let screen = terminal.snapshot().unwrap().text();
            if screen.contains(needle) {
                return;
            }
            assert!(Instant::now() < deadline, "missing {needle:?}: {screen}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let fixture = Fixture::new();
    let hook = fixture.root.join(".git/hooks/pre-commit");
    fs::write(&hook,"#!/bin/sh\nexec 3<>/dev/tty\nsaved=$(stty -g <&3)\ntrap 'stty \"$saved\" <&3 2>/dev/null || :; exit 129' HUP INT TERM\nstty -echo <&3\nprintf 'HOOK_TTY_READY\\n' >&3\nIFS= read -r answer <&3\nstty \"$saved\" <&3\n[ \"$answer\" = 'private-hook-answer' ] || exit 47\nexit 23\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(fixture.root.join("base"), "staged\n").unwrap();
    git(&fixture.root, &["add", "base"]);
    let service = ProjectService {
        store: &fixture.store,
    };
    let review = service
        .review_initialization(&fixture.project, &fixture.environment)
        .unwrap();
    service
        .approve_initialization(review.revision, review)
        .unwrap();
    let definition = fixture.store.load().unwrap().projects[0].terminals[0]
        .id
        .clone();
    let mut client =
        Client::ensure_host(&fixture.store, Path::new(env!("CARGO_BIN_EXE_idk"))).unwrap();
    let shell = client
        .start(
            &fixture.project,
            &definition,
            fixture.environment.variables().clone(),
            32,
            120,
            false,
        )
        .unwrap();
    let owned = client.attach(&shell.session_id, false).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let snapshot = client.snapshot(&shell.session_id, None).unwrap();
        if snapshot.session.state == idk_workspace::protocol::SessionState::Running
            && snapshot.screen.is_some()
        {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    client
        .input(
            &shell.session_id,
            owned.input_epoch,
            b"echo SHELL_BUSY; sleep 120\r",
        )
        .unwrap();
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_idk"));
    command.arg("--data-dir");
    command.arg(fixture.temporary.path().join("data"));
    command.cwd(&fixture.related);
    command.env_clear();
    for (key, value) in fixture.environment.variables() {
        command.env(key, value);
    }
    let mut ui = TerminalSession::spawn(command, 34, 120, 1000).unwrap();
    wait_tty(&ui, "Terminal definitions");
    ui.input(b"\r").unwrap();
    wait_tty(&ui, "Take over terminal input");
    ui.input(b"\x1bOQ").unwrap();
    wait_tty(&ui, "SHELL_BUSY");
    ui.input(b"\x072").unwrap();
    wait_tty(&ui, "Primary Git");
    wait_tty(&ui, "main ·");
    ui.input(b"c").unwrap();
    wait_tty(&ui, "Commit message");
    ui.input(b"Keep hook draft\x1bOQ").unwrap();
    wait_tty(&ui, "Commit all staged changes");
    ui.input(b"\x1bOQ").unwrap();
    wait_tty(&ui, "HOOK_TTY_READY");
    ui.input(b"private-hook-answer\r").unwrap();
    wait_tty(&ui, "Complete");
    assert!(!ui
        .snapshot()
        .unwrap()
        .text()
        .contains("private-hook-answer"));
    let operations = client.git_operations(Some(&fixture.project)).unwrap();
    let operation = operations
        .iter()
        .find(|operation| operation.kind == GitOperationKind::Commit)
        .unwrap();
    assert_eq!(
        operation.result.as_ref().unwrap().outcome,
        idk_workspace::git_wire::GitOutcome::Failed
    );
    let result = serde_json::to_string(operation).unwrap();
    assert!(!result.contains("private-hook-answer"));
    assert!(!result.contains("HOOK_TTY_READY"));
    let shell_screen = client
        .snapshot(&shell.session_id, None)
        .unwrap()
        .screen
        .unwrap()
        .text();
    assert!(!shell_screen.contains("private-hook-answer"));
    assert!(!shell_screen.contains("HOOK_TTY_READY"));
    assert_eq!(
        git(&fixture.root, &["rev-list", "--count", "HEAD"]).trim(),
        "1"
    );
    ui.input(b"\x07zc").unwrap();
    wait_tty(&ui, "Keep hook draft");
    ui.input(b"\x1bOQ").unwrap();
    wait_tty(&ui, "Commit all staged changes");
    ui.input(b"\x1bOQ").unwrap();
    wait_tty(&ui, "HOOK_TTY_READY");
    ui.input(b"\x07k").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let operations = client.git_operations(Some(&fixture.project)).unwrap();
        if operations.iter().any(|operation| {
            operation.result.as_ref().is_some_and(|result| {
                result.outcome == idk_workspace::git_wire::GitOutcome::Cancelled
            })
        }) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Git cancellation never completed"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    ui.input(b"q").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(exit) = ui.try_wait().unwrap() {
            assert_eq!(exit.code, 0);
            break;
        }
        assert!(Instant::now() < deadline, "UI failed to detach");
        std::thread::sleep(Duration::from_millis(20));
    }
}
