//! Shared no-follow checks for wiki pages and adjacent knowledge sources.
//! Parent containment remains the caller's responsibility.

use std::io;
use std::path::Path;

/// Reparse points include junctions, not just Rust's symlink file types.
pub fn is_link_metadata(meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)] {
        use std::os::windows::fs::MetadataExt;
        if is_reparse_attributes(meta.file_attributes()) { return true; }
    }
    meta.file_type().is_symlink()
}

#[cfg(windows)]
fn is_reparse_attributes(attributes: u32) -> bool {
    attributes & 0x400 != 0 // FILE_ATTRIBUTE_REPARSE_POINT, including junctions
}

#[cfg(windows)]
#[test]
fn windows_reparse_predicate_rejects_directory_and_file_reparse_points() {
    assert!(is_reparse_attributes(0x400 | 0x10)); // Junction / directory
    assert!(is_reparse_attributes(0x400 | 0x20)); // File reparse point
    assert!(!is_reparse_attributes(0x10));
    assert!(!is_reparse_attributes(0x20));
}

/// Open `full` for reading without following a symlink in its last
/// component. On unix `O_NOFOLLOW` makes the open itself fail on a link; on
/// Windows `FILE_FLAG_OPEN_REPARSE_POINT` opens the link instead of its
/// target, which the `fstat` check below then refuses.
fn open_nofollow(full: &Path) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(full)
}

/// Refuse links/reparse points and special files before and after opening.
/// Callers must use this same handle for metadata and content reads.
pub fn open_regular_file_nofollow(path: &Path) -> io::Result<std::fs::File> {
    let refused = || io::Error::new(io::ErrorKind::PermissionDenied, "not a regular no-follow file");
    let metadata = std::fs::symlink_metadata(path)?;
    if is_link_metadata(&metadata) || !metadata.is_file() { return Err(refused()); }
    let file = open_nofollow(path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound { error } else { refused() }
    })?;
    let opened = file.metadata()?;
    if is_link_metadata(&opened) || !opened.is_file() { return Err(refused()); }
    Ok(file)
}


#[cfg(all(test, unix))]
#[test]
fn nofollow_open_refuses_symlinks_and_reads_regular_files() {
    use std::io::Read;
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("page.md");
    std::fs::write(&path, "page").unwrap();
    symlink(&path, dir.path().join("alias.md")).unwrap();
    assert!(open_regular_file_nofollow(&dir.path().join("alias.md")).is_err());
    assert!(open_regular_file_nofollow(dir.path()).is_err());
    let mut file = open_regular_file_nofollow(&path).unwrap();
    assert_eq!(file.metadata().unwrap().len(), 4);
    let mut raw = String::new();
    file.read_to_string(&mut raw).unwrap();
    assert_eq!(raw, "page");
}
