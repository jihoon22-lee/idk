//! Same-candidate registered-run proof using finite synthetic csh programs.
//! This package probe needs neither a compiler nor Python and makes no field claim.
use crate::client::Client;
use crate::model::{new_id, EditorConfig, FailurePolicy, ShellConfig, TaskDefinition, TaskLogging};
use crate::project::{ConnectDraft, LaunchEnvironment, ProjectService};
use crate::run_wire::*;
use crate::store::{ensure_private_dir, Store};
use anyhow::{bail, ensure, Context, Result};
use base64::Engine;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};
const JOB_BUDGET: Duration = Duration::from_secs(10);
const SCREEN_BUDGET: Duration = Duration::from_secs(1);
fn job(client: &mut Client, request: RunRequest) -> Result<RunResult> {
    let mut job = client.run(request)?;
    let deadline = Instant::now() + JOB_BUDGET;
    loop {
        match job.state {
            RunJobState::Complete => return job.result.context("packaged run job has no result"),
            RunJobState::Failed | RunJobState::Cancelled => {
                bail!("packaged run job failed: {}", job.error.unwrap_or_default())
            }
            RunJobState::Pending => {}
        }
        ensure!(
            Instant::now() < deadline,
            "packaged run job exceeded 10 seconds"
        );
        std::thread::sleep(Duration::from_millis(20));
        job = client.run(RunRequest::Job { job_id: job.job_id })?;
    }
}
fn finished(client: &mut Client, id: &str) -> Result<RunInfo> {
    let deadline = Instant::now() + JOB_BUDGET;
    loop {
        let RunResult::Run(run) = job(client, RunRequest::Info { run_id: id.into() })? else {
            bail!("unexpected run state reply");
        };
        if !run.state.is_live() && run.cleanup_confirmed && run.log.state != LogState::Recording {
            return Ok(run);
        }
        ensure!(
            Instant::now() < deadline,
            "packaged run did not finish owned cleanup/logging"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn start(
    client: &mut Client,
    project: &str,
    task: &str,
    environment: &BTreeMap<String, String>,
) -> Result<RunStartReply> {
    let RunResult::Started(started) = job(
        client,
        RunRequest::Start {
            project_id: project.into(),
            task_id: task.into(),
            operation_id: new_id(),
            environment: environment.clone(),
            parallel: false,
            rows: 24,
            cols: 100,
        },
    )?
    else {
        bail!("unexpected packaged run start reply");
    };
    ensure!(
        !started.existing,
        "new synthetic run unexpectedly reused prior execution"
    );
    Ok(started)
}
fn approve(
    client: &mut Client,
    project: &str,
    task: &str,
    environment: &BTreeMap<String, String>,
) -> Result<()> {
    let RunResult::Review(review) = job(
        client,
        RunRequest::ReviewTask {
            project_id: project.into(),
            task_id: task.into(),
            environment: environment.clone(),
        },
    )?
    else {
        bail!("unexpected packaged task review reply");
    };
    ensure!(
        review.common_approved,
        "synthetic common initialization was not approved"
    );
    ensure!(
        matches!(
            job(
                client,
                RunRequest::ApproveTask {
                    revision: review.revision,
                    project_id: project.into(),
                    task_id: task.into(),
                    digest: review.digest,
                    environment: environment.clone()
                }
            )?,
            RunResult::Approved
        ),
        "unexpected packaged approval reply"
    );
    Ok(())
}
fn initialization_count(root: &Path) -> Result<usize> {
    Ok(std::fs::read_to_string(root.join("run-initializations"))?
        .lines()
        .count())
}
pub(crate) fn run(
    client: &mut Client,
    store: &Store,
    sandbox: &Path,
    shell: &Path,
    launcher: &Path,
    environment: &BTreeMap<String, String>,
) -> Result<Value> {
    let root = sandbox.join("run-proof");
    ensure_private_dir(&root)?;
    let common = root.join("common.csh");
    std::fs::write(&common,"set idk_run_local = kept\nalias idk_run_alias 'echo IDK_RUN_ALIAS_OK'\necho once >> run-initializations\n")?;
    let source = root.join("diagnostic 한글.cpp");
    std::fs::write(&source, "first\nsecond\nthird\n")?;
    let service = ProjectService { store };
    let preview = service.preview_connect(ConnectDraft {
        name: "Synthetic packaged Run proof".into(),
        root: root.clone(),
        shell: ShellConfig {
            executable: shell.into(),
            login: false,
            init_cwd: root.clone(),
            sources: vec![common.into()],
            trusted_digest: None,
        },
        terminals: vec![],
    })?;
    let project = service.create(preview.revision, preview)?;
    let review = service.review_initialization(
        &project.id,
        &LaunchEnvironment::from_variables(environment.clone())?,
    )?;
    ensure!(
        review.common.error.is_none(),
        "synthetic run initialization review failed"
    );
    service.approve_initialization(review.revision, review)?;
    let definition = |name: &str, command: &str| TaskDefinition {
        id: new_id(),
        name: name.into(),
        command: command.into(),
        cwd: root.clone(),
        sources: vec![],
        artifact: None,
        approved_digest: None,
        steps: vec![],
        failure_policy: FailurePolicy::Stop,
        logging: TaskLogging::Raw,
        interactive: false,
        build_outputs: vec![],
        artifact_from_task: None,
        timeout_seconds: Some(10),
    };
    let build=definition("Synthetic diagnostic command","idk_run_alias\necho IDK_RUN_STATE:${idk_run_local}\necho 'diagnostic 한글.cpp:3:2: error: synthetic packaged diagnostic'\n/bin/sh -c 'exit 7'");
    let cancel = definition(
        "Synthetic cancellation command",
        "echo IDK_RUN_CANCEL_READY\nsleep 30",
    );
    for task in [&build, &cancel] {
        let revision = store.load()?.revision;
        ensure!(
            matches!(
                job(
                    client,
                    RunRequest::SaveTask {
                        revision,
                        project_id: project.id.clone(),
                        task: task.clone()
                    }
                )?,
                RunResult::Task(_)
            ),
            "synthetic task registration failed"
        );
        approve(client, &project.id, &task.id, environment)?;
    }
    let start_time = Instant::now();
    let first = start(client, &project.id, &build.id, environment)?;
    let start_ms = start_time.elapsed().as_millis() as u64;
    ensure!(
        start_time.elapsed() <= JOB_BUDGET,
        "packaged Run start exceeded 10 seconds"
    );
    let session = first
        .run
        .session_id
        .as_ref()
        .context("packaged run has no owned session")?;
    let reconnect = Instant::now();
    let host = client.info().host_instance.clone();
    let mut reconnected = Client::connect_with_launcher(store, launcher)?;
    ensure!(
        reconnected.info().host_instance == host,
        "packaged Run reconnect reached a replacement host"
    );
    let attached = reconnected.attach(session, false)?;
    loop {
        let reply = reconnected.snapshot(session, None)?;
        if reply
            .screen
            .is_some_and(|screen| screen.text().contains("IDK_RUN_STATE:kept"))
        {
            break;
        }
        ensure!(
            reconnect.elapsed() <= SCREEN_BUDGET,
            "packaged Run first retained screen exceeded 1000ms"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let screen_ms = reconnect.elapsed().as_millis() as u64;
    ensure!(
        reconnect.elapsed() <= SCREEN_BUDGET,
        "packaged Run reattach first screen exceeded 1000ms"
    );
    if attached.state.is_live() {
        reconnected.detach(session, attached.input_epoch)?;
    }
    drop(reconnected);
    let first = finished(client, &first.run.run_id)?;
    ensure!(
        first.state == RunState::Failed && first.exit_code == Some(7) && first.signal.is_none(),
        "packaged run did not retain actual synthetic failure7"
    );
    ensure!(
        initialization_count(&root)? == 1,
        "first packaged Run did not initialize exactly once"
    );
    let RunResult::Log(log) = job(
        client,
        RunRequest::Log {
            run_id: first.run_id.clone(),
            generation: first.log.generation,
            offset: 0,
            limit: 65536,
        },
    )?
    else {
        bail!("unexpected packaged raw log reply");
    };
    let raw = base64::engine::general_purpose::STANDARD.decode(log.data_base64)?;
    let text = std::str::from_utf8(&raw)?;
    ensure!(
        text.contains("IDK_RUN_ALIAS_OK")
            && text.contains("IDK_RUN_STATE:kept")
            && text.contains("synthetic packaged diagnostic"),
        "packaged raw log lost initialized shell state or diagnostic"
    );
    ensure!(
        first.log.state == LogState::Complete
            && first.log.bytes > 0
            && first.log.bytes <= first.log.limit_bytes,
        "packaged raw log was incomplete/unbounded"
    );
    let RunResult::Problems(problems) = job(
        client,
        RunRequest::Problems {
            run_id: first.run_id.clone(),
        },
    )?
    else {
        bail!("unexpected packaged Problems reply");
    };
    let problem = problems
        .problems
        .iter()
        .find(|problem| problem.message.contains("synthetic packaged diagnostic"))
        .context("packaged diagnostic did not reach Problems")?;
    ensure!(
        problem
            .range
            .as_ref()
            .is_some_and(|range| range.line == 3 && range.column == Some(2)),
        "packaged diagnostic position changed"
    );
    let receipt = root.join("editor-receipt");
    let config = EditorConfig {
        executable: "/bin/sh".into(),
        args: vec![
            "-c".into(),
            "printf '%s\\n' \"$2\" \"$3\" > \"$4\"".into(),
            "idk-synthetic-editor".into(),
            "--".into(),
            "{file}".into(),
            "{line}".into(),
            receipt
                .to_str()
                .context("receipt path is not UTF-8")?
                .into(),
        ],
        external_gui: false,
    };
    let revision = store.load()?.revision;
    ensure!(
        matches!(
            job(
                client,
                RunRequest::SaveEditor {
                    revision,
                    project_id: project.id.clone(),
                    config
                }
            )?,
            RunResult::EditorSaved
        ),
        "synthetic editor configuration was not saved"
    );
    let RunResult::EditorReview { review_id, review } = job(
        client,
        RunRequest::EditorReview {
            run_id: first.run_id.clone(),
            problem_id: problem.id.clone(),
            log_generation: first.log.generation,
        },
    )?
    else {
        bail!("unexpected packaged editor review");
    };
    ensure!(
        review.file == source && review.line == Some(3),
        "packaged editor reviewed a different source location"
    );
    let RunResult::EditorOpened { session_id } = job(
        client,
        RunRequest::EditorOpen {
            review_id,
            environment: environment.clone(),
            rows: 24,
            cols: 100,
        },
    )?
    else {
        bail!("unexpected packaged editor open reply");
    };
    let deadline = Instant::now() + JOB_BUDGET;
    loop {
        let editor = client.snapshot(&session_id, None)?;
        if !editor.session.state.is_live() {
            ensure!(
                editor
                    .session
                    .exit
                    .is_some_and(|exit| exit.code == 0 && exit.signal.is_none()),
                "synthetic editor process failed"
            );
            break;
        }
        ensure!(
            Instant::now() < deadline,
            "synthetic editor cleanup timed out"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    ensure!(
        std::fs::read_to_string(receipt)? == format!("{}\n3\n", source.display()),
        "source path/line did not remain structured literal editor argv"
    );
    let repeat = start(client, &project.id, &build.id, environment)?;
    let repeat = finished(client, &repeat.run.run_id)?;
    ensure!(
        repeat.run_id != first.run_id
            && repeat.state == RunState::Failed
            && repeat.exit_code == Some(7),
        "explicit repeated build reused or misclassified a run"
    );
    ensure!(
        initialization_count(&root)? == 2,
        "repeat/reconnect source count changed unexpectedly"
    );
    let cancel = start(client, &project.id, &cancel.id, environment)?;
    let cancel_session = cancel
        .run
        .session_id
        .as_ref()
        .context("cancel run has no owned session")?;
    let deadline = Instant::now() + JOB_BUDGET;
    loop {
        if client
            .snapshot(cancel_session, None)?
            .screen
            .is_some_and(|screen| screen.text().contains("IDK_RUN_CANCEL_READY"))
        {
            break;
        }
        ensure!(
            Instant::now() < deadline,
            "synthetic cancellation command did not start"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    ensure!(
        matches!(
            job(
                client,
                RunRequest::Cancel {
                    run_id: cancel.run.run_id.clone(),
                    force: true
                }
            )?,
            RunResult::Run(_)
        ),
        "synthetic cancellation request failed"
    );
    let cancelled = finished(client, &cancel.run.run_id)?;
    ensure!(
        cancelled.state == RunState::Cancelled
            && cancelled.cancel_requested
            && cancelled.cleanup_confirmed,
        "synthetic cancellation was not confirmed through owned cleanup"
    );
    ensure!(
        initialization_count(&root)? == 3,
        "cancellation run did not initialize once"
    );
    ensure!(
        client.list(Some(&project.id))?.is_empty(),
        "registered run/editor sessions leaked into ordinary shell definitions"
    );
    Ok(
        json!({"scope":"actual same-candidate host and csh with synthetic diagnostic/editor programs; no compiler/Python or field acceptance", "candidate_build_id":client.info().build_id,"checks":{"same_shell_state_and_init_once":"PASS","actual_failure7":"PASS","raw_log_to_problem":"PASS","sealed_editor_literal_argv":"PASS","explicit_repeat":"PASS","reattach_without_reexecution":"PASS","cancel_and_owned_cleanup":"PASS","ordinary_shell_list_separate":"PASS"},"measurements":{"run_start_ms":start_ms,"reattach_first_screen_ms":screen_ms,"retained_log_bytes":first.log.bytes,"observed_log_bytes":first.log.observed_bytes,"problem_count":problems.problems.len(),"initialization_count":3},"budgets":{"run_start_ms":10000,"reattach_first_screen_ms":1000,"log_bytes":first.log.limit_bytes},"budget_result":"PASS"}),
    )
}
