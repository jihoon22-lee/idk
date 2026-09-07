//! Actual Git discovery for project bindings and the source-use gate.
//!
//! Worktrees share objects but have distinct index/HEAD directories. Their
//! canonical per-worktree git directory is the gate identity. These paths do not
//! detect a repository deleted and recreated at the identical path, and an
//! external Git process remains outside the host's source-use lease.
mod parse;
mod service;
mod types;
pub use service::{GitOperationPlan, GitService};
pub use types::*;

use std::ffi::OsString;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(5);
const DISCOVERY_OUTPUT_LIMIT: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repository {
    pub root: PathBuf,
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
}

impl Repository {
    pub fn discover(path: &Path) -> Result<Self> {
        GitExecutable::discover()?.discover_repository(path)
    }

    /// Re-observe all three bindings before an operation. No cached cwd or
    /// terminal path is accepted as a substitute for the project repository.
    pub fn verify(&self) -> Result<()> {
        GitExecutable::discover()?.verify_repository(self)
    }

    pub fn identity(&self) -> &Path {
        &self.git_dir
    }
}

/// An absolute executable selected from absolute PATH entries and checked with
/// `git --version`. This is executable selection, not a binary authenticity or
/// sandbox guarantee. An explicitly selected absolute executable is supported.
#[derive(Debug, Clone)]
pub struct GitExecutable {
    executable: PathBuf,
}

impl GitExecutable {
    pub fn discover() -> Result<Self> {
        let search = std::env::var_os("PATH")
            .context("PATH is unavailable; select an absolute Git executable")?;
        for directory in std::env::split_paths(&search).filter(|path| path.is_absolute()) {
            let candidate = directory.join("git");
            if let Ok(metadata) = candidate.metadata() {
                if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
                    // Respect the first executable PATH match. Do not silently
                    // replace a broken selected Git with a different program.
                    return Self::from_path(&candidate);
                }
            }
        }
        bail!("Git executable was not found in absolute PATH directories")
    }

    pub fn from_path(path: &Path) -> Result<Self> {
        ensure!(
            path.is_absolute(),
            "Git executable must be an absolute path"
        );
        let executable = path.canonicalize().context("resolve Git executable")?;
        let metadata = executable.metadata().context("inspect Git executable")?;
        ensure!(
            metadata.is_file() && metadata.permissions().mode() & 0o111 != 0,
            "Git executable must be an executable regular file"
        );
        let output = run(
            &executable,
            Path::new("/"),
            &[OsString::from("--version")],
            Duration::from_secs(2),
            4096,
        )?;
        ensure!(
            output.status.success() && output.stdout.starts_with(b"git version "),
            "selected executable did not identify itself as Git"
        );
        Ok(Self { executable })
    }

    pub fn path(&self) -> &Path {
        &self.executable
    }

    pub fn discover_repository(&self, path: &Path) -> Result<Repository> {
        let cwd = path.canonicalize().context("resolve repository location")?;
        ensure!(cwd.is_dir(), "repository location must be a directory");
        // One result per process preserves path bytes, even embedded newlines.
        // These switches also work with older Git versions shipped on RHEL 8.
        let repository = Repository {
            root: self.rev_parse_path(&cwd, "--show-toplevel")?,
            git_dir: self.rev_parse_path(&cwd, "--absolute-git-dir")?,
            common_dir: self.rev_parse_path(&cwd, "--git-common-dir")?,
        };
        // Detect a changed gitfile between the independent observations. This
        // reduces, but cannot eliminate, external filesystem/Git races.
        ensure!(
            self.rev_parse_path(&cwd, "--absolute-git-dir")? == repository.git_dir,
            "repository binding changed during discovery; retry"
        );
        Ok(repository)
    }

    pub fn verify_repository(&self, repository: &Repository) -> Result<()> {
        ensure!(
            repository.root.is_absolute()
                && repository.git_dir.is_absolute()
                && repository.common_dir.is_absolute(),
            "stored repository binding must use absolute paths"
        );
        let observed = self.discover_repository(&repository.root)?;
        ensure!(
            &observed == repository,
            "repository/worktree binding changed; review the project repository before continuing"
        );
        Ok(())
    }

    fn rev_parse_path(&self, cwd: &Path, option: &str) -> Result<PathBuf> {
        let args: Vec<OsString> = [
            "--no-pager",
            "-c",
            "core.fsmonitor=",
            "-c",
            "core.quotePath=false",
            "-c",
            "color.ui=false",
            "rev-parse",
            option,
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        let output = run(
            &self.executable,
            cwd,
            &args,
            DISCOVERY_TIMEOUT,
            DISCOVERY_OUTPUT_LIMIT,
        )?;
        if !output.status.success() {
            let diagnostic: String = String::from_utf8_lossy(&output.stderr)
                .chars()
                .filter(|c| !c.is_control() || *c == '\n')
                .take(2048)
                .collect();
            bail!("Git repository discovery failed: {}", diagnostic.trim());
        }
        let bytes = output
            .stdout
            .strip_suffix(b"\n")
            .context("Git path response is incomplete")?;
        ensure!(
            !bytes.is_empty() && !bytes.contains(&0),
            "Git returned an invalid repository path"
        );
        let path = PathBuf::from(OsString::from_vec(bytes.to_vec()));
        let path = if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        };
        let canonical = path
            .canonicalize()
            .context("resolve Git-reported repository path")?;
        ensure!(
            canonical.is_dir(),
            "Git-reported repository path is not a directory"
        );
        Ok(canonical)
    }
}

#[derive(Debug)]
struct Output {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Environment intended for discovery/read plumbing. Authentication and normal
/// user configuration stay available, while inherited repository/index/object
/// selection and injected config cannot redirect a project operation.
fn sanitize_environment(command: &mut Command) {
    let names: Vec<OsString> = std::env::vars_os()
        .map(|(name, _)| name)
        .chain(command.get_envs().map(|(name, _)| name.to_owned()))
        .collect();
    for name in names {
        let name_text = name.to_string_lossy();
        if matches!(
            name_text.as_ref(),
            "GIT_DIR"
                | "GIT_WORK_TREE"
                | "GIT_INDEX_FILE"
                | "GIT_COMMON_DIR"
                | "GIT_OBJECT_DIRECTORY"
                | "GIT_ALTERNATE_OBJECT_DIRECTORIES"
                | "GIT_NAMESPACE"
                | "GIT_CEILING_DIRECTORIES"
                | "GIT_DISCOVERY_ACROSS_FILESYSTEM"
                | "GIT_CONFIG"
                | "GIT_CONFIG_SYSTEM"
                | "GIT_CONFIG_GLOBAL"
                | "GIT_CONFIG_NOSYSTEM"
                | "GIT_CONFIG_COUNT"
                | "GIT_CONFIG_PARAMETERS"
                | "GIT_REPLACE_REF_BASE"
                | "GIT_SHALLOW_FILE"
                | "GIT_GRAFT_FILE"
                | "GIT_PREFIX"
                | "GIT_EXEC_PATH"
                | "GIT_EXTERNAL_DIFF"
                | "GIT_DIFF_OPTS"
                | "GIT_TEMPLATE_DIR"
                | "GIT_FSMONITOR_TEST"
        ) || name_text.starts_with("GIT_CONFIG_KEY_")
            || name_text.starts_with("GIT_CONFIG_VALUE_")
            || name_text.starts_with("GIT_TRACE")
        {
            command.env_remove(name);
        }
    }
    command
        .env("GIT_PAGER", "cat")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C");
}

fn run(
    executable: &Path,
    cwd: &Path,
    args: &[OsString],
    timeout: Duration,
    limit: usize,
) -> Result<Output> {
    let mut command = Command::new(executable);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    sanitize_environment(&mut command);
    let mut child = command.spawn().context("start Git process")?;
    let result = collect(&mut child, timeout, limit);
    if result.is_err() {
        stop_owned_process(&mut child);
    }
    result
}

fn collect(child: &mut Child, timeout: Duration, limit: usize) -> Result<Output> {
    let mut stdout_pipe = child.stdout.take().context("Git stdout pipe unavailable")?;
    let mut stderr_pipe = child.stderr.take().context("Git stderr pipe unavailable")?;
    for fd in [stdout_pipe.as_raw_fd(), stderr_pipe.as_raw_fd()] {
        // SAFETY: these descriptors are owned live pipes. Flags do not transfer
        // ownership, and only this collector reads these pipe endpoints.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Err(std::io::Error::last_os_error()).context("configure Git output pipe");
        }
    }
    let deadline = Instant::now() + timeout;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut stdout_closed = false;
    let mut stderr_closed = false;
    loop {
        ensure!(
            Instant::now() < deadline,
            "Git command exceeded its deadline"
        );
        if !stdout_closed {
            stdout_closed = read_chunk(&mut stdout_pipe, &mut stdout)?;
        }
        if !stderr_closed {
            stderr_closed = read_chunk(&mut stderr_pipe, &mut stderr)?;
        }
        ensure!(
            stdout.len().saturating_add(stderr.len()) <= limit,
            "Git command exceeded its output limit"
        );
        // Do not reap the process before both pipes close: descendants might
        // still hold a pipe, and an unreaped child anchors our owned group ID.
        if stdout_closed && stderr_closed {
            if let Some(status) = child.try_wait().context("wait for Git process")? {
                return Ok(Output {
                    status,
                    stdout,
                    stderr,
                });
            }
        }
        let mut descriptors = [
            libc::pollfd {
                fd: if stdout_closed {
                    -1
                } else {
                    stdout_pipe.as_raw_fd()
                },
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: if stderr_closed {
                    -1
                } else {
                    stderr_pipe.as_raw_fd()
                },
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: the array contains two initialized pollfds with owned pipes.
        let polled = unsafe {
            libc::poll(
                descriptors.as_mut_ptr(),
                descriptors.len() as libc::nfds_t,
                5,
            )
        };
        if polled < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error).context("poll Git output");
            }
        }
    }
}

fn read_chunk(reader: &mut impl Read, output: &mut Vec<u8>) -> Result<bool> {
    let mut bytes = [0u8; 8192];
    match reader.read(&mut bytes) {
        Ok(0) => Ok(true),
        Ok(count) => {
            output.extend_from_slice(&bytes[..count]);
            Ok(false)
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(error).context("read Git output"),
    }
}

fn stop_owned_process(child: &mut Child) {
    let pid = child.id() as libc::pid_t;
    // The collector has not reaped this child on error. Its PID cannot be reused.
    // Only the group established for this invocation is eligible for signalling.
    if unsafe { libc::getpgid(pid) } == pid {
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Bounded read-only Git capture for run provenance. Does not acquire a mutation lease.
pub(crate) fn run_readonly(executable: &Path, cwd: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let args = args.iter().map(OsString::from).collect::<Vec<_>>();
    let output = run(executable, cwd, &args, Duration::from_secs(2), 1024 * 1024)?;
    ensure!(output.status.success(), "Git provenance observation failed");
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn read_environment_removes_routing_and_preserves_auth() {
        let mut command = Command::new("/usr/bin/git");
        command
            .env("GIT_DIR", "/wrong")
            .env("GIT_INDEX_FILE", "/wrong/index")
            .env("GIT_CONFIG_KEY_0", "core.worktree")
            .env("GIT_CONFIG_VALUE_0", "/wrong")
            .env("GIT_TRACE", "/wrong/trace")
            .env("HOME", "/home/fixture")
            .env("SSH_AUTH_SOCK", "/agent.sock")
            .env("GIT_SSH_COMMAND", "ssh -F /fixture/config");
        sanitize_environment(&mut command);
        let environment: std::collections::HashMap<_, _> = command.get_envs().collect();
        for name in [
            "GIT_DIR",
            "GIT_INDEX_FILE",
            "GIT_CONFIG_KEY_0",
            "GIT_CONFIG_VALUE_0",
            "GIT_TRACE",
        ] {
            assert_eq!(environment.get(OsStr::new(name)), Some(&None));
        }
        assert_eq!(
            environment.get(OsStr::new("HOME")),
            Some(&Some(OsStr::new("/home/fixture")))
        );
        assert_eq!(
            environment.get(OsStr::new("SSH_AUTH_SOCK")),
            Some(&Some(OsStr::new("/agent.sock")))
        );
        assert_eq!(
            environment.get(OsStr::new("GIT_SSH_COMMAND")),
            Some(&Some(OsStr::new("ssh -F /fixture/config")))
        );
    }

    #[test]
    fn command_runner_bounds_output_and_deadline() {
        let args = [
            OsString::from("-c"),
            OsString::from("while :; do printf 1234567890; done"),
        ];
        let error = run(
            Path::new("/bin/sh"),
            Path::new("/"),
            &args,
            Duration::from_secs(2),
            1024,
        )
        .unwrap_err();
        assert!(error.to_string().contains("output limit"));
        let start = Instant::now();
        let args = [OsString::from("-c"), OsString::from("sleep 30")];
        let error = run(
            Path::new("/bin/sh"),
            Path::new("/"),
            &args,
            Duration::from_millis(50),
            1024,
        )
        .unwrap_err();
        assert!(error.to_string().contains("deadline"));
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn executable_selection_requires_absolute_actual_git() {
        assert!(GitExecutable::from_path(Path::new("git")).is_err());
        assert!(GitExecutable::from_path(Path::new("/bin/true")).is_err());
        assert!(GitExecutable::discover().unwrap().path().is_absolute());
    }
}
