use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use idk_workspace::project::{LaunchEnvironment, ProjectService};
use idk_workspace::store::Store;
use idk_workspace::ui::{App, UiOutcome};
use ratatui::{backend::TestBackend, text::Line, Terminal};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

struct Fixture {
    temp: TempDir,
    store: Store,
    root: PathBuf,
    shell: PathBuf,
    environment: LaunchEnvironment,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("프로젝트");
        let home = temp.path().join("home");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&home).unwrap();
        let shell = std::env::var_os("IDK_TEST_SHELL")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/usr/bin/tcsh"));
        let environment = LaunchEnvironment::from_variables(BTreeMap::from([
            ("HOME".into(), home.to_string_lossy().into_owned()),
            (
                "PATH".into(),
                format!("{}:/usr/bin:/bin", shell.parent().unwrap().display()),
            ),
            ("TERM".into(), "xterm-256color".into()),
            ("LANG".into(), "C.UTF-8".into()),
            ("UI_PRIVATE_VALUE".into(), "do-not-save-this".into()),
        ]))
        .unwrap();
        let store = Store::open(Some(&temp.path().join("data"))).unwrap();
        Self {
            temp,
            store,
            root,
            shell,
            environment,
        }
    }
    fn app(&self) -> App<'_> {
        App::with_environment(&self.store, self.environment.clone()).unwrap()
    }
    fn folder(&self, name: &str) -> PathBuf {
        let path = self.temp.path().join(name);
        fs::create_dir(&path).unwrap();
        path
    }
}

fn key(app: &mut App<'_>, code: KeyCode) -> UiOutcome {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
        .unwrap()
}
fn modified(app: &mut App<'_>, code: KeyCode, modifiers: KeyModifiers) -> UiOutcome {
    app.handle_key(KeyEvent::new(code, modifiers)).unwrap()
}
fn paste(app: &mut App<'_>, text: &str) {
    app.handle_event(Event::Paste(text.into())).unwrap();
}
fn replace(app: &mut App<'_>, text: &str) {
    key(app, KeyCode::Home);
    modified(app, KeyCode::Char('k'), KeyModifiers::CONTROL);
    paste(app, text);
}
fn tabs(app: &mut App<'_>, count: usize) {
    for _ in 0..count {
        key(app, KeyCode::Tab);
    }
}
fn save(app: &mut App<'_>) {
    key(app, KeyCode::F(2));
    key(app, KeyCode::F(2));
}
fn screen(app: &mut App<'_>, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| app.render(frame)).unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            let mut line = String::new();
            let mut x = 0;
            while x < width {
                let symbol = buffer[(x, y)].symbol();
                line.push_str(symbol);
                x += Line::from(symbol).width().max(1) as u16;
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}
fn connect_form(app: &mut App<'_>, fixture: &Fixture, name: &str) {
    key(app, KeyCode::Char('p'));
    key(app, KeyCode::Char('n'));
    replace(app, name);
    tabs(app, 1);
    replace(app, fixture.root.to_str().unwrap());
    tabs(app, 1);
    replace(app, fixture.shell.to_str().unwrap());
}
fn connect(app: &mut App<'_>, fixture: &Fixture, name: &str) {
    connect_form(app, fixture, name);
    save(app);
    assert_eq!(
        fixture.store.load().unwrap().projects.len(),
        1,
        "{}",
        screen(app, 110, 30)
    );
}
fn add_terminal(app: &mut App<'_>, name: &str, cwd: &Path, persistent: bool) {
    key(app, KeyCode::Char('1'));
    key(app, KeyCode::Char('n'));
    replace(app, name);
    tabs(app, 1);
    replace(app, cwd.to_str().unwrap());
    tabs(app, 1);
    if !persistent {
        key(app, KeyCode::Char(' '));
    }
    save(app);
}

#[test]
fn six_named_terminals_use_distinct_ids_and_do_not_create_missing_paths() {
    let fixture = Fixture::new();
    let mut app = fixture.app();
    connect(&mut app, &fixture, "한글 project");
    add_terminal(&mut app, "Development 2", &fixture.root, true);
    for number in 1..=3 {
        let folder = fixture.folder(&format!("external test {number}"));
        add_terminal(&mut app, &format!("Test {number}"), &folder, true);
    }
    let missing = fixture.temp.path().join("missing sixth");
    add_terminal(&mut app, "Sixth", &missing, true);
    let workspace = fixture.store.load().unwrap();
    let project = &workspace.projects[0];
    assert_eq!(project.terminals.len(), 6);
    assert_eq!(
        project
            .terminals
            .iter()
            .map(|terminal| &terminal.id)
            .collect::<HashSet<_>>()
            .len(),
        6
    );
    assert_eq!(project.terminals[0].cwd, project.terminals[1].cwd);
    assert!(!missing.exists());
    let text = screen(&mut app, 110, 30);
    assert!(text.contains("Sixth") && text.contains("missing"), "{text}");
    assert!(text.contains("available"), "{text}");
    let id = app.selected_terminal_definition().unwrap().id;
    modified(&mut app, KeyCode::Up, KeyModifiers::ALT);
    key(&mut app, KeyCode::Char('d'));
    let workspace = fixture.store.load().unwrap();
    assert_eq!(workspace.projects[0].terminals[4].id, id);
    assert_eq!(
        workspace.projects[0].default_terminal.as_deref(),
        Some(id.as_str())
    );
    key(&mut app, KeyCode::Char('e'));
    replace(&mut app, "여섯째");
    save(&mut app);
    assert_eq!(app.selected_terminal_definition().unwrap().id, id);
    assert_eq!(app.selected_terminal_definition().unwrap().name, "여섯째");
    key(&mut app, KeyCode::Enter);
    assert!(screen(&mut app, 110, 30).contains("nothing was started"));
}

#[test]
fn canceled_unicode_draft_survives_resize_and_review_does_not_persist() {
    let fixture = Fixture::new();
    let mut app = fixture.app();
    key(&mut app, KeyCode::Char('n'));
    paste(&mut app, "한글A");
    key(&mut app, KeyCode::Left);
    key(&mut app, KeyCode::Backspace);
    assert!(screen(&mut app, 44, 16).contains("한A"));
    key(&mut app, KeyCode::Esc);
    assert!(!fixture.store.config_path().exists());
    key(&mut app, KeyCode::Char('n'));
    assert!(screen(&mut app, 100, 32).contains("한A"));
    tabs(&mut app, 1);
    replace(&mut app, fixture.root.to_str().unwrap());
    tabs(&mut app, 1);
    replace(&mut app, fixture.shell.to_str().unwrap());
    key(&mut app, KeyCode::F(2));
    assert!(screen(&mut app, 44, 16).contains("Review"));
    assert!(!fixture.store.config_path().exists());
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Char('n'));
    save(&mut app);
    assert_eq!(fixture.store.load().unwrap().projects[0].name, "한A");
    let saved = fs::read_to_string(fixture.store.config_path()).unwrap();
    assert!(!saved.contains("do-not-save-this"));
}

#[test]
fn source_paths_and_each_literal_argument_are_edited_without_shell_parsing() {
    let fixture = Fixture::new();
    let source = fixture.root.join("environment 한글.csh");
    let original = "echo UNEXPECTED_EXECUTION > must-not-exist\n";
    fs::write(&source, original).unwrap();
    let mut app = fixture.app();
    connect_form(&mut app, &fixture, "Source args");
    tabs(&mut app, 3);
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::Char('a'));
    paste(&mut app, source.to_str().unwrap());
    key(&mut app, KeyCode::F(3));
    paste(&mut app, "space inside one argument");
    key(&mut app, KeyCode::F(3));
    paste(&mut app, "$VALUE ; 'quotes' ! 한글");
    key(&mut app, KeyCode::F(3));
    // Explicit empty third argument must not disappear.
    key(&mut app, KeyCode::F(2));
    key(&mut app, KeyCode::F(2));
    save(&mut app);
    let workspace = fixture.store.load().unwrap();
    let saved = &workspace.projects[0].shell.sources[0];
    assert_eq!(saved.path, source);
    assert_eq!(
        saved.args,
        ["space inside one argument", "$VALUE ; 'quotes' ! 한글", ""]
    );
    assert_eq!(fs::read_to_string(&source).unwrap(), original);
    assert!(!fixture.root.join("must-not-exist").exists());
    key(&mut app, KeyCode::Char('t'));
    let text = screen(&mut app, 110, 38);
    assert!(
        text.contains("Review initialization") && text.contains("Argument 1"),
        "{text}"
    );
    assert!(text.contains("space inside one argument"), "{text}");
    key(&mut app, KeyCode::Esc);
    assert!(!fixture.root.join("must-not-exist").exists());
}

#[test]
fn temporary_definitions_keep_their_id_when_explicitly_saved() {
    let fixture = Fixture::new();
    let mut app = fixture.app();
    connect(&mut app, &fixture, "Temporary");
    let revision = fixture.store.load().unwrap().revision;
    add_terminal(&mut app, "Scratch", &fixture.root, false);
    let transient = app.selected_terminal_definition().unwrap();
    assert!(!transient.persistent);
    assert_eq!(fixture.store.load().unwrap().projects[0].terminals.len(), 1);
    assert_eq!(fixture.store.load().unwrap().revision, revision);
    assert!(screen(&mut app, 100, 28).contains("temporary"));
    let mut restarted = fixture.app();
    assert!(!screen(&mut restarted, 100, 28).contains("Scratch"));
    key(&mut app, KeyCode::Char('s'));
    key(&mut app, KeyCode::F(2));
    let saved = app.selected_terminal_definition().unwrap();
    assert!(saved.persistent);
    assert_eq!(saved.id, transient.id);
    assert_eq!(fixture.store.load().unwrap().projects[0].terminals.len(), 2);
}

#[test]
fn stale_form_cannot_overwrite_external_changes_and_can_reload_saved_values() {
    let fixture = Fixture::new();
    let mut app = fixture.app();
    connect(&mut app, &fixture, "Original");
    let id = app.selected_project_id().unwrap();
    key(&mut app, KeyCode::Char('r'));
    replace(&mut app, "My draft");
    let service = ProjectService {
        store: &fixture.store,
    };
    service
        .rename(
            fixture.store.load().unwrap().revision,
            &id,
            "External edit".into(),
        )
        .unwrap();
    save(&mut app);
    assert_eq!(
        fixture.store.load().unwrap().projects[0].name,
        "External edit"
    );
    let text = screen(&mut app, 110, 30);
    assert!(text.contains("configuration changed elsewhere"), "{text}");
    assert!(text.contains("My draft"), "{text}");
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::F(4));
    assert!(screen(&mut app, 100, 26).contains("External edit"));
    replace(&mut app, "Reviewed latest");
    save(&mut app);
    assert_eq!(
        fixture.store.load().unwrap().projects[0].name,
        "Reviewed latest"
    );
}

#[test]
fn duplicate_connection_requires_separate_definition_choice_and_removal_preserves_files() {
    let fixture = Fixture::new();
    let source = fixture.root.join("original.csh");
    fs::write(&source, "set kept = original\n").unwrap();
    let mut app = fixture.app();
    connect(&mut app, &fixture, "First");
    let first_id = app.selected_project_id().unwrap();
    connect_form(&mut app, &fixture, "Separate");
    save(&mut app);
    assert_eq!(fixture.store.load().unwrap().projects.len(), 1);
    assert!(screen(&mut app, 110, 32).contains("already connected"));
    key(&mut app, KeyCode::Char('d'));
    key(&mut app, KeyCode::F(2));
    let workspace = fixture.store.load().unwrap();
    assert_eq!(workspace.projects.len(), 2);
    assert_ne!(app.selected_project_id().unwrap(), first_id);
    key(&mut app, KeyCode::Char('p'));
    let before = fs::read(fixture.store.config_path()).unwrap();
    key(&mut app, KeyCode::Delete);
    key(&mut app, KeyCode::Esc);
    assert_eq!(fs::read(fixture.store.config_path()).unwrap(), before);
    key(&mut app, KeyCode::Delete);
    key(&mut app, KeyCode::F(2));
    assert_eq!(fixture.store.load().unwrap().projects.len(), 1);
    assert_eq!(fs::read_to_string(source).unwrap(), "set kept = original\n");
}

#[test]
fn moved_project_review_preserves_identity_and_external_terminal_paths() {
    let fixture = Fixture::new();
    let mut app = fixture.app();
    connect(&mut app, &fixture, "Moved");
    let id = app.selected_project_id().unwrap();
    let external = fixture.folder("external");
    add_terminal(&mut app, "External", &external, true);
    let before = fixture.store.load().unwrap().projects[0].clone();
    let new_root = fixture.temp.path().join("new project folder");
    fs::rename(&fixture.root, &new_root).unwrap();
    key(&mut app, KeyCode::F(5));
    key(&mut app, KeyCode::Char('m'));
    replace(&mut app, new_root.to_str().unwrap());
    key(&mut app, KeyCode::F(2));
    assert_eq!(fixture.store.load().unwrap().projects[0].root, fixture.root);
    assert!(screen(&mut app, 110, 32).contains("Review"));
    key(&mut app, KeyCode::F(2));
    let after = &fixture.store.load().unwrap().projects[0];
    assert_eq!(after.id, id);
    assert_eq!(after.root, new_root);
    assert_eq!(after.terminals[0].id, before.terminals[0].id);
    assert_eq!(after.terminals[1].cwd, external);
    assert!(!fixture.root.exists());
}

#[test]
fn terminal_focus_forwards_real_keys_and_double_control_g_without_raw_encoding() {
    let fixture = Fixture::new();
    let mut app = fixture.app();
    app.set_terminal_connected(true);
    for event in [
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
    ] {
        assert_eq!(
            app.handle_key(event).unwrap(),
            UiOutcome::ForwardTerminalKey(event)
        );
    }
    let ctrl_g = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL);
    assert_eq!(app.handle_key(ctrl_g).unwrap(), UiOutcome::Continue);
    assert_eq!(
        app.handle_key(ctrl_g).unwrap(),
        UiOutcome::ForwardTerminalKey(ctrl_g)
    );
    assert_eq!(
        app.handle_event(Event::Paste("한글\tpasted".into()))
            .unwrap(),
        UiOutcome::ForwardTerminalPaste("한글\tpasted".into())
    );
    app.set_terminal_connected(false);
    assert_eq!(key(&mut app, KeyCode::Char('q')), UiOutcome::Quit);
}

#[test]
fn control_character_paste_is_rejected_without_rewriting_the_draft() {
    let fixture = Fixture::new();
    let mut app = fixture.app();
    key(&mut app, KeyCode::Char('n'));
    paste(&mut app, "Kept");
    paste(&mut app, "\u{1b}]52;c;do-not-execute\u{7}");
    let text = screen(&mut app, 80, 26);
    assert!(text.contains("Kept"));
    assert!(text.contains("control characters"));
    assert!(!text.contains("do-not-execute"));
    assert!(!fixture.store.config_path().exists());
}

#[test]
fn repository_form_keeps_related_binding_separate_until_explicit_selection() {
    let fixture = Fixture::new();
    let repository = fixture.folder("related repository");
    assert!(std::process::Command::new("git")
        .args(["-c", "init.defaultBranch=main", "init", "--quiet"])
        .arg(&repository)
        .status()
        .unwrap()
        .success());
    let mut app = fixture.app();
    connect(&mut app, &fixture, "Non-Git project");
    key(&mut app, KeyCode::Char('g'));
    replace(&mut app, repository.to_str().unwrap());
    tabs(&mut app, 1);
    key(&mut app, KeyCode::Char(' '));
    save(&mut app);
    let workspace = fixture.store.load().unwrap();
    assert!(workspace.projects[0].repository.is_none());
    assert_eq!(
        workspace.projects[0].related_repositories[0].root,
        repository
    );
    key(&mut app, KeyCode::Char('2'));
    assert!(screen(&mut app, 100, 26).contains("Related repository"));
    key(&mut app, KeyCode::Enter);
    save(&mut app);
    assert_eq!(
        fixture.store.load().unwrap().projects[0]
            .repository
            .as_ref(),
        Some(&repository)
    );
    key(&mut app, KeyCode::Char('g'));
    replace(&mut app, "");
    save(&mut app);
    assert!(fixture.store.load().unwrap().projects[0]
        .repository
        .is_none());
    assert!(repository.join(".git").is_dir());
}

#[test]
fn transient_trust_is_explicit_in_memory_and_changed_startup_requires_review() {
    let fixture = Fixture::new();
    assert!(
        fixture.shell.is_file(),
        "set IDK_TEST_SHELL to a real tcsh for trust inspection"
    );
    let mut app = fixture.app();
    connect(&mut app, &fixture, "Trust");
    key(&mut app, KeyCode::Char('t'));
    key(&mut app, KeyCode::F(2));
    assert!(fixture.store.load().unwrap().projects[0]
        .shell
        .trusted_digest
        .is_some());
    add_terminal(&mut app, "Private scratch", &fixture.root, false);
    assert!(app
        .selected_terminal_definition()
        .unwrap()
        .trusted_digest
        .is_none());
    key(&mut app, KeyCode::Char('t'));
    assert!(screen(&mut app, 110, 30).contains("Review temporary initialization"));
    key(&mut app, KeyCode::F(2));
    assert!(app
        .selected_terminal_definition()
        .unwrap()
        .trusted_digest
        .is_some());
    let saved = fs::read_to_string(fixture.store.config_path()).unwrap();
    assert!(!saved.contains("Private scratch"));
    fs::write(
        fixture.environment.home().join(".cshrc"),
        "set changed_after_review = true\n",
    )
    .unwrap();
    key(&mut app, KeyCode::Char('t'));
    let text = screen(&mut app, 110, 32);
    assert!(
        text.contains("Review common initialization first"),
        "{text}"
    );
    assert!(text.contains("review needed"), "{text}");
}

#[test]
fn actual_tui_connects_through_a_pty_resizes_and_exits_without_running_scripts() {
    use idk_workspace::terminal::TerminalSession;
    use portable_pty::CommandBuilder;
    use std::time::{Duration, Instant};

    fn wait_screen(session: &TerminalSession, needle: &str) {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let snapshot = session.snapshot().unwrap();
            assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
            let text = snapshot.text();
            if text.contains(needle) {
                return;
            }
            assert!(Instant::now() < deadline, "missing {needle:?}: {text}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    let fixture = Fixture::new();
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_idk"));
    command.arg("--data-dir");
    command.arg(fixture.temp.path().join("data"));
    command.cwd(&fixture.root);
    for (key, value) in fixture.environment.variables() {
        command.env(key, value);
    }
    let mut terminal = TerminalSession::spawn(command, 30, 110, 1000).unwrap();
    wait_screen(&terminal, "Connect an existing folder");
    terminal.input(b"n").unwrap();
    wait_screen(&terminal, "Project name");
    terminal.input("PTY 한글".as_bytes()).unwrap();
    terminal.input(b"\t\t\x01\x0b").unwrap();
    terminal
        .input(fixture.shell.to_str().unwrap().as_bytes())
        .unwrap();
    terminal.input(b"\x1bOQ").unwrap();
    wait_screen(&terminal, "Review");
    terminal.resize(18, 48).unwrap();
    wait_screen(&terminal, "Review");
    terminal.input(b"\x1bOQ").unwrap();
    wait_screen(&terminal, "Definition saved");
    let workspace = fixture.store.load().unwrap();
    assert_eq!(workspace.projects[0].name, "PTY 한글");
    assert_eq!(workspace.projects[0].root, fixture.root);
    assert_eq!(workspace.projects[0].terminals.len(), 1);
    assert!(workspace.projects[0].shell.trusted_digest.is_none());
    terminal.input(b"q").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(exit) = terminal.try_wait().unwrap() {
            assert_eq!(exit.code, 0);
            break;
        }
        assert!(Instant::now() < deadline, "TUI did not exit");
        std::thread::sleep(Duration::from_millis(20));
    }
}
