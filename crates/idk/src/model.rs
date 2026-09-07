use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const SCHEMA: u32 = 1;
pub const PROTOCOL: u32 = 1;
pub const MAX_PROJECTS: usize = 128;
pub const MAX_TERMINALS: usize = 64;
pub const MAX_MESSAGE: usize = 4 * 1024 * 1024;

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    pub schema: u32,
    pub revision: u64,
    #[serde(default)]
    pub projects: Vec<Project>,
    pub selected_project: Option<String>,
    #[serde(default)]
    pub recent_projects: Vec<String>,
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            schema: SCHEMA,
            revision: 0,
            projects: Vec::new(),
            selected_project: None,
            recent_projects: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub root: PathBuf,
    pub repository: Option<PathBuf>,
    #[serde(default)]
    pub repository_binding: Option<crate::git::Repository>,
    #[serde(default)]
    pub related_repositories: Vec<crate::git::Repository>,
    #[serde(default)]
    pub default_terminal: Option<String>,
    pub shell: ShellConfig,
    #[serde(default)]
    pub terminals: Vec<TerminalDefinition>,
    #[serde(default)]
    pub tasks: Vec<TaskDefinition>,
    #[serde(default)]
    pub editor: Option<EditorConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShellConfig {
    pub executable: PathBuf,
    pub login: bool,
    pub init_cwd: PathBuf,
    #[serde(default)]
    pub sources: Vec<SourceSpec>,
    /// Approval binds to the chosen script bytes and definition, not arbitrary nested source files.
    pub trusted_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalDefinition {
    pub id: String,
    pub name: String,
    pub cwd: PathBuf,
    #[serde(default)]
    pub sources: Vec<SourceSpec>,
    #[serde(default = "default_true")]
    pub persistent: bool,
    #[serde(default)]
    pub trusted_digest: Option<String>,
}

/// Source arguments are argv items, never an additional shell command string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceSpec {
    pub path: PathBuf,
    pub args: Vec<String>,
}

impl From<PathBuf> for SourceSpec {
    fn from(path: PathBuf) -> Self {
        Self {
            path,
            args: Vec::new(),
        }
    }
}

impl<'de> Deserialize<'de> for SourceSpec {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Detail {
            path: PathBuf,
            #[serde(default)]
            args: Vec<String>,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Input {
            Path(PathBuf),
            Detail(Detail),
        }
        Ok(match Input::deserialize(deserializer)? {
            Input::Path(path) => path.into(),
            Input::Detail(value) => Self {
                path: value.path,
                args: value.args,
            },
        })
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskDefinition {
    pub id: String,
    pub name: String,
    /// An explicitly reviewed csh program. Paths supplied by the UI are quoted separately.
    pub command: String,
    pub cwd: PathBuf,
    #[serde(default)]
    pub sources: Vec<SourceSpec>,
    pub artifact: Option<PathBuf>,
    pub approved_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorConfig {
    pub executable: PathBuf,
    /// argv elements, never a shell string. {file}, {line}, {column} substitutions are literal.
    pub args: Vec<String>,
    #[serde(default)]
    pub external_gui: bool,
}

impl Workspace {
    pub fn validate(&self) -> Result<()> {
        if self.schema != SCHEMA {
            bail!(
                "unsupported configuration schema {}; supported {} (file preserved)",
                self.schema,
                SCHEMA
            );
        }
        if self.projects.len() > MAX_PROJECTS {
            bail!("too many projects (limit {MAX_PROJECTS})");
        }
        let mut project_ids = std::collections::HashSet::new();
        for project in &self.projects {
            valid_id(&project.id)?;
            if !project_ids.insert(&project.id) {
                bail!("duplicate project id");
            }
            project.validate()?;
        }
        if let Some(id) = &self.selected_project {
            if !project_ids.contains(id) {
                bail!("selected project is not registered");
            }
        }
        let mut recent = std::collections::HashSet::new();
        for id in &self.recent_projects {
            if !project_ids.contains(id) || !recent.insert(id) {
                bail!("recent project IDs must be unique registered projects");
            }
        }
        Ok(())
    }

    pub fn project(&self, id: &str) -> Result<&Project> {
        self.projects
            .iter()
            .find(|p| p.id == id)
            .context("project not found")
    }

    pub fn project_mut(&mut self, id: &str) -> Result<&mut Project> {
        self.projects
            .iter_mut()
            .find(|p| p.id == id)
            .context("project not found")
    }
}

impl Project {
    pub fn validate(&self) -> Result<()> {
        valid_name(&self.name)?;
        absolute_path(&self.root)?;
        if let Some(repo) = &self.repository {
            absolute_path(repo)?;
        }
        if let Some(binding) = &self.repository_binding {
            validate_repository(binding)?;
            if self.repository.as_ref() != Some(&binding.root) {
                bail!("primary repository path and observed binding disagree");
            }
        }
        let mut repository_ids = std::collections::HashSet::new();
        if let Some(binding) = &self.repository_binding {
            repository_ids.insert(&binding.git_dir);
        }
        for binding in &self.related_repositories {
            validate_repository(binding)?;
            if !repository_ids.insert(&binding.git_dir) {
                bail!("duplicate repository binding");
            }
        }
        absolute_path(&self.shell.executable)?;
        absolute_path(&self.shell.init_cwd)?;
        validate_sources(&self.shell.sources)?;
        if self.terminals.len() > MAX_TERMINALS || self.tasks.len() > 128 {
            bail!("project definition exceeds limits");
        }
        let mut ids = std::collections::HashSet::new();
        for terminal in &self.terminals {
            valid_id(&terminal.id)?;
            valid_name(&terminal.name)?;
            absolute_path(&terminal.cwd)?;
            validate_sources(&terminal.sources)?;
            if !ids.insert(&terminal.id) {
                bail!("duplicate terminal/task id");
            }
        }
        if let Some(default) = &self.default_terminal {
            if !self
                .terminals
                .iter()
                .any(|t| &t.id == default && t.persistent)
            {
                bail!("default terminal must identify a persistent terminal");
            }
        }
        for task in &self.tasks {
            valid_id(&task.id)?;
            valid_name(&task.name)?;
            absolute_path(&task.cwd)?;
            validate_sources(&task.sources)?;
            if task.command.trim().is_empty()
                || task.command.len() > 65536
                || task.command.contains('\0')
            {
                bail!("invalid task command");
            }
            if let Some(path) = &task.artifact {
                absolute_path(path)?;
            }
            if !ids.insert(&task.id) {
                bail!("duplicate terminal/task id");
            }
        }
        if let Some(editor) = &self.editor {
            absolute_path(&editor.executable)?;
            if editor.args.len() > 64
                || editor
                    .args
                    .iter()
                    .any(|arg| arg.contains('\0') || arg.len() > 8192)
            {
                bail!("invalid editor argv");
            }
            if !editor.args.iter().any(|arg| arg.contains("{file}")) {
                bail!("editor argv must contain {{file}}");
            }
        }
        Ok(())
    }

    /// Definition/direct-source fingerprint. Full execution trust additionally
    /// binds HOME and startup inventory in ProjectService; this alone is not approval.
    pub fn source_digest(&self) -> Result<String> {
        let mut digest = Sha256::new();
        hash_field(&mut digest, b"idk-source-definition-v2");
        hash_field(
            &mut digest,
            self.shell.executable.as_os_str().as_encoded_bytes(),
        );
        hash_field(&mut digest, &[u8::from(self.shell.login)]);
        hash_field(
            &mut digest,
            self.shell.init_cwd.as_os_str().as_encoded_bytes(),
        );
        hash_sources(&mut digest, &self.shell.sources)?;
        Ok(format!("{:x}", digest.finalize()))
    }

    pub fn task_digest(&self, task: &TaskDefinition) -> Result<String> {
        let mut digest = Sha256::new();
        hash_field(&mut digest, b"idk-task-definition-v2");
        hash_field(&mut digest, self.source_digest()?.as_bytes());
        hash_field(&mut digest, task.command.as_bytes());
        hash_field(&mut digest, task.cwd.as_os_str().as_encoded_bytes());
        hash_sources(&mut digest, &task.sources)?;
        if let Some(artifact) = &task.artifact {
            hash_field(&mut digest, artifact.as_os_str().as_encoded_bytes());
        }
        Ok(format!("{:x}", digest.finalize()))
    }
}

pub(crate) fn hash_field(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
}

fn hash_sources(digest: &mut Sha256, sources: &[SourceSpec]) -> Result<()> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    hash_field(digest, &(sources.len() as u64).to_be_bytes());
    for source in sources {
        hash_field(digest, source.path.as_os_str().as_encoded_bytes());
        hash_field(digest, &serde_json::to_vec(&source.args)?);
        let canonical = source.path.canonicalize().with_context(|| {
            format!(
                "cannot resolve initialization script {}",
                source.path.display()
            )
        })?;
        let target = canonical.metadata()?;
        if !target.is_file() {
            bail!("initialization source must be a regular file");
        }
        if target.len() > 4 * 1024 * 1024 {
            bail!("initialization script exceeds 4 MiB");
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&canonical)
            .with_context(|| {
                format!(
                    "cannot read initialization script {}",
                    source.path.display()
                )
            })?;
        if !file.metadata()?.is_file() {
            bail!("initialization source must be a regular file");
        }
        let mut bytes = Vec::new();
        file.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > 4 * 1024 * 1024 {
            bail!("initialization script exceeds 4 MiB");
        }
        hash_field(digest, &bytes);
    }
    Ok(())
}

fn validate_repository(repo: &crate::git::Repository) -> Result<()> {
    absolute_path(&repo.root)?;
    absolute_path(&repo.git_dir)?;
    absolute_path(&repo.common_dir)
}

pub fn valid_id(value: &str) -> Result<()> {
    if uuid::Uuid::parse_str(value).is_err() {
        bail!("invalid object id");
    }
    Ok(())
}

pub fn valid_name(value: &str) -> Result<()> {
    if value.trim().is_empty() || value.chars().count() > 100 || value.chars().any(char::is_control)
    {
        bail!("name must be 1–100 visible characters");
    }
    Ok(())
}

pub fn absolute_path(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path
            .to_str()
            .is_none_or(|p| p.chars().any(char::is_control))
    {
        bail!("an absolute UTF-8 path without control characters is required");
    }
    Ok(())
}

pub fn validate_sources(paths: &[SourceSpec]) -> Result<()> {
    if paths.len() > 32 {
        bail!("too many initialization scripts");
    }
    for source in paths {
        absolute_path(&source.path)?;
        if source.args.len() > 64
            || source
                .args
                .iter()
                .any(|arg| arg.len() > 8192 || arg.chars().any(char::is_control))
        {
            bail!("source arguments must be at most 64 literal values without control characters");
        }
    }
    Ok(())
}

/// Host-owned source-use contract shared by Git mutations and run startup.
/// Reserving before spawn closes the check/start race. External Git remains outside this lock.
#[derive(Clone, Default)]
pub struct SourceGate(Arc<Mutex<HashMap<PathBuf, GateState>>>);

#[derive(Debug, Default, Serialize, Clone)]
pub struct GateState {
    pub provider_ready: bool,
    pub runs: BTreeMap<String, String>,
    pub mutation: Option<String>,
    pub generation: u64,
}

pub struct GateLease {
    gate: SourceGate,
    identity: PathBuf,
    operation: String,
    mutation: bool,
}

impl SourceGate {
    pub fn ready(&self, identity: &Path) -> Result<()> {
        let mut states = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("source-use provider unavailable"))?;
        states
            .entry(identity.to_path_buf())
            .or_default()
            .provider_ready = true;
        Ok(())
    }

    pub fn state(&self, identity: &Path) -> Result<GateState> {
        let states = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("source-use provider unavailable"))?;
        Ok(states.get(identity).cloned().unwrap_or_default())
    }

    pub fn reserve_run(&self, identity: &Path, operation: &str, name: &str) -> Result<GateLease> {
        self.reserve(identity, operation, Some(name))
    }

    pub fn reserve_mutation(&self, identity: &Path, operation: &str) -> Result<GateLease> {
        self.reserve(identity, operation, None)
    }

    fn reserve(&self, identity: &Path, operation: &str, run: Option<&str>) -> Result<GateLease> {
        let mut states = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("source-use provider unavailable"))?;
        let state = states.entry(identity.to_path_buf()).or_default();
        if !state.provider_ready {
            bail!("source-use state unknown; provider is not ready");
        }
        if state.mutation.is_some() || (run.is_none() && !state.runs.is_empty()) {
            bail!("worktree is in use; stop/wait for owned work before changing sources");
        }
        if state.runs.contains_key(operation) {
            bail!("duplicate operation id");
        }
        if let Some(name) = run {
            state.runs.insert(operation.to_owned(), name.to_owned());
        } else {
            state.mutation = Some(operation.to_owned());
        }
        Ok(GateLease {
            gate: self.clone(),
            identity: identity.to_path_buf(),
            operation: operation.to_owned(),
            mutation: run.is_none(),
        })
    }
}

impl Drop for GateLease {
    fn drop(&mut self) {
        if let Ok(mut states) = self.gate.0.lock() {
            if let Some(state) = states.get_mut(&self.identity) {
                if self.mutation && state.mutation.as_deref() == Some(&self.operation) {
                    state.mutation = None;
                    state.generation += 1;
                } else if !self.mutation {
                    state.runs.remove(&self.operation);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_is_not_idle_and_reservations_exclude_source_changes() {
        let gate = SourceGate::default();
        let repo = Path::new("/repo/worktree");
        assert!(gate.reserve_mutation(repo, "branch").is_err());
        gate.ready(repo).unwrap();
        let run = gate.reserve_run(repo, "run1", "build").unwrap();
        assert!(gate.reserve_mutation(repo, "branch").is_err());
        assert!(gate.reserve_run(repo, "run1", "duplicate").is_err());
        drop(run);
        let change = gate.reserve_mutation(repo, "branch").unwrap();
        assert!(gate.reserve_run(repo, "run2", "build").is_err());
        drop(change);
        assert_eq!(gate.state(repo).unwrap().generation, 1);
        assert!(gate.reserve_run(repo, "run2", "build").is_ok());
    }

    #[test]
    fn competing_run_and_mutation_cannot_both_hold_a_lease() {
        let gate = SourceGate::default();
        let repo = PathBuf::from("/repo/worktree");
        gate.ready(&repo).unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let finish = Arc::new(std::sync::Barrier::new(3));
        let handles: Vec<_> = (0..2)
            .map(|index| {
                let (gate, repo, barrier, finish) =
                    (gate.clone(), repo.clone(), barrier.clone(), finish.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    let lease = if index == 0 {
                        gate.reserve_run(&repo, "run", "build")
                    } else {
                        gate.reserve_mutation(&repo, "pull")
                    };
                    finish.wait();
                    lease.is_ok()
                })
            })
            .collect();
        barrier.wait();
        finish.wait();
        assert_eq!(
            handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .filter(|ok| *ok)
                .count(),
            1
        );
    }
}
