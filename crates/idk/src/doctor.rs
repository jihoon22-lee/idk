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
    let connected_host = match crate::client::Client::connect(store) {
        Ok(client) => {
            findings.push(Finding { area: "host".into(), status: "ok".into(),
                detail: format!("same-user host {} / protocol {} / matching binary; project commands were not started",
                    client.info().version, client.info().protocol) });
            Some((client.info().host_instance.clone(), client.info().pid))
        }
        Err(error)
            if error.downcast_ref::<std::io::Error>().is_some_and(|error| {
                matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                )
            }) =>
        {
            findings.push(Finding {
                area: "host".into(),
                status: "ok".into(),
                detail: "no active host listener; an explicit session start can create one".into(),
            });
            None
        }
        Err(error) => {
            findings.push(Finding { area: "host".into(), status: "warning".into(),
                detail: format!("{error:#}; keep existing host and use its original generation binary when builds differ") });
            None
        }
    };
    match store.read_state::<crate::saved_state::HostExit>("host-exit.json") {
        Ok(Some(exit)) if exit.validate().is_ok() => {
            match (exit.stopped_at_ms, exit.error) {
                (None, _) => {
                    // A PID can belong to another process after a crash or
                    // reboot. Only the verified handshake identifies this host.
                    if connected_host.as_ref().is_some_and(|(instance, pid)| {
                        instance == &exit.host_instance && *pid == exit.pid
                    }) {
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
                                "host instance {} (pid {}) has no recorded exit and its running identity was not confirmed; inspect host status with its original generation binary",
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
    fn an_unconfirmed_host_record_remains_a_failure_in_the_brief_report() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(Some(&temp.path().join("data"))).unwrap();
        let record = crate::saved_state::HostExit {
            schema: 1,
            host_instance: crate::model::new_id(),
            pid: std::process::id(),
            started_at_ms: 1,
            stopped_at_ms: None,
            error: None,
        };
        store.write_state("host-exit.json", &record).unwrap();
        let path = store.state_dir.join("host-exit.json");
        let before = std::fs::read(&path).unwrap();
        let mut findings = Vec::new();
        inspect_runtime(&store, &mut findings);
        let report = Report {
            version: env!("CARGO_PKG_VERSION"),
            findings,
            field_acceptance: "not inferred",
        };
        assert!(report.has_failures());
        let brief = report.brief();
        assert!(brief.contains("host-exit: ok=0 warning=1 error=0"));
        assert!(!brief.contains(&record.host_instance));
        assert_eq!(std::fs::read(path).unwrap(), before);
        assert!(!store.socket_path().exists());
    }

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
