//! Keyboard-only complete UI path through an actual executable and nested PTYs.
//! Set IDK_TEST_LAUNCHER to exercise an explicitly selected packaged candidate.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use idk_workspace::{
    client::Client, project::LaunchEnvironment, protocol::SessionState, run_wire::*, store::Store,
    terminal::TerminalSession, ui::input,
};
use portable_pty::CommandBuilder;
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

#[derive(Default, Debug)]
struct Actions {
    navigation: usize,
    editing: usize,
    paste_events: usize,
    pasted_chars: usize,
    sequence: Vec<String>,
}
struct Flow {
    terminal: TerminalSession,
    store: Store,
    binary: PathBuf,
    actions: Actions,
    ui_first_frame_ms: u128,
}
impl Flow {
    fn screen(&self) -> String {
        let snapshot = self.terminal.snapshot().unwrap();
        assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
        snapshot.text()
    }
    fn wait(&self, needle: &str) {
        self.wait_for(|text| text.contains(needle), needle);
    }
    fn wait_for(&self, predicate: impl Fn(&str) -> bool, description: &str) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let text = self.screen();
            if predicate(&text) {
                return;
            }
            assert!(Instant::now() < deadline, "missing {description}:\n{text}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    fn key(&mut self, code: KeyCode) {
        self.modified(code, KeyModifiers::NONE);
    }
    fn control(&mut self, character: char) {
        self.modified(KeyCode::Char(character), KeyModifiers::CONTROL);
    }
    fn modified(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        if modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('u') {
            self.actions.editing += 1;
        } else {
            self.actions.navigation += 1;
            self.actions
                .sequence
                .push(format!("{modifiers:?}+{code:?}"));
        }
        let modes = self.terminal.snapshot().unwrap().modes;
        self.terminal
            .input(&input::encode_key(KeyEvent::new(code, modifiers), &modes).unwrap())
            .unwrap();
    }
    fn paste(&mut self, text: &str) {
        self.actions.paste_events += 1;
        self.actions.pasted_chars += text.chars().count();
        let modes = self.terminal.snapshot().unwrap().modes;
        self.terminal
            .input(&input::encode_paste(text, &modes).unwrap())
            .unwrap();
    }
    fn replace(&mut self, text: &str) {
        self.control('u');
        self.paste(text);
    }
    fn resize(&mut self, rows: u16, cols: u16) {
        self.terminal.resize(rows, cols).unwrap();
    }
    fn git_done(&mut self, kind: &str) {
        self.wait(&format!("Git {kind}"));
        self.wait("Complete");
        self.control('g');
        self.key(KeyCode::Char('z'));
        self.wait_for(
            |s| s.contains("Changes") && !s.contains("working…"),
            "refreshed Git Changes",
        );
    }
    fn client(&self) -> Client {
        Client::connect_with_launcher(&self.store, &self.binary).unwrap()
    }
}
impl Drop for Flow {
    fn drop(&mut self) {
        if let Ok(mut client) = Client::connect_with_launcher(&self.store, &self.binary) {
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
fn git(root: &Path, home: &Path, args: &[&str]) -> String {
    let output = Command::new("/usr/bin/git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home)
        .env("LANG", "C.UTF-8")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{:?}: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}
fn run_result(client: &mut Client, request: RunRequest) -> RunResult {
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

pub(super) fn run() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let source = root.join("s");
    let external = root.join("t");
    let init = root.join("i");
    let home = root.join("h");
    for path in [&source, &external, &init, &home] {
        fs::create_dir(path).unwrap();
    }
    let diagnostic = source.join("진단 한글.cpp");
    fs::write(&diagnostic, "original\n").unwrap();
    for args in [
        &["init", "-b", "main"][..],
        &["config", "user.name", "Scripted UI Fixture"],
        &["config", "user.email", "ui@example.invalid"],
        &["config", "commit.gpgSign", "false"],
        &["add", "--", "진단 한글.cpp"],
        &["commit", "-m", "initial fixture"],
    ] {
        git(&source, &home, args);
    }
    fs::write(&diagnostic, "새 소스\n").unwrap();
    let script = init.join("setup.csh");
    let script_bytes =
        "set ui_local = kept\nalias ui_marker 'echo UI_MARKER_KEPT'\necho once >> source-count\n";
    fs::write(&script, script_bytes).unwrap();
    let editor = root.join("editor");
    fs::write(
        &editor,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > editor-arguments\n",
    )
    .unwrap();
    fs::set_permissions(&editor, fs::Permissions::from_mode(0o700)).unwrap();
    let legacy = home.join("legacy.toml");
    fs::write(&legacy, "keep legacy configuration\n").unwrap();
    let shell = std::env::var_os("IDK_TEST_SHELL")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/usr/bin/tcsh".into());
    assert!(shell.is_file());
    let binary = std::env::var_os("IDK_TEST_LAUNCHER")
        .map(PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_idk").into())
        .canonicalize()
        .unwrap();
    let store = Store::open(Some(&root.join("data"))).unwrap();
    let environment = LaunchEnvironment::from_variables(BTreeMap::from([
        ("HOME".into(), home.to_string_lossy().into_owned()),
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("TERM".into(), "xterm-256color".into()),
        ("LANG".into(), "C.UTF-8".into()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
    ]))
    .unwrap();
    let mut command = CommandBuilder::new(&binary);
    command.arg("--data-dir");
    command.arg(root.join("data"));
    command.cwd(&source);
    command.env_clear();
    for (name, value) in environment.variables() {
        command.env(name, value);
    }
    let entering = Instant::now();
    let terminal = TerminalSession::spawn(command, 36, 120, 2000).unwrap();
    let mut flow = Flow {
        terminal,
        store,
        binary,
        actions: Actions::default(),
        ui_first_frame_ms: 0,
    };
    flow.wait("Connect an existing folder");
    flow.ui_first_frame_ms = entering.elapsed().as_millis();

    // Connect entirely through fields and visible menus; no configuration DSL.
    flow.key(KeyCode::Char('n'));
    flow.wait("Project name");
    flow.paste("전체 한글");
    flow.key(KeyCode::Tab);
    flow.replace(source.to_str().unwrap());
    flow.key(KeyCode::Tab);
    flow.replace(shell.to_str().unwrap());
    flow.key(KeyCode::Tab);
    flow.key(KeyCode::Tab);
    flow.replace(init.to_str().unwrap());
    flow.key(KeyCode::Tab);
    flow.key(KeyCode::Enter);
    flow.wait("Ordered initialization scripts");
    flow.key(KeyCode::Char('a'));
    flow.wait("Add initialization script");
    flow.paste(script.to_str().unwrap());
    flow.key(KeyCode::F(2));
    flow.wait("Ordered initialization scripts");
    flow.key(KeyCode::F(2));
    flow.wait("Connect project");
    flow.key(KeyCode::Tab);
    flow.replace("외부 테스트");
    flow.key(KeyCode::Tab);
    flow.replace(external.to_str().unwrap());
    flow.key(KeyCode::F(2));
    flow.wait("F2 Save definition");
    flow.key(KeyCode::F(2));
    flow.wait("Definition saved");
    let project = flow.store.load().unwrap().projects.remove(0);
    assert_eq!(project.root, source);
    assert_eq!(project.terminals[0].cwd, external);
    assert_eq!(project.shell.sources[0].path, script);
    assert!(!init.join("source-count").exists());
    flow.key(KeyCode::Char('t'));
    flow.wait("Review initialization");
    flow.key(KeyCode::F(2));
    flow.wait("Readable initialization groups approved");
    flow.key(KeyCode::Char('1'));
    flow.key(KeyCode::Enter);
    flow.wait_for(
        |s| s.contains("initialization ready") && !s.contains("READ ONLY"),
        "initialized writable terminal",
    );
    flow.paste("ui_marker");
    flow.key(KeyCode::Enter);
    flow.wait("UI_MARKER_KEPT");
    let mut client = flow.client();
    let original_shell = client.list(None).unwrap().remove(0);
    assert_eq!(original_shell.cwd.as_deref(), Some(external.as_path()));

    // A terminal in an external cwd still targets the saved primary repository.
    flow.control('g');
    flow.key(KeyCode::Char('2'));
    flow.wait("Primary Git");
    flow.wait("진단 한글.cpp");
    flow.key(KeyCode::Enter);
    flow.wait("새 소스");
    flow.key(KeyCode::Char('z'));
    flow.wait("Changes");
    flow.key(KeyCode::Char('s'));
    flow.git_done("Stage");
    flow.key(KeyCode::Char('c'));
    flow.wait("Commit message");
    flow.paste("한글 검토");
    flow.key(KeyCode::F(2));
    flow.wait("Commit all staged changes");
    flow.key(KeyCode::F(2));
    flow.git_done("Commit");
    assert_eq!(
        git(&source, &home, &["log", "-1", "--format=%s"]).trim(),
        "한글 검토"
    );

    // Create and review a Korean task at the same 48x18 narrow size used by terminal evidence.
    flow.key(KeyCode::Char('3'));
    flow.wait("Saved revision");
    flow.key(KeyCode::Char('n'));
    flow.wait("Task draft");
    flow.resize(18, 48);
    flow.replace("한글 빌드");
    flow.wait("한글 빌드");
    flow.resize(36, 120);
    flow.key(KeyCode::Tab);
    flow.paste(
        "ui_marker\necho UI_LOCAL_$ui_local\necho '진단 한글.cpp:1:1: error: 한글 오류'\nexit 7",
    );
    flow.key(KeyCode::F(2));
    flow.wait("Definition saved");
    flow.resize(18, 48);
    flow.key(KeyCode::Enter);
    flow.wait("Review task");
    flow.wait("한글 빌드");
    flow.key(KeyCode::F(2));
    flow.resize(36, 120);
    flow.wait("Task approved");
    flow.key(KeyCode::Enter);
    flow.wait("F2 Start reviewed task");
    flow.key(KeyCode::F(2));
    flow.wait("Run registered");
    flow.wait("한글 오류");
    flow.control('g');
    flow.key(KeyCode::Char('4'));
    flow.wait("Failed");
    flow.wait("cleanup confirmed true");
    let RunResult::Runs(runs) = run_result(
        &mut client,
        RunRequest::List {
            project_id: Some(project.id.clone()),
        },
    ) else {
        panic!("expected Runs")
    };
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].name, "한글 빌드");
    assert_eq!(runs[0].exit_code, Some(7));
    assert_eq!(runs[0].cwd, source);
    flow.key(KeyCode::Char('E'));
    flow.wait("Editor configuration");
    flow.paste(editor.to_str().unwrap());
    flow.key(KeyCode::F(2));
    flow.wait("Definition saved");
    flow.key(KeyCode::Char('p'));
    flow.wait("한글 오류");
    flow.resize(18, 48);
    // A help round trip clears the old wide frame before we assert the narrow redraw.
    flow.key(KeyCode::F(1));
    flow.wait_for(
        |s| s.contains("Project controls") && !s.contains("한글 오류"),
        "narrow help replaces Problems",
    );
    flow.key(KeyCode::Esc);
    flow.wait("진단 한글.cpp");
    flow.wait("한글 오류");
    flow.key(KeyCode::Enter);
    flow.wait("Review editor launch");
    flow.wait("진단 한글.cpp");
    flow.resize(36, 120);
    flow.wait("argv[1] = --");
    flow.key(KeyCode::F(2));
    let deadline = Instant::now() + Duration::from_secs(15);
    while !source.join("editor-arguments").exists() {
        assert!(Instant::now() < deadline, "{}", flow.screen());
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        fs::read_to_string(source.join("editor-arguments")).unwrap(),
        format!("+1\n--\n{}\n", diagnostic.display())
    );
    flow.wait("Editor");
    flow.control('g');
    flow.wait("1 Terminal");
    flow.key(KeyCode::Char('q'));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(exit) = flow.terminal.try_wait().unwrap() {
            assert_eq!(exit.code, 0);
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    let remaining = client.list(None).unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].child_pid, original_shell.child_pid);
    assert_eq!(remaining[0].state, SessionState::Running);
    assert_eq!(
        fs::read_to_string(init.join("source-count"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    assert_eq!(fs::read_to_string(&script).unwrap(), script_bytes);
    assert_eq!(fs::read_to_string(&diagnostic).unwrap(), "새 소스\n");
    assert_eq!(
        fs::read_to_string(legacy).unwrap(),
        "keep legacy configuration\n"
    );
    eprintln!("S08_COMPLETE_UI binary_build_id={} ui_first_frame_ms={} narrow=48x18 full=120x36 scripted_nonhuman=true {:?}",client.info().build_id,flow.ui_first_frame_ms,flow.actions);
}
