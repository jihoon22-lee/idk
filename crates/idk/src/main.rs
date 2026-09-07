use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "idk", version, about = "Project-centric terminal workspace")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Inspect this binary's build and protocol identity.
    Version,
    /// Run finite synthetic csh/PTY checks without accessing project data.
    Probe {
        #[arg(long)]
        shell: std::path::PathBuf,
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
        Some(Commands::Version) => {
            println!("idk {} (workspace protocol 1)", env!("CARGO_PKG_VERSION"))
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
            if let Err(error) = idk_workspace::ui::run() {
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
