use super::*;
use sha2::{Digest, Sha256};
use std::io::Read;
impl CaptureArtifact {
    /// Reopen without consulting mutable agent files. Metadata authorization,
    /// runtime/slot fencing and base-dependency membership remain caller duties.
    pub fn open(root: &Path, expected: CaptureExpectation<'_>, limits: Limits) -> Result<Self> {
        let limits = limits.validate()?;
        let publication = PublicationReceipt::load(root, expected.binding)?;
        let directory = crate::publication::directory(root)?;
        let input = crate::publication::read_file(&directory, "manifest.json")?;
        if input.metadata()?.len() > MAX_MANIFEST {
            return Err(Error::Limit);
        }
        let mut bytes = Vec::new();
        input.take(MAX_MANIFEST + 1).read_to_end(&mut bytes)?;
        let digest: [u8; 32] = Sha256::digest(&bytes).into();
        if bytes.len() as u64 > MAX_MANIFEST || digest != publication.manifest_digest()? {
            return Err(Error::InvalidBaseline);
        }
        let manifest: Manifest = serde_json::from_slice(&bytes)?;
        if serde_json::to_vec(&manifest)? != bytes
            || manifest.version != 1
            || manifest.tenant != expected.scope.tenant()
            || manifest.repository != expected.scope.repository().to_string()
            || manifest.base_snapshot != expected.snapshot.to_string()
            || manifest.selection_hex != hex(&expected.selection.encode())
        {
            return Err(Error::InvalidBaseline);
        }
        if manifest.objects.len() > limits.max_entries.saturating_add(1)
            || manifest.baseline_dependencies.len() > treeharbor_core::MAX_CAPTURE_PATHS * 2
            || manifest.encoded_bytes > limits.max_source_bytes
            || manifest.filesystem_entries > limits.max_entries
            || manifest.filesystem_bytes > limits.max_workspace_bytes
            || manifest.path_bytes > limits.max_path_bytes
        {
            return Err(Error::Limit);
        }
        // Existing-only read: do not turn an incomplete/corrupt artifact into a
        // new empty object store as a side effect of reopening.
        for p in [
            "objects",
            "objects/capture-pack",
            "staging",
            "staging/capture-pack",
        ] {
            crate::publication::directory(&root.join(p))?;
        }
        let store = LocalStore::open(root, &TenantId::new(TENANT)?)?;
        let operations = object_id(&manifest.operations)?;
        let mut dependencies = BTreeMap::new();
        let mut previous = None;
        for (text, kind) in &manifest.baseline_dependencies {
            let id = object_id(text)?;
            if !matches!(kind, 1 | 2) || previous.is_some_and(|p| p >= id) {
                return Err(Error::InvalidBaseline);
            }
            previous = Some(id);
            dependencies.insert(id, *kind);
        }
        let mut objects = Vec::new();
        let mut seen = BTreeMap::new();
        let mut edges = BTreeMap::new();
        let mut encoded = 0u64;
        let mut changes = None;
        for text in &manifest.objects {
            let id = object_id(text)?;
            if seen.contains_key(&id) || dependencies.contains_key(&id) {
                return Err(Error::InvalidBaseline);
            }
            let frame = store.open_object(id)?;
            let length = frame.length();
            encoded = encoded.checked_add(length).ok_or(Error::Limit)?;
            if encoded > limits.max_source_bytes {
                return Err(Error::Limit);
            }
            let object = frame.read()?;
            if let Object::Blob(bytes) = &object
                && bytes.len() as u64 > limits.max_file_bytes
            {
                return Err(Error::Limit);
            }
            let kind = object.kind();
            if id == operations {
                let Object::ChangeOperations(value) = &object else {
                    return Err(Error::InvalidBaseline);
                };
                changes = Some(value.clone());
            } else if !matches!(kind, 1 | 2) {
                return Err(Error::InvalidBaseline);
            }
            let refs = object.references();
            for (child, expected_kind) in &refs {
                if seen.get(child).or_else(|| dependencies.get(child)) != Some(expected_kind) {
                    return Err(Error::InvalidBaseline);
                }
            }
            seen.insert(id, kind);
            edges.insert(id, refs);
            objects.push(id);
        }
        if encoded != manifest.encoded_bytes || objects.last() != Some(&operations) {
            return Err(Error::InvalidBaseline);
        }
        let changes = changes.ok_or(Error::InvalidBaseline)?;
        let paths: BTreeSet<_> = expected
            .selection
            .replace()
            .iter()
            .chain(expected.selection.delete())
            .collect();
        if changes.operations().len() != paths.len() {
            return Err(Error::InvalidBaseline);
        }
        for op in changes.operations() {
            if !paths.contains(&op.path)
                || op.replacement.is_some() != expected.selection.replace().contains(&op.path)
            {
                return Err(Error::InvalidBaseline);
            }
        }
        // Reject hidden unused objects/dependencies, not merely valid hashes.
        let mut reachable = BTreeSet::new();
        let mut pending = vec![operations];
        while let Some(id) = pending.pop() {
            if reachable.insert(id)
                && let Some(refs) = edges.get(&id)
            {
                pending.extend(refs.iter().map(|(child, _)| *child));
            }
        }
        if reachable.len() != objects.len() + dependencies.len() {
            return Err(Error::InvalidBaseline);
        }
        Ok(Self {
            root: root.canonicalize()?,
            store,
            digest,
            changes,
            objects,
            dependencies,
        })
    }
}
fn object_id(text: &str) -> Result<ObjectId> {
    let id: ObjectId = text.parse()?;
    if id.to_string() != text {
        return Err(Error::InvalidBaseline);
    }
    Ok(id)
}
