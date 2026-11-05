//! Native process backends. Isolation authority remains in the root daemon.
pub mod io;
pub mod command;
#[cfg(windows)]
mod windows_job;
pub mod local;
pub mod selection;
pub mod program_helper;
pub mod code_map_helper;
pub mod office_helper;
pub mod recording_helper;
#[cfg(target_os = "linux")]
mod recording_io;
#[cfg(target_os = "linux")]
mod office_io;
#[cfg(target_os = "linux")]
pub mod spawnd;

pub use duduclaw_isolation::execution;
pub use local::LocalBackend;
#[cfg(target_os = "linux")]
pub use spawnd::SpawndBackend;

pub mod fork_helper;
#[cfg(target_os = "linux")]
mod fork_io;
