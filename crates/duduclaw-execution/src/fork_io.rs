//! Descriptor-relative bounded clone; executed only inside the employee helper.
//! Manifests attest content digests only, never permissions or other metadata.
use std::{collections::BTreeMap, fs::File, io::{Read, Write}, os::{fd::{AsRawFd, FromRawFd}, unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt}}, path::Path};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use crate::fork_helper::{Request, Operation, MAX_FILES, MAX_BYTES};
const DIR: i32 = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
fn open(root: &File, name: &str, flags: i32, mode: u64) -> Result<File, ()> {
    let name = std::ffi::CString::new(name).map_err(|_| ())?;
    #[repr(C)] struct How { flags:u64, mode:u64, resolve:u64 }
    let how = How { flags: flags as u64, mode, resolve: 0x01 | 0x02 | 0x04 | 0x08 };
    // SAFETY: owned dirfd, terminated relative name and initialized openat2 ABI.
    let fd = unsafe { libc::syscall(libc::SYS_openat2,root.as_raw_fd(),name.as_ptr(),&how,std::mem::size_of::<How>()) };
    if fd < 0 { return Err(()); }
    // SAFETY: successful syscall returns a new owned fd.
    Ok(unsafe { File::from_raw_fd(fd as i32) })
}
fn mkdir(root:&File,name:&str,existing:bool) -> Result<(),()> {
    let name = std::ffi::CString::new(name).map_err(|_| ())?;
    // SAFETY: retained dirfd and terminated single component.
    if unsafe { libc::mkdirat(root.as_raw_fd(),name.as_ptr(),0o700) } != 0
        && !(existing && std::io::Error::last_os_error().raw_os_error()==Some(libc::EEXIST)) { return Err(()); }
    Ok(())
}
struct Directory(*mut libc::DIR);
impl Drop for Directory { fn drop(&mut self) {
    // SAFETY: fdopendir transferred exclusive ownership to this guard.
    unsafe { libc::closedir(self.0); }
} }
fn names(root:&File) -> Result<Vec<String>,()> {
    // SAFETY: dup creates an independent owned fd for fdopendir.
    let fd = unsafe { libc::fcntl(root.as_raw_fd(),libc::F_DUPFD_CLOEXEC,3) };
    if fd < 0 { return Err(()); }
    // SAFETY: fd is a valid owned directory; closedir will close it.
    let pointer = unsafe { libc::fdopendir(fd) };
    if pointer.is_null() {
        // SAFETY: ownership was not transferred on failure.
        unsafe { libc::close(fd); } return Err(());
    }
    let directory = Directory(pointer); let mut names = vec![];
    loop {
        // SAFETY: valid exclusive DIR pointer; returned entry lasts until next read.
        unsafe { *libc::__errno_location() = 0; }
        let entry = unsafe { libc::readdir(directory.0) };
        if entry.is_null() {
            // SAFETY: thread-local errno distinguishes EOF from read failure.
            return if unsafe { *libc::__errno_location() } == 0 { Ok(names) } else { Err(()) };
        }
        // SAFETY: readdir supplies a terminated d_name within the live entry.
        let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) }.to_str().map_err(|_| ())?;
        if matches!(name,"."|"..") { continue; }
        if name.is_empty() || name.contains('/') || name.contains('\\') || names.len() >= MAX_FILES { return Err(()); }
        names.push(name.to_owned());
    }
}
struct Budget { entries:usize, bytes:u64, files:BTreeMap<String,String> }
fn stamp(m:&std::fs::Metadata) -> (u64,u64,u64,i64,i64,i64,i64) {
    (m.dev(),m.ino(),m.len(),m.mtime(),m.mtime_nsec(),m.ctime(),m.ctime_nsec())
}
fn clone_tree(source:&File,dest:&File,prefix:&str,budget:&mut Budget,depth:usize,parent:bool) -> Result<(),()> {
    if depth > 32 { return Err(()); }
    let before = source.metadata().map_err(|_| ())?;
    for name in names(source)? {
        if parent && matches!(name.as_str(),".forks"|".fork-results") { continue; }
        budget.entries += 1;
        if budget.entries > MAX_FILES { return Err(()); }
        let relative = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
        if relative.len() > 4096 { return Err(()); }
        let mut input = open(source,&name,libc::O_RDONLY|libc::O_NOFOLLOW|libc::O_NONBLOCK|libc::O_CLOEXEC|libc::O_NOCTTY,0)?;
        let metadata = input.metadata().map_err(|_| ())?;
        if metadata.is_dir() {
            mkdir(dest,&name,false)?; let directory = open(dest,&name,DIR,0)?;
            clone_tree(&input,&directory,&relative,budget,depth+1,false)?;
        } else if metadata.is_file() && metadata.nlink()==1 {
            if metadata.len() > MAX_BYTES.saturating_sub(budget.bytes) { return Err(()); }
            let mut output = open(dest,&name,libc::O_WRONLY|libc::O_CREAT|libc::O_EXCL|libc::O_NOFOLLOW|libc::O_CLOEXEC,0o600)?;
            let mut digest=Sha256::new(); let mut count=0u64; let mut buffer=[0;8192];
            loop {
                let n=input.read(&mut buffer).map_err(|_| ())?; if n==0 { break; }
                count=count.checked_add(n as u64).ok_or(())?;
                if count > MAX_BYTES.saturating_sub(budget.bytes) { return Err(()); }
                output.write_all(&buffer[..n]).map_err(|_| ())?; digest.update(&buffer[..n]);
            }
            if count!=metadata.len() || stamp(&metadata)!=stamp(&input.metadata().map_err(|_| ())?) { return Err(()); }
            // Preserve owner execution without copying set-id/group/world bits.
            let mode=if metadata.mode() & 0o100 != 0 { 0o700 } else { 0o600 };
            output.set_permissions(std::fs::Permissions::from_mode(mode)).map_err(|_| ())?;
            output.sync_all().map_err(|_| ())?; budget.bytes+=count;
            budget.files.insert(relative,format!("{:x}",digest.finalize()));
        } else { return Err(()); }
    }
    if stamp(&before)!=stamp(&source.metadata().map_err(|_| ())?) { return Err(()); }
    dest.sync_all().map_err(|_| ())?; Ok(())
}
/// Retains failed staging for conservative operator recovery, never removes
/// preexisting files. Successful results are no-clobber publications; owning
/// employee access is not an immutable-storage guarantee.
pub(crate) fn snapshot(workspace:&Path,request:&Request) -> Result<Value,()> {
    request.encode().map_err(|_| ())?;
    let root=std::fs::OpenOptions::new().read(true).custom_flags(DIR).open(workspace).map_err(|_| ())?;
    let parent_name=match request.operation { Operation::CloneParent=>".forks",Operation::SnapshotBranch=>".fork-results" };
    mkdir(&root,parent_name,true)?; let parent=open(&root,parent_name,DIR,0)?;
    let source=match request.operation { Operation::CloneParent=>root.try_clone().map_err(|_| ())?,
        Operation::SnapshotBranch=>open(&root,&format!(".forks/{}",request.id),DIR,0)? };
    let staging=format!(".pending-{}",uuid::Uuid::new_v4().simple());
    mkdir(&parent,&staging,false)?; let dest=open(&parent,&staging,DIR,0)?;
    let mut budget=Budget { entries:0,bytes:0,files:BTreeMap::new() };
    clone_tree(&source,&dest,"",&mut budget,0,request.operation==Operation::CloneParent)?;
    let canonical=serde_json::to_vec(&json!({"files":budget.files})).map_err(|_| ())?;
    let result=json!({"id":request.id,"workspace":format!("fork-{}",request.id),"files":budget.files,
        "digest":format!("{:x}",Sha256::digest(canonical))});
    let bytes=serde_json::to_vec(&result).map_err(|_| ())?;
    crate::fork_helper::decode_result(&bytes).map_err(|_| ())?;
    let from=std::ffi::CString::new(staging).map_err(|_| ())?;
    let to=std::ffi::CString::new(request.id.clone()).map_err(|_| ())?;
    // SAFETY: retained parent, generated names, atomic RENAME_NOREPLACE.
    if unsafe { libc::syscall(libc::SYS_renameat2,parent.as_raw_fd(),from.as_ptr(),parent.as_raw_fd(),to.as_ptr(),1u32) } != 0 { return Err(()); }
    parent.sync_all().map_err(|_| ())?; Ok(result)
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn fork_snapshot_clones_exact_bytes_rejects_links_and_never_clobbers() {
        let work=tempfile::tempdir().unwrap(); std::fs::write(work.path().join("a"),b"hello").unwrap();
        std::fs::set_permissions(work.path().join("a"),std::fs::Permissions::from_mode(0o755)).unwrap();
        let request=Request{id:"ab".repeat(16),operation:Operation::CloneParent};
        let result=snapshot(work.path(),&request).unwrap(); assert_eq!(result["files"].as_object().unwrap().len(),1);
        assert_eq!(std::fs::read(work.path().join(".forks").join(&request.id).join("a")).unwrap(),b"hello");
        assert_eq!(std::fs::metadata(work.path().join(".forks").join(&request.id).join("a")).unwrap().mode() & 0o7777,0o700);
        assert!(snapshot(work.path(),&request).is_err());
        std::os::unix::fs::symlink("/etc/passwd",work.path().join("evil")).unwrap();
        assert!(snapshot(work.path(),&Request{id:"cd".repeat(16),operation:Operation::CloneParent}).is_err());
        assert!(!work.path().join(".forks").join("cd".repeat(16)).exists());
    }
}
