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

#[derive(Debug, Error)]
pub enum SessionError {
    #[error(transparent)]
    Pty(#[from] PtyError),

    #[error("session is currently handling another request")]
    Busy,

    #[error("session has been shut down")]
    Shutdown,

    #[error("CLI returned malformed frame (no sentinel match)")]
    MalformedResponse,

    #[error("CLI reported error: {0}")]
    CliError(String),

    #[error("invoke timed out after {0:?}")]
    InvokeTimeout(Duration),

    #[error("boot timed out after {0:?}")]
    BootTimeout(Duration),

    #[error("child process exited during invoke (exit_code={code:?})")]
    ChildExited { code: Option<i32> },

    #[error("unknown CLI kind: {0}")]
    UnknownCliKind(String),
}

#[derive(Debug, Error)]
pub enum PoolError {
    #[error(transparent)]
    Session(#[from] SessionError),

    #[error("pool capacity exhausted for agent_id={0}")]
    Exhausted(String),

    #[error("pool is shutting down")]
    ShuttingDown,
}

/// Catch-all for code paths that bubble up any sub-error.
#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error(transparent)]
    Pty(#[from] PtyError),

    #[error(transparent)]
    Session(#[from] SessionError),

    #[error(transparent)]
    Pool(#[from] PoolError),
}
