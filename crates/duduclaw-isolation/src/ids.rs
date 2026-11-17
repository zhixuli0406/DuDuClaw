//! Identity constants and UID allocation checks (design §3, §5, §5.1, §17.7).
use serde::{Deserialize, Serialize};
use crate::ContractError;

macro_rules! id_type {
    ($name:ident, $size:expr, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub [u8; $size]);
    };
}
id_type!(EmployeeId, 16, "Immutable registry identity, independent of employee display names (§3, §5). Never recycled.");
id_type!(RunId, 16, "Unpredictable run identity reserved before capability creation (§4, §6.1).");
id_type!(RequestId, 16, "Opaque idempotency/correlation identity (§4, §8).");
id_type!(Sha256Digest, 32, "Raw SHA-256 digest bytes, never a self-asserted trust decision (§6.1, §8).");

pub const EMPLOYEE_UID_MIN: u32 = 70_000;
pub const EMPLOYEE_UID_MAX: u32 = 99_999;
pub const GATEWAY_USER: &str = "duduclaw";
pub const SUPERVISOR_USER: &str = "duduclaw-runsup";
pub const PARSE_WORKER_USER: &str = "duduclaw-work";
pub const IMPORT_WORKER_USER: &str = "duduclaw-import";
pub const EGRESS_USER: &str = "duduclaw-egress";
pub const DOWNLOAD_USER: &str = "duduclaw-dl";
/// The root registry stores a unique, permanently reserved account suffix
/// separately from the 128-bit EmployeeId (§3, §5). Use the non-recycled numeric
/// uid in decimal as that immutable suffix; never truncate/hash an EmployeeId
/// into a username. This keeps even a u32 uid below the 32-byte account limit.
pub const EMPLOYEE_USER_PATTERN: &str = "duduclaw-emp-<registry-uid-decimal>";

// Paths and unit names below are phase-zero selections: r7 requires a fixed,
// root-owned endpoint but does not assign exact socket basenames.
pub const SPAWND_SOCKET: &str = "/run/duduclaw-spawnd/control.sock";
pub const EGRESS_CONTROL_SOCKET: &str = "/run/duduclaw-egress/control.sock";
pub const UPDATED_SOCKET: &str = "/run/duduclaw-updated/control.sock";
pub const IMPORT_SOCKET: &str = "/run/duduclaw-migrate/import.sock";
pub const RUN_UNIT_PATTERN: &str = "duduclaw-run-<run-id>.service";
pub const WORKER_UNIT_PATTERN: &str = "duduclaw-work-<request-id>.service";
pub const IMPORT_UNIT_PATTERN: &str = "duduclaw-import-<request-id>.service";
pub const EMPLOYEES_SLICE: &str = "duduclaw-employees.slice";
pub const EMPLOYEE_SLICE_PATTERN: &str = "duduclaw-employees-<immutable-id>.slice";
pub const UPDATED_UNIT: &str = "duduclaw-updated.service";
/// R6 (plan addendum "啟動順序與「一致」的判定", points 3 and 6): the oneshot
/// unit that completes only once spawnd reports the isolated component set
/// verified and consistent (`duduclaw-spawnd wait-consistent`). It is bound to
/// spawnd (`BindsTo=`/`PartOf=`/`After=`), so it drops whenever spawnd stops,
/// crashes or restarts, and spawnd pulls it in again (`Wants=`) on every start.
pub const ISOLATION_READY_UNIT: &str = "duduclaw-isolation-ready.service";
/// The two `[Unit]` lines the isolated-install gateway unit (written by the
/// installer package, R3) must carry, exactly as `duduclaw-egress.service`
/// does, so the gateway never starts on an unverified or mixed set and is
/// stopped whenever the gate drops (`BindsTo=`, not only `Requires=`). The
/// single-binary gateway unit (no spawnd on those hosts) must not carry them.
pub const ISOLATED_GATEWAY_UNIT_DEPENDENCIES: &str =
    "BindsTo=duduclaw-isolation-ready.service\nAfter=duduclaw-isolation-ready.service\n";
/// The egress service (bound to [`ISOLATION_READY_UNIT`]) and the socket that
/// activates it, and the isolated-install gateway unit (bound to it as well).
pub const EGRESS_UNIT: &str = "duduclaw-egress.service";
pub const EGRESS_SOCKET_UNIT: &str = "duduclaw-egress.socket";
pub const ISOLATED_GATEWAY_UNIT: &str = "duduclaw-gateway.service";
/// R6 `Apply` precondition: the consistency gate, every unit bound to it and
/// the socket that could start one of them again. The updater stops and checks
/// exactly these before `Apply`; spawnd refuses `Apply` unless PID 1 reports
/// each of them inactive with no pending job.
pub const ISOLATION_GATED_UNITS: [&str; 4] = [ISOLATION_READY_UNIT, EGRESS_SOCKET_UNIT, EGRESS_UNIT, ISOLATED_GATEWAY_UNIT];
/// R7a: the dedicated, non-login account `duduclaw-run-selftest` drops to. It
/// owns no data, is never an employee and never a service identity.
pub const SELFTEST_USER: &str = "duduclaw-selftest";
/// R7a: the fixed, pre-installed oneshot unit that runs the isolation self-test.
/// The only unit spawnd starts by name (`Systemd::run_fixed_unit`); its name is
/// deliberately not of the run-unit shape (`duduclaw-run-<hex>.service`), so no
/// run inventory, census or reconciliation ever counts it as an employee run.
pub const ISOLATION_SELFTEST_UNIT: &str = "duduclaw-isolation-selftest.service";
/// R7c: the pre-installed template unit (root:root 0644, no `[Install]`) that
/// runs the one-shot SSH denial probe for one employee account. Instances are
/// `duduclaw-isolation-probe@<account>.service`; never the run-unit shape.
pub const ISOLATION_PROBE_UNIT_TEMPLATE: &str = "duduclaw-isolation-probe@.service";
/// R7c: name prefix of every probe instance (followed by the account name and
/// `.service`).
pub const ISOLATION_PROBE_UNIT_PREFIX: &str = "duduclaw-isolation-probe@";
/// The literal prefix of every employee account name (`EMPLOYEE_USER_PATTERN`).
pub const EMPLOYEE_ACCOUNT_PREFIX: &str = "duduclaw-emp-";

/// R7c: the single account-name rule the SSH denial probe enforces,
/// `^duduclaw-emp-[a-z0-9-]{1,48}$`. Every name spawnd allocates
/// (`duduclaw-emp-<uid decimal>`) satisfies it.
pub fn employee_account_name_valid(name: &str) -> bool {
    name.strip_prefix(EMPLOYEE_ACCOUNT_PREFIX).is_some_and(|rest| (1..=48).contains(&rest.len())
        && rest.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'))
}
/// Service accounts the self-test identity must never coincide with.
pub const SERVICE_USERS: [&str; 6] = [GATEWAY_USER, SUPERVISOR_USER, PARSE_WORKER_USER,
    IMPORT_WORKER_USER, EGRESS_USER, DOWNLOAD_USER];

/// R7a: the resolved self-test identity is acceptable only if it is neither
/// root nor in the employee range, and neither its uid nor its gid equals any
/// resolved service account's (`services`: `(uid, gid)` of each [`SERVICE_USERS`]
/// entry that exists on the host).
pub fn selftest_identity_allowed(uid: u32, gid: u32, services: &[(u32, u32)]) -> bool {
    let employee = |id: u32| (EMPLOYEE_UID_MIN..=EMPLOYEE_UID_MAX).contains(&id);
    uid != 0 && gid != 0 && uid != u32::MAX && gid != u32::MAX && !employee(uid) && !employee(gid)
        && !services.iter().any(|(service_uid, service_gid)| *service_uid == uid || *service_gid == gid)
}

/// Inclusive numeric interval, checked before allocation (§5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UidRange {
    pub first: u32,
    pub last: u32,
}

pub const PERMANENT_UID_EXCLUSIONS: &[UidRange] = &[
    UidRange { first: 0, last: 999 },
    UidRange { first: 60001, last: 60513 },
    UidRange { first: 61184, last: 65519 },
    UidRange { first: 65534, last: 65535 },
    UidRange { first: 524288, last: 1879048191 },
    UidRange { first: 2147483648, last: u32::MAX },
];

impl UidRange {
    fn overlaps(self, other: Self) -> bool {
        self.first <= other.last && other.first <= self.last
    }
}

/// Host-derived exclusion snapshot (§5, R7-M5). `None` means unreadable, not empty.
/// The allocator must also check NSS/userdb, owner collisions, IAM reservations,
/// permanent tombstones and account lockdown under its global lock. This pure
/// check does not allocate an account or establish protected readiness.
pub struct UidPolicy<'a> {
    pub employee_range: UidRange,
    pub system_accounts: Option<UidRange>,
    pub login_accounts: Option<UidRange>,
    pub subuids: Option<&'a [UidRange]>,
    pub subgids: Option<&'a [UidRange]>,
    pub systemd_reserved: Option<&'a [UidRange]>,
}

impl UidPolicy<'_> {
    /// Reject the entire configured range on any overlap, even outside `uid`.
    pub fn check(&self, uid: u32) -> Result<(), ContractError> {
        let range = self.employee_range;
        if range.first > range.last || uid < range.first || uid > range.last {
            return Err(ContractError::InvalidValue("employee uid range"));
        }
        let system = self.system_accounts.ok_or(ContractError::InvalidValue("uid exclusions unavailable"))?;
        let login = self.login_accounts.ok_or(ContractError::InvalidValue("uid exclusions unavailable"))?;
        let subuids = self.subuids.ok_or(ContractError::InvalidValue("uid exclusions unavailable"))?;
        let subgids = self.subgids.ok_or(ContractError::InvalidValue("uid exclusions unavailable"))?;
        let reserved = self.systemd_reserved.ok_or(ContractError::InvalidValue("uid exclusions unavailable"))?;
        for excluded in [system, login].iter().chain(PERMANENT_UID_EXCLUSIONS)
            .chain(subuids).chain(subgids).chain(reserved) {
            if excluded.first > excluded.last || range.overlaps(*excluded) {
                return Err(ContractError::InvalidValue("uid exclusion overlap"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn isolated_gateway_unit_dependencies_are_pinned() {
        assert_eq!(ISOLATION_READY_UNIT, "duduclaw-isolation-ready.service");
        assert_eq!(ISOLATED_GATEWAY_UNIT_DEPENDENCIES,
            "BindsTo=duduclaw-isolation-ready.service\nAfter=duduclaw-isolation-ready.service\n");
        let lines: Vec<&str> = ISOLATED_GATEWAY_UNIT_DEPENDENCIES.lines().collect();
        assert_eq!(lines, [format!("BindsTo={ISOLATION_READY_UNIT}"), format!("After={ISOLATION_READY_UNIT}")]);
        // Requires= alone would leave the gateway running after the gate drops.
        assert!(!ISOLATED_GATEWAY_UNIT_DEPENDENCIES.contains("Requires="));
        assert_eq!(ISOLATION_GATED_UNITS, ["duduclaw-isolation-ready.service", "duduclaw-egress.socket",
            "duduclaw-egress.service", "duduclaw-gateway.service"]);
    }
    #[test]
    fn selftest_unit_and_identity_are_pinned_and_refuse_privileged_or_employee_ids() {
        assert_eq!(SELFTEST_USER, "duduclaw-selftest");
        assert_eq!(ISOLATION_SELFTEST_UNIT, "duduclaw-isolation-selftest.service");
        // Never the run-unit shape (duduclaw-run-<32 hex>.service).
        assert!(!ISOLATION_SELFTEST_UNIT.starts_with("duduclaw-run-"));
        assert!(!SERVICE_USERS.contains(&SELFTEST_USER));
        let services = [(990, 990), (991, 989)];
        assert!(selftest_identity_allowed(995, 995, &services));
        for (uid, gid) in [(0, 995), (995, 0), (990, 995), (995, 989), (70000, 995), (995, 99999),
            (u32::MAX, 995), (995, u32::MAX)] {
            assert!(!selftest_identity_allowed(uid, gid, &services), "{uid}:{gid}");
        }
    }
    #[test]
    fn probe_unit_names_and_employee_account_rule_are_pinned() {
        assert_eq!(ISOLATION_PROBE_UNIT_TEMPLATE, "duduclaw-isolation-probe@.service");
        assert_eq!(ISOLATION_PROBE_UNIT_PREFIX, "duduclaw-isolation-probe@");
        assert!(ISOLATION_PROBE_UNIT_TEMPLATE.starts_with(ISOLATION_PROBE_UNIT_PREFIX));
        assert!(!ISOLATION_PROBE_UNIT_PREFIX.starts_with("duduclaw-run-"));
        assert_eq!(EMPLOYEE_ACCOUNT_PREFIX, "duduclaw-emp-");
        assert!(EMPLOYEE_USER_PATTERN.starts_with(EMPLOYEE_ACCOUNT_PREFIX));
        let longest = format!("duduclaw-emp-{}", "a".repeat(48));
        let too_long = format!("duduclaw-emp-{}", "a".repeat(49));
        for good in ["duduclaw-emp-70000", "duduclaw-emp-probetest", "duduclaw-emp-a", "duduclaw-emp--", longest.as_str()] {
            assert!(employee_account_name_valid(good), "{good}");
        }
        for bad in ["duduclaw-emp-", "duduclaw-emp-A", "duduclaw-emp-a_b", "duduclaw-emp-a.b", "duduclaw-emp-a b",
            "Duduclaw-emp-1", "xduduclaw-emp-1", "duduclaw-emp-1\n", "duduclaw-emp-é", "duduclaw-runsup", "root", "",
            too_long.as_str()] {
            assert!(!employee_account_name_valid(bad), "{bad:?}");
        }
    }
    #[test]
    fn uid_range_checks_fail_closed() {
        let mut policy = UidPolicy {
            employee_range: UidRange { first: EMPLOYEE_UID_MIN, last: EMPLOYEE_UID_MAX },
            system_accounts: Some(UidRange { first: 100, last: 999 }),
            login_accounts: Some(UidRange { first: 1000, last: 60000 }),
            subuids: Some(&[]), subgids: Some(&[]), systemd_reserved: Some(&[]),
        };
        for uid in [70000, 99999] { assert_eq!(policy.check(uid), Ok(())); }
        for uid in [0, 61184, 65519, 69999, 100000, u32::MAX] { assert!(policy.check(uid).is_err()); }
        policy.subuids = None;
        assert!(policy.check(70000).is_err());
        policy.subuids = Some(&[UidRange { first: 90000, last: 100000 }]);
        assert!(policy.check(70000).is_err());
        policy.subuids = Some(&[]);
        for excluded in PERMANENT_UID_EXCLUSIONS {
            policy.employee_range = *excluded;
            assert!(policy.check(excluded.first).is_err());
        }
    }
}
