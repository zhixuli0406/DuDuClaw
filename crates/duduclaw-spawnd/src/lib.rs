//! Root launch coordinator. Wire authority is always re-derived by the host.
pub mod admission;
pub mod readiness;
pub mod audit;
pub mod registry;
pub mod accounts;
pub mod systemd;
pub mod policy;
pub mod helper_policy;
pub mod helper_cgroup;
/// HX4 root side of in-run helper calls (per-run engine on the lifecycle thread).
#[cfg(target_os = "linux")]
pub mod helper_host;
pub mod storage;
// Linux-only: descriptor-relative root-owned anchors (`libc` is a Linux-only
// dependency of this crate, and the anchor directory only exists on Linux).
#[cfg(target_os = "linux")]
pub mod work_anchor;
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
/// R6 root update endpoint (`update.sock`): Prepare/Drain/Apply/Readback/Resume.
#[cfg(target_os = "linux")]
pub mod update_control;
#[cfg(target_os = "linux")]
pub mod channels;
#[cfg(target_os = "linux")]
pub mod egress;
/// H2 root work broker core (D-H1); H4 wires it into `server.rs` dispatch.
#[cfg(target_os = "linux")]
pub mod work_broker;

pub type Result<T> = std::result::Result<T, duduclaw_isolation::ipc::ErrorCode>;

#[cfg(target_os = "linux")]
pub mod allocation;
/// R8r-2: root's write of an employee's model connection document inside
/// the verified home of its temporarily attached image.
#[cfg(target_os = "linux")]
pub(crate) mod providers_file;

#[cfg(target_os = "linux")]
pub mod native_trampoline;
#[cfg(target_os = "linux")]
mod native_pty;

#[cfg(target_os = "linux")]
pub mod installed;

/// R8d: root writes of the host entitlements (Entitle, removal, rollover).
#[cfg(target_os = "linux")]
pub mod entitlement;

/// R7a: trampoline readiness from spawnd's own isolation self-test.
#[cfg(any(target_os = "linux", test))]
pub mod selftest;

/// R7b: `dns_route_verified` from a live probe through egress.
#[cfg(any(target_os = "linux", test))]
pub mod dns_evidence;

/// R7b: `egress_second_layer` from PID 1's view of the egress unit's IP rules.
#[cfg(any(target_os = "linux", test))]
pub mod egress_layer;

/// R7c: per-account SSH denial evidence from the probe unit run through PID 1.
#[cfg(any(target_os = "linux", test))]
pub mod ssh_evidence;

/// R7b: employee-range owners of files in spawnd's own state directories,
/// scanned once per UID allocation.
#[cfg(target_os = "linux")]
pub mod owner_inventory;

#[cfg(target_os = "linux")]
pub mod gateway_mcp;

#[cfg(any(target_os = "linux", test))]
pub mod gateway_plan;

#[cfg(target_os = "linux")]
mod native_home;

#[cfg(any(target_os = "linux", test))]
mod native_home_strategy;
