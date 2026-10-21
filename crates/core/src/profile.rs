//! Canonical source-selection profiles. A profile selects delivery, not access.
use crate::{Error, ObjectId, RepoPath, Result};
use sha2::{Digest, Sha256};
use std::{fmt, str::FromStr};
pub const MAX_PROFILE_PATHS: usize = 1024;
pub const MAX_PROFILE_BYTES: usize = 64 * 1024;
const MAGIC: &[u8; 19] = b"treeharbor-profile\0";

/// Shares the textual SHA-256 spelling, but is not a source object identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProfileId([u8; 32]);
impl ProfileId {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}
impl fmt::Display for ProfileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", ObjectId::from_bytes(self.0))
    }
}
impl FromStr for ProfileId {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self> {
        Ok(Self(*value.parse::<ObjectId>()?.as_bytes()))
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceProfile {
    paths: Option<Vec<RepoPath>>,
}
impl SourceProfile {
    pub fn full() -> Self {
        Self { paths: None }
    }
    pub fn sparse(mut paths: Vec<RepoPath>) -> Result<Self> {
        if paths.is_empty()
            || paths.len() > MAX_PROFILE_PATHS
            || paths
                .iter()
                .try_fold(26usize, |n, p| n.checked_add(4 + p.as_bytes().len()))
                .is_none_or(|n| n > MAX_PROFILE_BYTES)
        {
            return Err(Error::TooLarge);
        }
        paths.sort();
        paths.dedup();
        let mut normalized: Vec<RepoPath> = Vec::new();
        for path in paths {
            if !normalized.iter().any(|parent| parent.is_ancestor_of(&path)) {
                normalized.push(path);
            }
        }
        Ok(Self {
            paths: Some(normalized),
        })
    }
    pub fn paths(&self) -> Option<&[RepoPath]> {
        self.paths.as_deref()
    }
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.push(u8::from(self.paths.is_some()));
        let paths = self.paths.as_deref().unwrap_or_default();
        bytes.extend_from_slice(&(paths.len() as u32).to_be_bytes());
        for path in paths {
            bytes.extend_from_slice(&(path.as_bytes().len() as u32).to_be_bytes());
            bytes.extend_from_slice(path.as_bytes());
        }
        bytes
    }
    pub fn id(&self) -> ProfileId {
        ProfileId(Sha256::digest(self.encode()).into())
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_PROFILE_BYTES {
            return Err(Error::TooLarge);
        }
        if bytes.len() < 26 || &bytes[..19] != MAGIC || bytes[19..21] != 1u16.to_be_bytes() {
            return Err(Error::InvalidObject("profile header"));
        }
        let count = u32::from_be_bytes(
            bytes[22..26]
                .try_into()
                .map_err(|_| Error::InvalidObject("profile count"))?,
        ) as usize;
        if count > MAX_PROFILE_PATHS {
            return Err(Error::TooLarge);
        }
        let profile = match bytes[21] {
            0 if count == 0 => Self::full(),
            1 => {
                let mut at = 26usize;
                let mut paths = Vec::with_capacity(count);
                for _ in 0..count {
                    let length = bytes
                        .get(at..at + 4)
                        .ok_or(Error::InvalidObject("profile length"))?;
                    let length = u32::from_be_bytes(
                        length
                            .try_into()
                            .map_err(|_| Error::InvalidObject("profile length"))?,
                    ) as usize;
                    at += 4;
                    let end = at.checked_add(length).ok_or(Error::TooLarge)?;
                    let path = bytes
                        .get(at..end)
                        .ok_or(Error::InvalidObject("profile path"))?;
                    paths.push(RepoPath::new(path.to_vec())?);
                    at = end;
                }
                Self::sparse(paths)?
            }
            _ => return Err(Error::InvalidObject("profile selection")),
        };
        if profile.encode() != bytes {
            return Err(Error::InvalidObject("noncanonical profile"));
        }
        Ok(profile)
    }
}
