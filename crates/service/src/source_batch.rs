//! Blob-only source upload request frames for the v1 local transport.
use crate::{Error, Result};
use treeharbor_core::{FRAME_HEADER, ObjectId};
use treeharbor_metadata::uploads::SOURCE_BLOB_BATCH_OBJECTS;
use treeharbor_storage::PreparedObject;

const MAGIC: &[u8] = b"THSB\x01";
pub(crate) const HEADER_BYTES: usize = MAGIC.len() + 2;
pub(crate) const LENGTH_BYTES: usize = 4;
pub(crate) const MAX_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_OBJECTS: usize = SOURCE_BLOB_BATCH_OBJECTS;

pub(crate) fn encode(frames: &[(ObjectId, Vec<u8>)]) -> Result<Vec<u8>> {
    if !(1..=MAX_OBJECTS).contains(&frames.len())
        || frames.windows(2).any(|pair| pair[0].0 >= pair[1].0)
    {
        return Err(Error::ImportState);
    }
    let size = frames
        .iter()
        .try_fold(HEADER_BYTES, |size, (_, frame)| {
            size.checked_add(LENGTH_BYTES)?.checked_add(frame.len())
        })
        .filter(|&size| size <= MAX_BYTES)
        .ok_or(Error::Size)?;
    let mut raw = Vec::with_capacity(size);
    raw.extend_from_slice(MAGIC);
    raw.extend_from_slice(&(frames.len() as u16).to_be_bytes());
    for (_, frame) in frames {
        raw.extend_from_slice(&(frame.len() as u32).to_be_bytes());
        raw.extend_from_slice(frame);
    }
    Ok(raw)
}

pub(crate) fn decode(raw: &[u8]) -> Option<Vec<PreparedObject>> {
    if raw.len() > MAX_BYTES {
        return None;
    }
    let (count, mut rest) = raw.strip_prefix(MAGIC)?.split_at_checked(2)?;
    let count = usize::from(u16::from_be_bytes(count.try_into().ok()?));
    if !(1..=MAX_OBJECTS).contains(&count) {
        return None;
    }
    let mut blobs = Vec::with_capacity(count);
    let mut previous = None;
    for _ in 0..count {
        let (length, tail) = rest.split_at_checked(LENGTH_BYTES)?;
        let length = u32::from_be_bytes(length.try_into().ok()?) as usize;
        if length < FRAME_HEADER {
            return None;
        }
        let (frame, tail) = tail.split_at_checked(length)?;
        let id = ObjectId::hash_frame(frame);
        if previous.is_some_and(|value| value >= id) {
            return None;
        }
        let object = PreparedObject::from_frame(id, frame.to_vec()).ok()?;
        if object.kind() != 1 {
            return None;
        }
        blobs.push(object);
        previous = Some(id);
        rest = tail;
    }
    rest.is_empty().then_some(blobs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use treeharbor_core::Object;

    #[test]
    fn golden_empty_blob_and_strict_boundaries() {
        // THSB v1, one frame, frame length 22, treeharbor v1 empty blob.
        let golden = b"THSB\x01\x00\x01\x00\x00\x00\x16treeharbor\0\x00\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00";
        let blob = Object::Blob(Vec::new());
        let frames = [(blob.id().unwrap(), blob.encode().unwrap())];
        assert_eq!(encode(&frames).unwrap(), golden);
        let decoded = decode(golden).unwrap();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].id(), frames[0].0);
        assert!(decode(&golden[..golden.len() - 1]).is_none());
        assert!(decode(&[golden.as_slice(), &[0]].concat()).is_none());
        let mut wrong_kind = golden.to_owned();
        wrong_kind[HEADER_BYTES + LENGTH_BYTES + 13] = 2;
        assert!(decode(&wrong_kind).is_none());
        let mut duplicate = golden.to_vec();
        duplicate[HEADER_BYTES - 1] = 2;
        duplicate.extend_from_slice(&golden[HEADER_BYTES..]);
        assert!(decode(&duplicate).is_none());
    }
}
