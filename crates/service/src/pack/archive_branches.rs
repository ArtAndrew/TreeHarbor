//! Durable local capture of the complete verified branch set of an archived GitHub source.
use super::{Error, MAX_MANIFEST, Result, files, publish_stage};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::ErrorKind,
    os::unix::fs::PermissionsExt,
    path::Path,
    time::Instant,
};
use treeharbor_core::{GitOid, Object, ObjectId, ObjectStore, RevisionRef};
use treeharbor_git_bridge::{self as git_bridge, BundleInput, ImportedBranches, Limits};
use treeharbor_github::ArchivedBranches;
use treeharbor_metadata::github::{ArchiveBranchTip, ArchiveStageSpec, Upstream};
use treeharbor_storage::{LocalStore, TenantId};

const FORMAT: &str = "treeharbor-archive-branch-spool";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: String,
    version: u8,
    archive: ArchivedBranches,
    branches: BTreeMap<String, ArchiveBranchTip>,
    objects: Vec<String>,
    encoded_bytes: u64,
}

fn expected_refs(archive: &ArchivedBranches) -> Result<BTreeMap<String, GitOid>> {
    if archive.version != 1
        || archive.binding.validate().is_err()
        || archive.app_id == 0
        || archive.repository_node_id.is_empty()
        || archive.started_at_unix == 0
        || archive.finished_at_unix < archive.started_at_unix
        || !archive.token_revoked
        || archive.heads.is_empty()
        || archive.heads.len() > git_bridge::MAX_EXPORT_REFS
        || !archive
            .heads
            .iter()
            .any(|head| head.name == archive.default_branch)
    {
        return Err(Error::Invalid);
    }
    let mut expected = BTreeMap::new();
    let mut node_ids = BTreeSet::new();
    for head in &archive.heads {
        if head.prefix != "refs/heads/"
            || head.target.kind != "Commit"
            || head.id.is_empty()
            || !node_ids.insert(head.id.as_str())
        {
            return Err(Error::Invalid);
        }
        let revision = format!("git:sha1:{}", head.target.oid);
        let RevisionRef::Git(oid) = revision.parse()? else {
            return Err(Error::Invalid);
        };
        if revision != RevisionRef::Git(oid).to_string()
            || expected
                .insert(format!("refs/heads/{}", head.name), oid)
                .is_some()
        {
            return Err(Error::Invalid);
        }
    }
    Ok(expected)
}

fn manifest(archive: ArchivedBranches, imported: ImportedBranches) -> Manifest {
    Manifest {
        format: FORMAT.into(),
        version: 1,
        archive,
        branches: imported
            .refs
            .into_iter()
            .map(|(name, tip)| {
                (
                    name,
                    ArchiveBranchTip {
                        git_revision: RevisionRef::Git(tip.origin).to_string(),
                        native_revision: RevisionRef::Native(tip.commit).to_string(),
                        snapshot: tip.snapshot.to_string(),
                    },
                )
            })
            .collect(),
        objects: imported.objects.iter().map(ToString::to_string).collect(),
        encoded_bytes: imported.encoded_bytes,
    }
}

fn fingerprint(bytes: &[u8]) -> ObjectId {
    let mut hash = Sha256::new();
    hash.update(b"treeharbor-archive-branch-spool\0v1\0");
    hash.update(bytes);
    ObjectId::from_bytes(hash.finalize().into())
}

fn store(directory: &Path) -> Result<LocalStore> {
    Ok(LocalStore::open(
        directory,
        &TenantId::new("archive-preflight")?,
    )?)
}

pub(crate) enum Destination<'a> {
    Temporary(&'a Path),
    Durable(&'a Path),
}

pub(crate) fn capture(
    archive: ArchivedBranches,
    git: &Path,
    bundle: &Path,
    destination: Destination<'_>,
    limits: Limits,
) -> Result<Value> {
    let expected = expected_refs(&archive)?;
    let (parent, name) = match destination {
        Destination::Temporary(parent) => (parent, None),
        Destination::Durable(path) => (
            path.parent().ok_or(Error::Output)?,
            Some(path.file_name().ok_or(Error::Output)?),
        ),
    };
    let parent = files::private_directory(parent).map_err(|_| Error::Output)?;
    if let Some(name) = name {
        match parent.join(name).symlink_metadata() {
            Ok(_) => return Err(Error::Output),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    let stage = tempfile::Builder::new()
        .prefix(".treeharbor-archive-preflight-")
        .tempdir_in(&parent)?;
    fs::set_permissions(stage.path(), fs::Permissions::from_mode(0o700))?;
    let store = store(stage.path())?;
    let imported = git_bridge::import_branches(
        &store,
        BundleInput {
            git,
            bundle,
            scratch: stage.path(),
            limits,
        },
        &expected,
    )?;
    let manifest = manifest(archive, imported);
    let retained = if let Some(name) = name {
        let bytes = serde_json::to_vec(&manifest).map_err(|_| Error::Invalid)?;
        if bytes.len() > MAX_MANIFEST {
            return Err(Error::Limit);
        }
        files::write_private(&stage.path().join("manifest.json"), &bytes)
            .map_err(|_| Error::Output)?;
        publish_stage(stage.path(), &parent, name)?;
        Some(fingerprint(&bytes))
    } else {
        stage.close()?;
        None
    };
    let mut report = json!({
        "version":1,
        "archive":manifest.archive,
        "branches":manifest.branches,
        "source_objects":manifest.objects.len(),
        "encoded_bytes":manifest.encoded_bytes,
    });
    if let Some(retained) = retained {
        report["spool_fingerprint"] = json!(retained.to_string());
    }
    Ok(report)
}

struct OpenedSpool {
    manifest: Manifest,
    store: LocalStore,
    refs: BTreeMap<String, ObjectId>,
    ids: Vec<ObjectId>,
    fingerprint: ObjectId,
}

fn reopen(
    directory: &Path,
    expected_fingerprint: Option<ObjectId>,
    mut limits: Limits,
    deadline: Instant,
) -> Result<OpenedSpool> {
    let directory = files::private_directory(directory).map_err(|_| Error::Output)?;
    let bytes = files::read_private(&directory.join("manifest.json"), MAX_MANIFEST)
        .map_err(|_| Error::Invalid)?;
    let fingerprint = fingerprint(&bytes);
    if expected_fingerprint.is_some_and(|expected| expected != fingerprint) {
        return Err(Error::Invalid);
    }
    let manifest: Manifest = serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)?;
    if manifest.version != 1
        || manifest.format != FORMAT
        || serde_json::to_vec(&manifest).map_err(|_| Error::Invalid)? != bytes
    {
        return Err(Error::Invalid);
    }
    if manifest.objects.len() > limits.objects || manifest.encoded_bytes > limits.source_bytes {
        return Err(Error::Limit);
    }
    let expected = expected_refs(&manifest.archive)?;
    if manifest.branches.len() != expected.len() {
        return Err(Error::Invalid);
    }
    let store = store(&directory)?;
    let mut refs = BTreeMap::new();
    for (name, git) in expected {
        let branch = manifest.branches.get(&name).ok_or(Error::Invalid)?;
        let RevisionRef::Native(tip) = branch.native_revision.parse()? else {
            return Err(Error::Invalid);
        };
        let Object::NativeCommit(commit) = store.get(tip)? else {
            return Err(Error::Invalid);
        };
        if branch.git_revision != RevisionRef::Git(git).to_string()
            || branch.native_revision != RevisionRef::Native(tip).to_string()
            || branch.snapshot != commit.snapshot.to_string()
            || commit.git_origin.as_ref().map(|origin| origin.oid) != Some(git)
        {
            return Err(Error::Invalid);
        }
        refs.insert(name, tip);
    }
    limits.timeout = deadline.saturating_duration_since(Instant::now());
    if limits.timeout.is_zero() {
        return Err(Error::Limit);
    }
    let (ids, bytes) = git_bridge::verified_branch_objects(&store, &refs, limits)?;
    if bytes != manifest.encoded_bytes
        || ids.len() != manifest.objects.len()
        || ids
            .iter()
            .zip(&manifest.objects)
            .any(|(id, declared)| id.to_string() != *declared)
    {
        return Err(Error::Invalid);
    }
    Ok(OpenedSpool {
        manifest,
        store,
        refs,
        ids,
        fingerprint,
    })
}

pub(crate) struct StageSource {
    pub specification: ArchiveStageSpec,
    pub store: LocalStore,
    pub ids: Vec<ObjectId>,
}

pub(crate) fn for_stage(directory: &Path, fingerprint: ObjectId) -> Result<StageSource> {
    let limits = Limits::default();
    let source = reopen(
        directory,
        Some(fingerprint),
        limits,
        Instant::now() + limits.timeout,
    )?;
    let archive = source.manifest.archive;
    let spec = ArchiveStageSpec {
        version: 1,
        spool_fingerprint: source.fingerprint.to_string(),
        app_id: archive.app_id,
        upstream: Upstream {
            installation_id: archive.binding.installation_id,
            repository_id: archive.binding.repository_id,
            owner_id: archive.binding.owner_id,
            owner: archive.binding.owner,
            name: archive.binding.name,
        },
        default_branch: archive.default_branch,
        branches: source.manifest.branches,
        object_count: source.ids.len() as i64,
        encoded_bytes: source.manifest.encoded_bytes as i64,
    };
    spec.validate().map_err(|_| Error::Invalid)?;
    Ok(StageSource {
        specification: spec,
        store: source.store,
        ids: source.ids,
    })
}

pub(crate) fn export(
    directory: &Path,
    expected_fingerprint: Option<ObjectId>,
    git: &Path,
    output: &Path,
    algorithm: git_bridge::HashAlgorithm,
    mut limits: Limits,
) -> Result<git_bridge::Manifest> {
    let deadline = Instant::now() + limits.timeout;
    let spool = reopen(directory, expected_fingerprint, limits, deadline)?;
    limits.timeout = deadline.saturating_duration_since(Instant::now());
    if limits.timeout.is_zero() {
        return Err(Error::Limit);
    }
    Ok(git_bridge::export(
        &spool.store,
        git_bridge::Export {
            git,
            output,
            refs: spool.refs,
            algorithm,
            limits,
        },
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spool_manifest_and_fingerprint_match_the_independent_vector() {
        let bytes = include_bytes!("../../tests/fixtures/archive-branch-spool-v1.json")
            .strip_suffix(b"\n")
            .unwrap();
        let manifest: Manifest = serde_json::from_slice(bytes).unwrap();
        assert_eq!(manifest.format, FORMAT);
        assert_eq!(manifest.version, 1);
        assert_eq!(expected_refs(&manifest.archive).unwrap().len(), 1);
        assert_eq!(serde_json::to_vec(&manifest).unwrap(), bytes);
        assert_eq!(
            fingerprint(bytes).to_string(),
            "sha256:7d1555241d2ed3ddbd1b6429697cccc1db6218022e90c82c33444294bc18ed88"
        );
    }
}
