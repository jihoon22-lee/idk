use crate::model::TerminalDefinition;
use crate::project::{LaunchEnvironment, ProjectService};
use crate::shell::InitializationState;
use crate::store::{read_private, Store};
use crate::terminal::TerminalSession;
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

pub(super) struct Job {
    pub session: String,
    pub project: String,
    pub definition: TerminalDefinition,
    pub environment: BTreeMap<String, String>,
    pub rows: u16,
    pub cols: u16,
    pub cancelled: Arc<AtomicBool>,
}
pub(super) struct Runtime {
    pub terminal: TerminalSession,
    pub state_path: PathBuf,
    pub resource_dir: PathBuf,
    pub bootstrap: Vec<u8>,
    pub revision: u64,
    pub digest: String,
}
impl Runtime {
    pub fn initialization(&self) -> Result<InitializationState> {
        let bytes = read_private(&self.state_path, 64)?;
        match std::str::from_utf8(&bytes)?.trim() {
            "initializing" | "" => Ok(InitializationState::Initializing),
            "ready" => Ok(InitializationState::Ready),
            "failed" => Ok(InitializationState::Failed),
            _ => bail!("unrecognized shell initialization state"),
        }
    }
}
pub(super) struct Completed {
    pub session: String,
    pub result: Result<Runtime>,
}

pub(super) fn worker(
    store: Store,
    launcher: PathBuf,
    resources: PathBuf,
) -> Result<(mpsc::SyncSender<Job>, mpsc::Receiver<Completed>)> {
    let (jobs, incoming) = mpsc::sync_channel::<Job>(8);
    let (outgoing, results) = mpsc::sync_channel(8);
    std::thread::Builder::new()
        .name("idk-shell-prepare".into())
        .spawn(move || {
            while let Ok(job) = incoming.recv() {
                let session = job.session.clone();
                let result = prepare(&store, &launcher, &resources, job);
                if outgoing.send(Completed { session, result }).is_err() {
                    break;
                }
            }
        })
        .context("start shell preparation worker")?;
    Ok((jobs, results))
}
fn prepare(
    store: &Store,
    launcher: &std::path::Path,
    resources: &std::path::Path,
    job: Job,
) -> Result<Runtime> {
    if job.cancelled.load(Ordering::Acquire) {
        bail!("terminal creation cancelled before preparation");
    }
    let environment = LaunchEnvironment::from_variables(job.environment)?;
    let plan = (ProjectService { store }).terminal_launch_plan(
        &job.project,
        &job.definition,
        environment,
    )?;
    if job.cancelled.load(Ordering::Acquire) {
        bail!("terminal creation cancelled before shell creation");
    }
    let prepared = plan.shell.prepare(resources, launcher)?;
    if job.cancelled.load(Ordering::Acquire) {
        let _ = std::fs::remove_dir_all(&prepared.resource_dir);
        bail!("terminal creation cancelled before shell creation");
    }
    let mut terminal = match TerminalSession::spawn(prepared.command, job.rows, job.cols, 2000) {
        Ok(terminal) => terminal,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&prepared.resource_dir);
            return Err(error);
        }
    };
    // Enable before any input/try_wait call can release a fast-exiting leader.
    terminal.defer_reaping();
    // Only the actor may send bootstrap. Cancellation, persistence and ownership
    // are checked there after the newly owned child has been returned.
    Ok(Runtime {
        terminal,
        state_path: prepared.state_path,
        resource_dir: prepared.resource_dir,
        bootstrap: prepared.bootstrap_bytes,
        revision: plan.definition_revision,
        digest: plan.launch_digest,
    })
}
