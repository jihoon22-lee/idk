//! Detached, same-user terminal owner. Socket workers never own a PTY or run
//! initialization; the actor accepts each prepared shell before its first input.
mod actor;
mod children;
mod git_bridge;
mod git_jobs;
mod git_operation;
mod launch;
mod run_jobs;

use crate::model::{new_id, MAX_TERMINALS, PROTOCOL};
use crate::protocol::{self, Envelope, HostInfo, Response};
use crate::store::{ensure_private_dir, FileLock, Store};
use anyhow::{bail, ensure, Context, Result};
use std::fs;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

struct Work {
    envelope: Envelope,
    peer: protocol::PeerCredentials,
    reply: mpsc::SyncSender<Response>,
}
struct Accepted {
    stream: UnixStream,
    deadline: Instant,
    count: Arc<AtomicUsize>,
}
impl Drop for Accepted {
    fn drop(&mut self) {
        self.count.fetch_sub(1, Ordering::AcqRel);
    }
}
struct SocketGuard {
    path: PathBuf,
    device: u64,
    inode: u64,
}
impl Drop for SocketGuard {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path).is_ok_and(|m| {
            m.file_type().is_socket()
                && m.uid() == unsafe { libc::geteuid() }
                && m.dev() == self.device
                && m.ino() == self.inode
        }) {
            let _ = fs::remove_file(&self.path);
        }
    }
}
/// An IPC worker that exited for any reason decrements this counter. Panic
/// unwinds the guard the same way a channel close does.
struct LiveWorker(Arc<AtomicUsize>);
impl Drop for LiveWorker {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

pub fn serve(store: Store, launcher: PathBuf) -> Result<()> {
    children::enable_subreaper()?;
    for directory in [&store.config_dir, &store.state_dir, &store.runtime_dir] {
        ensure_private_dir(directory)?;
    }
    let _lock = FileLock::acquire(&store.runtime_dir.join("host.lock"), true)
        .context("another host holds the lifetime lock")?;
    let build_id = protocol::executable_build_id(&launcher)?;
    ensure!(
        build_id == protocol::executable_build_id(std::path::Path::new("/proc/self/exe"))?,
        "host launcher differs from the running executable"
    );
    let info = HostInfo {
        protocol: PROTOCOL,
        version: env!("CARGO_PKG_VERSION").into(),
        build_id,
        host_instance: new_id(),
        pid: std::process::id(),
        uid: unsafe { libc::geteuid() },
        max_sessions: MAX_TERMINALS,
        max_cells: crate::terminal::MAX_TERMINAL_CELLS,
    };
    let started_at_ms = now_ms();
    // A host that owns an identity records its own exit; null stdio means this
    // durable note is the only diagnosis after a crash or wedged worker. A
    // panic is recorded the same way before the unwind continues.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        serve_owned(&store, launcher, &info, started_at_ms)
    }));
    match outcome {
        Ok(outcome) => {
            record_exit(&store, &info, started_at_ms, &outcome);
            outcome
        }
        Err(payload) => {
            record_exit(
                &store,
                &info,
                started_at_ms,
                &Err(anyhow::anyhow!("host serve loop panicked")),
            );
            std::panic::resume_unwind(payload)
        }
    }
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}
/// Once the socket is bound the host owns its identity: mark it running so a
/// later crash is distinguishable from a recorded shutdown.
fn record_running(store: &Store, info: &HostInfo, started_at_ms: u64) {
    let record = crate::saved_state::HostExit {
        schema: 1,
        host_instance: info.host_instance.clone(),
        pid: info.pid,
        started_at_ms,
        stopped_at_ms: None,
        error: None,
    };
    let _ = store.write_state("host-exit.json", &record);
}
fn record_exit(store: &Store, info: &HostInfo, started_at_ms: u64, outcome: &Result<()>) {
    let record = crate::saved_state::HostExit {
        schema: 1,
        host_instance: info.host_instance.clone(),
        pid: info.pid,
        started_at_ms,
        stopped_at_ms: Some(now_ms()),
        error: outcome.as_ref().err().map(protocol::safe_error),
    };
    let _ = store.write_state("host-exit.json", &record);
}

fn serve_owned(
    store: &Store,
    launcher: PathBuf,
    info: &HostInfo,
    started_at_ms: u64,
) -> Result<()> {
    // Validate the durable ledger before advertising a replacement host.
    let mut actor = actor::Actor::new(store.clone(), launcher.clone(), info.clone())?;
    let path = store.socket_path();
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            ensure!(
                metadata.file_type().is_socket() && metadata.uid() == info.uid,
                "host endpoint is not an owned socket; preserved"
            );
            match protocol::connect_deadline(&path, Instant::now() + Duration::from_millis(250)) {
                Ok(_) => bail!("a live host endpoint already exists; preserved"),
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::ConnectionRefused) =>
                {
                    remove_stale_socket(&path, &metadata, info.uid)?
                }
                Err(error) => {
                    return Err(error).context("host endpoint could not be proved stale; preserved")
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let listener = UnixListener::bind(&path).context("bind host endpoint")?;
    let metadata = fs::symlink_metadata(&path)?;
    let _socket = SocketGuard {
        path: path.clone(),
        device: metadata.dev(),
        inode: metadata.ino(),
    };
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    record_running(store, info, started_at_ms);
    let (accepted_tx, accepted_rx) = mpsc::sync_channel::<Accepted>(16);
    let accepted_rx = Arc::new(Mutex::new(accepted_rx));
    let (request_tx, request_rx) = mpsc::sync_channel::<Work>(64);
    let inflight = Arc::new(AtomicUsize::new(0));
    let workers_alive = Arc::new(AtomicUsize::new(0));
    let mut workers = Vec::new();
    for number in 0..4 {
        let incoming = accepted_rx.clone();
        let outgoing = request_tx.clone();
        let instance = info.host_instance.clone();
        workers_alive.fetch_add(1, Ordering::AcqRel);
        let alive = workers_alive.clone();
        match std::thread::Builder::new()
            .name(format!("idk-ipc-{number}"))
            .spawn(move || {
                let _alive = LiveWorker(alive);
                loop {
                    let item = match incoming.lock() {
                        Ok(receiver) => receiver.recv(),
                        Err(_) => return,
                    };
                    let Ok(mut item) = item else {
                        return;
                    };
                    let _ = handle_connection(&mut item, &outgoing, &instance);
                }
            }) {
            Ok(worker) => workers.push(worker),
            Err(error) => {
                workers_alive.fetch_sub(1, Ordering::AcqRel);
                return Err(error).context("start host IPC worker");
            }
        }
    }
    drop(request_tx);
    loop {
        // A dead IPC pool can never serve another request; exiting lets a
        // client-spawned host recover through the durable ledgers.
        ensure!(
            workers_alive.load(Ordering::Acquire) > 0,
            "all host IPC workers exited"
        );
        if !actor.finished() {
            for _ in 0..16 {
                match listener.accept() {
                    Ok((stream, _)) => {
                        // Authenticate before queueing or allocating a message body.
                        if protocol::peer_credentials(&stream).is_err() {
                            continue;
                        }
                        inflight.fetch_add(1, Ordering::AcqRel);
                        let accepted = Accepted {
                            stream,
                            deadline: Instant::now()
                                + Duration::from_millis(protocol::MAX_REQUEST_MS),
                            count: inflight.clone(),
                        };
                        let _ = accepted_tx.try_send(accepted);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error).context("accept host connection"),
                }
            }
        }
        for _ in 0..32 {
            match request_rx.try_recv() {
                Ok(work) => {
                    let response = actor.respond(work.envelope, work.peer);
                    let _ = work.reply.try_send(response);
                }
                Err(_) => break,
            }
        }
        actor.tick();
        if actor.finished() && inflight.load(Ordering::Acquire) == 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(accepted_tx);
    for worker in workers {
        let _ = worker.join();
    }
    Ok(())
}

fn remove_stale_socket(path: &std::path::Path, observed: &fs::Metadata, uid: u32) -> Result<()> {
    let current = fs::symlink_metadata(path)?;
    ensure!(
        current.file_type().is_socket()
            && current.uid() == uid
            && current.dev() == observed.dev()
            && current.ino() == observed.ino(),
        "host endpoint changed while checking whether it was stale; replacement preserved"
    );
    fs::remove_file(path).context("remove the verified stale host endpoint")
}

fn handle_connection(
    item: &mut Accepted,
    actor: &mpsc::SyncSender<Work>,
    instance: &str,
) -> Result<()> {
    let peer = protocol::peer_credentials(&item.stream)?;
    let envelope: Envelope = protocol::read_frame_deadline(&mut item.stream, item.deadline)?;
    let request_id = envelope.request_id.clone();
    if let Err(error) = envelope.validate() {
        return protocol::write_frame_deadline(
            &mut item.stream,
            &Response::failure(request_id, error).with_host(instance),
            item.deadline,
        );
    }
    let deadline = item.deadline.min(
        Instant::now()
            + Duration::from_millis(
                envelope
                    .deadline_ms
                    .saturating_sub(protocol::monotonic_ms()),
            ),
    );
    let (tx, rx) = mpsc::sync_channel(1);
    let response = match actor.try_send(Work {
        envelope,
        peer,
        reply: tx,
    }) {
        Ok(()) => rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .context("host request outcome unknown after deadline; inspect state without replay")?,
        Err(_) => Response::failure(
            request_id,
            "host request queue is full; no mutation accepted",
        )
        .with_host(instance),
    };
    protocol::write_frame_deadline(&mut item.stream, &response, deadline)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_socket_cleanup_preserves_a_replacement_regular_file() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("host.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let observed = fs::symlink_metadata(&path).unwrap();
        drop(listener);
        fs::remove_file(&path).unwrap();
        fs::write(&path, b"replacement must survive").unwrap();
        assert!(remove_stale_socket(&path, &observed, unsafe { libc::geteuid() }).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"replacement must survive");
    }
}
