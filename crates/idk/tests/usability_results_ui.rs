//! Keyboard actions against App/TestBackend with an actual private host and native tasks.
#[path = "common/launcher.rs"]
mod launcher;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use idk_workspace::{
    client::Client,
    portable,
    project::LaunchEnvironment,
    run_wire::{RunInfo, RunJobState, RunRequest, RunResult},
    store::Store,
    ui::{App, UiOutcome},
};
use ratatui::{backend::TestBackend, text::Line, Terminal};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    time::{Duration, Instant},
};

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    store: Store,
    project: String,
    env: LaunchEnvironment,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("source");
        let home = temp.path().join("home");
        let bin = temp.path().join("bin");
        for path in [&root, &home, &bin] {
            fs::create_dir(path).unwrap();
        }
        fs::write(
            root.join("idk.toml"),
            "# preserved declaration\nschema_version = 1\n",
        )
        .unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname='ui-fixture'\nversion='0.1.0'\n",
        )
        .unwrap();
        fs::write(root.join("a.cpp"), "original source\n").unwrap();
        let cargo = bin.join("cargo");
        fs::write(&cargo,"#!/bin/sh\nprintf x >> run-count\n/bin/cp selected-result.json \"$IDK_RESULT_PATH\"\nprintf 'task complete\\n'\n").unwrap();
        fs::set_permissions(&cargo, fs::Permissions::from_mode(0o700)).unwrap();
        let editor = bin.join("editor");
        fs::write(
            &editor,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > editor-arguments\n",
        )
        .unwrap();
        fs::set_permissions(&editor, fs::Permissions::from_mode(0o700)).unwrap();
        let env = LaunchEnvironment::from_variables(BTreeMap::from([
            ("HOME".into(), home.to_string_lossy().into_owned()),
            ("SHELL".into(), "/bin/bash".into()),
            ("PATH".into(), format!("{}:/usr/bin:/bin", bin.display())),
            ("TERM".into(), "xterm-256color".into()),
            ("EDITOR".into(), editor.to_string_lossy().into_owned()),
        ]))
        .unwrap();
        let store = Store::open(Some(&temp.path().join("data"))).unwrap();
        let resolved = portable::resolve(&store, &root, &env).unwrap();
        let project = portable::apply(&store, resolved).unwrap().id;
        Self {
            _temp: temp,
            root,
            store,
            project,
            env,
        }
    }
    fn app(&self) -> App<'_> {
        let mut app = App::with_environment(&self.store, self.env.clone()).unwrap();
        app.enable_terminal_runtime(launcher::path().into())
            .unwrap();
        key(&mut app, KeyCode::Char('3'));
        wait(&mut app, "Saved revision");
        app
    }
    fn client(&self) -> Client {
        Client::connect_with_launcher(&self.store, launcher::path()).unwrap()
    }
    fn runs(&self) -> Vec<RunInfo> {
        let mut client = self.client();
        let RunResult::Runs(runs) = complete(
            &mut client,
            RunRequest::List {
                project_id: Some(self.project.clone()),
            },
        ) else {
            panic!("list result")
        };
        runs
    }
    fn payload(&self, complete: bool, range: std::ops::Range<usize>, large: bool) {
        let findings=range.map(|i|serde_json::json!({"severity":"warn","code":format!("code-{i:03}"),"message":format!("finding-{i:03}"),"evidence":{"path":"a.cpp","object":format!("item-{i:03}")}})).collect::<Vec<_>>();
        let payload = serde_json::json!({"schema_id":"lens.result","schema_version":1,"producer":{"name":"lens","version":"0.10.0"},"run_id":null,
            "complete":complete,"execution":{"exit_code":0},"source":{"kind":"command","identity":["fixture","same-scope"]},"findings":findings,
            "data":{"large":if large {"x".repeat(1100*1024)}else{String::new()}}});
        fs::write(
            self.root.join("selected-result.json"),
            serde_json::to_vec(&payload).unwrap(),
        )
        .unwrap();
    }
    fn finish_run(&self, app: &mut App<'_>, count: usize) -> Vec<RunInfo> {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            app.tick();
            let runs = self.runs();
            if runs.len() >= count && runs.iter().all(|r| r.cleanup_confirmed) {
                return runs;
            }
            assert!(
                Instant::now() < deadline,
                "Run not completed: {runs:?}\n{}",
                screen(app)
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Ok(mut client) = Client::connect_with_launcher(&self.store, launcher::path()) {
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
    let deadline = Instant::now() + Duration::from_secs(15);
    while job.state == RunJobState::Pending {
        assert!(Instant::now() < deadline, "pending {job:?}");
        std::thread::sleep(Duration::from_millis(10));
        job = client.run(RunRequest::Job { job_id: job.job_id }).unwrap();
    }
    assert_eq!(job.state, RunJobState::Complete, "{:?}", job.error);
    job.result.unwrap()
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
fn key(app: &mut App<'_>, code: KeyCode) {
    event(app, Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
}
fn ctrl(app: &mut App<'_>, ch: char) {
    event(
        app,
        Event::Key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL)),
    );
}
fn screen(app: &mut App<'_>) -> String {
    let mut terminal = Terminal::new(TestBackend::new(150, 45)).unwrap();
    terminal.draw(|f| app.render(f)).unwrap();
    let buffer = terminal.backend().buffer();
    (0..45)
        .map(|row| {
            let mut text = String::new();
            let mut col = 0;
            while col < 150 {
                let value = buffer[(col, row)].symbol();
                text.push_str(value);
                col += Line::from(value).width().max(1) as u16;
            }
            text
        })
        .collect::<Vec<_>>()
        .join("\n")
}
fn wait(app: &mut App<'_>, needle: &str) {
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        app.tick();
        let shown = screen(app);
        if shown.contains(needle) {
            return;
        }
        assert!(Instant::now() < deadline, "missing {needle}:\n{shown}");
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn controls(app: &mut App<'_>) {
    if !screen(app).contains("1 Terminal") {
        ctrl(app, 'g');
    }
}
fn discover_start(f: &Fixture, app: &mut App<'_>) {
    key(app, KeyCode::Char('d'));
    wait(app, "Discover tasks");
    assert!(!f.root.join("run-count").exists());
    key(app, KeyCode::Down);
    key(app, KeyCode::F(2));
    wait(app, "Definition saved");
    let workspace = f.store.load().unwrap();
    let tasks = &workspace.project(&f.project).unwrap().tasks;
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].name, "cargo test");
    assert!(tasks[0].approved_digest.is_none());
    assert!(!f.root.join("run-count").exists());
    key(app, KeyCode::Enter);
    wait(app, "F2 Approve and start this exact task");
    key(app, KeyCode::F(2));
}
fn open_results(app: &mut App<'_>, run_id: &str) {
    // Wait for the run-start reply to settle before navigating away from its
    // automatically attached terminal, then wait for the requested list itself.
    let settle = Instant::now() + Duration::from_millis(200);
    while Instant::now() < settle {
        app.tick();
        std::thread::sleep(Duration::from_millis(10));
    }
    controls(app);
    key(app, KeyCode::Char('4'));
    key(app, KeyCode::Char('z'));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        app.tick();
        let shown = screen(app);
        if shown.contains(run_id) && !shown.contains("request(s) working") {
            break;
        }
        assert!(Instant::now() < deadline, "run list not ready:\n{shown}");
        std::thread::sleep(Duration::from_millis(20));
    }
    for _ in 0..64 {
        key(app, KeyCode::Up);
    }
    for _ in 0..64 {
        if screen(app)
            .lines()
            .any(|line| line.contains("▶") && line.contains(run_id))
        {
            break;
        }
        key(app, KeyCode::Down);
    }
    assert!(screen(app)
        .lines()
        .any(|line| line.contains("▶") && line.contains(run_id)));
    key(app, KeyCode::Enter);
    wait(app, "s Findings");
}

#[test]
fn discovered_task_registers_then_approves_and_runs_in_one_review_flow() {
    let f = Fixture::new();
    f.payload(true, 0..2, false);
    let mut app = f.app();
    discover_start(&f, &mut app);
    let runs = f.finish_run(&mut app, 1);
    assert_eq!(runs[0].exit_code, Some(0));
    assert_eq!(fs::read_to_string(f.root.join("run-count")).unwrap(), "x");
    controls(&mut app);
    key(&mut app, KeyCode::Char('3'));
    wait(&mut app, "cargo test");
    key(&mut app, KeyCode::Enter);
    wait(&mut app, "F2 Start reviewed task");
    assert!(!screen(&mut app).contains("F2 Approve and start"));
    key(&mut app, KeyCode::F(2));
    f.finish_run(&mut app, 2);
    assert_eq!(fs::read_to_string(f.root.join("run-count")).unwrap(), "xx");
}

#[test]
fn large_findings_paging_editor_pin_and_incomplete_baseline_comparison() {
    let f = Fixture::new();
    f.payload(true, 0..150, true);
    let mut app = f.app();
    discover_start(&f, &mut app);
    let baseline = f.finish_run(&mut app, 1).remove(0);
    assert!(
        baseline
            .structured_result
            .as_ref()
            .unwrap()
            .body()
            .unwrap()
            .bytes
            > 1024 * 1024
    );
    open_results(&mut app, &baseline.run_id);
    key(&mut app, KeyCode::Char('b'));
    wait(&mut app, "Run retention updated");
    assert!(f.runs()[0].pinned);
    key(&mut app, KeyCode::Char('s'));
    wait(&mut app, "Findings 1–100 of 150");
    for _ in 0..20 {
        key(&mut app, KeyCode::Down);
    }
    assert!(
        screen(&mut app).contains("▶ warn code-020"),
        "selected finding scrolled out of view:\n{}",
        screen(&mut app)
    );
    key(&mut app, KeyCode::Char(']'));
    wait(&mut app, "Findings 101–150 of 150");
    key(&mut app, KeyCode::Enter);
    wait(&mut app, "Review editor launch");
    assert!(!f.root.join("editor-arguments").exists());
    key(&mut app, KeyCode::F(2));
    let deadline = Instant::now() + Duration::from_secs(10);
    while !f.root.join("editor-arguments").exists() {
        app.tick();
        assert!(
            Instant::now() < deadline,
            "editor not started:\n{}",
            screen(&mut app)
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        fs::read_to_string(f.root.join("editor-arguments")).unwrap(),
        format!("--\n{}\n", f.root.join("a.cpp").display())
    );
    f.payload(false, 140..150, false);
    controls(&mut app);
    key(&mut app, KeyCode::Char('3'));
    wait(&mut app, "cargo test");
    key(&mut app, KeyCode::Enter);
    wait(&mut app, "F2 Start reviewed task");
    key(&mut app, KeyCode::F(2));
    let runs = f.finish_run(&mut app, 2);
    let current = runs.iter().find(|r| r.run_id != baseline.run_id).unwrap();
    assert!(!current.pinned);
    open_results(&mut app, &current.run_id);
    key(&mut app, KeyCode::Char('c'));
    wait(&mut app, "Partial:");
    wait(&mut app, "resolved 0 · carried 140");
    assert!(
        f.runs()
            .iter()
            .find(|r| r.run_id == baseline.run_id)
            .unwrap()
            .pinned
    );
}

#[test]
fn project_configuration_conflict_preserves_external_original_and_local_draft() {
    let f = Fixture::new();
    let mut app = f.app();
    let declaration = f.root.join("idk.toml");
    key(&mut app, KeyCode::Char('C'));
    wait(&mut app, "Project settings");
    event(&mut app, Event::Paste("\nname = 'local draft'\n".into()));
    let external = "# external edit retained\nschema_version = 1\nname = 'external'\n";
    fs::write(&declaration, external).unwrap();
    key(&mut app, KeyCode::F(2));
    wait(&mut app, "changed externally; original and draft preserved");
    assert_eq!(fs::read_to_string(&declaration).unwrap(), external);
    assert!(screen(&mut app).contains("local draft"));
    key(&mut app, KeyCode::Esc);
    wait(&mut app, "Declaration draft kept");
    let drafts = fs::read_dir(&f.store.config_dir)
        .unwrap()
        .filter_map(|entry| {
            let path = entry.unwrap().path();
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("declaration-draft-")
                .then_some(path)
        })
        .collect::<Vec<_>>();
    assert_eq!(drafts.len(), 1);
    assert!(fs::read_to_string(&drafts[0])
        .unwrap()
        .contains("name = 'local draft'"));
    assert_eq!(fs::read_to_string(&declaration).unwrap(), external);
}

#[test]
fn rediscovering_registered_candidate_does_not_create_ambiguous_duplicates() {
    let f = Fixture::new();
    let mut app = f.app();
    key(&mut app, KeyCode::Char('d'));
    wait(&mut app, "Discover tasks");
    key(&mut app, KeyCode::F(2));
    wait(&mut app, "Definition saved");
    key(&mut app, KeyCode::Char('d'));
    wait(&mut app, "Discover tasks");
    key(&mut app, KeyCode::F(2));
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        app.tick();
        let shown = screen(&mut app);
        if !shown.contains("request(s) working") {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    let workspace = f.store.load().unwrap();
    let tasks = &workspace.project(&f.project).unwrap().tasks;
    assert_eq!(
        tasks.len(),
        1,
        "same discovery candidate was saved twice with ambiguous names"
    );
    assert!(!f.root.join("run-count").exists());
}

#[test]
fn failed_pin_does_not_claim_a_durable_baseline() {
    let f = Fixture::new();
    f.payload(true, 0..2, false);
    let mut app = f.app();
    discover_start(&f, &mut app);
    let run = f.finish_run(&mut app, 1).remove(0);
    open_results(&mut app, &run.run_id);
    idk_workspace::result_store::ResultStore::open(&f.store.state_dir)
        .unwrap()
        .remove(
            run.structured_result.as_ref().unwrap().body().unwrap(),
            false,
        )
        .unwrap();
    key(&mut app, KeyCode::Char('b'));
    wait(&mut app, "cannot pin expired or tampered result");
    assert!(!f.runs()[0].pinned);
    assert!(
        !screen(&mut app).contains("Baseline:"),
        "failed pin must not establish a baseline:\n{}",
        screen(&mut app)
    );
}

#[test]
fn literal_multiline_argv_survives_an_edit_to_another_argument() {
    let f = Fixture::new();
    let candidate = idk_workspace::discovery::TaskCandidate {
        name: "literal arguments".into(),
        program: "/bin/echo".into(),
        resolved_program: Some("/bin/echo".into()),
        args: vec!["-c".into(), "print(1)\nprint(2)".into(), "old".into()],
        cwd: f.root.clone(),
        origin: f.root.join("Cargo.toml"),
        available: true,
        missing_tool: None,
    };
    idk_workspace::task::TaskService { store: &f.store }
        .save(
            f.store.load().unwrap().revision,
            &f.project,
            candidate.task_definition().unwrap(),
        )
        .unwrap();
    let mut app = f.app();
    key(&mut app, KeyCode::Char('e'));
    wait(&mut app, "Task draft");
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Tab);
    wait(&mut app, "Arguments (JSON array");
    let replacement = vec![
        "-c".to_string(),
        "print(1)\nprint(2)".to_string(),
        "new".to_string(),
    ];
    let text = if screen(&mut app).contains("JSON") {
        serde_json::to_string(&replacement).unwrap()
    } else {
        replacement.join("\n")
    };
    ctrl(&mut app, 'u');
    event(&mut app, Event::Paste(text));
    key(&mut app, KeyCode::F(2));
    wait(&mut app, "Definition saved");
    let workspace = f.store.load().unwrap();
    let task = &workspace.project(&f.project).unwrap().tasks[0];
    let Some(idk_workspace::model::TaskExecution::Process { args, .. }) = &task.execution else {
        panic!("process task changed kind")
    };
    assert_eq!(
        args, &replacement,
        "editing one argument split a separate embedded-newline argument"
    );
}

#[test]
fn single_empty_argument_can_be_explicitly_cleared_in_ui() {
    let f = Fixture::new();
    let candidate = idk_workspace::discovery::TaskCandidate {
        name: "empty argument".into(),
        program: "/bin/echo".into(),
        resolved_program: Some("/bin/echo".into()),
        args: vec![String::new()],
        cwd: f.root.clone(),
        origin: f.root.join("Cargo.toml"),
        available: true,
        missing_tool: None,
    };
    idk_workspace::task::TaskService { store: &f.store }
        .save(
            f.store.load().unwrap().revision,
            &f.project,
            candidate.task_definition().unwrap(),
        )
        .unwrap();
    let mut app = f.app();
    key(&mut app, KeyCode::Char('e'));
    wait(&mut app, "Task draft");
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Tab);
    wait(&mut app, "Arguments (JSON array");
    assert!(
        screen(&mut app).contains("JSON"),
        "an empty argument needs lossless representation"
    );
    ctrl(&mut app, 'u');
    event(&mut app, Event::Paste("[]".into()));
    key(&mut app, KeyCode::F(2));
    wait(&mut app, "Definition saved");
    let workspace = f.store.load().unwrap();
    let task = &workspace.project(&f.project).unwrap().tasks[0];
    let Some(idk_workspace::model::TaskExecution::Process { args, .. }) = &task.execution else {
        panic!("process task changed kind")
    };
    assert!(
        args.is_empty(),
        "explicit empty argv retained the old empty argument"
    );
}

#[test]
fn rapid_next_page_input_does_not_duplicate_back_history() {
    let f = Fixture::new();
    f.payload(true, 0..150, false);
    let mut app = f.app();
    discover_start(&f, &mut app);
    let run = f.finish_run(&mut app, 1).remove(0);
    open_results(&mut app, &run.run_id);
    key(&mut app, KeyCode::Char('s'));
    wait(&mut app, "Findings 1–100 of 150");
    key(&mut app, KeyCode::Char(']'));
    key(&mut app, KeyCode::Char(']'));
    wait(&mut app, "Findings 101–150 of 150");
    key(&mut app, KeyCode::Char('['));
    wait(&mut app, "Findings 1–100 of 150");
    key(&mut app, KeyCode::Char('['));
    wait(&mut app, "First finding page");
}
