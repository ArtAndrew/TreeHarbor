//! Git metadata is untrusted source data. Preserving a signature is not checking it.
use crate::{Error, HashAlgorithm, Result};
use treeharbor_core::{Commit, GitOid};

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
pub(crate) fn parse_oid(bytes: &[u8], algorithm: HashAlgorithm) -> Result<Vec<u8>> {
    if bytes.len() != algorithm.hex_len() {
        return Err(Error::InvalidProjection);
    }
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            fn nibble(b: u8) -> Result<u8> {
                match b {
                    b'0'..=b'9' => Ok(b - b'0'),
                    b'a'..=b'f' => Ok(b - b'a' + 10),
                    _ => Err(Error::InvalidProjection),
                }
            }
            Ok(nibble(pair[0])? * 16 + nibble(pair[1])?)
        })
        .collect()
}
pub(crate) fn hash(kind: &str, bytes: &[u8], algorithm: HashAlgorithm) -> Result<Vec<u8>> {
    let header = format!("{kind} {}\0", bytes.len());
    match algorithm {
        HashAlgorithm::Sha1 => {
            use sha1_checked::{Digest, Sha1};
            let mut hash = Sha1::new();
            hash.update(header.as_bytes());
            hash.update(bytes);
            let result = hash.try_finalize();
            if result.has_collision() {
                return Err(Error::InvalidProjection);
            }
            Ok(result.hash().to_vec())
        }
        HashAlgorithm::Sha256 => {
            use sha2::{Digest, Sha256};
            let mut hash = Sha256::new();
            hash.update(header.as_bytes());
            hash.update(bytes);
            Ok(hash.finalize().to_vec())
        }
    }
}
fn identity(c: &Commit) -> Result<String> {
    if c.timestamp_seconds < 0
        || c.message.contains('\0')
        || c.author_name.trim() != c.author_name
        || c.author_name.bytes().any(|b| b.is_ascii_control())
        || c.author_email
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
    {
        return Err(Error::Unsupported(
            "Git-incompatible author, date or message",
        ));
    }
    let offset = c.timezone_minutes.unsigned_abs();
    Ok(format!(
        "{} <{}> {} {}{:02}{:02}",
        c.author_name,
        c.author_email,
        c.timestamp_seconds,
        if c.timezone_minutes < 0 { '-' } else { '+' },
        offset / 60,
        offset % 60
    ))
}
pub(crate) fn native_payload(c: &Commit, tree: &[u8], parents: &[Vec<u8>]) -> Result<Vec<u8>> {
    let mut raw = format!("tree {}\n", hex(tree));
    for parent in parents {
        raw.push_str(&format!("parent {}\n", hex(parent)));
    }
    let author = identity(c)?;
    raw.push_str(&format!(
        "author {author}\ncommitter {author}\n\n{}",
        c.message
    ));
    Ok(raw.into_bytes())
}

fn author_matches(raw: &[u8], c: &Commit) -> Result<()> {
    let text = std::str::from_utf8(raw).map_err(|_| Error::Unsupported("non-UTF8 Git author"))?;
    let (who_time, offset) = text.rsplit_once(' ').ok_or(Error::InvalidProjection)?;
    let (who, time) = who_time.rsplit_once(' ').ok_or(Error::InvalidProjection)?;
    let (name, email) = who.rsplit_once(" <").ok_or(Error::InvalidProjection)?;
    let email = email.strip_suffix('>').ok_or(Error::InvalidProjection)?;
    let offset = offset.as_bytes();
    if offset.len() != 5
        || !matches!(offset[0], b'+' | b'-')
        || !offset[1..].iter().all(u8::is_ascii_digit)
    {
        return Err(Error::InvalidProjection);
    }
    let hours = i16::from(offset[1] - b'0') * 10 + i16::from(offset[2] - b'0');
    let minutes = i16::from(offset[3] - b'0') * 10 + i16::from(offset[4] - b'0');
    if hours > 23 || minutes > 59 {
        return Err(Error::InvalidProjection);
    }
    let zone = (hours * 60 + minutes) * if offset[0] == b'-' { -1 } else { 1 };
    if name != c.author_name
        || email != c.author_email
        || zone != c.timezone_minutes
        || time.parse::<i64>().map_err(|_| Error::InvalidProjection)? != c.timestamp_seconds
    {
        return Err(Error::InvalidProjection);
    }
    Ok(())
}
/// Validate the claimed OID AND its association with the complete native tree,
/// ordered mapped parents and user-facing author/message. Typed CAS closure alone
/// cannot establish this. Never fall back to a lossy projection on mismatch.
pub(crate) fn verify(
    c: &Commit,
    raw: &[u8],
    tree: &[u8],
    parents: &[Vec<u8>],
    algorithm: HashAlgorithm,
) -> Result<()> {
    let origin = c.git_origin.as_ref().ok_or(Error::InvalidProjection)?;
    let expected = match (&origin.oid, algorithm) {
        (GitOid::Sha1(id), HashAlgorithm::Sha1) => id.as_slice(),
        (GitOid::Sha256(id), HashAlgorithm::Sha256) => id.as_slice(),
        _ => {
            return Err(Error::Unsupported(
                "Git origin object format differs from export",
            ));
        }
    };
    if hash("commit", raw, algorithm)? != expected || raw.contains(&0) {
        return Err(Error::InvalidProjection);
    }
    let end = raw
        .windows(2)
        .position(|b| b == b"\n\n")
        .ok_or(Error::InvalidProjection)?;
    if &raw[end + 2..] != c.message.as_bytes() {
        return Err(Error::InvalidProjection);
    }
    let mut lines = raw[..end].split(|b| *b == b'\n');
    let source_tree = lines
        .next()
        .and_then(|l| l.strip_prefix(b"tree "))
        .ok_or(Error::InvalidProjection)?;
    if parse_oid(source_tree, algorithm)? != tree {
        return Err(Error::InvalidProjection);
    }
    for parent in parents {
        let source_parent = lines
            .next()
            .and_then(|l| l.strip_prefix(b"parent "))
            .ok_or(Error::InvalidProjection)?;
        if parse_oid(source_parent, algorithm)? != *parent {
            return Err(Error::InvalidProjection);
        }
    }
    let author = lines
        .next()
        .and_then(|l| l.strip_prefix(b"author "))
        .ok_or(Error::InvalidProjection)?;
    author_matches(author, c)?;
    if !lines.next().is_some_and(|l| l.starts_with(b"committer ")) {
        return Err(Error::InvalidProjection);
    }
    let mut continuation = false;
    for line in lines {
        if line.starts_with(b" ") {
            if !continuation {
                return Err(Error::InvalidProjection);
            }
            continue;
        }
        let Some(space) = line.iter().position(|b| *b == b' ') else {
            return Err(Error::InvalidProjection);
        };
        let key = &line[..space];
        if key.is_empty()
            || !key.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'-')
            || matches!(key, b"tree" | b"parent" | b"author" | b"committer")
        {
            return Err(Error::InvalidProjection);
        }
        if key == b"encoding" && !line[space + 1..].eq_ignore_ascii_case(b"UTF-8") {
            return Err(Error::Unsupported("non-UTF8 Git encoding"));
        }
        continuation = true;
    }
    identity(c)?;
    Ok(())
}
