//! Prepare exact native proposals at the optional Git import boundary.
use super::{Error, Result, files, json, preserve};
use crate::pack;
use serde_json::Value;
use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    fs,
    io::ErrorKind,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    time::{Duration, Instant},
};
use treeharbor_core::{
    Object, ObjectId, ObjectStore, TreeDiffLimits, diff_snapshots, prepare_change,
};
use treeharbor_git_bridge::{BundleInput, ImportedBranches, Limits, verify_archive_bundle};
use treeharbor_history::History;
use treeharbor_metadata::github::{
    ARCHIVE_PROPOSALS_FORMAT, ArchiveProposal as Proposal, ArchiveProposalManifest as Manifest,
    ArchiveProposalPatch as Patch, ArchiveProposalResolution as Resolution,
    archive_manifest_fingerprint, archive_proposal_labels as labels,
    archive_proposal_tip as source_tip, archive_proposals_fingerprint as fingerprint,
};
use treeharbor_storage::{LocalStore, TenantId};

pub(super) const STORE_TENANT: &str = "archive-proposals";

#[derive(clap::Args)]
pub struct Options {
    /// Directory returned by archive-preserve or archive-recover.
    #[arg(long)]
    archive: PathBuf,
    /// Exact manifest_sha256 from the archive preservation or retention receipt.
    #[arg(long)]
    archive_fingerprint: String,
    /// New private directory for the native objects and manifest.
    #[arg(long)]
    output: PathBuf,
    #[arg(long, default_value = "/usr/bin/git")]
    git: PathBuf,
    #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u64).range(1..=3600))]
    timeout_seconds: u64,
}

// History and tree diff already bound an individual traversal. This shared
// budget also covers repeated traversals across an entire archive.
struct PlanningStore<'a> {
    store: &'a LocalStore,
    deadline: Instant,
    visits_left: Cell<usize>,
}
impl PlanningStore<'_> {
    fn check(&self) -> treeharbor_core::Result<()> {
        if Instant::now() >= self.deadline {
            return Err(treeharbor_core::Error::TooLarge);
        }
        Ok(())
    }
}
impl ObjectStore for PlanningStore<'_> {
    fn get(&self, id: ObjectId) -> treeharbor_core::Result<Object> {
        self.check()?;
        self.visits_left.set(
            self.visits_left
                .get()
                .checked_sub(1)
                .ok_or(treeharbor_core::Error::TooLarge)?,
        );
        self.store.get(id)
    }
    fn put(&self, object: &Object) -> treeharbor_core::Result<ObjectId> {
        self.check()?;
        self.store.put(object)
    }
}

fn prepare_proposals(
    store: &PlanningStore<'_>,
    limits: Limits,
    mut source: preserve::Manifest,
    source_fingerprint: String,
    mut imported: ImportedBranches,
) -> pack::Result<Manifest> {
    let history = History::new(store, limits.objects);
    let upstream = source.receipt.observation.upstream.clone();
    let mut seen: BTreeSet<_> = imported.objects.iter().copied().collect();
    let mut proposals = Vec::with_capacity(source.inventory.open_pulls.len());
    source.inventory.open_pulls.sort_by_key(|pull| pull.number);
    for pull in source.inventory.open_pulls {
        store.check()?;
        let (base_label, head_label) = labels(pull.number);
        let base = imported.refs.get(&base_label).ok_or(pack::Error::Invalid)?;
        let head = imported.refs.get(&head_label).ok_or(pack::Error::Invalid)?;
        let merge_bases = history.merge_bases(base.commit, head.commit)?;
        let patch = match merge_bases.as_slice() {
            [id] => {
                let merge_base = source_tip(&history, *id)?;
                let base_snapshot = merge_base.snapshot.parse()?;
                let delta = diff_snapshots(
                    store,
                    base_snapshot,
                    head.snapshot,
                    TreeDiffLimits::default(),
                )?;
                let applied = prepare_change(store, base_snapshot, &delta)?;
                if applied.result_snapshot() != head.snapshot {
                    return Err(pack::Error::Invalid);
                }
                let operations = applied.operations();
                if !seen.contains(&operations) {
                    let frame = store.get(operations)?;
                    if frame
                        .references()
                        .iter()
                        .any(|(child, _)| !seen.contains(child))
                    {
                        return Err(pack::Error::Invalid);
                    }
                    imported.encoded_bytes = imported
                        .encoded_bytes
                        .checked_add(frame.encode()?.len() as u64)
                        .ok_or(pack::Error::Limit)?;
                    if imported.objects.len() >= limits.objects
                        || imported.encoded_bytes > limits.source_bytes
                    {
                        return Err(pack::Error::Limit);
                    }
                    seen.insert(operations);
                    imported.objects.push(operations);
                }
                Patch::Ready {
                    merge_base,
                    operations: operations.to_string(),
                }
            }
            other => Patch::NeedsResolution {
                reason: if other.is_empty() {
                    Resolution::UnrelatedHistory
                } else {
                    Resolution::MultipleMergeBases
                },
                merge_bases: other
                    .iter()
                    .map(|id| source_tip(&history, *id))
                    .collect::<treeharbor_core::Result<_>>()?,
            },
        };
        let target_present = source
            .receipt
            .observation
            .heads
            .contains_key(&format!("refs/heads/{}", pull.base_ref_name));
        proposals.push(Proposal {
            discussion: format!(
                "https://github.com/{}/{}/pull/{}",
                upstream.owner, upstream.name, pull.number
            ),
            source: pull,
            target_present,
            base_tip: source_tip(&history, base.commit)?,
            head: source_tip(&history, head.commit)?,
            patch,
        });
    }
    Ok(Manifest {
        format: ARCHIVE_PROPOSALS_FORMAT.into(),
        version: 1,
        archive_manifest_sha256: source_fingerprint,
        archive_bundle_sha256: source.bundle.sha256,
        observation: source.receipt.request,
        upstream,
        proposals,
        objects: imported.objects.iter().map(ToString::to_string).collect(),
        encoded_bytes: imported.encoded_bytes,
    })
}

fn prepare(options: Options) -> Result<Value> {
    let limits = Limits {
        timeout: Duration::from_secs(options.timeout_seconds),
        ..Limits::default()
    };
    let deadline = Instant::now() + limits.timeout;
    let archive = files::private_directory(&options.archive)?;
    let bytes = files::read_private(&archive.join("manifest.json"), treeharbor_core::MAX_PAYLOAD)?;
    let source_fingerprint = archive_manifest_fingerprint(&bytes).to_string();
    if source_fingerprint != options.archive_fingerprint {
        return Err(Error::PublicationState);
    }
    let source: preserve::Manifest =
        serde_json::from_slice(&bytes).map_err(|_| Error::PublicationState)?;
    source.validate().map_err(Error::ArchiveArtifact)?;
    source.inventory.archive.binding.validate()?;
    let (expected, bases) =
        preserve::expected_refs(&source.inventory, &source.receipt.observation)?;
    let parent = files::private_directory(options.output.parent().ok_or(Error::UnsafeFile)?)?;
    let name = options.output.file_name().ok_or(Error::UnsafeFile)?;
    match parent.join(name).symlink_metadata() {
        Ok(_) => return Err(Error::Io(std::io::Error::from(ErrorKind::AlreadyExists))),
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let stage = tempfile::Builder::new()
        .prefix(".treeharbor-archive-proposals-")
        .tempdir_in(&parent)?;
    fs::set_permissions(stage.path(), fs::Permissions::from_mode(0o700))?;
    let mut verified = verify_archive_bundle(
        BundleInput {
            git: &options.git,
            bundle: &archive.join("archive.bundle"),
            scratch: stage.path(),
            limits: Limits {
                timeout: deadline.saturating_duration_since(Instant::now()),
                ..limits
            },
        },
        &expected,
        &format!("refs/heads/{}", source.receipt.observation.default_branch),
        bases,
    )?;
    if verified.info != source.bundle {
        return Err(Error::PublicationState);
    }
    let mut commits = BTreeMap::new();
    for pull in &source.inventory.open_pulls {
        let (base, head) = labels(pull.number);
        commits.insert(
            base,
            preserve::sha1(&format!("git:sha1:{}", pull.base_ref_oid))?,
        );
        commits.insert(
            head,
            preserve::sha1(&format!("git:sha1:{}", pull.head_ref_oid))?,
        );
    }
    let tenant = TenantId::new(STORE_TENANT).map_err(pack::Error::Object)?;
    let store = LocalStore::open(stage.path(), &tenant).map_err(pack::Error::Object)?;
    let imported = if commits.is_empty() {
        ImportedBranches {
            refs: BTreeMap::new(),
            objects: vec![],
            encoded_bytes: 0,
        }
    } else {
        verified.import_commits(&store, &commits)?
    };
    drop(verified);
    let planning_store = PlanningStore {
        store: &store,
        deadline,
        visits_left: Cell::new(limits.path_visits),
    };
    let manifest = prepare_proposals(
        &planning_store,
        limits,
        source,
        source_fingerprint,
        imported,
    )?;
    planning_store.check().map_err(pack::Error::Object)?;
    let bytes = serde_json::to_vec(&manifest).map_err(|_| Error::PublicationState)?;
    if bytes.len() > pack::MAX_MANIFEST {
        return Err(Error::Size);
    }
    files::write_private(&stage.path().join("manifest.json"), &bytes)?;
    pack::publish_stage(stage.path(), &parent, name)?;
    let ready = manifest
        .proposals
        .iter()
        .filter(|proposal| proposal.target_present && matches!(proposal.patch, Patch::Ready { .. }))
        .count();
    Ok(json!({
        "version":1,"state":"prepared","observation_key":manifest.observation.observation_key,
        "manifest_sha256":fingerprint(&bytes).to_string(),"open_pulls":manifest.proposals.len(),
        "ready":ready,"needs_resolution":manifest.proposals.len()-ready,
        "object_count":manifest.objects.len(),"encoded_bytes":manifest.encoded_bytes,
    }))
}

pub(in crate::github) fn run(options: Options) -> Result<bool> {
    println!("{}", prepare(options)?);
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_proposal_manifest_matches_v1_vector() {
        let raw = include_bytes!("../../../tests/fixtures/github-archive-proposals-v1.json")
            .strip_suffix(b"\n")
            .unwrap();
        let manifest: Manifest = serde_json::from_slice(raw).unwrap();
        assert_eq!(manifest.format, ARCHIVE_PROPOSALS_FORMAT);
        assert_eq!(manifest.version, 1);
        assert!(matches!(manifest.proposals[0].patch, Patch::Ready { .. }));
        assert!(matches!(
            manifest.proposals[1].patch,
            Patch::NeedsResolution { .. }
        ));
        assert_eq!(serde_json::to_vec(&manifest).unwrap(), raw);
        assert_eq!(
            fingerprint(raw).to_string(),
            "sha256:4fddec2efec515e9cb75fe16ef4c4243370e87c83cdd7ca4c91be52e39c88055"
        );
    }
}
