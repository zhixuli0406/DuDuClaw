//! Cross-platform PTY invocation for CLI-based AI agents.
//!
//! Spawns a CLI (`claude`, `codex`, …) under a real pseudo-terminal so it sees
//! a TTY on every platform, via [`portable-pty`] (ConPTY on Windows 10 1809+,
//! openpty on Unix), then drains its stdout to EOF.
//!
//! History: this crate used to also host a long-lived **PTY session pool**
//! (sentinel-framed interactive REPL sessions, an eviction policy, a restart
//! supervisor, and an out-of-process worker). That was built as standby for
//! Anthropic's 2026-06-15 programmatic-usage split, which was paused on the
//! day and never re-activated; the pool also shared one REPL across an agent's
//! conversations, which leaked context between them. It was removed in
//! 2026-09 — see `docs/features/27-pty-pool-runtime.md`. What remains is the
//! one-shot invocation the gateway actually uses.

pub mod ansi;
pub mod error;
pub mod oneshot;
pub mod pty;

pub use ansi::strip_ansi;
pub use error::{PtyError, RuntimeError};
pub use oneshot::{OneshotInvocation, OneshotOutput, oneshot_pty_invoke};
pub use pty::{PtyCommand, PtyHandle, PtySystemKind, spawn_pty};
