use anyhow::{ensure, Result};
use clap::Subcommand;
use idk_workspace::install::Installer;
use idk_workspace::package::{valid_digest, VerifiedBundle};
use idk_workspace::store::Store;
use serde_json::json;
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum PackageCommand {
    /// Verify an offline archive against a digest obtained through a trusted channel.
    Verify {
        bundle: PathBuf,
        #[arg(long)]
        sha256: String,
    },
    /// Review an offline installation/update; --yes activates verified bytes.
    Install {
        bundle: PathBuf,
        #[arg(long)]
        sha256: String,
        /// Installation root (default: $HOME/.local/share/idk).
        #[arg(long)]
        prefix: Option<PathBuf>,
        #[arg(long)]
        yes: bool,
    },
    /// Verify an HTTPS archive using curl and an independently trusted SHA-256.
    VerifyUrl {
        url: String,
        #[arg(long)]
        sha256: String,
    },
    /// Download, verify and review an installation/update; --yes activates it.
    InstallUrl {
        url: String,
        #[arg(long)]
        sha256: String,
        /// Installation root (default: $HOME/.local/share/idk).
        #[arg(long)]
        prefix: Option<PathBuf>,
        #[arg(long)]
        yes: bool,
    },
    /// List distribution targets supported by this package format.
    Targets,
    /// Inspect the committed installation and preserved generations.
    Status {
        /// Installation root (default: $HOME/.local/share/idk).
        #[arg(long)]
        prefix: Option<PathBuf>,
    },
    /// Resolve an interrupted activation without restoring live workspace data.
    Recover {
        /// Installation root (default: $HOME/.local/share/idk).
        #[arg(long)]
        prefix: Option<PathBuf>,
        #[arg(long)]
        yes: bool,
    },
    /// Find a verified client matching the live host without stopping or replacing it.
    CompatibleClient {
        /// Optional known installation root; otherwise inspect the peer's generation and default root.
        #[arg(long)]
        prefix: Option<PathBuf>,
    },
    /// Verify all bytes of an explicitly selected preserved installation generation.
    VerifyGeneration {
        generation: String,
        /// Installation root (default: $HOME/.local/share/idk).
        #[arg(long)]
        prefix: Option<PathBuf>,
    },
    /// Remove managed entrypoints only; preserve generations, data, logs and live hosts.
    Uninstall {
        /// Installation root (default: $HOME/.local/share/idk).
        #[arg(long)]
        prefix: Option<PathBuf>,
        #[arg(long)]
        yes: bool,
    },
}

pub fn execute(data_dir: Option<&std::path::Path>, command: PackageCommand) -> Result<()> {
    let value = match command {
        PackageCommand::Verify { bundle, sha256 } => {
            valid_digest(&sha256)?;
            let verified = VerifiedBundle::read(&bundle, &sha256)?;
            json!(verified.review())
        }
        PackageCommand::Install {
            bundle,
            sha256,
            prefix,
            yes,
        } => {
            let prefix = resolve_prefix(prefix)?;
            ensure!(prefix.is_absolute(), "installation prefix must be absolute");
            let verified = VerifiedBundle::read(&bundle, &sha256)?;
            install_verified(data_dir, verified, prefix, yes)?
        }
        PackageCommand::VerifyUrl { url, sha256 } => {
            json!(VerifiedBundle::download(&url, &sha256)?.review())
        }
        PackageCommand::InstallUrl {
            url,
            sha256,
            prefix,
            yes,
        } => {
            let prefix = resolve_prefix(prefix)?;
            ensure!(prefix.is_absolute(), "installation prefix must be absolute");
            install_verified(
                data_dir,
                VerifiedBundle::download(&url, &sha256)?,
                prefix,
                yes,
            )?
        }
        PackageCommand::Targets => json!(idk_workspace::package::SUPPORTED_TARGETS),
        PackageCommand::Status { prefix } => {
            let prefix = resolve_prefix(prefix)?;
            ensure!(prefix.is_dir(), "installation prefix does not exist");
            json!(Installer::open(&prefix)?.status()?)
        }
        PackageCommand::Recover { prefix, yes } => {
            let prefix = resolve_prefix(prefix)?;
            ensure!(prefix.is_dir(), "installation prefix does not exist");
            if yes {
                json!(Installer::open(&prefix)?.recover()?)
            } else {
                json!({"prefix":prefix,"review":Installer::open(&prefix)?.review_recovery()?, "recovery":"not performed; --yes resolves only the recorded interrupted activation",
                    "preserved":"live workspace data and all installation generations; no host is restarted or stopped"})
            }
        }
        PackageCommand::CompatibleClient { prefix } => {
            let store = Store::open(data_dir)?;
            let matched = idk_workspace::client::compatible_host_client(&store, prefix.as_deref())?;
            json!({"compatible_client":matched,"performed":false,"preserved":"live host, processes, workspace data and installation generations","next":"Use inspect_command first; older TUI requires a workspace schema that its generation supports."})
        }
        PackageCommand::VerifyGeneration { prefix, generation } => {
            let prefix = resolve_prefix(prefix)?;
            ensure!(prefix.is_dir(), "installation prefix does not exist");
            json!(Installer::open(&prefix)?.verify(&generation)?)
        }
        PackageCommand::Uninstall { prefix, yes } => {
            let prefix = resolve_prefix(prefix)?;
            ensure!(prefix.is_dir(), "installation prefix does not exist");
            let installer = Installer::open(&prefix)?;
            let state = installer.status()?;
            if yes {
                json!(installer.uninstall_entrypoints()?)
            } else {
                json!({"installation":state,"remove":[prefix.join("idk"),prefix.join("current")],
                    "performed":false,"confirmation":"--yes removes only these managed links",
                    "preserved":"all generations, configuration, runs, logs and active hosts"})
            }
        }
    };
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

fn install_verified(
    data_dir: Option<&std::path::Path>,
    verified: VerifiedBundle,
    prefix: PathBuf,
    yes: bool,
) -> Result<serde_json::Value> {
    if !yes {
        let previous = if prefix.try_exists()? {
            Some(Installer::open(&prefix)?.status()?)
        } else {
            None
        };
        Ok(
            json!({"review":verified.review(), "prefix":prefix, "previous":previous,
            "activation":"not performed; repeat with --yes after reviewing the trusted digest and target",
            "preserved":"all existing generations, workspace data, terminal/run hosts and user files",
            "compatibility":"existing hosts keep their original generation; activation checks saved schema compatibility"}),
        )
    } else {
        let store = Store::open(data_dir)?;
        Ok(json!(Installer::open(&prefix)?.install(&verified, &store)?))
    }
}

fn resolve_prefix(value: Option<PathBuf>) -> Result<PathBuf> {
    use anyhow::Context;
    let prefix = match value {
        Some(path) => path,
        None => PathBuf::from(
            std::env::var_os("HOME").context("HOME is not set; supply --prefix /absolute/path")?,
        )
        .join(".local/share/idk"),
    };
    ensure!(prefix.is_absolute(), "installation prefix must be absolute");
    Ok(prefix)
}
