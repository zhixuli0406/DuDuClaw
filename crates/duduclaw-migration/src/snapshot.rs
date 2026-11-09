//! Linux-only fd-relative walker for malicious legacy trees (§10.5).
//! No path supplied by an entry is used outside its parent dirfd.
use std::{collections::BTreeMap, fs::File, io::Read};
use rustix::{fd::{AsFd, OwnedFd}, fs::{self, AtFlags, FileType, Mode, OFlags, ResolveFlags, StatxFlags}};
use serde::Serialize;
use sha2::{Digest, Sha256};
use duduclaw_isolation::ids::Sha256Digest;
use crate::Error;

const RESOLVE: ResolveFlags = ResolveFlags::BENEATH.union(ResolveFlags::NO_SYMLINKS)
    .union(ResolveFlags::NO_MAGICLINKS).union(ResolveFlags::NO_XDEV);

fn io<T>(value: rustix::io::Result<T>) -> Result<T, Error> { value.map_err(|_| Error::Storage) }

#[derive(Debug, Clone, Copy)]
pub struct Limits { pub max_entries: u32, pub max_bytes: u64, pub max_depth: u32 }
impl Limits {
    fn validate(self) -> Result<(), Error> {
        if self.max_entries == 0 || self.max_entries > 100_000 || self.max_depth == 0
            || self.max_depth > 32 || self.max_bytes == 0 { return Err(Error::Limit); }
        Ok(())
    }
}

/// Names and link text are byte strings to preserve non-UTF-8 Linux filenames.
/// These are archive data only, not instructions to recreate filesystem objects.
#[derive(Debug, Serialize)]
#[serde(tag = "record", rename_all = "snake_case")]
pub enum Event {
    Entry { id: u32, path: Vec<Vec<u8>>, mode: u16, kind: &'static str, size: u64 },
    LinkText { id: u32, text: Vec<u8> },
    Hardlink { id: u32, original: u32 },
    Chunk { id: u32, bytes: Vec<u8> },
    Digest { id: u32, sha256: Sha256Digest },
    Xattr { id: u32, name: Vec<u8>, value: Vec<u8> },
    XattrsTruncated { id: u32 },
    End { entries: u32, bytes: u64 },
}

/// Root-owned sink must bound its own encoded output and encrypt immediately.
/// A failed walk leaves an incomplete archive that must never be published.
pub trait Sink { fn emit(&mut self, event: &Event) -> Result<(), Error>; }
impl<W: std::io::Write> Sink for crate::archive::ArchiveWriter<W> {
    fn emit(&mut self, event: &Event) -> Result<(), Error> {
        let mut json = serde_json::to_vec(event)?;
        json.push(b'\n');
        self.append(&json)
    }
}

fn stat(fd: &impl AsFd) -> Result<fs::Statx, Error> {
    let s = io(fs::statx(fd, "", AtFlags::EMPTY_PATH | AtFlags::SYMLINK_NOFOLLOW,
        StatxFlags::BASIC_STATS | StatxFlags::MNT_ID))?;
    if s.stx_mask & StatxFlags::MNT_ID.bits() == 0 { return Err(Error::Unavailable); }
    Ok(s)
}
fn same(a: &fs::Statx, b: &fs::Statx) -> bool {
    a.stx_ino == b.stx_ino && a.stx_dev_major == b.stx_dev_major
        && a.stx_dev_minor == b.stx_dev_minor && a.stx_mnt_id == b.stx_mnt_id
        && a.stx_mode == b.stx_mode && a.stx_size == b.stx_size
        && a.stx_mtime.tv_sec == b.stx_mtime.tv_sec && a.stx_mtime.tv_nsec == b.stx_mtime.tv_nsec
        && a.stx_ctime.tv_sec == b.stx_ctime.tv_sec && a.stx_ctime.tv_nsec == b.stx_ctime.tv_nsec
}

pub fn walk(root: &OwnedFd, limits: Limits, sink: &mut impl Sink) -> Result<(), Error> {
    limits.validate()?;
    let metadata = stat(root)?;
    if FileType::from_raw_mode(metadata.stx_mode.into()) != FileType::Directory { return Err(Error::Invalid); }
    let mut walker = Walker { limits, entries: 0, bytes: 0, seen: BTreeMap::new(), mount: metadata.stx_mnt_id };
    walker.directory(root, &mut Vec::new(), sink)?;
    sink.emit(&Event::End { entries: walker.entries, bytes: walker.bytes })
}

struct Walker {
    limits: Limits, entries: u32, bytes: u64, mount: u64,
    seen: BTreeMap<(u32, u32, u64), u32>,
}
impl Walker {
    fn directory(&mut self, parent: &OwnedFd, path: &mut Vec<Vec<u8>>, sink: &mut impl Sink) -> Result<(), Error> {
        let mut dir = io(fs::Dir::read_from(parent))?;
        while let Some(entry) = dir.read() {
            let entry = io(entry)?;
            let name = entry.file_name();
            if name.to_bytes() == b"." || name.to_bytes() == b".." { continue; }
            if name.to_bytes().len() > 255 || path.len() >= self.limits.max_depth as usize { return Err(Error::Limit); }
            self.entries = self.entries.checked_add(1).ok_or(Error::Limit)?;
            if self.entries > self.limits.max_entries { return Err(Error::Limit); }
            let id = self.entries;
            // O_PATH never opens a device driver/FIFO. Final symlinks may be
            // inspected with NOFOLLOW; NO_SYMLINKS still forbids traversal.
            let handle = io(fs::openat2(parent, name, OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(), RESOLVE))?;
            let before = stat(&handle)?;
            if before.stx_mnt_id != self.mount { return Err(Error::Invalid); }
            path.push(name.to_bytes().to_vec());
            let kind = FileType::from_raw_mode(before.stx_mode.into());
            let label = match kind {
                FileType::RegularFile => "file", FileType::Directory => "directory", FileType::Symlink => "symlink",
                FileType::Socket => "socket", FileType::Fifo => "fifo", FileType::CharacterDevice => "character_device",
                FileType::BlockDevice => "block_device", _ => return Err(Error::Invalid),
            };
            sink.emit(&Event::Entry { id, path: path.clone(), mode: before.stx_mode, kind: label, size: before.stx_size })?;
            match kind {
                FileType::Symlink => {
                    let mut text = [0; 4096];
                    // Empty-path readlinkat operates on the pinned O_PATH fd.
                    let length = io(fs::readlinkat_raw(&handle, "", &mut text[..]))?;
                    if length == text.len() { return Err(Error::Limit); }
                    sink.emit(&Event::LinkText { id, text: text[..length].to_vec() })?;
                }
                FileType::Directory => {
                    let directory = io(fs::openat2(parent, name,
                        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                        Mode::empty(), RESOLVE))?;
                    if !same(&before, &stat(&directory)?) { return Err(Error::Conflict); }
                    xattrs(&directory, id, sink)?;
                    self.directory(&directory, path, sink)?;
                    if !same(&before, &stat(&directory)?) { return Err(Error::Conflict); }
                }
                FileType::RegularFile => {
                    let key = (before.stx_dev_major, before.stx_dev_minor, before.stx_ino);
                    if let Some(original) = self.seen.get(&key) {
                        sink.emit(&Event::Hardlink { id, original: *original })?;
                    } else {
                        self.bytes = self.bytes.checked_add(before.stx_size).ok_or(Error::Limit)?;
                        if self.bytes > self.limits.max_bytes { return Err(Error::Limit); }
                        let opened = io(fs::openat2(parent, name,
                            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                            Mode::empty(), RESOLVE))?;
                        if !same(&before, &stat(&opened)?) { return Err(Error::Conflict); }
                        xattrs(&opened, id, sink)?;
                        let mut file = File::from(opened);
                        let mut remaining = before.stx_size;
                        let mut buffer = [0; 16 * 1024];
                        let mut digest = Sha256::new();
                        while remaining != 0 {
                            let cap = remaining.min(buffer.len() as u64) as usize;
                            let count = file.read(&mut buffer[..cap])?;
                            if count == 0 { return Err(Error::Conflict); }
                            digest.update(&buffer[..count]);
                            sink.emit(&Event::Chunk { id, bytes: buffer[..count].to_vec() })?;
                            remaining -= count as u64;
                        }
                        if file.read(&mut buffer[..1])? != 0 || !same(&before, &stat(&file)?) { return Err(Error::Conflict); }
                        sink.emit(&Event::Digest { id, sha256: Sha256Digest(digest.finalize().into()) })?;
                        self.seen.insert(key, id);
                    }
                }
                _ => (), // Special files are metadata only, never opened for I/O.
            }
            path.pop();
        }
        Ok(())
    }
}

fn xattrs(fd: &impl AsFd, id: u32, sink: &mut impl Sink) -> Result<(), Error> {
    let mut names = [0; 16 * 1024];
    let length = match fs::flistxattr(fd, &mut names[..]) {
        Ok(n) => n,
        Err(rustix::io::Errno::RANGE) => return sink.emit(&Event::XattrsTruncated { id }),
        Err(rustix::io::Errno::NOTSUP) => return Ok(()),
        Err(_) => return Err(Error::Storage),
    };
    let mut value = vec![0; 64 * 1024];
    for (index, name) in names[..length].split(|b| *b == 0).filter(|n| !n.is_empty()).enumerate() {
        if index >= 64 || name.len() > 255 { return sink.emit(&Event::XattrsTruncated { id }); }
        let name_c = std::ffi::CString::new(name).map_err(|_| Error::Invalid)?;
        let size = match fs::fgetxattr(fd, name_c.as_c_str(), &mut value[..]) {
            Ok(n) => n,
            Err(rustix::io::Errno::RANGE) => { sink.emit(&Event::XattrsTruncated { id })?; continue; }
            Err(_) => return Err(Error::Storage),
        };
        sink.emit(&Event::Xattr { id, name: name.to_vec(), value: value[..size].to_vec() })?;
    }
    Ok(())
}

/// Only same-filesystem quarantine is supported here. EXDEV fails closed; the
/// installer must never substitute a recursive copy/chown of the legacy tree.
/// Both parents must be pinned root-owned directories, and the old uid must be
/// stopped before calling. The destination parent is the root-only migration id.
pub fn quarantine(old_parent: &OwnedFd, old_name: &str, destination: &OwnedFd) -> Result<(), Error> {
    if old_name.is_empty() || matches!(old_name, "." | "..") || old_name.bytes().any(|b| b == b'/' || b == 0) {
        return Err(Error::Invalid);
    }
    let target = stat(destination)?;
    let parent = stat(old_parent)?;
    if target.stx_uid != 0 || target.stx_mode & 0o777 != 0o700 || parent.stx_uid != 0
        || parent.stx_mode & 0o022 != 0 { return Err(Error::Invalid); }
    let source = io(fs::openat2(old_parent, old_name, OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(), RESOLVE))?;
    let source_stat = stat(&source)?;
    io(fs::renameat_with(old_parent, old_name, destination, "legacy", fs::RenameFlags::NOREPLACE))?;
    io(fs::fsync(destination))?;
    io(fs::fsync(old_parent))?;
    let landed = io(fs::openat2(destination, "legacy", OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(), RESOLVE))?;
    let landed_stat = stat(&landed)?;
    if source_stat.stx_ino != landed_stat.stx_ino || source_stat.stx_dev_major != landed_stat.stx_dev_major
        || source_stat.stx_dev_minor != landed_stat.stx_dev_minor { return Err(Error::Readback); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Records(Vec<String>);
    impl Sink for Records { fn emit(&mut self, event: &Event) -> Result<(), Error> {
        self.0.push(serde_json::to_string(event)?); Ok(())
    } }
    fn directory(path: &std::path::Path) -> OwnedFd {
        fs::open(path, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC, Mode::empty()).unwrap()
    }
    #[test]
    fn snapshot_records_links_without_following_or_duplicating_data() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("file"), b"content").unwrap();
        std::fs::hard_link(tmp.path().join("file"), tmp.path().join("hard")).unwrap();
        std::os::unix::fs::symlink(".", tmp.path().join("cycle")).unwrap();
        let socket = std::os::unix::net::UnixListener::bind(tmp.path().join("socket")).unwrap();
        let mut records = Records::default();
        walk(&directory(tmp.path()), Limits { max_entries: 10, max_depth: 4, max_bytes: 7 }, &mut records).unwrap();
        assert_eq!(records.0.iter().filter(|s| s.contains("\"record\":\"chunk\"")).count(), 1);
        assert!(records.0.iter().any(|s| s.contains("\"kind\":\"socket\"")));
        assert!(records.0.iter().any(|s| s.contains("\"record\":\"link_text\"")));
        drop(socket);
    }
    #[test]
    fn snapshot_enforces_entry_depth_and_byte_limits() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("file"), b"large").unwrap();
        assert!(walk(&directory(tmp.path()), Limits { max_entries: 10, max_depth: 4, max_bytes: 4 }, &mut Records::default()).is_err());
        std::fs::create_dir(tmp.path().join("nested")).unwrap();
        std::fs::write(tmp.path().join("nested/file"), b"x").unwrap();
        assert!(walk(&directory(tmp.path()), Limits { max_entries: 10, max_depth: 1, max_bytes: 100 }, &mut Records::default()).is_err());
        assert!(walk(&directory(tmp.path()), Limits { max_entries: 1, max_depth: 4, max_bytes: 100 }, &mut Records::default()).is_err());
    }
    #[test]
    #[ignore = "requires root on a disposable Linux host"]
    fn iso_root_quarantine_renames_without_clobbering() {
        assert!(rustix::process::geteuid().is_root());
        let tmp = tempfile::tempdir().unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::create_dir(tmp.path().join("old")).unwrap();
        std::fs::create_dir(tmp.path().join("quarantine")).unwrap();
        std::fs::set_permissions(tmp.path().join("quarantine"), std::fs::Permissions::from_mode(0o700)).unwrap();
        quarantine(&directory(tmp.path()), "old", &directory(&tmp.path().join("quarantine"))).unwrap();
        std::fs::create_dir(tmp.path().join("old")).unwrap();
        assert!(quarantine(&directory(tmp.path()), "old", &directory(&tmp.path().join("quarantine"))).is_err());
        assert!(tmp.path().join("old").is_dir());
    }
}
