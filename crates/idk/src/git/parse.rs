use super::types::*;
use anyhow::{bail, ensure, Context, Result};

#[derive(Default)]
pub(super) struct Porcelain {
    pub entries: Vec<GitChange>,
    pub upstream: Option<String>,
    pub ahead: Option<u64>,
    pub behind: Option<u64>,
}
/// Porcelain v2 with -z: fixed metadata fields, then arbitrary filename bytes;
/// rename/copy consumes one additional NUL record for the original path.
pub(super) fn status(bytes: &[u8]) -> Result<Porcelain> {
    ensure!(
        bytes.is_empty() || bytes.ends_with(&[0]),
        "incomplete Git status response"
    );
    let mut records = bytes.split(|b| *b == 0);
    let mut parsed = Porcelain::default();
    while let Some(record) = records.next() {
        if record.is_empty() {
            continue;
        }
        if record[0] == b'#' {
            if let Some(value) = record.strip_prefix(b"# branch.upstream ") {
                parsed.upstream = Some(text(value)?);
            }
            if let Some(value) = record.strip_prefix(b"# branch.ab ") {
                let value = text(value)?;
                let mut counts = value.split_whitespace();
                parsed.ahead = Some(
                    counts
                        .next()
                        .and_then(|v| v.strip_prefix('+'))
                        .context("invalid ahead count")?
                        .parse()?,
                );
                parsed.behind = Some(
                    counts
                        .next()
                        .and_then(|v| v.strip_prefix('-'))
                        .context("invalid behind count")?
                        .parse()?,
                );
            }
            continue; // Git explicitly permits unknown optional headers.
        }
        if record.starts_with(b"? ") || record.starts_with(b"! ") {
            parsed.entries.push(GitChange {
                path: GitPath::from_bytes(record[2..].to_vec())?,
                original_path: None,
                index_status: '?',
                worktree_status: '?',
                conflict: false,
                untracked: true,
                submodule: None,
            });
            continue;
        }
        let (fields, conflict, renamed) = match record[0] {
            b'1' => (9, false, false),
            b'2' => (10, false, true),
            b'u' => (11, true, false),
            _ => bail!("unrecognized Git status record"),
        };
        let parts: Vec<_> = record.splitn(fields, |byte| *byte == b' ').collect();
        ensure!(
            parts.len() == fields && parts[1].len() == 2 && parts[2].len() == 4,
            "invalid Git status record"
        );
        let original_path = if renamed {
            Some(GitPath::from_bytes(
                records.next().context("incomplete rename record")?.to_vec(),
            )?)
        } else {
            None
        };
        parsed.entries.push(GitChange {
            path: GitPath::from_bytes(parts[fields - 1].to_vec())?,
            original_path,
            index_status: parts[1][0] as char,
            worktree_status: parts[1][1] as char,
            conflict,
            untracked: false,
            submodule: if parts[2][0] == b'S' {
                Some(text(parts[2])?)
            } else {
                None
            },
        });
    }
    Ok(parsed)
}

/// --raw --no-renames --no-abbrev -z, for an actual index/commit delta.
pub(super) fn raw_diff(bytes: &[u8]) -> Result<Vec<GitChange>> {
    ensure!(
        bytes.is_empty() || bytes.ends_with(&[0]),
        "incomplete Git raw diff"
    );
    let mut records = bytes.split(|b| *b == 0);
    let mut entries = Vec::new();
    while let Some(metadata) = records.next() {
        if metadata.is_empty() {
            continue;
        }
        ensure!(metadata.starts_with(b":"), "invalid Git raw diff header");
        let fields: Vec<_> = metadata[1..].split(|b| *b == b' ').collect();
        ensure!(
            fields.len() == 5 && !fields[4].is_empty(),
            "invalid Git raw diff fields"
        );
        ObjectId::parse(&text(fields[2])?)?;
        ObjectId::parse(&text(fields[3])?)?;
        let status = fields[4][0] as char;
        ensure!(
            !matches!(status, 'R' | 'C'),
            "raw diff unexpectedly enabled renames"
        );
        entries.push(GitChange {
            path: GitPath::from_bytes(records.next().context("raw diff missing path")?.to_vec())?,
            original_path: None,
            index_status: status,
            worktree_status: '.',
            conflict: status == 'U',
            untracked: false,
            submodule: if fields[0] == b"160000" || fields[1] == b"160000" {
                Some("S...".into())
            } else {
                None
            },
        });
    }
    Ok(entries)
}

pub(super) fn history(bytes: &[u8]) -> Result<Vec<CommitSummary>> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    ensure!(bytes.ends_with(&[0]), "incomplete history response");
    let fields: Vec<_> = bytes[..bytes.len() - 1].split(|b| *b == 0).collect();
    ensure!(fields.len() % 5 == 0, "invalid history field count");
    fields
        .chunks_exact(5)
        .map(|row| {
            Ok(CommitSummary {
                oid: ObjectId::parse(&text(row[0])?)?,
                parents: text(row[1])?
                    .split_whitespace()
                    .map(ObjectId::parse)
                    .collect::<Result<_>>()?,
                author: display_bytes(row[2]),
                authored_at: text(row[3])?.parse()?,
                subject: display_bytes(row[4]),
            })
        })
        .collect()
}
