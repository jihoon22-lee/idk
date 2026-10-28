//! Host-owned registered-run intents, source leases and bounded private PTY logs.
//! No process is adopted or signalled here; the host reports confirmed ownership cleanup.
use crate::model::{
    new_id, valid_id, FailurePolicy, GateLease, SourceGate, TaskExecution, TaskLogging,
};
use crate::run_wire::*;
use crate::saved_state::RunLedger as Ledger;
use crate::shell::ShellPlan;
use crate::store::{ensure_private_dir, read_private, FileLock, Store};
use crate::task::{steps, TaskLaunchPlan};
use crate::terminal::TerminalExit;
use anyhow::{ensure, Context, Result};
use base64::Engine;
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
const HISTORY_LIMIT: usize = crate::saved_state::RUN_RECORD_LIMIT;
const LOG_LIMIT: u64 = crate::saved_state::LOG_BYTE_LIMIT;
const CHUNK_LIMIT: usize = 64 * 1024;
const LEDGER: &str = "runs.json";
/// Automatic finalization is deliberately smaller than the explicit 1 GiB helper.
pub const MAX_AUTO_ARTIFACT_BYTES: u64 = 16 * 1024 * 1024;
#[derive(Clone)]
struct ArtifactMeasurement {
    snapshot: Option<crate::discovery::ArtifactSnapshot>,
    error: Option<String>,
    observed_at_ms: u64,
}
type SharedArtifact = Arc<Mutex<Option<ArtifactMeasurement>>>;

pub struct ProcessLaunch {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: PathBuf,
    pub interactive: bool,
}

#[derive(Clone)]
pub struct RunOutputSink(Arc<OutputInner>);
struct OutputInner {
    sender: mpsc::SyncSender<Vec<u8>>,
    descriptor: Arc<Mutex<LogDescriptor>>,
    observed: AtomicU64,
    stopped: AtomicBool,
    limited: AtomicBool,
    partial: AtomicBool,
    finished: Arc<AtomicBool>,
}
impl RunOutputSink {
    fn new(path: &Path, descriptor: LogDescriptor) -> Result<Self> {
        let file = if descriptor.state == LogState::Disabled {
            None
        } else {
            Some(
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(path)?,
            )
        };
        Self::with_file(file, descriptor)
    }
    fn with_file(file: Option<File>, mut descriptor: LogDescriptor) -> Result<Self> {
        if let Some(file) = &file {
            let metadata = file.metadata()?;
            descriptor.file_identity = Some((metadata.dev(), metadata.ino()));
        }
        let descriptor = Arc::new(Mutex::new(descriptor));
        let finished = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = mpsc::sync_channel::<Vec<u8>>(32);
        let state = descriptor.clone();
        let done = finished.clone();
        std::thread::Builder::new()
            .name("idk-run-log".into())
            .spawn(move || {
                let mut file = file;
                loop {
                    let bytes = match receiver.recv_timeout(Duration::from_millis(20)) {
                        Ok(bytes) => bytes,
                        Err(mpsc::RecvTimeoutError::Timeout) if !done.load(Ordering::Acquire) => {
                            continue
                        }
                        Err(_) => break,
                    };
                    let (count, writable) = match state.lock() {
                        Ok(desc) => (
                            (desc.limit_bytes.saturating_sub(desc.bytes) as usize).min(bytes.len()),
                            matches!(desc.state, LogState::Recording | LogState::Limited),
                        ),
                        Err(_) => break,
                    };
                    if !writable {
                        continue;
                    }
                    let result = file.as_mut().map(|file| file.write_all(&bytes[..count]));
                    if let Ok(mut desc) = state.lock() {
                        if result.is_some_and(|result| result.is_err()) {
                            desc.state = LogState::WriteFailed;
                            file = None;
                        } else {
                            desc.bytes += count as u64;
                            if count < bytes.len() {
                                desc.state = LogState::Limited;
                            }
                        }
                    }
                }
                let sync = file.as_mut().map(|file| file.sync_data());
                if let Ok(mut desc) = state.lock() {
                    if sync.is_some_and(|result| result.is_err()) {
                        desc.state = LogState::WriteFailed;
                    } else if desc.state == LogState::Recording {
                        desc.state = LogState::Complete;
                    }
                }
            })?;
        Ok(Self(Arc::new(OutputInner {
            sender,
            descriptor,
            observed: AtomicU64::new(0),
            stopped: AtomicBool::new(false),
            limited: AtomicBool::new(false),
            partial: AtomicBool::new(false),
            finished,
        })))
    }
    /// Never waits for disk or queue space. After loss, retain only the contiguous prefix.
    pub fn push(&self, bytes: &[u8]) {
        self.0
            .observed
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        if self.0.stopped.load(Ordering::Acquire) {
            return;
        }
        if self.descriptor().state == LogState::Disabled {
            return;
        }
        for chunk in bytes.chunks(8192) {
            if self.0.sender.try_send(chunk.to_vec()).is_err() {
                self.0.limited.store(true, Ordering::Release);
                self.0.stopped.store(true, Ordering::Release);
                break;
            }
        }
    }
    /// Host calls when PTY draining is cut off or the reader reports lost output.
    pub fn mark_partial(&self) {
        self.0.partial.store(true, Ordering::Release);
    }
    pub fn finish(&self) {
        self.0.stopped.store(true, Ordering::Release);
        self.0.finished.store(true, Ordering::Release);
    }
    pub fn descriptor(&self) -> LogDescriptor {
        let mut desc = self
            .0
            .descriptor
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        desc.observed_bytes = self.0.observed.load(Ordering::Relaxed);
        if self.0.limited.load(Ordering::Acquire)
            && matches!(desc.state, LogState::Recording | LogState::Complete)
        {
            desc.state = LogState::Limited;
        }
        if self.0.partial.load(Ordering::Acquire)
            && matches!(desc.state, LogState::Recording | LogState::Complete)
        {
            desc.state = LogState::Partial;
        }
        desc
    }
}
struct Active {
    plan: TaskLaunchPlan,
    shell: ShellPlan,
    sink: RunOutputSink,
    _lease: Option<GateLease>,
    _run_use: Option<FileLock>,
    directory: PathBuf,
    started: Instant,
    /// The host has reported the execution's end. Source observation and
    /// durable finalization may still be pending, with the lease retained.
    finishing: bool,
    result_snapshot: Option<crate::structured_result::StructuredResult>,
    events_snapshot: Option<crate::result_store::EventProgress>,
    artifact_measurement: SharedArtifact,
}
struct PendingResources {
    directory: PathBuf,
    log: PathBuf,
    retained: bool,
}
impl Drop for PendingResources {
    fn drop(&mut self) {
        if !self.retained {
            let _ = std::fs::remove_dir_all(&self.directory);
            let _ = std::fs::remove_file(&self.log);
        }
    }
}
pub struct RunRegistry {
    store: Store,
    gate: SourceGate,
    runs: Vec<RunInfo>,
    retired: Vec<crate::saved_state::RetiredRun>,
    active: BTreeMap<String, Active>,
    logs: PathBuf,
    results: crate::result_store::ResultStore,
    published: RefCell<HashSet<PathBuf>>,
    /// Reserved starts whose source observation is still off-thread. They hold
    /// source leases, so dedup and output-conflict checks must see them.
    pending_starts: Vec<PendingStart>,
}
/// A reserved but unpersisted run start, deduplicated like a live run.
struct PendingStart {
    run_id: String,
    project_id: String,
    task_id: String,
    operation_id: String,
    build_outputs: Vec<PathBuf>,
}
/// Inputs needed to observe a repository off the registry worker.
pub struct SourceProbe {
    repository: Option<crate::git::Repository>,
    gate: SourceGate,
    artifact: Option<(PathBuf, SharedArtifact)>,
}
impl SourceProbe {
    pub(crate) fn from_parts(repository: Option<crate::git::Repository>, gate: SourceGate) -> Self {
        Self {
            repository,
            gate,
            artifact: None,
        }
    }
    pub fn into_parts(self) -> (Option<crate::git::Repository>, SourceGate) {
        (self.repository, self.gate)
    }
    /// The observed repository identity, when the run owns a repository.
    pub fn identity(&self) -> Option<PathBuf> {
        self.repository
            .as_ref()
            .map(|repository| repository.identity().into())
    }
    pub fn has_artifact(&self) -> bool {
        self.artifact.is_some()
    }
    /// A saturated/dead worker must not move artifact hashing to the registry
    /// actor. Keep a precise unavailable observation and finish the real Run.
    pub fn skip_artifact(mut self, reason: &str) -> Self {
        if let Some((_, shared)) = self.artifact.take() {
            if let Ok(mut value) = shared.lock() {
                *value = Some(ArtifactMeasurement {
                    snapshot: None,
                    error: Some(reason.into()),
                    observed_at_ms: now_ms(),
                });
            }
        }
        self
    }
    /// Explicit synchronous API; the host dispatches artifact work off-thread.
    pub fn observe_now(self) -> SourceObservation {
        if let Some((path, shared)) = &self.artifact {
            let result = crate::discovery::snapshot_artifact(path, MAX_AUTO_ARTIFACT_BYTES);
            let measurement = match result {
                Ok(snapshot) => ArtifactMeasurement {
                    snapshot: Some(snapshot),
                    error: None,
                    observed_at_ms: now_ms(),
                },
                Err(error) => ArtifactMeasurement {
                    snapshot: None,
                    error: Some(crate::structured_result::safe(error, 512)),
                    observed_at_ms: now_ms(),
                },
            };
            if let Ok(mut value) = shared.lock() {
                *value = Some(measurement);
            }
        }
        observe_source(self.repository.as_ref(), &self.gate)
    }
}
pub enum BeginReservation {
    /// An identical or non-parallel execution already exists; no lease held.
    Existing(Box<RunStartReply>),
    /// Lease reserved; commit once the source observation arrives.
    Fresh(Box<BeginTicket>),
}
/// A reserved start whose observation is in flight. Dropping it releases the
/// source lease; the registry's pending record is removed by commit/abandon.
pub struct BeginTicket {
    run_id: String,
    plan: TaskLaunchPlan,
    operation_id: String,
    outputs: Vec<PathBuf>,
    lease: Option<GateLease>,
    run_use: FileLock,
}
impl BeginTicket {
    pub fn probe(&self, gate: &SourceGate) -> SourceProbe {
        SourceProbe::from_parts(self.plan.repository.clone(), gate.clone())
    }
    pub fn probe_identity(&self) -> Option<PathBuf> {
        self.plan
            .repository
            .as_ref()
            .map(|repository| repository.identity().into())
    }
    pub fn project_id(&self) -> &str {
        &self.plan.project_id
    }
    pub fn task_id(&self) -> &str {
        &self.plan.task.id
    }
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }
    pub fn launch_digest(&self) -> &str {
        &self.plan.launch_digest
    }
    /// Whether a second request refers to the same execution intent.
    pub fn same_intent(&self, plan: &TaskLaunchPlan) -> bool {
        self.plan.project_id == plan.project_id
            && self.plan.task.id == plan.task.id
            && self.plan.launch_digest == plan.launch_digest
    }
}
impl RunRegistry {
    pub fn open(store: Store, gate: SourceGate) -> Result<Self> {
        for project in store.load()?.projects {
            if let Some(repository) = project.repository_binding {
                gate.unavailable(repository.identity())?;
            }
        }
        let logs = store.state_dir.join("run-logs");
        ensure_private_dir(&logs)?;
        let ledger = store.read_state::<Ledger>(LEDGER)?.unwrap_or(Ledger {
            schema: 2,
            runs: vec![],
            retired: vec![],
        });
        ledger.validate()?;
        let results = crate::result_store::ResultStore::open(&store.state_dir)?;
        if ledger.schema == 1 {
            let bytes = read_private(&store.state_dir.join(LEDGER), crate::model::MAX_MESSAGE)?;
            let backup = store.state_dir.join("runs.previous-v1.json");
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&backup)
            {
                Ok(mut file) => {
                    file.write_all(&bytes)?;
                    file.sync_all()?;
                    File::open(&store.state_dir)?.sync_all()?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    ensure!(
                        read_private(&backup, crate::model::MAX_MESSAGE)? == bytes,
                        "prior v1 Run backup differs; preserve both before migration"
                    );
                }
                Err(error) => return Err(error.into()),
            }
        }
        let retired = ledger.retired;
        let mut runs = ledger.runs;
        let mut ids = std::collections::HashSet::new();
        for run in &mut runs {
            if let Some(old) = &run.structured_result {
                let migrated = results.migrate(&run.run_id, old);
                ensure!(
                    !matches!(
                        migrated,
                        crate::structured_result::StructuredResult::Limited { .. }
                            | crate::structured_result::StructuredResult::StorageFailed { .. }
                    ),
                    "cannot snapshot legacy result; original ledger preserved"
                );
                run.structured_result = Some(migrated);
            }
            valid_id(&run.run_id)?;
            valid_id(&run.operation_id)?;
            valid_id(&run.project_id)?;
            valid_id(&run.task_id)?;
            ensure!(ids.insert(run.run_id.clone()), "duplicate run ledger ID");
            crate::model::valid_name(&run.name)?;
            crate::model::absolute_path(&run.cwd)?;
            ensure!(
                run.log.run_id == run.run_id
                    && run.log.limit_bytes <= LOG_LIMIT
                    && run.log.bytes <= LOG_LIMIT
                    && run.log.generation > 0,
                "invalid run log descriptor; ledger preserved"
            );
            ensure!(
                run.steps.len() <= 64
                    && run.source_roots.len() <= 128
                    && run.build_outputs.len() <= 32,
                "run ledger record exceeds definition bounds"
            );
            for path in run.source_roots.iter().chain(&run.build_outputs) {
                crate::model::absolute_path(path)?;
            }
            if run.state.is_live() {
                run.state = RunState::Unknown;
                run.error = Some("previous host ended before confirmed cleanup; no automatic retry or process adoption".into());
            }
            if run.log.state == LogState::Recording {
                run.log.state = LogState::Partial;
            }
        }
        let mut result = Self {
            store,
            gate,
            runs,
            retired,
            active: BTreeMap::new(),
            logs,
            results,
            published: RefCell::new(HashSet::new()),
            pending_starts: Vec::new(),
        };
        result.persist()?;
        result.cleanup_retired()?;
        result.expire_history()?;
        Ok(result)
    }
    pub fn gate(&self) -> &SourceGate {
        &self.gate
    }
    /// Unknown recovered work is a restriction, never idle. Host calls after recovery.
    pub fn publish_source(&self, identity: &Path) -> Result<()> {
        ensure!(
            !self.runs.iter().any(|run| run.state == RunState::Unknown
                && !run.cleanup_confirmed
                && run.source_start.identity.as_deref() == Some(identity)),
            "source-use unknown from previous run; inspect old processes before recovery"
        );
        self.gate.ready(identity)?;
        self.published.borrow_mut().insert(identity.to_path_buf());
        Ok(())
    }
    /// Registration can change while the host is alive. Recovered unknown
    /// executions and independent Git blocks still prevent ready source use.
    pub fn publish_registered_sources(&self) -> Result<()> {
        for project in self.store.load()?.projects {
            if let Some(repository) = project.repository_binding {
                let _ = self.publish_source(repository.identity());
            }
            for repository in project.related_repositories {
                let _ = self.publish_source(repository.identity());
            }
        }
        Ok(())
    }
    /// Synchronous begin for callers that cannot defer the source observation.
    pub fn begin(
        &mut self,
        plan: TaskLaunchPlan,
        operation_id: &str,
        parallel: bool,
    ) -> Result<RunStartReply> {
        match self.begin_reserve(plan, operation_id, parallel)? {
            BeginReservation::Existing(reply) => Ok(*reply),
            BeginReservation::Fresh(ticket) => {
                let source_start = observe_source(ticket.plan.repository.as_ref(), &self.gate);
                self.begin_commit(ticket, source_start)
            }
        }
    }
    /// Validates the intent, deduplicates against live and in-flight starts
    /// and reserves the source lease. The observation may then run off this
    /// thread while the lease stays held; finish with `begin_commit` or
    /// `begin_abandon`.
    pub fn begin_reserve(
        &mut self,
        plan: TaskLaunchPlan,
        operation_id: &str,
        parallel: bool,
    ) -> Result<BeginReservation> {
        valid_id(operation_id)?;
        if let Some(run) = self
            .runs
            .iter()
            .find(|run| run.operation_id == operation_id)
        {
            ensure!(
                run.project_id == plan.project_id
                    && run.task_id == plan.task.id
                    && run.launch_digest == plan.launch_digest,
                "operation ID already refers to a different execution intent"
            );
            return Ok(BeginReservation::Existing(Box::new(RunStartReply {
                run: self.info(&run.run_id)?,
                existing: true,
            })));
        }
        // Root reconnection takes this same workspace-wide lock exclusively.
        // Acquire it before trusting a captured plan or recording a pending
        // reservation: a plan can have been read just before a rebind committed.
        let run_use = FileLock::acquire_shared(&self.store.runtime_dir.join("run-use.lock"), true)
            .context("root reconnection is in progress; task was not reserved")?;
        let fresh = (crate::task::TaskService { store: &self.store }).launch_plan(
            &plan.project_id,
            &plan.task.id,
            crate::project::LaunchEnvironment::from_variables(plan.shell.env.clone())?,
        )?;
        ensure!(
            fresh.launch_digest == plan.launch_digest,
            "task definition changed before reservation; review the current task"
        );
        // Keep the revision and all derived paths from the guarded observation.
        let plan = fresh;
        ensure!(
            !self
                .pending_starts
                .iter()
                .any(|pending| pending.operation_id == operation_id),
            "this execution is already being prepared; await its result"
        );
        if let Some(run) = self.runs.iter().rev().find(|run| {
            run.project_id == plan.project_id && run.task_id == plan.task.id && run.state.is_live()
        }) {
            if !parallel {
                return Ok(BeginReservation::Existing(Box::new(RunStartReply {
                    run: self.info(&run.run_id)?,
                    existing: true,
                })));
            }
        }
        ensure!(
            !self
                .pending_starts
                .iter()
                .any(|pending| pending.project_id == plan.project_id
                    && pending.task_id == plan.task.id
                    && !parallel),
            "a start of this task is already being prepared; await its result"
        );
        ensure!(
            !self.runs.iter().any(|run| run.project_id == plan.project_id
                && run.task_id == plan.task.id
                && run.state == RunState::Unknown
                && !run.cleanup_confirmed),
            "previous task cleanup is unknown; inspect old processes before another start"
        );
        let outputs = plan
            .task
            .build_outputs
            .iter()
            .map(|p| output_identity(p))
            .collect::<Result<Vec<_>>>()?;
        for active in self.runs.iter().filter(|run| {
            run.state.is_live() || (run.state == RunState::Unknown && !run.cleanup_confirmed)
        }) {
            ensure!(!outputs.iter().any(|path| active.build_outputs.iter().any(|other| path.starts_with(other) || other.starts_with(path))), "build output conflicts with active or unknown run; wait or register distinct output locations");
        }
        for pending in &self.pending_starts {
            ensure!(!outputs.iter().any(|path| pending.build_outputs.iter().any(|other| path.starts_with(other) || other.starts_with(path))), "build output conflicts with a start still being prepared; wait or register distinct output locations");
        }
        // Each deferred ticket will add one record when its observation
        // completes. Reserve that capacity now, alongside committed history.
        self.cleanup_retired()?;
        self.expire_history()?;
        while self.runs.iter().filter(|run| !run.pinned).count() + self.pending_starts.len()
            >= HISTORY_LIMIT
        {
            let index = self
                .runs
                .iter()
                .position(|run| self.can_rotate(run))
                .context("run history full of active, unknown or unfinalized executions")?;
            self.remove_history(index)?;
        }
        let run_id = new_id();
        let lease = if let Some(repository) = &plan.repository {
            repository.verify()?;
            self.publish_source(repository.identity())?;
            Some(
                self.gate
                    .reserve_run(repository.identity(), operation_id, &plan.task.name)?,
            )
        } else {
            None
        };
        self.pending_starts.push(PendingStart {
            run_id: run_id.clone(),
            project_id: plan.project_id.clone(),
            task_id: plan.task.id.clone(),
            operation_id: operation_id.into(),
            build_outputs: outputs.clone(),
        });
        Ok(BeginReservation::Fresh(Box::new(BeginTicket {
            run_id,
            plan,
            operation_id: operation_id.into(),
            outputs,
            lease,
            run_use,
        })))
    }
    /// Releases a reserved start that will never commit (cancelled or failed
    /// before observation completed). The lease is dropped with the ticket.
    pub fn begin_abandon(&mut self, ticket: impl Into<Box<BeginTicket>>) {
        let ticket = ticket.into();
        self.pending_starts
            .retain(|pending| pending.run_id != ticket.run_id);
    }
    /// Persists a reserved start after its source observation arrived.
    pub fn begin_commit(
        &mut self,
        ticket: impl Into<Box<BeginTicket>>,
        source_start: SourceObservation,
    ) -> Result<RunStartReply> {
        let ticket = ticket.into();
        self.pending_starts
            .retain(|pending| pending.run_id != ticket.run_id);
        let BeginTicket {
            run_id,
            plan,
            operation_id,
            outputs,
            lease,
            run_use,
        } = *ticket;
        let directory = self.store.runtime_dir.join(format!("run-{run_id}"));
        ensure_private_dir(&directory)?;
        let mut pending = PendingResources {
            directory: directory.clone(),
            log: self.logs.join(format!("{run_id}.log")),
            retained: false,
        };
        let mut shell = create_steps(&plan, &directory)?;
        run_environment(&mut shell.env, &run_id, &directory);
        let log = LogDescriptor {
            run_id: run_id.clone(),
            generation: 1,
            state: if plan.task.logging == TaskLogging::Disabled {
                LogState::Disabled
            } else {
                LogState::Recording
            },
            bytes: 0,
            observed_bytes: 0,
            limit_bytes: LOG_LIMIT,
            merged_pty: plan.task.execution.is_none() || plan.task.interactive,
            file_identity: None,
        };
        let sink = RunOutputSink::new(&self.logs.join(format!("{run_id}.log")), log)?;
        let log = sink.descriptor();
        let from_run = plan.task.artifact_from_task.as_ref().and_then(|task| {
            self.runs
                .iter()
                .rev()
                .find(|run| {
                    run.project_id == plan.project_id
                        && &run.task_id == task
                        && run.state == RunState::Succeeded
                        && run.artifact.path == plan.task.artifact
                })
                .map(|run| run.run_id.clone())
        });
        let run = RunInfo {
            structured_result: None,
            pinned: false,
            events: None,
            run_id: run_id.clone(),
            operation_id,
            project_id: plan.project_id.clone(),
            task_id: plan.task.id.clone(),
            name: plan.task.name.clone(),
            session_id: None,
            state: RunState::Preparing,
            definition_revision: plan.definition_revision,
            launch_digest: plan.launch_digest.clone(),
            initialization_digest: plan.initialization_digest.clone(),
            cwd: plan.task.cwd.clone(),
            source_roots: plan.source_roots.clone(),
            build_outputs: outputs,
            started_at_ms: now_ms(),
            finished_at_ms: None,
            timeout_seconds: plan.task.timeout_seconds,
            timeout_requested: false,
            cancel_requested: false,
            cleanup_confirmed: false,
            exit_code: None,
            signal: None,
            steps: steps(&plan.task)
                .into_iter()
                .enumerate()
                .map(|(index, step)| StepResult {
                    index,
                    name: step.name,
                    exit_code: None,
                })
                .collect(),
            source_start,
            source_end: None,
            source_changed: None,
            artifact: ArtifactRelation {
                path: plan.task.artifact.clone(),
                from_task: plan.task.artifact_from_task.clone(),
                from_run,
                verified_bytes: false,
                snapshot: None,
                snapshot_error: None,
                observed_at_ms: None,
            },
            log,
            error: None,
        };
        self.runs.push(run.clone());
        if let Err(error) = self.persist() {
            self.runs.pop();
            sink.finish();
            return Err(error);
        }
        self.active.insert(
            run_id,
            Active {
                plan,
                shell,
                sink,
                _lease: lease,
                _run_use: Some(run_use),
                directory,
                started: Instant::now(),
                finishing: false,
                result_snapshot: None,
                events_snapshot: None,
                artifact_measurement: Arc::new(Mutex::new(None)),
            },
        );
        pending.retained = true;
        Ok(RunStartReply {
            run,
            existing: false,
        })
    }
    pub fn shell_plan(&self, run_id: &str) -> Result<ShellPlan> {
        let active = self
            .active
            .get(run_id)
            .context("run is not owned by this host")?;
        let fresh = (crate::task::TaskService { store: &self.store }).launch_plan(
            &active.plan.project_id,
            &active.plan.task.id,
            crate::project::LaunchEnvironment::from_variables(active.plan.shell.env.clone())?,
        )?;
        ensure!(
            fresh.launch_digest == active.plan.launch_digest,
            "task definition changed before spawn"
        );
        ensure!(
            self.info(run_id)?.state == RunState::Preparing,
            "run cancelled or already started"
        );
        Ok(active.shell.clone())
    }
    /// Validated native launch details; None preserves legacy same-shell steps.
    pub fn process_plan(&self, run_id: &str) -> Result<Option<ProcessLaunch>> {
        self.shell_plan(run_id)?; // Recheck definition and durable Preparing state.
        let active = self
            .active
            .get(run_id)
            .context("run is not owned by this host")?;
        let Some(execution) = &active.plan.task.execution else {
            return Ok(None);
        };
        let (program, mut args, overrides) = match execution {
            TaskExecution::Process { program, args, env } => (program.clone(), args.clone(), env),
            TaskExecution::Script {
                interpreter,
                args,
                env,
                ..
            } => {
                let mut args = args.clone();
                args.push(
                    active
                        .directory
                        .join("task.script")
                        .to_string_lossy()
                        .into_owned(),
                );
                (interpreter.clone(), args, env)
            }
        };
        let mut env = active.plan.shell.env.clone();
        env.extend(overrides.clone());
        run_environment(&mut env, run_id, &active.directory);
        Ok(Some(ProcessLaunch {
            program,
            args: std::mem::take(&mut args),
            env,
            cwd: active.plan.task.cwd.clone(),
            interactive: active.plan.task.interactive,
        }))
    }

    pub fn output_sink(&self, run_id: &str) -> Result<RunOutputSink> {
        Ok(self
            .active
            .get(run_id)
            .context("run is not owned by this host")?
            .sink
            .clone())
    }
    pub fn mark_running(&mut self, run_id: &str, session_id: &str) -> Result<()> {
        valid_id(session_id)?;
        let run = self.run_mut(run_id)?;
        ensure!(run.state.is_live(), "run is no longer live");
        run.session_id = Some(session_id.into());
        if run.state == RunState::Preparing {
            run.state = RunState::Running;
        }
        self.persist()
    }
    /// Cancellation request is durable before the caller attempts owned process cleanup.
    pub fn cancel(&mut self, run_id: &str, timeout: bool) -> Result<RunInfo> {
        ensure!(
            !self
                .active
                .get(run_id)
                .is_some_and(|active| active.finishing),
            "run execution already ended; source observation or durable finalization is pending"
        );
        let run = self.run_mut(run_id)?;
        ensure!(
            run.state.is_live(),
            "run is not live; unknown outcomes require inspection"
        );
        let previous = run.clone();
        run.state = RunState::Cancelling;
        run.cancel_requested = true;
        run.timeout_requested |= timeout;
        if let Err(error) = self.persist() {
            // Keep failed automatic timeouts eligible for the next sweep. The
            // caller must not clean up processes until cancellation is durable.
            *self.run_mut(run_id)? = previous;
            return Err(error);
        }
        self.info(run_id)
    }
    /// Explicit user reconciliation after inspecting old processes. Never infers an
    /// exit result and never adopts/signals a PID. Live host-owned runs cannot use this.
    pub fn acknowledge_unknown_cleanup(&mut self, run_id: &str) -> Result<RunInfo> {
        ensure!(
            !self.active.contains_key(run_id),
            "use owned cleanup for runs still tracked by this host"
        );
        let run = self.run_mut(run_id)?;
        ensure!(
            run.state == RunState::Unknown,
            "only a recovered unknown run needs reconciliation"
        );
        let previous = run.clone();
        run.cleanup_confirmed = true;
        run.error =
            Some("user confirmed old process cleanup; execution outcome remains unknown".into());
        if let Err(error) = self.persist() {
            *self.run_mut(run_id)? = previous;
            return Err(error);
        }
        self.info(run_id)
    }
    pub fn timed_out(&self) -> Vec<String> {
        self.timed_out_at(Instant::now())
    }
    /// `timed_out` against an explicit clock so the boundary is testable
    /// without sleeping; a `started` in the future never counts as expired.
    pub fn timed_out_at(&self, now: Instant) -> Vec<String> {
        self.runs
            .iter()
            .filter(|run| {
                matches!(run.state, RunState::Preparing | RunState::Running)
                    && run.timeout_seconds.is_some_and(|seconds| {
                        self.active.get(&run.run_id).is_some_and(|active| {
                            !active.finishing
                                && now.saturating_duration_since(active.started)
                                    >= Duration::from_secs(seconds)
                        })
                    })
            })
            .map(|run| run.run_id.clone())
            .collect()
    }
    /// Begins finalization after the caller observes execution end. The
    /// execution timeout stops here; the source lease stays held while this
    /// observation and the eventual durable `finish_observed` are pending.
    pub fn finish_probe(&mut self, run_id: &str) -> Result<SourceProbe> {
        let active = self
            .active
            .get_mut(run_id)
            .context("run not owned by this host")?;
        active.finishing = true;
        Ok(SourceProbe {
            repository: active.plan.repository.clone(),
            gate: self.gate.clone(),
            artifact: active
                .plan
                .task
                .artifact
                .as_ref()
                .map(|path| (path.clone(), active.artifact_measurement.clone())),
        })
    }
    pub fn finish(
        &mut self,
        run_id: &str,
        exit: Option<TerminalExit>,
        cleanup_confirmed: bool,
        error: Option<String>,
    ) -> Result<RunInfo> {
        let probe = self.finish_probe(run_id)?;
        let source_end = probe.observe_now();
        self.finish_observed(run_id, exit, cleanup_confirmed, error, source_end)
    }
    /// Completes a run once its end observation arrived. The lease is still
    /// held at probe time, so the observation cannot race a source mutation.
    pub fn finish_observed(
        &mut self,
        run_id: &str,
        exit: Option<TerminalExit>,
        cleanup_confirmed: bool,
        error: Option<String>,
        source_end: SourceObservation,
    ) -> Result<RunInfo> {
        let active = self
            .active
            .get_mut(run_id)
            .context("run not owned by this host")?;
        // Direct/synchronous finalization has the same retention boundary as
        // a deferred probe. A failed save must leave its retry state and lease
        // intact even though the in-memory outcome below is already final.
        active.finishing = true;
        let results = if active.plan.task.execution.is_some() {
            vec![StepResult {
                index: 0,
                name: active.plan.task.name.clone(),
                exit_code: exit
                    .as_ref()
                    .filter(|value| value.signal.is_none())
                    .and_then(|value| i32::try_from(value.code).ok()),
            }]
        } else {
            read_steps(&active.directory, &steps(&active.plan.task))
        };
        active.sink.finish();
        let log = active.sink.descriptor();
        if cleanup_confirmed && active.result_snapshot.is_none() {
            let result = self
                .results
                .capture(run_id, &active.directory.join("result.json"));
            active.events_snapshot = Some(
                self.results.events(
                    run_id,
                    &active.directory.join("events.jsonl"),
                    result
                        .summary()
                        .and_then(|summary| summary.producer_run_id.as_deref()),
                    true,
                ),
            );
            active.result_snapshot = Some(result);
        }
        let structured_result = active.result_snapshot.clone();
        let events = active.events_snapshot.clone();
        let measurement = active
            .artifact_measurement
            .lock()
            .ok()
            .and_then(|value| value.clone());
        let existing = self.info(run_id)?;
        let predecessor = existing.artifact.from_run.as_ref().and_then(|id| {
            self.runs.iter().find(|run| {
                &run.run_id == id
                    && run.project_id == existing.project_id
                    && Some(&run.task_id) == existing.artifact.from_task.as_ref()
                    && run.state == RunState::Succeeded
                    && run.cleanup_confirmed
                    && run.artifact.path == existing.artifact.path
            })
        });
        let verified_bytes = cleanup_confirmed
            && measurement
                .as_ref()
                .and_then(|value| value.snapshot.as_ref())
                .zip(predecessor.and_then(|run| run.artifact.snapshot.as_ref()))
                .is_some_and(|(current, previous)| {
                    current.sha256 == previous.sha256 && current.size == previous.size
                });
        let run = self.run_mut(run_id)?;
        if run.artifact.path.is_some() && cleanup_confirmed {
            if let Some(measurement) = measurement {
                run.artifact.snapshot = measurement.snapshot;
                run.artifact.snapshot_error = measurement.error;
                run.artifact.observed_at_ms = Some(measurement.observed_at_ms);
            } else {
                run.artifact.snapshot_error = Some(
                    "artifact measurement did not complete within the observation budget".into(),
                );
            }
            run.artifact.verified_bytes = verified_bytes;
        }
        run.structured_result = structured_result;
        run.events = events;
        run.source_changed = if run.source_start.error.is_none()
            && source_end.error.is_none()
            && run.source_start.identity.is_some()
        {
            Some(
                run.source_start.git_head != source_end.git_head
                    || run.source_start.status_digest != source_end.status_digest
                    || run.source_start.generation != source_end.generation,
            )
        } else {
            None
        };
        run.source_end = Some(source_end);
        run.steps = results;
        run.log = log;
        run.error = error;
        run.exit_code = exit.as_ref().map(|exit| exit.code);
        run.signal = exit.as_ref().and_then(|exit| exit.signal.clone());
        run.state = if !cleanup_confirmed {
            RunState::Unknown
        } else if run.cancel_requested {
            RunState::Cancelled
        } else if exit
            .as_ref()
            .is_some_and(|exit| exit.code == 0 && exit.signal.is_none())
        {
            RunState::Succeeded
        } else if exit.is_some() || run.error.is_some() {
            RunState::Failed
        } else {
            RunState::Unknown
        };
        run.finished_at_ms = Some(now_ms());
        run.cleanup_confirmed = cleanup_confirmed;
        // Persist actual outcome BEFORE allowing source mutation again.
        self.persist()?;
        if let Some(active) = self.active.get_mut(run_id) {
            active.finishing = false;
            if cleanup_confirmed {
                active._lease.take();
                active._run_use.take();
            }
        }
        self.info(run_id)
    }
    pub fn info(&self, run_id: &str) -> Result<RunInfo> {
        let mut run = self
            .runs
            .iter()
            .find(|run| run.run_id == run_id)
            .context("run not found or retention expired")?
            .clone();
        if let Some(active) = self.active.get(run_id) {
            run.log = active.sink.descriptor();
            if active.plan.task.execution.is_none() {
                run.steps = read_steps(&active.directory, &steps(&active.plan.task));
            }
        }
        if run.log.state != LogState::Disabled {
            match log_file(&self.logs.join(format!("{run_id}.log"))) {
                Ok(file) => {
                    let meta = file.metadata()?;
                    if run.log.file_identity != Some((meta.dev(), meta.ino())) {
                        run.log.state = LogState::Expired;
                        run.log.generation = run.log.generation.saturating_add(1);
                    } else if meta.len() < run.log.bytes {
                        run.log.state = LogState::Partial;
                        run.log.bytes = meta.len();
                        run.log.generation = run.log.generation.saturating_add(1);
                    } else if !self.active.contains_key(run_id) {
                        if meta.len() != run.log.bytes {
                            run.log.state = LogState::Partial;
                        }
                        run.log.bytes = meta.len().min(LOG_LIMIT);
                    }
                }
                Err(_) => {
                    run.log.state = LogState::Expired;
                }
            }
        }
        Ok(run)
    }
    pub fn list(&self, project_id: Option<&str>) -> Vec<RunInfo> {
        self.runs
            .iter()
            .filter(|run| project_id.is_none_or(|id| run.project_id == id))
            .map(|run| {
                self.info(&run.run_id).unwrap_or_else(|error| {
                    let mut run = run.clone();
                    run.log.state = LogState::Partial;
                    run.error = Some(format!("run details unavailable: {error}"));
                    run
                })
            })
            .collect()
    }
    pub fn read_log(
        &self,
        run_id: &str,
        generation: u64,
        offset: u64,
        limit: usize,
    ) -> Result<LogChunk> {
        let mut descriptor = self.info(run_id)?.log;
        ensure!(
            generation == descriptor.generation,
            "log generation changed; reload descriptor"
        );
        ensure!(
            limit > 0 && limit <= CHUNK_LIMIT,
            "log read limit must be 1–65536"
        );
        let path = self.logs.join(format!("{run_id}.log"));
        let mut bytes = Vec::new();
        if !matches!(descriptor.state, LogState::Disabled | LogState::Expired) {
            match log_file(&path) {
                Ok(mut file) => {
                    let meta = file.metadata()?;
                    ensure!(
                        descriptor.file_identity == Some((meta.dev(), meta.ino())),
                        "log file replaced; reload descriptor"
                    );
                    let size = meta.len();
                    if size < descriptor.bytes {
                        descriptor.state = LogState::Partial;
                    }
                    descriptor.bytes = size.min(LOG_LIMIT);
                    ensure!(
                        offset <= descriptor.bytes,
                        "log position no longer available"
                    );
                    file.seek(SeekFrom::Start(offset))?;
                    file.take(limit as u64).read_to_end(&mut bytes)?;
                }
                Err(error) if !path.exists() => {
                    descriptor.state = LogState::Expired;
                    let _ = error;
                }
                Err(error) => return Err(error),
            }
        }
        let next_offset = offset + bytes.len() as u64;
        Ok(LogChunk {
            eof: next_offset >= descriptor.bytes,
            descriptor,
            offset,
            next_offset,
            data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
        })
    }
    pub fn search(
        &self,
        run_id: &str,
        generation: u64,
        query: &str,
        cancel: &AtomicBool,
    ) -> Result<LogSearch> {
        ensure!(
            !query.is_empty() && query.len() <= 1024 && !query.chars().any(char::is_control),
            "search needs 1–1024 bytes without controls"
        );
        let descriptor = self.info(run_id)?.log;
        ensure!(
            descriptor.generation == generation,
            "log generation changed"
        );
        let mut reply = LogSearch {
            descriptor,
            matches: Vec::new(),
            truncated: false,
            cancelled: false,
        };
        if matches!(
            reply.descriptor.state,
            LogState::Disabled | LogState::Expired
        ) {
            return Ok(reply);
        }
        let bytes = match read_log_bytes(
            &self.logs.join(format!("{run_id}.log")),
            reply.descriptor.file_identity,
        ) {
            Ok(bytes) => bytes,
            Err(_) => {
                reply.descriptor.state = LogState::Expired;
                return Ok(reply);
            }
        };
        let mut offset = 0;
        for (line, raw) in bytes.split_inclusive(|byte| *byte == b'\n').enumerate() {
            if cancel.load(Ordering::Acquire) {
                reply.cancelled = true;
                break;
            }
            let text = safe_text(raw);
            if text.contains(query) {
                reply.matches.push(LogMatch {
                    offset: offset as u64,
                    line: line as u64 + 1,
                    text: text.chars().take(2048).collect(),
                });
                if reply.matches.len() == 200 {
                    reply.truncated = true;
                    break;
                }
            }
            offset += raw.len();
        }
        Ok(reply)
    }
    fn can_rotate(&self, run: &RunInfo) -> bool {
        !run.pinned
            && !run.state.is_live()
            && run.cleanup_confirmed
            && !self
                .active
                .get(&run.run_id)
                .is_some_and(|active| active.finishing)
    }
    fn cleanup_retired(&mut self) -> Result<()> {
        while let Some(retired) = self.retired.first().cloned() {
            if let Some(body) = &retired.result {
                self.results.remove(body, false)?;
            }
            if let Some(body) = &retired.events {
                self.results.remove(body, true)?;
            }
            let log = self.logs.join(format!("{}.log", retired.run_id));
            match log.symlink_metadata() {
                Ok(meta) => {
                    ensure!(
                        meta.is_file()
                            && Some((meta.dev(), meta.ino())) == retired.log_identity
                            && meta.uid() == unsafe { libc::geteuid() },
                        "retired log was replaced or has unknown identity; preserved"
                    );
                    std::fs::remove_file(&log)?;
                    File::open(&self.logs)?.sync_all()?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            let directory = self
                .store
                .runtime_dir
                .join(format!("run-{}", retired.run_id));
            match directory.symlink_metadata() {
                Ok(meta) => {
                    ensure!(
                        meta.is_dir()
                            && Some((meta.dev(), meta.ino())) == retired.runtime_identity
                            && meta.uid() == unsafe { libc::geteuid() },
                        "retired runtime directory was replaced or has unknown identity; preserved"
                    );
                    std::fs::remove_dir_all(&directory)?;
                    File::open(&self.store.runtime_dir)?.sync_all()?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            self.retired.remove(0);
            if let Err(error) = self.persist() {
                self.retired.insert(0, retired);
                return Err(error);
            }
        }
        Ok(())
    }
    fn remove_history(&mut self, index: usize) -> Result<()> {
        ensure!(
            self.can_rotate(&self.runs[index]),
            "run retention cannot remove live, pinned or unconfirmed work"
        );
        ensure!(
            self.retired.len() < HISTORY_LIMIT,
            "retention cleanup journal full; inspect pending cleanup before continuing"
        );
        let old = self.runs[index].clone();
        let directory = self.store.runtime_dir.join(format!("run-{}", old.run_id));
        let runtime_identity = match directory.symlink_metadata() {
            Ok(meta) => {
                ensure!(
                    meta.is_dir() && meta.uid() == unsafe { libc::geteuid() },
                    "unexpected Run runtime directory; preserved"
                );
                Some((meta.dev(), meta.ino()))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        self.retired.push(crate::saved_state::RetiredRun {
            run_id: old.run_id.clone(),
            result: old
                .structured_result
                .as_ref()
                .and_then(|value| value.body())
                .cloned(),
            events: old.events.as_ref().and_then(|events| events.body.clone()),
            log_identity: old.log.file_identity,
            runtime_identity,
        });
        self.runs.remove(index);
        if let Err(error) = self.persist() {
            self.runs.insert(index, old);
            self.retired.pop();
            return Err(error);
        }
        self.active.remove(&old.run_id);
        self.cleanup_retired()
    }
    fn expire_history(&mut self) -> Result<()> {
        loop {
            let now = now_ms();
            let oldest = self.runs.iter().position(|run| {
                self.can_rotate(run)
                    && run.finished_at_ms.is_some_and(|finished| {
                        now.saturating_sub(finished) > crate::result_store::RETENTION_MS
                    })
            });
            let Some(index) = oldest else { break };
            self.remove_history(index)?;
        }
        Ok(())
    }
    fn result(&self, run_id: &str) -> Result<crate::structured_result::StructuredResult> {
        Ok(self
            .info(run_id)?
            .structured_result
            .unwrap_or(crate::structured_result::StructuredResult::Missing))
    }
    pub fn result_raw(
        &self,
        run_id: &str,
        offset: u64,
        limit: usize,
    ) -> Result<crate::result_store::RawResultPage> {
        self.results.raw(&self.result(run_id)?, offset, limit)
    }
    pub fn findings(
        &self,
        run_id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<crate::result_store::FindingPage> {
        self.results.findings(&self.result(run_id)?, offset, limit)
    }
    /// Editor callers bind their review to the immutable body digest and exact
    /// producer index. Resolving the path is a separate project-root review.
    pub fn structured_finding(
        &self,
        run_id: &str,
        sha256: &str,
        index: usize,
    ) -> Result<crate::result_store::StructuredFinding> {
        let page = self.findings(run_id, index, 1)?;
        ensure!(
            page.result.body().is_some_and(|body| body.sha256 == sha256),
            "result body changed since finding review"
        );
        page.findings
            .into_iter()
            .next()
            .context("finding is missing, expired or unavailable")
    }
    pub fn pin_result(&mut self, run_id: &str, pinned: bool) -> Result<RunInfo> {
        let current = self.info(run_id)?;
        ensure!(
            !current.state.is_live() && current.cleanup_confirmed,
            "pin requires confirmed process cleanup"
        );
        if current.pinned == pinned {
            return Ok(current);
        }
        if pinned {
            if let Some(result) = &current.structured_result {
                ensure!(
                    !matches!(
                        self.results.verify(result),
                        crate::structured_result::StructuredResult::Expired { .. }
                            | crate::structured_result::StructuredResult::Tampered { .. }
                    ),
                    "cannot pin expired or tampered result"
                );
            }
            if let Some(events) = &current.events {
                ensure!(
                    !matches!(
                        self.results.retained_events(events).state,
                        crate::result_store::EventState::Expired
                            | crate::result_store::EventState::Tampered
                    ),
                    "cannot pin expired or tampered events"
                );
            }
            let pinned: Vec<_> = self.runs.iter().filter(|run| run.pinned).collect();
            ensure!(
                pinned.len() < crate::result_store::PINNED_RUN_LIMIT,
                "at most 16 pinned runs; unpin or export an existing baseline"
            );
            let bytes = pinned
                .iter()
                .map(|run| retained_bytes(run))
                .fold(0u64, u64::saturating_add)
                .saturating_add(retained_bytes(&current));
            ensure!(
                bytes <= crate::result_store::PINNED_RESULT_BYTES,
                "pinned data exceeds 128 MiB; unpin or export an existing baseline"
            );
        } else {
            while self.runs.iter().filter(|run| !run.pinned).count() >= HISTORY_LIMIT {
                let index = self
                    .runs
                    .iter()
                    .position(|run| self.can_rotate(run))
                    .context(
                        "ordinary history is full of active or unconfirmed work; cannot unpin yet",
                    )?;
                self.remove_history(index)?;
            }
        }
        self.run_mut(run_id)?.pinned = pinned;
        if let Err(error) = self.persist() {
            self.run_mut(run_id)?.pinned = current.pinned;
            return Err(error);
        }
        self.info(run_id)
    }
    pub fn export_result(
        &self,
        run_id: &str,
        path: &Path,
    ) -> Result<crate::result_store::ExportedResult> {
        self.results.export(&self.result(run_id)?, path)
    }
    pub fn compare_results(
        &self,
        baseline_id: &str,
        run_id: &str,
    ) -> Result<crate::result_store::ResultComparison> {
        let baseline = self.info(baseline_id)?;
        let current = self.info(run_id)?;
        let mut compared = self.results.compare(
            baseline_id,
            &self.result(baseline_id)?,
            run_id,
            &self.result(run_id)?,
        );
        if baseline.project_id != current.project_id || baseline.task_id != current.task_id {
            compared.state = crate::result_store::ComparisonState::Incompatible;
            compared.reason = "baseline belongs to a different project or task".into();
            compared.new_count = 0;
            compared.unchanged_count = 0;
            compared.resolved_count = 0;
            compared.carried_count = 0;
            compared.new_sample.clear();
            compared.resolved_sample.clear();
        }
        Ok(compared)
    }
    pub fn events(&self, run_id: &str) -> Result<crate::result_store::EventProgress> {
        let run = self.info(run_id)?;
        if let Some(events) = &run.events {
            return Ok(self.results.retained_events(events));
        }
        if let Some(active) = self.active.get(run_id) {
            return Ok(self.results.events(
                run_id,
                &active.directory.join("events.jsonl"),
                run.structured_result
                    .as_ref()
                    .and_then(|result| result.summary())
                    .and_then(|summary| summary.producer_run_id.as_deref()),
                false,
            ));
        }
        Ok(crate::result_store::EventProgress::default())
    }
    pub fn persist(&self) -> Result<()> {
        let ledger = Ledger {
            schema: 2,
            runs: self.list(None),
            retired: self.retired.clone(),
        };
        ledger.validate()?;
        self.store.write_state(LEDGER, &ledger)
    }
    fn run_mut(&mut self, run_id: &str) -> Result<&mut RunInfo> {
        self.runs
            .iter_mut()
            .find(|run| run.run_id == run_id)
            .context("run not found")
    }
}
impl Drop for RunRegistry {
    fn drop(&mut self) {
        for identity in self.published.borrow().iter() {
            let _ = self.gate.unavailable(identity);
        }
        for active in self.active.values() {
            active.sink.finish();
            if let Some(repository) = &active.plan.repository {
                let _ = self.gate.unavailable(repository.identity());
            }
        }
    }
}
fn retained_bytes(run: &RunInfo) -> u64 {
    run.structured_result
        .as_ref()
        .and_then(|result| result.body())
        .map_or(0, |body| body.bytes)
        .saturating_add(
            run.events
                .as_ref()
                .and_then(|events| events.body.as_ref())
                .map_or(0, |body| body.bytes),
        )
        .saturating_add(run.log.bytes)
}
fn run_environment(env: &mut BTreeMap<String, String>, run_id: &str, directory: &Path) {
    env.insert("IDK_RUN_ID".into(), run_id.into());
    env.insert(
        "IDK_RESULT_PATH".into(),
        directory.join("result.json").to_string_lossy().into_owned(),
    );
    env.insert(
        "IDK_EVENTS_PATH".into(),
        directory
            .join("events.jsonl")
            .to_string_lossy()
            .into_owned(),
    );
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}
fn output_identity(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return Ok(path.canonicalize()?);
    }
    let name = path
        .file_name()
        .context("output path requires a basename")?;
    let parent = path.parent().context("output path has no parent")?;
    Ok(output_identity(parent)?.join(name))
}
fn quote(path: &Path) -> Result<String> {
    let value = path.to_str().context("task path must be UTF-8")?;
    ensure!(
        !value.chars().any(char::is_control),
        "task path contains control characters"
    );
    Ok(format!(
        "'{}'",
        value.replace('!', "\\!").replace('\'', "'\\''")
    ))
}
fn create_steps(plan: &TaskLaunchPlan, directory: &Path) -> Result<ShellPlan> {
    if let Some(execution) = &plan.task.execution {
        if let TaskExecution::Script { script, .. } = execution {
            crate::store::atomic_write(&directory.join("task.script"), script.as_bytes())?;
        }
        return Ok(plan.shell.clone());
    }
    let prefix = format!("__idk_{}", &new_id().replace('-', "")[..12]);
    let builtin = format!("{prefix}b");
    let code = format!("{prefix}c");
    let failure = format!("{prefix}f");
    let mut command =
        format!("if (1) set {builtin} = ( source set echo exit )\n${builtin}[2] {failure} = 0\n");
    for (index, step) in steps(&plan.task).iter().enumerate() {
        let path = directory.join(format!("step-{index}.csh"));
        crate::store::atomic_write(&path, format!("{}\n", step.command).as_bytes())?;
        let status_path = directory.join(format!("step-{index}.status"));
        crate::store::atomic_write(&status_path, b"")?;
        let status = quote(&status_path)?;
        command.push_str(&format!("${builtin}[1] {}\n${builtin}[2] {code} = $status\n${builtin}[3] ${code} >! {status}\nif (${code} != 0) then\nif (${failure} == 0) ${builtin}[2] {failure} = ${code}\n", quote(&path)?));
        if plan.task.failure_policy == FailurePolicy::Stop {
            command.push_str(&format!("${builtin}[4] ${code}\n"));
        }
        command.push_str("endif\n");
    }
    command.push_str(&format!("${builtin}[4] ${failure}\n"));
    let mut shell = plan.shell.clone();
    shell.command = Some(command);
    Ok(shell)
}
fn read_steps(directory: &Path, steps: &[crate::model::TaskStep]) -> Vec<StepResult> {
    steps
        .iter()
        .enumerate()
        .map(|(index, step)| StepResult {
            index,
            name: step.name.clone(),
            exit_code: read_private(&directory.join(format!("step-{index}.status")), 32)
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .and_then(|text| text.trim().parse::<i32>().ok())
                .filter(|code| (0..=255).contains(code)),
        })
        .collect()
}
fn log_file(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_file() && meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o077 == 0,
        "log must be a private owned regular file"
    );
    Ok(file)
}
pub fn safe_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .map(|c| {
            if c.is_control() && c != '\n' && c != '\t' {
                '\u{fffd}'
            } else {
                c
            }
        })
        .collect()
}
pub(crate) fn observe_source(
    repository: Option<&crate::git::Repository>,
    gate: &SourceGate,
) -> SourceObservation {
    let mut result = SourceObservation {
        identity: repository.map(|repo| repo.identity().into()),
        generation: repository
            .and_then(|repo| gate.state(repo.identity()).ok().map(|s| s.generation)),
        git_head: None,
        dirty: None,
        status_digest: None,
        error: None,
    };
    let Some(repo) = repository else {
        result.error = Some("no registered repository; source identity unconfirmed".into());
        return result;
    };
    let outcome = (|| -> Result<()> {
        repo.verify()?;
        let git = crate::git::GitExecutable::discover()?;
        let invoke = |args: &[&str]| -> Result<Vec<u8>> {
            // Bounded subprocess helper is shared with read-only repository discovery.
            crate::git::run_readonly(git.path(), &repo.root, args)
        };
        let head = invoke(&["rev-parse", "--verify", "HEAD"])?;
        result.git_head = Some(String::from_utf8(head)?.trim().into());
        let status = invoke(&["status", "--porcelain=v1", "-z", "--untracked-files=normal"])?;
        result.dirty = Some(!status.is_empty());
        if status
            .split(|byte| *byte == 0)
            .any(|entry| entry.starts_with(b"?? "))
        {
            result.error =
                Some("untracked source contents are not covered by this Git observation".into());
        }
        let diff = invoke(&[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--binary",
            "HEAD",
            "--",
        ])?;
        let mut digest = Sha256::new();
        digest.update(status);
        digest.update(diff);
        result.status_digest = Some(format!("{:x}", digest.finalize()));
        Ok(())
    })();
    if let Err(error) = outcome {
        result.error = Some(error.to_string());
    }
    result
}

fn read_log_bytes(path: &Path, identity: Option<(u64, u64)>) -> Result<Vec<u8>> {
    let file = log_file(path)?;
    let metadata = file.metadata()?;
    ensure!(
        identity == Some((metadata.dev(), metadata.ino())),
        "log file replaced"
    );
    let mut bytes = Vec::new();
    file.take(LOG_LIMIT).read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }
    #[test]
    fn safe_text_never_panics_and_emits_no_forbidden_controls() {
        let mut rng = Rng(0xd1b54a32d192ed03);
        for _ in 0..20_000 {
            let len = (rng.next() % 512) as usize;
            let bytes: Vec<u8> = (0..len).map(|_| (rng.next() & 0xff) as u8).collect();
            let text = safe_text(&bytes);
            assert!(
                text.chars()
                    .all(|c| !c.is_control() || c == '\n' || c == '\t'),
                "safe_text leaked a control character"
            );
        }
    }
    #[test]
    fn actual_enospc_stops_log_writes_without_inventing_a_process_result() {
        let sink = RunOutputSink::with_file(
            Some(OpenOptions::new().write(true).open("/dev/full").unwrap()),
            LogDescriptor {
                run_id: new_id(),
                generation: 1,
                state: LogState::Recording,
                bytes: 0,
                observed_bytes: 0,
                limit_bytes: LOG_LIMIT,
                merged_pty: true,
                file_identity: None,
            },
        )
        .unwrap();
        sink.push(b"real ENOSPC response");
        let deadline = Instant::now() + Duration::from_secs(2);
        while sink.descriptor().state != LogState::WriteFailed {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        sink.finish();
        assert_eq!(sink.descriptor().bytes, 0);
        assert_eq!(sink.descriptor().state, LogState::WriteFailed);
    }
}
