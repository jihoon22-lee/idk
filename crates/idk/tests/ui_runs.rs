use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use idk_workspace::{
    client::Client,
    model::*,
    project::{LaunchEnvironment, ProjectService},
    run_wire::*,
    store::Store,
    task::TaskService,
    ui::{App, UiOutcome},
};
use ratatui::{backend::TestBackend, text::Line, Terminal};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
#[derive(Default, Debug)]
struct KeyboardEvidence {
    navigation_actions: usize,
    editing_keys: usize,
    paste_events: usize,
    pasted_chars: usize,
    sequence: Vec<String>,
}
thread_local! { static KEYBOARD_EVIDENCE:std::cell::RefCell<Option<KeyboardEvidence>>=const {std::cell::RefCell::new(None)}; }
fn begin_keyboard_evidence() {
    KEYBOARD_EVIDENCE.with(|e| *e.borrow_mut() = Some(KeyboardEvidence::default()));
}
fn finish_keyboard_evidence() {
    KEYBOARD_EVIDENCE.with(|e| {
        eprintln!(
            "KEYBOARD_FLOW task-edit-to-editor {:?}",
            e.borrow_mut().take().unwrap()
        )
    });
}
fn record_keyboard_event(event: &Event) {
    KEYBOARD_EVIDENCE.with(|e| {
        if let Some(evidence) = e.borrow_mut().as_mut() {
            match event {
                Event::Key(key) => {
                    if key.code == KeyCode::Char('u')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        evidence.editing_keys += 1;
                    } else {
                        evidence.navigation_actions += 1;
                        evidence
                            .sequence
                            .push(format!("{:?}+{:?}", key.modifiers, key.code));
                    }
                }
                Event::Paste(text) => {
                    evidence.paste_events += 1;
                    evidence.pasted_chars += text.chars().count();
                }
                _ => {}
            }
        }
    });
}

struct Fixture {
    _tmp: tempfile::TempDir,
    store: Store,
    project: String,
    task: String,
    root: PathBuf,
    env: LaunchEnvironment,
}
impl Fixture {
    fn new(command: &str, interactive: bool) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("source space");
        let home = tmp.path().join("home");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&home).unwrap();
        let store = Store::open(Some(&tmp.path().join("data"))).unwrap();
        let env = LaunchEnvironment::from_variables(BTreeMap::from([
            ("HOME".into(), home.to_string_lossy().into_owned()),
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("TERM".into(), "xterm-256color".into()),
        ]))
        .unwrap();
        let shell = std::env::var_os("IDK_TEST_SHELL")
            .map(PathBuf::from)
            .unwrap_or_else(|| "/usr/bin/tcsh".into());
        assert!(shell.is_file());
        let init = root.join("init.csh");
        fs::write(&init, "echo initialized >> init-count\n").unwrap();
        let task = TaskDefinition {
            id: new_id(),
            name: "Build task".into(),
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
            timeout_seconds: None,
        };
        let project = Project {
            id: new_id(),
            name: "Run UI fixture".into(),
            root: root.clone(),
            repository: None,
            repository_binding: None,
            related_repositories: vec![],
            default_terminal: None,
            shell: ShellConfig {
                executable: shell,
                login: false,
                init_cwd: root.clone(),
                sources: vec![SourceSpec::from(init)],
                trusted_digest: None,
            },
            terminals: vec![],
            tasks: vec![task.clone()],
            editor: None,
        };
        let mut workspace = Workspace {
            projects: vec![project.clone()],
            selected_project: Some(project.id.clone()),
            ..Default::default()
        };
        store.save(&mut workspace, 0).unwrap();
        let service = ProjectService { store: &store };
        let review = service.review_initialization(&project.id, &env).unwrap();
        service
            .approve_initialization(review.revision, review)
            .unwrap();
        let service = TaskService { store: &store };
        let review = service.review(&project.id, &task.id, &env).unwrap();
        service
            .approve(review.revision, &project.id, &task.id, &review.digest, &env)
            .unwrap();
        Self {
            _tmp: tmp,
            store,
            project: project.id,
            task: task.id,
            root,
            env,
        }
    }
    fn app(&self) -> App<'_> {
        let mut app = App::with_environment(&self.store, self.env.clone()).unwrap();
        app.enable_terminal_runtime(PathBuf::from(env!("CARGO_BIN_EXE_idk")))
            .unwrap();
        key(&mut app, KeyCode::Char('3'));
        wait(&mut app, "Saved revision");
        app
    }
    fn client(&self) -> Client {
        Client::connect_with_launcher(&self.store, Path::new(env!("CARGO_BIN_EXE_idk"))).unwrap()
    }
    fn runs(&self) -> Vec<RunInfo> {
        let mut client = self.client();
        match complete(
            &mut client,
            RunRequest::List {
                project_id: Some(self.project.clone()),
            },
        ) {
            RunResult::Runs(r) => r,
            _ => panic!("wrong list"),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Ok(mut client) =
            Client::connect_with_launcher(&self.store, Path::new(env!("CARGO_BIN_EXE_idk")))
        {
            if let Ok(preview) = client.preview_close(None) {
                let ids = preview
                    .targets
                    .into_iter()
                    .map(|s| s.session_id)
                    .collect::<Vec<_>>();
                let _ = client.shutdown(&ids, true);
            }
        }
    }
}
fn complete(client: &mut Client, request: RunRequest) -> RunResult {
    let mut job = client.run(request).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while job.state == RunJobState::Pending {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
        job = client.run(RunRequest::Job { job_id: job.job_id }).unwrap();
    }
    assert_eq!(job.state, RunJobState::Complete, "{:?}", job.error);
    job.result.unwrap()
}
fn key(app: &mut App<'_>, code: KeyCode) {
    event(app, Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
}
fn ctrl(app: &mut App<'_>, character: char) {
    event(
        app,
        Event::Key(KeyEvent::new(
            KeyCode::Char(character),
            KeyModifiers::CONTROL,
        )),
    );
}
fn event(app: &mut App<'_>, event: Event) {
    record_keyboard_event(&event);
    let result = app.handle_event(event).unwrap();
    if matches!(
        result,
        UiOutcome::ForwardTerminalKey(_) | UiOutcome::ForwardTerminalPaste(_)
    ) {
        app.forward_terminal(result).unwrap();
    }
}
fn screen(app: &mut App<'_>) -> String {
    let mut terminal = Terminal::new(TestBackend::new(140, 42)).unwrap();
    terminal.draw(|frame| app.render(frame)).unwrap();
    let b = terminal.backend().buffer();
    (0..42)
        .map(|row| {
            let mut s = String::new();
            let mut col = 0;
            while col < 140 {
                let text = b[(col, row)].symbol();
                s.push_str(text);
                col += Line::from(text).width().max(1) as u16;
            }
            s
        })
        .collect::<Vec<_>>()
        .join("\n")
}
fn wait(app: &mut App<'_>, needle: &str) {
    let end = Instant::now() + Duration::from_secs(20);
    loop {
        app.tick();
        let s = screen(app);
        if s.contains(needle) {
            return;
        }
        assert!(Instant::now() < end, "missing {needle:?}:\n{s}");
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn pump(app: &mut App<'_>, duration: Duration) {
    let end = Instant::now() + duration;
    while Instant::now() < end {
        app.tick();
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn start(app: &mut App<'_>) {
    key(app, KeyCode::Enter);
    wait(app, "Review task");
    wait(app, "F2 Start reviewed task");
    key(app, KeyCode::F(2));
}
fn controls(app: &mut App<'_>) {
    if !screen(app).contains("1 Terminal") {
        ctrl(app, 'g');
    }
}

#[test]
fn task_draft_approval_failed_result_log_search_problem_and_literal_editor() {
    let f = Fixture::new(
        "echo 'source.c:1:1: error: build example'\necho next-line\nexit 7",
        false,
    );
    fs::write(f.root.join("source.c"), "int main(void) {}\n").unwrap();
    let editor = f.root.join("editor fixture");
    fs::write(
        &editor,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > editor-arguments\n",
    )
    .unwrap();
    fs::set_permissions(&editor, fs::Permissions::from_mode(0o700)).unwrap();
    let mut workspace = f.store.load().unwrap();
    let revision = workspace.revision;
    workspace.projects[0].editor = Some(EditorConfig {
        executable: editor,
        args: vec!["+{line}".into(), "--".into(), "{file}".into()],
        external_gui: false,
    });
    f.store.save(&mut workspace, revision).unwrap();
    begin_keyboard_evidence();
    let mut app = f.app();
    key(&mut app, KeyCode::Char('e'));
    wait(&mut app, "Task draft");
    ctrl(&mut app, 'u');
    event(&mut app, Event::Paste("Changed task".into()));
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Char('e'));
    wait(&mut app, "Changed task");
    key(&mut app, KeyCode::F(2));
    wait(&mut app, "Definition saved");
    assert!(!f.root.join("init-count").exists());
    key(&mut app, KeyCode::Enter);
    wait(&mut app, "F2 Approve task");
    key(&mut app, KeyCode::F(2));
    wait(&mut app, "Task approved");
    assert!(!f.root.join("init-count").exists());
    start(&mut app);
    wait(&mut app, "Run registered");
    pump(&mut app, Duration::from_millis(300));
    controls(&mut app);
    key(&mut app, KeyCode::Char('4'));
    wait(&mut app, "Failed");
    wait(&mut app, "cleanup confirmed true");
    key(&mut app, KeyCode::Char('l'));
    wait(&mut app, "build example");
    assert!(screen(&mut app).contains("Raw log generation"));
    key(&mut app, KeyCode::Char('/'));
    event(&mut app, Event::Paste("next-line".into()));
    key(&mut app, KeyCode::Enter);
    wait(&mut app, "byte");
    wait(&mut app, "next-line");
    key(&mut app, KeyCode::Enter);
    wait(&mut app, "Raw log generation");
    key(&mut app, KeyCode::Char('p'));
    wait(&mut app, "Error source.c");
    wait(&mut app, "build example");
    key(&mut app, KeyCode::Enter);
    wait(&mut app, "Review editor launch");
    wait(&mut app, "argv[1] = --");
    assert!(!f.root.join("editor-arguments").exists());
    key(&mut app, KeyCode::F(2));
    let end = Instant::now() + Duration::from_secs(15);
    while !f.root.join("editor-arguments").exists() {
        app.tick();
        assert!(Instant::now() < end, "{}", screen(&mut app));
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        fs::read_to_string(f.root.join("editor-arguments")).unwrap(),
        format!("+1\n--\n{}\n", f.root.join("source.c").display())
    );
    assert_eq!(
        fs::read_to_string(f.root.join("init-count"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    let runs = f.runs();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].state, RunState::Failed);
    assert_eq!(runs[0].exit_code, Some(7));
    assert!(runs[0].cleanup_confirmed);
    assert_eq!(runs[0].task_id, f.task);
    finish_keyboard_evidence();
}

#[test]
fn interactive_run_duplicate_start_ui_detach_reattach_and_explicit_cancel() {
    let f = Fixture::new(
        "echo READY-FOR-RUN\nset answer=\"$<\"\necho \"$answer\" > answer\nsleep 120",
        true,
    );
    let mut app = f.app();
    start(&mut app);
    wait(&mut app, "READY-FOR-RUN");
    event(&mut app, Event::Paste("literal run input".into()));
    key(&mut app, KeyCode::Enter);
    let end = Instant::now() + Duration::from_secs(10);
    while !f.root.join("answer").exists() {
        app.tick();
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        fs::read_to_string(f.root.join("answer")).unwrap().trim(),
        "literal run input"
    );
    controls(&mut app);
    key(&mut app, KeyCode::Char('3'));
    wait(&mut app, "Saved revision");
    start(&mut app);
    wait(&mut app, "Existing Run reattached");
    assert_eq!(f.runs().len(), 1);
    assert_eq!(
        fs::read_to_string(f.root.join("init-count"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    let run_id = f.runs()[0].run_id.clone();
    let mut workspace = f.store.load().unwrap();
    let revision = workspace.revision;
    workspace.projects.clear();
    workspace.selected_project = None;
    workspace.recent_projects.clear();
    f.store.save(&mut workspace, revision).unwrap();
    drop(app);
    let mut app = App::with_environment(&f.store, f.env.clone()).unwrap();
    app.enable_terminal_runtime(PathBuf::from(env!("CARGO_BIN_EXE_idk")))
        .unwrap();
    app.attach_run(&run_id, false).unwrap();
    wait(&mut app, "READY-FOR-RUN");
    controls(&mut app);
    key(&mut app, KeyCode::F(9));
    wait(&mut app, "Running");
    key(&mut app, KeyCode::Enter);
    wait(&mut app, "Run results");
    key(&mut app, KeyCode::Char('k'));
    wait(&mut app, "Cancel Run");
    key(&mut app, KeyCode::Esc);
    pump(&mut app, Duration::from_millis(200));
    assert_eq!(f.runs()[0].state, RunState::Running);
    key(&mut app, KeyCode::Char('k'));
    wait(&mut app, "Cancel Run");
    key(&mut app, KeyCode::F(2));
    wait(&mut app, "Cancelled");
    wait(&mut app, "cleanup confirmed true");
    assert_eq!(
        fs::read_to_string(f.root.join("init-count"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert_eq!(f.runs()[0].log.state, LogState::Disabled);
}

#[test]
fn new_task_form_preserves_literal_source_arguments_and_starts_selected_ordered_steps() {
    let f = Fixture::new("echo WRONG > wrong-task", false);
    let source = f.root.join("task source.csh");
    fs::write(&source, "echo \"$argv[1]\" > source-argument\n").unwrap();
    let mut app = f.app();
    key(&mut app, KeyCode::Char('n'));
    wait(&mut app, "Task draft");
    ctrl(&mut app, 'u');
    event(&mut app, Event::Paste("Ordered UI task".into()));
    key(&mut app, KeyCode::F(7));
    key(&mut app, KeyCode::Tab);
    event(&mut app, Event::Paste("echo FIRST > ordered-output".into()));
    key(&mut app, KeyCode::F(7));
    key(&mut app, KeyCode::Tab);
    event(
        &mut app,
        Event::Paste("echo SECOND >> ordered-output".into()),
    );
    key(&mut app, KeyCode::F(6));
    event(
        &mut app,
        Event::Paste(source.to_string_lossy().into_owned()),
    );
    key(&mut app, KeyCode::Tab);
    event(&mut app, Event::Paste("literal ; $HOME".into()));
    key(&mut app, KeyCode::F(2));
    wait(&mut app, "Definition saved");
    assert_eq!(f.store.load().unwrap().projects[0].tasks.len(), 2);
    assert!(!f.root.join("ordered-output").exists());
    key(&mut app, KeyCode::Enter);
    wait(&mut app, "Review task");
    wait(&mut app, "Name Ordered UI task");
    key(&mut app, KeyCode::F(2));
    wait(&mut app, "Task approved");
    start(&mut app);
    wait(&mut app, "Run registered");
    pump(&mut app, Duration::from_millis(300));
    controls(&mut app);
    key(&mut app, KeyCode::Char('4'));
    wait(&mut app, "Succeeded");
    wait(&mut app, "cleanup confirmed true");
    let runs = f.runs();
    assert_eq!(
        runs[0].name,
        "Ordered UI task",
        "recorded Runs: {runs:?}; saved tasks: {:?}",
        f.store.load().unwrap().projects[0].tasks
    );
    assert_eq!(
        fs::read_to_string(f.root.join("ordered-output")).unwrap(),
        "FIRST\nSECOND\n"
    );
    assert_eq!(
        fs::read_to_string(f.root.join("source-argument")).unwrap(),
        "literal ; $HOME\n"
    );
    assert!(!f.root.join("wrong-task").exists());
    let runs = f.runs();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].name, "Ordered UI task");
    assert_eq!(runs[0].steps.len(), 2);
    assert_eq!(runs[0].exit_code, Some(0));
}

#[test]
fn captured_run_consumes_keys_and_paste_without_recording_them_and_can_be_cancelled() {
    use base64::Engine;
    let f = Fixture::new("echo RAW-READY\nsleep 120", false);
    let mut app = f.app();
    start(&mut app);
    wait(&mut app, "RAW-READY");
    wait(&mut app, "Captured Run is READ ONLY");
    assert_eq!(
        app.handle_event(Event::Paste("SECRET-NOT-SENT".into()))
            .unwrap(),
        UiOutcome::Continue
    );
    assert_eq!(
        app.handle_event(Event::Key(KeyEvent::new(
            KeyCode::Char('s'),
            KeyModifiers::NONE
        )))
        .unwrap(),
        UiOutcome::Continue
    );
    assert!(app
        .forward_terminal(UiOutcome::ForwardTerminalPaste("BYPASS-NOT-SENT".into()))
        .is_err());
    pump(&mut app, Duration::from_millis(100));
    assert!(!screen(&mut app).contains("SECRET-NOT-SENT"));
    controls(&mut app);
    key(&mut app, KeyCode::Char('k'));
    wait(&mut app, "Cancel Run");
    key(&mut app, KeyCode::F(2));
    wait(&mut app, "Cancelled");
    wait(&mut app, "cleanup confirmed true");
    let run = f.runs().remove(0);
    let mut client = f.client();
    let RunResult::Log(log) = complete(
        &mut client,
        RunRequest::Log {
            run_id: run.run_id,
            generation: run.log.generation,
            offset: 0,
            limit: 65536,
        },
    ) else {
        panic!("expected raw log")
    };
    let text = String::from_utf8_lossy(
        &base64::engine::general_purpose::STANDARD
            .decode(log.data_base64)
            .unwrap(),
    )
    .into_owned();
    assert!(text.contains("RAW-READY"));
    assert!(!text.contains("SECRET-NOT-SENT"));
    assert!(!text.contains("BYPASS-NOT-SENT"));
}

#[path = "common/complete_ui.rs"]
mod complete_ui;

#[test]
fn complete_menu_flow_connects_external_terminal_git_task_problem_editor_and_closes_only_ui() {
    complete_ui::run();
}
