//! Finite, local Git proof through the distributed candidate's actual host API.
use crate::client::Client;
use crate::git_wire::*;
use crate::model::ShellConfig;
use crate::project::{ConnectDraft, ProjectService, TerminalDraft};
use crate::store::{ensure_private_dir, Store};
use anyhow::{bail, ensure, Context, Result};
use base64::Engine;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

const JOB_BUDGET: Duration = Duration::from_secs(10);
const STATUS_BUDGET: Duration = Duration::from_secs(1);
struct FixtureChild(Child);
impl Drop for FixtureChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}
fn setup_git(root: &Path, env: &BTreeMap<String, String>, args: &[&str]) -> Result<()> {
    let mut child = FixtureChild(
        crate::probe_host::probe_child_command(Path::new("/usr/bin/git"))
            .current_dir(root)
            .env_clear()
            .envs(env)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("spawn synthetic Git fixture setup")?,
    );
    let start = Instant::now();
    loop {
        if let Some(status) = child.0.try_wait()? {
            ensure!(status.success(), "synthetic Git fixture setup failed");
            return Ok(());
        }
        ensure!(start.elapsed() < JOB_BUDGET, "Git fixture setup timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn job(client: &mut Client, task: GitTask) -> Result<GitValue> {
    let id = client.git_submit(task)?.id;
    let start = Instant::now();
    loop {
        let job = client.git_job(&id)?;
        match job.state {
            GitJobState::Ready => return job.result.context("Git probe job has no result"),
            GitJobState::Failed => bail!("Git probe job failed: {}", job.error.unwrap_or_default()),
            GitJobState::Pending | GitJobState::Running => {}
        }
        ensure!(start.elapsed() < JOB_BUDGET, "Git probe job timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn execute(client: &mut Client, task: GitTask) -> Result<GitOperationResult> {
    let GitValue::Plan(plan) = job(client, task)? else {
        bail!("Git probe did not receive an immutable operation plan");
    };
    let operation = client.git_execute(&plan.id, 24, 100)?;
    let start = Instant::now();
    loop {
        let current = client
            .git_operation_snapshot(&operation.id, None)?
            .operation;
        if matches!(
            current.state,
            GitOperationState::Complete | GitOperationState::Unknown
        ) {
            let result = current
                .result
                .context("Git probe operation has no confirmed result")?;
            ensure!(
                result.outcome == GitOutcome::Succeeded,
                "Git probe operation did not succeed"
            );
            return Ok(result);
        }
        ensure!(
            start.elapsed() < JOB_BUDGET,
            "Git probe operation timed out"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn path_matches(path: &GitPath, expected: &str) -> bool {
    path.bytes_base64 == base64::engine::general_purpose::STANDARD.encode(expected.as_bytes())
}

pub(crate) fn run(
    client: &mut Client,
    store: &Store,
    sandbox: &Path,
    shell: &Path,
    env: &BTreeMap<String, String>,
) -> Result<Value> {
    let root = sandbox.join("git-proof");
    let external = sandbox.join("git-external-test");
    ensure_private_dir(&root)?;
    ensure_private_dir(&external)?;
    for args in [
        vec!["init", "--quiet"],
        vec!["symbolic-ref", "HEAD", "refs/heads/main"],
        vec!["config", "user.name", "Synthetic Probe"],
        vec!["config", "user.email", "probe@example.invalid"],
        vec!["config", "commit.gpgSign", "false"],
    ] {
        setup_git(&root, env, &args)?;
    }
    std::fs::write(root.join("already-staged"), "staged before TUI selection\n")?;
    setup_git(&root, env, &["add", "--", "already-staged"])?;
    let selected = "selected 한글 [literal]";
    std::fs::write(
        root.join(selected),
        "selected through exact host snapshot index\n",
    )?;
    let service = ProjectService { store };
    let preview = service.preview_connect(ConnectDraft {
        name: "Synthetic packaged Git proof".into(),
        root: root.clone(),
        shell: ShellConfig {
            executable: shell.into(),
            login: false,
            init_cwd: external.clone(),
            sources: Vec::new(),
            trusted_digest: None,
        },
        terminals: vec![TerminalDraft {
            name: "External test definition".into(),
            cwd: external,
            sources: Vec::new(),
            persistent: true,
        }],
    })?;
    let project = service.create(preview.revision, preview)?;
    let GitValue::Open(context) = job(
        client,
        GitTask::Open {
            project: project.id,
            repository: None,
            env: env.clone(),
        },
    )?
    else {
        bail!("Git probe context missing");
    };
    ensure!(
        context.repository.root == root,
        "Git probe used a different repository"
    );
    let start = Instant::now();
    let GitValue::Status(status) = job(
        client,
        GitTask::Status {
            context: context.id.clone(),
            refresh: true,
        },
    )?
    else {
        bail!("Git probe status missing");
    };
    let status_ms = start.elapsed().as_millis() as u64;
    ensure!(
        start.elapsed() <= STATUS_BUDGET,
        "Git probe status exceeded 1000 ms budget"
    );
    let entry = status
        .snapshot
        .entries
        .iter()
        .position(|entry| path_matches(&entry.path, selected))
        .context("Git probe literal filename missing from status")?;
    execute(
        client,
        GitTask::Stage {
            context: context.id.clone(),
            snapshot: status.snapshot_id,
            entries: vec![entry],
        },
    )?;
    let GitValue::CommitReview(review) = job(
        client,
        GitTask::CommitReview {
            context: context.id.clone(),
        },
    )?
    else {
        bail!("Git probe full index review missing");
    };
    ensure!(
        review.staged.len() == 2
            && review
                .staged
                .iter()
                .any(|entry| path_matches(&entry.path, "already-staged"))
            && review
                .staged
                .iter()
                .any(|entry| path_matches(&entry.path, selected)),
        "Git probe review did not include the entire actual index"
    );
    let commit = execute(
        client,
        GitTask::Commit {
            context: context.id.clone(),
            review: review.review_id,
            message: "Synthetic packaged Git proof".into(),
        },
    )?;
    let GitValue::History(history) = job(
        client,
        GitTask::History {
            context: context.id.clone(),
            limit: 10,
        },
    )?
    else {
        bail!("Git probe history missing");
    };
    ensure!(
        history.len() == 1
            && commit
                .commit
                .as_ref()
                .is_some_and(|created| created.oid == history[0].oid),
        "Git probe history did not observe its actual commit"
    );
    let GitValue::Status(after) = job(
        client,
        GitTask::Status {
            context: context.id,
            refresh: true,
        },
    )?
    else {
        bail!("Git probe final status missing");
    };
    ensure!(
        after.snapshot.clean(),
        "Git probe commit left staged or worktree changes"
    );
    Ok(json!({
        "scope": "actual same-candidate host Git operations on a synthetic local repository; no network or field acceptance",
        "checks": {"saved_repository_target": "PASS", "status": "PASS", "literal_path_stage": "PASS", "entire_index_review": "PASS", "actual_commit": "PASS", "history": "PASS", "clean_after_commit": "PASS"},
        "measurements": {"status_ms": status_ms}, "budgets": {"status_ms": 1000}, "budget_result": "PASS"
    }))
}
