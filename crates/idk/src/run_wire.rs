//! Run and raw-log positions are independent of terminal viewport generations.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunState {
    Preparing,
    Running,
    Cancelling,
    Succeeded,
    Failed,
    Cancelled,
    Unknown,
}
impl RunState {
    pub fn is_live(self) -> bool {
        matches!(self, Self::Preparing | Self::Running | Self::Cancelling)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LogState {
    Disabled,
    Recording,
    Complete,
    Limited,
    WriteFailed,
    Expired,
    Partial,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogDescriptor {
    pub run_id: String,
    pub generation: u64,
    pub state: LogState,
    pub bytes: u64,
    pub observed_bytes: u64,
    pub limit_bytes: u64,
    /// PTY output is merged; relative stdout/stderr ordering is not recoverable.
    pub merged_pty: bool,
    #[serde(default)]
    pub file_identity: Option<(u64, u64)>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResult {
    pub index: usize,
    pub name: String,
    pub exit_code: Option<i32>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceObservation {
    pub identity: Option<PathBuf>,
    pub generation: Option<u64>,
    pub git_head: Option<String>,
    pub dirty: Option<bool>,
    pub status_digest: Option<String>,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactRelation {
    pub path: Option<PathBuf>,
    pub from_task: Option<String>,
    pub from_run: Option<String>,
    /// Explicit registration relates runs; this never proves binary byte identity.
    pub verified_bytes: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunInfo {
    pub run_id: String,
    pub operation_id: String,
    pub project_id: String,
    pub task_id: String,
    pub name: String,
    pub session_id: Option<String>,
    pub state: RunState,
    pub definition_revision: u64,
    pub launch_digest: String,
    pub initialization_digest: String,
    pub cwd: PathBuf,
    pub source_roots: Vec<PathBuf>,
    pub build_outputs: Vec<PathBuf>,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
    pub timeout_seconds: Option<u64>,
    pub timeout_requested: bool,
    #[serde(default)]
    pub cancel_requested: bool,
    #[serde(default)]
    pub cleanup_confirmed: bool,
    pub exit_code: Option<u32>,
    pub signal: Option<String>,
    pub steps: Vec<StepResult>,
    pub source_start: SourceObservation,
    pub source_end: Option<SourceObservation>,
    pub source_changed: Option<bool>,
    pub artifact: ArtifactRelation,
    pub log: LogDescriptor,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunStartReply {
    pub run: RunInfo,
    pub existing: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogChunk {
    pub descriptor: LogDescriptor,
    pub offset: u64,
    pub next_offset: u64,
    /// Raw bytes are base64, never terminal escape-bearing UI text.
    pub data_base64: String,
    pub eof: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogMatch {
    pub offset: u64,
    pub line: u64,
    pub text: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogSearch {
    pub descriptor: LogDescriptor,
    pub matches: Vec<LogMatch>,
    pub truncated: bool,
    pub cancelled: bool,
}
