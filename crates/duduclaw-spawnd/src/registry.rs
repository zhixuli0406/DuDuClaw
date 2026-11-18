//! Permanent root-owned employee identities. Allocation precedes account creation.
use std::{collections::BTreeSet, io};
use serde::{Deserialize, Serialize};
use duduclaw_isolation::ids::{EmployeeId, UidPolicy, UidRange, EMPLOYEE_UID_MIN, EMPLOYEE_UID_MAX};

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
    /// R7c: wait, bounded, until the SSH denial probe of a just-created (or
    /// resumed) account reports Denied. An unresolved result is an error
    /// carrying `crate::accounts::SshRefusal` (`probe_unresolved`), which
    /// keeps the reservation resumable instead of tombstoning it. Default:
    /// nothing to wait for (hosts and fakes without a probe).
    fn await_login_denied(&mut self, _account: &EmployeeAccount) -> io::Result<()> { Ok(()) }
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
    pub fn verify_with(&self, host: &mut dyn AccountHost, resumable: &BTreeSet<EmployeeId>) -> io::Result<()> {
        self.validate(&host.policy()?)?;
        host.verify_namespace(self)?;
        for account in &self.employees {
            if account.state == AccountState::Reserved && !resumable.contains(&account.employee) { return Err(invalid()); }
            if account.state == AccountState::Ready { host.verify_locked(account)?; }
        }
        Ok(())
    }

    /// Persistence must be durable before this returns a candidate to useradd.
    #[cfg(any(target_os = "linux", test))]
    fn reserve(&mut self, employee: EmployeeId, host: &mut dyn AccountHost) -> io::Result<usize> {
        self.validate(&host.policy()?)?;
        host.verify_namespace(self)?;
        if self.employees.iter().any(|account| account.employee == employee) { return Err(invalid()); }
        let used: BTreeSet<_> = self.employees.iter().map(|account| account.uid).collect();
        // R7b: one file-owner inventory per allocation. The candidate's gid is
        // its uid, so an id owning a file as either is skipped.
        let occupied = host.occupied_ids(self.range)?;
        for uid in self.range.first..=self.range.last {
            if !used.contains(&uid) && !occupied.contains(&uid) && host.uid_available(uid)? {
                self.employees.push(EmployeeAccount { employee, uid, gid: uid, state: AccountState::Reserved });
                return Ok(self.employees.len() - 1);
            }
        }
        Err(io::Error::new(io::ErrorKind::StorageFull, "employee identity range exhausted"))
    }
}

/// R7c: the allocation state machine of one registry (portable; the root
/// `RegistryStore` persists around it).
///
/// - New employee: `reserve` (durable `Reserved`), `create_locked`, then
///   `await_login_denied` (the SSH probe of the new account, bounded wait),
///   then `verify_locked`. All pass ⇒ `Ready`.
/// - The probe is unresolved (pending at the deadline, inconclusive, PID 1
///   error, contradictory "no sshd") ⇒ the row STAYS `Reserved`, is
///   remembered here as resumable, nothing is quarantined, and the error is
///   returned. The uid is not burned.
/// - The next allocation of the SAME employee in this process resumes it:
///   `recover_locked` (lockdown of the existing user and no-process readback,
///   as start-up recovery does), then the same probe wait and `verify_locked`,
///   with the same three outcomes. A `Reserved` row this set does not know
///   (a crash mid-allocation) is still refused as before.
/// - Every other failure, including a probe that proves the account
///   reachable, ⇒ `Tombstone` + best-effort `recover_locked`, as before.
///
/// The set is memory only: after a restart `RegistryStore::reconcile` turns
/// every `Reserved` row into a `Tombstone` exactly as before (unchanged rule),
/// so an unresolved reservation does not outlive the process that made it.
#[cfg(any(target_os = "linux", test))]
#[derive(Debug, Default)]
pub struct Allocations { unresolved: BTreeSet<EmployeeId> }
#[cfg(any(target_os = "linux", test))]
impl Allocations {
    /// Employees whose `Reserved` row an unresolved probe left resumable.
    pub fn resumable(&self) -> &BTreeSet<EmployeeId> { &self.unresolved }
    /// Allocate (or resume) `employee` in `registry`; `save` persists the
    /// registry durably (before useradd, and after the outcome is decided).
    pub fn allocate(&mut self, registry: &mut Registry, employee: EmployeeId, host: &mut dyn AccountHost,
        save: &mut dyn FnMut(&Registry) -> io::Result<()>) -> io::Result<EmployeeAccount> {
        let (index, resumed) = match registry.employees.iter().position(|account| account.employee == employee) {
            Some(index) => match registry.employees[index].state {
                AccountState::Ready => {
                    registry.verify_with(host, &self.unresolved)?;
                    return Ok(registry.employees[index].clone());
                }
                AccountState::Reserved if self.unresolved.contains(&employee) => (index, true),
                _ => return Err(invalid()),
            },
            None => {
                let index = registry.reserve(employee, host)?;
                save(registry)?;
                (index, false)
            }
        };
        let account = registry.employees[index].clone();
        let prepared = if resumed { host.recover_locked(&account) } else { host.create_locked(&account) };
        let result = prepared.and_then(|()| host.await_login_denied(&account)).and_then(|()| host.verify_locked(&account));
        let unresolved = result.as_ref().err().is_some_and(crate::accounts::probe_unresolved);
        registry.employees[index].state = match (&result, unresolved) {
            (Ok(()), _) => AccountState::Ready,
            (Err(_), true) => AccountState::Reserved,
            (Err(_), false) => AccountState::Tombstone,
        };
        if unresolved { self.unresolved.insert(employee); } else { self.unresolved.remove(&employee); }
        save(registry)?;
        if let Err(error) = result {
            // Best-effort immediate quarantine of a tombstone. A failure stays
            // durable and startup retries it before any protected execution.
            if !unresolved { let _ = host.recover_locked(&registry.employees[index]); }
            return Err(error);
        }
        Ok(registry.employees[index].clone())
    }
    /// The employee's row became a tombstone (explicit retirement/compensation).
    pub fn forget(&mut self, employee: EmployeeId) { self.unresolved.remove(&employee); }
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
    pub struct RegistryStore { directory: PathBuf, _lock: File, data: Registry, poisoned: bool, allocations: Allocations }

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
                allocations: Allocations::default() };
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
        pub fn allocate(&mut self, employee: EmployeeId, host: &mut dyn AccountHost) -> io::Result<EmployeeAccount> {
            if self.poisoned { return Err(invalid()); }
            let Self { directory, data, poisoned, allocations, .. } = self;
            allocations.allocate(data, employee, host, &mut |registry: &Registry| {
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
    }
    #[test]
    fn uid_range_overlap_refuses_whole_range() {
        let mut policy = Host.policy().unwrap();
        policy.subgids.push(UidRange { first: 99999, last: 150000 });
        assert!(policy.check(Registry::default().range, 70000).is_err());
        assert_eq!(account_name(70000), "duduclaw-emp-70000");
    }

    /// R7c: a host whose SSH probe outcomes are scripted (one per
    /// `await_login_denied`), with injected create/verify failures.
    #[derive(Default)]
    struct Probing { probes: Vec<io::Result<()>>, create_fail: bool, verify_fail: bool,
        creates: usize, recoveries: usize, waits: usize }
    impl AccountHost for Probing {
        fn policy(&mut self) -> io::Result<HostUidPolicy> { Host.policy() }
        fn verify_namespace(&mut self, _: &Registry) -> io::Result<()> { Ok(()) }
        fn uid_available(&mut self, _: u32) -> io::Result<bool> { Ok(true) }
        fn occupied_ids(&mut self, _: UidRange) -> io::Result<BTreeSet<u32>> { Ok(BTreeSet::new()) }
        fn create_locked(&mut self, _: &EmployeeAccount) -> io::Result<()> {
            self.creates += 1;
            if self.create_fail { Err(io::Error::other("injected useradd failure")) } else { Ok(()) }
        }
        fn verify_locked(&mut self, _: &EmployeeAccount) -> io::Result<()> {
            if self.verify_fail { Err(io::Error::other("injected lockdown readback failure")) } else { Ok(()) }
        }
        fn recover_locked(&mut self, _: &EmployeeAccount) -> io::Result<()> { self.recoveries += 1; Ok(()) }
        fn await_login_denied(&mut self, _: &EmployeeAccount) -> io::Result<()> {
            self.waits += 1;
            if self.probes.is_empty() { panic!("unexpected probe wait") }
            self.probes.remove(0)
        }
    }
    use crate::accounts::SshRefusal;
    fn inconclusive() -> io::Result<()> { Err(SshRefusal::Inconclusive("identity (exit 3)".into()).error()) }
    fn reachable() -> io::Result<()> { Err(SshRefusal::Reachable("login_not_denied (exit 10)".into()).error()) }
    fn pending() -> io::Result<()> { Err(SshRefusal::Pending.error()) }
    /// Runs `Allocations::allocate`, counting the durable saves.
    fn allocate(state: &mut Allocations, registry: &mut Registry, employee: u8, host: &mut Probing, saves: &mut usize)
        -> io::Result<EmployeeAccount> {
        state.allocate(registry, EmployeeId([employee; 16]), host, &mut |_: &Registry| { *saves += 1; Ok(()) })
    }

    #[test]
    fn allocation_waits_for_the_probe_and_denied_is_ready() {
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
            || Err(SshRefusal::NotApplicable.error())] {
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
            // A retry for the same employee resumes: re-lock, probe again, no new account.
            assert!(crate::accounts::probe_unresolved(&allocate(&mut state, &mut registry, 1, &mut host, &mut saves).unwrap_err()));
            assert_eq!((host.creates, host.recoveries, host.waits), (1, 1, 2));
            assert_eq!(registry.employees[0].state, AccountState::Reserved);
            let account = allocate(&mut state, &mut registry, 1, &mut host, &mut saves).unwrap();
            assert_eq!((account.uid, account.state), (70000, AccountState::Ready), "same uid, now Ready");
            assert_eq!((host.creates, host.recoveries, host.waits), (1, 2, 3));
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
