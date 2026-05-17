//! Cross-platform PTY-pool runtime for orchestrating CLI-based AI agents.
//!
//! Design lineage:
//! - [`dorkitude/maude`](https://github.com/dorkitude/maude) — tmux-backed long-lived Claude TUI
//!   sessions (Unix only).
//! - [`runtorque/torque`](https://github.com/runtorque/torque) — PTY supervisor over Unix Domain
//!   Sockets (Unix only).
//!
//! This crate reimplements the same idea (long-lived CLI sessions reused across many
//! requests) on top of [`portable-pty`], which abstracts the OS PTY layer (ConPTY on
//! Windows 10 1809+, openpty on Unix). Response framing uses an in-band sentinel
//! protocol — no scrollback scraping, no sidecar.
//!
//! See `commercial/docs/TODO-cli-pty-pool-worker.md` for the full design rationale.

pub mod envelope;
pub mod error;
pub mod oneshot;
pub mod platform;
pub mod pty;
pub mod session;

// Phase 2+ surfaces (stubs added later)
pub mod pool;
pub mod supervisor;

pub use envelope::{
    Envelope, Frame, INTERACTIVE_SENTINEL, REQ_END, REQ_START, RSP_END, RSP_START, ResponseFormat,
    extract_payload_with_chrome_filter, frame_request, parse_frame, strip_ansi,
};
pub use error::{PoolError, PtyError, RuntimeError, SessionError};
pub use oneshot::{OneshotInvocation, OneshotOutput, oneshot_pty_invoke};
pub use pool::{AgentKey, PoolConfig, PooledSession, PtyPool};
pub use pty::{PtyCommand, PtyHandle, PtySystemKind, spawn_pty};
pub use session::{CliKind, PtySession, SpawnOpts};
pub use supervisor::{RestartPolicy, Supervisor};
