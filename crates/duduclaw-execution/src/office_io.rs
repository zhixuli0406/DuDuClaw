//! Office inputs are snapshots; scripts never receive a live workspace path.
//! Outputs publish without replacement through retained authorized directory fds.
use std::{fs::File, io::{Read, Write}, os::{fd::{AsRawFd, FromRawFd}, unix::fs::{MetadataExt, OpenOptionsExt}}, path::Path};
use crate::office_helper::Request;
const MAX_DOCUMENT: u64 = 16 * 1024 * 1024;
pub(crate) struct Prepared { pub arguments: Vec<String>, output: Option<(File, String, String, String)> }
fn open(directory: &File, path: &str, flags: i32, mode: u64) -> Result<File, String> {
    let path = std::ffi::CString::new(path).map_err(|_| "invalid path")?;
    #[repr(C)]
    struct How { flags: u64, mode: u64, resolve: u64 }
    let how = How { flags: flags as u64, mode, resolve: 0x01 | 0x02 | 0x04 | 0x08 };
    // SAFETY: retained directory, terminated string, initialized Linux open_how.
    let fd = unsafe { libc::syscall(libc::SYS_openat2, directory.as_raw_fd(), path.as_ptr(), &how, std::mem::size_of::<How>()) };
    if fd < 0 { return Err("workspace file unavailable".into()); }
    // SAFETY: successful syscall returns a fresh owned file descriptor.
    Ok(unsafe { File::from_raw_fd(fd as i32) })
}
fn directory(root: &File, path: &str) -> Result<File, String> {
    open(root, path, libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW, 0)
}
impl Prepared {
    pub(crate) fn prepare(request: &Request, skill: &str, script: &str, workspace: &Path, temporary: &Path) -> Result<Self, String> {
        let (input, output, format) = crate::office_helper::paths(request, skill, script)?;
        let root = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(workspace).map_err(|_| "workspace unavailable")?;
        let source = open(&root, &input, libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC, 0)?;
        let metadata = source.metadata().map_err(|_| "input unavailable")?;
        if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() > MAX_DOCUMENT { return Err("invalid input".into()); }
        // B2 supplies a new private /tmp per execution. No caller controls this
        // path, and create_dir fails on any preexisting entry.
        std::fs::create_dir(temporary).map_err(|_| "temporary workspace unavailable")?;
        let extension = Path::new(&input).extension().and_then(|v| v.to_str()).unwrap_or("data");
        if extension.len() > 16 || !extension.bytes().all(|b| b.is_ascii_alphanumeric()) { return Err("invalid extension".into()); }
        let snapshot = temporary.join(format!("input.{extension}"));
        let mut destination = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&snapshot).map_err(|_| "snapshot unavailable")?;
        let mut bytes = Vec::new(); source.take(MAX_DOCUMENT + 1).read_to_end(&mut bytes).map_err(|_| "input unavailable")?;
        if bytes.len() as u64 > MAX_DOCUMENT { return Err("input too large".into()); }
        destination.write_all(&bytes).map_err(|_| "snapshot unavailable")?;
        drop(destination);
        let mut arguments = vec![snapshot.to_str().ok_or("temporary workspace unavailable")?.to_owned()];
        if let Some(format) = format { arguments.extend(["--format".into(), format]); }
        let output = if script == "extract" { None } else {
            let result = if script == "create" {
                let extension = match skill { "docx" => "docx", "xlsx" => "xlsx", "pptx" => "pptx", "pdf" => "pdf", _ => return Err("invalid skill".into()) };
                let path = temporary.join(format!("output.{extension}"));
                arguments.extend(["--out".into(), path.to_str().ok_or("invalid temporary path")?.into()]); path
            } else {
                arguments.extend(["--outdir".into(), temporary.to_str().ok_or("invalid temporary path")?.into()]);
                temporary.join("input.pdf")
            };
            let selected = if script == "create" { output.ok_or("missing output")? } else {
                let original = Path::new(&input).file_stem().and_then(|v| v.to_str()).ok_or("invalid input name")?;
                match output { Some(dir) => format!("{dir}/{original}.pdf"), None => Path::new(&input).with_extension("pdf").to_str().ok_or("invalid input name")?.into() }
            };
            let path = Path::new(&selected);
            let parent = path.parent().filter(|v| !v.as_os_str().is_empty()).map(|v| v.to_str().ok_or("invalid output")).transpose()?.unwrap_or(".");
            let parent = directory(&root, parent)?;
            Some((parent, path.file_name().and_then(|v| v.to_str()).ok_or("invalid output")?.into(), result.to_str().ok_or("invalid output")?.into(), selected))
        };
        Ok(Self { arguments, output })
    }
    pub(crate) fn publish(self) -> Result<Option<String>, String> {
        let Some((parent, name, temporary, selected)) = self.output else { return Ok(None); };
        let source = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(temporary).map_err(|_| "output unavailable")?;
        let metadata = source.metadata().map_err(|_| "output unavailable")?;
        if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() > MAX_DOCUMENT { return Err("invalid output".into()); }
        let mut bytes = Vec::new(); source.take(MAX_DOCUMENT + 1).read_to_end(&mut bytes).map_err(|_| "output unavailable")?;
        if bytes.len() as u64 > MAX_DOCUMENT { return Err("output too large".into()); }
        // Create a private staging entry, fsync, then atomically publish without
        // replacing any existing file/link. Remove our staging entry on failure.
        let staging = format!(".office-{}", uuid::Uuid::new_v4().simple());
        let mut destination = open(&parent, &staging, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC, 0o600)?;
        let staging_name = std::ffi::CString::new(staging).map_err(|_| "invalid output")?;
        let result = (|| {
            destination.write_all(&bytes).and_then(|_| destination.sync_all()).map_err(|_| "output publication failed")?;
            let name = std::ffi::CString::new(name).map_err(|_| "invalid output")?;
            // SAFETY: both directories are retained; names are terminated;
            // RENAME_NOREPLACE refuses existing files and symlinks atomically.
            if unsafe { libc::syscall(libc::SYS_renameat2, parent.as_raw_fd(), staging_name.as_ptr(),
                parent.as_raw_fd(), name.as_ptr(), 1u32) } != 0 { return Err("output publication failed"); }
            parent.sync_all().map_err(|_| "output publication failed")?;
            Ok(())
        })();
        if result.is_err() {
            // SAFETY: only our freshly-created random staging name is removed.
            unsafe { libc::unlinkat(parent.as_raw_fd(), staging_name.as_ptr(), 0); }
        }
        result?;
        Ok(Some(selected))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn office_snapshots_reject_workspace_links_and_publish_without_replacement() {
        use std::os::unix::fs::symlink;
        let home = tempfile::tempdir().unwrap();
        let work = home.path().join("work"); std::fs::create_dir(&work).unwrap();
        std::fs::write(home.path().join("secret.md"), "secret").unwrap();
        symlink(home.path().join("secret.md"), work.join("input.md")).unwrap();
        let request = Request { args: vec!["input.md".into(), "--out".into(), "output.docx".into()] };
        assert!(Prepared::prepare(&request, "docx", "create", &work, &home.path().join("tmp")).is_err());
        std::fs::remove_file(work.join("input.md")).unwrap(); std::fs::write(work.join("input.md"), "hello").unwrap();
        let temporary = home.path().join("tmp");
        let prepared = Prepared::prepare(&request, "docx", "create", &work, &temporary).unwrap();
        assert_eq!(std::fs::read(temporary.join("input.md")).unwrap(), b"hello");
        std::fs::write(temporary.join("output.docx"), "document").unwrap();
        symlink(home.path().join("secret.md"), work.join("output.docx")).unwrap();
        assert!(prepared.publish().is_err());
        assert_eq!(std::fs::read(home.path().join("secret.md")).unwrap(), b"secret");
    }
}
