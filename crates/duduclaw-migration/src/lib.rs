//! Durable migration components. Host and product adapters must fail closed.
pub mod archive;
pub mod bootstrap_protocol;
pub mod journal;
pub mod confirmation;
pub mod import_receiver;
pub mod import_manifest;
#[cfg(any(target_os = "linux", test))]
mod import_profile;
#[cfg(target_os = "linux")]
pub mod import_launcher;
#[cfg(target_os = "linux")]
pub mod root_sources;
#[cfg(target_os = "linux")]
pub mod root_host;
#[cfg(target_os = "linux")]
pub mod import_blobs;
pub use duduclaw_isolation::{ids, migration as contract};
#[cfg(target_os = "linux")]
pub mod snapshot;

/// Opaque failures, safe for logs: never expose old data or secret values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("migration storage unavailable")] Storage,
    #[error("migration input rejected")] Invalid,
    #[error("legacy source rejected by isolated parser")] SourceRejected,
    #[error("migration limit exceeded")] Limit,
    #[error("migration state changed")] Conflict,
    #[error("migration readback failed")] Readback,
    #[error("migration adapter unavailable")] Unavailable,
    #[error("migration authentication failed")] Authentication,
}

impl From<rusqlite::Error> for Error {
    fn from(_: rusqlite::Error) -> Self { Self::Storage }
}
impl From<std::io::Error> for Error {
    fn from(_: std::io::Error) -> Self { Self::Storage }
}
impl From<serde_json::Error> for Error {
    fn from(_: serde_json::Error) -> Self { Self::Invalid }
}
