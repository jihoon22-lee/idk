//! Git workflow core. Reads suppress configurable helpers/network side effects;
//! writes retain Git hooks, filters, signing and authentication through an owned
//! executor. Plans are authoritative in-process objects, never client commands.
use super::parse;
use super::types::*;
use super::{collect, sanitize_environment, stop_owned_process, GitExecutable, Output, Repository};
use crate::model::{new_id, SourceGate};
use crate::store::{ensure_private_dir, FileLock};
use anyhow::{bail, ensure, Context, Result};
use portable_pty::CommandBuilder;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const READ_LIMIT: usize = 8 * 1024 * 1024;
const INDEX_LIMIT: usize = 64 * 1024 * 1024;
const DIFF_LIMIT: usize = 1024 * 1024;
const CONFIG_LIMIT: usize = 256 * 1024;
const ARG_LIMIT: usize = 128 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct GitService(Arc<Inner>);
struct Inner {
    git: GitExecutable,
    binding: Repository,
    environment: BTreeMap<String, String>,
    version: (u32, u32),
    id: String,
    mutation: Mutex<()>,
    status_cache: Mutex<Option<(Instant, GitSnapshot)>>,
}
impl std::fmt::Debug for GitService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitService")
            .field("repository", &self.0.binding)
            .finish()
    }
}

/// No Deserialize/Serialize and no public mutation fields. The host stores this
/// object and exposes only preview()/id() to clients; cloned plans share a
/// one-shot execution state, so a repeated click cannot repeat a mutation.
#[derive(Clone)]
pub struct GitOperationPlan {
    preview: GitOperationPreview,
    service: String,
    operation: Operation,
    state: Arc<AtomicU8>,
}
impl std::fmt::Debug for GitOperationPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitOperationPlan")
            .field("preview", &self.preview)
            .finish()
    }
}
impl GitOperationPlan {
    pub fn id(&self) -> &str {
        &self.preview.id
    }
    pub fn preview(&self) -> &GitOperationPreview {
        &self.preview
    }
}
#[derive(Clone)]
enum Operation {
    Stage(Vec<GitPath>),
    Unstage(Vec<GitPath>),
    Commit {
        message: String,
        staged_digest: String,
    },
    CreateBranch {
        name: String,
        start: ObjectId,
    },
    Switch {
        name: String,
        oid: ObjectId,
    },
    Fetch {
        remote: String,
        endpoint_digest: String,
    },
    Pull {
        remote: String,
        branch: String,
        endpoint_digest: String,
    },
    Push {
        remote: String,
        branch: String,
        source: ObjectId,
        endpoint_digest: String,
    },
}
struct RemoteState {
    view: Remote,
    fetch_digest: String,
    push_digest: String,
    fetch_unsafe: bool,
    push_unsafe: bool,
}
struct Resources(PathBuf);
impl Drop for Resources {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

impl GitService {
    pub fn new(
        git: GitExecutable,
        binding: Repository,
        mut environment: BTreeMap<String, String>,
    ) -> Result<Self> {
        ensure!(
            environment.len() <= 1024,
            "Git environment exceeds 1024 entries"
        );
        let total: usize = environment
            .iter()
            .map(|(k, v)| k.len().saturating_add(v.len()).saturating_add(2))
            .sum();
        ensure!(total <= 256 * 1024, "Git environment exceeds 256 KiB");
        for (key, value) in &environment {
            ensure!(
                !key.is_empty() && !key.contains(['=', '\0']) && !value.contains('\0'),
                "invalid Git environment entry"
            );
        }
        // Use the existing routing sanitization while preserving the caller's
        // current agent/CA/helper references, not the host's first environment.
        let mut sanitized = Command::new(git.path());
        sanitized.env_clear().envs(&environment);
        sanitize_environment(&mut sanitized);
        for (name, value) in sanitized.get_envs() {
            if let Some(name) = name.to_str() {
                match value {
                    Some(value) => {
                        if let Some(value) = value.to_str() {
                            environment.insert(name.into(), value.into());
                        }
                    }
                    None => {
                        environment.remove(name);
                    }
                }
            }
        }
        for name in [
            "GIT_GLOB_PATHSPECS",
            "GIT_NOGLOB_PATHSPECS",
            "GIT_ICASE_PATHSPECS",
            "GIT_LITERAL_PATHSPECS",
            "GIT_FSMONITOR_TEST",
        ] {
            environment.remove(name);
        }
        environment.insert("GIT_NO_LAZY_FETCH".into(), "1".into());
        let output = super::run(
            git.path(),
            Path::new("/"),
            &["--version".into()],
            Duration::from_secs(2),
            4096,
        )?;
        let version = parse_version(&output.stdout)?;
        ensure!(
            version >= (2, 18),
            "Git 2.18 or newer is required for this workflow"
        );
        let service = Self(Arc::new(Inner {
            git,
            binding,
            environment,
            version,
            id: new_id(),
            mutation: Mutex::new(()),
            status_cache: Mutex::new(None),
        }));
        service.verify_binding()?;
        Ok(service)
    }
    pub fn repository(&self) -> &Repository {
        &self.0.binding
    }
    /// Clones share one status flight/cache per repository service. Host must
    /// keep one service per canonical worktree identity, not one per terminal.
    pub fn status(&self) -> Result<GitSnapshot> {
        self.status_cached(false)
    }
    pub fn refresh(&self) -> Result<GitSnapshot> {
        self.status_cached(true)
    }
    fn status_cached(&self, force: bool) -> Result<GitSnapshot> {
        let mut cache = self
            .0
            .status_cache
            .lock()
            .map_err(|_| anyhow::anyhow!("Git status provider unavailable"))?;
        if !force {
            if let Some((time, snapshot)) = &*cache {
                if time.elapsed() < Duration::from_millis(200) {
                    return Ok(snapshot.clone());
                }
            }
        }
        let snapshot = self.snapshot()?;
        *cache = Some((Instant::now(), snapshot.clone()));
        Ok(snapshot)
    }
    fn snapshot(&self) -> Result<GitSnapshot> {
        self.verify_binding()?;
        let filters = self.read_filter_overrides()?;
        self.reject_unsafe_lazy_fetch()?;
        for _ in 0..2 {
            let head = self.head()?;
            let index_path = self.git_path("index")?;
            let before = index_file_digest(&index_path)?;
            let logical = self.read_prepared(
                &strings(&["ls-files", "--stage", "-z"]),
                INDEX_LIMIT,
                &filters,
            )?;
            require_success(&logical, "read actual index")?;
            validate_index(&logical.stdout)?;
            let output = self.read_prepared(
                &strings(&[
                    "status",
                    "--porcelain=v2",
                    "-z",
                    "--branch",
                    "--untracked-files=all",
                    "--ignore-submodules=dirty",
                ]),
                READ_LIMIT,
                &filters,
            )?;
            require_success(&output, "read status")?;
            let parsed = parse::status(&output.stdout)?;
            let after = index_file_digest(&index_path)?;
            if before != after || head != self.head()? {
                continue;
            }
            let index_digest = fingerprint(&[b"idk-index-v1", after.as_bytes(), &logical.stdout]);
            return Ok(GitSnapshot {
                revision: GitRevision {
                    repository: self.0.binding.clone(),
                    head,
                    index_digest,
                },
                entries: parsed.entries,
                upstream: parsed.upstream,
                ahead: parsed.ahead,
                behind: parsed.behind,
                conversion_filters_disabled: !filters.is_empty(),
                submodule_worktrees_unchecked: logical
                    .stdout
                    .split(|b| *b == 0)
                    .any(|entry| entry.starts_with(b"160000 ")),
            });
        }
        bail!("repository/index changed during inspection; refresh before continuing")
    }
    pub fn diff(&self, target: DiffTarget, path: Option<&GitPath>) -> Result<DiffView> {
        self.verify_binding()?;
        let filters = self.read_filter_overrides()?;
        self.reject_unsafe_lazy_fetch()?;
        let mut args = strings(&[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--no-renames",
            "--ignore-submodules=dirty",
            "--submodule=short",
            "--ita-invisible-in-index",
        ]);
        if target == DiffTarget::Index {
            args.push("--cached".into());
        }
        args.push("--".into());
        if let Some(path) = path {
            args.push(path.as_os_str().into());
        }
        let output = self.read_prepared(&args, DIFF_LIMIT, &filters);
        match output {
            Ok(output) => {
                require_success(&output, "read diff")?;
                let binary = output
                    .stdout
                    .windows(b"Binary files ".len())
                    .any(|w| w == b"Binary files ");
                Ok(DiffView {
                    text: display_patch(&output.stdout),
                    truncated: false,
                    binary,
                    conversion_filters_disabled: !filters.is_empty(),
                })
            }
            Err(error) if error.to_string().contains("output limit") => Ok(DiffView {
                text: "Diff exceeds the preview limit; select a file for a smaller view.".into(),
                truncated: true,
                binary: false,
                conversion_filters_disabled: !filters.is_empty(),
            }),
            Err(error) => Err(error),
        }
    }
    pub fn history(&self, limit: usize) -> Result<Vec<CommitSummary>> {
        ensure!((1..=200).contains(&limit), "history limit must be 1–200");
        self.verify_binding()?;
        if self.head()?.oid().is_none() {
            return Ok(Vec::new());
        }
        let args = strings(&[
            "log",
            "--no-show-signature",
            "-z",
            "--format=%H%x00%P%x00%an%x00%at%x00%s",
            &format!("-n{limit}"),
            "HEAD",
            "--",
        ]);
        let output = self.read(&args, READ_LIMIT)?;
        require_success(&output, "read history")?;
        parse::history(&output.stdout)
    }
    pub fn commit_files(&self, oid: &ObjectId) -> Result<Vec<GitChange>> {
        self.verify_binding()?;
        let output = self.read(
            &strings(&[
                "diff-tree",
                "--root",
                "-r",
                "--raw",
                "-z",
                "--no-abbrev",
                "--no-renames",
                "--no-commit-id",
                "--no-ext-diff",
                "--no-textconv",
                oid.as_str(),
                "--",
            ]),
            READ_LIMIT,
        )?;
        require_success(&output, "read commit files")?;
        parse::raw_diff(&output.stdout)
    }
    pub fn branches(&self) -> Result<Vec<Branch>> {
        self.verify_binding()?;
        let output = self.read(
            &strings(&[
                "for-each-ref",
                "--format=%(refname)%00%(objectname)%00%(HEAD)%00%(upstream)%00",
                "refs/heads/",
            ]),
            READ_LIMIT,
        )?;
        require_success(&output, "list branches")?;
        let mut result = Vec::new();
        for line in output
            .stdout
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
        {
            let fields: Vec<_> = line.split(|b| *b == 0).collect();
            ensure!(
                fields.len() == 5 && fields[4].is_empty(),
                "invalid branch response"
            );
            let reference = text(fields[0])?;
            let name = reference
                .strip_prefix("refs/heads/")
                .context("invalid local branch ref")?
                .to_owned();
            result.push(Branch {
                name,
                reference,
                oid: ObjectId::parse(&text(fields[1])?)?,
                current: fields[2] == b"*",
                upstream: if fields[3].is_empty() {
                    None
                } else {
                    Some(text(fields[3])?)
                },
            });
        }
        Ok(result)
    }
    pub fn remotes(&self) -> Result<Vec<Remote>> {
        self.verify_binding()?;
        let output = self.read_config(&strings(&["remote"]), CONFIG_LIMIT)?;
        require_success(&output, "list remotes")?;
        output
            .stdout
            .split(|b| *b == b'\n')
            .filter(|s| !s.is_empty())
            .map(|name| Ok(self.remote_state(&text(name)?)?.view))
            .collect()
    }
    pub fn commit_preview(&self) -> Result<CommitPreview> {
        let snapshot = self.refresh()?;
        ensure!(
            !snapshot.conflicted(),
            "resolve index conflicts before committing"
        );
        let raw = self.staged_raw()?;
        let staged = parse::raw_diff(&raw)?;
        let diff = self.diff(DiffTarget::Index, None)?;
        let after = self.refresh()?;
        ensure!(
            after.revision == snapshot.revision,
            "index/HEAD changed while preparing commit review"
        );
        Ok(CommitPreview {
            seal: CommitSeal {
                service: self.0.id.clone(),
                revision: snapshot.revision.clone(),
                staged_digest: fingerprint(&[&raw]),
            },
            snapshot,
            staged,
            diff,
        })
    }

    pub fn plan_stage(
        &self,
        paths: Vec<GitPath>,
        expected: &GitRevision,
    ) -> Result<GitOperationPlan> {
        validate_selection(&paths)?;
        let snapshot = self.refresh()?;
        ensure_revision(&snapshot.revision, expected)?;
        ensure!(
            paths.iter().all(|path| snapshot
                .entries
                .iter()
                .any(|entry| entry.paths().contains(path))),
            "selected file is not present in the reviewed status"
        );
        Ok(self.plan(
            snapshot.revision,
            GitOperationKind::Stage,
            paths.clone(),
            None,
            None,
            Operation::Stage(paths),
        ))
    }
    pub fn plan_unstage(
        &self,
        paths: Vec<GitPath>,
        expected: &GitRevision,
    ) -> Result<GitOperationPlan> {
        validate_selection(&paths)?;
        let snapshot = self.refresh()?;
        ensure_revision(&snapshot.revision, expected)?;
        ensure!(
            paths.iter().all(|path| snapshot
                .entries
                .iter()
                .any(|entry| entry.paths().contains(path))),
            "selected file is not present in the reviewed status"
        );
        Ok(self.plan(
            snapshot.revision,
            GitOperationKind::Unstage,
            paths.clone(),
            None,
            None,
            Operation::Unstage(paths),
        ))
    }
    pub fn plan_commit(&self, preview: &CommitPreview, message: &str) -> Result<GitOperationPlan> {
        ensure!(
            preview.seal.service == self.0.id,
            "commit review belongs to another service"
        );
        ensure!(
            !message.trim().is_empty() && message.len() <= 65536 && !message.contains('\0'),
            "commit message must be nonempty and at most 64 KiB"
        );
        let snapshot = self.refresh()?;
        ensure_revision(&snapshot.revision, &preview.seal.revision)?;
        let raw = self.staged_raw()?;
        ensure!(!raw.is_empty(), "there are no staged changes to commit");
        ensure!(
            fingerprint(&[&raw]) == preview.seal.staged_digest,
            "staged contents changed since review"
        );
        Ok(self.plan(
            snapshot.revision,
            GitOperationKind::Commit,
            Vec::new(),
            None,
            None,
            Operation::Commit {
                message: message.into(),
                staged_digest: preview.seal.staged_digest.clone(),
            },
        ))
    }
    pub fn plan_branch_create(
        &self,
        name: &str,
        start: Option<&ObjectId>,
    ) -> Result<GitOperationPlan> {
        self.validate_branch(name)?;
        ensure!(self.branch_oid(name)?.is_none(), "branch already exists");
        let snapshot = self.refresh()?;
        let start = start
            .or_else(|| snapshot.revision.head.oid())
            .context("an unborn branch has no starting commit")?
            .clone();
        self.verify_commit(&start)?;
        Ok(self.plan(
            snapshot.revision,
            GitOperationKind::CreateBranch,
            Vec::new(),
            Some(name.into()),
            None,
            Operation::CreateBranch {
                name: name.into(),
                start,
            },
        ))
    }
    pub fn plan_switch(&self, name: &str, expected: &GitRevision) -> Result<GitOperationPlan> {
        self.validate_branch(name)?;
        let oid = self
            .branch_oid(name)?
            .context("local branch not found; remote branch guessing is disabled")?;
        let snapshot = self.refresh()?;
        ensure_revision(&snapshot.revision, expected)?;
        ensure!(snapshot.clean(), "working tree/index must be clean before switching branches; no stash or discard is performed");
        Ok(self.plan(
            snapshot.revision,
            GitOperationKind::SwitchBranch,
            Vec::new(),
            Some(name.into()),
            None,
            Operation::Switch {
                name: name.into(),
                oid,
            },
        ))
    }
    pub fn plan_fetch(&self, remote: &str) -> Result<GitOperationPlan> {
        let state = self.remote_state(remote)?;
        safe_remote(&state, false)?;
        let snapshot = self.refresh()?;
        let target = RemoteTarget {
            remote: state.view,
            destination: None,
            source: None,
            cached_remote_tip: None,
            ahead: None,
            behind: None,
        };
        Ok(self.plan(
            snapshot.revision,
            GitOperationKind::Fetch,
            Vec::new(),
            None,
            Some(target),
            Operation::Fetch {
                remote: remote.into(),
                endpoint_digest: state.fetch_digest,
            },
        ))
    }
    pub fn plan_pull_ff_only(
        &self,
        remote: &str,
        branch: &str,
        expected: &GitRevision,
    ) -> Result<GitOperationPlan> {
        self.validate_branch(branch)?;
        let state = self.remote_state(remote)?;
        safe_remote(&state, false)?;
        let snapshot = self.refresh()?;
        ensure_revision(&snapshot.revision, expected)?;
        ensure!(
            matches!(snapshot.revision.head, Head::Branch { .. }),
            "fast-forward pull requires an existing local branch"
        );
        ensure!(
            snapshot.clean(),
            "working tree/index must be clean before pull; no automatic stash is performed"
        );
        let target =
            self.remote_target(state.view, branch, snapshot.revision.head.oid().cloned())?;
        Ok(self.plan(
            snapshot.revision,
            GitOperationKind::PullFastForward,
            Vec::new(),
            Some(branch.into()),
            Some(target),
            Operation::Pull {
                remote: remote.into(),
                branch: branch.into(),
                endpoint_digest: state.fetch_digest,
            },
        ))
    }
    pub fn plan_push(
        &self,
        remote: &str,
        destination: &str,
        expected: &GitRevision,
    ) -> Result<GitOperationPlan> {
        self.validate_branch(destination)?;
        let state = self.remote_state(remote)?;
        safe_remote(&state, true)?;
        let snapshot = self.refresh()?;
        ensure_revision(&snapshot.revision, expected)?;
        let source = snapshot
            .revision
            .head
            .oid()
            .context("there is no commit to push")?
            .clone();
        let target = self.remote_target(state.view, destination, Some(source.clone()))?;
        Ok(self.plan(
            snapshot.revision,
            GitOperationKind::Push,
            Vec::new(),
            Some(destination.into()),
            Some(target),
            Operation::Push {
                remote: remote.into(),
                branch: destination.into(),
                source,
                endpoint_digest: state.push_digest,
            },
        ))
    }
    fn plan(
        &self,
        before: GitRevision,
        kind: GitOperationKind,
        paths: Vec<GitPath>,
        branch: Option<String>,
        remote: Option<RemoteTarget>,
        operation: Operation,
    ) -> GitOperationPlan {
        let target_oid = match &operation {
            Operation::CreateBranch { start, .. } => Some(start.clone()),
            Operation::Switch { oid, .. } => Some(oid.clone()),
            Operation::Push { source, .. } => Some(source.clone()),
            _ => None,
        };
        GitOperationPlan {
            preview: GitOperationPreview {
                id: new_id(),
                kind,
                repository: self.0.binding.clone(),
                before,
                paths,
                branch,
                target_oid,
                remote,
                requires_source_lease: matches!(
                    kind,
                    GitOperationKind::SwitchBranch | GitOperationKind::PullFastForward
                ),
                interactive_output_sensitive: true,
            },
            service: self.0.id.clone(),
            operation,
            state: Arc::new(AtomicU8::new(0)),
        }
    }

    /// The callback runs on a worker and returns only after its owned process has
    /// exited/reaped (including cancellation). B03 may attach a dedicated PTY;
    /// no password input or raw process output belongs in ordinary metadata.
    /// No alternate index, index.lock takeover, hook bypass or signing override.
    pub fn execute_with<F>(
        &self,
        plan: &GitOperationPlan,
        gate: &SourceGate,
        resource_root: &Path,
        runner: F,
    ) -> Result<GitOperationResult>
    where
        F: FnOnce(CommandBuilder) -> Result<CommandOutcome>,
    {
        ensure!(
            plan.service == self.0.id,
            "operation plan belongs to another repository service"
        );
        let _serial = self
            .0
            .mutation
            .lock()
            .map_err(|_| anyhow::anyhow!("Git mutation provider unavailable"))?;
        ensure!(
            plan.state.load(Ordering::Acquire) == 0,
            "operation already started; inspect its result instead of retrying"
        );
        ensure_private_dir(resource_root)?;
        let lock_name = format!(
            "git-{}.lock",
            fingerprint(&[self.0.binding.git_dir.as_os_str().as_encoded_bytes()])
        );
        let _process_lock = FileLock::acquire(&resource_root.join(lock_name), true)?;
        let _source_lease = if plan.preview.requires_source_lease {
            Some(gate.reserve_mutation(self.0.binding.identity(), plan.id())?)
        } else {
            None
        };
        self.revalidate(plan)?;
        let resources = resource_root.join(format!("git-operation-{}", plan.id()));
        ensure!(
            !resources.exists(),
            "operation resources already exist; inspect prior outcome"
        );
        ensure_private_dir(&resources)?;
        let _resources = Resources(resources.clone());
        let command = self.operation_command(plan, &resources)?;
        plan.state
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| anyhow::anyhow!("operation already started"))?;
        let execution = runner(command);
        plan.state.store(2, Ordering::Release);
        if let Ok(mut cache) = self.0.status_cache.lock() {
            *cache = None;
        }
        let after = self.refresh().ok();
        let mut result = GitOperationResult {
            id: plan.id().into(),
            kind: plan.preview.kind,
            outcome: GitOutcome::Unknown,
            exit_code: None,
            after,
            commit: None,
            warnings: Vec::new(),
        };
        match execution {
            Ok(outcome) => {
                result.exit_code = outcome.exit_code;
                result.outcome = if outcome.cancelled {
                    GitOutcome::Cancelled
                } else if outcome.exit_code == Some(0) {
                    GitOutcome::Succeeded
                } else if outcome.exit_code.is_some() {
                    GitOutcome::Failed
                } else {
                    GitOutcome::Unknown
                };
                if outcome.output_limited {
                    result.warnings.push(
                        "Operation output was limited; no full transcript is claimed.".into(),
                    );
                }
            }
            Err(_) => result.warnings.push(
                "Executor could not confirm an outcome. Do not automatically retry this operation."
                    .into(),
            ),
        }
        if result.after.is_none() {
            result.outcome = GitOutcome::Unknown;
            result
                .warnings
                .push("Post-operation repository state could not be verified.".into());
        }
        self.assess_effects(plan, &mut result);
        Ok(result)
    }
    /// Explicit no-TTY executor for fixtures/batch callers. Hooks/signing remain
    /// enabled; a required interactive prompt fails rather than being bypassed.
    pub fn run_noninteractive(
        &self,
        plan: &GitOperationPlan,
        gate: &SourceGate,
        resource_root: &Path,
    ) -> Result<GitOperationResult> {
        self.execute_with(plan, gate, resource_root, |builder| {
            let mut command = Command::new(&builder.get_argv()[0]);
            command.args(&builder.get_argv()[1..]);
            command.current_dir(builder.get_cwd().context("Git command cwd missing")?);
            command
                .env_clear()
                .envs(builder.iter_full_env_as_str())
                .env("GIT_TERMINAL_PROMPT", "0");
            command
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0);
            let mut child = command.spawn().context("start owned Git operation")?;
            let deadline = Instant::now() + Duration::from_secs(60);
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        return Ok(CommandOutcome {
                            exit_code: Some(exit_code(status)),
                            cancelled: false,
                            output_limited: false,
                        })
                    }
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Ok(None) => {
                        stop_owned_process(&mut child);
                        return Ok(CommandOutcome {
                            exit_code: None,
                            cancelled: true,
                            output_limited: false,
                        });
                    }
                    Err(error) => {
                        stop_owned_process(&mut child);
                        return Err(error.into());
                    }
                }
            }
        })
    }
    fn revalidate(&self, plan: &GitOperationPlan) -> Result<()> {
        let current = self.refresh()?;
        match &plan.operation {
            Operation::Fetch { .. } => { /* Fetch is independent of local index/HEAD. */ }
            Operation::CreateBranch { .. } | Operation::Push { .. } => ensure!(
                current.revision.head == plan.preview.before.head,
                "HEAD changed since operation review"
            ),
            _ => ensure_revision(&current.revision, &plan.preview.before)?,
        }
        match &plan.operation {
            Operation::Commit { staged_digest, .. } => {
                ensure!(!current.conflicted(), "index has unresolved conflicts");
                ensure!(
                    fingerprint(&[&self.staged_raw()?]) == *staged_digest,
                    "actual staged contents changed since review"
                );
            }
            Operation::CreateBranch { name, start } => {
                ensure!(
                    self.branch_oid(name)?.is_none(),
                    "branch was created elsewhere"
                );
                self.verify_commit(start)?;
            }
            Operation::Switch { name, oid } => {
                ensure!(current.clean(), "working tree changed before branch switch");
                ensure!(
                    self.branch_oid(name)?.as_ref() == Some(oid),
                    "target branch changed since review"
                );
            }
            Operation::Pull {
                remote,
                endpoint_digest,
                ..
            } => {
                ensure!(current.clean(), "working tree changed before pull");
                let remote = self.remote_state(remote)?;
                safe_remote(&remote, false)?;
                ensure!(
                    &remote.fetch_digest == endpoint_digest,
                    "remote target changed since review"
                );
            }
            Operation::Fetch {
                remote,
                endpoint_digest,
            } => {
                let remote = self.remote_state(remote)?;
                safe_remote(&remote, false)?;
                ensure!(
                    &remote.fetch_digest == endpoint_digest,
                    "remote target changed since review"
                );
            }
            Operation::Push {
                remote,
                endpoint_digest,
                source,
                ..
            } => {
                let remote = self.remote_state(remote)?;
                safe_remote(&remote, true)?;
                ensure!(
                    &remote.push_digest == endpoint_digest,
                    "push endpoint changed since review"
                );
                self.verify_commit(source)?;
            }
            _ => {}
        }
        Ok(())
    }
    fn operation_command(
        &self,
        plan: &GitOperationPlan,
        resources: &Path,
    ) -> Result<CommandBuilder> {
        let mut args = self.common_arguments();
        match &plan.operation {
            Operation::Stage(paths) => {
                args.extend(strings(&["add", "--"]));
                args.extend(paths.iter().map(|p| p.as_os_str().into()));
            }
            Operation::Unstage(paths) => {
                if plan.preview.before.head.oid().is_some() {
                    args.extend(strings(&["reset", "--quiet", "HEAD", "--"]));
                } else {
                    args.extend(strings(&["update-index", "--force-remove", "--"]));
                }
                args.extend(paths.iter().map(|p| p.as_os_str().into()));
            }
            Operation::Commit { message, .. } => {
                let path = resources.join("message");
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                    .open(&path)?;
                file.write_all(message.as_bytes())?;
                file.sync_all()?;
                args.extend(strings(&["commit", "--file"]));
                args.push(path.into_os_string());
            }
            Operation::CreateBranch { name, start } => {
                args.extend(strings(&["branch", "--no-track", name, start.as_str()]));
            }
            Operation::Switch { name, .. } => {
                args.extend(strings(&[
                    "checkout",
                    "--no-guess",
                    "--no-recurse-submodules",
                    name,
                    "--",
                ]));
            }
            Operation::Fetch { remote, .. } => {
                args.extend(strings(&[
                    "fetch",
                    "--no-recurse-submodules",
                    "--no-prune",
                    "--no-prune-tags",
                    "--",
                    remote,
                ]));
            }
            Operation::Pull { remote, branch, .. } => {
                args.extend(strings(&[
                    "-c",
                    "rebase.autoStash=false",
                    "-c",
                    "merge.autoStash=false",
                    "pull",
                    "--ff-only",
                    "--no-rebase",
                    "--no-recurse-submodules",
                    "--",
                    remote,
                    &format!("refs/heads/{branch}"),
                ]));
            }
            Operation::Push {
                remote,
                branch,
                source,
                ..
            } => {
                args.extend(strings(&[
                    "push",
                    "--porcelain",
                    "--no-force",
                    "--no-mirror",
                    "--no-follow-tags",
                    "--recurse-submodules=no",
                    "--",
                    remote,
                    &format!("{}:refs/heads/{branch}", source.as_str()),
                ]));
            }
        }
        ensure!(
            args.iter()
                .map(|arg| arg.as_encoded_bytes().len() + 1)
                .sum::<usize>()
                <= ARG_LIMIT,
            "selected paths exceed argument bounds; use a smaller explicit selection"
        );
        let mut command = CommandBuilder::new(self.0.git.path());
        command.args(args);
        command.cwd(&self.0.binding.root);
        command.env_clear();
        for (key, value) in &self.0.environment {
            command.env(key, value);
        }
        command.env("GIT_TERMINAL_PROMPT", "1");
        Ok(command)
    }
    fn assess_effects(&self, plan: &GitOperationPlan, result: &mut GitOperationResult) {
        let Some(after) = &result.after else {
            return;
        };
        if let Operation::Commit { staged_digest, .. } = &plan.operation {
            if after.revision.head != plan.preview.before.head {
                let actual = (|| -> (Option<CommitSummary>, bool) {
                    let commit = self.history(1).ok().and_then(|mut commits| commits.pop());
                    let Some(commit) = &commit else {
                        return (None, false);
                    };
                    let parents_match = match plan.preview.before.head.oid() {
                        Some(before) => commit.parents.as_slice() == [before.clone()],
                        None => commit.parents.is_empty(),
                    };
                    let mut args = strings(&[
                        "diff-tree",
                        "-r",
                        "--raw",
                        "-z",
                        "--no-abbrev",
                        "--no-renames",
                        "--no-commit-id",
                        "--no-ext-diff",
                        "--no-textconv",
                    ]);
                    if let Some(before) = plan.preview.before.head.oid() {
                        args.push(before.as_str().into());
                    } else {
                        args.push("--root".into());
                    }
                    args.push(commit.oid.as_str().into());
                    args.push("--".into());
                    let delta_match = self
                        .read(&args, READ_LIMIT)
                        .ok()
                        .filter(|out| out.status.success())
                        .is_some_and(|out| fingerprint(&[&out.stdout]) == *staged_digest);
                    (commit.clone().into(), parents_match && delta_match)
                })();
                result.commit = actual.0;
                if !actual.1 || result.exit_code != Some(0) {
                    result.outcome = GitOutcome::ChangedAfterExecution;
                    result.warnings.push("A commit exists, but its result differs from the reviewed index or the reported exit. Inspect the actual commit; no retry or rollback was performed.".into());
                }
            } else if result.exit_code == Some(0) {
                result.outcome = GitOutcome::Unknown;
                result
                    .warnings
                    .push("Git returned success but no new HEAD was observed.".into());
            }
        }
        if let Operation::Switch { name, oid } = &plan.operation {
            if result.exit_code == Some(0)
                && (after.revision.head.reference() != Some(format!("refs/heads/{name}").as_str())
                    || after.revision.head.oid() != Some(oid))
            {
                result.outcome = GitOutcome::ChangedAfterExecution;
                result
                    .warnings
                    .push("The resulting branch differs from the reviewed target.".into());
            }
        }
        let remote_check = match &plan.operation {
            Operation::Fetch {
                remote,
                endpoint_digest,
            }
            | Operation::Pull {
                remote,
                endpoint_digest,
                ..
            } => Some((remote, endpoint_digest, false)),
            Operation::Push {
                remote,
                endpoint_digest,
                ..
            } => Some((remote, endpoint_digest, true)),
            _ => None,
        };
        if let Some((name, expected, push)) = remote_check {
            let same = self.remote_state(name).ok().is_some_and(|remote| {
                if push {
                    &remote.push_digest == expected
                } else {
                    &remote.fetch_digest == expected
                }
            });
            if !same {
                result.outcome = GitOutcome::Unknown;
                result.warnings.push("Remote configuration changed or became unavailable during execution; inspect the destination before retrying.".into());
            }
        }
        if !matches!(plan.operation, Operation::Commit { .. })
            && result.exit_code != Some(0)
            && after.revision != plan.preview.before
        {
            result.warnings.push("Repository state changed despite an incomplete/failed operation. Existing changes were preserved.".into());
        }
    }

    fn common_arguments(&self) -> Vec<OsString> {
        // Empty fsmonitor disables it in old pathname-based Git too; "false"
        // would be interpreted as an executable pathname by Git 2.18.
        strings(&[
            "--no-pager",
            "--literal-pathspecs",
            "-c",
            "core.fsmonitor=",
            "-c",
            "color.ui=false",
            "-c",
            "log.showSignature=false",
            "-c",
            "submodule.recurse=false",
            "-c",
            "gc.auto=0",
            "-c",
            "maintenance.auto=false",
        ])
    }
    fn read_config(&self, args: &[OsString], limit: usize) -> Result<Output> {
        self.read_prepared(args, limit, &[])
    }
    fn read(&self, args: &[OsString], limit: usize) -> Result<Output> {
        let filters = self.read_filter_overrides()?;
        self.reject_unsafe_lazy_fetch()?;
        self.read_prepared(args, limit, &filters)
    }
    fn read_prepared(
        &self,
        args: &[OsString],
        limit: usize,
        overrides: &[OsString],
    ) -> Result<Output> {
        let mut command = Command::new(self.0.git.path());
        command
            .args(self.common_arguments())
            .args(overrides)
            .args(args)
            .current_dir(&self.0.binding.root)
            .env_clear()
            .envs(&self.0.environment)
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut child = command.spawn().context("start Git inspection")?;
        let result = collect(&mut child, READ_TIMEOUT, limit);
        if result.is_err() {
            stop_owned_process(&mut child);
        }
        result
    }
    fn read_filter_overrides(&self) -> Result<Vec<OsString>> {
        let output = self.read_config(
            &strings(&[
                "config",
                "--null",
                "--name-only",
                "--get-regexp",
                "^filter\\..*\\.(clean|smudge|process|required)$",
            ]),
            CONFIG_LIMIT,
        )?;
        if output.status.code() == Some(1) {
            return Ok(Vec::new());
        }
        require_success(&output, "inspect conversion filter policy")?;
        let mut drivers = BTreeSet::new();
        for field in output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|f| !f.is_empty())
        {
            let field = text(field)?;
            ensure!(
                !field.contains('=') && !field.chars().any(char::is_control),
                "conversion filter key cannot be safely overridden for inspection"
            );
            let (driver, _) = field
                .rsplit_once('.')
                .context("invalid conversion filter key")?;
            drivers.insert(driver.to_owned());
        }
        ensure!(
            drivers.len() <= 256,
            "too many configured conversion filters"
        );
        let mut overrides = Vec::new();
        for driver in drivers {
            for key in ["clean", "smudge", "process"] {
                overrides.extend(strings(&["-c", &format!("{driver}.{key}=")]));
            }
            overrides.extend(strings(&["-c", &format!("{driver}.required=false")]));
        }
        Ok(overrides)
    }
    fn reject_unsafe_lazy_fetch(&self) -> Result<()> {
        if self.0.version >= (2, 46) {
            return Ok(());
        }
        let output = self.read_config(
            &strings(&[
                "config",
                "--null",
                "--get-regexp",
                "^(extensions\\.partialclone|remote\\..*\\.(promisor|partialclonefilter))$",
            ]),
            CONFIG_LIMIT,
        )?;
        if output.status.code() == Some(1) {
            return Ok(());
        }
        require_success(&output, "inspect partial clone policy")?;
        for entry in output.stdout.split(|b| *b == 0).filter(|e| !e.is_empty()) {
            let (key, value) = split_config_entry(entry)?;
            let enabled = if key.ends_with(".promisor") {
                !matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "false" | "0" | "off" | "no"
                )
            } else {
                !value.is_empty()
            };
            ensure!(!enabled, "this Git version cannot suppress lazy fetch in a partial clone; object inspection is unavailable without network");
        }
        Ok(())
    }
    fn verify_binding(&self) -> Result<()> {
        for (option, expected) in [
            ("--show-toplevel", &self.0.binding.root),
            ("--absolute-git-dir", &self.0.binding.git_dir),
            ("--git-common-dir", &self.0.binding.common_dir),
        ] {
            let actual = self.rev_parse_path(option)?;
            ensure!(
                &actual == expected,
                "repository/worktree binding changed; reconnect explicitly before Git operations"
            );
        }
        ensure!(
            self.rev_parse_path("--absolute-git-dir")? == self.0.binding.git_dir,
            "repository changed during verification"
        );
        Ok(())
    }
    fn rev_parse_path(&self, option: &str) -> Result<PathBuf> {
        let output = self.read_config(&strings(&["rev-parse", option]), CONFIG_LIMIT)?;
        require_success(&output, "verify repository binding")?;
        let path = path_line(&output.stdout, &self.0.binding.root)?;
        path.canonicalize().context("resolve repository binding")
    }
    fn git_path(&self, name: &str) -> Result<PathBuf> {
        let output =
            self.read_config(&strings(&["rev-parse", "--git-path", name]), CONFIG_LIMIT)?;
        require_success(&output, "locate actual Git metadata")?;
        path_line(&output.stdout, &self.0.binding.root)
    }
    fn head(&self) -> Result<Head> {
        let symbolic =
            self.read_config(&strings(&["symbolic-ref", "--quiet", "HEAD"]), CONFIG_LIMIT)?;
        let reference = if symbolic.status.success() {
            Some(text(trim_line(&symbolic.stdout)?)?)
        } else if symbolic.status.code() == Some(1) {
            None
        } else {
            bail!("cannot inspect symbolic HEAD");
        };
        let resolved = self.read(
            &strings(&["rev-parse", "--verify", "HEAD^{commit}"]),
            CONFIG_LIMIT,
        )?;
        if resolved.status.success() {
            let oid = ObjectId::parse(&text(trim_line(&resolved.stdout)?)?)?;
            return Ok(match reference {
                Some(reference) => Head::Branch { reference, oid },
                None => Head::Detached { oid },
            });
        }
        if let Some(reference) = reference {
            let exists = self.read_config(
                &strings(&["show-ref", "--verify", "--quiet", &reference]),
                CONFIG_LIMIT,
            )?;
            if exists.status.code() == Some(1) {
                return Ok(Head::Unborn { reference });
            }
        }
        bail!("HEAD object is unavailable or invalid; it is not assumed unborn")
    }
    fn staged_raw(&self) -> Result<Vec<u8>> {
        let output = self.read(
            &strings(&[
                "diff",
                "--cached",
                "--raw",
                "-z",
                "--no-abbrev",
                "--no-renames",
                "--no-ext-diff",
                "--no-textconv",
                "--ita-invisible-in-index",
                "--",
            ]),
            READ_LIMIT,
        )?;
        require_success(&output, "read full staged index delta")?;
        Ok(output.stdout)
    }
    fn validate_branch(&self, name: &str) -> Result<()> {
        ensure!(
            !name.is_empty()
                && name.len() <= 1024
                && !name.starts_with('-')
                && !name.contains("@{")
                && !name.chars().any(char::is_control),
            "invalid explicit branch name"
        );
        let output = self.read_config(
            &strings(&["check-ref-format", "--branch", name]),
            CONFIG_LIMIT,
        )?;
        require_success(&output, "validate branch name")
    }
    fn branch_oid(&self, name: &str) -> Result<Option<ObjectId>> {
        let output = self.read_config(
            &strings(&[
                "show-ref",
                "--verify",
                "--hash",
                &format!("refs/heads/{name}"),
            ]),
            CONFIG_LIMIT,
        )?;
        if output.status.success() {
            return Ok(Some(ObjectId::parse(&text(trim_line(&output.stdout)?)?)?));
        }
        if output.status.code() == Some(1) {
            return Ok(None);
        }
        // show-ref --verify returns 128 for a missing explicit ref on some Git
        // versions. A listing is an unambiguous local existence check.
        let list = self.read_config(
            &strings(&[
                "for-each-ref",
                "--format=%(objectname)",
                &format!("refs/heads/{name}"),
            ]),
            CONFIG_LIMIT,
        )?;
        require_success(&list, "inspect branch")?;
        if list.stdout.is_empty() {
            Ok(None)
        } else {
            bail!("branch reference could not be verified")
        }
    }
    fn verify_commit(&self, oid: &ObjectId) -> Result<()> {
        let output = self.read(
            &strings(&["cat-file", "-e", &format!("{}^{{commit}}", oid.as_str())]),
            CONFIG_LIMIT,
        )?;
        require_success(&output, "verify starting commit")
    }
    fn remote_state(&self, name: &str) -> Result<RemoteState> {
        ensure!(
            !name.is_empty()
                && name.len() <= 1024
                && !name.starts_with('-')
                && !name.chars().any(|c| c.is_control() || c.is_whitespace()),
            "invalid explicit remote name"
        );
        let listed = self.read_config(&strings(&["remote"]), CONFIG_LIMIT)?;
        require_success(&listed, "list configured remotes")?;
        ensure!(
            listed
                .stdout
                .split(|b| *b == b'\n')
                .any(|entry| entry == name.as_bytes()),
            "configured remote not found"
        );
        let mut raw_counts = Vec::new();
        for key in ["url", "pushurl"] {
            let raw = self.read_config(
                &strings(&[
                    "config",
                    "--null",
                    "--get-all",
                    &format!("remote.{name}.{key}"),
                ]),
                CONFIG_LIMIT,
            )?;
            ensure!(
                raw.status.success() || raw.status.code() == Some(1),
                "cannot read remote configuration"
            );
            let values: Vec<_> = raw
                .stdout
                .split(|b| *b == 0)
                .filter(|v| !v.is_empty())
                .collect();
            for value in &values {
                ensure!(
                    !text(value)?.chars().any(char::is_control),
                    "remote endpoint contains unsupported control characters"
                );
            }
            raw_counts.push(values.len());
        }
        ensure!(raw_counts[0] > 0, "remote has no fetch endpoint");
        let mut endpoints = Vec::new();
        for push in [false, true] {
            let mut args = strings(&["remote", "get-url", "--all"]);
            if push {
                args.push("--push".into());
            }
            args.push(name.into());
            let output = self.read_config(&args, CONFIG_LIMIT)?;
            require_success(&output, "resolve remote endpoints")?;
            let values = output
                .stdout
                .strip_suffix(b"\n")
                .context("incomplete remote endpoint response")?
                .split(|b| *b == b'\n')
                .map(text)
                .collect::<Result<Vec<_>>>()?;
            let expected = if push && raw_counts[1] > 0 {
                raw_counts[1]
            } else {
                raw_counts[0]
            };
            ensure!(
                values.len() == expected
                    && values
                        .iter()
                        .all(|v| !v.is_empty() && !v.chars().any(char::is_control)),
                "remote endpoint rewrite is ambiguous or contains control characters"
            );
            endpoints.push(values);
        }
        let summarize = |values: &[String]| {
            let mut display = Vec::new();
            let mut credentials = false;
            let mut unsupported = false;
            for value in values {
                let (safe, secret, unsafe_transport) = redact_endpoint(value);
                display.push(safe);
                credentials |= secret;
                unsupported |= unsafe_transport;
            }
            (display, credentials, unsupported)
        };
        let (fetch_urls, fetch_credentials, fetch_unsupported) = summarize(&endpoints[0]);
        let (push_urls, push_credentials, push_unsupported) = summarize(&endpoints[1]);
        let digest = |values: &[String]| {
            fingerprint(&values.iter().map(|v| v.as_bytes()).collect::<Vec<_>>())
        };
        Ok(RemoteState {
            view: Remote {
                name: name.into(),
                fetch_urls,
                push_urls,
                embedded_credentials: fetch_credentials || push_credentials,
            },
            fetch_digest: digest(&endpoints[0]),
            push_digest: digest(&endpoints[1]),
            fetch_unsafe: fetch_credentials || fetch_unsupported,
            push_unsafe: push_credentials || push_unsupported,
        })
    }
    fn remote_target(
        &self,
        remote: Remote,
        branch: &str,
        source: Option<ObjectId>,
    ) -> Result<RemoteTarget> {
        let output = self.read_config(
            &strings(&[
                "config",
                "--null",
                "--get-all",
                &format!("remote.{}.fetch", remote.name),
            ]),
            CONFIG_LIMIT,
        )?;
        ensure!(
            output.status.success() || output.status.code() == Some(1),
            "cannot inspect local remote tracking configuration"
        );
        let source_ref = format!("refs/heads/{branch}");
        let mut destinations = BTreeSet::new();
        for spec in output.stdout.split(|b| *b == 0).filter(|v| !v.is_empty()) {
            let spec = text(spec)?;
            let spec = spec.strip_prefix('+').unwrap_or(&spec);
            let Some((from, to)) = spec.split_once(':') else {
                continue;
            };
            if from == source_ref {
                destinations.insert(to.to_owned());
            } else if let (Some((prefix, suffix)), Some((to_prefix, to_suffix))) =
                (from.split_once('*'), to.split_once('*'))
            {
                if !suffix.contains('*') && !to_suffix.contains('*') {
                    if let Some(middle) = source_ref
                        .strip_prefix(prefix)
                        .and_then(|s| s.strip_suffix(suffix))
                    {
                        destinations.insert(format!("{to_prefix}{middle}{to_suffix}"));
                    }
                }
            }
        }
        let mut tip = None;
        if destinations.len() == 1 {
            let reference = destinations.first().unwrap();
            if reference.starts_with("refs/") && !reference.chars().any(char::is_control) {
                let output = self.read(
                    &strings(&["rev-parse", "--verify", &format!("{reference}^{{commit}}")]),
                    CONFIG_LIMIT,
                )?;
                if output.status.success() {
                    tip = Some(ObjectId::parse(&text(trim_line(&output.stdout)?)?)?);
                }
            }
        }
        let (mut ahead, mut behind) = (None, None);
        if let (Some(tip), Some(source)) = (&tip, &source) {
            let output = self.read(
                &strings(&[
                    "rev-list",
                    "--left-right",
                    "--count",
                    &format!("{}...{}", tip.as_str(), source.as_str()),
                    "--",
                ]),
                CONFIG_LIMIT,
            )?;
            if output.status.success() {
                let counts = text(trim_line(&output.stdout)?)?;
                let counts = counts.split_whitespace().collect::<Vec<_>>();
                if counts.len() == 2 {
                    behind = counts[0].parse().ok();
                    ahead = counts[1].parse().ok();
                }
            }
        }
        Ok(RemoteTarget {
            remote,
            destination: Some(source_ref),
            source,
            cached_remote_tip: tip,
            ahead,
            behind,
        })
    }
}

fn strings(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}
fn require_success(output: &Output, action: &str) -> Result<()> {
    ensure!(
        output.status.success(),
        "Git could not {action} (exit {:?}); inspect through the dedicated operation view",
        output.status.code()
    );
    Ok(())
}
fn parse_version(bytes: &[u8]) -> Result<(u32, u32)> {
    let value = text(bytes)?;
    let value = value
        .strip_prefix("git version ")
        .context("invalid Git version response")?;
    let mut numbers = value.split('.');
    Ok((
        numbers
            .next()
            .context("missing Git major version")?
            .parse()?,
        numbers
            .next()
            .context("missing Git minor version")?
            .parse()?,
    ))
}
fn trim_line(bytes: &[u8]) -> Result<&[u8]> {
    bytes
        .strip_suffix(b"\n")
        .context("incomplete Git line response")
}
fn path_line(bytes: &[u8], root: &Path) -> Result<PathBuf> {
    let bytes = trim_line(bytes)?;
    ensure!(
        !bytes.is_empty() && !bytes.contains(&0),
        "invalid Git metadata path"
    );
    let path = PathBuf::from(OsString::from_vec(bytes.to_vec()));
    Ok(if path.is_absolute() {
        path
    } else {
        root.join(path)
    })
}
fn fingerprint(parts: &[&[u8]]) -> String {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part);
    }
    format!("{:x}", digest.finalize())
}
fn index_file_digest(path: &Path) -> Result<String> {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok("absent".into()),
        Err(error) => return Err(error).context("open actual index"),
    };
    let before = file.metadata()?;
    ensure!(
        before.is_file() && before.len() <= INDEX_LIMIT as u64,
        "index is not a bounded regular file"
    );
    let mut bytes = Vec::new();
    (&mut file)
        .take(INDEX_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= INDEX_LIMIT, "index exceeded read limit");
    let after = file.metadata()?;
    ensure!(
        before.dev() == after.dev()
            && before.ino() == after.ino()
            && before.len() == after.len()
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec(),
        "index changed while reading"
    );
    Ok(fingerprint(&[&bytes]))
}
fn validate_index(bytes: &[u8]) -> Result<()> {
    ensure!(
        bytes.is_empty() || bytes.ends_with(&[0]),
        "incomplete actual index response"
    );
    for entry in bytes.split(|b| *b == 0).filter(|v| !v.is_empty()) {
        let split = entry
            .iter()
            .position(|b| *b == b'\t')
            .context("invalid index entry")?;
        GitPath::from_bytes(entry[split + 1..].to_vec())?;
        let header = text(&entry[..split])?;
        let fields: Vec<_> = header.split(' ').collect();
        ensure!(
            fields.len() == 3 && matches!(fields[2], "0" | "1" | "2" | "3"),
            "invalid index stage"
        );
        ObjectId::parse(fields[1])?;
    }
    Ok(())
}
fn split_config_entry(bytes: &[u8]) -> Result<(String, String)> {
    let index = bytes
        .iter()
        .position(|b| *b == b'\n')
        .context("invalid config entry")?;
    Ok((text(&bytes[..index])?, text(&bytes[index + 1..])?))
}
fn validate_selection(paths: &[GitPath]) -> Result<()> {
    ensure!(
        !paths.is_empty()
            && paths.len() <= 4096
            && paths.iter().map(|p| p.bytes().len() + 1).sum::<usize>() < ARG_LIMIT - 4096,
        "select a bounded, nonempty set of files"
    );
    Ok(())
}
fn ensure_revision(actual: &GitRevision, expected: &GitRevision) -> Result<()> {
    ensure!(
        actual == expected,
        "repository HEAD/index changed since review; refresh and review again"
    );
    Ok(())
}
fn display_patch(bytes: &[u8]) -> String {
    bytes
        .split(|b| *b == b'\n')
        .map(display_bytes)
        .collect::<Vec<_>>()
        .join("\n")
}
fn exit_code(status: std::process::ExitStatus) -> u32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .map(|v| v as u32)
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0) as u32)
}
fn safe_remote(state: &RemoteState, push: bool) -> Result<()> {
    ensure!(!(if push { state.push_unsafe } else { state.fetch_unsafe }), "remote endpoint contains embedded credentials or an unsupported transport; configure a credential helper/agent and a standard endpoint before this operation");
    Ok(())
}
fn redact_endpoint(value: &str) -> (String, bool, bool) {
    if let Some((scheme, rest)) = value.split_once("://") {
        if !matches!(scheme, "http" | "https" | "ssh" | "git" | "file") {
            return ("[unsupported transport]".into(), false, true);
        }
        let (rest, query) = rest
            .split_once(['?', '#'])
            .map_or((rest, false), |(prefix, _)| (prefix, true));
        let (authority, path) = rest
            .split_once('/')
            .map_or((rest, ""), |(host, path)| (host, path));
        let (authority, credentials) = match authority.rsplit_once('@') {
            Some((userinfo, host)) => (
                format!("[redacted]@{host}"),
                scheme != "ssh" || userinfo.contains(':'),
            ),
            None => (authority.to_owned(), false),
        };
        let slash = if path.is_empty() { "" } else { "/" };
        return (
            format!(
                "{scheme}://{authority}{slash}{path}{}",
                if query { "[redacted parameters]" } else { "" }
            ),
            credentials || query,
            false,
        );
    }
    if value.contains("::") {
        return ("[unsupported transport]".into(), false, true);
    }
    // scp-like SSH endpoints have a colon before the first slash. Local paths
    // containing @/? are filenames, not credential-bearing network URLs.
    if let Some((host, path)) = value.split_once(':') {
        if !host.contains('/') {
            let host = host
                .rsplit_once('@')
                .map_or(host.to_owned(), |(_, host)| format!("[redacted]@{host}"));
            return (format!("{host}:{path}"), false, false);
        }
    }
    (value.into(), false, false)
}
