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
    fn create_locked(&mut self, account: &EmployeeAccount) -> io::Result<()>;
    fn verify_locked(&mut self, account: &EmployeeAccount) -> io::Result<()>;
    /// Quarantine an interrupted/retired identity, including a group-only residue.
    /// Must not create a missing user, delete the permanent mapping, or accept a
    /// same-uid alias. Success includes a complete no-process readback.
    fn recover_locked(&mut self, _account: &EmployeeAccount) -> io::Result<()> { Err(invalid()) }
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

    pub fn verify(&self, host: &mut dyn AccountHost) -> io::Result<()> {
        self.validate(&host.policy()?)?;
        host.verify_namespace(self)?;
        for account in &self.employees {
            if account.state == AccountState::Reserved { return Err(invalid()); }
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
        for uid in self.range.first..=self.range.last {
            if !used.contains(&uid) && host.uid_available(uid)? {
                self.employees.push(EmployeeAccount { employee, uid, gid: uid, state: AccountState::Reserved });
                return Ok(self.employees.len() - 1);
            }
        }
        Err(io::Error::new(io::ErrorKind::StorageFull, "employee identity range exhausted"))
    }
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
    pub struct RegistryStore { directory: PathBuf, _lock: File, data: Registry, poisoned: bool }

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
            let mut store = Self { directory: directory.to_path_buf(), _lock: lock, data, poisoned: false };
            // Persist even an empty initial registry; never infer unused IDs later.
            store.save()?;
            Ok(store)
        }

        fn save(&mut self) -> io::Result<()> {
            if self.poisoned { return Err(invalid()); }
            let result = self.save_inner();
            if result.is_err() { self.poisoned = true; }
            result
        }

        fn save_inner(&self) -> io::Result<()> {
            let path = self.directory.join("registry.json");
            let temporary = self.directory.join("registry.next");
            match fs::symlink_metadata(&temporary) {
                Ok(_) => { root_path(&temporary, false)?; fs::remove_file(&temporary)?; }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {},
                Err(error) => return Err(error),
            }
            let bytes = serde_json::to_vec(&self.data).map_err(|_| invalid())?;
            if bytes.len() as u64 > MAX_REGISTRY_BYTES { return Err(invalid()); }
            let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&temporary, path)?;
            File::open(&self.directory)?.sync_all()
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

        pub fn allocate(&mut self, employee: EmployeeId, host: &mut dyn AccountHost) -> io::Result<EmployeeAccount> {
            if self.poisoned { return Err(invalid()); }
            if let Some(existing) = self.data.employees.iter().find(|account| account.employee == employee) {
                if existing.state != AccountState::Ready { return Err(invalid()); }
                self.data.verify(host)?;
                return Ok(existing.clone());
            }
            let index = self.data.reserve(employee, host)?;
            self.save()?;
            let result = host.create_locked(&self.data.employees[index])
                .and_then(|()| host.verify_locked(&self.data.employees[index]));
            self.data.employees[index].state = if result.is_ok() { AccountState::Ready } else { AccountState::Tombstone };
            self.save()?;
            if let Err(error) = result {
                // Best-effort immediate quarantine. A failure stays durable and
                // startup retries it before any protected execution is admitted.
                let _ = host.recover_locked(&self.data.employees[index]);
                return Err(error);
            }
            Ok(self.data.employees[index].clone())
        }

        /// Explicit retirement never removes the mapping and never recycles its uid.
        pub fn retire(&mut self, employee: EmployeeId) -> io::Result<()> {
            if self.poisoned { return Err(invalid()); }
            let account = self.data.employees.iter_mut().find(|account| account.employee == employee).ok_or_else(invalid)?;
            account.state = AccountState::Tombstone;
            self.save()
        }
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
        fn create_locked(&mut self, _: &EmployeeAccount) -> io::Result<()> { Ok(()) }
        fn verify_locked(&mut self, _: &EmployeeAccount) -> io::Result<()> { Ok(()) }
        fn recover_locked(&mut self, _: &EmployeeAccount) -> io::Result<()> { Ok(()) }
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
