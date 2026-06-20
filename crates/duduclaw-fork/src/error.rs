//! Error types for the fork subsystem.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ForkError {
    #[error("overlay error: {0}")]
    Overlay(String),

    #[error("branch executor error: {0}")]
    Executor(String),

    #[error("invalid fork config: {0}")]
    Config(String),

    #[error("fork not found: {0}")]
    NotFound(String),
}

pub type Result<T> = std::result::Result<T, ForkError>;
