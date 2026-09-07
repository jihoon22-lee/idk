//! Blocking Git reads/planning and execution callbacks stay off the host actor.
use crate::git::{self, GitOperationPlan, GitService, Repository};
use crate::model::SourceGate;
use crate::terminal::TerminalSession;
use anyhow::{ensure, Context, Result};
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};

pub(super) enum Task {
    Open,
    Status {
        refresh: bool,
    },
    Diff {
        target: git::DiffTarget,
        path: Option<git::GitPath>,
    },
    History(usize),
    CommitFiles(git::ObjectId),
    Branches,
    Remotes,
    CommitReview,
    Stage {
        paths: Vec<git::GitPath>,
        revision: git::GitRevision,
    },
    Unstage {
        paths: Vec<git::GitPath>,
        revision: git::GitRevision,
    },
    Commit {
        review: Box<git::CommitPreview>,
        message: String,
    },
    CreateBranch {
        name: String,
        start: Option<git::ObjectId>,
    },
    Switch {
        name: String,
        revision: git::GitRevision,
    },
    Fetch(String),
    Pull {
        remote: String,
        branch: String,
        revision: git::GitRevision,
    },
    Push {
        remote: String,
        branch: String,
        revision: git::GitRevision,
    },
}
pub(super) enum Payload {
    Open,
    Status(git::GitSnapshot),
    Diff(git::DiffView),
    History(Vec<git::CommitSummary>),
    CommitFiles(Vec<git::GitChange>),
    Branches(Vec<git::Branch>),
    Remotes(Vec<git::Remote>),
    Review(git::CommitPreview),
    Plan(GitOperationPlan),
}
pub(super) struct ReadWork {
    pub id: String,
    pub binding: Repository,
    pub service: Option<GitService>,
    pub environment: BTreeMap<String, String>,
    pub task: Task,
}
pub(super) struct ExecuteWork {
    pub id: String,
    pub service: GitService,
    pub plan: GitOperationPlan,
    pub gate: SourceGate,
    pub resources: PathBuf,
    pub rows: u16,
    pub cols: u16,
    pub cancelled: Arc<AtomicBool>,
}
pub(super) enum Event {
    Read {
        id: String,
        service: Option<GitService>,
        result: Box<Result<Payload>>,
    },
    Terminal {
        id: String,
        terminal: TerminalSession,
        completion: mpsc::SyncSender<git::CommandOutcome>,
    },
    Executed {
        id: String,
        result: Box<Result<git::GitOperationResult>>,
    },
}
pub(super) struct Workers {
    pub reads: mpsc::SyncSender<ReadWork>,
    pub executions: mpsc::SyncSender<ExecuteWork>,
    pub events: mpsc::Receiver<Event>,
}

pub(super) fn workers() -> Result<Workers> {
    let (read_tx, read_rx) = mpsc::sync_channel::<ReadWork>(8);
    let (execute_tx, execute_rx) = mpsc::sync_channel::<ExecuteWork>(4);
    let (events_tx, events_rx) = mpsc::sync_channel::<Event>(16);
    let reads = Arc::new(Mutex::new(read_rx));
    for number in 0..2 {
        let incoming = reads.clone();
        let outgoing = events_tx.clone();
        std::thread::Builder::new()
            .name(format!("idk-git-read-{number}"))
            .spawn(move || loop {
                let work = match incoming.lock() {
                    Ok(receiver) => receiver.recv(),
                    Err(_) => return,
                };
                let Ok(work) = work else {
                    return;
                };
                let mut service = work.service;
                let result = (|| {
                    if service.is_none() {
                        service = Some(new_service(work.binding, work.environment)?);
                    }
                    perform(service.as_ref().unwrap(), work.task)
                })();
                if outgoing
                    .send(Event::Read {
                        id: work.id,
                        service,
                        result: Box::new(result),
                    })
                    .is_err()
                {
                    return;
                }
            })?;
    }
    let executions = Arc::new(Mutex::new(execute_rx));
    for number in 0..4 {
        let incoming = executions.clone();
        let outgoing = events_tx.clone();
        std::thread::Builder::new()
            .name(format!("idk-git-operation-{number}"))
            .spawn(move || loop {
                let work = match incoming.lock() {
                    Ok(receiver) => receiver.recv(),
                    Err(_) => return,
                };
                let Ok(work) = work else {
                    return;
                };
                let result =
                    work.service
                        .execute_with(&work.plan, &work.gate, &work.resources, |command| {
                            if work.cancelled.load(Ordering::Acquire) {
                                return Ok(git::CommandOutcome {
                                    exit_code: None,
                                    cancelled: true,
                                    output_limited: false,
                                });
                            }
                            let mut terminal =
                                TerminalSession::spawn(command, work.rows, work.cols, 2000)?;
                            terminal.defer_reaping();
                            let (completion, outcome) = mpsc::sync_channel(1);
                            outgoing
                                .send(Event::Terminal {
                                    id: work.id.clone(),
                                    terminal,
                                    completion,
                                })
                                .map_err(|_| {
                                    anyhow::anyhow!("host operation owner became unavailable")
                                })?;
                            // No timeout or optimistic cancellation result releases the
                            // service's source lease while the owned child is still alive.
                            outcome.recv().context("host operation outcome is unknown")
                        });
                if outgoing
                    .send(Event::Executed {
                        id: work.id,
                        result: Box::new(result),
                    })
                    .is_err()
                {
                    return;
                }
            })?;
    }
    Ok(Workers {
        reads: read_tx,
        executions: execute_tx,
        events: events_rx,
    })
}
fn new_service(binding: Repository, environment: BTreeMap<String, String>) -> Result<GitService> {
    let search = environment
        .get("PATH")
        .context("current frontend environment needs PATH to select Git")?;
    for directory in std::env::split_paths(search).filter(|path| path.is_absolute()) {
        let candidate = directory.join("git");
        if candidate
            .metadata()
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        {
            return GitService::new(
                git::GitExecutable::from_path(&candidate)?,
                binding,
                environment,
            );
        }
    }
    anyhow::bail!(
        "Git executable was not found in the current frontend's absolute PATH directories"
    )
}
fn perform(service: &GitService, task: Task) -> Result<Payload> {
    Ok(match task {
        Task::Open => Payload::Open,
        Task::Status { refresh } => Payload::Status(if refresh {
            service.refresh()?
        } else {
            service.status()?
        }),
        Task::Diff { target, path } => Payload::Diff(service.diff(target, path.as_ref())?),
        Task::History(limit) => {
            ensure!(limit <= 200, "history limit exceeded");
            Payload::History(service.history(limit)?)
        }
        Task::CommitFiles(oid) => Payload::CommitFiles(service.commit_files(&oid)?),
        Task::Branches => Payload::Branches(service.branches()?),
        Task::Remotes => Payload::Remotes(service.remotes()?),
        Task::CommitReview => Payload::Review(service.commit_preview()?),
        Task::Stage { paths, revision } => Payload::Plan(service.plan_stage(paths, &revision)?),
        Task::Unstage { paths, revision } => Payload::Plan(service.plan_unstage(paths, &revision)?),
        Task::Commit { review, message } => Payload::Plan(service.plan_commit(&review, &message)?),
        Task::CreateBranch { name, start } => {
            Payload::Plan(service.plan_branch_create(&name, start.as_ref())?)
        }
        Task::Switch { name, revision } => Payload::Plan(service.plan_switch(&name, &revision)?),
        Task::Fetch(remote) => Payload::Plan(service.plan_fetch(&remote)?),
        Task::Pull {
            remote,
            branch,
            revision,
        } => Payload::Plan(service.plan_pull_ff_only(&remote, &branch, &revision)?),
        Task::Push {
            remote,
            branch,
            revision,
        } => Payload::Plan(service.plan_push(&remote, &branch, &revision)?),
    })
}
