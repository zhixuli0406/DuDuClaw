//! Root-only content copier. Names in imported records never become paths.
//! B/H supplies confirmed, stopped employee allocation and pinned directories.
use std::{fs::File, io::{Read, Write}, os::fd::AsRawFd};
use rustix::{fd::{AsFd, OwnedFd}, fs::{self, AtFlags, FileType, Mode, OFlags, ResolveFlags, StatxFlags, RenameFlags}};
use sha2::{Digest, Sha256};
use duduclaw_isolation::{ids::RequestId, migration::ImportedBlob};
use crate::Error;
const RESOLVE: ResolveFlags = ResolveFlags::BENEATH.union(ResolveFlags::NO_SYMLINKS).union(ResolveFlags::NO_MAGICLINKS).union(ResolveFlags::NO_XDEV);
fn io<T>(result: rustix::io::Result<T>) -> Result<T, Error> { result.map_err(|_| Error::Storage) }
fn stat(fd: &impl AsFd) -> Result<fs::Statx, Error> {
    let s = io(fs::statx(fd,"",AtFlags::EMPTY_PATH,StatxFlags::BASIC_STATS | StatxFlags::MNT_ID))?;
    if s.stx_mask & StatxFlags::MNT_ID.bits() == 0 { return Err(Error::Unavailable); }
    Ok(s)
}
fn stable(a: &fs::Statx, b: &fs::Statx) -> bool {
    a.stx_ino == b.stx_ino && a.stx_mnt_id == b.stx_mnt_id && a.stx_size == b.stx_size && a.stx_nlink == b.stx_nlink
        && a.stx_ctime.tv_sec == b.stx_ctime.tv_sec && a.stx_ctime.tv_nsec == b.stx_ctime.tv_nsec
        && a.stx_mtime.tv_sec == b.stx_mtime.tv_sec && a.stx_mtime.tv_nsec == b.stx_mtime.tv_nsec
}
fn digest(file: &mut File, max: u64) -> Result<([u8;32],u64), Error> {
    let mut hash = Sha256::new(); let mut total = 0u64; let mut buffer = [0u8;65536];
    loop { let n = file.read(&mut buffer)?; if n == 0 { break; }
        total = total.checked_add(n as u64).ok_or(Error::Limit)?;
        if total > max { return Err(Error::Limit); } hash.update(&buffer[..n]); }
    Ok((hash.finalize().into(), total))
}
/// `source_path` is a root-manifest path, never `display_name`. The destination
/// is generated from the operation id. No xattr, mode, owner or link is copied.
/// The integrator persists/reconstructs remaining quota and file count before calling.
#[derive(Clone, Copy)]
pub enum BlobDestination { Attachment, WorkProduct }

pub fn copy_blob(kind: BlobDestination, source_root: &OwnedFd, source_path: &str, staging: &OwnedFd, destination: &OwnedFd,
    operation: RequestId, blob: &ImportedBlob, target_uid: u32, remaining_bytes: u64, remaining_files: u32,
) -> Result<String, Error> {
    if !rustix::process::geteuid().is_root() || target_uid == 0
        || remaining_files > 100_000 { return Err(Error::Limit); }
    let parts: Vec<_> = source_path.split('/').collect();
    if parts.is_empty() || parts.len() > 32 || parts.iter().any(|p| p.is_empty() || matches!(*p,"."|"..") || p.len()>255) {
        return Err(Error::Invalid);
    }
    if matches!(kind, BlobDestination::WorkProduct) {
        let denied = ["agent.toml","SOUL.md","IDENTITY.md","INSTRUCTIONS.md","CONTRACT.toml","MEMORY.md","memory.db",".mcp.json",".scope.toml","SKILLS","state","tasks","wiki","attachments","tmp",".claude",".grok"];
        let extension = parts.last().and_then(|p| p.rsplit_once('.')).map(|(_,ext)| ext);
        if parts.iter().any(|p| denied.contains(p) || p.strip_prefix(".soul_").is_some())
            || !matches!(extension, Some("docx"|"xlsx"|"pdf"|"csv"|"md"|"txt"|"json"|"png"|"jpg"|"jpeg"|"svg")) {
            return Err(Error::Invalid);
        }
    }
    let stage = stat(staging)?; let dest = stat(destination)?;
    if FileType::from_raw_mode(stage.stx_mode.into()) != FileType::Directory || stage.stx_uid != 0 || stage.stx_mode & 0o077 != 0
        || FileType::from_raw_mode(dest.stx_mode.into()) != FileType::Directory || dest.stx_uid != target_uid
        || stage.stx_mnt_id != dest.stx_mnt_id { return Err(Error::Readback); }
    let source = io(fs::openat2(source_root,source_path,OFlags::PATH|OFlags::CLOEXEC|OFlags::NOFOLLOW,Mode::empty(),RESOLVE))?;
    let before = stat(&source)?;
    if FileType::from_raw_mode(before.stx_mode.into()) != FileType::RegularFile || before.stx_nlink != 1 || before.stx_size != blob.size_bytes {
        return Err(Error::Invalid);
    }
    // Reopen the already classified inode through the trusted proc fd table,
    // never re-resolve the attacker-controlled source name as a device/FIFO.
    let source = io(fs::open(format!("/proc/self/fd/{}",source.as_raw_fd()),OFlags::RDONLY|OFlags::CLOEXEC|OFlags::NONBLOCK,Mode::empty()))?;
    if !stable(&before,&stat(&source)?) { return Err(Error::Readback); }
    // Even seemingly benign attributes are refused; no capabilities or ACLs survive.
    let mut names = [0u8;1];
    let xattrs: usize = io(fs::flistxattr(&source, &mut names[..]))?;
    if xattrs != 0 { return Err(Error::Invalid); }
    let name: String = operation.0.iter().map(|b| format!("{b:02x}")).collect();
    let name = format!("import-{name}");
    if let Ok(existing) = fs::openat2(destination,name.as_str(),OFlags::RDONLY|OFlags::CLOEXEC|OFlags::NOFOLLOW|OFlags::NONBLOCK,Mode::empty(),RESOLVE) {
        let s = stat(&existing)?;
        if FileType::from_raw_mode(s.stx_mode.into()) != FileType::RegularFile || s.stx_nlink != 1 || s.stx_uid != target_uid
            || s.stx_mode & 0o7777 != 0o600 { return Err(Error::Readback); }
        let (hash,size) = digest(&mut File::from(existing),blob.size_bytes)?;
        if hash != blob.sha256.0 || size != blob.size_bytes { return Err(Error::Conflict); }
        return Ok(name);
    }
    // A verified replay consumes no new quota, even when the previous publish
    // used the final byte/file reservation. New writes still require capacity.
    if remaining_files == 0 || blob.size_bytes > remaining_bytes { return Err(Error::Limit); }
    // Only this operation's file inside a validated root-only staging directory.
    match fs::unlinkat(staging,name.as_str(),AtFlags::empty()) { Ok(()) => {}, Err(e) if e == rustix::io::Errno::NOENT => {}, Err(_) => return Err(Error::Storage) }
    let target = io(fs::openat2(staging,name.as_str(),OFlags::WRONLY|OFlags::CREATE|OFlags::EXCL|OFlags::CLOEXEC|OFlags::NOFOLLOW,
        Mode::from_raw_mode(0o600),RESOLVE))?;
    let mut input = File::from(source); let mut output = File::from(target);
    let mut hash = Sha256::new(); let mut total = 0u64; let mut buffer = [0u8;65536];
    loop { let n = input.read(&mut buffer)?; if n == 0 { break; }
        total = total.checked_add(n as u64).ok_or(Error::Limit)?;
        if total > blob.size_bytes { return Err(Error::Limit); }
        hash.update(&buffer[..n]); output.write_all(&buffer[..n])?; }
    if total != blob.size_bytes || <[u8;32]>::from(hash.finalize()) != blob.sha256.0 || !stable(&before,&stat(&input)?) { return Err(Error::Readback); }
    io(fs::fchmod(&output,Mode::from_raw_mode(0o600)))?;
    io(fs::fchown(&output,Some(rustix::process::Uid::from_raw(target_uid)),None))?;
    output.sync_all()?;
    io(fs::renameat_with(staging,name.as_str(),destination,name.as_str(),RenameFlags::NOREPLACE))?;
    io(fs::fsync(destination))?; io(fs::fsync(staging))?;
    let published=io(fs::openat2(destination,name.as_str(),OFlags::RDONLY|OFlags::NOFOLLOW|OFlags::CLOEXEC,Mode::empty(),RESOLVE))?;
    let after=stat(&published)?;
    if after.stx_uid!=target_uid || after.stx_mode & 0o7777 != 0o600 || after.stx_nlink!=1 {return Err(Error::Readback);}
    let (read_hash,read_size)=digest(&mut File::from(published),blob.size_bytes)?;
    if read_hash!=blob.sha256.0 || read_size!=blob.size_bytes {return Err(Error::Readback);}
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires Linux root; creates only isolated temporary directories"]
    fn iso_root_import_blob_copies_once_rejects_links_and_xattrs() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        for name in ["source","stage","destination"] { std::fs::create_dir(temp.path().join(name)).unwrap(); }
        std::fs::set_permissions(temp.path().join("stage"), std::fs::Permissions::from_mode(0o700)).unwrap();
        let open = |name| fs::open(temp.path().join(name),OFlags::RDONLY|OFlags::DIRECTORY|OFlags::CLOEXEC,Mode::empty()).unwrap();
        let source = open("source"); let stage = open("stage"); let destination = open("destination");
        fs::fchown(&destination,Some(rustix::process::Uid::from_raw(65534)),None).unwrap();
        std::fs::write(temp.path().join("source/report.pdf"),b"report").unwrap();
        let blob = ImportedBlob { source_handle: RequestId([1;16]), display_name: "../../evil".into(), size_bytes:6,
            sha256:duduclaw_isolation::ids::Sha256Digest(Sha256::digest(b"report").into()) };
        let name = copy_blob(BlobDestination::WorkProduct,&source,"report.pdf",&stage,&destination,RequestId([2;16]),&blob,65534,10,5).unwrap();
        assert_eq!(std::fs::read(temp.path().join("destination").join(&name)).unwrap(),b"report");
        assert_eq!(copy_blob(BlobDestination::WorkProduct,&source,"report.pdf",&stage,&destination,RequestId([2;16]),&blob,65534,10,5).unwrap(),name);
        assert_eq!(copy_blob(BlobDestination::WorkProduct,&source,"report.pdf",&stage,&destination,RequestId([2;16]),&blob,65534,0,0).unwrap(),name);
        assert!(copy_blob(BlobDestination::WorkProduct,&source,"report.pdf",&stage,&destination,RequestId([4;16]),&blob,65534,0,0).is_err());
        std::os::unix::fs::symlink("report.pdf",temp.path().join("source/link.pdf")).unwrap();
        assert!(copy_blob(BlobDestination::WorkProduct,&source,"link.pdf",&stage,&destination,RequestId([3;16]),&blob,65534,10,5).is_err());
        std::fs::hard_link(temp.path().join("source/report.pdf"),temp.path().join("source/hard.pdf")).unwrap();
        assert!(copy_blob(BlobDestination::WorkProduct,&source,"hard.pdf",&stage,&destination,RequestId([3;16]),&blob,65534,10,5).is_err());
        std::fs::remove_file(temp.path().join("source/hard.pdf")).unwrap();
        let input=File::open(temp.path().join("source/report.pdf")).unwrap();
        fs::fsetxattr(&input,"user.migration",b"x",fs::XattrFlags::empty()).unwrap();
        assert!(copy_blob(BlobDestination::WorkProduct,&source,"report.pdf",&stage,&destination,RequestId([3;16]),&blob,65534,10,5).is_err());
    }
}
