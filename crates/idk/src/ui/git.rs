//! Git UI actions bind to project repository identities, never to terminal cwd.
use super::{forms::TextInput, git_draft::CommitDraft, runtime::Tag, App, Dialog, Focus};
use crate::{git_wire::*, protocol::Request};
use anyhow::{ensure, Context, Result};
use crossterm::event::{KeyCode, KeyEvent};

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct Target {
    pub project: String,
    pub root: PathBuf,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Page {
    Changes,
    Diff,
    History,
    Files,
    Branches,
    Remotes,
    Operations,
}
#[derive(Clone, Debug)]
enum Purpose {
    Open,
    Status,
    Diff,
    History,
    Files,
    Branches,
    Remotes,
    CommitReview(String),
    ReviewPlan,
    ExecutePlan,
}
impl Purpose {
    fn page(&self) -> Option<Page> {
        match self {
            Self::Diff => Some(Page::Diff),
            Self::History => Some(Page::History),
            Self::Files => Some(Page::Files),
            Self::Branches => Some(Page::Branches),
            Self::Remotes => Some(Page::Remotes),
            _ => None,
        }
    }
    fn topic(&self) -> Option<u8> {
        match self {
            Self::Open => Some(0),
            Self::Status => Some(1),
            Self::Diff => Some(2),
            Self::History => Some(3),
            Self::Files => Some(4),
            Self::Branches => Some(5),
            Self::Remotes => Some(6),
            _ => None,
        }
    }
}
#[derive(Clone, Debug)]
struct Intent {
    target: Target,
    purpose: Purpose,
    nonce: u64,
    context: Option<String>,
}
#[derive(Clone, Debug)]
pub(super) enum GitDialog {
    Reconcile {
        operation: Box<GitOperationInfo>,
    },
    Operations {
        entries: Vec<GitOperationInfo>,
        selected: usize,
    },
    Repositories {
        project: String,
        roots: Vec<PathBuf>,
        selected: usize,
    },
    Message {
        target: Target,
    },
    Waiting {
        target: Target,
        nonce: u64,
        title: String,
    },
    CommitReview {
        target: Target,
        review: Box<GitCommitReview>,
        message: String,
        scroll: usize,
        diff: bool,
    },
    Plan {
        target: Target,
        preview: Box<GitOperationPreview>,
        scroll: usize,
    },
    Branch {
        target: Target,
        input: TextInput,
    },
    Remote {
        target: Target,
        remote: String,
        push: bool,
        input: TextInput,
    },
    Takeover {
        operation: String,
        name: String,
    },
}
pub(super) struct RepoView {
    pub context: Option<GitContextInfo>,
    pub snapshot: Option<GitStatusReply>,
    pub page: Page,
    pub selected: usize,
    pub marked: BTreeSet<usize>,
    pub diff: Option<DiffView>,
    pub diff_target: DiffTarget,
    pub history: Vec<CommitSummary>,
    pub files: Vec<GitChange>,
    pub branches: Vec<Branch>,
    pub remotes: Vec<Remote>,
    pub draft: CommitDraft,
    pub scroll: u16,
    pub busy: usize,
    pub error: Option<String>,
    latest: HashMap<u8, u64>,
    refreshed: Instant,
}
impl Default for RepoView {
    fn default() -> Self {
        Self {
            context: None,
            snapshot: None,
            page: Page::Changes,
            selected: 0,
            marked: BTreeSet::new(),
            diff: None,
            diff_target: DiffTarget::Worktree,
            history: Vec::new(),
            files: Vec::new(),
            branches: Vec::new(),
            remotes: Vec::new(),
            draft: CommitDraft::default(),
            scroll: 0,
            busy: 0,
            error: None,
            latest: HashMap::new(),
            refreshed: Instant::now(),
        }
    }
}
pub(super) struct GitUi {
    pub roots: HashMap<String, PathBuf>,
    pub repos: HashMap<Target, RepoView>,
    submitted: HashMap<u64, Intent>,
    jobs: HashMap<String, Intent>,
    polling: HashSet<String>,
    cancelled: HashSet<u64>,
    next: u64,
    last_poll: Instant,
    pub operations: Vec<GitOperationInfo>,
    pub operation: Option<GitOperationInfo>,
    pub screen: Option<crate::terminal::TerminalSnapshot>,
    pub focused: bool,
    snapshot_pending: bool,
    snapshot_finished: bool,
    operations_pending: bool,
    last_operation_list: Instant,
    resized: Option<(String, u64, u16, u16)>,
    executing: HashMap<String, Target>,
    submitted_messages: HashMap<String, (Target, String)>,
    reconnect: Option<Target>,
}
impl Default for GitUi {
    fn default() -> Self {
        Self {
            roots: HashMap::new(),
            repos: HashMap::new(),
            submitted: HashMap::new(),
            jobs: HashMap::new(),
            polling: HashSet::new(),
            cancelled: HashSet::new(),
            next: 0,
            last_poll: Instant::now(),
            operations: Vec::new(),
            operation: None,
            screen: None,
            focused: false,
            snapshot_pending: false,
            snapshot_finished: false,
            operations_pending: false,
            last_operation_list: Instant::now() - Duration::from_secs(2),
            resized: None,
            executing: HashMap::new(),
            submitted_messages: HashMap::new(),
            reconnect: None,
        }
    }
}

impl App<'_> {
    pub(super) fn git_target(&self) -> Option<Target> {
        let project = &self.current_project()?.project;
        let root = self
            .git
            .roots
            .get(&project.id)
            .filter(|root| {
                project.repository.as_ref() == Some(root)
                    || project
                        .related_repositories
                        .iter()
                        .any(|related| &related.root == *root)
            })
            .cloned()
            .or_else(|| project.repository.clone())?;
        Some(Target {
            project: project.id.clone(),
            root,
        })
    }
    fn git_request(&mut self, target: Target, task: GitTask, purpose: Purpose) -> Result<u64> {
        ensure!(
            self.git.submitted.len() + self.git.jobs.len() < 16,
            "Git is busy; wait for current requests to finish."
        );
        self.git.next = self
            .git
            .next
            .checked_add(1)
            .context("Git request counter exhausted")?;
        let nonce = self.git.next;
        let context = task.context().map(str::to_owned);
        self.runtime
            .as_mut()
            .context("Git needs the workspace host.")?
            .submit(Some(Request::GitSubmit { task }), Tag::GitSubmit(nonce))?;
        let repo = self.git.repos.entry(target.clone()).or_default();
        if let Some(page) = purpose.page() {
            if repo.page != page {
                repo.selected = 0;
                repo.scroll = 0;
            }
            repo.page = page;
        }
        repo.busy += 1;
        if let Some(topic) = purpose.topic() {
            repo.latest.insert(topic, nonce);
        }

        self.git.submitted.insert(
            nonce,
            Intent {
                target,
                purpose,
                nonce,
                context,
            },
        );
        Ok(nonce)
    }
    fn git_context(&self, target: &Target) -> Result<String> {
        self.git
            .repos
            .get(target)
            .and_then(|repo| repo.context.as_ref())
            .map(|context| context.id.clone())
            .context("Repository is not ready. Use F5 to refresh.")
    }
    fn git_snapshot_id(&self, target: &Target) -> Result<String> {
        self.git
            .repos
            .get(target)
            .and_then(|repo| repo.snapshot.as_ref())
            .map(|snapshot| snapshot.snapshot_id.clone())
            .context("Read repository status before this action.")
    }
    fn open_git(&mut self, target: Target) -> Result<()> {
        let primary = self.project(&target.project)?.repository.as_ref() == Some(&target.root);
        self.git_request(
            target.clone(),
            GitTask::Open {
                project: target.project.clone(),
                repository: if primary {
                    None
                } else {
                    Some(target.root.clone())
                },
                env: self.environment.variables().clone(),
            },
            Purpose::Open,
        )?;
        Ok(())
    }
    fn refresh_git(&mut self, target: Target) -> Result<()> {
        if let Ok(context) = self.git_context(&target) {
            self.git_request(
                target,
                GitTask::Status {
                    context,
                    refresh: true,
                },
                Purpose::Status,
            )?;
            Ok(())
        } else {
            self.open_git(target)
        }
    }
    pub(super) fn tick_git(&mut self) {
        if self.runtime.is_none() {
            return;
        }
        if self.tab == 1 && self.focus == Focus::Content {
            if let Some(target) = self.git_target() {
                let repo = self.git.repos.entry(target.clone()).or_default();
                if repo.context.is_none() && repo.busy == 0 && repo.error.is_none() {
                    if let Err(error) = self.open_git(target) {
                        self.error(error.to_string());
                    }
                } else if repo.context.is_some()
                    && repo.busy == 0
                    && repo.error.is_none()
                    && repo.page == Page::Changes
                    && repo.refreshed.elapsed() >= Duration::from_secs(2)
                    && self.dialog.is_none()
                {
                    if let Err(error) = self.refresh_git(target) {
                        self.error(error.to_string());
                    }
                }
            }
        }
        if !self.runtime.as_ref().unwrap().online {
            return;
        }
        if self.git.last_poll.elapsed() < Duration::from_millis(100) {
            return;
        }
        self.git.last_poll = Instant::now();
        let jobs: Vec<_> = self
            .git
            .jobs
            .keys()
            .filter(|id| !self.git.polling.contains(*id))
            .take(3)
            .cloned()
            .collect();
        for id in jobs {
            if self
                .runtime
                .as_mut()
                .unwrap()
                .submit(
                    Some(Request::GitJob { job: id.clone() }),
                    Tag::GitJob(id.clone()),
                )
                .is_ok()
            {
                self.git.polling.insert(id);
            }
        }
        if let Some(operation) = &self.git.operation {
            if !self.git.snapshot_pending
                && !self.git.snapshot_finished
                && self
                    .runtime
                    .as_ref()
                    .and_then(|runtime| runtime.host.as_ref())
                    .is_some_and(|host| host.host_instance == operation.host_instance)
            {
                let id = operation.id.clone();
                let since = self.git.screen.as_ref().map(|screen| screen.generation);
                if self
                    .runtime
                    .as_mut()
                    .unwrap()
                    .submit(
                        Some(Request::GitOperationSnapshot {
                            operation: id.clone(),
                            since,
                        }),
                        Tag::GitSnapshot(id),
                    )
                    .is_ok()
                {
                    self.git.snapshot_pending = true;
                }
            }
        }
        if self.git.focused && self.git_writable() {
            let operation = self.git.operation.as_ref().unwrap();
            let (rows, cols) = self.runtime.as_ref().unwrap().dimensions;
            let target = (operation.id.clone(), operation.input_epoch, rows, cols);
            if self.git.resized.as_ref() != Some(&target)
                && self
                    .runtime
                    .as_mut()
                    .unwrap()
                    .submit(
                        Some(Request::GitOperationResize {
                            operation: target.0.clone(),
                            epoch: target.1,
                            rows,
                            cols,
                        }),
                        Tag::Ack,
                    )
                    .is_ok()
            {
                self.git.resized = Some(target);
            }
        }
        if (!self.git.operations.is_empty() || !self.git.executing.is_empty() || self.tab == 1)
            && self.git.last_operation_list.elapsed() >= Duration::from_secs(1)
            && !self.git.operations_pending
            && self
                .runtime
                .as_mut()
                .unwrap()
                .submit(
                    Some(Request::GitOperations { project: None }),
                    Tag::GitOperations,
                )
                .is_ok()
        {
            self.git.operations_pending = true;
            self.git.last_operation_list = Instant::now();
        }
    }
    pub(super) fn git_rpc_error(&mut self, tag: &Tag, message: &str) {
        let intent = match tag {
            Tag::GitSubmit(nonce) => self.git.submitted.remove(nonce),
            Tag::GitJob(id) => {
                self.git.polling.remove(id);
                self.git.jobs.remove(id)
            }
            Tag::GitSnapshot(_) => {
                self.git.snapshot_pending = false;
                None
            }
            Tag::GitOperations => {
                self.git.operations_pending = false;
                None
            }
            _ => None,
        };
        if let Some(intent) = intent {
            self.finish_git_error(intent, message);
        }
    }
    fn finish_git_error(&mut self, intent: Intent, message: &str) {
        self.git.cancelled.remove(&intent.nonce);
        let repo = self.git.repos.entry(intent.target.clone()).or_default();
        repo.busy = repo.busy.saturating_sub(1);
        repo.error = Some(message.into());
        if matches!(self.dialog.as_ref(),Some(Dialog::Git(dialog)) if matches!(dialog.as_ref(),GitDialog::Waiting{nonce,..} if *nonce==intent.nonce))
        {
            self.dialog = if matches!(intent.purpose, Purpose::CommitReview(_)) {
                Some(Dialog::Git(Box::new(GitDialog::Message {
                    target: intent.target,
                })))
            } else {
                None
            };
        }
        self.error(message);
    }
    pub(super) fn accept_git_reply(&mut self, tag: Tag, value: serde_json::Value) -> Result<()> {
        match tag {
            Tag::GitSubmit(nonce) => {
                let job: GitJobInfo = serde_json::from_value(value)?;
                if let Some(intent) = self.git.submitted.remove(&nonce) {
                    self.receive_git_job(job, intent)?;
                }
            }
            Tag::GitJob(id) => {
                self.git.polling.remove(&id);
                let job: GitJobInfo = serde_json::from_value(value)?;
                if let Some(intent) = self.git.jobs.remove(&id) {
                    self.receive_git_job(job, intent)?;
                }
            }
            Tag::GitExecute(plan) => {
                let operation: GitOperationInfo = serde_json::from_value(value)?;
                self.git.executing.remove(&plan);
                self.git.operation = Some(operation.clone());
                self.git_attach(&operation.id, false)?;
            }
            Tag::GitAttached => {
                let operation: GitOperationInfo = serde_json::from_value(value)?;
                self.git.operation = Some(operation);
                self.git.screen = None;
                self.git.resized = None;
                self.git.snapshot_finished = false;
                self.git.focused = true;
                self.set_terminal_connected(true);
            }
            Tag::GitSnapshot(id) => {
                self.git.snapshot_pending = false;
                let reply: GitOperationSnapshot = serde_json::from_value(value)?;
                if self
                    .git
                    .operation
                    .as_ref()
                    .is_some_and(|operation| operation.id == id)
                {
                    self.git.snapshot_finished = matches!(
                        reply.operation.state,
                        GitOperationState::Complete | GitOperationState::Unknown
                    );
                    self.note_git_result(&reply.operation);
                    self.git.operation = Some(reply.operation);
                    if let Some(screen) = reply.screen {
                        super::screen::validate(&screen)?;
                        self.git.screen = Some(screen);
                    }
                }
            }
            Tag::GitUpdated => {
                let operation: GitOperationInfo = serde_json::from_value(value)?;
                self.note_git_result(&operation);
                if self
                    .git
                    .operation
                    .as_ref()
                    .is_some_and(|active| active.id == operation.id)
                {
                    self.git.operation = Some(operation);
                }
            }
            Tag::GitOperations => {
                self.git.operations_pending = false;
                let operations: Vec<GitOperationInfo> = serde_json::from_value(value)?;
                for operation in &operations {
                    self.note_git_result(operation);
                }
                if let Some(Dialog::Git(dialog)) = &mut self.dialog {
                    if let GitDialog::Operations { entries, selected } = dialog.as_mut() {
                        let previous = entries.get(*selected).map(|operation| operation.id.clone());
                        *entries = operations.clone();
                        *selected = previous
                            .and_then(|id| entries.iter().position(|operation| operation.id == id))
                            .unwrap_or(0);
                    }
                }
                self.git.operations = operations;
            }
            _ => {}
        }
        Ok(())
    }
    fn receive_git_job(&mut self, job: GitJobInfo, intent: Intent) -> Result<()> {
        if matches!(job.state, GitJobState::Pending | GitJobState::Running) {
            self.git.jobs.insert(job.id, intent);
            return Ok(());
        }
        let stale = self.git.repos.get(&intent.target).is_none_or(|repo| {
            intent
                .purpose
                .topic()
                .is_some_and(|topic| repo.latest.get(&topic) != Some(&intent.nonce))
                || intent.context.as_ref().is_some_and(|context| {
                    repo.context
                        .as_ref()
                        .is_none_or(|current| &current.id != context)
                })
        });
        if stale {
            if let Some(repo) = self.git.repos.get_mut(&intent.target) {
                repo.busy = repo.busy.saturating_sub(1);
            }
            self.git.cancelled.remove(&intent.nonce);
            return Ok(());
        }
        if job.state == GitJobState::Failed {
            self.finish_git_error(
                intent,
                job.error
                    .as_deref()
                    .unwrap_or("Git request failed; refresh before retrying."),
            );
            return Ok(());
        }
        self.git
            .repos
            .entry(intent.target.clone())
            .or_default()
            .busy = self.git.repos[&intent.target].busy.saturating_sub(1);
        if self.git.cancelled.remove(&intent.nonce) {
            return Ok(());
        }
        let target = intent.target.clone();
        self.git.repos.get_mut(&target).unwrap().error = None;
        let context = self.git_context(&target).ok();
        let active_dialog = matches!(self.dialog.as_ref(),Some(Dialog::Git(dialog)) if matches!(dialog.as_ref(),GitDialog::Waiting{nonce,..} if *nonce==intent.nonce));
        let result = job.result.context("Git job returned no result")?;
        match result {
            GitValue::Open(open) => {
                ensure!(
                    open.project_id == target.project && open.repository.root == target.root,
                    "Repository reply belongs to another target"
                );
                let repo = self.git.repos.entry(target.clone()).or_default();
                repo.context = Some(open);
                repo.snapshot = None;
                repo.marked.clear();
                repo.error = None;
                self.refresh_git(target)?;
            }
            GitValue::Status(status) => {
                ensure!(
                    Some(&status.context_id) == context.as_ref(),
                    "Repository context changed; refresh again."
                );
                let repo = self.git.repos.get_mut(&target).unwrap();
                let selected_path = repo
                    .snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot.snapshot.entries.get(repo.selected))
                    .map(|entry| entry.path.bytes_base64.clone());
                repo.selected = selected_path
                    .and_then(|path| {
                        status
                            .snapshot
                            .entries
                            .iter()
                            .position(|entry| entry.path.bytes_base64 == path)
                    })
                    .unwrap_or(0);
                let marked_paths: HashSet<_> = repo
                    .snapshot
                    .as_ref()
                    .map(|snapshot| {
                        repo.marked
                            .iter()
                            .filter_map(|index| snapshot.snapshot.entries.get(*index))
                            .map(|entry| entry.path.bytes_base64.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                repo.marked = status
                    .snapshot
                    .entries
                    .iter()
                    .enumerate()
                    .filter_map(|(index, entry)| {
                        marked_paths
                            .contains(&entry.path.bytes_base64)
                            .then_some(index)
                    })
                    .collect();
                repo.snapshot = Some(status);
                repo.refreshed = Instant::now();
                repo.error = None;
                repo.selected = repo.selected.min(
                    repo.snapshot
                        .as_ref()
                        .unwrap()
                        .snapshot
                        .entries
                        .len()
                        .saturating_sub(1),
                );
                if repo.page == Page::Diff {
                    self.request_git_diff(target)?;
                }
            }
            GitValue::Diff(diff) => self.git.repos.get_mut(&target).unwrap().diff = Some(diff),
            GitValue::History(entries) => {
                let repo = self.git.repos.get_mut(&target).unwrap();
                repo.history = entries;
            }
            GitValue::CommitFiles(entries) => {
                let repo = self.git.repos.get_mut(&target).unwrap();
                repo.files = entries;
                repo.scroll = 0;
            }
            GitValue::Branches(entries) => {
                let repo = self.git.repos.get_mut(&target).unwrap();
                repo.branches = entries;
            }
            GitValue::Remotes(entries) => {
                let repo = self.git.repos.get_mut(&target).unwrap();
                repo.remotes = entries;
            }
            GitValue::CommitReview(review) => {
                if active_dialog {
                    let Purpose::CommitReview(message) = intent.purpose else {
                        anyhow::bail!("Unexpected commit review")
                    };
                    self.dialog = Some(Dialog::Git(Box::new(GitDialog::CommitReview {
                        target,
                        review: Box::new(review),
                        message,
                        scroll: 0,
                        diff: false,
                    })));
                }
            }
            GitValue::Plan(preview) => {
                ensure!(
                    preview.repository.root == target.root,
                    "Git plan targets another repository"
                );
                match intent.purpose {
                    Purpose::ExecutePlan => {
                        if active_dialog {
                            self.dialog = None;
                        }
                        self.execute_git_plan(target, &preview)?;
                    }
                    Purpose::ReviewPlan if active_dialog => {
                        self.dialog = Some(Dialog::Git(Box::new(GitDialog::Plan {
                            target,
                            preview: Box::new(preview),
                            scroll: 0,
                        })))
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }
    fn execute_git_plan(&mut self, target: Target, preview: &GitOperationPreview) -> Result<()> {
        let (rows, cols) = self.runtime.as_ref().unwrap().dimensions;
        if preview.kind == GitOperationKind::Commit {
            let message = self.git.repos[&target].draft.text.clone();
            self.git
                .submitted_messages
                .insert(preview.id.clone(), (target.clone(), message));
        }
        self.runtime.as_mut().unwrap().submit(
            Some(Request::GitExecute {
                plan: preview.id.clone(),
                rows,
                cols,
            }),
            Tag::GitExecute(preview.id.clone()),
        )?;
        self.git.executing.insert(preview.id.clone(), target);
        Ok(())
    }
    fn note_git_result(&mut self, operation: &GitOperationInfo) {
        if operation.state != GitOperationState::Complete {
            return;
        }
        if let Some((target, message)) = self.git.submitted_messages.remove(&operation.id) {
            if operation.result.as_ref().is_some_and(|result| {
                result.outcome == GitOutcome::Succeeded && result.commit.is_some()
            }) {
                if let Some(repo) = self.git.repos.get_mut(&target) {
                    if repo.draft.text == message {
                        repo.draft = CommitDraft::default();
                    }
                }
            }
        }
    }
    fn git_attach(&mut self, operation: &str, takeover: bool) -> Result<()> {
        self.runtime.as_mut().context("Host unavailable")?.submit(
            Some(Request::GitOperationAttach {
                operation: operation.into(),
                takeover,
            }),
            Tag::GitAttached,
        )
    }
    pub(super) fn git_writable(&self) -> bool {
        self.runtime.as_ref().is_some_and(|runtime| runtime.online)
            && self
                .git
                .screen
                .as_ref()
                .is_some_and(|screen| screen.error.is_none() && !screen.reader_closed)
            && self.git.operation.as_ref().is_some_and(|operation| {
                operation.state == GitOperationState::Running
                    && self
                        .runtime
                        .as_ref()
                        .and_then(|runtime| runtime.host.as_ref())
                        .is_some_and(|host| host.host_instance == operation.host_instance)
                    && operation.owner.as_ref().is_some_and(|owner| {
                        Some(&owner.client_id)
                            == self
                                .runtime
                                .as_ref()
                                .and_then(|runtime| runtime.client_id.as_ref())
                    })
            })
    }
    fn request_git_diff(&mut self, target: Target) -> Result<()> {
        let context = self.git_context(&target)?;
        let snapshot = self.git_snapshot_id(&target)?;
        let repo = self.git.repos.get_mut(&target).unwrap();
        let entry = if repo.snapshot.as_ref().unwrap().snapshot.entries.is_empty() {
            None
        } else {
            Some(repo.selected)
        };
        repo.page = Page::Diff;
        repo.scroll = 0;
        let target_kind = repo.diff_target;
        self.git_request(
            target,
            GitTask::Diff {
                context,
                snapshot,
                entry,
                target: target_kind,
            },
            Purpose::Diff,
        )?;
        Ok(())
    }
    fn git_wait(
        &mut self,
        target: Target,
        task: GitTask,
        purpose: Purpose,
        title: &str,
    ) -> Result<()> {
        let nonce = self.git_request(target.clone(), task, purpose)?;
        self.dialog = Some(Dialog::Git(Box::new(GitDialog::Waiting {
            target,
            nonce,
            title: title.into(),
        })));
        Ok(())
    }
    pub(super) fn git_menu_key(&mut self, key: KeyEvent) -> bool {
        if self.runtime.is_none() || self.focus != Focus::Content || self.tab != 1 {
            return false;
        }
        if matches!(key.code, KeyCode::Char('v')) {
            if let Some(project) = self.current_id() {
                self.dialog = Some(Dialog::Git(Box::new(GitDialog::Repositories {
                    project,
                    roots: self.repositories(),
                    selected: 0,
                })));
            }
            return true;
        }
        let Some(target) = self.git_target() else {
            return false;
        };
        let result = (|| -> Result<bool> {
            if key.code == KeyCode::F(5) {
                self.git.repos.entry(target.clone()).or_default().error = None;
                if !self.runtime.as_ref().unwrap().online {
                    self.git.reconnect = Some(target);
                    self.runtime.as_mut().unwrap().connect()?;
                } else {
                    self.open_git(target)?;
                }
                return Ok(true);
            }
            if key.code == KeyCode::Char('c') {
                self.git.repos.entry(target.clone()).or_default();
                self.dialog = Some(Dialog::Git(Box::new(GitDialog::Message { target })));
                return Ok(true);
            }
            let page = self
                .git
                .repos
                .get(&target)
                .map_or(Page::Changes, |repo| repo.page);
            let owned = match key.code {
                KeyCode::Up
                | KeyCode::Down
                | KeyCode::PageUp
                | KeyCode::PageDown
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::Enter
                | KeyCode::Char('h' | 'b' | 'r' | 'o') => true,
                KeyCode::Esc | KeyCode::Char('z') => page != Page::Changes,
                KeyCode::Char('s' | 'u' | ' ') => page == Page::Changes,
                KeyCode::Char('t') => page == Page::Diff,
                KeyCode::Char('n') => page == Page::Branches,
                KeyCode::Char('f' | 'p' | 'P') => page == Page::Remotes,
                _ => false,
            };
            if !owned {
                return Ok(false);
            }
            let context = self.git_context(&target)?;
            match key.code {
                KeyCode::Char('h') => {
                    self.git_request(
                        target,
                        GitTask::History { context, limit: 50 },
                        Purpose::History,
                    )?;
                }
                KeyCode::Char('b') => {
                    self.git_request(target, GitTask::Branches { context }, Purpose::Branches)?;
                }
                KeyCode::Char('r') => {
                    self.git_request(target, GitTask::Remotes { context }, Purpose::Remotes)?;
                }
                KeyCode::Char('o') => {
                    self.git.repos.get_mut(&target).unwrap().page = Page::Operations;
                    self.git.repos.get_mut(&target).unwrap().selected = 0;
                }
                KeyCode::Char('z') | KeyCode::Esc if page != Page::Changes => {
                    self.git.repos.get_mut(&target).unwrap().page = Page::Changes;
                    self.git.repos.get_mut(&target).unwrap().selected = 0;
                    self.refresh_git(target)?;
                }
                KeyCode::Up
                | KeyCode::Down
                | KeyCode::PageUp
                | KeyCode::PageDown
                | KeyCode::Home
                | KeyCode::End => {
                    let repo = self.git.repos.get_mut(&target).unwrap();
                    let count = match page {
                        Page::Changes => repo
                            .snapshot
                            .as_ref()
                            .map_or(0, |snapshot| snapshot.snapshot.entries.len()),
                        Page::History => repo.history.len(),
                        Page::Branches => repo.branches.len(),
                        Page::Remotes => repo.remotes.len(),
                        Page::Operations => self
                            .git
                            .operations
                            .iter()
                            .filter(|operation| {
                                operation.project_id == target.project
                                    && operation.repository.root == target.root
                            })
                            .count(),
                        _ => 0,
                    };
                    let down = matches!(key.code, KeyCode::Down | KeyCode::PageDown | KeyCode::End);
                    let amount = if matches!(key.code, KeyCode::PageDown | KeyCode::PageUp) {
                        8
                    } else {
                        1
                    };
                    if matches!(page, Page::Diff | Page::Files) {
                        repo.scroll = if down {
                            repo.scroll.saturating_add(amount)
                        } else {
                            repo.scroll.saturating_sub(amount)
                        };
                    } else {
                        repo.selected = match key.code {
                            KeyCode::Home => 0,
                            KeyCode::End => count.saturating_sub(1),
                            _ if down => repo
                                .selected
                                .saturating_add(amount as usize)
                                .min(count.saturating_sub(1)),
                            _ => repo.selected.saturating_sub(amount as usize),
                        };
                    }
                }
                KeyCode::Char(' ') if page == Page::Changes => {
                    let repo = self.git.repos.get_mut(&target).unwrap();
                    if !repo.marked.insert(repo.selected) {
                        repo.marked.remove(&repo.selected);
                    }
                }
                KeyCode::Char('s' | 'u') if page == Page::Changes => {
                    ensure!(
                        !self
                            .git
                            .submitted
                            .values()
                            .chain(self.git.jobs.values())
                            .any(|intent| intent.target == target
                                && matches!(intent.purpose, Purpose::ExecutePlan))
                            && !self
                                .git
                                .executing
                                .values()
                                .any(|pending| pending == &target),
                        "A Git file action is already pending for this repository."
                    );
                    let snapshot = self.git_snapshot_id(&target)?;
                    let repo = &self.git.repos[&target];
                    ensure!(
                        !repo.snapshot.as_ref().unwrap().snapshot.entries.is_empty(),
                        "No changed files are selected."
                    );
                    let entries = if repo.marked.is_empty() {
                        vec![repo.selected]
                    } else {
                        repo.marked.iter().copied().collect()
                    };
                    let task = if key.code == KeyCode::Char('s') {
                        GitTask::Stage {
                            context,
                            snapshot,
                            entries,
                        }
                    } else {
                        GitTask::Unstage {
                            context,
                            snapshot,
                            entries,
                        }
                    };
                    self.git_request(target, task, Purpose::ExecutePlan)?;
                }
                KeyCode::Char('t') if page == Page::Diff => {
                    let repo = self.git.repos.get_mut(&target).unwrap();
                    repo.diff_target = if repo.diff_target == DiffTarget::Index {
                        DiffTarget::Worktree
                    } else {
                        DiffTarget::Index
                    };
                    self.request_git_diff(target)?;
                }
                KeyCode::Char('n') if page == Page::Branches => {
                    self.dialog = Some(Dialog::Git(Box::new(GitDialog::Branch {
                        target,
                        input: TextInput::new(""),
                    })))
                }
                KeyCode::Enter => match page {
                    Page::Changes => self.request_git_diff(target)?,
                    Page::History => {
                        let oid = self.git.repos[&target]
                            .history
                            .get(self.git.repos[&target].selected)
                            .context("No commit selected")?
                            .oid
                            .clone();
                        self.git_request(
                            target,
                            GitTask::CommitFiles { context, oid },
                            Purpose::Files,
                        )?;
                    }
                    Page::Branches => {
                        let snapshot = self.git_snapshot_id(&target)?;
                        let name = self.git.repos[&target]
                            .branches
                            .get(self.git.repos[&target].selected)
                            .context("No branch selected")?
                            .name
                            .clone();
                        self.git_wait(
                            target,
                            GitTask::Switch {
                                context,
                                snapshot,
                                name,
                            },
                            Purpose::ReviewPlan,
                            "Review branch switch",
                        )?;
                    }
                    Page::Operations => {
                        let operation = self
                            .git
                            .operations
                            .iter()
                            .filter(|operation| {
                                operation.project_id == target.project
                                    && operation.repository.root == target.root
                            })
                            .nth(self.git.repos[&target].selected)
                            .context("No Git operation selected")?
                            .clone();
                        if operation.owner.as_ref().is_some_and(|owner| {
                            Some(&owner.client_id)
                                != self
                                    .runtime
                                    .as_ref()
                                    .and_then(|runtime| runtime.client_id.as_ref())
                        }) {
                            self.dialog = Some(Dialog::Git(Box::new(GitDialog::Takeover {
                                operation: operation.id,
                                name: format!("{:?}", operation.kind),
                            })));
                        } else {
                            self.git_attach(&operation.id, false)?;
                        }
                    }
                    _ => {}
                },
                KeyCode::Char('f' | 'p' | 'P') if page == Page::Remotes => {
                    let remote = self.git.repos[&target]
                        .remotes
                        .get(self.git.repos[&target].selected)
                        .context("No remote selected")?
                        .name
                        .clone();
                    if key.code == KeyCode::Char('f') {
                        self.git_wait(
                            target,
                            GitTask::Fetch { context, remote },
                            Purpose::ReviewPlan,
                            "Review fetch",
                        )?;
                    } else {
                        self.dialog = Some(Dialog::Git(Box::new(GitDialog::Remote {
                            target,
                            remote,
                            push: key.code == KeyCode::Char('P'),
                            input: TextInput::new(""),
                        })));
                    }
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
    pub(super) fn handle_git_dialog(&mut self, mut dialog: GitDialog, key: KeyEvent) {
        if key.code == KeyCode::Esc {
            if let GitDialog::Waiting { nonce, .. } = dialog {
                self.git.cancelled.insert(nonce);
            }
            self.info("Git draft kept. No additional action was started.");
            return;
        }
        let result = (|| -> Result<bool> {
            match &mut dialog {
                GitDialog::Reconcile { operation } => {
                    if key.code == KeyCode::F(2) {
                        self.runtime.as_mut().unwrap().submit(
                            Some(Request::GitOperationReconcile {
                                operation: operation.id.clone(),
                                repository: operation.repository.clone(),
                            }),
                            Tag::GitUpdated,
                        )?;
                        self.info("Cleanup acknowledgment submitted. The operation result remains Unknown.");
                        return Ok(true);
                    }
                }
                GitDialog::Operations { entries, selected } => match key.code {
                    KeyCode::Char('u') => {
                        let operation = entries
                            .get(*selected)
                            .context("Select an Unknown Git operation")?
                            .clone();
                        ensure!(
                            operation.state == GitOperationState::Unknown
                                && !operation.cleanup_acknowledged,
                            "Only unacknowledged Unknown Git operations need reconciliation"
                        );
                        self.dialog = Some(Dialog::Git(Box::new(GitDialog::Reconcile {
                            operation: Box::new(operation),
                        })));
                        return Ok(true);
                    }

                    KeyCode::Up => *selected = selected.saturating_sub(1),
                    KeyCode::Down => {
                        *selected = selected
                            .saturating_add(1)
                            .min(entries.len().saturating_sub(1))
                    }
                    KeyCode::F(5) => {
                        self.runtime.as_mut().unwrap().submit(
                            Some(Request::GitOperations { project: None }),
                            Tag::GitOperations,
                        )?;
                    }
                    KeyCode::Enter => {
                        let operation = entries
                            .get(*selected)
                            .context("No Git operation is available")?
                            .clone();
                        if operation.owner.as_ref().is_some_and(|owner| {
                            Some(&owner.client_id)
                                != self
                                    .runtime
                                    .as_ref()
                                    .and_then(|runtime| runtime.client_id.as_ref())
                        }) {
                            self.dialog = Some(Dialog::Git(Box::new(GitDialog::Takeover {
                                operation: operation.id,
                                name: kind_label(operation.kind).into(),
                            })));
                        } else {
                            self.git_attach(&operation.id, false)?;
                        }
                        return Ok(true);
                    }
                    _ => {}
                },
                GitDialog::Repositories {
                    project,
                    roots,
                    selected,
                } => match key.code {
                    KeyCode::Up => *selected = selected.saturating_sub(1),
                    KeyCode::Down => {
                        *selected = selected
                            .saturating_add(1)
                            .min(roots.len().saturating_sub(1))
                    }
                    KeyCode::Enter => {
                        let root = roots
                            .get(*selected)
                            .context("No repository is connected.")?
                            .clone();
                        self.git.roots.insert(project.clone(), root.clone());
                        self.open_git(Target {
                            project: project.clone(),
                            root,
                        })?;
                        return Ok(true);
                    }
                    _ => {}
                },
                GitDialog::Message { target } => {
                    if key.code == KeyCode::F(2) {
                        let context = self.git_context(target)?;
                        let message = self.git.repos[target].draft.text.clone();
                        ensure!(!message.trim().is_empty(), "Enter a commit message.");
                        self.git_wait(
                            target.clone(),
                            GitTask::CommitReview { context },
                            Purpose::CommitReview(message),
                            "Reading all staged changes",
                        )?;
                        return Ok(true);
                    }
                    self.git
                        .repos
                        .get_mut(target)
                        .unwrap()
                        .draft
                        .key(key)
                        .map_err(anyhow::Error::msg)?;
                }
                GitDialog::Waiting { .. } => {}
                GitDialog::CommitReview {
                    target,
                    review,
                    message,
                    scroll,
                    diff,
                } => {
                    if key.code == KeyCode::Tab {
                        *diff = !*diff;
                    } else if key.code == KeyCode::F(2) {
                        let context = self.git_context(target)?;
                        self.git_wait(
                            target.clone(),
                            GitTask::Commit {
                                context,
                                review: review.review_id.clone(),
                                message: message.clone(),
                            },
                            Purpose::ExecutePlan,
                            "Preparing the reviewed commit",
                        )?;
                        return Ok(true);
                    } else {
                        super::scroll(scroll, key);
                    }
                }
                GitDialog::Plan {
                    target,
                    preview,
                    scroll,
                } => {
                    if key.code == KeyCode::F(2) {
                        self.execute_git_plan(target.clone(), preview)?;
                        return Ok(true);
                    }
                    super::scroll(scroll, key);
                }
                GitDialog::Branch { target, input } => {
                    if key.code == KeyCode::F(2) {
                        let context = self.git_context(target)?;
                        self.git_wait(
                            target.clone(),
                            GitTask::CreateBranch {
                                context,
                                name: input.text.clone(),
                                start: None,
                            },
                            Purpose::ExecutePlan,
                            "Creating branch",
                        )?;
                        return Ok(true);
                    }
                    input.key(key);
                }
                GitDialog::Remote {
                    target,
                    remote,
                    push,
                    input,
                } => {
                    if key.code == KeyCode::F(2) {
                        let context = self.git_context(target)?;
                        let snapshot = self.git_snapshot_id(target)?;
                        let task = if *push {
                            GitTask::Push {
                                context,
                                snapshot,
                                remote: remote.clone(),
                                branch: input.text.clone(),
                            }
                        } else {
                            GitTask::Pull {
                                context,
                                snapshot,
                                remote: remote.clone(),
                                branch: input.text.clone(),
                            }
                        };
                        self.git_wait(
                            target.clone(),
                            task,
                            Purpose::ReviewPlan,
                            if *push {
                                "Review push"
                            } else {
                                "Review fast-forward pull"
                            },
                        )?;
                        return Ok(true);
                    }
                    input.key(key);
                }
                GitDialog::Takeover { operation, .. } => {
                    if key.code == KeyCode::F(2) {
                        self.git_attach(operation, true)?;
                        return Ok(true);
                    }
                }
            }
            Ok(false)
        })();
        match result {
            Ok(true) => {}
            Ok(false) => self.dialog = Some(Dialog::Git(Box::new(dialog))),
            Err(error) => {
                self.dialog = Some(Dialog::Git(Box::new(dialog)));
                self.error(error.to_string());
            }
        }
    }
    pub(super) fn git_paste(&mut self, text: &str) -> Result<()> {
        if let Some(Dialog::Git(dialog)) = &mut self.dialog {
            match dialog.as_mut() {
                GitDialog::Message { target } => self
                    .git
                    .repos
                    .get_mut(target)
                    .unwrap()
                    .draft
                    .insert(text)
                    .map_err(anyhow::Error::msg)?,
                GitDialog::Branch { input, .. } | GitDialog::Remote { input, .. } => {
                    input.insert(text).map_err(anyhow::Error::msg)?
                }
                _ => {}
            }
        }
        Ok(())
    }
}

impl App<'_> {
    pub(super) fn terminal_snapshot(&self) -> Option<&crate::terminal::TerminalSnapshot> {
        if self.git.focused {
            self.git.screen.as_ref()
        } else {
            self.runtime
                .as_ref()
                .and_then(|runtime| runtime.screen.as_ref())
        }
    }
    pub(super) fn terminal_writable(&self) -> bool {
        if self.git.focused {
            self.git_writable()
        } else {
            self.runtime
                .as_ref()
                .is_some_and(|runtime| runtime.can_input())
        }
    }
    pub(super) fn git_operation_key(&mut self, key: KeyEvent) -> bool {
        if self.runtime.is_some()
            && (key.code == KeyCode::F(6)
                || (key.code == KeyCode::Char('o')
                    && self.tab == 1
                    && self.focus == Focus::Content
                    && self.git_target().is_none()))
        {
            self.dialog = Some(Dialog::Git(Box::new(GitDialog::Operations {
                entries: self.git.operations.clone(),
                selected: 0,
            })));
            if let Err(error) = self.runtime.as_mut().unwrap().submit(
                Some(Request::GitOperations { project: None }),
                Tag::GitOperations,
            ) {
                self.error(error.to_string());
            }
            return true;
        }
        if !self.git.focused {
            return false;
        }
        let result = (|| -> Result<bool> {
            match key.code {
                KeyCode::Char('z') => {
                    self.git.focused = false;
                    if self
                        .git
                        .operation
                        .as_ref()
                        .is_some_and(|operation| operation.state == GitOperationState::Complete)
                    {
                        self.git.screen = None;
                    }
                    self.terminal_connected = self
                        .runtime
                        .as_ref()
                        .is_some_and(|runtime| runtime.active.is_some());
                    self.menu_visible = true;
                    self.tab = 1;
                    self.focus = Focus::Content;
                    if let Some(target) = self.git_target() {
                        let repo = self.git.repos.entry(target.clone()).or_default();
                        repo.page = Page::Changes;
                        repo.scroll = 0;
                        self.refresh_git(target)?;
                    }
                }
                KeyCode::Char('k') => {
                    let operation = self
                        .git
                        .operation
                        .as_ref()
                        .context("No Git operation is selected")?;
                    ensure!(
                        operation
                            .owner
                            .as_ref()
                            .is_some_and(|owner| Some(&owner.client_id)
                                == self
                                    .runtime
                                    .as_ref()
                                    .and_then(|runtime| runtime.client_id.as_ref())),
                        "Attach the Git operation before cancelling it."
                    );
                    self.runtime.as_mut().unwrap().submit(
                        Some(Request::GitOperationCancel {
                            operation: operation.id.clone(),
                            epoch: operation.input_epoch,
                            force: false,
                        }),
                        Tag::GitUpdated,
                    )?;
                    self.info("Cancellation requested; waiting for the Git process and its owned children to finish.");
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
    pub(super) fn git_reconnected(&mut self) -> Result<()> {
        if let Some(target) = self.git.reconnect.take() {
            self.open_git(target)?;
        }
        Ok(())
    }
    pub(super) fn git_host_changed(&mut self) {
        for repo in self.git.repos.values_mut() {
            repo.context = None;
            repo.snapshot = None;
            repo.busy = 0;
            repo.marked.clear();
            repo.error = Some(
                "Host changed. F5 reconnects this repository; the message draft is kept.".into(),
            );
        }
        self.git.cancelled.clear();
        self.git.submitted.clear();
        self.git.jobs.clear();
        self.git.polling.clear();
        self.git.executing.clear();
        self.git.submitted_messages.clear();
        self.git.operations.clear();
        self.git.snapshot_pending = false;
        self.git.snapshot_finished = true;
        self.git.operations_pending = false;
        if let Some(operation) = &mut self.git.operation {
            operation.state = GitOperationState::Unknown;
            operation.error = Some(
                "Host changed; inspect repository state before retrying this operation.".into(),
            );
        }
    }
}
pub(super) fn kind_label(kind: GitOperationKind) -> &'static str {
    match kind {
        GitOperationKind::Stage => "Stage",
        GitOperationKind::Unstage => "Unstage",
        GitOperationKind::Commit => "Commit",
        GitOperationKind::CreateBranch => "Create branch",
        GitOperationKind::SwitchBranch => "Switch branch",
        GitOperationKind::Fetch => "Fetch",
        GitOperationKind::PullFastForward => "Fast-forward pull",
        GitOperationKind::Push => "Push",
    }
}

#[cfg(test)]
mod response_tests {
    use super::*;
    use crate::{git::Repository, project::LaunchEnvironment, store::Store};
    #[test]
    fn late_diff_and_replaced_context_responses_cannot_replace_current_preview() {
        let temporary = tempfile::tempdir().unwrap();
        let store = Store::open(Some(temporary.path())).unwrap();
        let environment = LaunchEnvironment::from_variables(std::collections::BTreeMap::from([(
            "HOME".into(),
            temporary.path().to_string_lossy().into_owned(),
        )]))
        .unwrap();
        let mut app = App::with_environment(&store, environment).unwrap();
        let target = Target {
            project: "project".into(),
            root: temporary.path().join("repo"),
        };
        let repository = Repository {
            root: target.root.clone(),
            git_dir: target.root.join(".git"),
            common_dir: target.root.join(".git"),
        };
        let mut repo = RepoView {
            context: Some(GitContextInfo {
                id: "context".into(),
                project_id: target.project.clone(),
                repository,
                primary: true,
                source_use: GitGateState::default(),
            }),
            busy: 2,
            page: Page::Diff,
            ..RepoView::default()
        };
        repo.latest.insert(2, 2);
        app.git.repos.insert(target.clone(), repo);
        let reply = |text: &str| GitJobInfo {
            id: "job".into(),
            context_id: Some("context".into()),
            state: GitJobState::Ready,
            result: Some(GitValue::Diff(DiffView {
                text: text.into(),
                truncated: false,
                binary: false,
                conversion_filters_disabled: false,
            })),
            error: None,
        };
        app.receive_git_job(
            reply("new preview"),
            Intent {
                target: target.clone(),
                purpose: Purpose::Diff,
                nonce: 2,
                context: Some("context".into()),
            },
        )
        .unwrap();
        app.receive_git_job(
            reply("late old preview"),
            Intent {
                target: target.clone(),
                purpose: Purpose::Diff,
                nonce: 1,
                context: Some("context".into()),
            },
        )
        .unwrap();
        assert_eq!(
            app.git.repos[&target].diff.as_ref().unwrap().text,
            "new preview"
        );
        app.git
            .repos
            .get_mut(&target)
            .unwrap()
            .context
            .as_mut()
            .unwrap()
            .id = "replacement".into();
        app.git.repos.get_mut(&target).unwrap().latest.insert(2, 3);
        app.receive_git_job(
            reply("old context preview"),
            Intent {
                target: target.clone(),
                purpose: Purpose::Diff,
                nonce: 3,
                context: Some("context".into()),
            },
        )
        .unwrap();
        assert_eq!(
            app.git.repos[&target].diff.as_ref().unwrap().text,
            "new preview"
        );
    }
    #[test]
    fn late_attach_keeps_the_commit_editor_visible_instead_of_hiding_its_input_target() {
        use crossterm::event::KeyModifiers;
        use ratatui::{backend::TestBackend, Terminal};
        let temporary = tempfile::tempdir().unwrap();
        let store = Store::open(Some(temporary.path())).unwrap();
        let environment = LaunchEnvironment::from_variables(std::collections::BTreeMap::from([(
            "HOME".into(),
            temporary.path().to_string_lossy().into_owned(),
        )]))
        .unwrap();
        let mut app = App::with_environment(&store, environment).unwrap();
        let target = Target {
            project: "project".into(),
            root: temporary.path().join("repo"),
        };
        app.git.repos.insert(target.clone(), RepoView::default());
        app.dialog = Some(Dialog::Git(Box::new(GitDialog::Message {
            target: target.clone(),
        })));
        app.set_terminal_connected(true);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("Commit message"));
        assert!(app.menu_visible);
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE))
                .unwrap(),
            super::super::UiOutcome::Continue
        );
        assert_eq!(app.git.repos[&target].draft.text, "q");
    }
}
