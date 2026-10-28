//! Asynchronous task, Run, raw-log and editor workflows bound to reviewed identities.
use super::{
    git_draft::CommitDraft, run_form::RunForm, run_log::LogView, runtime::Tag, App, Dialog, Focus,
};
use crate::{
    model::{FailurePolicy, SourceSpec, TaskDefinition, TaskLogging, TaskStep},
    protocol::{Request, SessionInfo, SnapshotReply},
    run_wire::*,
};
use anyhow::{ensure, Context, Result};
use crossterm::event::{KeyCode, KeyEvent};
use std::{
    collections::HashMap,
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Page {
    Runs,
    Detail,
    Log,
    Problems,
    Search,
    Findings,
    RawResult,
    Comparison,
    Events,
}
#[derive(Clone, Debug)]
pub(super) enum RunDialog {
    Form(Box<RunForm>),
    TaskReview {
        review: Box<crate::task::TaskReview>,
        parallel: bool,
        scroll: u16,
    },
    Confirm {
        title: String,
        lines: Vec<String>,
        request: Box<RunRequest>,
        scroll: u16,
    },
    Editor {
        review_id: String,
        review: crate::editor::EditorReview,
        scroll: u16,
    },
    Search(CommitDraft),
    Export(CommitDraft),
    Configuration {
        root: PathBuf,
        expected: Option<String>,
        draft: CommitDraft,
    },
    Discovery {
        report: crate::discovery::DiscoveryReport,
        selected: usize,
    },
    Attach {
        session: String,
        owner: String,
    },
}
#[derive(Clone)]
struct Intent {
    request: RunRequest,
    project: Option<String>,
    run: Option<String>,
    epoch: u64,
    tab: usize,
    job: Option<String>,
    polling: bool,
    cancelled: bool,
}
pub(super) struct RunUi {
    pub tasks: HashMap<String, (u64, Vec<TaskDefinition>)>,
    pub task_index: usize,
    pub runs: Vec<RunInfo>,
    pub run_index: usize,
    pub all_projects: bool,
    pub selected: Option<String>,
    pub page: Page,
    pub log: Option<LogView>,
    pub problems: Option<crate::problems::ProblemSet>,
    pub search: Option<LogSearch>,
    pub result_raw: Option<crate::result_store::RawResultPage>,
    pub findings: Option<crate::result_store::FindingPage>,
    pub comparison: Option<crate::result_store::ResultComparison>,
    pub events: Option<crate::result_store::EventProgress>,
    approval_start: Option<RunRequest>,
    pub baseline: Option<String>,
    baseline_pending: Option<String>,
    pub advanced_details: bool,
    pub finding_index: usize,
    raw_history: Vec<u64>,
    finding_history: Vec<usize>,
    pub problem_index: usize,
    pub match_index: usize,
    pub scroll: u16,
    pub drafts: HashMap<(String, Option<String>), RunForm>,
    pub attached: Option<String>,
    input_policy: HashMap<String, bool>,
    pending_attach: Option<(String, bool)>,
    attach_inflight: Option<(String, u64)>,
    intents: HashMap<u64, Intent>,
    nonce: u64,
    epoch: u64,
    project: Option<String>,
    last_poll: Instant,
    last_jobs: Instant,
}
impl Default for RunUi {
    fn default() -> Self {
        Self {
            tasks: HashMap::new(),
            task_index: 0,
            runs: vec![],
            run_index: 0,
            all_projects: false,
            selected: None,
            page: Page::Runs,
            log: None,
            problems: None,
            search: None,
            result_raw: None,
            findings: None,
            comparison: None,
            events: None,
            approval_start: None,
            baseline: None,
            baseline_pending: None,
            advanced_details: false,
            finding_index: 0,
            raw_history: vec![],
            finding_history: vec![],
            problem_index: 0,
            match_index: 0,
            scroll: 0,
            drafts: HashMap::new(),
            attached: None,
            input_policy: HashMap::new(),
            pending_attach: None,
            attach_inflight: None,
            intents: HashMap::new(),
            nonce: 0,
            epoch: 0,
            project: None,
            last_poll: Instant::now() - Duration::from_secs(3),
            last_jobs: Instant::now(),
        }
    }
}
impl App<'_> {
    /// Open the recorded Run terminal; it never resolves a shell definition or starts a task.
    pub fn attach_run(&mut self, run_id: &str, takeover: bool) -> Result<()> {
        crate::model::valid_id(run_id)?;
        self.runs.project = self.current_id();
        self.runs.selected = Some(run_id.into());
        self.runs.pending_attach = Some((run_id.into(), takeover));
        self.runs.page = Page::Detail;
        self.tab = 3;
        self.focus = Focus::Content;
        self.run_submit(RunRequest::Info {
            run_id: run_id.into(),
        })
    }

    fn run_submit(&mut self, request: RunRequest) -> Result<()> {
        let limit = if matches!(request, RunRequest::CancelJob { .. }) {
            32
        } else {
            16
        };
        ensure!(
            self.runs.intents.len() < limit,
            "Run request queue is full; wait for a result or cancel pending reads"
        );
        self.runs.nonce += 1;
        let nonce = self.runs.nonce;
        let intent = Intent {
            request: request.clone(),
            project: self.current_id(),
            run: self.runs.selected.clone(),
            epoch: self.runs.epoch,
            tab: self.tab,
            job: None,
            polling: false,
            cancelled: false,
        };
        self.runtime
            .as_mut()
            .context("Open the terminal runtime to use Tasks")?
            .submit(Some(Request::Run { request }), Tag::RunSubmit(nonce))?;
        self.runs.intents.insert(nonce, intent);
        Ok(())
    }
    pub(super) fn invalidate_run_navigation(&mut self) {
        self.runs.approval_start = None;
        self.runs.epoch += 1;
        self.runs.pending_attach = None;
        self.runs.attach_inflight = None;
    }
    pub(super) fn run_host_changed(&mut self) {
        self.runs.approval_start = None;
        self.runs.baseline_pending = None;
        self.runs.attach_inflight = None;
        self.runs.result_raw = None;
        self.runs.findings = None;
        self.runs.comparison = None;
        self.runs.events = None;
        self.runs.intents.clear();
        self.runs.attached = None;
        self.runs.input_policy.clear();
        self.runs.pending_attach = None;
        self.runs.epoch += 1;
        self.runs.log = None;
        self.runs.problems = None;
        self.runs.search = None;
        self.runs.runs.clear();
    }
    pub(super) fn run_rpc_error(&mut self, tag: &Tag) {
        if matches!(tag, Tag::RunAttached(_) | Tag::RunAttachInfo(_)) {
            self.runs.attach_inflight = None;
        }
        match tag {
            Tag::RunSubmit(n) | Tag::RunJob(n) => {
                if self
                    .runs
                    .intents
                    .get(n)
                    .is_some_and(|intent| matches!(intent.request, RunRequest::ResultPin { .. }))
                {
                    self.runs.baseline_pending = None;
                }
                if self
                    .runs
                    .intents
                    .get(n)
                    .is_some_and(|intent| matches!(intent.request, RunRequest::ApproveTask { .. }))
                {
                    self.runs.approval_start = None;
                }
                self.runs.intents.remove(n);
            }
            _ => {}
        }
    }
    pub(super) fn tick_runs(&mut self) {
        if self.runtime.is_none() {
            return;
        }
        if self.runs.project != self.current_id() {
            self.runs.project = self.current_id();
            self.runs.epoch += 1;
            self.runs.selected = None;
            self.runs.page = Page::Runs;
            self.runs.log = None;
            self.runs.problems = None;
            self.runs.search = None;
            self.runs.task_index = 0;
            self.runs.run_index = 0;
        }
        if self.runs.last_jobs.elapsed() > Duration::from_millis(100) {
            let jobs = self
                .runs
                .intents
                .iter()
                .filter_map(|(nonce, intent)| {
                    (!intent.polling)
                        .then(|| intent.job.clone().map(|job| (*nonce, job)))
                        .flatten()
                })
                .collect::<Vec<_>>();
            for (nonce, job_id) in jobs {
                if self
                    .runtime
                    .as_mut()
                    .unwrap()
                    .submit(
                        Some(Request::Run {
                            request: RunRequest::Job { job_id },
                        }),
                        Tag::RunJob(nonce),
                    )
                    .is_ok()
                {
                    self.runs.intents.get_mut(&nonce).unwrap().polling = true;
                }
            }
            self.runs.last_jobs = Instant::now();
        }
        if !self.runtime.as_ref().is_some_and(|r| r.online) || !self.runs.intents.is_empty() {
            return;
        }
        if self.dialog.is_none()
            && matches!(self.tab, 2 | 3)
            && self.runs.last_poll.elapsed() > Duration::from_millis(700)
        {
            self.runs.last_poll = Instant::now();
            let result = if self.tab == 2 {
                self.current_id()
                    .map(|project_id| self.run_submit(RunRequest::Tasks { project_id }))
                    .unwrap_or(Ok(()))
            } else if self.runs.page == Page::Runs {
                self.run_submit(RunRequest::List {
                    project_id: if self.runs.all_projects {
                        None
                    } else {
                        self.current_id()
                    },
                })
            } else if let Some(run_id) = self.runs.selected.clone() {
                self.run_submit(RunRequest::Info { run_id })
            } else {
                Ok(())
            };
            if let Err(error) = result {
                self.error(error.to_string());
            }
        }
    }
    pub(super) fn accept_run_reply(&mut self, tag: Tag, value: serde_json::Value) -> Result<()> {
        if let Tag::RunAttached(epoch) = tag {
            let session: SessionInfo = serde_json::from_value(value)?;
            if self.runs.attach_inflight.as_ref() == Some(&(session.session_id.clone(), epoch)) {
                self.runs.attach_inflight = None;
            }
            if epoch != self.runs.epoch {
                self.runtime.as_mut().unwrap().submit(
                    Some(Request::Detach {
                        session: session.session_id,
                        epoch: session.input_epoch,
                    }),
                    Tag::Ack,
                )?;
                return Ok(());
            }
            self.git.focused = false;
            self.runs.attached = Some(session.session_id.clone());
            let runtime = self.runtime.as_mut().unwrap();
            runtime.input_read_only = self
                .runs
                .input_policy
                .get(&session.session_id)
                .copied()
                .unwrap_or(true);
            runtime.active = Some(session);
            runtime.screen = None;
            runtime.resized = None;
            runtime.definition_notice = None;
            self.set_terminal_connected(true);
            return Ok(());
        }
        if let Tag::RunAttachInfo(epoch) = tag {
            if epoch != self.runs.epoch {
                return Ok(());
            }
            let reply: SnapshotReply = serde_json::from_value(value)?;
            let owner = reply.session.owner.as_ref().filter(|owner| {
                Some(&owner.client_id) != self.runtime.as_ref().and_then(|r| r.client_id.as_ref())
            });
            if let Some(owner) = owner {
                self.dialog = Some(Dialog::Run(Box::new(RunDialog::Attach {
                    session: reply.session.session_id,
                    owner: owner.client_id.clone(),
                })));
            } else {
                self.run_attach(&reply.session.session_id, false)?;
            }
            return Ok(());
        }
        let nonce = match tag {
            Tag::RunSubmit(n) | Tag::RunJob(n) => n,
            _ => return Ok(()),
        };
        let Some(mut intent) = self.runs.intents.remove(&nonce) else {
            return Ok(());
        };
        let job: RunJob = serde_json::from_value(value)?;
        let current = intent.epoch == self.runs.epoch
            && intent.project == self.current_id()
            && intent.tab == self.tab;
        if job.state == RunJobState::Pending {
            if intent.cancelled {
                self.run_submit(RunRequest::CancelJob { job_id: job.job_id })?;
                return Ok(());
            }
            if intent.job.is_none()
                && current
                && matches!(
                    intent.request,
                    RunRequest::Start { .. } | RunRequest::EditorOpen { .. }
                )
            {
                if let Some(session) = &job.session_id {
                    self.runs.input_policy.insert(
                        session.clone(),
                        matches!(intent.request, RunRequest::Start { .. }),
                    );
                    self.run_attach_review(session)?;
                }
            }
            intent.job = Some(job.job_id);
            intent.polling = false;
            self.runs.intents.insert(nonce, intent);
            return Ok(());
        }
        if job.state != RunJobState::Complete {
            if matches!(intent.request, RunRequest::ResultPin { .. }) {
                self.runs.baseline_pending = None;
            }
            if matches!(intent.request, RunRequest::ApproveTask { .. }) {
                self.runs.approval_start = None;
            }
            if matches!(intent.request, RunRequest::Info { .. }) {
                self.runs.pending_attach = None;
            }
            self.error(
                job.error
                    .unwrap_or_else(|| format!("Run request {:?}; local drafts kept", job.state)),
            );
            return Ok(());
        }
        let Some(result) = job.result else {
            return Ok(());
        };
        match result {
            RunResult::Tasks { revision, tasks } => {
                if let Some(project) = intent.project {
                    if self
                        .runs
                        .tasks
                        .get(&project)
                        .is_some_and(|(saved, _)| *saved > revision)
                    {
                        return Ok(());
                    }
                    let selected = self
                        .runs
                        .tasks
                        .get(&project)
                        .and_then(|(_, old)| old.get(self.runs.task_index))
                        .map(|task| task.id.clone());
                    if current {
                        self.runs.task_index = selected
                            .and_then(|id| tasks.iter().position(|task| task.id == id))
                            .unwrap_or(0);
                    }
                    self.runs.tasks.insert(project, (revision, tasks));
                }
            }
            RunResult::Runs(runs) => {
                if current {
                    let selected = self
                        .runs
                        .runs
                        .get(self.runs.run_index)
                        .map(|run| run.run_id.clone());
                    self.runs.runs = runs;
                    self.runs.run_index = selected
                        .and_then(|id| self.runs.runs.iter().position(|run| run.run_id == id))
                        .unwrap_or(0);
                }
            }
            RunResult::Review(review) => {
                if current && self.menu_visible && self.dialog.is_none() {
                    self.dialog = Some(Dialog::Run(Box::new(RunDialog::TaskReview {
                        review: Box::new(review),
                        parallel: false,
                        scroll: 0,
                    })));
                }
            }
            RunResult::Task(task) => {
                self.reload(None)?;
                if let Some(project_id) = intent.project {
                    let tasks = self.project(&project_id)?.tasks.clone();
                    if current {
                        self.runs.task_index = tasks
                            .iter()
                            .position(|saved| saved.id == task.id)
                            .unwrap_or(0);
                    }
                    self.runs
                        .tasks
                        .insert(project_id.clone(), (self.view.revision, tasks));
                    self.runs.drafts.retain(|(project, _), form| {
                        project != &project_id
                            || form.task.as_ref().is_none_or(|draft| draft.id != task.id)
                    });
                }
                self.info("Definition saved; it has not been launched. Review before use.");
            }
            RunResult::EditorSaved => {
                self.reload(None)?;
                if let Some(project) = intent.project {
                    self.runs.drafts.remove(&(project, Some("@editor".into())));
                }
                self.info("Definition saved; it has not been launched. Review before use.");
            }
            RunResult::Approved => {
                self.reload(None)?;
                if let RunRequest::ApproveTask {
                    project_id,
                    task_id,
                    ..
                } = &intent.request
                {
                    let tasks = self.project(project_id)?.tasks.clone();
                    if current {
                        self.runs.task_index = tasks
                            .iter()
                            .position(|task| &task.id == task_id)
                            .unwrap_or(0);
                    }
                    self.runs
                        .tasks
                        .insert(project_id.clone(), (self.view.revision, tasks));
                }
                if let Some(request) = self.runs.approval_start.take() {
                    let same = match (&request, &intent.request) {
                        (
                            RunRequest::Start {
                                project_id: a,
                                task_id: b,
                                expected_digest: Some(c),
                                ..
                            },
                            RunRequest::ApproveTask {
                                project_id: x,
                                task_id: y,
                                digest: z,
                                ..
                            },
                        ) => a == x && b == y && c == z,
                        _ => false,
                    };
                    if same && current && self.dialog.is_none() && self.menu_visible {
                        self.run_submit(request)?;
                    } else {
                        self.info("Task approved. Start was cancelled because the review context changed.");
                    }
                } else {
                    self.info("Task approved.");
                }
                self.runs.last_poll = Instant::now() - Duration::from_secs(3);
            }
            RunResult::Started(reply) => {
                let session = reply.run.session_id.clone();
                let id = reply.run.run_id.clone();
                self.run_update(reply.run);
                self.info(if reply.existing {
                    "Existing Run reattached; task was not launched again."
                } else {
                    "Run registered. Ctrl+g opens controls; closing the UI keeps it running."
                });
                if current {
                    self.runs.selected = Some(id);
                    self.tab = 3;
                    self.runs.page = Page::Detail;
                    if let Some(session) = session {
                        if self.runs.attached.as_ref() != Some(&session) {
                            self.run_attach_review(&session)?;
                        }
                    }
                }
            }
            RunResult::Run(run) | RunResult::Reconciled(run) => {
                if current
                    && self
                        .runs
                        .pending_attach
                        .as_ref()
                        .is_some_and(|(id, _)| id == &run.run_id)
                {
                    let (_, takeover) = self.runs.pending_attach.take().unwrap();
                    if let Some(index) = self
                        .view
                        .projects
                        .iter()
                        .position(|project| project.project.id == run.project_id)
                    {
                        self.project_index = index;
                        self.runs.project = self.current_id();
                    }
                    let session = run.session_id.clone().context(
                        "This recorded Run has no available terminal; its result remains available",
                    )?;
                    if takeover {
                        self.run_attach(&session, true)?;
                    } else {
                        self.run_attach_review(&session)?;
                    }
                }

                let log = (run.run_id.clone(), run.log.generation, run.log.state);
                self.run_update(run);
                if current && intent.run == self.runs.selected && self.runs.page == Page::Log {
                    if let Some(view) = &self.runs.log {
                        if view.follow
                            && view.generation == log.1
                            && !matches!(log.2, LogState::Disabled | LogState::Expired)
                        {
                            self.run_read_log()?;
                        }
                    }
                }
            }
            RunResult::Log(chunk) => {
                if current && intent.run == self.runs.selected && self.runs.page == Page::Log {
                    if let Some(log) = &mut self.runs.log {
                        log.push(&chunk)?;
                        if !chunk.eof && log.follow {
                            self.run_read_log()?;
                        }
                    }
                }
            }
            RunResult::Search(search) => {
                if current && intent.run == self.runs.selected && self.runs.page == Page::Search {
                    self.runs.search = Some(search);
                }
            }
            RunResult::Problems(problems) => {
                if current && intent.run == self.runs.selected && self.runs.page == Page::Problems {
                    self.runs.problems = Some(problems);
                    self.runs.problem_index = 0;
                }
            }
            RunResult::ResultRaw(page) => {
                if current && intent.run == self.runs.selected {
                    if let Some(old) = &self.runs.result_raw {
                        if page.offset > old.offset {
                            self.runs.raw_history.push(old.offset);
                        } else if page.offset < old.offset
                            && self.runs.raw_history.last() == Some(&page.offset)
                        {
                            self.runs.raw_history.pop();
                        }
                    }
                    self.runs.result_raw = Some(page);
                }
            }
            RunResult::Findings(page) => {
                if current && intent.run == self.runs.selected {
                    if let Some(old) = &self.runs.findings {
                        if page.offset > old.offset {
                            self.runs.finding_history.push(old.offset);
                        } else if page.offset < old.offset
                            && self.runs.finding_history.last() == Some(&page.offset)
                        {
                            self.runs.finding_history.pop();
                        }
                    }
                    self.runs.findings = Some(page);
                }
            }
            RunResult::ResultPinned(run) => {
                if self.runs.baseline_pending.as_deref() == Some(&run.run_id) {
                    self.runs.baseline_pending = None;
                    if run.pinned {
                        self.runs.baseline = Some(run.run_id.clone());
                    }
                }
                self.run_update(run);
                self.info("Run retention updated.");
            }
            RunResult::ResultExported(export) => {
                self.info(format!(
                    "Exported {} bytes to {} · SHA256 {}",
                    export.bytes,
                    export.path.display(),
                    export.sha256
                ));
            }
            RunResult::Compared(comparison) => {
                if current && intent.run == self.runs.selected {
                    self.runs.comparison = Some(comparison);
                }
            }
            RunResult::Events(progress) => {
                if current && intent.run == self.runs.selected {
                    self.runs.events = Some(progress);
                }
            }
            RunResult::EditorReview { review_id, review } => {
                if current
                    && intent.run == self.runs.selected
                    && self.menu_visible
                    && self.dialog.is_none()
                {
                    self.dialog = Some(Dialog::Run(Box::new(RunDialog::Editor {
                        review_id,
                        review,
                        scroll: 0,
                    })));
                }
            }
            RunResult::EditorOpened { session_id } => {
                self.runs.input_policy.insert(session_id.clone(), false);
                if current && self.runs.attached.as_ref() != Some(&session_id) {
                    self.run_attach_review(&session_id)?;
                }
            }
        }
        Ok(())
    }
    fn run_update(&mut self, run: RunInfo) {
        if let Some(session) = &run.session_id {
            let read_only = run.log.state != LogState::Disabled;
            self.runs.input_policy.insert(session.clone(), read_only);
            if let Some(runtime) = &mut self.runtime {
                if runtime
                    .active
                    .as_ref()
                    .is_some_and(|active| &active.session_id == session)
                {
                    runtime.input_read_only = read_only;
                }
            }
        }
        if let Some(old) = self
            .runs
            .runs
            .iter_mut()
            .find(|old| old.run_id == run.run_id)
        {
            *old = run
        } else {
            self.runs.runs.push(run);
        }
    }
    fn run_attach_review(&mut self, session: &str) -> Result<()> {
        if self
            .runs
            .attach_inflight
            .as_ref()
            .is_some_and(|(id, epoch)| id == session && *epoch == self.runs.epoch)
        {
            return Ok(());
        }
        self.runtime.as_mut().context("No runtime")?.submit(
            Some(Request::Snapshot {
                session: session.into(),
                since: None,
            }),
            Tag::RunAttachInfo(self.runs.epoch),
        )?;
        self.runs.attach_inflight = Some((session.into(), self.runs.epoch));
        Ok(())
    }
    fn run_attach(&mut self, session: &str, takeover: bool) -> Result<()> {
        self.runtime.as_mut().context("No runtime")?.submit(
            Some(Request::Attach {
                session: session.into(),
                takeover,
            }),
            Tag::RunAttached(self.runs.epoch),
        )?;
        self.runs.attach_inflight = Some((session.into(), self.runs.epoch));
        Ok(())
    }
    pub(super) fn captured_run_input(&self) -> bool {
        !self.git.focused
            && self
                .runtime
                .as_ref()
                .is_some_and(|runtime| runtime.input_read_only)
    }
    pub(super) fn run_pending_count(&self) -> usize {
        self.runs.intents.len()
    }
    pub(super) fn current_run(&self) -> Option<&RunInfo> {
        self.runs
            .selected
            .as_ref()
            .and_then(|id| self.runs.runs.iter().find(|r| &r.run_id == id))
    }
    fn run_read_log(&mut self) -> Result<()> {
        let log = self.runs.log.as_ref().context("Choose a raw log first")?;
        self.run_submit(RunRequest::Log {
            run_id: log.run_id.clone(),
            generation: log.generation,
            offset: log.next,
            limit: 65536,
        })
    }
    fn run_open_log(&mut self, offset: u64) -> Result<()> {
        let run = self.current_run().context("Select a Run")?;
        ensure!(
            run.log.state != LogState::Disabled,
            "Logging was disabled for this Run"
        );
        self.runs.log = Some(LogView::new(run.run_id.clone(), run.log.generation, offset));
        self.runs.page = Page::Log;
        self.runs.epoch += 1;
        self.run_read_log()
    }
    pub(super) fn run_menu_key(&mut self, key: KeyEvent) -> bool {
        if key.code == KeyCode::F(9) {
            self.tab = 3;
            self.focus = Focus::Content;
            self.runs.page = Page::Runs;
            self.runs.all_projects = true;
            self.runs.epoch += 1;
            if let Err(error) = self.run_submit(RunRequest::List { project_id: None }) {
                self.error(error.to_string());
            }
            return true;
        }
        if matches!(key.code, KeyCode::Char('3') | KeyCode::Char('4')) {
            self.tab = if key.code == KeyCode::Char('3') { 2 } else { 3 };
            self.focus = Focus::Content;
            self.runs.epoch += 1;
            let request = if self.tab == 2 {
                self.current_id()
                    .map(|project_id| RunRequest::Tasks { project_id })
            } else {
                Some(RunRequest::List {
                    project_id: if self.runs.all_projects {
                        None
                    } else {
                        self.current_id()
                    },
                })
            };
            if let Some(request) = request {
                if let Err(error) = self.run_submit(request) {
                    self.error(error.to_string());
                }
            }
            return true;
        }
        if !matches!(self.tab, 2 | 3) || self.focus != Focus::Content {
            return false;
        }
        let result = (|| -> Result<bool> {
            match key.code {
                KeyCode::Char('n') | KeyCode::Char('e') if self.tab == 2 => {
                    self.run_task_form(key.code == KeyCode::Char('n'))?
                }
                KeyCode::Char('d') if self.tab == 2 => {
                    let root = &self
                        .current_project()
                        .context("Select a project")?
                        .project
                        .root;
                    let report =
                        crate::discovery::discover_tasks(root, self.environment.variables())?;
                    self.dialog = Some(Dialog::Run(Box::new(RunDialog::Discovery {
                        report,
                        selected: 0,
                    })));
                }
                KeyCode::Char('E') => {
                    let project = self
                        .current_project()
                        .context("Select a project")?
                        .project
                        .clone();
                    let key = (project.id.clone(), Some("@editor".into()));
                    let form = self.runs.drafts.get(&key).cloned().unwrap_or_else(|| {
                        let config = project.editor.or_else(|| {
                            crate::editor::discover_editor(
                                None,
                                self.environment.variables(),
                                &project.root,
                            )
                            .ok()
                            .flatten()
                            .filter(|c| c.available)
                            .map(|c| c.config)
                        });
                        RunForm::editor(project.id, self.view.revision, config)
                    });
                    self.dialog = Some(Dialog::Run(Box::new(RunDialog::Form(Box::new(form)))));
                }
                KeyCode::Enter if self.tab == 2 => {
                    let project_id = self.current_id().context("Select a project")?;
                    let task_id = self
                        .runs
                        .tasks
                        .get(&project_id)
                        .and_then(|(_, tasks)| tasks.get(self.runs.task_index))
                        .context("Select a saved task")?
                        .id
                        .clone();
                    self.run_submit(RunRequest::ReviewTask {
                        project_id,
                        task_id,
                        environment: self.environment.variables().clone(),
                    })?;
                }
                KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown => {
                    self.runs.approval_start = None;
                    let down = matches!(key.code, KeyCode::Down | KeyCode::PageDown);
                    let step = if matches!(key.code, KeyCode::PageUp | KeyCode::PageDown) {
                        8
                    } else {
                        1
                    };
                    let (index, count) = if self.tab == 2 {
                        let count = self
                            .current_id()
                            .and_then(|id| self.runs.tasks.get(&id))
                            .map_or(0, |(_, t)| t.len());
                        (&mut self.runs.task_index, count)
                    } else {
                        match self.runs.page {
                            Page::Runs => (&mut self.runs.run_index, self.runs.runs.len()),
                            Page::Problems => (
                                &mut self.runs.problem_index,
                                self.runs.problems.as_ref().map_or(0, |p| p.problems.len()),
                            ),
                            Page::Findings => (
                                &mut self.runs.finding_index,
                                self.runs.findings.as_ref().map_or(0, |p| p.findings.len()),
                            ),
                            Page::Search => (
                                &mut self.runs.match_index,
                                self.runs.search.as_ref().map_or(0, |s| s.matches.len()),
                            ),
                            _ => {
                                self.runs.scroll = if down {
                                    self.runs.scroll.saturating_add(step as u16)
                                } else {
                                    self.runs.scroll.saturating_sub(step as u16)
                                };
                                if let Some(log) = &mut self.runs.log {
                                    log.follow = false;
                                    log.scroll = self.runs.scroll;
                                }
                                return Ok(true);
                            }
                        }
                    };
                    *index = if down {
                        index.saturating_add(step).min(count.saturating_sub(1))
                    } else {
                        index.saturating_sub(step)
                    };
                }
                KeyCode::Enter if self.runs.page == Page::Runs => {
                    let run = self
                        .runs
                        .runs
                        .get(self.runs.run_index)
                        .context("Select a Run")?;
                    let id = run.run_id.clone();
                    self.runs.selected = Some(id.clone());
                    self.runs.epoch += 1;
                    self.runs.page = Page::Detail;
                    self.runs.scroll = 0;
                    self.run_submit(RunRequest::Info { run_id: id })?;
                }
                KeyCode::Enter if self.runs.page == Page::Problems => {
                    let problem = self
                        .runs
                        .problems
                        .as_ref()
                        .and_then(|p| p.problems.get(self.runs.problem_index))
                        .context("Select a Problem")?
                        .clone();
                    ensure!(
                        self.current_run()
                            .is_some_and(|run| run.log.generation == problem.log_generation),
                        "Problem belongs to an older log generation; refresh Problems"
                    );
                    if problem.file.is_some() {
                        self.run_submit(RunRequest::EditorReview {
                            run_id: problem.run_id,
                            problem_id: problem.id,
                            log_generation: problem.log_generation,
                        })?;
                    } else {
                        self.run_open_log(problem.log_offset)?;
                    }
                }
                KeyCode::Enter if self.runs.page == Page::Search => {
                    let offset = self
                        .runs
                        .search
                        .as_ref()
                        .and_then(|s| s.matches.get(self.runs.match_index))
                        .context("Select a match")?
                        .offset;
                    self.run_open_log(offset)?;
                }
                KeyCode::Enter if self.runs.page == Page::Findings => {
                    let finding = self
                        .runs
                        .findings
                        .as_ref()
                        .and_then(|p| p.findings.get(self.runs.finding_index))
                        .context("Select a finding")?;
                    ensure!(
                        finding.location.is_some(),
                        "This finding has no source location; inspect its message and raw result."
                    );
                    self.run_submit(RunRequest::FindingEditorReview {
                        run_id: self.runs.selected.clone().context("Select a Run")?,
                        body_sha256: finding.body_sha256.clone(),
                        index: finding.index,
                    })?;
                }
                KeyCode::Char('A') if self.tab == 3 => {
                    self.runs.advanced_details = !self.runs.advanced_details;
                }
                KeyCode::Char('s') if self.tab == 3 => {
                    let run_id = self
                        .runs
                        .selected
                        .clone()
                        .context("Open Run details first")?;
                    self.runs.epoch += 1;
                    self.runs.page = Page::Findings;
                    self.runs.findings = None;
                    self.runs.finding_index = 0;
                    self.runs.finding_history.clear();
                    self.runs.scroll = 0;
                    self.run_submit(RunRequest::Findings {
                        run_id,
                        offset: 0,
                        limit: 100,
                    })?;
                }
                KeyCode::Char('v') if self.tab == 3 => {
                    let run_id = self
                        .runs
                        .selected
                        .clone()
                        .context("Open Run details first")?;
                    self.runs.epoch += 1;
                    self.runs.page = Page::RawResult;
                    self.runs.result_raw = None;
                    self.runs.raw_history.clear();
                    self.runs.scroll = 0;
                    self.run_submit(RunRequest::ResultRaw {
                        run_id,
                        offset: 0,
                        limit: 65536,
                    })?;
                }
                KeyCode::Char(']') | KeyCode::Char('[')
                    if matches!(self.runs.page, Page::RawResult | Page::Findings) =>
                {
                    ensure!(
                        !self
                            .runs
                            .intents
                            .values()
                            .any(|intent| intent.epoch == self.runs.epoch
                                && intent.run == self.runs.selected
                                && matches!(
                                    intent.request,
                                    RunRequest::ResultRaw { .. } | RunRequest::Findings { .. }
                                )),
                        "Wait for the requested result page before moving again"
                    );
                    let next = key.code == KeyCode::Char(']');
                    let run_id = self.runs.selected.clone().context("Select a Run")?;
                    let request = if self.runs.page == Page::RawResult {
                        let page = self
                            .runs
                            .result_raw
                            .as_ref()
                            .context("Wait for the result page")?;
                        let offset = if next {
                            ensure!(!page.eof, "Last result page");
                            page.next_offset
                        } else {
                            *self.runs.raw_history.last().context("First result page")?
                        };
                        RunRequest::ResultRaw {
                            run_id,
                            offset,
                            limit: 65536,
                        }
                    } else {
                        let page = self
                            .runs
                            .findings
                            .as_ref()
                            .context("Wait for the finding page")?;
                        let offset = if next {
                            ensure!(page.more, "Last finding page");
                            page.next_offset
                        } else {
                            *self
                                .runs
                                .finding_history
                                .last()
                                .context("First finding page")?
                        };
                        self.runs.finding_index = 0;
                        RunRequest::Findings {
                            run_id,
                            offset,
                            limit: 100,
                        }
                    };
                    self.runs.scroll = 0;
                    self.run_submit(request)?;
                }
                KeyCode::Char('P') if self.tab == 3 => {
                    let run = self.current_run().context("Open Run details first")?;
                    self.run_submit(RunRequest::ResultPin {
                        run_id: run.run_id.clone(),
                        pinned: !run.pinned,
                    })?;
                }
                KeyCode::Char('b') if self.tab == 3 => {
                    let run_id = self
                        .current_run()
                        .context("Open Run details first")?
                        .run_id
                        .clone();
                    self.run_submit(RunRequest::ResultPin {
                        run_id: run_id.clone(),
                        pinned: true,
                    })?;
                    self.runs.baseline_pending = Some(run_id);
                    self.info("Pinning baseline; c compares another Run after confirmation.");
                }
                KeyCode::Char('c') if self.tab == 3 => {
                    let run_id = self
                        .runs
                        .selected
                        .clone()
                        .context("Open Run details first")?;
                    let baseline_run_id = self
                        .runs
                        .baseline
                        .clone()
                        .context("Open a baseline Run and press b first")?;
                    self.runs.page = Page::Comparison;
                    self.runs.comparison = None;
                    self.runs.scroll = 0;
                    self.run_submit(RunRequest::Compare {
                        baseline_run_id,
                        run_id,
                    })?;
                }
                KeyCode::Char('i') if self.tab == 3 => {
                    let run_id = self
                        .runs
                        .selected
                        .clone()
                        .context("Open Run details first")?;
                    self.runs.page = Page::Events;
                    self.runs.events = None;
                    self.runs.scroll = 0;
                    self.run_submit(RunRequest::Events { run_id })?;
                }
                KeyCode::Char('o') if self.tab == 3 => {
                    ensure!(self.runs.selected.is_some(), "Open Run details first");
                    self.dialog = Some(Dialog::Run(Box::new(RunDialog::Export(
                        CommitDraft::default(),
                    ))));
                }
                KeyCode::Char('l') if self.tab == 3 => self.run_open_log(0)?,
                KeyCode::Char('p') if self.tab == 3 && self.runs.selected.is_some() => {
                    let run_id = self.runs.selected.clone().unwrap();
                    self.runs.page = Page::Problems;
                    self.runs.problems = None;
                    self.runs.epoch += 1;
                    self.run_submit(RunRequest::Problems { run_id })?;
                }
                KeyCode::Char('/') if self.tab == 3 && self.runs.selected.is_some() => {
                    self.dialog = Some(Dialog::Run(Box::new(RunDialog::Search(
                        CommitDraft::default(),
                    ))))
                }
                KeyCode::Char('f') if self.runs.page == Page::Log => {
                    if let Some(log) = &mut self.runs.log {
                        log.follow = !log.follow;
                        if log.follow {
                            self.run_read_log()?;
                        }
                    }
                }
                KeyCode::Char('a') if self.tab == 3 => {
                    let id = self
                        .current_run()
                        .and_then(|r| r.session_id.clone())
                        .context("This Run has no available terminal")?;
                    self.run_attach_review(&id)?;
                }
                KeyCode::Char('k') | KeyCode::Char('K') if self.tab == 3 => {
                    let run = self.current_run().context("Select a Run")?;
                    ensure!(
                        run.state.is_live(),
                        "Run is not live; Unknown needs an explicit cleanup reconciliation"
                    );
                    let force = key.code == KeyCode::Char('K');
                    self.dialog=Some(Dialog::Run(Box::new(RunDialog::Confirm{title:if force{"Force Run cancellation"}else{"Cancel Run"}.into(),lines:vec![format!("Run {} · {} · {:?}",run.run_id,run.name,run.state),"Only the owned Run process group is targeted. Cleanup may remain Unknown.".into()],request:Box::new(RunRequest::Cancel{run_id:run.run_id.clone(),force}),scroll:0})));
                }
                KeyCode::Char('u') if self.tab == 3 => {
                    let run = self.current_run().context("Select a Run")?;
                    ensure!(
                        run.state == RunState::Unknown,
                        "Only Unknown Runs need reconciliation"
                    );
                    self.dialog=Some(Dialog::Run(Box::new(RunDialog::Confirm{title:"Acknowledge independently verified cleanup".into(),lines:vec![format!("Run {} · {}",run.run_id,run.name),format!("Source roots: {:?}",run.source_roots),"Confirm only after independently verifying the old process tree is gone. This does not kill a process or replay commands. Result remains Unknown; this Run's source blocker is released.".into()],request:Box::new(RunRequest::Reconcile{run_id:run.run_id.clone()}),scroll:0})));
                }
                KeyCode::Char('z') if self.tab == 3 => {
                    self.runs.epoch += 1;
                    self.runs.page = Page::Runs;
                    self.runs.selected = None;
                    self.run_submit(RunRequest::List {
                        project_id: if self.runs.all_projects {
                            None
                        } else {
                            self.current_id()
                        },
                    })?;
                }
                KeyCode::Char('r') if self.tab == 3 => {
                    let run = self.current_run().context("Select a recorded Run")?;
                    self.run_submit(RunRequest::ReviewTask {
                        project_id: run.project_id.clone(),
                        task_id: run.task_id.clone(),
                        environment: self.environment.variables().clone(),
                    })?;
                }
                KeyCode::Char('x') => {
                    self.runs.epoch += 1;
                    self.runs.pending_attach = None;
                    for intent in self.runs.intents.values_mut() {
                        if matches!(
                            intent.request,
                            RunRequest::Tasks { .. }
                                | RunRequest::List { .. }
                                | RunRequest::Info { .. }
                                | RunRequest::Log { .. }
                                | RunRequest::Search { .. }
                                | RunRequest::Problems { .. }
                                | RunRequest::ReviewTask { .. }
                                | RunRequest::EditorReview { .. }
                                | RunRequest::FindingEditorReview { .. }
                                | RunRequest::ResultRaw { .. }
                                | RunRequest::Findings { .. }
                                | RunRequest::Compare { .. }
                                | RunRequest::Events { .. }
                        ) {
                            intent.cancelled = true;
                        }
                    }
                    let jobs = self
                        .runs
                        .intents
                        .values()
                        .filter(|intent| {
                            matches!(
                                intent.request,
                                RunRequest::Tasks { .. }
                                    | RunRequest::List { .. }
                                    | RunRequest::Info { .. }
                                    | RunRequest::Log { .. }
                                    | RunRequest::Search { .. }
                                    | RunRequest::Problems { .. }
                                    | RunRequest::ReviewTask { .. }
                                    | RunRequest::EditorReview { .. }
                                    | RunRequest::FindingEditorReview { .. }
                                    | RunRequest::ResultRaw { .. }
                                    | RunRequest::Findings { .. }
                                    | RunRequest::Compare { .. }
                                    | RunRequest::Events { .. }
                            )
                        })
                        .filter_map(|intent| intent.job.clone())
                        .collect::<Vec<_>>();
                    for job_id in jobs {
                        self.run_submit(RunRequest::CancelJob { job_id })?;
                    }
                    if let Some(log) = &mut self.runs.log {
                        log.follow = false;
                    }
                    self.info("Cancellation requested for pending reads; task and editor launches are unchanged.");
                }
                KeyCode::F(5) => {
                    self.runtime.as_mut().context("No runtime")?.connect()?;
                    self.runs.last_poll = Instant::now() - Duration::from_secs(3);
                }
                _ => return Ok(false),
            }
            Ok(true)
        })();
        match result {
            Ok(handled) => handled,
            Err(error) => {
                self.error(error.to_string());
                true
            }
        }
    }
    fn run_task_form(&mut self, new: bool) -> Result<()> {
        let project = self
            .current_project()
            .context("Select a project")?
            .project
            .clone();
        let selected = if new {
            None
        } else {
            Some(
                self.runs
                    .tasks
                    .get(&project.id)
                    .and_then(|(_, tasks)| tasks.get(self.runs.task_index))
                    .context("Select a task")?
                    .clone(),
            )
        };
        let key = (project.id.clone(), selected.as_ref().map(|t| t.id.clone()));
        let task = selected.unwrap_or(TaskDefinition {
            execution: Some(crate::model::TaskExecution::Process {
                program: std::path::PathBuf::new(),
                args: vec![],
                env: Default::default(),
            }),
            id: uuid::Uuid::new_v4().to_string(),
            name: "New task".into(),
            command: String::new(),
            cwd: project.root.clone(),
            sources: vec![],
            artifact: None,
            approved_digest: None,
            steps: vec![],
            failure_policy: FailurePolicy::Stop,
            logging: TaskLogging::Raw,
            interactive: false,
            build_outputs: vec![],
            artifact_from_task: None,
            timeout_seconds: None,
        });
        let form = self
            .runs
            .drafts
            .get(&key)
            .cloned()
            .unwrap_or_else(|| RunForm::task(project.id, self.view.revision, task));
        self.dialog = Some(Dialog::Run(Box::new(RunDialog::Form(Box::new(form)))));
        Ok(())
    }
    fn keep_run_form(&mut self, form: RunForm) {
        let key = (
            form.project.clone(),
            form.task
                .as_ref()
                .map(|t| {
                    if self
                        .runs
                        .tasks
                        .get(&form.project)
                        .is_some_and(|(_, tasks)| tasks.iter().any(|saved| saved.id == t.id))
                    {
                        t.id.clone()
                    } else {
                        String::new()
                    }
                })
                .or_else(|| Some("@editor".into()))
                .filter(|id| !id.is_empty()),
        );
        self.runs.drafts.insert(key, form);
    }
    pub(super) fn open_configuration(&mut self) -> Result<()> {
        let root = self
            .current_project()
            .context("Select a project")?
            .project
            .root
            .clone();
        let snapshot = crate::portable::read(&root)?;
        let (text, expected) = snapshot
            .map(|value| (value.raw, Some(value.digest)))
            .unwrap_or(("schema_version = 1\n".into(), None));
        self.dialog = Some(Dialog::Run(Box::new(RunDialog::Configuration {
            root,
            expected,
            draft: CommitDraft {
                cursor: text.len(),
                text,
            },
        })));
        Ok(())
    }
    pub(super) fn run_paste(&mut self, text: &str) -> Result<()> {
        if let Some(Dialog::Run(dialog)) = &mut self.dialog {
            match dialog.as_mut() {
                RunDialog::Form(form) => form.fields[form.selected]
                    .value
                    .insert(text)
                    .map_err(anyhow::Error::msg)?,
                RunDialog::Search(input)
                | RunDialog::Export(input)
                | RunDialog::Configuration { draft: input, .. } => {
                    input.insert(text).map_err(anyhow::Error::msg)?
                }
                _ => {}
            }
        }
        Ok(())
    }
    pub(super) fn handle_run_dialog(&mut self, mut dialog: RunDialog, key: KeyEvent) -> Result<()> {
        if key.code == KeyCode::Esc {
            self.runs.approval_start = None;
            self.runs.attach_inflight = None;
            match dialog {
                RunDialog::Form(form) => self.keep_run_form(*form),
                RunDialog::Configuration { draft, .. } => {
                    let path = self
                        .service
                        .store
                        .config_dir
                        .join(format!("declaration-draft-{}.toml", crate::model::new_id()));
                    crate::store::atomic_write(&path, draft.text.as_bytes())?;
                    self.info(format!("Declaration draft kept at {}", path.display()));
                }
                _ => {}
            }
            self.runs.epoch += 1;
            return Ok(());
        }
        let result = (|| -> Result<bool> {
            match &mut dialog {
                RunDialog::Form(form) => match key.code {
                    KeyCode::F(9) => form.fields[form.selected].value = CommitDraft::default(),
                    KeyCode::F(11) if form.task.is_some() => form.toggle_arguments()?,
                    KeyCode::Tab => form.move_field(false),
                    KeyCode::BackTab => form.move_field(true),
                    KeyCode::F(10) => {
                        form.advanced = !form.advanced;
                        form.selected = 0;
                    }
                    KeyCode::F(6) if form.task.is_some() => {
                        ensure!(form.task.as_ref().is_some_and(|task| task.execution.is_none()), "Sources belong to legacy csh tasks; use an explicit script for native initialization");
                        ensure!(
                            form.source_count < 32,
                            "A task supports at most 32 source scripts"
                        );
                        form.advanced = true;
                        form.add_source(SourceSpec {
                            path: PathBuf::new(),
                            args: vec![],
                        });
                    }
                    KeyCode::F(7) if form.task.is_some() => {
                        ensure!(form.task.as_ref().is_some_and(|task| task.execution.is_none()), "Same-shell steps belong to legacy csh tasks; use one explicit script for shared state");
                        ensure!(form.step_count < 64, "A task supports at most 64 steps");
                        form.advanced = true;
                        form.add_step(TaskStep {
                            name: format!("Step {}", form.step_count + 1),
                            command: String::new(),
                        });
                    }
                    KeyCode::F(8) if form.task.is_some() => {
                        form.cycle_execution()?;
                    }
                    KeyCode::F(4) => {
                        form.revision = self.service.store.load()?.revision;
                        self.info("Draft kept and revision refreshed. Saving will replace this task's current definition.");
                    }
                    KeyCode::F(2) => {
                        let request = if form.task.is_some() {
                            RunRequest::SaveTask {
                                revision: form.revision,
                                project_id: form.project.clone(),
                                task: Box::new(form.build_task()?),
                            }
                        } else {
                            RunRequest::SaveEditor {
                                revision: form.revision,
                                project_id: form.project.clone(),
                                config: form.build_editor()?,
                            }
                        };
                        self.keep_run_form(*form.clone());
                        self.run_submit(request)?;
                        return Ok(false);
                    }
                    _ => form.fields[form.selected]
                        .value
                        .key(key)
                        .map_err(anyhow::Error::msg)?,
                },
                RunDialog::Configuration {
                    root,
                    expected,
                    draft,
                } => {
                    if key.code == KeyCode::F(2) {
                        let digest = crate::portable::write_declaration(
                            self.service.store,
                            &root.join(crate::portable::DECLARATION),
                            expected.as_deref(),
                            &draft.text,
                        )?;
                        // A saved file may still fail to resolve (e.g. missing executable). Keep its new CAS identity and the draft.
                        *expected = Some(digest);
                        let id = self.current_id().context("Select a project")?;
                        let resolved = crate::portable::resolve_for(
                            self.service.store,
                            root,
                            &self.environment,
                            Some(&id),
                        )?;
                        crate::portable::apply(self.service.store, resolved)?;
                        self.reload(Some(&id))?;
                        self.info("Project declaration saved and synchronized. Changed execution requires review.");
                        return Ok(false);
                    }
                    draft.key(key).map_err(anyhow::Error::msg)?;
                }
                RunDialog::Discovery { report, selected } => match key.code {
                    KeyCode::Up => *selected = selected.saturating_sub(1),
                    KeyCode::Down => {
                        *selected = (*selected + 1).min(report.candidates.len().saturating_sub(1))
                    }
                    KeyCode::F(2) => {
                        let candidate = report
                            .candidates
                            .get(*selected)
                            .context("No discovered tasks; n creates a command draft")?;
                        ensure!(!self.service.store.load()?.project(&self.current_id().context("Select a project")?)?.tasks.iter().any(|task|task.name==candidate.name),"A task with this name is already registered; edit or select the existing task.");
                        let task = candidate.task_definition()?;
                        self.run_submit(RunRequest::SaveTask {
                            revision: self.view.revision,
                            project_id: self.current_id().context("Select a project")?,
                            task: Box::new(task),
                        })?;
                        return Ok(false);
                    }
                    _ => {}
                },
                RunDialog::Export(input) => {
                    if matches!(key.code, KeyCode::F(2) | KeyCode::Enter) {
                        let mut path = PathBuf::from(&input.text);
                        if !path.is_absolute() {
                            path = std::env::current_dir()?.join(path);
                        }
                        ensure!(!input.text.is_empty(), "Enter a new export file path");
                        self.run_submit(RunRequest::ResultExport {
                            run_id: self.runs.selected.clone().context("Select a Run")?,
                            path,
                        })?;
                        return Ok(false);
                    }
                    input.key(key).map_err(anyhow::Error::msg)?;
                }
                RunDialog::TaskReview {
                    review,
                    parallel,
                    scroll,
                } => match key.code {
                    KeyCode::F(3) => *parallel = !*parallel,
                    KeyCode::F(2) => {
                        ensure!(
                            review.common_approved,
                            "Project initialization needs approval first: close this review and use t on the project"
                        );
                        let (rows, cols) = self.runtime.as_ref().unwrap().dimensions;
                        let start = RunRequest::Start {
                            project_id: review.project_id.clone(),
                            task_id: review.task.id.clone(),
                            operation_id: uuid::Uuid::new_v4().to_string(),
                            environment: self.environment.variables().clone(),
                            expected_digest: Some(review.digest.clone()),
                            parallel: *parallel,
                            rows,
                            cols,
                        };
                        let request = if review.approved {
                            start
                        } else {
                            self.runs.approval_start = Some(start);
                            RunRequest::ApproveTask {
                                revision: review.revision,
                                project_id: review.project_id.clone(),
                                task_id: review.task.id.clone(),
                                digest: review.digest.clone(),
                                environment: self.environment.variables().clone(),
                            }
                        };
                        self.run_submit(request)?;
                        return Ok(false);
                    }
                    _ => scroll_key(scroll, key),
                },
                RunDialog::Confirm {
                    request, scroll, ..
                } => {
                    if key.code == KeyCode::F(2) {
                        self.run_submit(*request.clone())?;
                        return Ok(false);
                    } else {
                        scroll_key(scroll, key)
                    }
                }
                RunDialog::Editor {
                    review_id, scroll, ..
                } => {
                    if key.code == KeyCode::F(2) {
                        let (rows, cols) = self.runtime.as_ref().unwrap().dimensions;
                        self.run_submit(RunRequest::EditorOpen {
                            review_id: review_id.clone(),
                            environment: self.environment.variables().clone(),
                            rows,
                            cols,
                        })?;
                        return Ok(false);
                    } else {
                        scroll_key(scroll, key)
                    }
                }
                RunDialog::Search(input) => {
                    if matches!(key.code, KeyCode::F(2) | KeyCode::Enter) {
                        let run = self.current_run().context("Select a Run")?;
                        let request = RunRequest::Search {
                            run_id: run.run_id.clone(),
                            generation: run.log.generation,
                            query: input.text.clone(),
                        };
                        self.runs.page = Page::Search;
                        self.runs.search = None;
                        self.runs.match_index = 0;
                        self.runs.epoch += 1;
                        self.run_submit(request)?;
                        return Ok(false);
                    } else {
                        input.key(key).map_err(anyhow::Error::msg)?;
                    }
                }
                RunDialog::Attach { session, .. } => {
                    if key.code == KeyCode::F(2) {
                        self.run_attach(session, true)?;
                        return Ok(false);
                    }
                }
            }
            Ok(true)
        })();
        match result {
            Ok(false) => {}
            Ok(true) => self.dialog = Some(Dialog::Run(Box::new(dialog))),
            Err(error) => {
                self.dialog = Some(Dialog::Run(Box::new(dialog)));
                return Err(error);
            }
        }
        Ok(())
    }
}
fn scroll_key(scroll: &mut u16, key: KeyEvent) {
    match key.code {
        KeyCode::Up => *scroll = scroll.saturating_sub(1),
        KeyCode::Down => *scroll = scroll.saturating_add(1),
        KeyCode::PageUp => *scroll = scroll.saturating_sub(10),
        KeyCode::PageDown => *scroll = scroll.saturating_add(10),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    #[test]
    fn older_task_list_cannot_undo_a_newly_saved_selection() {
        let temporary = tempfile::tempdir().unwrap();
        let store = Store::open(Some(temporary.path())).unwrap();
        let mut app = App::load(&store).unwrap();
        let task = |id: &str| {
            serde_json::from_value::<TaskDefinition>(
                serde_json::json!({"id":id,"name":id,"command":"echo reviewed","cwd":"/"}),
            )
            .unwrap()
        };
        app.tab = 2;
        app.runs.task_index = 1;
        app.runs
            .tasks
            .insert("project".into(), (5, vec![task("old"), task("new")]));
        app.runs.intents.insert(
            1,
            Intent {
                request: RunRequest::Tasks {
                    project_id: "project".into(),
                },
                project: Some("project".into()),
                run: None,
                epoch: app.runs.epoch,
                tab: 2,
                job: None,
                polling: false,
                cancelled: false,
            },
        );
        let job = RunJob {
            job_id: "old-read".into(),
            state: RunJobState::Complete,
            result: Some(RunResult::Tasks {
                revision: 4,
                tasks: vec![task("old")],
            }),
            error: None,
            session_id: None,
        };
        app.accept_run_reply(Tag::RunSubmit(1), serde_json::to_value(job).unwrap())
            .unwrap();
        assert_eq!(app.runs.tasks["project"].0, 5);
        assert_eq!(app.runs.tasks["project"].1.len(), 2);
        assert_eq!(app.runs.tasks["project"].1[app.runs.task_index].id, "new");
    }
    #[test]
    fn late_problem_reply_does_not_replace_a_changed_run_or_pane() {
        let temporary = tempfile::tempdir().unwrap();
        let store = Store::open(Some(temporary.path())).unwrap();
        let mut app = App::load(&store).unwrap();
        app.tab = 3;
        app.runs.selected = Some("new-run".into());
        app.runs.page = Page::Problems;
        let result = RunResult::Problems(crate::problems::ProblemSet {
            run_id: "old-run".into(),
            project_id: "project".into(),
            definition_revision: 1,
            source_generation: None,
            source_changed: None,
            log_generation: 1,
            parsed_bytes: 0,
            problems: vec![],
            limited: false,
            partial: false,
            control_sequences_removed: false,
        });
        for (nonce, run, tab) in [(1, "old-run", 3), (2, "new-run", 2)] {
            app.runs.intents.insert(
                nonce,
                Intent {
                    request: RunRequest::Problems { run_id: run.into() },
                    project: None,
                    run: Some(run.into()),
                    epoch: app.runs.epoch,
                    tab,
                    job: None,
                    polling: false,
                    cancelled: false,
                },
            );
            app.accept_run_reply(
                Tag::RunSubmit(nonce),
                serde_json::to_value(RunJob {
                    job_id: format!("job-{nonce}"),
                    state: RunJobState::Complete,
                    result: Some(result.clone()),
                    error: None,
                    session_id: None,
                })
                .unwrap(),
            )
            .unwrap();
            assert!(app.runs.problems.is_none());
            assert_eq!(app.runs.selected.as_deref(), Some("new-run"));
        }
    }
}
