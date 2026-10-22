//! Trusted local node: durable files, managed external-runtime tasks and capture.
mod capture;
mod execution;
mod private;
mod seal_publication;
mod storage;
mod volumes;
mod worker;
pub use storage::{Identity, Node};
pub use volumes::VolumeRegistration;
pub use worker::Prepare;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("node runtime boundary failed")]
    Runtime(#[from] treeharbor_runtime::Error),
    #[error("node filesystem operation failed")]
    Io(#[from] std::io::Error),
    #[error("node metadata operation failed")]
    Metadata(#[from] treeharbor_metadata::Error),
    #[error("workspace materialization failed")]
    Materializer(#[from] treeharbor_materializer::Error),
    #[error("invalid node record")]
    Record(#[from] serde_json::Error),
    #[error("node filesystem worker failed")]
    Worker(#[from] tokio::task::JoinError),
    #[error("node paths must be private, owned, unlinked and operator controlled")]
    Private,
    #[error("node storage is already in use")]
    Busy,
    #[error("node identity or durable placement differs; retained data requires reconciliation")]
    Identity,
    #[error("incomplete or unknown local state; data was retained")]
    Incomplete,
    #[error("invalid node configuration")]
    Configuration,
}
pub type Result<T> = std::result::Result<T, Error>;
