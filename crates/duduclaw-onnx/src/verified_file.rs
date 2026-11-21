//! Handle-based verification for the portable, path-based runtime loader.

use std::fs::{File, Metadata, OpenOptions};
use std::path::Path;
use std::time::SystemTime;

use super::sha256_reader;

#[derive(Debug, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
    size: u64,
    modified: SystemTime,
}

#[derive(Debug)]
pub(crate) struct VerifiedLibrary {
    // Retained by runtime for the entire process lifetime, even on load failure.
    _file: File,
    identity: Identity,
}

fn io_error(e: std::io::Error) -> String {
    format!("無法驗證 ONNX Runtime 執行庫：{e}；請重新安裝元件")
}

pub(super) fn open_regular(path: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // OPEN_REPARSE_POINT opens the link itself. Deny write/delete sharing
        // while the verified handle is retained, including during LoadLibrary.
        options.custom_flags(0x00200000).share_mode(1);
    }
    let file = options.open(path).map_err(io_error)?;
    regular(&file.metadata().map_err(io_error)?)?;
    Ok(file)
}

fn regular(meta: &Metadata) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return Err("ONNX Runtime 執行庫是 reparse point；請重新安裝元件".into());
        }
    }
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err("ONNX Runtime 執行庫不是一般檔案；請重新安裝元件".into());
    }
    Ok(())
}

#[cfg(unix)]
pub(super) fn owned_private(meta: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: geteuid has no arguments and no memory preconditions.
    meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o022 == 0
}

fn permissions(path: &Path, meta: &Metadata) -> Result<(), String> {
    #[cfg(unix)]
    {
        let dir = std::fs::symlink_metadata(path.parent().ok_or("執行庫沒有父目錄")?)
            .map_err(io_error)?;
        if !dir.is_dir() || !owned_private(&dir) || !owned_private(meta) {
            return Err("ONNX Runtime 目錄或執行庫的擁有者／寫入權限不安全；請重新安裝元件".into());
        }
    }
    #[cfg(not(unix))]
    let _ = (path, meta);
    Ok(())
}

#[cfg(unix)]
fn identity(_file: &File, meta: &Metadata) -> Result<Identity, String> {
    use std::os::unix::fs::MetadataExt;
    Ok(Identity {
        device: meta.dev(), inode: meta.ino(), size: meta.len(),
        modified: meta.modified().map_err(io_error)?,
    })
}

#[cfg(windows)]
fn identity(file: &File, meta: &Metadata) -> Result<Identity, String> {
    use std::os::windows::io::AsRawHandle;
    // BY_HANDLE_FILE_INFORMATION, laid out as thirteen Windows DWORDs.
    // This API is used instead of the unstable std volume/file-index accessors.
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandle(handle: *mut std::ffi::c_void, info: *mut u32) -> i32;
        fn GetFileType(handle: *mut std::ffi::c_void) -> u32;
    }
    let mut info = [0u32; 13];
    // SAFETY: the live File owns the handle; info has the API's exact size and alignment.
    if unsafe { GetFileType(file.as_raw_handle()) } != 1
        || unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0
    {
        return Err(io_error(std::io::Error::last_os_error()));
    }
    Ok(Identity {
        device: u64::from(info[7]),
        inode: (u64::from(info[11]) << 32) | u64::from(info[12]),
        size: meta.len(), modified: meta.modified().map_err(io_error)?,
    })
}

#[cfg(not(any(unix, windows)))]
fn identity(_file: &File, _meta: &Metadata) -> Result<Identity, String> {
    Err("這個平台不支援 ONNX Runtime 檔案身分驗證".into())
}

/// Hash through a no-follow handle on every initialization, never a path cache.
/// Retain this handle for the process lifetime and call check_path immediately
/// after init_from. This narrows but does not eliminate the race on platforms
/// whose loader only accepts a path (including macOS and Windows). Protected
/// Linux installs use the sealed memfd runtime to remove that window instead.
pub(crate) fn verify_for_load(path: &Path, pins: &[(u64, &str)]) -> Result<VerifiedLibrary, String> {
    let mut file = open_regular(path)?;
    let meta = file.metadata().map_err(io_error)?;
    permissions(path, &meta)?;
    let before = identity(&file, &meta)?;
    if !pins.iter().any(|p| p.0 == meta.len()) {
        return Err("ONNX Runtime 執行庫大小與版本紀錄不符；請重新安裝元件".into());
    }
    let sha = sha256_reader(&mut file).map_err(|e| e.to_string())?;
    if !crate::manifest::matches_lib_pin(pins, meta.len(), &sha)
        || identity(&file, &file.metadata().map_err(io_error)?)? != before
    {
        return Err("ONNX Runtime 執行庫雜湊或身分不符；請重新安裝元件".into());
    }
    let verified = VerifiedLibrary { _file: file, identity: before };
    verified.check_path(path)?;
    Ok(verified)
}

impl VerifiedLibrary {
    pub(crate) fn check_path(&self, path: &Path) -> Result<(), String> {
        let meta = std::fs::symlink_metadata(path).map_err(io_error)?;
        regular(&meta)?;
        permissions(path, &meta)?;
        #[cfg(unix)]
        let current = identity(&self._file, &meta)?;
        #[cfg(not(unix))]
        let current = {
            let file = open_regular(path)?;
            identity(&file, &file.metadata().map_err(io_error)?)?
        };
        if current != self.identity {
            return Err("ONNX Runtime 執行庫在載入時被替換；請重新安裝元件並重新啟動".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn load_verification_detects_same_size_same_mtime_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lib.so");
        std::fs::write(&path, b"good").unwrap();
        let sha = format!("{:x}", Sha256::digest(b"good"));
        let pins = [(4, sha.as_str())];
        super::super::verify_lib_at(&path, &pins).unwrap();
        let time = std::fs::metadata(&path).unwrap().modified().unwrap();
        let replacement = dir.path().join("replacement");
        std::fs::write(&replacement, b"evil").unwrap();
        File::options().write(true).open(&replacement).unwrap().set_modified(time).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::rename(replacement, &path).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), time);
        assert!(verify_for_load(&path, &pins).unwrap_err().contains("雜湊"));
    }

    #[cfg(unix)]
    #[test]
    fn retained_handle_detects_path_replacement_after_verification() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lib.so");
        std::fs::write(&path, b"good").unwrap();
        let sha = format!("{:x}", Sha256::digest(b"good"));
        let verified = verify_for_load(&path, &[(4, &sha)]).unwrap();
        verified.check_path(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"good").unwrap();
        assert!(verified.check_path(&path).is_err());
        assert_eq!(verified._file.metadata().unwrap().len(), 4);
    }

    #[cfg(unix)]
    #[test]
    fn load_verification_refuses_symlink_and_special_file() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        let link = dir.path().join("lib.so");
        std::fs::write(&real, b"good").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let sha = format!("{:x}", Sha256::digest(b"good"));
        assert!(verify_for_load(&link, &[(4, &sha)]).is_err());
        assert!(open_regular(Path::new("/dev/null")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn load_verification_refuses_group_writable_directory_and_library() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lib.so");
        std::fs::write(&path, b"good").unwrap();
        let sha = format!("{:x}", Sha256::digest(b"good"));
        for (directory_mode, file_mode) in [(0o770, 0o600), (0o700, 0o660), (0o707, 0o600)] {
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(directory_mode)).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(file_mode)).unwrap();
            assert!(verify_for_load(&path, &[(4, &sha)]).unwrap_err().contains("重新安裝"));
        }
        super::super::prepare_lib_dir(dir.path()).unwrap();
        assert_eq!(std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777, 0o700);
        verify_for_load(&path, &[(4, &sha)]).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn retained_windows_handle_denies_write_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lib.dll");
        std::fs::write(&path, b"good").unwrap();
        let sha = format!("{:x}", Sha256::digest(b"good"));
        let verified = verify_for_load(&path, &[(4, &sha)]).unwrap();
        assert!(std::fs::write(&path, b"evil").is_err());
        assert!(std::fs::remove_file(&path).is_err());
        verified.check_path(&path).unwrap();
    }
}
