//! Linux protected-key mode loads only an immutable, freshly verified snapshot.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::OnceLock;

use sha2::{Digest, Sha256};

static LOADED: OnceLock<File> = OnceLock::new();

const SEALS: libc::c_int =
    libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;

// Called under the shared runtime initialization lock, before the one ort load.
pub(super) fn prepare(path: &Path) -> Result<std::path::PathBuf, String> {
    // Retain the descriptor even if environment creation subsequently fails:
    // ort may already hold a dlopen handle referencing /proc/self/fd/N.
    let pins = crate::manifest::ort_lib_pins(crate::manifest::current_target())
        .ok_or_else(|| "這個平台沒有可用的 ONNX Runtime".to_string())?;
    let sealed = snapshot(path, pins)?;
    let proc_path = format!("/proc/self/fd/{}", sealed.as_raw_fd());
    LOADED.set(sealed).map_err(|_| "ONNX Runtime 初始化狀態衝突".to_string())?;
    Ok(proc_path.into())
}

pub(super) fn verify_loaded() -> Result<(), String> {
    // A previously loaded module can have the same /proc/self/fd/N name after
    // descriptor reuse. Identify the mapping itself, never dladdr's pathname.
    let file = LOADED.get().ok_or("ONNX Runtime 缺少封存檔案")?;
    verify_symbol_identity(ort::api().CreateEnv as *const () as usize, file)
}

#[derive(Debug, PartialEq, Eq)]
struct MappingIdentity {
    major: u64,
    minor: u64,
    inode: u64,
}

fn file_identity(file: &File) -> Result<MappingIdentity, String> {
    use std::os::unix::fs::MetadataExt;
    let meta = file.metadata().map_err(io_error)?;
    Ok(MappingIdentity {
        major: libc::major(meta.dev()) as u64,
        minor: libc::minor(meta.dev()) as u64,
        inode: meta.ino(),
    })
}

fn mapping_identity(maps: &str, symbol: usize) -> Result<MappingIdentity, String> {
    let malformed = || "ONNX Runtime 無法解析程式庫記憶體映射".to_string();
    let mut found = None;
    for line in maps.lines() {
        let mut fields = line.split_whitespace();
        let (start, end) = fields.next().and_then(|s| s.split_once('-')).ok_or_else(malformed)?;
        let start = usize::from_str_radix(start, 16).map_err(|_| malformed())?;
        let end = usize::from_str_radix(end, 16).map_err(|_| malformed())?;
        if start >= end { return Err(malformed()); }
        if symbol < start || symbol >= end { continue; }
        let permissions = fields.next().ok_or_else(malformed)?;
        if !permissions.as_bytes().get(2).is_some_and(|c| *c == b'x') {
            return Err(malformed());
        }
        let _offset = u64::from_str_radix(fields.next().ok_or_else(malformed)?, 16)
            .map_err(|_| malformed())?;
        let (major, minor) = fields.next().and_then(|s| s.split_once(':')).ok_or_else(malformed)?;
        let major = u64::from_str_radix(major, 16).map_err(|_| malformed())?;
        let minor = u64::from_str_radix(minor, 16).map_err(|_| malformed())?;
        let inode = fields.next().ok_or_else(malformed)?.parse::<u64>().map_err(|_| malformed())?;
        if inode == 0 || found.is_some() { return Err(malformed()); }
        // The remaining pathname is intentionally ignored: it can have spaces,
        // " (deleted)", or a reused descriptor name without proving identity.
        found = Some(MappingIdentity { major, minor, inode });
    }
    found.ok_or_else(malformed)
}

fn require_identity(mapped: &MappingIdentity, retained: &MappingIdentity) -> Result<(), String> {
    if mapped != retained {
        return Err("ONNX Runtime 映射並非已封存檔案，拒絕在獨立金鑰模式使用".into());
    }
    Ok(())
}

fn verify_symbol_identity(symbol: usize, retained: &File) -> Result<(), String> {
    let maps = std::fs::read_to_string("/proc/self/maps").map_err(io_error)?;
    require_identity(&mapping_identity(&maps, symbol)?, &file_identity(retained)?)
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
    fn maps_parser_handles_spaces_deleted_paths_and_boundaries() {
        let maps = "1000-2000 r--p 00000000 08:02 12 /a path/lib.so\n\
                    2000-3000 r-xp 00001000 00:0a 42 /memfd:a name (deleted)\n\
                    3000-4000 rw-p 00000000 00:00 0 [heap]\n";
        let expected = MappingIdentity { major: 0, minor: 10, inode: 42 };
        assert_eq!(mapping_identity(maps, 0x2000).unwrap(), expected);
        assert_eq!(mapping_identity(maps, 0x2fff).unwrap(), expected);
        assert!(mapping_identity(maps, 0x3000).is_err());
        assert!(mapping_identity(maps, 0x4000).is_err());
        for bad in ["broken", "2000-3000 r-xp 0 xx:02 12 /x", "2000-3000 r-xp 0 00:02 bad /x",
            "2000-3000 r-xp 0 00:02 0 /x", "2000-3000 r-xp"] {
            assert!(mapping_identity(bad, 0x2000).is_err(), "{bad}");
        }
    }

    #[test]
    fn reused_proc_fd_name_cannot_impersonate_retained_memfd() {
        let first = seal_and_verify(b"good".as_slice(), &[(4, &hash(b"good"))]).unwrap();
        let second = seal_and_verify(b"good".as_slice(), &[(4, &hash(b"good"))]).unwrap();
        let first_id = file_identity(&first).unwrap();
        let second_id = file_identity(&second).unwrap();
        let name = format!("/proc/self/fd/{}", second.as_raw_fd());
        // Even if dladdr reports the current memfd name for a previous mapping,
        // the mapping's dev/inode still belongs to the first file.
        let maps = format!("1000-2000 r-xp 0 {:x}:{:x} {} {name}",
            first_id.major, first_id.minor, first_id.inode);
        let mapped = mapping_identity(&maps, 0x1000).unwrap();
        assert!(require_identity(&mapped, &second_id).is_err());
        assert!(require_identity(&mapped, &first_id).is_ok());
        let wrong_device = MappingIdentity { major: mapped.major + 1, minor: mapped.minor, inode: mapped.inode };
        assert!(require_identity(&mapped, &wrong_device).is_err());
    }

    #[test]
    fn a_foreign_module_cannot_pass_as_the_sealed_runtime() {
        let symbol = libc::getpid as *const () as usize;
        let file = seal_and_verify(b"good".as_slice(), &[(4, &hash(b"good"))]).unwrap();
        assert!(verify_symbol_identity(symbol, &file).is_err());
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
