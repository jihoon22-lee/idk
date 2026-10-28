//! Producer claims, independent of the owned process exit and raw-log diagnostics.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::OpenOptions,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

pub const MAX_RESULT_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BodyReference {
    /// A local idk run, never a producer-controlled pathname.
    pub run_id: String,
    pub sha256: String,
    pub bytes: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultSummary {
    pub schema_id: String,
    pub schema_version: u64,
    pub producer: String,
    pub producer_version: String,
    pub producer_run_id: Option<String>,
    pub findings_count: usize,
    pub complete: Option<bool>,
    pub selected_gate: Option<String>,
    pub workspace_gate: Option<String>,
    pub scope_kind: Option<String>,
    pub identity_digest: String,
    pub scope_digest: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum StructuredResult {
    Missing,
    Invalid {
        error: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body: Option<BodyReference>,
    },
    Unsupported {
        schema_id: String,
        schema_version: u64,
        #[serde(default)]
        body: Option<BodyReference>,
    },
    Oversize {
        limit_bytes: u64,
        observed_bytes: u64,
    },
    Limited {
        limit_bytes: u64,
        required_bytes: u64,
    },
    StorageFailed {
        error: String,
    },
    Expired {
        body: BodyReference,
    },
    Tampered {
        body: BodyReference,
        error: String,
    },
    Stored {
        summary: ResultSummary,
        body: BodyReference,
    },
    /// v0.5 reader only. RunRegistry snapshots this form before serving it over IPC.
    Available {
        idk_run_id: String,
        path: PathBuf,
        sha256: String,
        schema_id: String,
        schema_version: u64,
        producer_version: String,
        producer_run_id: Option<String>,
        document: Value,
    },
}
impl StructuredResult {
    pub fn body(&self) -> Option<&BodyReference> {
        match self {
            Self::Stored { body, .. } | Self::Expired { body } | Self::Tampered { body, .. } => {
                Some(body)
            }
            Self::Invalid { body, .. } | Self::Unsupported { body, .. } => body.as_ref(),
            _ => None,
        }
    }
    pub fn summary(&self) -> Option<&ResultSummary> {
        match self {
            Self::Stored { summary, .. } => Some(summary),
            _ => None,
        }
    }
    pub fn diagnostic(&self) -> Option<String> {
        match self {
            Self::Missing => Some("No structured result snapshot is available.".into()),
            Self::Invalid {error, ..} | Self::StorageFailed {error} | Self::Tampered {error, ..} => Some(error.clone()),
            Self::Unsupported {schema_id, schema_version, ..} => Some(format!("Unsupported schema {schema_id} version {schema_version}; use raw/export to inspect retained bytes.")),
            Self::Oversize {limit_bytes, observed_bytes} => Some(format!("Result body exceeds {limit_bytes} bytes (observed {observed_bytes}).")),
            Self::Limited {limit_bytes, required_bytes} => Some(format!("Managed result budget {limit_bytes} bytes cannot fit another {required_bytes} bytes; export/unpin old baselines.")),
            Self::Expired {..} => Some("Stored result body is missing; the retained Run outcome is unchanged.".into()),
            Self::Stored {..} | Self::Available {..} => None,
        }
    }
    pub fn status_label(&self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Invalid { .. } => "invalid",
            Self::Unsupported { .. } => "unsupported",
            Self::Oversize { .. } => "oversize",
            Self::Limited { .. } => "limited",
            Self::StorageFailed { .. } => "storage_failed",
            Self::Expired { .. } => "expired",
            Self::Tampered { .. } => "tampered",
            Self::Stored { .. } => "stored",
            Self::Available { .. } => "legacy_inline",
        }
    }
}

pub(crate) fn safe(value: impl ToString, limit: usize) -> String {
    value
        .to_string()
        .chars()
        .filter(|ch| !ch.is_control())
        .take(limit)
        .collect()
}
pub(crate) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(crate) fn json_digest(value: &Value) -> String {
    digest(&serde_json::to_vec(value).expect("JSON value serializes"))
}

/// Compatibility standalone reader. New host results must use ResultStore::capture.
pub fn inspect(path: &Path, run_id: &str) -> StructuredResult {
    match read_result(path, run_id) {
        Ok(result) => result,
        Err(error) => StructuredResult::Invalid {
            error: safe(format!("{error:#}"), 512),
            body: None,
        },
    }
}
pub fn read_result(path: &Path, run_id: &str) -> Result<StructuredResult> {
    let bytes = match read_input(path, MAX_RESULT_BYTES)? {
        Input::Missing => return Ok(StructuredResult::Missing),
        Input::Oversize(size) => {
            return Ok(StructuredResult::Oversize {
                limit_bytes: MAX_RESULT_BYTES,
                observed_bytes: size,
            })
        }
        Input::Bytes(bytes) => bytes,
    };
    let document: Value =
        serde_json::from_slice(&bytes).context("malformed structured result JSON")?;
    if let Some((schema_id, schema_version)) = unsupported(&document) {
        return Ok(StructuredResult::Unsupported {
            schema_id,
            schema_version,
            body: None,
        });
    }
    let summary = summarize(&document, run_id)?;
    Ok(StructuredResult::Available {
        idk_run_id: run_id.into(),
        path: path.into(),
        sha256: digest(&bytes),
        schema_id: summary.schema_id,
        schema_version: summary.schema_version,
        producer_version: summary.producer_version,
        producer_run_id: summary.producer_run_id,
        document,
    })
}
pub(crate) enum Input {
    Missing,
    Oversize(u64),
    Bytes(Vec<u8>),
}
pub(crate) fn read_input(path: &Path, limit: u64) -> Result<Input> {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Input::Missing),
        Err(error) => return Err(error).context("open producer result"),
    };
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "producer result must be a regular file");
    if metadata.len() > limit {
        return Ok(Input::Oversize(metadata.len()));
    }
    let mut bytes = Vec::new();
    (&mut file).take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Ok(Input::Oversize(bytes.len() as u64));
    }
    let after = file.metadata()?;
    let current = path
        .symlink_metadata()
        .context("producer result path changed while reading")?;
    let identity = |meta: &std::fs::Metadata| {
        (
            meta.dev(),
            meta.ino(),
            meta.len(),
            meta.mtime(),
            meta.mtime_nsec(),
            meta.ctime(),
            meta.ctime_nsec(),
        )
    };
    ensure!(
        after.is_file()
            && current.is_file()
            && identity(&metadata) == identity(&after)
            && identity(&after) == identity(&current),
        "producer result changed while reading; snapshot unavailable"
    );
    Ok(Input::Bytes(bytes))
}
pub(crate) fn unsupported(document: &Value) -> Option<(String, u64)> {
    let schema = document.get("schema_id")?.as_str()?;
    let version = document.get("schema_version")?.as_u64()?;
    (!matches!((schema, version), ("ici.next.run", 2) | ("lens.result", 1)))
        .then(|| (safe(schema, 128), version))
}
pub(crate) fn text<'a>(document: &'a Value, pointer: &str) -> Result<&'a str> {
    document
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .with_context(|| format!("missing nonempty string {pointer}"))
}
pub(crate) fn summarize(document: &Value, run_id: &str) -> Result<ResultSummary> {
    let schema = text(document, "/schema_id")?;
    let version = document
        .get("schema_version")
        .and_then(Value::as_u64)
        .context("missing schema_version")?;
    let (producer, producer_version, complete, identity, scope) = match (schema, version) {
        ("ici.next.run", 2) => {
            text(document, "/run_id")?;
            for key in ["identity", "scope", "execution", "gate", "publication"] {
                ensure!(
                    document.get(key).is_some_and(Value::is_object),
                    "missing {key} object"
                );
            }
            ensure!(
                document
                    .get("checks")
                    .is_some_and(|value| value.is_null() || value.is_array()),
                "ici v2 requires checks; null means unknown check scope"
            );
            let complete = document
                .pointer("/execution/required_complete")
                .and_then(Value::as_bool);
            (
                "ici",
                text(document, "/producer/ici_version")?,
                complete,
                serde_json::json!({"policy":document.pointer("/identity/policy_digest"),"toolchain":document.pointer("/identity/toolchain_digest"),"fingerprint":document.pointer("/identity/fingerprint_version"),"producer":document.get("producer")}),
                document["scope"].clone(),
            )
        }
        ("lens.result", 1) => {
            ensure!(
                text(document, "/producer/name")? == "lens",
                "unexpected Lens producer"
            );
            ensure!(
                document.get("complete").is_some_and(Value::is_boolean),
                "Lens complete must be boolean"
            );
            for key in ["execution", "source"] {
                ensure!(
                    document.get(key).is_some_and(Value::is_object),
                    "missing {key} object"
                );
            }
            ensure!(
                document
                    .get("run_id")
                    .is_some_and(|value| value.is_null() || value.is_string()),
                "Lens run_id must be string or null"
            );
            if let Some(id) = document.get("run_id").and_then(Value::as_str) {
                ensure!(
                    id == run_id,
                    "Lens run_id differs from owning idk invocation"
                );
            }
            (
                "lens",
                text(document, "/producer/version")?,
                document["complete"].as_bool(),
                serde_json::json!({"producer":document.get("producer"),"source":document.get("source"),"cwd":document.pointer("/execution/cwd")}),
                serde_json::json!({"source":document.get("source"),"domain_schema":document.pointer("/data/schema")}),
            )
        }
        _ => anyhow::bail!("unsupported result schema"),
    };
    ensure!(
        producer_version.len() <= 256,
        "producer version exceeds limit"
    );
    let producer_run_id = document
        .get("run_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    ensure!(
        producer_run_id
            .as_ref()
            .is_none_or(|value| value.len() <= 256),
        "producer run ID exceeds limit"
    );
    let findings = document
        .get("findings")
        .and_then(Value::as_array)
        .context("missing findings array")?;
    for finding in findings {
        text(finding, "/message")?;
        text(finding, "/severity")?;
        // Source locations are optional observations. Keep locationless findings
        // readable; editor review independently validates an actionable position.
    }
    Ok(ResultSummary {
        schema_id: schema.into(),
        schema_version: version,
        producer: producer.into(),
        producer_version: producer_version.into(),
        producer_run_id,
        findings_count: findings.len(),
        complete,
        selected_gate: document
            .pointer("/gate/selected")
            .and_then(Value::as_str)
            .map(|s| safe(s, 64)),
        workspace_gate: document
            .pointer("/gate/workspace")
            .and_then(Value::as_str)
            .map(|s| safe(s, 64)),
        scope_kind: document
            .pointer("/scope/kind")
            .and_then(Value::as_str)
            .map(|s| safe(s, 64)),
        identity_digest: json_digest(&identity),
        scope_digest: json_digest(&scope),
    })
}
