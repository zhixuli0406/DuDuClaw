//! Package H integration boundary. No host path or image fd crosses this API.
use duduclaw_isolation::{execution::{DiskQuota, RunHandle}, ids::{EmployeeId, RequestId, Sha256Digest}, ipc::ErrorCode};
use crate::Result;

#[cfg(any(target_os = "linux", test))]
mod adapter_state;
#[cfg(target_os = "linux")]
mod adapter;
#[cfg(target_os = "linux")]
pub use adapter::LoopStorage;

/// G must bind the complete approved proposal to a stable operation id. Retries
/// with the same id and different content must fail, including after a restart.
#[derive(Debug, Clone)]
pub struct ProvisionRequest {
    pub employee: EmployeeId,
    pub uid: u32,
    pub operation_id: RequestId,
    pub proposal_digest: Sha256Digest,
    pub quota: DiskQuota,
}
impl ProvisionRequest {
    pub fn validate(&self) -> Result<()> {
        let (bytes, inodes) = quota_limits(&self.quota);
        if !valid_employee_uid(self.uid) || bytes == 0 || inodes == 0 {
            return Err(ErrorCode::InvalidRequest);
        }
        Ok(())
    }
}
pub fn valid_employee_uid(uid: u32) -> bool {
    uid >= 1000 && !duduclaw_isolation::ids::PERMANENT_UID_EXCLUSIONS.iter().any(|range| uid >= range.first && uid <= range.last)
}
pub fn quota_limits(quota: &DiskQuota) -> (u64, u64) {
    match quota { DiskQuota::LoopFilesystem { bytes, inodes } | DiskQuota::ProjectQuota { bytes, inodes } => (*bytes, *inodes) }
}

/// H verifies the entire backing-device graph, hard quota and host reserve.
/// A successful provision does not itself authorize an attach or a new run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageUsage {
    pub employee: EmployeeId,
    pub bytes: u64,
    pub inodes: u64,
    pub limit_bytes: u64,
    pub limit_inodes: u64,
    pub hard_quota_verified: bool,
    pub device_graph_verified: bool,
    pub host_reserve_verified: bool,
}
impl StorageUsage {
    pub fn check(&self, employee: EmployeeId, quota: &DiskQuota) -> Result<()> {
        let (bytes, inodes) = quota_limits(quota);
        if self.employee != employee || bytes == 0 || inodes == 0
            || self.limit_bytes == 0 || self.limit_bytes > bytes || self.limit_inodes == 0 || self.limit_inodes > inodes
            || self.bytes > self.limit_bytes || self.inodes > self.limit_inodes
            || !self.hard_quota_verified || !self.device_graph_verified || !self.host_reserve_verified {
            return Err(ErrorCode::IsolationUnavailable);
        }
        Ok(())
    }
}

/// Host-side attachment identity; never sent to the gateway. The token is H's
/// durable generation, preventing an old cleanup from detaching a newer run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub employee: EmployeeId,
    pub run: RunHandle,
    pub token: RequestId,
    pub device_major: u32,
    pub device_minor: u32,
}
impl Attachment {
    /// Cleanup also accepts a durable attach intent with no observed device.
    pub fn check_identity(&self, employee: EmployeeId, run: RunHandle) -> Result<()> {
        if self.employee != employee || self.run != run { return Err(ErrorCode::IsolationUnavailable); }
        Ok(())
    }
    /// A launch additionally requires an actually observed mounted device.
    pub fn check(&self, employee: EmployeeId, run: RunHandle) -> Result<()> {
        self.check_identity(employee, run)?;
        if self.device_major == 0 { return Err(ErrorCode::IsolationUnavailable); }
        Ok(())
    }
}

/// H's adapter supplies detached private recursive mounts, never ordinary
/// directory fds. B1/B2 must read back mount identities and restrictive flags.
/// Images, loop devices and root anchor directories must never be handed over.
pub struct AttachedStorage {
    pub receipt: Attachment,
    #[cfg(target_os = "linux")]
    pub home: std::os::fd::OwnedFd,
    #[cfg(target_os = "linux")]
    pub work: std::os::fd::OwnedFd,
}

/// Implemented by LoopStorage using package H and a separate durable B1 journal.
/// All calls are bounded worker operations; never block the intrusion scanner.
/// Provision/destroy require G's administrator-approved idempotent operation.
/// Attach rechecks space/quota/device evidence. Detach is idempotent and only
/// occurs after inactive/empty/no-UID cleanup, including restart reconciliation.
/// Destroy never recycles the employee uid and refuses any active attachment.
pub trait Storage: Send {
    fn readiness(&mut self) -> Result<()>;
    fn provision(&mut self, request: &ProvisionRequest) -> Result<StorageUsage>;
    fn attach(&mut self, employee: EmployeeId, uid: u32, run: RunHandle, quota: &DiskQuota) -> Result<AttachedStorage>;
    fn detach(&mut self, attachment: &Attachment) -> Result<()>;
    fn destroy(&mut self, employee: EmployeeId, operation: RequestId, proposal_digest: Sha256Digest) -> Result<()>;
    fn usage(&mut self, employee: EmployeeId) -> Result<StorageUsage>;
    /// Durable attachments left by an earlier daemon. An unavailable inventory
    /// is an error, not an empty list. B1 reconciles every listed run first.
    fn attachments(&mut self) -> Result<Vec<Attachment>>;
    /// H3 (D-H1): `(dev, inode)` of the employee's root-owned work anchor, the
    /// stable `employee-work` identity. Only `work_anchor::AnchoredStorage`
    /// (the allocation/retire hook) has one; everything else fails closed.
    /// Absent anchor ⇒ `NotFound`. Never creates.
    fn work_anchor(&mut self, _employee: EmployeeId) -> Result<duduclaw_isolation::ipc::WorkAnchorIdentity> {
        Err(ErrorCode::WorkspaceBrokerUnavailable)
    }
    /// H4 start-up backfill for an already allocated employee (allocated
    /// before H3): create its anchor if absent. `Ok(true)` = created now,
    /// `Ok(false)` = an existing anchor passed every check. Never repairs.
    /// Only `work_anchor::AnchoredStorage` implements it.
    fn backfill_work_anchor(&mut self, _employee: EmployeeId) -> Result<bool> {
        Err(ErrorCode::WorkspaceBrokerUnavailable)
    }
    /// F6l (F6 §3.4 S5a/S5b, §3.12 step 7): every employee storage still
    /// holds any trace of — an image in the kernel-verified inventory or a
    /// ledger entry that is not `Destroyed`. Unreadable ⇒ error, never an
    /// empty list (an authoritative `NotFound` needs a positive "nothing").
    fn traces(&mut self) -> Result<Vec<EmployeeId>> { Err(ErrorCode::IsolationUnavailable) }
    /// R8r-2: write or remove the employee's model connection document in
    /// its own home (`home/.duduclaw-api/providers.json` of the image), under
    /// a durable maintenance marker ([`maintain`]). Only `LoopStorage` (and
    /// the anchor hook forwarding to it) implements it; everything else
    /// refuses. A live lease of the employee is [`ProvidersFailure::Busy`].
    fn write_providers(&mut self, _employee: EmployeeId, _uid: u32, _change: ProvidersChange<'_>)
        -> std::result::Result<(), ProvidersFailure> {
        Err(ProvidersFailure::Unavailable)
    }
    /// R8r-2 start-up reconciliation: finish every maintenance an earlier
    /// daemon left unfinished ([`recover`]). Storage that never maintains
    /// has nothing to finish.
    fn recover_maintenance(&mut self) -> Result<()> { Ok(()) }
}

/// R8r-2: one change to the employee's provider document. The bytes are
/// borrowed from the caller's zeroizing buffer (never copied here) and never
/// printed.
pub enum ProvidersChange<'a> { Write(&'a [u8]), Clear }
impl std::fmt::Debug for ProvidersChange<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self { Self::Write(_) => "Write(<redacted>)", Self::Clear => "Clear" })
    }
}
/// R8r-2 failures (fixed; the allocator maps them to schema-5 codes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvidersFailure {
    /// The employee holds a lease (a run is attached).
    Busy,
    /// `.duduclaw-api` (or a directory at `providers.json`) is not what root
    /// may write through; nothing was written.
    HomeEntryUnsafe,
    /// ENOSPC / EDQUOT in the image.
    NoSpace,
    /// Attach, detach, I/O, ledger or readback failure.
    Unavailable,
}
/// The fixed prefix of the temporary file a provider write creates in
/// `.duduclaw-api` (followed by 16 lowercase hex digits).
pub const PROVIDERS_TEMP_PREFIX: &str = ".providers.json.tmp-";
pub fn providers_temp_name(random: [u8; 8]) -> String {
    let mut name = String::from(PROVIDERS_TEMP_PREFIX);
    for byte in random { name.push_str(&format!("{byte:02x}")); }
    name
}
pub fn valid_providers_temp(name: &str) -> bool {
    name.strip_prefix(PROVIDERS_TEMP_PREFIX)
        .is_some_and(|hex| hex.len() == 16 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
}
/// R8r-2: the steps of one maintenance; `LoopStorage` supplies them. The
/// order (and when the marker is cleared) is the security property and lives
/// in [`maintain`] / [`recover`] so it is tested without root.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) trait MaintenanceSteps {
    /// The verified employee home directory of the attached image.
    type Home;
    /// Persist the maintenance marker (no secret: the temp name only).
    fn mark(&mut self, temp: &str) -> std::result::Result<(), ProvidersFailure>;
    /// Temporary noexec attach; the verified home descriptor.
    fn attach(&mut self) -> std::result::Result<Self::Home, ProvidersFailure>;
    /// Detach (idempotent when nothing is mounted).
    fn detach(&mut self) -> std::result::Result<(), ProvidersFailure>;
    /// Clear the marker.
    fn unmark(&mut self) -> std::result::Result<(), ProvidersFailure>;
}
/// Marker → attach → `work` in the verified home → detach (attempted after
/// every outcome, once the marker is durable) → marker cleared only after the
/// detach succeeded. A failed detach keeps the marker: [`recover`] (start-up,
/// or the employee's next maintenance) finishes it, and the adapter refuses
/// to attach the employee for a run meanwhile.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn maintain<S: MaintenanceSteps>(steps: &mut S, temp: &str,
    work: impl FnOnce(&S::Home) -> std::result::Result<(), ProvidersFailure>) -> std::result::Result<(), ProvidersFailure> {
    steps.mark(temp)?;
    let outcome = steps.attach().and_then(|home| work(&home));
    steps.detach()?;
    steps.unmark()?;
    outcome
}
/// A left-over maintenance: attach, `work` (remove the left-over temp file),
/// detach; the marker is cleared only when both succeeded.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn recover<S: MaintenanceSteps>(steps: &mut S,
    work: impl FnOnce(&S::Home) -> std::result::Result<(), ProvidersFailure>) -> std::result::Result<(), ProvidersFailure> {
    let outcome = steps.attach().and_then(|home| work(&home));
    steps.detach()?;
    outcome?;
    steps.unmark()
}

pub struct UnavailableStorage;
impl Storage for UnavailableStorage {
    fn readiness(&mut self) -> Result<()> { Err(ErrorCode::IsolationUnavailable) }
    fn provision(&mut self, _: &ProvisionRequest) -> Result<StorageUsage> { Err(ErrorCode::IsolationUnavailable) }
    fn attach(&mut self, _: EmployeeId, _: u32, _: RunHandle, _: &DiskQuota) -> Result<AttachedStorage> { Err(ErrorCode::IsolationUnavailable) }
    fn detach(&mut self, _: &Attachment) -> Result<()> { Err(ErrorCode::IsolationUnavailable) }
    fn destroy(&mut self, _: EmployeeId, _: RequestId, _: Sha256Digest) -> Result<()> { Err(ErrorCode::IsolationUnavailable) }
    fn usage(&mut self, _: EmployeeId) -> Result<StorageUsage> { Err(ErrorCode::IsolationUnavailable) }
    fn attachments(&mut self) -> Result<Vec<Attachment>> { Err(ErrorCode::IsolationUnavailable) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn storage_usage_requires_identity_hard_limits_and_live_evidence() {
        let employee = EmployeeId([1; 16]);
        let quota = DiskQuota::LoopFilesystem { bytes: 1024, inodes: 32 };
        let valid = StorageUsage { employee, bytes: 512, inodes: 3, limit_bytes: 1024, limit_inodes: 32,
            hard_quota_verified: true, device_graph_verified: true, host_reserve_verified: true };
        assert!(valid.check(employee, &quota).is_ok());
        assert!(valid.check(EmployeeId([2; 16]), &quota).is_err());
        let mut bad = valid.clone(); bad.host_reserve_verified = false; assert!(bad.check(employee, &quota).is_err());
        let mut bad = valid.clone(); bad.limit_bytes += 1; assert!(bad.check(employee, &quota).is_err());
        let mut bad = valid.clone(); bad.bytes = 1025; assert!(bad.check(employee, &quota).is_err());
        let mut bad = valid.clone(); bad.device_graph_verified = false; assert!(bad.check(employee, &quota).is_err());
        let mut bad = valid; bad.hard_quota_verified = false; assert!(bad.check(employee, &quota).is_err());
    }
    #[test]
    fn storage_adapter_unmounted_intent_is_cleanup_only() {
        let employee = EmployeeId([1; 16]);
        let run = RunHandle(duduclaw_isolation::ids::RunId([2; 16]));
        let intent = Attachment { employee, run, token: RequestId([3; 16]), device_major: 0, device_minor: 0 };
        assert!(intent.check_identity(employee, run).is_ok());
        assert!(intent.check(employee, run).is_err());
        assert!(intent.check_identity(EmployeeId([4; 16]), run).is_err());
        assert!(intent.check_identity(employee, RunHandle(duduclaw_isolation::ids::RunId([5; 16]))).is_err());
    }
    #[test]
    fn unavailable_storage_never_returns_empty_success() {
        let mut storage = UnavailableStorage;
        assert!(storage.readiness().is_err());
        assert!(storage.attachments().is_err());
        assert!(storage.usage(EmployeeId([1; 16])).is_err());
        assert_eq!(storage.write_providers(EmployeeId([1; 16]), 70_000, ProvidersChange::Clear), Err(ProvidersFailure::Unavailable));
    }

    /// R8r-2 fake maintenance steps: an event log, a durable marker and
    /// injected failures.
    #[derive(Default)]
    struct Steps { log: Vec<String>, marker: Option<String>, mounted: bool, fail: Option<&'static str> }
    impl Steps {
        fn step(&mut self, name: &'static str) -> std::result::Result<(), ProvidersFailure> {
            self.log.push(name.into());
            if self.fail == Some(name) { Err(ProvidersFailure::Unavailable) } else { Ok(()) }
        }
    }
    impl MaintenanceSteps for Steps {
        type Home = ();
        fn mark(&mut self, temp: &str) -> std::result::Result<(), ProvidersFailure> {
            self.step("mark")?; self.marker = Some(temp.into()); Ok(())
        }
        fn attach(&mut self) -> std::result::Result<(), ProvidersFailure> {
            assert!(self.marker.is_some(), "the marker is durable before any attach");
            self.step("attach")?; self.mounted = true; Ok(())
        }
        fn detach(&mut self) -> std::result::Result<(), ProvidersFailure> { self.step("detach")?; self.mounted = false; Ok(()) }
        fn unmark(&mut self) -> std::result::Result<(), ProvidersFailure> {
            assert!(!self.mounted, "the marker is cleared only after the detach");
            self.step("unmark")?; self.marker = None; Ok(())
        }
    }
    fn temp() -> String { providers_temp_name([0xab; 8]) }

    #[test]
    fn providers_maintenance_marks_before_attach_and_clears_after_detach() {
        let mut steps = Steps::default();
        maintain(&mut steps, &temp(), |_| { Ok(()) }).unwrap();
        assert_eq!(steps.log, ["mark", "attach", "detach", "unmark"]);
        assert_eq!((steps.marker, steps.mounted), (None, false));
        // A failed write still detaches, then clears the marker; the write's failure is the answer.
        let mut steps = Steps::default();
        assert_eq!(maintain(&mut steps, &temp(), |_| Err(ProvidersFailure::HomeEntryUnsafe)), Err(ProvidersFailure::HomeEntryUnsafe));
        assert_eq!(steps.log, ["mark", "attach", "detach", "unmark"]);
        // A failed attach is still followed by a detach.
        let mut steps = Steps { fail: Some("attach"), ..Steps::default() };
        assert!(maintain(&mut steps, &temp(), |_| panic!("no work without an attach")).is_err());
        assert_eq!(steps.log, ["mark", "attach", "detach", "unmark"]);
        // No marker, nothing else.
        let mut steps = Steps { fail: Some("mark"), ..Steps::default() };
        assert!(maintain(&mut steps, &temp(), |_| panic!("no work without a marker")).is_err());
        assert_eq!(steps.log, ["mark"]);
    }

    #[test]
    fn providers_maintenance_failed_detach_keeps_the_marker_for_reconciliation() {
        let mut steps = Steps { fail: Some("detach"), ..Steps::default() };
        assert_eq!(maintain(&mut steps, &temp(), |_| Ok(())), Err(ProvidersFailure::Unavailable));
        assert_eq!(steps.log, ["mark", "attach", "detach"]);
        assert_eq!(steps.marker, Some(temp()), "left for start-up reconciliation");
        // Reconciliation at start: attach, remove the left-over temp file, detach, clear.
        steps.fail = None; steps.log.clear();
        let left = steps.marker.clone().unwrap();
        let mut removed = None;
        recover(&mut steps, |_| { removed = Some(left.clone()); Ok(()) }).unwrap();
        assert_eq!(steps.log, ["attach", "detach", "unmark"]);
        assert_eq!((removed, steps.marker.clone(), steps.mounted), (Some(temp()), None, false));
        // A left-over that cannot be cleaned keeps its marker (the employee stays unusable).
        let mut steps = Steps { marker: Some(temp()), ..Steps::default() };
        assert!(recover(&mut steps, |_| Err(ProvidersFailure::Unavailable)).is_err());
        assert_eq!((steps.log.as_slice(), steps.marker.is_some()), (&["attach".to_string(), "detach".to_string()][..], true));
    }

    #[test]
    fn providers_temp_names_are_fixed_prefix_plus_16_hex() {
        assert_eq!(providers_temp_name([0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef]), ".providers.json.tmp-0123456789abcdef");
        assert!(valid_providers_temp(&temp()));
        for bad in ["providers.json", ".providers.json.tmp-", ".providers.json.tmp-0123456789ABCDEF", ".providers.json.tmp-0123456789abcde",
            ".providers.json.tmp-0123456789abcdef0", "../.providers.json.tmp-0123456789abcdef"] {
            assert!(!valid_providers_temp(bad), "{bad}");
        }
        assert_eq!(format!("{:?}", ProvidersChange::Write(b"sk-secret")), "Write(<redacted>)");
    }
}
