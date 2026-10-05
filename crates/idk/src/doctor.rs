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
    pub fn brief(&self) -> String {
        let mut counts = std::collections::BTreeMap::<&str, [usize; 3]>::new();
        for finding in &self.findings {
            let area = finding.area.split(':').next().unwrap_or("check");
            let index = match finding.status.as_str() {
                "ok" => 0,
                "warning" => 1,
                _ => 2,
            };
            counts.entry(area).or_default()[index] += 1;
        }
        let mut lines = vec![format!(
            "idk {} / {} / {}",
            self.version,
            std::env::consts::OS,
            std::env::consts::ARCH
        )];
        for (area, [ok, warnings, errors]) in counts {
            lines.push(format!("{area}: ok={ok} warning={warnings} error={errors}"));
        }
        lines.push(
            "target-field-acceptance=not-inferred; follow local policy before sharing any result"
                .into(),
        );
        lines.join("\n")
    }
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
            inspect_runtime(&store, &mut findings);
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
    inspect_installation(&mut findings);
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

fn inspect_runtime(store: &Store, findings: &mut Vec<Finding>) {
    match crate::install::health(store) {
        Ok(report) => findings.push(Finding { area: "state".into(), status: "ok".into(),
            detail: format!("configuration schema {}; {} durable ledger schema(s) readable; no migration performed",
                report.configuration_schema, report.state_schemas.len()) }),
        Err(error) => findings.push(Finding { area: "state".into(), status: "error".into(),
            detail: format!("{error:#}; original files preserved") }),
    }
    match crate::client::Client::connect(store) {
        Ok(client) => findings.push(Finding { area: "host".into(), status: "ok".into(),
            detail: format!("same-user host {} / protocol {} / matching binary; project commands were not started",
                client.info().version, client.info().protocol) }),
        Err(error) if error.downcast_ref::<std::io::Error>().is_some_and(|error|
            matches!(error.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused)) => {
                findings.push(Finding { area: "host".into(), status: "ok".into(),
                    detail: "no active host listener; an explicit session start can create one".into() });
            }
        Err(error) => findings.push(Finding { area: "host".into(), status: "warning".into(),
            detail: format!("{error:#}; keep existing host and use its original generation binary when builds differ") }),
    }
    match store.read_state::<crate::saved_state::HostExit>("host-exit.json") {
        Ok(Some(exit)) if exit.validate().is_ok() => {
            match (exit.stopped_at_ms, exit.error) {
                (None, _) => {
                    // A running record with a dead pid is a crash: no exit
                    // path ran to record an outcome.
                    let alive = unsafe { libc::kill(exit.pid as libc::pid_t, 0) } == 0
                        || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
                    if alive {
                        findings.push(Finding {
                            area: "host-exit".into(),
                            status: "ok".into(),
                            detail: format!(
                                "host is running (instance {}, pid {})",
                                exit.host_instance, exit.pid
                            ),
                        });
                    } else {
                        findings.push(Finding {
                            area: "host-exit".into(),
                            status: "warning".into(),
                            detail: format!(
                                "host instance {} (pid {}) exited without recording an outcome; its ledgers reconcile on the next host",
                                exit.host_instance, exit.pid
                            ),
                        });
                    }
                }
                (Some(_), None) => findings.push(Finding {
                    area: "host-exit".into(),
                    status: "ok".into(),
                    detail: "previous host recorded a clean shutdown".into(),
                }),
                (Some(_), Some(error)) => findings.push(Finding {
                    area: "host-exit".into(),
                    status: "warning".into(),
                    detail: format!("previous host exited without a clean shutdown: {error}"),
                }),
            }
        }
        Ok(Some(_)) => findings.push(Finding {
            area: "host-exit".into(),
            status: "warning".into(),
            detail: "previous host exit record is unsupported; file preserved".into(),
        }),
        Err(error) => findings.push(Finding {
            area: "host-exit".into(),
            status: "warning".into(),
            detail: format!("previous host exit record is unreadable; preserved: {error:#}"),
        }),
        Ok(None) => {}
    }
}

fn inspect_installation(findings: &mut Vec<Finding>) {
    let installed = std::env::current_exe().ok().and_then(|binary| {
        let generation = binary.parent()?;
        let generations = generation.parent()?;
        if generations.file_name()? != "generations" {
            return None;
        }
        Some((
            generations.parent()?.to_owned(),
            generation.file_name()?.to_str()?.to_owned(),
        ))
    });
    let Some((prefix, generation)) = installed else {
        findings.push(Finding { area: "package".into(), status: "ok".into(),
            detail: "standalone binary; use package verify with the explicitly supplied archive and trusted digest to check bundle provenance".into() });
        return;
    };
    let result = crate::install::Installer::open(&prefix).and_then(|installer| {
        installer.status()?;
        installer.verify(&generation)
    });
    match result {
        Ok(value) => findings.push(Finding { area: "package".into(), status: "ok".into(),
            detail: format!("installed generation {}: archive, executable and full notice bytes verified", value.name) }),
        Err(error) => findings.push(Finding { area: "package".into(), status: "error".into(),
            detail: format!("{error:#}; review package status/recover at {} without deleting preserved generations", prefix.display()) }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn brief_keeps_failure_counts_without_local_project_or_error_payloads() {
        let report = Report {
            version: "0.4.0",
            field_acceptance: "not inferred",
            findings: vec![
                Finding {
                    area: "project:private-project".into(),
                    status: "warning".into(),
                    detail: "/private/source/path".into(),
                },
                Finding {
                    area: "state".into(),
                    status: "error".into(),
                    detail: "private error detail".into(),
                },
            ],
        };
        let brief = report.brief();
        assert!(!brief.contains("private"));
        assert!(brief.contains("project: ok=0 warning=1 error=0"));
        assert!(brief.contains("state: ok=0 warning=0 error=1"));
        assert!(report.has_failures());
    }
}
