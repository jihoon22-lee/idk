use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventState, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use idk_workspace::{
    client::Client,
    model::ShellConfig,
    project::{ConnectDraft, LaunchEnvironment, ProjectService, TerminalDraft},
    store::Store,
    terminal::{TerminalCell, TerminalColor, TerminalModes, TerminalSession, TerminalSnapshot},
    ui::{input, screen, App, UiOutcome},
};
use portable_pty::CommandBuilder;
use ratatui::{backend::TestBackend, layout::Rect, Terminal};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[test]
fn xterm_keys_modes_and_paste_have_exact_bounded_bytes() {
    let mut modes = TerminalModes::default();
    for (code, modifiers, expected) in [
        (
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
            b"\x03".as_slice(),
        ),
        (KeyCode::Char('z'), KeyModifiers::CONTROL, b"\x1a"),
        (KeyCode::Up, KeyModifiers::NONE, b"\x1b[A"),
        (KeyCode::F(1), KeyModifiers::NONE, b"\x1bOP"),
        (KeyCode::BackTab, KeyModifiers::SHIFT, b"\x1b[Z"),
        (KeyCode::Char('가'), KeyModifiers::NONE, "가".as_bytes()),
    ] {
        assert_eq!(
            input::encode_key(KeyEvent::new(code, modifiers), &modes).unwrap(),
            expected
        );
    }
    modes.application_cursor = true;
    assert_eq!(
        input::encode_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), &modes).unwrap(),
        b"\x1bOA"
    );
    modes.application_keypad = true;
    let mut keypad = KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE);
    keypad.state = KeyEventState::KEYPAD;
    assert_eq!(input::encode_key(keypad, &modes).unwrap(), b"\x1bOq");
    assert_eq!(
        input::encode_key(
            KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE),
            &modes
        )
        .unwrap(),
        b"1"
    );
    modes.bracketed_paste = true;
    assert_eq!(
        input::encode_paste("한글\ntext", &modes).unwrap(),
        "\x1b[200~한글\ntext\x1b[201~".as_bytes()
    );
    assert!(input::encode_paste(&"a".repeat(65525), &modes).is_err());
    assert_eq!(
        input::encode_paste(&"a".repeat(65524), &modes)
            .unwrap()
            .len(),
        65536
    );
    assert!(input::encode_paste("\x1b]52;c;attack\x07", &modes).is_err());
    modes.focus_reporting = true;
    assert_eq!(input::encode_focus(false, &modes), b"\x1b[O");
    modes.mouse_click = true;
    modes.mouse_sgr = true;
    let event = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 11,
        row: 6,
        modifiers: KeyModifiers::CONTROL,
    };
    assert_eq!(
        input::encode_mouse(event, &modes, Rect::new(10, 5, 20, 10)).unwrap(),
        b"\x1b[<16;2;2M"
    );
    assert!(input::encode_mouse(event, &modes, Rect::new(20, 5, 20, 10)).is_none());
}

fn blank() -> TerminalCell {
    TerminalCell {
        text: " ".into(),
        fg: TerminalColor::Default,
        bg: TerminalColor::Default,
        bold: false,
        dim: false,
        italic: false,
        underline: false,
        inverse: false,
        strike: false,
        hidden: false,
        wide: false,
        wide_spacer: false,
    }
}
fn snapshot(rows: u16, cols: u16) -> TerminalSnapshot {
    TerminalSnapshot {
        generation: 7,
        rows,
        cols,
        cells: vec![blank(); usize::from(rows) * usize::from(cols)],
        cursor: None,
        display_offset: 0,
        modes: TerminalModes::default(),
        title: String::new(),
        reader_closed: false,
        error: None,
        output_limited: false,
    }
}
#[test]
fn wide_cells_repaint_after_narrow_resize_and_control_text_is_never_replayed() {
    let mut terminal = Terminal::new(TestBackend::new(6, 2)).unwrap();
    let mut source = snapshot(2, 6);
    source.cells[0].text = "한".into();
    source.cells[0].wide = true;
    source.cells[1].wide_spacer = true;
    terminal
        .draw(|frame| screen::render(frame, frame.area(), &source, true))
        .unwrap();
    assert_eq!(terminal.backend().buffer()[(0, 0)].symbol(), "한");
    terminal.backend_mut().resize(2, 2);
    terminal.resize(Rect::new(0, 0, 2, 2)).unwrap();
    let mut narrow = snapshot(2, 2);
    narrow.cells[0].text = "a".into();
    narrow.cells[1].text = "b".into();
    terminal
        .draw(|frame| screen::render(frame, frame.area(), &narrow, true))
        .unwrap();
    assert_eq!(terminal.backend().buffer()[(1, 0)].symbol(), "b");
    terminal.backend_mut().resize(6, 2);
    terminal.resize(Rect::new(0, 0, 6, 2)).unwrap();
    source.cells[1] = blank();
    source.cells[0] = blank();
    source.cells[1].text = "Z\x1b\x07".into();
    source.cells[2].text = "secret".into();
    source.cells[2].hidden = true;
    terminal
        .draw(|frame| screen::render(frame, frame.area(), &source, true))
        .unwrap();
    assert_eq!(terminal.backend().buffer()[(1, 0)].symbol(), "Z");
    assert_eq!(terminal.backend().buffer()[(2, 0)].symbol(), " ");
    source.cells.pop();
    assert!(screen::validate(&source).is_err());
}

struct Fixture {
    temp: tempfile::TempDir,
    store: Store,
    root: PathBuf,
    environment: LaunchEnvironment,
    project: String,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("한글 project");
        let home = temp.path().join("home");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&home).unwrap();
        let shell = std::env::var_os("IDK_TEST_SHELL")
            .map(PathBuf::from)
            .unwrap_or_else(|| "/usr/bin/tcsh".into());
        assert!(shell.is_file(), "set IDK_TEST_SHELL to real tcsh");
        let setup = root.join("setup.csh");
        std::fs::write(
            &setup,
            "set saved_value = retained\necho init >> source-count\nset prompt = 'IDK_PROMPT> '\n",
        )
        .unwrap();
        let environment = LaunchEnvironment::from_variables(BTreeMap::from([
            ("HOME".into(), home.to_string_lossy().into()),
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("TERM".into(), "xterm-256color".into()),
            ("LANG".into(), "C.UTF-8".into()),
        ]))
        .unwrap();
        let store = Store::open(Some(&temp.path().join("data"))).unwrap();
        let service = ProjectService { store: &store };
        let preview = service
            .preview_connect(ConnectDraft {
                name: "UI runtime".into(),
                root: root.clone(),
                shell: ShellConfig {
                    executable: shell,
                    login: false,
                    init_cwd: root.clone(),
                    sources: vec![setup.into()],
                    trusted_digest: None,
                },
                terminals: vec![TerminalDraft {
                    name: "Development".into(),
                    cwd: root.clone(),
                    sources: vec![],
                    persistent: true,
                }],
            })
            .unwrap();
        let project = service.create(preview.revision, preview).unwrap().id;
        let review = service
            .review_initialization(&project, &environment)
            .unwrap();
        service
            .approve_initialization(review.revision, review)
            .unwrap();
        Self {
            temp,
            store,
            root,
            environment,
            project,
        }
    }
    fn ui(&self, attach: Option<&str>) -> TerminalSession {
        let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_idk"));
        command.arg("--data-dir");
        command.arg(self.temp.path().join("data"));
        if let Some(id) = attach {
            command.args(["session", "attach", id]);
        }
        command.cwd(&self.root);
        command.env_clear();
        for (key, value) in self.environment.variables() {
            command.env(key, value);
        }
        TerminalSession::spawn(command, 28, 100, 1000).unwrap()
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
            if let Ok(preview) = client.preview_close(None) {
                let ids = preview
                    .targets
                    .into_iter()
                    .map(|session| session.session_id)
                    .collect::<Vec<_>>();
                let _ = client.shutdown(&ids, true);
            }
        }
    }
}
fn wait_screen(session: &TerminalSession, needle: &str) {
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        let screen = session.snapshot().unwrap();
        let text = screen.text();
        if text.contains(needle) {
            return;
        }
        assert!(Instant::now() < deadline, "missing {needle:?}: {text}");
        std::thread::sleep(Duration::from_millis(25));
    }
}
fn wait_exit(session: &mut TerminalSession) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(exit) = session.try_wait().unwrap() {
            assert_eq!(exit.code, 0);
            return;
        }
        assert!(Instant::now() < deadline, "UI did not detach");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn actual_ui_preserves_shell_state_across_detach_resize_and_orphan_attach() {
    let fixture = Fixture::new();
    let mut ui = fixture.ui(None);
    wait_screen(&ui, "Terminal definitions");
    ui.input(b"\r").unwrap();
    wait_screen(&ui, "IDK_PROMPT>");
    let mut client = fixture.client();
    let initial = client.list(Some(&fixture.project)).unwrap().pop().unwrap();
    ui.input(b"echo $saved_value\r").unwrap();
    wait_screen(&ui, "retained");
    std::fs::write(
        fixture.root.join("keys.py"),
        r#"import os,sys,termios,tty
old=termios.tcgetattr(0)
try:
 tty.setraw(0)
 os.write(1,b'\x1b[?1h\x1b[?2004hKEY_CAPTURE_READY')
 data=b''
 while len(data)<8:
  data+=os.read(0,8-len(data))
 open('captured-keys','wb').write(data)
 os.write(1,b'\x1b[?1l\x1b[?2004l\r\nKEY_CAPTURE_DONE\r\n')
finally:
 termios.tcsetattr(0,termios.TCSANOW,old)
"#,
    )
    .unwrap();
    ui.input(b"python3 keys.py\r").unwrap();
    wait_screen(&ui, "KEY_CAPTURE_READY");
    ui.input(b"\x1bOP\x1b[A\t\x07\x07").unwrap();
    wait_screen(&ui, "KEY_CAPTURE_DONE");
    assert_eq!(
        std::fs::read(fixture.root.join("captured-keys")).unwrap(),
        b"\x1bOP\x1bOA\t\x07"
    );
    std::fs::write(
        fixture.root.join("full.py"),
        r#"import os,time,termios,tty
old=termios.tcgetattr(0)
try:
 tty.setraw(0)
 os.write(1,'\x1b[?1049h\x1b[2J\x1b[H전체화면 한글\x1b[31m RED\x1b[0m'.encode())
 os.read(0,1)
 os.write(1,b'\x1b[?1049lFULLSCREEN_DONE\r\n')
finally:
 termios.tcsetattr(0,termios.TCSANOW,old)
"#,
    )
    .unwrap();
    ui.input(b"python3 full.py\r").unwrap();
    wait_screen(&ui, "전체화면 한글");
    ui.resize(18, 48).unwrap();
    wait_screen(&ui, "전체화면 한글");
    ui.resize(28, 100).unwrap();
    wait_screen(&ui, "전체화면 한글");
    ui.input(b"x").unwrap();
    wait_screen(&ui, "FULLSCREEN_DONE");
    // Real Ctrl+Z/fg and Ctrl+C traverse the outer UI and reach the foreground PTY.
    ui.input(b"sleep 30\r").unwrap();
    std::thread::sleep(Duration::from_millis(200));
    ui.input(b"\x1a").unwrap();
    wait_screen(&ui, "Suspended");
    ui.input(b"fg\r").unwrap();
    std::thread::sleep(Duration::from_millis(200));
    ui.input(b"\x03").unwrap();
    ui.input("echo 한글표시\r".as_bytes()).unwrap();
    wait_screen(&ui, "한글표시");
    ui.resize(18, 48).unwrap();
    wait_screen(&ui, "Ctrl+g");
    ui.resize(28, 100).unwrap();
    ui.input(b"\x07q").unwrap();
    wait_exit(&mut ui);
    let detached = client
        .list(None)
        .unwrap()
        .into_iter()
        .find(|session| session.session_id == initial.session_id)
        .unwrap();
    assert_eq!(detached.child_pid, initial.child_pid);
    assert!(detached.state.is_live());
    assert!(
        detached.owner.is_none(),
        "normal detach must release ownership"
    );
    let service = ProjectService {
        store: &fixture.store,
    };
    let workspace = fixture.store.load().unwrap();
    service
        .remove_definition(workspace.revision, &fixture.project)
        .unwrap();
    let mut ui = fixture.ui(Some(&initial.session_id));
    wait_screen(&ui, "IDK_PROMPT>");
    ui.input(b"echo $saved_value\r").unwrap();
    wait_screen(&ui, "retained");
    ui.input(b"\x07q").unwrap();
    wait_exit(&mut ui);
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("source-count"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[test]
fn slow_host_handshake_does_not_block_forms_or_quit() {
    use std::os::unix::{fs::PermissionsExt, net::UnixListener};
    let fixture = Fixture::new();
    let listener = UnixListener::bind(fixture.store.socket_path()).unwrap();
    std::fs::set_permissions(
        fixture.store.socket_path(),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let server = std::thread::spawn(move || {
        let (_stream, _) = listener.accept().unwrap();
        std::thread::sleep(Duration::from_millis(700));
    });
    let mut app = App::with_environment(&fixture.store, fixture.environment.clone()).unwrap();
    let before = Instant::now();
    app.enable_terminal_runtime(PathBuf::from(env!("CARGO_BIN_EXE_idk")))
        .unwrap();
    app.handle_event(Event::Key(KeyEvent::new(
        KeyCode::Char('n'),
        KeyModifiers::NONE,
    )))
    .unwrap();
    let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
    terminal.draw(|frame| app.render(frame)).unwrap();
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE))
            .unwrap(),
        UiOutcome::Quit
    );
    assert!(before.elapsed() < Duration::from_millis(200));
    drop(app);
    server.join().unwrap();
    std::fs::remove_file(fixture.store.socket_path()).unwrap();
}

fn app_screen(app: &mut App<'_>) -> String {
    let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
    terminal.draw(|frame| app.render(frame)).unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>()
}
fn wait_app(app: &mut App<'_>, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        app.tick();
        let text = app_screen(app);
        if text.contains(expected) {
            return;
        }
        assert!(Instant::now() < deadline, "missing {expected}: {text}");
        std::thread::sleep(Duration::from_millis(20));
    }
}
#[test]
fn clipboard_is_user_opt_in_and_lost_ownership_keeps_keys_out_of_menu_actions() {
    let fixture = Fixture::new();
    let mut app = App::with_environment(&fixture.store, fixture.environment.clone()).unwrap();
    app.enable_terminal_runtime(PathBuf::from(env!("CARGO_BIN_EXE_idk")))
        .unwrap();
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    wait_app(&mut app, "IDK_PROMPT>");
    app.forward_terminal(UiOutcome::ForwardTerminalPaste(
        "printf '\\033]52;c;YXR0YWNr\\007'; echo COPY_VISIBLE\n".into(),
    ))
    .unwrap();
    wait_app(&mut app, "COPY_VISIBLE");
    assert!(app.take_clipboard_request().is_none());
    app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL))
        .unwrap();
    app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE))
        .unwrap();
    assert!(app_screen(&mut app).contains("Plain copy preview"));
    assert!(app.take_clipboard_request().is_none());
    app.handle_key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE))
        .unwrap();
    let copied = app.take_clipboard_request().unwrap();
    assert!(copied.contains("COPY_VISIBLE"));
    assert!(app.take_clipboard_request().is_none());
    app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL))
        .unwrap();
    let mut other = fixture.client();
    let session = other.list(None).unwrap().pop().unwrap();
    other.attach(&session.session_id, true).unwrap();
    wait_app(&mut app, "READ ONLY");
    let key = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
    let outcome = app.handle_key(key).unwrap();
    assert_eq!(outcome, UiOutcome::ForwardTerminalKey(key));
    assert!(app.forward_terminal(outcome).is_err());
    assert_eq!(fixture.store.load().unwrap().projects[0].terminals.len(), 1);
}

#[test]
fn temporary_shell_close_and_reopen_require_confirmation_and_keep_definition_identity() {
    let fixture = Fixture::new();
    let mut app = App::with_environment(&fixture.store, fixture.environment.clone()).unwrap();
    fn key(app: &mut App<'_>, code: KeyCode) {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
            .unwrap();
    }
    key(&mut app, KeyCode::Char('n'));
    app.handle_event(Event::Paste("Temporary shell".into()))
        .unwrap();
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Char(' '));
    key(&mut app, KeyCode::F(2));
    key(&mut app, KeyCode::F(2));
    let definition = app.selected_terminal_definition().unwrap();
    assert!(!definition.persistent);
    key(&mut app, KeyCode::Char('t'));
    key(&mut app, KeyCode::F(2));
    app.enable_terminal_runtime(PathBuf::from(env!("CARGO_BIN_EXE_idk")))
        .unwrap();
    key(&mut app, KeyCode::Enter);
    wait_app(&mut app, "IDK_PROMPT>");
    let mut client = fixture.client();
    let original = client.list(None).unwrap().pop().unwrap();
    assert_eq!(original.terminal_id, definition.id);
    app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL))
        .unwrap();
    key(&mut app, KeyCode::Char('x'));
    assert!(app_screen(&mut app).contains("Close selected shell"));
    key(&mut app, KeyCode::Esc);
    assert!(client.list(None).unwrap()[0].state.is_live());
    key(&mut app, KeyCode::Char('x'));
    key(&mut app, KeyCode::F(2));
    wait_app(&mut app, "Closed");
    key(&mut app, KeyCode::Enter);
    assert!(app_screen(&mut app).contains("Reopen terminal with a new shell"));
    key(&mut app, KeyCode::Esc);
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("source-count"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    key(&mut app, KeyCode::Enter);
    key(&mut app, KeyCode::F(2));
    wait_app(&mut app, "IDK_PROMPT>");
    let reopened = client.list(None).unwrap().pop().unwrap();
    assert_ne!(reopened.session_id, original.session_id);
    assert_eq!(reopened.terminal_id, definition.id);
    assert_ne!(reopened.child_pid, original.child_pid);
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("source-count"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    assert_eq!(fixture.store.load().unwrap().projects[0].terminals.len(), 1);
}
