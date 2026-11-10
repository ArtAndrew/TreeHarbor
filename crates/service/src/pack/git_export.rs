use super::{Error, Limits, MAX_PACK_BYTES, MAX_PACK_OBJECTS, Pack};
use std::{
    collections::{BTreeMap, btree_map::Entry},
    path::{Path, PathBuf},
    time::Instant,
};
use treeharbor_core::{Object, ObjectId, ObjectStore};
use treeharbor_git_bridge as git_bridge;
use treeharbor_storage::LocalStore;

/// Read only the verified members of the input packs. Shared objects use one
/// locator. An invalid pack cannot borrow missing members from another pack.
#[derive(Default)]
struct PackObjects {
    stores: Vec<LocalStore>,
    locations: BTreeMap<ObjectId, usize>,
}

impl ObjectStore for PackObjects {
    fn get(&self, id: ObjectId) -> treeharbor_core::Result<Object> {
        let store = self
            .locations
            .get(&id)
            .ok_or(treeharbor_core::Error::MissingObject(id))?;
        self.stores[*store].get(id)
    }

    fn put(&self, _: &Object) -> treeharbor_core::Result<ObjectId> {
        Err(treeharbor_core::Error::InvalidObject(
            "read-only export packs",
        ))
    }
}

pub(crate) fn export(
    branches: BTreeMap<String, PathBuf>,
    git: &Path,
    output: &Path,
    algorithm: git_bridge::HashAlgorithm,
    mut limits: git_bridge::Limits,
) -> crate::Result<git_bridge::Manifest> {
    if branches.is_empty() || branches.len() > git_bridge::MAX_EXPORT_REFS {
        return Err(git_bridge::Error::Configuration.into());
    }
    let deadline = Instant::now() + limits.timeout;
    let mut input_budget = Limits {
        objects: MAX_PACK_OBJECTS,
        bytes: MAX_PACK_BYTES,
    };
    let mut objects = PackObjects::default();
    let mut tips = BTreeMap::new();
    let mut refs = BTreeMap::new();
    for (name, path) in branches {
        if Instant::now() >= deadline {
            return Err(git_bridge::Error::Limit.into());
        }
        let directory = crate::files::private_directory(&path).map_err(|_| Error::Output)?;
        let tip = match tips.entry(directory) {
            Entry::Occupied(tip) => *tip.get(),
            Entry::Vacant(tip) => {
                let pack = Pack::open_private(tip.key().clone(), input_budget, Some(deadline))?;
                input_budget.objects -= pack.ids.len() as u64;
                input_budget.bytes -= pack.bytes;
                let store = objects.stores.len();
                for id in pack.ids {
                    objects.locations.entry(id).or_insert(store);
                }
                objects.stores.push(pack.store);
                *tip.insert(pack.commit)
            }
        };
        refs.insert(name, tip);
    }
    limits.timeout = deadline.saturating_duration_since(Instant::now());
    if limits.timeout.is_zero() {
        return Err(git_bridge::Error::Limit.into());
    }
    Ok(git_bridge::export(
        &objects,
        git_bridge::Export {
            git,
            output,
            refs,
            algorithm,
            limits,
        },
    )?)
}
