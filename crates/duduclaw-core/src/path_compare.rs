//! Filesystem path comparisons for spawn gates. Comparison keys are never
//! used for filesystem access: stripping a verbatim prefix can change Win32
//! filename semantics. Keep the canonical path for all I/O.

use std::io;
use std::path::{Path, PathBuf};

pub fn canonicalize(path: &Path) -> io::Result<PathBuf> {
    dunce::canonicalize(path)
}

/// Child-facing spelling only; security snapshots retain their canonical path.
/// Simplify Windows verbatim disk paths only when dunce proves equivalence.
/// Paths requiring verbatim semantics and all non-Windows paths stay unchanged.
pub fn spawn_path(path: &Path) -> &Path {
    dunce::simplified(path)
}

/// Resolve an absent leaf through its closest existing ancestor. Only
/// NotFound permits a fallback; permissions, malformed paths and I/O errors
/// must reach the gate. This also resolves 8.3 names above an absent `run`.
pub fn canonicalize_allow_missing(path: &Path) -> io::Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    let mut cursor = absolute.as_path();
    let mut missing = Vec::new();
    loop {
        match canonicalize(cursor) {
            Ok(mut resolved) => {
                for name in missing.into_iter().rev() { resolved.push(name); }
                return Ok(resolved);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let Some(name) = cursor.file_name() else { return Err(error) };
                missing.push(name.to_os_string());
                let Some(parent) = cursor.parent() else { return Err(error) };
                cursor = parent;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Windows syntax classification, deliberately independent of the host OS.
/// Unknown device namespaces are never interpreted as local disk paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowsPathKind {
    Absolute,
    DriveRelative,
    RootRelative,
    Relative,
    Unc,
    UnsupportedNamespace,
}

pub fn classify_windows_path(path: &str) -> WindowsPathKind {
    use WindowsPathKind::*;
    let normalized = if path.starts_with(r"\\?\") {
        path.to_owned()
    } else {
        path.replace('/', "\\")
    };
    let disk = if let Some(rest) = normalized.strip_prefix(r"\\?\") {
        if rest.strip_prefix(r"UNC\").is_some() { return Unc; }
        rest
    } else if normalized.starts_with(r"\\.\") {
        return UnsupportedNamespace;
    } else if normalized.starts_with(r"\\") {
        return Unc;
    } else {
        normalized.as_str()
    };
    if let Some((letter, rest)) = disk.split_once(':') {
        if letter.len() == 1 && letter.as_bytes()[0].is_ascii_alphabetic() {
            return if rest.starts_with('\\') { Absolute } else { DriveRelative };
        }
    }
    if normalized.starts_with(r"\\?\") { return UnsupportedNamespace; }
    if normalized.starts_with('\\') { RootRelative } else { Relative }
}

/// Only positively identified local drives can support a containment proof.
/// Call on both the configured and canonical spelling (junctions may cross
/// namespaces). A missing/unknown drive is not evidence of locality.
#[cfg(windows)]
pub fn is_local_windows_path(path: &Path) -> io::Result<bool> {
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Component, Prefix};
    use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;
    if classify_windows_path(path_text(path)?) != WindowsPathKind::Absolute
        || !path.is_absolute() { return Ok(false); }
    let Some(Component::Prefix(prefix)) = path.components().next() else { return Ok(false) };
    if !matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)) { return Ok(false); }
    let mut root = PathBuf::from(prefix.as_os_str());
    root.push(r"\");
    let wide: Vec<u16> = root.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: `wide` is a live, NUL-terminated root directory string.
    let kind = unsafe { GetDriveTypeW(wide.as_ptr()) };
    Ok(is_local_windows_drive_type(kind))
}

#[cfg(windows)]
fn is_local_windows_drive_type(kind: u32) -> bool {
    use windows_sys::Win32::System::WindowsProgramming::{
        DRIVE_CDROM, DRIVE_FIXED, DRIVE_RAMDISK, DRIVE_REMOVABLE,
    };
    matches!(kind, DRIVE_FIXED | DRIVE_REMOVABLE | DRIVE_CDROM | DRIVE_RAMDISK)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileIdentity {
    #[cfg(unix)]
    Unix { device: u64, inode: u64 },
    #[cfg(windows)]
    Windows128 { volume: u64, id: [u8; 16] },
    #[cfg(windows)]
    WindowsLegacy { volume: u32, index: u64 },
}

impl FileIdentity {
    /// Legacy Windows IDs can be truncated or unavailable on some volumes.
    /// A mismatch involving one must never justify a containment exemption.
    pub fn is_strong(self) -> bool {
        match self {
            #[cfg(unix)]
            Self::Unix { .. } => true,
            #[cfg(windows)]
            Self::Windows128 { volume, id } => volume != 0 && id != [0; 16],
            #[cfg(windows)]
            Self::WindowsLegacy { .. } => false,
        }
    }
}

#[cfg(unix)]
pub fn file_identity(path: &Path) -> io::Result<FileIdentity> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(path)?;
    Ok(FileIdentity::Unix { device: metadata.dev(), inode: metadata.ino() })
}

#[cfg(not(any(unix, windows)))]
pub fn file_identity(_path: &Path) -> io::Result<FileIdentity> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "file identity unavailable"))
}

/// Compare already resolved paths. On Windows use component-wise Unicode
/// `str::to_lowercase` (including ASCII), not locale-sensitive casing or a
/// claim to reproduce NTFS's upcase table. File identity backs up denials;
/// callers granting an exemption must additionally check identity.
pub fn starts_with(path: &Path, base: &Path) -> io::Result<bool> {
    #[cfg(windows)]
    { Ok(windows_components(path_text(path)?).starts_with(&windows_components(path_text(base)?))) }
    #[cfg(not(windows))]
    { Ok(path.starts_with(base)) }
}

pub fn equal(path: &Path, other: &Path) -> io::Result<bool> {
    Ok(starts_with(path, other)? && starts_with(other, path)?)
}

/// Stable hash input for a resolved Windows path, never an I/O path.
#[cfg(windows)]
pub fn comparison_key(path: &Path) -> io::Result<String> {
    Ok(windows_components(path_text(path)?).join("\0"))
}

#[cfg(windows)]
fn path_text(path: &Path) -> io::Result<&str> {
    path.to_str().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "non-Unicode path"))
}

/// Parse Windows spellings independently of the host OS so Unix CI exercises
/// the same comparison logic. `dunce` handles safe disk simplification during
/// canonicalization; the comparison key also unifies verbatim UNC and long
/// disk paths that dunce deliberately leaves verbatim. Other namespaces keep
/// their prefix. No short-name expansion or dot resolution is attempted here.
#[cfg(any(windows, test))]
fn windows_components(path: &str) -> Vec<String> {
    let separators = if path.starts_with(r"\\?\") {
        path.to_owned()
    } else {
        path.replace('/', "\\")
    };
    let plain = if let Some(unc) = separators.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else if let Some(disk) = separators.strip_prefix(r"\\?\").filter(|rest| {
        let mut chars = rest.chars();
        chars.next().is_some_and(|c| c.is_ascii_alphabetic())
            && chars.next() == Some(':') && chars.next() == Some('\\')
    }) {
        disk.to_owned()
    } else {
        separators
    };
    let mut parts = plain.split('\\');
    let first = parts.next().unwrap_or("");
    // Preserve a drive's root separator: C: (drive-relative) is not C:\.
    let rooted_disk = first.len() == 2 && first.as_bytes()[0].is_ascii_alphabetic()
        && first.ends_with(':') && plain.contains('\\');
    let mut key = if rooted_disk {
        vec![format!("{}\\", first.to_lowercase())]
    } else {
        vec![first.to_lowercase()]
    };
    // Leading empty components distinguish UNC, rooted and relative paths;
    // only trailing separators are irrelevant to directory containment.
    key.extend(parts.map(str::to_lowercase));
    while key.len() > 1 && key.last().is_some_and(String::is_empty) { key.pop(); }
    key
}

/// Bind a handle's final DOS path to the validated spelling using exact UTF-16.
/// Only the verbatim disk prefix, one trailing separator and ASCII drive-letter
/// case are ignored. Component case and Unicode normalization remain significant.
/// None means no local disk namespace can be proved. Pure for Unix CI coverage.
#[cfg(any(windows, test))]
pub fn same_final_path_exact(final_path: &str, canonical: &str) -> Option<bool> {
    fn disk_path(path: &str) -> Option<&str> {
        if classify_windows_path(path) != WindowsPathKind::Absolute { return None; }
        let disk = path.strip_prefix(r"\\?\").unwrap_or(path);
        let bytes = disk.as_bytes();
        if bytes.len() < 3 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
            return None;
        }
        Some(disk.strip_suffix('\\').or_else(|| disk.strip_suffix('/')).unwrap_or(disk))
    }
    let final_path = disk_path(final_path)?;
    let canonical = disk_path(canonical)?;
    // Only the ASCII drive letter is folded; compare every remaining code unit.
    Some(final_path.as_bytes()[0].eq_ignore_ascii_case(&canonical.as_bytes()[0])
        && final_path.encode_utf16().skip(1).eq(canonical.encode_utf16().skip(1)))
}

/// Follow reparse points and obtain the final path and strong identity from
/// ONE handle, refusing nonlocal final paths with ErrorKind::Unsupported.
/// Callers must compare the final path with the canonical spelling before
/// using this snapshot.
/// Keep the verbatim final path for I/O; comparison keys are never I/O paths.
#[cfg(windows)]
pub fn windows_directory_identity(path: &Path) -> io::Result<(PathBuf, FileIdentity)> {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, GetFinalPathNameByHandleW, GetFileInformationByHandleEx,
        FileIdInfo, FileStandardInfo, FILE_ID_INFO, FILE_STANDARD_INFO,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_NAME_NORMALIZED, FILE_READ_ATTRIBUTES,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, VOLUME_NAME_DOS,
    };
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "NUL in directory path"));
    }
    wide.push(0);
    // SAFETY: the path is NUL-terminated; optional pointers are null. Omitting
    // OPEN_REPARSE_POINT intentionally opens the final target, not the link.
    let raw = unsafe { CreateFileW(wide.as_ptr(), FILE_READ_ATTRIBUTES,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, std::ptr::null(),
        OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, std::ptr::null_mut()) };
    if raw == INVALID_HANDLE_VALUE { return Err(io::Error::last_os_error()); }
    // SAFETY: CreateFileW returned a valid owned handle, closed on every exit.
    let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut standard = std::mem::MaybeUninit::<FILE_STANDARD_INFO>::uninit();
    // SAFETY: the live handle and output buffer match FileStandardInfo.
    if unsafe { GetFileInformationByHandleEx(handle.as_raw_handle(), FileStandardInfo,
        standard.as_mut_ptr().cast(), std::mem::size_of::<FILE_STANDARD_INFO>() as u32) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful call initialized the structure.
    if unsafe { standard.assume_init() }.Directory == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a directory"));
    }
    // A bounded buffer also fails closed if the name changes/grows during the
    // query. No second open, truncated name, lossy UTF-16 or namespace fallback.
    let mut buffer = vec![0u16; 32768];
    // SAFETY: the live handle and writable buffer remain valid for the call.
    let length = unsafe { GetFinalPathNameByHandleW(handle.as_raw_handle(),
        buffer.as_mut_ptr(), buffer.len() as u32, FILE_NAME_NORMALIZED | VOLUME_NAME_DOS) };
    if length == 0 { return Err(io::Error::last_os_error()); }
    if length as usize >= buffer.len() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "final directory path too long"));
    }
    buffer.truncate(length as usize);
    let final_path = PathBuf::from(OsString::from_wide(&buffer));
    // Derive locality from the FINAL path, never the input's drive prefix.
    // UNC/device/GUID names are refused before GetDriveTypeW is called.
    if !is_local_windows_path(&final_path)? {
        return Err(io::Error::new(io::ErrorKind::Unsupported, "nonlocal final directory path"));
    }
    let mut info = std::mem::MaybeUninit::<FILE_ID_INFO>::uninit();
    // SAFETY: this is the SAME handle used for the final path; the output
    // buffer and size match FileIdInfo. There is no legacy identity fallback.
    if unsafe { GetFileInformationByHandleEx(handle.as_raw_handle(), FileIdInfo,
        info.as_mut_ptr().cast(), std::mem::size_of::<FILE_ID_INFO>() as u32) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful call initialized the structure.
    let info = unsafe { info.assume_init() };
    let identity = FileIdentity::Windows128 {
        volume: info.VolumeSerialNumber, id: info.FileId.Identifier,
    };
    if !identity.is_strong() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "incomplete directory identity"));
    }
    Ok((final_path, identity))
}

/// Follow directory aliases/reparse points just as canonicalization does.
/// Open with metadata access only, including directories, and close via RAII.
#[cfg(windows)]
pub fn file_identity(path: &Path) -> io::Result<FileIdentity> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, GetFileInformationByHandleEx, FileIdInfo,
        BY_HANDLE_FILE_INFORMATION, FILE_ID_INFO,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    let file = std::fs::OpenOptions::new().access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS).open(path)?;
    let mut info = std::mem::MaybeUninit::<FILE_ID_INFO>::uninit();
    // SAFETY: the live handle and buffer match the FileIdInfo structure/size.
    if unsafe { GetFileInformationByHandleEx(file.as_raw_handle(), FileIdInfo,
        info.as_mut_ptr().cast(), std::mem::size_of::<FILE_ID_INFO>() as u32) } != 0 {
        // SAFETY: the successful API call initialized the entire structure.
        let info = unsafe { info.assume_init() };
        return Ok(FileIdentity::Windows128 {
            volume: info.VolumeSerialNumber, id: info.FileId.Identifier,
        });
    }
    let error = io::Error::last_os_error();
    use windows_sys::Win32::Foundation::{ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED};
    // Only explicit unsupported-operation errors permit the weaker API.
    // Invalid parameters, permissions and I/O errors remain errors.
    if !matches!(error.raw_os_error().map(|code| code as u32),
        Some(ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED)) { return Err(error); }
    let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    // SAFETY: the owned file handle is live and the output buffer is valid.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful API call initialized the entire structure.
    let info = unsafe { info.assume_init() };
    Ok(FileIdentity::WindowsLegacy { volume: info.dwVolumeSerialNumber,
        index: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_path_preserves_native_paths() {
        for path in [Path::new("/tmp/workspace"), Path::new("relative/workspace")] {
            assert_eq!(spawn_path(path), path);
        }
    }

    #[cfg(windows)]
    #[test]
    fn spawn_path_simplifies_only_lossless_windows_disk_paths() {
        assert_eq!(spawn_path(Path::new(r"\\?\C:\x\y")), Path::new(r"C:\x\y"));
        for path in [r"\\?\C:\CON\x", r"\\?\C:\x.\y", r"\\?\C:\x \y", r"\\?\UNC\server\share\x"] {
            assert_eq!(spawn_path(Path::new(path)), Path::new(path));
        }
        let long = format!(r"\\?\C:\{}\{}", "x".repeat(200), "y".repeat(100));
        assert_eq!(spawn_path(Path::new(&long)), Path::new(&long));
    }

    #[cfg(unix)]
    #[test]
    fn spawn_path_is_a_noop_on_unix() {
        let path = Path::new("/tmp/工作目錄/🙂");
        assert!(std::ptr::eq(spawn_path(path), path));
    }

    #[test]
    fn same_final_path_exact_normalizes_only_disk_prefix_trailing_separator_and_drive_case() {
        for (final_path, canonical) in [
            (r"\\?\C:\Work\Root", r"C:\Work\Root"),
            (r"C:\Work\Root\", r"C:\Work\Root"),
            (r"c:\Work\Root", r"C:\Work\Root"),
        ] {
            assert_eq!(same_final_path_exact(final_path, canonical), Some(true));
            assert_eq!(same_final_path_exact(canonical, final_path), Some(true));
        }
    }

    #[test]
    fn same_final_path_exact_preserves_component_case_and_separators() {
        // The former windows_final_path_matches_normalized_local_paths test
        // accepted component case/slash changes; snapshot binding must be exact.
        for (final_path, canonical) in [
            (r"C:\Work\root", r"C:\Work\Root"),
            (r"C:\X\", r"c:\x"),
            (r"\\?\C:\ÉCOLE\測試\", "c:/école/測試"),
            (r"C:\Work\Root", "C:/Work/Root"),
            (r"C:\Work\Root\\", r"C:\Work\Root"),
            (r"\\?\C:\other", r"C:\x"),
            (r"\\?\D:\x", r"C:\x"),
            (r"C:\xy", r"C:\x"),
        ] {
            assert_eq!(same_final_path_exact(final_path, canonical), Some(false));
        }
    }

    #[test]
    fn same_final_path_exact_preserves_unicode_normalization() {
        assert_eq!(same_final_path_exact(r"C:\Work\Röot", "C:\\Work\\Ro\u{308}ot"), Some(false));
    }

    #[test]
    fn windows_final_path_rejects_remote_and_device_namespaces() {
        // No SMB share is needed: exercise the exact classifier used by the
        // gate for remote final names, including a loopback administrative share.
        for final_path in [
            r"\\srv\sh\x", r"\\?\UNC\srv\sh\x", r"\\localhost\C$\x",
            r"\\?\UNC\localhost\C$\x", r"\\?\Volume{123}\x",
            r"\\.\C:\x", r"\Device\HarddiskVolume1\x", "C:", r"C:x", r"\x",
        ] {
            assert_eq!(same_final_path_exact(final_path, r"C:\x"), None, "{final_path}");
            assert_eq!(same_final_path_exact(r"C:\x", final_path), None, "{final_path}");
        }
    }

    #[test]
    fn windows_path_classification_is_host_independent() {
        use WindowsPathKind::*;
        for (path, expected) in [
            ("C:", DriveRelative), (r"C:foo", DriveRelative),
            (r"\foo", RootRelative), ("/foo", RootRelative), ("foo", Relative),
            (r"C:\x", Absolute), ("C:/x", Absolute),
            (r"\\server\share\x", Unc), ("//server/share/x", Unc),
            (r"\\?\C:\x", Absolute), (r"\\?\UNC\s\sh\x", Unc),
            (r"\\.\C:\x", UnsupportedNamespace),
            (r"\\?\Volume{123}\x", UnsupportedNamespace),
            ("工作資料夾", Relative),
        ] {
            assert_eq!(classify_windows_path(path), expected, "{path}");
        }
    }

    #[test]
    fn windows_verbatim_disk_and_unc_have_plain_comparison_keys() {
        for (verbatim, plain) in [
            (r"\\?\C:\Users\worker\run", r"C:\Users\worker\run"),
            (r"\\?\UNC\server\share\home\run", r"\\server\share\home\run"),
        ] {
            assert_eq!(windows_components(verbatim), windows_components(plain));
        }
        let special = r"\\?\Volume{123}\home";
        assert_ne!(windows_components(special), windows_components(r"Volume{123}\home"));
        assert_ne!(windows_components(special), windows_components(r"\\Volume{123}\home"));
    }

    #[test]
    fn windows_keys_fold_case_and_ignore_trailing_separators() {
        assert_eq!(windows_components(r"\\?\C:\ÉCOLE\測試\RUN\\"),
            windows_components("c:/école/測試/run/"));
        assert_eq!(windows_components(r"\\?\UNC\SERVER\Share\"),
            windows_components(r"\\server\share"));
    }

    #[test]
    fn windows_containment_respects_components_and_volume_roots() {
        let run = windows_components(r"\\?\C:\home\run");
        assert!(run.starts_with(&windows_components(r"c:\")));
        assert!(run.starts_with(&windows_components(r"C:\HOME\")));
        for other in [r"C:\ho", r"C:\home\runner", r"D:\", "C:", "/"] {
            assert!(!run.starts_with(&windows_components(other)), "{other}");
        }
        let unc = windows_components(r"\\?\UNC\server\share\home\run");
        assert!(unc.starts_with(&windows_components(r"\\SERVER\SHARE\")));
        assert!(!unc.starts_with(&windows_components(r"\\server\share2")));
        assert!(!unc.starts_with(&windows_components(r"\\other\share")));
    }

    #[cfg(windows)]
    #[test]
    fn windows_directory_identity_agrees_across_case_and_verbatim_paths() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("MixedCase");
        std::fs::create_dir(&dir).unwrap();
        let canonical = std::fs::canonicalize(&dir).unwrap();
        let lower = PathBuf::from(canonical.to_str().unwrap().to_lowercase());
        assert_eq!(file_identity(&dir).unwrap(), file_identity(&canonical).unwrap());
        assert_eq!(file_identity(&dir).unwrap(), file_identity(&lower).unwrap());
    }

    #[cfg(windows)]
    #[test]
    fn windows_directory_identity_uses_file_id_info() {
        let base = tempfile::tempdir().unwrap();
        let id = file_identity(base.path()).unwrap();
        assert!(matches!(id, FileIdentity::Windows128 { .. }), "FileIdInfo required: {id:?}");
        assert!(id.is_strong(), "partial identity: {id:?}");
        let other = tempfile::tempdir().unwrap();
        assert_ne!(id, file_identity(other.path()).unwrap());
    }

    #[cfg(windows)]
    #[test]
    fn windows_legacy_and_partial_identities_are_weak() {
        for id in [
            FileIdentity::WindowsLegacy { volume: 1, index: 2 },
            FileIdentity::Windows128 { volume: 0, id: [1; 16] },
            FileIdentity::Windows128 { volume: 1, id: [0; 16] },
        ] {
            assert!(!id.is_strong(), "{id:?}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_remote_and_unknown_drive_types_are_refused() {
        use windows_sys::Win32::System::WindowsProgramming::{
            DRIVE_FIXED, DRIVE_NO_ROOT_DIR, DRIVE_REMOTE, DRIVE_UNKNOWN,
        };
        assert!(is_local_windows_drive_type(DRIVE_FIXED));
        for kind in [DRIVE_REMOTE, DRIVE_UNKNOWN, DRIVE_NO_ROOT_DIR, u32::MAX] {
            assert!(!is_local_windows_drive_type(kind));
        }
    }
}
