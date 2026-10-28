//! Portable project intent, resolved local definitions, and compare-and-swap editing.
//! Reading/resolving never executes a program or writes a project declaration.
use crate::model::{
    new_id, EditorConfig, FailurePolicy, NativeShell, Project, ShellConfig, SourceSpec,
    TaskDefinition, TaskExecution, TaskLogging, TaskStep, TerminalDefinition, Workspace,
};
use crate::project::{discover_shells, ConnectDraft, LaunchEnvironment, ProjectService};
use crate::store::{atomic_write, read_private, FileLock, Store};
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub const DECLARATION: &str = "idk.toml";
pub const DECLARATION_LIMIT: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Declaration {
    pub schema_version: u32,
    pub name: Option<String>,
    pub shell: Option<ShellDeclaration>,
    pub editor: Option<EditorDeclaration>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub terminals: BTreeMap<String, TerminalDeclaration>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tasks: BTreeMap<String, TaskDeclaration>,
}
impl Default for Declaration {
    fn default() -> Self {
        Self {
            schema_version: 1,
            name: None,
            shell: None,
            editor: None,
            terminals: BTreeMap::new(),
            tasks: BTreeMap::new(),
        }
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShellDeclaration {
    pub program: Option<String>,
    pub args: Option<Vec<String>>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub unset_env: Vec<String>,
    #[serde(default)]
    pub clear_env: bool,
    pub cwd: Option<PathBuf>,
    pub legacy_csh: Option<bool>,
    pub login: Option<bool>,
    pub sources: Option<Vec<SourceSpec>>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorDeclaration {
    pub program: Option<String>,
    pub args: Option<Vec<String>>,
    pub external_gui: Option<bool>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalDeclaration {
    pub name: Option<String>,
    pub cwd: Option<PathBuf>,
    pub sources: Option<Vec<SourceSpec>>,
    pub persistent: Option<bool>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskDeclaration {
    pub name: Option<String>,
    pub program: Option<String>,
    pub args: Option<Vec<String>>,
    pub interpreter: Option<String>,
    pub script: Option<String>,
    pub command: Option<String>,
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub sources: Vec<SourceSpec>,
    #[serde(default)]
    pub steps: Vec<TaskStep>,
    #[serde(default)]
    pub interactive: bool,
    #[serde(default)]
    pub disable_logging: bool,
    #[serde(default)]
    pub continue_on_failure: bool,
    pub artifact: Option<PathBuf>,
    #[serde(default)]
    pub build_outputs: Vec<PathBuf>,
    pub artifact_from_task: Option<String>,
    pub timeout_seconds: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    pub schema_version: u32,
    pub shell: Option<ShellDeclaration>,
    pub editor: Option<EditorDeclaration>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            schema_version: 1,
            shell: None,
            editor: None,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortableBinding {
    pub project_id: String,
    pub path: PathBuf,
    pub digest: String,
    #[serde(default)]
    pub task_ids: BTreeMap<String, String>,
    #[serde(default)]
    pub terminal_ids: BTreeMap<String, String>,
    pub shell_review_required: bool,
    #[serde(default)]
    pub terminal_reviews: BTreeSet<String>,
    /// Last resolved definitions, without approval changes. Used to preserve explicit local overrides.
    pub resolved: Project,
}
#[derive(Debug, Clone)]
pub struct DeclarationSnapshot {
    pub path: PathBuf,
    pub raw: String,
    pub digest: String,
    pub declaration: Declaration,
}
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedProject {
    pub revision: u64,
    pub project: Project,
    pub binding: Option<PortableBinding>,
    pub preferences_digest: Option<String>,
    /// Human-readable provenance; command values themselves remain in project.
    pub origins: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default)]
pub struct EnvironmentPatch {
    pub replace: Option<BTreeMap<String, String>>,
    pub clear: bool,
    pub set: BTreeMap<String, String>,
    pub unset: Vec<String>,
}
impl EnvironmentPatch {
    pub fn apply(&self, target: &mut BTreeMap<String, String>) -> Result<()> {
        ensure!(
            !(self.replace.is_some()
                && (self.clear || !self.set.is_empty() || !self.unset.is_empty())),
            "environment replacement cannot be combined with key patches/clear"
        );
        let mut next = if self.clear {
            BTreeMap::new()
        } else {
            target.clone()
        };
        if let Some(replacement) = &self.replace {
            next = replacement.clone();
        }
        for key in &self.unset {
            ensure!(
                !key.is_empty() && !key.contains(['=', '\0']),
                "invalid environment key to remove"
            );
            next.remove(key);
        }
        next.extend(self.set.clone());
        crate::model::validate_execution_arguments(&[], &next)?;
        *target = next;
        Ok(())
    }
}
/// UI/CLI share explicit argv replacement and key-based environment editing.
pub fn patch_native(
    shell: &mut ShellConfig,
    native: bool,
    args: Option<Vec<String>>,
    env: &EnvironmentPatch,
) -> Result<()> {
    if native && shell.native.is_none() {
        shell.native = Some(NativeShell::default());
        shell.login = false;
        shell.sources.clear();
    }
    if args.is_some()
        || env.replace.is_some()
        || env.clear
        || !env.set.is_empty()
        || !env.unset.is_empty()
    {
        let definition = shell
            .native
            .as_mut()
            .context("native argv/environment editing requires native mode")?;
        if let Some(args) = args {
            crate::model::validate_execution_arguments(&args, &definition.env)?;
            definition.args = args;
        }
        env.apply(&mut definition.env)?;
    }
    Ok(())
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn read_bytes(path: &Path) -> Result<Option<Vec<u8>>> {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("read declaration {}", path.display()))
        }
    };
    let before = file.metadata()?;
    ensure!(
        before.is_file(),
        "declaration must be a regular file: {}",
        path.display()
    );
    ensure!(
        before.len() <= DECLARATION_LIMIT as u64,
        "declaration exceeds 1 MiB: {}",
        path.display()
    );
    let mut bytes = Vec::new();
    (&mut file)
        .take(DECLARATION_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= DECLARATION_LIMIT,
        "declaration exceeds 1 MiB"
    );
    let after = file.metadata()?;
    ensure!(
        before.len() == after.len()
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec()
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec(),
        "declaration changed while reading; retry"
    );
    Ok(Some(bytes))
}
fn parse(raw: &str, path: &Path) -> Result<Declaration> {
    let declaration: Declaration = toml::from_str(raw)
        .with_context(|| format!("invalid {} (original preserved)", path.display()))?;
    ensure!(
        declaration.schema_version == 1,
        "unsupported declaration schema {} in {} (preserved)",
        declaration.schema_version,
        path.display()
    );
    ensure!(
        declaration.tasks.len() <= 128 && declaration.terminals.len() <= 64,
        "declaration exceeds task/terminal limits"
    );
    for key in declaration.tasks.keys().chain(declaration.terminals.keys()) {
        crate::model::valid_name(key)?;
    }
    Ok(declaration)
}
pub fn read(root: &Path) -> Result<Option<DeclarationSnapshot>> {
    let path = root.join(DECLARATION);
    let Some(bytes) = read_bytes(&path)? else {
        return Ok(None);
    };
    let raw = String::from_utf8(bytes).context("idk.toml is not UTF-8 (preserved)")?;
    let declaration = parse(&raw, &path)?;
    Ok(Some(DeclarationSnapshot {
        path,
        digest: digest(raw.as_bytes()),
        raw,
        declaration,
    }))
}
pub fn load_preferences(store: &Store) -> Result<(Preferences, Option<String>)> {
    let path = store.config_dir.join("preferences.toml");
    let bytes = match read_private(&path, DECLARATION_LIMIT) {
        Ok(bytes) => bytes,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok((Preferences::default(), None))
        }
        Err(error) => return Err(error),
    };
    let value: Preferences = toml::from_str(std::str::from_utf8(&bytes)?)
        .context("invalid preferences.toml (preserved)")?;
    ensure!(
        value.schema_version == 1,
        "unsupported preferences schema (preserved)"
    );
    Ok((value, Some(digest(&bytes))))
}
pub fn save_preferences(
    store: &Store,
    expected_digest: Option<&str>,
    preferences: &Preferences,
) -> Result<String> {
    ensure!(
        preferences.schema_version == 1,
        "unsupported preferences schema"
    );
    let _lock = FileLock::acquire(&store.config_dir.join("preferences.lock"), false)?;
    let (_, current) = load_preferences(store)?;
    ensure!(
        current.as_deref() == expected_digest,
        "preferences changed externally; draft and original preserved"
    );
    let text = toml::to_string_pretty(preferences)?;
    ensure!(
        text.len() <= DECLARATION_LIMIT,
        "preferences exceed size limit"
    );
    atomic_write(&store.config_dir.join("preferences.toml"), text.as_bytes())?;
    Ok(digest(text.as_bytes()))
}
/// Resolve an explicit path literally, or a bare executable name using the request PATH.
/// No probing process is spawned and a missing explicit program is never substituted.
pub fn resolve_program(
    program: &str,
    base: &Path,
    environment: &LaunchEnvironment,
) -> Result<PathBuf> {
    ensure!(
        !program.is_empty() && !program.contains('\0'),
        "executable must be nonempty without NUL"
    );
    let path = Path::new(program);
    let candidates = if path.is_absolute() {
        vec![path.to_path_buf()]
    } else if program.contains('/') {
        vec![base.join(path)]
    } else {
        environment
            .variables()
            .get("PATH")
            .map(|paths| {
                std::env::split_paths(paths)
                    .map(|p| {
                        if p.is_absolute() {
                            p.join(program)
                        } else {
                            base.join(p).join(program)
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    for path in candidates {
        if path
            .metadata()
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        {
            return Ok(path);
        }
    }
    bail!("executable {program:?} unavailable; install it or set an explicit path (no substitute selected)")
}
fn path(base: &Path, value: &Path) -> PathBuf {
    let value = if value.is_absolute() {
        value.into()
    } else {
        base.join(value)
    };
    value
        .components()
        .filter(|part| !matches!(part, std::path::Component::CurDir))
        .collect()
}
fn sources(base: &Path, values: &[SourceSpec]) -> Vec<SourceSpec> {
    values
        .iter()
        .map(|s| SourceSpec {
            path: path(base, &s.path),
            args: s.args.clone(),
        })
        .collect()
}
fn apply_shell(
    shell: &mut ShellConfig,
    declaration: &ShellDeclaration,
    base: &Path,
    env: &LaunchEnvironment,
) -> Result<()> {
    if let Some(program) = &declaration.program {
        shell.executable = resolve_program(program, base, env)?;
    }
    if let Some(cwd) = &declaration.cwd {
        shell.init_cwd = path(base, cwd);
    }
    if let Some(legacy) = declaration.legacy_csh {
        if legacy {
            shell.native = None;
        } else {
            patch_native(shell, true, None, &EnvironmentPatch::default())?;
        }
    }
    if let Some(login) = declaration.login {
        shell.login = login;
    }
    if let Some(values) = &declaration.sources {
        shell.sources = sources(base, values);
    }
    if declaration.args.is_some()
        || !declaration.env.is_empty()
        || !declaration.unset_env.is_empty()
        || declaration.clear_env
    {
        patch_native(
            shell,
            false,
            declaration.args.clone(),
            &EnvironmentPatch {
                clear: declaration.clear_env,
                set: declaration.env.clone(),
                unset: declaration.unset_env.clone(),
                replace: None,
            },
        )?;
    }
    Ok(())
}
fn apply_editor(
    editor: &mut Option<EditorConfig>,
    declaration: &EditorDeclaration,
    base: &Path,
    environment: &LaunchEnvironment,
) -> Result<()> {
    let program = match &declaration.program {
        Some(program) => resolve_program(program, base, environment)?,
        None => editor
            .as_ref()
            .context("editor args require an editor program or user preference")?
            .executable
            .clone(),
    };
    let args = declaration
        .args
        .clone()
        .or_else(|| editor.as_ref().map(|e| e.args.clone()))
        .unwrap_or_else(|| vec!["{file}".into()]);
    let external_gui = declaration
        .external_gui
        .or_else(|| editor.as_ref().map(|value| value.external_gui))
        .unwrap_or(false);
    *editor = Some(EditorConfig {
        executable: program,
        args,
        external_gui,
    });
    Ok(())
}
fn discover_default_editor(
    project: &mut Project,
    environment: &LaunchEnvironment,
    root: &Path,
    origins: &mut BTreeMap<String, String>,
) -> Result<()> {
    if project.editor.is_none() {
        if let Some(candidate) =
            crate::editor::discover_editor(None, environment.variables(), root)?
        {
            if candidate.available {
                origins.insert("editor".into(), format!("discovered: {}", candidate.origin));
                project.editor = Some(candidate.config);
            } else {
                origins.insert(
                    "editor".into(),
                    format!(
                        "unavailable {}: {}",
                        candidate.origin,
                        candidate.reason.unwrap_or_default()
                    ),
                );
            }
        }
    }
    Ok(())
}

fn semantic_task(task: &TaskDefinition) -> TaskDefinition {
    let mut task = task.clone();
    task.approved_digest = None;
    task
}
fn semantic_shell(shell: &ShellConfig) -> ShellConfig {
    let mut shell = shell.clone();
    shell.trusted_digest = None;
    shell
}
fn semantic_terminal(terminal: &TerminalDefinition) -> TerminalDefinition {
    let mut terminal = terminal.clone();
    terminal.trusted_digest = None;
    terminal
}
fn local_overrides(
    project: &mut Project,
    current: &Project,
    baseline: &Project,
    origins: &mut BTreeMap<String, String>,
) {
    macro_rules! scalar {
        ($field:ident) => {
            if current.shell.$field != baseline.shell.$field {
                project.shell.$field = current.shell.$field.clone();
                origins.insert(
                    concat!("shell.", stringify!($field)).into(),
                    "local override".into(),
                );
            }
        };
    }
    scalar!(executable);
    scalar!(login);
    scalar!(init_cwd);
    scalar!(sources);
    match (
        &current.shell.native,
        &baseline.shell.native,
        &mut project.shell.native,
    ) {
        (Some(local), Some(old), Some(next)) => {
            if local.args != old.args {
                next.args = local.args.clone();
                origins.insert("shell.args".into(), "local override".into());
            }
            for key in old.env.keys().filter(|key| !local.env.contains_key(*key)) {
                next.env.remove(key);
            }
            for (key, value) in &local.env {
                if old.env.get(key) != Some(value) {
                    next.env.insert(key.clone(), value.clone());
                    origins.insert(format!("shell.env.{key}"), "local override".into());
                }
            }
        }
        (local, old, next) if local != old => {
            *next = local.clone();
            origins.insert("shell.mode".into(), "local override".into());
        }
        _ => {}
    }
    if current.editor != baseline.editor {
        project.editor = current.editor.clone();
        origins.insert("editor".into(), "local override".into());
    }
    if current.name != baseline.name {
        project.name = current.name.clone();
        origins.insert("name".into(), "local override".into());
    }
    for next in &mut project.terminals {
        if let (Some(local), Some(old)) = (
            current.terminals.iter().find(|t| t.id == next.id),
            baseline.terminals.iter().find(|t| t.id == next.id),
        ) {
            if local.name != old.name {
                next.name = local.name.clone();
            }
            if local.cwd != old.cwd {
                next.cwd = local.cwd.clone();
            }
            if local.sources != old.sources {
                next.sources = local.sources.clone();
            }
            if local.persistent != old.persistent {
                next.persistent = local.persistent;
            }
        }
    }
}
pub fn resolve(
    store: &Store,
    root: &Path,
    environment: &LaunchEnvironment,
) -> Result<ResolvedProject> {
    resolve_for(store, root, environment, None)
}
pub fn resolve_for(
    store: &Store,
    root: &Path,
    environment: &LaunchEnvironment,
    selected: Option<&str>,
) -> Result<ResolvedProject> {
    let root = root
        .canonicalize()
        .context("project folder is unavailable")?;
    ensure!(root.is_dir(), "project path must be a directory");
    let workspace = store.load()?;
    let matches: Vec<_> = workspace
        .projects
        .iter()
        .filter(|p| p.root.canonicalize().ok().as_ref() == Some(&root))
        .collect();
    let existing = if let Some(id) = selected {
        let p = workspace.project(id)?;
        ensure!(
            matches.iter().any(|candidate| candidate.id == p.id),
            "selected project belongs to a different root"
        );
        Some(p)
    } else {
        ensure!(
            matches.len() <= 1,
            "multiple local projects share this root; choose a project ID"
        );
        matches.first().copied()
    };
    let previous = existing.and_then(|project| {
        workspace
            .portable
            .iter()
            .find(|binding| binding.project_id == project.id)
    });
    let declaration = read(&root)?;
    ensure!(previous.is_none() || declaration.is_some(), "managed idk.toml was removed; original local definitions preserved; restore the declaration before syncing");
    let (preferences, preferences_digest) = load_preferences(store)?;
    let mut origins = BTreeMap::new();
    if declaration.is_none() {
        if let Some(project) = existing {
            origins.insert("project".into(), "existing local definition".into());
            let mut project = project.clone();
            discover_default_editor(&mut project, environment, &root, &mut origins)?;
            return Ok(ResolvedProject {
                revision: workspace.revision,
                project,
                binding: None,
                preferences_digest,
                origins,
            });
        }
    }
    let declaration_value = declaration
        .as_ref()
        .map(|s| s.declaration.clone())
        .unwrap_or_default();
    let default_shell = discover_shells(environment)
        .first()
        .map(|s| s.path.clone())
        .context("no executable default shell; set a shell preference or declaration")?;
    let mut shell = ShellConfig {
        executable: default_shell,
        native: Some(NativeShell::default()),
        login: false,
        init_cwd: root.clone(),
        sources: vec![],
        trusted_digest: None,
    };
    origins.insert("shell".into(), "default shell candidate".into());
    if let Some(pref) = &preferences.shell {
        let mut pref = pref.clone();
        if declaration_value
            .shell
            .as_ref()
            .is_some_and(|shell| shell.program.is_some())
        {
            pref.program = None;
        }
        apply_shell(&mut shell, &pref, &store.config_dir, environment)?;
        origins.insert("shell".into(), "user preference".into());
    }
    if let Some(value) = &declaration_value.shell {
        apply_shell(&mut shell, value, &root, environment)?;
        origins.insert("shell".into(), "project declaration".into());
    }
    let name = declaration_value.name.clone().unwrap_or_else(|| {
        root.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    });
    let mut project = (ProjectService { store })
        .preview_connect(ConnectDraft {
            name,
            root: root.clone(),
            shell,
            terminals: vec![],
        })?
        .project;
    if let Some(existing) = existing {
        project.id = existing.id.clone();
        project.repository = existing.repository.clone();
        project.repository_binding = existing.repository_binding.clone();
        project.related_repositories = existing.related_repositories.clone();
    }
    let mut task_ids = previous.map(|p| p.task_ids.clone()).unwrap_or_default();
    let mut terminal_ids = previous.map(|p| p.terminal_ids.clone()).unwrap_or_default();
    for key in declaration_value.tasks.keys() {
        task_ids.entry(key.clone()).or_insert_with(new_id);
    }
    let terminal_values = if declaration_value.terminals.is_empty() {
        BTreeMap::from([("dev".into(), TerminalDeclaration::default())])
    } else {
        declaration_value.terminals.clone()
    };
    for key in terminal_values.keys() {
        terminal_ids.entry(key.clone()).or_insert_with(new_id);
    }
    for (key, value) in &terminal_values {
        project.terminals.push(TerminalDefinition {
            id: terminal_ids[key].clone(),
            name: value.name.clone().unwrap_or_else(|| key.clone()),
            cwd: value
                .cwd
                .as_ref()
                .map(|p| path(&root, p))
                .unwrap_or_else(|| {
                    if project.shell.native.is_some() {
                        project.shell.init_cwd.clone()
                    } else {
                        root.clone()
                    }
                }),
            sources: sources(&root, value.sources.as_deref().unwrap_or_default()),
            persistent: value.persistent.unwrap_or(true),
            trusted_digest: None,
        });
    }
    project.default_terminal = project
        .terminals
        .first()
        .filter(|t| t.persistent)
        .map(|t| t.id.clone());
    for (key, value) in &declaration_value.tasks {
        let modes = usize::from(value.program.is_some())
            + usize::from(value.interpreter.is_some() || value.script.is_some())
            + usize::from(value.command.is_some() || !value.steps.is_empty());
        ensure!(modes == 1, "tasks.{key} must choose exactly one of program, interpreter+script, or legacy command/steps");
        let execution = if let Some(program) = &value.program {
            Some(TaskExecution::Process {
                program: resolve_program(program, &root, environment)
                    .with_context(|| format!("tasks.{key}.program"))?,
                args: value.args.clone().unwrap_or_default(),
                env: value.env.clone(),
            })
        } else if let Some(interpreter) = &value.interpreter {
            Some(TaskExecution::Script {
                interpreter: resolve_program(interpreter, &root, environment)
                    .with_context(|| format!("tasks.{key}.interpreter"))?,
                args: value.args.clone().unwrap_or_default(),
                script: value
                    .script
                    .clone()
                    .context("interpreter requires inline script")?,
                env: value.env.clone(),
            })
        } else {
            ensure!(
                value.script.is_none() && value.args.is_none() && value.env.is_empty(),
                "legacy task args/env are configured through its legacy shell"
            );
            None
        };
        ensure!(
            execution.is_some() || project.shell.native.is_none(),
            "tasks.{key}: legacy command requires shell.legacy_csh=true"
        );
        project.tasks.push(TaskDefinition {
            id: task_ids[key].clone(),
            name: value.name.clone().unwrap_or_else(|| key.clone()),
            execution,
            command: value.command.clone().unwrap_or_default(),
            cwd: value
                .cwd
                .as_ref()
                .map(|p| path(&root, p))
                .unwrap_or_else(|| root.clone()),
            sources: sources(&root, &value.sources),
            artifact: value.artifact.as_ref().map(|p| path(&root, p)),
            approved_digest: None,
            steps: value.steps.clone(),
            failure_policy: if value.continue_on_failure {
                FailurePolicy::Continue
            } else {
                FailurePolicy::Stop
            },
            logging: if value.interactive || value.disable_logging {
                TaskLogging::Disabled
            } else {
                TaskLogging::Raw
            },
            interactive: value.interactive,
            build_outputs: value.build_outputs.iter().map(|p| path(&root, p)).collect(),
            artifact_from_task: value
                .artifact_from_task
                .as_ref()
                .map(|key| {
                    ensure!(
                        declaration_value.tasks.contains_key(key),
                        "unknown artifact_from_task key {key}"
                    );
                    Ok(task_ids[key].clone())
                })
                .transpose()?,
            timeout_seconds: value.timeout_seconds,
        });
        origins.insert(format!("tasks.{key}"), "project declaration".into());
    }
    if preferences.editor.is_some() || declaration_value.editor.is_some() {
        let mut editor = preferences.editor.clone().unwrap_or_default();
        let mut base = &store.config_dir;
        origins.insert("editor".into(), "user preference".into());
        if let Some(value) = &declaration_value.editor {
            if value.program.is_some() {
                editor.program = value.program.clone();
                base = &root;
            }
            if value.args.is_some() {
                editor.args = value.args.clone();
            }
            if value.external_gui.is_some() {
                editor.external_gui = value.external_gui;
            }
            origins.insert("editor".into(), "project declaration".into());
        }
        apply_editor(&mut project.editor, &editor, base, environment)?;
    }
    let mut baseline = project.clone();
    if let Some(existing) = existing {
        if let Some(previous) = previous {
            local_overrides(&mut project, existing, &previous.resolved, &mut origins);
            // A local edit of a declaration-owned command must never disappear on sync.
            for old in &previous.resolved.tasks {
                if let Some(local) = existing.tasks.iter().find(|t| t.id == old.id) {
                    ensure!(semantic_task(local) == semantic_task(old) || project.tasks.iter().any(|t| t.id == local.id && semantic_task(t) == semantic_task(local)), "declaration task {} was edited locally; update idk.toml or preserve the local draft before syncing", old.name);
                }
            }
        } else {
            // Connecting a declaration to an existing workspace preserves explicitly local shell/editor choices when unspecified.
            if declaration_value.shell.is_none() {
                project.shell = existing.shell.clone();
                origins.insert("shell".into(), "existing local definition".into());
            }
            if declaration_value.editor.is_none() {
                project.editor = existing.editor.clone();
            }
        }
        let managed_tasks: BTreeSet<_> = previous
            .into_iter()
            .flat_map(|p| p.task_ids.values())
            .collect();
        project.tasks.extend(
            existing
                .tasks
                .iter()
                .filter(|t| {
                    !managed_tasks.contains(&t.id) && !project.tasks.iter().any(|n| n.id == t.id)
                })
                .cloned()
                .collect::<Vec<_>>(),
        );
        let managed_terminals: BTreeSet<_> = previous
            .into_iter()
            .flat_map(|p| p.terminal_ids.values())
            .collect();
        project.terminals.extend(
            existing
                .terminals
                .iter()
                .filter(|t| {
                    !managed_terminals.contains(&t.id)
                        && !project.terminals.iter().any(|n| n.id == t.id)
                })
                .cloned()
                .collect::<Vec<_>>(),
        );
        if semantic_shell(&project.shell) == semantic_shell(&existing.shell) {
            project.shell.trusted_digest = existing.shell.trusted_digest.clone();
        }
        for terminal in &mut project.terminals {
            if let Some(old) = existing.terminals.iter().find(|t| {
                t.id == terminal.id && semantic_terminal(t) == semantic_terminal(terminal)
            }) {
                terminal.trusted_digest = old.trusted_digest.clone();
            }
        }
        for task in &mut project.tasks {
            if let Some(old) = existing
                .tasks
                .iter()
                .find(|t| t.id == task.id && semantic_task(t) == semantic_task(task))
            {
                task.approved_digest = old.approved_digest.clone();
            }
        }
        if existing.default_terminal.as_ref().is_some_and(|id| {
            project
                .terminals
                .iter()
                .any(|t| &t.id == id && t.persistent)
        }) {
            project.default_terminal = existing.default_terminal.clone();
        }
    }
    let undiscovered = project.editor.is_none();
    discover_default_editor(&mut project, environment, &root, &mut origins)?;
    if undiscovered {
        baseline.editor = project.editor.clone();
    }
    project.validate()?;
    let binding = declaration.map(|snapshot| PortableBinding {
        project_id: project.id.clone(),
        path: snapshot.path,
        digest: snapshot.digest,
        task_ids,
        terminal_ids: terminal_ids.clone(),
        shell_review_required: declaration_value
            .shell
            .as_ref()
            .is_some_and(|shell| shell != &ShellDeclaration::default()),
        terminal_reviews: declaration_value
            .terminals
            .iter()
            .filter(|(_, v)| v.cwd.is_some() || v.sources.is_some())
            .map(|(key, _)| terminal_ids[key].clone())
            .collect(),
        resolved: baseline,
    });
    Ok(ResolvedProject {
        revision: workspace.revision,
        project,
        binding,
        preferences_digest,
        origins,
    })
}
/// Persist definition and its identity mapping together. Declaration/preferences edits invalidate the preview.
pub fn apply(store: &Store, resolved: ResolvedProject) -> Result<Project> {
    let mut workspace = store.load()?;
    ensure!(
        workspace.revision == resolved.revision,
        "workspace changed; resolved draft preserved, resolve again"
    );
    if let Some(binding) = &resolved.binding {
        check_binding(binding)?;
    }
    ensure!(
        load_preferences(store)?.1 == resolved.preferences_digest,
        "preferences changed; resolve again"
    );
    let project = resolved.project;
    if let Some(existing) = workspace.projects.iter_mut().find(|p| p.id == project.id) {
        *existing = project.clone();
    } else {
        workspace.projects.push(project.clone());
    }
    workspace
        .portable
        .retain(|binding| binding.project_id != project.id);
    if let Some(binding) = resolved.binding {
        workspace.portable.push(binding);
    }
    workspace.selected_project = Some(project.id.clone());
    workspace.recent_projects.retain(|id| id != &project.id);
    workspace.recent_projects.insert(0, project.id.clone());
    workspace.recent_projects.truncate(32);
    store.save(&mut workspace, resolved.revision)?;
    Ok(project)
}
fn check_binding(binding: &PortableBinding) -> Result<()> {
    let current = read_bytes(&binding.path)?
        .context("managed idk.toml was removed; restore/sync before reviewing or executing")?;
    ensure!(digest(&current) == binding.digest, "idk.toml changed externally; sync and review the current resolved definition before executing");
    Ok(())
}
pub fn ensure_current(store: &Store, project_id: &str) -> Result<()> {
    let workspace = store.load()?;
    ensure_workspace_current(&workspace, project_id)
}
pub fn ensure_workspace_current(workspace: &Workspace, project_id: &str) -> Result<()> {
    if let Some(binding) = workspace
        .portable
        .iter()
        .find(|b| b.project_id == project_id)
    {
        check_binding(binding)?;
    }
    Ok(())
}
/// Compare-and-swap the supplied raw draft. Existing comments and overrides are retained if the caller retains them.
/// The lock serializes idk writers; external editor races are checked immediately before atomic rename.
pub fn write_declaration(
    store: &Store,
    path: &Path,
    expected_digest: Option<&str>,
    draft: &str,
) -> Result<String> {
    ensure!(
        path.is_absolute() && path.file_name().is_some_and(|name| name == DECLARATION),
        "declaration target must be an absolute idk.toml path"
    );
    ensure!(
        draft.len() <= DECLARATION_LIMIT,
        "declaration exceeds 1 MiB"
    );
    parse(draft, path)?;
    let parent = path.parent().context("declaration has no parent")?;
    let lock_name = format!(
        "portable-{}.lock",
        digest(path.as_os_str().as_encoded_bytes())
    );
    let _lock = FileLock::acquire(&store.config_dir.join(lock_name), false)?;
    // A descriptor lock also serializes cooperating writers using different data directories.
    // If another writer renamed this inode while we waited, reject instead of editing its replacement.
    let manifest = if expected_digest.is_some() {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
            .context("declaration disappeared before edit")?;
        ensure!(
            file.metadata()?.is_file(),
            "declaration must be a regular file"
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                break;
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::WouldBlock
                || std::time::Instant::now() >= deadline
            {
                return Err(error).context("declaration writer is busy; draft preserved");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        Some(file)
    } else {
        None
    };
    let check = || -> Result<()> {
        if let Some(file) = &manifest {
            let held = file.metadata()?;
            let named = fs::symlink_metadata(path)
                .context("declaration path changed while waiting for writer")?;
            ensure!(
                named.is_file() && held.dev() == named.dev() && held.ino() == named.ino(),
                "declaration was replaced while editing; original and draft preserved"
            );
        }
        let current = read_bytes(path)?;
        ensure!(
            current.as_ref().map(|bytes| digest(bytes)).as_deref() == expected_digest,
            "idk.toml changed externally; original and draft preserved"
        );
        Ok(())
    };
    check()?;
    let mode = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata.permissions().mode() & 0o777,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0o666,
        Err(error) => return Err(error.into()),
    };
    let temporary = parent.join(format!(".idk-declaration-{}.tmp", new_id()));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temporary)?;
        if manifest.is_some() {
            file.set_permissions(fs::Permissions::from_mode(mode))?;
        }
        file.write_all(draft.as_bytes())?;
        file.sync_all()?;
        check()?;
        if expected_digest.is_none() {
            // Atomic no-clobber publication, including another store or an external editor creating the file.
            fs::hard_link(&temporary, path)
                .context("idk.toml appeared during initialization; existing file preserved")?;
            fs::remove_file(&temporary)?;
        } else {
            fs::rename(&temporary, path)?;
        }
        fs::File::open(parent)?
            .sync_all()
            .context("declaration was replaced but directory durability could not be confirmed")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result?;
    Ok(digest(draft.as_bytes()))
}
/// Patch selected TOML fields while retaining all other declared overrides (formatting may normalize).
pub fn patch_declaration(
    snapshot: &DeclarationSnapshot,
    keys: &[&str],
    value: Option<toml::Value>,
) -> Result<String> {
    ensure!(
        !keys.is_empty() && keys[0] != "schema_version",
        "choose a declaration field"
    );
    let mut document: toml::Value = toml::from_str(&snapshot.raw)?;
    let mut table = document
        .as_table_mut()
        .context("declaration must be a table")?;
    for key in &keys[..keys.len() - 1] {
        table = table
            .entry((*key).to_owned())
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
            .as_table_mut()
            .context("patch parent must be a table")?;
    }
    if let Some(value) = value {
        table.insert(keys[keys.len() - 1].into(), value);
    } else {
        table.remove(keys[keys.len() - 1]);
    }
    let draft = toml::to_string_pretty(&document)?;
    parse(&draft, &snapshot.path)?;
    Ok(draft)
}
pub fn validate_binding(binding: &PortableBinding, workspace: &Workspace) -> Result<()> {
    crate::model::valid_id(&binding.project_id)?;
    crate::model::absolute_path(&binding.path)?;
    ensure!(
        binding.digest.len() == 64 && binding.digest.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid declaration digest"
    );
    let project = workspace.project(&binding.project_id)?;
    ensure!(
        binding.path == project.root.join(DECLARATION) && binding.resolved.id == project.id,
        "portable binding does not match its project"
    );
    ensure!(
        binding.task_ids.len() <= 1024 && binding.terminal_ids.len() <= 1024,
        "portable key history exceeds limit"
    );
    let mut ids = BTreeSet::new();
    for (key, id) in binding.task_ids.iter().chain(binding.terminal_ids.iter()) {
        crate::model::valid_name(key)?;
        crate::model::valid_id(id)?;
        ensure!(ids.insert(id), "duplicate portable definition ID");
    }
    ensure!(
        binding
            .terminal_reviews
            .iter()
            .all(|id| binding.terminal_ids.values().any(|v| v == id)),
        "unknown portable terminal trust scope"
    );
    binding.resolved.validate()?;
    Ok(())
}

/// Export explicit local execution definitions, never UUIDs, approvals, history or runtime state.
/// Paths beneath the project become relative; intentionally external paths remain absolute.
pub fn export_declaration(project: &Project) -> Result<Declaration> {
    project.validate()?;
    fn relative(root: &Path, value: &Path) -> PathBuf {
        match value.strip_prefix(root) {
            Ok(path) if path.as_os_str().is_empty() => PathBuf::from("."),
            Ok(path) => path.into(),
            Err(_) => value.into(),
        }
    }
    fn executable(root: &Path, value: &Path) -> Result<String> {
        let value = match value.strip_prefix(root) {
            Ok(value) => Path::new(".").join(value),
            Err(_) => value.to_path_buf(),
        };
        Ok(value
            .to_str()
            .context("portable executable path must be UTF-8; original preserved")?
            .into())
    }
    fn exported_sources(root: &Path, values: &[SourceSpec]) -> Vec<SourceSpec> {
        values
            .iter()
            .map(|source| SourceSpec {
                path: relative(root, &source.path),
                args: source.args.clone(),
            })
            .collect()
    }
    fn unique_keys<'a>(
        values: impl Iterator<Item = (&'a str, &'a str)>,
    ) -> BTreeMap<String, String> {
        let mut used = BTreeSet::new();
        let mut result = BTreeMap::new();
        for (id, name) in values {
            let mut key = name.to_owned();
            let mut suffix = 2;
            while !used.insert(key.clone()) {
                key = format!("{}-{suffix}", name.chars().take(90).collect::<String>());
                suffix += 1;
            }
            result.insert(id.into(), key);
        }
        result
    }
    let task_keys = unique_keys(
        project
            .tasks
            .iter()
            .map(|task| (task.id.as_str(), task.name.as_str())),
    );
    let terminal_keys = unique_keys(
        project
            .terminals
            .iter()
            .map(|terminal| (terminal.id.as_str(), terminal.name.as_str())),
    );
    let shell = ShellDeclaration {
        program: Some(executable(&project.root, &project.shell.executable)?),
        args: project
            .shell
            .native
            .as_ref()
            .map(|native| native.args.clone()),
        env: project
            .shell
            .native
            .as_ref()
            .map(|native| native.env.clone())
            .unwrap_or_default(),
        clear_env: project.shell.native.is_some(),
        unset_env: vec![],
        cwd: Some(relative(&project.root, &project.shell.init_cwd)),
        legacy_csh: Some(project.shell.native.is_none()),
        login: Some(project.shell.login),
        sources: Some(exported_sources(&project.root, &project.shell.sources)),
    };
    let editor = project
        .editor
        .as_ref()
        .map(|editor| -> Result<EditorDeclaration> {
            Ok(EditorDeclaration {
                program: Some(executable(&project.root, &editor.executable)?),
                args: Some(editor.args.clone()),
                external_gui: Some(editor.external_gui),
            })
        })
        .transpose()?;
    let terminals = project
        .terminals
        .iter()
        .map(|terminal| {
            (
                terminal_keys[&terminal.id].clone(),
                TerminalDeclaration {
                    name: Some(terminal.name.clone()),
                    cwd: Some(relative(&project.root, &terminal.cwd)),
                    sources: Some(exported_sources(&project.root, &terminal.sources)),
                    persistent: Some(terminal.persistent),
                },
            )
        })
        .collect();
    let tasks = project.tasks.iter().map(|task| -> Result<(String,TaskDeclaration)> {
        let (program, interpreter, script, args, env) = match &task.execution {
            Some(TaskExecution::Process {program,args,env}) => (Some(executable(&project.root,program)?),None,None,Some(args.clone()),env.clone()),
            Some(TaskExecution::Script {interpreter,script,args,env}) => (None,Some(executable(&project.root,interpreter)?),Some(script.clone()),Some(args.clone()),env.clone()),
            None => (None,None,None,None,BTreeMap::new()),
        };
        Ok((task_keys[&task.id].clone(), TaskDeclaration {
            name: Some(task.name.clone()), program, interpreter, script, args, env,
            command: task.execution.is_none().then(|| task.command.clone()),
            cwd: Some(relative(&project.root,&task.cwd)), sources: exported_sources(&project.root,&task.sources), steps: task.steps.clone(),
            interactive: task.interactive, disable_logging: task.logging == TaskLogging::Disabled, continue_on_failure: task.failure_policy == FailurePolicy::Continue,
            artifact: task.artifact.as_ref().map(|p|relative(&project.root,p)), build_outputs: task.build_outputs.iter().map(|p|relative(&project.root,p)).collect(),
            artifact_from_task: task.artifact_from_task.as_ref().map(|id|task_keys.get(id).cloned().with_context(||format!("artifact predecessor {id} is not registered; cannot export a dangling reference"))).transpose()?,
            timeout_seconds: task.timeout_seconds,
        }))
    }).collect::<Result<BTreeMap<_,_>>>()?;
    Ok(Declaration {
        schema_version: 1,
        name: Some(project.name.clone()),
        shell: Some(shell),
        editor,
        terminals,
        tasks,
    })
}
