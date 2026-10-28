use anyhow::{ensure, Context, Result};
use clap::{Args, ValueEnum};
use idk_workspace::discovery::{self, DiscoveryReport, OptionalTool, TaskCandidate};
use idk_workspace::model::{TaskDefinition, Workspace};
use idk_workspace::project::{LaunchEnvironment, ProjectService};
use idk_workspace::store::Store;
use idk_workspace::task::TaskService;
use serde_json::json;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ToolChoice {
    Ici,
    Lens,
}
impl From<ToolChoice> for OptionalTool {
    fn from(value: ToolChoice) -> Self {
        match value {
            ToolChoice::Ici => Self::Ici,
            ToolChoice::Lens => Self::Lens,
        }
    }
}
#[derive(Args)]
pub struct DiscoverArgs {
    /// Connected project name or ID.
    project: String,
    /// Emit candidate/draft metadata as JSON.
    #[arg(long)]
    json: bool,
    /// Save a candidate by its 1-based index/name, or name the selected optional tool draft.
    #[arg(long, conflicts_with = "artifact")]
    register: Option<String>,
    /// Explicitly inspect this optional tool's version; never runs its analysis.
    #[arg(long, value_enum, requires = "executable", conflicts_with = "artifact")]
    tool: Option<ToolChoice>,
    /// Absolute path of the optional tool the user chose to execute with --version.
    #[arg(long, requires = "tool")]
    executable: Option<PathBuf>,
    /// Literal report argument, repeatable. ici defaults to analyze.
    #[arg(long = "arg", allow_hyphen_values = true, requires = "tool")]
    args: Vec<String>,
    /// Measure the actual bytes at a registered task's artifact path (name or ID).
    #[arg(long, conflicts_with_all = ["tool", "register"])]
    artifact: Option<String>,
    /// Bound explicit artifact measurement; maximum 1 GiB.
    #[arg(long, requires = "artifact")]
    max_bytes: Option<u64>,
}

pub fn execute(store: &Store, args: DiscoverArgs) -> Result<()> {
    let project_id = ProjectService { store }.resolve_project_id(&args.project)?;
    let workspace = store.load()?;
    let project = workspace.project(&project_id)?;
    let environment = LaunchEnvironment::capture()?;
    if let Some(selector) = &args.artifact {
        let matches = project
            .tasks
            .iter()
            .filter(|task| task.id == *selector || task.name == *selector)
            .collect::<Vec<_>>();
        ensure!(
            matches.len() == 1,
            "artifact task selector is missing or ambiguous; use its ID"
        );
        let task = matches[0];
        let path = task
            .artifact
            .as_ref()
            .context("selected task has no registered artifact path")?;
        let measured = discovery::snapshot_artifact(
            path,
            args.max_bytes.unwrap_or(discovery::MAX_ARTIFACT_BYTES),
        )?;
        if args.json {
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &json!({"project_id":project_id,"task_id":task.id,"measurement":measured,"run_relation_modified":false})
                )?
            );
        } else {
            println!("Artifact measurement: {}", json!(measured.path));
            println!("SHA-256: {}\nBytes: {}", measured.sha256, measured.size);
            println!(
                "Current file bytes measured; no historical Run or path relation was changed."
            );
        }
        return Ok(());
    }
    if let Some(tool) = args.tool {
        let inspected = discovery::inspect_optional_tool(
            tool.into(),
            args.executable
                .as_deref()
                .context("--tool requires --executable")?,
            environment.variables(),
        )?;
        if !inspected.supported {
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &json!({"inspection":inspected,"draft":null,"registered":false})
                    )?
                );
            } else {
                println!(
                    "Optional tool unavailable: {}",
                    inspected.reason.as_deref().unwrap_or("unsupported version")
                );
            }
            ensure!(
                args.register.is_none(),
                "optional tool draft was not registered; inspect a supported executable"
            );
            return Ok(());
        }
        let mut draft = discovery::optional_task_draft(&inspected, &project.root, &args.args)?;
        if let Some(name) = args.register.as_ref() {
            draft.name = name.clone();
            draft = register(store, &workspace, &project_id, draft)?;
        }
        if args.json {
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &json!({"inspection":inspected,"draft":draft,"registered":args.register.is_some(),"approved":false,"executed":false})
                )?
            );
        } else {
            println!(
                "Selected tool: {} version {}",
                json!(inspected.resolved_path),
                inspected.version.as_deref().unwrap_or("unknown")
            );
            println!(
                "cwd: {}\nDefinition: {}",
                json!(draft.cwd),
                serde_json::to_string_pretty(&draft)?
            );
            print_next(&args.project, &draft, args.register.is_some());
        }
        return Ok(());
    }
    let report = discovery::discover_tasks(&project.root, environment.variables())?;
    let saved = if let Some(selector) = &args.register {
        let candidate = select_candidate(&report, selector)?;
        Some(register(
            store,
            &workspace,
            &project_id,
            candidate.task_definition()?,
        )?)
    } else {
        None
    };
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &json!({"project_id":project_id,"report":report,"registered":saved,"approved":false,"executed":false})
            )?
        );
    } else {
        println!(
            "Discovered tasks for {} (files only; no commands executed)",
            json!(project.name)
        );
        if report.candidates.is_empty() {
            println!("No supported task definitions found. Register a command with idk task add.");
        }
        for (index, candidate) in report.candidates.iter().enumerate() {
            println!(
                "{}. {} [{}]",
                index + 1,
                candidate.name,
                if candidate.available {
                    "available"
                } else {
                    "missing tool"
                }
            );
            println!(
                "   program: {}  argv: {}",
                json!(candidate.resolved_program),
                json!(candidate.args)
            );
            println!(
                "   cwd: {}  origin: {}",
                json!(candidate.cwd),
                json!(candidate.origin)
            );
            if let Some(tool) = &candidate.missing_tool {
                println!("   unavailable executable: {}", json!(tool));
            }
        }
        for warning in &report.warnings {
            println!("Warning: {}", json!(warning));
        }
        if let Some(task) = &saved {
            print_next(&args.project, task, true);
        } else {
            println!("Choose a candidate with --register NAME or --register INDEX; registration does not run it.");
        }
    }
    Ok(())
}
fn select_candidate<'a>(report: &'a DiscoveryReport, selector: &str) -> Result<&'a TaskCandidate> {
    // Exact name wins over numeric interpretation, should a future producer use numbers.
    if let Some(candidate) = report
        .candidates
        .iter()
        .find(|candidate| candidate.name == selector)
    {
        return Ok(candidate);
    }
    let index = selector
        .parse::<usize>()
        .ok()
        .and_then(|index| index.checked_sub(1));
    index
        .and_then(|index| report.candidates.get(index))
        .context("task candidate not found; select its displayed name or 1-based index")
}
fn register(
    store: &Store,
    workspace: &Workspace,
    project_id: &str,
    task: TaskDefinition,
) -> Result<TaskDefinition> {
    idk_workspace::portable::ensure_current(store, project_id)?;
    ensure!(
        !workspace
            .project(project_id)?
            .tasks
            .iter()
            .any(|saved| saved.name == task.name),
        "a task with this name already exists; use it or explicitly edit its definition"
    );
    TaskService { store }.save(workspace.revision, project_id, task)
}
fn print_next(project: &str, task: &TaskDefinition, registered: bool) {
    if registered {
        println!(
            "Registered {} ({}), not approved or executed.",
            json!(task.name),
            task.id
        );
        println!(
            "Review and run with: idk run start {} {}",
            shell_quote(project),
            shell_quote(&task.id)
        );
    } else {
        println!("Draft only. Add --register NAME to save it before review and execution.");
    }
}
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
