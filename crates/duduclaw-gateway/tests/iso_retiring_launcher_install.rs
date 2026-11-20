//! R8n-2 A: the production parse launcher (host-wide retirement) satisfies
//! the boot contract. A separate test binary because the launcher slot is a
//! process-wide OnceLock that the gateway's unit tests install a fake into.
#![cfg(target_os = "linux")]

use duduclaw_gateway::isolation_execution::install_worker_services;
use duduclaw_parse_worker::client::WorkerLauncher;
use duduclaw_parse_worker::retirement::RetiringLauncher;

#[test]
fn boot_worker_services_accept_the_retiring_launcher() {
    assert!(RetiringLauncher.supports_live_authority());
    // Installation does not launch: no lease, bus or socket is touched here.
    assert!(install_worker_services(Box::new(RetiringLauncher)).is_ok());
    assert!(duduclaw_parse_worker::client::launcher_installed());
    // The slot is final: reconfiguration is refused.
    assert!(install_worker_services(Box::new(RetiringLauncher)).is_err());
}
