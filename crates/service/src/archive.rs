//! Bounded native source archive v1; no Git or authorization semantics.
use crate::{
    files,
    pack::{self, Pack},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::Path,
};
use treeharbor_core::{
    EntryMode, FRAME_HEADER, MAX_PAYLOAD, Object, ObjectId, ObjectStore, RepoPath,
};
use treeharbor_storage::{LocalStore, TenantId};

pub const MAGIC: &[u8] = b"treeharbor-source-archive\0\x00\x01";
pub const HEADER: usize = MAGIC.len() + 32 + 32 + 4 + 8;
pub const MAX_OBJECTS: usize = 100_000;
pub const MAX_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub const MAX_ARCHIVE: u64 = HEADER as u64 + MAX_BYTES + MAX_OBJECTS as u64 * 40;
pub const CONTENT_TYPE: &str = "application/vnd.treeharbor.source-archive-v1";
type Result<T> = pack::Result<T>;
fn invalid() -> pack::Error {
    pack::Error::Invalid
}

pub(crate) fn header(
    out: &mut impl Write,
    revision: ObjectId,
    snapshot: ObjectId,
    count: usize,
    bytes: u64,
) -> Result<()> {
    if count == 0 || count > MAX_OBJECTS || bytes > MAX_BYTES {
        return Err(pack::Error::Limit);
    }
    out.write_all(MAGIC)?;
    out.write_all(revision.as_bytes())?;
    out.write_all(snapshot.as_bytes())?;
    out.write_all(&(count as u32).to_be_bytes())?;
    out.write_all(&bytes.to_be_bytes())?;
    Ok(())
}
pub(crate) fn record(out: &mut impl Write, id: ObjectId, frame: &[u8]) -> Result<()> {
    out.write_all(id.as_bytes())?;
    out.write_all(&(frame.len() as u64).to_be_bytes())?;
    out.write_all(frame)?;
    Ok(())
}
fn fixed<const N: usize>(r: &mut impl Read) -> Result<[u8; N]> {
    let mut b = [0; N];
    r.read_exact(&mut b)?;
    Ok(b)
}
pub(crate) type Graph = BTreeMap<ObjectId, (u8, Vec<(ObjectId, u8)>)>;
pub(crate) fn order(graph: &Graph, root: ObjectId) -> Result<Vec<ObjectId>> {
    let mut pending = vec![(root, false)];
    let mut active = BTreeSet::new();
    let mut done = BTreeSet::new();
    let mut ordered = Vec::new();
    while let Some((id, finish)) = pending.pop() {
        if done.contains(&id) {
            continue;
        }
        let (_, edges) = graph.get(&id).ok_or_else(invalid)?;
        if finish {
            active.remove(&id);
            done.insert(id);
            ordered.push(id);
            continue;
        }
        if !active.insert(id) {
            return Err(invalid());
        }
        pending.push((id, true));
        for &(child, kind) in edges {
            if graph.get(&child).map(|v| v.0) != Some(kind) {
                return Err(invalid());
            }
            pending.push((child, false));
        }
    }
    if done.len() != graph.len() {
        return Err(invalid());
    }
    Ok(ordered)
}
fn entries(store: &LocalStore, snapshot: ObjectId) -> Result<u64> {
    let Object::Snapshot { root } = store.get(snapshot)? else {
        return Err(invalid());
    };
    let mut pending = vec![(Vec::new(), root)];
    let mut count = 0u64;
    let mut paths = 0usize;
    while let Some((prefix, id)) = pending.pop() {
        let Object::Directory(dir) = store.get(id)? else {
            return Err(invalid());
        };
        for e in dir.entries() {
            count += 1;
            let mut path = prefix.clone();
            if !path.is_empty() {
                path.push(b'/');
            }
            path.extend_from_slice(e.name.as_bytes());
            RepoPath::new(path.clone())?;
            paths += path.len();
            if count > 1_000_000 || paths > 64 * 1024 * 1024 {
                return Err(pack::Error::Limit);
            }
            if e.value.mode == EntryMode::Directory {
                pending.push((path, e.value.target));
            }
        }
    }
    Ok(count)
}
/// The caller owns a finite complete reader; no output is visible until all
/// bytes, IDs, typed edges and exact root closure have been verified and synced.
pub fn receive(input: &mut impl Read, expected: ObjectId, output: &Path) -> Result<Pack> {
    let parent = files::private_directory(output.parent().ok_or(pack::Error::Output)?)
        .map_err(|_| pack::Error::Output)?;
    let name = output.file_name().ok_or(pack::Error::Output)?;
    let output = parent.join(name);
    if output.symlink_metadata().is_ok() {
        return Err(pack::Error::Output);
    }
    let stage = tempfile::Builder::new()
        .prefix(".treeharbor-native-export-")
        .tempdir_in(&parent)?;
    fs::set_permissions(stage.path(), fs::Permissions::from_mode(0o700))?;
    let store = LocalStore::open(stage.path(), &TenantId::new("native-pack")?)?;
    let magic = fixed::<{ MAGIC.len() }>(input)?;
    if magic != MAGIC {
        return Err(invalid());
    }
    let revision = ObjectId::from_bytes(fixed(input)?);
    let snapshot = ObjectId::from_bytes(fixed(input)?);
    let count = u32::from_be_bytes(fixed(input)?) as usize;
    let total = u64::from_be_bytes(fixed(input)?);
    if revision != expected {
        return Err(invalid());
    }
    if count == 0 || count > MAX_OBJECTS || total > MAX_BYTES {
        return Err(pack::Error::Limit);
    }
    let mut graph = Graph::new();
    let mut bytes = 0u64;
    let mut edges = 0usize;
    for _ in 0..count {
        let id = ObjectId::from_bytes(fixed(input)?);
        let len = u64::from_be_bytes(fixed(input)?);
        bytes = bytes.checked_add(len).ok_or(pack::Error::Limit)?;
        if len > (FRAME_HEADER + MAX_PAYLOAD) as u64 || len < FRAME_HEADER as u64 || bytes > total {
            return Err(pack::Error::Limit);
        }
        if graph.contains_key(&id) {
            return Err(invalid());
        }
        let mut frame = vec![0; len as usize];
        input.read_exact(&mut frame)?;
        let object = Object::decode_verified(id, &frame)?;
        let refs = object.references();
        edges += refs.len();
        if edges > 2_000_000 {
            return Err(pack::Error::Limit);
        }
        graph.insert(id, (object.kind(), refs));
        if store.put(&object)? != id {
            return Err(invalid());
        }
    }
    if bytes != total || input.read(&mut [0])? != 0 {
        return Err(invalid());
    }
    let ids = order(&graph, revision)?;
    let Object::NativeCommit(commit) = store.get(revision)? else {
        return Err(invalid());
    };
    if commit.snapshot != snapshot {
        return Err(invalid());
    }
    let count = entries(&store, snapshot)?;
    pack::native_manifest(stage.path(), revision, snapshot, &ids, total, count)?;
    drop(Pack::open(stage.path())?);
    File::open(stage.path())?.sync_all()?;
    let fd = File::open(&parent)?;
    rustix::fs::renameat_with(
        &fd,
        stage.path().file_name().ok_or(pack::Error::Output)?,
        &fd,
        name,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(|e| pack::Error::Io(e.into()))?;
    fd.sync_all()?;
    Pack::open(&output)
}

pub async fn download(
    socket: &Path,
    credential: &crate::client::Credential,
    fence: treeharbor_metadata::exports::ExportFence,
    output: &Path,
    timeout: std::time::Duration,
) -> crate::Result<crate::client::Reply> {
    let parent = files::private_directory(output.parent().ok_or(crate::Error::UnsafeFile)?)?;
    if output.symlink_metadata().is_ok() {
        return Err(crate::Error::UnsafeFile);
    }
    let archive = tempfile::tempfile_in(parent)?;
    let raw = crate::client::source_export(
        socket,
        credential,
        serde_json::json!({"authority":fence.authority.as_str(),"epoch":fence.epoch,
            "revision":treeharbor_core::RevisionRef::Native(fence.revision).to_string()}),
        archive,
        timeout,
    )
    .await?;
    if !raw.status.is_success() {
        return Ok(crate::client::Reply {
            status: raw.status,
            body: serde_json::from_slice(&raw.bytes).map_err(|_| crate::Error::Http)?,
        });
    }
    let mut archive = raw.file.ok_or(crate::Error::Http)?;
    let output = output.to_path_buf();
    let pack = tokio::task::spawn_blocking(move || {
        std::io::Seek::rewind(&mut archive)?;
        receive(&mut archive, fence.revision, &output)
    })
    .await
    .map_err(|_| crate::Error::ExportWorker)??;
    Ok(crate::client::Reply {
        status: raw.status,
        body: serde_json::json!({"revision":treeharbor_core::RevisionRef::Native(pack.commit()).to_string(),"snapshot":pack.snapshot().to_string(),"objects":pack.object_count(),"encoded_bytes":pack.encoded_bytes(),"filesystem_entries":pack.filesystem_entries()}),
    })
}
