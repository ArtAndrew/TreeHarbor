//! Native snapshot computation, not publication or validation authority.
//! No Git process, checkout, repository configuration, hooks or merge drivers.
//! See docs/native-merge.md for the deliberately bounded compatibility profile.

mod candidate;
mod engine;
mod renames;

use treeharbor_core::{Object, ObjectId, ObjectStore, RepoPath};
use treeharbor_history::History;

pub use candidate::{CandidateInput, CandidateOutcome, PreparedCandidate, prepare_candidate};
pub use candidate::{PreparedRebase, REBASE_PROFILE, RebaseOutcome, prepare_rebase};
pub use engine::merge_snapshots;

/// A behavior change or merge-library upgrade requires a new profile identity.
pub const PROFILE: &str =
    "treeharbor-native-merge-v2/diffy-0.5.2/exact-sibling-renames/subtree-delta";

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_commits: usize,
    pub max_directories: usize,
    pub max_entries: usize,
    pub max_path_bytes: usize,
    pub max_read_bytes: usize,
    pub max_write_bytes: usize,
    pub max_text_bytes: usize,
    /// Sum of lines in all three text inputs, including an unterminated last line.
    pub max_text_lines: usize,
    /// Conservative square of the total line count, charged across all merges.
    pub max_diff_work: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_commits: 10_000,
            max_directories: 20_000,
            max_entries: 200_000,
            max_path_bytes: 32 * 1024 * 1024,
            max_read_bytes: 128 * 1024 * 1024,
            max_write_bytes: 128 * 1024 * 1024,
            max_text_bytes: 1024 * 1024,
            max_text_lines: 12_000,
            max_diff_work: 32_000_000,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unsupported {
    Attributes,
    EmptyDirectory(RepoPath),
    AmbiguousRename,
    CrossDirectoryRename,
    /// A side deleted and added unmatched leaves. An edited rename cannot be
    /// distinguished from unrelated delete/add without another merge profile.
    PossibleEditedRename,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Core(#[from] treeharbor_core::Error),
    #[error("merge resource limit exceeded: {0}")]
    Limit(&'static str),
    #[error("unsupported native merge case: {0:?}")]
    Unsupported(Unsupported),
    #[error("multiple best merge bases require virtual-base synthesis")]
    MultipleBases(Vec<ObjectId>),
    #[error("histories have no common ancestor")]
    UnrelatedHistories,
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictKind {
    Content,
    Binary,
    Symlink,
    Type,
    Mode,
    ModifyDelete,
    RenameDelete,
    RenameRename,
    DestinationCollision,
    FileDirectory,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    pub path: RepoPath,
    pub related_path: Option<RepoPath>,
    pub kind: ConflictKind,
    pub base: Option<treeharbor_core::EntryRef>,
    pub current: Option<treeharbor_core::EntryRef>,
    pub proposed: Option<treeharbor_core::EntryRef>,
}

/// Constructed by computation over exact inputs. It does not acknowledge durable
/// storage, trusted checks, a native commit, or permission to update a branch.
#[derive(Clone, Debug)]
pub struct MergedSnapshot {
    base: ObjectId,
    current: ObjectId,
    proposed: ObjectId,
    result: ObjectId,
}

impl MergedSnapshot {
    pub fn base(&self) -> ObjectId {
        self.base
    }
    pub fn current(&self) -> ObjectId {
        self.current
    }
    pub fn proposed(&self) -> ObjectId {
        self.proposed
    }
    pub fn result(&self) -> ObjectId {
        self.result
    }
    pub fn profile(&self) -> &'static str {
        PROFILE
    }
}

#[derive(Clone, Debug)]
pub enum Outcome {
    Clean(MergedSnapshot),
    Conflicted(Vec<Conflict>),
}

/// Find every best common ancestor before merging. Multiple bases are an explicit
/// error, including when the tips happen to contain the same snapshot. Never pick
/// one base by timestamp, digest order or first-parent traversal.
pub fn merge_revisions(
    store: &impl ObjectStore,
    current: ObjectId,
    proposed: ObjectId,
    limits: Limits,
) -> Result<Outcome> {
    let reader = HistoryReader {
        store,
        remaining: std::cell::Cell::new(limits.max_read_bytes),
    };
    let history = History::new(&reader, limits.max_commits);
    let bases = history.merge_bases(current, proposed)?;
    let base = match bases.as_slice() {
        [] => return Err(Error::UnrelatedHistories),
        [base] => *base,
        _ => return Err(Error::MultipleBases(bases)),
    };
    let [base, current, proposed] = [
        history.commit(base)?.snapshot,
        history.commit(current)?.snapshot,
        history.commit(proposed)?.snapshot,
    ];
    merge_snapshots(
        store,
        base,
        current,
        proposed,
        Limits {
            max_read_bytes: reader.remaining.get(),
            ..limits
        },
    )
}

struct HistoryReader<'a, S> {
    store: &'a S,
    remaining: std::cell::Cell<usize>,
}

impl<S: ObjectStore> ObjectStore for HistoryReader<'_, S> {
    fn get(&self, id: ObjectId) -> treeharbor_core::Result<Object> {
        let object = self.store.get(id)?;
        let frame = object.encode()?;
        self.remaining.set(
            self.remaining
                .get()
                .checked_sub(frame.len())
                .ok_or(treeharbor_core::Error::TooLarge)?,
        );
        if ObjectId::hash_frame(&frame) != id {
            return Err(treeharbor_core::Error::InvalidObject(
                "wrong merge history identity",
            ));
        }
        Ok(object)
    }
    fn put(&self, _: &Object) -> treeharbor_core::Result<ObjectId> {
        Err(treeharbor_core::Error::Storage(
            "read-only merge history".into(),
        ))
    }
}

fn checked_put(store: &impl ObjectStore, object: &Object) -> Result<ObjectId> {
    let id = store.put(object)?;
    if object.id()? != id {
        return Err(treeharbor_core::Error::InvalidObject("wrong merge write identity").into());
    }
    Ok(id)
}
