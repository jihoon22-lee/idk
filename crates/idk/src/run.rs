//! Host-owned registered-run intents, source leases and bounded private PTY logs.
//! No process is adopted or signalled here; the host reports confirmed ownership cleanup.
use crate::model::{new_id, valid_id, FailurePolicy, GateLease, SourceGate, TaskLogging};
use crate::run_wire::*;
use crate::saved_state::RunLedger as Ledger;
use crate::shell::ShellPlan;
use crate::store::{ensure_private_dir, read_private, Store};
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
    directory: PathBuf,
    started: Instant,
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
    active: BTreeMap<String, Active>,
    logs: PathBuf,
    published: RefCell<HashSet<PathBuf>>,
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
            schema: 1,
            runs: vec![],
        });
        ledger.validate()?;
        let mut runs = ledger.runs;
        let mut ids = std::collections::HashSet::new();
        for run in &mut runs {
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
        let result = Self {
            store,
            gate,
            runs,
            active: BTreeMap::new(),
            logs,
            published: RefCell::new(HashSet::new()),
        };
        result.persist()?;
        Ok(result)
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
    pub fn begin(
        &mut self,
        plan: TaskLaunchPlan,
        operation_id: &str,
        parallel: bool,
    ) -> Result<RunStartReply> {
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
            return Ok(RunStartReply {
                run: self.info(&run.run_id)?,
                existing: true,
            });
        }
        if let Some(run) = self.runs.iter().rev().find(|run| {
            run.project_id == plan.project_id && run.task_id == plan.task.id && run.state.is_live()
        }) {
            if !parallel {
                return Ok(RunStartReply {
                    run: self.info(&run.run_id)?,
                    existing: true,
                });
            }
        }
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
        while self.runs.len() >= HISTORY_LIMIT {
            let index = self
                .runs
                .iter()
                .position(|run| {
                    !run.state.is_live()
                        && (run.state != RunState::Unknown || run.cleanup_confirmed)
                })
                .context("run history full of active/unknown executions")?;
            let old = self.runs.remove(index);
            self.active.remove(&old.run_id);
            let path = self.logs.join(format!("{}.log", old.run_id));
            if path.exists() {
                std::fs::remove_file(path)?;
            }
            let directory = self.store.runtime_dir.join(format!("run-{}", old.run_id));
            if directory.exists() {
                std::fs::remove_dir_all(directory)?;
            }
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
        let source_start = observe_source(plan.repository.as_ref(), &self.gate);
        let directory = self.store.runtime_dir.join(format!("run-{run_id}"));
        ensure_private_dir(&directory)?;
        let mut pending = PendingResources {
            directory: directory.clone(),
            log: self.logs.join(format!("{run_id}.log")),
            retained: false,
        };
        let shell = create_steps(&plan, &directory)?;
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
            merged_pty: true,
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
            run_id: run_id.clone(),
            operation_id: operation_id.into(),
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
                directory,
                started: Instant::now(),
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
        self.runs
            .iter()
            .filter(|run| {
                matches!(run.state, RunState::Preparing | RunState::Running)
                    && run.timeout_seconds.is_some_and(|seconds| {
                        self.active.get(&run.run_id).is_some_and(|active| {
                            active.started.elapsed() >= Duration::from_secs(seconds)
                        })
                    })
            })
            .map(|run| run.run_id.clone())
            .collect()
    }
    pub fn finish(
        &mut self,
        run_id: &str,
        exit: Option<TerminalExit>,
        cleanup_confirmed: bool,
        error: Option<String>,
    ) -> Result<RunInfo> {
        let active = self
            .active
            .get(run_id)
            .context("run not owned by this host")?;
        let source_end = observe_source(active.plan.repository.as_ref(), &self.gate);
        let results = read_steps(&active.directory, &steps(&active.plan.task));
        active.sink.finish();
        let log = active.sink.descriptor();
        let run = self.run_mut(run_id)?;
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
        if cleanup_confirmed {
            if let Some(active) = self.active.get_mut(run_id) {
                active._lease.take();
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
            run.steps = read_steps(&active.directory, &steps(&active.plan.task));
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
    pub fn persist(&self) -> Result<()> {
        self.store.write_state(
            LEDGER,
            &Ledger {
                schema: 1,
                runs: self.list(None),
            },
        )
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
fn observe_source(
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
