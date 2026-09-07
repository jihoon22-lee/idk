use super::children;
use super::launch::{self, Completed, Job, Runtime};
use crate::model::{new_id, valid_id, TerminalDefinition, MAX_PROJECTS, MAX_TERMINALS};
use crate::protocol::*;
use crate::shell::InitializationState;
use crate::store::{ensure_private_dir, Store};
use crate::terminal::{TerminalExit, TerminalSnapshot};
use anyhow::{ensure, Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

const LEDGER: &str = "host-sessions.json";
const MAX_CACHE_BYTES: usize = 4 * 1024 * 1024;
const MAX_CACHED_REPLY: usize = 64 * 1024;
const MAX_TRACKED: usize = MAX_PROJECTS * MAX_TERMINALS + MAX_TERMINALS;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    schema: u32,
    sessions: Vec<Tombstone>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Tombstone {
    session_id: String,
    project_id: String,
    terminal_id: String,
    name: String,
    persistent: bool,
    state: SessionState,
    definition_revision: u64,
    launch_digest: Option<String>,
    exit: Option<TerminalExit>,
}
impl Tombstone {
    fn from_info(info: &SessionInfo) -> Self {
        Self {
            session_id: info.session_id.clone(),
            project_id: info.project_id.clone(),
            terminal_id: info.terminal_id.clone(),
            name: info.name.clone(),
            persistent: info.persistent,
            state: info.state,
            definition_revision: info.definition_revision,
            launch_digest: info.launch_digest.clone(),
            exit: info.exit.clone(),
        }
    }
    fn restore(self, host: &str) -> Result<SessionInfo> {
        valid_id(&self.session_id)?;
        valid_id(&self.project_id)?;
        valid_id(&self.terminal_id)?;
        crate::model::valid_name(&self.name)?;
        ensure!(
            self.launch_digest.as_ref().is_none_or(
                |digest| digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit())
            ),
            "invalid launch digest in ledger"
        );
        let unknown = self.state.is_live() || self.state == SessionState::Unknown;
        Ok(SessionInfo { session_id: self.session_id, project_id: self.project_id, terminal_id: self.terminal_id, name: self.name,
            host_instance: host.into(), persistent: self.persistent, state: if unknown { SessionState::Unknown } else { self.state },
            initialization: None, input_epoch: 0, owner: None, generation: 0, child_pid: None, cwd: None,
            definition_revision: self.definition_revision, launch_digest: self.launch_digest, exit: self.exit,
            error: unknown.then(|| "previous host ended without a confirmed terminal exit; old processes were not adopted or signalled".into()) })
    }
}

// Typed purpose is intentionally narrow: callers cannot inject arbitrary child
// commands. Operations and runs will add distinct variants and policies.
enum SlotPurpose {
    Terminal(Option<TerminalDefinition>),
}
struct Slot {
    info: SessionInfo,
    purpose: SlotPurpose,
    runtime: Option<Runtime>,
    frozen: Option<TerminalSnapshot>,
    cancel: Arc<AtomicBool>,
    preparing: bool,
    closing_at: Option<Instant>,
    exited_at: Option<Instant>,
    cleanup: Cleanup,
}
#[derive(Default)]
struct Cleanup {
    requested: bool,
    force: bool,
    done: bool,
    signalled: HashMap<libc::pid_t, bool>,
}
impl Slot {
    fn owner(&self, client: &str, epoch: u64) -> Result<()> {
        ensure!(
            self.info
                .owner
                .as_ref()
                .is_some_and(|owner| owner.client_id == client)
                && self.info.input_epoch == epoch,
            "terminal input ownership changed; attach or explicitly take over before retrying"
        );
        Ok(())
    }
    fn terminal(&mut self) -> Result<&mut crate::terminal::TerminalSession> {
        Ok(&mut self
            .runtime
            .as_mut()
            .context("terminal has no live PTY")?
            .terminal)
    }
    fn advance_epoch(&mut self) -> Result<()> {
        self.info.input_epoch = self
            .info
            .input_epoch
            .checked_add(1)
            .context("terminal ownership epoch exhausted")?;
        Ok(())
    }
}
struct CacheEntry {
    fingerprint: [u8; 32],
    deadline: u64,
    response: Response,
    bytes: usize,
}
struct ClientIdentity {
    pid: u32,
    seen: Instant,
}
struct Batch {
    info: BatchInfo,
    env: BTreeMap<String, String>,
    rows: u16,
    cols: u16,
    waiting_since: Option<Instant>,
}
pub(super) struct Actor {
    store: Store,
    info: HostInfo,
    slots: BTreeMap<String, Slot>,
    cache: HashMap<(String, String), CacheEntry>,
    clients: HashMap<String, ClientIdentity>,
    batches: BTreeMap<String, Batch>,
    jobs: mpsc::SyncSender<Job>,
    results: mpsc::Receiver<Completed>,
    quiescing: bool,
    last_batch_tick: Instant,
    child_scans: mpsc::SyncSender<Vec<children::Anchor>>,
    child_reports: mpsc::Receiver<children::Report>,
    scan_inflight: bool,
    last_scan: Instant,
}

impl Actor {
    pub fn new(store: Store, launcher: PathBuf, info: HostInfo) -> Result<Self> {
        let mut slots = BTreeMap::new();
        let mut keys = std::collections::HashSet::new();
        if let Some(ledger) = store.read_state::<Ledger>(LEDGER)? {
            ensure!(
                ledger.schema == 1 && ledger.sessions.len() <= MAX_TRACKED,
                "unsupported or oversized host session ledger; preserved"
            );
            for tombstone in ledger.sessions {
                let session = tombstone.restore(&info.host_instance)?;
                ensure!(
                    keys.insert((session.project_id.clone(), session.terminal_id.clone()))
                        && !slots.contains_key(&session.session_id),
                    "duplicate session identity in host ledger; preserved"
                );
                slots.insert(
                    session.session_id.clone(),
                    Slot {
                        info: session,
                        purpose: SlotPurpose::Terminal(None),
                        runtime: None,
                        frozen: None,
                        cancel: Arc::new(AtomicBool::new(false)),
                        preparing: false,
                        closing_at: None,
                        exited_at: None,
                        cleanup: Cleanup::default(),
                    },
                );
            }
        }
        let resources = store
            .runtime_dir
            .join(format!("host-{}", info.host_instance));
        ensure_private_dir(&resources)?;
        let (jobs, results) = launch::worker(store.clone(), launcher, resources)?;
        let (child_scans, child_reports) = children::worker()?;
        let actor = Self {
            store,
            info,
            slots,
            cache: HashMap::new(),
            clients: HashMap::new(),
            batches: BTreeMap::new(),
            jobs,
            results,
            quiescing: false,
            last_batch_tick: Instant::now(),
            child_scans,
            child_reports,
            scan_inflight: false,
            last_scan: Instant::now(),
        };
        actor.persist()?;
        Ok(actor)
    }
    fn persist(&self) -> Result<()> {
        self.store.write_state(
            LEDGER,
            &Ledger {
                schema: 1,
                sessions: self
                    .slots
                    .values()
                    .map(|slot| Tombstone::from_info(&slot.info))
                    .collect(),
            },
        )
    }
    pub fn finished(&self) -> bool {
        self.quiescing
            && self
                .slots
                .values()
                .all(|slot| !slot.info.state.is_live() && !slot.preparing && slot.runtime.is_none())
    }
    pub fn respond(&mut self, envelope: Envelope, peer: PeerCredentials) -> Response {
        let request_id = envelope.request_id.clone();
        let response = self
            .respond_checked(&envelope, peer)
            .unwrap_or_else(|error| Response::failure(request_id, error));
        response.with_host(&self.info.host_instance)
    }
    fn respond_checked(&mut self, envelope: &Envelope, peer: PeerCredentials) -> Result<Response> {
        envelope.validate()?;
        if !matches!(envelope.request, Request::Hello) {
            ensure!(
                envelope.host_instance.as_deref() == Some(&self.info.host_instance),
                "host instance changed; reconnect explicitly; request was not replayed"
            );
        }
        let now = Instant::now();
        self.clients.retain(|id, client| {
            now.duration_since(client.seen) < Duration::from_secs(30)
                || self.slots.values().any(|slot| {
                    slot.info
                        .owner
                        .as_ref()
                        .is_some_and(|owner| &owner.client_id == id)
                })
        });
        if let Some(client) = self.clients.get_mut(&envelope.client_id) {
            ensure!(
                client.pid == peer.pid,
                "client identity belongs to a different process"
            );
            client.seen = now;
        } else {
            ensure!(
                matches!(envelope.request, Request::Hello),
                "client identity expired; reconnect explicitly"
            );
            ensure!(
                self.clients.len() < 256,
                "host client identity limit reached"
            );
            self.clients.insert(
                envelope.client_id.clone(),
                ClientIdentity {
                    pid: peer.pid,
                    seen: now,
                },
            );
        }
        let key = (envelope.client_id.clone(), envelope.request_id.clone());
        let fingerprint = envelope.fingerprint()?;
        self.cache
            .retain(|_, value| value.deadline > monotonic_ms());
        if let Some(cached) = self.cache.get(&key) {
            ensure!(
                cached.fingerprint == fingerprint,
                "request ID was reused with different content; no mutation accepted"
            );
            return Ok(cached.response.clone());
        }
        let mutating = envelope.request.is_mutating();
        if mutating {
            ensure!(
                self.cache.len() < 1024
                    && self.cache.values().map(|item| item.bytes).sum::<usize>() + MAX_CACHED_REPLY
                        <= MAX_CACHE_BYTES,
                "host retry cache is full; no mutation accepted"
            );
        }
        let mut response = match self.execute(&envelope.client_id, &envelope.request) {
            Ok(value) => Response::success(envelope.request_id.clone(), &value)?,
            Err(error) => Response::failure(envelope.request_id.clone(), error),
        }
        .with_host(&self.info.host_instance);
        if mutating {
            let mut bytes = serde_json::to_vec(&response)?.len();
            if bytes > MAX_CACHED_REPLY {
                response = Response::failure(envelope.request_id.clone(), "mutation accepted but response exceeds retry cache limit; inspect current state").with_host(&self.info.host_instance);
                bytes = serde_json::to_vec(&response)?.len();
            }
            self.cache.insert(
                key,
                CacheEntry {
                    fingerprint,
                    deadline: envelope.deadline_ms,
                    response: response.clone(),
                    bytes,
                },
            );
        }
        Ok(response)
    }
    fn slot(&self, session: &str) -> Result<&Slot> {
        self.slots
            .get(session)
            .context("terminal session is unknown")
    }
    fn slot_mut(&mut self, session: &str) -> Result<&mut Slot> {
        self.slots
            .get_mut(session)
            .context("terminal session is unknown")
    }
    fn execute(&mut self, client: &str, request: &Request) -> Result<serde_json::Value> {
        fn value(data: impl Serialize) -> Result<serde_json::Value> {
            Ok(serde_json::to_value(data)?)
        }
        match request {
            Request::Hello => value(&self.info),
            Request::List { project } => value(
                self.slots
                    .values()
                    .filter(|slot| {
                        project
                            .as_ref()
                            .is_none_or(|id| id == &slot.info.project_id)
                    })
                    .map(|slot| &slot.info)
                    .collect::<Vec<_>>(),
            ),
            Request::Definition { session } => {
                let SlotPurpose::Terminal(definition) = &self.slot(session)?.purpose;
                value(definition.as_ref().context(
                    "original terminal definition is no longer retained for this ended session",
                )?)
            }
            Request::Start {
                project,
                terminal,
                rows,
                cols,
                env,
                reopen,
            } => value(self.start_saved(project, terminal, env.clone(), *rows, *cols, *reopen)?),
            Request::StartTransient {
                project,
                terminal,
                rows,
                cols,
                env,
                reopen,
            } => {
                let workspace = self.store.load()?;
                ensure!(
                    !workspace
                        .projects
                        .iter()
                        .filter(|saved| saved.id == *project)
                        .flat_map(|saved| &saved.terminals)
                        .any(|saved| saved.id == terminal.id),
                    "transient ID is already registered; use the saved terminal start operation"
                );
                value(self.start(
                    project,
                    terminal.clone(),
                    env.clone(),
                    *rows,
                    *cols,
                    *reopen,
                )?)
            }
            Request::StartDefaults {
                project,
                rows,
                cols,
                env,
            } => value(self.start_defaults(project, env.clone(), *rows, *cols)?),
            Request::Batch { batch } => value(
                &self
                    .batches
                    .get(batch)
                    .context("default-open batch is unknown")?
                    .info,
            ),
            Request::ContinueDefaults { batch } => {
                let batch = self
                    .batches
                    .get_mut(batch)
                    .context("default-open batch is unknown")?;
                ensure!(
                    batch.info.state == BatchState::Paused,
                    "default-open batch is not paused"
                );
                batch.info.waiting_session = None;
                batch.waiting_since = None;
                batch.info.state = BatchState::Running;
                value(&batch.info)
            }
            Request::CancelDefaults { batch } => {
                let batch = self
                    .batches
                    .get_mut(batch)
                    .context("default-open batch is unknown")?;
                cancel_batch(batch);
                value(&batch.info)
            }
            Request::Attach { session, takeover } => {
                let slot = self.slot_mut(session)?;
                if slot.info.state.is_live() {
                    if let Some(owner) = &slot.info.owner {
                        ensure!(
                            owner.client_id == client || *takeover,
                            "terminal is owned by another client; explicit takeover required"
                        );
                    }
                    if slot
                        .info
                        .owner
                        .as_ref()
                        .is_none_or(|owner| owner.client_id != client)
                    {
                        slot.advance_epoch()?;
                        slot.info.owner = Some(InputOwner {
                            client_id: client.into(),
                        });
                    }
                }
                value(&slot.info)
            }
            Request::Detach { session, epoch } => {
                let slot = self.slot_mut(session)?;
                if slot.info.state.is_live() {
                    slot.owner(client, *epoch)?;
                    slot.advance_epoch()?;
                    slot.info.owner = None;
                }
                value(&slot.info)
            }
            Request::Snapshot { session, since } => {
                let slot = self.slot_mut(session)?;
                let screen = if let Some(screen) = &slot.frozen {
                    (Some(screen.generation) != *since).then(|| screen.clone())
                } else if let Some(runtime) = &slot.runtime {
                    runtime.terminal.snapshot_since(*since)?
                } else {
                    None
                };
                let mut info = slot.info.clone();
                // The screen and its revision are one observation; a later output
                // cannot advance a client's cached revision without its cells.
                if let Some(screen) = &screen {
                    info.generation = screen.generation;
                } else if let Some(since) = since {
                    info.generation = *since;
                }
                value(SnapshotReply {
                    session: info,
                    screen,
                })
            }
            Request::Input {
                session,
                data,
                epoch,
            } => {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .context("invalid terminal input encoding")?;
                ensure!(
                    bytes.len() <= MAX_INPUT_PACKET,
                    "terminal input exceeds 64 KiB"
                );
                let slot = self.slot_mut(session)?;
                slot.owner(client, *epoch)?;
                ensure!(
                    slot.info.state == SessionState::Running,
                    "terminal is not accepting input"
                );
                slot.terminal()?.input(&bytes)?;
                value(())
            }
            Request::Resize {
                session,
                rows,
                cols,
                epoch,
            } => {
                let slot = self.slot_mut(session)?;
                slot.owner(client, *epoch)?;
                slot.terminal()?.resize(*rows, *cols)?;
                value(())
            }
            Request::Scroll {
                session,
                delta,
                epoch,
            } => {
                let slot = self.slot_mut(session)?;
                slot.owner(client, *epoch)?;
                slot.terminal()?.scroll(*delta)?;
                value(())
            }
            Request::Search {
                session,
                query,
                backwards,
                epoch,
            } => {
                let slot = self.slot_mut(session)?;
                slot.owner(client, *epoch)?;
                let (found, searched_rows) = slot.terminal()?.search(query, *backwards)?;
                value(SearchReply {
                    found,
                    searched_rows,
                })
            }
            Request::Close {
                session,
                epoch,
                force,
            } => {
                if self.slot(session)?.info.state.is_live() {
                    self.slot(session)?.owner(client, *epoch)?;
                    self.close_one(session, *force)?;
                }
                value(&self.slot(session)?.info)
            }
            Request::PreviewClose { project } => value(ClosePreview {
                project: project.clone(),
                targets: self.targets(project.as_deref()),
            }),
            Request::CloseProject {
                project,
                sessions,
                force,
            } => value(self.close_many(Some(project), sessions, *force)?),
            Request::Shutdown { sessions, force } => {
                value(self.close_many(None, sessions, *force)?)
            }
        }
    }

    fn start_saved(
        &mut self,
        project: &str,
        terminal: &str,
        env: BTreeMap<String, String>,
        rows: u16,
        cols: u16,
        reopen: bool,
    ) -> Result<SessionInfo> {
        if let Some(slot) = self
            .slots
            .values()
            .find(|slot| slot.info.project_id == project && slot.info.terminal_id == terminal)
        {
            if slot.info.state.is_live() {
                ensure!(
                    slot.info.state != SessionState::Closing,
                    "terminal is still closing; wait for confirmed exit"
                );
                return Ok(slot.info.clone());
            }
            ensure!(
                reopen,
                "terminal previously ended or has unknown state; explicit reopen required"
            );
        }
        let workspace = self.store.load()?;
        let definition = workspace
            .project(project)?
            .terminals
            .iter()
            .find(|item| item.id == terminal)
            .context("saved terminal definition is unavailable")?
            .clone();
        self.start(project, definition, env, rows, cols, reopen)
    }
    fn start(
        &mut self,
        project: &str,
        terminal: TerminalDefinition,
        env: BTreeMap<String, String>,
        rows: u16,
        cols: u16,
        reopen: bool,
    ) -> Result<SessionInfo> {
        ensure!(
            !self.quiescing,
            "host shutdown is in progress; no new terminals accepted"
        );
        crate::model::valid_name(&terminal.name)?;
        crate::model::absolute_path(&terminal.cwd)?;
        crate::model::validate_sources(&terminal.sources)?;
        let previous_id = self
            .slots
            .values()
            .find(|slot| slot.info.project_id == project && slot.info.terminal_id == terminal.id)
            .map(|slot| slot.info.session_id.clone());
        if let Some(id) = &previous_id {
            let previous = self.slot(id)?;
            if previous.info.state.is_live() {
                ensure!(
                    previous.info.state != SessionState::Closing,
                    "terminal is still closing; wait for confirmed exit"
                );
                return Ok(previous.info.clone());
            }
            ensure!(
                reopen,
                "terminal previously ended or has unknown state; explicit reopen required"
            );
            ensure!(
                previous.runtime.is_none(),
                "terminal output is still draining; retry explicit reopen shortly"
            );
        }
        ensure!(
            self.slots
                .values()
                .filter(|slot| slot.info.state.is_live())
                .count()
                < MAX_TERMINALS,
            "host live terminal limit reached"
        );
        ensure!(
            previous_id.is_some() || self.slots.len() < MAX_TRACKED,
            "host tracked terminal capacity reached; existing tombstones preserved"
        );
        // Full trust/path hashing belongs to the worker. The actor records the
        // definition identity before scheduling any child or source execution.
        let session_id = new_id();
        let cancel = Arc::new(AtomicBool::new(false));
        let info = SessionInfo {
            session_id: session_id.clone(),
            project_id: project.into(),
            terminal_id: terminal.id.clone(),
            name: terminal.name.clone(),
            host_instance: self.info.host_instance.clone(),
            persistent: terminal.persistent,
            state: SessionState::Preparing,
            initialization: Some(InitializationState::Initializing),
            input_epoch: 0,
            owner: None,
            generation: 0,
            child_pid: None,
            cwd: None,
            definition_revision: 0,
            launch_digest: None,
            exit: None,
            error: None,
        };
        let previous = previous_id.as_ref().and_then(|id| self.slots.remove(id));
        self.slots.insert(
            session_id.clone(),
            Slot {
                info: info.clone(),
                purpose: SlotPurpose::Terminal(Some(terminal.clone())),
                runtime: None,
                frozen: None,
                cancel: cancel.clone(),
                preparing: true,
                closing_at: None,
                exited_at: None,
                cleanup: Cleanup::default(),
            },
        );
        let scheduled = self.persist().and_then(|()| {
            self.jobs
                .try_send(Job {
                    session: session_id.clone(),
                    project: project.into(),
                    definition: terminal,
                    environment: env,
                    rows,
                    cols,
                    cancelled: cancel,
                })
                .map_err(|_| anyhow::anyhow!("shell preparation queue is full; no shell created"))
        });
        if let Err(error) = scheduled {
            self.slots.remove(&session_id);
            if let Some(previous) = previous {
                self.slots
                    .insert(previous.info.session_id.clone(), previous);
            }
            // The original Preparing record, if any, is safe on crash: it is
            // restored as Unknown, never used as an automatic retry instruction.
            let _ = self.persist();
            return Err(error);
        }
        Ok(info)
    }
    fn targets(&self, project: Option<&str>) -> Vec<SessionInfo> {
        self.slots
            .values()
            .filter(|slot| {
                slot.info.state.is_live() && project.is_none_or(|id| slot.info.project_id == id)
            })
            .map(|slot| slot.info.clone())
            .collect()
    }
    fn close_one(&mut self, session: &str, force: bool) -> Result<()> {
        let signalled = self.request_close(session, force);
        if let Err(error) = self.persist() {
            self.slot_mut(session)?.info.error =
                Some("terminal close requested but durable state could not be saved".into());
            return Err(error).context("close requested; durable state save failed");
        }
        signalled
    }
    fn request_close(&mut self, session: &str, force: bool) -> Result<()> {
        let slot = self.slot_mut(session)?;
        if !slot.info.state.is_live() {
            return Ok(());
        }
        slot.cancel.store(true, Ordering::Release);
        slot.info.state = SessionState::Closing;
        slot.cleanup.requested = true;
        slot.cleanup.force |= force;
        slot.closing_at.get_or_insert_with(Instant::now);
        if let Some(runtime) = &mut slot.runtime {
            let result = runtime.terminal.request_host_close(slot.cleanup.force);
            if let Err(error) = result {
                slot.info.error = Some(safe_error(&error));
                return Err(error);
            }
        }
        Ok(())
    }
    fn close_many(
        &mut self,
        project: Option<&str>,
        sessions: &[String],
        force: bool,
    ) -> Result<CloseReply> {
        let mut expected: Vec<_> = self
            .targets(project)
            .into_iter()
            .map(|info| info.session_id)
            .collect();
        expected.sort();
        let mut supplied = sessions.to_vec();
        supplied.sort();
        ensure!(
            expected == supplied,
            "close targets changed; review a fresh target preview before closing any terminal"
        );
        for batch in self
            .batches
            .values_mut()
            .filter(|batch| project.is_none_or(|id| batch.info.project_id == id))
        {
            cancel_batch(batch);
        }
        if project.is_none() {
            self.quiescing = true;
        }
        let mut errors = BTreeMap::new();
        for session in &expected {
            if let Err(error) = self.request_close(session, force) {
                errors.insert(session.clone(), safe_error(error));
            }
        }
        // One durable inventory update covers the already confirmed target set;
        // avoid rewriting a full ledger once for each of up to 64 owned shells.
        if self.persist().is_err() {
            for session in &expected {
                errors.entry(session.clone()).or_insert_with(|| {
                    "terminal close requested but durable state could not be saved".into()
                });
                if let Some(slot) = self.slots.get_mut(session) {
                    slot.info.error = Some(
                        "terminal close requested but durable state could not be saved".into(),
                    );
                }
            }
        }
        let pending = expected
            .iter()
            .filter(|id| {
                self.slots
                    .get(*id)
                    .is_some_and(|slot| slot.info.state.is_live())
            })
            .cloned()
            .collect();
        Ok(CloseReply {
            sessions: expected,
            pending,
            errors,
            shutting_down: self.quiescing,
        })
    }

    fn start_defaults(
        &mut self,
        project: &str,
        env: BTreeMap<String, String>,
        rows: u16,
        cols: u16,
    ) -> Result<BatchInfo> {
        ensure!(!self.quiescing, "host shutdown is in progress");
        if let Some(batch) = self.batches.values().find(|batch| {
            batch.info.project_id == project
                && matches!(batch.info.state, BatchState::Running | BatchState::Paused)
        }) {
            return Ok(batch.info.clone());
        }
        if self.batches.len() >= 16 {
            if let Some(id) = self
                .batches
                .iter()
                .find(|(_, batch)| {
                    matches!(
                        batch.info.state,
                        BatchState::Complete | BatchState::Cancelled
                    )
                })
                .map(|(id, _)| id.clone())
            {
                self.batches.remove(&id);
            }
        }
        ensure!(
            self.batches.len() < 16,
            "too many active default-open batches"
        );
        let workspace = self.store.load()?;
        let definition = workspace.project(project)?;
        let mut remaining: Vec<_> = definition
            .terminals
            .iter()
            .filter(|terminal| terminal.persistent)
            .map(|terminal| terminal.id.clone())
            .collect();
        if let Some(default) = &definition.default_terminal {
            if let Some(index) = remaining.iter().position(|id| id == default) {
                let first = remaining.remove(index);
                remaining.insert(0, first);
            }
        }
        let batch_id = new_id();
        let info = BatchInfo {
            batch_id: batch_id.clone(),
            project_id: project.into(),
            state: BatchState::Running,
            sessions: Vec::new(),
            remaining,
            waiting_session: None,
            failures: BTreeMap::new(),
        };
        self.batches.insert(
            batch_id,
            Batch {
                info: info.clone(),
                env,
                rows,
                cols,
                waiting_since: None,
            },
        );
        Ok(info)
    }
    fn tick_batches(&mut self) {
        let ids: Vec<_> = self.batches.keys().cloned().collect();
        for id in ids {
            // Removing the small batch lets start_saved borrow the actor without
            // holding a map borrow. Only the actor accesses either collection.
            let Some(mut batch) = self.batches.remove(&id) else {
                continue;
            };
            if batch.info.state == BatchState::Running {
                let mut waiting = false;
                if let Some(session) = &batch.info.waiting_session {
                    if let Some(slot) = self.slots.get(session) {
                        if !slot.info.state.is_live()
                            || slot.info.initialization == Some(InitializationState::Failed)
                        {
                            batch.info.failures.insert(
                                slot.info.terminal_id.clone(),
                                slot.info.error.clone().unwrap_or_else(|| {
                                    "terminal initialization did not complete successfully".into()
                                }),
                            );
                        } else if slot.info.initialization != Some(InitializationState::Ready) {
                            waiting = true;
                            if batch
                                .waiting_since
                                .is_some_and(|time| time.elapsed() >= Duration::from_secs(5))
                            {
                                batch.info.state = BatchState::Paused;
                            }
                        }
                    } else {
                        batch
                            .info
                            .failures
                            .insert(session.clone(), "waiting terminal is unavailable".into());
                    }
                }
                if !waiting {
                    batch.info.waiting_session = None;
                    batch.waiting_since = None;
                    if batch.info.remaining.is_empty() {
                        batch.info.state = BatchState::Complete;
                        batch.env.clear();
                    } else {
                        let terminal = batch.info.remaining.remove(0);
                        match self.start_saved(
                            &batch.info.project_id,
                            &terminal,
                            batch.env.clone(),
                            batch.rows,
                            batch.cols,
                            false,
                        ) {
                            Ok(session) => {
                                batch.info.sessions.push(session.session_id.clone());
                                batch.info.waiting_session = Some(session.session_id);
                                batch.waiting_since = Some(Instant::now());
                            }
                            Err(error) => {
                                batch.info.failures.insert(terminal, safe_error(error));
                            }
                        }
                    }
                }
            }
            self.batches.insert(id, batch);
        }
    }

    fn poll_child_inventory(&mut self) {
        let Ok(report) = self.child_reports.try_recv() else {
            return;
        };
        self.scan_inflight = false;
        let host_pid = std::process::id() as libc::pid_t;
        for anchor in report.anchors {
            let Some(slot) = self.slots.get_mut(&anchor.session) else {
                continue;
            };
            if slot.info.exit.is_none()
                || slot
                    .runtime
                    .as_ref()
                    .and_then(|runtime| runtime.terminal.child_pid())
                    != Some(anchor.pid as u32)
            {
                continue;
            }
            let mut found = 0usize;
            let mut failure = report.error.clone();
            for process in report
                .processes
                .iter()
                .filter(|process| process.session == anchor.pid)
            {
                found += 1;
                // A nested process is not signalled speculatively. Its direct
                // ancestor remains pending; subreaping exposes it after exit.
                if process.parent != host_pid {
                    continue;
                }
                let result = match children::verify_child(process.pid, anchor.pid) {
                    Ok(children::ChildState::Exited) => {
                        let result = children::reap_child(process.pid);
                        if result.is_ok() {
                            slot.cleanup.signalled.remove(&process.pid);
                        }
                        result
                    }
                    Ok(children::ChildState::Running) => {
                        if slot.cleanup.requested
                            && slot.cleanup.signalled.get(&process.pid).copied()
                                != Some(slot.cleanup.force)
                        {
                            let result = children::signal_child(process.pid, slot.cleanup.force);
                            if result.is_ok() {
                                slot.cleanup
                                    .signalled
                                    .insert(process.pid, slot.cleanup.force);
                            }
                            result
                        } else {
                            Ok(())
                        }
                    }
                    Err(error) => Err(error),
                };
                if let Err(error) = result {
                    failure = Some(safe_error(error));
                }
            }
            // Reaping even the last observed child requires a NEW empty scan:
            // that child's nested descendants may have just been reparented.
            slot.cleanup.done = report.complete && failure.is_none() && found == 0;
            if let Some(error) = failure {
                cleanup_notice(
                    slot,
                    &format!("ownership inventory is incomplete: {error}; exit is not confirmed"),
                );
            } else if found > 0 {
                let action = if slot.cleanup.force {
                    "force requested; waiting for observed exits"
                } else if slot.cleanup.requested {
                    "hangup requested; explicit force close is available"
                } else {
                    "shell exited; explicit close or force close is available"
                };
                cleanup_notice(
                    slot,
                    &format!("{found} same-session processes remain; {action}"),
                );
            }
        }
    }
    fn schedule_child_inventory(&mut self) {
        if self.scan_inflight || self.last_scan.elapsed() < Duration::from_millis(100) {
            return;
        }
        let anchors: Vec<_> = self
            .slots
            .values()
            .filter(|slot| slot.info.exit.is_some() && !slot.cleanup.done)
            .filter_map(|slot| {
                slot.runtime
                    .as_ref()?
                    .terminal
                    .child_pid()
                    .map(|pid| children::Anchor {
                        session: slot.info.session_id.clone(),
                        pid: pid as libc::pid_t,
                    })
            })
            .collect();
        if anchors.is_empty() {
            return;
        }
        if self.child_scans.try_send(anchors).is_ok() {
            self.scan_inflight = true;
            self.last_scan = Instant::now();
        } else {
            for slot in self
                .slots
                .values_mut()
                .filter(|slot| slot.info.exit.is_some() && slot.runtime.is_some())
            {
                cleanup_notice(slot, "inventory worker unavailable; exit is not confirmed");
            }
        }
    }

    pub fn tick(&mut self) {
        for _ in 0..8 {
            let Ok(completed) = self.results.try_recv() else {
                break;
            };
            self.accept_prepared(completed);
        }
        self.poll_child_inventory();
        let mut changed = false;
        for slot in self.slots.values_mut() {
            let Some(runtime) = &mut slot.runtime else {
                continue;
            };
            if slot.frozen.is_none() {
                if let Ok(status) = runtime.terminal.status() {
                    slot.info.generation = status.generation;
                    if let Some(error) = status.error {
                        slot.info.error = Some(safe_error(error));
                    }
                }
            }
            if slot.info.state == SessionState::Running {
                match runtime.initialization() {
                    Ok(state) => {
                        slot.info.initialization = Some(state);
                    }
                    Err(error) => {
                        slot.info.error = Some(safe_error(error));
                        slot.info.initialization = None;
                    }
                }
            }
            match runtime.terminal.try_wait() {
                Ok(Some(exit)) => {
                    if slot.info.exit.is_none() {
                        // The actual shell status is known, but the leader stays
                        // unreaped until a fresh anchored inventory is empty.
                        slot.info.exit = Some(exit);
                        slot.info.state = SessionState::Closing;
                        slot.info.child_pid = None;
                        slot.info.cwd = None;
                        slot.exited_at = Some(Instant::now());
                        changed = true;
                        cleanup_notice(slot, "shell exited; checking same-session descendants");
                    }
                    let runtime = slot.runtime.as_mut().unwrap();
                    if slot.frozen.is_none()
                        && (runtime
                            .terminal
                            .status()
                            .is_ok_and(|status| status.reader_closed)
                            || slot
                                .exited_at
                                .is_some_and(|time| time.elapsed() >= Duration::from_secs(2)))
                    {
                        let cutoff = !runtime
                            .terminal
                            .status()
                            .is_ok_and(|status| status.reader_closed);
                        if cutoff {
                            if let Err(error) = runtime.terminal.end_collection() {
                                slot.info.error = Some(safe_error(error));
                                continue;
                            }
                            if slot
                                .info
                                .error
                                .as_ref()
                                .is_none_or(|error| error.starts_with("background cleanup:"))
                            {
                                slot.info.error = Some("final output collection ended before PTY EOF; later descendant output may be missing".into());
                            }
                        }
                        slot.frozen = runtime.terminal.snapshot().ok();
                        if let Some(screen) = &slot.frozen {
                            slot.info.generation = screen.generation;
                        } else {
                            slot.info.error.get_or_insert_with(|| {
                                "final terminal screen could not be collected".into()
                            });
                        }
                    }
                    // Final-screen collection and descendant cleanup are separate:
                    // dropping PTY endpoints cannot release the leader SID anchor.
                    if slot.cleanup.done && slot.frozen.is_some() {
                        match runtime.terminal.reap_exit() {
                            Ok(exit) => {
                                slot.info.exit = Some(exit);
                                slot.info.state = if slot.cleanup.requested {
                                    SessionState::Closed
                                } else {
                                    SessionState::Exited
                                };
                                slot.info.owner = None;
                                if slot
                                    .info
                                    .error
                                    .as_ref()
                                    .is_some_and(|error| error.starts_with("background cleanup:"))
                                {
                                    slot.info.error = None;
                                }
                                let resource_dir = runtime.resource_dir.clone();
                                slot.runtime.take();
                                slot.cleanup.signalled.clear();
                                if slot.info.persistent {
                                    slot.purpose = SlotPurpose::Terminal(None);
                                }
                                let _ = std::fs::remove_dir_all(resource_dir);
                                changed = true;
                            }
                            Err(error) => {
                                slot.info.error = Some(safe_error(error));
                            }
                        }
                    }
                }
                Ok(None) => {
                    slot.info.cwd = runtime.terminal.cwd();
                    if slot
                        .closing_at
                        .is_some_and(|time| time.elapsed() >= Duration::from_secs(2))
                    {
                        cleanup_notice(
                            slot,
                            if slot.cleanup.force {
                                "force requested; terminal leader exit is not yet observed"
                            } else {
                                "terminal has not exited after hangup; explicit force close is available"
                            },
                        );
                    }
                }
                Err(error) => {
                    slot.info.error = Some(safe_error(error));
                }
            }
        }
        self.schedule_child_inventory();
        // Preserve minimal durable tombstones for every saved definition. Only
        // final screen memory is evicted; closed defaults never silently reopen.
        let mut ended: Vec<_> = self
            .slots
            .iter()
            .filter(|(_, slot)| slot.frozen.is_some() && !slot.info.state.is_live())
            .map(|(id, slot)| (slot.exited_at, id.clone()))
            .collect();
        ended.sort();
        let discard = ended.len().saturating_sub(16);
        for (_, id) in ended.into_iter().take(discard) {
            if let Some(slot) = self.slots.get_mut(&id) {
                slot.frozen = None;
            }
        }
        let transient_definitions = self
            .slots
            .values()
            .filter(|slot| {
                !slot.info.persistent && matches!(slot.purpose, SlotPurpose::Terminal(Some(_)))
            })
            .count();
        let mut retired_definitions: Vec<_> = self
            .slots
            .iter()
            .filter(|(_, slot)| {
                !slot.info.persistent
                    && !slot.info.state.is_live()
                    && matches!(slot.purpose, SlotPurpose::Terminal(Some(_)))
            })
            .map(|(id, slot)| (slot.exited_at, id.clone()))
            .collect();
        retired_definitions.sort();
        for (_, id) in retired_definitions
            .into_iter()
            .take(transient_definitions.saturating_sub(MAX_TERMINALS))
        {
            if let Some(slot) = self.slots.get_mut(&id) {
                slot.purpose = SlotPurpose::Terminal(None);
            }
        }
        if changed && self.persist().is_err() {
            for slot in self
                .slots
                .values_mut()
                .filter(|slot| slot.info.exit.is_some())
            {
                slot.info.error = Some(
                    "exit observed but durable state save failed; restart may report unknown"
                        .into(),
                );
            }
        }
        if !self.quiescing && self.last_batch_tick.elapsed() >= Duration::from_millis(100) {
            self.last_batch_tick = Instant::now();
            self.tick_batches();
        }
    }
    fn accept_prepared(&mut self, completed: Completed) {
        let Some(slot) = self.slots.get_mut(&completed.session) else {
            // An unreachable defensive path: no slot is discarded while a launch
            // is outstanding. Drop closes its uninitialized PTY if that invariant fails.
            return;
        };
        slot.preparing = false;
        match completed.result {
            Err(error) => {
                slot.info.state = if slot.cancel.load(Ordering::Acquire) {
                    SessionState::Closed
                } else {
                    SessionState::Failed
                };
                slot.info.initialization = Some(InitializationState::Failed);
                slot.info.error = Some(safe_error(error));
                slot.info.owner = None;
                slot.info.child_pid = None;
                slot.info.cwd = None;
                slot.exited_at = Some(Instant::now());
                if slot.info.persistent {
                    slot.purpose = SlotPurpose::Terminal(None);
                }
            }
            Ok(runtime) => {
                slot.info.child_pid = runtime.terminal.child_pid();
                slot.info.definition_revision = runtime.revision;
                slot.info.launch_digest = Some(runtime.digest.clone());
                let cancelled = slot.cancel.load(Ordering::Acquire) || self.quiescing;
                slot.info.state = if cancelled {
                    SessionState::Closing
                } else {
                    SessionState::Running
                };
                slot.runtime = Some(runtime);
                // Persist the accepted runtime BEFORE the sole bootstrap write.
                let persistence = self.persist();
                let slot = self.slots.get_mut(&completed.session).unwrap();
                let runtime = slot.runtime.as_mut().unwrap();
                if cancelled || persistence.is_err() {
                    slot.info.state = SessionState::Closing;
                    slot.cleanup.requested = true;
                    slot.closing_at = Some(Instant::now());
                    slot.info.error = Some(
                        if cancelled {
                            "terminal creation cancelled before initialization"
                        } else {
                            "durable session record failed; initialization was not executed"
                        }
                        .into(),
                    );
                    runtime.bootstrap.clear();
                    let _ = runtime.terminal.request_host_close(false);
                } else {
                    let bootstrap = std::mem::take(&mut runtime.bootstrap);
                    if let Err(error) = runtime.terminal.input(&bootstrap) {
                        slot.info.state = SessionState::Closing;
                        slot.cleanup.requested = true;
                        slot.closing_at = Some(Instant::now());
                        slot.info.error = Some(safe_error(error));
                        let _ = runtime.terminal.request_host_close(false);
                    }
                }
            }
        }
        if self.persist().is_err() {
            if let Some(slot) = self.slots.get_mut(&completed.session) {
                slot.info.error = Some(
                    "durable session record failed; inspect current host before restart".into(),
                );
            }
        }
    }
}
fn cleanup_notice(slot: &mut Slot, message: &str) {
    if slot
        .info
        .error
        .as_ref()
        .is_none_or(|error| error.starts_with("background cleanup:"))
    {
        slot.info.error = Some(safe_error(format!("background cleanup: {message}")));
    }
}
fn cancel_batch(batch: &mut Batch) {
    if matches!(batch.info.state, BatchState::Running | BatchState::Paused) {
        batch.info.state = BatchState::Cancelled;
    }
    batch.info.remaining.clear();
    batch.info.waiting_session = None;
    batch.env.clear();
    batch.waiting_since = None;
}
