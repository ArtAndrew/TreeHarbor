//! Authenticated local HTTP transport and CLI support. Unix-only at this stage.
#![cfg(unix)]
mod api_run_new;
pub mod archive;
mod backup;
pub mod cli;
pub mod client;
pub mod config;
pub mod database;
mod doctor;
pub mod download;
pub mod files;
mod github;
mod history;
pub mod import;
mod init;
mod mcp;
mod native;
mod native_advance;
mod node_cli;
pub mod pack;
mod proposals;
mod rebases;
mod review;
mod seals;
pub mod server;
mod source;
mod source_export;
mod source_read;
mod webhooks;
mod workspace_new;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "local initialization refused or incomplete; use valid private input, a short absolute path and a new directory under a private parent; preserve any existing output for inspection"
    )]
    InitState,
    #[error("{0}")]
    Backup(#[from] backup::Error),
    #[error(
        "external task requires valid exact inputs and matching private state; preserve the original state and inputs on retry"
    )]
    ApiRunState,
    #[error(
        "workspace request requires an exact native base, profile digest and matching private state; preserve the original state and inputs on retry"
    )]
    WorkspaceState,
    #[error("invalid or changed native preparation intent")]
    NativeAdvanceState,
    #[error(
        "policy draft requires valid requirements and a complete, stable configuration inspection"
    )]
    PolicyDraft,
    #[error(
        "MCP connection failed; submitted mutations may have committed; inspect their original operation keys"
    )]
    Mcp,
    #[error(
        "GitHub operation state does not match the retained request; preserve the state directory and inspect that operation's status"
    )]
    PublicationState,
    #[error("GitHub binding operation rejected by the TreeHarbor server (HTTP {status})")]
    GithubBinding { status: u16 },
    #[error("GitHub connection failed: {0}")]
    Github(#[from] treeharbor_github::Error),
    #[error(
        "local export worker failed; inspect the output before retrying with a new destination"
    )]
    ExportWorker,
    #[error("Git export failed: {0}")]
    GitExport(#[from] treeharbor_git_bridge::Error),
    #[error("node operation failed: {0}")]
    Node(#[from] treeharbor_node::Error),
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
    #[error(
        "configured PostgreSQL schema is unavailable; ensure it exists and the database role has USAGE before migration or startup"
    )]
    SchemaUnavailable,
    #[error("credential file is invalid")]
    Credential,
    #[error("invalid source profile; use full selection or bounded repository-relative paths")]
    Profile,
    #[error("native source import failed: {0}")]
    Pack(#[from] pack::Error),
    #[error(
        "import state does not match this pack, repository or request; reuse its original inputs or choose a new state file"
    )]
    ImportState,
    #[error("local import worker failed; the existing pack and state are retained")]
    ImportWorker,
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
