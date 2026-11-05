//! Root launch coordinator. Wire authority is always re-derived by the host.
pub mod admission;
pub mod readiness;
pub mod audit;
pub mod registry;
pub mod accounts;
pub mod systemd;
pub mod policy;
pub mod storage;
pub mod bootstrap;
#[cfg(target_os = "linux")]
pub mod bootstrap_mounts;
#[cfg(target_os = "linux")]
pub mod host;
#[cfg(any(target_os = "linux", all(test, unix)))]
mod host_worker;
#[cfg(target_os = "linux")]
mod host_state;
#[cfg(target_os = "linux")]
pub mod transport;
#[cfg(target_os = "linux")]
pub mod fds;
#[cfg(target_os = "linux")]
pub mod server;
#[cfg(target_os = "linux")]
pub mod channels;
#[cfg(target_os = "linux")]
pub mod egress;

pub type Result<T> = std::result::Result<T, duduclaw_isolation::ipc::ErrorCode>;

#[cfg(target_os = "linux")]
pub mod allocation;

#[cfg(target_os = "linux")]
pub mod native_trampoline;
#[cfg(target_os = "linux")]
mod native_pty;

#[cfg(target_os = "linux")]
pub mod installed;

#[cfg(target_os = "linux")]
pub mod gateway_mcp;

#[cfg(any(target_os = "linux", test))]
pub mod gateway_plan;

#[cfg(target_os = "linux")]
mod native_home;

#[cfg(any(target_os = "linux", test))]
mod native_home_strategy;
