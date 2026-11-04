//! Linux syscalls are confined here. The lifetime lock serializes accounting and
//! lifecycle operations across processes. Never lazy-unmount employee storage.
use std::{collections::BTreeSet, fs::{self, File}, io::{Read, Seek, SeekFrom, Write},
    os::{fd::{AsRawFd, OwnedFd}, unix::{fs::MetadataExt, process::CommandExt}},
    path::{Component, Path, PathBuf}, process::{Command, Stdio}, time::{Duration, Instant}};
use nix::{libc, mount::{mount, umount, MsFlags}};
use rustix::fs::{self as rfs, AtFlags, FallocateFlags, FlockOperation, Mode, OFlags, ResolveFlags};
use duduclaw_isolation::ids::EmployeeId;
use crate::{accounting::Capacity, device::{self, BackingGraph, DeviceId, DeviceKind, DeviceNode},
    employee_key, metadata::{Image, Phase, Registry, MAX_IMAGES, MAX_METADATA_BYTES},
    Error, MountAnchor, Quota, Result, Usage};

const RESOLVE: ResolveFlags = ResolveFlags::BENEATH.union(ResolveFlags::NO_SYMLINKS)
    .union(ResolveFlags::NO_MAGICLINKS).union(ResolveFlags::NO_XDEV);
const FILE_MODE: Mode = Mode::from_raw_mode(0o600);
const DIR_MODE: Mode = Mode::from_raw_mode(0o700);

// linux/uapi/linux/loop.h. ioctl_read_bad/write_ptr_bad encode legacy ioctl
// numbers verbatim, unlike _IOR/_IOW. No pointer or process-local fd is persisted.
#[repr(C)]
struct LoopInfo {
    device: u64, inode: u64, rdevice: u64, offset: u64, size_limit: u64,
    number: u32, encrypt_type: u32, encrypt_key_size: u32, flags: u32,
    file_name: [u8; 64], crypt_name: [u8; 64], encrypt_key: [u8; 32], init: [u64; 2],
}
impl Default for LoopInfo {
    fn default() -> Self {
        Self { device: 0, inode: 0, rdevice: 0, offset: 0, size_limit: 0,
            number: 0, encrypt_type: 0, encrypt_key_size: 0, flags: 0,
            file_name: [0; 64], crypt_name: [0; 64], encrypt_key: [0; 32], init: [0; 2] }
    }
}
#[repr(C)]
struct LoopConfig { fd: u32, block_size: u32, info: LoopInfo, reserved: [u64; 8] }
nix::ioctl_read_bad!(loop_status, 0x4c05, LoopInfo);
nix::ioctl_write_ptr_bad!(loop_configure, 0x4c0a, LoopConfig);
nix::ioctl_none_bad!(loop_free, 0x4c82);
nix::ioctl_none_bad!(loop_set_capacity, 0x4c07);
// fs/ext4/ext4.h: _IOW('f', 16, __u64), stable since before Linux 5.15.
nix::ioctl_write_ptr!(ext4_resize, b'f', 16, u64);
const AUTO_CLEAR: u32 = 4;

pub struct Storage {
    path: PathBuf,
    root: File,
    _lock: File,
    state: Registry,
    poisoned: bool,
}
impl Storage {
    /// Caller creates the root:root 0700 storage directory during installation.
    /// B1 must make its mount tree private before this call.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        if !nix::unistd::geteuid().is_root() { return Err(Error::Unsupported); }
        let path = path.as_ref().to_path_buf();
        let root = trusted_directory(&path)?;
        let lock = open_file(&root, "storage.lock", OFlags::RDWR | OFlags::CREATE)?;
        check_regular(&lock, None)?;
        rfs::flock(&lock, FlockOperation::NonBlockingLockExclusive).map_err(|_| Error::Busy)?;
        let graph = backing_graph(&root)?;
        require_private_mount(&path)?;
        let state = match open_file(&root, "state.json", OFlags::RDONLY) {
            Ok(file) => {
                check_regular(&file, None)?;
                Registry::decode(&read_bounded(file, MAX_METADATA_BYTES)?)?
            }
            Err(Error::NotFound) => {
                // Never reconstruct an empty registry around unknown images.
                for entry in fs::read_dir(&path).map_err(|_| Error::Io)? {
                    if entry.map_err(|_| Error::Io)?.file_name() != "storage.lock" { return Err(Error::RecoveryRequired); }
                }
                Registry { version: 1, backing: graph.clone(), images: vec![] }
            }
            Err(error) => return Err(error),
        };
        if state.backing != graph { return Err(Error::UnsafeBacking); }
        let mut storage = Self { path, root, _lock: lock, state, poisoned: false };
        storage.inventory()?;
        storage.persist()?;
        Ok(storage)
    }

    pub fn readiness(&self) -> Result<()> {
        self.healthy()?;
        if backing_graph(&self.root)? != self.state.backing { return Err(Error::UnsafeBacking); }
        require_private_mount(&self.path)?;
        self.inventory()?;
        self.capacity()?.admit(self.state.charge(), 0)?;
        for image in &self.state.images {
            match image.phase {
                Phase::Ready => { self.image_file(image)?; },
                Phase::Quarantined => return Err(Error::Quarantined),
                _ => return Err(Error::RecoveryRequired),
            }
        }
        Ok(())
    }

    /// UID is B1's root-registry mapping, never a gateway request field.
    pub fn provision(&mut self, employee: EmployeeId, uid: u32, quota: Quota) -> Result<()> {
        self.healthy()?;
        let quota = quota.validate()?;
        if let Ok(existing) = self.state.image(employee) {
            return if existing.uid == uid && existing.quota == quota && existing.phase == Phase::Ready {
                self.image_file(existing).map(|_| ())
            } else { Err(Error::RecoveryRequired) };
        }
        self.readiness()?;
        if self.state.images.len() == MAX_IMAGES || self.state.images.iter().any(|image| image.uid == uid)
            || !(duduclaw_isolation::ids::EMPLOYEE_UID_MIN..=duduclaw_isolation::ids::EMPLOYEE_UID_MAX).contains(&uid) { return Err(Error::Invalid); }
        self.capacity()?.admit(self.state.charge(), quota.bytes)?;
        self.state.images.push(Image { employee, uid, quota, phase: Phase::Creating, inode: None });
        self.persist()?; // Capacity reservation is durable BEFORE creating a file.
        let key = employee_key(employee);
        let image = open_file(&self.root, &format!("{key}.img"), OFlags::RDWR | OFlags::CREATE | OFlags::EXCL)?;
        check_regular(&image, None)?;
        rfs::flock(&image, FlockOperation::NonBlockingLockExclusive).map_err(|_| Error::Busy)?;
        rfs::fallocate(&image, FallocateFlags::empty(), 0, quota.bytes).map_err(|_| Error::UnsafeBacking)?;
        check_regular(&image, Some(quota.bytes))?;
        image.sync_all().map_err(|_| Error::Io)?;
        // A trusted helper sees only a fixed fd path and fixed argv. No shell,
        // config inheritance, caller-selected features or lazy initialization.
        run_helper("/usr/sbin/mkfs.ext4", &[
            "-q".into(), "-F".into(), "-b".into(), "4096".into(), "-m".into(), "0".into(),
            "-N".into(), quota.inodes.to_string(), "-E".into(),
            "lazy_itable_init=0,lazy_journal_init=0,nodiscard".into(), "/proc/self/fd/3".into(),
        ], Some(&image), Duration::from_secs(120))?;
        image.sync_all().map_err(|_| Error::Io)?;
        check_regular(&image, Some(quota.bytes))?;
        check_ext4(&image, quota)?;
        self.root.sync_all().map_err(|_| Error::Io)?;
        let entry = self.state.images.iter_mut().find(|image| image.employee == employee).ok_or(Error::NotFound)?;
        entry.inode = Some(image.metadata().map_err(|_| Error::Io)?.ino());
        entry.phase = Phase::Ready;
        self.persist()
    }

    pub fn attach(&mut self, employee: EmployeeId, noexec: bool) -> Result<MountAnchor> {
        self.readiness()?;
        let entry = self.state.image(employee)?.clone();
        let image = self.image_file(&entry)?;
        rfs::flock(&image, FlockOperation::NonBlockingLockExclusive).map_err(|_| Error::Busy)?;
        let target = self.mount_path(employee);
        if let Some(mounted) = mounted_at(&target)? {
            self.verify_mount(&entry, &image, &mounted, noexec)?;
            return Ok(MountAnchor { path: target, noexec });
        }
        if let Err(error) = check_ext4(&image, entry.quota) {
            self.quarantine(employee)?;
            return Err(error);
        }
        let name = format!("{}.mnt", employee_key(employee));
        match rfs::mkdirat(&self.root, name.as_str(), DIR_MODE) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => {},
            Err(_) => return Err(Error::Io),
        }
        let anchor = open_file(&self.root, &name, OFlags::RDONLY | OFlags::DIRECTORY)?;
        check_directory(&anchor, true)?;
        if fs::read_dir(&target).map_err(|_| Error::Io)?.next().is_some() { return Err(Error::RecoveryRequired); }
        if !matching_loops(&image)?.is_empty() { return Err(Error::Busy); }
        let control = open_device("/dev/loop-control", false, None)?;
        // SAFETY: loop-control is checked as a root-owned character device.
        let number = unsafe { loop_free(control.as_raw_fd()) }.map_err(|_| Error::Io)?;
        if number < 0 { return Err(Error::Io); }
        let loop_path = PathBuf::from(format!("/dev/loop{number}"));
        let loop_fd = open_device(&loop_path, true, Some(DeviceId { major: 7, minor: number as u32 }))?;
        let config = LoopConfig { fd: image.as_raw_fd() as u32, block_size: 4096,
            info: LoopInfo { flags: AUTO_CLEAR, ..LoopInfo::default() }, reserved: [0; 8] };
        // SAFETY: kernel copies a correctly laid-out initialized loop_config.
        unsafe { loop_configure(loop_fd.as_raw_fd(), &config) }.map_err(|_| Error::Busy)?;
        let mut flags = MsFlags::MS_NOSUID | MsFlags::MS_NODEV;
        if noexec { flags |= MsFlags::MS_NOEXEC; }
        if mount(Some(&loop_path), &target, Some("ext4"), flags, Some("errors=remount-ro")).is_err() {
            self.quarantine(employee)?;
            return Err(Error::Quarantined); // AUTOCLEAR releases on last close.
        }
        let readback = (|| {
            let mounted = mounted_at(&target)?.ok_or(Error::RecoveryRequired)?;
            self.verify_mount(&entry, &image, &mounted, noexec)?;
            let root = open_file(&self.root, &name, OFlags::RDONLY | OFlags::DIRECTORY)
                .or_else(|_| open_mount_directory(&target))?;
            for child in ["home", "work"] {
                match rfs::mkdirat(&root, child, DIR_MODE) {
                    Ok(()) => {
                        let dir = open_file(&root, child, OFlags::RDONLY | OFlags::DIRECTORY)?;
                        rfs::fchown(&dir, Some(rustix::process::Uid::from_raw(entry.uid)),
                            Some(rustix::process::Gid::from_raw(entry.uid))).map_err(|_| Error::Io)?;
                        rfs::fchmod(&dir, DIR_MODE).map_err(|_| Error::Io)?;
                    },
                    Err(rustix::io::Errno::EXIST) => {
                        let dir = open_file(&root, child, OFlags::RDONLY | OFlags::DIRECTORY)?;
                        let meta = dir.metadata().map_err(|_| Error::Io)?;
                        if meta.uid() != entry.uid || meta.gid() != entry.uid || meta.mode() & 0o7777 != 0o700 {
                            return Err(Error::RecoveryRequired);
                        }
                    },
                    Err(_) => return Err(Error::Io),
                }
            }
            root.sync_all().map_err(|_| Error::Io)?;
            Ok(())
        })();
        if let Err(error) = readback {
            // Never hide a failed unmount with a lazy detach. Keep metadata
            // quarantined even when a mount remains busy for operator recovery.
            let _ = umount(&target);
            self.quarantine(employee)?;
            return Err(error);
        }
        Ok(MountAnchor { path: target, noexec })
    }

    /// B1 must first revoke run capabilities and all detached mount clones.
    pub fn detach(&mut self, employee: EmployeeId) -> Result<()> {
        self.healthy()?;
        let entry = self.state.image(employee)?.clone();
        let image = self.open_image_identity(&entry)?;
        rfs::flock(&image, FlockOperation::NonBlockingLockExclusive).map_err(|_| Error::Busy)?;
        let target = self.mount_path(employee);
        if let Some(mounted) = mounted_at(&target)? {
            self.verify_mount_identity(&image, &mounted)?;
            umount(&target).map_err(|_| Error::Busy)?;
        }
        // AUTOCLEAR is asynchronous. Active detached mounts keep the device
        // configured, which is an explicit refusal to release capacity/delete.
        if mounted_at(&target)?.is_some() || !matching_loops(&image)?.is_empty() { return Err(Error::Busy); }
        Ok(())
    }

    pub fn destroy(&mut self, employee: EmployeeId) -> Result<()> {
        self.healthy()?;
        let entry = self.state.image(employee)?.clone();
        let key = employee_key(employee);
        if mounted_at(&self.mount_path(employee))?.is_some() { return Err(Error::Busy); }
        let file = match open_file(&self.root, &format!("{key}.img"), OFlags::RDWR) {
            Ok(file) => {
                check_regular(&file, None)?;
                if entry.inode.is_some_and(|ino| file.metadata().map(|m| m.ino() != ino).unwrap_or(true)) {
                    return Err(Error::RecoveryRequired);
                }
                rfs::flock(&file, FlockOperation::NonBlockingLockExclusive).map_err(|_| Error::Busy)?;
                if !matching_loops(&file)?.is_empty() { return Err(Error::Busy); }
                Some(file)
            },
            Err(Error::NotFound) if matches!(entry.phase, Phase::Creating | Phase::Deleting) => None,
            Err(error) => return Err(error),
        };
        self.state.images.iter_mut().find(|image| image.employee == employee).ok_or(Error::NotFound)?.phase = Phase::Deleting;
        self.persist()?;
        if file.is_some() { rfs::unlinkat(&self.root, format!("{key}.img"), AtFlags::empty()).map_err(|_| Error::Io)?; }
        match rfs::unlinkat(&self.root, format!("{key}.mnt"), AtFlags::REMOVEDIR) {
            Ok(()) | Err(rustix::io::Errno::NOENT) => {},
            Err(_) => return Err(Error::RecoveryRequired),
        }
        self.root.sync_all().map_err(|_| Error::Io)?;
        self.state.images.retain(|image| image.employee != employee);
        self.persist()
    }

    /// B1 drains the employee and releases every detached mount clone first.
    /// Existing host mounts are refused. Only kernel online growth is used;
    /// no root resize2fs/fsck parses employee-controlled filesystem structures.
    pub fn resize(&mut self, employee: EmployeeId, quota: Quota) -> Result<()> {
        self.resize_steps(employee, quota, |_| Ok(()))
    }
    fn resize_steps(&mut self, employee: EmployeeId, quota: Quota,
        mut checkpoint: impl FnMut(ResizeStep) -> Result<()>) -> Result<()> {
        self.readiness()?;
        let entry = self.state.image(employee)?.clone();
        let delta = crate::growth_delta(entry.quota, quota)?;
        if delta == 0 { self.image_file(&entry)?; return Ok(()); }
        let image = self.image_file(&entry)?;
        let target = self.mount_path(employee);
        if mounted_at(&target)?.is_some() || !matching_loops(&image)?.is_empty() { return Err(Error::Busy); }
        self.capacity()?.admit(self.state.charge(), delta)?;
        // Attach the OLD verified size before publishing the target reservation.
        // B1 cannot hand out clones during this root-local maintenance operation.
        self.attach(employee, true)?;
        rfs::flock(&image, FlockOperation::NonBlockingLockExclusive).map_err(|_| Error::Busy)?;
        let mounted = mounted_at(&target)?.ok_or(Error::RecoveryRequired)?;
        self.verify_mount(&entry, &image, &mounted, true)?;
        let dir = open_mount_directory(&target)?;
        let loop_fd = open_device(format!("/dev/loop{}", mounted.device.minor), true, Some(mounted.device))?;
        let pending = self.state.images.iter_mut().find(|image| image.employee == employee).ok_or(Error::NotFound)?;
        pending.quota = quota; pending.phase = Phase::Resizing;
        self.persist()?; // Charge the FULL target before allocation or kernel grow.
        let result = (|| {
            checkpoint(ResizeStep::Reserved)?;
            rfs::fallocate(&image, FallocateFlags::empty(), 0, quota.bytes).map_err(|_| Error::UnsafeBacking)?;
            check_regular(&image, Some(quota.bytes))?;
            image.sync_all().map_err(|_| Error::Io)?;
            checkpoint(ResizeStep::Allocated)?;
            // SAFETY: verified loop device and ext4 root; kernel copies a u64.
            unsafe { loop_set_capacity(loop_fd.as_raw_fd()) }.map_err(|_| Error::Io)?;
            let blocks = quota.bytes / 4096;
            unsafe { ext4_resize(dir.as_raw_fd(), &blocks) }.map_err(|_| Error::Io)?;
            rfs::syncfs(&dir).map_err(|_| Error::Io)?;
            checkpoint(ResizeStep::Grown)?;
            let mut grown = entry.clone(); grown.quota = quota;
            let mounted = mounted_at(&target)?.ok_or(Error::RecoveryRequired)?;
            self.verify_mount(&grown, &image, &mounted, true)?;
            check_ext4(&image, quota)?;
            Ok(())
        })();
        // Never roll back the file length or reservation after a partial grow.
        // Resizing survives process death; errors quarantine the charged target.
        if let Err(error) = result { self.quarantine(employee)?; return Err(error); }
        drop(dir); drop(loop_fd); drop(image);
        self.detach(employee)?;
        self.state.images.iter_mut().find(|image| image.employee == employee).ok_or(Error::NotFound)?.phase = Phase::Ready;
        self.persist()
    }
    pub fn usage(&self, employee: EmployeeId) -> Result<Usage> {
        self.healthy()?;
        let entry = self.state.image(employee)?;
        let image = self.image_file(entry)?;
        let target = self.mount_path(employee);
        let mounted = mounted_at(&target)?.ok_or(Error::NotFound)?;
        self.verify_mount(entry, &image, &mounted, mounted.noexec)?;
        let dir = open_mount_directory(&target)?;
        let stat = rfs::fstatvfs(&dir).map_err(|_| Error::Io)?;
        let capacity_bytes = stat.f_blocks.checked_mul(stat.f_frsize).ok_or(Error::Io)?;
        let free = stat.f_bfree.checked_mul(stat.f_frsize).ok_or(Error::Io)?;
        Ok(Usage { capacity_bytes, used_bytes: capacity_bytes.checked_sub(free).ok_or(Error::Io)?,
            capacity_inodes: stat.f_files, used_inodes: stat.f_files.checked_sub(stat.f_ffree).ok_or(Error::Io)? })
    }

    fn healthy(&self) -> Result<()> { if self.poisoned { Err(Error::RecoveryRequired) } else { Ok(()) } }
    fn mount_path(&self, employee: EmployeeId) -> PathBuf { self.path.join(format!("{}.mnt", employee_key(employee))) }
    fn capacity(&self) -> Result<Capacity> {
        let stat = rfs::fstatvfs(&self.root).map_err(|_| Error::Io)?;
        Ok(Capacity { total: stat.f_blocks.checked_mul(stat.f_frsize).ok_or(Error::Capacity)?,
            available: stat.f_bavail.checked_mul(stat.f_frsize).ok_or(Error::Capacity)? })
    }
    fn image_file(&self, image: &Image) -> Result<File> {
        if image.phase == Phase::Quarantined { return Err(Error::Quarantined); }
        if image.phase != Phase::Ready { return Err(Error::RecoveryRequired); }
        let file = self.open_image_identity(image)?;
        check_regular(&file, Some(image.quota.bytes))?;
        Ok(file)
    }
    fn open_image_identity(&self, image: &Image) -> Result<File> {
        let file = open_file(&self.root, &format!("{}.img", employee_key(image.employee)), OFlags::RDWR)?;
        check_regular(&file, None)?;
        let meta = file.metadata().map_err(|_| Error::Io)?;
        if image.inode.is_none_or(|ino| ino != meta.ino()) || meta.dev() != self.root.metadata().map_err(|_| Error::Io)?.dev() {
            return Err(Error::RecoveryRequired);
        }
        Ok(file)
    }
    fn quarantine(&mut self, employee: EmployeeId) -> Result<()> {
        self.state.images.iter_mut().find(|image| image.employee == employee).ok_or(Error::NotFound)?.phase = Phase::Quarantined;
        self.persist()
    }
    fn persist(&mut self) -> Result<()> {
        let result = atomic_state(&self.root, &self.state);
        if result.is_err() { self.poisoned = true; }
        result
    }
    fn inventory(&self) -> Result<()> {
        let mut allowed: BTreeSet<String> = ["storage.lock", "state.json", "state.new"].into_iter().map(str::to_owned).collect();
        for image in &self.state.images {
            let key = employee_key(image.employee);
            allowed.insert(format!("{key}.img")); allowed.insert(format!("{key}.mnt"));
        }
        for entry in fs::read_dir(&self.path).map_err(|_| Error::Io)? {
            let name = entry.map_err(|_| Error::Io)?.file_name().into_string().map_err(|_| Error::RecoveryRequired)?;
            if !allowed.contains(&name) { return Err(Error::RecoveryRequired); }
        }
        Ok(())
    }
    fn verify_mount_identity(&self, file: &File, mounted: &Mounted) -> Result<()> {
        let loops = matching_loops(file)?;
        if mounted.filesystem != "ext4" || loops.len() != 1 || loops[0] != mounted.device {
            return Err(Error::RecoveryRequired);
        }
        Ok(())
    }
    fn verify_mount(&self, image: &Image, file: &File, mounted: &Mounted, noexec: bool) -> Result<()> {
        if mounted.filesystem != "ext4" || mounted.noexec != noexec || !mounted.nosuid || !mounted.nodev || mounted.readonly {
            return Err(Error::RecoveryRequired);
        }
        self.verify_mount_identity(file, mounted)?;
        let dir = open_mount_directory(&self.mount_path(image.employee))?;
        let stat = rfs::fstatfs(&dir).map_err(|_| Error::Io)?;
        let vfs = rfs::fstatvfs(&dir).map_err(|_| Error::Io)?;
        let flags = vfs.f_flag;
        if stat.f_type as u64 != device::EXT4_MAGIC
            || !flags.contains(rfs::StatVfsMountFlags::NOSUID | rfs::StatVfsMountFlags::NODEV)
            || flags.contains(rfs::StatVfsMountFlags::NOEXEC) != noexec
            || flags.contains(rfs::StatVfsMountFlags::RDONLY)
            || vfs.f_files > image.quota.inodes
            || vfs.f_blocks.checked_mul(vfs.f_frsize).ok_or(Error::Io)? > image.quota.bytes {
            return Err(Error::RecoveryRequired);
        }
        Ok(())
    }
}

fn io_error(error: rustix::io::Errno) -> Error {
    if error == rustix::io::Errno::NOENT { Error::NotFound } else { Error::Io }
}
fn open_file(root: &File, name: &str, flags: OFlags) -> Result<File> {
    let fd = rfs::openat2(root, name, flags | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        if flags.contains(OFlags::CREATE) { FILE_MODE } else { Mode::empty() }, RESOLVE).map_err(io_error)?;
    Ok(File::from(fd))
}
fn check_regular(file: &File, size: Option<u64>) -> Result<()> {
    let meta = file.metadata().map_err(|_| Error::Io)?;
    if !meta.is_file() || meta.uid() != 0 || meta.gid() != 0 || meta.nlink() != 1 || meta.mode() & 0o7777 != 0o600 {
        return Err(Error::RecoveryRequired);
    }
    if let Some(size) = size {
        if meta.len() != size || meta.blocks().checked_mul(512).ok_or(Error::UnsafeBacking)? < size {
            return Err(Error::UnsafeBacking);
        }
    }
    Ok(())
}
fn check_directory(file: &File, private: bool) -> Result<()> {
    let meta = file.metadata().map_err(|_| Error::Io)?;
    if !meta.is_dir() || meta.uid() != 0 || meta.gid() != 0 || meta.mode() & 0o7022 != 0
        || (private && meta.mode() & 0o7777 != 0o700) { return Err(Error::RecoveryRequired); }
    Ok(())
}
fn trusted_directory(path: &Path) -> Result<File> {
    if !path.is_absolute() { return Err(Error::Invalid); }
    let mut root = File::open("/").map_err(|_| Error::Io)?;
    check_directory(&root, false)?;
    for component in path.components() {
        match component {
            Component::RootDir => {},
            Component::Normal(name) => {
                // Allow crossing trusted installation mountpoints, but no links.
                let fd = rfs::openat2(&root, name, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
                    Mode::empty(), ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS).map_err(io_error)?;
                root = File::from(fd); check_directory(&root, false)?;
            },
            _ => return Err(Error::Invalid),
        }
    }
    check_directory(&root, true)?;
    Ok(root)
}
fn open_mount_directory(path: &Path) -> Result<File> {
    let slash = File::open("/").map_err(|_| Error::Io)?;
    let relative = path.strip_prefix("/").map_err(|_| Error::Invalid)?;
    Ok(File::from(rfs::openat2(&slash, relative, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(), ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS).map_err(io_error)?))
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResizeStep { Reserved, Allocated, Grown }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublishStep { Written, FileSynced, Renamed, DirectorySynced }
fn atomic_state(root: &File, state: &Registry) -> Result<()> {
    atomic_state_steps(root, state, |_| Ok(()))
}
fn atomic_state_steps(root: &File, state: &Registry, mut checkpoint: impl FnMut(PublishStep) -> Result<()>) -> Result<()> {
    let bytes = state.encode()?;
    match open_file(root, "state.new", OFlags::RDONLY) {
        Ok(file) => { check_regular(&file, None)?; rfs::unlinkat(root, "state.new", AtFlags::empty()).map_err(|_| Error::Io)?; },
        Err(Error::NotFound) => {},
        Err(error) => return Err(error),
    }
    let mut temp = open_file(root, "state.new", OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL)?;
    check_regular(&temp, None)?;
    temp.write_all(&bytes).map_err(|_| Error::Io)?;
    checkpoint(PublishStep::Written)?;
    temp.sync_all().map_err(|_| Error::Io)?;
    checkpoint(PublishStep::FileSynced)?;
    rfs::renameat(root, "state.new", root, "state.json").map_err(|_| Error::Io)?;
    checkpoint(PublishStep::Renamed)?;
    root.sync_all().map_err(|_| Error::Io)?;
    checkpoint(PublishStep::DirectorySynced)
}
fn read_bounded(file: impl Read, maximum: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    file.take(maximum as u64 + 1).read_to_end(&mut out).map_err(|_| Error::Io)?;
    if out.len() > maximum { return Err(Error::RecoveryRequired); }
    Ok(out)
}

fn run_helper(executable: &str, args: &[String], image: Option<&File>, timeout: Duration) -> Result<Vec<u8>> {
    // Validate root-controlled executable and its ancestors (usrmerge symlinks
    // are resolved once; their root-owned parent prevents replacement by users).
    let executable = fs::canonicalize(executable).map_err(|_| Error::Unsupported)?;
    let parent = executable.parent().ok_or(Error::Unsupported)?;
    // Ancestors may be 0755, so validate independently of storage's 0700 rule.
    trusted_helper(&executable, parent)?;
    let mut command = Command::new(&executable);
    command.args(args).env_clear().env("PATH", "/usr/sbin:/usr/bin").env("LC_ALL", "C")
        .current_dir("/").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    if let Some(image) = image {
        let source = image.as_raw_fd();
        // SAFETY: only async-signal-safe fd syscalls execute between fork/exec.
        unsafe { command.pre_exec(move || {
            if source != 3 && libc::dup2(source, 3) < 0 { return Err(std::io::Error::last_os_error()); }
            if libc::fcntl(3, libc::F_SETFD, 0) < 0 { return Err(std::io::Error::last_os_error()); }
            Ok(())
        }); }
    }
    let mut child = command.spawn().map_err(|_| Error::Io)?;
    let stdout = child.stdout.take().ok_or(Error::Io)?;
    let reader = std::thread::spawn(move || read_bounded(stdout, 64 * 1024));
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(10)),
            _ => { let _ = child.kill(); let _ = child.wait(); break Err(Error::Io); },
        }
    };
    let out = reader.join().map_err(|_| Error::Io)??;
    if !status?.success() { return Err(Error::Io); }
    Ok(out)
}
fn trusted_helper(executable: &Path, parent: &Path) -> Result<()> {
    let mut cursor = PathBuf::from("/");
    for part in parent.components() {
        if let Component::Normal(name) = part { cursor.push(name); }
        let meta = fs::symlink_metadata(&cursor).map_err(|_| Error::Unsupported)?;
        if !meta.is_dir() || meta.uid() != 0 || meta.mode() & 0o022 != 0 { return Err(Error::Unsupported); }
    }
    let meta = fs::symlink_metadata(executable).map_err(|_| Error::Unsupported)?;
    if !meta.is_file() || meta.uid() != 0 || meta.mode() & 0o6022 != 0 || meta.mode() & 0o100 == 0 {
        return Err(Error::Unsupported);
    }
    Ok(())
}

fn open_device(path: impl AsRef<Path>, block: bool, expected: Option<DeviceId>) -> Result<File> {
    let dev = File::open("/dev").map_err(|_| Error::Io)?;
    let name = path.as_ref().strip_prefix("/dev").map_err(|_| Error::Invalid)?;
    let fd: OwnedFd = rfs::openat2(&dev, name, OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(), RESOLVE).map_err(io_error)?;
    let file = File::from(fd);
    let meta = file.metadata().map_err(|_| Error::Io)?;
    let kind = meta.mode() & libc::S_IFMT;
    if meta.uid() != 0 || kind != if block { libc::S_IFBLK } else { libc::S_IFCHR }
        || expected.is_some_and(|id| device_id(meta.rdev()) != id)
        || (!block && device_id(meta.rdev()) != (DeviceId { major: 10, minor: 237 })) {
        return Err(Error::RecoveryRequired);
    }
    Ok(file)
}
fn device_id(dev: u64) -> DeviceId {
    DeviceId { major: libc::major(dev), minor: libc::minor(dev) }
}
fn matching_loops(image: &File) -> Result<Vec<DeviceId>> {
    let meta = image.metadata().map_err(|_| Error::Io)?;
    let mut found = Vec::new();
    for entry in fs::read_dir("/sys/block").map_err(|_| Error::Io)? {
        let entry = entry.map_err(|_| Error::Io)?;
        let name = entry.file_name(); let name = name.to_str().ok_or(Error::UnsafeBacking)?;
        let Some(number) = name.strip_prefix("loop").filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) else { continue; };
        let number: u32 = number.parse().map_err(|_| Error::UnsafeBacking)?;
        let id = DeviceId { major: 7, minor: number };
        let fd = open_device(format!("/dev/loop{number}"), true, Some(id))?;
        let mut info = LoopInfo::default();
        // SAFETY: initialized loop_info64 is a writable kernel output buffer.
        match unsafe { loop_status(fd.as_raw_fd(), &mut info) } {
            Ok(_) if info.device == meta.dev() && info.inode == meta.ino() => {
                if info.offset != 0 || info.size_limit != 0 || info.flags & AUTO_CLEAR == 0 { return Err(Error::RecoveryRequired); }
                found.push(id);
            },
            Ok(_) | Err(nix::errno::Errno::ENXIO) => {},
            Err(_) => return Err(Error::Io),
        }
    }
    Ok(found)
}

fn check_ext4(image: &File, quota: Quota) -> Result<()> {
    // Bounded fixed-offset structural check; no fsck or plugin interpretation.
    let mut file = image.try_clone().map_err(|_| Error::Io)?;
    file.seek(SeekFrom::Start(1024)).map_err(|_| Error::Io)?;
    let mut bytes = [0u8; 1024]; file.read_exact(&mut bytes).map_err(|_| Error::Io)?;
    crate::ext4::validate_superblock(&bytes, quota)
}

#[derive(Debug)]
struct Mounted { device: DeviceId, filesystem: String, noexec: bool, nosuid: bool, nodev: bool, readonly: bool }
fn decode_mount_path(value: &str) -> Result<PathBuf> {
    let mut bytes = Vec::new(); let mut source = value.bytes();
    while let Some(byte) = source.next() {
        if byte == b'\\' {
            let octal: Vec<_> = source.by_ref().take(3).collect();
            let decoded = match octal.as_slice() {
                b"040" => b' ', b"011" => b'\t', b"012" => b'\n', b"134" => b'\\',
                _ => return Err(Error::RecoveryRequired),
            };
            bytes.push(decoded);
        } else { bytes.push(byte); }
    }
    use std::os::unix::ffi::OsStringExt;
    Ok(std::ffi::OsString::from_vec(bytes).into())
}
fn mount_lines() -> Result<String> {
    String::from_utf8(read_bounded(File::open("/proc/self/mountinfo").map_err(|_| Error::Io)?, 4 * 1024 * 1024)?)
        .map_err(|_| Error::RecoveryRequired)
}
fn parse_device(value: &str) -> Result<DeviceId> {
    let (major, minor) = value.split_once(':').ok_or(Error::UnsafeBacking)?;
    Ok(DeviceId { major: major.parse().map_err(|_| Error::UnsafeBacking)?, minor: minor.parse().map_err(|_| Error::UnsafeBacking)? })
}
fn mounted_at(path: &Path) -> Result<Option<Mounted>> {
    let mut found = None;
    for line in mount_lines()?.lines() {
        let (left, right) = line.split_once(" - ").ok_or(Error::RecoveryRequired)?;
        let columns: Vec<_> = left.split_ascii_whitespace().collect();
        if columns.len() < 6 { return Err(Error::RecoveryRequired); }
        if decode_mount_path(columns[4])? != path { continue; }
        if found.is_some() { return Err(Error::RecoveryRequired); }
        let flags: BTreeSet<_> = columns[5].split(',').collect();
        let right: Vec<_> = right.split_ascii_whitespace().collect();
        if right.len() != 3 || columns[3] != "/" || !right[2].split(',').any(|v| v == "errors=remount-ro") {
            return Err(Error::RecoveryRequired);
        }
        found = Some(Mounted { device: parse_device(columns[2])?, filesystem: right[0].into(),
            noexec: flags.contains("noexec"), nosuid: flags.contains("nosuid"), nodev: flags.contains("nodev"), readonly: flags.contains("ro") });
    }
    Ok(found)
}
fn require_private_mount(path: &Path) -> Result<()> {
    let mut selected = None;
    for line in mount_lines()?.lines() {
        let (left, _) = line.split_once(" - ").ok_or(Error::RecoveryRequired)?;
        let columns: Vec<_> = left.split_ascii_whitespace().collect();
        if columns.len() < 6 { return Err(Error::RecoveryRequired); }
        let mountpoint = decode_mount_path(columns[4])?;
        if path.starts_with(&mountpoint) && selected.as_ref().is_none_or(|(len, _)| mountpoint.components().count() >= *len) {
            let shared = columns.iter().skip(6).any(|value| value.split_once(':').is_some_and(|(key, _)| matches!(key, "shared" | "master")));
            selected = Some((mountpoint.components().count(), shared));
        }
    }
    if selected.is_none_or(|(_, shared)| shared) { return Err(Error::UnsafeBacking); }
    Ok(())
}

fn backing_graph(root: &File) -> Result<BackingGraph> {
    let stat = rfs::fstatfs(root).map_err(|_| Error::Io)?;
    if !matches!(stat.f_type as u64, device::EXT4_MAGIC | device::XFS_MAGIC) { return Err(Error::UnsafeBacking); }
    let id = device_id(root.metadata().map_err(|_| Error::Io)?.dev());
    let mut graph = BackingGraph { filesystem: stat.f_type as u64, root: id, nodes: vec![] };
    collect_device(id, &mut graph, &mut BTreeSet::new(), 0)?;
    graph.canonicalize(); graph.validate()?; Ok(graph)
}
fn read_sys(path: &Path) -> Result<String> {
    String::from_utf8(read_bounded(File::open(path).map_err(|_| Error::UnsafeBacking)?, 4096)?)
        .map_err(|_| Error::UnsafeBacking)
}
fn digits(value: &str) -> bool { !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) }
fn physical_name(name: &str) -> bool {
    if let Some(rest) = name.strip_prefix("nvme") {
        return rest.split_once('n').is_some_and(|(controller, namespace)| digits(controller) && digits(namespace));
    }
    ["sd", "vd", "xvd"].into_iter().any(|prefix| name.strip_prefix(prefix)
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_lowercase())))
}
fn collect_device(id: DeviceId, graph: &mut BackingGraph, active: &mut BTreeSet<DeviceId>, depth: usize) -> Result<()> {
    if depth > device::MAX_DEVICE_DEPTH || active.contains(&id) { return Err(Error::UnsafeBacking); }
    if graph.nodes.iter().any(|node| node.id == id) { return Ok(()); }
    if graph.nodes.len() >= device::MAX_DEVICES { return Err(Error::UnsafeBacking); }
    active.insert(id);
    let path = fs::canonicalize(format!("/sys/dev/block/{}:{}", id.major, id.minor)).map_err(|_| Error::UnsafeBacking)?;
    if !path.starts_with("/sys/devices") || parse_device(read_sys(&path.join("dev"))?.trim())? != id { return Err(Error::UnsafeBacking); }
    let mut dependencies = Vec::new();
    let mut has_slaves = false;
    if path.join("slaves").is_dir() {
        for entry in fs::read_dir(path.join("slaves")).map_err(|_| Error::UnsafeBacking)? {
            if dependencies.len() >= device::MAX_DEVICES { return Err(Error::UnsafeBacking); }
            let entry = entry.map_err(|_| Error::UnsafeBacking)?;
            dependencies.push(parse_device(read_sys(&entry.path().join("dev"))?.trim())?); has_slaves = true;
        }
    }
    let kind = if path.join("partition").exists() {
        if has_slaves { return Err(Error::UnsafeBacking); }
        dependencies.push(parse_device(read_sys(&path.parent().ok_or(Error::UnsafeBacking)?.join("dev"))?.trim())?);
        DeviceKind::Partition
    } else if path.join("dm").is_dir() {
        let output = run_helper("/usr/sbin/dmsetup", &["table".into(), "--major".into(), id.major.to_string(),
            "--minor".into(), id.minor.to_string()], None, Duration::from_secs(5))?;
        let table = std::str::from_utf8(&output).map_err(|_| Error::UnsafeBacking)?;
        DeviceKind::DeviceMapper(device::parse_dm_targets(table)?)
    } else if path.join("md").is_dir() { DeviceKind::Md }
    else {
        let name = path.file_name().and_then(|name| name.to_str()).ok_or(Error::UnsafeBacking)?;
        if has_slaves || !physical_name(name) || !path.join("device").exists() { return Err(Error::UnsafeBacking); }
        DeviceKind::Physical
    };
    graph.nodes.push(DeviceNode { id, kind, dependencies: dependencies.clone() });
    for dependency in dependencies { collect_device(dependency, graph, active, depth + 1)?; }
    active.remove(&id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::DirBuilderExt;

    #[test]
    fn loop_uapi_layout_matches_kernel() {
        assert_eq!(std::mem::size_of::<LoopInfo>(), 232);
        assert_eq!(std::mem::size_of::<LoopConfig>(), 304);
        assert_eq!(std::mem::offset_of!(LoopConfig, info), 8);
    }
    #[test]
    fn sysfs_device_names_are_anchored() {
        for name in ["nvme0n1", "sda", "vda", "xvdaa"] { assert!(physical_name(name)); }
        for name in ["nvme", "nvme0n1p1", "sd", "sda/evil", "loop0", "zram0", "dm-0", "sda1"] {
            assert!(!physical_name(name));
        }
    }
    #[test]
    fn mount_path_decoding_and_device_parser_fail_closed() {
        assert_eq!(decode_mount_path(r"/space\040and\134slash").unwrap(), PathBuf::from("/space and\\slash"));
        assert!(decode_mount_path(r"/bad\000").is_err());
        assert!(decode_mount_path(r"/bad\04").is_err());
        assert!(parse_device("8:1:2").is_err());
        assert!(parse_device("8:1").is_ok());
    }

    fn test_path(name: &str) -> PathBuf {
        assert!(nix::unistd::geteuid().is_root(), "root required");
        let base = std::env::var_os("ISO_STORAGE_TEST_ROOT").expect("set isolated ISO_STORAGE_TEST_ROOT");
        let base = PathBuf::from(base);
        trusted_directory(&base).expect("root-owned private test directory");
        let path = base.join(format!("{name}-{}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&path).expect("fresh test directory");
        path
    }
    fn remove_test_state(path: &Path) {
        for name in ["state.new", "state.json", "storage.lock"] {
            match fs::remove_file(path.join(name)) {
                Ok(()) => {}, Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                Err(error) => panic!("test cleanup failed: {error}"),
            }
        }
        fs::remove_dir(path).expect("test directory has no leaked images or mounts");
    }

    #[test]
    #[ignore = "root, private mount tree, real ext4/XFS and loop devices required"]
    fn iso_root_provision_mount_usage_unmount_destroy() {
        let path = test_path("lifecycle");
        let mut storage = Storage::open(&path).unwrap();
        let employee = EmployeeId([1; 16]);
        let quota = Quota { bytes: 128 * 1024 * 1024, inodes: 1024 };
        storage.provision(employee, 70000, quota).unwrap();
        storage.provision(employee, 70000, quota).unwrap();
        assert_eq!(storage.resize(employee, Quota { bytes: quota.bytes / 2, ..quota }), Err(Error::ResizeUnsupported));
        let anchor = storage.attach(employee, true).unwrap();
        assert_eq!(storage.attach(employee, false).unwrap_err(), Error::RecoveryRequired);
        let usage = storage.usage(employee).unwrap();
        assert!(usage.capacity_bytes <= quota.bytes && usage.capacity_inodes <= quota.inodes);
        let home = fs::metadata(anchor.path.join("home")).unwrap();
        assert_eq!(home.uid(), 70000); assert_eq!(home.mode() & 0o777, 0o700);
        let image = storage.image_file(storage.state.image(employee).unwrap()).unwrap();
        assert_eq!(matching_loops(&image).unwrap().len(), 1);
        // Boundaries are actual kernel mount flags, not only intended policy.
        let mounted = mounted_at(&anchor.path).unwrap().unwrap();
        assert!(mounted.noexec && mounted.nosuid && mounted.nodev && !mounted.readonly);
        drop(image);
        // Exhaust real filesystem inodes. Host metadata must remain writable.
        let mut created = Vec::new();
        let mut inode_full = false;
        for number in 0..=quota.inodes {
            let file = anchor.path.join("work").join(format!("inode-{number}"));
            match File::create(&file) {
                Ok(_) => created.push(file),
                Err(error) => { assert_eq!(error.raw_os_error(), Some(libc::ENOSPC)); inode_full = true; break; },
            }
        }
        assert!(inode_full);
        storage.persist().unwrap();
        for file in created { fs::remove_file(file).unwrap(); }
        // Exhaust real data blocks; a fixed image cannot consume extra host space.
        let fill_path = anchor.path.join("work/fill");
        let mut fill = File::create(&fill_path).unwrap();
        let block = [0u8; 64 * 1024];
        let mut full = false;
        for _ in 0..=(quota.bytes / block.len() as u64) {
            match fill.write_all(&block) {
                Ok(()) => {},
                Err(error) => { assert_eq!(error.raw_os_error(), Some(libc::ENOSPC)); full = true; break; },
            }
        }
        assert!(full);
        drop(fill); fs::remove_file(fill_path).unwrap();
        storage.persist().unwrap();
        assert_eq!(storage.destroy(employee), Err(Error::Busy));
        // Recovery must unmount a verified readonly/quarantined filesystem.
        mount(None::<&str>, &anchor.path, None::<&str>, MsFlags::MS_REMOUNT | MsFlags::MS_RDONLY
            | MsFlags::MS_NOSUID | MsFlags::MS_NODEV | MsFlags::MS_NOEXEC, Some("errors=remount-ro")).unwrap();
        storage.quarantine(employee).unwrap();
        storage.detach(employee).unwrap();
        storage.destroy(employee).unwrap();
        storage.readiness().unwrap();
        drop(storage);
        remove_test_state(&path);
    }

    #[test]
    #[ignore = "root, private mount tree and real supported backing required"]
    fn iso_root_resize_grows_preserves_data_and_rejects_live_mount() {
        let path = test_path("grow");
        let mut storage = Storage::open(&path).unwrap();
        let employee = EmployeeId([5; 16]);
        let old = Quota { bytes: 64 * 1024 * 1024, inodes: 1024 };
        let target = Quota { bytes: 256 * 1024 * 1024, inodes: 4096 };
        storage.provision(employee, 70003, old).unwrap();
        let anchor = storage.attach(employee, true).unwrap();
        fs::write(anchor.path.join("work/retained"), b"retained across grow").unwrap();
        assert_eq!(storage.resize(employee, target), Err(Error::Busy));
        storage.detach(employee).unwrap();
        storage.resize(employee, target).unwrap();
        assert!(mounted_at(&anchor.path).unwrap().is_none());
        storage.resize(employee, target).unwrap();
        let anchor = storage.attach(employee, true).unwrap();
        assert_eq!(fs::read(anchor.path.join("work/retained")).unwrap(), b"retained across grow");
        let usage = storage.usage(employee).unwrap();
        assert!(usage.capacity_bytes > old.bytes && usage.capacity_bytes <= target.bytes);
        assert!(usage.capacity_inodes <= target.inodes);
        storage.detach(employee).unwrap();
        drop(storage);
        let mut storage = Storage::open(&path).unwrap();
        storage.readiness().unwrap();
        assert_eq!(storage.state.image(employee).unwrap().quota, target);
        storage.destroy(employee).unwrap();
        drop(storage); remove_test_state(&path);
    }

    #[test]
    #[ignore = "root required; interrupts real allocation/kernel growth at durable checkpoints"]
    fn iso_root_resize_interruption_keeps_full_charge_and_refuses_attach() {
        for stage in [ResizeStep::Reserved, ResizeStep::Allocated, ResizeStep::Grown] {
            let path = test_path(&format!("grow-crash-{stage:?}"));
            let mut storage = Storage::open(&path).unwrap();
            let employee = EmployeeId([6; 16]);
            let old = Quota { bytes: 64 * 1024 * 1024, inodes: 1024 };
            let target = Quota { bytes: 256 * 1024 * 1024, inodes: 4096 };
            storage.provision(employee, 70004, old).unwrap();
            // Unwind skips normal error quarantine, like process death after a
            // durable checkpoint. It does not simulate hardware power loss.
            let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                storage.resize_steps(employee, target, |current| {
                    assert_ne!(current, stage, "injected interruption"); Ok(())
                })
            }));
            assert!(interrupted.is_err());
            drop(storage);
            let mut storage = Storage::open(&path).unwrap();
            assert_eq!(storage.state.image(employee).unwrap().phase, Phase::Resizing);
            assert_eq!(storage.state.charge().sum::<u64>(), target.bytes);
            assert_eq!(storage.readiness(), Err(Error::RecoveryRequired));
            assert_eq!(storage.attach(employee, true).unwrap_err(), Error::RecoveryRequired);
            storage.detach(employee).unwrap();
            storage.destroy(employee).unwrap();
            storage.readiness().unwrap();
            drop(storage); remove_test_state(&path);
        }
    }

    #[test]
    #[ignore = "root required; verifies error quarantine after durable resize reservation"]
    fn iso_root_resize_error_quarantines_full_target() {
        let path = test_path("grow-error");
        let mut storage = Storage::open(&path).unwrap();
        let employee = EmployeeId([9; 16]);
        let old = Quota { bytes: 64 * 1024 * 1024, inodes: 1024 };
        let target = Quota { bytes: 256 * 1024 * 1024, inodes: 4096 };
        storage.provision(employee, 70007, old).unwrap();
        assert_eq!(storage.resize_steps(employee, target, |_| Err(Error::Io)), Err(Error::Io));
        assert_eq!(storage.state.image(employee).unwrap().phase, Phase::Quarantined);
        assert_eq!(storage.state.charge().sum::<u64>(), target.bytes);
        assert_eq!(storage.attach(employee, true).unwrap_err(), Error::Quarantined);
        drop(storage);
        let mut storage = Storage::open(&path).unwrap();
        assert_eq!(storage.readiness(), Err(Error::Quarantined));
        storage.detach(employee).unwrap(); storage.destroy(employee).unwrap();
        drop(storage); remove_test_state(&path);
    }

    #[test]
    #[ignore = "root required; verifies actual tmpfs and loop-backed host rejection"]
    fn iso_root_rejects_tmpfs_and_recursive_loop_backing() {
        let path = test_path("unsafe-backing");
        mount(Some("tmpfs"), &path, Some("tmpfs"), MsFlags::MS_NOSUID | MsFlags::MS_NODEV,
            Some("mode=0700,size=1m")).unwrap();
        assert!(matches!(Storage::open(&path), Err(Error::UnsafeBacking)));
        umount(&path).unwrap();
        let mut storage = Storage::open(&path).unwrap();
        let employee = EmployeeId([7; 16]);
        storage.provision(employee, 70005, Quota { bytes: 64 * 1024 * 1024, inodes: 1024 }).unwrap();
        let anchor = storage.attach(employee, true).unwrap();
        let nested = anchor.path.join("nested-root");
        fs::DirBuilder::new().mode(0o700).create(&nested).unwrap();
        assert!(matches!(Storage::open(&nested), Err(Error::UnsafeBacking)));
        fs::remove_file(nested.join("storage.lock")).unwrap();
        fs::remove_dir(nested).unwrap();
        storage.detach(employee).unwrap(); storage.destroy(employee).unwrap();
        drop(storage); remove_test_state(&path);
    }

    #[test]
    #[ignore = "root required; checks real inode replacement and alias refusal"]
    fn iso_root_rejects_image_alias_and_replacement() {
        use std::os::unix::fs::symlink;
        let path = test_path("identity");
        let mut storage = Storage::open(&path).unwrap();
        let employee = EmployeeId([8; 16]);
        storage.provision(employee, 70006, Quota { bytes: 64 * 1024 * 1024, inodes: 1024 }).unwrap();
        let name = path.join(format!("{}.img", employee_key(employee)));
        let held = path.join("held-image");
        fs::rename(&name, &held).unwrap();
        symlink(&held, &name).unwrap();
        assert_eq!(storage.open_image_identity(storage.state.image(employee).unwrap()).unwrap_err(), Error::Io);
        fs::remove_file(&name).unwrap();
        fs::hard_link(&held, &name).unwrap();
        assert_eq!(storage.open_image_identity(storage.state.image(employee).unwrap()).unwrap_err(), Error::RecoveryRequired);
        fs::remove_file(&name).unwrap();
        let replacement = open_file(&storage.root, &format!("{}.img", employee_key(employee)),
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL).unwrap();
        assert_eq!(storage.open_image_identity(storage.state.image(employee).unwrap()).unwrap_err(), Error::RecoveryRequired);
        drop(replacement); fs::remove_file(&name).unwrap();
        fs::rename(&held, &name).unwrap();
        storage.destroy(employee).unwrap();
        drop(storage); remove_test_state(&path);
    }

    #[test]
    #[ignore = "root required; simulates interrupted durable metadata publication"]
    fn iso_root_metadata_crash_safety() {
        let path = test_path("metadata");
        let root = trusted_directory(&path).unwrap();
        let old = Registry { version: 1, backing: crate::device::tests::graph(), images: vec![] };
        let mut new = old.clone();
        new.images.push(Image { employee: EmployeeId([3; 16]), uid: 70001,
            quota: Quota { bytes: 64 * 1024 * 1024, inodes: 1024 }, phase: Phase::Creating, inode: None });
        for stage in [PublishStep::Written, PublishStep::FileSynced, PublishStep::Renamed, PublishStep::DirectorySynced] {
            atomic_state(&root, &old).unwrap();
            assert_eq!(atomic_state_steps(&root, &new, |current| if current == stage { Err(Error::Io) } else { Ok(()) }), Err(Error::Io));
            let recovered = Registry::decode(&read_bounded(open_file(&root, "state.json", OFlags::RDONLY).unwrap(), MAX_METADATA_BYTES).unwrap()).unwrap();
            assert_eq!(recovered, if matches!(stage, PublishStep::Written | PublishStep::FileSynced) { old.clone() } else { new.clone() });
            // Retry removes only a validated stale temporary file, then commits.
            atomic_state(&root, &new).unwrap();
        }
        drop(root);
        remove_test_state(&path);
    }

    #[test]
    #[ignore = "root required; checks incomplete image recovery and exclusive lock"]
    fn iso_root_incomplete_creation_never_attaches() {
        let path = test_path("incomplete");
        let mut storage = Storage::open(&path).unwrap();
        let employee = EmployeeId([4; 16]);
        storage.state.images.push(Image { employee, uid: 70002,
            quota: Quota { bytes: 64 * 1024 * 1024, inodes: 1024 }, phase: Phase::Creating, inode: None });
        storage.persist().unwrap();
        assert!(matches!(Storage::open(&path), Err(Error::Busy)));
        drop(storage);
        let mut storage = Storage::open(&path).unwrap();
        assert_eq!(storage.attach(employee, true).unwrap_err(), Error::RecoveryRequired);
        storage.destroy(employee).unwrap();
        drop(storage);
        remove_test_state(&path);
    }
}
