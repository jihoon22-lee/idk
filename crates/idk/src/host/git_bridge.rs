//! Host-held Git authority: immutable sealed plans/reviews, repository-scoped
//! single flight, explicit frontend environments, and dedicated operation PTYs.
use super::{children, git_jobs as jobs, git_operation::Operation};
use crate::git::{self, GitService, Repository};
use crate::git_wire::*;
use crate::model::{new_id, valid_id, SourceGate};
use crate::protocol::{safe_error, Request, MAX_INPUT_PACKET};
use crate::store::{ensure_private_dir, Store};
use anyhow::{bail, ensure, Context, Result};
use base64::Engine;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const LEDGER: &str = "git-operations.json";
const MAX_VALUE: usize = 3 * 1024 * 1024;
const CACHE_BYTES: usize = 16 * 1024 * 1024;
const TOKEN_TTL: Duration = Duration::from_secs(300);
const MAX_OPERATIONS: usize = 128;

struct Repo {
    binding: Repository,
    environment: BTreeMap<String, String>,
    environment_digest: [u8; 32],
    generation: String,
    service: Option<GitService>,
    busy: Option<String>,
    touched: Instant,
}
struct ContextRecord {
    info: GitContextInfo,
    client: String,
    repository: PathBuf,
    generation: String,
    ready: bool,
    touched: Instant,
}
struct Job {
    info: GitJobInfo,
    client: String,
    project: String,
    repository: PathBuf,
    generation: String,
    task: Option<jobs::Task>,
    created: Instant,
    bytes: usize,
}
struct Snapshot {
    context: String,
    value: git::GitSnapshot,
    created: Instant,
    bytes: usize,
}
struct Review {
    context: String,
    value: git::CommitPreview,
    created: Instant,
    bytes: usize,
}
struct Plan {
    context: String,
    client: String,
    repository: PathBuf,
    generation: String,
    value: git::GitOperationPlan,
    created: Instant,
    bytes: usize,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    schema: u32,
    operations: Vec<Record>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    id: String,
    context_id: String,
    project_id: String,
    repository: Repository,
    kind: GitOperationKind,
    state: GitOperationState,
    #[serde(default)]
    cleanup_acknowledged: bool,
    outcome: Option<GitOutcome>,
    exit_code: Option<u32>,
    commit: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

pub(super) struct Bridge {
    store: Store,
    instance: String,
    resources: PathBuf,
    repos: BTreeMap<PathBuf, Repo>,
    contexts: HashMap<String, ContextRecord>,
    jobs: HashMap<String, Job>,
    queue: VecDeque<String>,
    snapshots: HashMap<String, Snapshot>,
    reviews: HashMap<String, Review>,
    plans: HashMap<String, Plan>,
    operations: BTreeMap<String, Operation>,
    workers: jobs::Workers,
    reading: usize,
}

fn wire<T: DeserializeOwned>(value: &impl Serialize) -> Result<T> {
    Ok(serde_json::from_value(serde_json::to_value(value)?)?)
}
fn size(value: &impl Serialize) -> Result<usize> {
    Ok(serde_json::to_vec(value)?.len())
}
fn bounded(value: &impl Serialize) -> Result<usize> {
    let bytes = size(value)?;
    ensure!(
        bytes <= MAX_VALUE,
        "Git result exceeds the 3 MiB display/cache limit; no full result is claimed"
    );
    Ok(bytes)
}

impl Bridge {
    pub fn new(store: Store, instance: &str) -> Result<Self> {
        let resources = store
            .runtime_dir
            .join(format!("host-{instance}"))
            .join("git");
        ensure_private_dir(&resources)?;
        let mut bridge = Self {
            store,
            instance: instance.into(),
            resources,
            repos: BTreeMap::new(),
            contexts: HashMap::new(),
            jobs: HashMap::new(),
            queue: VecDeque::new(),
            snapshots: HashMap::new(),
            reviews: HashMap::new(),
            plans: HashMap::new(),
            operations: BTreeMap::new(),
            workers: jobs::workers()?,
            reading: 0,
        };
        if let Some(ledger) = bridge.store.read_state::<Ledger>(LEDGER)? {
            ensure!(
                ledger.schema == 1 && ledger.operations.len() <= MAX_OPERATIONS,
                "unsupported or oversized Git operation ledger; preserved"
            );
            for record in ledger.operations {
                valid_id(&record.id)?;
                valid_id(&record.context_id)?;
                valid_id(&record.project_id)?;
                for path in [
                    &record.repository.root,
                    &record.repository.git_dir,
                    &record.repository.common_dir,
                ] {
                    crate::model::absolute_path(path)?;
                }
                ensure!(
                    !bridge.operations.contains_key(&record.id),
                    "duplicate Git operation ledger identity"
                );
                let complete = record.state == GitOperationState::Complete;
                let result = record.outcome.filter(|_| complete).map(|outcome| GitOperationResult {
                    id: record.id.clone(), kind: record.kind, outcome, exit_code: record.exit_code, after: None, commit: None,
                    warnings: vec![record.commit.map(|oid| format!("Historical result; recorded commit {oid}. Refresh the repository for current state.")).unwrap_or_else(|| "Historical result; refresh the repository for current state.".into())],
                });
                let mut operation = Operation::new(GitOperationInfo {
                    id: record.id.clone(),
                    context_id: record.context_id,
                    project_id: record.project_id,
                    repository: record.repository,
                    host_instance: instance.into(),
                    kind: record.kind,
                    state: if complete {
                        GitOperationState::Complete
                    } else {
                        GitOperationState::Unknown
                    },
                    cleanup_acknowledged: record.cleanup_acknowledged,
                    owner: None,
                    input_epoch: 0,
                    generation: 0,
                    terminal_available: false,
                    result,
                    error: if complete {
                        record.error
                    } else {
                        Some("previous host did not record a confirmed Git outcome; no command or PID was adopted/replayed; inspect repository and remote state".into())
                    },
                });
                operation.finished_at = Some(Instant::now());
                bridge.operations.insert(record.id, operation);
            }
        }
        bridge.persist()?;
        Ok(bridge)
    }
    fn persist(&self) -> Result<()> {
        let operations = self
            .operations
            .values()
            .map(|operation| Record {
                id: operation.info.id.clone(),
                context_id: operation.info.context_id.clone(),
                project_id: operation.info.project_id.clone(),
                repository: operation.info.repository.clone(),
                kind: operation.info.kind,
                state: operation.info.state,
                cleanup_acknowledged: operation.info.cleanup_acknowledged,
                outcome: operation
                    .info
                    .result
                    .as_ref()
                    .map(|result| result.outcome.clone()),
                exit_code: operation
                    .info
                    .result
                    .as_ref()
                    .and_then(|result| result.exit_code),
                commit: operation
                    .info
                    .result
                    .as_ref()
                    .and_then(|result| result.commit.as_ref().map(|commit| commit.oid.clone())),
                error: operation.info.error.clone(),
            })
            .collect();
        self.store.write_state(
            LEDGER,
            &Ledger {
                schema: 1,
                operations,
            },
        )
    }
    pub fn unresolved_sources(&self) -> Vec<(PathBuf, String)> {
        self.operations
            .values()
            .filter(|operation| {
                operation.info.state == GitOperationState::Unknown
                    && !operation.info.cleanup_acknowledged
            })
            .map(|operation| {
                (
                    operation.info.repository.identity().to_path_buf(),
                    operation.info.id.clone(),
                )
            })
            .collect()
    }
    pub fn occupied(&self) -> usize {
        self.operations
            .values()
            .filter(|operation| operation.active())
            .count()
    }
    pub fn client_owns_input(&self, client: &str) -> bool {
        self.operations.values().any(|operation| {
            operation.active()
                && operation
                    .info
                    .owner
                    .as_ref()
                    .is_some_and(|owner| owner.client_id == client)
        })
    }
    pub fn check_close(&self, project: Option<&str>) -> Result<()> {
        let operations: Vec<_> = self
            .operations
            .values()
            .filter(|operation| {
                operation.active()
                    && project.is_none_or(|project| project == operation.info.project_id)
            })
            .take(3)
            .map(|operation| format!("{} ({:?})", operation.info.id, operation.info.kind))
            .collect();
        ensure!(operations.is_empty(), "active Git operations must finish or be explicitly cancelled before reviewing terminal/host close: {}", operations.join(", "));
        ensure!(!self.jobs.values().any(|job| matches!(job.info.state, GitJobState::Pending | GitJobState::Running) && project.is_none_or(|project| project == job.project)),
            "Git read/prepare jobs are still running; wait for their result before reviewing terminal/host close");
        Ok(())
    }
    pub fn idle(&self) -> bool {
        self.occupied() == 0 && self.reading == 0 && self.queue.is_empty()
    }
    pub fn anchors(&self) -> Vec<children::Anchor> {
        self.operations
            .values()
            .filter_map(Operation::anchor)
            .collect()
    }
    pub fn inventory(&mut self, report: &children::Report) {
        for operation in self.operations.values_mut() {
            operation.inventory(report);
        }
    }

    pub fn request(
        &mut self,
        client: &str,
        request: &Request,
        shell_count: usize,
        quiescing: bool,
        gate: &SourceGate,
    ) -> Result<Option<serde_json::Value>> {
        fn value(value: impl Serialize) -> Result<Option<serde_json::Value>> {
            Ok(Some(serde_json::to_value(value)?))
        }
        match request {
            Request::GitSubmit { task } => {
                ensure!(!quiescing, "host shutdown is in progress");
                value(self.submit(client, task.clone(), gate)?)
            }
            Request::GitJob { job } => {
                let job = self
                    .jobs
                    .get(job)
                    .context("Git job expired or is unknown; refresh explicitly")?;
                ensure!(job.client == client, "Git job belongs to another client");
                value(&job.info)
            }
            Request::GitExecute { plan, rows, cols } => {
                ensure!(!quiescing, "host shutdown is in progress");
                value(self.execute(client, plan, *rows, *cols, shell_count, gate)?)
            }
            Request::GitOperations { project } => value(
                self.operations
                    .values()
                    .filter(|operation| {
                        project
                            .as_ref()
                            .is_none_or(|project| project == &operation.info.project_id)
                    })
                    .map(|operation| &operation.info)
                    .collect::<Vec<_>>(),
            ),
            Request::GitOperationAttach {
                operation,
                takeover,
            } => value(self.operation(operation)?.attach(client, *takeover)?),
            Request::GitOperationDetach { operation, epoch } => {
                value(self.operation(operation)?.detach(client, *epoch)?)
            }
            Request::GitOperationSnapshot { operation, since } => {
                value(self.operation(operation)?.snapshot(*since)?)
            }
            Request::GitOperationInput {
                operation,
                epoch,
                data,
            } => {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .context("invalid Git operation input encoding")?;
                ensure!(
                    bytes.len() <= MAX_INPUT_PACKET,
                    "Git operation input exceeds 64 KiB"
                );
                self.operation(operation)?.input(client, *epoch, &bytes)?;
                value(())
            }
            Request::GitOperationResize {
                operation,
                epoch,
                rows,
                cols,
            } => {
                self.operation(operation)?
                    .resize(client, *epoch, *rows, *cols)?;
                value(())
            }
            Request::GitOperationReconcile {
                operation,
                repository,
            } => {
                let saved = self.operation(operation)?;
                ensure!(
                    saved.info.state == GitOperationState::Unknown
                        && !saved.active()
                        && !saved.info.terminal_available,
                    "only a recovered unknown Git operation can be reconciled"
                );
                ensure!(
                    &saved.info.repository == repository,
                    "Git reconciliation target changed; review the exact operation and repository"
                );
                let previous = saved.info.clone();
                saved.info.cleanup_acknowledged = true;
                saved.info.error=Some("user confirmed old process cleanup; Git outcome remains unknown, inspect local and remote state".into());
                if let Err(error) = self.persist() {
                    self.operation(operation)?.info = previous;
                    return Err(error);
                }
                gate.unblock(repository.identity(), operation)?;
                value(self.operation(operation)?.info.clone())
            }
            Request::GitOperationCancel {
                operation,
                epoch,
                force,
            } => {
                let info = self.operation(operation)?.cancel(client, *epoch, *force)?;
                if self.persist().is_err() {
                    self.operation(operation)?.info.error = Some(
                        "Git cancellation requested, but durable state could not be saved".into(),
                    );
                }
                value(info)
            }
            _ => Ok(None),
        }
    }
    fn operation(&mut self, id: &str) -> Result<&mut Operation> {
        self.operations
            .get_mut(id)
            .context("Git operation is unknown or its completed history expired")
    }
    fn gate_state(gate: &SourceGate, repository: &Repository) -> GitGateState {
        gate.state(repository.identity())
            .ok()
            .and_then(|state| wire(&state).ok())
            .unwrap_or_default()
    }
    fn binding(&self, project: &str, selection: Option<&Path>) -> Result<(Repository, bool)> {
        let workspace = self.store.load()?;
        let project = workspace.project(project)?;
        match selection {
            None => Ok((
                project
                    .repository_binding
                    .clone()
                    .context("project has no verified primary Git binding")?,
                true,
            )),
            Some(root) => Ok((
                project
                    .related_repositories
                    .iter()
                    .find(|repository| repository.root == root)
                    .context("selected related repository is not registered for this project")?
                    .clone(),
                false,
            )),
        }
    }
    fn context(&self, id: &str, client: &str) -> Result<&ContextRecord> {
        let context = self.contexts.get(id).context("Git context expired or its environment changed; reopen Git with the current frontend environment")?;
        ensure!(
            context.client == client && context.ready,
            "Git context belongs to another client or is still opening"
        );
        let repo = self
            .repos
            .get(&context.repository)
            .context("Git repository context expired")?;
        ensure!(
            repo.generation == context.generation,
            "Git environment changed; review again"
        );
        let (binding, _) = self.binding(
            &context.info.project_id,
            (!context.info.primary).then_some(context.info.repository.root.as_path()),
        )?;
        ensure!(
            binding == context.info.repository,
            "project Git binding changed; reopen its context before another operation"
        );
        Ok(context)
    }
    fn invalidate_repo(&mut self, repository: &Path) {
        let ids: std::collections::HashSet<_> = self
            .contexts
            .iter()
            .filter(|(_, context)| context.repository == repository)
            .map(|(id, _)| id.clone())
            .collect();
        self.contexts.retain(|id, _| !ids.contains(id));
        self.snapshots
            .retain(|_, value| !ids.contains(&value.context));
        self.reviews
            .retain(|_, value| !ids.contains(&value.context));
        self.plans.retain(|_, value| value.repository != repository);
    }
    fn prune(&mut self) {
        self.snapshots.retain(|_, value| {
            value.created.elapsed() < TOKEN_TTL && self.contexts.contains_key(&value.context)
        });
        self.reviews.retain(|_, value| {
            value.created.elapsed() < TOKEN_TTL && self.contexts.contains_key(&value.context)
        });
        self.plans.retain(|_, value| {
            value.created.elapsed() < TOKEN_TTL && self.contexts.contains_key(&value.context)
        });
        self.jobs.retain(|_, job| {
            matches!(job.info.state, GitJobState::Pending | GitJobState::Running)
                || job.created.elapsed() < TOKEN_TTL
        });
        self.contexts.retain(|id, context| {
            context.touched.elapsed() < TOKEN_TTL
                || self.jobs.values().any(|job| {
                    job.info.context_id.as_ref() == Some(id)
                        && matches!(job.info.state, GitJobState::Pending | GitJobState::Running)
                })
                || self
                    .operations
                    .values()
                    .any(|operation| operation.active() && &operation.info.context_id == id)
        });
    }
    fn job_capacity(&mut self) -> Result<()> {
        self.prune();
        while self.jobs.len() >= 128
            || self.jobs.values().map(|job| job.bytes).sum::<usize>() + MAX_VALUE > CACHE_BYTES
        {
            let oldest = self
                .jobs
                .iter()
                .filter(|(_, job)| {
                    matches!(job.info.state, GitJobState::Ready | GitJobState::Failed)
                })
                .min_by_key(|(_, job)| job.created)
                .map(|(id, _)| id.clone())
                .context("Git job cache is full; no request accepted")?;
            self.jobs.remove(&oldest);
        }
        ensure!(
            self.queue.len() + self.reading < 32,
            "Git preparation queue is full; no request accepted"
        );
        Ok(())
    }
    fn submit(&mut self, client: &str, task: GitTask, gate: &SourceGate) -> Result<GitJobInfo> {
        task.validate()?;
        self.job_capacity()?;
        let (context_id, project, repository, generation, task) = match task {
            GitTask::Open {
                project,
                repository: selection,
                env,
            } => {
                let (binding, primary) = self.binding(&project, selection.as_deref())?;
                let key = binding.git_dir.clone();
                let environment_digest: [u8; 32] = Sha256::digest(serde_json::to_vec(&env)?).into();
                let changed = self.repos.get(&key).is_some_and(|repo| {
                    repo.environment_digest != environment_digest || repo.binding != binding
                });
                if changed {
                    ensure!(self.repos.get(&key).unwrap().busy.is_none() && !self.queue.iter().any(|id| self.jobs.get(id).is_some_and(|job| job.repository == key)),
                        "Git repository is busy with its accepted environment; finish that work before opening with changed authentication/helper references");
                    self.invalidate_repo(&key);
                    self.repos.remove(&key);
                }
                if !self.repos.contains_key(&key) {
                    if self.repos.len() >= 16 {
                        let oldest = self
                            .repos
                            .iter()
                            .filter(|(key, repo)| {
                                repo.busy.is_none()
                                    && !self.queue.iter().any(|id| {
                                        self.jobs.get(id).is_some_and(|job| &job.repository == *key)
                                    })
                            })
                            .min_by_key(|(_, repo)| repo.touched)
                            .map(|(key, _)| key.clone())
                            .context("all Git repository slots are busy")?;
                        self.invalidate_repo(&oldest);
                        self.repos.remove(&oldest);
                    }
                    self.repos.insert(
                        key.clone(),
                        Repo {
                            binding: binding.clone(),
                            environment: env,
                            environment_digest,
                            generation: new_id(),
                            service: None,
                            busy: None,
                            touched: Instant::now(),
                        },
                    );
                }
                if self.contexts.len() >= 256 {
                    let oldest = self
                        .contexts
                        .iter()
                        .filter(|(id, _)| {
                            !self.jobs.values().any(|job| {
                                job.info.context_id.as_ref() == Some(*id)
                                    && matches!(
                                        job.info.state,
                                        GitJobState::Pending | GitJobState::Running
                                    )
                            }) && !self.operations.values().any(|operation| {
                                operation.active() && &operation.info.context_id == *id
                            })
                        })
                        .min_by_key(|(_, context)| context.touched)
                        .map(|(id, _)| id.clone())
                        .context("Git context capacity reached")?;
                    self.contexts.remove(&oldest);
                }
                let id = new_id();
                let generation = self.repos.get(&key).unwrap().generation.clone();
                let info = GitContextInfo {
                    id: id.clone(),
                    project_id: project.clone(),
                    repository: binding.clone(),
                    primary,
                    source_use: Self::gate_state(gate, &binding),
                };
                self.contexts.insert(
                    id.clone(),
                    ContextRecord {
                        info,
                        client: client.into(),
                        repository: key.clone(),
                        generation: generation.clone(),
                        ready: false,
                        touched: Instant::now(),
                    },
                );
                (id, project, key, generation, jobs::Task::Open)
            }
            task => {
                let id = task
                    .context()
                    .context("Git task needs a context")?
                    .to_owned();
                let context = self.context(&id, client)?;
                let (project, repository, generation) = (
                    context.info.project_id.clone(),
                    context.repository.clone(),
                    context.generation.clone(),
                );
                let task = self.resolve_task(&id, task)?;
                self.contexts.get_mut(&id).unwrap().touched = Instant::now();
                (id, project, repository, generation, task)
            }
        };
        let id = new_id();
        let info = GitJobInfo {
            id: id.clone(),
            context_id: Some(context_id),
            state: GitJobState::Pending,
            result: None,
            error: None,
        };
        self.jobs.insert(
            id.clone(),
            Job {
                info: info.clone(),
                client: client.into(),
                project,
                repository,
                generation,
                task: Some(task),
                created: Instant::now(),
                bytes: 512,
            },
        );
        self.queue.push_back(id);
        Ok(info)
    }
    fn snapshot(&self, context: &str, snapshot: &str) -> Result<&git::GitSnapshot> {
        let snapshot = self
            .snapshots
            .get(snapshot)
            .context("Git snapshot expired; refresh before selecting files")?;
        ensure!(
            snapshot.context == context && snapshot.created.elapsed() < TOKEN_TTL,
            "Git snapshot belongs to another or expired context"
        );
        Ok(&snapshot.value)
    }
    fn resolve_task(&self, context: &str, task: GitTask) -> Result<jobs::Task> {
        fn selected(snapshot: &git::GitSnapshot, entries: &[usize]) -> Result<Vec<git::GitPath>> {
            let mut paths = std::collections::BTreeSet::new();
            for index in entries {
                for path in snapshot
                    .entries
                    .get(*index)
                    .context("selected Git entry is not in the reviewed snapshot")?
                    .paths()
                {
                    paths.insert(path);
                }
            }
            Ok(paths.into_iter().collect())
        }
        Ok(match task {
            GitTask::Status { refresh, .. } => jobs::Task::Status { refresh },
            GitTask::Diff {
                snapshot,
                entry,
                target,
                ..
            } => {
                let snapshot = self.snapshot(context, &snapshot)?;
                let path = entry
                    .map(|index| {
                        snapshot
                            .entries
                            .get(index)
                            .context("Git diff entry is not in its snapshot")
                            .map(|entry| entry.path.clone())
                    })
                    .transpose()?;
                jobs::Task::Diff {
                    target: match target {
                        DiffTarget::Index => git::DiffTarget::Index,
                        DiffTarget::Worktree => git::DiffTarget::Worktree,
                    },
                    path,
                }
            }
            GitTask::History { limit, .. } => jobs::Task::History(limit),
            GitTask::CommitFiles { oid, .. } => {
                jobs::Task::CommitFiles(git::ObjectId::parse(&oid)?)
            }
            GitTask::Branches { .. } => jobs::Task::Branches,
            GitTask::Remotes { .. } => jobs::Task::Remotes,
            GitTask::CommitReview { .. } => jobs::Task::CommitReview,
            GitTask::Stage {
                snapshot, entries, ..
            } => {
                let snapshot = self.snapshot(context, &snapshot)?;
                jobs::Task::Stage {
                    paths: selected(snapshot, &entries)?,
                    revision: snapshot.revision.clone(),
                }
            }
            GitTask::Unstage {
                snapshot, entries, ..
            } => {
                let snapshot = self.snapshot(context, &snapshot)?;
                jobs::Task::Unstage {
                    paths: selected(snapshot, &entries)?,
                    revision: snapshot.revision.clone(),
                }
            }
            GitTask::Commit {
                review, message, ..
            } => {
                let review = self
                    .reviews
                    .get(&review)
                    .context("sealed commit review expired; review the real index again")?;
                ensure!(
                    review.context == context && review.created.elapsed() < TOKEN_TTL,
                    "commit review belongs to another context"
                );
                jobs::Task::Commit {
                    review: Box::new(review.value.clone()),
                    message,
                }
            }
            GitTask::CreateBranch { name, start, .. } => jobs::Task::CreateBranch {
                name,
                start: start.map(|oid| git::ObjectId::parse(&oid)).transpose()?,
            },
            GitTask::Switch { snapshot, name, .. } => jobs::Task::Switch {
                name,
                revision: self.snapshot(context, &snapshot)?.revision.clone(),
            },
            GitTask::Fetch { remote, .. } => jobs::Task::Fetch(remote),
            GitTask::Pull {
                snapshot,
                remote,
                branch,
                ..
            } => jobs::Task::Pull {
                remote,
                branch,
                revision: self.snapshot(context, &snapshot)?.revision.clone(),
            },
            GitTask::Push {
                snapshot,
                remote,
                branch,
                ..
            } => jobs::Task::Push {
                remote,
                branch,
                revision: self.snapshot(context, &snapshot)?.revision.clone(),
            },
            GitTask::Open { .. } => bail!("Git Open requires repository resolution"),
        })
    }
    fn execute(
        &mut self,
        client: &str,
        id: &str,
        rows: u16,
        cols: u16,
        shell_count: usize,
        gate: &SourceGate,
    ) -> Result<GitOperationInfo> {
        if let Some(operation) = self.operations.get(id) {
            return Ok(operation.info.clone());
        }
        self.prune();
        ensure!(
            self.occupied() < 4 && self.occupied() + shell_count < crate::model::MAX_TERMINALS,
            "owned Git/terminal process capacity reached"
        );
        if self.operations.len() >= MAX_OPERATIONS {
            let oldest = self.operations.iter().filter(|(_, operation)| !operation.active() && (operation.info.state == GitOperationState::Complete || (operation.info.state == GitOperationState::Unknown && operation.info.cleanup_acknowledged)))
                .min_by_key(|(_, operation)| operation.finished_at).map(|(id, _)| id.clone()).context("Git history capacity is occupied by unresolved outcomes; inspect them before new operations")?;
            self.operations.remove(&oldest);
        }
        let plan = self
            .plans
            .get(id)
            .context("Git plan expired or is unknown; review again; no command was replayed")?;
        ensure!(
            plan.client == client && plan.created.elapsed() < TOKEN_TTL,
            "Git plan belongs to another client or expired"
        );
        let context = self.context(&plan.context, client)?;
        ensure!(
            context.generation == plan.generation,
            "Git environment changed since planning"
        );
        let repo = self
            .repos
            .get(&plan.repository)
            .context("Git repository context expired")?;
        ensure!(
            repo.busy.is_none()
                && !self.queue.iter().any(|id| self
                    .jobs
                    .get(id)
                    .is_some_and(|job| job.repository == plan.repository)),
            "repository has another active/pending Git request; wait for its result"
        );
        let service = repo
            .service
            .clone()
            .context("Git repository service is not ready")?;
        let info = GitOperationInfo {
            id: id.into(),
            context_id: plan.context.clone(),
            project_id: context.info.project_id.clone(),
            repository: repo.binding.clone(),
            host_instance: self.instance.clone(),
            kind: wire(&plan.value.preview().kind)?,
            state: GitOperationState::Pending,
            cleanup_acknowledged: false,
            owner: None,
            input_epoch: 0,
            generation: 0,
            terminal_available: false,
            result: None,
            error: None,
        };
        let operation = Operation::new(info.clone());
        let cancelled = operation.cancelled.clone();
        self.operations.insert(id.into(), operation);
        if let Err(error) = self.persist() {
            self.operations.remove(id);
            return Err(error).context(
                "Git operation was not started because its durable record could not be saved",
            );
        }
        let plan = self.plans.remove(id).unwrap();
        self.repos.get_mut(&plan.repository).unwrap().busy = Some(id.into());
        let work = jobs::ExecuteWork {
            id: id.into(),
            service,
            plan: plan.value,
            gate: gate.clone(),
            resources: self.resources.clone(),
            rows,
            cols,
            cancelled,
        };
        if self.workers.executions.try_send(work).is_err() {
            self.repos.get_mut(&plan.repository).unwrap().busy = None;
            self.operations
                .get_mut(id)
                .unwrap()
                .finish(Err(anyhow::anyhow!(
                    "Git execution worker is unavailable; no child was started"
                )));
            let _ = self.persist();
        }
        Ok(self.operations.get(id).unwrap().info.clone())
    }

    fn publish(
        &mut self,
        job: &Job,
        payload: jobs::Payload,
        gate: &SourceGate,
    ) -> Result<GitValue> {
        let context_id = job
            .info
            .context_id
            .as_ref()
            .context("Git job context is missing")?;
        match payload {
            jobs::Payload::Open => {
                let binding = self
                    .contexts
                    .get(context_id)
                    .context("Git context expired while opening")?
                    .info
                    .repository
                    .clone();
                let source_use = Self::gate_state(gate, &binding);
                let context = self.contexts.get_mut(context_id).unwrap();
                context.ready = true;
                context.info.source_use = source_use;
                Ok(GitValue::Open(context.info.clone()))
            }
            jobs::Payload::Status(snapshot) => {
                let snapshot_wire = wire(&snapshot)?;
                let bytes = bounded(&snapshot_wire)?;
                while self.snapshots.len() >= 64
                    || self
                        .snapshots
                        .values()
                        .map(|value| value.bytes)
                        .sum::<usize>()
                        + bytes
                        > CACHE_BYTES
                {
                    let oldest = self
                        .snapshots
                        .iter()
                        .min_by_key(|(_, value)| value.created)
                        .map(|(id, _)| id.clone())
                        .context("Git snapshot cache capacity reached")?;
                    self.snapshots.remove(&oldest);
                }
                let source_use = Self::gate_state(gate, &snapshot.revision.repository);
                let id = new_id();
                self.snapshots.insert(
                    id.clone(),
                    Snapshot {
                        context: context_id.clone(),
                        value: snapshot,
                        created: Instant::now(),
                        bytes,
                    },
                );
                Ok(GitValue::Status(GitStatusReply {
                    context_id: context_id.clone(),
                    snapshot_id: id,
                    snapshot: snapshot_wire,
                    source_use,
                }))
            }
            jobs::Payload::Diff(value) => Ok(GitValue::Diff(wire(&value)?)),
            jobs::Payload::History(value) => Ok(GitValue::History(wire(&value)?)),
            jobs::Payload::CommitFiles(value) => Ok(GitValue::CommitFiles(wire(&value)?)),
            jobs::Payload::Branches(value) => Ok(GitValue::Branches(wire(&value)?)),
            jobs::Payload::Remotes(value) => Ok(GitValue::Remotes(wire(&value)?)),
            jobs::Payload::Review(review) => {
                let value = GitCommitReview {
                    context_id: context_id.clone(),
                    review_id: new_id(),
                    snapshot: wire(&review.snapshot)?,
                    staged: wire(&review.staged)?,
                    diff: wire(&review.diff)?,
                };
                let bytes = bounded(&value)?;
                while self.reviews.len() >= 32
                    || self
                        .reviews
                        .values()
                        .map(|value| value.bytes)
                        .sum::<usize>()
                        + bytes
                        > CACHE_BYTES
                {
                    let oldest = self
                        .reviews
                        .iter()
                        .min_by_key(|(_, value)| value.created)
                        .map(|(id, _)| id.clone())
                        .context("Git commit review cache capacity reached")?;
                    self.reviews.remove(&oldest);
                }
                self.reviews.insert(
                    value.review_id.clone(),
                    Review {
                        context: context_id.clone(),
                        value: review,
                        created: Instant::now(),
                        bytes,
                    },
                );
                Ok(GitValue::CommitReview(value))
            }
            jobs::Payload::Plan(plan) => {
                let preview: GitOperationPreview = wire(plan.preview())?;
                let bytes = bounded(&preview)?;
                while self.plans.len() >= 128
                    || self.plans.values().map(|value| value.bytes).sum::<usize>() + bytes
                        > CACHE_BYTES
                {
                    let oldest = self
                        .plans
                        .iter()
                        .min_by_key(|(_, value)| value.created)
                        .map(|(id, _)| id.clone())
                        .context("Git plan cache capacity reached")?;
                    self.plans.remove(&oldest);
                }
                self.plans.insert(
                    preview.id.clone(),
                    Plan {
                        context: context_id.clone(),
                        client: job.client.clone(),
                        repository: job.repository.clone(),
                        generation: job.generation.clone(),
                        value: plan,
                        created: Instant::now(),
                        bytes,
                    },
                );
                Ok(GitValue::Plan(preview))
            }
        }
    }
    pub fn tick(&mut self, gate: &SourceGate) {
        for _ in 0..16 {
            let Ok(event) = self.workers.events.try_recv() else {
                break;
            };
            match event {
                jobs::Event::Read {
                    id,
                    service,
                    result,
                } => {
                    self.reading = self.reading.saturating_sub(1);
                    let Some(mut job) = self.jobs.remove(&id) else {
                        continue;
                    };
                    let current = self
                        .repos
                        .get_mut(&job.repository)
                        .filter(|repo| repo.generation == job.generation);
                    let valid = if let Some(repo) = current {
                        if repo.busy.as_deref() == Some(&id) {
                            repo.busy = None;
                        }
                        repo.touched = Instant::now();
                        if service.is_some() {
                            repo.service = service;
                        }
                        true
                    } else {
                        false
                    };
                    let published = if valid {
                        (*result)
                            .and_then(|payload| self.publish(&job, payload, gate))
                            .and_then(|value| {
                                bounded(&value)?;
                                Ok(value)
                            })
                    } else {
                        Err(anyhow::anyhow!(
                            "Git context changed while the request ran; stale result discarded"
                        ))
                    };
                    match published {
                        Ok(value) => {
                            job.info.state = GitJobState::Ready;
                            job.info.result = Some(value);
                        }
                        Err(error) => {
                            job.info.state = GitJobState::Failed;
                            job.info.error = Some(safe_error(error));
                        }
                    }
                    job.bytes = size(&job.info).unwrap_or(512);
                    while self.jobs.values().map(|stored| stored.bytes).sum::<usize>() + job.bytes
                        > CACHE_BYTES
                    {
                        let oldest = self
                            .jobs
                            .iter()
                            .filter(|(_, stored)| {
                                matches!(
                                    stored.info.state,
                                    GitJobState::Ready | GitJobState::Failed
                                )
                            })
                            .min_by_key(|(_, stored)| stored.created)
                            .map(|(id, _)| id.clone());
                        if let Some(oldest) = oldest {
                            self.jobs.remove(&oldest);
                        } else {
                            job.info.state = GitJobState::Failed;
                            job.info.result = None;
                            job.info.error = Some(
                                "Git result cache capacity reached; refresh explicitly".into(),
                            );
                            job.bytes = size(&job.info).unwrap_or(512);
                            break;
                        }
                    }
                    self.jobs.insert(id, job);
                }
                jobs::Event::Terminal {
                    id,
                    terminal,
                    completion,
                } => {
                    // Active entries are never evicted, and no public request
                    // constructs this event or supplies a CommandBuilder.
                    self.operations
                        .get_mut(&id)
                        .expect("active Git operation retained")
                        .install(terminal, completion);
                    if self.persist().is_err() {
                        self.operations.get_mut(&id).unwrap().info.error = Some("Git process is owned, but its running state could not be recorded; inspect before restart".into());
                    }
                }
                jobs::Event::Executed { id, result } => {
                    let Some(operation) = self.operations.get_mut(&id) else {
                        continue;
                    };
                    let result = (*result).and_then(|result| {
                        let result = wire(&result)?;
                        bounded(&result)?;
                        Ok(result)
                    });
                    operation.finish(result);
                    if let Some(repo) = self.repos.get_mut(&operation.info.repository.git_dir) {
                        if repo.busy.as_deref() == Some(&id) {
                            repo.busy = None;
                        }
                    }
                    if self.persist().is_err() {
                        self.operations.get_mut(&id).unwrap().info.error = Some("Git outcome observed, but durable history save failed; restart may report unknown".into());
                    }
                }
            }
        }
        for operation in self.operations.values_mut() {
            operation.tick();
        }
        let mut complete: Vec<_> = self
            .operations
            .iter()
            .filter(|(_, operation)| !operation.active())
            .map(|(id, operation)| (operation.finished_at, id.clone()))
            .collect();
        complete.sort();
        let remove_screens = complete.len().saturating_sub(8);
        for (_, id) in complete.into_iter().take(remove_screens) {
            self.operations.get_mut(&id).unwrap().forget_screen();
        }
        while self.reading < 2 {
            let position = self.queue.iter().position(|id| {
                self.jobs.get(id).is_some_and(|job| {
                    self.repos
                        .get(&job.repository)
                        .is_some_and(|repo| repo.busy.is_none())
                })
            });
            let Some(position) = position else {
                break;
            };
            let id = self.queue.remove(position).unwrap();
            let job = self.jobs.get_mut(&id).unwrap();
            let repo = self.repos.get_mut(&job.repository).unwrap();
            let work = jobs::ReadWork {
                id: id.clone(),
                binding: repo.binding.clone(),
                service: repo.service.clone(),
                environment: repo.environment.clone(),
                task: job.task.take().unwrap(),
            };
            match self.workers.reads.try_send(work) {
                Ok(()) => {
                    repo.busy = Some(id);
                    job.info.state = GitJobState::Running;
                    self.reading += 1;
                }
                Err(_) => {
                    job.info.state = GitJobState::Failed;
                    job.info.error = Some("Git preparation worker is unavailable".into());
                }
            }
        }
    }
}
