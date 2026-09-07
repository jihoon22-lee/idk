use clap::{Parser, Subcommand};
mod cli_project;
mod cli_session;

#[derive(Parser)]
#[command(name = "idk", version, about = "Project-centric terminal workspace")]
struct Cli {
    /// Isolated configuration/state/runtime root (default: XDG idk/v0.4).
    #[arg(long, global = true)]
    data_dir: Option<std::path::PathBuf>,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Inspect this binary's build and protocol identity.
    Version,
    /// Connect and manage project definitions.
    Project {
        #[command(subcommand)]
        command: cli_project::ProjectCommand,
    },
    /// Manage saved terminal definitions.
    Terminal {
        #[command(subcommand)]
        command: cli_project::TerminalCommand,
    },
    /// Inspect or explicitly manage the per-user background host.
    #[command(name = "host")]
    HostControl {
        #[command(subcommand)]
        command: cli_session::HostCommand,
    },
    /// Inspect and manage runtime shells independently of their saved definitions.
    Session {
        #[command(subcommand)]
        command: cli_session::SessionCommand,
    },
    /// Diagnose configuration and local tools without starting project commands.
    Doctor {
        #[arg(long)]
        json: bool,
        /// Return nonzero for warnings/errors (default diagnostic exit is zero).
        #[arg(long)]
        strict: bool,
    },
    /// Run finite synthetic csh/PTY checks without accessing project data.
    Probe {
        #[arg(long)]
        shell: std::path::PathBuf,
    },
    #[command(name = "__host", hide = true)]
    Host {
        #[arg(long)]
        config_dir: std::path::PathBuf,
        #[arg(long)]
        state_dir: std::path::PathBuf,
        #[arg(long)]
        runtime_dir: std::path::PathBuf,
    },
    #[command(name = "__shell-exec", hide = true)]
    ShellExec {
        #[arg(long)]
        shell: std::path::PathBuf,
        #[arg(long)]
        login: bool,
        #[arg(long)]
        bsd_login: bool,
        #[arg(long)]
        command: Option<String>,
    },
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Some(Commands::Project { command }) => {
            let result = idk_workspace::store::Store::open(cli.data_dir.as_deref())
                .and_then(|store| cli_project::project(&store, command));
            if let Err(error) = result {
                eprintln!("idk: {error:#}");
                std::process::exit(1);
            }
        }
        Some(Commands::Terminal { command }) => {
            let result = idk_workspace::store::Store::open(cli.data_dir.as_deref())
                .and_then(|store| cli_project::terminal(&store, command));
            if let Err(error) = result {
                eprintln!("idk: {error:#}");
                std::process::exit(1);
            }
        }
        Some(Commands::HostControl { command }) => {
            let result = idk_workspace::store::Store::open(cli.data_dir.as_deref())
                .and_then(|store| cli_session::host(&store, &std::env::current_exe()?, command));
            if let Err(error) = result {
                eprintln!("idk: {error:#}");
                std::process::exit(1);
            }
        }
        Some(Commands::Session { command }) => {
            let result = idk_workspace::store::Store::open(cli.data_dir.as_deref())
                .and_then(|store| cli_session::session(&store, &std::env::current_exe()?, command));
            if let Err(error) = result {
                eprintln!("idk: {error:#}");
                std::process::exit(1);
            }
        }
        Some(Commands::Version) => {
            println!(
                "idk {} (workspace protocol {})",
                env!("CARGO_PKG_VERSION"),
                idk_workspace::model::PROTOCOL
            )
        }
        Some(Commands::Host {
            config_dir,
            state_dir,
            runtime_dir,
        }) => {
            let result = (|| -> anyhow::Result<()> {
                for path in [&config_dir, &state_dir, &runtime_dir] {
                    if !path.is_absolute() {
                        anyhow::bail!("host directories must be absolute");
                    }
                    idk_workspace::store::ensure_private_dir(path)?;
                }
                // This internal process is started with null stdio. Separate its
                // lifetime from the UI's controlling terminal before any threads.
                if unsafe { libc::setsid() } < 0 {
                    return Err(std::io::Error::last_os_error().into());
                }
                let store = idk_workspace::store::Store {
                    config_dir,
                    state_dir,
                    runtime_dir,
                };
                idk_workspace::host::serve(store, std::env::current_exe()?)
            })();
            if let Err(error) = result {
                eprintln!("host: {error:#}");
                std::process::exit(1);
            }
        }
        Some(Commands::Doctor { json, strict }) => {
            let report = idk_workspace::doctor::inspect(cli.data_dir.as_deref());
            if json {
                println!("{}", serde_json::to_string_pretty(&report).unwrap());
            } else {
                println!("idk {}", report.version);
                for finding in &report.findings {
                    println!("{} [{}]: {}", finding.area, finding.status, finding.detail);
                }
                println!("{}", report.field_acceptance);
            }
            if strict && report.has_failures() {
                std::process::exit(1);
            }
        }
        Some(Commands::Probe { shell }) => match idk_workspace::probe::run(&shell) {
            Ok(report) => println!("{}", serde_json::to_string_pretty(&report).unwrap()),
            Err(error) => {
                eprintln!("probe failed: {error:#}");
                std::process::exit(1);
            }
        },
        None => {
            use std::io::IsTerminal;
            if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
                eprintln!("idk requires a terminal; use --help for commands");
                std::process::exit(2);
            }
            let result = idk_workspace::store::Store::open(cli.data_dir.as_deref())
                .and_then(|store| idk_workspace::ui::run(&store));
            if let Err(error) = result {
                eprintln!("{error:#}");
                std::process::exit(1);
            }
        }
        Some(Commands::ShellExec {
            shell,
            login,
            bsd_login,
            command,
        }) => {
            use std::os::unix::process::CommandExt;
            let basename = shell.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if !shell.is_absolute() || !matches!(basename, "tcsh" | "csh" | "bsd-csh") {
                eprintln!("an absolute csh/tcsh executable is required");
                std::process::exit(2);
            }
            let mut process = std::process::Command::new(&shell);
            process.arg0(if login || bsd_login {
                format!(
                    "-{}",
                    if basename == "bsd-csh" {
                        "csh"
                    } else {
                        basename
                    }
                )
            } else {
                basename.to_owned()
            });
            if bsd_login {
                if command.is_some() {
                    eprintln!("BSD login requires interactive wrapper startup");
                    std::process::exit(2);
                }
                process.env_remove("HOME");
            } else if let Some(command) = command {
                process.arg("-f");
                process.arg("-c").arg(command);
            } else {
                process.args(["-f", "-i"]);
            }
            let error = process.exec();
            eprintln!("cannot execute selected shell: {error}");
            std::process::exit(1);
        }
    }
}
