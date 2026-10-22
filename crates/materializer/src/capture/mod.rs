//! Bounded immutable capture. This library does NOT prove runtime quiescence,
//! authorize a node, acknowledge a seal or submit an accepted ChangeSet.
mod filesystem;
mod recovery;
use crate::{Error, Limits, MaterializedTree, Publication, PublicationReceipt, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
use treeharbor_core::{
    CaptureSelection, ChangeOperation, Changes, EntryMode, EntryRef, Object, ObjectId, ObjectStore,
    RepoPath,
};
use treeharbor_metadata::Scope;
use treeharbor_storage::{LocalStore, TenantId};

const MAX_MANIFEST: u64 = 16 * 1024 * 1024;
const TENANT: &str = "capture-pack";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u8,
    tenant: String,
    repository: String,
    base_snapshot: String,
    selection_hex: String,
    operations: String,
    objects: Vec<String>,
    baseline_dependencies: Vec<(String, u8)>,
    encoded_bytes: u64,
    filesystem_entries: usize,
    filesystem_bytes: u64,
    path_bytes: u64,
}
/// Supplied by the trusted controller from its durable request, not from this
/// artifact's unverified manifest. It is still not proof of stopped writers.
pub struct CaptureExpectation<'a> {
    pub scope: &'a Scope,
    pub snapshot: ObjectId,
    pub selection: &'a CaptureSelection,
    pub binding: [u8; 32],
}
pub struct CaptureArtifact {
    root: PathBuf,
    store: LocalStore,
    digest: [u8; 32],
    changes: Changes,
    objects: Vec<ObjectId>,
    dependencies: BTreeMap<ObjectId, u8>,
}
impl CaptureArtifact {
    pub fn path(&self) -> &Path {
        &self.root
    }
    pub fn manifest_digest(&self) -> [u8; 32] {
        self.digest
    }
    pub fn operations(&self) -> &Changes {
        &self.changes
    }
    /// Canonical captured objects in child-first order, followed by operations.
    pub fn objects(&self) -> &[ObjectId] {
        &self.objects
    }
    /// These exact original-base descendants require scoped source membership
    /// and retention at publication; a digest alone never grants that membership.
    pub fn baseline_dependencies(&self) -> &BTreeMap<ObjectId, u8> {
        &self.dependencies
    }
    pub fn object(&self, id: ObjectId) -> Result<Object> {
        if !self.objects.contains(&id) {
            return Err(Error::InvalidBaseline);
        }
        Ok(self.store.get(id)?)
    }
}
struct Builder<'a> {
    tree: &'a MaterializedTree,
    selection: &'a CaptureSelection,
    store: LocalStore,
    limits: Limits,
    device: u64,
    ids: Vec<ObjectId>,
    seen: BTreeSet<ObjectId>,
    dependencies: BTreeMap<ObjectId, u8>,
    encoded: u64,
    entries: usize,
    path_bytes: u64,
    bytes: u64,
}
impl Builder<'_> {
    fn put(&mut self, object: Object) -> Result<ObjectId> {
        let frame = object.encode()?;
        let id = ObjectId::hash_frame(&frame);
        if self.seen.contains(&id) {
            return Ok(id);
        }
        if self.ids.len() >= self.limits.max_entries.saturating_add(1)
            || frame.len() as u64 > self.limits.max_source_bytes.saturating_sub(self.encoded)
        {
            return Err(Error::Limit);
        }
        self.store.put(&object)?;
        self.encoded += frame.len() as u64;
        self.seen.insert(id);
        self.ids.push(id);
        Ok(id)
    }
    fn dependency(&mut self, value: Option<EntryRef>) -> Result<Option<EntryRef>> {
        if let Some(value) = value {
            let kind = if value.mode == EntryMode::Directory {
                2
            } else {
                1
            };
            if self
                .dependencies
                .insert(value.target, kind)
                .is_some_and(|old| old != kind)
            {
                return Err(Error::InvalidBaseline);
            }
        }
        Ok(value)
    }
    fn baseline(&self, path: &RepoPath) -> Option<EntryRef> {
        self.tree.manifest.entries.get(path).copied()
    }
    fn visit(&mut self, path: &RepoPath) -> Result<()> {
        self.entries = self.entries.checked_add(1).ok_or(Error::Limit)?;
        self.path_bytes = self
            .path_bytes
            .checked_add(path.as_bytes().len() as u64)
            .ok_or(Error::Limit)?;
        if self.entries > self.limits.max_entries || self.path_bytes > self.limits.max_path_bytes {
            return Err(Error::Limit);
        }
        Ok(())
    }
    fn order(&mut self, root: ObjectId) -> Result<()> {
        // A preserved baseline digest can also be captured at a later selected
        // path. Resolve the final graph before assigning child-first order.
        let mut done = BTreeSet::new();
        let mut active = BTreeSet::new();
        let mut pending = vec![(root, false)];
        let mut order = Vec::new();
        while let Some((id, visited)) = pending.pop() {
            if done.contains(&id) || self.dependencies.contains_key(&id) {
                continue;
            }
            if !self.seen.contains(&id) {
                return Err(Error::InvalidBaseline);
            }
            if visited {
                active.remove(&id);
                done.insert(id);
                order.push(id);
            } else {
                if !active.insert(id) {
                    return Err(Error::InvalidBaseline);
                }
                let refs = self.store.get(id)?.references();
                pending.push((id, true));
                pending.extend(refs.into_iter().rev().map(|(child, _)| (child, false)));
            }
        }
        if done != self.seen {
            return Err(Error::InvalidBaseline);
        }
        self.ids = order;
        Ok(())
    }
}
impl MaterializedTree {
    /// Blocking IO. The caller must prove and retain exclusion of ALL writers,
    /// including stopped runtime/slot identity, until publication completes.
    /// `publication` owns the caller's guard even if its async task is abandoned.
    /// Never invoke this on live files and call its result an atomic workspace.
    pub fn capture_selected(
        &self,
        selection: &CaptureSelection,
        publication: &Publication,
        limits: Limits,
    ) -> Result<CaptureArtifact> {
        let limits = limits.validate()?;
        // An ancestor of a sparse root is NOT a fully delivered subtree.
        for path in selection.replace().iter().chain(selection.delete()) {
            if !self
                .manifest
                .selection
                .paths
                .as_ref()
                .is_none_or(|roots| roots.iter().any(|p| p == path || p.is_ancestor_of(path)))
            {
                return Err(Error::InvalidBaseline);
            }
            // A path beneath an original leaf cannot be inferred absent.
            for (i, b) in path.as_bytes().iter().enumerate() {
                if *b == b'/' {
                    let parent = RepoPath::new(path.as_bytes()[..i].to_vec())?;
                    if self
                        .manifest
                        .entries
                        .get(&parent)
                        .is_some_and(|p| p.mode != EntryMode::Directory)
                    {
                        return Err(Error::CaptureConflict);
                    }
                }
            }
        }
        let input = filesystem::Root::open(&self.path())?;
        let parent = publication
            .destination()
            .parent()
            .ok_or(Error::Configuration)?;
        crate::publication::directory(parent)?;
        let parent = parent.canonicalize()?;
        if parent.starts_with(self.path().canonicalize()?) {
            return Err(Error::Configuration);
        }
        let stage = tempfile::Builder::new()
            .prefix("capture-")
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(parent)?;
        let store = LocalStore::open(stage.path(), &TenantId::new(TENANT)?)?;
        let mut b = Builder {
            tree: self,
            selection,
            store,
            limits,
            device: input.device(),
            ids: Vec::new(),
            seen: BTreeSet::new(),
            dependencies: BTreeMap::new(),
            encoded: 0,
            entries: 0,
            path_bytes: 0,
            bytes: 0,
        };
        let mut operations = Vec::new();
        for path in selection.replace() {
            let expected = b.dependency(b.baseline(path))?;
            let replacement = b.capture_path(&input.file, path)?;
            operations.push(ChangeOperation {
                path: path.clone(),
                expected,
                replacement: Some(replacement),
            });
        }
        for path in selection.delete() {
            b.visit(path)?;
            input.confirm_absent(path)?;
            let expected = b
                .dependency(b.baseline(path))?
                .ok_or(Error::CaptureConflict)?;
            operations.push(ChangeOperation {
                path: path.clone(),
                expected: Some(expected),
                replacement: None,
            });
        }
        let changes = Changes::with_subtrees(operations)?;
        let operations = b.put(Object::ChangeOperations(changes))?;
        b.dependencies.retain(|id, _| !b.seen.contains(id));
        b.order(operations)?;
        let manifest = Manifest {
            version: 1,
            tenant: self.manifest.tenant.clone(),
            repository: self.manifest.repository.clone(),
            base_snapshot: self.snapshot().to_string(),
            selection_hex: hex(&selection.encode()),
            operations: operations.to_string(),
            objects: b.ids.iter().map(ToString::to_string).collect(),
            baseline_dependencies: b
                .dependencies
                .iter()
                .map(|(id, kind)| (id.to_string(), *kind))
                .collect(),
            encoded_bytes: b.encoded,
            filesystem_entries: b.entries,
            filesystem_bytes: b.bytes,
            path_bytes: b.path_bytes,
        };
        let bytes = serde_json::to_vec(&manifest)?;
        if bytes.len() as u64 > MAX_MANIFEST {
            return Err(Error::Limit);
        }
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(stage.path().join("manifest.json"))?;
        output.write_all(&bytes)?;
        output.sync_all()?;
        File::open(stage.path())?.sync_all()?;
        input.confirm()?;
        let published = publication.publish(stage.path())?;
        let scope = Scope::new(
            &self.manifest.tenant,
            self.manifest
                .repository
                .parse()
                .map_err(|_| Error::InvalidBaseline)?,
        )?;
        CaptureArtifact::open(
            &published,
            CaptureExpectation {
                scope: &scope,
                snapshot: self.snapshot(),
                selection,
                binding: publication.binding(),
            },
            limits,
        )
    }
}
fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 15) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests;
