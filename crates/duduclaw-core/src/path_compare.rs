//! Filesystem path comparisons for spawn gates. Comparison keys are never
//! used for filesystem access: stripping a verbatim prefix can change Win32
//! filename semantics. Keep the canonical path for all I/O.

use std::io;
use std::path::{Path, PathBuf};

pub fn canonicalize(path: &Path) -> io::Result<PathBuf> {
    dunce::canonicalize(path)
}

/// Resolve an absent leaf through its closest existing ancestor. Only
/// NotFound permits a fallback; permissions, malformed paths and I/O errors
/// must reach the gate. This also resolves 8.3 names above an absent `run`.
#[cfg(windows)]
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

/// Follow directory aliases/reparse points just as canonicalization does.
/// Open with metadata access only, including directories, and close via RAII.
#[cfg(windows)]
pub fn file_identity(path: &Path) -> io::Result<(u32, u64)> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    let file = std::fs::OpenOptions::new().access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS).open(path)?;
    let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    // SAFETY: the owned file handle is live and the output buffer is valid.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful API call initialized the entire structure.
    let info = unsafe { info.assume_init() };
    Ok((info.dwVolumeSerialNumber, (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow)))
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
