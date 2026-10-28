//! Read-only ecosystem discovery and explicitly requested optional-tool preparation.
//! Reading a project never invokes its package manager, installs tools or executes scripts.
use crate::model::{new_id, FailurePolicy, TaskDefinition, TaskExecution, TaskLogging};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

const MAX_MANIFEST: u64 = 1024 * 1024;
const MAX_CANDIDATES: usize = 128;
pub const MAX_ARTIFACT_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCandidate {
    pub name: String,
    pub program: String,
    pub resolved_program: Option<PathBuf>,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub origin: PathBuf,
    pub available: bool,
    pub missing_tool: Option<String>,
}
impl TaskCandidate {
    /// A draft only. The caller must use the existing task review/approval/start contract.
    pub fn task_definition(&self) -> Result<TaskDefinition> {
        ensure!(self.available, "candidate tool is unavailable");
        let program = self
            .resolved_program
            .clone()
            .context("candidate has no resolved executable")?;
        Ok(process_task(
            &self.name,
            &self.cwd,
            program,
            self.args.clone(),
        ))
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiscoveryReport {
    pub candidates: Vec<TaskCandidate>,
    pub warnings: Vec<String>,
}

pub fn discover_tasks(root: &Path, env: &BTreeMap<String, String>) -> Result<DiscoveryReport> {
    let root = root
        .canonicalize()
        .context("project directory unavailable")?;
    ensure!(root.is_dir(), "project root must be a directory");
    let mut report = DiscoveryReport::default();
    let cargo = root.join("Cargo.toml");
    match read_manifest(&cargo) {
        Ok(Some(bytes)) => match std::str::from_utf8(&bytes)
            .ok()
            .and_then(|s| s.parse::<toml::Value>().ok())
        {
            Some(manifest)
                if manifest.get("package").is_some_and(toml::Value::is_table)
                    || manifest.get("workspace").is_some_and(toml::Value::is_table) =>
            {
                for verb in ["build", "test"] {
                    report.candidates.push(candidate(
                        format!("cargo {verb}"),
                        "cargo",
                        vec![verb.into(), "--manifest-path".into(), "Cargo.toml".into()],
                        &root,
                        &cargo,
                        env,
                    ));
                }
            }
            _ => report
                .warnings
                .push("Cargo.toml is malformed or has no package/workspace table".into()),
        },
        Ok(None) => {}
        Err(error) => report.warnings.push(format!("Cargo.toml: {error}")),
    }
    let package = root.join("package.json");
    match read_manifest(&package) {
        Ok(Some(bytes)) => match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(manifest) => {
                let manager = match manifest.get("packageManager") {
                    None => "npm",
                    Some(value) => value
                        .as_str()
                        .and_then(|text| text.split('@').next())
                        .unwrap_or(""),
                };
                if !["npm", "pnpm", "yarn"].contains(&manager) {
                    report
                        .warnings
                        .push("package.json declares an unsupported package manager".into());
                } else if let Some(scripts) = manifest.get("scripts").and_then(|v| v.as_object()) {
                    for (name, script) in scripts {
                        if report.candidates.len() >= MAX_CANDIDATES {
                            report
                                .warnings
                                .push("task discovery reached the 128-candidate limit".into());
                            break;
                        }
                        if !script.is_string()
                            || name.is_empty()
                            || name.starts_with('-')
                            || name.len() + manager.len() + 1 > 100
                            || name.chars().any(char::is_control)
                        {
                            report.warnings.push(
                                "package.json contains an unsupported script name/value".into(),
                            );
                            continue;
                        }
                        report.candidates.push(candidate(
                            format!("{manager} {name}"),
                            manager,
                            vec!["run".into(), name.clone()],
                            &root,
                            &package,
                            env,
                        ));
                    }
                } else if manifest.get("scripts").is_some() {
                    report
                        .warnings
                        .push("package.json scripts must be an object".into());
                }
            }
            Err(_) => report
                .warnings
                .push("package.json is malformed JSON".into()),
        },
        Ok(None) => {}
        Err(error) => report.warnings.push(format!("package.json: {error}")),
    }
    Ok(report)
}
fn candidate(
    name: String,
    program: &str,
    args: Vec<String>,
    cwd: &Path,
    origin: &Path,
    env: &BTreeMap<String, String>,
) -> TaskCandidate {
    let resolved = resolve_program(program, cwd, env).ok();
    TaskCandidate {
        name,
        program: program.into(),
        available: resolved.is_some(),
        missing_tool: resolved.is_none().then(|| program.into()),
        resolved_program: resolved,
        args,
        cwd: cwd.into(),
        origin: origin.into(),
    }
}
fn read_manifest(path: &Path) -> Result<Option<Vec<u8>>> {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let before = FileIdentity::of(&file.metadata()?);
    ensure!(
        file.metadata()?.is_file() && before.size <= MAX_MANIFEST,
        "manifest must be an ordinary file of at most 1 MiB"
    );
    let mut bytes = Vec::new();
    (&mut file).take(MAX_MANIFEST + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 == before.size
            && before == FileIdentity::of(&file.metadata()?)
            && before == FileIdentity::of(&fs::symlink_metadata(path)?),
        "manifest changed while reading"
    );
    Ok(Some(bytes))
}

/// Resolve exactly the selected executable; explicit paths never fall back to another program.
pub fn resolve_program(
    program: &str,
    cwd: &Path,
    env: &BTreeMap<String, String>,
) -> Result<PathBuf> {
    ensure!(
        !program.is_empty() && !program.contains('\0'),
        "empty or invalid executable"
    );
    let path = Path::new(program);
    let candidates = if path.is_absolute() {
        vec![path.to_path_buf()]
    } else if program.contains('/') {
        vec![cwd.join(path)]
    } else {
        env.get("PATH")
            .map(|search| {
                std::env::split_paths(search)
                    .map(|entry| {
                        if entry.is_absolute() {
                            entry.join(path)
                        } else {
                            cwd.join(entry).join(path)
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    for candidate in candidates {
        if let Ok(metadata) = fs::metadata(&candidate) {
            if metadata.is_file() && metadata.mode() & 0o111 != 0 && executable_access(&candidate) {
                return candidate.canonicalize().context("resolve executable");
            }
        }
    }
    anyhow::bail!("selected executable is unavailable: {program}")
}
fn executable_access(path: &Path) -> bool {
    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) else {
        return false;
    };
    unsafe { libc::faccessat(libc::AT_FDCWD, path.as_ptr(), libc::X_OK, libc::AT_EACCESS) == 0 }
}

fn process_task(name: &str, cwd: &Path, program: PathBuf, args: Vec<String>) -> TaskDefinition {
    TaskDefinition {
        id: new_id(),
        name: name.into(),
        command: String::new(),
        cwd: cwd.into(),
        execution: Some(TaskExecution::Process {
            program,
            args,
            env: BTreeMap::new(),
        }),
        sources: Vec::new(),
        artifact: None,
        approved_digest: None,
        steps: Vec::new(),
        failure_policy: FailurePolicy::Stop,
        logging: TaskLogging::Raw,
        interactive: false,
        build_outputs: Vec::new(),
        artifact_from_task: None,
        timeout_seconds: None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileIdentity {
    pub device: u64,
    pub inode: u64,
    pub size: u64,
    pub mtime_seconds: i64,
    pub mtime_nanos: i64,
    pub ctime_seconds: i64,
    pub ctime_nanos: i64,
}
impl FileIdentity {
    fn of(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.len(),
            mtime_seconds: metadata.mtime(),
            mtime_nanos: metadata.mtime_nsec(),
            ctime_seconds: metadata.ctime(),
            ctime_nanos: metadata.ctime_nsec(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactSnapshot {
    pub path: PathBuf,
    pub sha256: String,
    pub size: u64,
    pub identity: FileIdentity,
}
/// Measure bytes, not a path relation. This does not modify ArtifactRelation.verified_bytes.
/// A snapshot is a digest observation, not a copy or a promise the path stays unchanged.
pub fn snapshot_artifact(path: &Path, max_bytes: u64) -> Result<ArtifactSnapshot> {
    ensure!(path.is_absolute(), "artifact path must be absolute");
    ensure!(
        (1..=MAX_ARTIFACT_BYTES).contains(&max_bytes),
        "artifact budget must be 1 byte–1 GiB"
    );
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .context("open registered artifact")?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file(),
        "artifact must be an ordinary file, not a link or device"
    );
    let before = FileIdentity::of(&metadata);
    ensure!(
        before.size <= max_bytes,
        "artifact exceeds measurement budget"
    );
    ensure!(
        before == FileIdentity::of(&fs::symlink_metadata(path)?),
        "artifact path changed while opening"
    );
    let mut digest = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        ensure!(
            total <= max_bytes,
            "artifact grew beyond measurement budget"
        );
        digest.update(&buffer[..count]);
    }
    ensure!(
        total == before.size
            && before == FileIdentity::of(&file.metadata()?)
            && before == FileIdentity::of(&fs::symlink_metadata(path)?),
        "artifact changed during measurement"
    );
    Ok(ArtifactSnapshot {
        path: path.into(),
        sha256: format!("{:x}", digest.finalize()),
        size: total,
        identity: before,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptionalTool {
    Ici,
    Lens,
}
impl OptionalTool {
    fn name(self) -> &'static str {
        match self {
            Self::Ici => "ici",
            Self::Lens => "lens",
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInspection {
    pub kind: OptionalTool,
    pub requested_path: PathBuf,
    pub resolved_path: Option<PathBuf>,
    pub version: Option<String>,
    pub supported: bool,
    pub reason: Option<String>,
    pub executable_identity: Option<FileIdentity>,
}
/// This function executes --version, so callers MUST expose it only as an explicit
/// user action on a selected executable, never as part of discover_tasks.
pub fn inspect_optional_tool(
    kind: OptionalTool,
    path: &Path,
    environment: &BTreeMap<String, String>,
) -> Result<ToolInspection> {
    ensure!(
        path.is_absolute(),
        "optional tool selection must be an absolute path"
    );
    let mut inspection = ToolInspection {
        kind,
        requested_path: path.into(),
        resolved_path: None,
        version: None,
        supported: false,
        reason: None,
        executable_identity: None,
    };
    let result = (|| -> Result<()> {
        let resolved = path
            .canonicalize()
            .context("selected optional tool is missing")?;
        let metadata = fs::metadata(&resolved)?;
        ensure!(
            metadata.is_file() && metadata.mode() & 0o111 != 0 && executable_access(&resolved),
            "selected optional tool is not executable"
        );
        let identity = FileIdentity::of(&metadata);
        inspection.resolved_path = Some(resolved.clone());
        let output = version_output(&resolved, environment)?;
        ensure!(
            identity == FileIdentity::of(&fs::metadata(&resolved)?),
            "optional tool changed during version inspection"
        );
        let words = output.split_whitespace().collect::<Vec<_>>();
        ensure!(
            words.len() == 2 && words[0] == kind.name(),
            "unexpected optional tool version response"
        );
        let version = words[1].trim_start_matches('v');
        let parts = version
            .split('.')
            .map(str::parse::<u32>)
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("optional tool version is not a stable numeric release")?;
        ensure!(
            parts.len() == 3,
            "optional tool version must have major.minor.patch"
        );
        inspection.version = Some(version.into());
        let minimum = match kind {
            OptionalTool::Ici => 13,
            OptionalTool::Lens => 10,
        };
        ensure!(
            parts[0] == 0 && parts[1] >= minimum,
            "unsupported optional tool version; need {} 0.{}.x or a later 0.x release",
            kind.name(),
            minimum
        );
        inspection.executable_identity = Some(identity);
        inspection.supported = true;
        Ok(())
    })();
    if let Err(error) = result {
        inspection.reason = Some(error.to_string());
    }
    Ok(inspection)
}
fn version_output(path: &Path, environment: &BTreeMap<String, String>) -> Result<String> {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let mut command = Command::new(path);
    command
        .arg("--version")
        .env_clear()
        .envs(environment)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .context("start selected tool version check")?;
    let pid = child.id() as libc::pid_t;
    let result = (|| -> Result<String> {
        let mut stdout = child.stdout.take().context("version output unavailable")?;
        let fd = stdout.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        ensure!(
            flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0,
            "cannot bound version output"
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut bytes = Vec::new();
        let mut eof = false;
        loop {
            ensure!(
                Instant::now() < deadline,
                "optional tool version check timed out"
            );
            let mut poll = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            let polled = unsafe { libc::poll(&mut poll, 1, 20) };
            if polled < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::Interrupted {
                    return Err(error.into());
                }
            }
            if !eof && poll.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
                let mut chunk = [0; 1024];
                match stdout.read(&mut chunk) {
                    Ok(0) => eof = true,
                    Ok(count) => {
                        bytes.extend_from_slice(&chunk[..count]);
                        ensure!(
                            bytes.len() <= 8192,
                            "optional tool version output exceeds 8 KiB"
                        );
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) => {}
                    Err(error) => return Err(error.into()),
                }
            }
            let mut status: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let waited = unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid as u32,
                    &mut status,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if waited != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            if unsafe { status.si_pid() } != 0 && eof {
                ensure!(
                    status.si_code == libc::CLD_EXITED && unsafe { status.si_status() } == 0,
                    "optional tool version check failed"
                );
                return String::from_utf8(bytes).context("optional tool version is not UTF-8");
            }
        }
    })();
    // Keep the unreaped leader as SID identity until bounded inspection ends.
    // Only this newly created session is signalled; never arbitrary tool processes.
    let cleanup = cleanup_version_session(pid);
    let _ = child.kill();
    let _ = child.wait();
    cleanup.context("optional tool version cleanup is unconfirmed")?;
    result
}

fn cleanup_version_session(leader: libc::pid_t) -> Result<()> {
    use std::time::{Duration, Instant};
    ensure!(
        unsafe { libc::getsid(leader) } == leader,
        "version process session identity changed"
    );
    // The unreaped leader pins the SID. Include same-session children which moved
    // to their own process group; group-only kill would miss those children.
    unsafe {
        libc::kill(-leader, libc::SIGKILL);
    }
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let mut remaining = 0;
        for entry in fs::read_dir("/proc").context("inspect version child session")? {
            let entry = entry?;
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<libc::pid_t>().ok())
            else {
                continue;
            };
            if pid == leader {
                continue;
            }
            let stat = match fs::read_to_string(entry.path().join("stat")) {
                Ok(stat) => stat,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                    ) =>
                {
                    continue
                }
                Err(error) => return Err(error).context("read version child session identity"),
            };
            let fields = stat
                .rsplit_once(") ")
                .context("invalid process identity")?
                .1
                .split_whitespace()
                .collect::<Vec<_>>();
            let session = fields
                .get(3)
                .context("missing process session")?
                .parse::<libc::pid_t>()?;
            if session != leader {
                continue;
            }
            remaining += 1;
            // pidfd pins a particular process. Recheck its SID after opening to
            // prevent signalling a reused numeric PID from an inventory race.
            if fields.first() != Some(&"Z") {
                use std::os::fd::{AsRawFd, FromRawFd};
                let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
                if fd < 0 {
                    if std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                        continue;
                    }
                    return Err(std::io::Error::last_os_error())
                        .context("pin version child identity");
                }
                let pidfd = unsafe { fs::File::from_raw_fd(fd as i32) };
                let current = match fs::read_to_string(entry.path().join("stat")) {
                    Ok(value) => value,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.into()),
                };
                let sid = current
                    .rsplit_once(") ")
                    .context("invalid child identity")?
                    .1
                    .split_whitespace()
                    .nth(3)
                    .context("missing child session")?
                    .parse::<libc::pid_t>()?;
                if sid != leader {
                    continue;
                }
                let sent = unsafe {
                    libc::syscall(
                        libc::SYS_pidfd_send_signal,
                        pidfd.as_raw_fd(),
                        libc::SIGKILL,
                        std::ptr::null::<libc::siginfo_t>(),
                        0,
                    )
                };
                if sent != 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
                {
                    return Err(std::io::Error::last_os_error()).context("stop version child");
                }
            }
        }
        if remaining == 0 {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "same-session version descendants remain"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Prepare a reviewed script automatically, preserving argv and process exit. No
/// tool runs here, and no definition is saved or approved by this helper.
pub fn optional_task_draft(
    inspection: &ToolInspection,
    root: &Path,
    args: &[String],
) -> Result<TaskDefinition> {
    ensure!(
        inspection.supported,
        "optional tool is unavailable or unsupported; inspect a selected executable first"
    );
    let program = inspection
        .resolved_path
        .as_ref()
        .context("missing inspected executable")?;
    ensure!(
        inspection.executable_identity.as_ref() == Some(&FileIdentity::of(&fs::metadata(program)?)),
        "optional tool changed; repeat explicit version inspection"
    );
    let root = root
        .canonicalize()
        .context("project directory unavailable")?;
    ensure!(root.is_dir(), "project root must be a directory");
    crate::model::validate_execution_arguments(args, &BTreeMap::new())?;
    let args = if args.is_empty() {
        match inspection.kind {
            OptionalTool::Ici => vec!["analyze".into()],
            OptionalTool::Lens => {
                let database = root.join("compile_commands.json");
                ensure!(
                    database.is_file(),
                    "choose Lens report arguments; no compile_commands.json was found"
                );
                vec![
                    "build".into(),
                    "inspect".into(),
                    database
                        .to_str()
                        .context("compile database path is not UTF-8")?
                        .into(),
                ]
            }
        }
    } else {
        args.to_vec()
    };
    ensure!(
        !args.iter().any(
            |arg| matches!(arg.as_str(), "--result" | "--events" | "--format")
                || arg.starts_with("--result=")
                || arg.starts_with("--events=")
                || arg.starts_with("--format=")
        ),
        "result/events/format arguments are managed by the integration draft"
    );
    if inspection.kind == OptionalTool::Ici {
        ensure!(
            args.first().is_some_and(|arg| arg == "analyze")
                || args.get(..2).is_some_and(|pair| pair == ["next", "verify"]),
            "ici integration supports analyze or next verify"
        );
    } else {
        ensure!(
            args.first().is_some_and(|arg| [
                "build", "env", "abi", "test", "log", "sys", "trace", "net", "bundle"
            ]
            .contains(&arg.as_str())),
            "select a finite Lens report command"
        );
        ensure!(
            !args.iter().any(|arg| arg == "--follow"),
            "streaming Lens reports are not supported"
        );
    }
    let mut argv = vec![quote_arg(
        program.to_str().context("tool path is not UTF-8")?,
    )?];
    if inspection.kind == OptionalTool::Lens {
        argv.extend(["--format".into(), "envelope".into()]);
    }
    argv.extend(
        args.iter()
            .map(|arg| quote_arg(arg))
            .collect::<Result<Vec<_>>>()?,
    );
    let ending = match inspection.kind {
        OptionalTool::Ici => " --result \"${IDK_RESULT_PATH:?}\" --events \"${IDK_EVENTS_PATH:?}\"",
        OptionalTool::Lens => " > \"${IDK_RESULT_PATH:?}\"",
    };
    let script = format!("exec {}{}\n", argv.join(" "), ending);
    let mut task = process_task(
        &format!("{} report", inspection.kind.name()),
        &root,
        program.clone(),
        Vec::new(),
    );
    task.execution = Some(TaskExecution::Script {
        interpreter: PathBuf::from("/bin/sh"),
        args: Vec::new(),
        script,
        env: BTreeMap::new(),
    });
    Ok(task)
}
fn quote_arg(value: &str) -> Result<String> {
    ensure!(!value.contains('\0'), "argument contains NUL");
    Ok(format!("'{}'", value.replace('\'', "'\\''")))
}
