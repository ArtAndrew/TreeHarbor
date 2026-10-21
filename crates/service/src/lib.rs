//! Authenticated local HTTP transport and CLI support. Unix-only at this stage.
#![cfg(unix)]
pub mod cli;
pub mod client;
pub mod config;
pub mod database;
pub mod files;
pub mod server;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("local filesystem or socket operation failed")]
    Io(#[from] std::io::Error),
    #[error("configuration is invalid; use a private Unix PostgreSQL socket and valid limits")]
    Config,
    #[error("file or directory must be owned by the current user, private, and not a symlink")]
    UnsafeFile,
    #[error("another server owns this socket or its lock")]
    AlreadyRunning,
    #[error("database connection, schema or durability check failed")]
    Database,
    #[error("credential file is invalid")]
    Credential,
    #[error("request or response exceeds its size limit")]
    Size,
    #[error("request timed out; its outcome may be unknown, retry with the same idempotency key")]
    Timeout,
    #[error("HTTP connection or response failed; retry mutations with the same idempotency key")]
    Http,
    #[error("administrative operation failed")]
    Administration,
    #[error(
        "credential publication failed; token {token} was revoked; remove any resulting file before retrying"
    )]
    CredentialWriteRevoked { token: uuid::Uuid },
    #[error("credential publication and revocation failed; revoke token {token} before retrying")]
    CredentialWriteUncertain { token: uuid::Uuid },
}
pub type Result<T> = std::result::Result<T, Error>;
