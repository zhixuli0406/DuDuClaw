//! Shared path checks for every wiki read and write.
//!
//! A wiki page path comes from a caller (an MCP tool argument, a dashboard
//! RPC, a backlink in a page) and is joined onto a wiki directory. Every
//! surface that does that join goes through this module so the rules live in
//! one place:
//!
//! - [`validate_relative_wiki_path`] is the string-level gate: the path must
//!   be relative, with no NUL, no `..`, no percent-encoded separators, no
//!   Windows drive prefix (`C:\x`, `C:x.md`) and no `Prefix` / `RootDir` /
//!   `ParentDir` component. It does not look at the disk, so it runs first
//!   and an invalid path never reaches `stat`/`exists`.
//! - [`ensure_within_wiki_dir`] canonicalizes the nearest existing ancestor
//!   of the target (the target itself when it exists) and refuses anything
//!   that resolves outside the canonical wiki directory. Every failure to
//!   resolve denies (fail closed); only a wiki directory that does not exist
//!   yet is treated as "nothing to escape through".
//! - [`read_wiki_file_nofollow`] reads one page without following a symlink
//!   in the last component (`symlink_metadata` + `O_NOFOLLOW`, then `fstat`
//!   on the opened handle), so a page swapped for a link between the check
//!   and the read is refused instead of followed.
//! - [`prepare_wiki_write_target`] and [`atomic_write_nofollow`] validate
//!   before creating any directory, re-check after, and write the temp file
//!   with `O_EXCL` so a planted `*.md.tmp` symlink is never written through.
//!
//! Residual race: a directory component swapped for a symlink after
//! [`ensure_within_wiki_dir`] and before the open is not caught (only the
//! last component is opened with `O_NOFOLLOW`). Closing that needs
//! `openat` walking per component; the writer of such a swap already has
//! write access inside the wiki directory.

use std::io;
use std::path::{Component, Path, PathBuf};

/// Why a path was refused, as an `io::Error` kind the callers can match:
/// `InvalidInput` for a malformed path, `PermissionDenied` for a path that
/// resolves outside the wiki or is not a regular file.
fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg.to_string())
}

fn denied(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, msg.to_string())
}

/// String-level validation of a caller-supplied wiki-relative path.
///
/// Does not check the `.md` suffix or reserved names (callers differ: reads
/// may open `_index.md`, writes may not). Never touches the filesystem.
pub fn validate_relative_wiki_path(path: &str) -> std::result::Result<(), String> {
    if path.is_empty() {
        return Err("page_path is required".to_string());
    }
    if path.contains('\0') {
        return Err("NUL bytes are not allowed in page_path".to_string());
    }
    if path.contains("..") {
        return Err("Path traversal (..) is not allowed".to_string());
    }
    if path.starts_with('/') || path.starts_with('\\') {
        return Err("Absolute paths are not allowed".to_string());
    }
    let lower = path.to_ascii_lowercase();
    if lower.contains("%2e") || lower.contains("%2f") || lower.contains("%5c") {
        return Err("Percent-encoded path separators are not allowed".to_string());
    }
    // Windows drive prefixes: `C:\x` (absolute) and `C:x.md` (drive-relative,
    // resolves against that drive's current directory, i.e. outside the
    // wiki). Checked as bytes on every platform so the rule does not depend
    // on where the server runs.
    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Err("Drive-prefixed paths are not allowed".to_string());
    }
    // On Windows a `:` anywhere names an NTFS alternate data stream.
    if cfg!(windows) && path.contains(':') {
        return Err("':' is not allowed in page_path".to_string());
    }
    for component in Path::new(path).components() {
        if matches!(
            component,
            Component::Prefix(_) | Component::RootDir | Component::ParentDir
        ) {
            return Err("Path must stay inside the wiki directory".to_string());
        }
    }
    Ok(())
}

/// Refuse `full` (a path under `wiki_dir`) when its nearest existing
/// ancestor, or `full` itself, resolves outside the canonical wiki directory.
///
/// Fail closed: a wiki directory that exists but cannot be canonicalized, a
/// metadata error other than "not found", or a dangling symlink all deny. A
/// wiki directory that does not exist yet passes — nothing inside it exists,
/// so there is no link to escape through; writers create it before calling
/// [`prepare_wiki_write_target`], which checks again.
pub fn ensure_within_wiki_dir(wiki_dir: &Path, full: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(wiki_dir) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    }
    let canon_wiki = wiki_dir
        .canonicalize()
        .map_err(|_| denied("wiki directory cannot be resolved"))?;
    if !full.starts_with(wiki_dir) {
        return Err(denied("path is not under the wiki directory"));
    }
    let mut probe = full.to_path_buf();
    loop {
        match std::fs::symlink_metadata(&probe) {
            Ok(_) => break,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if probe == wiki_dir || !probe.pop() {
                    return Err(denied("wiki directory cannot be resolved"));
                }
            }
            Err(e) => return Err(e),
        }
    }
    let canon = probe
        .canonicalize()
        .map_err(|_| denied("path cannot be resolved"))?;
    if !canon.starts_with(&canon_wiki) {
        return Err(denied("resolved path escapes wiki directory"));
    }
    Ok(())
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

/// Read one wiki file as UTF-8 without following symlinks.
///
/// `rel` must pass [`validate_relative_wiki_path`] (checked here again). The
/// file must be a regular file (not a link, directory or device) both before
/// and after opening, and must stay inside the wiki directory. A missing
/// file returns `NotFound` so callers can say "page not found".
pub fn read_wiki_file_nofollow(wiki_dir: &Path, rel: &str) -> io::Result<String> {
    use std::io::Read;
    validate_relative_wiki_path(rel).map_err(|e| invalid(&e))?;
    let full = wiki_dir.join(rel);
    let meta = std::fs::symlink_metadata(&full)?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(denied("wiki page is not a regular file"));
    }
    ensure_within_wiki_dir(wiki_dir, &full)?;
    let mut file = open_nofollow(&full).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            e
        } else {
            denied("wiki page cannot be opened")
        }
    })?;
    let opened = file.metadata()?;
    if !opened.is_file() || opened.file_type().is_symlink() {
        return Err(denied("wiki page is not a regular file"));
    }
    let mut content = String::new();
    file.read_to_string(&mut content)?;
    Ok(content)
}

/// Validate `rel`, confirm the target stays inside `wiki_dir`, then create
/// its parent directories and confirm again. Nothing is created before the
/// first check passes, so a symlinked subdirectory or a drive path cannot
/// make this create directories outside the wiki. `wiki_dir` itself is
/// created first (it is operator-controlled, not caller-supplied).
pub fn prepare_wiki_write_target(wiki_dir: &Path, rel: &str) -> io::Result<PathBuf> {
    validate_relative_wiki_path(rel).map_err(|e| invalid(&e))?;
    std::fs::create_dir_all(wiki_dir)?;
    let full = wiki_dir.join(rel);
    ensure_within_wiki_dir(wiki_dir, &full)?;
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent)?;
        ensure_within_wiki_dir(wiki_dir, parent)?;
    }
    // The page itself, when present, must be a regular file or a link the
    // rename below replaces (never follows); a directory there is refused.
    match std::fs::symlink_metadata(&full) {
        Ok(m) if m.is_dir() => return Err(denied("wiki page path is a directory")),
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    Ok(full)
}

/// Write `content` to `full` through a temp file + rename. The temp file is
/// removed first (removing a link removes the link, not its target) and
/// created with `create_new` (`O_EXCL`), so a symlink planted at the temp
/// name is never written through. `rename` replaces a link at `full` rather
/// than following it.
pub fn atomic_write_nofollow(full: &Path, content: &str) -> io::Result<()> {
    use std::io::Write;
    let tmp = full.with_extension("md.tmp");
    match std::fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let write = (|| -> io::Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        f.write_all(content.as_bytes())?;
        f.sync_all()
    })();
    if let Err(e) = write {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, full).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_pass() {
        for ok in ["a.md", "entities/a.md", "auto/notes/x-y.md", "./a.md"] {
            assert!(validate_relative_wiki_path(ok).is_ok(), "{ok}");
        }
        // `..` anywhere stays refused (pre-existing, conservative rule).
        assert!(validate_relative_wiki_path("a..b.md").is_err());
    }

    #[test]
    fn traversal_absolute_nul_and_encoded_forms_are_refused() {
        for bad in [
            "",
            "../x.md",
            "a/../../x.md",
            "a/..",
            "/etc/passwd",
            "\\x.md",
            "\\\\server\\share\\x.md",
            "\\\\?\\C:\\x.md",
            "//server/share/x.md",
            "a\0b.md",
            "%2e%2e/x.md",
            "a%2Fb.md",
            "a%5cb.md",
            "..\\x.md",
        ] {
            assert!(validate_relative_wiki_path(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn windows_drive_forms_are_refused_on_every_platform() {
        for bad in ["C:\\x.md", "C:x.md", "c:/x.md", "z:", "D:sub\\x.md"] {
            assert!(validate_relative_wiki_path(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn missing_wiki_dir_is_not_an_escape_but_unresolvable_parts_deny() {
        let tmp = tempfile::TempDir::new().unwrap();
        let wiki = tmp.path().join("wiki");
        // Not created yet: nothing to escape through.
        assert!(ensure_within_wiki_dir(&wiki, &wiki.join("a/b.md")).is_ok());
        std::fs::create_dir_all(&wiki).unwrap();
        assert!(ensure_within_wiki_dir(&wiki, &wiki.join("a/b.md")).is_ok());
        // A path that is not under the wiki directory at all.
        assert!(ensure_within_wiki_dir(&wiki, tmp.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_subdirectory_and_page_are_refused() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::TempDir::new().unwrap();
        let wiki = tmp.path().join("wiki");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&wiki).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.md"), "secret").unwrap();
        symlink(&outside, wiki.join("linked")).unwrap();
        symlink(outside.join("secret.md"), wiki.join("page.md")).unwrap();
        // Dangling link: canonicalize fails, must deny (was fail-open).
        symlink(tmp.path().join("nowhere.md"), wiki.join("dangling.md")).unwrap();

        assert!(ensure_within_wiki_dir(&wiki, &wiki.join("linked/secret.md")).is_err());
        assert!(ensure_within_wiki_dir(&wiki, &wiki.join("linked/new/x.md")).is_err());
        assert!(ensure_within_wiki_dir(&wiki, &wiki.join("dangling.md")).is_err());

        let e = read_wiki_file_nofollow(&wiki, "linked/secret.md").unwrap_err();
        assert_ne!(e.kind(), io::ErrorKind::NotFound);
        assert!(read_wiki_file_nofollow(&wiki, "page.md").is_err());

        // Write through a symlinked subdirectory creates nothing outside.
        assert!(prepare_wiki_write_target(&wiki, "linked/deep/x.md").is_err());
        assert!(!outside.join("deep").exists());
    }

    #[cfg(unix)]
    #[test]
    fn planted_temp_symlink_is_not_written_through() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::TempDir::new().unwrap();
        let wiki = tmp.path().join("wiki");
        std::fs::create_dir_all(&wiki).unwrap();
        let victim = tmp.path().join("victim.txt");
        std::fs::write(&victim, "original").unwrap();
        symlink(&victim, wiki.join("p.md.tmp")).unwrap();
        let full = prepare_wiki_write_target(&wiki, "p.md").unwrap();
        atomic_write_nofollow(&full, "new").unwrap();
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "original");
        assert_eq!(read_wiki_file_nofollow(&wiki, "p.md").unwrap(), "new");
    }

    #[test]
    fn read_reports_not_found_for_a_missing_page() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("wiki")).unwrap();
        let e = read_wiki_file_nofollow(&tmp.path().join("wiki"), "nope.md").unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
    }
}
