//! External results are producer claims, independent of process exit and raw-log diagnostics.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::OpenOptions,
    io::Read,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

pub const MAX_RESULT_BYTES: u64 = 32 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum StructuredResult {
    Missing,
    Invalid {
        error: String,
    },
    Available {
        /// The owning idk invocation, not an assertion about the producer's run identity.
        idk_run_id: String,
        path: PathBuf,
        sha256: String,
        schema_id: String,
        schema_version: u64,
        producer_version: String,
        producer_run_id: Option<String>,
        /// Complete producer document. No field implies independently verified artifact bytes.
        document: Value,
    },
}

/// Called only after process cleanup. Missing, malformed and future schemas remain distinct
/// from an empty successful report and never modify the actual Run exit/state.
pub fn inspect(path: &Path, run_id: &str) -> StructuredResult {
    match read_result(path, run_id) {
        Ok(result) => result,
        Err(error) => StructuredResult::Invalid {
            error: format!("{error:#}")
                .chars()
                .filter(|ch| !ch.is_control())
                .take(512)
                .collect(),
        },
    }
}

pub fn read_result(path: &Path, run_id: &str) -> Result<StructuredResult> {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(StructuredResult::Missing)
        }
        Err(error) => return Err(error).context("open structured result"),
    };
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file(),
        "structured result must be a regular file"
    );
    ensure!(
        metadata.len() <= MAX_RESULT_BYTES,
        "structured result exceeds 32 KiB limit"
    );
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_RESULT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_RESULT_BYTES,
        "structured result exceeds 32 KiB limit"
    );
    let document: Value =
        serde_json::from_slice(&bytes).context("malformed structured result JSON")?;
    let schema = text(&document, "/schema_id")?;
    let version = document
        .get("schema_version")
        .and_then(Value::as_u64)
        .context("missing numeric schema_version")?;
    let producer_version = match (schema, version) {
        ("ici.next.run", 2) => {
            text(&document, "/run_id")?;
            for key in ["identity", "scope", "execution", "gate", "publication"] {
                ensure!(
                    document.get(key).is_some_and(Value::is_object),
                    "missing {key} object"
                );
            }
            ensure!(
                document
                    .get("checks")
                    .is_some_and(|v| v.is_null() || v.is_array()),
                "ici v2 requires checks (null means unknown scope)"
            );
            text(&document, "/producer/ici_version")?
        }
        ("lens.result", 1) => {
            ensure!(
                text(&document, "/producer/name")? == "lens",
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
                    .is_some_and(|v| v.is_null() || v.is_string()),
                "Lens run_id must be string or null"
            );
            if let Some(id) = document.get("run_id").and_then(Value::as_str) {
                ensure!(
                    id == run_id,
                    "Lens run_id differs from owning idk invocation"
                );
            }
            text(&document, "/producer/version")?
        }
        _ => anyhow::bail!("unsupported structured result schema {schema} v{version}"),
    }
    .to_owned();
    let findings = document
        .get("findings")
        .and_then(Value::as_array)
        .context("missing findings array")?;
    for finding in findings {
        text(finding, "/message")?;
        text(finding, "/severity")?;
        if schema == "ici.next.run" {
            text(finding, "/primary_location/path")?;
            ensure!(
                finding
                    .pointer("/primary_location/start_line")
                    .and_then(Value::as_u64)
                    .is_some_and(|n| n > 0),
                "invalid finding source line"
            );
        }
    }
    let result = StructuredResult::Available {
        idk_run_id: run_id.into(),
        path: path.into(),
        sha256: format!("{:x}", Sha256::digest(&bytes)),
        schema_id: schema.into(),
        schema_version: version,
        producer_version,
        producer_run_id: document
            .get("run_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        document,
    };
    // Persistence and IPC pretty-print nested values. Charge room for enclosing
    // indentation as well as the canonical envelope, not merely input bytes.
    let encoded = serde_json::to_vec_pretty(&result)?;
    let charged = encoded.len() + 32 * encoded.iter().filter(|byte| **byte == b'\n').count();
    ensure!(
        charged as u64 <= MAX_RESULT_BYTES,
        "structured result exceeds retained JSON budget"
    );
    Ok(result)
}
fn text<'a>(document: &'a Value, pointer: &str) -> Result<&'a str> {
    document
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .with_context(|| format!("missing nonempty string {pointer}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[test]
    fn missing_future_and_malformed_are_never_empty_success() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("result.json");
        assert!(matches!(inspect(&path, "run"), StructuredResult::Missing));
        for json in [
            r#"{"schema_id":"ici.next.run","schema_version":99}"#,
            "{",
            "{}",
        ] {
            fs::write(&path, json).unwrap();
            assert!(matches!(
                inspect(&path, "run"),
                StructuredResult::Invalid { .. }
            ));
        }
        std::os::unix::fs::symlink("/dev/zero", dir.path().join("link")).unwrap();
        assert!(matches!(
            inspect(&dir.path().join("link"), "run"),
            StructuredResult::Invalid { .. }
        ));
    }
    #[test]
    fn compact_input_cannot_expand_beyond_the_durable_ledger_budget() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("result.json");
        let mut extra = serde_json::json!(vec![0; 1000]);
        for _ in 0..60 {
            extra = serde_json::json!([extra]);
        }
        let payload = serde_json::json!({"schema_id":"lens.result","schema_version":1,
            "producer":{"name":"lens","version":"0.10.0"},"run_id":"run",
            "complete":false,"execution":{},"source":{},"findings":[],"extra":extra});
        let bytes = serde_json::to_vec(&payload).unwrap();
        assert!(bytes.len() < MAX_RESULT_BYTES as usize);
        fs::write(&path, bytes).unwrap();
        assert!(matches!(
            inspect(&path, "run"),
            StructuredResult::Invalid { .. }
        ));
    }
    #[test]
    fn lens_keeps_incomplete_and_producer_exit_separate_and_rejects_other_run() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("result.json");
        let payload = serde_json::json!({"schema_id":"lens.result","schema_version":1,
            "producer":{"name":"lens","version":"0.10.0"},"run_id":"run",
            "complete":false,"execution":{"exit_code":0},"source":{"kind":"command"},
            "findings":[{"severity":"warn","code":"partial","message":"incomplete"}]});
        fs::write(&path, serde_json::to_vec(&payload).unwrap()).unwrap();
        let StructuredResult::Available {
            document, sha256, ..
        } = inspect(&path, "run")
        else {
            panic!("valid result rejected")
        };
        assert_eq!(document, payload);
        assert_eq!(sha256.len(), 64);
        assert!(matches!(
            inspect(&path, "other-run"),
            StructuredResult::Invalid { .. }
        ));
    }
}
