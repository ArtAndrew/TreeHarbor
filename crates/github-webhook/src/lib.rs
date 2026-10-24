//! Authenticate original webhook bytes before exposing typed, scoped facts.
//! An authentic event is not membership, CI, approval or final merge proof.
#![forbid(unsafe_code)]
mod payload;
use hmac::{Hmac, KeyInit, Mac};
pub use payload::{DestroyReason, GroupAction, MergeGroupEvent, WebhookEvent};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zeroize::Zeroizing;

pub const MAX_BODY_BYTES: usize = 1024 * 1024;
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("invalid webhook key configuration")]
    Key,
    #[error("invalid webhook signature")]
    Signature,
    #[error("webhook payload exceeds configured bounds")]
    Size,
    #[error("unsupported webhook event or action")]
    Unsupported,
    #[error("invalid webhook payload or identity")]
    Payload,
}
pub type Result<T> = std::result::Result<T, Error>;

/// Operator-pinned App identity and secret. Deliberately neither Debug nor Serialize.
pub struct WebhookKey {
    app_id: u64,
    key_id: Uuid,
    secret: Zeroizing<Vec<u8>>,
}
impl WebhookKey {
    pub fn new(app_id: u64, key_id: Uuid, secret: Vec<u8>) -> Result<Self> {
        let secret = Zeroizing::new(secret);
        if app_id == 0
            || app_id > i64::MAX as u64
            || key_id.is_nil()
            || !(32..=256).contains(&secret.len())
            || std::str::from_utf8(&secret).is_err()
            || secret.iter().any(|b| b.is_ascii_control())
        {
            return Err(Error::Key);
        }
        Ok(Self {
            app_id,
            key_id,
            secret,
        })
    }
    pub fn app_id(&self) -> u64 {
        self.app_id
    }
    pub fn key_id(&self) -> Uuid {
        self.key_id
    }
    /// Only the body is signed. `event_header` is a routing consistency check,
    /// never a source of authorization. A delivery UUID is handled by the inbox.
    pub fn authenticate(
        &self,
        event_header: &str,
        signature: &str,
        body: Vec<u8>,
    ) -> Result<VerifiedDelivery> {
        if body.is_empty() || body.len() > MAX_BODY_BYTES {
            return Err(Error::Size);
        }
        verify_mac(&self.secret, signature, &body)?;
        let event = payload::decode(self.app_id, event_header, &body)?;
        Ok(VerifiedDelivery {
            app_id: self.app_id,
            key_id: self.key_id,
            digest: Sha256::digest(&body).into(),
            body,
            event,
        })
    }
}
fn verify_mac(secret: &[u8], signature: &str, body: &[u8]) -> Result<()> {
    let signature = signature.strip_prefix("sha256=").ok_or(Error::Signature)?;
    if signature.len() != 64 {
        return Err(Error::Signature);
    }
    let mut bytes = [0u8; 32];
    for (i, pair) in signature.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        fn digit(b: u8) -> Result<u8> {
            match b {
                b'0'..=b'9' => Ok(b - b'0'),
                b'a'..=b'f' => Ok(b - b'a' + 10),
                _ => Err(Error::Signature),
            }
        }
        bytes[i] = (digit(pair[0])? << 4) | digit(pair[1])?;
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).map_err(|_| Error::Key)?;
    mac.update(body);
    mac.verify_slice(&bytes).map_err(|_| Error::Signature)
}

/// Constructible only by authenticating raw bytes; cannot be deserialized from
/// an agent request or mutated into another App/payload identity after validation.
pub struct VerifiedDelivery {
    app_id: u64,
    key_id: Uuid,
    digest: [u8; 32],
    body: Vec<u8>,
    event: WebhookEvent,
}
impl VerifiedDelivery {
    pub fn app_id(&self) -> u64 {
        self.app_id
    }
    pub fn key_id(&self) -> Uuid {
        self.key_id
    }
    pub fn digest(&self) -> &[u8; 32] {
        &self.digest
    }
    pub fn body(&self) -> &[u8] {
        &self.body
    }
    pub fn event(&self) -> &WebhookEvent {
        &self.event
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn github_documented_hmac_vector_and_strict_header_encoding() {
        // GitHub's published validation vector, independent of fixture generation.
        let sig = "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17";
        verify_mac(b"It's a Secret to Everybody", sig, b"Hello, World!").unwrap();
        for bad in [
            sig.to_uppercase(),
            sig.replace("sha256=", "sha1="),
            format!("{sig}0"),
            sig[..sig.len() - 1].into(),
        ] {
            assert!(verify_mac(b"It's a Secret to Everybody", &bad, b"Hello, World!").is_err());
        }
        assert!(verify_mac(b"It's a Secret to Everybody", sig, b"Hello, world!").is_err());
    }
}
