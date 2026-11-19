//! Permanent root-owned employee identities. Allocation precedes account creation.
use std::{collections::BTreeSet, io};
use serde::{Deserialize, Serialize};
use duduclaw_isolation::ids::{EmployeeId, UidPolicy, UidRange, EMPLOYEE_UID_MIN, EMPLOYEE_UID_MAX};
use crate::accounts::{at_step, for_account, Step};

fn invalid() -> io::Error { io::Error::new(io::ErrorKind::InvalidData, "employee registry unavailable") }

pub fn account_name(uid: u32) -> String { format!("duduclaw-emp-{uid}") }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountState { Reserved, Ready, Tombstone }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmployeeAccount {
    pub employee: EmployeeId,
    pub uid: u32,
    pub gid: u32,
    pub state: AccountState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    pub version: u16,
    pub range: UidRange,
    pub employees: Vec<EmployeeAccount>,
}

impl Default for Registry {
    fn default() -> Self {
        Self { version: 1, range: UidRange { first: EMPLOYEE_UID_MIN, last: EMPLOYEE_UID_MAX }, employees: Vec::new() }
    }
}

/// A fresh host snapshot, including complete NSS/userdb enumeration evidence.
pub struct HostUidPolicy {
    pub system: UidRange,
    pub login: UidRange,
    pub subuids: Vec<UidRange>,
    pub subgids: Vec<UidRange>,
    pub systemd: Vec<UidRange>,
}

impl HostUidPolicy {
    pub fn check(&self, range: UidRange, uid: u32) -> io::Result<()> {
        UidPolicy { employee_range: range, system_accounts: Some(self.system),
            login_accounts: Some(self.login), subuids: Some(&self.subuids),
            subgids: Some(&self.subgids), systemd_reserved: Some(&self.systemd) }
            .check(uid).map_err(|_| invalid())
    }
}

/// Implementations must fail closed on unreadable NSS, userdb, owner scans or ACLs.
/// Called while the root global allocation lock is held, never from employee data.
pub trait AccountHost {
    fn policy(&mut self) -> io::Result<HostUidPolicy>;
    fn verify_namespace(&mut self, registry: &Registry) -> io::Result<()>;
    fn uid_available(&mut self, uid: u32) -> io::Result<bool>;
    /// Every id in `range` that already owns (as uid or gid) a file the host
    /// can see in spawnd's state directories. Called once per reservation,
    /// before any candidate; an error refuses the reservation.
    fn occupied_ids(&mut self, range: UidRange) -> io::Result<BTreeSet<u32>>;
    fn create_locked(&mut self, account: &EmployeeAccount) -> io::Result<()>;
    fn verify_locked(&mut self, account: &EmployeeAccount) -> io::Result<()>;
    /// Quarantine an interrupted/retired identity, including a group-only residue.
    /// Must not create a missing user, delete the permanent mapping, or accept a
    /// same-uid alias. Success includes a complete no-process readback.
    fn recover_locked(&mut self, _account: &EmployeeAccount) -> io::Result<()> { Err(invalid()) }
    /// R7c (F6): the SSH denial probe result of a just-created (or resumed)
    /// account, WITHOUT waiting: `Ok` only when it is Denied; otherwise the
    /// probe is requested (forced, ahead of background requests) and an error
    /// carrying `crate::accounts::SshRefusal` is returned at once. An
    /// unresolved one (`probe_unresolved`: pending, inconclusive) keeps the
    /// reservation resumable instead of tombstoning it. Default: nothing to
    /// prove (hosts and fakes without a probe).
    fn login_denied(&mut self, _account: &EmployeeAccount) -> io::Result<()> { Ok(()) }
}

/// R7c (F6): at most this many reservations may wait for their SSH denial
/// probe at once ([`Allocations`]); a further NEW employee allocation is
/// refused with [`PendingLimit`] until one resolves, so an untrusted gateway
/// cannot pile up reserved uids or forced probe requests.
pub const MAX_PENDING_RESERVATIONS: usize = 4;
/// A reservation unresolved for longer than this (monotonic milliseconds
/// since it was reserved; resuming does not extend it) is retired by the
/// lifecycle thread's maintenance ([`Allocations::expire`]), so abandoned
/// reservations cannot hold the [`MAX_PENDING_RESERVATIONS`] cap.
pub const PENDING_RESERVATION_TTL_MS: u64 = 10 * 60 * 1000;
/// The refusal of a new employee allocation while [`MAX_PENDING_RESERVATIONS`]
/// reservations are pending (kind `WouldBlock`: retry later). Nothing was
/// reserved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingLimit;
impl std::fmt::Display for PendingLimit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "resource limit: {MAX_PENDING_RESERVATIONS} employee reservations already wait for their SSH denial probe")
    }
}
impl std::error::Error for PendingLimit {}
impl PendingLimit {
    pub fn error(self) -> io::Error { io::Error::new(io::ErrorKind::WouldBlock, self) }
}
/// `error` is the [`PendingLimit`] refusal.
pub fn pending_limit(error: &io::Error) -> bool {
    crate::accounts::unstepped(error).get_ref().is_some_and(|inner| inner.downcast_ref::<PendingLimit>().is_some())
}
/// R8w: the journal line of a failed allocation (`RegistryStore::allocate`),
/// `None` for the two "retry later" answers that are not failures: an
/// unresolved SSH probe and [`PendingLimit`].
pub fn allocation_log_line(error: &io::Error) -> Option<String> {
    if crate::accounts::probe_unresolved(error) || pending_limit(error) { return None; }
    Some(crate::accounts::allocation_line(error))
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecoveryReport { pub abandoned_reservations: usize, pub quarantined_identities: usize }

impl Registry {
    pub fn validate(&self, policy: &HostUidPolicy) -> io::Result<()> {
        if self.version != 1 || self.employees.len() > 30_000 { return Err(invalid()); }
        policy.check(self.range, self.range.first)?;
        let mut employees = BTreeSet::new();
        let mut uids = BTreeSet::new();
        for account in &self.employees {
            policy.check(self.range, account.uid)?;
            if account.uid != account.gid || !employees.insert(account.employee) || !uids.insert(account.uid) {
                return Err(invalid());
            }
        }
        Ok(())
    }

    pub fn lookup(&self, employee: EmployeeId) -> io::Result<&EmployeeAccount> {
        self.employees.iter().find(|account| account.employee == employee && account.state == AccountState::Ready)
            .ok_or_else(invalid)
    }

    pub fn verify(&self, host: &mut dyn AccountHost) -> io::Result<()> { self.verify_with(host, &BTreeSet::new()) }
    /// [`Self::verify`], except that a `Reserved` row of an employee in
    /// `resumable` ([`Allocations`]: an allocation of this process stopped on an
    /// unresolved SSH probe) is not a refusal; its local account is still
    /// checked by `verify_namespace` like every account in the range.
    /// R8w: every failure carries its step (`crate::accounts::step_of`):
    /// the host's own (`login_defs`, `nss_modules`, `passwd_row`, …), or
    /// `registry_rows` for the rows themselves.
    pub fn verify_with(&self, host: &mut dyn AccountHost, resumable: &BTreeSet<EmployeeId>) -> io::Result<()> {
        let policy = host.policy()?;
        self.validate(&policy).map_err(at_step(Step::RegistryRows))?;
        host.verify_namespace(self)?;
        for account in &self.employees {
            if account.state == AccountState::Reserved && !resumable.contains(&account.employee) {
                return Err(for_account(account.uid)(at_step(Step::RegistryRows)(invalid())));
            }
            if account.state == AccountState::Ready { host.verify_locked(account)?; }
        }
        Ok(())
    }

    /// Persistence must be durable before this returns a candidate to useradd.
    #[cfg(any(target_os = "linux", test))]
    fn reserve(&mut self, employee: EmployeeId, host: &mut dyn AccountHost) -> io::Result<usize> {
        let policy = host.policy()?;
        self.validate(&policy).map_err(at_step(Step::RegistryRows))?;
        host.verify_namespace(self)?;
        if self.employees.iter().any(|account| account.employee == employee) { return Err(at_step(Step::RegistryRows)(invalid())); }
        let used: BTreeSet<_> = self.employees.iter().map(|account| account.uid).collect();
        // R7b: one file-owner inventory per allocation. The candidate's gid is
        // its uid, so an id owning a file as either is skipped.
        let occupied = host.occupied_ids(self.range).map_err(at_step(Step::OwnerInventory))?;
        for uid in self.range.first..=self.range.last {
            if !used.contains(&uid) && !occupied.contains(&uid) && host.uid_available(uid).map_err(at_step(Step::Candidate))? {
                self.employees.push(EmployeeAccount { employee, uid, gid: uid, state: AccountState::Reserved });
                return Ok(self.employees.len() - 1);
            }
        }
        Err(io::Error::new(io::ErrorKind::StorageFull, "employee identity range exhausted"))
    }
}

/// R7c: the allocation state machine of one registry (portable; the root
/// `RegistryStore` persists around it). It runs on the lifecycle thread and
/// NEVER waits for the SSH denial probe (F6): every step is bounded by the
/// existing per-command limits.
///
/// - New employee: refused with [`PendingLimit`] (nothing reserved) while
///   [`MAX_PENDING_RESERVATIONS`] reservations are pending; otherwise
///   `reserve` (durable `Reserved`), `create_locked`, then `login_denied`
///   (queues the forced probe of the new account and returns at once), then
///   `verify_locked`. All pass ⇒ `Ready` (only on a host without a probe:
///   a just-created account has no result yet, so it is normally pending).
/// - The probe is unresolved (pending, inconclusive, PID 1 error) ⇒ the row
///   STAYS `Reserved`, is remembered here as resumable, nothing is
///   quarantined, and the error is returned at once ("probe pending"). The
///   uid is not burned.
/// - The next allocation of the SAME employee in this process resumes it:
///   `login_denied` first (the current result; re-queued if the worker
///   dropped the request). Still unresolved ⇒ pending again, the account
///   untouched. Denied ⇒ `recover_locked` (lockdown of the existing user and
///   no-process readback, as start-up recovery does) and `verify_locked` ⇒
///   `Ready`. A `Reserved` row this set does not know (a crash
///   mid-allocation) is still refused as before.
/// - Every other failure, including a probe that proves the account
///   reachable (sshd's own Accepted record), ⇒ `Tombstone` + best-effort
///   `recover_locked`, as before.
///
/// - Deadline ([`Self::expire`], lifecycle maintenance): a reservation still
///   unresolved [`PENDING_RESERVATION_TTL_MS`] after it was reserved is
///   retired as a failure is: `Tombstone` (saved durably first), then it is
///   forgotten here, then best-effort `recover_locked`. The employee keeps
///   its single row; from then on it is a tombstoned employee like any other
///   (its next allocation is refused as every Tombstone is), and its cap
///   slot is free for other employees. Before the deadline nothing changes.
///
/// The set is memory only: after a restart `RegistryStore::reconcile` turns
/// every `Reserved` row into a `Tombstone` exactly as before (unchanged rule),
/// so an unresolved reservation does not outlive the process that made it.
#[cfg(any(target_os = "linux", test))]
#[derive(Debug, Default)]
pub struct Allocations {
    unresolved: BTreeSet<EmployeeId>,
    /// When each `unresolved` reservation was made (monotonic ms; same keys).
    reserved_at: std::collections::BTreeMap<EmployeeId, u64>,
}
/// One reservation retired by the deadline, and its quarantine result.
#[cfg(any(target_os = "linux", test))]
#[derive(Debug)]
pub struct ExpiredReservation { pub account: EmployeeAccount, pub quarantine: io::Result<()> }
#[cfg(any(target_os = "linux", test))]
impl Allocations {
    /// Employees whose `Reserved` row an unresolved probe left resumable.
    pub fn resumable(&self) -> &BTreeSet<EmployeeId> { &self.unresolved }
    fn resolved(&mut self, employee: &EmployeeId) { self.unresolved.remove(employee); self.reserved_at.remove(employee); }
    /// R8w review round 1: the store-level entry of [`Self::allocate`]
    /// (`RegistryStore::allocate`): a poisoned store (`store_poisoned`) and an
    /// unreadable monotonic clock (`clock`) are refused here, labelled, so
    /// that every allocation failure passes through the store's single
    /// logging point with a step.
    pub fn allocate_stored(&mut self, registry: &mut Registry, employee: EmployeeId, host: &mut dyn AccountHost,
        poisoned: bool, clock: &mut dyn FnMut() -> io::Result<u64>,
        save: &mut dyn FnMut(&Registry) -> io::Result<()>) -> io::Result<EmployeeAccount> {
        if poisoned { return Err(at_step(Step::StorePoisoned)(invalid())); }
        let now_ms = clock().map_err(at_step(Step::Clock))?;
        self.allocate(registry, employee, host, now_ms, save)
    }
    /// Allocate (or resume) `employee` in `registry` at monotonic `now_ms`;
    /// `save` persists the registry durably (before useradd, and after the
    /// outcome is decided). R8w: every failure carries a step
    /// (`crate::accounts::step_of`), never `unlabelled`; the [`PendingLimit`]
    /// answer is not a failure (nothing reserved, not logged) and carries none.
    pub fn allocate(&mut self, registry: &mut Registry, employee: EmployeeId, host: &mut dyn AccountHost, now_ms: u64,
        save: &mut dyn FnMut(&Registry) -> io::Result<()>) -> io::Result<EmployeeAccount> {
        // `Some`: resume this process's unresolved reservation; `None`: a new
        // employee. Every other existing row (a Tombstone, including one the
        // deadline retired, or a Reserved row this process does not know) is
        // refused (`existing_row`).
        let resume = match registry.employees.iter().position(|account| account.employee == employee) {
            Some(index) => match registry.employees[index].state {
                AccountState::Ready => {
                    // The host's own step wins; `readback` only for an unlabelled one.
                    registry.verify_with(host, &self.unresolved).map_err(at_step(Step::Readback))?;
                    return Ok(registry.employees[index].clone());
                }
                AccountState::Reserved if self.unresolved.contains(&employee) => Some(index),
                _ => return Err(for_account(registry.employees[index].uid)(at_step(Step::ExistingRow)(invalid()))),
            },
            None => None,
        };
        let (index, resumed) = match resume {
            Some(index) => (index, true),
            None => {
                if self.unresolved.len() >= MAX_PENDING_RESERVATIONS { return Err(PendingLimit.error()); }
                let index = registry.reserve(employee, host).map_err(at_step(Step::Reserve))?;
                save(registry).map_err(at_step(Step::PersistReservation))
                    .map_err(for_account(registry.employees[index].uid))?;
                (index, false)
            }
        };
        let account = registry.employees[index].clone();
        // R8w: each allocation step labels a failure the host left
        // unlabelled (the host's own, finer step wins): `create_account`,
        // `ssh_probe`, `quarantine`, `readback`.
        let result = if resumed {
            // Pending again leaves the account untouched; only a proven
            // denial re-locks and reads it back before Ready.
            host.login_denied(&account).map_err(at_step(Step::SshProbe))
                .and_then(|()| host.recover_locked(&account).map_err(at_step(Step::Quarantine)))
                .and_then(|()| host.verify_locked(&account).map_err(at_step(Step::Readback)))
        } else {
            host.create_locked(&account).map_err(at_step(Step::CreateAccount))
                .and_then(|()| host.login_denied(&account).map_err(at_step(Step::SshProbe)))
                .and_then(|()| host.verify_locked(&account).map_err(at_step(Step::Readback)))
        };
        let result = result.map_err(for_account(account.uid));
        let unresolved = result.as_ref().err().is_some_and(crate::accounts::probe_unresolved);
        registry.employees[index].state = match (&result, unresolved) {
            (Ok(()), _) => AccountState::Ready,
            (Err(_), true) => AccountState::Reserved,
            (Err(_), false) => AccountState::Tombstone,
        };
        if unresolved {
            // The deadline counts from the reservation; a resume never extends it.
            self.unresolved.insert(employee);
            self.reserved_at.entry(employee).or_insert(now_ms);
        } else { self.resolved(&employee); }
        save(registry).map_err(at_step(Step::PersistFinal)).map_err(for_account(account.uid))?;
        if let Err(error) = result {
            // Best-effort immediate quarantine of a tombstone. A failure stays
            // durable and startup retries it before any protected execution.
            if !unresolved { let _ = host.recover_locked(&registry.employees[index]); }
            return Err(error);
        }
        Ok(registry.employees[index].clone())
    }
    /// The deadline, run by the lifecycle thread's maintenance at monotonic
    /// `now_ms` (never waits for a probe; at most [`MAX_PENDING_RESERVATIONS`]
    /// entries). Each reservation unresolved for longer than
    /// [`PENDING_RESERVATION_TTL_MS`] is retired exactly as a failed
    /// allocation is: its row becomes `Tombstone` and the registry is saved
    /// durably; only then is it forgotten here (so the saved registry never
    /// holds a `Reserved` row this set forgot), and then `recover_locked`
    /// quarantines it (best effort, as the failure branch; start-up retries a
    /// failure). A save failure restores the rows and remembers them still.
    /// The probe result is not consulted: completion to `Ready` stays the job
    /// of the same employee's next allocation, before the deadline. After
    /// it, the employee's single row is a Tombstone and its next allocation
    /// is refused like any tombstoned employee's.
    pub fn expire(&mut self, registry: &mut Registry, host: &mut dyn AccountHost, now_ms: u64,
        save: &mut dyn FnMut(&Registry) -> io::Result<()>) -> io::Result<Vec<ExpiredReservation>> {
        let due: Vec<EmployeeId> = self.reserved_at.iter()
            .filter(|(_, reserved)| now_ms.saturating_sub(**reserved) > PENDING_RESERVATION_TTL_MS)
            .map(|(employee, _)| *employee).take(MAX_PENDING_RESERVATIONS).collect();
        if due.is_empty() { return Ok(Vec::new()); }
        let mut retired = Vec::new();
        for employee in &due {
            if let Some(index) = registry.employees.iter().position(|account| account.employee == *employee) {
                if registry.employees[index].state == AccountState::Reserved {
                    registry.employees[index].state = AccountState::Tombstone;
                    retired.push(index);
                }
            }
        }
        if !retired.is_empty() {
            if let Err(error) = save(registry) {
                for index in &retired { registry.employees[*index].state = AccountState::Reserved; }
                return Err(error);
            }
        }
        // Durable: forget them (a due entry without a Reserved row has nothing to retire).
        for employee in &due { self.resolved(employee); }
        Ok(retired.into_iter().map(|index| {
            let account = registry.employees[index].clone();
            let quarantine = host.recover_locked(&account);
            ExpiredReservation { account, quarantine }
        }).collect())
    }
    /// The employee's row became a tombstone (explicit retirement/compensation).
    pub fn forget(&mut self, employee: EmployeeId) { self.resolved(&employee); }
}

#[cfg(target_os = "linux")]
mod disk {
    use super::*;
    use std::{fs::{self, File, OpenOptions}, io::{Read, Write}, os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt}, path::{Path, PathBuf}};
    use fs2::FileExt;

    pub const REGISTRY_DIRECTORY: &str = "/var/lib/duduclaw-spawnd";
    const MAX_REGISTRY_BYTES: u64 = 16 * 1024 * 1024;

    /// Checks every ancestor, so a root-owned file below a writable directory fails.
    pub fn root_path(path: &Path, directory: bool) -> io::Result<()> {
        if !path.is_absolute() || path.components().any(|part| !matches!(part,
            std::path::Component::RootDir | std::path::Component::Normal(_))) { return Err(invalid()); }
        for ancestor in path.ancestors() {
            let meta = fs::symlink_metadata(ancestor)?;
            if meta.uid() != 0 || meta.mode() & 0o022 != 0 || meta.file_type().is_symlink()
                || (ancestor != path && !meta.is_dir()) { return Err(invalid()); }
        }
        let meta = fs::symlink_metadata(path)?;
        if (directory && !meta.is_dir()) || (!directory && (!meta.is_file() || meta.nlink() != 1)) { return Err(invalid()); }
        Ok(())
    }

    /// An instance owns the global lock for its entire lifetime, including useradd.
    /// `allocations` (R7c) remembers which `Reserved` rows of this process
    /// an unresolved SSH probe left resumable.
    /// R8y review round 1 (F2): `lines` rate-limits the `allocation failed:
    /// step=<label>` journal line per label (at most `Step::ALL.len()` labels).
    pub struct RegistryStore { directory: PathBuf, _lock: File, data: Registry, poisoned: bool, allocations: Allocations,
        lines: crate::start_refusal::LineLimiter }

    impl RegistryStore {
        pub fn open() -> io::Result<Self> { Self::open_at(Path::new(REGISTRY_DIRECTORY)) }

        /// Installation/startup only. Never follows links or repairs a directory
        /// owned/writable by another account. Its trusted parent must exist.
        pub fn initialize() -> io::Result<Self> { Self::initialize_at(Path::new(REGISTRY_DIRECTORY)) }

        pub fn initialize_at(directory: &Path) -> io::Result<Self> {
            root_path(directory.parent().ok_or_else(invalid)?, true)?;
            match fs::DirBuilder::new().mode(0o700).create(directory) {
                Ok(()) => File::open(directory.parent().ok_or_else(invalid)?)?.sync_all()?,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {},
                Err(error) => return Err(error),
            }
            Self::open_at(directory)
        }

        pub fn open_at(directory: &Path) -> io::Result<Self> {
            root_path(directory, true)?;
            if fs::metadata(directory)?.mode() & 0o077 != 0 { return Err(invalid()); }
            let lock_path = directory.join("registry.lock");
            // Root-only directory plus create_new avoids following an existing link.
            let lock = match OpenOptions::new().read(true).write(true).create_new(true).mode(0o600).open(&lock_path) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    root_path(&lock_path, false)?;
                    if fs::metadata(&lock_path)?.mode() & 0o077 != 0 { return Err(invalid()); }
                    OpenOptions::new().read(true).write(true).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(&lock_path)?
                }
                Err(error) => return Err(error),
            };
            lock.try_lock_exclusive()?;
            let path = directory.join("registry.json");
            let data: Registry = match fs::symlink_metadata(&path) {
                Ok(_) => {
                    root_path(&path, false)?;
                    if fs::metadata(&path)?.mode() & 0o077 != 0 { return Err(invalid()); }
                    let mut bytes = Vec::new();
                    File::open(&path)?.take(MAX_REGISTRY_BYTES + 1).read_to_end(&mut bytes)?;
                    if bytes.len() as u64 > MAX_REGISTRY_BYTES { return Err(invalid()); }
                    serde_json::from_slice(&bytes).map_err(|_| invalid())?
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => Registry::default(),
                Err(error) => return Err(error),
            };
            // Recovery must retain the reservation until it can quarantine any
            // group/user created before the previous daemon stopped.
            let mut store = Self { directory: directory.to_path_buf(), _lock: lock, data, poisoned: false,
                allocations: Allocations::default(),
                lines: crate::start_refusal::LineLimiter::new("allocation failed", crate::accounts::Step::ALL.len()) };
            // Persist even an empty initial registry; never infer unused IDs later.
            store.save()?;
            Ok(store)
        }

        fn save(&mut self) -> io::Result<()> {
            if self.poisoned { return Err(invalid()); }
            let result = save_inner(&self.directory, &self.data);
            if result.is_err() { self.poisoned = true; }
            result
        }

        pub fn registry(&self) -> &Registry { &self.data }

        /// Call after orphaned run units have been stopped. Tombstones remain
        /// permanent even when quarantine fails; startup must keep admission off.
        pub fn reconcile(&mut self, host: &mut dyn AccountHost) -> io::Result<RecoveryReport> {
            if self.poisoned { return Err(invalid()); }
            self.data.validate(&host.policy()?)?;
            let mut report = RecoveryReport::default();
            for account in &mut self.data.employees {
                if account.state == AccountState::Reserved {
                    account.state = AccountState::Tombstone;
                    report.abandoned_reservations += 1;
                }
            }
            self.save()?;
            for account in &self.data.employees {
                if account.state == AccountState::Tombstone {
                    host.recover_locked(account)?;
                    report.quarantined_identities += 1;
                }
            }
            self.data.verify(host)?;
            Ok(report)
        }

        /// The R7c allocation state machine ([`Allocations::allocate`]), each
        /// decision persisted (a failed save poisons the store).
        /// R8w: a failed allocation logs one fixed line naming its step
        /// ([`allocation_log_line`]). An unresolved SSH probe and the
        /// pending-reservation cap are not logged: they are "retry later"
        /// answers with their own wire codes, repeated at the gateway's
        /// retry cadence. Every other failure, the poisoned store and the
        /// clock included ([`Allocations::allocate_stored`]), reaches this
        /// one logging point exactly once.
        pub fn allocate(&mut self, employee: EmployeeId, host: &mut dyn AccountHost) -> io::Result<EmployeeAccount> {
            let Self { directory, data, poisoned, allocations, lines, .. } = self;
            let was_poisoned = *poisoned;
            let result = allocations.allocate_stored(data, employee, host, was_poisoned, &mut || crate::host_state::boottime_ms(),
                &mut |registry: &Registry| {
                    if *poisoned { return Err(invalid()); }
                    let result = save_inner(directory.as_path(), registry);
                    if result.is_err() { *poisoned = true; }
                    result
                });
            if let Err(error) = &result {
                // Logged per failing request, not only when the step changes
                // (unlike the verification line): Allocate requests are
                // operator-initiated (a confirmed migration proposal), so
                // their volume is the operator's, not a refresh cadence.
                // R8y review round 1 (F2): but the gateway account can repeat
                // them, so the line is rate limited per label (first 5 per
                // 10 s, then a `suppressed=<count>` line; `flush_lines`).
                if let Some(line) = allocation_log_line(error) {
                    for line in lines.note(line, 1, std::time::Instant::now()) { eprintln!("duduclaw-spawnd: {line}"); }
                }
            }
            result
        }

        /// R8y review round 1 (F2): the allocation-failure summaries of an
        /// ended window (`Core::refresh` calls this on its cadence).
        pub fn flush_lines(&mut self, now: std::time::Instant) -> Vec<String> { self.lines.flush(now) }
        /// R8y review round 2: the pending allocation-failure summaries at
        /// shutdown, whatever the window's age (`Core::drop`; idempotent).
        pub fn drain_lines(&mut self) -> Vec<String> { self.lines.drain() }

        /// The reservation deadline ([`Allocations::expire`]) at the current
        /// boottime, for the lifecycle thread's maintenance.
        pub fn expire_pending(&mut self, host: &mut dyn AccountHost) -> io::Result<Vec<ExpiredReservation>> {
            if self.poisoned { return Err(invalid()); }
            if self.allocations.resumable().is_empty() { return Ok(Vec::new()); }
            let now_ms = crate::host_state::boottime_ms()?;
            let Self { directory, data, poisoned, allocations, .. } = self;
            allocations.expire(data, host, now_ms, &mut |registry: &Registry| {
                if *poisoned { return Err(invalid()); }
                let result = save_inner(directory.as_path(), registry);
                if result.is_err() { *poisoned = true; }
                result
            })
        }

        /// [`Registry::verify_with`] for this store: a reservation an
        /// unresolved SSH probe of this process left resumable is not a
        /// host-wide refusal (readiness refresh).
        pub fn verify(&self, host: &mut dyn AccountHost) -> io::Result<()> {
            self.data.verify_with(host, self.allocations.resumable())
        }

        /// Explicit retirement never removes the mapping and never recycles its uid.
        pub fn retire(&mut self, employee: EmployeeId) -> io::Result<()> {
            if self.poisoned { return Err(invalid()); }
            let account = self.data.employees.iter_mut().find(|account| account.employee == employee).ok_or_else(invalid)?;
            account.state = AccountState::Tombstone;
            self.allocations.forget(employee);
            self.save()
        }
    }

    fn save_inner(directory: &Path, data: &Registry) -> io::Result<()> {
        let path = directory.join("registry.json");
        let temporary = directory.join("registry.next");
        match fs::symlink_metadata(&temporary) {
            Ok(_) => { root_path(&temporary, false)?; fs::remove_file(&temporary)?; }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {},
            Err(error) => return Err(error),
        }
        let bytes = serde_json::to_vec(data).map_err(|_| invalid())?;
        if bytes.len() as u64 > MAX_REGISTRY_BYTES { return Err(invalid()); }
        let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        File::open(directory)?.sync_all()
    }
}
#[cfg(target_os = "linux")]
pub use disk::{root_path, RegistryStore, REGISTRY_DIRECTORY};

#[cfg(test)]
mod tests {
    use super::*;
    struct Host;
    impl AccountHost for Host {
        fn policy(&mut self) -> io::Result<HostUidPolicy> { Ok(HostUidPolicy {
            system: UidRange { first: 100, last: 999 }, login: UidRange { first: 1000, last: 60000 },
            subuids: vec![], subgids: vec![], systemd: vec![],
        }) }
        fn verify_namespace(&mut self, _: &Registry) -> io::Result<()> { Ok(()) }
        fn uid_available(&mut self, _: u32) -> io::Result<bool> { Ok(true) }
        fn occupied_ids(&mut self, _: UidRange) -> io::Result<BTreeSet<u32>> { Ok(BTreeSet::new()) }
        fn create_locked(&mut self, _: &EmployeeAccount) -> io::Result<()> { Ok(()) }
        fn verify_locked(&mut self, _: &EmployeeAccount) -> io::Result<()> { Ok(()) }
        fn recover_locked(&mut self, _: &EmployeeAccount) -> io::Result<()> { Ok(()) }
    }
    /// R7b: a host whose file-owner inventory is scripted; counts inventories
    /// and every candidate the getent check saw.
    struct Inventory { occupied: io::Result<BTreeSet<u32>>, scans: usize, candidates: Vec<u32> }
    impl AccountHost for Inventory {
        fn policy(&mut self) -> io::Result<HostUidPolicy> { Host.policy() }
        fn verify_namespace(&mut self, _: &Registry) -> io::Result<()> { Ok(()) }
        fn uid_available(&mut self, uid: u32) -> io::Result<bool> { self.candidates.push(uid); Ok(true) }
        fn occupied_ids(&mut self, range: UidRange) -> io::Result<BTreeSet<u32>> {
            assert_eq!(range, Registry::default().range);
            self.scans += 1;
            match &self.occupied { Ok(ids) => Ok(ids.clone()), Err(_) => Err(io::Error::other("inventory bound exceeded")) }
        }
        fn create_locked(&mut self, _: &EmployeeAccount) -> io::Result<()> { Ok(()) }
        fn verify_locked(&mut self, _: &EmployeeAccount) -> io::Result<()> { Ok(()) }
    }
    #[test]
    fn reserve_skips_file_owning_ids_with_one_inventory_per_allocation() {
        let mut registry = Registry::default();
        let mut host = Inventory { occupied: Ok([70000, 70001, 70003].into()), scans: 0, candidates: vec![] };
        let index = registry.reserve(EmployeeId([1; 16]), &mut host).unwrap();
        assert_eq!((registry.employees[index].uid, registry.employees[index].gid), (70002, 70002));
        assert_eq!(host.scans, 1);
        // Occupied ids never reach the per-candidate check.
        assert_eq!(host.candidates, [70002]);
        let index = registry.reserve(EmployeeId([2; 16]), &mut host).unwrap();
        assert_eq!(registry.employees[index].uid, 70004);
        assert_eq!((host.scans, host.candidates.as_slice()), (2, &[70002, 70004][..]));
    }
    #[test]
    fn reserve_refuses_when_the_inventory_fails_and_records_nothing() {
        let mut registry = Registry::default();
        registry.reserve(EmployeeId([1; 16]), &mut Host).unwrap();
        registry.employees[0].state = AccountState::Ready;
        let before = registry.employees.clone();
        let mut host = Inventory { occupied: Err(io::ErrorKind::Other.into()), scans: 0, candidates: vec![] };
        assert!(registry.reserve(EmployeeId([2; 16]), &mut host).is_err());
        assert_eq!(host.scans, 1);
        assert!(host.candidates.is_empty());
        // No reservation was made, so nothing can be tombstoned by `allocate`,
        // and the existing account is untouched.
        assert_eq!(registry.employees, before);
        assert!(registry.employees.iter().all(|account| account.state != AccountState::Tombstone));
    }
    #[test]
    fn tombstones_and_reserved_ids_are_never_recycled() {
        let mut registry = Registry::default();
        let first = registry.reserve(EmployeeId([1; 16]), &mut Host).unwrap();
        registry.employees[first].state = AccountState::Tombstone;
        let second = registry.reserve(EmployeeId([2; 16]), &mut Host).unwrap();
        assert_eq!(registry.employees[first].uid, 70000);
        assert_eq!(registry.employees[second].uid, 70001);
        assert!(registry.reserve(EmployeeId([1; 16]), &mut Host).is_err());
        assert!(registry.lookup(EmployeeId([1; 16])).is_err());
        assert!(registry.verify(&mut Host).is_err());
    }
    #[test]
    fn duplicate_identity_and_uid_are_refused() {
        let mut registry = Registry::default();
        registry.reserve(EmployeeId([1; 16]), &mut Host).unwrap();
        registry.employees.push(registry.employees[0].clone());
        assert!(registry.validate(&Host.policy().unwrap()).is_err());
        // One row per employee: a second row with its own uid is refused,
        // even when the first one is a Tombstone.
        registry.employees.truncate(1);
        registry.employees[0].state = AccountState::Tombstone;
        registry.employees.push(EmployeeAccount { employee: EmployeeId([1; 16]), uid: 70001, gid: 70001, state: AccountState::Reserved });
        assert!(registry.validate(&Host.policy().unwrap()).is_err());
        // One row per uid.
        registry.employees[1].employee = EmployeeId([2; 16]);
        registry.validate(&Host.policy().unwrap()).unwrap();
        registry.employees[1].uid = 70000; registry.employees[1].gid = 70000;
        assert!(registry.validate(&Host.policy().unwrap()).is_err());
    }
    #[test]
    fn uid_range_overlap_refuses_whole_range() {
        let mut policy = Host.policy().unwrap();
        policy.subgids.push(UidRange { first: 99999, last: 150000 });
        assert!(policy.check(Registry::default().range, 70000).is_err());
        assert_eq!(account_name(70000), "duduclaw-emp-70000");
    }

    /// R7c: a host whose SSH probe outcomes are scripted (one per
    /// `login_denied`), with injected create/verify failures.
    #[derive(Default)]
    struct Probing { probes: Vec<io::Result<()>>, create_fail: bool, verify_fail: bool,
        creates: usize, recoveries: usize, waits: usize, recovered: Vec<u32>,
        /// R8w: the step a failing readback names itself (as `LinuxAccounts` does).
        verify_step: Option<Step>,
        /// R8w round 1: unlabelled failures of the other host calls.
        policy_fail: bool, namespace_fail: bool, candidate_fail: bool, inventory_fail: bool, recover_fail: bool }
    impl AccountHost for Probing {
        fn policy(&mut self) -> io::Result<HostUidPolicy> {
            if self.policy_fail { return Err(io::Error::other("injected policy failure")); }
            Host.policy()
        }
        fn verify_namespace(&mut self, _: &Registry) -> io::Result<()> {
            if self.namespace_fail { Err(io::Error::other("injected namespace failure")) } else { Ok(()) }
        }
        fn uid_available(&mut self, _: u32) -> io::Result<bool> {
            if self.candidate_fail { Err(io::Error::other("injected getent failure")) } else { Ok(true) }
        }
        fn occupied_ids(&mut self, _: UidRange) -> io::Result<BTreeSet<u32>> {
            if self.inventory_fail { Err(io::Error::other("injected inventory failure")) } else { Ok(BTreeSet::new()) }
        }
        fn create_locked(&mut self, _: &EmployeeAccount) -> io::Result<()> {
            self.creates += 1;
            if self.create_fail { Err(io::Error::other("injected useradd failure")) } else { Ok(()) }
        }
        fn verify_locked(&mut self, _: &EmployeeAccount) -> io::Result<()> {
            if !self.verify_fail { return Ok(()); }
            let error = io::Error::other("injected lockdown readback failure");
            Err(match self.verify_step { Some(step) => at_step(step)(error), None => error })
        }
        fn recover_locked(&mut self, account: &EmployeeAccount) -> io::Result<()> {
            self.recoveries += 1; self.recovered.push(account.uid);
            if self.recover_fail { Err(io::Error::other("injected quarantine failure")) } else { Ok(()) }
        }
        fn login_denied(&mut self, _: &EmployeeAccount) -> io::Result<()> {
            self.waits += 1;
            if self.probes.is_empty() { panic!("unexpected probe request") }
            self.probes.remove(0)
        }
    }
    use crate::accounts::SshRefusal;
    fn inconclusive() -> io::Result<()> { Err(SshRefusal::Inconclusive("identity (exit 3)".into()).error()) }
    fn reachable() -> io::Result<()> { Err(SshRefusal::Reachable("login_not_denied (exit 10)".into()).error()) }
    fn pending() -> io::Result<()> { Err(SshRefusal::Pending.error()) }
    /// Runs `Allocations::allocate` (at monotonic time 0), counting the durable saves.
    fn allocate(state: &mut Allocations, registry: &mut Registry, employee: u8, host: &mut Probing, saves: &mut usize)
        -> io::Result<EmployeeAccount> {
        allocate_at(state, registry, employee, host, saves, 0)
    }
    fn allocate_at(state: &mut Allocations, registry: &mut Registry, employee: u8, host: &mut Probing, saves: &mut usize, now: u64)
        -> io::Result<EmployeeAccount> {
        state.allocate(registry, EmployeeId([employee; 16]), host, now, &mut |_: &Registry| { *saves += 1; Ok(()) })
    }
    /// Runs the deadline (`Allocations::expire`) at `now`, counting the durable saves.
    fn expire_at(state: &mut Allocations, registry: &mut Registry, host: &mut Probing, saves: &mut usize, now: u64)
        -> io::Result<Vec<ExpiredReservation>> {
        state.expire(registry, host, now, &mut |_: &Registry| { *saves += 1; Ok(()) })
    }
    fn states(registry: &Registry) -> Vec<AccountState> { registry.employees.iter().map(|account| account.state).collect() }

    #[test]
    fn allocation_denied_probe_is_ready() {
        let (mut state, mut registry, mut saves) = (Allocations::default(), Registry::default(), 0);
        let mut host = Probing { probes: vec![Ok(())], ..Probing::default() };
        let account = allocate(&mut state, &mut registry, 1, &mut host, &mut saves).unwrap();
        assert_eq!((account.uid, account.state), (70000, AccountState::Ready));
        assert_eq!((host.creates, host.waits, host.recoveries, saves), (1, 1, 0, 2));
        assert!(state.resumable().is_empty());
        // A replay of a Ready employee neither creates nor probes again.
        assert_eq!(allocate(&mut state, &mut registry, 1, &mut host, &mut saves).unwrap(), account);
        assert_eq!((host.creates, host.waits), (1, 1));
    }

    #[test]
    fn allocation_reachable_probe_tombstones_and_burns_the_uid() {
        let (mut state, mut registry, mut saves) = (Allocations::default(), Registry::default(), 0);
        let mut host = Probing { probes: vec![reachable(), Ok(())], ..Probing::default() };
        let error = allocate(&mut state, &mut registry, 1, &mut host, &mut saves).unwrap_err();
        assert!(matches!(crate::accounts::ssh_refusal(&error), Some(SshRefusal::Reachable(_))), "{error}");
        assert_eq!(registry.employees[0].state, AccountState::Tombstone);
        assert_eq!(host.recoveries, 1, "a tombstone is quarantined at once");
        assert!(state.resumable().is_empty());
        // Never resumed, never recycled.
        assert!(allocate(&mut state, &mut registry, 1, &mut host, &mut saves).is_err());
        assert_eq!((host.creates, host.waits), (1, 1));
        assert_eq!(allocate(&mut state, &mut registry, 2, &mut host, &mut saves).unwrap().uid, 70001);
    }

    #[test]
    fn allocation_unresolved_probe_keeps_the_reservation_and_the_same_employee_resumes() {
        for unresolved in [inconclusive as fn() -> io::Result<()>, pending,
            || Err(SshRefusal::Inconclusive("login_unverified (exit 18)".into()).error())] {
            let (mut state, mut registry, mut saves) = (Allocations::default(), Registry::default(), 0);
            let mut host = Probing { probes: vec![unresolved(), unresolved(), Ok(())], ..Probing::default() };
            let error = allocate(&mut state, &mut registry, 1, &mut host, &mut saves).unwrap_err();
            assert!(crate::accounts::probe_unresolved(&error), "{error}");
            assert_eq!(registry.employees[0].state, AccountState::Reserved, "never tombstoned");
            assert_eq!(host.recoveries, 0, "nothing quarantined");
            assert!(state.resumable().contains(&EmployeeId([1; 16])));
            assert_eq!(saves, 2, "the Reserved outcome is durable");
            // Host-wide: the resumable reservation does not fail verification;
            // any other Reserved row (or the plain rule) still does.
            registry.verify_with(&mut Probing::default(), state.resumable()).unwrap();
            assert!(registry.verify(&mut Probing::default()).is_err());
            // Another employee is allocated meanwhile, with the next uid.
            let mut other = Probing { probes: vec![Ok(())], ..Probing::default() };
            assert_eq!(allocate(&mut state, &mut registry, 2, &mut other, &mut saves).unwrap().uid, 70001);
            // A retry for the same employee asks the probe again; still
            // unresolved ⇒ pending again, the account untouched, no new account.
            assert!(crate::accounts::probe_unresolved(&allocate(&mut state, &mut registry, 1, &mut host, &mut saves).unwrap_err()));
            assert_eq!((host.creates, host.recoveries, host.waits), (1, 0, 2));
            assert_eq!(registry.employees[0].state, AccountState::Reserved);
            // Denied ⇒ re-lock (recover_locked), readback, Ready.
            let account = allocate(&mut state, &mut registry, 1, &mut host, &mut saves).unwrap();
            assert_eq!((account.uid, account.state), (70000, AccountState::Ready), "same uid, now Ready");
            assert_eq!((host.creates, host.recoveries, host.waits), (1, 1, 3));
            assert!(state.resumable().is_empty());
            registry.verify(&mut Probing::default()).unwrap();
        }
        // An unresolved reservation whose resumed probe proves the account reachable is tombstoned.
        let (mut state, mut registry, mut saves) = (Allocations::default(), Registry::default(), 0);
        let mut host = Probing { probes: vec![inconclusive(), reachable()], ..Probing::default() };
        assert!(allocate(&mut state, &mut registry, 1, &mut host, &mut saves).is_err());
        assert!(allocate(&mut state, &mut registry, 1, &mut host, &mut saves).is_err());
        assert_eq!(registry.employees[0].state, AccountState::Tombstone);
        assert!(state.resumable().is_empty());
        assert!(allocate(&mut state, &mut registry, 1, &mut host, &mut saves).is_err());
        // Retirement (compensation) forgets it.
        let (mut state, mut registry, mut saves) = (Allocations::default(), Registry::default(), 0);
        let mut host = Probing { probes: vec![pending()], ..Probing::default() };
        assert!(allocate(&mut state, &mut registry, 1, &mut host, &mut saves).is_err());
        state.forget(EmployeeId([1; 16]));
        assert!(allocate(&mut state, &mut registry, 1, &mut host, &mut saves).is_err(), "no longer resumable");
        assert_eq!(host.waits, 1);
        // A resumed Denied whose re-lock readback fails is tombstoned.
        let (mut state, mut registry, mut saves) = (Allocations::default(), Registry::default(), 0);
        let mut host = Probing { probes: vec![pending(), Ok(())], ..Probing::default() };
        assert!(allocate(&mut state, &mut registry, 1, &mut host, &mut saves).is_err());
        host.verify_fail = true;
        assert!(allocate(&mut state, &mut registry, 1, &mut host, &mut saves).is_err());
        assert_eq!(registry.employees[0].state, AccountState::Tombstone);
        assert!(state.resumable().is_empty());
    }

    /// F6: at most MAX_PENDING_RESERVATIONS reservations wait for their
    /// probe; a further NEW employee is refused (resource limit, nothing
    /// reserved, retry later); resuming a pending one is still allowed, and
    /// once one resolves a new employee is admitted again.
    #[test]
    fn allocation_pending_reservations_are_capped() {
        assert_eq!(MAX_PENDING_RESERVATIONS, 4);
        let (mut state, mut registry, mut saves) = (Allocations::default(), Registry::default(), 0);
        let mut host = Probing { probes: (0..MAX_PENDING_RESERVATIONS).map(|_| pending()).collect(), ..Probing::default() };
        for employee in 1..=MAX_PENDING_RESERVATIONS as u8 {
            assert!(crate::accounts::probe_unresolved(&allocate(&mut state, &mut registry, employee, &mut host, &mut saves).unwrap_err()));
        }
        assert_eq!(state.resumable().len(), MAX_PENDING_RESERVATIONS);
        let rows = registry.employees.clone();
        let (creates, recorded) = (host.creates, saves);
        let error = allocate(&mut state, &mut registry, 9, &mut host, &mut saves).unwrap_err();
        assert!(pending_limit(&error), "{error}");
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert!(error.to_string().starts_with("resource limit: "), "{error}");
        assert!(!crate::accounts::probe_unresolved(&error), "not a probe refusal");
        assert_eq!(registry.employees, rows, "nothing reserved");
        assert_eq!((host.creates, saves), (creates, recorded), "no useradd, no save");
        // Resuming a pending reservation is not a new one.
        host.probes = vec![pending(), Ok(())];
        assert!(crate::accounts::probe_unresolved(&allocate(&mut state, &mut registry, 1, &mut host, &mut saves).unwrap_err()));
        assert_eq!(allocate(&mut state, &mut registry, 1, &mut host, &mut saves).unwrap().state, AccountState::Ready);
        // One resolved: a new employee is admitted (and pends in turn).
        host.probes = vec![pending()];
        assert!(crate::accounts::probe_unresolved(&allocate(&mut state, &mut registry, 9, &mut host, &mut saves).unwrap_err()));
        assert_eq!(state.resumable().len(), MAX_PENDING_RESERVATIONS);
        assert!(pending_limit(&allocate(&mut state, &mut registry, 10, &mut host, &mut saves).unwrap_err()));
        // Retirement frees a slot too.
        state.forget(EmployeeId([2; 16]));
        host.probes = vec![pending()];
        assert!(crate::accounts::probe_unresolved(&allocate(&mut state, &mut registry, 10, &mut host, &mut saves).unwrap_err()));
        assert!(!pending_limit(&io::Error::other("x")) && !pending_limit(&SshRefusal::Pending.error()));
    }

    /// The deadline: four reservations a gateway abandoned stop holding the
    /// cap after PENDING_RESERVATION_TTL_MS; each is retired as a failure is
    /// (Tombstone, one durable save, quarantine), never probed; a fifth
    /// employee is admitted, and the freed slots serve other employees.
    #[test]
    fn allocation_abandoned_pending_reservations_expire_after_the_ttl() {
        const TTL: u64 = PENDING_RESERVATION_TTL_MS;
        assert_eq!(TTL, 10 * 60 * 1000);
        let (mut state, mut registry, mut saves) = (Allocations::default(), Registry::default(), 0);
        let mut host = Probing { probes: (0..MAX_PENDING_RESERVATIONS).map(|_| pending()).collect(), ..Probing::default() };
        for employee in 1..=MAX_PENDING_RESERVATIONS as u8 {
            let error = allocate_at(&mut state, &mut registry, employee, &mut host, &mut saves, u64::from(employee)).unwrap_err();
            assert!(crate::accounts::probe_unresolved(&error), "{error}");
        }
        assert!(pending_limit(&allocate_at(&mut state, &mut registry, 9, &mut host, &mut saves, 100).unwrap_err()));
        // Exactly TTL after the oldest: nothing is due, nothing saved.
        let recorded = saves;
        assert!(expire_at(&mut state, &mut registry, &mut host, &mut saves, 1 + TTL).unwrap().is_empty());
        assert_eq!((saves, state.resumable().len()), (recorded, MAX_PENDING_RESERVATIONS));
        assert!(registry.employees.iter().all(|account| account.state == AccountState::Reserved));
        // Past the deadline of all four.
        let expired = expire_at(&mut state, &mut registry, &mut host, &mut saves, MAX_PENDING_RESERVATIONS as u64 + TTL + 1).unwrap();
        assert_eq!(expired.len(), MAX_PENDING_RESERVATIONS);
        assert!(expired.iter().all(|retired| retired.account.state == AccountState::Tombstone && retired.quarantine.is_ok()));
        assert_eq!(saves, recorded + 1, "one durable save");
        assert!(registry.employees.iter().all(|account| account.state == AccountState::Tombstone));
        assert_eq!(host.recovered, [70000, 70001, 70002, 70003], "every expired row went through the quarantine path");
        assert_eq!(host.waits, MAX_PENDING_RESERVATIONS, "the deadline never asks the probe");
        assert!(state.resumable().is_empty());
        assert!(expire_at(&mut state, &mut registry, &mut host, &mut saves, 3 * TTL).unwrap().is_empty(), "retired once");
        // A fifth employee is admitted (and pends in turn), with the next uid.
        let now = 2 * TTL;
        host.probes = vec![pending()];
        assert!(crate::accounts::probe_unresolved(&allocate_at(&mut state, &mut registry, 9, &mut host, &mut saves, now).unwrap_err()));
        assert_eq!(registry.employees[4].uid, 70004);
        // Every slot the four held is free for other employees: three more
        // pend, then the cap applies again.
        host.probes = (0..3).map(|_| pending()).collect();
        for employee in 10..13 {
            assert!(crate::accounts::probe_unresolved(&allocate_at(&mut state, &mut registry, employee, &mut host, &mut saves, now).unwrap_err()));
        }
        assert!(pending_limit(&allocate_at(&mut state, &mut registry, 13, &mut host, &mut saves, now).unwrap_err()));
        // Still one row per employee; the expired rows stay burned Tombstones.
        registry.validate(&Host.policy().unwrap()).unwrap();
        assert_eq!(registry.employees[0], EmployeeAccount { employee: EmployeeId([1; 16]), uid: 70000, gid: 70000, state: AccountState::Tombstone });
    }

    /// After the deadline the expired employee is a tombstoned employee like
    /// any other: its next allocation gets the existing Tombstone refusal
    /// (not the cap, not a probe refusal; no reservation, no useradd, no
    /// probe, no save), and the registry keeps exactly one row for it.
    #[test]
    fn allocation_after_expiry_refuses_the_same_employee_as_any_tombstone() {
        const TTL: u64 = PENDING_RESERVATION_TTL_MS;
        let (mut state, mut registry, mut saves) = (Allocations::default(), Registry::default(), 0);
        let mut host = Probing { probes: vec![pending()], ..Probing::default() };
        assert!(crate::accounts::probe_unresolved(&allocate_at(&mut state, &mut registry, 1, &mut host, &mut saves, 0).unwrap_err()));
        // The reference: an employee tombstoned by a failed allocation, and its refusal.
        host.create_fail = true;
        assert!(allocate_at(&mut state, &mut registry, 2, &mut host, &mut saves, 0).is_err());
        host.create_fail = false;
        assert_eq!(registry.employees[1].state, AccountState::Tombstone);
        let tombstoned = allocate_at(&mut state, &mut registry, 2, &mut host, &mut saves, 0).unwrap_err();
        // The deadline retires employee 1.
        let expired = expire_at(&mut state, &mut registry, &mut host, &mut saves, TTL + 1).unwrap();
        assert_eq!(expired.iter().map(|retired| retired.account.uid).collect::<Vec<_>>(), [70000]);
        assert!(state.resumable().is_empty(), "no slot is held");
        let (rows, creates, waits, recorded) = (registry.employees.clone(), host.creates, host.waits, saves);
        for now in [TTL + 2, 3 * TTL] {
            let error = allocate_at(&mut state, &mut registry, 1, &mut host, &mut saves, now).unwrap_err();
            assert_eq!((error.kind(), error.to_string()), (tombstoned.kind(), tombstoned.to_string()), "the Tombstone refusal");
            assert!(!pending_limit(&error) && !crate::accounts::probe_unresolved(&error), "{error}");
        }
        assert_eq!(registry.employees, rows, "nothing reserved");
        assert_eq!((host.creates, host.waits, saves), (creates, waits, recorded), "no useradd, no probe, no save");
        assert_eq!(registry.employees.iter().filter(|account| account.employee == EmployeeId([1; 16])).count(), 1);
        assert_eq!(registry.employees[0], EmployeeAccount { employee: EmployeeId([1; 16]), uid: 70000, gid: 70000, state: AccountState::Tombstone });
        assert!(registry.lookup(EmployeeId([1; 16])).is_err());
        registry.validate(&Host.policy().unwrap()).unwrap();
        registry.verify(&mut Probing::default()).unwrap();
        // Another employee is still admitted, with the next uid.
        host.probes = vec![Ok(())];
        assert_eq!(allocate_at(&mut state, &mut registry, 3, &mut host, &mut saves, 3 * TTL).unwrap().uid, 70002);
    }

    /// The deadline counts from the reservation (a pending resume does not
    /// extend it), touches only reservations that are due, never a Ready one,
    /// and an allocation before the deadline behaves as without it.
    #[test]
    fn allocation_expiry_spares_resumed_and_younger_reservations() {
        const MINUTE: u64 = 60_000;
        let (mut state, mut registry, mut saves) = (Allocations::default(), Registry::default(), 0);
        let mut host = Probing { probes: vec![pending(), pending(), pending()], ..Probing::default() };
        for (employee, now) in [(1, 0), (3, MINUTE), (2, 5 * MINUTE)] {
            assert!(crate::accounts::probe_unresolved(&allocate_at(&mut state, &mut registry, employee, &mut host, &mut saves, now).unwrap_err()));
        }
        // Before the deadline: 1 resumes Denied ⇒ Ready; 3 resumes still pending.
        host.probes = vec![Ok(()), pending()];
        assert_eq!(allocate_at(&mut state, &mut registry, 1, &mut host, &mut saves, 9 * MINUTE).unwrap().state, AccountState::Ready);
        assert!(crate::accounts::probe_unresolved(&allocate_at(&mut state, &mut registry, 3, &mut host, &mut saves, 9 * MINUTE).unwrap_err()));
        assert!(host.probes.is_empty());
        let recovered = host.recovered.len();
        // 11 min + 1 ms: only 3 (reserved at 1 min) is due.
        let expired = expire_at(&mut state, &mut registry, &mut host, &mut saves, 11 * MINUTE + 1).unwrap();
        assert_eq!(expired.iter().map(|retired| retired.account.uid).collect::<Vec<_>>(), [70001]);
        assert_eq!(states(&registry), [AccountState::Ready, AccountState::Tombstone, AccountState::Reserved]);
        assert_eq!(host.recovered[recovered..], [70001]);
        assert_eq!(state.resumable(), &BTreeSet::from([EmployeeId([2; 16])]));
        // 2 resumes just before its deadline exactly as today (still pending), which stays 15 min.
        host.probes = vec![pending()];
        assert!(crate::accounts::probe_unresolved(&allocate_at(&mut state, &mut registry, 2, &mut host, &mut saves, 15 * MINUTE - 1).unwrap_err()));
        assert!(expire_at(&mut state, &mut registry, &mut host, &mut saves, 15 * MINUTE).unwrap().is_empty());
        let expired = expire_at(&mut state, &mut registry, &mut host, &mut saves, 15 * MINUTE + 1).unwrap();
        assert_eq!(expired.iter().map(|retired| retired.account.uid).collect::<Vec<_>>(), [70002]);
        assert_eq!(states(&registry), [AccountState::Ready, AccountState::Tombstone, AccountState::Tombstone]);
        assert_eq!(registry.lookup(EmployeeId([1; 16])).unwrap().uid, 70000, "the Ready account is untouched");
        assert_eq!(host.waits, 6, "the deadline never asks the probe");
        assert!(state.resumable().is_empty());
        registry.verify(&mut Probing::default()).unwrap();
    }

    /// The saved registry never holds a `Reserved` row the in-memory set
    /// forgot: the Tombstones are saved before the set forgets them, and a
    /// failed save keeps (and restores) every reservation.
    #[test]
    fn allocation_expiry_never_leaves_a_forgotten_reserved_row_on_disk() {
        fn forgotten(disk: &Registry, state: &Allocations) -> bool {
            disk.employees.iter().any(|account| account.state == AccountState::Reserved && !state.resumable().contains(&account.employee))
        }
        let (mut state, mut registry) = (Allocations::default(), Registry::default());
        let mut host = Probing { probes: vec![pending(), pending()], ..Probing::default() };
        let mut disk: Option<Registry> = None;
        for (employee, now) in [(1u8, 0u64), (2, 1)] {
            let mut save = |saved: &Registry| -> io::Result<()> { disk = Some(saved.clone()); Ok(()) };
            assert!(state.allocate(&mut registry, EmployeeId([employee; 16]), &mut host, now, &mut save).is_err());
            assert!(!forgotten(disk.as_ref().unwrap(), &state));
        }
        let due = PENDING_RESERVATION_TTL_MS + 2;
        assert!(state.expire(&mut registry, &mut host, due,
            &mut |_: &Registry| -> io::Result<()> { Err(io::Error::other("injected save failure")) }).is_err());
        assert_eq!(state.resumable().len(), 2, "nothing forgotten");
        assert!(registry.employees.iter().all(|account| account.state == AccountState::Reserved), "rows restored");
        assert!(host.recovered.is_empty(), "nothing quarantined");
        assert!(!forgotten(disk.as_ref().unwrap(), &state));
        let expired = state.expire(&mut registry, &mut host, due, &mut |saved: &Registry| -> io::Result<()> {
            assert!(saved.employees.iter().all(|account| account.state == AccountState::Tombstone), "saved before forgotten");
            disk = Some(saved.clone()); Ok(())
        }).unwrap();
        assert_eq!(expired.len(), 2);
        assert!(!forgotten(disk.as_ref().unwrap(), &state));
        assert_eq!(disk.as_ref().unwrap().employees, registry.employees);
        assert_eq!(host.recovered, [70000, 70001]);
    }

    /// F6 with the real evidence component: allocation queues the forced
    /// probe and returns "pending" at once (no worker runs at all here); once
    /// the worker has proven the account denied, the same employee's next
    /// allocation is Ready; a Reachable proof tombstones it.
    #[test]
    fn allocation_returns_pending_without_waiting_and_resumes_on_the_worker_result() {
        use crate::ssh_evidence::{self, AccountKey, HostKey, ProbeEvidence, ProbeOutcome, Shared, SshEvidence};
        use duduclaw_isolation::ids::Sha256Digest;
        use std::sync::{atomic::{AtomicU64, Ordering}, Arc, Mutex};
        /// Accounts backed by a `ProbeEvidence`, as `LinuxAccounts::login_denied` is.
        struct Evidence<'a> { evidence: &'a ProbeEvidence }
        impl AccountHost for Evidence<'_> {
            fn policy(&mut self) -> io::Result<HostUidPolicy> { Host.policy() }
            fn verify_namespace(&mut self, _: &Registry) -> io::Result<()> { Ok(()) }
            fn uid_available(&mut self, _: u32) -> io::Result<bool> { Ok(true) }
            fn occupied_ids(&mut self, _: UidRange) -> io::Result<BTreeSet<u32>> { Ok(BTreeSet::new()) }
            fn create_locked(&mut self, _: &EmployeeAccount) -> io::Result<()> { Ok(()) }
            fn verify_locked(&mut self, _: &EmployeeAccount) -> io::Result<()> { Ok(()) }
            fn recover_locked(&mut self, _: &EmployeeAccount) -> io::Result<()> { Ok(()) }
            fn login_denied(&mut self, account: &EmployeeAccount) -> io::Result<()> {
                let key = AccountKey { account: account_name(account.uid), uid: account.uid, sshd_config: Sha256Digest([5; 32]), port: 22 };
                match ssh_evidence::refusal(self.evidence.request(&key).as_ref()) { None => Ok(()), Some(refusal) => Err(refusal.error()) }
            }
        }
        struct Worker { clock: Arc<AtomicU64>, answers: Vec<ProbeOutcome> }
        impl ssh_evidence::Host for Worker {
            fn host_key(&mut self) -> io::Result<HostKey> {
                Ok(HostKey { boot_id: "boot".into(), generation: Sha256Digest([1; 32]),
                    probe: crate::selftest::ArtifactKey { sha256: Sha256Digest([2; 32]), size: 1, mode: 0o755 },
                    unit: Sha256Digest([3; 32]), listener: None })
            }
            fn probe(&mut self, _: &str, port: u16) -> ProbeOutcome { assert_eq!(port, 22); self.answers.remove(0) }
            fn now_ms(&mut self) -> io::Result<u64> { Ok(self.clock.load(Ordering::SeqCst)) }
            fn log(&mut self, _: &str) {}
        }
        let clock = Arc::new(AtomicU64::new(10_000));
        let slot: Arc<Mutex<Option<Arc<Shared>>>> = Arc::new(Mutex::new(None));
        let (started, time) = (slot.clone(), clock.clone());
        let evidence = ProbeEvidence::new(Box::new(move |shared: Arc<Shared>| *started.lock().unwrap() = Some(shared)),
            Box::new(move || -> io::Result<u64> { Ok(time.load(Ordering::SeqCst)) }));
        let mut worker = Worker { clock: clock.clone(), answers: vec![ProbeOutcome::Denied,
            ProbeOutcome::Reachable("login_not_denied (exit 10)".into())] };
        let (mut state, mut registry) = (Allocations::default(), Registry::default());
        let mut save = |_: &Registry| -> io::Result<()> { Ok(()) };
        let begun = std::time::Instant::now();
        let error = state.allocate(&mut registry, EmployeeId([1; 16]), &mut Evidence { evidence: &evidence }, 0, &mut save).unwrap_err();
        assert!(begun.elapsed() < std::time::Duration::from_secs(1), "allocation never waits for the probe");
        assert_eq!(crate::accounts::ssh_refusal(&error), Some(&SshRefusal::Pending), "{error}");
        assert_eq!(registry.employees[0].state, AccountState::Reserved);
        // Still pending until the worker runs.
        assert!(crate::accounts::probe_unresolved(&state.allocate(&mut registry, EmployeeId([1; 16]),
            &mut Evidence { evidence: &evidence }, 0, &mut save).unwrap_err()));
        let shared = slot.lock().unwrap().clone().expect("worker started");
        assert!(ssh_evidence::step(&mut worker, &shared));
        let account = state.allocate(&mut registry, EmployeeId([1; 16]), &mut Evidence { evidence: &evidence }, 0, &mut save).unwrap();
        assert_eq!((account.uid, account.state), (70000, AccountState::Ready));
        // A second employee whose probe proves it reachable is tombstoned on resume.
        assert!(state.allocate(&mut registry, EmployeeId([2; 16]), &mut Evidence { evidence: &evidence }, 0, &mut save).is_err());
        assert!(ssh_evidence::step(&mut worker, &shared));
        let error = state.allocate(&mut registry, EmployeeId([2; 16]), &mut Evidence { evidence: &evidence }, 0, &mut save).unwrap_err();
        assert!(matches!(crate::accounts::ssh_refusal(&error), Some(SshRefusal::Reachable(_))), "{error}");
        assert_eq!(registry.employees[1].state, AccountState::Tombstone);
        assert!(state.resumable().is_empty());
    }

    /// R8w item 2: every allocation failure names its step: the host's own
    /// when it gave one, else the allocation step; the account is the
    /// reserved uid; kind, text and the typed probe refusal are unchanged.
    #[test]
    fn allocation_failures_carry_their_step() {
        use crate::accounts::{step_account, step_of};
        let (mut state, mut registry, mut saves) = (Allocations::default(), Registry::default(), 0);
        let mut host = Probing { create_fail: true, ..Probing::default() };
        let error = allocate(&mut state, &mut registry, 1, &mut host, &mut saves).unwrap_err();
        assert_eq!((step_of(&error), step_account(&error)), (Some(Step::CreateAccount), Some(70000)));
        assert_eq!(error.to_string(), "injected useradd failure");
        assert_eq!(crate::accounts::allocation_line(&error), "allocation failed: step=create_account");
        let mut host = Probing { probes: vec![Ok(())], verify_fail: true, ..Probing::default() };
        let error = allocate(&mut state, &mut registry, 2, &mut host, &mut saves).unwrap_err();
        assert_eq!((step_of(&error), step_account(&error)), (Some(Step::Readback), Some(70001)));
        // The host's finer step wins over the allocation step.
        let mut host = Probing { probes: vec![Ok(())], verify_fail: true, verify_step: Some(Step::AtSpool), ..Probing::default() };
        let error = allocate(&mut state, &mut registry, 3, &mut host, &mut saves).unwrap_err();
        assert_eq!((step_of(&error), step_account(&error)), (Some(Step::AtSpool), Some(70002)));
        assert_eq!(registry.employees[2].state, AccountState::Tombstone);
        let mut host = Probing { probes: vec![reachable()], ..Probing::default() };
        let error = allocate(&mut state, &mut registry, 4, &mut host, &mut saves).unwrap_err();
        assert_eq!(step_of(&error), Some(Step::SshProbe));
        assert!(matches!(crate::accounts::ssh_refusal(&error), Some(SshRefusal::Reachable(_))), "{error}");
        let mut host = Probing { probes: vec![pending()], ..Probing::default() };
        let error = allocate(&mut state, &mut registry, 5, &mut host, &mut saves).unwrap_err();
        assert!(crate::accounts::probe_unresolved(&error), "{error}");
        assert_eq!((step_of(&error), error.kind()), (Some(Step::SshProbe), io::ErrorKind::WouldBlock));
        // Reservation: the inventory, then an exhausted range.
        let mut inventory = Inventory { occupied: Err(io::ErrorKind::Other.into()), scans: 0, candidates: vec![] };
        assert_eq!(step_of(&registry.reserve(EmployeeId([6; 16]), &mut inventory).unwrap_err()), Some(Step::OwnerInventory));
        let mut full = Registry { range: UidRange { first: 70000, last: 70000 }, ..Registry::default() };
        let mut host = Probing { probes: vec![Ok(())], ..Probing::default() };
        let mut fresh = Allocations::default();
        allocate(&mut fresh, &mut full, 1, &mut host, &mut saves).unwrap();
        let error = allocate(&mut fresh, &mut full, 2, &mut host, &mut saves).unwrap_err();
        assert_eq!((step_of(&error), step_account(&error), error.kind()), (Some(Step::Reserve), None, io::ErrorKind::StorageFull));
        // Host-wide verification: a Reserved row nobody can resume.
        full.employees.push(EmployeeAccount { employee: EmployeeId([9; 16]), uid: 70000, gid: 70000, state: AccountState::Reserved });
        full.employees.remove(0);
        let error = full.verify(&mut Probing::default()).unwrap_err();
        assert_eq!((step_of(&error), step_account(&error)), (Some(Step::RegistryRows), Some(70000)));
    }

    /// R8w review round 1 (F2): EVERY production failure branch of the
    /// store-level allocation (`Allocations::allocate_stored`, what
    /// `RegistryStore::allocate` runs) yields a fixed step, never
    /// `unlabelled`, and its journal line ([`allocation_log_line`]) names it;
    /// the two "retry later" answers (unresolved probe, pending limit) are
    /// not logged. The fakes fail without a label of their own wherever the
    /// production host could, so the allocation's own label is what is
    /// pinned. (`reserve`'s "employee already has a row" refusal is
    /// unreachable through `allocate`: the existing row is refused first,
    /// `existing_row`.)
    #[test]
    fn every_allocation_failure_branch_is_labelled() {
        use crate::accounts::step_of;
        type Prepare = fn(&mut Allocations, &mut Registry, &mut Probing);
        struct Case { name: &'static str, step: Step, poisoned: bool, clock_fails: bool, fail_save: usize, prepare: Prepare }
        fn row(registry: &mut Registry, employee: u8, uid: u32, state: AccountState) {
            registry.employees.push(EmployeeAccount { employee: EmployeeId([employee; 16]), uid, gid: uid, state });
        }
        /// Employee 1 left resumable by a pending probe (its Reserved row, uid 70000).
        fn resumable(state: &mut Allocations, registry: &mut Registry, host: &mut Probing) {
            host.probes = vec![pending()];
            let error = state.allocate(registry, EmployeeId([1; 16]), host, 0, &mut |_: &Registry| Ok(())).unwrap_err();
            assert!(crate::accounts::probe_unresolved(&error));
        }
        fn case(name: &'static str, step: Step, prepare: Prepare) -> Case {
            Case { name, step, poisoned: false, clock_fails: false, fail_save: 0, prepare }
        }
        let cases = [
            Case { poisoned: true, ..case("store poisoned", Step::StorePoisoned, |_, _, _| {}) },
            Case { clock_fails: true, ..case("clock unreadable", Step::Clock, |_, _, _| {}) },
            // A Ready employee is re-verified (host-wide).
            case("ready: policy", Step::Readback, |_, registry, host| { row(registry, 1, 70000, AccountState::Ready); host.policy_fail = true; }),
            case("ready: namespace", Step::Readback, |_, registry, host| { row(registry, 1, 70000, AccountState::Ready); host.namespace_fail = true; }),
            case("ready: readback", Step::Readback, |_, registry, host| { row(registry, 1, 70000, AccountState::Ready); host.verify_fail = true; }),
            case("ready: foreign reserved row", Step::RegistryRows, |_, registry, _| {
                row(registry, 1, 70000, AccountState::Ready); row(registry, 9, 70001, AccountState::Reserved); }),
            // An existing row no allocation can use.
            case("tombstone", Step::ExistingRow, |_, registry, _| row(registry, 1, 70000, AccountState::Tombstone)),
            case("unknown reserved row", Step::ExistingRow, |_, registry, _| row(registry, 1, 70000, AccountState::Reserved)),
            // The reservation.
            case("reserve: policy", Step::Reserve, |_, _, host| host.policy_fail = true),
            case("reserve: registry rows", Step::RegistryRows, |_, registry, _| registry.version = 2),
            case("reserve: namespace", Step::Reserve, |_, _, host| host.namespace_fail = true),
            case("reserve: owner inventory", Step::OwnerInventory, |_, _, host| host.inventory_fail = true),
            case("reserve: candidate", Step::Candidate, |_, _, host| host.candidate_fail = true),
            case("reserve: range exhausted", Step::Reserve, |_, registry, _| {
                registry.range = UidRange { first: 70000, last: 70000 }; row(registry, 9, 70000, AccountState::Tombstone); }),
            Case { fail_save: 1, ..case("persist reservation", Step::PersistReservation, |_, _, _| {}) },
            // A new account.
            case("create", Step::CreateAccount, |_, _, host| host.create_fail = true),
            case("probe: sshd -T", Step::SshProbe, |_, _, host| host.probes = vec![Err(io::Error::new(io::ErrorKind::PermissionDenied, "sshd -T"))]),
            case("probe: reachable", Step::SshProbe, |_, _, host| host.probes = vec![reachable()]),
            case("readback", Step::Readback, |_, _, host| { host.probes = vec![Ok(())]; host.verify_fail = true; }),
            Case { fail_save: 2, ..case("persist final: ready", Step::PersistFinal, |_, _, host| host.probes = vec![Ok(())]) },
            Case { fail_save: 2, ..case("persist final: tombstone", Step::PersistFinal, |_, _, host| host.create_fail = true) },
            Case { fail_save: 2, ..case("persist final: pending", Step::PersistFinal, |_, _, host| host.probes = vec![pending()]) },
            // A resumed reservation.
            case("resume: reachable", Step::SshProbe, |state, registry, host| { resumable(state, registry, host); host.probes = vec![reachable()]; }),
            case("resume: sshd -T", Step::SshProbe, |state, registry, host| {
                resumable(state, registry, host); host.probes = vec![Err(io::Error::new(io::ErrorKind::PermissionDenied, "sshd -T"))]; }),
            case("resume: quarantine", Step::Quarantine, |state, registry, host| {
                resumable(state, registry, host); host.probes = vec![Ok(())]; host.recover_fail = true; }),
            case("resume: readback", Step::Readback, |state, registry, host| {
                resumable(state, registry, host); host.probes = vec![Ok(())]; host.verify_fail = true; }),
            Case { fail_save: 1, ..case("resume: persist final", Step::PersistFinal, |state, registry, host| {
                resumable(state, registry, host); host.probes = vec![Ok(())]; }) },
        ];
        let mut covered = BTreeSet::new();
        for case in &cases {
            let (mut state, mut registry, mut host) = (Allocations::default(), Registry::default(), Probing::default());
            (case.prepare)(&mut state, &mut registry, &mut host);
            let (rows, creates) = (registry.employees.clone(), host.creates);
            let mut saves = 0usize;
            let error = state.allocate_stored(&mut registry, EmployeeId([1; 16]), &mut host, case.poisoned,
                &mut || if case.clock_fails { Err(io::Error::other("injected clock failure")) } else { Ok(0) },
                &mut |_: &Registry| { saves += 1; if saves == case.fail_save { Err(io::Error::other("injected save failure")) } else { Ok(()) } })
                .unwrap_err();
            assert_eq!(step_of(&error), Some(case.step), "{}: {error}", case.name);
            assert_ne!(step_of(&error), Some(Step::Unlabelled), "{}", case.name);
            assert_eq!(allocation_log_line(&error), Some(format!("allocation failed: step={}", case.step.name())), "{}", case.name);
            if case.poisoned || case.clock_fails {
                assert_eq!((registry.employees, host.creates, saves), (rows, creates, 0), "{}: refused before any effect", case.name);
            }
            covered.insert(case.step.name());
        }
        for step in [Step::StorePoisoned, Step::Clock, Step::ExistingRow, Step::PersistReservation, Step::PersistFinal,
            Step::Reserve, Step::CreateAccount, Step::SshProbe, Step::Quarantine, Step::Readback] {
            assert!(covered.contains(step.name()), "{}", step.name());
        }
        // Not failures, not logged: the unresolved probe and the pending cap.
        let (mut state, mut registry, mut saves) = (Allocations::default(), Registry::default(), 0);
        let mut host = Probing { probes: (0..MAX_PENDING_RESERVATIONS).map(|_| pending()).collect(), ..Probing::default() };
        for employee in 1..=MAX_PENDING_RESERVATIONS as u8 {
            let error = allocate(&mut state, &mut registry, employee, &mut host, &mut saves).unwrap_err();
            assert_eq!(allocation_log_line(&error), None, "{error}");
        }
        let error = state.allocate_stored(&mut registry, EmployeeId([9; 16]), &mut host, false, &mut || Ok(0),
            &mut |_: &Registry| Ok(())).unwrap_err();
        assert!(pending_limit(&error));
        assert_eq!(allocation_log_line(&error), None);
        assert_eq!(allocation_log_line(&inconclusive().unwrap_err()), None);
    }

    #[test]
    fn allocation_keeps_every_existing_tombstone_trigger() {
        // useradd/groupadd/lockdown failure: tombstone, quarantine, no probe wait.
        let (mut state, mut registry, mut saves) = (Allocations::default(), Registry::default(), 0);
        let mut host = Probing { create_fail: true, ..Probing::default() };
        assert!(allocate(&mut state, &mut registry, 1, &mut host, &mut saves).is_err());
        assert_eq!((registry.employees[0].state, host.waits, host.recoveries), (AccountState::Tombstone, 0, 1));
        // Readback after a Denied probe fails: tombstone.
        let mut host = Probing { probes: vec![Ok(())], verify_fail: true, ..Probing::default() };
        assert!(allocate(&mut state, &mut registry, 2, &mut host, &mut saves).is_err());
        assert_eq!(registry.employees[1].state, AccountState::Tombstone);
        // A non-probe error from the probe step (e.g. the sshd effective configuration) tombstones.
        let mut host = Probing { probes: vec![Err(io::Error::new(io::ErrorKind::PermissionDenied, "sshd -T"))], ..Probing::default() };
        assert!(allocate(&mut state, &mut registry, 3, &mut host, &mut saves).is_err());
        assert_eq!(registry.employees[2].state, AccountState::Tombstone);
        assert!(state.resumable().is_empty());
        // A Reserved row this process did not leave resumable (a crash) is refused untouched.
        registry.employees.push(EmployeeAccount { employee: EmployeeId([9; 16]), uid: 70009, gid: 70009, state: AccountState::Reserved });
        let mut host = Probing::default();
        assert!(allocate(&mut state, &mut registry, 9, &mut host, &mut saves).is_err());
        assert_eq!((registry.employees[3].state, host.creates, host.recoveries, host.waits), (AccountState::Reserved, 0, 0, 0));
        assert!(registry.verify_with(&mut Probing::default(), state.resumable()).is_err());
        // Tombstones are never revived.
        assert!(allocate(&mut state, &mut registry, 1, &mut Probing::default(), &mut saves).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires root; creates only a private registry under /run, no system accounts"]
    fn iso_root_registry_restart_quarantines_reserved_uid_and_preserves_tombstone() {
        use std::{fs, os::unix::fs::{DirBuilderExt, MetadataExt}};
        let directory = std::path::PathBuf::from(format!("/run/duduclaw-registry-test-{}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&directory).unwrap();
        let mut registry = Registry::default();
        registry.reserve(EmployeeId([7; 16]), &mut Host).unwrap();
        let path = directory.join("registry.json");
        {
            use std::{io::Write, os::unix::fs::OpenOptionsExt};
            let mut file = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path).unwrap();
            file.write_all(&serde_json::to_vec(&registry).unwrap()).unwrap();
            file.sync_all().unwrap();
        }
        let mut store = RegistryStore::open_at(&directory).unwrap();
        assert!(RegistryStore::open_at(&directory).is_err(), "global lock must exclude a second allocator");
        assert_eq!(store.registry().employees[0].state, AccountState::Reserved);
        assert_eq!(store.reconcile(&mut Host).unwrap(), RecoveryReport { abandoned_reservations: 1, quarantined_identities: 1 });
        assert!(store.allocate(EmployeeId([7; 16]), &mut Host).is_err());
        assert_eq!(store.allocate(EmployeeId([8; 16]), &mut Host).unwrap().uid, 70001);
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        drop(store);
        let store = RegistryStore::open_at(&directory).unwrap();
        assert_eq!(store.registry().employees[0].state, AccountState::Tombstone);
        assert_eq!(store.registry().employees[1].uid, 70001);
        drop(store);
        fs::remove_file(path).unwrap();
        fs::remove_file(directory.join("registry.lock")).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires root; verifies symlink and non-private state directories are refused"]
    fn iso_root_registry_rejects_untrusted_state_directory() {
        use std::{fs, os::unix::fs::{DirBuilderExt, PermissionsExt, symlink}};
        let directory = std::path::PathBuf::from(format!("/run/duduclaw-registry-perms-{}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o750)).unwrap();
        assert!(RegistryStore::open_at(&directory).is_err());
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        symlink("/etc/passwd", directory.join("registry.lock")).unwrap();
        assert!(RegistryStore::open_at(&directory).is_err());
        fs::remove_file(directory.join("registry.lock")).unwrap();
        fs::remove_dir(directory).unwrap();
    }
}
