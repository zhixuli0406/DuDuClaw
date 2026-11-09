//! Root-local package H adapter. Its ledger is separate from H's image registry.
use std::{fs::{self, File, OpenOptions}, io::{Read, Write}, os::{fd::{AsRawFd, FromRawFd, OwnedFd},
    unix::{ffi::OsStrExt, fs::{MetadataExt, OpenOptionsExt}}}, path::{Path, PathBuf}};
use duduclaw_isolation::{execution::{DiskQuota, RunHandle}, ids::{EmployeeId, RequestId, Sha256Digest}, ipc::ErrorCode};
use duduclaw_isolation_storage::{EmployeeStorage, ImageStatus, Quota};
use super::{adapter_state::{Employee, Journal, Lease, Operation, Phase, MAX_BYTES}, *};

fn unavailable<T>(_: T) -> ErrorCode { ErrorCode::IsolationUnavailable }

/// Construct after root installation has created both directories. The adapter
/// never allocates accounts or authorizes a migration proposal. B1 retains its
/// admission/UID/cgroup fences while calling these synchronous methods.
pub struct LoopStorage {
    provider: Box<dyn EmployeeStorage>,
    journal: Journal,
    directory: PathBuf,
    _lock: File,
    poisoned: bool,
}
impl LoopStorage {
    pub fn open(storage_directory: &Path, journal_directory: &Path) -> Result<Self> {
        let provider = duduclaw_isolation_storage::Storage::open(storage_directory).map_err(unavailable)?;
        Self::with_provider(Box::new(provider), journal_directory)
    }
    fn with_provider(provider: Box<dyn EmployeeStorage>, directory: &Path) -> Result<Self> {
        if unsafe { libc::geteuid() } != 0 { return Err(ErrorCode::IsolationUnavailable); }
        crate::registry::root_path(directory, true).map_err(unavailable)?;
        let meta = fs::symlink_metadata(directory).map_err(unavailable)?;
        if meta.gid() != 0 || meta.mode() & 0o7777 != 0o700 { return Err(ErrorCode::IsolationUnavailable); }
        let lock = OpenOptions::new().read(true).write(true).create(true).truncate(false)
            .mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(directory.join("storage-adapter.lock")).map_err(unavailable)?;
        check_file(&lock)?;
        fs2::FileExt::try_lock_exclusive(&lock).map_err(unavailable)?;
        let journal = match OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(directory.join("storage-adapter.json")) {
            Ok(file) => {
                check_file(&file)?;
                let mut bytes = Vec::new(); file.take(MAX_BYTES + 1).read_to_end(&mut bytes).map_err(unavailable)?;
                if bytes.len() as u64 > MAX_BYTES { return Err(ErrorCode::IsolationUnavailable); }
                serde_json::from_slice(&bytes).map_err(unavailable)?
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // A lost ledger must not adopt existing images or mount effects.
                if !provider.images().map_err(unavailable)?.is_empty()
                    || directory.join("storage-adapter.next").try_exists().map_err(unavailable)? {
                    return Err(ErrorCode::IsolationUnavailable);
                }
                Journal::empty()
            },
            Err(error) => return Err(unavailable(error)),
        };
        let mut value = Self { provider, journal, directory: directory.to_path_buf(), _lock: lock, poisoned: false };
        value.inventory()?;
        value.save()?;
        Ok(value)
    }
    fn healthy(&self) -> Result<()> {
        if self.poisoned { Err(ErrorCode::IsolationUnavailable) } else { Ok(()) }
    }
    fn inventory(&self) -> Result<Vec<ImageStatus>> {
        self.healthy()?;
        let images = self.provider.images().map_err(unavailable)?;
        self.journal.reconcile(&images)?;
        Ok(images)
    }
    fn publish_candidate(&mut self, candidate: Journal) -> Result<()> {
        // Invalid caller proposals have no durable effects and cannot poison a
        // healthy adapter. Once assigned, publication errors still fail closed.
        candidate.validate().map_err(|_| ErrorCode::InvalidRequest)?;
        self.journal = candidate;
        self.save()
    }
    fn save(&mut self) -> Result<()> {
        self.healthy()?;
        let result = (|| {
            self.journal.validate()?;
            let bytes = serde_json::to_vec(&self.journal).map_err(unavailable)?;
            if bytes.len() as u64 > MAX_BYTES { return Err(ErrorCode::ResourceLimit); }
            let temp = self.directory.join("storage-adapter.next");
            match OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(&temp) {
                Ok(file) => { check_file(&file)?; fs::remove_file(&temp).map_err(unavailable)?; },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                Err(error) => return Err(unavailable(error)),
            }
            let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(&temp).map_err(unavailable)?;
            check_file(&file)?;
            file.write_all(&bytes).map_err(unavailable)?; file.sync_all().map_err(unavailable)?;
            fs::rename(&temp, self.directory.join("storage-adapter.json")).map_err(unavailable)?;
            File::open(&self.directory).and_then(|file| file.sync_all()).map_err(unavailable)
        })();
        if result.is_err() { self.poisoned = true; }
        result
    }
    fn live_usage(&self, employee: EmployeeId) -> Result<StorageUsage> {
        self.healthy()?;
        self.provider.readiness().map_err(unavailable)?;
        let entry = self.journal.entry(employee)?;
        let actual = self.provider.usage(employee).map_err(unavailable)?;
        let usage = StorageUsage { employee, bytes: actual.used_bytes, inodes: actual.used_inodes,
            limit_bytes: actual.capacity_bytes, limit_inodes: actual.capacity_inodes,
            hard_quota_verified: true, device_graph_verified: true, host_reserve_verified: true };
        usage.check(employee, &DiskQuota::LoopFilesystem { bytes: entry.quota.bytes, inodes: entry.quota.inodes })?;
        Ok(usage)
    }
    fn image(&self, employee: EmployeeId) -> Result<ImageStatus> {
        self.inventory()?.into_iter().find(|image| image.employee == employee).ok_or(ErrorCode::NotFound)
    }
}
impl Storage for LoopStorage {
    fn readiness(&mut self) -> Result<()> {
        self.inventory()?;
        if self.journal.employees.iter().any(|entry| matches!(entry.phase, Phase::Provisioning | Phase::Destroying)) {
            return Err(ErrorCode::IsolationUnavailable);
        }
        self.provider.readiness().map_err(unavailable)
    }
    fn provision(&mut self, request: &ProvisionRequest) -> Result<StorageUsage> {
        self.inventory()?; request.validate()?;
        let quota = Quota::try_from(&request.quota).map_err(unavailable)?;
        let operation = Operation { id: request.operation_id, digest: request.proposal_digest };
        let mut candidate = self.journal.clone();
        if let Ok(entry) = self.journal.entry(request.employee) {
            if entry.provision != operation || entry.uid != request.uid || entry.quota != quota
                || matches!(entry.phase, Phase::Destroying | Phase::Destroyed) {
                return Err(ErrorCode::InvalidRequest);
            }
            if entry.lease.is_some() { return self.live_usage(request.employee); }
            candidate.entry_mut(request.employee)?.phase = Phase::Provisioning;
        } else {
            candidate.employees.push(Employee { employee: request.employee, uid: request.uid,
                quota, provision: operation, destroy: None, phase: Phase::Provisioning, lease: None });
        }
        self.publish_candidate(candidate)?;
        // This temporary mount can never be handed to a runtime. A crash leaves
        // Provisioning. The same operation can retry a completed H image;
        // incomplete/quarantined H images require explicit destroy/recovery.
        match self.image(request.employee) {
            Ok(image) if image.mounted_device.is_some() => self.provider.detach(request.employee).map_err(unavailable)?,
            Ok(_) | Err(ErrorCode::NotFound) => {},
            Err(error) => return Err(error),
        }
        self.provider.provision(request.employee, request.uid, quota).map_err(unavailable)?;
        self.provider.attach(request.employee, true).map_err(unavailable)?;
        let usage = self.live_usage(request.employee);
        self.provider.detach(request.employee).map_err(unavailable)?;
        let usage = usage?;
        self.journal.entry_mut(request.employee)?.phase = Phase::Ready;
        self.save()?;
        Ok(usage)
    }
    fn attach(&mut self, employee: EmployeeId, uid: u32, run: RunHandle, quota: &DiskQuota) -> Result<AttachedStorage> {
        self.readiness()?;
        let expected = Quota::try_from(quota).map_err(unavailable)?;
        let entry = self.journal.entry(employee)?;
        if entry.phase != Phase::Ready || entry.uid != uid || entry.quota != expected || entry.lease.is_some() {
            return Err(ErrorCode::InvalidRequest);
        }
        let mut token = [0u8; 16];
        // Raw syscall: glibc's `getrandom` wrapper is above the release baseline.
        if duduclaw_core::linux_syscalls::getrandom(&mut token, 0).ok() != Some(token.len()) {
            return Err(ErrorCode::IsolationUnavailable);
        }
        let mut candidate = self.journal.clone();
        candidate.entry_mut(employee)?.lease = Some(Lease { run: run.0, token: RequestId(token) });
        self.publish_candidate(candidate)?;
        let anchor = self.provider.attach(employee, true).map_err(unavailable)?;
        if !anchor.noexec { return Err(ErrorCode::IsolationUnavailable); }
        self.live_usage(employee)?.check(employee, quota)?;
        let image = self.image(employee)?;
        let device = image.mounted_device.ok_or(ErrorCode::IsolationUnavailable)?;
        let (home, work) = mounts(&anchor.path, uid, device)?;
        let receipt = Attachment { employee, run, token: RequestId(token), device_major: device.major, device_minor: device.minor };
        receipt.check(employee, run)?;
        Ok(AttachedStorage { receipt, home, work })
    }
    fn detach(&mut self, attachment: &Attachment) -> Result<()> {
        self.healthy()?;
        let entry = self.journal.entry(attachment.employee)?;
        let Some(lease) = &entry.lease else { return Ok(()); };
        if lease.run != attachment.run.0 || lease.token != attachment.token { return Err(ErrorCode::InvalidRequest); }
        let image = self.image(attachment.employee)?;
        if image.mounted_device.is_some_and(|device| device.major != attachment.device_major || device.minor != attachment.device_minor) {
            return Err(ErrorCode::IsolationUnavailable);
        }
        self.provider.detach(attachment.employee).map_err(unavailable)?;
        self.journal.entry_mut(attachment.employee)?.lease = None;
        self.save()
    }
    fn destroy(&mut self, employee: EmployeeId, operation: RequestId, proposal_digest: Sha256Digest) -> Result<()> {
        let images = self.inventory()?;
        // A confirmed allocation can fail before H receives provision. Only a
        // complete provider inventory plus an absent B1 lease/journal proves
        // this compensation is a no-op; unreadable/unknown images still fail.
        if !self.journal.employees.iter().any(|entry| entry.employee == employee) {
            return if images.iter().all(|image| image.employee != employee) { Ok(()) }
                else { Err(ErrorCode::IsolationUnavailable) };
        }
        let requested = Operation { id: operation, digest: proposal_digest };
        let mut candidate = self.journal.clone();
        let entry = candidate.entry_mut(employee)?;
        if entry.lease.is_some() || entry.destroy.as_ref().is_some_and(|op| *op != requested) {
            return Err(ErrorCode::InvalidRequest);
        }
        if entry.phase == Phase::Destroyed { return Ok(()); }
        entry.destroy = Some(requested); entry.phase = Phase::Destroying;
        self.publish_candidate(candidate)?;
        if let Some(image) = self.inventory()?.into_iter().find(|image| image.employee == employee) {
            if image.mounted_device.is_some() { self.provider.detach(employee).map_err(unavailable)?; }
            self.provider.destroy(employee).map_err(unavailable)?;
        }
        if self.inventory()?.iter().any(|image| image.employee == employee) { return Err(ErrorCode::IsolationUnavailable); }
        self.journal.entry_mut(employee)?.phase = Phase::Destroyed;
        self.save()
    }
    fn usage(&mut self, employee: EmployeeId) -> Result<StorageUsage> {
        self.inventory()?;
        let entry = self.journal.entry(employee)?;
        if entry.phase != Phase::Ready { return Err(ErrorCode::IsolationUnavailable); }
        if entry.lease.is_some() { return self.live_usage(employee); }
        // B1 checks usage before attach. H requires a real mounted statvfs,
        // so reuse the durably fenced maintenance path, never cached usage.
        let request = ProvisionRequest { employee, uid: entry.uid, operation_id: entry.provision.id,
            proposal_digest: entry.provision.digest,
            quota: DiskQuota::LoopFilesystem { bytes: entry.quota.bytes, inodes: entry.quota.inodes } };
        self.provision(&request)
    }
    fn traces(&mut self) -> Result<Vec<EmployeeId>> {
        let images = self.inventory()?;
        let mut employees: Vec<EmployeeId> = images.iter().map(|image| image.employee).collect();
        employees.extend(self.journal.employees.iter().filter(|entry| entry.phase != Phase::Destroyed).map(|entry| entry.employee));
        employees.sort_unstable_by_key(|employee| employee.0);
        employees.dedup();
        Ok(employees)
    }
    fn attachments(&mut self) -> Result<Vec<Attachment>> {
        let images = self.inventory()?;
        self.journal.employees.iter().filter_map(|entry| entry.lease.as_ref().map(|lease| (entry, lease)))
            .map(|(entry, lease)| {
                let image = images.iter().find(|image| image.employee == entry.employee).ok_or(ErrorCode::IsolationUnavailable)?;
                // An intent can precede the mount. Zero means no device was
                // observed; this receipt is cleanup-only and cannot pass launch.
                let device = image.mounted_device.unwrap_or(duduclaw_isolation_storage::device::DeviceId { major: 0, minor: 0 });
                Ok(Attachment { employee: entry.employee, run: RunHandle(lease.run), token: lease.token,
                    device_major: device.major, device_minor: device.minor })
            }).collect()
    }
}

fn check_file(file: &File) -> Result<()> {
    let meta = file.metadata().map_err(unavailable)?;
    if !meta.is_file() || meta.uid() != 0 || meta.gid() != 0 || meta.mode() & 0o7777 != 0o600 || meta.nlink() != 1 {
        return Err(ErrorCode::IsolationUnavailable);
    }
    Ok(())
}

/// H returns only a verified root mount anchor. Resolve home/work beneath its
/// descriptor, never through employee symlinks, then clone private/noexec trees.
fn mounts(path: &Path, uid: u32, device: duduclaw_isolation_storage::device::DeviceId) -> Result<(OwnedFd, OwnedFd)> {
    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(unavailable)?;
    let raw = unsafe { libc::open(name.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
    if raw < 0 { return Err(ErrorCode::IsolationUnavailable); }
    let root = unsafe { OwnedFd::from_raw_fd(raw) };
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(root.as_raw_fd(), &mut stat) } != 0 || stat.st_uid != 0
        || stat.st_mode & 0o022 != 0 || libc::major(stat.st_dev) != device.major || libc::minor(stat.st_dev) != device.minor {
        return Err(ErrorCode::IsolationUnavailable);
    }
    let clone = |name: &std::ffi::CStr| -> Result<OwnedFd> {
        #[repr(C)] struct OpenHow { flags: u64, mode: u64, resolve: u64 }
        let how = OpenHow { flags: (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW) as u64,
            mode: 0, resolve: 0x08 | 0x04 | 0x02 | 0x01 };
        let raw = unsafe { libc::syscall(libc::SYS_openat2, root.as_raw_fd(), name.as_ptr(), &how, std::mem::size_of::<OpenHow>()) };
        if raw < 0 { return Err(ErrorCode::IsolationUnavailable); }
        let child = unsafe { OwnedFd::from_raw_fd(raw as i32) };
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(child.as_raw_fd(), &mut stat) } != 0 || stat.st_uid != uid || stat.st_gid != uid
            || stat.st_mode & 0o7777 != 0o700 || libc::major(stat.st_dev) != device.major || libc::minor(stat.st_dev) != device.minor {
            return Err(ErrorCode::IsolationUnavailable);
        }
        let raw = unsafe { libc::syscall(libc::SYS_open_tree, child.as_raw_fd(), c"".as_ptr(),
            1u32 | libc::O_CLOEXEC as u32 | 0x8000u32 | libc::AT_EMPTY_PATH as u32) };
        if raw < 0 { return Err(ErrorCode::IsolationUnavailable); }
        let mount = unsafe { OwnedFd::from_raw_fd(raw as i32) };
        #[repr(C)] struct Attr { set: u64, clear: u64, propagation: u64, userns: u64 }
        for attr in [Attr { set: 0, clear: 0, propagation: libc::MS_PRIVATE, userns: 0 },
            Attr { set: 2 | 4 | 8, clear: 0, propagation: 0, userns: 0 }] {
            if unsafe { libc::syscall(libc::SYS_mount_setattr, mount.as_raw_fd(), c"".as_ptr(),
                libc::AT_EMPTY_PATH as u32 | 0x8000u32, &attr, std::mem::size_of::<Attr>()) } != 0 {
                return Err(ErrorCode::IsolationUnavailable);
            }
        }
        Ok(mount)
    };
    Ok((clone(c"home")?, clone(c"work")?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::DirBuilderExt;
    use duduclaw_isolation::ids::RunId;

    #[test]
    #[ignore = "root disposable Linux VM, private mount tree, ext4/XFS backing, loop devices; set ISO_STORAGE_TEST_ROOT"]
    fn iso_root_storage_adapter_reopen_generation_and_proposal_binding() {
        assert_eq!(unsafe { libc::geteuid() }, 0);
        let base = PathBuf::from(std::env::var_os("ISO_STORAGE_TEST_ROOT").expect("set isolated ISO_STORAGE_TEST_ROOT"));
        let root = base.join(format!("b1-h-adapter-{}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let images = root.join("images"); let journal = root.join("journal");
        for path in [&images, &journal] { fs::DirBuilder::new().mode(0o700).create(path).unwrap(); }
        let mut storage = LoopStorage::open(&images, &journal).unwrap();
        let employee = EmployeeId([0x72; 16]);
        storage.destroy(employee, RequestId([0x70; 16]), Sha256Digest([0x71; 32])).unwrap();
        assert!(storage.attachments().unwrap().is_empty());
        let request = ProvisionRequest { employee, uid: 70_000, operation_id: RequestId([0x73; 16]),
            proposal_digest: Sha256Digest([0x74; 32]), quota: DiskQuota::LoopFilesystem { bytes: 128 * 1024 * 1024, inodes: 1024 } };
        storage.provision(&request).unwrap().check(employee, &request.quota).unwrap();
        storage.provision(&request).unwrap();
        let mut duplicate = request.clone(); duplicate.employee = EmployeeId([0x7c; 16]);
        duplicate.uid += 1;
        assert_eq!(storage.provision(&duplicate), Err(ErrorCode::InvalidRequest));
        duplicate.operation_id = RequestId([0x7d; 16]); duplicate.uid = request.uid;
        assert_eq!(storage.provision(&duplicate), Err(ErrorCode::InvalidRequest));
        assert_eq!(storage.destroy(employee, request.operation_id, request.proposal_digest), Err(ErrorCode::InvalidRequest));
        storage.readiness().unwrap(); storage.usage(employee).unwrap();
        let mut changed = request.clone(); changed.proposal_digest = Sha256Digest([0x75; 32]);
        assert_eq!(storage.provision(&changed), Err(ErrorCode::InvalidRequest));
        let attached = storage.attach(employee, request.uid, RunHandle(RunId([0x76; 16])), &request.quota).unwrap();
        let first = attached.receipt.clone();
        assert_eq!(storage.attachments().unwrap(), vec![first.clone()]);
        assert!(storage.attach(employee, request.uid, RunHandle(RunId([0x77; 16])), &request.quota).is_err());
        drop(attached); drop(storage);
        let mut storage = LoopStorage::open(&images, &journal).unwrap();
        assert_eq!(storage.attachments().unwrap(), vec![first.clone()]);
        storage.detach(&first).unwrap(); storage.detach(&first).unwrap();
        let attached = storage.attach(employee, request.uid, RunHandle(RunId([0x78; 16])), &request.quota).unwrap();
        assert_eq!(storage.detach(&first), Err(ErrorCode::InvalidRequest));
        let second = attached.receipt.clone(); drop(attached);
        storage.detach(&second).unwrap();
        let delete = RequestId([0x79; 16]); let digest = Sha256Digest([0x7a; 32]);
        storage.destroy(employee, delete, digest).unwrap();
        storage.destroy(employee, delete, digest).unwrap();
        assert_eq!(storage.destroy(employee, delete, Sha256Digest([0x7b; 32])), Err(ErrorCode::InvalidRequest));
        assert!(storage.provision(&request).is_err());
        storage.readiness().unwrap(); drop(storage);
        for name in ["storage.lock", "state.json"] { fs::remove_file(images.join(name)).unwrap(); }
        for name in ["storage-adapter.lock", "storage-adapter.json"] { fs::remove_file(journal.join(name)).unwrap(); }
        fs::remove_dir(images).unwrap(); fs::remove_dir(journal).unwrap(); fs::remove_dir(root).unwrap();
    }
}
