//! Linux protected-key mode loads only an immutable, freshly verified snapshot.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::OnceLock;

use sha2::{Digest, Sha256};

const SEALS: libc::c_int =
    libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;

// Called under the shared runtime initialization lock, before the one ort load.
pub(super) fn prepare(path: &Path) -> Result<std::path::PathBuf, String> {
    // Retain the descriptor even if environment creation subsequently fails:
    // ort may already hold a dlopen handle referencing /proc/self/fd/N.
    static LOADED: OnceLock<File> = OnceLock::new();
    let pins = crate::manifest::ort_lib_pins(crate::manifest::current_target())
        .ok_or_else(|| "這個平台沒有可用的 ONNX Runtime".to_string())?;
    let sealed = snapshot(path, pins)?;
    let proc_path = format!("/proc/self/fd/{}", sealed.as_raw_fd());
    LOADED.set(sealed).map_err(|_| "ONNX Runtime 初始化狀態衝突".to_string())?;
    Ok(proc_path.into())
}

pub(super) fn verify_loaded(path: &Path) -> Result<(), String> {
    // rc.15 init_from silently keeps an already loaded library. Check the
    // actual API's module before creating an environment, so a foreign init
    // cannot be mistaken for the sealed load requested above.
    verify_symbol_path(ort::api().CreateEnv as *const () as *const libc::c_void, path)
}

fn verify_symbol_path(symbol: *const libc::c_void, path: &Path) -> Result<(), String> {
    use std::os::unix::ffi::OsStrExt;
    let mut info = std::mem::MaybeUninit::<libc::Dl_info>::uninit();
    // SAFETY: dladdr inspects the address without dereferencing it and writes
    // a Dl_info on success. Its returned module name remains owned by dlopen.
    let matches = unsafe {
        if libc::dladdr(symbol, info.as_mut_ptr()) == 0 {
            false
        } else {
            let info = info.assume_init();
            !info.dli_fname.is_null()
                && std::ffi::CStr::from_ptr(info.dli_fname).to_bytes() == path.as_os_str().as_bytes()
        }
    };
    if !matches {
        return Err("ONNX Runtime 並非從已封存路徑載入，拒絕在獨立金鑰模式使用".into());
    }
    Ok(())
}

fn snapshot(path: &Path, pins: &[(u64, &str)]) -> Result<File, String> {
    // O_NONBLOCK prevents a swapped FIFO from hanging before fstat. Symlinks
    // are refused, and source mutation during copying is caught by the hash of
    // the sealed destination, not by source metadata or the install cache.
    let source = OpenOptions::new().read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path).map_err(|e| format!("無法開啟 ONNX Runtime 執行庫：{e}"))?;
    if !source.metadata().map_err(io_error)?.is_file() {
        return Err("ONNX Runtime 執行庫不是一般檔案，拒絕載入".into());
    }
    seal_and_verify(source, pins)
}

fn io_error(e: std::io::Error) -> String {
    format!("ONNX Runtime 安全載入失敗：{e}")
}

fn seal_and_verify(source: impl Read, pins: &[(u64, &str)]) -> Result<File, String> {
    // SAFETY: a static NUL-terminated name and defined memfd flags. Calling
    // the syscall directly avoids the glibc 2.27 memfd_create wrapper.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_memfd_create,
            c"duduclaw-onnx".as_ptr(),
            libc::MFD_ALLOW_SEALING | libc::MFD_CLOEXEC,
        ) as libc::c_int
    };
    if fd < 0 {
        return Err(io_error(std::io::Error::last_os_error()));
    }
    // SAFETY: memfd_create returned a new descriptor owned exclusively here.
    let mut file = unsafe { File::from_raw_fd(fd) };
    let size = pins.iter().map(|p| p.0).max().ok_or("ONNX Runtime 缺少版本紀錄")?;
    let limit = size.checked_add(1).ok_or("ONNX Runtime 大小紀錄錯誤")?;
    let copied = std::io::copy(&mut source.take(limit), &mut file).map_err(io_error)?;
    // SAFETY: fcntl receives a live descriptor and the seal bitmask.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, SEALS) } != 0 {
        return Err(io_error(std::io::Error::last_os_error()));
    }
    // Check the immutable object itself, never the source path or its cache.
    if !pins.iter().any(|p| p.0 == copied) || file.metadata().map_err(io_error)?.len() != copied {
        return Err("ONNX Runtime 執行庫大小與版本紀錄不符，拒絕載入".into());
    }
    file.seek(SeekFrom::Start(0)).map_err(io_error)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buffer).map_err(io_error)?;
        if n == 0 { break; }
        digest.update(&buffer[..n]);
    }
    let got = format!("{:x}", digest.finalize());
    if !crate::manifest::matches_lib_pin(pins, copied, &got) {
        return Err("ONNX Runtime 執行庫雜湊與版本紀錄不符，拒絕載入".into());
    }
    file.seek(SeekFrom::Start(0)).map_err(io_error)?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn hash(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    #[test]
    fn a_foreign_module_cannot_pass_as_the_sealed_runtime() {
        let symbol = libc::getpid as *const () as *const libc::c_void;
        assert!(verify_symbol_path(symbol, Path::new("/proc/self/fd/999999")).is_err());
    }

    #[test]
    fn sealed_snapshot_is_verified_immutable_and_cloexec() {
        let bytes = b"pinned library fixture";
        let mut file = seal_and_verify(bytes.as_slice(), &[(bytes.len() as u64, &hash(bytes))]).unwrap();
        // SAFETY: queries on a valid owned descriptor.
        assert_eq!(unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) }, SEALS);
        assert_ne!(unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC, 0);
        assert!(file.write_all(b"changed").is_err());
        assert!(file.set_len(0).is_err());
        assert!(file.set_len(1000).is_err());
        assert_eq!(std::fs::read(format!("/proc/self/fd/{}", file.as_raw_fd())).unwrap(), bytes);
    }

    #[test]
    fn wrong_digest_short_and_oversized_inputs_fail_closed() {
        let pin = hash(b"good");
        for bad in [b"evil".as_slice(), b"goo".as_slice(), b"good-extra".as_slice()] {
            assert!(seal_and_verify(bad, &[(4, &pin)]).is_err());
        }
    }

    #[test]
    fn source_replacement_cannot_change_the_verified_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lib.so");
        std::fs::write(&path, b"good").unwrap();
        let file = snapshot(&path, &[(4, &hash(b"good"))]).unwrap();
        std::fs::write(&path, b"evil").unwrap();
        assert_eq!(std::fs::read(format!("/proc/self/fd/{}", file.as_raw_fd())).unwrap(), b"good");
        assert!(snapshot(&path, &[(4, &hash(b"good"))]).is_err());
    }

    #[test]
    fn symlinks_and_special_files_are_refused_before_copying() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("lib.so");
        std::os::unix::fs::symlink("/dev/zero", &link).unwrap();
        assert!(snapshot(&link, &[(4, &hash(b"good"))]).is_err());
        assert!(snapshot(Path::new("/dev/zero"), &[(4, &hash(b"good"))]).is_err());
    }
}
