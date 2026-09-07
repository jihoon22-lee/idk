use crate::model::{Workspace, MAX_MESSAGE};
use anyhow::{bail, Context, Result};
use serde::{de::DeserializeOwned, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Store {
    pub config_dir: PathBuf,
    pub state_dir: PathBuf,
    pub runtime_dir: PathBuf,
}

impl Store {
    pub fn open(data_dir: Option<&Path>) -> Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .context("HOME is not set")?;
        let user_root = data_dir
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("IDK_HOME").map(PathBuf::from));
        let store = if let Some(root) = user_root {
            if !root.is_absolute() {
                bail!("data directory must be absolute");
            }
            Self {
                config_dir: root.join("config"),
                state_dir: root.join("state"),
                runtime_dir: root.join("run"),
            }
        } else {
            let config = std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config"));
            let state = std::env::var_os("XDG_STATE_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".local/state"));
            let runtime = std::env::var_os("XDG_RUNTIME_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    PathBuf::from(format!("/tmp/idk-{}", unsafe { libc::geteuid() }))
                });
            Self {
                config_dir: config.join("idk/v0.4"),
                state_dir: state.join("idk/v0.4"),
                runtime_dir: runtime.join("idk-v04"),
            }
        };
        for path in [&store.config_dir, &store.state_dir, &store.runtime_dir] {
            if !path.is_absolute() {
                bail!("XDG paths must be absolute");
            }
            ensure_private_dir(path)?;
        }
        // sockaddr_un.sun_path is 108 bytes including NUL on supported Linux.
        if store.socket_path().as_os_str().as_encoded_bytes().len() >= 104 {
            bail!("runtime path is too long for a Unix socket; choose a shorter local runtime directory");
        }
        Ok(store)
    }

    pub fn config_path(&self) -> PathBuf {
        self.config_dir.join("workspace.toml")
    }
    pub fn socket_path(&self) -> PathBuf {
        self.runtime_dir.join("host.sock")
    }

    pub fn load(&self) -> Result<Workspace> {
        let path = self.config_path();
        match read_private(&path, MAX_MESSAGE) {
            Ok(bytes) => {
                let text = std::str::from_utf8(&bytes)
                    .context("configuration is not UTF-8 (preserved)")?;
                let workspace: Workspace = toml::from_str(text)
                    .context("invalid configuration (preserved; no reset performed)")?;
                workspace.validate()?;
                Ok(workspace)
            }
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                Ok(Workspace::default())
            }
            Err(error) => Err(error),
        }
    }

    /// Re-read under a lock and compare revisions: a stale UI cannot overwrite a newer definition.
    pub fn save(&self, workspace: &mut Workspace, expected_revision: u64) -> Result<()> {
        let _lock = FileLock::acquire(&self.config_dir.join("workspace.lock"), false)?;
        let previous = self.load()?;
        if previous.revision != expected_revision {
            bail!("configuration changed elsewhere; reload before saving");
        }
        workspace.validate()?;
        workspace.revision = expected_revision
            .checked_add(1)
            .context("configuration revision exhausted")?;
        let encoded = toml::to_string_pretty(workspace)?;
        if encoded.len() > MAX_MESSAGE {
            bail!("configuration exceeds size limit");
        }
        if self.config_path().exists() {
            let old = read_private(&self.config_path(), MAX_MESSAGE)?;
            atomic_write(&self.config_dir.join("workspace.previous.toml"), &old)?;
        }
        atomic_write(&self.config_path(), encoded.as_bytes())
    }

    pub fn update<T>(&self, update: impl FnOnce(&mut Workspace) -> Result<T>) -> Result<T> {
        let mut workspace = self.load()?;
        let revision = workspace.revision;
        let value = update(&mut workspace)?;
        self.save(&mut workspace, revision)?;
        Ok(value)
    }

    pub fn read_state<T: DeserializeOwned>(&self, name: &str) -> Result<Option<T>> {
        validate_filename(name)?;
        let path = self.state_dir.join(name);
        match read_private(&path, MAX_MESSAGE) {
            Ok(bytes) => Ok(Some(
                serde_json::from_slice(&bytes).context("invalid state (preserved)")?,
            )),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    pub fn write_state<T: Serialize>(&self, name: &str, state: &T) -> Result<()> {
        validate_filename(name)?;
        let bytes = serde_json::to_vec_pretty(state)?;
        if bytes.len() > MAX_MESSAGE {
            bail!("state exceeds size limit");
        }
        atomic_write(&self.state_dir.join(name), &bytes)
    }
}

fn validate_filename(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 100
        || name.starts_with('.')
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    {
        bail!("invalid state filename");
    }
    Ok(())
}

pub fn ensure_private_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create data parent {}", parent.display()))?;
    }
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("cannot create private directory {}", path.display()))
        }
    }
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.permissions().mode() & 0o077 != 0
    {
        bail!(
            "{} must be a real directory owned by this user with mode 0700",
            path.display()
        );
    }
    Ok(())
}

pub fn read_private(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.permissions().mode() & 0o077 != 0
    {
        bail!(
            "{} must be a regular file owned by this user with mode 0600",
            path.display()
        );
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        bail!("{} exceeds size limit", path.display());
    }
    Ok(bytes)
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("destination has no parent")?;
    if let Ok(meta) = fs::symlink_metadata(path) {
        if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } {
            bail!("destination must be a regular owned file");
        }
    }
    let pending = parent.join(format!(".pending-{}", crate::model::new_id()));
    let outcome = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&pending)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&pending, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if outcome.is_err() {
        let _ = fs::remove_file(&pending);
    }
    outcome
}

pub struct FileLock(File);

impl FileLock {
    pub fn acquire(path: &Path, nonblocking: bool) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        let meta = file.metadata()?;
        if !meta.is_file()
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.permissions().mode() & 0o077 != 0
        {
            bail!("invalid lock file ownership/permissions");
        }
        let flags = libc::LOCK_EX | if nonblocking { libc::LOCK_NB } else { 0 };
        if unsafe { libc::flock(file.as_raw_fd(), flags) } != 0 {
            return Err(std::io::Error::last_os_error())
                .context("resource is locked or filesystem locking is unavailable");
        }
        Ok(Self(file))
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn future_schema_and_corruption_are_preserved() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(Some(root.path())).unwrap();
        let bytes = b"schema = 99\nrevision = 1\n";
        atomic_write(&store.config_path(), bytes).unwrap();
        assert!(store.load().is_err());
        assert!(store.save(&mut Workspace::default(), 0).is_err());
        assert_eq!(fs::read(store.config_path()).unwrap(), bytes);
        atomic_write(&store.config_path(), b"invalid [").unwrap();
        assert!(store.load().is_err());
    }

    #[test]
    fn stale_write_cannot_overwrite_new_revision() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(Some(root.path())).unwrap();
        let mut first = store.load().unwrap();
        let mut stale = first.clone();
        store.save(&mut first, 0).unwrap();
        assert!(store.save(&mut stale, 0).is_err());
        assert_eq!(store.load().unwrap().revision, 1);
    }

    #[test]
    fn symlink_and_shared_runtime_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(Some(root.path())).unwrap();
        let victim = root.path().join("original");
        fs::write(&victim, "unchanged").unwrap();
        symlink(&victim, store.config_path()).unwrap();
        assert!(store.load().is_err());
        assert!(atomic_write(&store.config_path(), b"overwrite").is_err());
        assert_eq!(fs::read_to_string(&victim).unwrap(), "unchanged");
        fs::set_permissions(&store.runtime_dir, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(Store::open(Some(root.path())).is_err());
    }
}
