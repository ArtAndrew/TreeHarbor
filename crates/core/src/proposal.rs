use crate::{Changes, Error, Object, ObjectId, ObjectStore, Result, apply_changes};

/// Produced by applying exact operations, never by accepting a caller's claimed
/// result tree. This is a computation receipt, not a durability or trust grant.
#[derive(Clone, Debug)]
pub struct AppliedChange {
    base: ObjectId,
    operations: ObjectId,
    result: ObjectId,
}
impl AppliedChange {
    pub fn base_snapshot(&self) -> ObjectId {
        self.base
    }
    pub fn operations(&self) -> ObjectId {
        self.operations
    }
    pub fn result_snapshot(&self) -> ObjectId {
        self.result
    }
}

/// The store is service-controlled and must verify immutable bytes on reads.
/// Keep the base and all new objects protected until metadata publication.
/// Intermediate writes can remain orphaned on error and do not acknowledge a change.
pub fn prepare_change(
    store: &impl ObjectStore,
    base: ObjectId,
    changes: &Changes,
) -> Result<AppliedChange> {
    let snapshot = store.get(base)?;
    if snapshot.id()? != base {
        return Err(Error::InvalidObject("wrong base snapshot"));
    }
    let Object::Snapshot { root } = snapshot else {
        return Err(Error::WrongKind);
    };
    let result_root = apply_changes(store, root, changes)?;
    let operations_object = Object::ChangeOperations(changes.clone());
    let result_object = Object::Snapshot { root: result_root };
    let operations = store.put(&operations_object)?;
    let result = store.put(&result_object)?;
    if operations != operations_object.id()? || result != result_object.id()? {
        return Err(Error::InvalidObject("store returned wrong write identity"));
    }
    Ok(AppliedChange {
        base,
        operations,
        result,
    })
}
