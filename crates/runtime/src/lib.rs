//! Trusted external-runtime control. This is not agent-facing authorization.
#![recursion_limit = "256"]
mod engine;
mod journal;
mod profile;
mod volume;
pub use engine::{Container, Engine, Inspection, RuntimeState, Specification};
pub use journal::{CleanupGuard, Controller, Outcome, RemovalExpectation, RemovedGuard};
pub use profile::{Definition, Profile, Task};
pub use volume::{Volume, VolumeIdentity};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unsupported runtime host or engine")]
    Unsupported,
    #[error("invalid runtime profile or task")]
    Invalid,
    #[error("runtime filesystem or transport operation failed ({})", .0.kind())]
    Io(#[from] std::io::Error),
    #[error("invalid runtime response or private record")]
    Json(#[from] serde_json::Error),
    #[error("runtime identity or configuration does not match")]
    Mismatch,
    #[error("runtime request outcome is unknown; reconcile instead of repeating execution")]
    Unknown,
    #[error("runtime state does not permit the operation")]
    State,
    #[error("runtime response exceeds the limit")]
    Limit,
    #[error("engine returned unexpected status {0}; inspect before retrying a mutation")]
    EngineStatus(u16),
}
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod error_tests {
    #[test]
    fn filesystem_diagnostic_includes_kind_without_private_error_message() {
        let error = super::Error::Io(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "private-source-or-credential-sentinel",
        ));
        let public = error.to_string();
        assert!(public.contains("permission denied"));
        assert!(!public.contains("sentinel"));
    }
}
