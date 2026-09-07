//! Display-only Git messages. Sealed reviews, plans, commands and credentials
//! remain in the host; clients select opaque IDs and exact status entry indices.
use crate::git::Repository;
use crate::protocol::InputOwner;
use crate::terminal::TerminalSnapshot;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "task", rename_all = "snake_case", deny_unknown_fields)]
pub enum GitTask {
    Open {
        project: String,
        repository: Option<PathBuf>,
        env: BTreeMap<String, String>,
    },
    Status {
        context: String,
        refresh: bool,
    },
    Diff {
        context: String,
        snapshot: String,
        entry: Option<usize>,
        target: DiffTarget,
    },
    History {
        context: String,
        limit: usize,
    },
    CommitFiles {
        context: String,
        oid: String,
    },
    Branches {
        context: String,
    },
    Remotes {
        context: String,
    },
    CommitReview {
        context: String,
    },
    Stage {
        context: String,
        snapshot: String,
        entries: Vec<usize>,
    },
    Unstage {
        context: String,
        snapshot: String,
        entries: Vec<usize>,
    },
    Commit {
        context: String,
        review: String,
        message: String,
    },
    CreateBranch {
        context: String,
        name: String,
        start: Option<String>,
    },
    Switch {
        context: String,
        snapshot: String,
        name: String,
    },
    Fetch {
        context: String,
        remote: String,
    },
    Pull {
        context: String,
        snapshot: String,
        remote: String,
        branch: String,
    },
    Push {
        context: String,
        snapshot: String,
        remote: String,
        branch: String,
    },
}
impl std::fmt::Debug for GitTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitTask")
            .field("kind", &std::mem::discriminant(self))
            .field("payload", &"[redacted]")
            .finish()
    }
}
impl GitTask {
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        use crate::model::{absolute_path, valid_id};
        use anyhow::ensure;
        if let Some(context) = self.context() {
            valid_id(context)?;
        }
        fn text(value: &str, limit: usize) -> anyhow::Result<()> {
            ensure!(
                !value.is_empty() && value.len() <= limit && !value.chars().any(char::is_control),
                "Git field exceeds its visible-text limit"
            );
            Ok(())
        }
        match self {
            Self::Open {
                project,
                repository,
                env,
            } => {
                valid_id(project)?;
                if let Some(path) = repository {
                    absolute_path(path)?;
                }
                crate::protocol::validate_environment(env)?;
            }
            Self::Diff {
                snapshot, entry, ..
            } => {
                valid_id(snapshot)?;
                ensure!(
                    entry.is_none_or(|entry| entry < 100_000),
                    "Git entry index exceeds limit"
                );
            }
            Self::History { limit, .. } => {
                ensure!((1..=200).contains(limit), "history limit must be 1–200")
            }
            Self::CommitFiles { oid, .. } => {
                crate::git::ObjectId::parse(oid)?;
            }
            Self::Stage {
                snapshot, entries, ..
            }
            | Self::Unstage {
                snapshot, entries, ..
            } => {
                valid_id(snapshot)?;
                ensure!(
                    !entries.is_empty()
                        && entries.len() <= 4096
                        && entries.iter().all(|entry| *entry < 100_000),
                    "select 1–4096 bounded Git entries"
                );
                ensure!(
                    entries
                        .iter()
                        .collect::<std::collections::HashSet<_>>()
                        .len()
                        == entries.len(),
                    "duplicate Git entry selection"
                );
            }
            Self::Commit {
                review, message, ..
            } => {
                valid_id(review)?;
                ensure!(
                    !message.trim().is_empty() && message.len() <= 65536 && !message.contains('\0'),
                    "commit message must be nonempty and at most 64 KiB"
                );
            }
            Self::CreateBranch { name, start, .. } => {
                text(name, 1024)?;
                if let Some(oid) = start {
                    crate::git::ObjectId::parse(oid)?;
                }
            }
            Self::Switch { snapshot, name, .. } => {
                valid_id(snapshot)?;
                text(name, 1024)?;
            }
            Self::Fetch { remote, .. } => text(remote, 256)?,
            Self::Pull {
                snapshot,
                remote,
                branch,
                ..
            }
            | Self::Push {
                snapshot,
                remote,
                branch,
                ..
            } => {
                valid_id(snapshot)?;
                text(remote, 256)?;
                text(branch, 1024)?;
            }
            Self::Status { .. }
            | Self::Branches { .. }
            | Self::Remotes { .. }
            | Self::CommitReview { .. } => {}
        }
        Ok(())
    }
    pub fn context(&self) -> Option<&str> {
        match self {
            Self::Open { .. } => None,
            Self::Status { context, .. }
            | Self::Diff { context, .. }
            | Self::History { context, .. }
            | Self::CommitFiles { context, .. }
            | Self::Branches { context }
            | Self::Remotes { context }
            | Self::CommitReview { context }
            | Self::Stage { context, .. }
            | Self::Unstage { context, .. }
            | Self::Commit { context, .. }
            | Self::CreateBranch { context, .. }
            | Self::Switch { context, .. }
            | Self::Fetch { context, .. }
            | Self::Pull { context, .. }
            | Self::Push { context, .. } => Some(context),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitJobState {
    Pending,
    Running,
    Ready,
    Failed,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitJobInfo {
    pub id: String,
    pub context_id: Option<String>,
    pub state: GitJobState,
    pub result: Option<GitValue>,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum GitValue {
    Open(GitContextInfo),
    Status(GitStatusReply),
    Diff(DiffView),
    History(Vec<CommitSummary>),
    CommitFiles(Vec<GitChange>),
    Branches(Vec<Branch>),
    Remotes(Vec<Remote>),
    CommitReview(GitCommitReview),
    Plan(GitOperationPreview),
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GitGateState {
    pub provider_ready: bool,
    pub runs: BTreeMap<String, String>,
    pub mutation: Option<String>,
    pub generation: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitContextInfo {
    pub id: String,
    pub project_id: String,
    pub repository: Repository,
    pub primary: bool,
    pub source_use: GitGateState,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitStatusReply {
    pub context_id: String,
    pub snapshot_id: String,
    pub snapshot: GitSnapshot,
    pub source_use: GitGateState,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitCommitReview {
    pub context_id: String,
    pub review_id: String,
    pub snapshot: GitSnapshot,
    pub staged: Vec<GitChange>,
    pub diff: DiffView,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitPath {
    pub bytes_base64: String,
    pub display: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Head {
    Unborn { reference: String },
    Branch { reference: String, oid: String },
    Detached { oid: String },
}
impl Head {
    pub fn oid(&self) -> Option<&str> {
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitRevision {
    pub repository: Repository,
    pub head: Head,
    pub index_digest: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
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
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitSnapshot {
    pub revision: GitRevision,
    pub entries: Vec<GitChange>,
    pub upstream: Option<String>,
    pub ahead: Option<u64>,
    pub behind: Option<u64>,
    pub conversion_filters_disabled: bool,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffTarget {
    Index,
    Worktree,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffView {
    pub text: String,
    pub truncated: bool,
    pub binary: bool,
    pub conversion_filters_disabled: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitSummary {
    pub oid: String,
    pub parents: Vec<String>,
    pub author: String,
    pub authored_at: i64,
    pub subject: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Branch {
    pub name: String,
    pub reference: String,
    pub oid: String,
    pub current: bool,
    pub upstream: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Remote {
    pub name: String,
    pub fetch_urls: Vec<String>,
    pub push_urls: Vec<String>,
    pub embedded_credentials: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteTarget {
    pub remote: Remote,
    pub destination: Option<String>,
    pub source: Option<String>,
    pub cached_remote_tip: Option<String>,
    pub ahead: Option<u64>,
    pub behind: Option<u64>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitOperationPreview {
    pub id: String,
    pub kind: GitOperationKind,
    pub repository: Repository,
    pub before: GitRevision,
    pub paths: Vec<GitPath>,
    pub branch: Option<String>,
    pub target_oid: Option<String>,
    pub remote: Option<RemoteTarget>,
    pub requires_source_lease: bool,
    pub interactive_output_sensitive: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitOutcome {
    Succeeded,
    Failed,
    Cancelled,
    Unknown,
    ChangedAfterExecution,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitOperationResult {
    pub id: String,
    pub kind: GitOperationKind,
    pub outcome: GitOutcome,
    pub exit_code: Option<u32>,
    pub after: Option<GitSnapshot>,
    pub commit: Option<CommitSummary>,
    pub warnings: Vec<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitOperationState {
    Pending,
    Running,
    Cancelling,
    Complete,
    Unknown,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitOperationInfo {
    pub id: String,
    pub context_id: String,
    pub project_id: String,
    pub repository: Repository,
    pub host_instance: String,
    pub kind: GitOperationKind,
    pub state: GitOperationState,
    pub owner: Option<InputOwner>,
    pub input_epoch: u64,
    pub generation: u64,
    pub terminal_available: bool,
    pub result: Option<GitOperationResult>,
    pub error: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct GitOperationSnapshot {
    pub operation: GitOperationInfo,
    pub screen: Option<TerminalSnapshot>,
}
impl std::fmt::Debug for GitOperationSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitOperationSnapshot")
            .field("operation", &self.operation.id)
            .field("screen", &"[redacted]")
            .finish()
    }
}
