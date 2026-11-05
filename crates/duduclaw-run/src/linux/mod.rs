//! All unsafe OS calls are confined to Linux modules. Call from a single-threaded
//! trampoline process only; never load the launcher into the gateway process.
#![allow(unsafe_code)]
pub mod bootstrap;
pub mod fd;
pub mod mounts;
pub mod privileges;
pub mod probe;
pub mod runner;

pub(crate) fn checked(value: libc::c_int, error: crate::Error) -> crate::Result<()> {
    if value == -1 { Err(error) } else { Ok(()) }
}
pub(crate) fn errno() -> i32 { std::io::Error::last_os_error().raw_os_error().unwrap_or(0) }
