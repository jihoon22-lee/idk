//! A client binds every request to the exact host instance and binary from its
//! handshake. Transport failures never automatically replay mutations.
use crate::model::{new_id, TerminalDefinition, PROTOCOL};
use crate::protocol::*;
use crate::store::Store;
use anyhow::{ensure, Context, Result};
use base64::Engine;
use serde::de::DeserializeOwned;
use serde::Serialize;
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
    pub fn git_reconcile(
        &mut self,
        operation: &str,
        repository: crate::git::Repository,
    ) -> Result<GitOperationInfo> {
        self.call(Request::GitOperationReconcile {
            operation: operation.into(),
            repository,
        })
    }
    pub fn git_submit(&mut self, task: GitTask) -> Result<GitJobInfo> {
        self.call(Request::GitSubmit { task })
    }
    pub fn git_job(&mut self, job: &str) -> Result<GitJobInfo> {
        self.call(Request::GitJob { job: job.into() })
    }
    pub fn git_execute(&mut self, plan: &str, rows: u16, cols: u16) -> Result<GitOperationInfo> {
        self.call(Request::GitExecute {
            plan: plan.into(),
            rows,
            cols,
        })
    }
    pub fn git_operations(&mut self, project: Option<&str>) -> Result<Vec<GitOperationInfo>> {
        self.call(Request::GitOperations {
            project: project.map(str::to_owned),
        })
    }
    pub fn git_operation_attach(
        &mut self,
        operation: &str,
        takeover: bool,
    ) -> Result<GitOperationInfo> {
        self.call(Request::GitOperationAttach {
            operation: operation.into(),
            takeover,
        })
    }
    pub fn git_operation_detach(
        &mut self,
        operation: &str,
        epoch: u64,
    ) -> Result<GitOperationInfo> {
        self.call(Request::GitOperationDetach {
            operation: operation.into(),
            epoch,
        })
    }
    pub fn git_operation_snapshot(
        &mut self,
        operation: &str,
        since: Option<u64>,
    ) -> Result<GitOperationSnapshot> {
        self.call(Request::GitOperationSnapshot {
            operation: operation.into(),
            since,
        })
    }
    pub fn git_operation_input(&mut self, operation: &str, epoch: u64, data: &[u8]) -> Result<()> {
        ensure!(
            data.len() <= MAX_INPUT_PACKET,
            "Git operation input packet exceeds 64 KiB"
        );
        self.call(Request::GitOperationInput {
            operation: operation.into(),
            epoch,
            data: base64::engine::general_purpose::STANDARD.encode(data),
        })
    }
    pub fn git_operation_resize(
        &mut self,
        operation: &str,
        epoch: u64,
        rows: u16,
        cols: u16,
    ) -> Result<()> {
        self.call(Request::GitOperationResize {
            operation: operation.into(),
            epoch,
            rows,
            cols,
        })
    }
    pub fn git_operation_cancel(
        &mut self,
        operation: &str,
        epoch: u64,
        force: bool,
    ) -> Result<GitOperationInfo> {
        self.call(Request::GitOperationCancel {
            operation: operation.into(),
            epoch,
            force,
        })
    }
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
        let response = exchange(&store.socket_path(), &request, timeout)
            .map_err(|error| compatibility_error(store, error))?;
        let host: HostInfo = response
            .decode()
            .map_err(|error| compatibility_error(store, error))?;
        if host.protocol != PROTOCOL || host.version != env!("CARGO_PKG_VERSION") {
            return Err(compatibility_error(
                store,
                anyhow::anyhow!("running host version differs; existing sessions were preserved"),
            ));
        }
        if host.build_id != build {
            return Err(compatibility_error(
                store,
                anyhow::anyhow!("running host binary differs; existing sessions were preserved"),
            ));
        }
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
    pub fn run(&mut self, request: crate::run_wire::RunRequest) -> Result<crate::run_wire::RunJob> {
        self.request(Request::Run { request })?.decode()
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

/// A verified installation client matching the kernel-observed socket peer's exact bytes.
/// These are instructions only: discovery never executes the client or stops the host.
#[derive(Debug, Clone, Serialize)]
pub struct CompatibleHostClient {
    pub host_pid: u32,
    pub observed_executable: PathBuf,
    pub client: PathBuf,
    pub binary_sha256: String,
    pub invocation: String,
    pub inspect_command: String,
    pub config_dir: PathBuf,
    pub state_dir: PathBuf,
    pub runtime_dir: PathBuf,
}

fn command_word(value: &std::ffi::OsStr) -> Result<String> {
    let value = value
        .to_str()
        .context("recovery command path is not UTF-8")?;
    ensure!(
        !value.chars().any(char::is_control),
        "recovery command path contains control characters"
    );
    Ok(format!("'{}'", value.replace('\'', "'\"'\"'")))
}
fn compatible_invocation(store: &Store, binary: &Path) -> Result<String> {
    let binary = command_word(binary.as_os_str())?;
    if let Some(root) = store.config_dir.parent() {
        if store.config_dir == root.join("config")
            && store.state_dir == root.join("state")
            && store.runtime_dir == root.join("run")
        {
            return Ok(format!(
                "{binary} --data-dir {}",
                command_word(root.as_os_str())?
            ));
        }
    }
    let xdg_base = |path: &Path| -> Option<PathBuf> {
        (path.file_name()? == "v0.4" && path.parent()?.file_name()? == "idk")
            .then(|| path.parent().unwrap().parent().map(Path::to_path_buf))
            .flatten()
    };
    if let (Some(config), Some(state), Some(runtime)) = (
        xdg_base(&store.config_dir),
        xdg_base(&store.state_dir),
        store.runtime_dir.parent(),
    ) {
        ensure!(
            store
                .runtime_dir
                .file_name()
                .is_some_and(|name| name == "idk-v04"),
            "custom runtime path cannot be represented by the public CLI"
        );
        return Ok(format!(
            "env -u IDK_HOME XDG_CONFIG_HOME={} XDG_STATE_HOME={} XDG_RUNTIME_DIR={} {binary}",
            command_word(config.as_os_str())?,
            command_word(state.as_os_str())?,
            command_word(runtime.as_os_str())?
        ));
    }
    anyhow::bail!(
        "custom config/state/runtime paths cannot be represented by a same-data public CLI command"
    )
}
fn process_start(pid: u32) -> Result<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    stat.rsplit_once(") ")
        .context("invalid peer process stat")?
        .1
        .split_whitespace()
        .nth(19)
        .context("peer process start time missing")?
        .parse()
        .context("invalid peer start time")
}

/// Read-only compatibility lookup. The PID comes from SO_PEERCRED, never a stored PID record.
/// A same-UID peer is not enough: executable bytes must match a fully verified retained generation.
pub fn compatible_host_client(
    store: &Store,
    prefix: Option<&Path>,
) -> Result<Option<CompatibleHostClient>> {
    let socket = store.socket_path();
    let metadata = std::fs::symlink_metadata(&socket)?;
    ensure!(
        metadata.file_type().is_socket()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.permissions().mode() & 0o077 == 0,
        "host socket must be owned and private for compatibility lookup"
    );
    let stream = connect_deadline(&socket, Instant::now() + Duration::from_secs(2))?;
    let peer = peer_credentials(&stream)?;
    ensure!(peer.pid > 1, "invalid host peer PID");
    let started = process_start(peer.pid)?;
    let proc_exe = PathBuf::from(format!("/proc/{}/exe", peer.pid));
    let executable = std::fs::read_link(&proc_exe).context("read live host executable")?;
    let before = std::fs::metadata(&proc_exe)?;
    let build = executable_build_id(&proc_exe)?;
    let mut prefixes = Vec::new();
    if let Some(prefix) = prefix {
        prefixes.push(prefix.to_path_buf());
    }
    if executable
        .file_name()
        .is_some_and(|name| name == crate::package::BINARY)
    {
        if let Some(generations) = executable.parent().and_then(Path::parent) {
            if generations
                .file_name()
                .is_some_and(|name| name == "generations")
            {
                if let Some(root) = generations.parent() {
                    prefixes.push(root.into());
                }
            }
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        prefixes.push(PathBuf::from(home).join(".local/share/idk"));
    }
    let mut seen = std::collections::BTreeSet::new();
    for prefix in prefixes {
        if !seen.insert(prefix.clone()) {
            continue;
        }
        if let Some(client) = crate::install::Installer::compatible_client(&prefix, &build)? {
            let after = std::fs::metadata(&proc_exe)?;
            ensure!(
                started == process_start(peer.pid)?
                    && before.dev() == after.dev()
                    && before.ino() == after.ino()
                    && executable == std::fs::read_link(&proc_exe)?,
                "host process identity changed during compatibility lookup"
            );
            ensure!(
                executable_build_id(&client)? == build,
                "retained client bytes changed after verification"
            );
            let invocation = compatible_invocation(store, &client)?;
            return Ok(Some(CompatibleHostClient {
                host_pid: peer.pid,
                observed_executable: executable,
                client,
                binary_sha256: build,
                inspect_command: format!("{invocation} session list"),
                invocation,
                config_dir: store.config_dir.clone(),
                state_dir: store.state_dir.clone(),
                runtime_dir: store.runtime_dir.clone(),
            }));
        }
    }
    Ok(None)
}
fn compatibility_error(store: &Store, error: anyhow::Error) -> anyhow::Error {
    // Initial host absence must remain a cheap, typed condition for explicit host startup.
    if error.downcast_ref::<std::io::Error>().is_some_and(|error| {
        matches!(
            error.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
        )
    }) {
        return error;
    }
    match compatible_host_client(store, None) {
        Ok(Some(client)) => error.context(format!("Preserved host PID {} has a verified matching client. Inspect its sessions with the same data paths:\n{}\nFor a compatible workspace schema, use the same invocation with `session attach SESSION_ID`. Configuration migration may prevent the older TUI; session list/snapshot remain read-only. No process was stopped or command replayed.", client.host_pid, client.inspect_command)),
        Ok(None) => error.context("No verified matching installation generation was found. Preserve the host and use its original installation; `package compatible-client --prefix /absolute/installation` can inspect a known prefix without changing it."),
        Err(detail) => error.context(format!("Compatible-client verification was unavailable: {detail:#}. Existing host and data were preserved; no unverified executable was suggested.")),
    }
}

#[cfg(test)]
mod compatibility_tests {
    use super::*;
    #[test]
    fn recovery_invocation_keeps_exact_data_paths_and_quotes_shell_metacharacters() {
        let root = PathBuf::from("/tmp/idk data ' $(false)");
        let store = Store {
            config_dir: root.join("config"),
            state_dir: root.join("state"),
            runtime_dir: root.join("run"),
        };
        let command = compatible_invocation(&store, Path::new("/tmp/install path/idk")).unwrap();
        assert_eq!(
            command,
            "'/tmp/install path/idk' --data-dir '/tmp/idk data '\"'\"' $(false)'"
        );
        let xdg = Store {
            config_dir: "/a/config/idk/v0.4".into(),
            state_dir: "/b/state/idk/v0.4".into(),
            runtime_dir: "/c/run/idk-v04".into(),
        };
        let command = compatible_invocation(&xdg, Path::new("/installation/idk")).unwrap();
        assert!(
            command.contains("env -u IDK_HOME")
                && command.contains("XDG_CONFIG_HOME='/a/config'")
                && command.contains("XDG_STATE_HOME='/b/state'")
                && command.contains("XDG_RUNTIME_DIR='/c/run'")
        );
    }
    #[test]
    fn compatibility_lookup_does_not_create_an_installation_or_trust_a_saved_pid() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(Some(&tmp.path().join("data"))).unwrap();
        crate::store::atomic_write(
            &store.state_dir.join("host-exit.json"),
            format!("{{\"pid\":{}}}", std::process::id()).as_bytes(),
        )
        .unwrap();
        let absent = tmp.path().join("not-installed");
        assert!(compatible_host_client(&store, Some(&absent)).is_err());
        assert!(!absent.exists());
    }
}
