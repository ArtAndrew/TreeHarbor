//! Trusted publisher transport. This library does not grant repository permission
//! or persist publication intents. The service must do that before external IO.
use crate::{
    Error, Export, HashAlgorithm, Limits, Result, check_deadline, check_limits,
    download::{self, Step, Worker},
    export, private_parent, process,
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Instant,
};
use treeharbor_core::{GitOid, ObjectId, ObjectStore, RevisionRef};
use uuid::Uuid;

pub fn branch(id: Uuid) -> Result<String> {
    if id.is_nil() {
        return Err(Error::Configuration);
    }
    Ok(format!("refs/heads/treeharbor/{}", id.simple()))
}
#[cfg(any(target_os = "linux", test))]
pub(super) fn valid_branch(value: &str) -> bool {
    value
        .strip_prefix("refs/heads/treeharbor/")
        .is_some_and(|id| {
            id.len() == 32
                && id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                && id.bytes().any(|b| b != b'0')
        })
}
/// Created only by native conversion; an arbitrary manifest cannot mint this proof.
pub struct Prepared {
    artifact: tempfile::TempDir,
    git: PathBuf,
    native: ObjectId,
    sha: String,
    branch: String,
    bundle_hash: String,
    bundle_bytes: u64,
    limits: Limits,
}
impl Prepared {
    pub fn native(&self) -> ObjectId {
        self.native
    }
    pub fn branch(&self) -> &str {
        &self.branch
    }
    pub fn git_oid(&self) -> GitOid {
        let RevisionRef::Git(id) = format!("git:sha1:{}", self.sha)
            .parse()
            .expect("verified projection SHA-1")
        else {
            unreachable!()
        };
        id
    }
    pub fn timeout(&self) -> std::time::Duration {
        self.limits.timeout
    }
}

/// Converts a retained exact native graph and verifies a self-contained Git bundle.
/// The caller holds source retention; preparation alone has no external effects.
pub fn prepare(
    store: &impl ObjectStore,
    git: &Path,
    parent: &Path,
    identity: Uuid,
    native: ObjectId,
    limits: Limits,
) -> Result<Prepared> {
    check_limits(limits, git)?;
    let name = branch(identity)?;
    let parent = private_parent(&parent.join("prepared"))?;
    let artifact = tempfile::Builder::new()
        .prefix(".treeharbor-publish-")
        .tempdir_in(parent)?;
    fs::set_permissions(artifact.path(), fs::Permissions::from_mode(0o700))?;
    let output = artifact.path().join("projection");
    let manifest = export(
        store,
        Export {
            git,
            output: &output,
            refs: BTreeMap::from([(name.clone(), native)]),
            algorithm: HashAlgorithm::Sha1,
            limits,
        },
    )?;
    let sha = manifest
        .refs
        .get(&name)
        .ok_or(Error::InvalidProjection)?
        .git
        .clone();
    let RevisionRef::Git(GitOid::Sha1(_)) = format!("git:sha1:{sha}")
        .parse()
        .map_err(|_| Error::InvalidProjection)?
    else {
        return Err(Error::InvalidProjection);
    };
    Ok(Prepared {
        artifact,
        git: git.canonicalize()?,
        native,
        sha,
        branch: name,
        bundle_hash: manifest.bundle_sha256,
        bundle_bytes: manifest.bundle_bytes,
        limits,
    })
}
pub struct Push<'a> {
    pub worker: &'a Path,
    pub owner: &'a str,
    pub name: &'a str,
    pub token: &'a str,
    /// None means the branch must be absent. Never inferred from tracking refs.
    pub expected: Option<GitOid>,
}
impl Prepared {
    /// A failed call may have changed the remote. The trusted host adapter must
    /// read that exact ref; never convert this error into permission to retry.
    pub fn push(self, request: Push<'_>) -> Result<()> {
        if !cfg!(target_os = "linux") {
            return Err(Error::Unsupported(
                "GitHub publisher worker currently requires Linux",
            ));
        }
        if !download::component(request.owner)
            || !download::component(request.name)
            || !download::token(request.token)
            || !request.worker.is_absolute()
            || !request.worker.is_file()
        {
            return Err(Error::Configuration);
        }
        let expected = match request.expected {
            None => None,
            Some(GitOid::Sha1(id)) if id != [0; 20] => Some(
                RevisionRef::Git(GitOid::Sha1(id))
                    .to_string()
                    .trim_start_matches("git:sha1:")
                    .to_owned(),
            ),
            _ => return Err(Error::Configuration),
        };
        let deadline = Instant::now() + self.limits.timeout;
        let scratch = tempfile::Builder::new()
            .prefix("push-")
            .tempdir_in(self.artifact.path())?;
        fs::set_permissions(scratch.path(), fs::Permissions::from_mode(0o700))?;
        // Copy once into fresh private scratch while verifying the process-local
        // digest, so caller changes to an on-disk manifest cannot retarget a push.
        let mut input = OpenOptions::new()
            .read(true)
            .custom_flags(
                (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32,
            )
            .open(self.artifact.path().join("projection/history.bundle"))?;
        let m = input.metadata()?;
        if !m.is_file()
            || m.uid() != rustix::process::geteuid().as_raw()
            || m.mode() & 0o077 != 0
            || m.nlink() != 1
            || m.len() != self.bundle_bytes
        {
            return Err(Error::InvalidProjection);
        }
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(scratch.path().join("history.bundle"))?;
        let mut hash = Sha256::new();
        let mut bytes = 0u64;
        let mut buffer = [0; 65536];
        loop {
            check_deadline(deadline)?;
            let n = input.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            bytes += n as u64;
            if bytes > self.bundle_bytes {
                return Err(Error::InvalidProjection);
            }
            hash.update(&buffer[..n]);
            output.write_all(&buffer[..n])?;
        }
        if bytes != self.bundle_bytes
            || ObjectId::from_bytes(hash.finalize().into()).hex() != self.bundle_hash
        {
            return Err(Error::InvalidProjection);
        }
        output.sync_all()?;
        drop(output);
        drop(input);
        let repo = scratch.path().join("push.git");
        fs::create_dir(&repo)?;
        let template = scratch.path().join("empty-template");
        fs::create_dir(&template)?;
        let git = process::Git::new(self.git.clone(), scratch.path(), deadline);
        git.run(
            Some(&repo),
            "init publisher",
            &[
                "init".into(),
                "--bare".into(),
                "--quiet".into(),
                "--object-format=sha1".into(),
                format!("--template={}", template.display()).into(),
                repo.as_os_str().to_owned(),
            ],
            None,
            4096,
        )?;
        let make = |step| Worker {
            version: 1,
            parent_pid: std::process::id(),
            git: self.git.clone(),
            repo: repo.clone(),
            scratch: scratch.path().to_owned(),
            cpu_seconds: self.limits.timeout.as_secs().max(1),
            file_bytes: self.limits.stream_bytes,
            step,
        };
        let disk = self
            .limits
            .stream_bytes
            .checked_mul(3)
            .ok_or(Error::Limit)?;
        download::run(request.worker, &make(Step::Unbundle), deadline, disk)?;
        download::run(
            request.worker,
            &make(Step::Verify {
                sha: self.sha.clone(),
            }),
            deadline,
            disk,
        )?;
        let push = make(Step::Push {
            owner: request.owner.into(),
            name: request.name.into(),
            sha: self.sha.clone(),
            branch: self.branch.clone(),
            expected,
            token: request.token.into(),
        });
        download::run(request.worker, &push, deadline, disk).map_err(|_| Error::PublicationUnknown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generated_branch_cannot_address_main_or_accept_refspec_injection() {
        assert!(branch(Uuid::nil()).is_err());
        let generated = branch(Uuid::from_bytes([7; 16])).unwrap();
        assert_eq!(
            generated,
            "refs/heads/treeharbor/07070707070707070707070707070707"
        );
        assert!(valid_branch(&generated));
        for value in [
            "refs/heads/main",
            "refs/heads/treeharbor/00000000000000000000000000000000",
            "refs/heads/treeharbor/ABCDEF",
            "+refs/heads/treeharbor/07070707070707070707070707070707",
            "refs/heads/treeharbor/../../main",
        ] {
            assert!(!valid_branch(value));
        }
    }
}
