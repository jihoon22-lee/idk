//! Read-only descendant inventory plus targeted waits. The actor retains each
//! PTY leader unreaped throughout a scan, so its session ID cannot be recycled.
//! No saved PID, process name, or process-group lookup grants signal authority.
use anyhow::{ensure, Context, Result};
use std::collections::HashSet;
use std::io::Read;
use std::sync::mpsc;
use std::time::{Duration, Instant};

const MAX_MATCHES: usize = 256;
const MAX_PROC_ENTRIES: usize = 131_072;

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
        let mut bytes = Vec::with_capacity(1024);
        match std::fs::File::open(entry.path().join("stat"))
            .and_then(|file| file.take(8193).read_to_end(&mut bytes))
        {
            Ok(_) => {}
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    || error.raw_os_error() == Some(libc::ESRCH) =>
            {
                continue
            }
            Err(error) => return Err(error).context("read process session inventory"),
        }
        ensure!(
            bytes.len() <= 8192,
            "process status exceeded inventory limit"
        );
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
        let parent = fields[1].parse()?;
        let session = fields[3].parse()?;
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
