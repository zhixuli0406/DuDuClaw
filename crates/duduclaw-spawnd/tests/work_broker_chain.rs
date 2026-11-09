//! H4 root chain (Package H, D-H1): start-up anchor backfill, the root-held
//! read-only clone of a run's work tree, a sealed snapshot through the real H
//! storage lease, and the drop-before-detach rule, on real loop storage.
//!
//! Root disposable Linux VM only: `ISO_STORAGE_TEST_ROOT` on ext4/XFS (loop
//! devices, private mount tree), `DUDUCLAW_ISO_DISPOSABLE=1`. The test runs in
//! a private mount namespace of its thread and removes its fixture tree.
#![cfg(target_os = "linux")]
use std::{ffi::CString, fs, io::Read, os::{fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd}, unix::fs::{DirBuilderExt, MetadataExt}},
    path::PathBuf};
use duduclaw_isolation::{execution::{DiskQuota, RunHandle}, ids::{EmployeeId, RequestId, RunId, Sha256Digest}, ipc::{ErrorCode, WorkPath}};
use duduclaw_spawnd::{bootstrap_mounts::work_broker_root, storage::{LoopStorage, ProvisionRequest, Storage},
    work_anchor::{backfill, AnchorOwner, AnchoredStorage, Backfilled, WorkAnchors, ANCHOR_MODE, BASE_MODE},
    work_broker::{copy_snapshot, verify_lease, BrokerRoot, LiveRunView, WorkRefusal}};

fn private_mount_namespace() {
    assert_eq!(unsafe { libc::unshare(libc::CLONE_NEWNS) }, 0);
    assert_eq!(unsafe { libc::mount(std::ptr::null(), c"/".as_ptr(), std::ptr::null(),
        libc::MS_REC | libc::MS_PRIVATE, std::ptr::null()) }, 0);
}
fn hex(employee: EmployeeId) -> String { employee.0.iter().map(|byte| format!("{byte:02x}")).collect() }
/// Root creates an employee-owned file beneath the attached work mount.
fn employee_file(work: BorrowedFd<'_>, name: &str, bytes: &[u8], uid: u32) {
    let name = CString::new(name).unwrap();
    let raw = unsafe { libc::openat(work.as_raw_fd(), name.as_ptr(),
        libc::O_CREAT | libc::O_EXCL | libc::O_WRONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC, 0o600) };
    assert!(raw >= 0, "create employee file: {}", std::io::Error::last_os_error());
    let mut file = fs::File::from(unsafe { OwnedFd::from_raw_fd(raw) });
    std::io::Write::write_all(&mut file, bytes).unwrap();
    assert_eq!(unsafe { libc::fchown(file.as_raw_fd(), uid, uid) }, 0);
}

#[test]
#[ignore = "iso_root_: root disposable Linux VM; ISO_STORAGE_TEST_ROOT on ext4/XFS with loop devices; DUDUCLAW_ISO_DISPOSABLE=1"]
fn iso_root_work_broker_chain_backfill_snapshot_and_detach() {
    assert_eq!(unsafe { libc::geteuid() }, 0);
    assert_eq!(std::env::var("DUDUCLAW_ISO_DISPOSABLE").as_deref(), Ok("1"), "disposable VM only");
    let base = PathBuf::from(std::env::var_os("ISO_STORAGE_TEST_ROOT").expect("set isolated ISO_STORAGE_TEST_ROOT"));
    private_mount_namespace();
    let root = base.join(format!("h4-broker-chain-{}", std::process::id()));
    let (images, journal, anchors) = (root.join("images"), root.join("journal"), root.join("work-anchors"));
    for path in [&root, &images, &journal, &anchors] { fs::DirBuilder::new().mode(BASE_MODE).create(path).unwrap(); }

    let employee = EmployeeId([0x91; 16]);
    let uid = 70_000;
    let quota = DiskQuota::LoopFilesystem { bytes: 128 * 1024 * 1024, inodes: 1024 };
    let request = ProvisionRequest { employee, uid, operation_id: RequestId([0x92; 16]),
        proposal_digest: Sha256Digest([0x93; 32]), quota: quota.clone() };

    // 1. An allocation made before H3: image provisioned, no anchor. The
    //    start-up backfill creates it once; a second pass changes nothing.
    let mut inner = LoopStorage::open(&images, &journal).unwrap();
    inner.provision(&request).unwrap().check(employee, &quota).unwrap();
    let mut storage = AnchoredStorage::new(Box::new(inner), WorkAnchors::open_at(&anchors, AnchorOwner::ROOT).unwrap());
    assert_eq!(storage.work_anchor(employee), Err(ErrorCode::NotFound));
    assert_eq!(backfill(&mut storage, &[employee]), vec![(employee, Backfilled::Created)]);
    let anchor = storage.work_anchor(employee).unwrap();
    assert_eq!(backfill(&mut storage, &[employee]), vec![(employee, Backfilled::Present)]);
    assert_eq!(storage.work_anchor(employee).unwrap(), anchor);
    let meta = fs::symlink_metadata(anchors.join(hex(employee))).unwrap();
    assert_eq!((meta.uid(), meta.gid(), meta.mode() & 0o7777, meta.dev(), meta.ino()), (0, 0, ANCHOR_MODE, anchor.device, anchor.inode));

    // 2. A run: attach, then the root-held RO clone of the WHOLE work tree
    //    (taken before any workspace narrowing), then the attach fds go away
    //    exactly as at the end of `Core::start`.
    let run = RunHandle(RunId([0x94; 16]));
    let attached = storage.attach(employee, uid, run, &quota).unwrap();
    let receipt = attached.receipt.clone();
    employee_file(attached.work.as_fd(), "report.csv", b"a,b\n1,2\n", uid);
    let mut broker = BrokerRoot::new(Some(work_broker_root(&attached).unwrap()));
    drop(attached);
    let clone = broker.root().unwrap();
    let stat = nix_statfs_flags(clone);
    assert!(stat & libc::ST_RDONLY != 0 && stat & libc::ST_NOEXEC != 0 && stat & libc::ST_NODEV != 0 && stat & libc::ST_NOSUID != 0);
    let planted = unsafe { libc::openat(clone.as_raw_fd(), c"planted".as_ptr(), libc::O_CREAT | libc::O_WRONLY | libc::O_CLOEXEC, 0o600) };
    assert!(planted < 0, "root itself cannot write through the broker clone");

    // 3. The real lease check, then the sealed snapshot.
    let path = WorkPath::new("report.csv".into()).unwrap();
    let view = LiveRunView { run, employee, uid, running: true, receipt: Some(&receipt), work_root: broker.root() };
    let expected = verify_lease(&view, &mut storage, run, employee).unwrap();
    let stopped = LiveRunView { run, employee, uid, running: false, receipt: Some(&receipt), work_root: broker.root() };
    assert_eq!(verify_lease(&stopped, &mut storage, run, employee).unwrap_err(), WorkRefusal::NotLive);
    assert_eq!(verify_lease(&view, &mut storage, run, EmployeeId([0x98; 16])).unwrap_err(), WorkRefusal::WrongEmployee);
    let mut snapshot = copy_snapshot(broker.lease().unwrap(), &path, &expected, &|| true).unwrap();
    assert_eq!(broker.in_flight(), 0);

    // 4. Drop before detach: while a copy still holds a dup of the clone the
    //    loop device stays busy and H refuses to finish the detach; once the
    //    clone is released the same detach succeeds.
    let held = broker.lease().unwrap();
    assert_eq!(broker.release(), Err(ErrorCode::NotReady));
    assert!(storage.detach(&receipt).is_err(), "a held clone must keep the employee filesystem attached");
    drop(held);
    assert_eq!(broker.release(), Ok(()));
    storage.detach(&receipt).unwrap();
    assert!(storage.attachments().unwrap().is_empty());

    // 5. The sealed snapshot survives the detach (it references nothing on the
    //    employee filesystem) and still reads the bytes of the open.
    let mut bytes = Vec::new();
    snapshot.file.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"a,b\n1,2\n");

    // 6. Retire removes the anchor with the image; a later backfill never
    //    recreates it for a destroyed allocation (the caller only backfills Ready).
    storage.destroy(employee, RequestId([0x96; 16]), Sha256Digest([0x97; 32])).unwrap();
    assert_eq!(storage.work_anchor(employee), Err(ErrorCode::NotFound));
    drop(storage);
    let _ = fs::remove_dir_all(&root);
}

fn nix_statfs_flags(fd: BorrowedFd<'_>) -> libc::c_ulong {
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::fstatvfs(fd.as_raw_fd(), &mut stat) }, 0);
    stat.f_flag
}
