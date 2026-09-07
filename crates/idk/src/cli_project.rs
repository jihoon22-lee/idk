use anyhow::{bail, Context, Result};
use clap::Subcommand;
use idk_workspace::model::{ShellConfig, SourceSpec, TerminalDefinition};
use idk_workspace::project::{
    new_terminal, ConnectDraft, LaunchEnvironment, ProjectService, TerminalDraft,
};
use idk_workspace::store::Store;
use std::path::{Path, PathBuf};

#[derive(Subcommand)]
pub enum ProjectCommand {
    /// List connected projects in recent-use order.
    List,
    /// Connect an existing directory; does not run scripts or create a Git repository.
    #[command(alias = "add")]
    Connect {
        name: String,
        root: PathBuf,
        #[arg(long)]
        shell: Option<PathBuf>,
        #[arg(long)]
        init_cwd: Option<PathBuf>,
        #[arg(long)]
        login: bool,
        #[arg(long)]
        source: Vec<PathBuf>,
        /// Arguments for a single --source; repeat once per argv item.
        #[arg(long, allow_hyphen_values = true)]
        source_arg: Vec<String>,
        /// Deliberately keep a second project definition for an already connected root.
        #[arg(long)]
        allow_duplicate_root: bool,
    },
    Inspect {
        project: String,
    },
    Select {
        project: String,
    },
    Rename {
        project: String,
        name: String,
    },
    /// Remove only the definition, never sources, scripts, Git or live shells.
    Remove {
        project: String,
        #[arg(long)]
        yes: bool,
    },
    /// Preview moving paths below an old root; external test paths stay unchanged.
    Reconnect {
        project: String,
        root: PathBuf,
        #[arg(long)]
        yes: bool,
    },
    /// Review direct initialization files; --yes approves this freshly checked revision.
    Trust {
        project: String,
        #[arg(long)]
        yes: bool,
    },
    /// Update common initialization for future shells only.
    Environment {
        project: String,
        #[arg(long)]
        shell: Option<PathBuf>,
        #[arg(long)]
        init_cwd: Option<PathBuf>,
        #[arg(long)]
        login: Option<bool>,
        #[arg(long)]
        source: Vec<PathBuf>,
        #[arg(long, allow_hyphen_values = true)]
        source_arg: Vec<String>,
        #[arg(long, conflicts_with = "source")]
        clear_sources: bool,
    },
    /// Explicitly bind the primary or a related repository; never starts a network operation.
    Repository {
        project: String,
        root: PathBuf,
        #[arg(long)]
        related: bool,
    },
}

#[derive(Subcommand)]
pub enum TerminalCommand {
    List {
        project: String,
    },
    /// Save a terminal definition. Several terminals may share the same cwd.
    Add {
        project: String,
        name: String,
        cwd: PathBuf,
        #[arg(long)]
        source: Vec<PathBuf>,
        #[arg(long, allow_hyphen_values = true)]
        source_arg: Vec<String>,
    },
    Edit {
        project: String,
        terminal: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        cwd: Option<PathBuf>,
        #[arg(long)]
        source: Vec<PathBuf>,
        #[arg(long, allow_hyphen_values = true)]
        source_arg: Vec<String>,
        #[arg(long, conflicts_with = "source")]
        clear_sources: bool,
    },
    Remove {
        project: String,
        terminal: String,
        #[arg(long)]
        yes: bool,
    },
    Default {
        project: String,
        terminal: String,
    },
    /// Specify all terminal IDs in their desired order.
    Reorder {
        project: String,
        terminals: Vec<String>,
    },
}

pub fn absolute(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn sources(paths: Vec<PathBuf>, args: Vec<String>) -> Result<Vec<SourceSpec>> {
    if !args.is_empty() && paths.len() != 1 {
        bail!("--source-arg requires exactly one --source; use the TUI for arguments on multiple sources");
    }
    paths
        .into_iter()
        .map(|path| {
            Ok(SourceSpec {
                path: absolute(&path)?,
                args: args.clone(),
            })
        })
        .collect()
}

fn output(value: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

pub fn project(store: &Store, command: ProjectCommand) -> Result<()> {
    let service = ProjectService { store };
    match command {
        ProjectCommand::List => output(&service.list()?),
        ProjectCommand::Connect {
            name,
            root,
            shell,
            init_cwd,
            login,
            source,
            source_arg,
            allow_duplicate_root,
        } => {
            let root = absolute(&root)?;
            let environment = LaunchEnvironment::capture()?;
            let shell = match shell {
                Some(path) => absolute(&path)?,
                None => idk_workspace::project::discover_shells(&environment)
                    .first()
                    .map(|s| s.path.clone())
                    .context("no verified csh/tcsh found; provide --shell /absolute/path")?,
            };
            let mut preview = service.preview_connect(ConnectDraft {
                name,
                root: root.clone(),
                shell: ShellConfig {
                    executable: shell,
                    login,
                    init_cwd: init_cwd
                        .as_deref()
                        .map(absolute)
                        .transpose()?
                        .unwrap_or_else(|| root.clone()),
                    sources: sources(source, source_arg)?,
                    trusted_digest: None,
                },
                terminals: vec![TerminalDraft {
                    name: "dev".into(),
                    cwd: root,
                    sources: Vec::new(),
                    persistent: true,
                }],
            })?;
            preview.allow_duplicate_root = allow_duplicate_root;
            if !preview.duplicates.is_empty() && !allow_duplicate_root {
                output(&preview)?;
                bail!("root already connected; choose its existing ID or deliberately use --allow-duplicate-root");
            }
            output(&service.create(preview.revision, preview)?)
        }
        ProjectCommand::Inspect { project } => {
            output(&service.inspect(&service.resolve_project_id(&project)?)?)
        }
        ProjectCommand::Select { project } => {
            let id = service.resolve_project_id(&project)?;
            service.select(store.load()?.revision, &id)?;
            output(&service.inspect(&id)?)
        }
        ProjectCommand::Rename { project, name } => {
            let id = service.resolve_project_id(&project)?;
            output(&service.rename(store.load()?.revision, &id, name)?)
        }
        ProjectCommand::Remove { project, yes } => {
            let id = service.resolve_project_id(&project)?;
            if !yes {
                output(&service.inspect(&id)?)?;
                bail!(
                    "definition removal requires --yes; source files and live shells are preserved"
                );
            }
            output(&service.remove_definition(store.load()?.revision, &id)?)
        }
        ProjectCommand::Reconnect { project, root, yes } => {
            let preview = service
                .preview_rebind_root(&service.resolve_project_id(&project)?, absolute(&root)?)?;
            if yes {
                output(&service.rebind_root(preview.revision, preview)?)
            } else {
                output(&preview)
            }
        }
        ProjectCommand::Trust { project, yes } => {
            let id = service.resolve_project_id(&project)?;
            let environment = LaunchEnvironment::capture()?;
            let review = service.review_initialization(&id, &environment)?;
            output(&review)?;
            if yes {
                service.approve_initialization(review.revision, review)?;
            }
            Ok(())
        }
        ProjectCommand::Environment {
            project,
            shell,
            init_cwd,
            login,
            source,
            source_arg,
            clear_sources,
        } => {
            let id = service.resolve_project_id(&project)?;
            let workspace = store.load()?;
            let mut config = workspace.project(&id)?.shell.clone();
            if let Some(path) = shell {
                config.executable = absolute(&path)?;
            }
            if let Some(path) = init_cwd {
                config.init_cwd = absolute(&path)?;
            }
            if let Some(value) = login {
                config.login = value;
            }
            if clear_sources || !source.is_empty() || !source_arg.is_empty() {
                config.sources = sources(source, source_arg)?;
            }
            output(&service.update_shell(workspace.revision, &id, config)?)
        }
        ProjectCommand::Repository {
            project,
            root,
            related,
        } => {
            let id = service.resolve_project_id(&project)?;
            output(&service.bind_repository(
                store.load()?.revision,
                &id,
                absolute(&root)?,
                !related,
            )?)
        }
    }
}

pub fn resolve_terminal(
    project: &idk_workspace::model::Project,
    selector: &str,
) -> Result<TerminalDefinition> {
    if uuid::Uuid::parse_str(selector).is_ok() {
        return project
            .terminals
            .iter()
            .find(|terminal| terminal.id == selector)
            .cloned()
            .context("terminal ID not found");
    }
    let matches: Vec<_> = project
        .terminals
        .iter()
        .filter(|terminal| terminal.name == selector)
        .collect();
    match matches.as_slice() {
        [terminal] => Ok((*terminal).clone()),
        [] => bail!("terminal not found"),
        _ => bail!("terminal name is ambiguous; use its ID"),
    }
}

pub fn terminal(store: &Store, command: TerminalCommand) -> Result<()> {
    let service = ProjectService { store };
    match command {
        TerminalCommand::List { project } => output(
            &service
                .inspect(&service.resolve_project_id(&project)?)?
                .terminals,
        ),
        TerminalCommand::Add {
            project,
            name,
            cwd,
            source,
            source_arg,
        } => {
            let id = service.resolve_project_id(&project)?;
            let workspace = store.load()?;
            let terminal = new_terminal(
                workspace.project(&id)?,
                TerminalDraft {
                    name,
                    cwd: absolute(&cwd)?,
                    sources: sources(source, source_arg)?,
                    persistent: true,
                },
            )?;
            service.save_terminal(workspace.revision, &id, terminal.clone())?;
            output(&terminal)
        }
        TerminalCommand::Edit {
            project,
            terminal,
            name,
            cwd,
            source,
            source_arg,
            clear_sources,
        } => {
            let id = service.resolve_project_id(&project)?;
            let workspace = store.load()?;
            let mut terminal = resolve_terminal(workspace.project(&id)?, &terminal)?;
            if let Some(name) = name {
                terminal.name = name;
            }
            if let Some(path) = cwd {
                terminal.cwd = absolute(&path)?;
            }
            if clear_sources || !source.is_empty() || !source_arg.is_empty() {
                terminal.sources = sources(source, source_arg)?;
            }
            output(&service.save_terminal(workspace.revision, &id, terminal)?)
        }
        TerminalCommand::Remove {
            project,
            terminal,
            yes,
        } => {
            let id = service.resolve_project_id(&project)?;
            let workspace = store.load()?;
            let terminal = resolve_terminal(workspace.project(&id)?, &terminal)?;
            if !yes {
                output(&terminal)?;
                bail!("definition removal requires --yes; live shells are preserved");
            }
            output(&service.remove_terminal_definition(workspace.revision, &id, &terminal.id)?)
        }
        TerminalCommand::Default { project, terminal } => {
            let id = service.resolve_project_id(&project)?;
            let workspace = store.load()?;
            let terminal = resolve_terminal(workspace.project(&id)?, &terminal)?;
            service.set_default_terminal(workspace.revision, &id, &terminal.id)?;
            output(&service.inspect(&id)?)
        }
        TerminalCommand::Reorder { project, terminals } => {
            let id = service.resolve_project_id(&project)?;
            let workspace = store.load()?;
            let ids = terminals
                .iter()
                .map(|s| resolve_terminal(workspace.project(&id)?, s).map(|t| t.id))
                .collect::<Result<Vec<_>>>()?;
            service.reorder_terminals(workspace.revision, &id, ids)?;
            output(&service.inspect(&id)?)
        }
    }
}
