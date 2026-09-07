//! Finite, synthetic compatibility checks executed by the distributed binary.
use crate::shell::{InitializationState, ShellPlan};
use crate::store::ensure_private_dir;
use crate::terminal::TerminalSession;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

struct Sandbox(PathBuf);
impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn wait_line(session: &TerminalSession, line: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let snapshot = session.snapshot()?;
        if let Some(error) = snapshot.error {
            bail!("terminal I/O failed: {error}");
        }
        if snapshot.text().lines().any(|text| text.trim() == line) {
            return Ok(());
        }
        if Instant::now() > deadline || snapshot.reader_closed {
            bail!("synthetic terminal check did not produce {line:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_exit(session: &mut TerminalSession) -> Result<u32> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(exit) = session.try_wait()? {
            return Ok(exit.code);
        }
        if Instant::now() > deadline {
            let _ = session.terminate();
            bail!("synthetic shell did not exit");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub fn run(shell: &Path) -> Result<Value> {
    let shell = shell
        .canonicalize()
        .context("selected test shell is unavailable")?;
    let root = std::env::temp_dir().join(format!("idk-probe-{}", crate::model::new_id()));
    ensure_private_dir(&root)?;
    let _sandbox = Sandbox(root.clone());
    let home = root.join("home");
    let init = root.join("init");
    let start = root.join("test 한글");
    for path in [&home, &init, &start] {
        ensure_private_dir(path)?;
    }
    let source = init.join("setup.csh");
    std::fs::write(&source, "alias idk_probe_alias 'echo IDK_ALIAS_OK'\nset idk_local = kept\nsetenv IDK_PROBE_ENV exported\necho once >> initializations\n")?;
    let launcher = std::env::current_exe()?;
    let environment = BTreeMap::from([
        ("HOME".into(), home.to_string_lossy().into_owned()),
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("TERM".into(), "xterm-256color".into()),
        ("LANG".into(), "C.UTF-8".into()),
    ]);
    let mut plan = ShellPlan {
        shell: shell.clone(),
        login: false,
        init_cwd: init.clone(),
        start_cwd: start.clone(),
        sources: vec![source],
        env: environment,
        command: None,
    };
    let prepared = plan.prepare(&root, &launcher)?;
    let mut session = TerminalSession::spawn(prepared.command.clone(), 24, 100, 2000)?;
    session.input(&prepared.bootstrap_bytes)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match prepared.state()? {
            InitializationState::Ready => break,
            InitializationState::Failed => {
                let _ = session.terminate();
                bail!("synthetic shell initialization failed");
            }
            InitializationState::Initializing => {}
        }
        if session.try_wait()?.is_some() || Instant::now() > deadline {
            let _ = session.terminate();
            bail!("synthetic shell initialization incomplete");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let pid = session.child_pid();
    session.input(b"idk_probe_alias\recho IDK_VALUES:${idk_local}:${IDK_PROBE_ENV}\r")?;
    wait_line(&session, "IDK_ALIAS_OK")?;
    wait_line(&session, "IDK_VALUES:kept:exported")?;
    if session.cwd().as_ref() != Some(&start) {
        bail!("shell start cwd did not match definition");
    }
    session.resize(31, 105)?;
    session.input(b"stty size\r")?;
    wait_line(&session, "31 105")?;
    // The reader remains active without any attached UI or snapshot requests.
    session.input(b"sleep 0.1; idk_probe_alias; printf 'IDK_%s\\n' DETACHED\r")?;
    std::thread::sleep(Duration::from_millis(250));
    wait_line(&session, "IDK_DETACHED")?;
    if session.child_pid() != pid
        || std::fs::read_to_string(init.join("initializations"))?
            .lines()
            .count()
            != 1
    {
        bail!("detach repeated initialization or replaced the shell");
    }
    session.input(b"sleep 20\r")?;
    std::thread::sleep(Duration::from_millis(120));
    session.input(&[26])?;
    std::thread::sleep(Duration::from_millis(120));
    session.input(b"fg\r")?;
    std::thread::sleep(Duration::from_millis(120));
    session.input(&[3])?;
    session.input(b"printf 'IDK_%s\\n' JOB_CONTROL\r")?;
    wait_line(&session, "IDK_JOB_CONTROL")?;
    session.input(b"exit 0\r")?;
    if wait_exit(&mut session)? != 0 {
        bail!("synthetic terminal returned failure");
    }

    plan.command = Some(
        "idk_probe_alias\necho IDK_TASK:${idk_local}:${IDK_PROBE_ENV}\n/bin/sh -c 'exit 7'".into(),
    );
    let prepared_task = plan.prepare(&root, &launcher)?;
    let mut task = TerminalSession::spawn(prepared_task.command, 24, 100, 2000)?;
    task.input(&prepared_task.bootstrap_bytes)?;
    let code = wait_exit(&mut task)?;
    if code != 7 {
        bail!("registered task exit was {code}; expected actual exit 7");
    }
    let task_screen = task.snapshot()?;
    if !task_screen.text().contains("IDK_TASK:kept:exported") {
        bail!("registered task lost initialized shell state");
    }
    Ok(json!({
        "schema":1, "version":env!("CARGO_PKG_VERSION"), "shell":shell,
        "checks":{"same_shell_state":"PASS", "init_start_cwd":"PASS", "resize":"PASS", "detached_output":"PASS", "job_control":"PASS", "task_exit_code":"PASS"},
        "target_field_acceptance":"not performed; synthetic local/package probe only"
    }))
}
