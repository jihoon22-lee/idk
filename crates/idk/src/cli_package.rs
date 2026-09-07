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
        #[arg(long)]
        prefix: PathBuf,
        #[arg(long)]
        yes: bool,
    },
    /// Inspect the committed installation and preserved generations.
    Status {
        #[arg(long)]
        prefix: PathBuf,
    },
    /// Resolve an interrupted activation without restoring live workspace data.
    Recover {
        #[arg(long)]
        prefix: PathBuf,
        #[arg(long)]
        yes: bool,
    },
    /// Verify all bytes of an explicitly selected preserved installation generation.
    VerifyGeneration {
        generation: String,
        #[arg(long)]
        prefix: PathBuf,
    },
    /// Remove managed entrypoints only; preserve generations, data, logs and live hosts.
    Uninstall {
        #[arg(long)]
        prefix: PathBuf,
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
            ensure!(prefix.is_absolute(), "installation prefix must be absolute");
            let verified = VerifiedBundle::read(&bundle, &sha256)?;
            if !yes {
                let previous = if prefix.try_exists()? {
                    Some(Installer::open(&prefix)?.status()?)
                } else {
                    None
                };
                json!({"review":verified.review(), "prefix":prefix, "previous":previous,
                    "activation":"not performed; repeat with --yes after reviewing the trusted digest and target",
                    "preserved":"all existing generations, workspace data, terminal/run hosts and user files",
                    "compatibility":"no schema migration or committed-release downgrade; existing hosts keep their original generation"})
            } else {
                let store = Store::open(data_dir)?;
                json!(Installer::open(&prefix)?.install(&verified, &store)?)
            }
        }
        PackageCommand::Status { prefix } => {
            ensure!(prefix.is_dir(), "installation prefix does not exist");
            json!(Installer::open(&prefix)?.status()?)
        }
        PackageCommand::Recover { prefix, yes } => {
            ensure!(prefix.is_dir(), "installation prefix does not exist");
            if yes {
                json!(Installer::open(&prefix)?.recover()?)
            } else {
                json!({"prefix":prefix,"review":Installer::open(&prefix)?.review_recovery()?, "recovery":"not performed; --yes resolves only the recorded interrupted activation",
                    "preserved":"live workspace data and all installation generations; no host is restarted or stopped"})
            }
        }
        PackageCommand::VerifyGeneration { prefix, generation } => {
            ensure!(prefix.is_dir(), "installation prefix does not exist");
            json!(Installer::open(&prefix)?.verify(&generation)?)
        }
        PackageCommand::Uninstall { prefix, yes } => {
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
