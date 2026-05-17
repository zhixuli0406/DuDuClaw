//! Platform-specific child-process supervision primitives.
//!
//! - Unix: process group + `SIGTERM` → `SIGKILL` escalation.
//! - Windows: Job Objects with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` so the whole
//!   process tree dies when the gateway exits.
//!
//! Phase 1 ships minimal stubs; Phase 4 fleshes them out with real OS calls.

#[cfg(unix)]
pub mod unix;

#[cfg(windows)]
pub mod windows;

#[cfg(unix)]
pub use unix::ChildGroup;

#[cfg(windows)]
pub use windows::ChildGroup;
