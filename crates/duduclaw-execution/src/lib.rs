//! Native process backends. Isolation authority remains in the root daemon.
pub mod io;
pub mod command;
#[cfg(windows)]
mod windows_job;
pub mod local;
pub mod selection;
#[cfg(target_os = "linux")]
pub mod spawnd;

pub use duduclaw_isolation::execution;
pub use local::LocalBackend;
#[cfg(target_os = "linux")]
pub use spawnd::SpawndBackend;
