use std::time::{Duration, Instant};

use idk_workspace::terminal::{TerminalSession, TerminalSnapshot};
use portable_pty::CommandBuilder;

fn wait_for(session: &TerminalSession, predicate: impl Fn(&TerminalSnapshot) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let snapshot = session.snapshot().unwrap();
        assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
        if predicate(&snapshot) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out; visible screen: {:?}",
            snapshot.text()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn has_line(snapshot: &TerminalSnapshot, line: &str) -> bool {
    snapshot.text().lines().any(|actual| actual.trim() == line)
}

/// Explicitly ignored by ordinary unit checks; CI/native verification must set
/// IDK_TEST_SHELL to a real tcsh executable and run this test with --ignored.
#[test]
#[ignore = "requires actual tcsh; set IDK_TEST_SHELL and run --ignored"]
fn actual_tcsh_preserves_state_job_control_resize_and_detached_output() {
    let shell = std::env::var_os("IDK_TEST_SHELL")
        .expect("IDK_TEST_SHELL must name the actual tcsh binary");
    let directory = tempfile::tempdir().unwrap();
    let korean = directory.path().join("한글 작업");
    std::fs::create_dir(&korean).unwrap();
    let mut command = CommandBuilder::new(shell);
    command.args(["-f", "-i"]);
    command.cwd(directory.path());
    command.env("TERM", "xterm-256color");
    command.env("LANG", "C.UTF-8");
    let mut session = TerminalSession::spawn(command, 24, 100, 1000).unwrap();
    let original_pid = session.child_pid().unwrap();
    session
        .input(b"set prompt = ''; set kept = state; alias kept_alias 'echo ALIAS_$kept'\r")
        .unwrap();
    session.input(b"kept_alias\r").unwrap();
    wait_for(&session, |screen| has_line(screen, "ALIAS_state"));
    session
        .input(format!("cd '{}'\r", korean.display()).as_bytes())
        .unwrap();
    session.input(b"printf 'READY_%s\\n' cwd\r").unwrap();
    wait_for(&session, |screen| has_line(screen, "READY_cwd"));
    assert_eq!(session.cwd().unwrap(), korean);

    // No snapshots/client attached while the original shell continues working.
    session
        .input(b"sleep 0.15; kept_alias; printf 'DETACHED_%s\\n' done\r")
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(session.child_pid(), Some(original_pid));
    wait_for(&session, |screen| has_line(screen, "DETACHED_done"));

    session.resize(30, 110).unwrap();
    session.input(b"stty size\r").unwrap();
    wait_for(&session, |screen| has_line(screen, "30 110"));
    session.input(b"sleep 30\r").unwrap();
    std::thread::sleep(Duration::from_millis(100));
    session.input(&[26]).unwrap();
    wait_for(&session, |screen| {
        screen.text().contains("Suspended") || screen.text().contains("Stopped")
    });
    session.input(b"fg\r").unwrap();
    std::thread::sleep(Duration::from_millis(100));
    session.input(&[3]).unwrap();
    session.input(b"printf 'JOB_%s\\n' recovered\r").unwrap();
    wait_for(&session, |screen| has_line(screen, "JOB_recovered"));

    // Output flooding cannot starve Ctrl-C or grow scrollback without a bound.
    session.input(b"yes output\r").unwrap();
    wait_for(&session, |screen| has_line(screen, "output"));
    std::thread::sleep(Duration::from_millis(120));
    let interrupted = Instant::now();
    session.input(&[3]).unwrap();
    session.input(b"printf 'FLOOD_%s\\n' recovered\r").unwrap();
    loop {
        let snapshot = session.snapshot().unwrap();
        assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
        assert!(
            interrupted.elapsed() <= Duration::from_secs(2),
            "Ctrl+C to input recovery exceeded 2000 ms: {} ms",
            interrupted.elapsed().as_millis()
        );
        if has_line(&snapshot, "FLOOD_recovered") {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    eprintln!(
        "FLOOD_INPUT_RECOVERY_MS={}",
        interrupted.elapsed().as_millis()
    );
    session.scroll(i32::MAX).unwrap();
    assert!(session.snapshot().unwrap().display_offset <= 1000);
    session.scroll(i32::MIN).unwrap();
    session.input(b"exit 7\r").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(exit) = session.try_wait().unwrap() {
            assert_eq!(exit.code, 7);
            break;
        }
        assert!(Instant::now() < deadline, "tcsh did not exit");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(session.cwd().is_none());
    assert!(session.input(b"echo MUST_NOT_RUN\r").is_err());
    assert_eq!(session.try_wait().unwrap().unwrap().code, 7);
}

#[test]
fn terminal_rejects_invalid_allocation_bounds_before_spawn() {
    for (rows, cols, history) in [
        (0, 80, 100),
        (24, 0, 100),
        (24, 1, 100),
        (u16::MAX, u16::MAX, 0),
        (24, 80, usize::MAX),
    ] {
        assert!(TerminalSession::spawn(
            CommandBuilder::new("/does/not/exist"),
            rows,
            cols,
            history
        )
        .is_err());
    }
}

#[test]
fn final_synchronized_output_is_flushed_at_pty_eof() {
    let mut command = CommandBuilder::new("/bin/sh");
    command.args(["-c", "printf '\\033[?2026hFINAL_OUTPUT'; exit 7"]);
    let mut session = TerminalSession::spawn(command, 24, 80, 100).unwrap();
    wait_for(&session, |screen| screen.reader_closed);
    assert!(session.snapshot().unwrap().text().contains("FINAL_OUTPUT"));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(exit) = session.try_wait().unwrap() {
            assert_eq!(exit.code, 7);
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn drop_reaps_exited_child_without_requiring_explicit_poll() {
    let mut command = CommandBuilder::new("/bin/sh");
    command.args(["-c", "exit 0"]);
    let session = TerminalSession::spawn(command, 24, 80, 100).unwrap();
    let pid = session.child_pid().unwrap();
    wait_for(&session, |screen| screen.reader_closed);
    drop(session);
    let deadline = Instant::now() + Duration::from_secs(2);
    while std::path::Path::new(&format!("/proc/{pid}")).exists() {
        assert!(Instant::now() < deadline, "dropped child was not reaped");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn drop_returns_promptly_and_reaper_does_not_retain_pty_descriptors() {
    let mut command = CommandBuilder::new("/bin/sh");
    // Ignore HUP so only actual terminal EOF releases this read. A reaper that
    // retains the master or writer would deadlock instead of collecting exit.
    command.args(["-c", "trap '' HUP; printf DROP_READY; read value"]);
    let session = TerminalSession::spawn(command, 24, 80, 100).unwrap();
    let pid = session.child_pid().unwrap();
    wait_for(&session, |screen| screen.text().contains("DROP_READY"));
    let start = Instant::now();
    drop(session);
    assert!(
        start.elapsed() < Duration::from_millis(250),
        "Drop blocked on the live child"
    );
    let deadline = Instant::now() + Duration::from_secs(2);
    while std::path::Path::new(&format!("/proc/{pid}")).exists() {
        assert!(
            Instant::now() < deadline,
            "reaper retained a PTY descriptor or left a zombie"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "requires actual tcsh and coreutils; set IDK_TEST_SHELL and run --ignored"]
fn actual_pty_query_reply_reaches_child() {
    let shell = std::env::var_os("IDK_TEST_SHELL").expect("IDK_TEST_SHELL must name tcsh");
    let mut command = CommandBuilder::new(shell);
    command.args(["-f", "-c", "stty -echo -icanon min 1 time 20; printf '\\033[4;9H\\033[6n'; dd bs=1 count=6 status=none | od -An -tu1"]);
    command.env("TERM", "xterm-256color");
    let mut session = TerminalSession::spawn(command, 24, 80, 100).unwrap();
    wait_for(&session, |screen| {
        screen
            .text()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .contains("27 91 52 59 57 82")
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(exit) = session.try_wait().unwrap() {
            assert_eq!(exit.code, 0);
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
#[ignore = "requires actual tcsh; set IDK_TEST_SHELL and run --ignored"]
fn terminating_owned_session_preserves_unrelated_process() {
    let shell = std::env::var_os("IDK_TEST_SHELL").expect("IDK_TEST_SHELL must name tcsh");
    let mut unrelated = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let mut command = CommandBuilder::new(shell);
    command.args(["-f", "-i"]);
    let mut session = TerminalSession::spawn(command, 24, 80, 100).unwrap();
    session
        .input(b"set prompt = ''; printf '\\nOWNED_%s\\n' ready; sleep 30\r")
        .unwrap();
    wait_for(&session, |screen| has_line(screen, "OWNED_ready"));
    session.terminate().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while session.try_wait().unwrap().is_none() {
        assert!(
            Instant::now() < deadline,
            "owned terminal did not exit after hangup"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let survived = unrelated.try_wait().unwrap().is_none();
    unrelated.kill().unwrap();
    unrelated.wait().unwrap();
    assert!(survived, "unrelated process was terminated");
}

#[test]
#[ignore = "requires actual tcsh; set IDK_TEST_SHELL and run --ignored"]
fn actual_oversized_control_output_keeps_user_interrupt_working() {
    let shell = std::env::var_os("IDK_TEST_SHELL").expect("IDK_TEST_SHELL must name tcsh");
    let directory = tempfile::tempdir().unwrap();
    for name in ["osc", "queries"] {
        let payload = directory.path().join(name);
        let bytes = if name == "osc" {
            let mut bytes = b"\x1b]2;".to_vec();
            bytes.resize(1024 * 1024, b'x');
            bytes
        } else {
            b"\x1b[c".repeat(5000)
        };
        std::fs::write(&payload, bytes).unwrap();
        let mut command = CommandBuilder::new(&shell);
        command.args(["-f", "-c"]);
        command.arg(format!("/bin/cat '{}'; /bin/sleep 30", payload.display()));
        let mut session = TerminalSession::spawn(command, 24, 80, 100).unwrap();
        wait_for(&session, |screen| screen.output_limited);
        let interrupted = Instant::now();
        session
            .input(&[3])
            .expect("output limits must never disable Ctrl-C");
        let deadline = interrupted + Duration::from_secs(2);
        while session.try_wait().unwrap().is_none() {
            assert!(
                Instant::now() < deadline,
                "child did not receive Ctrl-C after {name} flood"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            interrupted.elapsed() <= Duration::from_secs(2),
            "oversized {name} interrupt exceeded 2000 ms"
        );
        eprintln!(
            "OVERSIZED_{name}_INTERRUPT_EXIT_MS={}",
            interrupted.elapsed().as_millis()
        );
    }
}
