use crate::{
    Container, Engine, Error, Inspection, Profile, Result, RuntimeState, Specification, Task,
    VolumeIdentity, profile::hex,
};
use rustix::fs::{FlockOperation, OFlags, flock};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

/// Local execution evidence only. Neither this enum nor a task's exit code is a
/// repository Validation or permission to publish changes.
pub enum Outcome {
    NotCreated,
    Observed(Inspection),
    Removed,
}
pub struct Controller<'a> {
    engine: &'a Engine,
    spec: Specification,
    journal: Journal,
    assignment: Vec<u8>,
    operation: tokio::sync::Mutex<()>,
}
struct Journal {
    root: PathBuf,
    _lock: File,
}
impl<'a> Controller<'a> {
    /// Trusted control plane only: obtain current metadata authorization BEFORE
    /// calling. This library does not mint a workspace lease or an AgentRun.
    pub fn open(root: &Path, engine: &'a Engine, spec: Specification) -> Result<Self> {
        if !root.is_absolute() {
            return Err(Error::Invalid);
        }
        let parent = root.parent().ok_or(Error::Invalid)?.canonicalize()?;
        if parent.starts_with(&spec.volume().identity().mount)
            || fs::metadata(&parent)?.dev() == spec.volume().identity().device
            || (exists(root)?
                && fs::symlink_metadata(root)?.dev() == spec.volume().identity().device)
        {
            return Err(Error::Mismatch);
        }
        let bytes = intent_bytes(
            engine.identity(),
            spec.attempt(),
            spec.binding(),
            spec.profile(),
            spec.task(),
            spec.volume().identity(),
        )?;
        let fingerprint = hex(&Sha256::digest(&bytes));
        let journal = Journal::open(root, &bytes)?;
        if journal.root.starts_with(&spec.volume().identity().mount)
            || fs::metadata(&journal.root)?.dev() == spec.volume().identity().device
        {
            return Err(Error::Mismatch);
        }
        journal.validate()?;
        let assignment = serde_json::to_vec(
            &json!({"version":1,"attempt":spec.attempt(),"intent_sha256":fingerprint}),
        )?;
        let controller = Self {
            engine,
            spec,
            journal,
            assignment,
            operation: tokio::sync::Mutex::new(()),
        };
        if !controller.journal.exists("removed.json")? {
            controller.claim_volume()?;
        }
        Ok(controller)
    }
    fn volume_record(&self) -> PathBuf {
        self.spec
            .volume()
            .identity()
            .mount
            .join("treeharbor-execution.json")
    }
    fn claim_volume(&self) -> Result<()> {
        self.spec.volume().verify()?;
        let path = self.volume_record();
        if exists(&path)? {
            if read(&path, 4096)? != self.assignment {
                return Err(Error::Mismatch);
            }
        } else {
            write_new(&path, &self.assignment)?;
        }
        Ok(())
    }
    fn release_volume(&self) -> Result<()> {
        self.spec.volume().verify()?;
        let path = self.volume_record();
        if exists(&path)? {
            // A terminal replay must never release another attempt's assignment.
            if read(&path, 4096)? != self.assignment {
                return Ok(());
            }
            fs::remove_file(path)?;
            File::open(&self.spec.volume().identity().mount)?.sync_all()?;
        }
        Ok(())
    }
    fn saved_id(&self) -> Result<Option<String>> {
        if !self.journal.exists("container.json")? {
            return Ok(None);
        }
        let value: Value =
            serde_json::from_slice(&read(&self.journal.root.join("container.json"), 1024)?)?;
        let id = value["id"].as_str().ok_or(Error::Mismatch)?;
        if value != json!({"version":1,"id":id}) {
            return Err(Error::Mismatch);
        }
        Ok(Some(id.into()))
    }
    fn save_id(&self, container: &Container) -> Result<()> {
        let bytes = serde_json::to_vec(&json!({"version":1,"id":container.id()}))?;
        self.journal.record("container.json", &bytes)
    }
    pub async fn status(&self) -> Result<Outcome> {
        let _operation = self.operation.lock().await;
        self.status_inner().await
    }
    async fn status_inner(&self) -> Result<Outcome> {
        self.journal.validate()?;
        if self.journal.exists("removed.json")? {
            self.journal.check_marker("removed.json")?;
            return Ok(Outcome::Removed);
        }
        self.claim_volume()?;
        let found = if let Some(id) = self.saved_id()? {
            self.engine
                .inspect_id(&self.spec, &id)
                .await?
                .ok_or(Error::Unknown)?
        } else {
            match self.engine.discover(&self.spec).await? {
                Some(observed) => {
                    if !self.journal.exists("create-issued.json")? {
                        return Err(Error::Mismatch);
                    }
                    self.save_id(&observed.container)?;
                    observed
                }
                None if self.journal.exists("create-issued.json")? => return Err(Error::Unknown),
                None => return Ok(Outcome::NotCreated),
            }
        };
        Ok(Outcome::Observed(found))
    }
    pub async fn create(&self) -> Result<Inspection> {
        let _operation = self.operation.lock().await;
        self.create_inner().await
    }
    async fn create_inner(&self) -> Result<Inspection> {
        self.journal.validate()?;
        if self.journal.exists("remove-issued.json")? {
            return Err(Error::State);
        }
        match self.status_inner().await? {
            Outcome::Observed(observed) => return Ok(observed),
            Outcome::Removed => return Err(Error::State),
            Outcome::NotCreated => (),
        }
        self.journal.mark("create-issued.json")?;
        let observed = self.engine.create(&self.spec).await?;
        self.save_id(&observed.container)?;
        Ok(observed)
    }
    pub async fn start(&self) -> Result<Inspection> {
        let _operation = self.operation.lock().await;
        let observed = self.create_inner().await?;
        if self.journal.exists("start-issued.json")? {
            self.journal.check_marker("start-issued.json")?;
            return match observed.state {
                RuntimeState::Running | RuntimeState::Exited => Ok(observed),
                _ => Err(Error::Unknown),
            };
        }
        if !observed.never_started() {
            return Err(Error::State);
        }
        self.journal.mark("start-issued.json")?;
        self.engine
            .start_once(&self.spec, &observed.container)
            .await?;
        let actual = self
            .engine
            .inspect(&self.spec, &observed.container)
            .await?
            .ok_or(Error::Unknown)?;
        if !matches!(actual.state, RuntimeState::Running | RuntimeState::Exited) {
            return Err(Error::Unknown);
        }
        Ok(actual)
    }
    pub async fn remove(&self) -> Result<()> {
        let _operation = self.operation.lock().await;
        self.journal.validate()?;
        if self.journal.exists("removed.json")? {
            self.journal.check_marker("removed.json")?;
            return self.release_volume();
        }
        self.claim_volume()?;
        let id = if let Some(id) = self.saved_id()? {
            id
        } else {
            match self.status_inner().await? {
                Outcome::Observed(observed) => observed.container.id().into(),
                // No create was ever issued. Nothing can later materialize.
                Outcome::NotCreated => {
                    self.journal.mark("removed.json")?;
                    return self.release_volume();
                }
                Outcome::Removed => return self.release_volume(),
            }
        };
        self.journal.mark("remove-issued.json")?;
        if let Some(observed) = self.engine.inspect_id(&self.spec, &id).await? {
            self.engine.remove(&self.spec, &observed.container).await?;
        }
        if self.engine.discover(&self.spec).await?.is_some() {
            return Err(Error::Unknown);
        }
        self.journal.mark("removed.json")?;
        self.release_volume()
    }
}
fn intent_bytes(
    engine: &str,
    attempt: uuid::Uuid,
    binding: [u8; 32],
    profile: &Profile,
    task: &Task,
    volume: &VolumeIdentity,
) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(
        &json!({"version":1,"engine":engine,"attempt":attempt,
        "binding":hex(&binding),"profile":profile.definition(),"argv":task.argv(),"volume":volume}),
    )?)
}
impl Journal {
    fn open(root: &Path, intent: &[u8]) -> Result<Self> {
        if !root.is_absolute() || intent.len() > 128 * 1024 {
            return Err(Error::Invalid);
        }
        private_dir(root.parent().ok_or(Error::Invalid)?)?;
        let fresh = match fs::DirBuilder::new().mode(0o700).create(root) {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(e) => return Err(e.into()),
        };
        private_dir(root)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(fresh)
            .mode(0o600)
            .custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK).bits() as i32)
            .open(root.join("task.lock"))?;
        private_file(&lock)?;
        flock(&lock, FlockOperation::NonBlockingLockExclusive).map_err(|_| Error::State)?;
        let journal = Self {
            root: root.canonicalize()?,
            _lock: lock,
        };
        if fresh {
            journal._lock.sync_all()?;
            write_new(&journal.root.join("intent.json"), intent)?;
            File::open(root.parent().ok_or(Error::Invalid)?)?.sync_all()?;
        } else if read(&journal.root.join("intent.json"), 128 * 1024)? != intent {
            return Err(Error::Mismatch);
        }
        Ok(journal)
    }
    fn exists(&self, name: &str) -> Result<bool> {
        exists(&self.root.join(name))
    }
    fn record(&self, name: &str, bytes: &[u8]) -> Result<()> {
        if self.exists(name)? {
            if read(&self.root.join(name), 128 * 1024)? != bytes {
                return Err(Error::Mismatch);
            }
        } else {
            write_new(&self.root.join(name), bytes)?;
        }
        Ok(())
    }
    fn mark(&self, name: &str) -> Result<()> {
        self.record(name, b"{\"version\":1}\n")
    }
    fn check_marker(&self, name: &str) -> Result<()> {
        if read(&self.root.join(name), 64)? != b"{\"version\":1}\n" {
            return Err(Error::Mismatch);
        }
        Ok(())
    }
    fn validate(&self) -> Result<()> {
        let mut flags = [false; 4];
        for (i, name) in [
            "create-issued.json",
            "start-issued.json",
            "remove-issued.json",
            "removed.json",
        ]
        .iter()
        .enumerate()
        {
            flags[i] = self.exists(name)?;
            if flags[i] {
                self.check_marker(name)?;
            }
        }
        let [create, start, remove, removed] = flags;
        let identified = self.exists("container.json")?;
        if (identified && !create)
            || ((start || remove) && !(create && identified))
            || (removed && create && !remove)
        {
            return Err(Error::Mismatch);
        }
        Ok(())
    }
}
fn exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}
fn private_dir(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir()
        || meta.uid() != rustix::process::geteuid().as_raw()
        || meta.mode() & 0o077 != 0
    {
        return Err(Error::Mismatch);
    }
    Ok(())
}
fn private_file(file: &File) -> Result<()> {
    let meta = file.metadata()?;
    if !meta.is_file()
        || meta.uid() != rustix::process::geteuid().as_raw()
        || meta.mode() & 0o077 != 0
        || meta.nlink() != 1
    {
        return Err(Error::Mismatch);
    }
    Ok(())
}
fn read(path: &Path, max: usize) -> Result<Vec<u8>> {
    private_dir(path.parent().ok_or(Error::Invalid)?)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK).bits() as i32)
        .open(path)?;
    private_file(&file)?;
    let mut bytes = Vec::new();
    file.take(max as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > max {
        return Err(Error::Limit);
    }
    Ok(bytes)
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or(Error::Invalid)?;
    private_dir(parent)?;
    let mut file = tempfile::Builder::new()
        .prefix("runtime-write-")
        .tempfile_in(parent)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist_noclobber(path)
        .map_err(|e| Error::Io(e.error))?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn independent_intent_vector_binds_engine_profile_task_and_physical_volume() {
        let profile = Profile::decode(include_bytes!("../tests/fixtures/profile-v1.json")).unwrap();
        let volume = VolumeIdentity {
            mount: "/private/slot".into(),
            files: "/private/slot/files".into(),
            device: 1795,
            mount_inode: 2,
            files_inode: 12,
            mount_id: 42,
            capacity_bytes: 120000000,
            limit_bytes: 134217728,
        };
        let bytes = intent_bytes(
            "engine-1",
            uuid::Uuid::from_u128(1),
            [42; 32],
            &profile,
            &Task::new(vec!["/probe".into(), "once".into()]).unwrap(),
            &volume,
        )
        .unwrap();
        assert_eq!(bytes, include_bytes!("../tests/fixtures/intent-v1.json"));
        assert_eq!(
            hex(&Sha256::digest(&bytes)),
            include_str!("../tests/fixtures/intent-v1.sha256").trim()
        );
    }

    #[test]
    fn journal_reopen_excludes_other_owners_and_preserves_unknown_start() {
        let parent = tempfile::tempdir().unwrap();
        fs::set_permissions(parent.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let path = parent.path().join("attempt");
        let journal = Journal::open(&path, b"exact intent").unwrap();
        assert!(Journal::open(&path, b"exact intent").is_err());
        journal.mark("create-issued.json").unwrap();
        journal.record("container.json", b"recorded id").unwrap();
        journal.mark("start-issued.json").unwrap();
        journal.validate().unwrap();
        drop(journal);
        assert!(Journal::open(&path, b"other intent").is_err());
        let journal = Journal::open(&path, b"exact intent").unwrap();
        journal.validate().unwrap();
        assert!(journal.exists("start-issued.json").unwrap());
        assert!(journal.record("container.json", b"replacement").is_err());
        assert_eq!(
            read(&path.join("container.json"), 64).unwrap(),
            b"recorded id"
        );
    }

    #[test]
    fn corrupt_or_out_of_order_markers_never_authorize_recovery() {
        let parent = tempfile::tempdir().unwrap();
        fs::set_permissions(parent.path(), fs::Permissions::from_mode(0o700)).unwrap();
        for marker in ["start-issued.json", "remove-issued.json"] {
            let path = parent.path().join(marker);
            let journal = Journal::open(&path, b"intent").unwrap();
            journal.mark(marker).unwrap();
            assert!(journal.validate().is_err());
        }
        let path = parent.path().join("corrupt");
        let journal = Journal::open(&path, b"intent").unwrap();
        journal.mark("create-issued.json").unwrap();
        journal.validate().unwrap();
        journal.mark("removed.json").unwrap();
        assert!(journal.validate().is_err());
        fs::remove_file(path.join("removed.json")).unwrap();
        fs::write(path.join("create-issued.json"), b"{\"version\":2}\n").unwrap();
        assert!(journal.validate().is_err());
    }

    #[test]
    fn journal_rejects_substituted_files_and_incomplete_initialization() {
        let parent = tempfile::tempdir().unwrap();
        fs::set_permissions(parent.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let path = parent.path().join("attempt");
        let journal = Journal::open(&path, b"intent").unwrap();
        journal.mark("create-issued.json").unwrap();
        let marker = path.join("create-issued.json");
        fs::hard_link(&marker, path.join("alias")).unwrap();
        assert!(journal.validate().is_err());
        fs::remove_file(&marker).unwrap();
        symlink(path.join("alias"), &marker).unwrap();
        assert!(journal.validate().is_err());
        let incomplete = parent.path().join("incomplete");
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&incomplete)
            .unwrap();
        assert!(Journal::open(&incomplete, b"intent").is_err());
        assert!(!incomplete.join("intent.json").exists());
    }
}
