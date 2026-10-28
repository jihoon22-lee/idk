use anyhow::{Context, Result};
use clap::Subcommand;
use idk_workspace::{portable, project::LaunchEnvironment, store::Store};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum ConfigCommand {
    /// Create the minimal optional idk.toml; never overwrite an existing file.
    Init {
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Show resolved values, executable paths and their origins without saving.
    Show {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        select: Option<String>,
    },
    /// Refresh local definitions from the current declaration without running them.
    Sync {
        #[arg(default_value = ".")]
        path: PathBuf,
        #[arg(long)]
        select: Option<String>,
    },
    /// Export one local project as portable TOML, excluding approvals and runtime state.
    Export { project: String },
    /// Set one declared field using a TOML literal (for example shell.env.KEY '"value"').
    Set {
        #[arg(long, default_value = ".")]
        path: PathBuf,
        key: String,
        value: String,
    },
    /// Remove one declared field. Other overrides are preserved.
    Unset {
        #[arg(long, default_value = ".")]
        path: PathBuf,
        key: String,
    },
    /// Show personal shell/editor preferences, or replace them with a reviewed TOML file.
    Preferences { file: Option<PathBuf> },
}
pub fn execute(store: &Store, command: ConfigCommand) -> Result<()> {
    match command {
        ConfigCommand::Init { path } => {
            let root = path.canonicalize()?;
            let target = root.join(portable::DECLARATION);
            portable::write_declaration(store, &target, None, "schema_version = 1\n")?;
            println!(
                "Created {}. Use idk . to open; declare only exceptions.",
                target.display()
            );
        }
        ConfigCommand::Show { path, select } => {
            let resolved = portable::resolve_for(
                store,
                &path,
                &LaunchEnvironment::capture()?,
                select.as_deref(),
            )?;
            println!(
                "Project: {}\nFolder: {}",
                resolved.project.name,
                resolved.project.root.display()
            );
            for (key, origin) in &resolved.origins {
                println!("{key}: {origin}");
            }
            print!(
                "{}",
                toml::to_string_pretty(&portable::export_declaration(&resolved.project)?)?
            );
        }
        ConfigCommand::Sync { path, select } => {
            let resolved = portable::resolve_for(
                store,
                &path,
                &LaunchEnvironment::capture()?,
                select.as_deref(),
            )?;
            let project = portable::apply(store, resolved)?;
            println!(
                "{} synchronized. Review changed execution definitions before starting.",
                project.name
            );
        }
        ConfigCommand::Export { project } => {
            let id =
                (idk_workspace::project::ProjectService { store }).resolve_project_id(&project)?;
            print!(
                "{}",
                toml::to_string_pretty(&portable::export_declaration(
                    store.load()?.project(&id)?
                )?)?
            );
        }
        ConfigCommand::Set { path, key, value } => {
            let document: toml::Table = toml::from_str(&format!("value = {value}")).context(
                "value must be a TOML literal, e.g. '\"bash\"', '[]', or '{KEY=\"value\"}'",
            )?;
            edit(store, path, key, document.get("value").cloned())?;
        }
        ConfigCommand::Unset { path, key } => edit(store, path, key, None)?,
        ConfigCommand::Preferences { file } => {
            let (preferences, digest) = portable::load_preferences(store)?;
            if let Some(file) = file {
                let input = std::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
                    .open(file)?;
                anyhow::ensure!(
                    input.metadata()?.is_file(),
                    "preferences import must be a regular file"
                );
                let mut raw = String::new();
                input
                    .take(portable::DECLARATION_LIMIT as u64 + 1)
                    .read_to_string(&mut raw)?;
                anyhow::ensure!(
                    raw.len() <= portable::DECLARATION_LIMIT,
                    "preferences import exceeds 1 MiB; original preserved"
                );
                let draft: portable::Preferences = toml::from_str(&raw)?;
                portable::save_preferences(store, digest.as_deref(), &draft)?;
                println!(
                    "Personal preferences saved. Use config show/sync to review affected projects."
                );
            } else {
                print!("{}", toml::to_string_pretty(&preferences)?);
            }
        }
    }
    Ok(())
}
fn edit(store: &Store, path: PathBuf, key: String, value: Option<toml::Value>) -> Result<()> {
    let root = path.canonicalize()?;
    let snapshot = portable::read(&root)?.context("no idk.toml; use idk config init first")?;
    let keys = key.split('.').collect::<Vec<_>>();
    let draft = portable::patch_declaration(&snapshot, &keys, value)?;
    if let Err(error) =
        portable::write_declaration(store, &snapshot.path, Some(&snapshot.digest), &draft)
    {
        // Keep a recoverable local draft without touching the checkout on a CAS conflict.
        let saved = store.config_dir.join(format!(
            "declaration-draft-{}.toml",
            idk_workspace::model::new_id()
        ));
        idk_workspace::store::atomic_write(&saved, draft.as_bytes())?;
        anyhow::bail!("{error:#}; draft saved at {}", saved.display());
    }
    println!(
        "Saved {}. Use config sync to review resolved changes.",
        snapshot.path.display()
    );
    Ok(())
}
