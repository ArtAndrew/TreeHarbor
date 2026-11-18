//! Local Git source packs. The final file contains only the manifest's reachable
//! frames, in manifest order. The scratch file has one writer and no pathname.
use super::{Error, Result};
use crate::files;
use rustix::fs::OFlags;
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{BufWriter, Write},
    os::unix::fs::{FileExt, OpenOptionsExt},
    path::Path,
    time::Instant,
};
use treeharbor_core::{FRAME_HEADER, MAX_PAYLOAD, Object, ObjectId, ObjectStore};
use treeharbor_storage::LocalStore;

const DATA: &str = "frames.bin";

struct Frames {
    file: File,
    positions: BTreeMap<ObjectId, (u64, u32)>,
}

impl Frames {
    fn read(&self, id: ObjectId, limit: u64) -> treeharbor_core::Result<Vec<u8>> {
        let &(offset, size) = self
            .positions
            .get(&id)
            .ok_or(treeharbor_core::Error::MissingObject(id))?;
        if u64::from(size) > limit {
            return Err(treeharbor_core::Error::TooLarge);
        }
        let mut frame = vec![0; size as usize];
        self.file
            .read_exact_at(&mut frame, offset)
            .map_err(storage_error)?;
        Ok(frame)
    }
}

fn storage_error(error: std::io::Error) -> treeharbor_core::Error {
    treeharbor_core::Error::Storage(error.to_string())
}

enum Layout {
    Loose(LocalStore),
    Packed(Frames),
}

pub(crate) struct PackStore(Layout);

impl PackStore {
    pub(super) fn loose(store: LocalStore) -> Self {
        Self(Layout::Loose(store))
    }

    pub(super) fn packed(
        dir: &Path,
        ids: &[ObjectId],
        bytes: u64,
        deadline: Option<Instant>,
    ) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK).bits() as i32)
            .open(dir.join(DATA))?;
        files::private_file(&file).map_err(|_| Error::Invalid)?;
        let length = file.metadata()?.len();
        if bytes.checked_add(4 * ids.len() as u64) != Some(length) {
            return Err(Error::Invalid);
        }
        let mut offset = 0u64;
        let mut positions = BTreeMap::new();
        for id in ids {
            if deadline.is_some_and(|t| Instant::now() >= t) {
                return Err(Error::Limit);
            }
            let mut prefix = [0; 4];
            file.read_exact_at(&mut prefix, offset)?;
            let size = u32::from_be_bytes(prefix);
            let start = offset.checked_add(4).ok_or(Error::Invalid)?;
            offset = start.checked_add(u64::from(size)).ok_or(Error::Invalid)?;
            if !(FRAME_HEADER..=FRAME_HEADER + MAX_PAYLOAD).contains(&(size as usize))
                || offset > length
                || positions.insert(*id, (start, size)).is_some()
            {
                return Err(Error::Invalid);
            }
        }
        if offset != length {
            return Err(Error::Invalid);
        }
        Ok(Self(Layout::Packed(Frames { file, positions })))
    }

    pub(super) fn read_limited(&self, id: ObjectId, remaining: u64) -> Result<Object> {
        match &self.0 {
            Layout::Loose(store) => {
                let opened = store.open_object(id)?;
                let length = opened.length();
                if length > remaining {
                    return Err(Error::Limit);
                }
                Ok(opened.read()?)
            }
            Layout::Packed(frames) => {
                let frame = frames.read(id, remaining)?;
                Ok(Object::decode_verified(id, &frame)?)
            }
        }
    }
}

impl ObjectStore for PackStore {
    fn get(&self, id: ObjectId) -> treeharbor_core::Result<Object> {
        match &self.0 {
            Layout::Loose(store) => store.get(id),
            Layout::Packed(frames) => Object::decode_verified(id, &frames.read(id, u64::MAX)?),
        }
    }

    fn put(&self, _: &Object) -> treeharbor_core::Result<ObjectId> {
        Err(treeharbor_core::Error::InvalidObject(
            "validated source pack is read-only",
        ))
    }
}

pub(super) struct PackedStage {
    frames: RefCell<Frames>,
    limits: treeharbor_git_bridge::Limits,
    length: Cell<u64>,
}

impl PackedStage {
    pub(super) fn new(dir: &Path, limits: treeharbor_git_bridge::Limits) -> Result<Self> {
        let file = tempfile::tempfile_in(dir)?;
        Ok(Self {
            frames: RefCell::new(Frames {
                file,
                positions: BTreeMap::new(),
            }),
            limits,
            length: Cell::new(0),
        })
    }

    pub(super) fn seal(self, dir: &Path, ids: &[ObjectId], deadline: Instant) -> Result<()> {
        let frames = self.frames.into_inner();
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(dir.join(DATA))?;
        let mut out = BufWriter::new(file);
        for id in ids {
            if Instant::now() >= deadline {
                return Err(Error::Limit);
            }
            let bytes = frames.read(*id, self.limits.source_bytes)?;
            out.write_all(&(bytes.len() as u32).to_be_bytes())?;
            out.write_all(&bytes)?;
        }
        out.flush()?;
        out.get_ref().sync_all()?;
        Ok(())
    }
}

impl ObjectStore for PackedStage {
    fn get(&self, id: ObjectId) -> treeharbor_core::Result<Object> {
        Object::decode_verified(
            id,
            &self.frames.borrow().read(id, self.limits.source_bytes)?,
        )
    }

    fn put(&self, object: &Object) -> treeharbor_core::Result<ObjectId> {
        let bytes = object.encode()?;
        let id = ObjectId::hash_frame(&bytes);
        let mut frames = self.frames.borrow_mut();
        if frames.positions.contains_key(&id) {
            if frames.read(id, self.limits.source_bytes)? != bytes {
                return Err(treeharbor_core::Error::InvalidObject(
                    "existing frame differs",
                ));
            }
            return Ok(id);
        }
        let start = self.length.get();
        let end = start
            .checked_add(bytes.len() as u64)
            .ok_or(treeharbor_core::Error::TooLarge)?;
        if end > self.limits.source_bytes || frames.positions.len() >= self.limits.objects {
            return Err(treeharbor_core::Error::TooLarge);
        }
        let size = u32::try_from(bytes.len()).map_err(|_| treeharbor_core::Error::TooLarge)?;
        frames
            .file
            .write_all_at(&bytes, start)
            .map_err(storage_error)?;
        frames.positions.insert(id, (start, size));
        self.length.set(end);
        Ok(id)
    }
}
