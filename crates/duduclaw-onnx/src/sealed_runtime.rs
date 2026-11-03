//! Linux protected-key mode loads only an immutable, freshly verified snapshot.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use sha2::{Digest, Sha256};

const SEALS: libc::c_int =
    libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;

pub(super) fn init(path: &Path) -> Result<(), String> {
    // Keep the sealed descriptor for the process lifetime. ort retains its
    // dlopen handle globally; never let /proc/self/fd/N be reused underneath it.
    static LOADED: OnceLock<File> = OnceLock::new();
    static INIT_LOCK: Mutex<()> = Mutex::new(());
    let _lock = INIT_LOCK.lock().map_err(|_| "ONNX Runtime 初始化鎖無法使用".to_string())?;
    if LOADED.get().is_some() {
        return Ok(());
    }
    let target = crate::manifest::current_target();
    let pin = crate::manifest::ort_lib_pin(target)
        .ok_or_else(|| format!("這個平台（{target}）沒有可用的 ONNX Runtime"))?;
    let sealed = snapshot(path, pin)?;
    let proc_path = format!("/proc/self/fd/{}", sealed.as_raw_fd());
    let builder = ort::init_from(&proc_path)
        .map_err(|e| format!("載入已驗證的 ONNX Runtime 失敗：{e}"))?;
    if !builder.commit() {
        // All repository call sites must reach this loader before any ort API.
        // An independently configured environment is not evidence of safe init.
        return Err("ONNX Runtime 已由其他路徑初始化，拒絕在獨立金鑰模式使用".into());
    }
    LOADED.set(sealed).map_err(|_| "ONNX Runtime 初始化狀態衝突".to_string())?;
    Ok(())
}

fn snapshot(path: &Path, pin: (u64, &str)) -> Result<File, String> {
    // O_NONBLOCK prevents a swapped FIFO from hanging before fstat. Symlinks
    // are refused, and source mutation during copying is caught by the hash of
    // the sealed destination, not by source metadata or the install cache.
    let source = OpenOptions::new().read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path).map_err(|e| format!("無法開啟 ONNX Runtime 執行庫：{e}"))?;
    if !source.metadata().map_err(io_error)?.is_file() {
        return Err("ONNX Runtime 執行庫不是一般檔案，拒絕載入".into());
    }
    seal_and_verify(source, pin)
}

fn io_error(e: std::io::Error) -> String {
    format!("ONNX Runtime 安全載入失敗：{e}")
}

fn seal_and_verify(source: impl Read, (size, hash): (u64, &str)) -> Result<File, String> {
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
    let limit = size.checked_add(1).ok_or("ONNX Runtime 大小紀錄錯誤")?;
    let copied = std::io::copy(&mut source.take(limit), &mut file).map_err(io_error)?;
    // SAFETY: fcntl receives a live descriptor and the seal bitmask.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, SEALS) } != 0 {
        return Err(io_error(std::io::Error::last_os_error()));
    }
    // Check the immutable object itself, never the source path or its cache.
    if copied != size || file.metadata().map_err(io_error)?.len() != size {
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
    if got != hash {
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
    fn sealed_snapshot_is_verified_immutable_and_cloexec() {
        let bytes = b"pinned library fixture";
        let mut file = seal_and_verify(bytes.as_slice(), (bytes.len() as u64, &hash(bytes))).unwrap();
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
            assert!(seal_and_verify(bad, (4, &pin)).is_err());
        }
    }

    #[test]
    fn source_replacement_cannot_change_the_verified_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lib.so");
        std::fs::write(&path, b"good").unwrap();
        let file = snapshot(&path, (4, &hash(b"good"))).unwrap();
        std::fs::write(&path, b"evil").unwrap();
        assert_eq!(std::fs::read(format!("/proc/self/fd/{}", file.as_raw_fd())).unwrap(), b"good");
        assert!(snapshot(&path, (4, &hash(b"good"))).is_err());
    }

    #[test]
    fn symlinks_and_special_files_are_refused_before_copying() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("lib.so");
        std::os::unix::fs::symlink("/dev/zero", &link).unwrap();
        assert!(snapshot(&link, (4, &hash(b"good"))).is_err());
        assert!(snapshot(Path::new("/dev/zero"), (4, &hash(b"good"))).is_err());
    }
}
