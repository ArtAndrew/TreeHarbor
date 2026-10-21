use crate::{Error, Result};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Name(Vec<u8>);

impl Name {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Result<Self> {
        let bytes = bytes.into();
        if bytes.is_empty() || bytes.len() > 255 {
            return Err(Error::InvalidPath("component length"));
        }
        if bytes == b"." || bytes == b".." {
            return Err(Error::InvalidPath("relative component"));
        }
        if bytes.iter().any(|b| matches!(b, 0 | b'/' | b'\\')) {
            return Err(Error::InvalidPath("separator or NUL"));
        }
        if bytes.eq_ignore_ascii_case(b".git") || bytes.eq_ignore_ascii_case(b".treeharbor") {
            return Err(Error::InvalidPath("reserved component"));
        }
        Ok(Self(bytes))
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RepoPath(Vec<u8>);

impl RepoPath {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Result<Self> {
        let bytes = bytes.into();
        if bytes.is_empty() || bytes.len() > 4096 {
            return Err(Error::InvalidPath("path length"));
        }
        let mut depth = 0;
        for component in bytes.split(|b| *b == b'/') {
            Name::new(component.to_vec())?;
            depth += 1;
        }
        if depth > 128 {
            return Err(Error::InvalidPath("path depth"));
        }
        Ok(Self(bytes))
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
    pub fn components(&self) -> impl Iterator<Item = &[u8]> {
        self.0.split(|b| *b == b'/')
    }
    pub fn is_ancestor_of(&self, other: &Self) -> bool {
        other.0.starts_with(&self.0) && other.0.get(self.0.len()) == Some(&b'/')
    }
}
