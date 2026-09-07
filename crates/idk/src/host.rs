//! Detached, same-user terminal owner. Socket workers never own a PTY or run
//! initialization; the actor accepts each prepared shell before its first input.
mod actor;
mod children;
mod launch;

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
use std::time::{Duration, Instant};

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
    // Validate the durable ledger before advertising a replacement host.
    let mut actor = actor::Actor::new(store.clone(), launcher, info.clone())?;
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
    let (accepted_tx, accepted_rx) = mpsc::sync_channel::<Accepted>(16);
    let accepted_rx = Arc::new(Mutex::new(accepted_rx));
    let (request_tx, request_rx) = mpsc::sync_channel::<Work>(64);
    let inflight = Arc::new(AtomicUsize::new(0));
    let mut workers = Vec::new();
    for number in 0..4 {
        let incoming = accepted_rx.clone();
        let outgoing = request_tx.clone();
        let instance = info.host_instance.clone();
        workers.push(
            std::thread::Builder::new()
                .name(format!("idk-ipc-{number}"))
                .spawn(move || loop {
                    let item = match incoming.lock() {
                        Ok(receiver) => receiver.recv(),
                        Err(_) => return,
                    };
                    let Ok(mut item) = item else {
                        return;
                    };
                    let _ = handle_connection(&mut item, &outgoing, &instance);
                })?,
        );
    }
    drop(request_tx);
    loop {
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
