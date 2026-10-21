//! Read a service-owned baseline without touching mutable workspace contents.
//! Loading is not a lease, an execution permit or a source-content verification.
use crate::{Error, Limits, MaterializedTree, Result, Selection, filesystem::Manifest};
use rustix::fs::{Mode, OFlags, openat};
use serde::{
    Deserialize, Deserializer,
    de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor},
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, BufReader, Read},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
    sync::Arc,
};
use treeharbor_core::{
    EntryMode, EntryRef, MAX_PROFILE_BYTES, MAX_PROFILE_PATHS, ObjectId, RepoPath, SourceProfile,
};
use treeharbor_metadata::Scope;

const MAX_MANIFEST_BYTES: u64 = 256 * 1024 * 1024;
// More than six JSON-escaped characters per byte of the longest hex path.
// Bound lexical tokens BEFORE serde_json can allocate a large String/number.
const MAX_STRING_TOKEN: usize = 64 * 1024;

pub struct BaselineExpectation<'a> {
    pub scope: &'a Scope,
    pub snapshot: ObjectId,
    pub profile: &'a SourceProfile,
    /// Exact previously recorded manifest SHA-256 from trusted durable state.
    /// Do not derive this from the same unverified input just to pass this check.
    pub manifest_digest: [u8; 32],
}

impl MaterializedTree {
    /// Recover only a baseline whose exact bytes are already known to the trusted
    /// controller. Ancestors of `root` must be operator-controlled. The caller
    /// owns lifecycle exclusion and must independently reconcile DB/runtime state.
    /// V1 contains no workspace/attempt identity; validate its separate placement
    /// binding before adoption, even if two directories have the same baseline.
    /// This method never reads, restores, deletes or seals mutable agent files.
    pub fn load_baseline(
        root: &Path,
        expected: BaselineExpectation<'_>,
        limits: Limits,
    ) -> Result<Self> {
        let limits = limits.validate()?;
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags((OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::NONBLOCK).bits() as i32)
            .open(root)?;
        let directory_meta = directory.metadata()?;
        private(&directory_meta, true)?;
        let root = root.canonicalize()?;
        same_directory(&directory_meta, &fs::symlink_metadata(&root)?)?;
        let file = File::from(
            openat(
                &directory,
                "manifest.json",
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(io::Error::from)?,
        );
        let before = file.metadata()?;
        private(&before, false)?;
        if before.len() > MAX_MANIFEST_BYTES {
            return Err(Error::Limit);
        }
        let mut reader = BufReader::new(Input::new(&file, MAX_MANIFEST_BYTES));
        let mut deserializer = serde_json::Deserializer::from_reader(&mut reader);
        let (header, entries) = ManifestSeed(limits).deserialize(&mut deserializer)?;
        deserializer.end()?;
        let reader = reader.into_inner();
        let after = file.metadata()?;
        if before.len() != reader.bytes
            || before.len() != after.len()
            || before.mtime() != after.mtime()
            || before.mtime_nsec() != after.mtime_nsec()
            || before.ctime() != after.ctime()
            || before.ctime_nsec() != after.ctime_nsec()
            || <[u8; 32]>::from(reader.hash.finalize()) != expected.manifest_digest
        {
            return Err(Error::InvalidBaseline);
        }
        private(&after, false)?;
        if header.version != 1
            || header.materializer != "copy-unix-v1"
            || header.tenant != expected.scope.tenant()
            || header.repository != expected.scope.repository().to_string()
            || object_id(&header.snapshot)? != expected.snapshot
            || &header.selection.0 != expected.profile
        {
            return Err(Error::InvalidBaseline);
        }
        let selection = Selection::from_profile(&header.selection.0);
        for path in entries.keys() {
            if !selection.relevant(path) {
                return Err(Error::InvalidBaseline);
            }
        }
        if let Some(paths) = &selection.paths
            && paths.iter().any(|p| !entries.contains_key(p))
        {
            return Err(Error::InvalidBaseline);
        }
        // Do not traverse `files`: it can legitimately contain edits, deletions,
        // symlinks or new files. Only reject substitution of the mount root itself.
        let files = rustix::fs::statat(&directory, "files", rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
            .map_err(io::Error::from)?;
        if rustix::fs::FileType::from_raw_mode(files.st_mode) != rustix::fs::FileType::Directory {
            return Err(Error::InvalidBaseline);
        }
        same_directory(&directory_meta, &fs::symlink_metadata(&root)?)?;
        Ok(Self {
            root,
            manifest: Arc::new(Manifest {
                tenant: header.tenant,
                repository: header.repository,
                snapshot: expected.snapshot,
                selection,
                entries,
                preserved_local_state: header.preserved_local_state,
            }),
            stats: None,
        })
    }
}

fn private(meta: &fs::Metadata, directory: bool) -> Result<()> {
    if meta.uid() != rustix::process::geteuid().as_raw()
        || meta.mode() & 0o077 != 0
        || if directory {
            !meta.is_dir()
        } else {
            !meta.is_file() || meta.nlink() != 1
        }
    {
        return Err(Error::UnsafePath(
            "baseline must be private, owned and not linked",
        ));
    }
    Ok(())
}

fn same_directory(a: &fs::Metadata, b: &fs::Metadata) -> Result<()> {
    private(b, true)?;
    if a.dev() != b.dev() || a.ino() != b.ino() {
        return Err(Error::InvalidBaseline);
    }
    Ok(())
}

fn bytes(value: &str, max: usize) -> Result<Vec<u8>> {
    if value.is_empty()
        || value.len() > max * 2
        || !value.len().is_multiple_of(2)
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::InvalidBaseline);
    }
    let nibble = |b| if b <= b'9' { b - b'0' } else { b - b'a' + 10 };
    Ok(value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| (nibble(b[0]) << 4) | nibble(b[1]))
        .collect())
}

fn object_id(value: &str) -> Result<ObjectId> {
    let digest = bytes(value, 32)?;
    Ok(ObjectId::from_bytes(
        digest.try_into().map_err(|_| Error::InvalidBaseline)?,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    version: u16,
    materializer: String,
    tenant: String,
    repository: String,
    snapshot: String,
    selection: Selected,
    preserved_local_state: bool,
}

struct Selected(SourceProfile);
impl<'de> Deserialize<'de> for Selected {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct Paths;
        impl<'de> Visitor<'de> for Paths {
            type Value = Selected;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("null or a canonical bounded source selection")
            }
            fn visit_unit<E: de::Error>(self) -> std::result::Result<Self::Value, E> {
                Ok(Selected(SourceProfile::full()))
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut paths = Vec::new();
                let mut size = 26;
                while let Some(value) = seq.next_element::<String>()? {
                    let path = RepoPath::new(bytes(&value, 4096).map_err(de::Error::custom)?)
                        .map_err(de::Error::custom)?;
                    size += 4 + path.as_bytes().len();
                    if paths.len() >= MAX_PROFILE_PATHS || size > MAX_PROFILE_BYTES {
                        return Err(de::Error::custom("selection limit"));
                    }
                    paths.push(path);
                }
                let profile = SourceProfile::sparse(paths.clone()).map_err(de::Error::custom)?;
                if profile.paths() != Some(paths.as_slice()) {
                    return Err(de::Error::custom("noncanonical selection"));
                }
                Ok(Selected(profile))
            }
        }
        d.deserialize_any(Paths)
    }
}

struct EntriesSeed(Limits);
impl<'de> DeserializeSeed<'de> for EntriesSeed {
    type Value = BTreeMap<RepoPath, EntryRef>;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> std::result::Result<Self::Value, D::Error> {
        d.deserialize_seq(self)
    }
}
impl<'de> Visitor<'de> for EntriesSeed {
    type Value = BTreeMap<RepoPath, EntryRef>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("ordered baseline entries")
    }
    fn visit_seq<A: SeqAccess<'de>>(
        self,
        mut seq: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let mut entries = BTreeMap::<RepoPath, EntryRef>::new();
        let mut path_bytes = 0u64;
        while let Some((path, mode, target)) = seq.next_element::<(String, u32, String)>()? {
            let path = RepoPath::new(bytes(&path, 4096).map_err(de::Error::custom)?)
                .map_err(de::Error::custom)?;
            path_bytes += path.as_bytes().len() as u64;
            if entries.len() >= self.0.max_entries.min(1_000_000)
                || path_bytes > self.0.max_path_bytes.min(64 * 1024 * 1024)
            {
                return Err(de::Error::custom("baseline entry/path limit"));
            }
            if entries
                .last_key_value()
                .is_some_and(|(previous, _)| previous >= &path)
            {
                return Err(de::Error::custom("duplicate or unordered baseline path"));
            }
            if let Some(slash) = path.as_bytes().iter().rposition(|b| *b == b'/') {
                let parent =
                    RepoPath::new(path.as_bytes()[..slash].to_vec()).map_err(de::Error::custom)?;
                if entries
                    .get(&parent)
                    .is_none_or(|entry| entry.mode != EntryMode::Directory)
                {
                    return Err(de::Error::custom(
                        "missing or non-directory baseline ancestor",
                    ));
                }
            }
            let entry = EntryRef {
                mode: EntryMode::try_from(mode).map_err(de::Error::custom)?,
                target: object_id(&target).map_err(de::Error::custom)?,
            };
            entries.insert(path, entry);
        }
        Ok(entries)
    }
}

struct ManifestSeed(Limits);
impl<'de> DeserializeSeed<'de> for ManifestSeed {
    type Value = (Header, BTreeMap<RepoPath, EntryRef>);
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> std::result::Result<Self::Value, D::Error> {
        d.deserialize_map(self)
    }
}
impl<'de> Visitor<'de> for ManifestSeed {
    type Value = (Header, BTreeMap<RepoPath, EntryRef>);
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("workspace baseline v1")
    }
    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let mut header = None;
        let mut entries = None;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "header" if header.is_none() => header = Some(map.next_value()?),
                "entries" if entries.is_none() => {
                    entries = Some(map.next_value_seed(EntriesSeed(self.0))?)
                }
                _ => return Err(de::Error::custom("unknown or duplicate baseline field")),
            }
        }
        Ok((
            header.ok_or_else(|| de::Error::missing_field("header"))?,
            entries.ok_or_else(|| de::Error::missing_field("entries"))?,
        ))
    }
}

/// Streaming raw-byte hash plus resource limits. serde still validates all JSON
/// syntax. Lexical lengths span read boundaries, including escaped quotes.
struct Input<R> {
    inner: R,
    maximum: u64,
    bytes: u64,
    hash: Sha256,
    string: bool,
    escape: bool,
    token: usize,
}
impl<R> Input<R> {
    fn new(inner: R, maximum: u64) -> Self {
        Self {
            inner,
            maximum,
            bytes: 0,
            hash: Sha256::new(),
            string: false,
            escape: false,
            token: 0,
        }
    }
}
impl<R: Read> Read for Input<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        let take = output.len().min((self.maximum - self.bytes + 1) as usize);
        let count = self.inner.read(&mut output[..take])?;
        if self.bytes + count as u64 > self.maximum {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "manifest byte limit",
            ));
        }
        for byte in &output[..count] {
            if self.string {
                self.token += 1;
                if self.token > MAX_STRING_TOKEN {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "manifest string limit",
                    ));
                }
                if self.escape {
                    self.escape = false;
                } else if *byte == b'\\' {
                    self.escape = true;
                } else if *byte == b'"' {
                    self.string = false;
                    self.token = 0;
                }
            } else if *byte == b'"' {
                self.string = true;
                self.token = 0;
            } else if byte.is_ascii_whitespace()
                || matches!(byte, b'{' | b'}' | b'[' | b']' | b',' | b':')
            {
                self.token = 0;
            } else {
                self.token += 1;
                if self.token > 128 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "manifest scalar limit",
                    ));
                }
            }
        }
        self.hash.update(&output[..count]);
        self.bytes += count as u64;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_hash_and_lexical_state_span_short_reads_and_escaped_quotes() {
        let raw = br#"{"key":"a\"b\\c","number":12}  "#;
        for size in [1, 3, 8, 32] {
            let mut input = Input::new(&raw[..], raw.len() as u64);
            let mut bytes = Vec::new();
            let mut chunk = vec![0; size];
            loop {
                let count = input.read(&mut chunk).unwrap();
                if count == 0 {
                    break;
                }
                bytes.extend_from_slice(&chunk[..count]);
            }
            assert_eq!(bytes, raw);
            assert_eq!(input.hash.finalize(), Sha256::digest(raw));
            assert!(!input.string);
        }
    }

    #[test]
    fn size_limit_never_becomes_silent_eof_and_tokens_are_bounded_across_reads() {
        let mut input = Input::new(&b"0123456789"[..], 9);
        let mut chunk = [0; 3];
        for _ in 0..3 {
            assert_eq!(input.read(&mut chunk).unwrap(), 3);
        }
        assert_eq!(
            input.read(&mut chunk).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        let long = format!("\"{}\"", "a".repeat(MAX_STRING_TOKEN + 1));
        let mut input = Input::new(long.as_bytes(), long.len() as u64);
        let mut chunk = [0; 17];
        loop {
            match input.read(&mut chunk) {
                Ok(0) => panic!("oversized token reached EOF without rejection"),
                Ok(_) => (),
                Err(error) => {
                    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
                    break;
                }
            }
        }
        assert!(input.bytes < long.len() as u64);
    }
}
