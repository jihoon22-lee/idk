//! Real tcsh and PTY tests. A missing tcsh is a failing prerequisite, never PASS.
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use idk_workspace::shell::{InitializationState, PreparedShell, ShellPlan};
use portable_pty::{native_pty_system, Child, MasterPty, PtySize};
use tempfile::TempDir;

fn shell() -> PathBuf {
    let candidates = [
        std::env::var_os("IDK_TEST_SHELL")
            .or_else(|| std::env::var_os("IDK_TEST_TCSH"))
            .map(PathBuf::from),
        Some(PathBuf::from("/usr/bin/tcsh")),
        Some(PathBuf::from("/bin/tcsh")),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|p| p.is_file())
        .expect("install tcsh or set IDK_TEST_SHELL to run real-shell tests")
}

fn plan(tmp: &TempDir) -> ShellPlan {
    let home = tmp.path().join("home");
    let init = tmp.path().join("init 한글");
    let start = tmp.path().join("start ' ! $ ` ;");
    for p in [&home, &init, &start] {
        std::fs::create_dir(p).unwrap();
    }
    ShellPlan {
        shell: shell(),
        login: false,
        init_cwd: init,
        start_cwd: start,
        sources: Vec::new(),
        command: None,
        env: BTreeMap::from([
            ("HOME".into(), home.to_str().unwrap().into()),
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("TERM".into(), "xterm-256color".into()),
            ("LANG".into(), "C.UTF-8".into()),
        ]),
    }
}

struct Session {
    prepared: PreparedShell,
    child: Box<dyn Child + Send + Sync>,
    _master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    output: Receiver<Vec<u8>>,
    seen: String,
}

impl Session {
    fn start(plan: &ShellPlan, tmp: &TempDir) -> Self {
        let launcher = std::env::var_os("IDK_TEST_LAUNCHER")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_idk")));
        let prepared = plan
            .prepare(&tmp.path().join("resources"), &launcher)
            .unwrap();
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 100,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let child = pair.slave.spawn_command(prepared.command.clone()).unwrap();
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().unwrap();
        let mut writer = pair.master.take_writer().unwrap();
        writer.write_all(&prepared.bootstrap_bytes).unwrap();
        let (tx, output) = mpsc::channel();
        thread::spawn(move || {
            let mut buf = [0; 4096];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        Self {
            prepared,
            child,
            _master: pair.master,
            writer,
            output,
            seen: String::new(),
        }
    }

    fn until(&mut self, needle: &str) {
        let end = Instant::now() + Duration::from_secs(8);
        while !self.seen.contains(needle) {
            assert!(
                Instant::now() < end,
                "missing {needle:?}; output: {:?}",
                self.seen
            );
            if let Ok(bytes) = self.output.recv_timeout(Duration::from_millis(40)) {
                self.seen.push_str(&String::from_utf8_lossy(&bytes));
            }
        }
    }

    fn ready(&mut self) {
        let end = Instant::now() + Duration::from_secs(8);
        while self.prepared.state().unwrap() != InitializationState::Ready {
            if let Ok(bytes) = self.output.recv_timeout(Duration::from_millis(20)) {
                self.seen.push_str(&String::from_utf8_lossy(&bytes));
            }
            assert!(
                Instant::now() < end,
                "initialization {:?}: {:?}",
                self.prepared.state(),
                self.seen
            );
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "shell exited: {:?}",
                self.seen
            );
        }
    }

    fn send(&mut self, text: &str) {
        self.writer.write_all(text.as_bytes()).unwrap();
    }

    fn exit_code(&mut self) -> u32 {
        let end = Instant::now() + Duration::from_secs(8);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status.exit_code();
            }
            assert!(Instant::now() < end, "shell did not exit: {:?}", self.seen);
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn write(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
}

#[test]
fn same_shell_preserves_alias_local_environment_nested_source_order_and_cwd() {
    let tmp = TempDir::new().unwrap();
    let mut p = plan(&tmp);
    let first = p.init_cwd.join("first ' ! $ ` ;.csh");
    let second = p.init_cwd.join("second.csh");
    write(&first, "set local_only = preserved\nsetenv TEST_ENV exported\nalias check_alias 'echo ALIAS_OK'\nset init_seen = \"$cwd\"\nset source_count = 1\nsource nested.csh\n");
    write(
        &p.init_cwd.join("nested.csh"),
        "set nested_value = nested\n",
    );
    write(
        &second,
        "@ source_count ++\nset order_seen = \"${local_only}-${nested_value}\"\n",
    );
    p.sources = vec![first.clone(), second];
    let original = std::fs::read(&first).unwrap();
    let mut s = Session::start(&p, &tmp);
    s.ready();
    s.send("check_alias\necho VALUES:${local_only}:${TEST_ENV}:${source_count}:${order_seen}\necho INIT:$init_seen\necho START:$cwd\necho PID:$$\n");
    s.until("VALUES:preserved:exported:2:preserved-nested");
    s.until("ALIAS_OK");
    s.until(&format!("INIT:{}", p.init_cwd.display()));
    s.until(&format!("START:{}", p.start_cwd.display()));
    s.send("exit\n");
    assert_eq!(s.exit_code(), 0);
    assert_eq!(std::fs::read(first).unwrap(), original);
}

#[test]
fn login_and_nonlogin_startup_use_original_home_and_rc_precedence() {
    for login in [false, true] {
        let tmp = TempDir::new().unwrap();
        let mut p = plan(&tmp);
        p.login = login;
        let home = Path::new(&p.env["HOME"]);
        write(&home.join(".cshrc"), "set chosen = cshrc\nset rc_home = \"$HOME\"\nset rc_shell_home = \"$home\"\nset rc_cwd = \"$cwd\"\nset rc_count = 1\n");
        write(&home.join(".tcshrc"), "set chosen = tcshrc\nset rc_home = \"$HOME\"\nset rc_shell_home = \"$home\"\nset rc_cwd = \"$cwd\"\nset rc_count = 1\n");
        write(
            &home.join(".login"),
            "set login_value = yes\n@ rc_count ++\n",
        );
        write(
            &home.join(".logout"),
            "echo logged_out > \"$home/logout-marker\"\n",
        );
        let mut s = Session::start(&p, &tmp);
        s.ready();
        s.send("echo RC:${chosen}:${rc_count}\necho HOME:$rc_home\necho SHELL_HOME:$rc_shell_home\necho LOGIN:$?loginsh\necho LOGIN_FILE:$?login_value\n");
        let tcsh = std::process::Command::new(&p.shell)
            .args(["-f", "-c", "echo $?tcsh"])
            .output()
            .unwrap()
            .stdout
            == b"1\n";
        s.until(&format!(
            "RC:{}:{}",
            if tcsh { "tcshrc" } else { "cshrc" },
            if login { 2 } else { 1 }
        ));
        s.until(&format!("HOME:{}", home.display()));
        s.until(&format!("SHELL_HOME:{}", home.display()));
        if tcsh {
            s.until(if login { "LOGIN:1" } else { "LOGIN:0" });
        }
        s.until(if login {
            "LOGIN_FILE:1"
        } else {
            "LOGIN_FILE:0"
        });
        s.send("exit\n");
        assert_eq!(s.exit_code(), 0);
        assert_eq!(home.join("logout-marker").exists(), login);
    }
}

#[test]
fn input_wait_cannot_consume_bootstrap_or_registered_command() {
    for task in [false, true] {
        let tmp = TempDir::new().unwrap();
        let mut p = plan(&tmp);
        let init = p.init_cwd.join("wait.csh");
        write(
            &init,
            "echo WAITING_FOR_USER\nset supplied = $<\necho RECEIVED:$supplied\n",
        );
        p.sources.push(init);
        if task {
            p.login = true;
            p.command = Some("echo TASK_AFTER:$supplied\nexit 23".into());
        }
        let mut s = Session::start(&p, &tmp);
        s.until("WAITING_FOR_USER");
        thread::sleep(Duration::from_millis(150));
        assert_eq!(
            s.prepared.state().unwrap(),
            InitializationState::Initializing
        );
        assert!(s.child.try_wait().unwrap().is_none());
        s.send("human-input\n");
        s.until("RECEIVED:human-input");
        if task {
            s.until("TASK_AFTER:human-input");
            assert_eq!(s.exit_code(), 23);
        } else {
            s.ready();
            s.send("echo STILL:$supplied\nexit\n");
            s.until("STILL:human-input");
            assert_eq!(s.exit_code(), 0);
        }
    }
}

#[test]
fn failed_initialization_never_starts_registered_task() {
    let tmp = TempDir::new().unwrap();
    let mut p = plan(&tmp);
    let init = p.init_cwd.join("fail.csh");
    write(&init, "/bin/false\n");
    p.sources.push(init);
    p.command = Some("echo SHOULD_NOT_RUN\n".into());
    let mut s = Session::start(&p, &tmp);
    assert_eq!(s.exit_code(), 125);
    assert_eq!(s.prepared.state().unwrap(), InitializationState::Failed);
}

#[test]
fn registered_task_retains_alias_local_variable_and_actual_exit_status() {
    let tmp = TempDir::new().unwrap();
    let mut p = plan(&tmp);
    p.login = true;
    let init = p.init_cwd.join("run.csh");
    write(
        &init,
        "set task_local = value\nalias do_task 'echo TASK_ALIAS:$task_local'\n",
    );
    p.sources.push(init);
    p.command = Some("do_task\n/bin/sh -c 'exit 37'".into());
    let mut s = Session::start(&p, &tmp);
    s.until("TASK_ALIAS:value");
    assert_eq!(s.exit_code(), 37);
}

#[test]
fn rejects_newline_paths_and_missing_directories_before_spawn() {
    let tmp = TempDir::new().unwrap();
    let mut p = plan(&tmp);
    let launcher = Path::new(env!("CARGO_BIN_EXE_idk"));
    p.sources.push(PathBuf::from("evil\necho INJECTED"));
    assert!(p.prepare(tmp.path(), launcher).is_err());
    p.sources.clear();
    p.start_cwd = tmp.path().join("absent");
    assert!(p.prepare(tmp.path(), launcher).is_err());
}

#[test]
fn startup_input_wait_and_builtin_aliases_do_not_redirect_initialization() {
    let tmp = TempDir::new().unwrap();
    let mut p = plan(&tmp);
    p.login = true;
    let home = Path::new(&p.env["HOME"]);
    // No .tcshrc: both shells must use this file. Generated source/cd commands
    // bypass these aliases while preserving them for the user's later work.
    write(&home.join(".cshrc"), "echo RC_WAITING\nset rc_answer = $<\nalias source 'echo USER_SOURCE_ALIAS'\nalias cd 'echo USER_CD_ALIAS'\n");
    let init = p.init_cwd.join("after-rc.csh");
    write(&init, "set from_rc = \"$rc_answer\"\n");
    p.sources.push(init);
    let mut s = Session::start(&p, &tmp);
    s.until("RC_WAITING");
    thread::sleep(Duration::from_millis(100));
    assert_eq!(
        s.prepared.state().unwrap(),
        InitializationState::Initializing
    );
    s.send("startup-answer\n");
    s.ready();
    s.send("echo RC_ANSWER:$from_rc\necho START:$cwd\nsource\ncd\nexit\n");
    s.until("RC_ANSWER:startup-answer");
    s.until(&format!("START:{}", p.start_cwd.display()));
    s.until("USER_SOURCE_ALIAS");
    s.until("USER_CD_ALIAS");
    assert_eq!(s.exit_code(), 0);
}

#[test]
fn malformed_initialization_does_not_report_ready_or_execute_task() {
    let tmp = TempDir::new().unwrap();
    let mut p = plan(&tmp);
    let init = p.init_cwd.join("malformed.csh");
    write(&init, "echo $IDK_UNDEFINED_FIXTURE_VARIABLE\n");
    p.sources.push(init);
    p.command = Some("touch TASK_STARTED".into());
    let mut s = Session::start(&p, &tmp);
    assert_ne!(s.exit_code(), 0);
    assert_ne!(s.prepared.state().unwrap(), InitializationState::Ready);
    assert!(!p.start_cwd.join("TASK_STARTED").exists());
}
