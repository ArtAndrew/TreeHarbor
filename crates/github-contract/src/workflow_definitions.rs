//! Executed root workflow bytes and owner requirements; neither proves runtime context.
use crate::{group_attempts as ga, queue::*, *};
use sha2::{Digest, Sha256};
use treeharbor_core::ObjectId;

pub const MAX_FILE_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExecutionContext {
    pub environment: String,
    pub toolchain: String,
    pub dependencies: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CheckApproval {
    pub required: RequiredCheck,
    pub workflow_id: String,
    pub path: String,
    pub content_sha256: String,
    pub expected_context: ExecutionContext,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub version: u8,
    pub checks: Vec<CheckApproval>,
}
fn digest(s: &str) -> Result<ObjectId> {
    s.parse().map_err(|_| InvalidEvidence)
}
fn path(s: &str) -> Result<()> {
    let name = s
        .strip_prefix(".github/workflows/")
        .ok_or(InvalidEvidence)?;
    if name.is_empty()
        || name.len() > 200
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        || !(name.ends_with(".yml") || name.ends_with(".yaml"))
    {
        return Err(InvalidEvidence);
    }
    Ok(())
}
impl Policy {
    pub fn canonical(&self) -> Result<Vec<u8>> {
        if self.version != 1 || self.checks.is_empty() || self.checks.len() > 32 {
            return Err(InvalidEvidence);
        }
        let mut p = self.clone();
        p.checks.sort_by(|a, b| a.required.cmp(&b.required));
        if p.checks.windows(2).any(|w| w[0].required == w[1].required) {
            return Err(InvalidEvidence);
        }
        for c in &p.checks {
            text(&c.required.name, 256)?;
            positive(c.required.app_id)?;
            node(&c.workflow_id)?;
            path(&c.path)?;
            for d in [
                &c.content_sha256,
                &c.expected_context.environment,
                &c.expected_context.toolchain,
                &c.expected_context.dependencies,
            ] {
                digest(d)?;
            }
        }
        let bytes = serde_json::to_vec(&p).map_err(|_| InvalidEvidence)?;
        if bytes.len() > 32 * 1024 {
            return Err(InvalidEvidence);
        }
        Ok(bytes)
    }
    pub fn digest(&self) -> Result<ObjectId> {
        let mut h = Sha256::new();
        h.update(b"treeharbor-github-check-policy\0v1\0");
        h.update(self.canonical()?);
        Ok(ObjectId::from_bytes(h.finalize().into()))
    }
    pub fn decode(bytes: &[u8], id: ObjectId) -> Result<Self> {
        let p: Self = serde_json::from_slice(bytes).map_err(|_| InvalidEvidence)?;
        if p.digest()? != id || p.canonical()? != bytes {
            return Err(InvalidEvidence);
        }
        Ok(p)
    }
    pub fn matches_required(&self, t: &QueueTarget) -> Result<()> {
        self.canonical()?;
        t.validate()?;
        let wanted: BTreeSet<_> = t.pull.required.iter().cloned().collect();
        let actual: BTreeSet<_> = self.checks.iter().map(|c| c.required.clone()).collect();
        if wanted != actual {
            return Err(InvalidEvidence);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FileSource {
    pub repository_name: String,
    pub revision: String,
    pub path: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub attempt_sequence: i64,
    pub check: ga::Target,
    pub resolved: ga::Resolved,
    pub selected: ga::Attempt,
}
impl Target {
    pub fn source(&self) -> Result<FileSource> {
        let f = self.check.workflow.file.as_ref().ok_or(InvalidEvidence)?;
        path(&f.path)?;
        let parts: Vec<_> = f.repository_name.split('/').collect();
        if parts.len() != 2
            || parts.iter().any(|s| {
                s.is_empty()
                    || s.len() > 100
                    || *s == "."
                    || *s == ".."
                    || !s
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
            })
        {
            return Err(InvalidEvidence);
        }
        let prefix = format!("https://github.com/{}/blob/", f.repository_name);
        let rest = f
            .repository_file_url
            .strip_prefix(&prefix)
            .ok_or(InvalidEvidence)?;
        let (sha, p) = rest.split_once('/').ok_or(InvalidEvidence)?;
        let rev = format!("git:sha1:{sha}");
        revision(&rev)?;
        if p != f.path || self.selected.path != f.path {
            return Err(InvalidEvidence);
        }
        Ok(FileSource {
            repository_name: f.repository_name.clone(),
            revision: rev,
            path: f.path.clone(),
        })
    }
    pub fn validate(&self, t: &QueueTarget) -> Result<()> {
        if !(1..=4096).contains(&self.attempt_sequence) {
            return Err(InvalidEvidence);
        }
        self.check.validate(t)?;
        for n in [
            self.resolved.check_id,
            self.resolved.run_id,
            self.resolved.workflow_id,
            self.resolved.suite_id,
        ] {
            positive(n)?;
        }
        self.selected.validate(t, &self.check, &self.resolved)?;
        if self.selected.number != self.check.workflow.observed_run_attempt {
            return Err(InvalidEvidence);
        }
        self.source()?;
        Ok(())
    }
    pub fn validate_source(
        &self,
        t: &QueueTarget,
        source: &ga::Report,
        finished: i64,
    ) -> Result<()> {
        self.validate(t)?;
        if !source.attempt_linked(t, finished)
            || self.check != source.target
            || self.resolved != source.resolved
            || self.selected != source.before.selected
        {
            return Err(InvalidEvidence);
        }
        Ok(())
    }
}

pub fn content_digest(content: &str) -> ObjectId {
    ObjectId::from_bytes(Sha256::digest(content.as_bytes()).into())
}
pub fn blob_oid(content: &str) -> Result<String> {
    use sha1_checked::{Digest as _, Sha1};
    let mut h = Sha1::new();
    h.update(format!("blob {}\0", content.len()).as_bytes());
    h.update(content.as_bytes());
    let v = h.try_finalize();
    if v.has_collision() {
        return Err(InvalidEvidence);
    }
    let hex: String = v.hash().iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!("git:sha1:{hex}"))
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct File {
    pub repository_id: u64,
    pub source: FileSource,
    pub tree: String,
    pub mode: u32,
    pub blob: String,
    pub byte_size: usize,
    pub content: String,
    pub content_sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub target: Target,
    pub file: File,
}
impl Report {
    pub fn validate(&self, t: &QueueTarget) -> Result<()> {
        self.target.validate(t)?;
        let f = &self.file;
        revision(&f.tree)?;
        if f.repository_id != t.pull.repository_id
            || f.source != self.target.source()?
            || !matches!(f.mode, 0o100644 | 0o100755)
            || f.content.len() > MAX_FILE_BYTES
            || f.content.is_empty()
            || f.content.contains('\0')
            || f.byte_size != f.content.len()
            || f.blob != blob_oid(&f.content)?
            || f.content_sha256 != content_digest(&f.content).to_string()
            || serde_json::to_vec(self).map_err(|_| InvalidEvidence)?.len() > 48 * 1024
        {
            return Err(InvalidEvidence);
        }
        Ok(())
    }
    /// Root file approval only. Expected runtime and transitive dependencies need separate proof.
    pub fn workflow_file_approved(&self, t: &QueueTarget, p: &Policy) -> bool {
        self.validate(t).is_ok()
            && p.matches_required(t).is_ok()
            && p.checks.iter().any(|c| {
                c.required.name == self.target.check.check.name
                    && c.required.app_id == self.target.check.app_id
                    && c.workflow_id == self.target.check.workflow.workflow_id
                    && c.path == self.file.source.path
                    && c.content_sha256 == self.file.content_sha256
            })
    }
}
