use super::Repository;
use anyhow::{bail, ensure, Result};
use base64::Engine;
use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

/// Repository-relative filename bytes. UI display strings never become pathspecs.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GitPath(Vec<u8>);
impl GitPath {
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
        ensure!(
            !bytes.is_empty() && bytes.len() <= 8192 && !bytes.contains(&0),
            "invalid Git filename"
        );
        ensure!(
            !bytes.starts_with(b"/")
                && !bytes
                    .split(|b| *b == b'/')
                    .any(|part| part == b".." || part == b"." || part.is_empty()),
            "Git filename must stay inside its repository"
        );
        Ok(Self(bytes))
    }
    pub fn from_text(path: &str) -> Result<Self> {
        Self::from_bytes(path.as_bytes().to_vec())
    }
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
    pub fn as_os_str(&self) -> &OsStr {
        OsStr::from_bytes(&self.0)
    }
    pub fn display(&self) -> String {
        display_bytes(&self.0)
    }
}
impl std::fmt::Debug for GitPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("GitPath").field(&self.display()).finish()
    }
}
impl Serialize for GitPath {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("GitPath", 2)?;
        state.serialize_field(
            "bytes_base64",
            &base64::engine::general_purpose::STANDARD.encode(&self.0),
        )?;
        state.serialize_field("display", &self.display())?;
        state.end()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ObjectId(String);
impl ObjectId {
    pub fn parse(value: &str) -> Result<Self> {
        ensure!(
            matches!(value.len(), 40 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid Git object ID"
        );
        Ok(Self(value.to_ascii_lowercase()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Head {
    Unborn { reference: String },
    Branch { reference: String, oid: ObjectId },
    Detached { oid: ObjectId },
}
impl Head {
    pub fn oid(&self) -> Option<&ObjectId> {
        match self {
            Self::Unborn { .. } => None,
            Self::Branch { oid, .. } | Self::Detached { oid } => Some(oid),
        }
    }
    pub fn reference(&self) -> Option<&str> {
        match self {
            Self::Unborn { reference } | Self::Branch { reference, .. } => Some(reference),
            Self::Detached { .. } => None,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GitRevision {
    pub repository: Repository,
    pub head: Head,
    pub index_digest: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GitChange {
    pub path: GitPath,
    pub original_path: Option<GitPath>,
    pub index_status: char,
    pub worktree_status: char,
    pub conflict: bool,
    pub untracked: bool,
    pub submodule: Option<String>,
}
impl GitChange {
    pub fn staged(&self) -> bool {
        !matches!(self.index_status, '.' | ' ' | '?') && !self.untracked
    }
    pub fn paths(&self) -> Vec<GitPath> {
        let mut paths = vec![self.path.clone()];
        if let Some(original) = &self.original_path {
            paths.push(original.clone());
        }
        paths
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct GitSnapshot {
    pub revision: GitRevision,
    pub entries: Vec<GitChange>,
    pub upstream: Option<String>,
    pub ahead: Option<u64>,
    pub behind: Option<u64>,
    /// External conversion filters are intentionally not run for read inspection.
    pub conversion_filters_disabled: bool,
    /// Nested worktrees are not traversed with their own helper configuration.
    pub submodule_worktrees_unchecked: bool,
}
impl GitSnapshot {
    pub fn clean(&self) -> bool {
        self.entries.is_empty() && !self.submodule_worktrees_unchecked
    }
    pub fn conflicted(&self) -> bool {
        self.entries.iter().any(|entry| entry.conflict)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffTarget {
    Index,
    Worktree,
}
#[derive(Clone, Debug, Serialize)]
pub struct DiffView {
    /// Control characters are escaped for an ordinary TUI, never replayed as ANSI.
    pub text: String,
    pub truncated: bool,
    pub binary: bool,
    pub conversion_filters_disabled: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct CommitSummary {
    pub oid: ObjectId,
    pub parents: Vec<ObjectId>,
    pub author: String,
    pub authored_at: i64,
    pub subject: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct Branch {
    pub name: String,
    pub reference: String,
    pub oid: ObjectId,
    pub current: bool,
    pub upstream: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Remote {
    pub name: String,
    /// URL userinfo/query data is never returned here.
    pub fetch_urls: Vec<String>,
    pub push_urls: Vec<String>,
    pub embedded_credentials: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct RemoteTarget {
    pub remote: Remote,
    pub destination: Option<String>,
    pub source: Option<ObjectId>,
    /// Only local tracking state; this is not a current network observation.
    pub cached_remote_tip: Option<ObjectId>,
    pub ahead: Option<u64>,
    pub behind: Option<u64>,
}
#[derive(Clone, Debug, Serialize)]
pub struct CommitPreview {
    pub snapshot: GitSnapshot,
    pub staged: Vec<GitChange>,
    pub diff: DiffView,
    #[serde(skip_serializing)]
    pub(super) seal: CommitSeal,
}
#[derive(Clone, Debug)]
pub(super) struct CommitSeal {
    pub service: String,
    pub revision: GitRevision,
    pub staged_digest: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitOperationKind {
    Stage,
    Unstage,
    Commit,
    CreateBranch,
    SwitchBranch,
    Fetch,
    PullFastForward,
    Push,
}
#[derive(Clone, Debug, Serialize)]
pub struct GitOperationPreview {
    pub id: String,
    pub kind: GitOperationKind,
    pub repository: Repository,
    pub before: GitRevision,
    pub paths: Vec<GitPath>,
    pub branch: Option<String>,
    pub target_oid: Option<ObjectId>,
    pub remote: Option<RemoteTarget>,
    pub requires_source_lease: bool,
    pub interactive_output_sensitive: bool,
}
/// This describes a *reaped* owned Git process. Returning from an executor while
/// its child still runs is a contract violation; cancellation must reap first.
#[derive(Clone, Debug)]
pub struct CommandOutcome {
    pub exit_code: Option<u32>,
    pub cancelled: bool,
    pub output_limited: bool,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitOutcome {
    Succeeded,
    Failed,
    Cancelled,
    Unknown,
    ChangedAfterExecution,
}
#[derive(Clone, Debug, Serialize)]
pub struct GitOperationResult {
    pub id: String,
    pub kind: GitOperationKind,
    pub outcome: GitOutcome,
    pub exit_code: Option<u32>,
    pub after: Option<GitSnapshot>,
    pub commit: Option<CommitSummary>,
    pub warnings: Vec<String>,
}

pub(super) fn display_bytes(bytes: &[u8]) -> String {
    let mut result = String::new();
    let mut rest = bytes;
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(text) => {
                result.extend(text.chars().flat_map(char::escape_debug));
                break;
            }
            Err(error) => {
                let valid = &rest[..error.valid_up_to()];
                result.extend(
                    std::str::from_utf8(valid)
                        .unwrap()
                        .chars()
                        .flat_map(char::escape_debug),
                );
                let invalid = error
                    .error_len()
                    .unwrap_or(rest.len() - error.valid_up_to());
                for byte in &rest[error.valid_up_to()..error.valid_up_to() + invalid] {
                    result.push_str(&format!("\\x{byte:02x}"));
                }
                rest = &rest[error.valid_up_to() + invalid..];
            }
        }
    }
    result
}
pub(super) fn text(bytes: &[u8]) -> Result<String> {
    match std::str::from_utf8(bytes) {
        Ok(value) if !value.contains('\0') => Ok(value.into()),
        _ => bail!("Git returned an unsupported text field"),
    }
}
