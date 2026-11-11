//! Retain the exact Git data and inventory behind a recorded observation.
use super::{ArchiveObservation, Capture, Error, Result, files, json};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    io::ErrorKind,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};
use treeharbor_core::{GitOid, RevisionRef};
use treeharbor_git_bridge::{
    ArchiveRef, BundleInput, GitObjectKind, Limits, verify_archive_bundle,
};
use treeharbor_github::{ArchivedRepository, RepositoryBinding};
use treeharbor_metadata::github::{
    ARCHIVE_ARTIFACT_FORMAT, ArchiveArtifactManifest, ArchiveObservationReceipt,
    archive_manifest_fingerprint,
};

const MAX_INVENTORY_BYTES: usize = 32 * 1024 * 1024;

#[derive(clap::Args)]
pub struct Options {
    /// Private directory with inventory.json and receipt.json from archive-observe.
    #[arg(long)]
    state: PathBuf,
    /// Self-contained local Git bundle of the observed branches, tags and PR heads.
    #[arg(long)]
    bundle: PathBuf,
    /// New directory under an existing private parent.
    #[arg(long)]
    output: PathBuf,
    #[arg(long, default_value = "/usr/bin/git")]
    git: PathBuf,
    #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u64).range(1..=3600))]
    timeout_seconds: u64,
}

type Manifest = ArchiveArtifactManifest<RepositoryBinding>;

fn load_state(state: &Path) -> Result<(Capture, ArchiveObservationReceipt)> {
    let state = files::private_directory(state)?;
    let read = |name| files::read_private(&state.join(name), MAX_INVENTORY_BYTES);
    let receipt: ArchiveObservationReceipt =
        serde_json::from_slice(&read("receipt.json")?).map_err(|_| Error::PublicationState)?;
    let capture: Capture =
        serde_json::from_slice(&read("inventory.json")?).map_err(|_| Error::PublicationState)?;
    let intent: Value =
        serde_json::from_slice(&read("intent.json")?).map_err(|_| Error::PublicationState)?;
    let request = &receipt.request;
    if receipt.version != 1
        || receipt.state != "observed"
        || request.version != 1
        || request.binding != capture.binding
        || request.binding.is_nil()
        || request.repository.is_nil()
        || request.epoch < 1
        || receipt.observation != capture.observation(&request.key)?
        || intent
            != json!({
                "version":1,"kind":"archive_observation","tenant":request.tenant,
                "repository":request.repository,"epoch":request.epoch,"request_key":request.key,
            })
    {
        return Err(Error::PublicationState);
    }
    Ok((capture, receipt))
}

fn sha1(oid: &str) -> Result<GitOid> {
    match oid.parse() {
        Ok(RevisionRef::Git(oid @ GitOid::Sha1(id))) if id != [0; 20] => Ok(oid),
        _ => Err(Error::PublicationState),
    }
}

fn expected_refs(
    inventory: &ArchivedRepository,
    observation: &ArchiveObservation,
) -> Result<(BTreeMap<String, ArchiveRef>, Vec<GitOid>)> {
    use GitObjectKind::*;
    let mut refs = BTreeMap::new();
    for (name, revision) in &observation.heads {
        refs.insert(
            name.clone(),
            ArchiveRef {
                oid: sha1(revision)?,
                kind: Commit,
            },
        );
    }
    for tag in &inventory.tags {
        let kind = match tag.target.kind.as_str() {
            "Tag" => Tag,
            "Commit" => Commit,
            "Tree" => Tree,
            "Blob" => Blob,
            _ => return Err(Error::PublicationState),
        };
        if tag.prefix != "refs/tags/"
            || refs
                .insert(
                    format!("refs/tags/{}", tag.name),
                    ArchiveRef {
                        oid: sha1(&format!("git:sha1:{}", tag.target.oid))?,
                        kind,
                    },
                )
                .is_some()
        {
            return Err(Error::PublicationState);
        }
    }
    let mut bases = Vec::with_capacity(inventory.open_pulls.len());
    for pull in &inventory.open_pulls {
        if pull.state != "OPEN"
            || pull.merged
            || pull.base_repository.database_id != inventory.archive.binding.repository_id
            || refs
                .insert(
                    format!("refs/pull/{}/head", pull.number),
                    ArchiveRef {
                        oid: sha1(&format!("git:sha1:{}", pull.head_ref_oid))?,
                        kind: Commit,
                    },
                )
                .is_some()
        {
            return Err(Error::PublicationState);
        }
        bases.push(sha1(&format!("git:sha1:{}", pull.base_ref_oid))?);
    }
    Ok((refs, bases))
}

fn preserve(options: Options) -> Result<Value> {
    let (capture, receipt) = load_state(&options.state)?;
    let (refs, bases) = expected_refs(&capture.inventory, &receipt.observation)?;
    let parent = files::private_directory(options.output.parent().ok_or(Error::UnsafeFile)?)?;
    let name = options.output.file_name().ok_or(Error::UnsafeFile)?;
    match parent.join(name).symlink_metadata() {
        Ok(_) => return Err(Error::Io(std::io::Error::from(ErrorKind::AlreadyExists))),
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let stage = tempfile::Builder::new()
        .prefix(".treeharbor-archive-preserve-")
        .tempdir_in(&parent)?;
    fs::set_permissions(stage.path(), fs::Permissions::from_mode(0o700))?;
    let archive = verify_archive_bundle(
        BundleInput {
            git: &options.git,
            bundle: &options.bundle,
            scratch: stage.path(),
            limits: Limits {
                timeout: Duration::from_secs(options.timeout_seconds),
                ..Limits::default()
            },
        },
        &refs,
        &format!("refs/heads/{}", receipt.observation.default_branch),
        bases,
    )?;
    fs::rename(archive.path(), stage.path().join("archive.bundle"))?;
    let manifest = Manifest {
        format: ARCHIVE_ARTIFACT_FORMAT.into(),
        version: 1,
        receipt,
        inventory: capture.inventory,
        bundle: archive.info.clone(),
    };
    drop(archive);
    let bytes = serde_json::to_vec(&manifest).map_err(|_| Error::PublicationState)?;
    if bytes.len() > crate::pack::MAX_MANIFEST {
        return Err(Error::Size);
    }
    files::write_private(&stage.path().join("manifest.json"), &bytes)?;
    crate::pack::publish_stage(stage.path(), &parent, name)?;
    Ok(json!({
        "version":1,"state":"preserved",
        "observation_key":manifest.receipt.request.observation_key,
        "manifest_sha256":archive_manifest_fingerprint(&bytes).to_string(),
        "bundle":manifest.bundle,
        "branches":manifest.inventory.archive.heads.len(),
        "tags":manifest.inventory.tags.len(),
        "open_pulls":manifest.inventory.open_pulls.len(),
    }))
}

pub(in crate::github) fn run(options: Options) -> Result<bool> {
    let result = preserve(options)?;
    println!("{result}");
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_preservation_manifest_matches_v1_vector() {
        let bytes = include_bytes!("../../../tests/fixtures/github-archive-preservation-v1.json");
        let bytes = bytes.strip_suffix(b"\n").unwrap();
        let manifest: Manifest = serde_json::from_slice(bytes).unwrap();
        assert_eq!(serde_json::to_vec(&manifest).unwrap(), bytes);
        assert_eq!(
            archive_manifest_fingerprint(bytes).to_string(),
            "sha256:469e9cb7276ed7664bcb2fd7a25a5493c00a0957c910d3f264896894bdead68e"
        );
        assert_eq!(
            manifest.receipt.observation,
            Capture {
                binding: manifest.receipt.request.binding,
                inventory: manifest.inventory,
            }
            .observation(&manifest.receipt.request.key)
            .unwrap()
        );
    }
}
