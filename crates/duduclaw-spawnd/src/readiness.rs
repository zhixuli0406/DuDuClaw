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
        if self.named_checks().into_iter().all(|(_, value)| value) {
            Ok(())
        } else { Err(ErrorCode::IsolationUnavailable) }
    }
    /// Every primary check (all of them `check` requires) under its
    /// readiness.json name.
    pub fn named_checks(&self) -> [(&'static str, bool); 14] {
        [("registry_verified", self.registry_verified), ("accounts_locked", self.accounts_locked),
            ("ssh_live_negative_checks", self.ssh_live_negative_checks),
            ("cron_at_acl_and_jobs_verified", self.cron_at_acl_and_jobs_verified),
            ("external_iam_reservation_verified", self.external_iam_reservation_verified),
            ("trampoline_verified", self.trampoline_verified), ("landlock_self_test", self.landlock_self_test),
            ("namespace_self_test", self.namespace_self_test), ("egress_verified", self.egress_verified),
            ("dns_route_verified", self.dns_route_verified), ("storage_verified", self.storage_verified),
            ("resources_verified", self.resources_verified), ("startup_reconciled", self.startup_reconciled),
            ("process_scan_verified", self.process_scan_verified)]
    }
}

/// spawnd's one-line evidence diagnosis: `evidence complete`, or
/// `evidence incomplete: <false checks>` with `resources` (the fixed name of
/// the first failing resources sub-condition) after `resources_verified` and
/// `dns` (the fixed `dns_evidence::Reason` name) after `dns_route_verified`.
/// Fixed identifiers only.
pub fn evidence_line(checks: &Readiness, resources: Option<&str>, dns: Option<&str>) -> String {
    let failed: Vec<String> = checks.named_checks().into_iter().filter(|(_, value)| !value).map(|(name, _)| match (resources, dns) {
        (Some(detail), _) if name == "resources_verified" => format!("{name}({detail})"),
        (_, Some(detail)) if name == "dns_route_verified" => format!("{name}({detail})"),
        _ => name.to_string(),
    }).collect();
    if failed.is_empty() { "evidence complete".into() } else { format!("evidence incomplete: {}", failed.join(", ")) }
}

/// HX in-run helper readiness (D8 revision §5). Reported beside, never inside,
/// `Readiness::check`: unavailable helpers must not block ordinary runs. The
/// IPC projection (feature bit, `ErrorCode::HelperCgroupUnavailable`) is HX4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelperReadiness {
    Ready,
    /// No helper-enabled run has probed open-time cgroup credentials yet.
    Unprobed,
    Unavailable(&'static str),
}
pub fn helper_readiness(launcher_supported: bool, open_time_credentials: Option<bool>) -> HelperReadiness {
    if !launcher_supported { return HelperReadiness::Unavailable("helper_launcher_unavailable"); }
    match open_time_credentials {
        None => HelperReadiness::Unprobed,
        Some(true) => HelperReadiness::Ready,
        Some(false) => HelperReadiness::Unavailable(crate::helper_cgroup::UNAVAILABLE_REASON),
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
    /// The evidence line uses exactly the readiness.json names, lists every
    /// false primary check, the resources sub-condition, or "evidence complete".
    #[test]
    fn evidence_line_names_false_checks_with_readiness_json_names() {
        let json = serde_json::to_value(Readiness::default()).unwrap();
        let fields: Vec<&str> = json.as_object().unwrap().iter().filter(|(_, value)| value.is_boolean())
            .map(|(name, _)| name.as_str()).collect();
        let named: Vec<&str> = Readiness::default().named_checks().iter().map(|(name, _)| *name).collect();
        assert_eq!(fields.len(), named.len());
        assert!(named.iter().all(|name| fields.contains(name)), "{named:?}");
        let mut all = Readiness::default();
        assert_eq!(evidence_line(&all, None, None), format!("evidence incomplete: {}", named.join(", ")));
        for (name, _) in all.named_checks() {
            let mut value = serde_json::to_value(&all).unwrap(); value[name] = true.into();
            all = serde_json::from_value(value).unwrap();
        }
        assert!(all.check().is_ok());
        assert_eq!(evidence_line(&all, None, None), "evidence complete");
        // A detail for a passing check is never printed.
        assert_eq!(evidence_line(&all, Some("employees_slice_cgroup"), Some("pending")), "evidence complete");
        all.resources_verified = false; all.dns_route_verified = false;
        assert_eq!(evidence_line(&all, Some("employees_slice_cgroup"), None),
            "evidence incomplete: dns_route_verified, resources_verified(employees_slice_cgroup)");
        // Defect 28: the DNS sub-reason, in the same form.
        for reason in crate::dns_evidence::Reason::ALL {
            assert_eq!(evidence_line(&all, Some("employees_slice_cgroup"), Some(reason.name())),
                format!("evidence incomplete: dns_route_verified({}), resources_verified(employees_slice_cgroup)", reason.name()));
        }
        all.resources_verified = true;
        assert_eq!(evidence_line(&all, None, Some("no_public_answer")), "evidence incomplete: dns_route_verified(no_public_answer)");
    }
    #[test]
    fn helper_readiness_fails_closed_without_open_time_credentials() {
        assert_eq!(helper_readiness(false, Some(true)), HelperReadiness::Unavailable("helper_launcher_unavailable"));
        assert_eq!(helper_readiness(true, Some(false)), HelperReadiness::Unavailable("helper_cgroup_unavailable"));
        assert_eq!(helper_readiness(true, None), HelperReadiness::Unprobed);
        assert_eq!(helper_readiness(true, Some(true)), HelperReadiness::Ready);
    }
}
