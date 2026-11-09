//! All unsafe OS calls are confined to Linux modules. Call from a single-threaded
//! trampoline process only; never load the launcher into the gateway process.
#![allow(unsafe_code)]
pub mod bootstrap;
pub mod sealed_registry;
pub mod fd;
pub(crate) mod launcher;
pub mod mounts;
pub mod privileges;
pub mod probe;
pub mod runner;
/// Raw-syscall wrappers for glibc functions newer than the release baseline;
/// the same source as `duduclaw_execution::linux_syscalls` (no extra dependency).
#[allow(dead_code)]
#[path = "../../../duduclaw-execution/src/linux_syscalls.rs"]
pub mod syscalls;

pub(crate) fn checked(value: libc::c_int, error: crate::Error) -> crate::Result<()> {
    if value == -1 { Err(error) } else { Ok(()) }
}
pub(crate) fn errno() -> i32 { std::io::Error::last_os_error().raw_os_error().unwrap_or(0) }
