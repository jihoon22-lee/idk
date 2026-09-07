//! A client binds every request to the exact host instance and binary from its
//! handshake. Transport failures never automatically replay mutations.
use crate::model::{new_id, TerminalDefinition, PROTOCOL};
use crate::protocol::*;
use crate::store::Store;
use anyhow::{ensure, Context, Result};
use base64::Engine;
use serde::de::DeserializeOwned;
use std::collections::BTreeMap;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub struct Client {
    socket: PathBuf,
    client_id: String,
    host: HostInfo,
    timeout: Duration,
}

impl Client {
    pub fn connect(store: &Store) -> Result<Self> {
        Self::connect_with_launcher(store, Path::new("/proc/self/exe"))
    }

    pub fn connect_with_launcher(store: &Store, launcher: &Path) -> Result<Self> {
        let build = executable_build_id(launcher)?;
        Self::connect_expected(store, &build)
    }

    fn connect_expected(store: &Store, build: &str) -> Result<Self> {
        let timeout = Duration::from_millis(MAX_REQUEST_MS);
        let client_id = new_id();
        let request = Envelope::new(&client_id, Request::Hello);
        let response = exchange(&store.socket_path(), &request, timeout)?;
        let host: HostInfo = response.decode()?;
        ensure!(host.protocol == PROTOCOL && host.version == env!("CARGO_PKG_VERSION"), "running host version differs; preserve its sessions and use the matching binary or stop it explicitly");
        ensure!(host.build_id == build, "running host binary differs; preserve its sessions and use the matching binary or stop it explicitly");
        crate::model::valid_id(&host.host_instance)?;
        Ok(Self {
            socket: store.socket_path(),
            client_id,
            host,
            timeout,
        })
    }

    pub fn ensure_host(store: &Store, launcher: &Path) -> Result<Self> {
        ensure!(launcher.is_absolute(), "host launcher must be absolute");
        let build = executable_build_id(launcher)?;
        match Self::connect_expected(store, &build) {
            Ok(client) => return Ok(client),
            Err(error) => {
                let missing = error.downcast_ref::<std::io::Error>().is_some_and(|error| {
                    matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                    )
                });
                if !missing {
                    return Err(error);
                }
            }
        }
        let mut child = Command::new(launcher)
            .arg("__host")
            .arg("--config-dir")
            .arg(&store.config_dir)
            .arg("--state-dir")
            .arg(&store.state_dir)
            .arg("--runtime-dir")
            .arg(&store.runtime_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("start the same-binary user host")?;
        let deadline = Instant::now() + Duration::from_secs(5);
        let result = loop {
            match Self::connect_expected(store, &build) {
                Ok(client) => break Ok(client),
                Err(error) => {
                    let retry = error.downcast_ref::<std::io::Error>().is_some_and(|error| {
                        matches!(
                            error.kind(),
                            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                        )
                    });
                    if !retry || Instant::now() >= deadline {
                        break Err(error)
                            .context("host did not become ready; no existing host was stopped");
                    }
                }
            }
            // A racing launcher can lose host.lock while the other host starts.
            let _ = child.try_wait();
            std::thread::sleep(Duration::from_millis(25));
        };
        // Reap only this spawned process. The thread does not own any PTY/socket.
        let _ = std::thread::Builder::new()
            .name("idk-host-reaper".into())
            .spawn(move || {
                let _ = child.wait();
            });
        result
    }

    pub fn info(&self) -> &HostInfo {
        &self.host
    }
    pub fn client_id(&self) -> &str {
        &self.client_id
    }
    pub fn set_timeout(&mut self, timeout: Duration) -> Result<()> {
        ensure!(
            timeout >= Duration::from_millis(50)
                && timeout <= Duration::from_millis(MAX_REQUEST_MS),
            "client timeout must be 50 ms to five seconds"
        );
        self.timeout = timeout;
        Ok(())
    }
    /// Preserve this envelope unchanged if explicitly retrying an uncertain
    /// transport result. Its request ID, instance, deadline and payload bind it.
    pub fn envelope(&self, request: Request) -> Envelope {
        let mut envelope = Envelope::new(&self.client_id, request);
        envelope.host_instance = Some(self.host.host_instance.clone());
        envelope.deadline_ms = monotonic_ms().saturating_add(self.timeout.as_millis() as u64);
        envelope
    }
    pub fn send_envelope(&self, envelope: &Envelope) -> Result<Response> {
        ensure!(
            envelope.client_id == self.client_id
                && envelope.host_instance.as_deref() == Some(&self.host.host_instance),
            "request belongs to another client or host instance"
        );
        envelope.validate()?;
        exchange(&self.socket, envelope, self.timeout)
    }
    pub fn request(&mut self, request: Request) -> Result<Response> {
        self.send_envelope(&self.envelope(request))
    }
    fn call<T: DeserializeOwned>(&mut self, request: Request) -> Result<T> {
        self.request(request)?.decode()
    }
    pub fn list(&mut self, project: Option<&str>) -> Result<Vec<SessionInfo>> {
        self.call(Request::List {
            project: project.map(str::to_owned),
        })
    }
    pub fn definition(&mut self, session: &str) -> Result<TerminalDefinition> {
        self.call(Request::Definition {
            session: session.into(),
        })
    }
    pub fn start(
        &mut self,
        project: &str,
        terminal: &str,
        env: BTreeMap<String, String>,
        rows: u16,
        cols: u16,
        reopen: bool,
    ) -> Result<SessionInfo> {
        self.call(Request::Start {
            project: project.into(),
            terminal: terminal.into(),
            env,
            rows,
            cols,
            reopen,
        })
    }
    pub fn start_transient(
        &mut self,
        project: &str,
        terminal: TerminalDefinition,
        env: BTreeMap<String, String>,
        rows: u16,
        cols: u16,
    ) -> Result<SessionInfo> {
        self.call(Request::StartTransient {
            project: project.into(),
            terminal,
            env,
            rows,
            cols,
            reopen: false,
        })
    }
    pub fn reopen_transient(
        &mut self,
        project: &str,
        terminal: TerminalDefinition,
        env: BTreeMap<String, String>,
        rows: u16,
        cols: u16,
    ) -> Result<SessionInfo> {
        self.call(Request::StartTransient {
            project: project.into(),
            terminal,
            env,
            rows,
            cols,
            reopen: true,
        })
    }
    pub fn start_defaults(
        &mut self,
        project: &str,
        env: BTreeMap<String, String>,
        rows: u16,
        cols: u16,
    ) -> Result<BatchInfo> {
        self.call(Request::StartDefaults {
            project: project.into(),
            env,
            rows,
            cols,
        })
    }
    pub fn batch(&mut self, batch: &str) -> Result<BatchInfo> {
        self.call(Request::Batch {
            batch: batch.into(),
        })
    }
    pub fn continue_defaults(&mut self, batch: &str) -> Result<BatchInfo> {
        self.call(Request::ContinueDefaults {
            batch: batch.into(),
        })
    }
    pub fn cancel_defaults(&mut self, batch: &str) -> Result<BatchInfo> {
        self.call(Request::CancelDefaults {
            batch: batch.into(),
        })
    }
    pub fn attach(&mut self, session: &str, takeover: bool) -> Result<SessionInfo> {
        self.call(Request::Attach {
            session: session.into(),
            takeover,
        })
    }
    pub fn detach(&mut self, session: &str, epoch: u64) -> Result<SessionInfo> {
        self.call(Request::Detach {
            session: session.into(),
            epoch,
        })
    }
    pub fn snapshot(&mut self, session: &str, since: Option<u64>) -> Result<SnapshotReply> {
        self.call(Request::Snapshot {
            session: session.into(),
            since,
        })
    }
    pub fn input(&mut self, session: &str, epoch: u64, data: &[u8]) -> Result<()> {
        ensure!(
            data.len() <= MAX_INPUT_PACKET,
            "terminal input packet exceeds 64 KiB"
        );
        self.call(Request::Input {
            session: session.into(),
            epoch,
            data: base64::engine::general_purpose::STANDARD.encode(data),
        })
    }
    pub fn resize(&mut self, session: &str, epoch: u64, rows: u16, cols: u16) -> Result<()> {
        self.call(Request::Resize {
            session: session.into(),
            epoch,
            rows,
            cols,
        })
    }
    pub fn scroll(&mut self, session: &str, epoch: u64, delta: i32) -> Result<()> {
        self.call(Request::Scroll {
            session: session.into(),
            epoch,
            delta,
        })
    }
    pub fn search(
        &mut self,
        session: &str,
        epoch: u64,
        query: &str,
        backwards: bool,
    ) -> Result<SearchReply> {
        self.call(Request::Search {
            session: session.into(),
            epoch,
            query: query.into(),
            backwards,
        })
    }
    pub fn close(&mut self, session: &str, epoch: u64, force: bool) -> Result<SessionInfo> {
        self.call(Request::Close {
            session: session.into(),
            epoch,
            force,
        })
    }
    pub fn preview_close(&mut self, project: Option<&str>) -> Result<ClosePreview> {
        self.call(Request::PreviewClose {
            project: project.map(str::to_owned),
        })
    }
    pub fn close_project(
        &mut self,
        project: &str,
        sessions: &[String],
        force: bool,
    ) -> Result<CloseReply> {
        self.call(Request::CloseProject {
            project: project.into(),
            sessions: sessions.to_vec(),
            force,
        })
    }
    pub fn shutdown(&mut self, sessions: &[String], force: bool) -> Result<CloseReply> {
        self.call(Request::Shutdown {
            sessions: sessions.to_vec(),
            force,
        })
    }
}

fn exchange(socket: &Path, request: &Envelope, timeout: Duration) -> Result<Response> {
    let metadata = std::fs::symlink_metadata(socket)?;
    ensure!(
        metadata.file_type().is_socket()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.permissions().mode() & 0o077 == 0,
        "host socket must be an owned private socket"
    );
    let deadline = Instant::now() + timeout;
    let mut stream = connect_deadline(socket, deadline)?;
    configure_stream(&stream)?;
    let peer = peer_credentials(&stream)?;
    write_frame_deadline(&mut stream, request, deadline)?;
    let response: Response = read_frame_deadline(&mut stream, deadline)?;
    response.validate(request)?;
    if matches!(request.request, Request::Hello) && response.error.is_none() {
        let info: HostInfo =
            serde_json::from_value(response.data.clone().context("missing host handshake")?)?;
        ensure!(
            info.host_instance == response.host_instance
                && info.pid == peer.pid
                && info.uid == peer.uid,
            "host handshake does not match its local peer identity"
        );
    }
    Ok(response)
}
