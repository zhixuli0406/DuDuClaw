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
