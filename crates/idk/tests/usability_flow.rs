//! Actual default/path entry and first-run workflows, independent of ici/Lens.
#[path = "common/launcher.rs"]
mod launcher;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use idk_workspace::{client::Client, store::Store, terminal::TerminalSession, ui::input};
use portable_pty::CommandBuilder;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{Duration, Instant},
};
struct Fixture {
    temp: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    store: Store,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("source space");
        let home = temp.path().join("home");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&home).unwrap();
        let store = Store::open(Some(&temp.path().join("data"))).unwrap();
        Self {
            temp,
            root,
            home,
            store,
        }
    }
    fn invoke(&self, args: &[&str]) -> Output {
        Command::new(launcher::path())
            .arg("--data-dir")
            .arg(self.temp.path().join("data"))
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.home)
            .env("SHELL", "/bin/bash")
            .env("PATH", "/usr/bin:/bin")
            .output()
            .unwrap()
    }
    fn ok(&self, args: &[&str]) -> String {
        let out = self.invoke(args);
        assert!(
            out.status.success(),
            "{:?}: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }
    fn ui(&self, path: &Path) -> TerminalSession {
        let mut cmd = CommandBuilder::new(launcher::path());
        cmd.arg("--data-dir");
        cmd.arg(self.temp.path().join("data"));
        cmd.arg(path);
        cmd.cwd(&self.root);
        cmd.env_clear();
        cmd.env("HOME", &self.home);
        cmd.env("SHELL", "/bin/bash");
        cmd.env("PATH", "/usr/bin:/bin");
        cmd.env("TERM", "xterm-256color");
        TerminalSession::spawn(cmd, 36, 130, 2000).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Ok(mut client) = Client::connect_with_launcher(&self.store, launcher::path()) {
            if let Ok(sessions) = client.list(None) {
                let _ = client.shutdown(
                    &sessions
                        .iter()
                        .map(|s| s.session_id.clone())
                        .collect::<Vec<_>>(),
                    true,
                );
            }
        }
    }
}
fn wait(ui: &TerminalSession, text: &str) {
    let until = Instant::now() + Duration::from_secs(12);
    loop {
        let screen = ui.snapshot().unwrap().text();
        if screen.contains(text) {
            return;
        }
        assert!(Instant::now() < until, "missing {text}:\n{screen}");
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn key(ui: &mut TerminalSession, code: KeyCode, modifiers: KeyModifiers) {
    let modes = ui.snapshot().unwrap().modes;
    ui.input(&input::encode_key(KeyEvent::new(code, modifiers), &modes).unwrap())
        .unwrap();
}
fn quit(ui: &mut TerminalSession) {
    key(ui, KeyCode::Char('g'), KeyModifiers::CONTROL);
    key(ui, KeyCode::Char('q'), KeyModifiers::NONE);
    let until = Instant::now() + Duration::from_secs(5);
    while ui.try_wait().unwrap().is_none() {
        assert!(Instant::now() < until, "UI did not exit");
        std::thread::sleep(Duration::from_millis(20));
    }
}
#[test]
fn config_defaults_partial_updates_and_external_typo_preserve_original() {
    let f = Fixture::new();
    let shown = f.ok(&["config", "show"]);
    assert!(shown.contains("source space"));
    assert!(!f.root.join("idk.toml").exists());
    assert!(f.store.load().unwrap().projects.is_empty());
    f.ok(&["config", "init"]);
    assert_eq!(
        fs::read_to_string(f.root.join("idk.toml")).unwrap(),
        "schema_version = 1\n"
    );
    f.ok(&["config", "set", "shell.env.A", "'one'"]);
    f.ok(&["config", "set", "shell.env.B", "'two'"]);
    f.ok(&["config", "set", "shell.args", "[]"]);
    f.ok(&["config", "unset", "shell.env.A"]);
    let raw = fs::read_to_string(f.root.join("idk.toml")).unwrap();
    let value: toml::Value = toml::from_str(&raw).unwrap();
    assert_eq!(value["shell"]["env"]["B"].as_str(), Some("two"));
    assert!(value["shell"]["env"].get("A").is_none());
    f.ok(&["config", "sync"]);
    let before = f.store.load().unwrap();
    fs::write(
        f.root.join("idk.toml"),
        "schema_version=1\n[tasks.test]\nprogam='oops'\n",
    )
    .unwrap();
    let out = f.invoke(&["config", "sync"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("progam"));
    assert_eq!(before, f.store.load().unwrap());
}
#[test]
fn path_entry_keeps_shell_pid_cwd_variables_and_startup_once_after_ui_reconnect() {
    let f = Fixture::new();
    let rc = f.root.join("startup.bash");
    fs::write(&rc, "printf once >> startup-count\nPS1='IDK_PROMPT> '\n").unwrap();
    fs::write(
        f.root.join("idk.toml"),
        format!(
            "schema_version=1\n[shell]\nprogram='/bin/bash'\nargs=['--rcfile',{},'-i']\n",
            toml::Value::String(rc.to_string_lossy().into_owned())
        ),
    )
    .unwrap();
    let mut ui = f.ui(Path::new("."));
    wait(&ui, "Review initialization");
    assert!(!f.root.join("startup-count").exists());
    key(&mut ui, KeyCode::F(2), KeyModifiers::NONE);
    wait(&ui, "IDK_PROMPT>");
    ui.input(b"export IDK_FLOW_VALUE=retained; mkdir -p sub; cd sub; printf '%s' \"$$\" > ../shell-pid\n").unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    while !f.root.join("shell-pid").exists() {
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(20));
    }
    let pid = fs::read_to_string(f.root.join("shell-pid")).unwrap();
    quit(&mut ui);
    drop(ui);
    let mut ui = f.ui(&f.root);
    wait(&ui, "IDK_PROMPT>");
    ui.input(b"printf '%s|%s|%s' \"$$\" \"$IDK_FLOW_VALUE\" \"$PWD\" > ../reconnect\n")
        .unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    while !f.root.join("reconnect").exists() {
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        fs::read_to_string(f.root.join("reconnect")).unwrap(),
        format!("{pid}|retained|{}", f.root.join("sub").display())
    );
    assert_eq!(
        fs::read_to_string(f.root.join("startup-count")).unwrap(),
        "once"
    );
    quit(&mut ui);
}
#[test]
fn first_run_approve_executes_literal_process_without_external_products() {
    let f = Fixture::new();
    fs::write(
        f.root.join("idk.toml"),
        "schema_version=1\n[tasks.test]\nprogram='/bin/sh'\nargs=['-c','printf literal > ran']\n",
    )
    .unwrap();
    f.ok(&["config", "sync"]);
    assert!(!f.root.join("ran").exists());
    let out = f.invoke(&["run", "start", "source space", "test", "--wait"]);
    assert!(!out.status.success());
    assert!(!f.root.join("ran").exists());
    f.ok(&[
        "run",
        "start",
        "source space",
        "test",
        "--approve",
        "--wait",
    ]);
    assert_eq!(fs::read_to_string(f.root.join("ran")).unwrap(), "literal");
    fs::remove_file(f.root.join("ran")).unwrap();
    f.ok(&["run", "start", "source space", "test", "--wait"]);
    assert!(f.root.join("ran").exists());
}

#[test]
fn preferences_import_rejects_special_and_oversize_files_without_touching_saved_values() {
    let f = Fixture::new();
    let fixture = f.temp.path().join("preferences.toml");
    fs::write(
        &fixture,
        "schema_version=1\n[editor]\nprogram='/bin/true'\n",
    )
    .unwrap();
    f.ok(&["config", "preferences", fixture.to_str().unwrap()]);
    let saved = fs::read(f.store.config_dir.join("preferences.toml")).unwrap();
    let huge = f.temp.path().join("huge.toml");
    fs::write(&huge, "x".repeat(1024 * 1024 + 1)).unwrap();
    let fifo = f.temp.path().join("preferences.fifo");
    let fifo_name = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);
    for path in [Path::new("/dev/zero"), huge.as_path(), fifo.as_path()] {
        let mut child = Command::new(launcher::path())
            .arg("--data-dir")
            .arg(f.temp.path().join("data"))
            .args(["config", "preferences"])
            .arg(path)
            .env("HOME", &f.home)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(!status.success());
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("preferences import must terminate for {}", path.display());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            fs::read(f.store.config_dir.join("preferences.toml")).unwrap(),
            saved
        );
    }
}

#[test]
fn default_current_folder_entry_starts_without_configuration_or_manual_identifiers() {
    let f = Fixture::new();
    let mut ui = f.ui(Path::new("."));
    let deadline = Instant::now() + Duration::from_secs(10);
    let prompt = loop {
        if let Ok(mut client) = Client::connect_with_launcher(&f.store, launcher::path()) {
            if let Some(session) = client
                .list(None)
                .unwrap()
                .iter()
                .find(|session| session.owner.is_some())
            {
                if let Some(screen) = client.snapshot(&session.session_id, None).unwrap().screen {
                    let text = screen.text();
                    if !text.trim().is_empty() {
                        break text.trim().to_owned();
                    }
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "default entry did not attach: {}",
            ui.snapshot().unwrap().text()
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    // Wait until the UI has received the first actual screen, independently of host-side attach completion.
    wait(&ui, &prompt);
    ui.input(b"printf '%s' \"$PWD\" > default-entry\n").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !f.root.join("default-entry").exists() {
        assert!(
            Instant::now() < deadline,
            "default command not observed: {}",
            ui.snapshot().unwrap().text()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        fs::read_to_string(f.root.join("default-entry")).unwrap(),
        f.root.to_string_lossy()
    );
    assert!(!f.root.join("idk.toml").exists());
    let workspace = f.store.load().unwrap();
    assert_eq!(workspace.projects.len(), 1);
    assert_eq!(workspace.projects[0].name, "source space");
    quit(&mut ui);
}
