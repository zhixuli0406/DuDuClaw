//! Native process backends. Isolation authority remains in the root daemon.
pub mod io;
pub mod command;
#[cfg(windows)]
mod windows_job;
pub mod local;
pub mod selection;
pub mod workspace;
pub mod program_helper;
pub mod helper_layout;
pub mod code_map_helper;
pub mod office_helper;
pub mod recording_helper;
#[cfg(target_os = "linux")]
mod recording_io;
/// HX6: nested Landlock (fs + TCP pin + scoping) for the recording's browser.
#[cfg(target_os = "linux")]
pub mod recording_sandbox;
/// HX6 security fix round: nested seccomp for the recording's browser tree
/// (TCP-only sockets, no `setsid`/`setpgid`, no TCP Fast Open).
pub mod recording_seccomp;
#[cfg(target_os = "linux")]
mod office_io;
#[cfg(target_os = "linux")]
pub mod spawnd;
/// Raw-syscall helpers for glibc wrappers newer than the release baseline.
#[cfg(target_os = "linux")]
pub mod linux_syscalls;

pub use duduclaw_isolation::execution;
pub use local::LocalBackend;
#[cfg(target_os = "linux")]
pub use spawnd::SpawndBackend;

pub mod fork_helper;
#[cfg(target_os = "linux")]
mod fork_io;
