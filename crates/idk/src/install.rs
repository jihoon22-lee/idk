//! User-owned, offline installation generations. Activation is a journalled
//! symlink transition, not a transaction over live workspace data or processes.
use crate::model::{new_id, PROTOCOL, SCHEMA};
use crate::package::{valid_digest, valid_version, BundleReview, VerifiedBundle, BINARY};
use crate::store::{atomic_write, ensure_private_dir, read_private, FileLock, Store};
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::os::unix::fs::{symlink, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const STATE: &str = "installation.json";
const JOURNAL: &str = "activation.json";
const ARCHIVE: &str = "verified-bundle.tar.gz";
const MAX_GENERATIONS: usize = 128;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Generation {
    pub name: String,
    pub version: String,
    pub bundle_sha256: String,
    pub binary_sha256: String,
}
impl Generation {
    fn from_review(review: &BundleReview) -> Self {
        Self {
            name: review.generation.clone(),
            version: review.manifest.version.clone(),
            bundle_sha256: review.bundle_sha256.clone(),
            binary_sha256: review.manifest.sha256.clone(),
        }
    }
    fn validate(&self) -> Result<()> {
        valid_version(&self.version)?;
        valid_digest(&self.bundle_sha256)?;
        valid_digest(&self.binary_sha256)?;
        ensure!(
            self.name == format!("{}-{}", self.version, self.bundle_sha256),
            "invalid installation generation identity"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InstallationState {
    pub schema: u32,
    pub active: Option<String>,
    pub committed_version_floor: Option<String>,
    pub generations: BTreeMap<String, Generation>,
}
impl Default for InstallationState {
    fn default() -> Self {
        Self {
            schema: 2,
            active: None,
            committed_version_floor: None,
            generations: BTreeMap::new(),
        }
    }
}
impl InstallationState {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.schema == 2 && self.generations.len() <= MAX_GENERATIONS,
            "unsupported or oversized installation state; preserved"
        );
        for (key, generation) in &self.generations {
            generation.validate()?;
            ensure!(
                key == &generation.name,
                "installation generation key differs from identity"
            );
        }
        ensure!(
            self.active
                .as_ref()
                .is_none_or(|name| self.generations.contains_key(name)),
            "active generation is missing from inventory"
        );
        if let Some(floor) = &self.committed_version_floor {
            valid_version(floor)?;
            ensure!(
                self.generations
                    .values()
                    .any(|entry| &entry.version == floor),
                "committed version floor is missing from preserved inventory"
            );
        }
        if let Some(active) = &self.active {
            let floor = self
                .committed_version_floor
                .as_ref()
                .context("active installation has no committed version floor")?;
            ensure!(
                version_tuple(&self.generations[active].version)? <= version_tuple(floor)?,
                "active generation exceeds the committed version floor"
            );
        }
        Ok(())
    }

    fn committed(&self, target: Option<&Generation>) -> Result<Self> {
        let mut state = self.clone();
        state.active = target.map(|target| target.name.clone());
        if let Some(target) = target {
            let raises_floor = match &state.committed_version_floor {
                Some(floor) => version_tuple(&target.version)? > version_tuple(floor)?,
                None => true,
            };
            if raises_floor {
                state.committed_version_floor = Some(target.version.clone());
            }
        }
        state.validate()?;
        Ok(state)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema: u32,
    before: InstallationState,
    target: Option<Generation>,
}

#[derive(Debug, Serialize)]
pub struct InstallResult {
    pub active: Generation,
    pub launcher: PathBuf,
    pub preserved_generations: Vec<PathBuf>,
    pub active_hosts: &'static str,
}

#[derive(Debug, Serialize)]
pub struct RecoveryReview {
    pub pending: bool,
    pub finish_committed_activation: bool,
    pub recovery_active: Option<PathBuf>,
    pub preserved_generations: Vec<PathBuf>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthReport {
    pub version: String,
    pub configuration_schema: u32,
    pub protocol: u32,
    pub state_schemas: BTreeMap<String, u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Checkpoint {
    Staged,
    Journaled,
    Activated,
    Healthy,
    Committed,
}

pub struct Installer {
    root: PathBuf,
}
impl Installer {
    pub fn open(root: &Path) -> Result<Self> {
        ensure!(root.is_absolute(), "installation prefix must be absolute");
        ensure_private_dir(root)?;
        ensure_private_dir(&root.join("generations"))?;
        Ok(Self {
            root: root.to_owned(),
        })
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn review_recovery(&self) -> Result<RecoveryReview> {
        let _lock = self.lock()?;
        let state = self.read_state()?;
        let journal = self.read_journal()?;
        let mut finish = false;
        let active = if let Some(journal) = &journal {
            let committed = journal.before.committed(journal.target.as_ref())?;
            finish = state == committed;
            ensure!(
                finish || state == journal.before,
                "installation state changed outside the recorded activation; preserved"
            );
            if finish {
                committed.active
            } else {
                journal.before.active.clone()
            }
        } else {
            state.active.clone()
        };
        Ok(RecoveryReview {
            pending: journal.is_some(),
            finish_committed_activation: finish,
            recovery_active: active.map(|name| self.generation_path(&name)),
            preserved_generations: state
                .generations
                .keys()
                .map(|name| self.generation_path(name))
                .collect(),
        })
    }
    pub fn status(&self) -> Result<InstallationState> {
        let _lock = self.lock()?;
        ensure!(
            !self.root.join(JOURNAL).try_exists()?,
            "interrupted activation requires `package recover`; existing generations are preserved"
        );
        let state = self.read_state()?;
        self.check_links(&state)?;
        Ok(state)
    }
    pub fn install(&self, bundle: &VerifiedBundle, store: &Store) -> Result<InstallResult> {
        self.install_with(
            bundle,
            |binary| run_health(binary, store, &bundle.review().manifest.version),
            |_| Ok(()),
        )
    }
    fn install_with(
        &self,
        bundle: &VerifiedBundle,
        mut health: impl FnMut(&Path) -> Result<()>,
        mut checkpoint: impl FnMut(Checkpoint) -> Result<()>,
    ) -> Result<InstallResult> {
        let _lock = self.lock()?;
        ensure!(
            !self.root.join(JOURNAL).try_exists()?,
            "interrupted activation requires recovery before another update"
        );
        let mut before = self.read_state()?;
        self.check_links(&before)?;
        let target = Generation::from_review(bundle.review());
        target.validate()?;
        ensure!(
            version_tuple(&target.version)? >= version_tuple(env!("CARGO_PKG_VERSION"))?,
            "downgrade is unsupported; preserve workspace data and use a compatible release"
        );
        if let Some(floor) = &before.committed_version_floor {
            ensure!(
                version_tuple(&target.version)? >= version_tuple(floor)?,
                "downgrade below a previously committed release is unsupported, including after uninstall; workspace data is preserved"
            );
        }
        if let Some(active) = &before.active {
            ensure!(
                version_tuple(&target.version)?
                    >= version_tuple(&before.generations[active].version)?,
                "downgrade is unsupported after activation; workspace data is preserved"
            );
        }
        let directory = self.generation_path(&target.name);
        if let Some(known) = before.generations.get(&target.name) {
            ensure!(
                known == &target,
                "existing generation identity differs from reviewed bundle"
            );
            bundle.verify_staged(&directory)?;
            self.verify_generation(known)?;
        } else {
            ensure!(
                before.generations.len() < MAX_GENERATIONS,
                "installation inventory is full; no generation was automatically removed"
            );
            if !directory.try_exists()? {
                let staging = self.root.join(format!("staging-{}", new_id()));
                let staged = (|| -> Result<()> {
                    bundle.stage(&staging)?;
                    atomic_write(&staging.join(ARCHIVE), bundle.archive_bytes())?;
                    bundle.verify_staged(&staging)?;
                    fs::rename(&staging, &directory)?;
                    fs::File::open(self.root.join("generations"))?.sync_all()?;
                    self.sync_root()
                })();
                if staged.is_err() {
                    let _ = fs::remove_dir_all(&staging);
                }
                staged?;
            }
            // A crash after rename but before inventory commit can be retried
            // only with the exact original sealed bundle; no file is overwritten.
            self.verify_generation(&target)?;
            before
                .generations
                .insert(target.name.clone(), target.clone());
            self.write_state(&before)?;
        }
        checkpoint(Checkpoint::Staged)?;
        health(&directory.join(BINARY))
            .context("staged executable health failed; active installation is unchanged")?;
        if before.active.as_deref() == Some(&target.name) {
            return Ok(self.result(&before, target));
        }
        let journal = Journal {
            schema: 1,
            before: before.clone(),
            target: Some(target.clone()),
        };
        atomic_write(
            &self.root.join(JOURNAL),
            &serde_json::to_vec_pretty(&journal)?,
        )?;
        checkpoint(Checkpoint::Journaled)?;
        self.set_active(Some(&target.name))?;
        self.ensure_launcher()?;
        checkpoint(Checkpoint::Activated)?;
        if let Err(error) = health(&self.root.join("idk")) {
            self.restore(&journal).context("activation health failed and automatic recovery could not finish; run package recover")?;
            return Err(error).context("activation health failed; previous installation restored, workspace data preserved");
        }
        checkpoint(Checkpoint::Healthy)?;
        let after = before.committed(Some(&target))?;
        self.write_state(&after)?;
        checkpoint(Checkpoint::Committed)?;
        self.remove_journal()?;
        Ok(self.result(&after, target))
    }

    /// Recovers only an interrupted activation. It never restores a snapshot of
    /// live state, downgrades a committed release, or stops an old host.
    pub fn recover(&self) -> Result<InstallationState> {
        let _lock = self.lock()?;
        let Some(journal) = self.read_journal()? else {
            let state = self.read_state()?;
            self.check_links(&state)?;
            return Ok(state);
        };
        let observed = self.read_state()?;
        let committed = journal.before.committed(journal.target.as_ref())?;
        if observed == committed {
            if let Some(target) = &journal.target {
                self.verify_generation(target)?;
            }
            self.check_links(&observed)?;
            self.remove_journal()?;
            return Ok(observed);
        }
        ensure!(observed == journal.before, "installation state changed outside the interrupted transaction; preserved for inspection");
        self.restore(&journal)?;
        Ok(journal.before)
    }

    /// Removes only the managed entrypoints. Generation binaries and every
    /// workspace configuration, run, log and live host remain intact.
    pub fn uninstall_entrypoints(&self) -> Result<InstallationState> {
        let _lock = self.lock()?;
        ensure!(
            !self.root.join(JOURNAL).try_exists()?,
            "recover interrupted activation before uninstalling entrypoints"
        );
        let state = self.read_state()?;
        self.check_links(&state)?;
        let journal = Journal {
            schema: 1,
            before: state.clone(),
            target: None,
        };
        atomic_write(
            &self.root.join(JOURNAL),
            &serde_json::to_vec_pretty(&journal)?,
        )?;
        self.remove_managed_link("idk", Some(Path::new("current").join(BINARY).as_path()))?;
        self.set_active(None)?;
        let state = state.committed(None)?;
        self.write_state(&state)?;
        self.remove_journal()?;
        Ok(state)
    }
    pub fn verify(&self, generation: &str) -> Result<Generation> {
        let _lock = self.lock()?;
        let state = self.read_state()?;
        let entry = state
            .generations
            .get(generation)
            .context("generation is not in this installation inventory")?;
        self.verify_generation(entry)?;
        Ok(entry.clone())
    }
    fn result(&self, state: &InstallationState, active: Generation) -> InstallResult {
        InstallResult { active, launcher: self.root.join("idk"),
            preserved_generations: state.generations.keys().map(|name| self.generation_path(name)).collect(),
            active_hosts: "unchanged; use the original generation binary for an existing host with a different build identity" }
    }
    fn lock(&self) -> Result<FileLock> {
        FileLock::acquire(&self.root.join("installation.lock"), true)
    }
    fn generation_path(&self, name: &str) -> PathBuf {
        self.root.join("generations").join(name)
    }
    fn read_state(&self) -> Result<InstallationState> {
        let state = match read_private(&self.root.join(STATE), 1024 * 1024) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).context("invalid installation state; preserved")?
            }
            Err(error) if missing(&error) => InstallationState::default(),
            Err(error) => return Err(error),
        };
        InstallationState::validate(&state)?;
        Ok(state)
    }
    fn write_state(&self, state: &InstallationState) -> Result<()> {
        state.validate()?;
        atomic_write(&self.root.join(STATE), &serde_json::to_vec_pretty(state)?)
    }
    fn read_journal(&self) -> Result<Option<Journal>> {
        let bytes = match read_private(&self.root.join(JOURNAL), 2 * 1024 * 1024) {
            Ok(bytes) => bytes,
            Err(error) if missing(&error) => return Ok(None),
            Err(error) => return Err(error),
        };
        let journal: Journal =
            serde_json::from_slice(&bytes).context("invalid activation journal; preserved")?;
        ensure!(
            journal.schema == 1,
            "unsupported activation journal; preserved"
        );
        journal.before.validate()?;
        if let Some(target) = &journal.target {
            target.validate()?;
            ensure!(
                journal.before.generations.get(&target.name) == Some(target),
                "journal target differs from sealed inventory"
            );
        }
        Ok(Some(journal))
    }
    fn verify_generation(&self, generation: &Generation) -> Result<()> {
        generation.validate()?;
        let path = self.generation_path(&generation.name);
        let bundle = VerifiedBundle::read(&path.join(ARCHIVE), &generation.bundle_sha256)?;
        ensure!(
            Generation::from_review(bundle.review()) == *generation,
            "generation archive differs from inventory"
        );
        bundle.verify_staged(&path)
    }
    fn check_links(&self, state: &InstallationState) -> Result<()> {
        let expected = state
            .active
            .as_ref()
            .map(|name| Path::new("generations").join(name));
        ensure!(self.read_link("current")? == expected, "current entrypoint differs from committed installation; run recovery or inspect without modifying user files");
        let launcher = self.read_link("idk")?;
        ensure!(
            launcher
                == state
                    .active
                    .as_ref()
                    .map(|_| Path::new("current").join(BINARY)),
            "launcher differs from managed installation; existing path is preserved"
        );
        Ok(())
    }
    fn read_link(&self, name: &str) -> Result<Option<PathBuf>> {
        let path = self.root.join(name);
        match fs::symlink_metadata(&path) {
            Ok(meta) => {
                ensure!(
                    meta.file_type().is_symlink() && meta.uid() == unsafe { libc::geteuid() },
                    "installation entrypoint is not an owned symlink; preserved"
                );
                Ok(Some(fs::read_link(path)?))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    fn set_active(&self, name: Option<&str>) -> Result<()> {
        if let Some(target) = name {
            let temporary = self.root.join(format!(".activate-{}", new_id()));
            symlink(Path::new("generations").join(target), &temporary)?;
            if let Err(error) = fs::rename(&temporary, self.root.join("current")) {
                let _ = fs::remove_file(temporary);
                return Err(error).context("activate installation; existing generations preserved");
            }
        } else {
            self.remove_managed_link("current", None)?;
        }
        self.sync_root()
    }
    fn ensure_launcher(&self) -> Result<()> {
        let target = Path::new("current").join(BINARY);
        match self.read_link("idk")? {
            Some(existing) => ensure!(
                existing == target,
                "existing launcher is not managed by this installation"
            ),
            None => symlink(target, self.root.join("idk"))?,
        }
        self.sync_root()
    }
    fn remove_managed_link(&self, name: &str, expected: Option<&Path>) -> Result<()> {
        if let Some(actual) = self.read_link(name)? {
            ensure!(
                expected.is_none_or(|path| path == actual),
                "entrypoint changed; preserved"
            );
            fs::remove_file(self.root.join(name))?;
            self.sync_root()?;
        }
        Ok(())
    }
    fn restore(&self, journal: &Journal) -> Result<()> {
        let current = self.read_link("current")?;
        let old = journal
            .before
            .active
            .as_ref()
            .map(|name| Path::new("generations").join(name));
        let new = journal
            .target
            .as_ref()
            .map(|target| Path::new("generations").join(&target.name));
        ensure!(
            current == old || current == new,
            "activation entrypoint changed outside this transaction; preserved"
        );
        if let Some(previous) = &journal.before.active {
            self.verify_generation(&journal.before.generations[previous])?;
            self.set_active(Some(previous))?;
            self.ensure_launcher()?;
        } else {
            self.remove_managed_link("idk", Some(Path::new("current").join(BINARY).as_path()))?;
            self.set_active(None)?;
        }
        self.write_state(&journal.before)?;
        self.remove_journal()
    }
    fn remove_journal(&self) -> Result<()> {
        fs::remove_file(self.root.join(JOURNAL))?;
        self.sync_root()
    }
    fn sync_root(&self) -> Result<()> {
        fs::File::open(&self.root)?
            .sync_all()
            .context("persist installation directory")
    }
}

fn missing(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}
fn version_tuple(version: &str) -> Result<(u32, u32, u32)> {
    valid_version(version)?;
    let parts: Vec<_> = version
        .split('.')
        .map(str::parse::<u32>)
        .collect::<std::result::Result<_, _>>()?;
    Ok((parts[0], parts[1], parts[2]))
}

/// Read-only health inspection. No host start, initialization, Git, build,
/// migration, network access or state backup/restore is performed.
pub fn health(store: &Store) -> Result<HealthReport> {
    for directory in [&store.config_dir, &store.state_dir, &store.runtime_dir] {
        ensure!(
            directory.is_absolute(),
            "health state paths must be absolute"
        );
        ensure_private_dir(directory)?;
    }
    store.load()?;
    let mut state_schemas = BTreeMap::new();
    for name in ["host-sessions.json", "runs.json"] {
        if let Some(value) = store.read_state::<serde_json::Value>(name)? {
            let schema = value
                .get("schema")
                .and_then(serde_json::Value::as_u64)
                .context("state schema is missing; original preserved")?;
            ensure!(
                schema == 1,
                "unsupported state schema; original preserved without migration"
            );
            state_schemas.insert(name.to_owned(), schema as u32);
        }
    }
    Ok(HealthReport {
        version: env!("CARGO_PKG_VERSION").into(),
        configuration_schema: SCHEMA,
        protocol: PROTOCOL,
        state_schemas,
    })
}

fn run_health(binary: &Path, store: &Store, expected_version: &str) -> Result<()> {
    health(store)?;
    let output_path = store
        .runtime_dir
        .join(format!("package-health-{}.json", new_id()));
    let output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&output_path)?;
    let result = (|| {
        let mut child = Command::new(binary)
            .arg("__package-health")
            .arg("--config-dir")
            .arg(&store.config_dir)
            .arg("--state-dir")
            .arg(&store.state_dir)
            .arg("--runtime-dir")
            .arg(&store.runtime_dir)
            .stdin(Stdio::null())
            .stdout(output)
            .stderr(Stdio::null())
            .spawn()
            .context("execute staged health check; noexec or permission policy is not bypassed")?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    ensure!(status.success(), "staged executable rejected health check");
                    break;
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                outcome => {
                    let _ = child.kill();
                    let _ = child.wait();
                    if let Err(error) = outcome {
                        return Err(error).context("observe health process exit");
                    }
                    bail!("staged executable health check timed out");
                }
            }
        }
        let report: HealthReport = serde_json::from_slice(&read_private(&output_path, 64 * 1024)?)?;
        ensure!(
            report.version == expected_version
                && report.configuration_schema == SCHEMA
                && report.protocol == PROTOCOL,
            "staged version/schema/protocol is incompatible; previous installation preserved"
        );
        Ok(())
    })();
    let _ = fs::remove_file(output_path);
    result
}

#[cfg(test)]
mod tests;
