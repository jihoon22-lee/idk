//! Bounded same-user IPC. Environments and input never enter Debug/history.
pub use crate::git_wire::{
    GitJobInfo, GitJobState, GitOperationInfo, GitOperationSnapshot, GitOperationState, GitTask,
    GitValue,
};
use crate::model::{new_id, valid_id, TerminalDefinition, MAX_MESSAGE, PROTOCOL};
use crate::shell::InitializationState;
use crate::terminal::{TerminalExit, TerminalMatch, TerminalSnapshot, MAX_TERMINAL_CELLS};
use anyhow::{bail, ensure, Context, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const MAX_REQUEST_MS: u64 = 5_000;
pub const MAX_INPUT_PACKET: usize = 64 * 1024;
pub const MAX_SEARCH_BYTES: usize = 256;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub protocol: u32,
    pub request_id: String,
    pub client_id: String,
    pub host_instance: Option<String>,
    /// Shared Linux CLOCK_MONOTONIC milliseconds. Retries keep the deadline.
    pub deadline_ms: u64,
    pub request: Request,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Run {
        request: crate::run_wire::RunRequest,
    },
    GitSubmit {
        task: GitTask,
    },
    GitJob {
        job: String,
    },
    GitExecute {
        plan: String,
        rows: u16,
        cols: u16,
    },
    GitOperations {
        project: Option<String>,
    },
    GitOperationAttach {
        operation: String,
        takeover: bool,
    },
    GitOperationDetach {
        operation: String,
        epoch: u64,
    },
    GitOperationSnapshot {
        operation: String,
        since: Option<u64>,
    },
    GitOperationInput {
        operation: String,
        epoch: u64,
        data: String,
    },
    GitOperationResize {
        operation: String,
        epoch: u64,
        rows: u16,
        cols: u16,
    },
    GitOperationReconcile {
        operation: String,
        repository: crate::git::Repository,
    },
    GitOperationCancel {
        operation: String,
        epoch: u64,
        force: bool,
    },
    Hello,
    List {
        project: Option<String>,
    },
    Definition {
        session: String,
    },
    Start {
        project: String,
        terminal: String,
        rows: u16,
        cols: u16,
        env: BTreeMap<String, String>,
        reopen: bool,
    },
    StartTransient {
        project: String,
        terminal: TerminalDefinition,
        rows: u16,
        cols: u16,
        env: BTreeMap<String, String>,
        reopen: bool,
    },
    StartDefaults {
        project: String,
        rows: u16,
        cols: u16,
        env: BTreeMap<String, String>,
    },
    Batch {
        batch: String,
    },
    ContinueDefaults {
        batch: String,
    },
    CancelDefaults {
        batch: String,
    },
    Attach {
        session: String,
        takeover: bool,
    },
    Detach {
        session: String,
        epoch: u64,
    },
    Snapshot {
        session: String,
        since: Option<u64>,
    },
    Input {
        session: String,
        data: String,
        epoch: u64,
    },
    Resize {
        session: String,
        rows: u16,
        cols: u16,
        epoch: u64,
    },
    Scroll {
        session: String,
        delta: i32,
        epoch: u64,
    },
    Search {
        session: String,
        query: String,
        backwards: bool,
        epoch: u64,
    },
    Close {
        session: String,
        epoch: u64,
        force: bool,
    },
    PreviewClose {
        project: Option<String>,
    },
    CloseProject {
        project: String,
        sessions: Vec<String>,
        force: bool,
    },
    Shutdown {
        sessions: Vec<String>,
        force: bool,
    },
}

impl Request {
    pub fn is_mutating(&self) -> bool {
        !matches!(
            self,
            Self::Run {
                request: crate::run_wire::RunRequest::Job { .. }
            } | Self::Hello
                | Self::List { .. }
                | Self::Definition { .. }
                | Self::Snapshot { .. }
                | Self::Batch { .. }
                | Self::PreviewClose { .. }
                | Self::GitJob { .. }
                | Self::GitOperations { .. }
                | Self::GitOperationSnapshot { .. }
        )
    }
    fn validate(&self) -> Result<()> {
        match self {
            Self::Run { request } => request.validate()?,
            Self::GitSubmit { task } => task.validate()?,
            Self::GitJob { job } => valid_id(job)?,
            Self::GitOperationReconcile {
                operation,
                repository,
            } => {
                valid_id(operation)?;
                crate::model::absolute_path(&repository.root)?;
                crate::model::absolute_path(&repository.git_dir)?;
                crate::model::absolute_path(&repository.common_dir)?;
            }
            Self::GitExecute { plan, rows, cols } => {
                valid_id(plan)?;
                validate_dimensions(*rows, *cols)?;
            }
            Self::GitOperations { project } => {
                if let Some(project) = project {
                    valid_id(project)?;
                }
            }
            Self::GitOperationAttach { operation, .. }
            | Self::GitOperationDetach { operation, .. }
            | Self::GitOperationSnapshot { operation, .. }
            | Self::GitOperationCancel { operation, .. } => valid_id(operation)?,
            Self::GitOperationInput {
                operation, data, ..
            } => {
                valid_id(operation)?;
                ensure!(
                    data.len() <= MAX_INPUT_PACKET.div_ceil(3) * 4,
                    "Git operation input packet exceeds 64 KiB"
                );
            }
            Self::GitOperationResize {
                operation,
                rows,
                cols,
                ..
            } => {
                valid_id(operation)?;
                validate_dimensions(*rows, *cols)?;
            }
            Self::Hello => {}
            Self::List { project } | Self::PreviewClose { project } => {
                if let Some(project) = project {
                    valid_id(project)?;
                }
            }
            Self::Start {
                project,
                terminal,
                rows,
                cols,
                env,
                ..
            } => {
                valid_id(project)?;
                valid_id(terminal)?;
                validate_dimensions(*rows, *cols)?;
                validate_environment(env)?;
            }
            Self::StartTransient {
                project,
                terminal,
                rows,
                cols,
                env,
                ..
            } => {
                valid_id(project)?;
                valid_id(&terminal.id)?;
                validate_dimensions(*rows, *cols)?;
                validate_environment(env)?;
                ensure!(
                    !terminal.persistent,
                    "transient terminal must not be persistent"
                );
            }
            Self::StartDefaults {
                project,
                rows,
                cols,
                env,
            } => {
                valid_id(project)?;
                validate_dimensions(*rows, *cols)?;
                validate_environment(env)?;
            }
            Self::Batch { batch }
            | Self::ContinueDefaults { batch }
            | Self::CancelDefaults { batch } => valid_id(batch)?,
            Self::Definition { session }
            | Self::Attach { session, .. }
            | Self::Detach { session, .. }
            | Self::Snapshot { session, .. }
            | Self::Scroll { session, .. }
            | Self::Close { session, .. } => valid_id(session)?,
            Self::Resize {
                session,
                rows,
                cols,
                ..
            } => {
                valid_id(session)?;
                validate_dimensions(*rows, *cols)?;
            }
            Self::Input { session, data, .. } => {
                valid_id(session)?;
                ensure!(
                    data.len() <= MAX_INPUT_PACKET.div_ceil(3) * 4,
                    "terminal input packet exceeds 64 KiB"
                );
            }
            Self::Search { session, query, .. } => {
                valid_id(session)?;
                ensure!(
                    !query.is_empty()
                        && query.len() <= MAX_SEARCH_BYTES
                        && !query.chars().any(char::is_control),
                    "search needs 1–256 bytes of visible text"
                );
            }
            Self::CloseProject {
                project, sessions, ..
            } => {
                valid_id(project)?;
                validate_targets(sessions)?;
            }
            Self::Shutdown { sessions, .. } => validate_targets(sessions)?,
        }
        Ok(())
    }
}

pub(crate) fn validate_environment(env: &BTreeMap<String, String>) -> Result<()> {
    ensure!(env.len() <= 1024, "launch environment exceeds 1024 entries");
    let mut total = 0usize;
    for (key, value) in env {
        ensure!(
            !key.is_empty() && !key.contains(['=', '\0']) && !value.contains('\0'),
            "invalid launch environment entry"
        );
        total = total
            .checked_add(key.len())
            .and_then(|n| n.checked_add(value.len()))
            .and_then(|n| n.checked_add(2))
            .context("launch environment size overflow")?;
        ensure!(total <= 256 * 1024, "launch environment exceeds 256 KiB");
    }
    ensure!(
        Path::new(env.get("HOME").context("launch environment needs HOME")?).is_absolute(),
        "launch HOME must be absolute"
    );
    Ok(())
}

fn validate_targets(sessions: &[String]) -> Result<()> {
    ensure!(
        sessions.len() <= crate::model::MAX_TERMINALS,
        "too many close targets"
    );
    let mut unique = std::collections::HashSet::new();
    for session in sessions {
        valid_id(session)?;
        ensure!(unique.insert(session), "duplicate close target");
    }
    Ok(())
}

pub fn validate_dimensions(rows: u16, cols: u16) -> Result<()> {
    ensure!(rows > 0 && cols >= 2 && usize::from(rows) * usize::from(cols) <= MAX_TERMINAL_CELLS, "terminal viewport must have rows > 0, columns >= 2, and at most {MAX_TERMINAL_CELLS} cells");
    Ok(())
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request")
            .field("kind", &std::mem::discriminant(self))
            .field("payload", &"[redacted]")
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostInfo {
    pub protocol: u32,
    pub version: String,
    pub build_id: String,
    pub host_instance: String,
    pub pid: u32,
    pub uid: u32,
    pub max_sessions: usize,
    pub max_cells: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Preparing,
    Running,
    Closing,
    Closed,
    Exited,
    Failed,
    Unknown,
}
impl SessionState {
    pub fn is_live(self) -> bool {
        matches!(self, Self::Preparing | Self::Running | Self::Closing)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputOwner {
    pub client_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionInfo {
    pub session_id: String,
    pub project_id: String,
    pub terminal_id: String,
    pub name: String,
    pub host_instance: String,
    pub persistent: bool,
    pub state: SessionState,
    pub initialization: Option<InitializationState>,
    pub input_epoch: u64,
    pub owner: Option<InputOwner>,
    pub generation: u64,
    pub child_pid: Option<u32>,
    pub cwd: Option<PathBuf>,
    pub definition_revision: u64,
    pub launch_digest: Option<String>,
    pub exit: Option<TerminalExit>,
    pub error: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SnapshotReply {
    pub session: SessionInfo,
    pub screen: Option<TerminalSnapshot>,
}
impl std::fmt::Debug for SnapshotReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SnapshotReply")
            .field("session_id", &self.session.session_id)
            .field("screen", &"[redacted]")
            .finish()
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchState {
    Running,
    Paused,
    Complete,
    Cancelled,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchInfo {
    pub batch_id: String,
    pub project_id: String,
    pub state: BatchState,
    pub sessions: Vec<String>,
    pub remaining: Vec<String>,
    pub waiting_session: Option<String>,
    pub failures: BTreeMap<String, String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchReply {
    pub found: Option<TerminalMatch>,
    pub searched_rows: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClosePreview {
    pub project: Option<String>,
    pub targets: Vec<SessionInfo>,
    #[serde(default)]
    pub run_relations: Vec<RunCloseRelation>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunCloseRelation {
    pub session_id: String,
    pub run_id: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloseReply {
    pub sessions: Vec<String>,
    pub pending: Vec<String>,
    pub errors: BTreeMap<String, String>,
    pub shutting_down: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub protocol: u32,
    pub request_id: String,
    pub host_instance: String,
    #[serde(default, deserialize_with = "present_json")]
    pub data: Option<serde_json::Value>,
    pub error: Option<String>,
}

fn present_json<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<serde_json::Value>, D::Error> {
    serde_json::Value::deserialize(deserializer).map(Some)
}
impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Response")
            .field("protocol", &self.protocol)
            .field("request_id", &self.request_id)
            .field("host_instance", &self.host_instance)
            .field("has_error", &self.error.is_some())
            .field("payload", &"[redacted]")
            .finish()
    }
}

impl Envelope {
    pub fn new(client_id: &str, request: Request) -> Self {
        Self {
            protocol: PROTOCOL,
            request_id: new_id(),
            client_id: client_id.to_owned(),
            host_instance: None,
            deadline_ms: monotonic_ms().saturating_add(MAX_REQUEST_MS),
            request,
        }
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.protocol == PROTOCOL,
            "protocol mismatch: client {}, host {}; use a compatible binary",
            self.protocol,
            PROTOCOL
        );
        valid_id(&self.request_id)?;
        valid_id(&self.client_id)?;
        let now = monotonic_ms();
        ensure!(
            self.deadline_ms > now && self.deadline_ms <= now.saturating_add(MAX_REQUEST_MS),
            "request deadline expired or exceeds five seconds"
        );
        if !matches!(self.request, Request::Hello) {
            valid_id(
                self.host_instance
                    .as_deref()
                    .context("request needs a host instance")?,
            )?;
        }
        self.request.validate()
    }
    pub fn fingerprint(&self) -> Result<[u8; 32]> {
        Ok(Sha256::digest(serde_json::to_vec(self)?).into())
    }
}

impl Response {
    pub fn success<T: Serialize>(request_id: String, data: &T) -> Result<Self> {
        Ok(Self {
            protocol: PROTOCOL,
            request_id,
            host_instance: String::new(),
            data: Some(serde_json::to_value(data)?),
            error: None,
        })
    }
    pub fn failure(request_id: String, error: impl ToString) -> Self {
        Self {
            protocol: PROTOCOL,
            request_id,
            host_instance: String::new(),
            data: None,
            error: Some(safe_error(error)),
        }
    }
    pub fn with_host(mut self, instance: &str) -> Self {
        self.host_instance = instance.into();
        self
    }
    pub fn validate(&self, request: &Envelope) -> Result<()> {
        ensure!(
            self.protocol == PROTOCOL,
            "host protocol changed; no request was replayed"
        );
        ensure!(
            self.request_id == request.request_id,
            "host response request ID mismatch"
        );
        valid_id(&self.host_instance)?;
        if let Some(expected) = &request.host_instance {
            ensure!(
                expected == &self.host_instance,
                "host instance changed; reconnect explicitly; no request was replayed"
            );
        }
        Ok(())
    }
    pub fn decode<T: DeserializeOwned>(self) -> Result<T> {
        if let Some(error) = self.error {
            bail!("{error}");
        }
        serde_json::from_value(self.data.context("missing response payload")?)
            .context("invalid response payload")
    }
}

pub fn safe_error(error: impl ToString) -> String {
    let mut result = String::new();
    for c in error.to_string().chars().filter(|c| !c.is_control()) {
        if result.len() + c.len_utf8() > 512 {
            break;
        }
        result.push(c);
    }
    result
}

pub fn monotonic_ms() -> u64 {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // Like Instant::now, fail if the OS clock fails; never invent an unexpired request.
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) },
        0,
        "Linux monotonic clock unavailable"
    );
    (now.tv_sec as u64)
        .saturating_mul(1000)
        .saturating_add(now.tv_nsec as u64 / 1_000_000)
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct BinaryIdentity {
    device: u64,
    inode: u64,
    length: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl BinaryIdentity {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            length: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
}

static BINARY_IDENTITIES: OnceLock<Mutex<Vec<(BinaryIdentity, String)>>> = OnceLock::new();

pub fn executable_build_id(path: &Path) -> Result<String> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .context("read selected binary identity")?;
    let before = file.metadata()?;
    ensure!(
        before.is_file() && before.len() <= 512 * 1024 * 1024,
        "binary identity needs a regular executable of at most 512 MiB"
    );
    let identity = BinaryIdentity::from_metadata(&before);
    let cache = BINARY_IDENTITIES.get_or_init(|| Mutex::new(Vec::new()));
    // Process-local only: never trust a writable on-disk checksum cache. The
    // launcher path and /proc/self/exe often refer to the same large debug
    // executable. A second open still verifies its current inode and timestamps.
    let cached = cache.lock().ok().and_then(|entries| {
        entries
            .iter()
            .find(|(key, _)| *key == identity)
            .map(|(_, digest)| digest.clone())
    });
    if let Some(digest) = cached {
        ensure!(
            BinaryIdentity::from_metadata(&file.metadata()?) == identity,
            "selected executable changed while checking its identity"
        );
        return Ok(digest);
    }
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0usize;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total = total.saturating_add(count);
        ensure!(
            total <= 512 * 1024 * 1024,
            "selected executable exceeds identity size limit"
        );
        digest.update(&buffer[..count]);
    }
    let after = file.metadata()?;
    ensure!(
        BinaryIdentity::from_metadata(&after) == identity && total as u64 == identity.length,
        "selected executable changed while checking its identity"
    );
    let digest = format!("{:x}", digest.finalize());
    if let Ok(mut entries) = cache.lock() {
        if !entries.iter().any(|(key, _)| *key == identity) {
            if entries.len() == 16 {
                entries.remove(0);
            }
            entries.push((identity, digest.clone()));
        }
    }
    Ok(digest)
}

pub fn read_frame<T: DeserializeOwned>(reader: &mut impl Read) -> Result<T> {
    let mut header = [0; 4];
    reader.read_exact(&mut header)?;
    let mut body = vec![0; frame_size(header)?];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body).context("invalid IPC message")
}
fn frame_size(header: [u8; 4]) -> Result<usize> {
    let size = u32::from_be_bytes(header) as usize;
    ensure!(
        size > 0 && size <= MAX_MESSAGE,
        "IPC message exceeds allowed size"
    );
    Ok(size)
}
pub fn write_frame(writer: &mut impl Write, value: &impl Serialize) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    ensure!(
        bytes.len() <= MAX_MESSAGE,
        "IPC response exceeds allowed size; reduce terminal dimensions"
    );
    writer.write_all(&(bytes.len() as u32).to_be_bytes())?;
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(())
}
pub fn read_frame_deadline<T: DeserializeOwned>(
    stream: &mut UnixStream,
    deadline: Instant,
) -> Result<T> {
    let mut header = [0u8; 4];
    read_until(stream, &mut header, deadline)?;
    let mut body = vec![0; frame_size(header)?];
    read_until(stream, &mut body, deadline)?;
    serde_json::from_slice(&body).context("invalid IPC message")
}
pub fn write_frame_deadline(
    stream: &mut UnixStream,
    value: &impl Serialize,
    deadline: Instant,
) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    ensure!(
        bytes.len() <= MAX_MESSAGE,
        "IPC response exceeds allowed size; reduce terminal dimensions"
    );
    write_until(stream, &(bytes.len() as u32).to_be_bytes(), deadline)?;
    write_until(stream, &bytes, deadline)
}
fn remaining(deadline: Instant) -> Result<Duration> {
    let left = deadline.saturating_duration_since(Instant::now());
    ensure!(!left.is_zero(), "IPC absolute deadline exceeded");
    Ok(left)
}
fn read_until(stream: &mut UnixStream, mut bytes: &mut [u8], deadline: Instant) -> Result<()> {
    while !bytes.is_empty() {
        stream.set_read_timeout(Some(remaining(deadline)?))?;
        match stream.read(bytes) {
            Ok(0) => bail!("IPC peer disconnected before the frame completed"),
            Ok(count) => bytes = &mut bytes[count..],
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error).context("read IPC frame before deadline"),
        }
    }
    Ok(())
}
fn write_until(stream: &mut UnixStream, mut bytes: &[u8], deadline: Instant) -> Result<()> {
    while !bytes.is_empty() {
        stream.set_write_timeout(Some(remaining(deadline)?))?;
        match stream.write(bytes) {
            Ok(0) => bail!("IPC peer stopped reading"),
            Ok(count) => bytes = &bytes[count..],
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error).context("write IPC frame before deadline"),
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub struct PeerCredentials {
    pub uid: u32,
    pub pid: u32,
}
pub fn peer_credentials(stream: &UnixStream) -> Result<PeerCredentials> {
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).context("cannot verify local peer");
    }
    ensure!(
        length as usize == std::mem::size_of::<libc::ucred>() && credentials.pid > 0,
        "invalid local peer credentials"
    );
    ensure!(
        credentials.uid == unsafe { libc::geteuid() },
        "IPC peer belongs to another user"
    );
    Ok(PeerCredentials {
        uid: credentials.uid,
        pid: credentials.pid as u32,
    })
}
pub fn configure_stream(stream: &UnixStream) -> Result<()> {
    peer_credentials(stream)?;
    stream.set_read_timeout(Some(Duration::from_millis(MAX_REQUEST_MS)))?;
    stream.set_write_timeout(Some(Duration::from_millis(MAX_REQUEST_MS)))?;
    Ok(())
}

/// Connect before the same absolute RPC deadline, including a full accept backlog.
pub fn connect_deadline(path: &Path, deadline: Instant) -> Result<UnixStream> {
    crate::local_socket::connect_deadline(path, deadline)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn binary_identity_cache_tracks_same_size_changes_and_symlink_replacement() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first");
        let second = root.path().join("second");
        let alias = root.path().join("selected");
        std::fs::write(&first, b"first binary").unwrap();
        std::fs::write(&second, b"other binary").unwrap();
        std::os::unix::fs::symlink(&first, &alias).unwrap();
        let original = executable_build_id(&alias).unwrap();
        assert_eq!(original, executable_build_id(&first).unwrap());
        std::fs::write(&first, b"newer binary").unwrap();
        let changed = executable_build_id(&first).unwrap();
        assert_ne!(original, changed);
        assert_eq!(changed, format!("{:x}", Sha256::digest(b"newer binary")));
        std::fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(&second, &alias).unwrap();
        assert_eq!(
            executable_build_id(&alias).unwrap(),
            executable_build_id(&second).unwrap()
        );
        assert_ne!(changed, executable_build_id(&alias).unwrap());
    }
    #[test]
    fn unit_success_survives_the_wire_and_missing_payload_is_still_rejected() {
        let response = Response::success(new_id(), &())
            .unwrap()
            .with_host(&new_id());
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &response).unwrap();
        read_frame::<Response>(&mut bytes.as_slice())
            .unwrap()
            .decode::<()>()
            .unwrap();
        let mut malformed = serde_json::to_value(&response).unwrap();
        malformed.as_object_mut().unwrap().remove("data");
        assert!(serde_json::from_value::<Response>(malformed)
            .unwrap()
            .decode::<()>()
            .is_err());
    }
    #[test]
    fn frame_bounds_protocol_and_deadlines_are_checked() {
        let oversized = ((MAX_MESSAGE + 1) as u32).to_be_bytes();
        assert!(read_frame::<Envelope>(&mut oversized.as_slice()).is_err());
        let mut envelope = Envelope::new(&new_id(), Request::Hello);
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &envelope).unwrap();
        read_frame::<Envelope>(&mut bytes.as_slice())
            .unwrap()
            .validate()
            .unwrap();
        envelope.protocol = 99;
        assert!(envelope.validate().is_err());
        envelope.protocol = PROTOCOL;
        envelope.deadline_ms = 0;
        assert!(envelope.validate().is_err());
        assert!(read_frame::<Envelope>(&mut &bytes[..bytes.len() - 1]).is_err());
    }
    #[test]
    fn debug_does_not_expose_launch_secrets_or_terminal_input() {
        let request = Request::Start {
            project: new_id(),
            terminal: new_id(),
            rows: 24,
            cols: 80,
            env: BTreeMap::from([("TOKEN".into(), "private-value".into())]),
            reopen: false,
        };
        assert!(!format!("{request:?}").contains("private-value"));
        let input = Request::Input {
            session: new_id(),
            data: "private-keystrokes".into(),
            epoch: 1,
        };
        assert!(!format!("{input:?}").contains("private-keystrokes"));
        assert!(!format!(
            "{:?}",
            Response::success(new_id(), &"private-output").unwrap()
        )
        .contains("private-output"));
    }
    #[test]
    fn trickle_reads_do_not_reset_the_absolute_budget() {
        let (mut reader, mut writer) = UnixStream::pair().unwrap();
        let thread = std::thread::spawn(move || {
            for byte in [0u8, 0, 0, 10, b'{'] {
                if writer.write_all(&[byte]).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        let start = Instant::now();
        assert!(read_frame_deadline::<serde_json::Value>(
            &mut reader,
            start + Duration::from_millis(55)
        )
        .is_err());
        assert!(start.elapsed() < Duration::from_millis(200));
        drop(reader);
        thread.join().unwrap();
    }
    #[test]
    fn binary_identity_rejects_fifo_before_open_can_block() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("binary");
        let cpath = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
        let start = Instant::now();
        assert!(executable_build_id(&path).is_err());
        assert!(start.elapsed() < Duration::from_millis(200));
    }
}
