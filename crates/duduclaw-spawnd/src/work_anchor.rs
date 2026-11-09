//! H3 (Package H, D-H1): per-employee root-owned work anchor directory.
//!
//! The anchor's `(st_dev, st_ino)` is the stable `employee-work` resource
//! identity answered by the `WorkAnchor` op. It lives on spawnd's own state
//! filesystem (`/var/lib/duduclaw-spawnd/work-anchors/<employee hex>`), never
//! inside the employee's loop image, so it exists with or without a live run
//! and its identity does not change across attach/detach or spawnd restarts.
//!
//! * Created at allocation: `AnchoredStorage::provision` (the storage adapter
//!   hook the allocation worker already calls) creates it after the image is
//!   provisioned; a provision replay keeps the existing anchor (same inode).
//! * Never writable by the employee: owned by root (`AnchorOwner::ROOT`), mode
//!   exactly 0500, on the same device as the 0700 root-owned base directory
//!   (so a mount over it is refused). Nothing is ever created inside it.
//! * Removed on retire: `AnchoredStorage::destroy` removes it after the image
//!   is destroyed; absent is success, so a Disable retry is idempotent.
//! * Read path: `WorkAnchors::read` / `Storage::work_anchor` re-checks the base
//!   and the anchor on every call and never creates or repairs anything.
//!
//! Every lookup is descriptor-relative (`openat`/`mkdirat`/`unlinkat` on the
//! held base fd, `O_NOFOLLOW`), so a symlink or rename planted in the path is
//! refused rather than followed. Any mismatch fails closed.
// `st_dev`/`st_mode` widths differ between Linux and macOS (fixture builds).
#![allow(clippy::unnecessary_cast)]
use std::{ffi::{CStr, CString}, os::{fd::{AsRawFd, FromRawFd, OwnedFd}, unix::ffi::OsStrExt}, path::Path};
use duduclaw_isolation::{execution::{DiskQuota, RunHandle}, ids::{EmployeeId, RequestId, Sha256Digest}, ipc::{ErrorCode, WorkAnchorIdentity}};
use crate::{Result, storage::{AttachedStorage, Attachment, ProvisionRequest, Storage, StorageUsage}};

/// Fixed root layout, next to the registry (`REGISTRY_DIRECTORY`).
pub const WORK_ANCHOR_DIRECTORY: &str = "/var/lib/duduclaw-spawnd/work-anchors";
/// Base directory: owner only.
pub const BASE_MODE: u32 = 0o700;
/// Anchor: owner read/search only, no write bit for anyone.
pub const ANCHOR_MODE: u32 = 0o500;

fn unavailable() -> ErrorCode { ErrorCode::IsolationUnavailable }
fn errno() -> i32 { std::io::Error::last_os_error().raw_os_error().unwrap_or(0) }

/// Who owns the base and every anchor. Production is always root; fixtures use
/// the test account so the non-root suite can exercise the same code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnchorOwner { pub uid: u32, pub gid: u32 }
impl AnchorOwner { pub const ROOT: Self = Self { uid: 0, gid: 0 }; }

/// The per-employee anchor set under one held base directory descriptor.
pub struct WorkAnchors { base: OwnedFd, owner: AnchorOwner, device: u64, inode: u64 }

impl WorkAnchors {
    /// Installed layout: requires euid 0, a root-safe `REGISTRY_DIRECTORY`
    /// (every ancestor root-owned, not group/other writable), and creates the
    /// 0700 base below it on first use (never through a symlink).
    #[cfg(target_os = "linux")]
    pub fn open_installed() -> Result<Self> {
        const BASE_NAME: &CStr = c"work-anchors";
        if unsafe { libc::geteuid() } != 0 { return Err(unavailable()); }
        let parent = Path::new(crate::registry::REGISTRY_DIRECTORY);
        crate::registry::root_path(parent, true).map_err(|_| unavailable())?;
        debug_assert_eq!(Path::new(WORK_ANCHOR_DIRECTORY).parent(), Some(parent));
        let parent = open_directory(parent)?;
        let parent_stat = fstat(&parent)?;
        if parent_stat.st_uid != 0 || parent_stat.st_mode as u32 & 0o022 != 0 { return Err(unavailable()); }
        if unsafe { libc::mkdirat(parent.as_raw_fd(), BASE_NAME.as_ptr(), BASE_MODE as libc::mode_t) } == 0 {
            let base = open_child(&parent, BASE_NAME)?;
            set_owner_mode(&base, AnchorOwner::ROOT, BASE_MODE)?;
        } else if errno() != libc::EEXIST {
            return Err(unavailable());
        }
        Self::from_descriptor(open_child(&parent, BASE_NAME)?, AnchorOwner::ROOT)
    }

    /// Open an existing base directory, which must be a real directory (not a
    /// symlink) owned by `owner` with mode exactly 0700.
    pub fn open_at(base: &Path, owner: AnchorOwner) -> Result<Self> {
        Self::from_descriptor(open_directory(base)?, owner)
    }

    fn from_descriptor(base: OwnedFd, owner: AnchorOwner) -> Result<Self> {
        let stat = fstat(&base)?;
        check_base(&stat, owner)?;
        Ok(Self { base, owner, device: stat.st_dev as u64, inode: stat.st_ino as u64 })
    }

    fn checked_base(&self) -> Result<()> {
        let stat = fstat(&self.base)?;
        check_base(&stat, self.owner)?;
        if stat.st_dev as u64 != self.device || stat.st_ino as u64 != self.inode { return Err(unavailable()); }
        Ok(())
    }

    /// Allocation hook: create the anchor if absent, then return its identity.
    /// An existing anchor is accepted only if it already passes every check
    /// (no repair), so a provision replay returns the same identity.
    pub fn ensure(&self, employee: EmployeeId) -> Result<WorkAnchorIdentity> {
        self.checked_base()?;
        let name = anchor_name(employee)?;
        if unsafe { libc::mkdirat(self.base.as_raw_fd(), name.as_ptr(), ANCHOR_MODE as libc::mode_t) } == 0 {
            // mkdirat honours the umask and the creator's egid; pin both on
            // the descriptor before anything can observe the identity.
            let anchor = open_child(&self.base, &name)?;
            set_owner_mode(&anchor, self.owner, ANCHOR_MODE)?;
        } else if errno() != libc::EEXIST {
            return Err(unavailable());
        }
        self.read(employee)
    }

    /// Read path for `WorkAnchor`: never creates. Absent ⇒ `NotFound`.
    pub fn read(&self, employee: EmployeeId) -> Result<WorkAnchorIdentity> {
        self.checked_base()?;
        let name = anchor_name(employee)?;
        let raw = unsafe { libc::openat(self.base.as_raw_fd(), name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
        if raw < 0 {
            return Err(if errno() == libc::ENOENT { ErrorCode::NotFound } else { unavailable() });
        }
        let anchor = unsafe { OwnedFd::from_raw_fd(raw) };
        let stat = fstat(&anchor)?;
        check_anchor(&stat, self.owner, self.device)?;
        let identity = WorkAnchorIdentity { device: stat.st_dev as u64, inode: stat.st_ino as u64 };
        identity.validate().map_err(|_| unavailable())?;
        Ok(identity)
    }

    /// Retire hook: remove the (always empty) anchor. Absent ⇒ success. A
    /// non-directory, foreign-owned, mounted-over or non-empty entry is never
    /// followed or deleted: it fails closed and the retire stays incomplete.
    pub fn remove(&self, employee: EmployeeId) -> Result<()> {
        self.checked_base()?;
        let name = anchor_name(employee)?;
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstatat(self.base.as_raw_fd(), name.as_ptr(), &mut stat, libc::AT_SYMLINK_NOFOLLOW) } != 0 {
            return if errno() == libc::ENOENT { Ok(()) } else { Err(unavailable()) };
        }
        if stat.st_mode as u32 & libc::S_IFMT as u32 != libc::S_IFDIR as u32 || stat.st_uid != self.owner.uid
            || stat.st_dev as u64 != self.device {
            return Err(unavailable());
        }
        if unsafe { libc::unlinkat(self.base.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } != 0 && errno() != libc::ENOENT {
            return Err(unavailable());
        }
        Ok(())
    }
}

/// Storage adapter hook: wraps the employee storage so allocation (provision)
/// creates the anchor and retire (destroy) removes it. Attach/detach/usage are
/// forwarded unchanged; the anchor is not part of any run.
pub struct AnchoredStorage { inner: Box<dyn Storage>, anchors: WorkAnchors }
impl AnchoredStorage {
    pub fn new(inner: Box<dyn Storage>, anchors: WorkAnchors) -> Self { Self { inner, anchors } }
}
impl Storage for AnchoredStorage {
    fn readiness(&mut self) -> Result<()> {
        self.anchors.checked_base()?;
        self.inner.readiness()
    }
    fn provision(&mut self, request: &ProvisionRequest) -> Result<StorageUsage> {
        let usage = self.inner.provision(request)?;
        self.anchors.ensure(request.employee)?;
        Ok(usage)
    }
    fn attach(&mut self, employee: EmployeeId, uid: u32, run: RunHandle, quota: &DiskQuota) -> Result<AttachedStorage> {
        self.inner.attach(employee, uid, run, quota)
    }
    fn detach(&mut self, attachment: &Attachment) -> Result<()> { self.inner.detach(attachment) }
    fn destroy(&mut self, employee: EmployeeId, operation: RequestId, proposal_digest: Sha256Digest) -> Result<()> {
        // Image first: a failed destroy keeps the anchor (still allocated).
        self.inner.destroy(employee, operation, proposal_digest)?;
        self.anchors.remove(employee)
    }
    fn usage(&mut self, employee: EmployeeId) -> Result<StorageUsage> { self.inner.usage(employee) }
    fn attachments(&mut self) -> Result<Vec<Attachment>> { self.inner.attachments() }
    fn traces(&mut self) -> Result<Vec<EmployeeId>> { self.inner.traces() }
    fn work_anchor(&mut self, employee: EmployeeId) -> Result<WorkAnchorIdentity> { self.anchors.read(employee) }
    fn backfill_work_anchor(&mut self, employee: EmployeeId) -> Result<bool> {
        match self.anchors.read(employee) {
            Ok(_) => Ok(false),
            // Only an absent anchor is created; a present-but-wrong one stays
            // refused (no repair), exactly like the allocation replay path.
            Err(ErrorCode::NotFound) => self.anchors.ensure(employee).map(|_| true),
            Err(error) => Err(error),
        }
    }
}

/// Per-employee result of `backfill`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backfilled { Present, Created, Failed(ErrorCode) }

/// H4 upgrade step (H3 open item 1), run once at spawnd start for every
/// allocation that is Ready in both the allocation journal and the registry:
/// create each missing anchor. Idempotent (a second pass reports `Present`
/// with the same identities) and never repairs: a tampered anchor stays
/// `Failed`, so `WorkAnchor` keeps answering unavailable for it. The caller
/// audits `Created` and `Failed`.
pub fn backfill(storage: &mut dyn Storage, employees: &[EmployeeId]) -> Vec<(EmployeeId, Backfilled)> {
    employees.iter().map(|employee| (*employee, match storage.backfill_work_anchor(*employee) {
        Ok(true) => Backfilled::Created,
        Ok(false) => Backfilled::Present,
        Err(code) => Backfilled::Failed(code),
    })).collect()
}

/// Lowercase hex of the never-recycled 128-bit EmployeeId; the all-zero id is
/// never allocated and is refused.
fn anchor_name(employee: EmployeeId) -> Result<CString> {
    if employee.0 == [0; 16] { return Err(ErrorCode::InvalidRequest); }
    let name: String = employee.0.iter().map(|byte| format!("{byte:02x}")).collect();
    CString::new(name).map_err(|_| ErrorCode::InvalidRequest)
}
fn open_directory(path: &Path) -> Result<OwnedFd> {
    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| unavailable())?;
    let raw = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
    if raw < 0 { return Err(unavailable()); }
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}
fn open_child(parent: &OwnedFd, name: &CStr) -> Result<OwnedFd> {
    let raw = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(),
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
    if raw < 0 { return Err(unavailable()); }
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}
fn fstat(fd: &OwnedFd) -> Result<libc::stat> {
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd.as_raw_fd(), &mut stat) } != 0 { return Err(unavailable()); }
    Ok(stat)
}
fn set_owner_mode(fd: &OwnedFd, owner: AnchorOwner, mode: u32) -> Result<()> {
    if unsafe { libc::fchown(fd.as_raw_fd(), owner.uid, owner.gid) } != 0
        || unsafe { libc::fchmod(fd.as_raw_fd(), mode as libc::mode_t) } != 0 {
        return Err(unavailable());
    }
    Ok(())
}
fn is_directory(stat: &libc::stat) -> bool { stat.st_mode as u32 & libc::S_IFMT as u32 == libc::S_IFDIR as u32 }
fn check_base(stat: &libc::stat, owner: AnchorOwner) -> Result<()> {
    if !is_directory(stat) || stat.st_uid != owner.uid || stat.st_gid != owner.gid || stat.st_mode as u32 & 0o7777 != BASE_MODE {
        return Err(unavailable());
    }
    Ok(())
}
fn check_anchor(stat: &libc::stat, owner: AnchorOwner, base_device: u64) -> Result<()> {
    if !is_directory(stat) || stat.st_uid != owner.uid || stat.st_gid != owner.gid
        || stat.st_mode as u32 & 0o7777 != ANCHOR_MODE || stat.st_dev as u64 != base_device {
        return Err(unavailable());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::{MetadataExt, PermissionsExt}};
    use duduclaw_isolation::ids::RunId;

    /// In-memory storage: provision/destroy/attach bookkeeping only.
    #[derive(Default)]
    struct Fake { provisioned: Vec<EmployeeId>, destroyed: Vec<EmployeeId>, leases: Vec<Attachment>, fail_destroy: bool }
    impl Storage for Fake {
        fn readiness(&mut self) -> Result<()> { Ok(()) }
        fn provision(&mut self, r: &ProvisionRequest) -> Result<StorageUsage> {
            if self.destroyed.contains(&r.employee) { return Err(ErrorCode::InvalidRequest); }
            if !self.provisioned.contains(&r.employee) { self.provisioned.push(r.employee); }
            self.usage(r.employee)
        }
        fn attach(&mut self, employee: EmployeeId, _: u32, run: RunHandle, _: &DiskQuota) -> Result<AttachedStorage> {
            let receipt = Attachment { employee, run, token: RequestId([9; 16]), device_major: 7, device_minor: 1 };
            self.leases.push(receipt.clone());
            Ok(AttachedStorage { receipt,
                #[cfg(target_os = "linux")] home: fs::File::open("/").unwrap().into(),
                #[cfg(target_os = "linux")] work: fs::File::open("/").unwrap().into() })
        }
        fn detach(&mut self, a: &Attachment) -> Result<()> { self.leases.retain(|l| l != a); Ok(()) }
        fn destroy(&mut self, employee: EmployeeId, _: RequestId, _: Sha256Digest) -> Result<()> {
            if self.fail_destroy || self.leases.iter().any(|l| l.employee == employee) { return Err(unavailable()); }
            self.provisioned.retain(|e| *e != employee);
            if !self.destroyed.contains(&employee) { self.destroyed.push(employee); }
            Ok(())
        }
        fn usage(&mut self, employee: EmployeeId) -> Result<StorageUsage> {
            Ok(StorageUsage { employee, bytes: 0, inodes: 0, limit_bytes: 1024, limit_inodes: 32,
                hard_quota_verified: true, device_graph_verified: true, host_reserve_verified: true })
        }
        fn attachments(&mut self) -> Result<Vec<Attachment>> { Ok(self.leases.clone()) }
    }

    const EMPLOYEE_UID: u32 = 70_001;
    fn quota() -> DiskQuota { DiskQuota::LoopFilesystem { bytes: 1024, inodes: 32 } }
    fn request(employee: EmployeeId) -> ProvisionRequest {
        ProvisionRequest { employee, uid: EMPLOYEE_UID, operation_id: RequestId([3; 16]), proposal_digest: Sha256Digest([4; 32]), quota: quota() }
    }
    /// A 0700 base owned by the test account (tempdir creates 0700).
    fn fixture() -> (tempfile::TempDir, std::path::PathBuf, AnchorOwner) {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("work-anchors");
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(BASE_MODE)).unwrap();
        let meta = fs::metadata(&base).unwrap();
        (dir, base, AnchorOwner { uid: meta.uid(), gid: meta.gid() })
    }
    fn storage(base: &Path, owner: AnchorOwner, fake: Fake) -> AnchoredStorage {
        AnchoredStorage::new(Box::new(fake), WorkAnchors::open_at(base, owner).unwrap())
    }
    fn anchor_path(base: &Path, employee: EmployeeId) -> std::path::PathBuf {
        base.join(anchor_name(employee).unwrap().to_str().unwrap())
    }

    #[test]
    fn work_anchor_identity_stable_across_runs() {
        let (_dir, base, owner) = fixture();
        let employee = EmployeeId([0x41; 16]);
        let mut storage = storage(&base, owner, Fake::default());
        // Not allocated yet: the read path never creates.
        assert_eq!(storage.work_anchor(employee), Err(ErrorCode::NotFound));
        assert!(!anchor_path(&base, employee).exists());
        storage.provision(&request(employee)).unwrap();
        let identity = storage.work_anchor(employee).unwrap();
        let meta = fs::symlink_metadata(anchor_path(&base, employee)).unwrap();
        assert_eq!((identity.device, identity.inode), (meta.dev(), meta.ino()));
        // Runs come and go; the anchor is not part of any run.
        for byte in [0x51, 0x52, 0x53] {
            let attached = storage.attach(employee, EMPLOYEE_UID, RunHandle(RunId([byte; 16])), &quota()).unwrap();
            assert_eq!(storage.work_anchor(employee).unwrap(), identity);
            storage.detach(&attached.receipt).unwrap();
            assert_eq!(storage.work_anchor(employee).unwrap(), identity);
        }
        // A provision replay (allocation retry) keeps the same inode.
        storage.provision(&request(employee)).unwrap();
        assert_eq!(storage.work_anchor(employee).unwrap(), identity);
        // A spawnd restart reopens the same anchor.
        drop(storage);
        let mut reopened = self::storage(&base, owner, Fake::default());
        assert_eq!(reopened.work_anchor(employee).unwrap(), identity);
        // Another employee has its own identity.
        let other = EmployeeId([0x42; 16]);
        reopened.provision(&request(other)).unwrap();
        assert_ne!(reopened.work_anchor(other).unwrap(), identity);
        // The never-allocated all-zero id is refused, not looked up.
        assert_eq!(reopened.work_anchor(EmployeeId([0; 16])), Err(ErrorCode::InvalidRequest));
        // Plain storage without the hook has no anchor read path.
        assert_eq!(Fake::default().work_anchor(employee), Err(ErrorCode::WorkspaceBrokerUnavailable));
    }

    #[test]
    fn work_anchor_not_writable_by_employee() {
        let (_dir, base, owner) = fixture();
        let employee = EmployeeId([0x43; 16]);
        let mut storage = storage(&base, owner, Fake::default());
        storage.provision(&request(employee)).unwrap();
        let path = anchor_path(&base, employee);
        let meta = fs::symlink_metadata(&path).unwrap();
        assert!(meta.is_dir());
        assert_eq!(meta.mode() & 0o7777, ANCHOR_MODE, "no write bit for anyone");
        assert_eq!((meta.uid(), meta.gid()), (owner.uid, owner.gid));
        assert_ne!(meta.uid(), EMPLOYEE_UID, "the employee never owns its anchor");
        if unsafe { libc::geteuid() } != 0 {
            // DAC applies to the non-root owner too: nothing can be created inside.
            let error = fs::File::create(path.join("planted")).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        }
        // A writable anchor (tampered mode) is refused on read and on replay.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(storage.work_anchor(employee), Err(ErrorCode::IsolationUnavailable));
        assert_eq!(storage.provision(&request(employee)).map(|_| ()), Err(ErrorCode::IsolationUnavailable));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(storage.work_anchor(employee), Err(ErrorCode::IsolationUnavailable));
        fs::set_permissions(&path, fs::Permissions::from_mode(ANCHOR_MODE)).unwrap();
        assert!(storage.work_anchor(employee).is_ok());
        // A symlink planted under an employee's name is never followed.
        let victim = EmployeeId([0x44; 16]);
        let target = base.parent().unwrap().join("elsewhere");
        fs::create_dir(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(ANCHOR_MODE)).unwrap();
        std::os::unix::fs::symlink(&target, anchor_path(&base, victim)).unwrap();
        assert_eq!(storage.work_anchor(victim), Err(ErrorCode::IsolationUnavailable));
        assert_eq!(storage.provision(&request(victim)).map(|_| ()), Err(ErrorCode::IsolationUnavailable));
        assert_eq!(storage.destroy(victim, RequestId([5; 16]), Sha256Digest([6; 32])), Err(ErrorCode::IsolationUnavailable));
        assert!(target.is_dir() && fs::symlink_metadata(anchor_path(&base, victim)).unwrap().file_type().is_symlink());
        // A base that became group/other writable fails every operation.
        fs::set_permissions(&base, fs::Permissions::from_mode(0o770)).unwrap();
        assert_eq!(storage.work_anchor(employee), Err(ErrorCode::IsolationUnavailable));
        assert_eq!(storage.readiness(), Err(ErrorCode::IsolationUnavailable));
        assert!(WorkAnchors::open_at(&base, owner).is_err());
        fs::set_permissions(&base, fs::Permissions::from_mode(BASE_MODE)).unwrap();
        // Wrong owner expectation (e.g. the employee's uid) is refused.
        assert!(WorkAnchors::open_at(&base, AnchorOwner { uid: EMPLOYEE_UID, gid: owner.gid }).is_err());
        // A symlinked base is refused.
        let link = base.parent().unwrap().join("anchors-link");
        std::os::unix::fs::symlink(&base, &link).unwrap();
        assert!(WorkAnchors::open_at(&link, owner).is_err());
    }

    #[test]
    fn work_anchor_removed_on_retire() {
        let (_dir, base, owner) = fixture();
        let employee = EmployeeId([0x45; 16]);
        let operation = RequestId([5; 16]); let digest = Sha256Digest([6; 32]);
        let mut storage = storage(&base, owner, Fake::default());
        storage.provision(&request(employee)).unwrap();
        storage.work_anchor(employee).unwrap();
        // A refused image destroy (active lease) keeps the anchor.
        let attached = storage.attach(employee, EMPLOYEE_UID, RunHandle(RunId([0x61; 16])), &quota()).unwrap();
        assert!(storage.destroy(employee, operation, digest).is_err());
        assert!(storage.work_anchor(employee).is_ok());
        storage.detach(&attached.receipt).unwrap();
        // Retire removes it; a Disable retry is idempotent.
        storage.destroy(employee, operation, digest).unwrap();
        assert_eq!(storage.work_anchor(employee), Err(ErrorCode::NotFound));
        assert!(fs::symlink_metadata(anchor_path(&base, employee)).is_err());
        storage.destroy(employee, operation, digest).unwrap();
        // A retired employee never gets an anchor back (the image refuses).
        assert!(storage.provision(&request(employee)).is_err());
        assert_eq!(storage.work_anchor(employee), Err(ErrorCode::NotFound));
        // A non-empty anchor is not removed: the retire stays incomplete.
        let other = EmployeeId([0x46; 16]);
        storage.provision(&request(other)).unwrap();
        let path = anchor_path(&base, other);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(path.join("planted"), b"x").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(ANCHOR_MODE)).unwrap();
        assert_eq!(storage.destroy(other, operation, digest), Err(ErrorCode::IsolationUnavailable));
        assert!(path.is_dir());
        // An inner destroy failure leaves the anchor in place.
        let mut failing = self::storage(&base, owner, Fake { fail_destroy: true, ..Fake::default() });
        let third = EmployeeId([0x47; 16]);
        failing.provision(&request(third)).unwrap();
        assert!(failing.destroy(third, operation, digest).is_err());
        assert!(failing.work_anchor(third).is_ok());
        // Destroy of a never-allocated employee is a no-op success.
        storage.destroy(EmployeeId([0x48; 16]), operation, digest).unwrap();
    }

    #[test]
    fn anchor_backfill_idempotent() {
        let (_dir, base, owner) = fixture();
        let legacy = EmployeeId([0x50; 16]);
        let current = EmployeeId([0x51; 16]);
        // `legacy` was allocated before H3: its image exists, its anchor does not.
        let mut inner = Fake::default();
        inner.provision(&request(legacy)).unwrap();
        let mut storage = storage(&base, owner, inner);
        storage.provision(&request(current)).unwrap();
        let current_identity = storage.work_anchor(current).unwrap();
        assert_eq!(storage.work_anchor(legacy), Err(ErrorCode::NotFound));
        assert_eq!(backfill(&mut storage, &[legacy, current]),
            vec![(legacy, Backfilled::Created), (current, Backfilled::Present)]);
        let legacy_identity = storage.work_anchor(legacy).unwrap();
        let meta = fs::symlink_metadata(anchor_path(&base, legacy)).unwrap();
        assert_eq!((meta.uid(), meta.gid(), meta.mode() & 0o7777), (owner.uid, owner.gid, ANCHOR_MODE));
        // Second pass and a restart: nothing created, identities unchanged.
        assert_eq!(backfill(&mut storage, &[legacy, current]),
            vec![(legacy, Backfilled::Present), (current, Backfilled::Present)]);
        assert_eq!(storage.work_anchor(legacy).unwrap(), legacy_identity);
        assert_eq!(storage.work_anchor(current).unwrap(), current_identity);
        drop(storage);
        let mut reopened = self::storage(&base, owner, Fake::default());
        assert_eq!(backfill(&mut reopened, &[legacy]), vec![(legacy, Backfilled::Present)]);
        assert_eq!(reopened.work_anchor(legacy).unwrap(), legacy_identity);
        // Never a repair: a tampered (writable) anchor stays refused.
        fs::set_permissions(anchor_path(&base, legacy), fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(backfill(&mut reopened, &[legacy]), vec![(legacy, Backfilled::Failed(ErrorCode::IsolationUnavailable))]);
        assert_eq!(fs::symlink_metadata(anchor_path(&base, legacy)).unwrap().mode() & 0o7777, 0o700);
        // A planted symlink is never followed or replaced.
        let victim = EmployeeId([0x52; 16]);
        std::os::unix::fs::symlink(base.parent().unwrap(), anchor_path(&base, victim)).unwrap();
        assert_eq!(backfill(&mut reopened, &[victim]), vec![(victim, Backfilled::Failed(ErrorCode::IsolationUnavailable))]);
        assert!(fs::symlink_metadata(anchor_path(&base, victim)).unwrap().file_type().is_symlink());
        // Storage without the anchor hook cannot backfill: fail closed.
        assert_eq!(backfill(&mut Fake::default(), &[legacy]),
            vec![(legacy, Backfilled::Failed(ErrorCode::WorkspaceBrokerUnavailable))]);
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[ignore = "root Linux host: creates a 0700 root directory under /run and setuid()s a forked child to an unused uid"]
    fn iso_root_work_anchor_not_writable_by_employee() {
        assert_eq!(unsafe { libc::geteuid() }, 0);
        let base = std::path::PathBuf::from(format!("/run/iso-work-anchor-test-{}", std::process::id()));
        fs::create_dir(&base).unwrap();
        fs::set_permissions(&base, fs::Permissions::from_mode(BASE_MODE)).unwrap();
        let mut storage = storage(&base, AnchorOwner::ROOT, Fake::default());
        let employee = EmployeeId([0x49; 16]);
        storage.provision(&request(employee)).unwrap();
        let identity = storage.work_anchor(employee).unwrap();
        let path = anchor_path(&base, employee);
        let meta = fs::symlink_metadata(&path).unwrap();
        assert_eq!((meta.uid(), meta.gid(), meta.mode() & 0o7777), (0, 0, ANCHOR_MODE));
        // As the employee uid: no create, no chmod, no remove — both by path
        // (blocked already by the 0700 root base) and through a descriptor of
        // the anchor itself (so the anchor's own 0500 root ownership is what
        // refuses, not the base's search permission).
        let anchor = CString::new(path.as_os_str().as_bytes()).unwrap();
        let planted = CString::new(path.join("planted").as_os_str().as_bytes()).unwrap();
        let held = open_directory(&path).unwrap();
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            unsafe {
                let fd = held.as_raw_fd();
                if libc::setgroups(0, std::ptr::null()) != 0 || libc::setgid(EMPLOYEE_UID) != 0
                    || libc::setuid(EMPLOYEE_UID) != 0 { libc::_exit(10); }
                if libc::open(planted.as_ptr(), libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL, 0o600) >= 0 { libc::_exit(11); }
                if libc::openat(fd, c"planted".as_ptr(), libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL, 0o600) >= 0 { libc::_exit(12); }
                if libc::mkdirat(fd, c"planted".as_ptr(), 0o700) == 0 { libc::_exit(13); }
                if libc::fchmod(fd, 0o777) == 0 || libc::chmod(anchor.as_ptr(), 0o777) == 0 { libc::_exit(14); }
                if libc::fchown(fd, EMPLOYEE_UID, EMPLOYEE_UID) == 0 { libc::_exit(15); }
                if libc::rmdir(anchor.as_ptr()) == 0 { libc::_exit(16); }
                libc::_exit(0);
            }
        }
        drop(held);
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
        assert!(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0, "child status {status}");
        assert_eq!(storage.work_anchor(employee).unwrap(), identity);
        storage.destroy(employee, RequestId([5; 16]), Sha256Digest([6; 32])).unwrap();
        assert_eq!(storage.work_anchor(employee), Err(ErrorCode::NotFound));
        drop(storage);
        fs::remove_dir(&base).unwrap();
    }
}
