//! Reviewed registered csh programs. Definition edits never execute anything.
use crate::model::{hash_field, Project, TaskDefinition, TaskStep};
use crate::project::{common_scope, LaunchEnvironment};
use crate::shell::ShellPlan;
use crate::store::Store;
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskReview {
    pub revision: u64,
    pub project_id: String,
    pub task: TaskDefinition,
    pub digest: String,
    pub approved: bool,
    pub common_approved: bool,
    pub home: PathBuf,
}
#[derive(Clone)]
pub struct TaskLaunchPlan {
    pub project_id: String,
    pub task: TaskDefinition,
    pub shell: ShellPlan,
    pub definition_revision: u64,
    pub launch_digest: String,
    pub initialization_digest: String,
    pub repository: Option<crate::git::Repository>,
    pub source_roots: Vec<PathBuf>,
}
pub struct TaskService<'a> {
    pub store: &'a Store,
}
impl TaskService<'_> {
    pub fn save(
        &self,
        expected: u64,
        project_id: &str,
        mut task: TaskDefinition,
    ) -> Result<TaskDefinition> {
        let mut workspace = self.store.load()?;
        ensure!(
            workspace.revision == expected,
            "configuration changed; reload task definition"
        );
        let project = workspace.project_mut(project_id)?;
        task.approved_digest = None;
        if let Some(saved) = project.tasks.iter_mut().find(|saved| saved.id == task.id) {
            *saved = task.clone();
        } else {
            ensure!(project.tasks.len() < 128, "task limit reached");
            project.tasks.push(task.clone());
        }
        workspace.validate()?;
        self.store.save(&mut workspace, expected)?;
        Ok(task)
    }
    pub fn review(
        &self,
        project_id: &str,
        task_id: &str,
        environment: &LaunchEnvironment,
    ) -> Result<TaskReview> {
        let workspace = self.store.load()?;
        let project = workspace.project(project_id)?;
        let task = project
            .tasks
            .iter()
            .find(|task| task.id == task_id)
            .context("task not registered")?;
        let common = common_scope(project, environment);
        let common_digest = common
            .digest
            .context("common initialization unavailable for task review")?;
        let digest = digest(project, task, &common_digest)?;
        Ok(TaskReview {
            revision: workspace.revision,
            project_id: project_id.into(),
            task: task.clone(),
            approved: task.approved_digest.as_ref() == Some(&digest),
            digest,
            common_approved: common.trusted,
            home: environment.home().into(),
        })
    }
    pub fn approve(
        &self,
        expected: u64,
        project_id: &str,
        task_id: &str,
        reviewed_digest: &str,
        environment: &LaunchEnvironment,
    ) -> Result<()> {
        let review = self.review(project_id, task_id, environment)?;
        ensure!(
            review.revision == expected && review.digest == reviewed_digest,
            "task or initialization changed since review"
        );
        ensure!(
            review.common_approved,
            "approve common initialization first"
        );
        let mut workspace = self.store.load()?;
        ensure!(
            workspace.revision == expected,
            "configuration changed since review"
        );
        workspace
            .project_mut(project_id)?
            .tasks
            .iter_mut()
            .find(|task| task.id == task_id)
            .context("task not registered")?
            .approved_digest = Some(review.digest);
        self.store.save(&mut workspace, expected)
    }
    pub fn launch_plan(
        &self,
        project_id: &str,
        task_id: &str,
        environment: LaunchEnvironment,
    ) -> Result<TaskLaunchPlan> {
        let workspace = self.store.load()?;
        let project = workspace.project(project_id)?;
        project.validate()?;
        let task = project
            .tasks
            .iter()
            .find(|task| task.id == task_id)
            .context("task not registered")?
            .clone();
        let common = common_scope(project, &environment);
        ensure!(common.trusted, "common initialization needs review");
        let initialization_digest = common.digest.context("common initialization unavailable")?;
        let launch_digest = digest(project, &task, &initialization_digest)?;
        ensure!(
            task.approved_digest.as_ref() == Some(&launch_digest),
            "task definition or initialization needs review"
        );
        ensure!(task.cwd.is_dir(), "task working directory unavailable");
        let mut sources = project.shell.sources.clone();
        sources.extend(task.sources.clone());
        let shell = ShellPlan {
            shell: project.shell.executable.clone(),
            login: project.shell.login,
            init_cwd: project.shell.init_cwd.clone(),
            start_cwd: task.cwd.clone(),
            sources,
            env: environment.variables().clone(),
            command: None,
        };
        let mut source_roots = vec![project.root.canonicalize()?, task.cwd.canonicalize()?];
        source_roots.sort();
        source_roots.dedup();
        Ok(TaskLaunchPlan {
            project_id: project_id.into(),
            task,
            shell,
            definition_revision: workspace.revision,
            launch_digest,
            initialization_digest,
            repository: project.repository_binding.clone(),
            source_roots,
        })
    }
}
fn digest(project: &Project, task: &TaskDefinition, common: &str) -> Result<String> {
    let mut hash = Sha256::new();
    hash_field(&mut hash, b"idk-approved-task-v1");
    hash_field(&mut hash, common.as_bytes());
    hash_field(
        &mut hash,
        project.root.canonicalize()?.as_os_str().as_encoded_bytes(),
    );
    hash_field(&mut hash, &serde_json::to_vec(&project.repository_binding)?);
    hash_field(&mut hash, project.task_digest(task)?.as_bytes());
    hash_field(
        &mut hash,
        task.cwd.canonicalize()?.as_os_str().as_encoded_bytes(),
    );
    Ok(format!("{:x}", hash.finalize()))
}
pub fn steps(task: &TaskDefinition) -> Vec<TaskStep> {
    if task.steps.is_empty() {
        vec![TaskStep {
            name: task.name.clone(),
            command: task.command.clone(),
        }]
    } else {
        task.steps.clone()
    }
}
