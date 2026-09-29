use std::io;
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PtyError {
    #[error("failed to open PTY: {0}")]
    OpenPty(String),

    #[error("failed to spawn child process `{program}`: {source}")]
    SpawnChild { program: String, source: io::Error },

    #[error("PTY I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("PTY closed unexpectedly")]
    Closed,

    #[error("read timed out after {0:?}")]
    ReadTimeout(Duration),

    #[error("write timed out after {0:?}")]
    WriteTimeout(Duration),

    #[error("background task panicked: {0}")]
    TaskPanicked(String),
}

/// Catch-all for code paths that bubble up any sub-error.
#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error(transparent)]
    Pty(#[from] PtyError),
}
