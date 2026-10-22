//! Immutable source objects. No filesystem, Git, network, or publication authority.

mod codec;
mod delta;
mod identity;
mod object;
mod path;
mod profile;
mod proposal;
mod tree;

pub use delta::{TreeDiffLimits, diff_snapshots};
pub use identity::{GitOid, ObjectId, RevisionRef};
pub use object::{ChangeOperation, Changes, Commit, Directory, Entry, EntryMode, EntryRef, Object};
pub use path::{Name, RepoPath};
pub use profile::{MAX_PROFILE_BYTES, MAX_PROFILE_PATHS, ProfileId, SourceProfile};
pub use proposal::{AppliedChange, prepare_change};
pub use tree::{apply_changes, lookup, validate_graph};

pub const MAX_PAYLOAD: usize = 64 * 1024 * 1024;
pub const MAX_ENTRIES: usize = 100_000;
pub const MAX_PARENTS: usize = 1_024;
pub const FRAME_HEADER: usize = 22;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid object identifier")]
    InvalidId,
    #[error("invalid repository path: {0}")]
    InvalidPath(&'static str),
    #[error("invalid canonical object: {0}")]
    InvalidObject(&'static str),
    #[error("unsupported object format version {0}")]
    UnsupportedVersion(u16),
    #[error("object exceeds decoder limits")]
    TooLarge,
    #[error("object is missing: {0}")]
    MissingObject(ObjectId),
    #[error("object has the wrong kind")]
    WrongKind,
    #[error("before-state predicate does not match the base tree")]
    PredicateMismatch,
    #[error("duplicate or overlapping operation paths")]
    OverlappingPaths,
    #[error("object storage failure: {0}")]
    Storage(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Implementations return verified immutable objects. This interface does not
/// grant publication rights or promise that a root has a metadata pin.
pub trait ObjectStore {
    fn get(&self, id: ObjectId) -> Result<Object>;
    fn put(&self, object: &Object) -> Result<ObjectId>;
}
