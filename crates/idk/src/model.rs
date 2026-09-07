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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    pub schema: u32,
    pub revision: u64,
    #[serde(default)]
    pub projects: Vec<Project>,
    pub selected_project: Option<String>,
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            schema: SCHEMA,
            revision: 0,
            projects: Vec::new(),
            selected_project: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub root: PathBuf,
    pub repository: Option<PathBuf>,
    pub shell: ShellConfig,
    #[serde(default)]
    pub terminals: Vec<TerminalDefinition>,
    #[serde(default)]
    pub tasks: Vec<TaskDefinition>,
    #[serde(default)]
    pub editor: Option<EditorConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShellConfig {
    pub executable: PathBuf,
    pub login: bool,
    pub init_cwd: PathBuf,
    #[serde(default)]
    pub sources: Vec<PathBuf>,
    /// Approval binds to the chosen script bytes and definition, not arbitrary nested source files.
    pub trusted_digest: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalDefinition {
    pub id: String,
    pub name: String,
    pub cwd: PathBuf,
    #[serde(default)]
    pub sources: Vec<PathBuf>,
    #[serde(default = "default_true")]
    pub persistent: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskDefinition {
    pub id: String,
    pub name: String,
    /// An explicitly reviewed csh program. Paths supplied by the UI are quoted separately.
    pub command: String,
    pub cwd: PathBuf,
    #[serde(default)]
    pub sources: Vec<PathBuf>,
    pub artifact: Option<PathBuf>,
    pub approved_digest: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
        Ok(())
    }

    pub fn project(&self, id: &str) -> Result<&Project> {
        self.projects
            .iter()
            .find(|p| p.id == id || p.name == id)
            .context("project not found")
    }

    pub fn project_mut(&mut self, id: &str) -> Result<&mut Project> {
        self.projects
            .iter_mut()
            .find(|p| p.id == id || p.name == id)
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

    pub fn source_digest(&self) -> Result<String> {
        let mut digest = Sha256::new();
        digest.update(self.shell.executable.as_os_str().as_encoded_bytes());
        digest.update([u8::from(self.shell.login)]);
        digest.update(self.shell.init_cwd.as_os_str().as_encoded_bytes());
        hash_sources(&mut digest, &self.shell.sources)?;
        Ok(format!("{:x}", digest.finalize()))
    }

    pub fn task_digest(&self, task: &TaskDefinition) -> Result<String> {
        let mut digest = Sha256::new();
        digest.update(self.source_digest()?);
        digest.update(task.command.as_bytes());
        digest.update(task.cwd.as_os_str().as_encoded_bytes());
        hash_sources(&mut digest, &task.sources)?;
        if let Some(artifact) = &task.artifact {
            digest.update(artifact.as_os_str().as_encoded_bytes());
        }
        Ok(format!("{:x}", digest.finalize()))
    }
}

fn hash_sources(digest: &mut Sha256, sources: &[PathBuf]) -> Result<()> {
    use std::io::Read;
    for source in sources {
        digest.update([0]);
        digest.update(source.as_os_str().as_encoded_bytes());
        let file = std::fs::File::open(source)
            .with_context(|| format!("cannot read initialization script {}", source.display()))?;
        let mut bytes = Vec::new();
        file.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > 4 * 1024 * 1024 {
            bail!("initialization script exceeds 4 MiB");
        }
        digest.update([0]);
        digest.update(bytes);
    }
    Ok(())
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
    if !path.is_absolute() || path.as_os_str().as_encoded_bytes().contains(&0) {
        bail!("an absolute path is required");
    }
    Ok(())
}

fn validate_sources(paths: &[PathBuf]) -> Result<()> {
    if paths.len() > 32 {
        bail!("too many initialization scripts");
    }
    for path in paths {
        absolute_path(path)?;
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
