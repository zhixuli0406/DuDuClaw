//! Shared path checks for every wiki read and write.
//!
//! A wiki page path comes from a caller (an MCP tool argument, a dashboard
//! RPC, a backlink in a page) and is joined onto a wiki directory. Every
//! surface that does that join goes through this module so the rules live in
//! one place:
//!
//! - [`validate_relative_wiki_path`] is the strict gate for writes: relative
//!   paths only, no control characters, traversal or encoded separators, no
//!   drive prefixes, trailing dots/spaces, Windows device names, backslashes,
//!   ADS colons or ambiguous DOS short-name tildes on every platform.
//! - [`validate_relative_wiki_read_path`] also accepts existing Unix names
//!   with trailing dots/spaces or device names. Windows stays strict.
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
//! resolution and the open is not caught if a component of the resolved
//! parent itself is replaced. The original alias is no longer traversed by
//! the open; only the final component is opened with `O_NOFOLLOW`. Closing
//! the remaining window needs
//! `openat` walking per component; the writer of such a swap already has
//! write access inside the wiki directory.

use std::io;
use std::path::{Component, Path, PathBuf};

/// The single filesystem-path → page-id boundary used by every wiki walker.
/// Keep ids as strings; converting back to PathBuf lets later OS operations
/// reintroduce native separators. Never lossy-decode or drop a component.
pub fn page_id_from_relative_path(path: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for part in path.components() {
        match part {
            Component::Normal(name) => {
                let name = name.to_str()?;
                // A literal backslash in a Unix filename is not a directory.
                // Omit it rather than alias another physical page's registry.
                if name.contains('\\') { return None; }
                parts.push(name);
            }
            Component::CurDir => {}
            _ => return None,
        }
    }
    let id = parts.join("/");
    validate_relative_wiki_read_path(&id).ok()?;
    Some(id)
}

pub use duduclaw_core::wiki_auto_namespace::canonical_page_id;

/// Canonicalize untrusted input at the API boundary, before any policy check.
pub fn canonical_page_argument(path: &str) -> io::Result<String> {
    let id = canonical_page_id(path).ok_or_else(|| invalid("invalid wiki page id"))?;
    validate_relative_wiki_read_path(&id).map_err(|e| invalid(&e))?;
    Ok(id)
}

/// Normalize page-valued fields before dispatch authorization. Search query
/// text, namespace names and document content are deliberately not paths.
pub fn canonicalize_wiki_arguments(args: &mut serde_json::Value) -> io::Result<()> {
    for field in ["page_path", "center"] {
        if let Some(value) = args.get_mut(field) {
            let raw = value.as_str().ok_or_else(|| invalid("page id must be a string"))?;
            *value = serde_json::Value::String(canonical_page_argument(raw)?);
        }
    }
    Ok(())
}

/// Low-level I/O accepts identities, never alternate spellings.
fn require_canonical_id(id: &str) -> io::Result<()> {
    if canonical_page_id(id).as_deref() != Some(id) {
        return Err(invalid("non-canonical wiki page id"));
    }
    Ok(())
}

/// Both identities participate in policy; registry/trust keys use `id`.
#[derive(Debug)]
pub struct ResolvedWikiPage {
    pub id: String,
    pub full_path: PathBuf,
    pub is_auto: bool,
    /// Whether the final component existed during resolution; opens recheck it.
    pub exists: bool,
}

/// Resolve the parent chain (including Windows junctions) before classifying
/// a page. A missing final component is accepted for writes and auto policy
/// checks; ordinary reads return NotFound. Missing parents return NotFound.
/// Opens use this resolved parent, retaining no-follow protection on the final name.
pub fn resolve_wiki_page(wiki_dir: &Path, id: &str, write: bool) -> io::Result<ResolvedWikiPage> {
    require_canonical_id(id)?;
    validate_relative_wiki_read_path(id).map_err(|e| invalid(&e))?;
    let root = wiki_dir.canonicalize()?;
    let lexical = root.join(id);
    let parent = lexical.parent().ok_or_else(|| invalid("missing page parent"))?.canonicalize()?;
    if !parent.starts_with(&root) { return Err(denied("resolved parent escapes wiki directory")); }
    let full_path = parent.join(lexical.file_name().ok_or_else(|| invalid("missing page name"))?);
    let resolved = page_id_from_relative_path(full_path.strip_prefix(&root)
        .map_err(|_| denied("resolved path escapes wiki directory"))?)
        .ok_or_else(|| invalid("invalid resolved wiki page id"))?;
    require_canonical_id(&resolved)?;
    let is_auto = duduclaw_core::wiki_auto_namespace::is_auto_page_path(id)
        || duduclaw_core::wiki_auto_namespace::is_auto_page_path(&resolved);
    // Classify before checking existence so an auto alias cannot disclose
    // whether a page exists by bypassing the auto policy on missing files.
    let exists = match std::fs::symlink_metadata(&full_path) {
        Ok(meta) => {
            if is_link_metadata(&meta) || !meta.is_file() {
                return Err(denied("wiki page is not a regular file"));
            }
            true
        }
        Err(e) if (write || is_auto) && e.kind() == io::ErrorKind::NotFound => false,
        Err(e) => return Err(e),
    };
    Ok(ResolvedWikiPage { id: resolved, full_path, is_auto, exists })
}

pub use duduclaw_core::fs_safety::is_link_metadata;

/// Employee writes may create ordinary directories, but must classify every
/// existing ancestor before creating anything below it. A missing directory
/// is created explicitly; it is never treated as a successful resolution.
pub fn prepare_employee_wiki_write(wiki_dir: &Path, id: &str) -> io::Result<ResolvedWikiPage> {
    prepare_wiki_write_with_policy(wiki_dir, id, |id| {
        if duduclaw_core::wiki_auto_namespace::is_auto_page_path(id) {
            Err(denied("auto namespace is host-owned"))
        } else { Ok(()) }
    })
}

/// Authorize the lexical id and each resolved target BEFORE creating missing
/// parents. Existing but unresolvable parents (including dangling links) deny.
/// This is separate from `resolve_wiki_page`, which never ignores a missing
/// parent. Ordinary nested-folder creation remains available to writers.
pub fn prepare_wiki_write_with_policy(
    wiki_dir: &Path, id: &str, authorize: impl Fn(&str) -> io::Result<()>,
) -> io::Result<ResolvedWikiPage> {
    require_canonical_id(id)?;
    validate_relative_wiki_path(id).map_err(|e| invalid(&e))?;
    authorize(id)?;
    std::fs::create_dir_all(wiki_dir)?;
    let root = wiki_dir.canonicalize()?;
    let mut parent = root.clone();
    let parts: Vec<_> = id.split('/').collect();
    for (index, part) in parts.iter().take(parts.len() - 1).enumerate() {
        let next = parent.join(part);
        let resolved = match std::fs::symlink_metadata(&next) {
            Ok(_) => next.canonicalize()?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                // This full prospective id was authorized at the previous step.
                authorize(&page_id_from_relative_path(
                    &next.strip_prefix(&root).map_err(|_| denied("parent escapes wiki"))?
                        .join(parts[index + 1..].join("/")),
                ).ok_or_else(|| invalid("invalid parent"))?)?;
                std::fs::create_dir(&next)?;
                next.canonicalize()?
            }
            Err(e) => return Err(e),
        };
        let relative = resolved.strip_prefix(&root)
            .map_err(|_| denied("resolved parent escapes wiki directory"))?;
        let prospective = relative.join(parts[index + 1..].join("/"));
        let resolved_id = page_id_from_relative_path(&prospective)
            .ok_or_else(|| invalid("invalid resolved page id"))?;
        authorize(&resolved_id)?;
        if !resolved.is_dir() { return Err(denied("page parent is not a directory")); }
        parent = resolved;
    }
    let page = resolve_wiki_page(wiki_dir, id, true)?;
    authorize(&page.id)?;
    Ok(page)
}

/// Old persisted separator spellings become canonical identities at the
/// persistence boundary; malformed ids have no fallback identity.
pub fn persisted_page_id(path: &str) -> Option<String> {
    canonical_page_id(path)
}

/// Compare an index link with a page id across old persisted separator forms.
pub fn index_line_links_to(line: &str, path: &str) -> bool {
    let Some(key) = persisted_page_id(path) else { return false; };
    line.split("](").skip(1).filter_map(|rest| rest.split_once(')'))
        .any(|(target, _)| persisted_page_id(target).as_ref() == Some(&key))
}

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
    if path.contains('\\') {
        return Err("Use forward slashes in page_path".to_string());
    }
    validate_relative_wiki_path_mode(path, true)
}

/// Read-only compatibility for existing names that have no aliases on Unix.
/// Windows uses the same strict component rules in both modes.
pub fn validate_relative_wiki_read_path(path: &str) -> std::result::Result<(), String> {
    validate_relative_wiki_path_mode(path, cfg!(windows))
}

fn validate_relative_wiki_path_mode(path: &str, strict: bool) -> std::result::Result<(), String> {
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
    // Writes use portable ids on every OS; reads retain existing Unix names.
    if strict && path.contains([':', '~']) {
        return Err("Streams and short-name aliases are not allowed in page_path".to_string());
    }
    if path.chars().any(char::is_control) {
        return Err("Control characters are not allowed in page_path".to_string());
    }
    for component in Path::new(path).components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => {
                return Err("Path must stay inside the wiki directory".to_string());
            }
            Component::CurDir => {}
            Component::Normal(name) => {
                let Some(name) = name.to_str() else {
                    return Err("page_path must be valid UTF-8".to_string());
                };
                if strict {
                    check_component(name)?;
                }
            }
        }
    }
    Ok(())
}

/// Strict write rules keep newly created pages portable; Windows reads
/// also need them because Win32 aliases these names.
fn check_component(name: &str) -> std::result::Result<(), String> {
    if name.ends_with('.') || name.ends_with(' ') {
        return Err("A path component may not end with '.' or a space".to_string());
    }
    if is_windows_device_name(name) {
        return Err("Windows reserved device names are not allowed in page_path".to_string());
    }
    Ok(())
}

/// Whether `name` (one path component) is a Windows reserved device name:
/// the part before the first `.` (trailing spaces ignored, as Windows does),
/// compared without regard to case. Public so writers that build page names
/// (e.g. the auto-page slug check) refuse the same names this validator does.
pub fn is_windows_device_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ');
    let upper = stem.to_uppercase();
    if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$") {
        return true;
    }
    let mut chars = upper.chars();
    let head: String = chars.by_ref().take(3).collect();
    let digit: Vec<char> = chars.collect();
    (head == "COM" || head == "LPT")
        && matches!(digit.as_slice(), [d] if d.is_ascii_digit() || matches!(d, '¹' | '²' | '³'))
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

/// Read one wiki file as UTF-8 without following symlinks.
///
/// `rel` must pass [`validate_relative_wiki_read_path`] (checked here again). The
/// file must be a regular file (not a link, directory or device) both before
/// and after opening, and must stay inside the wiki directory. A missing
/// file returns `NotFound` so callers can say "page not found".
pub fn read_wiki_file_nofollow(wiki_dir: &Path, rel: &str) -> io::Result<String> {
    require_canonical_id(rel)?;
    validate_relative_wiki_read_path(rel).map_err(|e| invalid(&e))?;
    let target = resolve_wiki_page(wiki_dir, rel, false)?;
    read_resolved_wiki_file_nofollow(&target)
}

/// Open the same resolved target that the caller classified. Never traverse
/// the original directory alias a second time after authorization.
pub fn read_resolved_wiki_file_nofollow(target: &ResolvedWikiPage) -> io::Result<String> {
    use std::io::Read;
    let mut file = open_resolved_wiki_file_nofollow(target)?;
    let mut content = String::new();
    file.read_to_string(&mut content)?;
    Ok(content)
}

/// Open a validated target once; callers use this handle for both metadata
/// and bounded reads, so the size check and bytes refer to the same file.
pub fn open_resolved_wiki_file_nofollow(target: &ResolvedWikiPage) -> io::Result<std::fs::File> {
    duduclaw_core::fs_safety::open_regular_file_nofollow(&target.full_path)
}

/// Validate `rel`, confirm the target stays inside `wiki_dir`, then create
/// its parent directories and confirm again. Nothing is created before the
/// first check passes, so a symlinked subdirectory or a drive path cannot
/// make this create directories outside the wiki. `wiki_dir` itself is
/// created first (it is operator-controlled, not caller-supplied).
pub fn prepare_wiki_write_target(wiki_dir: &Path, rel: &str) -> io::Result<PathBuf> {
    Ok(prepare_wiki_write_page(wiki_dir, rel)?.full_path)
}

/// Prepare once and return both the I/O path and the identity for metadata.
pub fn prepare_wiki_write_page(wiki_dir: &Path, rel: &str) -> io::Result<ResolvedWikiPage> {
    require_canonical_id(rel)?;
    validate_relative_wiki_path(rel).map_err(|e| invalid(&e))?;
    std::fs::create_dir_all(wiki_dir)?;
    let full = wiki_dir.join(rel);
    ensure_within_wiki_dir(wiki_dir, &full)?;
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent)?;
        ensure_within_wiki_dir(wiki_dir, parent)?;
    }
    // The resolver below refuses a final symlink/reparse point or directory;
    // a link swapped in later is replaced by rename, never followed.
    match std::fs::symlink_metadata(&full) {
        Ok(m) if m.is_dir() => return Err(denied("wiki page path is a directory")),
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    resolve_wiki_page(wiki_dir, rel, true)
}

/// Write `content` to `full` through a temp file + rename. The temp file is
/// removed first (removing a link removes the link, not its target) and
/// created with `create_new` (`O_EXCL`), so a symlink planted at the temp
/// name is never written through. `rename` replaces a link at `full` rather
/// than following it.
pub fn atomic_write_nofollow(full: &Path, content: &str) -> io::Result<()> {
    atomic_write_nofollow_checked(full, content, || Ok(()))
}

/// Check live authority before staging writes and again after fsync, immediately
/// before publishing the page. Refusal removes only the staging file.
pub fn atomic_write_nofollow_checked(
    full: &Path,
    content: &str,
    verify: impl Fn() -> io::Result<()>,
) -> io::Result<()> {
    use std::io::Write;
    verify()?;
    let tmp = full.with_extension("md.tmp");
    match std::fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let write = (|| -> io::Result<()> {
        verify()?;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        verify()?;
        f.write_all(content.as_bytes())?;
        f.sync_all()?;
        verify()
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

    #[cfg(unix)]
    #[test]
    fn file_io_refuses_noncanonical_ids_after_boundary_normalization() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("departments/hr")).unwrap();
        std::fs::write(tmp.path().join("departments/hr/salary.md"), "private salary").unwrap();
        for id in [r"departments\hr\salary.md", r"departments/hr\salary.md", r".\departments\hr\salary.md", "./departments/hr/salary.md", "departments//hr/salary.md"] {
            assert_eq!(canonical_page_argument(id).unwrap(), "departments/hr/salary.md");
            assert_eq!(read_wiki_file_nofollow(tmp.path(), id).unwrap_err().kind(), io::ErrorKind::InvalidInput);
            assert!(prepare_wiki_write_target(tmp.path(), id).is_err());
        }
        assert_eq!(read_wiki_file_nofollow(tmp.path(), "departments/hr/salary.md").unwrap(), "private salary");
    }

    #[test]
    fn missing_page_under_ordinary_parent_is_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("notes")).unwrap();
        let error = resolve_wiki_page(tmp.path(), "notes/none.md", false).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[cfg(unix)]
    #[test]
    fn missing_page_under_auto_alias_is_auto() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("auto")).unwrap();
        std::os::unix::fs::symlink("auto", tmp.path().join("public-alias")).unwrap();
        let target = resolve_wiki_page(tmp.path(), "public-alias/none.md", false).unwrap();
        assert!(target.is_auto);
        assert!(!target.exists);
        assert_eq!(target.id, "auto/none.md");
        assert_eq!(read_resolved_wiki_file_nofollow(&target).unwrap_err().kind(), io::ErrorKind::NotFound);
        assert!(prepare_employee_wiki_write(tmp.path(), "public-alias/none.md").is_err());
    }

    #[test]
    fn missing_page_parent_is_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let error = resolve_wiki_page(tmp.path(), "missing-parent/none.md", false).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn existing_ordinary_page_is_ordinary() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("notes")).unwrap();
        std::fs::write(tmp.path().join("notes/page.md"), "ordinary").unwrap();
        let target = resolve_wiki_page(tmp.path(), "notes/page.md", false).unwrap();
        assert!(!target.is_auto);
        assert!(target.exists);
        assert_eq!(target.id, "notes/page.md");
    }

    #[cfg(unix)]
    #[test]
    fn resolved_parent_classifies_auto_and_keeps_ordinary_links_working() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let wiki = tmp.path();
        std::fs::create_dir_all(wiki.join("auto/sop")).unwrap();
        std::fs::create_dir_all(wiki.join("ordinary")).unwrap();
        std::fs::write(wiki.join("auto/sop/secret.md"), "private").unwrap();
        std::fs::write(wiki.join("ordinary/page.md"), "ordinary").unwrap();
        symlink("auto", wiki.join("public-alias")).unwrap();
        symlink("ordinary", wiki.join("ordinary-alias")).unwrap();
        let target = resolve_wiki_page(wiki, "public-alias/sop/secret.md", false).unwrap();
        assert!(target.is_auto);
        assert_eq!(target.id, "auto/sop/secret.md");
        assert!(prepare_employee_wiki_write(wiki, "public-alias/sop/secret.md").is_err());
        assert!(prepare_employee_wiki_write(wiki, "public-alias/new/secret.md").is_err());
        assert!(!wiki.join("auto/new").exists());
        assert!(resolve_wiki_page(wiki, "ordinary/missing-parent/new.md", true).is_err());
        let target = prepare_employee_wiki_write(wiki, "ordinary-alias/new/page.md").unwrap();
        assert!(!target.is_auto);
        assert_eq!(target.id, "ordinary/new/page.md");
        atomic_write_nofollow(&target.full_path, "new ordinary").unwrap();
        assert_eq!(read_wiki_file_nofollow(wiki, "ordinary-alias/new/page.md").unwrap(), "new ordinary");
        // Either spelling being auto is enough, including an outward alias.
        symlink("../ordinary", wiki.join("auto/outward")).unwrap();
        assert!(resolve_wiki_page(wiki, "auto/outward/page.md", false).unwrap().is_auto);
        symlink("absent", wiki.join("dangling")).unwrap();
        assert!(resolve_wiki_page(wiki, "dangling/new.md", true).is_err());
        assert!(prepare_employee_wiki_write(wiki, "dangling/new.md").is_err());
        symlink("ordinary/page.md", wiki.join("file-link.md")).unwrap();
        assert!(read_wiki_file_nofollow(wiki, "file-link.md").is_err());
        assert!(prepare_employee_wiki_write(wiki, "file-link.md").is_err());
    }

    #[test]
    fn strict_writes_refuse_foreign_separators_and_ambiguous_aliases() {
        for path in [
            r"auto\sop\x.md", r"auto/sop\x.md", r".\auto\x.md",
            r"AUTO\x.md", r"auto\\x.md", r"a\..\auto\x.md",
            r"concepts\x.md", "auto:stream/x.md", "AUTO~1/x.md",
        ] {
            assert!(validate_relative_wiki_path(path).is_err(), "{path:?}");
        }
        assert_eq!(page_id_from_relative_path(&Path::new("auto").join("sop").join("x.md")).as_deref(), Some("auto/sop/x.md"));
        assert!(index_line_links_to(r"- [title](.\auto\sop\x.md)", "auto/sop/x.md"));
    }

    #[test]
    fn revoked_after_staging_sync_does_not_publish_or_leave_staging_file() {
        let dir = tempfile::tempdir().unwrap();
        let page = dir.path().join("page.md");
        std::fs::write(&page, "original").unwrap();
        let checks = std::cell::Cell::new(0);
        let result = atomic_write_nofollow_checked(&page, "replacement", || {
            let next = checks.get() + 1;
            checks.set(next);
            // The fourth check is after writing and syncing the staged bytes.
            if next == 4 {
                assert_eq!(std::fs::read_to_string(page.with_extension("md.tmp")).unwrap(), "replacement");
                Err(denied("authority revoked"))
            } else { Ok(()) }
        });
        assert!(result.is_err());
        assert_eq!(checks.get(), 4);
        assert_eq!(std::fs::read_to_string(&page).unwrap(), "original");
        assert!(!page.with_extension("md.tmp").exists());
        atomic_write_nofollow(&page, "host write").unwrap();
        assert_eq!(std::fs::read_to_string(&page).unwrap(), "host write");
    }

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

    /// New writes refuse nonportable alias/device names on every platform.
    #[test]
    fn windows_alias_forms_are_refused_for_writes_on_every_platform() {
        let trailing = [
            "auto./sop/x.md",
            "auto /sop/x.md",
            "auto/sop./x.md",
            "auto/sop /x.md",
            "auto/sop/x.md.",
            "auto/sop/x.md ",
            "auto. ./x.md",
            "a.md...",
        ];
        let devices = [
            "con.md",
            "CON",
            "entities/Aux.md",
            "nul.tar.md",
            "prn .md",
            "auto/COM1.md",
            "com9.md",
            "lpt1/x.md",
            "LPT9.md",
            "com\u{b9}.md",
            "conin$.md",
            "CONOUT$.md",
        ];
        let streams = ["a.md:stream", "auto/x.md::$DATA", "dir:x/a.md", "時間:10.md"];
        let controls = ["a\u{1}b.md", "auto\t/x.md", "a/b\n.md", "a\u{7f}.md", "a\u{85}.md"];
        for bad in trailing.iter().chain(&devices).chain(&controls) {
            assert!(validate_relative_wiki_path(bad).is_err(), "{bad:?}");
        }
        for path in streams {
            assert!(validate_relative_wiki_path(path).is_err(), "{path}");
        }
        for bad in controls {
            assert!(validate_relative_wiki_read_path(bad).is_err(), "{bad:?}");
        }
        // Names that only resemble the refused forms stay valid.
        for ok in [
            "concepts/a.md",
            "console.md",
            "auxiliary/x.md",
            "com10.md",
            "lpt.md",
            "comma.md",
            "nul-hypothesis.md",
            "a. b.md",
            ".hidden/x.md",
            "./auto/x.md",
            "auto/sop/x-y.md",
            "實體/王先生.md",
        ] {
            assert!(validate_relative_wiki_path(ok).is_ok(), "{ok:?}");
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn read_mode_accepts_existing_nonportable_names() {
        for path in ["時間:10.md", "v1./page.md", "aux.md", "auto /page.md", "auto./page.md"] {
            assert!(validate_relative_wiki_read_path(path).is_ok(), "{path}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn both_modes_refuse_windows_aliases_and_streams() {
        for path in ["時間:10.md", "v1./page.md", "aux.md", "auto /page.md", "auto./page.md"] {
            assert!(validate_relative_wiki_path(path).is_err(), "{path}");
            assert!(validate_relative_wiki_read_path(path).is_err(), "{path}");
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
