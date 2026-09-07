//! Managed tcsh startup, preserving one real shell and its original HOME.
//!
//! csh/tcsh have no rcfile option. We start with `-f`, then submit one complete source
//! invocation; all further initialization is read from private files. A script
//! reading stdin can therefore never consume a queued `cd` or task command.
//! Startup follows the Linux tcsh paths/order, including the `lf` build option.
//! This is an explicit managed policy: automatic startup error recovery is
//! replaced by final-source-status checks. tcsh and BSD csh use separate rc rules.
//! Commands, aliases and shell-local variables remain in this process. Native
//! login identity is established by the launcher before initialization. BSD csh
//! requires no arguments and initially absent HOME; the wrapper restores both
//! HOME and home before any startup script. tcsh uses -f with a login argv0.
//! Fast mode disables automatic history/dir-stack saving; existing history and
//! native live history are available. No full native-startup equivalence is claimed.
use std::collections::BTreeMap;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::model::SourceSpec;
use anyhow::{bail, Context, Result};
use portable_pty::CommandBuilder;
use uuid::Uuid;

pub const STARTUP_POLICY_VERSION: u32 = 2;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ShellKind {
    Tcsh,
    BsdCsh,
}

#[derive(Clone, Debug)]
pub struct ShellInspection {
    pub canonical: PathBuf,
    pub kind: ShellKind,
    pub login_first: bool,
}

#[derive(Clone)]
pub struct ShellPlan {
    pub shell: PathBuf,
    pub login: bool,
    pub init_cwd: PathBuf,
    pub start_cwd: PathBuf,
    pub sources: Vec<SourceSpec>,
    /// Complete project launch environment; host environment is not inherited.
    pub env: BTreeMap<String, String>,
    pub command: Option<String>,
}

impl std::fmt::Debug for ShellPlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ShellPlan")
            .field("shell", &self.shell)
            .field("login", &self.login)
            .field("init_cwd", &self.init_cwd)
            .field("start_cwd", &self.start_cwd)
            .field("source_count", &self.sources.len())
            .field("env", &"[redacted]")
            .field("has_command", &self.command.is_some())
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum InitializationState {
    /// Includes scripts waiting for terminal input. Silence is not readiness.
    Initializing,
    Ready,
    Failed,
}

pub struct PreparedShell {
    pub command: CommandBuilder,
    pub bootstrap_bytes: Vec<u8>,
    pub state_path: PathBuf,
    /// Caller keeps this directory for the shell lifetime and removes it after exit.
    pub resource_dir: PathBuf,
}

impl std::fmt::Debug for PreparedShell {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedShell")
            .field("state_path", &self.state_path)
            .field("resource_dir", &self.resource_dir)
            .field("bootstrap_len", &self.bootstrap_bytes.len())
            .finish()
    }
}

impl PreparedShell {
    pub fn state(&self) -> Result<InitializationState> {
        let bytes = crate::store::read_private(&self.state_path, 64)?;
        let state =
            std::str::from_utf8(&bytes).context("shell initialization state is not UTF-8")?;
        match state.trim() {
            "initializing" => Ok(InitializationState::Initializing),
            "ready" => Ok(InitializationState::Ready),
            "failed" => Ok(InitializationState::Failed),
            // Redirection briefly truncates the file. Never call it ready.
            "" => Ok(InitializationState::Initializing),
            _ => bail!("unrecognized shell initialization state"),
        }
    }
}

impl ShellPlan {
    /// `launcher` implements `__shell-exec --shell PATH [--login]
    /// [--command TEXT]`: exec selected shell with -f -i or -f -c TEXT.
    /// It sets argv0 to -tcsh/-csh for login, before any startup code runs.
    /// `--bsd-login` instead execs argv0=-csh with no arguments and HOME removed;
    /// this mode receives its single bootstrap source through the PTY even for tasks.
    pub fn prepare(&self, resource_root: &Path, launcher: &Path) -> Result<PreparedShell> {
        for (label, path) in [
            ("shell", &self.shell),
            ("initialization cwd", &self.init_cwd),
            ("start cwd", &self.start_cwd),
        ] {
            if !path.is_absolute() {
                bail!("{label} must be absolute");
            }
            quote_path(path)?;
        }
        if !self.shell.is_file() || !self.init_cwd.is_dir() || !self.start_cwd.is_dir() {
            bail!("shell and initialization/start directories must exist");
        }
        let home = self
            .env
            .get("HOME")
            .context("project environment needs HOME")?;
        if !Path::new(home).is_absolute() || !Path::new(home).is_dir() {
            bail!("project HOME must be an existing absolute directory");
        }
        quote(home)?;
        for (key, value) in &self.env {
            if key.is_empty() || key.contains(['=', '\0']) || value.contains('\0') {
                bail!("invalid environment entry");
            }
        }
        for source in &self.sources {
            let path = &source.path;
            quote_path(path)?;
            if source.args.len() > 64 {
                bail!("too many source arguments");
            }
            for arg in &source.args {
                if arg.len() > 8192 {
                    bail!("source argument exceeds 8192 bytes");
                }
                quote(arg)?;
            }
            let resolved = if path.is_absolute() {
                path.clone()
            } else {
                self.init_cwd.join(path)
            };
            if !resolved.is_file() {
                bail!(
                    "initialization source does not exist: {}",
                    resolved.display()
                );
            }
        }
        if self.command.as_ref().is_some_and(|c| c.contains('\0')) {
            bail!("registered command contains NUL");
        }
        if !resource_root.is_absolute() || !launcher.is_absolute() {
            bail!("shell resource root and launcher must be absolute");
        }
        let inspection = inspect_shell(&self.shell, &self.env)?;
        if inspection.kind == ShellKind::BsdCsh && self.sources.iter().any(|s| !s.args.is_empty()) {
            bail!("BSD csh does not support source arguments; select tcsh or remove the arguments");
        }
        fs::create_dir_all(resource_root)?;
        let resource_dir = resource_root.join(format!("shell-{}", Uuid::new_v4()));
        DirBuilder::new().mode(0o700).create(&resource_dir)?;
        let result = self.prepare_in(&resource_dir, launcher, home, &inspection);
        if result.is_err() {
            let _ = fs::remove_dir_all(&resource_dir);
        }
        result
    }

    fn prepare_in(
        &self,
        resource_dir: &Path,
        launcher: &Path,
        home: &str,
        inspection: &ShellInspection,
    ) -> Result<PreparedShell> {
        let bsd_login = self.login && inspection.kind == ShellKind::BsdCsh;
        let state_path = resource_dir.join("state");
        write_private(&state_path, "initializing\n")?;
        let state = quote_path(&state_path)?;
        let wrapper_path = resource_dir.join("startup.csh");
        let mut script =
            String::from("# Generated managed startup. Original files are sourced unchanged.\nset __idk_builtin = ( source echo exit set unset dirs cd rehash )\n");
        // BSD csh cannot combine native login identity and -f. Its launcher
        // omits HOME to trigger fast startup with argv0=-csh and no arguments.
        // Restore both forms before any user or system startup code can run.
        if bsd_login {
            script.push_str(&format!(
                "setenv HOME {}\nset home = {}\n",
                quote(home)?,
                quote(home)?
            ));
        }
        // Variable-expanded builtin names bypass aliases in BOTH shells. Quoted
        // builtin names are treated as external programs by BSD csh. Freeze the
        // inspected implementation policy: user environment/rc variables named
        // tcsh or version must not redirect our own startup decisions.
        script.push_str(&format!(
            "$__idk_builtin[4] __idk_login_first = {}\n",
            u8::from(inspection.login_first)
        ));
        if self.login {
            script.push_str("if ($__idk_login_first) then\n");
            optional_source(&mut script, Path::new("/etc/csh.login"), &state)?;
            script.push_str("endif\n");
        }
        optional_source(&mut script, Path::new("/etc/csh.cshrc"), &state)?;
        if self.login {
            script.push_str("if (! $__idk_login_first) then\n");
            optional_source(&mut script, Path::new("/etc/csh.login"), &state)?;
            script.push_str("endif\nif ($__idk_login_first) then\n");
            optional_source(&mut script, &Path::new(home).join(".login"), &state)?;
            script.push_str("endif\n");
        }
        let tcshrc = quote_path(&Path::new(home).join(".tcshrc"))?;
        let cshrc = quote_path(&Path::new(home).join(".cshrc"))?;
        script.push_str(&format!("$__idk_builtin[4] __idk_use_tcshrc = 0\nif ({}) then\nif (-e {tcshrc}) $__idk_builtin[4] __idk_use_tcshrc = 1\nendif\nif ($__idk_use_tcshrc) then\n$__idk_builtin[1] {tcshrc}\n", u8::from(inspection.kind == ShellKind::Tcsh)));
        check_status(&mut script, &state);
        script.push_str(&format!(
            "else if (-e {cshrc}) then\n$__idk_builtin[1] {cshrc}\n"
        ));
        check_status(&mut script, &state);
        script.push_str("endif\n");
        if self.command.is_none() {
            script.push_str("$__idk_builtin[8]\n");
        }
        // tcsh loads history between rc and .login, also for non-login shells.
        script.push_str("if ($?histfile) then\nif (-f \"$histfile\") $__idk_builtin[1] -h \"$histfile\"\nelse\nif (-f \"$home/.history\") $__idk_builtin[1] -h \"$home/.history\"\nendif\n");
        if self.login {
            script.push_str("if (! $__idk_login_first) then\n");
            optional_source(&mut script, &Path::new(home).join(".login"), &state)?;
            script.push_str("endif\n");
            if inspection.kind == ShellKind::Tcsh {
                script.push_str("$__idk_builtin[6] -L\n");
            }
        }
        script
            .push_str("$__idk_builtin[5] __idk_login_first\n$__idk_builtin[5] __idk_use_tcshrc\n");
        // A startup rc can cd; configured relative sources still start from the
        // configured initialization cwd, while subsequent sources may cd normally.
        script.push_str(&format!(
            "$__idk_builtin[7] {}\n",
            quote_path(&self.init_cwd)?
        ));
        check_status(&mut script, &state);
        for source in &self.sources {
            let path = &source.path;
            let resolved = if path.is_absolute() {
                path.clone()
            } else {
                self.init_cwd.join(path)
            };
            let args = source
                .args
                .iter()
                .map(|arg| quote(arg))
                .collect::<Result<Vec<_>>>()?
                .join(" ");
            script.push_str(&format!(
                "$__idk_builtin[1] {} {args}\n",
                quote_path(&resolved)?
            ));
            check_status(&mut script, &state);
        }
        script.push_str(&format!(
            "$__idk_builtin[7] {}\n",
            quote_path(&self.start_cwd)?
        ));
        check_status(&mut script, &state);
        script.push_str(&format!("$__idk_builtin[2] ready >! {state}\n"));
        if let Some(task) = &self.command {
            let task_path = resource_dir.join("task.csh");
            write_private(&task_path, &format!("# Registered csh command\n{task}\n"))?;
            script.push_str(&format!(
                "$__idk_builtin[1] {}\n$__idk_builtin[3] $status\n",
                quote_path(&task_path)?
            ));
        }
        if self.command.is_none() {
            script.push_str("$__idk_builtin[5] __idk_builtin\n");
        }
        let private_name = format!("__idk_{}", &Uuid::new_v4().simple().to_string()[..12]);
        let script = script
            .replace("__idk_builtin", &private_name)
            .replace("__idk_login_first", &format!("{private_name}l"))
            .replace("__idk_use_tcshrc", &format!("{private_name}r"));
        write_private(&wrapper_path, &script)?;
        let invocation = if bsd_login && self.command.is_some() {
            // BSD csh exit ends a sourced file. Execute the final exit at the
            // top level; this whole line is parsed before any script can read
            // stdin, so its tail cannot be consumed as an input response.
            format!("source {}; exit $status", quote_path(&wrapper_path)?)
        } else {
            format!("source {}", quote_path(&wrapper_path)?)
        };
        let mut command = CommandBuilder::new(launcher);
        command.args(["__shell-exec", "--shell"]);
        command.arg(&self.shell);
        if self.login {
            command.arg("--login");
        }
        if bsd_login {
            command.arg("--bsd-login");
        }
        let bootstrap_bytes = if self.command.is_some() && !bsd_login {
            command.arg("--command");
            // No post-source echo: that would replace the actual task exit code.
            command.arg(format!("source {}", quote_path(&wrapper_path)?));
            Vec::new()
        } else {
            format!("{invocation}\n").into_bytes()
        };
        command.cwd(&self.init_cwd);
        command.env_clear();
        for (key, value) in &self.env {
            command.env(key, value);
        }
        command.env("SHELL", &self.shell);
        Ok(PreparedShell {
            command,
            bootstrap_bytes,
            state_path,
            resource_dir: resource_dir.to_path_buf(),
        })
    }
}

/// Inspect only the selected executable, with startup disabled and bounded output/time.
pub fn inspect_shell(
    path: &Path,
    environment: &BTreeMap<String, String>,
) -> Result<ShellInspection> {
    if !path.is_absolute() {
        bail!("shell executable must be absolute");
    }
    quote_path(path)?;
    let canonical = path.canonicalize().context("resolve shell executable")?;
    let metadata = canonical.metadata()?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        bail!("shell must be an executable regular file");
    }
    let mut child = Command::new(path)
        .args([
            "-f",
            "-c",
            "if ($?tcsh) then\necho tcsh\necho \"$version\"\nelse\necho bsd\nendif\n",
        ])
        .env_clear()
        .envs(environment)
        .env_remove("tcsh")
        .env_remove("version")
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("probe selected shell implementation")?;
    let mut output = child.stdout.take().context("shell probe stdout")?;
    let descriptor = output.as_raw_fd();
    if unsafe { libc::fcntl(descriptor, libc::F_SETFL, libc::O_NONBLOCK) } < 0 {
        let error = std::io::Error::last_os_error();
        let _ = child.kill();
        let _ = child.wait();
        return Err(error.into());
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut bytes = Vec::new();
    let result = (|| -> Result<ShellInspection> {
        loop {
            let mut buffer = [0; 1024];
            loop {
                match output.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        bytes.extend_from_slice(&buffer[..count]);
                        if bytes.len() > 4096 {
                            bail!("shell probe output exceeds limit");
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) => return Err(error.into()),
                }
            }
            if let Some(status) = child.try_wait()? {
                // Data emitted immediately before exit may become available
                // between the previous read and wait. Drain without blocking.
                loop {
                    match output.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(count) => {
                            bytes.extend_from_slice(&buffer[..count]);
                            if bytes.len() > 4096 {
                                bail!("shell probe output exceeds limit");
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(error) => return Err(error.into()),
                    }
                }
                if !status.success() {
                    bail!("selected shell implementation probe failed");
                }
                let text = std::str::from_utf8(&bytes).context("invalid shell probe response")?;
                let mut lines = text.lines();
                let kind = match lines.next() {
                    Some("tcsh") => ShellKind::Tcsh,
                    Some("bsd") => ShellKind::BsdCsh,
                    _ => bail!("selected executable is not recognized as csh/tcsh"),
                };
                let version = lines.next().unwrap_or_default();
                let login_first = version
                    .split_whitespace()
                    .last()
                    .is_some_and(|options| options.split(',').any(|option| option == "lf"));
                return Ok(ShellInspection {
                    canonical,
                    kind,
                    login_first,
                });
            }
            if Instant::now() >= deadline {
                bail!("selected shell implementation probe timed out");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

fn check_status(script: &mut String, state: &str) {
    script.push_str(&format!(
        "if ($status != 0) then\n$__idk_builtin[2] failed >! {state}\n$__idk_builtin[3] 125\nendif\n"
    ));
}

fn optional_source(script: &mut String, path: &Path, state: &str) -> Result<()> {
    let path = quote_path(path)?;
    script.push_str(&format!("if (-e {path}) then\n$__idk_builtin[1] {path}\n"));
    check_status(script, state);
    script.push_str("endif\n");
    Ok(())
}

fn write_private(path: &Path, text: &str) -> Result<()> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?
        .write_all(text.as_bytes())?;
    Ok(())
}

fn quote_path(path: &Path) -> Result<String> {
    quote(path.to_str().context("shell paths must be UTF-8")?)
}

fn quote(value: &str) -> Result<String> {
    // tcsh single quotes retain history substitution; escape ! separately.
    // Reject line/control separators rather than generating ambiguous scripts.
    if value.chars().any(char::is_control) {
        bail!("shell paths must not contain control characters");
    }
    Ok(format!(
        "'{}'",
        value.replace('!', "\\!").replace('\'', "'\\''")
    ))
}
