//! Shared v1 format for the proposals prepared from a retained GitHub archive.
use super::{ArchiveArtifactManifest, ArchiveBranchTip, ArchiveObservationRequest, Upstream};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    time::Instant,
};
use treeharbor_core::{
    Changes, Directory, GitOid, Object, ObjectId, ObjectStore, RevisionRef, TreeDiffLimits,
    diff_snapshots, prepare_change,
};
use treeharbor_git_bridge::{Limits, verify_archive_commit_mappings};
use treeharbor_github_contract::archive_inventory::InventoryPull;
use treeharbor_history::History;

pub const ARCHIVE_PROPOSALS_FORMAT: &str = "treeharbor-archive-proposals";

#[derive(Deserialize, Serialize, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArchiveProposalManifest {
    pub format: String,
    pub version: u8,
    pub archive_manifest_sha256: String,
    pub archive_bundle_sha256: String,
    pub observation: ArchiveObservationRequest,
    pub upstream: Upstream,
    pub proposals: Vec<ArchiveProposal>,
    pub objects: Vec<String>,
    pub encoded_bytes: u64,
}

#[derive(Deserialize, Serialize, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArchiveProposal {
    pub source: InventoryPull,
    pub discussion: String,
    pub target_present: bool,
    pub base_tip: ArchiveBranchTip,
    pub head: ArchiveBranchTip,
    pub patch: ArchiveProposalPatch,
}

#[derive(Deserialize, Serialize, Debug, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ArchiveProposalPatch {
    Ready {
        merge_base: ArchiveBranchTip,
        operations: String,
    },
    NeedsResolution {
        reason: ArchiveProposalResolution,
        merge_bases: Vec<ArchiveBranchTip>,
    },
}

#[derive(Deserialize, Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveProposalResolution {
    UnrelatedHistory,
    MultipleMergeBases,
}

pub fn archive_proposal_labels(number: u64) -> (String, String) {
    (
        format!("refs/heads/archive-pr-{number}-base"),
        format!("refs/heads/archive-pr-{number}-head"),
    )
}

pub fn archive_proposal_tip(
    history: &History<'_, impl ObjectStore>,
    id: ObjectId,
) -> treeharbor_core::Result<ArchiveBranchTip> {
    let commit = history.commit(id)?;
    let origin = commit
        .git_origin
        .ok_or(treeharbor_core::Error::InvalidObject("missing Git origin"))?;
    Ok(ArchiveBranchTip {
        git_revision: RevisionRef::Git(origin.oid).to_string(),
        native_revision: RevisionRef::Native(id).to_string(),
        snapshot: commit.snapshot.to_string(),
    })
}

pub fn archive_proposals_fingerprint(bytes: &[u8]) -> ObjectId {
    let mut digest = Sha256::new();
    digest.update(b"treeharbor-archive-proposals\0v1\0");
    digest.update(bytes);
    ObjectId::from_bytes(digest.finalize().into())
}

struct TrackedSource<'a, S> {
    store: &'a S,
    deadline: Instant,
    reads_left: Cell<usize>,
    seen: RefCell<BTreeMap<ObjectId, u8>>,
}

impl<S: ObjectStore> ObjectStore for TrackedSource<'_, S> {
    fn get(&self, id: ObjectId) -> treeharbor_core::Result<Object> {
        if Instant::now() >= self.deadline {
            return Err(treeharbor_core::Error::TooLarge);
        }
        self.reads_left.set(
            self.reads_left
                .get()
                .checked_sub(1)
                .ok_or(treeharbor_core::Error::TooLarge)?,
        );
        let object = self.store.get(id)?;
        self.seen.borrow_mut().insert(id, object.kind());
        Ok(object)
    }

    fn put(&self, _object: &Object) -> treeharbor_core::Result<ObjectId> {
        Err(treeharbor_core::Error::InvalidObject(
            "read-only archive proof",
        ))
    }
}

// The imported first-parent delta of a root starts from the canonical empty
// snapshot. That synthetic baseline need not be in the uploaded closure.
struct Genesis<'a, S> {
    source: &'a S,
    root: ObjectId,
    snapshot: ObjectId,
}

impl<S: ObjectStore> ObjectStore for Genesis<'_, S> {
    fn get(&self, id: ObjectId) -> treeharbor_core::Result<Object> {
        if id == self.snapshot {
            Ok(Object::Snapshot { root: self.root })
        } else if id == self.root {
            Ok(Object::Directory(Directory::new(vec![])?))
        } else {
            self.source.get(id)
        }
    }

    fn put(&self, object: &Object) -> treeharbor_core::Result<ObjectId> {
        self.source.put(object)
    }
}

fn exact_delta(store: &impl ObjectStore, before: ObjectId, after: ObjectId) -> Result<Changes> {
    Ok(diff_snapshots(
        store,
        before,
        after,
        TreeDiffLimits::default(),
    )?)
}

fn verify_imported_deltas(store: &TrackedSource<'_, impl ObjectStore>, max: usize) -> Result<()> {
    let commits: Vec<_> = store
        .seen
        .borrow()
        .iter()
        .filter_map(|(&id, &kind)| (kind == 5).then_some(id))
        .collect();
    let history = History::new(store, max);
    let root = Object::Directory(Directory::new(vec![])?).id()?;
    let empty = Genesis {
        source: store,
        root,
        snapshot: Object::Snapshot { root }.id()?,
    };
    let mut origins = BTreeSet::new();
    for id in commits {
        let commit = history.commit(id)?;
        let Some(origin) = commit.git_origin else {
            return Err(Error::ArchiveArtifactMismatch);
        };
        if !origins.insert(RevisionRef::Git(origin.oid).to_string()) {
            return Err(Error::ArchiveArtifactMismatch);
        }
        let delta = match commit.parents.first() {
            Some(parent) => exact_delta(store, history.commit(*parent)?.snapshot, commit.snapshot)?,
            None => exact_delta(&empty, empty.snapshot, commit.snapshot)?,
        };
        if Object::ChangeOperations(delta).id()? != commit.changes {
            return Err(Error::ArchiveArtifactMismatch);
        }
    }
    Ok(())
}

fn claimed_tip(
    tip: &ArchiveBranchTip,
    git_hex: &str,
    history: &History<'_, impl ObjectStore>,
) -> Result<(ObjectId, GitOid)> {
    let Ok(RevisionRef::Native(native)) = tip.native_revision.parse() else {
        return Err(Error::ArchiveArtifactMismatch);
    };
    let Ok(RevisionRef::Git(git @ GitOid::Sha1(_))) = format!("git:sha1:{git_hex}").parse() else {
        return Err(Error::ArchiveArtifactMismatch);
    };
    if *tip != archive_proposal_tip(history, native)?
        || tip.git_revision != RevisionRef::Git(git).to_string()
    {
        return Err(Error::ArchiveArtifactMismatch);
    }
    Ok((native, git))
}

fn check_patch(
    store: &TrackedSource<'_, impl ObjectStore>,
    history: &History<'_, impl ObjectStore>,
    base: ObjectId,
    head: ObjectId,
    patch: &ArchiveProposalPatch,
    replay_bytes: &Cell<usize>,
) -> Result<Option<(ObjectId, u64)>> {
    let bases = history.merge_bases(base, head)?;
    if let [id] = bases.as_slice() {
        let expected_base = archive_proposal_tip(history, *id)?;
        let base = history.commit(*id)?.snapshot;
        let head = history.commit(head)?.snapshot;
        let delta = exact_delta(store, base, head)?;
        let frame = Object::ChangeOperations(delta.clone());
        let id = frame.id()?;
        if *patch
            != (ArchiveProposalPatch::Ready {
                merge_base: expected_base,
                operations: id.to_string(),
            })
            || store.get(id)? != frame
        {
            return Err(Error::ArchiveArtifactMismatch);
        }
        let replay = Replay {
            source: store,
            deadline: store.deadline,
            bytes_left: replay_bytes,
            objects: RefCell::new(BTreeMap::new()),
        };
        if prepare_change(&replay, base, &delta)?.result_snapshot() != head {
            return Err(Error::ArchiveArtifactMismatch);
        }
        return Ok(Some((id, frame.encode()?.len() as u64)));
    }
    let expected = ArchiveProposalPatch::NeedsResolution {
        reason: if bases.is_empty() {
            ArchiveProposalResolution::UnrelatedHistory
        } else {
            ArchiveProposalResolution::MultipleMergeBases
        },
        merge_bases: bases
            .into_iter()
            .map(|id| archive_proposal_tip(history, id))
            .collect::<treeharbor_core::Result<_>>()?,
    };
    if *patch != expected {
        return Err(Error::ArchiveArtifactMismatch);
    }
    Ok(None)
}

// Intermediate directories from applying several operations are private and
// need not be part of the retained source graph. Bound the writes across all
// proposals, and discard the intermediates after each exact replay.
struct Replay<'a, S> {
    source: &'a S,
    deadline: Instant,
    bytes_left: &'a Cell<usize>,
    objects: RefCell<BTreeMap<ObjectId, Object>>,
}
impl<S: ObjectStore> ObjectStore for Replay<'_, S> {
    fn get(&self, id: ObjectId) -> treeharbor_core::Result<Object> {
        if let Some(object) = self.objects.borrow().get(&id) {
            return Ok(object.clone());
        }
        self.source.get(id)
    }

    fn put(&self, object: &Object) -> treeharbor_core::Result<ObjectId> {
        if Instant::now() >= self.deadline {
            return Err(treeharbor_core::Error::TooLarge);
        }
        let frame = object.encode()?;
        self.bytes_left.set(
            self.bytes_left
                .get()
                .checked_sub(frame.len())
                .ok_or(treeharbor_core::Error::TooLarge)?,
        );
        let id = ObjectId::hash_frame(&frame);
        self.objects
            .borrow_mut()
            .entry(id)
            .or_insert_with(|| object.clone());
        Ok(id)
    }
}

pub(super) struct VerifiedProposals {
    pub(super) roots: BTreeSet<ObjectId>,
    pub(super) count: usize,
    pub(super) ready: usize,
    pub(super) proposals: Vec<ArchiveProposal>,
}

/// Compare with the exact manifest read from retained storage. The source
/// adapter must restrict reads to the uploaded generations.
pub(super) fn verify_proposals(
    source: &impl ObjectStore,
    manifest: ArchiveProposalManifest,
    archive: &ArchiveArtifactManifest<Upstream>,
    archive_fingerprint: &str,
    uploaded: &BTreeSet<ObjectId>,
    deadline: Instant,
) -> Result<VerifiedProposals> {
    let limits = Limits::default();
    let mut pulls: Vec<_> = archive.inventory.open_pulls.iter().collect();
    pulls.sort_unstable_by_key(|pull| pull.number);
    if manifest.format != ARCHIVE_PROPOSALS_FORMAT
        || manifest.version != 1
        || manifest.archive_manifest_sha256 != archive_fingerprint
        || manifest.archive_bundle_sha256 != archive.bundle.sha256
        || manifest.observation != archive.receipt.request
        || manifest.upstream != archive.receipt.observation.upstream
        || manifest.proposals.len() != pulls.len()
        || manifest.objects.len() != uploaded.len()
        || manifest.objects.len() > limits.objects
        || manifest.encoded_bytes > limits.source_bytes
    {
        return Err(Error::ArchiveArtifactMismatch);
    }
    let mut declared = BTreeSet::new();
    for object in &manifest.objects {
        let id: ObjectId = object.parse()?;
        if id.to_string() != *object || !declared.insert(id) {
            return Err(Error::ArchiveArtifactMismatch);
        }
    }
    if &declared != uploaded {
        return Err(Error::ArchiveArtifactMismatch);
    }

    let store = TrackedSource {
        store: source,
        deadline,
        reads_left: Cell::new(limits.path_visits),
        seen: RefCell::new(BTreeMap::new()),
    };
    let history = History::new(&store, limits.objects);
    let mut tips = BTreeMap::new();
    let mut pairs = Vec::with_capacity(pulls.len());
    for (proposal, pull) in manifest.proposals.iter().zip(pulls) {
        if proposal.source != *pull
            || proposal.discussion
                != format!(
                    "https://github.com/{}/{}/pull/{}",
                    manifest.upstream.owner, manifest.upstream.name, pull.number
                )
            || proposal.target_present
                != archive
                    .receipt
                    .observation
                    .heads
                    .contains_key(&format!("refs/heads/{}", pull.base_ref_name))
        {
            return Err(Error::ArchiveArtifactMismatch);
        }
        let (base_label, head_label) = archive_proposal_labels(pull.number);
        let base = claimed_tip(&proposal.base_tip, &pull.base_ref_oid, &history)?;
        let head = claimed_tip(&proposal.head, &pull.head_ref_oid, &history)?;
        tips.insert(base_label, base);
        tips.insert(head_label, head);
        pairs.push((base.0, head.0));
    }
    let mut roots: BTreeSet<_> = tips.values().map(|(native, _)| *native).collect();
    let mut bytes = 0;
    if !tips.is_empty() {
        let proof = verify_archive_commit_mappings(
            &store,
            tips,
            Limits {
                timeout: deadline.saturating_duration_since(Instant::now()),
                ..limits
            },
        )
        .map_err(|error| match error {
            treeharbor_git_bridge::Error::Limit => Error::UploadLimit,
            treeharbor_git_bridge::Error::Source(error) => Error::Objects(error),
            _ => Error::ArchiveArtifactMismatch,
        })?;
        bytes = proof.source_bytes();
        verify_imported_deltas(&store, limits.objects)?;
    }
    let mut ready = 0;
    let replay_bytes = Cell::new(128 << 20);
    for (proposal, (base, head)) in manifest.proposals.iter().zip(pairs) {
        let closure_seen = store.seen.borrow().len();
        if let Some((patch, length)) =
            check_patch(&store, &history, base, head, &proposal.patch, &replay_bytes)?
        {
            roots.insert(patch);
            if store.seen.borrow().len() > closure_seen {
                bytes += length;
            }
            ready += usize::from(proposal.target_present);
        }
    }
    if bytes != manifest.encoded_bytes || !store.seen.into_inner().into_keys().eq(declared) {
        return Err(Error::ArchiveArtifactMismatch);
    }
    Ok(VerifiedProposals {
        roots,
        count: manifest.proposals.len(),
        ready,
        proposals: manifest.proposals,
    })
}
