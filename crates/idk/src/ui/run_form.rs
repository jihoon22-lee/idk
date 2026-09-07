//! Local task/editor drafts; saving a definition never launches it.
use super::git_draft::CommitDraft;
use crate::model::{
    EditorConfig, FailurePolicy, SourceSpec, TaskDefinition, TaskLogging, TaskStep,
};
use anyhow::{ensure, Result};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub(super) struct DraftField {
    pub label: String,
    pub value: CommitDraft,
}
impl DraftField {
    fn new(label: impl Into<String>, text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            label: label.into(),
            value: CommitDraft {
                cursor: text.len(),
                text,
            },
        }
    }
}
#[derive(Clone, Debug)]
pub(super) struct RunForm {
    pub project: String,
    pub revision: u64,
    pub fields: Vec<DraftField>,
    pub selected: usize,
    pub task: Option<TaskDefinition>,
    editor: Option<EditorConfig>,
    pub source_count: usize,
    pub step_count: usize,
}
impl RunForm {
    pub fn task(project: String, revision: u64, task: TaskDefinition) -> Self {
        let fields = vec![
            DraftField::new("Name", task.name.clone()),
            DraftField::new(
                "csh command (multiline; used when no steps)",
                task.command.clone(),
            ),
            DraftField::new("Working directory", task.cwd.to_string_lossy()),
            DraftField::new(
                "Failure policy: stop / continue",
                if task.failure_policy == FailurePolicy::Stop {
                    "stop"
                } else {
                    "continue"
                },
            ),
            DraftField::new(
                "Logging: raw / disabled (interactive requires disabled)",
                if task.logging == TaskLogging::Raw {
                    "raw"
                } else {
                    "disabled"
                },
            ),
            DraftField::new(
                "Interactive: yes / no",
                if task.interactive { "yes" } else { "no" },
            ),
            DraftField::new(
                "Timeout seconds (blank: none)",
                task.timeout_seconds
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
            ),
            DraftField::new(
                "Build output directories (one literal path per line)",
                task.build_outputs
                    .iter()
                    .map(|p| p.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            DraftField::new(
                "Artifact path (optional)",
                task.artifact
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ),
            DraftField::new(
                "Artifact from task ID (relationship only)",
                task.artifact_from_task.clone().unwrap_or_default(),
            ),
        ];
        let mut form = Self {
            project,
            revision,
            fields,
            selected: 0,
            task: Some(task.clone()),
            editor: None,
            source_count: 0,
            step_count: 0,
        };
        for source in task.sources {
            form.add_source(source);
        }
        for step in task.steps {
            form.add_step(step);
        }
        form.selected = 0;
        form
    }
    pub fn editor(project: String, revision: u64, config: Option<EditorConfig>) -> Self {
        let config = config.unwrap_or(EditorConfig {
            executable: PathBuf::new(),
            args: vec!["+{line}".into(), "--".into(), "{file}".into()],
            external_gui: false,
        });
        Self {
            project,
            revision,
            selected: 0,
            task: None,
            editor: Some(config.clone()),
            source_count: 0,
            step_count: 0,
            fields: vec![
                DraftField::new(
                    "Editor executable (absolute path)",
                    config.executable.to_string_lossy(),
                ),
                DraftField::new(
                    "Arguments (one literal argument per line; -- before {file})",
                    config.args.join("\n"),
                ),
                DraftField::new(
                    "External GUI permission: yes / no",
                    if config.external_gui { "yes" } else { "no" },
                ),
            ],
        }
    }
    pub fn add_source(&mut self, source: SourceSpec) {
        let at = 10 + self.source_count * 2;
        self.source_count += 1;
        self.fields.splice(
            at..at,
            [
                DraftField::new(
                    format!("Source {} path (blank removes)", self.source_count),
                    source.path.to_string_lossy(),
                ),
                DraftField::new(
                    "Source arguments (one per line; Enter on blank passes an empty argument)",
                    source.args.join("\n"),
                ),
            ],
        );
        self.selected = at;
    }
    pub fn add_step(&mut self, step: TaskStep) {
        self.step_count += 1;
        self.selected = self.fields.len();
        self.fields.extend([
            DraftField::new(
                format!("Step {} name (blank removes)", self.step_count),
                step.name,
            ),
            DraftField::new("Step csh command (multiline)", step.command),
        ]);
    }
    fn value(&self, index: usize) -> &str {
        &self.fields[index].value.text
    }
    pub fn build_task(&self) -> Result<TaskDefinition> {
        let mut task = self.task.clone().expect("task form");
        task.name = self.value(0).into();
        task.command = self.value(1).into();
        task.cwd = path_value(self.value(2), &task.cwd);
        task.failure_policy = match self.value(3) {
            "stop" => FailurePolicy::Stop,
            "continue" => FailurePolicy::Continue,
            _ => anyhow::bail!("Failure policy must be stop or continue"),
        };
        task.logging = match self.value(4) {
            "raw" => TaskLogging::Raw,
            "disabled" => TaskLogging::Disabled,
            _ => anyhow::bail!("Logging must be raw or disabled"),
        };
        task.interactive = boolean(self.value(5))?;
        task.timeout_seconds = if self.value(6).is_empty() {
            None
        } else {
            Some(self.value(6).parse()?)
        };
        if self.value(7)
            != task
                .build_outputs
                .iter()
                .map(|p| p.to_string_lossy())
                .collect::<Vec<_>>()
                .join("\n")
        {
            task.build_outputs = self
                .value(7)
                .lines()
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .collect();
        }
        task.artifact = if self.value(8).is_empty() {
            None
        } else {
            Some(
                task.artifact
                    .as_ref()
                    .map(|path| path_value(self.value(8), path))
                    .unwrap_or_else(|| self.value(8).into()),
            )
        };
        task.artifact_from_task = (!self.value(9).is_empty()).then(|| self.value(9).into());
        task.sources = (0..self.source_count)
            .filter_map(|i| {
                let at = 10 + i * 2;
                (!self.value(at).is_empty()).then(|| SourceSpec {
                    path: task
                        .sources
                        .get(i)
                        .map(|source| path_value(self.value(at), &source.path))
                        .unwrap_or_else(|| self.value(at).into()),
                    args: task
                        .sources
                        .get(i)
                        .filter(|source| source.args.join("\n") == self.value(at + 1))
                        .map(|source| source.args.clone())
                        .unwrap_or_else(|| arguments(self.value(at + 1))),
                })
            })
            .collect();
        task.steps = (0..self.step_count)
            .filter_map(|i| {
                let at = 10 + self.source_count * 2 + i * 2;
                (!self.value(at).is_empty()).then(|| TaskStep {
                    name: self.value(at).into(),
                    command: self.value(at + 1).into(),
                })
            })
            .collect();
        task.approved_digest = None;
        ensure!(!task.name.is_empty(), "Task name is required");
        Ok(task)
    }
    pub fn build_editor(&self) -> Result<EditorConfig> {
        Ok(EditorConfig {
            executable: self
                .editor
                .as_ref()
                .map(|config| path_value(self.value(0), &config.executable))
                .unwrap_or_else(|| self.value(0).into()),
            args: self
                .editor
                .as_ref()
                .filter(|config| config.args.join("\n") == self.value(1))
                .map(|config| config.args.clone())
                .unwrap_or_else(|| arguments(self.value(1))),
            external_gui: boolean(self.value(2))?,
        })
    }
}
fn boolean(value: &str) -> Result<bool> {
    match value {
        "yes" => Ok(true),
        "no" => Ok(false),
        _ => anyhow::bail!("Enter yes or no"),
    }
}
fn arguments(value: &str) -> Vec<String> {
    if value.is_empty() {
        vec![]
    } else {
        value.split_terminator('\n').map(str::to_owned).collect()
    }
}

fn path_value(value: &str, path: &Path) -> PathBuf {
    if value == path.to_string_lossy() {
        path.to_path_buf()
    } else {
        value.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;
    #[test]
    fn unchanged_path_and_literal_argument_boundaries_survive_unrelated_edit() {
        let path = PathBuf::from(std::ffi::OsString::from_vec(b"/tmp/nonutf8-\xff".to_vec()));
        let mut task = TaskDefinition {
            id: "task".into(),
            name: "Name".into(),
            command: "echo okay".into(),
            cwd: path.clone(),
            sources: vec![SourceSpec {
                path: path.clone(),
                args: vec!["two\nlines".into(), String::new()],
            }],
            artifact: Some(path.clone()),
            approved_digest: None,
            steps: vec![],
            failure_policy: FailurePolicy::Stop,
            logging: TaskLogging::Raw,
            interactive: false,
            build_outputs: vec![PathBuf::from("/tmp/one\noutput"), path.clone()],
            artifact_from_task: None,
            timeout_seconds: None,
        };
        let mut form = RunForm::task("project".into(), 1, task.clone());
        form.fields[0].value.text = "Renamed".into();
        task.name = "Renamed".into();
        assert_eq!(form.build_task().unwrap(), task);
        let config = EditorConfig {
            executable: path,
            args: vec!["literal\nargument".into(), "--".into(), "{file}".into()],
            external_gui: false,
        };
        let form = RunForm::editor("project".into(), 1, Some(config.clone()));
        assert_eq!(form.build_editor().unwrap(), config);
        assert_eq!(arguments("\n"), vec![String::new()]);
        assert_eq!(
            arguments("value\n\n"),
            vec!["value".to_owned(), String::new()]
        );
    }
}
