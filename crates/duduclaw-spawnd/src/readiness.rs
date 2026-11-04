//! Readiness is a conjunction of current host evidence, never a service PID.
use duduclaw_isolation::ipc::{ErrorCode, PROTOCOL_VERSION};
use crate::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EgressSecondLayer { Verified, Unavailable }

/// Internal evidence, not a wire schema. Adapters must compute these checks;
/// a caller cannot supply booleans or authorize a missing component.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Readiness {
    pub protocol: u16,
    pub registry_verified: bool,
    pub accounts_locked: bool,
    pub ssh_live_negative_checks: bool,
    pub cron_at_acl_and_jobs_verified: bool,
    pub external_iam_reservation_verified: bool,
    pub trampoline_verified: bool,
    pub landlock_self_test: bool,
    pub namespace_self_test: bool,
    pub egress_verified: bool,
    pub dns_route_verified: bool,
    pub storage_verified: bool,
    pub resources_verified: bool,
    pub startup_reconciled: bool,
    pub process_scan_verified: bool,
    pub egress_second_layer: EgressSecondLayer,
}
impl Default for Readiness {
    fn default() -> Self {
        Self { protocol: PROTOCOL_VERSION, registry_verified: false, accounts_locked: false,
            ssh_live_negative_checks: false, cron_at_acl_and_jobs_verified: false,
            external_iam_reservation_verified: false, trampoline_verified: false,
            landlock_self_test: false, namespace_self_test: false, egress_verified: false,
            dns_route_verified: false, storage_verified: false, resources_verified: false,
            startup_reconciled: false, process_scan_verified: false, egress_second_layer: EgressSecondLayer::Unavailable }
    }
}
impl Readiness {
    pub fn check(&self) -> Result<()> {
        if self.protocol != PROTOCOL_VERSION { return Err(ErrorCode::UnsupportedVersion); }
        if [self.registry_verified, self.accounts_locked, self.ssh_live_negative_checks,
            self.cron_at_acl_and_jobs_verified, self.external_iam_reservation_verified,
            self.trampoline_verified, self.landlock_self_test, self.namespace_self_test,
            self.egress_verified, self.dns_route_verified, self.storage_verified,
            self.resources_verified, self.startup_reconciled, self.process_scan_verified].into_iter().all(|value| value) {
            Ok(())
        } else { Err(ErrorCode::IsolationUnavailable) }
    }
}

/// R7-M2: egress may use resolved's Unix socket or a dedicated DNS broker.
/// No private resolver address exception is accepted, irrespective of port intent.
pub fn validate_egress_ip_allow(address_exceptions: &[String]) -> Result<()> {
    if address_exceptions.is_empty() { Ok(()) } else { Err(ErrorCode::IsolationUnavailable) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn readiness_requires_every_primary_check_but_not_secondary_bpf() {
        assert!(Readiness::default().check().is_err());
        let valid = Readiness { protocol: 1, registry_verified: true, accounts_locked: true,
            ssh_live_negative_checks: true, cron_at_acl_and_jobs_verified: true,
            external_iam_reservation_verified: true, trampoline_verified: true,
            landlock_self_test: true, namespace_self_test: true, egress_verified: true,
            dns_route_verified: true, storage_verified: true, resources_verified: true,
            startup_reconciled: true, process_scan_verified: true, egress_second_layer: EgressSecondLayer::Unavailable };
        assert!(valid.check().is_ok());
        let mut checks = vec![valid.clone(); 14];
        checks[0].registry_verified = false;
        checks[1].accounts_locked = false;
        checks[2].ssh_live_negative_checks = false;
        checks[3].cron_at_acl_and_jobs_verified = false;
        checks[4].external_iam_reservation_verified = false;
        checks[5].trampoline_verified = false;
        checks[6].landlock_self_test = false;
        checks[7].namespace_self_test = false;
        checks[8].egress_verified = false;
        checks[9].dns_route_verified = false;
        checks[10].storage_verified = false;
        checks[11].resources_verified = false;
        checks[12].startup_reconciled = false;
        checks[13].process_scan_verified = false;
        for check in checks { assert_eq!(check.check(), Err(ErrorCode::IsolationUnavailable)); }
        assert!(validate_egress_ip_allow(&["10.0.0.1/32".into()]).is_err());
        assert!(validate_egress_ip_allow(&[]).is_ok());
    }
}
