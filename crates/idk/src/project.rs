//! Shared project-definition operations for CLI/TUI. None of these operations
//! starts, restarts, changes cwd, or terminates a live shell. Trust is optimistic:
//! direct files are re-read before launch; concurrent external edits and dynamic
//! or nested source dependencies cannot be completely tracked.
use crate::git::Repository;
use crate::model::{
    absolute_path, hash_field, new_id, valid_name, validate_sources, Project, ShellConfig,
    SourceSpec, TerminalDefinition, Workspace,
};
use crate::shell::{inspect_shell, ShellInspection, ShellPlan, STARTUP_POLICY_VERSION};
use crate::store::Store;
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

pub use crate::shell::ShellKind;
const SOURCE_LIMIT: usize = 4 * 1024 * 1024;
const SHELL_LIMIT: usize = 32 * 1024 * 1024;

#[derive(Clone)]
pub struct LaunchEnvironment {
    variables: BTreeMap<String, String>,
}
impl fmt::Debug for LaunchEnvironment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LaunchEnvironment")
            .field("values", &"[redacted]")
            .finish()
    }
}
impl LaunchEnvironment {
    pub fn capture() -> Result<Self> {
        let mut variables = BTreeMap::new();
        for (name, value) in std::env::vars_os() {
            let name = name
                .into_string()
                .map_err(|_| anyhow::anyhow!("environment key is not UTF-8"))?;
            let value = value
                .into_string()
                .map_err(|_| anyhow::anyhow!("environment value is not UTF-8"))?;
            variables.insert(name, value);
        }
        Self::from_variables(variables)
    }
    pub fn from_variables(variables: BTreeMap<String, String>) -> Result<Self> {
        ensure!(
            variables.len() <= 1024,
            "launch environment exceeds 1024 entries"
        );
        let total = variables
            .iter()
            .try_fold(0usize, |count, (key, value)| {
                count
                    .checked_add(key.len())?
                    .checked_add(value.len())?
                    .checked_add(2)
            })
            .context("launch environment size overflow")?;
        ensure!(total <= 256 * 1024, "launch environment exceeds 256 KiB");
        for (key, value) in &variables {
            ensure!(
                !key.is_empty() && !key.contains(['=', '\0']) && !value.contains('\0'),
                "invalid launch environment entry"
            );
        }
        absolute_path(Path::new(
            variables
                .get("HOME")
                .context("launch environment needs HOME")?,
        ))?;
        Ok(Self { variables })
    }
    pub fn variables(&self) -> &BTreeMap<String, String> {
        &self.variables
    }
    pub fn home(&self) -> &Path {
        Path::new(&self.variables["HOME"])
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ShellCandidate {
    pub path: PathBuf,
    pub kind: ShellKind,
}
#[derive(Clone, Debug, Serialize)]
pub struct TerminalDraft {
    pub name: String,
    pub cwd: PathBuf,
    pub sources: Vec<SourceSpec>,
    pub persistent: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct ConnectDraft {
    pub name: String,
    pub root: PathBuf,
    pub shell: ShellConfig,
    pub terminals: Vec<TerminalDraft>,
}
#[derive(Clone, Debug, Serialize)]
pub struct ProjectMatch {
    pub id: String,
    pub name: String,
    pub root: PathBuf,
}
#[derive(Clone, Debug, Serialize)]
pub struct ConnectPreview {
    pub revision: u64,
    pub project: Project,
    pub duplicates: Vec<ProjectMatch>,
    pub allow_duplicate_root: bool,
    pub repository_notice: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct WorkspaceView {
    pub revision: u64,
    pub selected_project: Option<String>,
    pub projects: Vec<ProjectView>,
}
#[derive(Clone, Debug, Serialize)]
pub struct ProjectView {
    pub project: Project,
    pub root: PathAvailability,
    pub init_cwd: PathAvailability,
    pub terminals: Vec<TerminalView>,
}
#[derive(Clone, Debug, Serialize)]
pub struct TerminalView {
    pub definition: TerminalDefinition,
    pub path: PathAvailability,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PathAvailability {
    Ready { canonical: PathBuf },
    Missing,
    Denied,
    NotDirectory,
    Unavailable { message: String },
}
#[derive(Clone, Debug, Serialize)]
pub struct PathChange {
    pub field: String,
    pub old: PathBuf,
    pub new: PathBuf,
}
#[derive(Clone, Debug, Serialize)]
pub struct RootRebindPreview {
    pub revision: u64,
    pub project_id: String,
    pub old_root: PathBuf,
    pub new_root: PathBuf,
    pub project: Project,
    pub changes: Vec<PathChange>,
    pub repository_notice: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct DefinitionImpact {
    pub project_id: String,
    pub terminal_ids: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TrustFile {
    pub path: PathBuf,
    pub canonical: Option<PathBuf>,
    pub digest: Option<String>,
    pub role: String,
    pub args: Vec<String>,
    pub missing_optional: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct TrustScopeReview {
    pub terminal_id: Option<String>,
    pub digest: Option<String>,
    pub trusted: bool,
    pub files: Vec<TrustFile>,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct InitializationReview {
    pub revision: u64,
    pub project_id: String,
    pub home: PathBuf,
    pub common: TrustScopeReview,
    pub terminals: Vec<TrustScopeReview>,
    #[serde(skip_serializing)]
    environment: LaunchEnvironment,
}
#[derive(Clone)]
pub struct TerminalLaunchPlan {
    pub shell: ShellPlan,
    pub definition_revision: u64,
    pub launch_digest: String,
}

pub struct ProjectService<'a> {
    pub store: &'a Store,
}

impl ProjectService<'_> {
    pub fn list(&self) -> Result<WorkspaceView> {
        let workspace = self.store.load()?;
        let mut ordered: Vec<_> = workspace
            .recent_projects
            .iter()
            .filter_map(|id| workspace.projects.iter().find(|p| &p.id == id))
            .collect();
        ordered.extend(
            workspace
                .projects
                .iter()
                .filter(|p| !workspace.recent_projects.contains(&p.id)),
        );
        Ok(WorkspaceView {
            revision: workspace.revision,
            selected_project: workspace.selected_project.clone(),
            projects: ordered.into_iter().map(project_view).collect(),
        })
    }
    pub fn inspect(&self, project_id: &str) -> Result<ProjectView> {
        Ok(project_view(self.store.load()?.project(project_id)?))
    }
    /// Convenience for CLI selectors only. All mutation methods accept IDs.
    pub fn resolve_project_id(&self, selector: &str) -> Result<String> {
        let workspace = self.store.load()?;
        if uuid::Uuid::parse_str(selector).is_ok() {
            return Ok(workspace.project(selector)?.id.clone());
        }
        let matches: Vec<_> = workspace
            .projects
            .iter()
            .filter(|p| p.name == selector)
            .collect();
        match matches.as_slice() {
            [project] => Ok(project.id.clone()),
            [] => bail!("project not found"),
            _ => bail!("project name is ambiguous; select its stable ID"),
        }
    }
    pub fn preview_connect(&self, draft: ConnectDraft) -> Result<ConnectPreview> {
        let workspace = self.store.load()?;
        absolute_path(&draft.root)?;
        valid_name(&draft.name)?;
        let (repository_binding, repository_notice) = discover_binding(&draft.root);
        let mut project = Project {
            id: new_id(),
            name: draft.name,
            root: draft.root,
            repository: repository_binding.as_ref().map(|r| r.root.clone()),
            repository_binding,
            related_repositories: Vec::new(),
            default_terminal: None,
            shell: draft.shell,
            terminals: Vec::new(),
            tasks: Vec::new(),
            editor: None,
        };
        project.shell.trusted_digest = None;
        for draft in draft.terminals {
            let terminal = new_terminal(&project, draft)?;
            ensure!(
                terminal.persistent,
                "transient terminals belong to the session, not connection defaults"
            );
            project.terminals.push(terminal);
        }
        project.default_terminal = project.terminals.first().map(|t| t.id.clone());
        project.validate()?;
        Ok(ConnectPreview {
            revision: workspace.revision,
            duplicates: duplicate_roots(&workspace, &project.root, None),
            project,
            allow_duplicate_root: false,
            repository_notice,
        })
    }
    pub fn create(&self, expected: u64, mut preview: ConnectPreview) -> Result<Project> {
        ensure!(expected == preview.revision, "connection preview is stale");
        ensure!(
            preview
                .project
                .terminals
                .iter()
                .all(|terminal| terminal.persistent),
            "transient terminals must stay outside saved project definitions"
        );
        preview.project.shell.trusted_digest = None;
        for terminal in &mut preview.project.terminals {
            terminal.trusted_digest = None;
        }
        self.change(expected, |workspace| {
            ensure!(
                !workspace
                    .projects
                    .iter()
                    .any(|p| p.id == preview.project.id),
                "project ID already exists"
            );
            ensure!(
                preview.allow_duplicate_root
                    || duplicate_roots(workspace, &preview.project.root, None).is_empty(),
                "project root is already connected; review keeping a separate definition"
            );
            if let Some(binding) = &preview.project.repository_binding {
                binding.verify()?;
            }
            preview.project.validate()?;
            workspace.projects.push(preview.project.clone());
            select_workspace(workspace, &preview.project.id);
            Ok(preview.project)
        })
    }
    pub fn select(&self, expected: u64, project_id: &str) -> Result<()> {
        self.change(expected, |workspace| {
            workspace.project(project_id)?;
            select_workspace(workspace, project_id);
            Ok(())
        })
    }
    pub fn rename(
        &self,
        expected: u64,
        project_id: &str,
        name: String,
    ) -> Result<DefinitionImpact> {
        valid_name(&name)?;
        self.change(expected, |workspace| {
            let p = workspace.project_mut(project_id)?;
            p.name = name;
            Ok(impact(p))
        })
    }
    pub fn remove_definition(&self, expected: u64, project_id: &str) -> Result<DefinitionImpact> {
        self.change(expected, |workspace| {
            let result = impact(workspace.project(project_id)?);
            workspace.projects.retain(|p| p.id != project_id);
            workspace.recent_projects.retain(|id| id != project_id);
            if workspace.selected_project.as_deref() == Some(project_id) {
                workspace.selected_project = workspace.recent_projects.first().cloned();
            }
            Ok(result)
        })
    }
    pub fn preview_rebind_root(
        &self,
        project_id: &str,
        root: PathBuf,
    ) -> Result<RootRebindPreview> {
        let workspace = self.store.load()?;
        rebind_preview(&workspace, project_id, root)
    }
    pub fn rebind_root(
        &self,
        expected: u64,
        preview: RootRebindPreview,
    ) -> Result<DefinitionImpact> {
        ensure!(
            expected == preview.revision,
            "root reconnection preview is stale"
        );
        self.change(expected, |workspace| {
            ensure!(
                workspace.project(&preview.project_id)?.root == preview.old_root,
                "project root changed; review again"
            );
            let fresh = rebind_preview(workspace, &preview.project_id, preview.new_root.clone())?;
            ensure!(
                fresh.project == preview.project,
                "root reconnection inputs changed; review again"
            );
            let result = impact(&fresh.project);
            *workspace.project_mut(&preview.project_id)? = fresh.project;
            Ok(result)
        })
    }
    pub fn update_shell(
        &self,
        expected: u64,
        project_id: &str,
        mut shell: ShellConfig,
    ) -> Result<DefinitionImpact> {
        self.change(expected, |workspace| {
            let project = workspace.project_mut(project_id)?;
            if shell.executable != project.shell.executable
                || shell.login != project.shell.login
                || shell.init_cwd != project.shell.init_cwd
                || shell.sources != project.shell.sources
            {
                shell.trusted_digest = None;
                for terminal in &mut project.terminals {
                    terminal.trusted_digest = None;
                }
            } else {
                shell.trusted_digest = project.shell.trusted_digest.clone();
            }
            project.shell = shell;
            project.validate()?;
            Ok(impact(project))
        })
    }
    pub fn save_terminal(
        &self,
        expected: u64,
        project_id: &str,
        mut terminal: TerminalDefinition,
    ) -> Result<DefinitionImpact> {
        // A transient stays with its caller/host. Withdrawing persistence removes
        // only the saved definition; it does not close or adopt a process.
        if !terminal.persistent {
            let workspace = self.load_expected(expected)?;
            let project = workspace.project(project_id)?;
            validate_terminal(project, &terminal)?;
            if project.terminals.iter().any(|t| t.id == terminal.id) {
                return self.remove_terminal_definition(expected, project_id, &terminal.id);
            }
            return Ok(DefinitionImpact {
                project_id: project_id.into(),
                terminal_ids: vec![terminal.id],
            });
        }
        self.change(expected, |workspace| {
            let project = workspace.project_mut(project_id)?;
            validate_terminal(project, &terminal)?;
            if let Some(previous) = project.terminals.iter_mut().find(|t| t.id == terminal.id) {
                terminal.trusted_digest =
                    if previous.cwd == terminal.cwd && previous.sources == terminal.sources {
                        previous.trusted_digest.clone()
                    } else {
                        None
                    };
                *previous = terminal.clone();
            } else {
                terminal.trusted_digest = None;
                project.terminals.push(terminal.clone());
            }
            if project.default_terminal.is_none() {
                project.default_terminal = Some(terminal.id.clone());
            }
            project.validate()?;
            Ok(DefinitionImpact {
                project_id: project_id.into(),
                terminal_ids: vec![terminal.id],
            })
        })
    }
    pub fn reorder_terminals(
        &self,
        expected: u64,
        project_id: &str,
        ids: Vec<String>,
    ) -> Result<()> {
        self.change(expected, |workspace| {
            let project = workspace.project_mut(project_id)?;
            let unique: HashSet<_> = ids.iter().collect();
            ensure!(
                ids.len() == project.terminals.len()
                    && unique.len() == ids.len()
                    && ids
                        .iter()
                        .all(|id| project.terminals.iter().any(|t| &t.id == id)),
                "terminal order must contain every terminal ID exactly once"
            );
            project.terminals = ids
                .iter()
                .map(|id| {
                    project
                        .terminals
                        .iter()
                        .find(|t| &t.id == id)
                        .unwrap()
                        .clone()
                })
                .collect();
            Ok(())
        })
    }
    pub fn set_default_terminal(
        &self,
        expected: u64,
        project_id: &str,
        terminal_id: &str,
    ) -> Result<()> {
        self.change(expected, |workspace| {
            let project = workspace.project_mut(project_id)?;
            ensure!(
                project
                    .terminals
                    .iter()
                    .any(|t| t.id == terminal_id && t.persistent),
                "default terminal must be persistent"
            );
            project.default_terminal = Some(terminal_id.into());
            Ok(())
        })
    }
    pub fn remove_terminal_definition(
        &self,
        expected: u64,
        project_id: &str,
        terminal_id: &str,
    ) -> Result<DefinitionImpact> {
        self.change(expected, |workspace| {
            let project = workspace.project_mut(project_id)?;
            ensure!(
                project.terminals.iter().any(|t| t.id == terminal_id),
                "terminal definition not found"
            );
            project.terminals.retain(|t| t.id != terminal_id);
            if project.default_terminal.as_deref() == Some(terminal_id) {
                project.default_terminal = project
                    .terminals
                    .iter()
                    .find(|t| t.persistent)
                    .map(|t| t.id.clone());
            }
            Ok(DefinitionImpact {
                project_id: project_id.into(),
                terminal_ids: vec![terminal_id.into()],
            })
        })
    }
    pub fn bind_repository(
        &self,
        expected: u64,
        project_id: &str,
        root: PathBuf,
        primary: bool,
    ) -> Result<Repository> {
        absolute_path(&root)?;
        let repository = Repository::discover(&root)?;
        self.change(expected, |workspace| {
            repository.verify()?;
            let project = workspace.project_mut(project_id)?;
            if primary {
                if let Some(previous) = project.repository_binding.take() {
                    if previous.git_dir != repository.git_dir
                        && !project
                            .related_repositories
                            .iter()
                            .any(|r| r.git_dir == previous.git_dir)
                    {
                        project.related_repositories.push(previous);
                    }
                }
                project
                    .related_repositories
                    .retain(|r| r.git_dir != repository.git_dir);
                project.repository = Some(repository.root.clone());
                project.repository_binding = Some(repository.clone());
            } else if project
                .repository_binding
                .as_ref()
                .is_none_or(|r| r.git_dir != repository.git_dir)
                && !project
                    .related_repositories
                    .iter()
                    .any(|r| r.git_dir == repository.git_dir)
            {
                project.related_repositories.push(repository.clone());
            }
            Ok(repository)
        })
    }
    pub fn clear_primary_repository(
        &self,
        expected: u64,
        project_id: &str,
    ) -> Result<DefinitionImpact> {
        self.change(expected, |workspace| {
            let p = workspace.project_mut(project_id)?;
            p.repository = None;
            p.repository_binding = None;
            Ok(impact(p))
        })
    }
    pub fn review_initialization(
        &self,
        project_id: &str,
        environment: &LaunchEnvironment,
    ) -> Result<InitializationReview> {
        let workspace = self.store.load()?;
        review_project(&workspace, project_id, environment)
    }
    pub fn approve_initialization(
        &self,
        expected: u64,
        review: InitializationReview,
    ) -> Result<()> {
        ensure!(
            expected == review.revision,
            "initialization review is stale"
        );
        self.change(expected, |workspace| {
            let fresh = review_project(workspace, &review.project_id, &review.environment)?;
            ensure!(
                fresh.common.digest == review.common.digest,
                "common initialization changed during review"
            );
            let project = workspace.project_mut(&review.project_id)?;
            let mut approved = false;
            if let Some(digest) = &review.common.digest {
                project.shell.trusted_digest = Some(digest.clone());
                approved = true;
            }
            for scope in &review.terminals {
                let id = scope
                    .terminal_id
                    .as_deref()
                    .context("terminal trust scope is missing its ID")?;
                let observed = fresh
                    .terminals
                    .iter()
                    .find(|s| s.terminal_id.as_deref() == Some(id))
                    .context("terminal changed during review")?;
                ensure!(
                    observed.digest == scope.digest,
                    "terminal initialization changed during review"
                );
                if let Some(digest) = &scope.digest {
                    project
                        .terminals
                        .iter_mut()
                        .find(|t| t.id == id)
                        .context("terminal no longer registered")?
                        .trusted_digest = Some(digest.clone());
                    approved = true;
                }
            }
            ensure!(approved, "no readable initialization scope can be approved");
            Ok(())
        })
    }
    pub fn review_terminal(
        &self,
        project_id: &str,
        terminal: &TerminalDefinition,
        environment: &LaunchEnvironment,
    ) -> Result<TrustScopeReview> {
        let workspace = self.store.load()?;
        let project = workspace.project(project_id)?;
        validate_terminal(project, terminal)?;
        let common = common_scope(project, environment);
        Ok(terminal_scope(project, terminal, environment, &common))
    }
    /// Approve a transient definition in memory only; the caller must keep it
    /// in its session namespace. No captured environment or transient is saved.
    pub fn approve_transient(
        &self,
        expected: u64,
        project_id: &str,
        terminal: &mut TerminalDefinition,
        review: &TrustScopeReview,
        environment: &LaunchEnvironment,
    ) -> Result<()> {
        let workspace = self.load_expected(expected)?;
        let project = workspace.project(project_id)?;
        ensure!(
            !terminal.persistent && !project.terminals.iter().any(|t| t.id == terminal.id),
            "transient ID must be outside saved definitions"
        );
        let common = common_scope(project, environment);
        ensure!(common.trusted, "review common initialization first");
        let fresh = terminal_scope(project, terminal, environment, &common);
        ensure!(
            fresh.terminal_id == review.terminal_id && fresh.digest == review.digest,
            "transient initialization changed during review"
        );
        terminal.trusted_digest = Some(
            fresh
                .digest
                .context("transient initialization cannot be approved")?,
        );
        Ok(())
    }
    pub fn terminal_launch_plan(
        &self,
        project_id: &str,
        terminal: &TerminalDefinition,
        environment: LaunchEnvironment,
    ) -> Result<TerminalLaunchPlan> {
        let workspace = self.store.load()?;
        let project = workspace.project(project_id)?;
        validate_terminal(project, terminal)?;
        if let Some(saved) = project.terminals.iter().find(|t| t.id == terminal.id) {
            ensure!(
                saved.cwd == terminal.cwd
                    && saved.sources == terminal.sources
                    && saved.trusted_digest == terminal.trusted_digest,
                "terminal definition changed; reload before starting"
            );
        } else {
            ensure!(
                !terminal.persistent,
                "persistent terminal is not registered"
            );
        }
        let common = common_scope(project, &environment);
        ensure!(
            common.trusted,
            "common initialization needs review: {}",
            common
                .error
                .as_deref()
                .unwrap_or("definition or startup files changed")
        );
        let terminal_review = terminal_scope(project, terminal, &environment, &common);
        ensure!(
            terminal_review.trusted,
            "terminal initialization needs review: {}",
            terminal_review
                .error
                .as_deref()
                .unwrap_or("definition or sources changed")
        );
        ensure!(
            matches!(
                path_availability(&project.shell.init_cwd),
                PathAvailability::Ready { .. }
            ),
            "initialization directory is unavailable"
        );
        ensure!(
            matches!(
                path_availability(&terminal.cwd),
                PathAvailability::Ready { .. }
            ),
            "terminal start directory is unavailable"
        );
        ensure!(
            matches!(
                path_availability(environment.home()),
                PathAvailability::Ready { .. }
            ),
            "launch HOME is unavailable"
        );
        let mut sources = project.shell.sources.clone();
        sources.extend(terminal.sources.clone());
        Ok(TerminalLaunchPlan {
            shell: ShellPlan {
                shell: project.shell.executable.clone(),
                login: project.shell.login,
                init_cwd: project.shell.init_cwd.clone(),
                start_cwd: terminal.cwd.clone(),
                sources,
                env: environment.variables,
                command: None,
            },
            definition_revision: workspace.revision,
            launch_digest: terminal_review.digest.context("missing launch digest")?,
        })
    }
    fn load_expected(&self, expected: u64) -> Result<Workspace> {
        let workspace = self.store.load()?;
        ensure!(
            workspace.revision == expected,
            "configuration changed elsewhere; reload before saving"
        );
        Ok(workspace)
    }
    fn change<T>(
        &self,
        expected: u64,
        update: impl FnOnce(&mut Workspace) -> Result<T>,
    ) -> Result<T> {
        let mut workspace = self.load_expected(expected)?;
        let output = update(&mut workspace)?;
        self.store.save(&mut workspace, expected)?;
        Ok(output)
    }
}

pub fn new_terminal(project: &Project, draft: TerminalDraft) -> Result<TerminalDefinition> {
    let terminal = TerminalDefinition {
        id: new_id(),
        name: draft.name,
        cwd: draft.cwd,
        sources: draft.sources,
        persistent: draft.persistent,
        trusted_digest: None,
    };
    validate_terminal(project, &terminal)?;
    Ok(terminal)
}

pub fn discover_shells(environment: &LaunchEnvironment) -> Vec<ShellCandidate> {
    let mut paths = Vec::new();
    if let Some(shell) = environment.variables.get("SHELL") {
        paths.push(PathBuf::from(shell));
    }
    if let Some(search) = environment.variables.get("PATH") {
        for directory in std::env::split_paths(search).filter(|p| p.is_absolute()) {
            paths.extend([directory.join("tcsh"), directory.join("csh")]);
        }
    }
    paths.extend(["/bin/tcsh", "/usr/bin/tcsh", "/bin/csh", "/usr/bin/csh"].map(PathBuf::from));
    let mut seen = HashSet::new();
    let mut candidates = Vec::new();
    for path in paths {
        if !path.is_absolute()
            || !matches!(
                path.file_name().and_then(|s| s.to_str()),
                Some("csh" | "tcsh" | "bsd-csh")
            )
        {
            continue;
        }
        let Ok(canonical) = path.canonicalize() else {
            continue;
        };
        if !seen.insert(canonical) {
            continue;
        }
        if let Ok(inspection) = inspect_shell(&path, &environment.variables) {
            candidates.push(ShellCandidate {
                path,
                kind: inspection.kind,
            });
        }
    }
    candidates
}

/// Only ~/, absolute paths, and a relative base are interpreted. Dollar signs,
/// quotes, wildcard characters, spaces and semicolons remain literal filename bytes.
pub fn resolve_input_path(
    input: &str,
    base: &Path,
    environment: &LaunchEnvironment,
) -> Result<PathBuf> {
    ensure!(!input.is_empty(), "path is required");
    let path = if input == "~" {
        environment.home().to_path_buf()
    } else if let Some(rest) = input.strip_prefix("~/") {
        environment.home().join(rest)
    } else {
        let p = PathBuf::from(input);
        if p.is_absolute() {
            p
        } else {
            base.join(p)
        }
    };
    absolute_path(&path)?;
    Ok(path)
}

pub fn path_availability(path: &Path) -> PathAvailability {
    fn failure(error: std::io::Error) -> PathAvailability {
        match error.kind() {
            std::io::ErrorKind::NotFound => PathAvailability::Missing,
            std::io::ErrorKind::PermissionDenied => PathAvailability::Denied,
            _ => PathAvailability::Unavailable {
                message: error.to_string(),
            },
        }
    }
    let metadata = match path.metadata() {
        Ok(value) => value,
        Err(error) => return failure(error),
    };
    if !metadata.is_dir() {
        return PathAvailability::NotDirectory;
    }
    let canonical = match path.canonicalize() {
        Ok(value) => value,
        Err(error) => return failure(error),
    };
    let Ok(c_path) = std::ffi::CString::new(canonical.as_os_str().as_encoded_bytes()) else {
        return PathAvailability::Unavailable {
            message: "path contains NUL".into(),
        };
    };
    if unsafe { libc::access(c_path.as_ptr(), libc::X_OK) } != 0 {
        return failure(std::io::Error::last_os_error());
    }
    PathAvailability::Ready { canonical }
}
pub fn inspect_path(path: &Path) -> PathAvailability {
    path_availability(path)
}

fn project_view(project: &Project) -> ProjectView {
    ProjectView {
        project: project.clone(),
        root: path_availability(&project.root),
        init_cwd: path_availability(&project.shell.init_cwd),
        terminals: project
            .terminals
            .iter()
            .map(|definition| TerminalView {
                definition: definition.clone(),
                path: path_availability(&definition.cwd),
            })
            .collect(),
    }
}
fn select_workspace(workspace: &mut Workspace, project_id: &str) {
    workspace.selected_project = Some(project_id.into());
    workspace.recent_projects.retain(|id| id != project_id);
    workspace.recent_projects.insert(0, project_id.into());
}
fn impact(project: &Project) -> DefinitionImpact {
    DefinitionImpact {
        project_id: project.id.clone(),
        terminal_ids: project.terminals.iter().map(|t| t.id.clone()).collect(),
    }
}
fn validate_terminal(project: &Project, terminal: &TerminalDefinition) -> Result<()> {
    let mut check = project.clone();
    check.terminals.retain(|t| t.id != terminal.id);
    check.terminals.push(terminal.clone());
    if !terminal.persistent && check.default_terminal.as_deref() == Some(&terminal.id) {
        check.default_terminal = None;
    }
    check.validate()
}
fn duplicate_roots(workspace: &Workspace, root: &Path, except: Option<&str>) -> Vec<ProjectMatch> {
    let canonical = root.canonicalize().ok();
    workspace
        .projects
        .iter()
        .filter(|p| Some(p.id.as_str()) != except)
        .filter(|p| {
            p.root == root
                || canonical
                    .as_ref()
                    .is_some_and(|path| p.root.canonicalize().ok().as_ref() == Some(path))
        })
        .map(|p| ProjectMatch {
            id: p.id.clone(),
            name: p.name.clone(),
            root: p.root.clone(),
        })
        .collect()
}
fn discover_binding(root: &Path) -> (Option<Repository>, Option<String>) {
    match Repository::discover(root) {
        Ok(repository) => (Some(repository), None),
        // Unbound is not a false claim that Git is absent or this is non-Git.
        Err(error) => (None, Some(format!("Git binding is unverified: {error:#}"))),
    }
}
fn remap_path(
    path: &mut PathBuf,
    old: &Path,
    new: &Path,
    field: String,
    changes: &mut Vec<PathChange>,
) {
    if let Ok(suffix) = path.strip_prefix(old) {
        let replacement = new.join(suffix);
        if &replacement != path {
            changes.push(PathChange {
                field,
                old: path.clone(),
                new: replacement.clone(),
            });
            *path = replacement;
        }
    }
}
fn rebind_preview(
    workspace: &Workspace,
    project_id: &str,
    root: PathBuf,
) -> Result<RootRebindPreview> {
    absolute_path(&root)?;
    let mut project = workspace.project(project_id)?.clone();
    let old_root = project.root.clone();
    let mut changes = Vec::new();
    remap_path(
        &mut project.root,
        &old_root,
        &root,
        "project root".into(),
        &mut changes,
    );
    remap_path(
        &mut project.shell.init_cwd,
        &old_root,
        &root,
        "initialization cwd".into(),
        &mut changes,
    );
    remap_path(
        &mut project.shell.executable,
        &old_root,
        &root,
        "shell executable".into(),
        &mut changes,
    );
    for (index, source) in project.shell.sources.iter_mut().enumerate() {
        remap_path(
            &mut source.path,
            &old_root,
            &root,
            format!("common source {}", index + 1),
            &mut changes,
        );
    }
    for terminal in &mut project.terminals {
        remap_path(
            &mut terminal.cwd,
            &old_root,
            &root,
            format!("{} start cwd", terminal.name),
            &mut changes,
        );
        for (index, source) in terminal.sources.iter_mut().enumerate() {
            remap_path(
                &mut source.path,
                &old_root,
                &root,
                format!("{} source {}", terminal.name, index + 1),
                &mut changes,
            );
        }
        terminal.trusted_digest = None;
    }
    // Deferred task workflows still keep their existing definitions;
    // paths moved by reconnect are displayed in the same explicit preview.
    for task in &mut project.tasks {
        remap_path(
            &mut task.cwd,
            &old_root,
            &root,
            format!("{} task cwd", task.name),
            &mut changes,
        );
        for (index, source) in task.sources.iter_mut().enumerate() {
            remap_path(
                &mut source.path,
                &old_root,
                &root,
                format!("{} task source {}", task.name, index + 1),
                &mut changes,
            );
        }
        if let Some(artifact) = &mut task.artifact {
            remap_path(
                artifact,
                &old_root,
                &root,
                format!("{} artifact", task.name),
                &mut changes,
            );
        }
        task.approved_digest = None;
    }
    let mut repository_notice = None;
    if let Some(path) = &mut project.repository {
        let previous = path.clone();
        remap_path(
            path,
            &old_root,
            &root,
            "primary repository".into(),
            &mut changes,
        );
        if previous != *path {
            let (binding, notice) = discover_binding(path);
            project.repository_binding = binding;
            repository_notice = notice;
        }
    }
    for repository in &mut project.related_repositories {
        if let Ok(suffix) = repository.root.strip_prefix(&old_root) {
            let replacement = root.join(suffix);
            if replacement != repository.root {
                changes.push(PathChange {
                    field: "related repository".into(),
                    old: repository.root.clone(),
                    new: replacement.clone(),
                });
                // Related bindings must remain verifiable; an unavailable moved
                // binding is retained as-is and reported, not invented as ready.
                match Repository::discover(&replacement) {
                    Ok(observed) => *repository = observed,
                    Err(error) => {
                        repository_notice = Some(format!(
                            "related repository needs explicit reconnection: {error:#}"
                        ))
                    }
                }
            }
        }
    }
    project.shell.trusted_digest = None;
    project.validate()?;
    Ok(RootRebindPreview {
        revision: workspace.revision,
        project_id: project_id.into(),
        old_root,
        new_root: root,
        project,
        changes,
        repository_notice,
    })
}

fn review_project(
    workspace: &Workspace,
    project_id: &str,
    environment: &LaunchEnvironment,
) -> Result<InitializationReview> {
    let project = workspace.project(project_id)?;
    let common = common_scope(project, environment);
    let terminals = project
        .terminals
        .iter()
        .map(|terminal| terminal_scope(project, terminal, environment, &common))
        .collect();
    Ok(InitializationReview {
        revision: workspace.revision,
        project_id: project_id.into(),
        home: environment.home().to_path_buf(),
        common,
        terminals,
        environment: environment.clone(),
    })
}
fn scope_result(
    terminal_id: Option<String>,
    approved: Option<&String>,
    files: Vec<TrustFile>,
    result: Result<String>,
) -> TrustScopeReview {
    match result {
        Ok(digest) => TrustScopeReview {
            terminal_id,
            trusted: approved == Some(&digest),
            digest: Some(digest),
            files,
            error: None,
        },
        Err(error) => TrustScopeReview {
            terminal_id,
            digest: None,
            trusted: false,
            files,
            error: Some(format!("{error:#}")),
        },
    }
}
fn common_scope(project: &Project, environment: &LaunchEnvironment) -> TrustScopeReview {
    let mut files = Vec::new();
    let result = (|| {
        let inspection = inspect_shell(&project.shell.executable, &environment.variables)?;
        append_file(
            &mut files,
            &project.shell.executable,
            "shell executable",
            &[],
            false,
            SHELL_LIMIT,
        )?;
        for (path, role) in startup_inventory(&project.shell, environment.home(), &inspection)? {
            append_file(&mut files, &path, &role, &[], true, SOURCE_LIMIT)?;
        }
        if inspection.kind == ShellKind::BsdCsh
            && project.shell.sources.iter().any(|s| !s.args.is_empty())
        {
            bail!("BSD csh does not support source arguments; select tcsh or remove them");
        }
        validate_sources(&project.shell.sources)?;
        for source in &project.shell.sources {
            append_file(
                &mut files,
                &source.path,
                "common source",
                &source.args,
                false,
                SOURCE_LIMIT,
            )?;
        }
        framed_digest(
            "idk-common-initialization-v2",
            &serde_json::json!({
                "policy": STARTUP_POLICY_VERSION, "project": project.id,
                "shell": project.shell.executable, "shell_kind": inspection.kind,
                "shell_canonical": inspection.canonical, "login_first": inspection.login_first,
                "login": project.shell.login, "home": environment.home(),
                "home_canonical": environment.home().canonicalize().ok(),
                "init_cwd": project.shell.init_cwd,
                "init_cwd_canonical": project.shell.init_cwd.canonicalize().ok(), "files": files,
            }),
        )
    })();
    scope_result(None, project.shell.trusted_digest.as_ref(), files, result)
}
fn terminal_scope(
    project: &Project,
    terminal: &TerminalDefinition,
    environment: &LaunchEnvironment,
    common: &TrustScopeReview,
) -> TrustScopeReview {
    let mut files = common.files.clone();
    let result = (|| {
        let common_digest = common.digest.as_ref().with_context(|| {
            format!(
                "common initialization is unavailable: {}",
                common.error.as_deref().unwrap_or("unknown")
            )
        })?;
        validate_sources(&terminal.sources)?;
        if terminal.sources.iter().any(|s| !s.args.is_empty())
            && inspect_shell(&project.shell.executable, &environment.variables)?.kind
                == ShellKind::BsdCsh
        {
            bail!("BSD csh does not support source arguments; select tcsh or remove them");
        }
        let first_extra = files.len();
        for source in &terminal.sources {
            append_file(
                &mut files,
                &source.path,
                "terminal source",
                &source.args,
                false,
                SOURCE_LIMIT,
            )?;
        }
        framed_digest(
            "idk-terminal-initialization-v2",
            &serde_json::json!({
                "common": common_digest, "terminal": terminal.id, "start_cwd": terminal.cwd,
                "start_cwd_canonical": terminal.cwd.canonicalize().ok(),
                "extra_sources": &files[first_extra..],
            }),
        )
    })();
    scope_result(
        Some(terminal.id.clone()),
        terminal.trusted_digest.as_ref(),
        files,
        result,
    )
}
fn framed_digest(label: &str, manifest: &serde_json::Value) -> Result<String> {
    let mut digest = Sha256::new();
    hash_field(&mut digest, label.as_bytes());
    hash_field(&mut digest, &serde_json::to_vec(manifest)?);
    Ok(format!("{:x}", digest.finalize()))
}
fn startup_inventory(
    shell: &ShellConfig,
    home: &Path,
    inspection: &ShellInspection,
) -> Result<Vec<(PathBuf, String)>> {
    let mut result = Vec::new();
    let mut push = |path: PathBuf, role: &str| result.push((path, role.into()));
    if shell.login && inspection.login_first {
        push("/etc/csh.login".into(), "system login");
    }
    push("/etc/csh.cshrc".into(), "system startup");
    if shell.login && !inspection.login_first {
        push("/etc/csh.login".into(), "system login");
    }
    if shell.login && inspection.login_first {
        push(home.join(".login"), "user login");
    }
    let tcshrc = home.join(".tcshrc");
    if inspection.kind == ShellKind::Tcsh {
        push(tcshrc.clone(), "user startup candidate");
        if !tcshrc
            .try_exists()
            .context("inspect user startup selection")?
        {
            push(home.join(".cshrc"), "user startup");
        }
    } else {
        push(home.join(".cshrc"), "user startup");
    }
    if shell.login && !inspection.login_first {
        push(home.join(".login"), "user login");
    }
    if shell.login {
        push("/etc/csh.logout".into(), "system logout");
        push(home.join(".logout"), "user logout");
    }
    Ok(result)
}
fn append_file(
    files: &mut Vec<TrustFile>,
    path: &Path,
    role: &str,
    args: &[String],
    optional: bool,
    limit: usize,
) -> Result<()> {
    absolute_path(path)?;
    files.push(TrustFile {
        path: path.into(),
        canonical: None,
        digest: None,
        role: role.into(),
        args: args.to_vec(),
        missing_optional: false,
    });
    let entry = files.last_mut().unwrap();
    let canonical = match path.canonicalize() {
        Ok(value) => value,
        Err(error) if optional && error.kind() == std::io::ErrorKind::NotFound => {
            entry.missing_optional = true;
            return Ok(());
        }
        Err(error) => {
            return Err(error).with_context(|| format!("cannot resolve {role}: {}", path.display()))
        }
    };
    entry.canonical = Some(canonical.clone());
    let target = canonical.metadata()?;
    ensure!(
        target.is_file(),
        "{role} must be a regular file: {}",
        path.display()
    );
    ensure!(
        target.len() <= limit as u64,
        "{role} exceeds the {} MiB limit",
        limit / (1024 * 1024)
    );
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&canonical)
        .with_context(|| format!("cannot read {role}: {}", path.display()))?;
    let before = file.metadata()?;
    ensure!(
        before.is_file(),
        "{role} must be a regular file: {}",
        path.display()
    );
    ensure!(
        before.len() <= limit as u64,
        "{role} exceeds the {} MiB limit",
        limit / (1024 * 1024)
    );
    ensure!(
        before.dev() == target.dev() && before.ino() == target.ino(),
        "{role} target changed during review"
    );
    let mut bytes = Vec::with_capacity(before.len() as usize);
    (&mut file).take(limit as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= limit, "{role} exceeds the read limit");
    let after = file.metadata()?;
    ensure!(
        before.len() == after.len()
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec()
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec(),
        "{role} changed while being read"
    );
    ensure!(
        path.canonicalize()? == canonical,
        "{role} symlink target changed during review"
    );
    entry.digest = Some(format!("{:x}", Sha256::digest(&bytes)));
    Ok(())
}
