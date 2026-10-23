//! Explicit capture scope. This selects bytes; it grants no filesystem/runtime
//! permission and does not establish that writers have stopped.
use crate::{Error, ObjectId, RepoPath, Result};
use sha2::{Digest, Sha256};

pub const MAX_CAPTURE_PATHS: usize = 256;
pub const MAX_CAPTURE_BYTES: usize = 64 * 1024;
const MAGIC: &[u8] = b"treeharbor-capture-selection\0";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureSelection {
    replace: Vec<RepoPath>,
    delete: Vec<RepoPath>,
    exclude: Vec<RepoPath>,
}
impl CaptureSelection {
    /// Replacements select complete subtrees, including their additions/deletions.
    /// Exclusions retain the original base at that path, not mutable local bytes.
    /// Duplicate/overlapping roots are rejected rather than silently normalized.
    pub fn new(
        mut replace: Vec<RepoPath>,
        mut delete: Vec<RepoPath>,
        mut exclude: Vec<RepoPath>,
    ) -> Result<Self> {
        let count = replace
            .len()
            .saturating_add(delete.len())
            .saturating_add(exclude.len());
        if replace.is_empty() && delete.is_empty() {
            return Err(Error::InvalidObject("empty capture selection"));
        }
        if count > MAX_CAPTURE_PATHS {
            return Err(Error::TooLarge);
        }
        replace.sort();
        delete.sort();
        exclude.sort();
        let roots: Vec<_> = replace.iter().chain(&delete).collect();
        for (i, path) in roots.iter().enumerate() {
            if roots.iter().skip(i + 1).any(|other| overlap(path, other)) {
                return Err(Error::OverlappingPaths);
            }
        }
        for (i, path) in exclude.iter().enumerate() {
            if !replace.iter().any(|root| root.is_ancestor_of(path))
                || exclude.iter().skip(i + 1).any(|other| overlap(path, other))
            {
                return Err(Error::InvalidObject("capture exclusion"));
            }
        }
        let value = Self {
            replace,
            delete,
            exclude,
        };
        if value.encode().len() > MAX_CAPTURE_BYTES {
            return Err(Error::TooLarge);
        }
        Ok(value)
    }
    pub fn replace(&self) -> &[RepoPath] {
        &self.replace
    }
    pub fn delete(&self) -> &[RepoPath] {
        &self.delete
    }
    pub fn exclude(&self) -> &[RepoPath] {
        &self.exclude
    }
    pub fn excluded(&self, path: &RepoPath) -> bool {
        self.exclude
            .iter()
            .any(|p| p == path || p.is_ancestor_of(path))
    }
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&1u16.to_be_bytes());
        for paths in [&self.replace, &self.delete, &self.exclude] {
            bytes.extend_from_slice(&(paths.len() as u32).to_be_bytes());
            for path in paths {
                bytes.extend_from_slice(&(path.as_bytes().len() as u32).to_be_bytes());
                bytes.extend_from_slice(path.as_bytes());
            }
        }
        bytes
    }
    /// A selection digest, not an immutable source object address.
    pub fn digest(&self) -> [u8; 32] {
        Sha256::digest(self.encode()).into()
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_CAPTURE_BYTES {
            return Err(Error::TooLarge);
        }
        let mut input = bytes;
        if take(&mut input, MAGIC.len())? != MAGIC || take(&mut input, 2)? != 1u16.to_be_bytes() {
            return Err(Error::InvalidObject("capture selection header"));
        }
        let mut groups = Vec::new();
        let mut total = 0usize;
        for _ in 0..3 {
            let count = number(&mut input)?;
            total = total.checked_add(count).ok_or(Error::TooLarge)?;
            if total > MAX_CAPTURE_PATHS {
                return Err(Error::TooLarge);
            }
            let mut paths = Vec::with_capacity(count);
            for _ in 0..count {
                let length = number(&mut input)?;
                if length > 4096 {
                    return Err(Error::TooLarge);
                }
                paths.push(RepoPath::new(take(&mut input, length)?.to_vec())?);
            }
            groups.push(paths);
        }
        let exclude = groups.pop().unwrap();
        let delete = groups.pop().unwrap();
        let replace = groups.pop().unwrap();
        let value = Self::new(replace, delete, exclude)?;
        if !input.is_empty() || value.encode() != bytes {
            return Err(Error::InvalidObject("noncanonical capture selection"));
        }
        Ok(value)
    }
    pub fn digest_text(&self) -> String {
        ObjectId::from_bytes(self.digest()).to_string()
    }
}
fn overlap(a: &RepoPath, b: &RepoPath) -> bool {
    a == b || a.is_ancestor_of(b) || b.is_ancestor_of(a)
}
fn take<'a>(input: &mut &'a [u8], n: usize) -> Result<&'a [u8]> {
    if n > input.len() {
        return Err(Error::InvalidObject("truncated capture selection"));
    }
    let (value, rest) = input.split_at(n);
    *input = rest;
    Ok(value)
}
fn number(input: &mut &[u8]) -> Result<usize> {
    Ok(u32::from_be_bytes(take(input, 4)?.try_into().unwrap()) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn path(s: &str) -> RepoPath {
        RepoPath::new(s.as_bytes()).unwrap()
    }
    #[test]
    fn selection_rejects_ambiguous_roots_and_exclusions() {
        for (replace, delete, exclude) in [
            (vec![], vec![], vec![]),
            (vec!["a", "a/b", "a-1"], vec![], vec![]),
            (vec!["a"], vec!["a"], vec![]),
            (vec!["a"], vec![], vec!["a"]),
            (vec!["a"], vec![], vec!["b"]),
            (vec!["a"], vec![], vec!["a/b", "a/b/c"]),
        ] {
            let paths = |s: Vec<&str>| s.into_iter().map(path).collect();
            assert!(CaptureSelection::new(paths(replace), paths(delete), paths(exclude)).is_err());
        }
        let value = CaptureSelection::new(
            vec![path("src")],
            vec![path("obsolete")],
            vec![path("src/private")],
        )
        .unwrap();
        assert!(value.excluded(&path("src/private/key")));
        assert!(!value.excluded(&path("src/private-new")));
        assert_eq!(CaptureSelection::decode(&value.encode()).unwrap(), value);
        for end in 0..value.encode().len() {
            assert!(CaptureSelection::decode(&value.encode()[..end]).is_err());
        }
        let mut extra = value.encode();
        extra.push(0);
        assert!(CaptureSelection::decode(&extra).is_err());
    }
    #[test]
    fn selection_matches_independent_binary_vector() {
        let bytes = include_bytes!("../tests/fixtures/capture-selection-v1.bin");
        let value = CaptureSelection::new(
            vec![path("src"), path("readme\n\".md")],
            vec![path("obsolete")],
            vec![path("src/build"), path("src/.env")],
        )
        .unwrap();
        assert_eq!(value.encode(), bytes);
        assert_eq!(
            value.digest_text(),
            include_str!("../tests/fixtures/capture-selection-v1.sha256").trim()
        );
        assert_eq!(CaptureSelection::decode(bytes).unwrap(), value);
        let mut huge = bytes.to_vec();
        huge[MAGIC.len() + 2..MAGIC.len() + 6].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(CaptureSelection::decode(&huge).is_err());
    }
}
