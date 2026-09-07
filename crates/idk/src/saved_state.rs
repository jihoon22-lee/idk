//! Pure persisted-record decoding and validation shared by startup and package
//! health. These checks never start workers, inspect processes, execute Git or
//! initialization, change outcomes, migrate records, or touch referenced logs.
use crate::git::Repository;
use crate::git_wire::{GitOperationKind, GitOperationState, GitOutcome};
use crate::model::{absolute_path, valid_id, valid_name, MAX_PROJECTS, MAX_TERMINALS};
use crate::protocol::SessionState;
use crate::run_wire::{RunInfo, SourceObservation};
use crate::store::Store;
use crate::terminal::TerminalExit;
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
pub(crate) const HOST_RECORD_LIMIT: usize = MAX_PROJECTS * MAX_TERMINALS + MAX_TERMINALS;
pub(crate) const RUN_RECORD_LIMIT: usize = 64;
pub(crate) const GIT_RECORD_LIMIT: usize = 128;
pub(crate) const LOG_BYTE_LIMIT: u64 = 4 * 1024 * 1024;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HostLedger {
    pub schema: u32,
    pub sessions: Vec<HostRecord>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HostRecord {
    pub session_id: String,
    pub project_id: String,
    pub terminal_id: String,
    pub name: String,
    pub persistent: bool,
    pub state: SessionState,
    pub definition_revision: u64,
    pub launch_digest: Option<String>,
    pub exit: Option<TerminalExit>,
    #[serde(default)]
    pub purpose: Option<String>,
    #[serde(default)]
    pub run_id: Option<String>,
}
impl HostRecord {
    pub(crate) fn validate(&self) -> Result<()> {
        valid_id(&self.session_id)?;
        valid_id(&self.project_id)?;
        valid_id(&self.terminal_id)?;
        valid_name(&self.name)?;
        if let Some(value) = &self.launch_digest {
            digest(value)?;
        }
        match self.purpose.as_deref() {
            None => ensure!(
                self.run_id.is_none(),
                "ordinary terminal record has a Run identity"
            ),
            Some("run") => {
                ensure!(
                    !self.persistent,
                    "Run terminal cannot be a saved shell definition"
                );
                if let Some(id) = &self.run_id {
                    valid_id(id)?;
                    ensure!(id == &self.terminal_id, "Run terminal identity mismatch");
                }
            }
            Some("editor") => ensure!(
                !self.persistent && self.run_id.is_none(),
                "invalid editor terminal identity"
            ),
            _ => anyhow::bail!("unsupported terminal purpose; record preserved"),
        }
        Ok(())
    }
}
impl HostLedger {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            self.schema == 1 && self.sessions.len() <= HOST_RECORD_LIMIT,
            "unsupported or oversized host session ledger; preserved"
        );
        let mut ids = HashSet::new();
        let mut definitions = HashSet::new();
        for record in &self.sessions {
            record.validate()?;
            ensure!(
                ids.insert(&record.session_id)
                    && definitions.insert((&record.project_id, &record.terminal_id)),
                "duplicate session identity in host ledger; preserved"
            );
        }
        Ok(())
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RunLedger {
    pub schema: u32,
    pub runs: Vec<RunInfo>,
}
impl RunLedger {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            self.schema == 1 && self.runs.len() <= RUN_RECORD_LIMIT,
            "unsupported or oversized run ledger; preserved"
        );
        let mut ids = HashSet::new();
        let mut operations = HashSet::new();
        let mut sessions = HashSet::new();
        for run in &self.runs {
            valid_id(&run.run_id)?;
            valid_id(&run.operation_id)?;
            valid_id(&run.project_id)?;
            valid_id(&run.task_id)?;
            ensure!(
                ids.insert(&run.run_id) && operations.insert(&run.operation_id),
                "duplicate run or operation identity; ledger preserved"
            );
            if let Some(id) = &run.session_id {
                valid_id(id)?;
                ensure!(
                    sessions.insert(id),
                    "duplicate Run session identity; ledger preserved"
                );
            }
            valid_name(&run.name)?;
            absolute_path(&run.cwd)?;
            digest(&run.launch_digest)?;
            digest(&run.initialization_digest)?;
            ensure!(
                !run.steps.is_empty()
                    && run.steps.len() <= 64
                    && run.source_roots.len() <= 128
                    && run.build_outputs.len() <= 32,
                "run ledger record exceeds definition bounds"
            );
            for (index, step) in run.steps.iter().enumerate() {
                ensure!(step.index == index, "run step order is invalid");
                valid_name(&step.name)?;
                ensure!(
                    step.exit_code.is_none_or(|code| (0..=255).contains(&code)),
                    "invalid step exit code"
                );
            }
            for path in run.source_roots.iter().chain(&run.build_outputs) {
                absolute_path(path)?;
            }
            if let Some(path) = &run.artifact.path {
                absolute_path(path)?;
            }
            for id in [&run.artifact.from_task, &run.artifact.from_run]
                .into_iter()
                .flatten()
            {
                valid_id(id)?;
            }
            ensure!(
                !run.artifact.verified_bytes,
                "this schema cannot attest artifact byte identity"
            );
            ensure!(
                run.timeout_seconds
                    .is_none_or(|seconds| (1..=604800).contains(&seconds)),
                "invalid run timeout"
            );
            let log = &run.log;
            ensure!(
                log.run_id == run.run_id
                    && log.generation > 0
                    && log.limit_bytes <= LOG_BYTE_LIMIT
                    && log.bytes <= log.limit_bytes,
                "invalid run log descriptor; ledger preserved"
            );
            observation(&run.source_start)?;
            if let Some(source) = &run.source_end {
                observation(source)?;
            }
        }
        Ok(())
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GitLedger {
    pub schema: u32,
    pub operations: Vec<GitRecord>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GitRecord {
    pub id: String,
    pub context_id: String,
    pub project_id: String,
    pub repository: Repository,
    pub kind: GitOperationKind,
    pub state: GitOperationState,
    #[serde(default)]
    pub cleanup_acknowledged: bool,
    pub outcome: Option<GitOutcome>,
    pub exit_code: Option<u32>,
    pub commit: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}
impl GitLedger {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            self.schema == 1 && self.operations.len() <= GIT_RECORD_LIMIT,
            "unsupported or oversized Git operation ledger; preserved"
        );
        let mut ids = HashSet::new();
        for record in &self.operations {
            valid_id(&record.id)?;
            valid_id(&record.context_id)?;
            valid_id(&record.project_id)?;
            ensure!(
                ids.insert(&record.id),
                "duplicate Git operation identity; ledger preserved"
            );
            for path in [
                &record.repository.root,
                &record.repository.git_dir,
                &record.repository.common_dir,
            ] {
                absolute_path(path)?;
            }
            if let Some(oid) = &record.commit {
                object_id(oid)?;
            }
            ensure!(
                !record.cleanup_acknowledged || record.state == GitOperationState::Unknown,
                "cleanup acknowledgment belongs only to an unknown Git outcome"
            );
        }
        Ok(())
    }
}
fn digest(value: &str) -> Result<()> {
    ensure!(
        value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid saved digest"
    );
    Ok(())
}
fn object_id(value: &str) -> Result<()> {
    ensure!(
        matches!(value.len(), 40 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid saved Git object identity"
    );
    Ok(())
}
fn observation(source: &SourceObservation) -> Result<()> {
    if let Some(path) = &source.identity {
        absolute_path(path)?;
    }
    if let Some(oid) = &source.git_head {
        object_id(oid)?;
    }
    if let Some(value) = &source.status_digest {
        digest(value)?;
    }
    Ok(())
}
/// Each file is an independent atomic snapshot. Live outcomes are left exactly
/// as recorded; cross-file joins would race ongoing host updates and retention.
pub(crate) fn inspect(store: &Store) -> Result<BTreeMap<String, u32>> {
    let mut schemas = BTreeMap::new();
    if let Some(ledger) = store.read_state::<HostLedger>("host-sessions.json")? {
        ledger.validate()?;
        schemas.insert("host-sessions.json".into(), ledger.schema);
    }
    if let Some(ledger) = store.read_state::<RunLedger>("runs.json")? {
        ledger.validate()?;
        schemas.insert("runs.json".into(), ledger.schema);
    }
    if let Some(ledger) = store.read_state::<GitLedger>("git-operations.json")? {
        ledger.validate()?;
        schemas.insert("git-operations.json".into(), ledger.schema);
    }
    Ok(schemas)
}
