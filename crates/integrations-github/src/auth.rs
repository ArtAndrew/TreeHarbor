use crate::{Error, Result};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use reqwest::header::HeaderValue;
use serde::{Deserialize, Serialize};
use std::fmt;
use zeroize::Zeroizing;

pub struct AppCredentials {
    pub(crate) app_id: u64,
    client_id: String,
    key: EncodingKey,
}
impl fmt::Debug for AppCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppCredentials")
            .field("app_id", &self.app_id)
            .finish_non_exhaustive()
    }
}
#[derive(Serialize)]
struct Claims<'a> {
    iat: u64,
    exp: u64,
    iss: &'a str,
}
impl AppCredentials {
    /// Takes ownership so the caller's original PEM buffer is wiped on drop.
    /// TLS/cryptographic libraries may maintain their own transient copies.
    pub fn from_pem(app_id: u64, client_id: String, pem: Vec<u8>) -> Result<Self> {
        let pem = Zeroizing::new(pem);
        if app_id == 0
            || client_id.is_empty()
            || client_id.len() > 128
            || !client_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            || pem.len() > 16 * 1024
        {
            return Err(Error::Configuration);
        }
        let key = EncodingKey::from_rsa_pem(&pem).map_err(|_| Error::Signing)?;
        let credentials = Self {
            app_id,
            client_id,
            key,
        };
        // Also check key strength/algorithm at configuration time, before network IO.
        credentials.jwt(crate::now()?)?;
        Ok(credentials)
    }
    pub(crate) fn jwt(&self, now: u64) -> Result<Secret> {
        let claims = Claims {
            iat: now.checked_sub(60).ok_or(Error::Clock)?,
            exp: now.checked_add(300).ok_or(Error::Clock)?,
            iss: &self.client_id,
        };
        jsonwebtoken::encode(&Header::new(Algorithm::RS256), &claims, &self.key)
            .map(|s| Secret(Zeroizing::new(s)))
            .map_err(|_| Error::Signing)
    }
}

pub(crate) struct Secret(Zeroizing<String>);
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}
impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        String::deserialize(d).map(|s| Self(Zeroizing::new(s)))
    }
}
impl Secret {
    pub(crate) fn header(&self) -> Result<HeaderValue> {
        if self.0.is_empty() || self.0.len() > 8192 || !self.0.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(Error::TokenScope);
        }
        let value = Zeroizing::new(format!("Bearer {}", self.0.as_str()));
        let mut header = HeaderValue::from_str(&value).map_err(|_| Error::TokenScope)?;
        header.set_sensitive(true);
        Ok(header)
    }
}
