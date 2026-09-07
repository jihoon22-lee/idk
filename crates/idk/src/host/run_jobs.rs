//! Run registry and filesystem work live on one bounded worker, outside the actor.
use super::launch::Runtime;
use crate::editor::{EditorPlan, EditorService};
use crate::model::{new_id, SourceGate};
use crate::problems::{ProblemParser, ProblemSet};
use crate::project::LaunchEnvironment;
use crate::protocol::safe_error;
use crate::run::RunRegistry;
use crate::run_wire::*;
use crate::store::{ensure_private_dir, Store};
use crate::task::TaskService;
use crate::terminal::{TerminalExit, TerminalSession};
use anyhow::{bail, ensure, Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
const LIMIT_JOBS: usize = 128;
const LIMIT_CACHE: usize = 16 * 1024 * 1024;
const TTL: Duration = Duration::from_secs(300);
struct JobRecord {
    owner: String,
    info: RunJob,
    cancel: Arc<AtomicBool>,
    created: Instant,
    bytes: usize,
}
pub(super) enum Work {
    Request {
        id: String,
        client: String,
        request: Box<RunRequest>,
        session: Option<String>,
        cancel: Arc<AtomicBool>,
    },
    Cancel {
        run_id: String,
        force: bool,
    },
    Finish {
        run_id: String,
        exit: Option<TerminalExit>,
        error: Option<String>,
        partial: bool,
    },
}
pub(super) struct Event {
    pub job_id: Option<String>,
    pub session_id: Option<String>,
    pub runtime: Option<Runtime>,
    pub run: Option<RunInfo>,
    pub result: Result<RunResult>,
    pub cancel: Option<(String, bool)>,
    pub work_done: bool,
}
pub(super) struct Bridge {
    sender: mpsc::SyncSender<Work>,
    receiver: mpsc::Receiver<Event>,
    jobs: HashMap<String, JobRecord>,
    pending: usize,
}
impl Bridge {
    pub fn new(
        store: Store,
        launcher: PathBuf,
        resources: PathBuf,
        gate: SourceGate,
    ) -> Result<Self> {
        let mut registry = RunRegistry::open(store.clone(), gate)?;
        for project in store.load()?.projects {
            if let Some(repo) = project.repository_binding {
                let _ = registry.publish_source(repo.identity());
            }
        }
        let (sender, incoming) = mpsc::sync_channel(32);
        let (outgoing, receiver) = mpsc::sync_channel(32);
        std::thread::Builder::new()
            .name("idk-runs".into())
            .spawn(move || {
                let mut reviews: HashMap<String, (String, EditorPlan, Instant)> = HashMap::new();
                loop {
                    reviews.retain(|_, (_, _, created)| created.elapsed() < TTL);
                    let work = match incoming.recv_timeout(Duration::from_millis(50)) {
                        Ok(work) => Some(work),
                        Err(mpsc::RecvTimeoutError::Timeout) => None,
                        Err(_) => break,
                    };
                    if let Some(work) = work {
                        let event = match work {
                            Work::Request {
                                id,
                                client,
                                request,
                                session,
                                cancel,
                            } => {
                                let mut event = Event {
                                    job_id: Some(id),
                                    session_id: session.clone(),
                                    runtime: None,
                                    run: None,
                                    result: Ok(RunResult::Approved),
                                    cancel: None,
                                    work_done: true,
                                };
                                event.result = execute(
                                    &store,
                                    &launcher,
                                    &resources,
                                    &mut registry,
                                    &mut reviews,
                                    &client,
                                    *request,
                                    session,
                                    &cancel,
                                    &mut event.runtime,
                                    &mut event.run,
                                    &mut event.cancel,
                                );
                                event
                            }
                            Work::Cancel { run_id, force } => {
                                let result = registry.cancel(&run_id, false);
                                let run = result
                                    .as_ref()
                                    .ok()
                                    .cloned()
                                    .or_else(|| registry.info(&run_id).ok());
                                let session = run.as_ref().and_then(|run| run.session_id.clone());
                                Event {
                                    job_id: None,
                                    session_id: session,
                                    runtime: None,
                                    run,
                                    result: result.map(RunResult::Run),
                                    cancel: Some((run_id, force)),
                                    work_done: true,
                                }
                            }
                            Work::Finish {
                                run_id,
                                exit,
                                error,
                                partial,
                            } => {
                                if partial {
                                    if let Ok(sink) = registry.output_sink(&run_id) {
                                        sink.mark_partial();
                                    }
                                }
                                let result = registry.finish(&run_id, exit, true, error);
                                let run = result
                                    .as_ref()
                                    .ok()
                                    .cloned()
                                    .or_else(|| registry.info(&run_id).ok());
                                Event {
                                    job_id: None,
                                    session_id: run.as_ref().and_then(|run| run.session_id.clone()),
                                    runtime: None,
                                    run,
                                    result: result.map(RunResult::Run),
                                    cancel: None,
                                    work_done: true,
                                }
                            }
                        };
                        if outgoing.send(event).is_err() {
                            break;
                        }
                    }
                    for run_id in registry.timed_out() {
                        let result = registry.cancel(&run_id, true);
                        let run = result
                            .as_ref()
                            .ok()
                            .cloned()
                            .or_else(|| registry.info(&run_id).ok());
                        let event = Event {
                            job_id: None,
                            session_id: run.as_ref().and_then(|run| run.session_id.clone()),
                            runtime: None,
                            run,
                            result: result.map(RunResult::Run),
                            cancel: Some((run_id, false)),
                            work_done: false,
                        };
                        if outgoing.send(event).is_err() {
                            return;
                        }
                    }
                }
            })?;
        Ok(Self {
            sender,
            receiver,
            jobs: HashMap::new(),
            pending: 0,
        })
    }
    pub fn submit(
        &mut self,
        client: &str,
        request: RunRequest,
        session: Option<String>,
        cancel: Arc<AtomicBool>,
    ) -> Result<RunJob> {
        self.jobs
            .retain(|_, job| job.info.state == RunJobState::Pending || job.created.elapsed() < TTL);
        while self.jobs.len() >= LIMIT_JOBS
            || self.jobs.values().map(|job| job.bytes).sum::<usize>() >= LIMIT_CACHE
        {
            let oldest = self
                .jobs
                .iter()
                .filter(|(_, job)| job.info.state != RunJobState::Pending)
                .min_by_key(|(_, job)| job.created)
                .map(|(id, _)| id.clone())
                .context("run job cache full of pending work; wait for completion")?;
            self.jobs.remove(&oldest);
        }
        let id = new_id();
        let info = RunJob {
            job_id: id.clone(),
            state: RunJobState::Pending,
            result: None,
            error: None,
            session_id: session.clone(),
        };
        self.sender
            .try_send(Work::Request {
                id: id.clone(),
                client: client.into(),
                request: Box::new(request),
                session,
                cancel: cancel.clone(),
            })
            .map_err(|_| anyhow::anyhow!("run worker queue full; no task accepted"))?;
        self.jobs.insert(
            id,
            JobRecord {
                owner: client.into(),
                info: info.clone(),
                cancel,
                created: Instant::now(),
                bytes: 0,
            },
        );
        self.pending += 1;
        Ok(info)
    }
    pub fn job(&self, client: &str, id: &str) -> Result<RunJob> {
        let job = self.jobs.get(id).context("run job expired or not found")?;
        ensure!(job.owner == client, "run job belongs to another client");
        Ok(job.info.clone())
    }
    pub fn cancel_job(&mut self, client: &str, id: &str) -> Result<RunJob> {
        let job = self.jobs.get(id).context("run job expired or not found")?;
        ensure!(job.owner == client, "run job belongs to another client");
        ensure!(
            job.info.state == RunJobState::Pending,
            "run job already finished"
        );
        job.cancel.store(true, Ordering::Release);
        Ok(job.info.clone())
    }
    pub fn cancel_run(&mut self, id: &str, force: bool) -> Result<()> {
        self.sender
            .try_send(Work::Cancel {
                run_id: id.into(),
                force,
            })
            .map_err(|_| anyhow::anyhow!("run cancellation queue full; no process signalled"))?;
        self.pending += 1;
        Ok(())
    }
    pub fn finish(
        &mut self,
        id: &str,
        exit: Option<TerminalExit>,
        error: Option<String>,
        partial: bool,
    ) -> Result<()> {
        self.sender
            .try_send(Work::Finish {
                run_id: id.into(),
                exit,
                error,
                partial,
            })
            .map_err(|_| anyhow::anyhow!("run finalization queue full; source lease retained"))?;
        self.pending += 1;
        Ok(())
    }
    pub fn poll(&mut self) -> Option<Event> {
        let event = self.receiver.try_recv().ok()?;
        if event.work_done {
            self.pending = self.pending.saturating_sub(1);
        }
        Some(event)
    }
    pub fn complete(&mut self, id: &str, result: Result<RunResult>) {
        let retained = self
            .jobs
            .iter()
            .filter(|(key, _)| key.as_str() != id)
            .map(|(_, job)| job.bytes)
            .sum::<usize>();
        if let Some(job) = self.jobs.get_mut(id) {
            match result {
                Ok(result) => {
                    let bytes = serde_json::to_vec(&result).map_or(usize::MAX, |bytes| bytes.len());
                    if bytes > 3 * 1024 * 1024 || retained.saturating_add(bytes) > LIMIT_CACHE {
                        job.info.state = RunJobState::Failed;
                        job.info.error = Some(
                            "run result exceeds display limit; inspect smaller log ranges".into(),
                        );
                    } else {
                        job.info.state = RunJobState::Complete;
                        job.bytes = bytes;
                        job.info.result = Some(result);
                    }
                }
                Err(error) => {
                    job.info.state = if job.cancel.load(Ordering::Acquire) {
                        RunJobState::Cancelled
                    } else {
                        RunJobState::Failed
                    };
                    job.info.error = Some(safe_error(error));
                }
            }
        }
    }
    pub fn editor_project(&self, client: &str, review_id: &str) -> Result<String> {
        self.jobs
            .values()
            .filter(|job| job.owner == client && job.created.elapsed() < TTL)
            .find_map(|job| match &job.info.result {
                Some(RunResult::EditorReview {
                    review_id: id,
                    review,
                }) if id == review_id => Some(review.project_id.clone()),
                _ => None,
            })
            .context("editor review expired or belongs to another client")
    }
    pub fn idle(&self) -> bool {
        self.pending == 0
    }
}
#[allow(clippy::too_many_arguments)]
fn execute(
    store: &Store,
    launcher: &std::path::Path,
    resources: &std::path::Path,
    registry: &mut RunRegistry,
    reviews: &mut HashMap<String, (String, EditorPlan, Instant)>,
    client: &str,
    request: RunRequest,
    session: Option<String>,
    cancel: &AtomicBool,
    runtime: &mut Option<Runtime>,
    run_out: &mut Option<RunInfo>,
    cancel_out: &mut Option<(String, bool)>,
) -> Result<RunResult> {
    ensure!(
        !cancel.load(Ordering::Acquire),
        "run job cancelled before execution"
    );
    let tasks = TaskService { store };
    match request {
        RunRequest::Tasks { project_id } => {
            let workspace = store.load()?;
            Ok(RunResult::Tasks {
                revision: workspace.revision,
                tasks: workspace.project(&project_id)?.tasks.clone(),
            })
        }
        RunRequest::List { project_id } => {
            Ok(RunResult::Runs(registry.list(project_id.as_deref())))
        }
        RunRequest::ReviewTask {
            project_id,
            task_id,
            environment,
        } => Ok(RunResult::Review(tasks.review(
            &project_id,
            &task_id,
            &LaunchEnvironment::from_variables(environment)?,
        )?)),
        RunRequest::ApproveTask {
            revision,
            project_id,
            task_id,
            digest,
            environment,
        } => {
            tasks.approve(
                revision,
                &project_id,
                &task_id,
                &digest,
                &LaunchEnvironment::from_variables(environment)?,
            )?;
            Ok(RunResult::Approved)
        }
        RunRequest::SaveTask {
            revision,
            project_id,
            task,
        } => Ok(RunResult::Task(tasks.save(revision, &project_id, task)?)),
        RunRequest::SaveEditor {
            revision,
            project_id,
            config,
        } => {
            (EditorService { store }).save(revision, &project_id, config)?;
            Ok(RunResult::EditorSaved)
        }
        RunRequest::Start {
            project_id,
            task_id,
            operation_id,
            environment,
            parallel,
            rows,
            cols,
        } => {
            let session = session.context("task session was not reserved")?;
            let plan = tasks.launch_plan(
                &project_id,
                &task_id,
                LaunchEnvironment::from_variables(environment)?,
            )?;
            ensure!(
                !cancel.load(Ordering::Acquire),
                "task preparation cancelled"
            );
            let started = registry.begin(plan, &operation_id, parallel)?;
            *run_out = Some(started.run.clone());
            if started.existing {
                return Ok(RunResult::Started(started));
            }
            let id = started.run.run_id.clone();
            let result = (|| -> Result<Runtime> {
                ensure!(
                    !cancel.load(Ordering::Acquire),
                    "task cancelled before shell preparation"
                );
                let shell = registry.shell_plan(&id)?;
                let prepared = shell.prepare(resources, launcher)?;
                if cancel.load(Ordering::Acquire) {
                    let _ = std::fs::remove_dir_all(&prepared.resource_dir);
                    bail!("task cancelled before shell creation");
                }
                let output = registry.output_sink(&id)?;
                let mut terminal = match TerminalSession::spawn_with_output(
                    prepared.command,
                    rows,
                    cols,
                    2000,
                    Some(output.clone()),
                ) {
                    Ok(terminal) => terminal,
                    Err(error) => {
                        let _ = std::fs::remove_dir_all(&prepared.resource_dir);
                        return Err(error);
                    }
                };
                terminal.defer_reaping();
                Ok(Runtime {
                    terminal,
                    state_path: prepared.state_path,
                    resource_dir: prepared.resource_dir,
                    bootstrap: prepared.bootstrap_bytes,
                    revision: started.run.definition_revision,
                    digest: started.run.launch_digest.clone(),
                    output: Some(output),
                })
            })();
            match result {
                Ok(created) => {
                    *runtime = Some(created);
                    if let Err(error) = registry.mark_running(&id, &session) {
                        cancel.store(true, Ordering::Release);
                        *run_out = Some(registry.info(&id)?);
                        return Err(error);
                    }
                    if cancel.load(Ordering::Acquire) {
                        registry.cancel(&id, false)?;
                        *cancel_out = Some((id.clone(), false));
                    }
                    let run = registry.info(&id)?;
                    *run_out = Some(run.clone());
                    Ok(RunResult::Started(RunStartReply {
                        run,
                        existing: false,
                    }))
                }
                Err(error) => {
                    if cancel.load(Ordering::Acquire) {
                        let _ = registry.cancel(&id, false);
                    }
                    *run_out = Some(registry.finish(&id, None, true, Some(safe_error(&error)))?);
                    Err(error)
                }
            }
        }
        RunRequest::Info { run_id } => {
            let run = registry.info(&run_id)?;
            *run_out = Some(run.clone());
            Ok(RunResult::Run(run))
        }
        RunRequest::Cancel { run_id, force } => {
            let run = registry.cancel(&run_id, false)?;
            *cancel_out = Some((run_id, force));
            *run_out = Some(run.clone());
            Ok(RunResult::Run(run))
        }
        RunRequest::Reconcile { run_id } => {
            let run = registry.acknowledge_unknown_cleanup(&run_id)?;
            if let Some(identity) = &run.source_start.identity {
                let _ = registry.publish_source(identity);
            }
            *run_out = Some(run.clone());
            Ok(RunResult::Reconciled(run))
        }
        RunRequest::Log {
            run_id,
            generation,
            offset,
            limit,
        } => Ok(RunResult::Log(
            registry.read_log(&run_id, generation, offset, limit)?,
        )),
        RunRequest::Search {
            run_id,
            generation,
            query,
        } => Ok(RunResult::Search(
            registry.search(&run_id, generation, &query, cancel)?,
        )),
        RunRequest::Problems { run_id } => {
            Ok(RunResult::Problems(problems(registry, &run_id, cancel)?))
        }
        RunRequest::EditorReview {
            run_id,
            problem_id,
            log_generation,
        } => {
            ensure!(reviews.len() < 64, "too many pending editor reviews");
            let run = registry.info(&run_id)?;
            ensure!(
                run.log.generation == log_generation,
                "log generation changed; refresh Problems"
            );
            let set = problems(registry, &run_id, cancel)?;
            let problem = set
                .problems
                .iter()
                .find(|problem| problem.id == problem_id)
                .context("diagnostic expired or not found")?;
            let plan = (EditorService { store }).review(&run, problem)?;
            let review = plan.review().clone();
            let id = new_id();
            reviews.insert(id.clone(), (client.into(), plan, Instant::now()));
            Ok(RunResult::EditorReview {
                review_id: id,
                review,
            })
        }
        RunRequest::EditorOpen {
            review_id,
            environment,
            rows,
            cols,
        } => {
            let (owner, plan, created) = reviews
                .get(&review_id)
                .context("editor review expired; review source location again")?;
            ensure!(
                owner == client && created.elapsed() < TTL,
                "editor review belongs to another client or expired"
            );
            let command = (EditorService { store }).command(plan, environment)?;
            let session = session.context("editor session was not reserved")?;
            let revision = store.load()?.revision;
            ensure!(!cancel.load(Ordering::Acquire), "editor opening cancelled");
            let directory = resources.join(format!("editor-{}", new_id()));
            ensure_private_dir(&directory)?;
            let state_path = directory.join("state");
            crate::store::atomic_write(&state_path, b"ready\n")?;
            let mut terminal = TerminalSession::spawn(command, rows, cols, 1000)?;
            terminal.defer_reaping();
            *runtime = Some(Runtime {
                terminal,
                state_path,
                resource_dir: directory,
                bootstrap: Vec::new(),
                revision,
                digest: String::new(),
                output: None,
            });
            reviews.remove(&review_id);
            Ok(RunResult::EditorOpened {
                session_id: session,
            })
        }
        RunRequest::Job { .. } | RunRequest::CancelJob { .. } => {
            bail!("job polling belongs to the host actor")
        }
    }
}
fn problems(registry: &RunRegistry, id: &str, cancel: &AtomicBool) -> Result<ProblemSet> {
    let run = registry.info(id)?;
    let mut parser = ProblemParser::new(&run);
    let mut offset = 0;
    loop {
        ensure!(
            !cancel.load(Ordering::Acquire),
            "Problems collection cancelled"
        );
        let chunk = registry.read_log(id, run.log.generation, offset, 65536)?;
        let next = chunk.next_offset;
        let eof = chunk.eof;
        parser.push(&chunk)?;
        offset = next;
        if eof {
            break;
        }
    }
    parser.finish(&registry.info(id)?.log)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Bridge, mpsc::Receiver<Work>) {
        let (sender, incoming) = mpsc::sync_channel(32);
        let (_outgoing, receiver) = mpsc::sync_channel(32);
        (
            Bridge {
                sender,
                receiver,
                jobs: HashMap::new(),
                pending: 0,
            },
            incoming,
        )
    }
    fn record(state: RunJobState) -> JobRecord {
        JobRecord {
            owner: "client".into(),
            info: RunJob {
                job_id: new_id(),
                state,
                result: None,
                error: None,
                session_id: None,
            },
            cancel: Arc::new(AtomicBool::new(false)),
            created: Instant::now(),
            bytes: 0,
        }
    }
    #[test]
    fn repeated_follow_requests_evict_completed_jobs_but_preserve_pending_work() {
        let (mut bridge, incoming) = fixture();
        for _ in 0..LIMIT_JOBS {
            let job = record(RunJobState::Complete);
            bridge.jobs.insert(job.info.job_id.clone(), job);
        }
        let accepted = bridge
            .submit(
                "client",
                RunRequest::List { project_id: None },
                None,
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        assert_eq!(bridge.jobs.len(), LIMIT_JOBS);
        assert_eq!(
            bridge.job("client", &accepted.job_id).unwrap().state,
            RunJobState::Pending
        );
        assert!(incoming.try_recv().is_ok());
        for job in bridge.jobs.values_mut() {
            job.info.state = RunJobState::Pending;
        }
        assert!(bridge
            .submit(
                "client",
                RunRequest::List { project_id: None },
                None,
                Arc::new(AtomicBool::new(false))
            )
            .is_err());
        assert_eq!(bridge.jobs.len(), LIMIT_JOBS);
    }
    #[test]
    fn pending_large_results_cannot_exceed_aggregate_cache_limit() {
        let (mut bridge, _incoming) = fixture();
        let log = RunResult::Log(LogChunk {
            descriptor: LogDescriptor {
                run_id: new_id(),
                generation: 1,
                state: LogState::Complete,
                bytes: 0,
                observed_bytes: 0,
                limit_bytes: 4 * 1024 * 1024,
                merged_pty: true,
                file_identity: None,
            },
            offset: 0,
            next_offset: 0,
            data_base64: "a".repeat(2 * 1024 * 1024),
            eof: true,
        });
        let mut rejected = 0;
        for _ in 0..10 {
            let job = record(RunJobState::Pending);
            let id = job.info.job_id.clone();
            bridge.jobs.insert(id.clone(), job);
            bridge.complete(&id, Ok(log.clone()));
            if bridge.job("client", &id).unwrap().state == RunJobState::Failed {
                rejected += 1;
            }
        }
        assert!(rejected > 0);
        assert!(bridge.jobs.values().map(|job| job.bytes).sum::<usize>() <= LIMIT_CACHE);
    }
}
