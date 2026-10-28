//! Private immutable result bodies and bounded, identity-aware views.
use crate::store::{ensure_private_dir, read_private, FileLock};
use crate::structured_result::{self as format, BodyReference, ResultSummary, StructuredResult};
use anyhow::{ensure, Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

pub const TOTAL_RESULT_BYTES: u64 = 256 * 1024 * 1024;
pub const PINNED_RESULT_BYTES: u64 = 128 * 1024 * 1024;
pub const PINNED_RUN_LIMIT: usize = 16;
pub const RETENTION_MS: u64 = 30 * 24 * 60 * 60 * 1000;
pub const RAW_PAGE_BYTES: usize = 65536;
pub const FINDING_PAGE_ITEMS: usize = 100;
pub const FINDING_PAGE_BYTES: usize = 65536;
pub const MAX_EVENT_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawResultPage {
    pub result: StructuredResult,
    pub offset: u64,
    pub next_offset: u64,
    pub data_base64: String,
    pub eof: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingLocation {
    /// Literal producer path. This is not permission to open it or a content identity.
    pub path: String,
    pub start_line: Option<u64>,
    pub start_column: Option<u64>,
    pub end_line: Option<u64>,
    pub end_column: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructuredFinding {
    pub body_sha256: String,
    pub index: usize,
    pub severity: String,
    pub code: Option<String>,
    pub provider: Option<String>,
    pub message: String,
    pub location: Option<FindingLocation>,
    pub evidence_object: Option<String>,
    pub data_pointer: Option<String>,
    /// Display fields were bounded. Original bytes remain available via raw/export.
    pub truncated: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindingPage {
    pub result: StructuredResult,
    pub offset: usize,
    pub next_offset: usize,
    pub total: usize,
    pub findings: Vec<StructuredFinding>,
    pub more: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedResult {
    pub path: PathBuf,
    pub sha256: String,
    pub bytes: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonState {
    Comparable,
    Partial,
    Incompatible,
    Unavailable,
    Unsupported,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultComparison {
    pub state: ComparisonState,
    pub baseline_run_id: String,
    pub run_id: String,
    pub reason: String,
    pub new_count: usize,
    pub unchanged_count: usize,
    pub resolved_count: usize,
    pub carried_count: usize,
    /// At most 100 producer identities in each sample; counts cover the full body.
    pub new_sample: Vec<String>,
    pub resolved_sample: Vec<String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventState {
    #[default]
    Missing,
    Observed,
    Partial,
    Invalid,
    Oversize,
    Expired,
    Tampered,
    StorageFailed,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressEvent {
    pub seq: u64,
    pub event_type: String,
    pub producer_run_id: String,
    pub task_id: Option<String>,
    pub message: String,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EventProgress {
    pub state: EventState,
    pub bytes_observed: u64,
    pub events_observed: u64,
    pub invalid_events: u64,
    pub sequence_gaps: u64,
    pub truncated_tail: bool,
    pub latest: Option<ProgressEvent>,
    pub error: Option<String>,
    pub body: Option<BodyReference>,
}

pub struct ResultStore {
    root: PathBuf,
    limit: u64,
}
impl ResultStore {
    pub fn open(state_dir: &Path) -> Result<Self> {
        let root = state_dir.join("results");
        ensure_private_dir(&root)?;
        Ok(Self {
            root,
            limit: TOTAL_RESULT_BYTES,
        })
    }
    fn path(&self, body: &BodyReference, events: bool) -> Result<PathBuf> {
        crate::model::valid_id(&body.run_id)?;
        ensure!(
            body.sha256.len() == 64 && body.sha256.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid result digest"
        );
        ensure!(
            body.bytes
                <= if events {
                    MAX_EVENT_BYTES
                } else {
                    format::MAX_RESULT_BYTES
                },
            "invalid result body length"
        );
        Ok(self.root.join(format!(
            "{}.{}.{}",
            body.run_id,
            body.sha256,
            if events { "events" } else { "json" }
        )))
    }
    pub fn bytes_used(&self) -> Result<u64> {
        let mut total = 0u64;
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            if entry.file_name() == "result.lock" {
                continue;
            }
            let meta = entry.path().symlink_metadata()?;
            ensure!(
                meta.is_file() && meta.uid() == unsafe { libc::geteuid() },
                "unexpected file in managed result storage; preserved"
            );
            total = total
                .checked_add(meta.len())
                .context("result storage size overflow")?;
        }
        Ok(total)
    }
    fn put(
        &self,
        run_id: &str,
        bytes: &[u8],
        events: bool,
    ) -> Result<std::result::Result<BodyReference, StructuredResult>> {
        crate::model::valid_id(run_id)?;
        let body = BodyReference {
            run_id: run_id.into(),
            sha256: format::digest(bytes),
            bytes: bytes.len() as u64,
        };
        let path = self.path(&body, events)?;
        let _lock = FileLock::acquire(&self.root.join("result.lock"), false)?;
        if path.symlink_metadata().is_ok() {
            let existing = read_private(&path, bytes.len())?;
            ensure!(
                existing == bytes,
                "existing immutable result differs; preserved"
            );
            File::open(&self.root)?.sync_all()?;
            return Ok(Ok(body));
        }
        let total = self.bytes_used()?;
        if total.saturating_add(body.bytes) > self.limit {
            return Ok(Err(StructuredResult::Limited {
                limit_bytes: self.limit,
                required_bytes: body.bytes,
            }));
        }
        // create_new refuses replacement, including symlinks. An incomplete write
        // remains charged to the budget and never becomes an Available reference.
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o400)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)?;
        let written = (|| -> Result<()> {
            file.write_all(bytes)?;
            file.sync_all()?;
            File::open(&self.root)?.sync_all()?;
            Ok(())
        })();
        if let Err(error) = written {
            // This attempt has not published a reference. Remove only the inode
            // we just created; a replaced path is preserved and stays charged.
            if let (Ok(opened), Ok(current)) = (file.metadata(), path.symlink_metadata()) {
                if current.is_file()
                    && (opened.dev(), opened.ino()) == (current.dev(), current.ino())
                {
                    let _ = std::fs::remove_file(&path);
                    let _ = File::open(&self.root).and_then(|directory| directory.sync_all());
                }
            }
            return Err(error);
        }
        Ok(Ok(body))
    }
    pub fn capture(&self, run_id: &str, path: &Path) -> StructuredResult {
        match format::read_input(path, format::MAX_RESULT_BYTES) {
            Ok(format::Input::Missing) => StructuredResult::Missing,
            Ok(format::Input::Oversize(observed_bytes)) => StructuredResult::Oversize {
                limit_bytes: format::MAX_RESULT_BYTES,
                observed_bytes,
            },
            Ok(format::Input::Bytes(bytes)) => self.capture_bytes(run_id, &bytes),
            Err(error) => StructuredResult::Invalid {
                error: format::safe(format!("{error:#}"), 512),
                body: None,
            },
        }
    }
    pub fn capture_bytes(&self, run_id: &str, bytes: &[u8]) -> StructuredResult {
        if bytes.len() as u64 > format::MAX_RESULT_BYTES {
            return StructuredResult::Oversize {
                limit_bytes: format::MAX_RESULT_BYTES,
                observed_bytes: bytes.len() as u64,
            };
        }
        let body = match self.put(run_id, bytes, false) {
            Ok(Ok(body)) => body,
            Ok(Err(state)) => return state,
            Err(error) => {
                return StructuredResult::StorageFailed {
                    error: format::safe(format!("{error:#}"), 512),
                }
            }
        };
        let document: Value = match serde_json::from_slice(bytes) {
            Ok(document) => document,
            Err(error) => {
                return StructuredResult::Invalid {
                    error: format::safe(error, 512),
                    body: Some(body),
                }
            }
        };
        if let Some((schema_id, schema_version)) = format::unsupported(&document) {
            return StructuredResult::Unsupported {
                schema_id,
                schema_version,
                body: Some(body),
            };
        }
        match format::summarize(&document, run_id) {
            Ok(summary) => StructuredResult::Stored { summary, body },
            Err(error) => StructuredResult::Invalid {
                error: format::safe(format!("{error:#}"), 512),
                body: Some(body),
            },
        }
    }
    /// Lossless JSON-value migration of old inline records. Their original raw
    /// digest cannot be reproduced from a parsed Value, so a new snapshot digest
    /// explicitly identifies the serialized body; the old ledger is backed up.
    pub fn migrate(&self, run_id: &str, result: &StructuredResult) -> StructuredResult {
        match result {
            StructuredResult::Available { document, .. } => match serde_json::to_vec(document) {
                Ok(bytes) => self.capture_bytes(run_id, &bytes),
                Err(error) => StructuredResult::StorageFailed {
                    error: format::safe(error, 512),
                },
            },
            _ => result.clone(),
        }
    }
    fn verified(&self, result: &StructuredResult) -> (StructuredResult, Option<Vec<u8>>) {
        let Some(body) = result.body() else {
            return (result.clone(), None);
        };
        if matches!(
            result,
            StructuredResult::Expired { .. } | StructuredResult::Tampered { .. }
        ) {
            return (result.clone(), None);
        }
        let outcome = (|| -> Result<Vec<u8>> {
            let path = self.path(body, false)?;
            let bytes = read_private(&path, format::MAX_RESULT_BYTES as usize)?;
            ensure!(
                bytes.len() as u64 == body.bytes && format::digest(&bytes) == body.sha256,
                "stored result digest or byte length changed"
            );
            if let StructuredResult::Stored { summary, .. } = result {
                let document: Value = serde_json::from_slice(&bytes)?;
                ensure!(
                    format::summarize(&document, &body.run_id)? == *summary,
                    "stored summary differs from immutable result body"
                );
            }
            Ok(bytes)
        })();
        match outcome {
            Ok(bytes) => (result.clone(), Some(bytes)),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                (StructuredResult::Expired { body: body.clone() }, None)
            }
            Err(error) => (
                StructuredResult::Tampered {
                    body: body.clone(),
                    error: format::safe(format!("{error:#}"), 512),
                },
                None,
            ),
        }
    }
    pub fn verify(&self, result: &StructuredResult) -> StructuredResult {
        self.verified(result).0
    }
    pub fn raw(
        &self,
        result: &StructuredResult,
        offset: u64,
        limit: usize,
    ) -> Result<RawResultPage> {
        ensure!(
            (1..=RAW_PAGE_BYTES).contains(&limit),
            "raw result page limit must be 1–65536"
        );
        let (result, bytes) = self.verified(result);
        let bytes = bytes.unwrap_or_default();
        ensure!(
            offset <= bytes.len() as u64 || bytes.is_empty(),
            "result offset exceeds body length"
        );
        let start = (offset as usize).min(bytes.len());
        let end = start.saturating_add(limit).min(bytes.len());
        Ok(RawResultPage {
            result,
            offset,
            next_offset: end as u64,
            data_base64: base64::engine::general_purpose::STANDARD.encode(&bytes[start..end]),
            eof: end == bytes.len(),
        })
    }
    pub fn findings(
        &self,
        result: &StructuredResult,
        offset: usize,
        limit: usize,
    ) -> Result<FindingPage> {
        ensure!(
            (1..=FINDING_PAGE_ITEMS).contains(&limit),
            "finding page limit must be 1–100"
        );
        let (result, bytes) = self.verified(result);
        let mut page = FindingPage {
            result,
            offset,
            next_offset: offset,
            total: 0,
            findings: vec![],
            more: false,
        };
        if page.result.summary().is_none() {
            return Ok(page);
        }
        let Some(bytes) = bytes else { return Ok(page) };
        let document: Value = serde_json::from_slice(&bytes)?;
        let findings = document["findings"]
            .as_array()
            .context("stored findings unavailable")?;
        page.total = findings.len();
        ensure!(offset <= page.total, "finding offset exceeds result");
        let sha = &page.result.body().unwrap().sha256;
        let mut charged = 0;
        for (index, value) in findings.iter().enumerate().skip(offset).take(limit) {
            let finding = finding(value, sha, index);
            let size = serde_json::to_vec(&finding)?.len();
            if charged + size > FINDING_PAGE_BYTES - 4096 {
                break;
            }
            charged += size;
            page.findings.push(finding);
            page.next_offset = index + 1;
        }
        page.more = page.next_offset < page.total;
        Ok(page)
    }
    pub fn export(&self, result: &StructuredResult, path: &Path) -> Result<ExportedResult> {
        ensure!(path.is_absolute(), "export destination must be absolute");
        let (state, bytes) = self.verified(result);
        let bytes =
            bytes.with_context(|| format!("cannot export {} result", state.status_label()))?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .context("export requires a new file; existing destinations are preserved")?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        File::open(path.parent().context("export parent missing")?)?.sync_all()?;
        Ok(ExportedResult {
            path: path.into(),
            sha256: format::digest(&bytes),
            bytes: bytes.len() as u64,
        })
    }
    pub fn remove(&self, body: &BodyReference, events: bool) -> Result<()> {
        let _lock = FileLock::acquire(&self.root.join("result.lock"), false)?;
        let path = self.path(body, events)?;
        match path.symlink_metadata() {
            Ok(meta) => {
                ensure!(
                    meta.is_file() && meta.uid() == unsafe { libc::geteuid() },
                    "unexpected retained result file; preserved"
                );
                let bytes = read_private(
                    &path,
                    if events {
                        MAX_EVENT_BYTES
                    } else {
                        format::MAX_RESULT_BYTES
                    } as usize,
                )?;
                ensure!(
                    bytes.len() as u64 == body.bytes && format::digest(&bytes) == body.sha256,
                    "retired result bytes changed; preserved with cleanup intent"
                );
                std::fs::remove_file(&path)?;
                File::open(&self.root)?.sync_all()?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }
}
fn finding(value: &Value, sha: &str, index: usize) -> StructuredFinding {
    let mut truncated = false;
    let mut bounded = |text: &str, limit: usize| {
        let result: String = text
            .chars()
            .filter(|ch| !ch.is_control())
            .take(limit)
            .collect();
        truncated |= result.len() != text.len();
        result
    };
    let message = bounded(
        value.get("message").and_then(Value::as_str).unwrap_or(""),
        8192,
    );
    let severity = bounded(
        value
            .get("severity")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
        64,
    );
    let code = value
        .get("rule_id")
        .or_else(|| value.get("code"))
        .and_then(Value::as_str)
        .map(|s| bounded(s, 256));
    let provider = value
        .get("provider")
        .and_then(Value::as_str)
        .map(|s| bounded(s, 256));
    let primary = value.get("primary_location");
    let path = primary
        .and_then(|p| p.get("path"))
        .or_else(|| value.pointer("/evidence/path"))
        .and_then(Value::as_str);
    // Do not truncate a source path into a different path. Oversized paths remain
    // in the raw document but are deliberately not actionable editor targets.
    let location = path
        .filter(|p| !p.is_empty() && p.len() <= 4096 && !p.chars().any(char::is_control))
        .map(|path| FindingLocation {
            path: path.into(),
            start_line: primary
                .and_then(|p| p.get("start_line"))
                .and_then(Value::as_u64),
            start_column: primary
                .and_then(|p| p.get("start_column"))
                .and_then(Value::as_u64),
            end_line: primary
                .and_then(|p| p.get("end_line"))
                .and_then(Value::as_u64),
            end_column: primary
                .and_then(|p| p.get("end_column"))
                .and_then(Value::as_u64),
        });
    let evidence_object = value
        .pointer("/evidence/object")
        .and_then(Value::as_str)
        .map(|s| bounded(s, 512));
    let data_pointer = value
        .pointer("/evidence/data_pointer")
        .and_then(Value::as_str)
        .map(|s| bounded(s, 1024));
    truncated |= path.is_some() && location.is_none();
    StructuredFinding {
        body_sha256: sha.into(),
        index,
        severity,
        code,
        provider,
        message,
        location,
        evidence_object,
        data_pointer,
        truncated,
    }
}

impl ResultStore {
    /// Events are progress observations only. They never complete or fail a Run.
    pub fn events(
        &self,
        run_id: &str,
        path: &Path,
        expected_producer_id: Option<&str>,
        retain: bool,
    ) -> EventProgress {
        let mut progress = EventProgress::default();
        let bytes = match format::read_input(path, MAX_EVENT_BYTES) {
            Ok(format::Input::Missing) => return progress,
            Ok(format::Input::Oversize(size)) => {
                progress.state = EventState::Oversize;
                progress.bytes_observed = size;
                progress.error = Some("events exceed 8 MiB observation limit".into());
                return progress;
            }
            Ok(format::Input::Bytes(bytes)) => bytes,
            Err(error) => {
                progress.state = EventState::Invalid;
                progress.error = Some(format::safe(error, 512));
                return progress;
            }
        };
        progress.bytes_observed = bytes.len() as u64;
        progress.state = EventState::Observed;
        let mut previous = None;
        let mut producer_id = expected_producer_id.map(str::to_owned);
        for raw in bytes.split_inclusive(|byte| *byte == b'\n') {
            if raw.last() != Some(&b'\n') {
                progress.truncated_tail = true;
                break;
            }
            let event = (|| -> Result<ProgressEvent> {
                ensure!(raw.len() <= 65536, "event line exceeds limit");
                let value: Value = serde_json::from_slice(raw)?;
                ensure!(
                    value.get("schema_id").and_then(Value::as_str) == Some("ici.next.event")
                        && value.get("schema_version").and_then(Value::as_u64) == Some(1),
                    "unsupported event schema"
                );
                let id = format::text(&value, "/run_id")?;
                ensure!(id.len() <= 256, "event run ID exceeds limit");
                ensure!(
                    producer_id.as_ref().is_none_or(|expected| expected == id),
                    "event run ID differs from result or preceding events"
                );
                let seq = value
                    .get("seq")
                    .and_then(Value::as_u64)
                    .context("event seq missing")?;
                ensure!(
                    previous.is_none_or(|last| seq > last),
                    "event sequence repeated or decreased"
                );
                format::text(&value, "/timestamp")?;
                let event_type = format::text(&value, "/event_type")?;
                ensure!(event_type.len() <= 128, "event type exceeds limit");
                Ok(ProgressEvent {
                    seq,
                    event_type: format::safe(event_type, 128),
                    producer_run_id: id.into(),
                    task_id: value
                        .get("task_id")
                        .and_then(Value::as_str)
                        .map(|s| format::safe(s, 256)),
                    message: format::safe(
                        value.get("message").and_then(Value::as_str).unwrap_or(""),
                        1024,
                    ),
                })
            })();
            match event {
                Ok(event) => {
                    if previous.is_none() && event.seq > 1 {
                        progress.sequence_gaps += 1;
                    }
                    if previous.is_some_and(|last: u64| event.seq > last.saturating_add(1)) {
                        progress.sequence_gaps += 1;
                    }
                    previous = Some(event.seq);
                    producer_id = Some(event.producer_run_id.clone());
                    progress.events_observed += 1;
                    progress.latest = Some(event);
                }
                Err(error) => {
                    progress.invalid_events += 1;
                    progress.error = Some(format::safe(error, 512));
                }
            }
        }
        if progress.invalid_events > 0 || progress.sequence_gaps > 0 || progress.truncated_tail {
            progress.state = EventState::Partial;
        }
        if retain && !bytes.is_empty() {
            match self.put(run_id, &bytes, true) {
                Ok(Ok(body)) => progress.body = Some(body),
                Ok(Err(_)) => {
                    progress.state = EventState::StorageFailed;
                    progress.error = Some("result storage budget cannot retain events".into());
                }
                Err(error) => {
                    progress.state = EventState::StorageFailed;
                    progress.error = Some(format::safe(error, 512));
                }
            }
        }
        progress
    }
    pub fn retained_events(&self, progress: &EventProgress) -> EventProgress {
        let mut result = progress.clone();
        let Some(body) = &progress.body else {
            return result;
        };
        let read = (|| -> Result<Vec<u8>> {
            read_private(&self.path(body, true)?, MAX_EVENT_BYTES as usize)
        })();
        match read {
            Ok(bytes)
                if bytes.len() as u64 == body.bytes && format::digest(&bytes) == body.sha256 => {}
            Ok(_) => {
                result.state = EventState::Tampered;
                result.error = Some("retained event bytes changed".into());
            }
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                result.state = EventState::Expired;
                result.error = Some("retained events expired".into());
            }
            Err(error) => {
                result.state = EventState::Tampered;
                result.error = Some(format::safe(error, 512));
            }
        }
        result
    }
    pub fn compare(
        &self,
        baseline_id: &str,
        baseline: &StructuredResult,
        run_id: &str,
        current: &StructuredResult,
    ) -> ResultComparison {
        let mut result = ResultComparison {
            state: ComparisonState::Unavailable,
            baseline_run_id: baseline_id.into(),
            run_id: run_id.into(),
            reason: String::new(),
            new_count: 0,
            unchanged_count: 0,
            resolved_count: 0,
            carried_count: 0,
            new_sample: vec![],
            resolved_sample: vec![],
        };
        let (old_state, old) = self.verified(baseline);
        let (new_state, new) = self.verified(current);
        let (Some(old_summary), Some(new_summary), Some(old), Some(new)) =
            (old_state.summary(), new_state.summary(), old, new)
        else {
            result.reason = format!(
                "results unavailable: baseline {}, current {}",
                old_state.status_label(),
                new_state.status_label()
            );
            return result;
        };
        if old_summary.schema_id != new_summary.schema_id
            || old_summary.schema_version != new_summary.schema_version
            || old_summary.producer != new_summary.producer
            || old_summary.producer_version != new_summary.producer_version
            || old_summary.identity_digest != new_summary.identity_digest
            || old_summary.scope_digest != new_summary.scope_digest
        {
            result.state = ComparisonState::Incompatible;
            result.reason =
                "producer, schema, tool/policy identity or declared scope differs".into();
            return result;
        }
        let documents = (
            serde_json::from_slice::<Value>(&old),
            serde_json::from_slice::<Value>(&new),
        );
        let (Ok(old), Ok(new)) = documents else {
            result.reason = "stored document could not be decoded".into();
            return result;
        };
        let compared = if old_summary.producer == "ici" {
            compare_ici(&old, &new, old_summary, new_summary)
        } else {
            compare_lens(&old, &new, old_summary, new_summary)
        };
        match compared {
            Ok((new, unchanged, resolved, carried, complete)) => {
                result.state = if complete {
                    ComparisonState::Comparable
                } else {
                    ComparisonState::Partial
                };
                result.reason = if complete {
                    "matching producer identities and completed evidence scope"
                } else {
                    "incomplete or unknown evidence: absent observations are carried, not resolved"
                }
                .into();
                result.new_count = new.len();
                result.unchanged_count = unchanged.len();
                result.resolved_count = resolved.len();
                result.carried_count = carried.len();
                result.new_sample = new
                    .into_iter()
                    .take(100)
                    .map(|s| format::safe(s, 256))
                    .collect();
                result.resolved_sample = resolved
                    .into_iter()
                    .take(100)
                    .map(|s| format::safe(s, 256))
                    .collect();
            }
            Err(error) => {
                result.state = ComparisonState::Incompatible;
                result.reason = format::safe(error, 512);
            }
        }
        result
    }
}
type Delta = (
    BTreeSet<String>,
    BTreeSet<String>,
    BTreeSet<String>,
    BTreeSet<String>,
    bool,
);
fn check_key(check: &Value) -> Option<String> {
    let mut keys = Vec::new();
    for key in [
        "task_id",
        "check_id",
        "component_id",
        "language",
        "scope_digest",
    ] {
        let value = check.get(key)?.as_str()?.to_owned();
        if value.is_empty() {
            return None;
        }
        keys.push(Value::String(value));
    }
    for key in ["analysis_unit_id", "variant"] {
        keys.push(check.get(key).cloned().unwrap_or(Value::Null));
    }
    Some(format::json_digest(&Value::Array(keys)))
}
fn compare_ici(
    old: &Value,
    new: &Value,
    old_summary: &ResultSummary,
    new_summary: &ResultSummary,
) -> Result<Delta> {
    ensure!(
        old.pointer("/identity/fingerprint_version")
            .and_then(Value::as_str)
            == Some("ici.next.fingerprint.v3"),
        "unsupported or unknown ici fingerprint normalization"
    );
    for pointer in ["/identity/policy_digest", "/identity/toolchain_digest"] {
        format::text(old, pointer)?;
        format::text(new, pointer)?;
    }
    let old_checks = old
        .get("checks")
        .and_then(Value::as_array)
        .context("baseline completed check scope is unknown")?;
    let new_checks = new
        .get("checks")
        .and_then(Value::as_array)
        .context("current completed check scope is unknown")?;
    let completed: BTreeSet<_> = new_checks
        .iter()
        .filter(|c| {
            c.get("evidence_complete").and_then(Value::as_bool) == Some(true)
                && c.get("state").and_then(Value::as_str) == Some("SUCCEEDED")
        })
        .filter_map(check_key)
        .collect();
    let old_findings = old["findings"]
        .as_array()
        .context("baseline findings missing")?;
    let new_findings = new["findings"]
        .as_array()
        .context("current findings missing")?;
    let fingerprints = |findings: &[Value]| -> Result<BTreeSet<String>> {
        findings
            .iter()
            .map(|finding| Ok(format::text(finding, "/fingerprint")?.to_owned()))
            .collect()
    };
    let previous = fingerprints(old_findings)?;
    let current = fingerprints(new_findings)?;
    let complete = old_summary.complete == Some(true)
        && new_summary.complete == Some(true)
        && old.pointer("/execution/cancelled").and_then(Value::as_bool) == Some(false)
        && new.pointer("/execution/cancelled").and_then(Value::as_bool) == Some(false);
    let mut resolved = BTreeSet::new();
    if complete {
        for finding in old_findings {
            let fp = format::text(finding, "/fingerprint")?;
            if current.contains(fp) {
                continue;
            }
            let matching: Vec<_> = old_checks
                .iter()
                .filter(|check| {
                    check.get("task_id").is_some()
                        && check.get("task_id") == finding.get("task_id")
                        && check.get("component_id") == finding.get("component_id")
                })
                .collect();
            if !matching.is_empty()
                && matching
                    .iter()
                    .all(|check| check_key(check).is_some_and(|key| completed.contains(&key)))
            {
                resolved.insert(fp.to_owned());
            }
        }
    }
    let absent: BTreeSet<_> = previous.difference(&current).cloned().collect();
    let carried: BTreeSet<_> = absent.difference(&resolved).cloned().collect();
    let complete = complete && carried.is_empty();
    Ok((
        current.difference(&previous).cloned().collect(),
        current.intersection(&previous).cloned().collect(),
        resolved,
        carried,
        complete,
    ))
}
fn compare_lens(
    old: &Value,
    new: &Value,
    old_summary: &ResultSummary,
    new_summary: &ResultSummary,
) -> Result<Delta> {
    // v1 has no universal finding fingerprint. Compare stable structured
    // evidence objects/paths when provided. Message text is never a location.
    format::text(old, "/source/kind")?;
    format::text(new, "/source/kind")?;
    ensure!(
        !old["source"]["identity"].is_null()
            && old["source"]["identity"] != serde_json::json!([])
            && old["source"]["identity"] != serde_json::json!("")
            && old["source"]["identity"] != serde_json::json!({}),
        "Lens source identity is unknown"
    );
    let keys = |document: &Value| -> Result<(BTreeSet<String>, bool)> {
        let mut keys = BTreeSet::new();
        let mut stable = true;
        for finding in document["findings"]
            .as_array()
            .context("Lens findings missing")?
        {
            let code = format::text(finding, "/code")?;
            let object = finding.pointer("/evidence/object").and_then(Value::as_str);
            let path = finding.pointer("/evidence/path").and_then(Value::as_str);
            if object.is_none() && path.is_none() {
                stable = false;
            }
            keys.insert(format::json_digest(&serde_json::json!({"code":code,"object":object,"path":path,"message_if_no_identity":if object.is_none()&&path.is_none(){finding.get("message")}else{None}})));
        }
        Ok((keys, stable))
    };
    let (previous, old_stable) = keys(old)?;
    let (current, new_stable) = keys(new)?;
    let complete = old_summary.complete == Some(true)
        && new_summary.complete == Some(true)
        && old_stable
        && new_stable;
    let absent: BTreeSet<_> = previous.difference(&current).cloned().collect();
    Ok((
        current.difference(&previous).cloned().collect(),
        current.intersection(&previous).cloned().collect(),
        if complete {
            absent.clone()
        } else {
            BTreeSet::new()
        },
        if complete { BTreeSet::new() } else { absent },
        complete,
    ))
}
