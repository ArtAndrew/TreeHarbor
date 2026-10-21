use crate::{Error, Result};
use sha2::{Digest, Sha256};
use std::{fmt, str::FromStr};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectId([u8; 32]);

impl ObjectId {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
    pub fn hash_frame(frame: &[u8]) -> Self {
        Self(Sha256::digest(frame).into())
    }
    pub fn hex(&self) -> String {
        encode_hex(&self.0)
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sha256:{}", self.hex())
    }
}
impl fmt::Debug for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
impl FromStr for ObjectId {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        let s = s.strip_prefix("sha256:").ok_or(Error::InvalidId)?;
        let bytes = decode_hex(s)?;
        Ok(Self(bytes.try_into().map_err(|_| Error::InvalidId)?))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GitOid {
    Sha1([u8; 20]),
    Sha256([u8; 32]),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RevisionRef {
    Native(ObjectId),
    Git(GitOid),
}

impl fmt::Display for RevisionRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Native(id) => write!(f, "native:{id}"),
            Self::Git(GitOid::Sha1(id)) => write!(f, "git:sha1:{}", encode_hex(id)),
            Self::Git(GitOid::Sha256(id)) => write!(f, "git:sha256:{}", encode_hex(id)),
        }
    }
}
impl FromStr for RevisionRef {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        if let Some(s) = s.strip_prefix("native:") {
            return Ok(Self::Native(s.parse()?));
        }
        if let Some(s) = s.strip_prefix("git:sha1:") {
            return Ok(Self::Git(GitOid::Sha1(
                decode_hex(s)?.try_into().map_err(|_| Error::InvalidId)?,
            )));
        }
        if let Some(s) = s.strip_prefix("git:sha256:") {
            return Ok(Self::Git(GitOid::Sha256(
                decode_hex(s)?.try_into().map_err(|_| Error::InvalidId)?,
            )));
        }
        Err(Error::InvalidId)
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        s.push(HEX[(byte >> 4) as usize] as char);
        s.push(HEX[(byte & 15) as usize] as char);
    }
    s
}
fn decode_hex(s: &str) -> Result<Vec<u8>> {
    if s.len() != 40 && s.len() != 64 {
        return Err(Error::InvalidId);
    }
    fn nibble(b: u8) -> Result<u8> {
        match b {
            b'0'..=b'9' => Ok(b - b'0'),
            b'a'..=b'f' => Ok(b - b'a' + 10),
            _ => Err(Error::InvalidId),
        }
    }
    s.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| Ok((nibble(p[0])? << 4) | nibble(p[1])?))
        .collect()
}
