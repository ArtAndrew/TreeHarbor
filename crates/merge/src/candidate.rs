use crate::{Conflict, Error, Limits, Outcome, Result, checked_put, merge_snapshots};
use std::cell::Cell;
use treeharbor_core::{Commit, Object, ObjectId, ObjectStore, prepare_change};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateInput {
    pub base_revision: ObjectId,
    pub base_snapshot: ObjectId,
    pub original_operations: ObjectId,
    pub proposed_snapshot: ObjectId,
    pub current_revision: ObjectId,
    pub current_snapshot: ObjectId,
    pub timestamp_seconds: i64,
    /// Stable service identity, normally queue entry and immutable change version.
    pub label: String,
}

#[derive(Clone, Debug)]
pub struct PreparedCandidate {
    input: CandidateInput,
    proposal: ObjectId,
    commit: Commit,
    revision: ObjectId,
}
impl PreparedCandidate {
    pub fn input(&self) -> &CandidateInput {
        &self.input
    }
    pub fn proposal_revision(&self) -> ObjectId {
        self.proposal
    }
    pub fn commit(&self) -> &Commit {
        &self.commit
    }
    pub fn revision(&self) -> ObjectId {
        self.revision
    }
    pub fn profile(&self) -> &'static str {
        crate::PROFILE
    }
}
pub enum CandidateOutcome {
    Ready(Box<PreparedCandidate>),
    Conflicted(Vec<Conflict>),
}
struct Budget<'a, S> {
    store: &'a S,
    read: Cell<usize>,
    write: Cell<usize>,
}
impl<S: ObjectStore> ObjectStore for Budget<'_, S> {
    fn get(&self, id: ObjectId) -> treeharbor_core::Result<Object> {
        let object = self.store.get(id)?;
        let frame = object.encode()?;
        self.read.set(
            self.read
                .get()
                .checked_sub(frame.len())
                .ok_or(treeharbor_core::Error::TooLarge)?,
        );
        if ObjectId::hash_frame(&frame) != id {
            return Err(treeharbor_core::Error::InvalidObject(
                "candidate read identity",
            ));
        }
        Ok(object)
    }
    fn put(&self, object: &Object) -> treeharbor_core::Result<ObjectId> {
        let frame = object.encode()?;
        self.write.set(
            self.write
                .get()
                .checked_sub(frame.len())
                .ok_or(treeharbor_core::Error::TooLarge)?,
        );
        let id = self.store.put(object)?;
        if ObjectId::hash_frame(&frame) != id {
            return Err(treeharbor_core::Error::InvalidObject(
                "candidate write identity",
            ));
        }
        Ok(id)
    }
}

/// Computes both proposal and integration commits. The original proposal is
/// re-applied to its exact base; the integration delta is recomputed against the
/// current target. No client-supplied result, parent list or green status is used.
/// Keep all inputs pinned and stage generated objects before durable acceptance.
pub fn prepare_candidate(
    store: &impl ObjectStore,
    input: &CandidateInput,
    limits: Limits,
) -> Result<CandidateOutcome> {
    if input.label.is_empty()
        || input.label.len() > 256
        || !input
            .label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b":/-_".contains(&b))
    {
        return Err(treeharbor_core::Error::InvalidObject("candidate label").into());
    }
    let store = Budget {
        store,
        read: Cell::new(limits.max_read_bytes),
        write: Cell::new(limits.max_write_bytes),
    };
    for (revision, snapshot) in [
        (input.base_revision, input.base_snapshot),
        (input.current_revision, input.current_snapshot),
    ] {
        let Object::NativeCommit(commit) = store.get(revision)? else {
            return Err(treeharbor_core::Error::WrongKind.into());
        };
        if commit.snapshot != snapshot {
            return Err(treeharbor_core::Error::PredicateMismatch.into());
        }
    }
    let Object::ChangeOperations(operations) = store.get(input.original_operations)? else {
        return Err(treeharbor_core::Error::WrongKind.into());
    };
    let original = prepare_change(&store, input.base_snapshot, &operations)?;
    if original.result_snapshot() != input.proposed_snapshot {
        return Err(treeharbor_core::Error::PredicateMismatch.into());
    }
    let merged = match merge_snapshots(
        &store,
        input.base_snapshot,
        input.current_snapshot,
        input.proposed_snapshot,
        limits,
    )? {
        Outcome::Conflicted(conflicts) => return Ok(CandidateOutcome::Conflicted(conflicts)),
        Outcome::Clean(merged) => merged,
    };
    let delta =
        crate::engine::snapshot_delta(&store, input.current_snapshot, merged.result(), limits)?;
    let applied = prepare_change(&store, input.current_snapshot, &delta)?;
    if applied.result_snapshot() != merged.result() {
        return Err(Error::Core(treeharbor_core::Error::PredicateMismatch));
    }
    let commit = |parents, snapshot, changes, message| Commit {
        parents,
        snapshot,
        changes,
        author_name: "TreeHarbor".into(),
        author_email: "integration@treeharbor.invalid".into(),
        timestamp_seconds: input.timestamp_seconds,
        timezone_minutes: 0,
        message,
    };
    let proposal = commit(
        vec![input.base_revision],
        input.proposed_snapshot,
        input.original_operations,
        format!("Propose {}\nMerge profile: {}", input.label, crate::PROFILE),
    );
    let proposal = checked_put(&store, &Object::NativeCommit(proposal))?;
    let parents = if input.current_revision == proposal {
        vec![proposal]
    } else {
        vec![input.current_revision, proposal]
    };
    let commit = commit(
        parents,
        merged.result(),
        applied.operations(),
        format!(
            "Integrate {}\nMerge profile: {}",
            input.label,
            crate::PROFILE
        ),
    );
    let revision = checked_put(&store, &Object::NativeCommit(commit.clone()))?;
    Ok(CandidateOutcome::Ready(Box::new(PreparedCandidate {
        input: input.clone(),
        proposal,
        commit,
        revision,
    })))
}
