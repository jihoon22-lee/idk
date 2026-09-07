//! Bounded RPC worker. A failed request is never retried and input never crosses a host instance.
use crate::{client::Client, protocol::*, store::Store, terminal::TerminalSnapshot};
use anyhow::{ensure, Context, Result};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc,
    },
    time::{Duration, Instant},
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum Tag {
    Connect,
    List,
    Snapshot(String),
    Attached,
    Started,
    Definition(String),
    Batch,
    Preview,
    Close,
    Ack,
    Input,
    Search,
    GitSubmit(u64),
    GitJob(String),
    GitExecute(String),
    GitAttached,
    GitSnapshot(String),
    GitOperations,
    GitUpdated,
}
#[derive(Clone, PartialEq, Eq)]
enum InputTarget {
    Shell(String),
    Git(String),
}
struct BufferedInput {
    host: String,
    target: InputTarget,
    epoch: u64,
    data: Vec<u8>,
}
struct Job {
    host: Option<String>,
    request: Option<Request>,
    tag: Tag,
}
pub(super) struct Reply {
    pub tag: Tag,
    pub result: Result<serde_json::Value>,
    pub identity: Option<(HostInfo, String)>,
}
pub(super) struct Runtime {
    sender: SyncSender<Job>,
    receiver: Receiver<Reply>,
    stop: Arc<AtomicBool>,
    finished: Receiver<()>,
    pub host: Option<HostInfo>,
    pub client_id: Option<String>,
    pub online: bool,
    pub sessions: Vec<SessionInfo>,
    pub active: Option<SessionInfo>,
    pub screen: Option<TerminalSnapshot>,
    pub batch: Option<BatchInfo>,
    pub seen: HashMap<String, u64>,
    pub definitions: HashMap<String, crate::model::TerminalDefinition>,
    pending: Vec<Tag>,
    input: VecDeque<BufferedInput>,
    input_bytes: usize,
    inflight_input: VecDeque<usize>,
    last_input: Instant,
    last_list: Instant,
    last_screen: Instant,
    pub dimensions: (u16, u16),
    pub resized: Option<(String, u64, u16, u16)>,
    pub definition_notice: Option<String>,
}
impl Runtime {
    pub fn new(store: Store, launcher: PathBuf) -> Result<Self> {
        let (sender, jobs) = mpsc::sync_channel::<Job>(16);
        let (events, receiver) = mpsc::sync_channel(16);
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let (done, finished) = mpsc::sync_channel(1);
        std::thread::Builder::new().name("idk-ui-rpc".into()).spawn(move || {
            let mut client: Option<Client> = None;
            let mut failed = false;
            let mut owned = HashMap::<String,u64>::new();
            let mut git_owned = HashMap::<String,u64>::new();
            while !stopping.load(Ordering::Acquire) {
                let job = match jobs.recv_timeout(Duration::from_millis(50)) { Ok(job)=>job, Err(mpsc::RecvTimeoutError::Timeout)=>continue, Err(_)=>break };
                let result = (|| -> Result<serde_json::Value> {
                    if job.tag == Tag::Connect {
                        if let Some(existing) = &mut client {
                            if existing.list(None).is_err() { client = Some(Client::connect_with_launcher(&store,&launcher)?); owned.clear(); git_owned.clear(); }
                        } else { client = Some(Client::connect_with_launcher(&store,&launcher)?); }
                        failed = false;
                    }
                    let starts = matches!(job.request,Some(Request::Start{..}|Request::StartTransient{..}|Request::StartDefaults{..}|Request::GitSubmit{task:crate::git_wire::GitTask::Open{..}}));
                    if client.is_none() && starts { client = Some(Client::ensure_host(&store,&launcher)?); failed = false; }
                    let client = client.as_mut().context("Host is unavailable. Open a terminal to start a host, or refresh to reconnect.")?;
                    client.set_timeout(Duration::from_secs(2))?;
                    if let Some(expected) = &job.host { ensure!(expected == &client.info().host_instance,"Host instance changed; queued action was discarded. Refresh and choose the terminal again."); }
                    if failed && job.tag != Tag::Connect { anyhow::bail!("Connection needs an explicit refresh. The action was not replayed."); }
                    let Some(request) = job.request else { return Ok(serde_json::Value::Null) };
                    let result: Result<serde_json::Value> = client.request(request).and_then(|response|response.decode());
                    if result.as_ref().is_err_and(|error| error.downcast_ref::<std::io::Error>().is_some() || matches!(job.tag,Tag::Input|Tag::Ack)) { failed = true; }
                    if let Ok(value) = &result {
                        if job.tag == Tag::GitAttached {
                            let operation:crate::git_wire::GitOperationInfo=serde_json::from_value(value.clone())?;
                            git_owned.insert(operation.id,operation.input_epoch);
                        }
                        if job.tag == Tag::Attached {
                            let session: SessionInfo = serde_json::from_value(value.clone())?;
                            owned.insert(session.session_id,session.input_epoch);
                        }
                    }
                    result
                })();
                let identity = client.as_ref().map(|client|(client.info().clone(),client.client_id().to_owned()));
                let mut reply = Reply { tag: job.tag, result, identity };
                loop {
                    match events.try_send(reply) {
                        Ok(())=>break,
                        Err(mpsc::TrySendError::Full(value))=>{ reply=value; if stopping.load(Ordering::Acquire) { break; } std::thread::sleep(Duration::from_millis(10)); },
                        Err(mpsc::TrySendError::Disconnected(_))=>break,
                    }
                }
            }
            if let Some(mut client) = client { let _ = client.set_timeout(Duration::from_millis(100)); for (id,epoch) in owned { let _ = client.detach(&id,epoch); }
                for (operation,epoch) in git_owned {let _=client.request(Request::GitOperationDetach{operation,epoch});} }
            let _ = done.send(());
        })?;
        let mut runtime = Self {
            sender,
            receiver,
            finished,
            stop,
            host: None,
            client_id: None,
            online: false,
            sessions: Vec::new(),
            active: None,
            screen: None,
            batch: None,
            seen: HashMap::new(),
            definitions: HashMap::new(),
            pending: Vec::new(),
            input: VecDeque::new(),
            input_bytes: 0,
            inflight_input: VecDeque::new(),
            last_input: Instant::now(),
            last_list: Instant::now() - Duration::from_secs(2),
            last_screen: Instant::now(),
            dimensions: (22, 80),
            resized: None,
            definition_notice: None,
        };
        runtime.connect()?;
        Ok(runtime)
    }
    pub fn connect(&mut self) -> Result<()> {
        self.submit(None, Tag::Connect)
    }
    pub fn submit(&mut self, request: Option<Request>, tag: Tag) -> Result<()> {
        ensure!(
            self.pending.len() < 16,
            "Host input queue is full. This input was not sent."
        );
        let host = if tag == Tag::Connect {
            None
        } else {
            self.host.as_ref().map(|host| host.host_instance.clone())
        };
        self.sender
            .try_send(Job {
                host,
                request,
                tag: tag.clone(),
            })
            .context("Host queue is busy. This action was not sent.")?;
        self.pending.push(tag);
        Ok(())
    }
    pub fn drain(&mut self) -> Vec<Reply> {
        let mut replies = Vec::new();
        while let Ok(reply) = self.receiver.try_recv() {
            if reply.tag == Tag::Input {
                self.inflight_input.pop_front();
            }
            if let Some(index) = self.pending.iter().position(|tag| tag == &reply.tag) {
                self.pending.remove(index);
            }
            replies.push(reply);
        }
        replies
    }
    pub fn can_input(&self) -> bool {
        self.online
            && self
                .screen
                .as_ref()
                .is_some_and(|screen| screen.error.is_none() && !screen.reader_closed)
            && self.active.as_ref().is_some_and(|session| {
                matches!(
                    session.state,
                    SessionState::Preparing | SessionState::Running
                ) && self
                    .host
                    .as_ref()
                    .is_some_and(|host| host.host_instance == session.host_instance)
                    && session
                        .owner
                        .as_ref()
                        .is_some_and(|owner| Some(&owner.client_id) == self.client_id.as_ref())
            })
    }
    pub fn clear_input(&mut self) {
        self.input.clear();
        self.input_bytes = 0;
    }
    pub fn buffer_input(&mut self, session: String, epoch: u64, data: Vec<u8>) -> Result<()> {
        self.buffer_target(InputTarget::Shell(session), epoch, data)
    }
    pub fn buffer_git_input(&mut self, operation: String, epoch: u64, data: Vec<u8>) -> Result<()> {
        self.buffer_target(InputTarget::Git(operation), epoch, data)
    }
    fn buffer_target(&mut self, target: InputTarget, epoch: u64, data: Vec<u8>) -> Result<()> {
        ensure!(
            data.len() <= MAX_INPUT_PACKET
                && self.input_bytes + self.inflight_input.iter().sum::<usize>() + data.len()
                    <= 1024 * 1024,
            "Terminal input queue exceeds 1 MiB. This input was not sent."
        );
        let host = self
            .host
            .as_ref()
            .context("No verified host identity")?
            .host_instance
            .clone();
        self.input_bytes += data.len();
        if let Some(last) = self.input.back_mut().filter(|last| {
            last.host == host
                && last.target == target
                && last.epoch == epoch
                && last.data.len() + data.len() <= MAX_INPUT_PACKET
        }) {
            last.data.extend_from_slice(&data);
        } else {
            self.input.push_back(BufferedInput {
                host,
                target,
                epoch,
                data,
            });
        }
        Ok(())
    }
    fn flush_input(&mut self) -> Result<()> {
        use base64::Engine;
        if self.last_input.elapsed() < Duration::from_millis(15) || self.pending.len() >= 4 {
            return Ok(());
        }
        let Some(input) = self.input.pop_front() else {
            return Ok(());
        };
        let count = input.data.len();
        self.input_bytes -= count;
        ensure!(
            self.host
                .as_ref()
                .is_some_and(|host| host.host_instance == input.host),
            "Host changed; buffered input was discarded."
        );
        let data = base64::engine::general_purpose::STANDARD.encode(input.data);
        let request = match input.target {
            InputTarget::Shell(session) => Request::Input {
                session,
                epoch: input.epoch,
                data,
            },
            InputTarget::Git(operation) => Request::GitOperationInput {
                operation,
                epoch: input.epoch,
                data,
            },
        };
        self.submit(Some(request), Tag::Input)?;
        self.inflight_input.push_back(count);
        self.last_input = Instant::now();
        Ok(())
    }
    pub fn poll(&mut self) -> Result<()> {
        if self.online {
            self.flush_input()?;
        }
        if !self.online || self.pending.len() > 4 {
            return Ok(());
        }
        let pending: HashSet<Tag> = self.pending.iter().cloned().collect();
        if self.last_list.elapsed() >= Duration::from_secs(1) && !pending.contains(&Tag::List) {
            self.submit(Some(Request::List { project: None }), Tag::List)?;
            self.last_list = Instant::now();
        }
        if self.last_screen.elapsed() >= Duration::from_millis(100) {
            if let Some(active) = &self.active {
                let id = active.session_id.clone();
                let tag = Tag::Snapshot(id.clone());
                if !pending.contains(&tag) {
                    let since = self.screen.as_ref().map(|screen| screen.generation);
                    self.submit(Some(Request::Snapshot { session: id, since }), tag)?;
                }
            }
            if let Some(batch) = &self.batch {
                if matches!(batch.state, BatchState::Running | BatchState::Paused)
                    && !pending.contains(&Tag::Batch)
                {
                    self.submit(
                        Some(Request::Batch {
                            batch: batch.batch_id.clone(),
                        }),
                        Tag::Batch,
                    )?;
                }
            }
            self.last_screen = Instant::now();
        }
        if self.can_input() {
            let active = self.active.as_ref().unwrap();
            let (rows, cols) = self.dimensions;
            let target = (active.session_id.clone(), active.input_epoch, rows, cols);
            if self.resized.as_ref() != Some(&target) {
                self.submit(
                    Some(Request::Resize {
                        session: target.0.clone(),
                        epoch: target.1,
                        rows,
                        cols,
                    }),
                    Tag::Ack,
                )?;
                self.resized = Some(target);
            }
        }
        Ok(())
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.finished.recv_timeout(Duration::from_millis(300));
    }
}
