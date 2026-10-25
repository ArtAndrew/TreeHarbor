use super::*;
const MAX_REPORTS: usize = 1024;
// A PR acknowledgment contains old/new text plus the observation. JSON may
// encode each allowed control byte as six bytes; bound the encoded record.
const MAX_RECORD_BYTES: usize = 256 * 1024;
pub(in crate::github) struct Journal {
    pub root: PathBuf,
    _lock: File,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::github) struct Record<R = PublicationReport> {
    version: u8,
    pub key: String,
    pub report: R,
    // Not a secret; retained even if reporting to the service is interrupted.
    token_expires_at: Option<String>,
}
impl<R> Record<R> {
    pub fn new(report: R, token_expires_at: Option<String>) -> Self {
        Self {
            version: 1,
            key: format!("github-publish-report-{}", Uuid::new_v4()),
            report,
            token_expires_at,
        }
    }
}
fn read<T: DeserializeOwned>(path: &Path) -> Result<T> {
    serde_json::from_slice(&files::read_private(path, MAX_RECORD_BYTES)?)
        .map_err(|_| Error::PublicationState)
}
impl Journal {
    pub fn read_optional<T: DeserializeOwned>(&self, name: &str) -> Result<Option<T>> {
        let path = self.root.join(name);
        if path.try_exists()? {
            Ok(Some(read(&path)?))
        } else {
            Ok(None)
        }
    }
    pub fn open(path: &Path, intent: &Value) -> Result<Self> {
        let root = files::private_directory(path)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(
                (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32,
            )
            .open(root.join("publish.lock"))?;
        files::private_file(&lock)?;
        rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive)
            .map_err(|_| Error::AlreadyRunning)?;
        File::open(&root)?.sync_all()?;
        let j = Self { root, _lock: lock };
        j.bind("intent.json", intent)?;
        Ok(j)
    }
    pub fn bind(&self, name: &str, value: &Value) -> Result<()> {
        let path = self.root.join(name);
        let bytes = serde_json::to_vec(value).map_err(|_| Error::PublicationState)?;
        if bytes.len() > MAX_RECORD_BYTES {
            return Err(Error::Size);
        }
        match files::write_private(&path, &bytes) {
            Ok(()) => {}
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        if read::<Value>(&path)? != *value {
            return Err(Error::PublicationState);
        }
        Ok(())
    }
    pub fn attempt(&self) -> Result<Uuid> {
        let path = self.root.join("attempt.json");
        if !path.try_exists()? {
            self.bind(
                "attempt.json",
                &json!({"version":1,"attempt":Uuid::new_v4().to_string()}),
            )?;
        }
        let v: Value = read(&path)?;
        if v["version"] != 1 {
            return Err(Error::PublicationState);
        }
        uuid(&v["attempt"])
    }
    fn count(&self) -> Result<usize> {
        let mut ids = std::collections::BTreeSet::new();
        for entry in std::fs::read_dir(&self.root)? {
            let name = entry?.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with("report-") {
                let Some(number) = name
                    .strip_prefix("report-")
                    .and_then(|v| v.strip_suffix(".json"))
                else {
                    return Err(Error::PublicationState);
                };
                let i = number
                    .parse::<usize>()
                    .map_err(|_| Error::PublicationState)?;
                if i >= MAX_REPORTS || name != format!("report-{i:04}.json") {
                    return Err(Error::PublicationState);
                }
                ids.insert(i);
            }
        }
        if ids.iter().copied().ne(0..ids.len()) {
            return Err(Error::PublicationState);
        }
        Ok(ids.len())
    }
    pub fn pending<R: DeserializeOwned>(&self) -> Result<Option<(usize, Record<R>)>> {
        let count = self.count()?;
        for i in 0..count {
            if !self.root.join(format!("ack-{i:04}.json")).try_exists()? {
                if i + 1 != count {
                    return Err(Error::PublicationState);
                }
                let r: Record<R> = read(&self.root.join(format!("report-{i:04}.json")))?;
                if r.version != 1
                    || !r.key.starts_with("github-publish-report-")
                    || r.key.len() > 128
                {
                    return Err(Error::PublicationState);
                }
                return Ok(Some((i, r)));
            }
            let _: Value = read(&self.root.join(format!("ack-{i:04}.json")))?;
        }
        Ok(None)
    }
    pub fn append<R: Serialize>(&self, record: &Record<R>) -> Result<()> {
        if self.pending::<Value>()?.is_some() {
            return Err(Error::PublicationState);
        }
        let i = self.count()?;
        if i >= MAX_REPORTS {
            return Err(Error::Size);
        }
        self.bind(&format!("report-{i:04}.json"), &json!(record))
    }
    pub fn ack(&self, i: usize, value: &Value) -> Result<()> {
        self.bind(&format!("ack-{i:04}.json"), value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };
    fn report() -> Record {
        Record::new(
            PublicationReport {
                attempt: Uuid::new_v4().to_string(),
                branch_id: Uuid::new_v4().to_string(),
                binding: Uuid::new_v4().to_string(),
                native_revision: format!("native:sha256:{}", "11".repeat(32)),
                prepared_git: format!("git:sha1:{}", "22".repeat(20)),
                remote_revision: None,
                outcome: PublicationOutcome::Unknown,
                observed_at_unix_ms: 1,
                token_revoked: false,
            },
            None,
        )
    }
    #[test]
    fn journal_serializes_attempts_retains_unacknowledged_reports_and_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let identity = json!({"publication":Uuid::new_v4().to_string()});
        let j = Journal::open(dir.path(), &identity).unwrap();
        assert!(matches!(
            Journal::open(dir.path(), &identity),
            Err(Error::AlreadyRunning)
        ));
        let attempt = j.attempt().unwrap();
        let record = report();
        j.append(&record).unwrap();
        assert!(j.append(&report()).is_err());
        drop(j);
        let j = Journal::open(dir.path(), &identity).unwrap();
        assert_eq!(j.attempt().unwrap(), attempt);
        let (index, pending) = j.pending::<PublicationReport>().unwrap().unwrap();
        assert_eq!(json!(pending), json!(record));
        assert!(!pending.report.token_revoked);
        j.ack(index, &json!({"state":"uncertain"})).unwrap();
        assert!(j.pending::<PublicationReport>().unwrap().is_none());
        assert!(j.ack(index, &json!({"state":"observed"})).is_err());
        j.append(&report()).unwrap();
        assert_eq!(j.pending::<PublicationReport>().unwrap().unwrap().0, 1);
        drop(j);
        assert!(Journal::open(dir.path(), &json!({"publication":"other"})).is_err());
        assert_eq!(
            read::<Value>(&dir.path().join("intent.json")).unwrap(),
            identity
        );
    }
    #[test]
    fn journal_rejects_links_holes_and_insecure_state() {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(Journal::open(dir.path(), &json!({})).is_err());
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let target = dir.path().join("target");
        files::write_private(&target, b"{}").unwrap();
        symlink(&target, dir.path().join("intent.json")).unwrap();
        assert!(Journal::open(dir.path(), &json!({})).is_err());
        fs::remove_file(dir.path().join("intent.json")).unwrap();
        let j = Journal::open(dir.path(), &json!({})).unwrap();
        j.bind("report-0001.json", &json!(report())).unwrap();
        assert!(j.pending::<PublicationReport>().is_err());
        assert!(j.append(&report()).is_err());
    }

    #[test]
    fn pull_text_with_json_expansion_survives_restart_and_oversized_records_are_not_written() {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let text = "\u{1}".repeat(9000);
        let intent = json!({"target":{"body":text,"previous":{"body":text}}});
        let observed = json!({"intent":intent,"observation":{"body":text}});
        let record = Record::new(json!({"body":text}), None);
        let j = Journal::open(dir.path(), &intent).unwrap();
        j.append(&record).unwrap();
        j.ack(0, &observed).unwrap();
        drop(j);
        let j = Journal::open(dir.path(), &intent).unwrap();
        assert!(j.pending::<Value>().unwrap().is_none());
        assert_eq!(
            read::<Value>(&dir.path().join("ack-0000.json")).unwrap(),
            observed
        );
        assert!(matches!(
            j.bind("oversized.json", &json!("x".repeat(MAX_RECORD_BYTES))),
            Err(Error::Size)
        ));
        assert!(!dir.path().join("oversized.json").exists());
    }
}
