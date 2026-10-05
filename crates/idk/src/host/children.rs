//! Read-only descendant inventory plus targeted waits. The actor retains each
//! PTY leader unreaped throughout a scan, so its session ID cannot be recycled.
//! No saved PID, process name, or process-group lookup grants signal authority.
use anyhow::{ensure, Context, Result};
use std::collections::{HashSet, VecDeque};
use std::io::Read;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const MAX_MATCHES: usize = 256;
const MAX_PROC_ENTRIES: usize = 131_072;
const MAX_VISITED: usize = 4096;

pub(super) fn enable_subreaper() -> Result<()> {
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    ensure!(
        unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), &mut action) } == 0,
        "cannot inspect host child-reaping policy"
    );
    ensure!(
        action.sa_sigaction != libc::SIG_IGN && action.sa_flags & libc::SA_NOCLDWAIT == 0,
        "host requires observable child exit status; inherited SIGCHLD policy discards it"
    );
    if unsafe {
        libc::prctl(
            libc::PR_SET_CHILD_SUBREAPER,
            1 as libc::c_ulong,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error())
            .context("enable rootless host descendant reaping");
    }
    Ok(())
}

#[derive(Clone)]
pub(super) struct Anchor {
    pub session: String,
    pub pid: libc::pid_t,
}
pub(super) struct Process {
    pub pid: libc::pid_t,
    pub parent: libc::pid_t,
    pub session: libc::pid_t,
}
pub(super) struct Report {
    pub anchors: Vec<Anchor>,
    pub processes: Vec<Process>,
    pub complete: bool,
    pub error: Option<String>,
}
pub(super) fn worker() -> Result<(mpsc::SyncSender<Vec<Anchor>>, mpsc::Receiver<Report>)> {
    let (requests, incoming) = mpsc::sync_channel::<Vec<Anchor>>(1);
    let (outgoing, results) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("idk-descendant-inventory".into())
        .spawn(move || {
            while let Ok(anchors) = incoming.recv() {
                let mut report = Report {
                    anchors,
                    processes: Vec::new(),
                    complete: true,
                    error: None,
                };
                if let Err(error) = inventory(&mut report) {
                    report.complete = false;
                    report.error = Some(crate::protocol::safe_error(error));
                }
                if outgoing.send(report).is_err() {
                    break;
                }
            }
        })
        .context("start bounded descendant inventory worker")?;
    Ok((requests, results))
}
fn inventory(report: &mut Report) -> Result<()> {
    // Narrow parentage traversal stays bounded on busy hosts; a task-children
    // read failure falls back to the full session scan rather than undercount.
    if narrow_inventory(report).is_err() {
        report.processes.clear();
        full_inventory(report)?;
    }
    Ok(())
}
/// Walks anchor-session descendants through /proc task children: anchors'
/// live subtrees plus this host's own children (subreaper reparents). A
/// mid-walk failure is never swallowed — a missed descendant would fake an
/// empty inventory, so the caller retries the complete scan instead.
fn narrow_inventory(report: &mut Report) -> Result<()> {
    let sessions: HashSet<libc::pid_t> = report.anchors.iter().map(|anchor| anchor.pid).collect();
    if sessions.is_empty() {
        return Ok(());
    }
    let host = std::process::id() as libc::pid_t;
    let started = Instant::now();
    let mut seen: HashSet<libc::pid_t> = HashSet::new();
    let mut pending: VecDeque<libc::pid_t> = VecDeque::new();
    let mut roots = Vec::new();
    for anchor in &report.anchors {
        ensure!(anchor.pid > 1, "descendant inventory anchor is not a child");
        task_children(anchor.pid, &mut roots)?;
    }
    task_children(host, &mut roots)?;
    for pid in roots.drain(..) {
        enqueue(pid, &mut pending, &mut seen);
    }
    let mut visited = 0usize;
    while let Some(pid) = pending.pop_front() {
        visited += 1;
        ensure!(
            visited <= MAX_VISITED
                && report.processes.len() < MAX_MATCHES
                && started.elapsed() < Duration::from_secs(2),
            "descendant inventory reached its bounded scan limit"
        );
        let Some((parent, session)) = process_stat(pid)? else {
            continue;
        };
        task_children(pid, &mut roots)?;
        for child in roots.drain(..) {
            enqueue(child, &mut pending, &mut seen);
        }
        if sessions.contains(&session) && pid != session {
            report.processes.push(Process {
                pid,
                parent,
                session,
            });
        }
    }
    Ok(())
}
fn enqueue(pid: libc::pid_t, pending: &mut VecDeque<libc::pid_t>, seen: &mut HashSet<libc::pid_t>) {
    if seen.insert(pid) {
        pending.push_back(pid);
    }
}
fn task_children(pid: libc::pid_t, out: &mut Vec<libc::pid_t>) -> Result<()> {
    let tasks = match std::fs::read_dir(format!("/proc/{pid}/task")) {
        Ok(tasks) => tasks,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                || error.raw_os_error() == Some(libc::ESRCH) =>
        {
            return Ok(())
        }
        Err(error) => return Err(error).context("read process task list"),
    };
    for task in tasks {
        let path = task?.path().join("children");
        let mut bytes = Vec::with_capacity(256);
        match std::fs::File::open(&path).and_then(|file| file.take(8193).read_to_end(&mut bytes)) {
            Ok(_) => {}
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    || error.raw_os_error() == Some(libc::ESRCH) =>
            {
                continue
            }
            Err(error) => return Err(error).context("read task children list"),
        }
        ensure!(
            bytes.len() <= 8192,
            "process children list exceeded inventory limit"
        );
        for token in std::str::from_utf8(&bytes)?.split_whitespace() {
            if let Ok(child) = token.parse::<libc::pid_t>() {
                out.push(child);
            }
        }
    }
    Ok(())
}
/// Reads one process's parent and session IDs; a process that exited during
/// the scan is reported as absent rather than failing the whole inventory.
fn process_stat(pid: libc::pid_t) -> Result<Option<(libc::pid_t, libc::pid_t)>> {
    let mut bytes = Vec::with_capacity(1024);
    match std::fs::File::open(format!("/proc/{pid}/stat"))
        .and_then(|file| file.take(8193).read_to_end(&mut bytes))
    {
        Ok(_) => {}
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                || error.raw_os_error() == Some(libc::ESRCH) =>
        {
            return Ok(None)
        }
        Err(error) => return Err(error).context("read process session inventory"),
    }
    ensure!(
        bytes.len() <= 8192,
        "process status exceeded inventory limit"
    );
    parse_stat_fields(&bytes).map(Some)
}
/// Parses the fields after a stat comm. The comm is skipped by scanning for
/// the last ") " so embedded spaces or parentheses cannot shift later fields;
/// ppid is the first post-comm field after state, pgrp/session follow.
fn parse_stat_fields(bytes: &[u8]) -> Result<(libc::pid_t, libc::pid_t)> {
    let tail = bytes
        .windows(2)
        .rposition(|pair| pair == b") ")
        .context("invalid process status format")?
        + 2;
    let fields: Vec<_> = std::str::from_utf8(&bytes[tail..])?
        .split_whitespace()
        .take(4)
        .collect();
    ensure!(fields.len() == 4, "incomplete process status inventory");
    Ok((fields[1].parse()?, fields[3].parse()?))
}
fn full_inventory(report: &mut Report) -> Result<()> {
    let sessions: HashSet<_> = report.anchors.iter().map(|anchor| anchor.pid).collect();
    let started = Instant::now();
    for (count, entry) in std::fs::read_dir("/proc")?.enumerate() {
        ensure!(
            count < MAX_PROC_ENTRIES && started.elapsed() < Duration::from_secs(2),
            "descendant inventory reached its bounded scan limit"
        );
        let entry = entry?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<libc::pid_t>().ok())
        else {
            continue;
        };
        if pid <= 1 {
            continue;
        }
        let Some((parent, session)) = process_stat(pid)? else {
            continue;
        };
        if sessions.contains(&session) && pid != session {
            ensure!(
                report.processes.len() < MAX_MATCHES,
                "descendant inventory exceeded 256 matches; cleanup continues in bounded rounds"
            );
            report.processes.push(Process {
                pid,
                parent,
                session,
            });
        }
    }
    Ok(())
}

pub(super) enum ChildState {
    Running,
    Exited,
}
/// WNOWAIT verifies an actual direct child and leaves it unreaped, preventing
/// PID reuse between this check, the SID check, and an actor's individual signal.
pub(super) fn verify_child(pid: libc::pid_t, session: libc::pid_t) -> Result<ChildState> {
    let mut status: libc::siginfo_t = unsafe { std::mem::zeroed() };
    if unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut status,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error()).context("verify adopted child ownership");
    }
    ensure!(
        unsafe { libc::getsid(pid) } == session,
        "adopted child's session changed; preserved"
    );
    Ok(if unsafe { status.si_pid() } == 0 {
        ChildState::Running
    } else {
        ChildState::Exited
    })
}
pub(super) fn reap_child(pid: libc::pid_t) -> Result<()> {
    let mut status = 0;
    ensure!(
        unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) } == pid,
        "adopted child's exit was not reaped"
    );
    Ok(())
}
pub(super) fn signal_child(pid: libc::pid_t, force: bool) -> Result<()> {
    if unsafe { libc::kill(pid, if force { libc::SIGKILL } else { libc::SIGHUP }) } != 0 {
        return Err(std::io::Error::last_os_error()).context("signal verified adopted child");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic pseudo-random bytes: no external fuzz dependency, and a
    /// failing case replays exactly from its seed.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn byte(&mut self) -> u8 {
            (self.next() & 0xff) as u8
        }
    }

    #[test]
    fn stat_field_parser_never_panics_on_arbitrary_bytes() {
        let mut rng = Rng(0x9e3779b97f4a7c15);
        for _ in 0..20_000 {
            let len = (rng.next() % 300) as usize;
            let bytes: Vec<u8> = (0..len).map(|_| rng.byte()).collect();
            let _ = parse_stat_fields(&bytes);
        }
    }

    #[test]
    fn stat_field_parser_survives_adversarial_comm_contents() {
        // The kernel comm can contain spaces and parentheses; the last ") "
        // delimiter must keep the ppid/session fields aligned.
        let comms = [
            "plain",
            "has space",
            "has)paren",
            ") weird (",
            "a ) b ) c",
            "",
            ")))(((",
        ];
        for comm in comms {
            let bytes = format!("1234 ({comm}) S 42 43 44 45").into_bytes();
            let (parent, session) = parse_stat_fields(&bytes).unwrap();
            assert_eq!(parent, 42);
            assert_eq!(session, 44);
        }
        // Truncated and non-UTF8 tails must error, never panic or misalign.
        assert!(parse_stat_fields(b"1 (a) S 2 3").is_err());
        assert!(parse_stat_fields(b"1 (a) S two three four").is_err());
        assert!(parse_stat_fields(b"no delimiter").is_err());
        assert!(parse_stat_fields(&[0xff, b')', b' ', 0xfe, 0, 0, 0]).is_err());
        assert!(parse_stat_fields(b"").is_err());
    }
}
