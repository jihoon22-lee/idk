//! Verify an explicitly supplied offline bundle before writing its contents.
//! An expected digest proves agreement with the caller's trusted source; it is
//! not a publisher signature or permission to run arbitrary install scripts.
use anyhow::{ensure, Context, Result};
use flate2::bufread::GzDecoder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{Cursor, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

pub const BINARY: &str = "idk-linux-x86_64";
pub const MANIFEST: &str = "idk-linux-x86_64.manifest.json";
pub const CHECKSUMS: &str = "idk-native-SHA256SUMS";
pub const INVENTORY: &str = "idk-third-party-licenses.json";
pub const NOTICES: &str = "idk-THIRD-PARTY-NOTICES.txt";
const FILES: [&str; 5] = [BINARY, MANIFEST, CHECKSUMS, INVENTORY, NOTICES];
const MAX_COMPRESSED: u64 = 64 * 1024 * 1024;
const MAX_UNPACKED: u64 = 128 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceIdentity {
    pub sha: String,
    pub dirty: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ElfIdentity {
    pub interpreter: Option<String>,
    pub needed: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleManifest {
    pub schema_version: u32,
    pub artifact: String,
    pub version: String,
    pub target: String,
    pub toolchain: String,
    pub source: SourceIdentity,
    pub candidate_kind: String,
    pub main_sha_verified: bool,
    pub input_tree_sha256: String,
    pub cargo_lock_sha256: String,
    pub size: u64,
    pub sha256: String,
    pub elf: ElfIdentity,
    pub license_inventory: String,
    pub notices: String,
    pub distribution_notices_complete: bool,
    // Schema 1 allows additional descriptive provenance. Only the explicit
    // fields above control package verification and executable selection.
    #[serde(flatten)]
    pub provenance: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BundleReview {
    pub bundle_sha256: String,
    pub generation: String,
    pub manifest: BundleManifest,
    pub files: Vec<String>,
    pub unpacked_bytes: u64,
}

/// Constructible only from verified immutable input. No source path is reopened
/// when staging, so replacing the supplied archive after review changes nothing.
pub struct VerifiedBundle {
    review: BundleReview,
    files: BTreeMap<String, Vec<u8>>,
    compressed: Vec<u8>,
}

impl VerifiedBundle {
    pub fn read(path: &Path, expected_sha256: &str) -> Result<Self> {
        ensure!(path.is_absolute(), "bundle path must be absolute");
        valid_digest(expected_sha256)?;
        let mut input = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
            .context("open supplied bundle")?;
        let before = input.metadata()?;
        ensure!(
            before.is_file() && before.len() <= MAX_COMPRESSED,
            "bundle must be a regular file of at most 64 MiB"
        );
        let mut compressed = Vec::with_capacity(before.len() as usize);
        (&mut input)
            .take(MAX_COMPRESSED + 1)
            .read_to_end(&mut compressed)?;
        let after = input.metadata()?;
        ensure!(
            compressed.len() as u64 <= MAX_COMPRESSED
                && same_file(&before, &after)
                && compressed.len() as u64 == before.len(),
            "bundle changed while being read"
        );
        Self::from_bytes(&compressed, expected_sha256)
    }

    pub fn from_bytes(compressed: &[u8], expected_sha256: &str) -> Result<Self> {
        valid_digest(expected_sha256)?;
        ensure!(
            compressed.len() as u64 <= MAX_COMPRESSED,
            "compressed bundle exceeds 64 MiB"
        );
        let digest = sha256(compressed);
        ensure!(
            digest == expected_sha256,
            "bundle SHA-256 differs from the supplied trusted digest"
        );
        let decoder = GzDecoder::new(Cursor::new(compressed));
        let mut archive = tar::Archive::new(decoder.take(MAX_UNPACKED + 1));
        let mut files = BTreeMap::new();
        let mut unpacked_bytes = 0u64;
        // Raw mode exposes and rejects PAX/GNU extension records rather than
        // silently applying a second, hidden pathname or size declaration.
        for entry in archive.entries()?.raw(true) {
            let mut entry = entry.context("read bundle archive header")?;
            ensure!(
                files.len() < FILES.len(),
                "bundle has extra or duplicate archive entries"
            );
            ensure!(
                entry.header().as_ustar().is_some() && entry.header().entry_type().is_file(),
                "bundle accepts only regular USTAR files; links and special entries are forbidden"
            );
            let path = entry.path_bytes();
            let name = std::str::from_utf8(&path)
                .context("bundle filename is not UTF-8")?
                .to_owned();
            ensure!(
                FILES.contains(&name.as_str()) && !files.contains_key(&name),
                "unexpected or duplicate bundle filename"
            );
            let expected_mode = if name == BINARY { 0o755 } else { 0o644 };
            ensure!(
                entry.header().mode()? == expected_mode
                    && entry.header().uid()? == 0
                    && entry.header().gid()? == 0,
                "bundle file permissions or owner metadata are not normalized"
            );
            let length = entry.size();
            let limit = match name.as_str() {
                MANIFEST | CHECKSUMS => 1024 * 1024,
                INVENTORY => 16 * 1024 * 1024,
                _ => 64 * 1024 * 1024,
            };
            ensure!(length <= limit, "bundle member exceeds its size limit");
            unpacked_bytes = unpacked_bytes
                .checked_add(length)
                .context("bundle size overflow")?;
            ensure!(
                unpacked_bytes <= MAX_UNPACKED,
                "unpacked bundle exceeds 128 MiB"
            );
            let mut bytes = Vec::with_capacity(length as usize);
            entry
                .read_to_end(&mut bytes)
                .context("read complete bundle member")?;
            ensure!(bytes.len() as u64 == length, "bundle member is truncated");
            files.insert(name, bytes);
        }
        ensure!(
            files.len() == FILES.len(),
            "bundle is missing required files"
        );
        let mut limited = archive.into_inner();
        let mut padding = [0u8; 8192];
        loop {
            let count = limited
                .read(&mut padding)
                .context("verify gzip trailer and archive padding")?;
            if count == 0 {
                break;
            }
            ensure!(
                padding[..count].iter().all(|byte| *byte == 0),
                "bundle contains hidden data after the TAR end marker"
            );
        }
        ensure!(limited.limit() > 0, "expanded archive exceeds 128 MiB");
        let consumed = limited.into_inner().into_inner().position();
        ensure!(
            consumed == compressed.len() as u64,
            "bundle has trailing bytes or additional gzip members"
        );
        verify_checksums(&files)?;
        let manifest: BundleManifest = serde_json::from_slice(&files[MANIFEST])
            .context("invalid bundle manifest (preserved)")?;
        validate_manifest(&manifest, &files)?;
        verify_static_elf(&files[BINARY])?;
        Ok(Self {
            review: BundleReview {
                bundle_sha256: digest.clone(),
                generation: format!("{}-{digest}", manifest.version),
                manifest,
                files: files.keys().cloned().collect(),
                unpacked_bytes,
            },
            files,
            compressed: compressed.to_vec(),
        })
    }

    pub fn review(&self) -> &BundleReview {
        &self.review
    }

    pub(crate) fn archive_bytes(&self) -> &[u8] {
        &self.compressed
    }

    pub fn verify_staged(&self, directory: &Path) -> Result<()> {
        crate::store::ensure_private_dir(directory)?;
        for (name, expected) in &self.files {
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(directory.join(name))?;
            let meta = file.metadata()?;
            ensure!(
                meta.is_file()
                    && meta.uid() == unsafe { libc::geteuid() }
                    && meta.permissions().mode() & 0o022 == 0
                    && meta.len() == expected.len() as u64,
                "installed component ownership, permissions or size changed"
            );
            let mut bytes = Vec::with_capacity(expected.len());
            file.take(expected.len() as u64 + 1)
                .read_to_end(&mut bytes)?;
            ensure!(
                bytes == *expected,
                "installed component differs from its verified bundle"
            );
        }
        Ok(())
    }

    /// Write only a new private staging directory. Existing directories and
    /// user files are never reused or overwritten, and no executable is run.
    pub fn stage(&self, directory: &Path) -> Result<()> {
        ensure!(
            directory.is_absolute(),
            "staging directory must be absolute"
        );
        let parent = directory
            .parent()
            .context("staging directory needs a parent")?;
        crate::store::ensure_owned_directory(parent, false)?;
        DirBuilder::new()
            .mode(0o700)
            .create(directory)
            .context("create a new staging directory; existing paths are preserved")?;
        let mut cleanup = StageCleanup {
            path: directory,
            committed: false,
        };
        for (name, bytes) in &self.files {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(directory.join(name))?;
            file.write_all(bytes)?;
            file.set_permissions(fs::Permissions::from_mode(if name == BINARY {
                0o755
            } else {
                0o644
            }))?;
            file.sync_all()?;
        }
        fs::File::open(directory)?.sync_all()?;
        fs::File::open(parent)?.sync_all()?;
        cleanup.committed = true;
        Ok(())
    }
}

struct StageCleanup<'a> {
    path: &'a Path,
    committed: bool,
}
impl Drop for StageCleanup<'_> {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_dir_all(self.path);
        }
    }
}

fn validate_manifest(manifest: &BundleManifest, files: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    ensure!(
        manifest.schema_version == 1,
        "unsupported bundle manifest schema; no files installed"
    );
    ensure!(
        manifest.artifact == BINARY && manifest.target == "x86_64-unknown-linux-musl",
        "bundle target is not supported by this Linux x86_64 installer"
    );
    valid_version(&manifest.version)?;
    valid_version(&manifest.toolchain)?;
    ensure!(
        matches!(manifest.source.sha.len(), 40 | 64)
            && manifest
                .source
                .sha
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit()),
        "invalid source commit identity"
    );
    for digest in [
        &manifest.sha256,
        &manifest.input_tree_sha256,
        &manifest.cargo_lock_sha256,
    ] {
        valid_digest(digest)?;
    }
    ensure!(
        manifest.size == files[BINARY].len() as u64 && manifest.sha256 == sha256(&files[BINARY]),
        "binary bytes differ from the manifest"
    );
    ensure!(
        manifest.elf.interpreter.is_none() && manifest.elf.needed.is_empty(),
        "manifest does not describe a static executable"
    );
    ensure!(
        manifest.license_inventory == INVENTORY
            && manifest.notices == NOTICES
            && manifest.distribution_notices_complete,
        "bundle distribution notices are incomplete"
    );
    ensure!(!files[NOTICES].is_empty(), "bundle notices are empty");
    verify_inventory(manifest, &files[INVENTORY], &files[NOTICES])?;
    ensure!(
        matches!(
            manifest.candidate_kind.as_str(),
            "dirty-development" | "clean-source" | "exact-main"
        ),
        "unrecognized candidate provenance kind"
    );
    let expected_kind = if manifest.main_sha_verified {
        "exact-main"
    } else if manifest.source.dirty {
        "dirty-development"
    } else {
        "clean-source"
    };
    ensure!(
        manifest.candidate_kind == expected_kind
            && !(manifest.main_sha_verified && manifest.source.dirty),
        "manifest main verification contradicts its source provenance"
    );
    ensure!(
        !manifest.source.dirty || manifest.candidate_kind == "dirty-development",
        "dirty source provenance is mislabeled"
    );
    Ok(())
}

fn verify_inventory(
    manifest: &BundleManifest,
    inventory_bytes: &[u8],
    notices: &[u8],
) -> Result<()> {
    let inventory: serde_json::Value =
        serde_json::from_slice(inventory_bytes).context("invalid dependency notice inventory")?;
    ensure!(
        inventory["schema_version"] == 1 && inventory["notices_complete"] == true,
        "unsupported or incomplete notice inventory"
    );
    ensure!(
        inventory["target"] == manifest.target
            && inventory["toolchain"] == manifest.toolchain
            && inventory["cargo_lock_sha256"] == manifest.cargo_lock_sha256,
        "notice inventory belongs to different build inputs"
    );
    ensure!(
        inventory["notices_sha256"] == sha256(notices),
        "notice text differs from inventory"
    );
    let policy = inventory["runtime_notice_policy_sha256"]
        .as_str()
        .context("missing runtime notice policy identity")?;
    valid_digest(policy)?;
    ensure!(
        manifest
            .provenance
            .get("runtime_notice_policy_sha256")
            .and_then(serde_json::Value::as_str)
            == Some(policy),
        "runtime notice policy differs from manifest"
    );
    let packages = inventory["packages"]
        .as_array()
        .context("missing package notice inventory")?;
    let runtime = inventory["runtime_components"]
        .as_array()
        .context("missing static runtime notices")?;
    ensure!(
        !packages.is_empty() && packages.len() <= 4096 && runtime.len() == 5,
        "notice component inventory is incomplete or oversized"
    );
    let actual: std::collections::BTreeSet<_> = runtime
        .iter()
        .map(|component| component["id"].as_str().unwrap_or(""))
        .collect();
    let expected = std::collections::BTreeSet::from([
        "rust-standard-library",
        "compiler-builtins",
        "musl",
        "llvm-libunwind",
        "compiler-rt-crt",
    ]);
    ensure!(
        actual == expected,
        "static runtime notice component is missing"
    );
    let mut extents = Vec::new();
    for component in packages.iter().chain(runtime) {
        let documents = component["documents"]
            .as_array()
            .context("missing full notice documents")?;
        ensure!(
            !documents.is_empty() && documents.len() <= 1024,
            "notice document list is empty or oversized"
        );
        for document in documents {
            let start = usize::try_from(
                document["notice_offset"]
                    .as_u64()
                    .context("invalid notice offset")?,
            )?;
            let size = usize::try_from(
                document["notice_size"]
                    .as_u64()
                    .context("invalid notice size")?,
            )?;
            let end = start.checked_add(size).context("notice extent overflow")?;
            ensure!(
                size > 0 && end <= notices.len(),
                "notice document exceeds body"
            );
            valid_digest(
                document["source_sha256"]
                    .as_str()
                    .context("invalid notice source identity")?,
            )?;
            ensure!(
                document["source_size"]
                    .as_u64()
                    .is_some_and(|size| size > 0),
                "notice source is empty"
            );
            ensure!(
                document["notice_sha256"] == sha256(&notices[start..end]),
                "notice document text differs from inventory"
            );
            extents.push((start, end));
            ensure!(extents.len() <= 16384, "too many notice documents");
        }
    }
    extents.sort_unstable();
    ensure!(
        extents.windows(2).all(|pair| pair[0].1 <= pair[1].0),
        "notice document extents overlap"
    );
    Ok(())
}

fn verify_checksums(files: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    let text = std::str::from_utf8(&files[CHECKSUMS]).context("checksum file is not UTF-8")?;
    let mut observed = BTreeMap::new();
    for line in text.lines() {
        let (digest, name) = line.split_once("  ").context("invalid checksum line")?;
        valid_digest(digest)?;
        ensure!(
            name != CHECKSUMS
                && files.contains_key(name)
                && observed.insert(name, digest).is_none(),
            "unexpected or duplicate checksum target"
        );
        ensure!(
            sha256(&files[name]) == digest,
            "bundle member checksum mismatch"
        );
    }
    ensure!(
        observed.len() == FILES.len() - 1,
        "checksum file does not cover every bundle member"
    );
    Ok(())
}

pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn valid_digest(value: &str) -> Result<()> {
    ensure!(
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "SHA-256 must contain 64 lowercase hexadecimal digits"
    );
    Ok(())
}
pub fn valid_version(value: &str) -> Result<()> {
    let pieces: Vec<_> = value.split('.').collect();
    ensure!(
        pieces.len() == 3
            && pieces.iter().all(|piece| piece
                .parse::<u32>()
                .is_ok_and(|number| number.to_string() == *piece)),
        "version must use three canonical numeric components"
    );
    Ok(())
}
fn same_file(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    before.dev() == after.dev()
        && before.ino() == after.ino()
        && before.len() == after.len()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
        && before.ctime() == after.ctime()
        && before.ctime_nsec() == after.ctime_nsec()
}

fn verify_static_elf(bytes: &[u8]) -> Result<()> {
    ensure!(
        bytes.len() >= 64 && &bytes[..4] == b"\x7fELF" && bytes[4] == 2 && bytes[5] == 1,
        "binary is not a little-endian ELF64 executable"
    );
    let u16_at = |offset| u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
    ensure!(
        matches!(u16_at(16), 2 | 3) && u16_at(18) == 62 && u16_at(52) == 64,
        "ELF target is not Linux x86_64"
    );
    let u64_at = |slice: &[u8], offset: usize| {
        u64::from_le_bytes(slice[offset..offset + 8].try_into().unwrap())
    };
    let start = usize::try_from(u64_at(bytes, 32)).context("invalid ELF program table")?;
    let size = usize::from(u16_at(54));
    let count = usize::from(u16_at(56));
    ensure!(
        size == 56 && count > 0 && count <= 1024,
        "unsupported ELF program table"
    );
    let end = start
        .checked_add(
            size.checked_mul(count)
                .context("ELF program table overflow")?,
        )
        .context("ELF program table overflow")?;
    ensure!(end <= bytes.len(), "ELF program table is truncated");
    for header in bytes[start..end].chunks_exact(size) {
        let kind = u32::from_le_bytes(header[..4].try_into().unwrap());
        ensure!(kind != 3, "binary contains a dynamic interpreter");
        if kind == 2 {
            let offset =
                usize::try_from(u64_at(header, 8)).context("invalid ELF dynamic segment")?;
            let length =
                usize::try_from(u64_at(header, 32)).context("invalid ELF dynamic segment")?;
            let end = offset
                .checked_add(length)
                .context("ELF dynamic segment overflow")?;
            ensure!(
                end <= bytes.len() && length % 16 == 0,
                "ELF dynamic segment is truncated"
            );
            for entry in bytes[offset..end].chunks_exact(16) {
                let tag = u64_at(entry, 0);
                if tag == 0 {
                    break;
                }
                ensure!(tag != 1, "binary requires a shared library");
            }
        }
    }
    Ok(())
}
