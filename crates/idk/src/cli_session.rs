use anyhow::{bail, Result};
use clap::Subcommand;
use idk_workspace::client::Client;
use idk_workspace::project::{LaunchEnvironment, ProjectService};
use idk_workspace::store::Store;
use std::io::IsTerminal;
use std::path::Path;

#[derive(Subcommand)]
pub enum HostCommand {
    /// Inspect an existing host without starting one.
    Status,
    /// Start the per-user host; no project scripts are run.
    Start,
    /// Preview host-owned sessions and request their shutdown with --yes.
    Stop {
        #[arg(long)]
        yes: bool,
        /// Force only verified host-owned processes after reviewing the targets.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
pub enum SessionCommand {
    /// List live and closed runtime slots; saved definitions use `terminal list`.
    List { project: Option<String> },
    /// Start an approved saved definition or return its existing live session.
    Start {
        project: String,
        terminal: String,
        /// Explicitly create a new shell for an exited/closed/unknown slot.
        #[arg(long)]
        reopen: bool,
        #[arg(long, default_value_t = 24)]
        rows: u16,
        #[arg(long, default_value_t = 80)]
        cols: u16,
    },
    /// Read the current screen as escaped JSON; does not replay control sequences.
    Snapshot { session: String },
    /// Attach a TUI to an existing shell, including one whose definition was removed.
    Attach {
        session: String,
        #[arg(long)]
        takeover: bool,
    },
    /// Preview one session and request its termination with --yes.
    Close {
        session: String,
        #[arg(long)]
        yes: bool,
        /// Explicitly take input ownership from a different attached client.
        #[arg(long)]
        takeover: bool,
        #[arg(long)]
        force: bool,
    },
}

fn output(value: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

pub fn host(store: &Store, launcher: &Path, command: HostCommand) -> Result<()> {
    match command {
        HostCommand::Status => output(Client::connect(store)?.info()),
        HostCommand::Start => output(Client::ensure_host(store, launcher)?.info()),
        HostCommand::Stop { yes, force } => {
            let mut client = Client::connect(store)?;
            let preview = client.preview_close(None)?;
            if !yes {
                output(&preview)?;
                bail!("host shutdown requires --yes; review the owned targets above");
            }
            let targets = preview
                .targets
                .into_iter()
                .map(|session| session.session_id)
                .collect::<Vec<_>>();
            output(&client.shutdown(&targets, force)?)
        }
    }
}

pub fn session(store: &Store, launcher: &Path, command: SessionCommand) -> Result<()> {
    let service = ProjectService { store };
    match command {
        SessionCommand::List { project } => {
            let id = project
                .as_deref()
                .map(|selector| service.resolve_project_id(selector))
                .transpose()?;
            output(&Client::connect(store)?.list(id.as_deref())?)
        }
        SessionCommand::Start {
            project,
            terminal,
            reopen,
            rows,
            cols,
        } => {
            let id = service.resolve_project_id(&project)?;
            let workspace = store.load()?;
            let definition =
                crate::cli_project::resolve_terminal(workspace.project(&id)?, &terminal)?;
            let environment = LaunchEnvironment::capture()?;
            let mut client = Client::ensure_host(store, launcher)?;
            output(&client.start(
                &id,
                &definition.id,
                environment.variables().clone(),
                rows,
                cols,
                reopen,
            )?)
        }
        SessionCommand::Snapshot { session } => {
            idk_workspace::model::valid_id(&session)?;
            output(&Client::connect(store)?.snapshot(&session, None)?)
        }
        SessionCommand::Attach { session, takeover } => {
            idk_workspace::model::valid_id(&session)?;
            if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
                bail!("session attach requires a terminal; use session snapshot for JSON output");
            }
            idk_workspace::ui::run_attached(store, &session, takeover)
        }
        SessionCommand::Close {
            session,
            yes,
            takeover,
            force,
        } => {
            idk_workspace::model::valid_id(&session)?;
            let mut client = Client::connect(store)?;
            if !yes {
                let sessions = client.list(None)?;
                let info = sessions
                    .iter()
                    .find(|item| item.session_id == session)
                    .ok_or_else(|| anyhow::anyhow!("session not found"))?;
                output(info)?;
                bail!("closing a shell requires --yes; use --takeover only to take another client's input ownership");
            }
            let attached = client.attach(&session, takeover)?;
            let result = client.close(&session, attached.input_epoch, force);
            // Detach is best effort: closing is still observed by the host. A
            // timeout here never triggers another close with a fresh request ID.
            let _ = client.detach(&session, attached.input_epoch);
            output(&result?)
        }
    }
}
