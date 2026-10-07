//! Run registry and filesystem work live on one bounded worker, outside the actor.
use super::launch::Runtime;
use crate::editor::{EditorPlan, EditorService};
use crate::model::{new_id, SourceGate};
use crate::problems::{ProblemParser, ProblemSet};
use crate::project::LaunchEnvironment;
use crate::protocol::safe_error;
use crate::run::{observe_source, BeginReservation, BeginTicket, RunRegistry, SourceProbe};
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
const LIMIT_DEFERRED: usize = 8;
/// End-to-end budget for one deferred source observation, including queueing
/// behind earlier probes. Expiry never claims confirmed state: the entry
/// resolves with an explicit unconfirmed record instead of waiting forever.
const PROBE_TIMEOUT: Duration = Duration::from_secs(60);
const TTL: Duration = Duration::from_secs(300);
/// Source observations (bounded git subprocesses) run on a dedicated worker so
/// the registry worker never blocks on git. Results carry a sequence tag.
struct Prober {
    jobs: mpsc::SyncSender<(u64, Option<crate::git::Repository>, SourceGate)>,
    results: mpsc::Receiver<(u64, SourceObservation)>,
    seq: u64,
}
impl Prober {
    fn new() -> Option<Self> {
        // Bounded like every other worker queue: at most one job per deferred
        // entry is ever outstanding. Results also cover observations whose
        // entry already timed out, so the worker never blocks indefinitely.
        let (jobs, incoming) =
            mpsc::sync_channel::<(u64, Option<crate::git::Repository>, SourceGate)>(LIMIT_DEFERRED);
        let (results, outgoing) =
            mpsc::sync_channel::<(u64, SourceObservation)>(2 * LIMIT_DEFERRED);
        std::thread::Builder::new()
            .name("idk-run-probe".into())
            .spawn(move || {
                while let Ok((seq, repository, gate)) = incoming.recv() {
                    if results
                        .send((seq, observe_source(repository.as_ref(), &gate)))
                        .is_err()
                    {
                        return;
                    }
                }
            })
            .ok()?;
        Some(Self {
            jobs,
            results: outgoing,
            seq: 0,
        })
    }
    /// Queues an observation. A dead or saturated probe worker hands the
    /// probe back so the caller can observe synchronously instead of losing
    /// the work.
    fn dispatch(&mut self, probe: SourceProbe) -> std::result::Result<u64, SourceProbe> {
        let (repository, gate) = probe.into_parts();
        self.seq += 1;
        self.jobs
            .try_send((self.seq, repository.clone(), gate.clone()))
            .map(|()| self.seq)
            .map_err(|_| SourceProbe::from_parts(repository, gate))
    }
}
/// A work item waiting on an off-thread source observation. Registry access
/// still happens only on the worker, after the observation arrives.
///
/// Lifecycle — every entry leaves this list by exactly one of:
///   result   — the probe worker reports a `SourceObservation` whose `seq`
///              matches; `complete_deferred` runs the normal commit path.
///   expired  — `now - dispatched >= PROBE_TIMEOUT`; completes with a
///              `timed_out_observation` (explicit unconfirmed record). A
///              result that arrives later is dropped by the `seq` miss.
///   orphaned — the probe worker died (`results` disconnected); completes
///              with `lost_observation`, the same explicit unconfirmed record.
/// In all three exits the commit path runs identically, so leases,
/// pending-start reservations, and attached waiters are always released or
/// resolved — never silently. There is no durable variant: deferred state
/// dies with the worker, and recovery treats missing outcomes as unknown.
enum Deferred {
    Start {
        seq: u64,
        dispatched: Instant,
        job_id: String,
        session: String,
        rows: u16,
        cols: u16,
        cancel: Arc<AtomicBool>,
        ticket: Box<BeginTicket>,
        /// Same-intent/non-parallel requests attached while observing; each
        /// resolves to Started(existing: true) when the begin commits.
        waiters: Vec<(String, Option<String>, Arc<AtomicBool>)>,
    },
    Finish {
        seq: u64,
        dispatched: Instant,
        run_id: String,
        exit: Option<TerminalExit>,
        error: Option<String>,
        /// Repository identity captured at dispatch for the error record if
        /// the observation can never arrive.
        identity: Option<PathBuf>,
    },
}
impl Deferred {
    fn seq(&self) -> u64 {
        match self {
            Self::Start { seq, .. } | Self::Finish { seq, .. } => *seq,
        }
    }
    fn dispatched(&self) -> Instant {
        match self {
            Self::Start { dispatched, .. } | Self::Finish { dispatched, .. } => *dispatched,
        }
    }
}
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
        /// Propagates to the actor's descendant signal level through the
        /// event; it is unrelated to `RunRegistry::cancel`'s timeout flag.
        signal_level: bool,
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
    dead: bool,
}
impl Bridge {
    pub fn new(
        store: Store,
        launcher: PathBuf,
        resources: PathBuf,
        gate: SourceGate,
    ) -> Result<Self> {
        let mut registry = RunRegistry::open(store.clone(), gate)?;
        registry.publish_registered_sources()?;
        let (sender, incoming) = mpsc::sync_channel(32);
        let (outgoing, receiver) = mpsc::sync_channel(32);
        std::thread::Builder::new()
            .name("idk-runs".into())
            .spawn(move || {
                let mut reviews: HashMap<String, (String, EditorPlan, Instant)> = HashMap::new();
                let mut registration_refresh = Instant::now();
                let mut prober = Prober::new();
                let mut deferred: Vec<Deferred> = Vec::new();
                'outer: loop {
                    // Completed observations drain first: a produced result must
                    // not wait behind newer work.
                    let mut completed = Vec::new();
                    let mut prober_dead = false;
                    if let Some(probe) = &prober {
                        loop {
                            match probe.results.try_recv() {
                                Ok(done) => completed.push(done),
                                Err(mpsc::TryRecvError::Empty) => break,
                                Err(mpsc::TryRecvError::Disconnected) => {
                                    prober_dead = true;
                                    break;
                                }
                            }
                        }
                    }
                    if prober_dead {
                        prober = None;
                    }
                    for (seq, observation) in completed {
                        let Some(index) = deferred.iter().position(|entry| entry.seq() == seq)
                        else {
                            continue;
                        };
                        for event in complete_deferred(
                            &launcher,
                            &resources,
                            &mut registry,
                            deferred.remove(index),
                            observation,
                        ) {
                            if outgoing.send(event).is_err() {
                                break 'outer;
                            }
                        }
                    }
                    if prober.is_none() && !deferred.is_empty() {
                        // The observation worker is gone: resolve reserved
                        // starts/finishes with an explicit unconfirmed-source
                        // record instead of wedging their leases.
                        let entries = std::mem::take(&mut deferred);
                        for entry in entries {
                            let observation = lost_observation(&entry);
                            for event in complete_deferred(
                                &launcher,
                                &resources,
                                &mut registry,
                                entry,
                                observation,
                            ) {
                                if outgoing.send(event).is_err() {
                                    break 'outer;
                                }
                            }
                        }
                    }
                    // A live worker can still stall (wedged read, full result
                    // queue). Entries past their end-to-end budget resolve
                    // with the same explicit unconfirmed-source record; a late
                    // result is dropped by the sequence check when it arrives.
                    for entry in take_expired(&mut deferred, Instant::now()) {
                        let observation = timed_out_observation(&entry);
                        for event in complete_deferred(
                            &launcher,
                            &resources,
                            &mut registry,
                            entry,
                            observation,
                        ) {
                            if outgoing.send(event).is_err() {
                                break 'outer;
                            }
                        }
                    }
                    if registration_refresh.elapsed() >= Duration::from_secs(1) {
                        let _ = registry.publish_registered_sources();
                        registration_refresh = Instant::now();
                    }
                    let now = Instant::now();
                    reviews.retain(|_, (_, _, created)| !expired(*created, TTL, now));
                    let work = match incoming.recv_timeout(Duration::from_millis(50)) {
                        Ok(work) => Some(work),
                        Err(mpsc::RecvTimeoutError::Timeout) => None,
                        Err(_) => break,
                    };
                    if let Some(work) = work {
                        let events = match work {
                            Work::Request {
                                id,
                                client,
                                request,
                                session,
                                cancel,
                            } => request_events(
                                &store,
                                &launcher,
                                &resources,
                                &mut registry,
                                &mut reviews,
                                &mut prober,
                                &mut deferred,
                                id,
                                client,
                                *request,
                                session,
                                &cancel,
                            ),
                            Work::Cancel {
                                run_id,
                                signal_level,
                            } => {
                                // `false` is the automatic-timeout flag, not
                                // the signal level: user cancellation intent is
                                // durable regardless of descendant signalling.
                                let result = registry.cancel(&run_id, false);
                                let run = result
                                    .as_ref()
                                    .ok()
                                    .cloned()
                                    .or_else(|| registry.info(&run_id).ok());
                                let session = run.as_ref().and_then(|run| run.session_id.clone());
                                vec![Event {
                                    job_id: None,
                                    session_id: session,
                                    runtime: None,
                                    run,
                                    result: result.map(RunResult::Run),
                                    cancel: Some((run_id, signal_level)),
                                    work_done: true,
                                }]
                            }
                            Work::Finish {
                                run_id,
                                exit,
                                error,
                                partial,
                            } => finish_events(
                                &mut registry,
                                &mut prober,
                                &mut deferred,
                                run_id,
                                exit,
                                error,
                                partial,
                            ),
                        };
                        for event in events {
                            if outgoing.send(event).is_err() {
                                break 'outer;
                            }
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
            dead: false,
        })
    }
    fn ensure_live(&self) -> Result<()> {
        ensure!(
            !self.dead,
            "run worker exited; restart the host so its ledger reconciles outstanding runs"
        );
        Ok(())
    }
    /// The single registry worker is gone: in-flight work can never report.
    /// Pending client-visible jobs fail explicitly instead of polling forever.
    fn mark_dead(&mut self) {
        if self.dead {
            return;
        }
        self.dead = true;
        for job in self.jobs.values_mut() {
            if job.info.state == RunJobState::Pending {
                job.info.state = RunJobState::Failed;
                job.info.error = Some("run worker exited before producing a result".into());
            }
        }
    }
    pub fn dead(&self) -> bool {
        self.dead
    }
    pub fn idle_or_dead(&self) -> bool {
        self.pending == 0 || self.dead
    }
    pub fn submit(
        &mut self,
        client: &str,
        request: RunRequest,
        session: Option<String>,
        cancel: Arc<AtomicBool>,
    ) -> Result<RunJob> {
        self.ensure_live()?;
        let now = Instant::now();
        self.jobs.retain(|_, job| {
            job.info.state == RunJobState::Pending || !expired(job.created, TTL, now)
        });
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
        self.ensure_live()?;
        self.sender
            .try_send(Work::Cancel {
                run_id: id.into(),
                signal_level: force,
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
        self.ensure_live()?;
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
        let event = match self.receiver.try_recv() {
            Ok(event) => event,
            // A dead registry worker cannot be confused with an empty queue:
            // live runs keep their ledger records and the actor marks them.
            Err(mpsc::TryRecvError::Disconnected) => {
                self.mark_dead();
                return None;
            }
            Err(mpsc::TryRecvError::Empty) => return None,
        };
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
        let now = Instant::now();
        self.jobs
            .values()
            .filter(|job| job.owner == client && !expired(job.created, TTL, now))
            .find_map(|job| match &job.info.result {
                Some(RunResult::EditorReview {
                    review_id: id,
                    review,
                }) if id == review_id => Some(review.project_id.clone()),
                _ => None,
            })
            .context("editor review expired or belongs to another client")
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
            spawn_start(
                launcher, resources, registry, &session, rows, cols, started, cancel, runtime,
                run_out, cancel_out,
            )
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
                owner == client && !expired(*created, TTL, Instant::now()),
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

/// The post-reservation half of a Start request: prepare and spawn the
/// runtime, then mark the run running. Shared by the synchronous execute()
/// path and deferred source-observation completions.
#[allow(clippy::too_many_arguments)]
fn spawn_start(
    launcher: &std::path::Path,
    resources: &std::path::Path,
    registry: &mut RunRegistry,
    session: &str,
    rows: u16,
    cols: u16,
    started: RunStartReply,
    cancel: &AtomicBool,
    runtime: &mut Option<Runtime>,
    run_out: &mut Option<RunInfo>,
    cancel_out: &mut Option<(String, bool)>,
) -> Result<RunResult> {
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
            if let Err(error) = registry.mark_running(&id, session) {
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
/// Commits a reserved start once its source observation arrives. A cancel
/// during observation abandons the reservation so its lease is released and
/// no pending record is left behind.
#[allow(clippy::too_many_arguments)]
fn complete_start(
    launcher: &std::path::Path,
    resources: &std::path::Path,
    registry: &mut RunRegistry,
    session: &str,
    rows: u16,
    cols: u16,
    ticket: BeginTicket,
    source_start: SourceObservation,
    cancel: &AtomicBool,
    runtime: &mut Option<Runtime>,
    run_out: &mut Option<RunInfo>,
    cancel_out: &mut Option<(String, bool)>,
) -> Result<RunResult> {
    if cancel.load(Ordering::Acquire) {
        registry.begin_abandon(ticket);
        bail!("task preparation cancelled");
    }
    let started = registry.begin_commit(ticket, source_start)?;
    *run_out = Some(started.run.clone());
    spawn_start(
        launcher, resources, registry, session, rows, cols, started, cancel, runtime, run_out,
        cancel_out,
    )
}
/// True when `created` is `ttl` or older as of `now`. Saturating: a `created`
/// timestamp in the future reads as fresh, never as expired. All TTL/reap
/// checks in this module go through here so the boundary is testable without
/// sleeping.
fn expired(created: Instant, ttl: Duration, now: Instant) -> bool {
    now.saturating_duration_since(created) >= ttl
}
/// Removes entries whose observation exceeded the end-to-end probe budget.
/// Split from the loop so the bound itself is directly testable.
fn take_expired(deferred: &mut Vec<Deferred>, now: Instant) -> Vec<Deferred> {
    let mut out = Vec::new();
    let mut index = 0;
    while index < deferred.len() {
        if expired(deferred[index].dispatched(), PROBE_TIMEOUT, now) {
            out.push(deferred.remove(index));
        } else {
            index += 1;
        }
    }
    out
}
/// An observation that cannot arrive in time. Recorded as an explicit error
/// so a dead or stalled probe worker never looks like confirmed source state.
fn unconfirmed_observation(entry: &Deferred, reason: &str) -> SourceObservation {
    let identity = match entry {
        Deferred::Start { ticket, .. } => ticket.probe_identity(),
        Deferred::Finish { identity, .. } => identity.clone(),
    };
    SourceObservation {
        identity,
        generation: None,
        git_head: None,
        dirty: None,
        status_digest: None,
        error: Some(reason.into()),
    }
}
fn lost_observation(entry: &Deferred) -> SourceObservation {
    unconfirmed_observation(
        entry,
        "source observation worker exited before reporting; source state unconfirmed",
    )
}
fn timed_out_observation(entry: &Deferred) -> SourceObservation {
    unconfirmed_observation(
        entry,
        "source observation exceeded its time budget; source state unconfirmed",
    )
}
/// Resolves a deferred work item once its observation (or an explicit error
/// observation) exists. Registry access stays on this thread.
fn complete_deferred(
    launcher: &std::path::Path,
    resources: &std::path::Path,
    registry: &mut RunRegistry,
    entry: Deferred,
    observation: SourceObservation,
) -> Vec<Event> {
    match entry {
        Deferred::Start {
            job_id,
            session,
            rows,
            cols,
            cancel,
            ticket,
            waiters,
            ..
        } => {
            let mut runtime = None;
            let mut run_out = None;
            let mut cancel_out = None;
            let result = complete_start(
                launcher,
                resources,
                registry,
                &session,
                rows,
                cols,
                *ticket,
                observation,
                &cancel,
                &mut runtime,
                &mut run_out,
                &mut cancel_out,
            );
            // Attached waiters share the start's outcome as existing:true, or
            // its explicit error when the commit failed before a run existed.
            let waiter_events: Vec<Event> = waiters
                .into_iter()
                .map(|(waiter_id, waiter_session, waiter_cancel)| {
                    let (run, result) = if waiter_cancel.load(Ordering::Acquire) {
                        (
                            run_out.clone(),
                            Err(anyhow::anyhow!("run job cancelled before execution")),
                        )
                    } else {
                        match &result {
                            Ok(RunResult::Started(reply)) => (
                                run_out.clone(),
                                Ok(RunResult::Started(RunStartReply {
                                    run: reply.run.clone(),
                                    existing: true,
                                })),
                            ),
                            Ok(other) => (run_out.clone(), Ok(other.clone())),
                            Err(error) => {
                                (run_out.clone(), Err(anyhow::anyhow!(error.to_string())))
                            }
                        }
                    };
                    Event {
                        job_id: Some(waiter_id),
                        session_id: waiter_session,
                        runtime: None,
                        run,
                        result,
                        cancel: None,
                        work_done: true,
                    }
                })
                .collect();
            let mut events = vec![Event {
                job_id: Some(job_id),
                session_id: Some(session),
                runtime,
                run: run_out,
                result,
                cancel: cancel_out,
                work_done: true,
            }];
            events.extend(waiter_events);
            events
        }
        Deferred::Finish {
            run_id,
            exit,
            error,
            ..
        } => {
            let result = registry.finish_observed(&run_id, exit, true, error, observation);
            let run = result
                .as_ref()
                .ok()
                .cloned()
                .or_else(|| registry.info(&run_id).ok());
            vec![Event {
                job_id: None,
                session_id: run.as_ref().and_then(|run| run.session_id.clone()),
                runtime: None,
                run,
                result: result.map(RunResult::Run),
                cancel: None,
                work_done: true,
            }]
        }
    }
}
/// Routes a client request: attaches duplicate starts to an in-flight
/// deferred begin, defers new source observations to the probe worker, and
/// runs everything else synchronously on the registry worker.
#[allow(clippy::too_many_arguments)]
fn request_events(
    store: &Store,
    launcher: &std::path::Path,
    resources: &std::path::Path,
    registry: &mut RunRegistry,
    reviews: &mut HashMap<String, (String, EditorPlan, Instant)>,
    prober: &mut Option<Prober>,
    deferred: &mut Vec<Deferred>,
    id: String,
    client: String,
    request: RunRequest,
    session: Option<String>,
    cancel: &Arc<AtomicBool>,
) -> Vec<Event> {
    if let RunRequest::Start {
        project_id,
        task_id,
        operation_id,
        environment,
        parallel,
        rows,
        cols,
    } = &request
    {
        // A start matching an in-flight begin attaches to its outcome instead
        // of racing a second reservation for the same intent or task.
        for entry in deferred.iter_mut() {
            let Deferred::Start {
                ticket, waiters, ..
            } = entry
            else {
                continue;
            };
            let same_operation = ticket.operation_id() == operation_id;
            let same_task = ticket.project_id() == project_id && ticket.task_id() == task_id;
            if !same_operation && !(same_task && !parallel) {
                continue;
            }
            if same_operation {
                let tasks = TaskService { store };
                let intent = LaunchEnvironment::from_variables(environment.clone())
                    .and_then(|env| tasks.launch_plan(project_id, task_id, env))
                    .map(|plan| ticket.same_intent(&plan));
                match intent {
                    Ok(true) => {}
                    Ok(false) | Err(_) => {
                        let error = intent.err().unwrap_or_else(|| {
                            anyhow::anyhow!(
                                "operation ID already refers to a different execution intent"
                            )
                        });
                        return vec![Event {
                            job_id: Some(id),
                            session_id: session,
                            runtime: None,
                            run: None,
                            result: Err(error),
                            cancel: None,
                            work_done: true,
                        }];
                    }
                }
            }
            waiters.push((id, session, cancel.clone()));
            return Vec::new();
        }
        // New starts defer their git observation when a session exists, the
        // caller has not cancelled, and the deferred bound allows it; anything
        // else stays synchronous.
        if let Some(session_id) = &session {
            if !cancel.load(Ordering::Acquire) && deferred.len() < LIMIT_DEFERRED {
                let tasks = TaskService { store };
                let dispatch = LaunchEnvironment::from_variables(environment.clone())
                    .and_then(|env| tasks.launch_plan(project_id, task_id, env))
                    .and_then(|plan| registry.begin_reserve(plan, operation_id, *parallel));
                match dispatch {
                    Ok(BeginReservation::Existing(reply)) => {
                        return vec![Event {
                            job_id: Some(id),
                            session_id: Some(session_id.clone()),
                            runtime: None,
                            run: Some(reply.run.clone()),
                            result: Ok(RunResult::Started(reply)),
                            cancel: None,
                            work_done: true,
                        }];
                    }
                    Ok(BeginReservation::Fresh(ticket)) => {
                        let probe = ticket.probe(registry.gate());
                        let dispatched = match prober.as_mut() {
                            Some(worker) => worker.dispatch(probe),
                            None => Err(probe),
                        };
                        match dispatched {
                            Ok(seq) => {
                                deferred.push(Deferred::Start {
                                    seq,
                                    dispatched: Instant::now(),
                                    job_id: id,
                                    session: session_id.clone(),
                                    rows: *rows,
                                    cols: *cols,
                                    cancel: cancel.clone(),
                                    ticket: Box::new(ticket),
                                    waiters: Vec::new(),
                                });
                                return Vec::new();
                            }
                            Err(probe) => {
                                let observation = probe.observe_now();
                                let mut runtime = None;
                                let mut run_out = None;
                                let mut cancel_out = None;
                                let result = complete_start(
                                    launcher,
                                    resources,
                                    registry,
                                    session_id,
                                    *rows,
                                    *cols,
                                    ticket,
                                    observation,
                                    cancel,
                                    &mut runtime,
                                    &mut run_out,
                                    &mut cancel_out,
                                );
                                return vec![Event {
                                    job_id: Some(id),
                                    session_id: Some(session_id.clone()),
                                    runtime,
                                    run: run_out,
                                    result,
                                    cancel: cancel_out,
                                    work_done: true,
                                }];
                            }
                        }
                    }
                    Err(error) => {
                        return vec![Event {
                            job_id: Some(id),
                            session_id: Some(session_id.clone()),
                            runtime: None,
                            run: None,
                            result: Err(error),
                            cancel: None,
                            work_done: true,
                        }];
                    }
                }
            }
        }
    }
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
        store,
        launcher,
        resources,
        registry,
        reviews,
        &client,
        request,
        session,
        cancel,
        &mut event.runtime,
        &mut event.run,
        &mut event.cancel,
    );
    vec![event]
}
/// Finishes a run: the source observation is deferred to the probe worker
/// when the run owns a repository; without one the observation is trivial
/// and the finish stays synchronous.
fn finish_events(
    registry: &mut RunRegistry,
    prober: &mut Option<Prober>,
    deferred: &mut Vec<Deferred>,
    run_id: String,
    exit: Option<TerminalExit>,
    error: Option<String>,
    partial: bool,
) -> Vec<Event> {
    if partial {
        if let Ok(sink) = registry.output_sink(&run_id) {
            sink.mark_partial();
        }
    }
    if deferred.len() < LIMIT_DEFERRED {
        if let Ok(probe) = registry.finish_probe(&run_id) {
            if probe.identity().is_some() {
                let identity = probe.identity();
                let dispatched = match prober.as_mut() {
                    Some(worker) => worker.dispatch(probe),
                    None => Err(probe),
                };
                match dispatched {
                    Ok(seq) => {
                        deferred.push(Deferred::Finish {
                            seq,
                            dispatched: Instant::now(),
                            run_id,
                            exit,
                            error,
                            identity,
                        });
                        return Vec::new();
                    }
                    Err(probe) => {
                        let observation = probe.observe_now();
                        let result =
                            registry.finish_observed(&run_id, exit, true, error, observation);
                        let run = result
                            .as_ref()
                            .ok()
                            .cloned()
                            .or_else(|| registry.info(&run_id).ok());
                        return vec![Event {
                            job_id: None,
                            session_id: run.as_ref().and_then(|run| run.session_id.clone()),
                            runtime: None,
                            run,
                            result: result.map(RunResult::Run),
                            cancel: None,
                            work_done: true,
                        }];
                    }
                }
            }
        }
    }
    let result = registry.finish(&run_id, exit, true, error);
    let run = result
        .as_ref()
        .ok()
        .cloned()
        .or_else(|| registry.info(&run_id).ok());
    vec![Event {
        job_id: None,
        session_id: run.as_ref().and_then(|run| run.session_id.clone()),
        runtime: None,
        run,
        result: result.map(RunResult::Run),
        cancel: None,
        work_done: true,
    }]
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Bridge, mpsc::Receiver<Work>, mpsc::SyncSender<Event>) {
        let (sender, incoming) = mpsc::sync_channel(32);
        let (outgoing, receiver) = mpsc::sync_channel(32);
        (
            Bridge {
                sender,
                receiver,
                jobs: HashMap::new(),
                pending: 0,
                dead: false,
            },
            incoming,
            outgoing,
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
    fn dead_registry_worker_fails_pending_jobs_and_rejects_work() {
        let (mut bridge, _incoming, outgoing) = fixture();
        let job = record(RunJobState::Pending);
        let id = job.info.job_id.clone();
        bridge.jobs.insert(id.clone(), job);
        bridge.pending = 1;
        // The only Event sender lives in the worker; dropping it is its death.
        drop(outgoing);
        assert!(bridge.poll().is_none());
        assert!(bridge.dead());
        assert!(bridge.idle_or_dead());
        let job = bridge.job("client", &id).unwrap();
        assert_eq!(job.state, RunJobState::Failed);
        assert!(job.error.unwrap().contains("run worker exited"));
        assert!(bridge
            .submit(
                "client",
                RunRequest::List { project_id: None },
                None,
                Arc::new(AtomicBool::new(false)),
            )
            .is_err());
        assert!(bridge.cancel_run(&new_id(), false).is_err());
        assert!(bridge.finish(&new_id(), None, None, false).is_err());
    }
    #[test]
    fn live_worker_empty_poll_is_not_death() {
        let (mut bridge, _incoming, _outgoing) = fixture();
        assert!(bridge.poll().is_none());
        assert!(!bridge.dead());
        assert!(bridge
            .submit(
                "client",
                RunRequest::List { project_id: None },
                None,
                Arc::new(AtomicBool::new(false)),
            )
            .is_ok());
    }
    #[test]
    fn repeated_follow_requests_evict_completed_jobs_but_preserve_pending_work() {
        let (mut bridge, incoming, _outgoing) = fixture();
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
        let (mut bridge, _incoming, _outgoing) = fixture();
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
    #[test]
    fn prober_queue_is_bounded_and_saturated_dispatch_hands_probe_back() {
        // Nothing drains the job channel: it fills at LIMIT_DEFERRED and the
        // next dispatch returns the probe for the synchronous fallback.
        let (jobs, _incoming) =
            mpsc::sync_channel::<(u64, Option<crate::git::Repository>, SourceGate)>(LIMIT_DEFERRED);
        let (_results, outgoing) =
            mpsc::sync_channel::<(u64, SourceObservation)>(2 * LIMIT_DEFERRED);
        let mut prober = Prober {
            jobs,
            results: outgoing,
            seq: 0,
        };
        for _ in 0..LIMIT_DEFERRED {
            assert!(prober
                .dispatch(SourceProbe::from_parts(None, SourceGate::default()))
                .is_ok());
        }
        assert!(prober
            .dispatch(SourceProbe::from_parts(None, SourceGate::default()))
            .is_err());
    }
    #[test]
    fn dead_prober_dispatch_hands_probe_back() {
        let (jobs, incoming) =
            mpsc::sync_channel::<(u64, Option<crate::git::Repository>, SourceGate)>(LIMIT_DEFERRED);
        let (_results, outgoing) =
            mpsc::sync_channel::<(u64, SourceObservation)>(2 * LIMIT_DEFERRED);
        let mut prober = Prober {
            jobs,
            results: outgoing,
            seq: 0,
        };
        drop(incoming);
        assert!(prober
            .dispatch(SourceProbe::from_parts(None, SourceGate::default()))
            .is_err());
    }
    #[test]
    fn expired_respects_ttl_boundary_and_future_timestamps() {
        let base = Instant::now();
        assert!(!expired(base, TTL, base));
        assert!(!expired(base, TTL, base + TTL - Duration::from_millis(1)));
        assert!(expired(base, TTL, base + TTL));
        // A `created` in the future saturates to fresh, never to expired.
        assert!(!expired(base + Duration::from_secs(1), TTL, base));
    }
    #[test]
    fn take_expired_removes_only_overbudget_entries() {
        let mut deferred = vec![
            Deferred::Finish {
                seq: 1,
                dispatched: Instant::now(),
                run_id: new_id(),
                exit: None,
                error: None,
                identity: Some(PathBuf::from("/repo")),
            },
            Deferred::Finish {
                seq: 2,
                dispatched: Instant::now(),
                run_id: new_id(),
                exit: None,
                error: None,
                identity: None,
            },
        ];
        // Fresh entries stay; an observation time past the budget expires.
        assert!(take_expired(&mut deferred, Instant::now()).is_empty());
        assert_eq!(deferred.len(), 2);
        let expired = take_expired(
            &mut deferred,
            Instant::now() + PROBE_TIMEOUT + Duration::from_secs(1),
        );
        assert_eq!(expired.len(), 2);
        assert!(deferred.is_empty());
    }
    #[test]
    fn timed_out_observation_is_explicitly_unconfirmed() {
        let entry = Deferred::Finish {
            seq: 7,
            dispatched: Instant::now(),
            run_id: new_id(),
            exit: None,
            error: None,
            identity: Some(PathBuf::from("/repo")),
        };
        let observation = timed_out_observation(&entry);
        assert_eq!(observation.identity, Some(PathBuf::from("/repo")));
        assert!(observation.generation.is_none());
        assert!(observation.git_head.is_none());
        assert!(observation.dirty.is_none());
        assert!(observation.status_digest.is_none());
        assert!(observation
            .error
            .unwrap()
            .contains("source state unconfirmed"));
    }
}
