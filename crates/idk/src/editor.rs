//! Explicit external editor configuration and reviewed source locations. A
//! filename from a log is never a shell program or an approved root by itself.
use crate::model::{EditorConfig, Project};
use crate::problems::Problem;
use crate::project::LaunchEnvironment;
use crate::run_wire::RunInfo;
use crate::store::Store;
use anyhow::{ensure, Context, Result};
use portable_pty::CommandBuilder;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditorReview {
    pub run_id: String,
    pub problem_id: String,
    pub project_id: String,
    pub file: PathBuf,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub external_gui: bool,
    pub configuration_changed_since_run: bool,
    pub source_changed_during_run: Option<bool>,
    pub source_identity_is_snapshot: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
    length: u64,
    mtime: i64,
    mtime_ns: i64,
    ctime: i64,
    ctime_ns: i64,
}
impl Identity {
    fn of(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            length: metadata.len(),
            mtime: metadata.mtime(),
            mtime_ns: metadata.mtime_nsec(),
            ctime: metadata.ctime(),
            ctime_ns: metadata.ctime_nsec(),
        }
    }
}
#[derive(Clone)]
pub struct EditorPlan {
    review: EditorReview,
    config: EditorConfig,
    source: Identity,
    executable: Identity,
    cwd: PathBuf,
    recorded_roots: Vec<PathBuf>,
}
impl EditorPlan {
    pub fn review(&self) -> &EditorReview {
        &self.review
    }
}
pub struct EditorService<'a> {
    pub store: &'a Store,
}
impl EditorService<'_> {
    pub fn save(
        &self,
        expected_revision: u64,
        project_id: &str,
        config: EditorConfig,
    ) -> Result<()> {
        validate_config(&config)?;
        executable(&config.executable)?;
        let mut workspace = self.store.load()?;
        ensure!(
            workspace.revision == expected_revision,
            "configuration changed; reload editor settings"
        );
        workspace.project_mut(project_id)?.editor = Some(config);
        self.store.save(&mut workspace, expected_revision)
    }
    pub fn review(&self, run: &RunInfo, problem: &Problem) -> Result<EditorPlan> {
        ensure!(
            problem.run_id == run.run_id
                && problem.project_id == run.project_id
                && problem.log_generation == run.log.generation,
            "diagnostic belongs to another run or log generation"
        );
        let workspace = self.store.load()?;
        let project = workspace.project(&run.project_id)?;
        let config = project.editor.clone().context(
            "configure an external editor for this project before opening a source location",
        )?;
        validate_config(&config)?;
        let reported = problem
            .file
            .as_ref()
            .context("this tool diagnostic has no file location; inspect its original log")?;
        let file = resolve_location(
            &run.cwd,
            reported,
            &run.source_roots,
            &current_roots(project)?,
        )?;
        let source = regular_source(&file)?;
        let (executable_path, executable_identity) = executable(&config.executable)?;
        let line = problem.range.as_ref().map(|position| position.line);
        let column = problem.range.as_ref().and_then(|position| position.column);
        let arguments = arguments(&config, &file, line, column)?;
        let cwd = project
            .root
            .canonicalize()
            .context("current project root unavailable")?;
        Ok(EditorPlan {
            review: EditorReview {
                run_id: run.run_id.clone(),
                problem_id: problem.id.clone(),
                project_id: run.project_id.clone(),
                file,
                line,
                column,
                executable: executable_path,
                arguments,
                external_gui: config.external_gui,
                configuration_changed_since_run: workspace.revision != run.definition_revision,
                source_changed_during_run: run.source_changed,
                source_identity_is_snapshot: false,
            },
            config,
            source,
            executable: executable_identity,
            cwd,
            recorded_roots: run.source_roots.clone(),
        })
    }
    /// The caller launches this reviewed command in a dedicated owned terminal
    /// (or an explicitly configured external GUI process), never a busy shell.
    pub fn command(
        &self,
        plan: &EditorPlan,
        environment: BTreeMap<String, String>,
    ) -> Result<CommandBuilder> {
        let workspace = self.store.load()?;
        let project = workspace.project(&plan.review.project_id)?;
        ensure!(
            project.editor.as_ref() == Some(&plan.config),
            "editor configuration changed after review"
        );
        let file = resolve_location(
            &plan.cwd,
            &plan.review.file,
            &plan.recorded_roots,
            &current_roots(project)?,
        )?;
        ensure!(
            file == plan.review.file && regular_source(&file)? == plan.source,
            "source file changed or was replaced after location review"
        );
        ensure!(
            project.root.canonicalize()? == plan.cwd,
            "project root moved after editor review"
        );
        let (path, identity) = executable(&plan.config.executable)?;
        ensure!(
            path == plan.review.executable && identity == plan.executable,
            "editor executable changed after review"
        );
        let environment = LaunchEnvironment::from_variables(environment)?;
        let mut command = CommandBuilder::new(path);
        command.args(&plan.review.arguments);
        command.cwd(&plan.cwd);
        command.env_clear();
        for (name, value) in environment.variables() {
            command.env(name, value);
        }
        Ok(command)
    }
}

pub fn validate_config(config: &EditorConfig) -> Result<()> {
    ensure!(
        config.executable.is_absolute() && config.args.len() <= 64,
        "editor needs an absolute executable and at most64 arguments"
    );
    let delimiter = config
        .args
        .iter()
        .position(|argument| argument == "--")
        .context("editor arguments need -- before the file placeholder")?;
    let mut files = 0;
    for (index, argument) in config.args.iter().enumerate() {
        ensure!(
            argument.len() <= 8192 && !argument.chars().any(char::is_control),
            "editor argument contains a control character or exceeds its bound"
        );
        if argument.contains("{file}") {
            ensure!(
                argument == "{file}" && index > delimiter,
                "{{file}} must be one literal argument after --; command strings are not supported"
            );
            files += 1;
        }
        let without = argument
            .replace("{file}", "")
            .replace("{line}", "")
            .replace("{column}", "");
        ensure!(!without.contains(['{', '}']), "unknown editor placeholder");
    }
    ensure!(files == 1, "editor requires exactly one file placeholder");
    Ok(())
}
fn arguments(
    config: &EditorConfig,
    file: &Path,
    line: Option<u32>,
    column: Option<u32>,
) -> Result<Vec<String>> {
    ensure!(
        line.is_none_or(|value| value > 0) && column.is_none_or(|value| value > 0),
        "invalid diagnostic position"
    );
    let file = file.to_str().context("editor location is not UTF-8")?;
    let mut arguments = Vec::with_capacity(config.args.len());
    for argument in &config.args {
        if argument.contains("{line}") {
            ensure!(
                line.is_some(),
                "configured editor needs a line number that this diagnostic does not provide"
            );
        }
        if argument.contains("{column}") {
            ensure!(
                column.is_some(),
                "configured editor needs a column number that this diagnostic does not provide"
            );
        }
        if argument == "{file}" {
            arguments.push(file.to_owned());
        } else {
            arguments.push(
                argument
                    .replace("{line}", &line.unwrap_or(1).to_string())
                    .replace("{column}", &column.unwrap_or(1).to_string()),
            );
        }
    }
    Ok(arguments)
}
fn current_roots(project: &Project) -> Result<Vec<PathBuf>> {
    // Missing unrelated paths do not prevent using another registered source.
    let mut roots = Vec::new();
    for root in std::iter::once(&project.root)
        .chain(project.tasks.iter().map(|task| &task.cwd))
        .chain(project.terminals.iter().map(|terminal| &terminal.cwd))
    {
        if let Ok(path) = root.canonicalize() {
            if path.is_dir() {
                roots.push(path);
            }
        }
    }
    ensure!(
        !roots.is_empty(),
        "registered source locations are unavailable"
    );
    roots.sort();
    roots.dedup();
    Ok(roots)
}
pub fn resolve_location(
    cwd: &Path,
    reported: &Path,
    recorded_roots: &[PathBuf],
    current_roots: &[PathBuf],
) -> Result<PathBuf> {
    let text = reported
        .to_str()
        .context("diagnostic filename is not UTF-8; inspect the original log")?;
    ensure!(
        !text.is_empty()
            && text.len() <= 4096
            && !text.chars().any(char::is_control)
            && !text.chars().any(
                |character| matches!(character,'\u{202a}'..='\u{202e}'|'\u{2066}'..='\u{2069}')
            ),
        "diagnostic filename contains unsupported controls or is too long"
    );
    ensure!(cwd.is_absolute(), "run working directory is not absolute");
    let candidate = if reported.is_absolute() {
        reported.to_owned()
    } else {
        cwd.join(reported)
    };
    let resolved = candidate
        .canonicalize()
        .context("diagnostic file is missing or unavailable; no file was created")?;
    let recorded = recorded_roots.iter().any(|root| {
        root.is_absolute()
            && root.canonicalize().is_ok_and(|current| current == *root)
            && resolved.starts_with(root)
    });
    let current = current_roots.iter().any(|root| {
        root.is_absolute()
            && root
                .canonicalize()
                .is_ok_and(|root| resolved.starts_with(root))
    });
    ensure!(
        recorded && current,
        "diagnostic path is outside a recorded and currently registered source/test root"
    );
    regular_source(&resolved)?;
    Ok(resolved)
}
fn regular_source(path: &Path) -> Result<Identity> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "editor source is not a regular file");
    Ok(Identity::of(&metadata))
}
fn executable(path: &Path) -> Result<(PathBuf, Identity)> {
    let resolved = path
        .canonicalize()
        .context("configured editor executable is unavailable")?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&resolved)?;
    let metadata = file.metadata()?;
    let uid = unsafe { libc::geteuid() };
    ensure!(metadata.is_file()&&(metadata.uid()==0||metadata.uid()==uid)&&metadata.mode()&0o111!=0&&metadata.mode()&0o022==0,
        "editor must be a regular executable owned by this user/root and not writable by other users");
    for parent in resolved
        .parent()
        .context("editor executable has no parent")?
        .ancestors()
    {
        let metadata = fs::metadata(parent)?;
        let sticky_root = metadata.uid() == 0 && metadata.mode() & 0o1000 != 0;
        ensure!(
            (metadata.uid() == 0 || metadata.uid() == uid)
                && (metadata.mode() & 0o022 == 0 || sticky_root),
            "editor executable has an untrusted writable parent"
        );
    }
    Ok((resolved, Identity::of(&metadata)))
}
