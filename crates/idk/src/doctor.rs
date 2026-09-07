use crate::git::GitExecutable;
use crate::store::Store;
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Serialize)]
pub struct Finding {
    pub area: String,
    pub status: String,
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub version: &'static str,
    pub findings: Vec<Finding>,
    pub field_acceptance: &'static str,
}

impl Report {
    pub fn has_failures(&self) -> bool {
        self.findings.iter().any(|f| f.status != "ok")
    }
}

pub fn inspect(data_dir: Option<&Path>) -> Report {
    let mut findings = vec![Finding {
        area: "binary".into(),
        status: "ok".into(),
        detail: format!("{} / {}", std::env::consts::OS, std::env::consts::ARCH),
    }];
    match Store::open(data_dir) {
        Ok(store) => {
            findings.push(Finding {
                area: "data".into(),
                status: "ok".into(),
                detail: format!(
                    "config={} state={} runtime={}",
                    store.config_dir.display(),
                    store.state_dir.display(),
                    store.runtime_dir.display()
                ),
            });
            match store.load() {
                Ok(workspace) => {
                    findings.push(Finding {
                        area: "configuration".into(),
                        status: "ok".into(),
                        detail: format!(
                            "{} projects; schema {}; revision {}",
                            workspace.projects.len(),
                            workspace.schema,
                            workspace.revision
                        ),
                    });
                    for project in &workspace.projects {
                        if !project.root.is_dir() {
                            findings.push(Finding {
                                area: format!("project:{}", project.name),
                                status: "warning".into(),
                                detail: "project path unavailable; reconnect its definition".into(),
                            });
                        }
                        if !project.shell.executable.is_file() {
                            findings.push(Finding {
                                area: format!("shell:{}", project.name),
                                status: "warning".into(),
                                detail: "selected csh/tcsh executable is unavailable".into(),
                            });
                        }
                        for terminal in &project.terminals {
                            if !terminal.cwd.is_dir() {
                                findings.push(Finding {
                                    area: format!("terminal:{}:{}", project.name, terminal.name),
                                    status: "warning".into(),
                                    detail:
                                        "start directory unavailable; other terminals remain usable"
                                            .into(),
                                });
                            }
                        }
                    }
                }
                Err(error) => findings.push(Finding {
                    area: "configuration".into(),
                    status: "error".into(),
                    detail: format!("{error:#}"),
                }),
            }
        }
        Err(error) => findings.push(Finding {
            area: "data".into(),
            status: "error".into(),
            detail: format!("{error:#}"),
        }),
    }
    match GitExecutable::discover() {
        Ok(git) => findings.push(Finding {
            area: "git".into(),
            status: "ok".into(),
            detail: git.path().display().to_string(),
        }),
        Err(error) => findings.push(Finding {
            area: "git".into(),
            status: "warning".into(),
            detail: format!("{error:#}"),
        }),
    }
    Report {
        version: env!("CARGO_PKG_VERSION"),
        findings,
        field_acceptance:
            "not inferred from this diagnostic; test the released package in the target environment",
    }
}
