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

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunRequest {
    Tasks {
        project_id: String,
    },
    List {
        project_id: Option<String>,
    },
    ReviewTask {
        project_id: String,
        task_id: String,
        environment: std::collections::BTreeMap<String, String>,
    },
    ApproveTask {
        revision: u64,
        project_id: String,
        task_id: String,
        digest: String,
        environment: std::collections::BTreeMap<String, String>,
    },
    SaveTask {
        revision: u64,
        project_id: String,
        task: crate::model::TaskDefinition,
    },
    Start {
        project_id: String,
        task_id: String,
        operation_id: String,
        environment: std::collections::BTreeMap<String, String>,
        parallel: bool,
        rows: u16,
        cols: u16,
    },
    Info {
        run_id: String,
    },
    Cancel {
        run_id: String,
        force: bool,
    },
    Reconcile {
        run_id: String,
    },
    Log {
        run_id: String,
        generation: u64,
        offset: u64,
        limit: usize,
    },
    Search {
        run_id: String,
        generation: u64,
        query: String,
    },
    Problems {
        run_id: String,
    },
    EditorReview {
        run_id: String,
        problem_id: String,
        log_generation: u64,
    },
    EditorOpen {
        review_id: String,
        environment: std::collections::BTreeMap<String, String>,
        rows: u16,
        cols: u16,
    },
    SaveEditor {
        revision: u64,
        project_id: String,
        config: crate::model::EditorConfig,
    },
    Job {
        job_id: String,
    },
    CancelJob {
        job_id: String,
    },
}
impl std::fmt::Debug for RunRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunRequest")
            .field("kind", &std::mem::discriminant(self))
            .field("payload", &"[redacted]")
            .finish()
    }
}
impl RunRequest {
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        use crate::model::valid_id;
        use anyhow::ensure;
        match self {
            Self::Tasks { project_id }
            | Self::SaveTask { project_id, .. }
            | Self::SaveEditor { project_id, .. } => valid_id(project_id)?,
            Self::List { project_id } => {
                if let Some(id) = project_id {
                    valid_id(id)?;
                }
            }
            Self::ReviewTask {
                project_id,
                task_id,
                environment,
            }
            | Self::ApproveTask {
                project_id,
                task_id,
                environment,
                ..
            } => {
                valid_id(project_id)?;
                valid_id(task_id)?;
                crate::project::LaunchEnvironment::from_variables(environment.clone())?;
            }
            Self::Start {
                project_id,
                task_id,
                operation_id,
                environment,
                rows,
                cols,
                ..
            } => {
                valid_id(project_id)?;
                valid_id(task_id)?;
                valid_id(operation_id)?;
                crate::project::LaunchEnvironment::from_variables(environment.clone())?;
                crate::protocol::validate_dimensions(*rows, *cols)?;
            }
            Self::Info { run_id }
            | Self::Cancel { run_id, .. }
            | Self::Reconcile { run_id }
            | Self::Problems { run_id } => valid_id(run_id)?,
            Self::Log { run_id, limit, .. } => {
                valid_id(run_id)?;
                ensure!(
                    *limit > 0 && *limit <= 65536,
                    "log read limit must be 1–65536"
                );
            }
            Self::Search { run_id, query, .. } => {
                valid_id(run_id)?;
                ensure!(
                    !query.is_empty()
                        && query.len() <= 1024
                        && !query.chars().any(char::is_control),
                    "invalid log search query"
                );
            }
            Self::EditorReview {
                run_id, problem_id, ..
            } => {
                valid_id(run_id)?;
                ensure!(
                    problem_id.len() <= 128 && !problem_id.is_empty(),
                    "invalid problem ID"
                );
            }
            Self::EditorOpen {
                review_id,
                environment,
                rows,
                cols,
            } => {
                valid_id(review_id)?;
                crate::project::LaunchEnvironment::from_variables(environment.clone())?;
                crate::protocol::validate_dimensions(*rows, *cols)?;
            }
            Self::Job { job_id } | Self::CancelJob { job_id } => valid_id(job_id)?,
        }
        if let Self::ApproveTask { digest, .. } = self {
            ensure!(
                digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()),
                "invalid task approval digest"
            );
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunJobState {
    Pending,
    Complete,
    Failed,
    Cancelled,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunJob {
    pub job_id: String,
    pub state: RunJobState,
    pub result: Option<RunResult>,
    pub error: Option<String>,
    pub session_id: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum RunResult {
    Tasks {
        revision: u64,
        tasks: Vec<crate::model::TaskDefinition>,
    },
    Runs(Vec<RunInfo>),
    Review(crate::task::TaskReview),
    Task(crate::model::TaskDefinition),
    Approved,
    EditorSaved,
    Started(RunStartReply),
    Run(RunInfo),
    Log(LogChunk),
    Search(LogSearch),
    Problems(crate::problems::ProblemSet),
    EditorReview {
        review_id: String,
        review: crate::editor::EditorReview,
    },
    EditorOpened {
        session_id: String,
    },
    Reconciled(RunInfo),
}
