use anyhow::{bail, ensure, Context, Result};
use clap::Subcommand;
use idk_workspace::client::Client;
use idk_workspace::editor::EditorService;
use idk_workspace::model::{
    new_id, valid_id, EditorConfig, FailurePolicy, SourceSpec, TaskDefinition, TaskExecution,
    TaskLogging, TaskStep,
};
use idk_workspace::project::{LaunchEnvironment, ProjectService};
use idk_workspace::run_wire::{RunInfo, RunJobState, RunRequest, RunResult, RunState};
use idk_workspace::store::Store;
use idk_workspace::task::TaskService;
use std::fs::OpenOptions;
use std::io::{IsTerminal, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(clap::Args)]
pub struct AddTaskArgs {
    project: String,
    name: String,
    #[arg(long)]
    cwd: Option<PathBuf>,
    #[arg(long, conflicts_with_all = ["steps", "program", "interpreter"], required_unless_present_any = ["steps", "program", "interpreter"])]
    command: Option<String>,
    /// Repeat NAME=CSH_PROGRAM; every step runs in the same initialized csh.
    #[arg(long = "step")]
    steps: Vec<String>,
    /// Execute directly, with literal arguments and pipe output by default.
    #[arg(long, conflicts_with_all = ["steps", "interpreter", "source", "source_arg"])]
    program: Option<PathBuf>,
    /// Explicit interpreter for --script-file; no implicit shell initialization.
    #[arg(long, requires = "script_file", conflicts_with_all = ["steps", "source", "source_arg"])]
    interpreter: Option<PathBuf>,
    #[arg(long, requires = "interpreter")]
    script_file: Option<PathBuf>,
    /// Repeat for each literal program/interpreter argument.
    #[arg(long = "arg", allow_hyphen_values = true)]
    args: Vec<String>,
    /// Explicit environment override, repeated NAME=VALUE.
    #[arg(long = "env")]
    env: Vec<String>,
    #[arg(long)]
    source: Vec<PathBuf>,
    #[arg(long, allow_hyphen_values = true)]
    source_arg: Vec<String>,
    #[arg(long)]
    artifact: Option<PathBuf>,
    #[arg(long)]
    artifact_from_task: Option<String>,
    #[arg(long)]
    build_output: Vec<PathBuf>,
    #[arg(long)]
    timeout_seconds: Option<u64>,
    #[arg(long)]
    continue_on_error: bool,
    /// Use a dedicated interactive TTY; persistent output logging is disabled.
    #[arg(long)]
    interactive: bool,
    #[arg(long)]
    no_log: bool,
}

#[derive(Subcommand)]
pub enum TaskCommand {
    /// List registered definitions; never execute their commands.
    List { project: String },
    /// Register a process, explicit script, or legacy csh program without executing it.
    Add(Box<AddTaskArgs>),
    /// Save a complete TOML task definition, replacing only its matching ID.
    Save { project: String, file: PathBuf },
    /// Review the exact task and common initialization before first/changed execution.
    Review { project: String, task: String },
    /// Approve the freshly reviewed task definition; common initialization needs prior approval.
    Approve {
        project: String,
        task: String,
        #[arg(long)]
        yes: bool,
    },
    /// Remove only a saved definition; active runs and logs remain available.
    Remove {
        project: String,
        task: String,
        #[arg(long)]
        yes: bool,
    },
    /// Configure an external editor with literal argv (for example +{line} -- {file}).
    Editor {
        project: String,
        executable: PathBuf,
        /// Repeat once per literal argument; {file} must stand alone after --.
        #[arg(long = "arg", allow_hyphen_values = true)]
        args: Vec<String>,
        /// Explicitly opt into a configured external GUI tool.
        #[arg(long)]
        external_gui: bool,
    },
}
#[derive(Subcommand)]
pub enum RunCommand {
    List {
        project: Option<String>,
    },
    Start {
        project: String,
        task: String,
        /// Explicit replay key; a retained existing operation is never run twice.
        #[arg(long)]
        operation_id: Option<String>,
        #[arg(long)]
        parallel: bool,
        /// Review and approve this exact task before starting; common initialization remains separate.
        #[arg(long)]
        approve: bool,
        /// Wait for observed completion. Interrupting this client does not cancel the run.
        #[arg(long)]
        wait: bool,
        #[arg(long, default_value_t = 24)]
        rows: u16,
        #[arg(long, default_value_t = 80)]
        cols: u16,
    },
    Status {
        run: String,
    },
    Wait {
        run: String,
    },
    Attach {
        run: String,
        #[arg(long)]
        takeover: bool,
    },
    Cancel {
        run: String,
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        force: bool,
    },
    /// Acknowledge externally checked cleanup of a recovered Unknown; never signal saved PIDs.
    Reconcile {
        run: String,
        #[arg(long)]
        yes: bool,
    },
    /// Read a bounded raw-log page as JSON; its data field is base64.
    Log {
        run: String,
        #[arg(long, default_value_t = 0)]
        offset: u64,
        #[arg(long, default_value_t = 65536)]
        limit: usize,
    },
    Search {
        run: String,
        query: String,
    },
    Problems {
        run: String,
    },
    /// Read findings (default) or a bounded raw result body page.
    Result {
        run: String,
        #[arg(long)]
        raw: bool,
        #[arg(long, default_value_t = 0)]
        offset: u64,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long)]
        json: bool,
    },
    /// Keep a baseline outside ordinary 64-run/30-day rotation (16 runs / 128 MiB maximum).
    Pin {
        run: String,
        #[arg(long)]
        json: bool,
    },
    /// Return a baseline to ordinary retention. Export it first to keep a separate copy.
    Unpin {
        run: String,
        #[arg(long)]
        json: bool,
    },
    /// Write original result bytes to a new file; existing destinations are preserved.
    Export {
        run: String,
        path: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Compare matching producer identities and scopes; incomplete evidence cannot resolve findings.
    Compare {
        baseline: String,
        run: String,
        #[arg(long)]
        json: bool,
    },
    /// Observe bounded producer progress independently of actual Run exit/cleanup.
    Events {
        run: String,
        #[arg(long)]
        json: bool,
    },
    /// Review/open an exact structured finding, independently of raw-log offsets.
    FindingEditor {
        run: String,
        index: usize,
        #[arg(long)]
        body_sha256: String,
        #[arg(long)]
        yes: bool,
    },
    /// Review/open one diagnostic in the configured dedicated editor terminal.
    Editor {
        run: String,
        problem: String,
        #[arg(long)]
        yes: bool,
    },
}
fn output(value: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
fn project(store: &Store, selector: &str) -> Result<String> {
    ProjectService { store }.resolve_project_id(selector)
}
fn task_definition(store: &Store, project: &str, selector: &str) -> Result<TaskDefinition> {
    let workspace = store.load()?;
    let project = workspace.project(project)?;
    if let Some(task) = project.tasks.iter().find(|task| task.id == selector) {
        return Ok(task.clone());
    }
    let matches = project
        .tasks
        .iter()
        .filter(|task| task.name == selector)
        .collect::<Vec<_>>();
    ensure!(
        matches.len() == 1,
        "task name is missing or ambiguous; select its exact ID"
    );
    Ok(matches[0].clone())
}
pub fn task(store: &Store, command: TaskCommand) -> Result<()> {
    let service = TaskService { store };
    match command {
        TaskCommand::List { project: selector } => {
            let id = project(store, &selector)?;
            output(&store.load()?.project(&id)?.tasks)
        }
        TaskCommand::Add(arguments) => {
            let AddTaskArgs {
                project: selector,
                name,
                cwd,
                command,
                steps,
                program,
                interpreter,
                script_file,
                args,
                env,
                source,
                source_arg,
                artifact,
                artifact_from_task,
                build_output,
                timeout_seconds,
                continue_on_error,
                interactive,
                no_log,
            } = *arguments;
            let id = project(store, &selector)?;
            let workspace = store.load()?;
            ensure!(
                source_arg.is_empty() || source.len() == 1,
                "source arguments require exactly one --source"
            );
            let sources = source
                .into_iter()
                .map(|path| SourceSpec {
                    path,
                    args: source_arg.clone(),
                })
                .collect();
            let steps = steps
                .into_iter()
                .map(|step| {
                    let (name, command) = step
                        .split_once('=')
                        .context("step must be NAME=CSH_PROGRAM")?;
                    Ok(TaskStep {
                        name: name.to_owned(),
                        command: command.to_owned(),
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let artifact_from_task = artifact_from_task
                .as_deref()
                .map(|selector| task_definition(store, &id, selector).map(|task| task.id))
                .transpose()?;
            let project_root = workspace.project(&id)?.root.clone();
            ensure!(
                program.is_some() || interpreter.is_some() || (args.is_empty() && env.is_empty()),
                "--arg/--env require --program or --interpreter"
            );
            let env = env
                .into_iter()
                .map(|entry| {
                    let (key, value) = entry.split_once('=').context("--env must be NAME=VALUE")?;
                    Ok((key.to_owned(), value.to_owned()))
                })
                .collect::<Result<std::collections::BTreeMap<_, _>>>()?;
            let execution = if let Some(program) = program {
                Some(TaskExecution::Process { program, args, env })
            } else if let Some(interpreter) = interpreter {
                let path = script_file.context("--interpreter requires --script-file")?;
                let script = std::fs::read_to_string(path).context("read explicit task script")?;
                Some(TaskExecution::Script {
                    interpreter,
                    args,
                    script,
                    env,
                })
            } else {
                None
            };
            let definition = TaskDefinition {
                execution,
                id: new_id(),
                name,
                command: command.unwrap_or_default(),
                cwd: cwd.unwrap_or(project_root),
                sources,
                artifact,
                approved_digest: None,
                steps,
                failure_policy: if continue_on_error {
                    FailurePolicy::Continue
                } else {
                    FailurePolicy::Stop
                },
                logging: if no_log || interactive {
                    TaskLogging::Disabled
                } else {
                    TaskLogging::Raw
                },
                interactive,
                build_outputs: build_output,
                artifact_from_task,
                timeout_seconds,
            };
            output(&service.save(workspace.revision, &id, definition)?)
        }
        TaskCommand::Save {
            project: selector,
            file,
        } => {
            let id = project(store, &selector)?;
            ensure!(file.is_absolute(), "task file path must be absolute");
            let input = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW)
                .open(&file)?;
            ensure!(
                input.metadata()?.is_file(),
                "task definition must be a regular file"
            );
            let mut text = String::new();
            input.take(256 * 1024 + 1).read_to_string(&mut text)?;
            ensure!(text.len() <= 256 * 1024, "task definition exceeds 256KiB");
            let definition: TaskDefinition =
                toml::from_str(&text).context("invalid task definition; original preserved")?;
            output(&service.save(store.load()?.revision, &id, definition)?)
        }
        TaskCommand::Review {
            project: selector,
            task,
        } => {
            let id = project(store, &selector)?;
            let task = task_definition(store, &id, &task)?;
            output(&service.review(&id, &task.id, &LaunchEnvironment::capture()?)?)
        }
        TaskCommand::Approve {
            project: selector,
            task,
            yes,
        } => {
            let id = project(store, &selector)?;
            let task = task_definition(store, &id, &task)?;
            let env = LaunchEnvironment::capture()?;
            let review = service.review(&id, &task.id, &env)?;
            if !yes {
                return output(&review);
            }
            service.approve(review.revision, &id, &task.id, &review.digest, &env)?;
            output(&serde_json::json!({"task_id":task.id,"approved":true,"executed":false}))
        }
        TaskCommand::Remove {
            project: selector,
            task,
            yes,
        } => {
            let id = project(store, &selector)?;
            let task = task_definition(store, &id, &task)?;
            if !yes {
                return output(
                    &serde_json::json!({"remove_definition":task,"performed":false,"preserved":"active runs, logs and source files; repeat with --yes"}),
                );
            }
            store.update(|workspace| {
                workspace
                    .project_mut(&id)?
                    .tasks
                    .retain(|saved| saved.id != task.id);
                Ok(())
            })?;
            output(&serde_json::json!({"removed_definition":task.id,"active_runs_stopped":false}))
        }
        TaskCommand::Editor {
            project: selector,
            executable,
            args,
            external_gui,
        } => {
            let id = project(store, &selector)?;
            let config = EditorConfig {
                executable,
                args,
                external_gui,
            };
            EditorService { store }.save(store.load()?.revision, &id, config.clone())?;
            output(&config)
        }
    }
}
fn request(client: &mut Client, request: RunRequest) -> Result<RunResult> {
    let mut job = client.run(request)?;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match job.state {
            RunJobState::Complete => {
                return job.result.context("completed run request has no result")
            }
            RunJobState::Failed => bail!(
                "{}",
                job.error.unwrap_or_else(|| "run request failed".into())
            ),
            RunJobState::Cancelled => bail!("run request was cancelled"),
            RunJobState::Pending => {
                ensure!(Instant::now()<deadline,"request remains pending ({}); inspect run list before retrying; no automatic replay",job.job_id);
                std::thread::sleep(Duration::from_millis(25));
                job = client.run(RunRequest::Job { job_id: job.job_id })?;
            }
        }
    }
}
fn info(client: &mut Client, id: &str) -> Result<RunInfo> {
    valid_id(id)?;
    match request(client, RunRequest::Info { run_id: id.into() })? {
        RunResult::Run(run) => Ok(run),
        _ => bail!("unexpected run information response"),
    }
}
fn wait_for(client: &mut Client, id: &str) -> Result<i32> {
    loop {
        let run = info(client, id)?;
        if !run.state.is_live() {
            output(&run)?;
            return Ok(match run.state {
                RunState::Succeeded => 0,
                RunState::Cancelled => 130,
                RunState::Failed => run
                    .exit_code
                    .filter(|code| *code != 0)
                    .unwrap_or(1)
                    .min(255) as i32,
                _ => 1,
            });
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
pub fn run(store: &Store, launcher: &Path, command: RunCommand) -> Result<i32> {
    if let RunCommand::Attach { run, takeover } = command {
        ensure!(
            std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
            "run attach requires a terminal; use run status/log for JSON"
        );
        idk_workspace::ui::run_attached_run(store, &run, takeover)?;
        return Ok(0);
    }
    let mut client = if matches!(command, RunCommand::Start { .. }) {
        Client::ensure_host(store, launcher)?
    } else {
        Client::connect(store)?
    };
    let pin_requested = matches!(command, RunCommand::Pin { .. });
    match command {
        RunCommand::List { project: selector } => {
            let project_id = selector
                .as_deref()
                .map(|selector| project(store, selector))
                .transpose()?;
            output(&request(&mut client, RunRequest::List { project_id })?)?;
        }
        RunCommand::Start {
            project: selector,
            task,
            operation_id,
            parallel,
            approve,
            wait,
            rows,
            cols,
        } => {
            let project_id = project(store, &selector)?;
            let task = task_definition(store, &project_id, &task)?;
            let environment = LaunchEnvironment::capture()?;
            let service = TaskService { store };
            let review = service.review(&project_id, &task.id, &environment)?;
            if approve {
                eprintln!("{}", serde_json::to_string_pretty(&review)?);
                ensure!(review.common_approved, "common initialization needs separate review: idk project trust {selector} --yes");
                if !review.approved {
                    service.approve(
                        review.revision,
                        &project_id,
                        &task.id,
                        &review.digest,
                        &environment,
                    )?;
                }
            } else {
                ensure!(
                    review.common_approved,
                    "common initialization needs review: idk project trust {selector} --yes"
                );
                ensure!(review.approved, "task needs review; run idk task review {selector} {} then repeat run start with --approve", task.name);
            }
            let operation_id = operation_id.unwrap_or_else(new_id);
            valid_id(&operation_id)?;
            let result = request(
                &mut client,
                RunRequest::Start {
                    project_id,
                    task_id: task.id,
                    operation_id,
                    expected_digest: Some(review.digest),
                    environment: environment.variables().clone(),
                    parallel,
                    rows,
                    cols,
                },
            )?;
            let RunResult::Started(reply) = result else {
                bail!("unexpected run start response")
            };
            if wait {
                return wait_for(&mut client, &reply.run.run_id);
            }
            output(&reply)?;
        }
        RunCommand::Status { run } => output(&info(&mut client, &run)?)?,
        RunCommand::Wait { run } => return wait_for(&mut client, &run),
        RunCommand::Cancel { run, yes, force } => {
            let current = info(&mut client, &run)?;
            if !yes {
                output(
                    &serde_json::json!({"target":current,"force":force,"performed":false,"confirmation":"--yes requests cancellation; actual cleanup is observed separately"}),
                )?;
            } else {
                output(&request(
                    &mut client,
                    RunRequest::Cancel { run_id: run, force },
                )?)?;
            }
        }
        RunCommand::Reconcile { run, yes } => {
            let current = info(&mut client, &run)?;
            ensure!(
                current.state == RunState::Unknown,
                "only a recovered Unknown can be reconciled"
            );
            if !yes {
                output(
                    &serde_json::json!({"target":current,"performed":false,"confirmation":"--yes acknowledges externally checked cleanup; never signals saved PIDs and never changes the Unknown exit outcome"}),
                )?;
            } else {
                output(&request(
                    &mut client,
                    RunRequest::Reconcile { run_id: run },
                )?)?;
            }
        }
        RunCommand::Log { run, offset, limit } => {
            let current = info(&mut client, &run)?;
            output(&request(
                &mut client,
                RunRequest::Log {
                    run_id: run,
                    generation: current.log.generation,
                    offset,
                    limit,
                },
            )?)?;
        }
        RunCommand::Search { run, query } => {
            let current = info(&mut client, &run)?;
            output(&request(
                &mut client,
                RunRequest::Search {
                    run_id: run,
                    generation: current.log.generation,
                    query,
                },
            )?)?;
        }
        RunCommand::Problems { run } => {
            output(&request(&mut client, RunRequest::Problems { run_id: run })?)?
        }
        RunCommand::Result {
            run,
            raw,
            offset,
            limit,
            json,
        } => {
            if raw {
                let result = request(
                    &mut client,
                    RunRequest::ResultRaw {
                        run_id: run,
                        offset,
                        limit: limit.unwrap_or(65536),
                    },
                )?;
                let RunResult::ResultRaw(page) = result else {
                    bail!("unexpected raw result response")
                };
                if json {
                    output(&page)?;
                } else {
                    use base64::Engine;
                    println!(
                        "{} · bytes {}..{} · {}",
                        page.result.status_label(),
                        page.offset,
                        page.next_offset,
                        if page.eof {
                            "end"
                        } else {
                            "more: use --offset with the next byte"
                        }
                    );
                    if let Some(diagnostic) = page.result.diagnostic() {
                        println!("{diagnostic}");
                    }
                    println!(
                        "{}",
                        idk_workspace::run::safe_text(
                            &base64::engine::general_purpose::STANDARD.decode(&page.data_base64)?
                        )
                    );
                }
            } else {
                let result = request(
                    &mut client,
                    RunRequest::Findings {
                        run_id: run,
                        offset: usize::try_from(offset)
                            .context("finding offset exceeds platform size")?,
                        limit: limit.unwrap_or(100),
                    },
                )?;
                let RunResult::Findings(page) = result else {
                    bail!("unexpected findings response")
                };
                if json {
                    output(&page)?;
                } else {
                    println!(
                        "{} · findings {}..{} / {}",
                        page.result.status_label(),
                        page.offset,
                        page.next_offset,
                        page.total
                    );
                    if let Some(diagnostic) = page.result.diagnostic() {
                        println!("{diagnostic}");
                    }
                    for finding in &page.findings {
                        let location = finding
                            .location
                            .as_ref()
                            .map(|location| {
                                format!(
                                    " · {}{}",
                                    location.path,
                                    location
                                        .start_line
                                        .map(|line| format!(":{line}"))
                                        .unwrap_or_default()
                                )
                            })
                            .unwrap_or_default();
                        println!(
                            "[{}] {} {}{}",
                            finding.index,
                            finding.severity,
                            finding.code.as_deref().unwrap_or(""),
                            location
                        );
                        println!("  {}", finding.message);
                    }
                    if let Some(body) = page.result.body() {
                        println!("body SHA-256: {}", body.sha256);
                    }
                    if page.more {
                        println!("More: --offset {}", page.next_offset);
                    }
                }
            }
        }
        RunCommand::Pin { run, json } | RunCommand::Unpin { run, json } => {
            let pinned = pin_requested;
            let result = request(
                &mut client,
                RunRequest::ResultPin {
                    run_id: run,
                    pinned,
                },
            )?;
            let RunResult::ResultPinned(run) = result else {
                bail!("unexpected pin response")
            };
            if json {
                output(&run)?;
            } else {
                println!(
                    "{} · {}",
                    run.run_id,
                    if run.pinned {
                        "pinned baseline"
                    } else {
                        "ordinary retention"
                    }
                );
            }
        }
        RunCommand::Export { run, path, json } => {
            let path = std::path::absolute(path)?;
            let result = request(&mut client, RunRequest::ResultExport { run_id: run, path })?;
            let RunResult::ResultExported(exported) = result else {
                bail!("unexpected export response")
            };
            if json {
                output(&exported)?;
            } else {
                println!(
                    "{} bytes · SHA-256 {} · {}",
                    exported.bytes,
                    exported.sha256,
                    exported.path.display()
                );
            }
        }
        RunCommand::Compare {
            baseline,
            run,
            json,
        } => {
            let result = request(
                &mut client,
                RunRequest::Compare {
                    baseline_run_id: baseline,
                    run_id: run,
                },
            )?;
            let RunResult::Compared(comparison) = result else {
                bail!("unexpected comparison response")
            };
            if json {
                output(&comparison)?;
            } else {
                println!("{:?}: {}", comparison.state, comparison.reason);
                println!(
                    "new {} · unchanged {} · resolved {} · carried {}",
                    comparison.new_count,
                    comparison.unchanged_count,
                    comparison.resolved_count,
                    comparison.carried_count
                );
            }
        }
        RunCommand::Events { run, json } => {
            let result = request(&mut client, RunRequest::Events { run_id: run })?;
            let RunResult::Events(events) = result else {
                bail!("unexpected events response")
            };
            if json {
                output(&events)?;
            } else {
                println!(
                    "{:?} · {} events / {} bytes · {} invalid · {} sequence gaps",
                    events.state,
                    events.events_observed,
                    events.bytes_observed,
                    events.invalid_events,
                    events.sequence_gaps
                );
                if let Some(latest) = events.latest {
                    println!("#{} {}: {}", latest.seq, latest.event_type, latest.message);
                }
                if let Some(error) = events.error {
                    println!("{error}");
                }
                println!("Progress observation; use run status for actual exit and cleanup.");
            }
        }
        RunCommand::FindingEditor {
            run,
            index,
            body_sha256,
            yes,
        } => {
            let review = request(
                &mut client,
                RunRequest::FindingEditorReview {
                    run_id: run,
                    body_sha256,
                    index,
                },
            )?;
            if !yes {
                output(&review)?;
            } else {
                let RunResult::EditorReview { review_id, .. } = review else {
                    bail!("unexpected editor review response")
                };
                output(&request(
                    &mut client,
                    RunRequest::EditorOpen {
                        review_id,
                        environment: LaunchEnvironment::capture()?.variables().clone(),
                        rows: 24,
                        cols: 80,
                    },
                )?)?;
            }
        }
        RunCommand::Editor { run, problem, yes } => {
            let current = info(&mut client, &run)?;
            let review = request(
                &mut client,
                RunRequest::EditorReview {
                    run_id: run,
                    problem_id: problem,
                    log_generation: current.log.generation,
                },
            )?;
            if !yes {
                output(&review)?;
            } else {
                let RunResult::EditorReview { review_id, .. } = review else {
                    bail!("unexpected editor review response")
                };
                output(&request(
                    &mut client,
                    RunRequest::EditorOpen {
                        review_id,
                        environment: LaunchEnvironment::capture()?.variables().clone(),
                        rows: 24,
                        cols: 80,
                    },
                )?)?;
            }
        }
        RunCommand::Attach { .. } => unreachable!(),
    }
    Ok(0)
}
