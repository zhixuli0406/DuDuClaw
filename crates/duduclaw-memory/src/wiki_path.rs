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
//! - Reads and writes walk opened directory handles without following links.
//!   Unix final opens, staging files and renames are relative to the verified
//!   parent fd. Windows pins the chain and checks parent handle identities.

use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
pub(crate) mod directory;
use directory::{Directory, Kind, Open};

/// Bound both one-page resolution and each scanner directory snapshot.
pub const MAX_DIRECTORY_ENTRIES: usize = 10_000;

pub(crate) struct FoldedNames(std::collections::HashMap<String, Option<std::ffi::OsString>>);

impl FoldedNames {
    pub(crate) fn new(names: &[std::ffi::OsString]) -> Self {
        let mut map = std::collections::HashMap::new();
        for name in names {
            if let Some(text) = name.to_str() {
                let key = duduclaw_core::wiki_auto_namespace::fold_segment(text);
                map.entry(key).and_modify(|entry| *entry = None).or_insert_with(|| Some(name.clone()));
            }
        }
        Self(map)
    }

    pub(crate) fn resolve(&self, name: &str) -> io::Result<std::ffi::OsString> {
        match self.0.get(&duduclaw_core::wiki_auto_namespace::fold_segment(name)) {
            Some(Some(name)) => Ok(name.clone()),
            Some(None) => Err(denied("ambiguous wiki page spelling")),
            None => Ok(name.into()),
        }
    }
}


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
    root: Arc<Directory>,
    parent: Arc<Directory>,
}

impl ResolvedWikiPage {
    /// The scanner has already refused ambiguous names in every ancestor.
    pub(crate) fn from_scan(root: Arc<Directory>, parent: Arc<Directory>, id: String) -> io::Result<Self> {
        require_canonical_id(&id)?;
        validate_relative_wiki_read_path(&id).map_err(|e| invalid(&e))?;
        finish_resolution(root, parent, &id, &id, false)
    }

    fn verify_parent(&self) -> io::Result<()> {
        let current_root = Directory::root(&self.root.path)?;
        let parent_id = Path::new(&self.id).parent().ok_or_else(|| invalid("missing parent"))?;
        if !self.root.same(&current_root)? || !self.parent.same(&self.root.descend(parent_id, false)?)? {
            return Err(denied("wiki parent directory changed"));
        }
        Ok(())
    }

    fn name(&self) -> io::Result<&std::ffi::OsStr> {
        self.full_path.file_name().ok_or_else(|| invalid("missing page name"))
    }
}

/// Resolve every component without following links. Folded aliases must be
/// unique, and every directory has an explicit listing budget.
pub fn resolve_wiki_page(wiki_dir: &Path, id: &str, write: bool) -> io::Result<ResolvedWikiPage> {
    require_canonical_id(id)?;
    validate_relative_wiki_read_path(id).map_err(|e| invalid(&e))?;
    let root = Arc::new(Directory::root(wiki_dir)?);
    resolve_from_root(root, id, write)
}

fn resolve_from_root(root: Arc<Directory>, id: &str, write: bool) -> io::Result<ResolvedWikiPage> {
    let mut parent = root.descend(Path::new(""), false)?;
    let parts: Vec<_> = id.split('/').collect();
    let mut resolved_parts = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        let names = parent.names(MAX_DIRECTORY_ENTRIES, &mut || Ok(()))?;
        let name = FoldedNames::new(&names).resolve(part)?;
        resolved_parts.push(name.to_str().ok_or_else(|| invalid("non-UTF8 wiki name"))?.to_owned());
        if index + 1 < parts.len() {
            parent = parent.child(&name, false)?;
        } else {
            let resolved = resolved_parts.join("/");
            require_canonical_id(&resolved)?;
            return finish_resolution(root, Arc::new(parent), id, &resolved, write);
        }
    }
    Err(invalid("empty page id"))
}

fn finish_resolution(
    root: Arc<Directory>, parent: Arc<Directory>, requested: &str, resolved: &str, write: bool,
) -> io::Result<ResolvedWikiPage> {
    require_canonical_id(resolved)?;
    let name = Path::new(resolved).file_name().ok_or_else(|| invalid("missing page name"))?;
    let is_auto = duduclaw_core::wiki_auto_namespace::is_auto_page_path(requested)
        || duduclaw_core::wiki_auto_namespace::is_auto_page_path(resolved);
    let exists = match parent.kind(name) {
        Ok(Kind::File) => true,
        Ok(_) => return Err(denied("wiki page is not a regular file")),
        Err(e) if (write || is_auto) && e.kind() == io::ErrorKind::NotFound => false,
        Err(e) => return Err(e),
    };
    let full_path = parent.path.join(name);
    let page = ResolvedWikiPage { id: resolved.to_string(), full_path, is_auto, exists, root, parent };
    page.verify_parent()?;
    Ok(page)
}

/// A read operation's directory snapshots. Never retain this across operations
/// or filesystem writes. Failed listings are cached too: a capped directory
/// must not be listed again for every rejected page. Keys use on-disk names,
/// so differently spelled requests share the same snapshot and held handle.
pub struct WikiPageResolver {
    root: io::Result<Arc<Directory>>,
    directories: std::collections::HashMap<String, io::Result<(Arc<Directory>, FoldedNames)>>,
}

fn copied_error(error: &io::Error) -> io::Error {
    io::Error::new(error.kind(), error.to_string())
}

impl WikiPageResolver {
    pub fn new(wiki_dir: &Path) -> Self {
        Self { root: Directory::root(wiki_dir).map(Arc::new), directories: Default::default() }
    }

    pub fn resolve(&mut self, id: &str) -> io::Result<ResolvedWikiPage> {
        require_canonical_id(id)?;
        validate_relative_wiki_read_path(id).map_err(|e| invalid(&e))?;
        let root = self.root.as_ref().map_err(copied_error)?.clone();
        let mut parent = root.clone();
        let mut resolved_parts = Vec::new();
        let parts: Vec<_> = id.split('/').collect();
        for (index, part) in parts.iter().enumerate() {
            let key = resolved_parts.join("/");
            let snapshot = self.directories.entry(key).or_insert_with(|| {
                let directory = if index == 0 { Ok(root.clone()) }
                    else { parent.child(std::ffi::OsStr::new(&resolved_parts[index - 1]), false).map(Arc::new) }?;
                let names = directory.names(MAX_DIRECTORY_ENTRIES, &mut || Ok(()))?;
                Ok((directory, FoldedNames::new(&names)))
            }).as_ref().map_err(copied_error)?;
            parent = snapshot.0.clone();
            let name = snapshot.1.resolve(part)?;
            resolved_parts.push(name.to_str().ok_or_else(|| invalid("non-UTF8 wiki name"))?.to_owned());
            if index + 1 == parts.len() {
                return finish_resolution(root, parent, id, &resolved_parts.join("/"), false);
            }
        }
        Err(invalid("empty page id"))
    }

    pub fn resolve_many(&mut self, ids: &[String]) -> Vec<io::Result<ResolvedWikiPage>> {
        ids.iter().map(|id| self.resolve(id)).collect()
    }
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
/// parents. Existing linked or unresolvable parents deny.
/// This is separate from `resolve_wiki_page`, which never ignores a missing
/// parent. Ordinary nested-folder creation remains available to writers.
pub fn prepare_wiki_write_with_policy(
    wiki_dir: &Path, id: &str, authorize: impl Fn(&str) -> io::Result<()>,
) -> io::Result<ResolvedWikiPage> {
    require_canonical_id(id)?;
    validate_relative_wiki_path(id).map_err(|e| invalid(&e))?;
    authorize(id)?;
    check_wiki_control_files(wiki_dir)?;
    std::fs::create_dir_all(wiki_dir)?;
    let root = Arc::new(Directory::root(wiki_dir)?);
    let mut parent = root.descend(Path::new(""), false)?;
    let parts: Vec<_> = id.split('/').collect();
    let mut resolved_parts = Vec::new();
    for (index, part) in parts.iter().take(parts.len() - 1).enumerate() {
        let names = parent.names(MAX_DIRECTORY_ENTRIES, &mut || Ok(()))?;
        let name = FoldedNames::new(&names).resolve(part)?;
        resolved_parts.push(name.to_str().ok_or_else(|| invalid("invalid parent name"))?.to_owned());
        let prospective = format!("{}/{}", resolved_parts.join("/"), parts[index + 1..].join("/"));
        authorize(&prospective)?;
        parent = parent.child(&name, true)?;
    }
    let page = resolve_from_root(root, id, true)?;
    authorize(&page.id)?;
    Ok(page)
}

/// Old persisted separator spellings become canonical identities at the
/// persistence boundary; malformed ids have no fallback identity.
pub fn persisted_page_id(path: &str) -> Option<String> {
    canonical_page_id(path)
}

/// Trust aliases use the same per-segment fold as filesystem resolution.
/// This key is for comparison only; never persist it as an on-disk identity.
pub fn folded_page_id(path: &str) -> Option<String> {
    let id = persisted_page_id(path)?;
    Some(id.split('/').map(duduclaw_core::wiki_auto_namespace::fold_segment).collect::<Vec<_>>().join("/"))
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

/// Read the classified target through its held parent, verifying that the
/// no-follow root-to-parent chain still has the same directory identities.
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
    target.verify_parent()?;
    let file = target.parent.open(target.name()?, Open::Read)?;
    target.verify_parent()?;
    Ok(file)
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
    prepare_wiki_write_with_policy(wiki_dir, rel, |_| Ok(()))
}

/// Create scaffold directories relative to the opened wiki root.
pub fn create_wiki_directory(wiki_dir: &Path, relative: &str) -> io::Result<()> {
    std::fs::create_dir_all(wiki_dir)?;
    Directory::root(wiki_dir)?.descend(Path::new(relative), true)?;
    Ok(())
}

/// Scaffold creation uses the same parent-handle boundary as page writes.
pub fn create_wiki_control_file(wiki_dir: &Path, name: &str) -> io::Result<std::fs::File> {
    let target = resolve_wiki_page(wiki_dir, name, true)?;
    if target.id != name { return Err(denied("noncanonical control filename")); }
    target.verify_parent()?;
    target.parent.open(target.name()?, Open::CreateNew)
}

/// Atomic publication of the exact target that was classified and authorized.
pub fn atomic_write_resolved_checked(
    target: &ResolvedWikiPage, content: &str, verify: impl Fn() -> io::Result<()>,
) -> io::Result<()> {
    atomic_write_in(&target.parent, target.name()?, content, || {
        verify()?;
        target.verify_parent()
    })
}

pub fn atomic_write_wiki_control_checked(
    wiki_dir: &Path, name: &str, content: &str, verify: impl Fn() -> io::Result<()>,
) -> io::Result<()> {
    let target = resolve_wiki_page(wiki_dir, name, true)?;
    if target.id != name { return Err(denied("noncanonical control filename")); }
    atomic_write_resolved_checked(&target, content, verify)
}

/// Archive/restore must retain both verified parent handles through rename.
pub fn rename_resolved_wiki_file(source: &ResolvedWikiPage, destination: &ResolvedWikiPage) -> io::Result<()> {
    source.verify_parent()?;
    destination.verify_parent()?;
    if source.parent.kind(source.name()?)? != Kind::File { return Err(denied("source is not regular")); }
    reject_nonregular_at(&destination.parent, destination.name()?)?;
    source.parent.rename_to(source.name()?, &destination.parent, destination.name()?)
}

pub fn remove_resolved_wiki_file(target: &ResolvedWikiPage) -> io::Result<()> {
    target.verify_parent()?;
    reject_nonregular_at(&target.parent, target.name()?)?;
    target.parent.remove(target.name()?)
}

/// Write through an exclusive staging file and a handle-relative rename.
/// Wiki callers should retain their resolved target across authorization and
/// call `atomic_write_resolved_checked` instead of resolving a path again.
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
    verify()?;
    let parent = full.parent().ok_or_else(|| invalid("missing parent"))?;
    let absolute = if parent.is_absolute() { parent.to_path_buf() } else { std::env::current_dir()?.join(parent) };
    let parent = Directory::open_absolute(&absolute)?;
    atomic_write_in(&parent, full.file_name().ok_or_else(|| invalid("missing name"))?, content, verify)
}

fn atomic_write_in(
    parent: &Directory, name: &std::ffi::OsStr, content: &str,
    verify: impl Fn() -> io::Result<()>,
) -> io::Result<()> {
    use std::io::Write;
    verify()?;
    reject_nonregular_at(parent, name)?;
    // Unique, exclusive staging names avoid unlinking another writer's file.
    let tmp = std::ffi::OsString::from(format!(".wiki-{}.tmp", uuid::Uuid::new_v4()));
    let mut file = parent.open(&tmp, Open::CreateNew)?;
    let result = (|| {
        verify()?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        drop(file);
        reject_nonregular_at(parent, name)?;
        verify()?;
        parent.rename(&tmp, name)
    })();
    if result.is_err() { let _ = parent.remove(&tmp); }
    result
}

fn reject_nonregular_at(parent: &Directory, name: &std::ffi::OsStr) -> io::Result<()> {
    match parent.kind(name) {
        Ok(Kind::File) => Ok(()),
        Ok(_) => Err(denied("target is not a regular file")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
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
    fn missing_page_under_linked_auto_parent_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("auto")).unwrap();
        std::os::unix::fs::symlink("auto", tmp.path().join("public-alias")).unwrap();
        assert!(resolve_wiki_page(tmp.path(), "public-alias/none.md", false).is_err());
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
    fn linked_parents_are_refused_even_when_they_stay_inside_wiki() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let wiki = tmp.path();
        std::fs::create_dir_all(wiki.join("auto/sop")).unwrap();
        std::fs::create_dir_all(wiki.join("ordinary")).unwrap();
        std::fs::write(wiki.join("auto/sop/secret.md"), "private").unwrap();
        std::fs::write(wiki.join("ordinary/page.md"), "ordinary").unwrap();
        symlink("auto", wiki.join("public-alias")).unwrap();
        symlink("ordinary", wiki.join("ordinary-alias")).unwrap();
        assert!(resolve_wiki_page(wiki, "public-alias/sop/secret.md", false).is_err());
        assert!(prepare_employee_wiki_write(wiki, "public-alias/new/secret.md").is_err());
        assert!(!wiki.join("auto/new").exists());
        assert!(prepare_employee_wiki_write(wiki, "ordinary-alias/new/page.md").is_err());
        assert!(!wiki.join("ordinary/new").exists());
        let target = prepare_employee_wiki_write(wiki, "ordinary/new/page.md").unwrap();
        atomic_write_resolved_checked(&target, "new ordinary", || Ok(())).unwrap();
        assert_eq!(read_wiki_file_nofollow(wiki, "ordinary/new/page.md").unwrap(), "new ordinary");
        symlink("../ordinary", wiki.join("auto/outward")).unwrap();
        assert!(resolve_wiki_page(wiki, "auto/outward/page.md", false).is_err());
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
                let staged = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().path())
                    .find(|p| p.extension().is_some_and(|ext| ext == "tmp")).unwrap();
                assert_eq!(std::fs::read_to_string(staged).unwrap(), "replacement");
                Err(denied("authority revoked"))
            } else { Ok(()) }
        });
        assert!(result.is_err());
        assert_eq!(checks.get(), 4);
        assert_eq!(std::fs::read_to_string(&page).unwrap(), "original");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
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

/// Preflight every control file before either writer mutates the wiki.
/// Folded aliases are resolved exactly as page identities are.
pub fn check_wiki_control_files(wiki_dir: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(wiki_dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
        Ok(meta) if is_link_metadata(&meta) || !meta.is_dir() => return Err(denied("linked wiki root")),
        Ok(_) => {}
    }
    for name in crate::wiki_scan::RESERVED_FILES.iter().copied().chain(["_index.lock", ".scope.toml"]) {
        let target = resolve_wiki_page(wiki_dir, name, true)?;
        if target.exists {
            // A control file with a different spelling must not create a
            // second control identity on a case-sensitive filesystem.
            if target.id != name { return Err(denied("noncanonical control filename")); }
            open_resolved_wiki_file_nofollow(&target)?;
        }
    }
    duduclaw_core::scope_policy::load(&wiki_dir.join(".scope.toml"))
        .map_err(|e| denied(&e.to_string()))?;
    Ok(())
}

/// Create/open an append or lock file without following links on any OS.
pub fn open_control_file_nofollow(wiki_dir: &Path, name: &str, append: bool) -> io::Result<std::fs::File> {
    let target = resolve_wiki_page(wiki_dir, name, true)?;
    if target.id != name { return Err(denied("noncanonical control filename")); }
    target.verify_parent()?;
    let file = target.parent.open(target.name()?, if append { Open::Append } else { Open::Lock })?;
    target.verify_parent()?;
    Ok(file)
}

#[cfg(test)]
mod rm209_identity_tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn control_handles_can_lock_and_append_without_overwriting() {
        use std::io::{Seek, SeekFrom, Write};
        use duduclaw_core::platform::flock_exclusive;
        use fs2::FileExt;

        let root = tempfile::tempdir().unwrap();
        let lock = open_control_file_nofollow(root.path(), "_index.lock", false).unwrap();
        flock_exclusive(&lock).unwrap();
        FileExt::unlock(&lock).unwrap();

        // Open both handles before either write: each append must use the
        // current EOF, even if its cursor was explicitly moved to the start.
        let first = open_control_file_nofollow(root.path(), "_log.md", true).unwrap();
        let second = open_control_file_nofollow(root.path(), "_log.md", true).unwrap();
        for (mut file, entry) in [(first, "first\n"), (second, "second\n")] {
            flock_exclusive(&file).unwrap();
            file.seek(SeekFrom::Start(0)).unwrap();
            file.write_all(entry.as_bytes()).unwrap();
            FileExt::unlock(&file).unwrap();
        }
        assert_eq!(std::fs::read_to_string(root.path().join("_log.md")).unwrap(), "first\nsecond\n");
    }

    #[test]
    fn batch_resolution_matches_single_page_resolution_with_one_listing_per_directory() {
        let root = tempfile::tempdir().unwrap();
        let mut ids = Vec::new();
        for directory in ["MiXeD", "Other", "Other/NeStEd", "auto"] {
            std::fs::create_dir_all(root.path().join(directory)).unwrap();
            for index in 0..32 {
                let id = format!("{directory}/PaGe{index}.md");
                std::fs::write(root.path().join(&id), &id).unwrap();
                ids.push(id.to_lowercase());
                ids.push(format!("{}/PAGE{index}.md", directory.to_uppercase()));
            }
        }
        ids.extend(["AUTO/not-yet.md", "mixed/missing.md", "../escape.md"].map(str::to_string));
        let expected: Vec<_> = ids.iter().map(|id| resolve_wiki_page(root.path(), id, false)).collect();
        let before = directory::LISTING_COUNT.with(|count| count.get());
        let actual = WikiPageResolver::new(root.path()).resolve_many(&ids);
        assert_eq!(directory::LISTING_COUNT.with(|count| count.get()) - before, 5);
        for (actual, expected) in actual.into_iter().zip(expected) {
            match (actual, expected) {
                (Ok(actual), Ok(expected)) => {
                    assert_eq!(actual.id, expected.id);
                    assert_eq!(actual.full_path, expected.full_path);
                    assert_eq!(actual.is_auto, expected.is_auto);
                    assert_eq!(actual.exists, expected.exists);
                    if actual.exists {
                        assert_eq!(read_resolved_wiki_file_nofollow(&actual).unwrap(), read_resolved_wiki_file_nofollow(&expected).unwrap());
                    }
                }
                (Err(actual), Err(expected)) => assert_eq!(actual.kind(), expected.kind()),
                (actual, expected) => panic!("batch {actual:?} differs from single {expected:?}"),
            }
        }
    }

    #[test]
    fn folded_filename_resolves_to_disk_identity_or_refuses() {
        let root = tempfile::tempdir().unwrap();
        for (disk, aliases) in [
            ("Bad.md", vec!["Bad.md", "bad.md", "BAD.md"]),
            ("café.md", vec!["café.md", "cafe\u{301}.md", "CAFÉ.md"]),
            ("tail.md", vec!["tail.md", "tail.md.", "tail.md "]),
        ] {
            std::fs::write(root.path().join(disk), "body").unwrap();
            let actual = resolve_wiki_page(root.path(), disk, false).unwrap().id;
            for alias in aliases {
                match resolve_wiki_page(root.path(), alias, false) {
                    Ok(page) => {
                        assert_eq!(page.id, actual);
                        assert_eq!(read_resolved_wiki_file_nofollow(&page).unwrap(), "body");
                    }
                    Err(error) => assert!(matches!(error.kind(), io::ErrorKind::PermissionDenied | io::ErrorKind::InvalidInput)),
                }
            }
        }
    }

    #[test]
    fn ambiguous_fold_matches_are_refused_even_for_exact_spelling() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("Bad.md"), "first").unwrap();
        match std::fs::OpenOptions::new().write(true).create_new(true).open(root.path().join("bad.md")) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                eprintln!("RM-209: skipping ambiguous-name test: filesystem does not allow both spellings");
                return;
            }
            Err(error) => panic!("fixture: {error}"),
        }
        for name in ["Bad.md", "bad.md", "BAD.md"] {
            assert!(resolve_wiki_page(root.path(), name, false).is_err());
            assert!(resolve_wiki_page(root.path(), name, true).is_err());
        }
        let before = directory::LISTING_COUNT.with(|count| count.get());
        let ids = ["Bad.md", "bad.md", "BAD.md"].map(str::to_string);
        assert!(WikiPageResolver::new(root.path()).resolve_many(&ids).iter().all(Result::is_err));
        assert_eq!(directory::LISTING_COUNT.with(|count| count.get()) - before, 1);
    }

    #[cfg(unix)]
    #[test]
    fn resolved_open_refuses_link_swapped_after_resolution() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.md"), "safe").unwrap();
        std::fs::write(outside.path().join("secret"), "private").unwrap();
        let page = resolve_wiki_page(root.path(), "a.md", false).unwrap();
        std::fs::remove_file(&page.full_path).unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret"), &page.full_path).unwrap();
        assert!(open_resolved_wiki_file_nofollow(&page).is_err());
    }
}

#[cfg(test)]
mod parent_race_tests {
    use super::*;

    #[test]
    fn resolver_enforces_directory_entry_cap() {
        let root = tempfile::tempdir().unwrap();
        for index in 0..MAX_DIRECTORY_ENTRIES {
            std::fs::File::create(root.path().join(format!("{index}.md"))).unwrap();
        }
        assert!(resolve_wiki_page(root.path(), "0.md", false).is_ok());
        let ids = ["0.md", "1.md", "2.md"].map(str::to_string);
        let before = directory::LISTING_COUNT.with(|count| count.get());
        assert!(WikiPageResolver::new(root.path()).resolve_many(&ids).iter().all(Result::is_ok));
        assert_eq!(directory::LISTING_COUNT.with(|count| count.get()) - before, 1);
        std::fs::File::create(root.path().join("over-limit.md")).unwrap();
        let error = resolve_wiki_page(root.path(), "0.md", false).unwrap_err();
        assert!(error.to_string().contains("directory entry limit"));
        let before = directory::LISTING_COUNT.with(|count| count.get());
        let results = WikiPageResolver::new(root.path()).resolve_many(&ids);
        assert!(results.iter().all(|result| result.as_ref().unwrap_err().to_string().contains("directory entry limit")));
        assert_eq!(directory::LISTING_COUNT.with(|count| count.get()) - before, 1, "failed snapshots are shared too");
    }

    #[cfg(unix)]
    #[test]
    fn swapped_parent_is_refused_by_read_and_write() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("notes")).unwrap();
        std::fs::write(root.path().join("notes/page.md"), "inside").unwrap();
        std::fs::write(outside.path().join("page.md"), "outside").unwrap();
        let page = resolve_wiki_page(root.path(), "notes/page.md", false).unwrap();
        let mut resolver = WikiPageResolver::new(root.path());
        let cached = resolver.resolve("NOTES/page.md").unwrap();
        std::fs::rename(root.path().join("notes"), root.path().join("held")).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("notes")).unwrap();
        assert!(read_resolved_wiki_file_nofollow(&page).is_err());
        assert!(read_resolved_wiki_file_nofollow(&cached).is_err());
        assert!(resolver.resolve("notes/page.md").is_err());
        assert!(WikiPageResolver::new(root.path()).resolve("notes/page.md").is_err());
        assert!(atomic_write_resolved_checked(&page, "attack", || Ok(())).is_err());
        assert!(atomic_write_nofollow(&page.full_path, "attack").is_err());
        assert!(prepare_wiki_write_page(root.path(), "notes/new/page.md").is_err());
        assert!(!outside.path().join("new").exists());
        assert_eq!(std::fs::read_to_string(outside.path().join("page.md")).unwrap(), "outside");
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn swaps_during_staging_never_publish_outside_or_leave_temporary_files() {
        for swap_at in [1, 2, 3] {
            let root = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            std::fs::create_dir(root.path().join("notes")).unwrap();
            let page = prepare_wiki_write_page(root.path(), "notes/new.md").unwrap();
            let calls = std::cell::Cell::new(0);
            let result = atomic_write_resolved_checked(&page, "attack", || {
                calls.set(calls.get() + 1);
                if calls.get() == swap_at {
                    std::fs::rename(root.path().join("notes"), root.path().join("held"))?;
                    std::os::unix::fs::symlink(outside.path(), root.path().join("notes"))?;
                }
                Ok(())
            });
            assert!(result.is_err(), "swap at check {swap_at}");
            assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
            assert_eq!(std::fs::read_dir(root.path().join("held")).unwrap().count(), 0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn controls_refuse_swapped_root_for_create_append_lock_and_rename() {
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let wiki = home.path().join("wiki");
        std::fs::create_dir(&wiki).unwrap();
        let page = resolve_wiki_page(&wiki, "_index.md", true).unwrap();
        std::fs::rename(&wiki, home.path().join("held")).unwrap();
        std::os::unix::fs::symlink(outside.path(), &wiki).unwrap();
        assert!(create_wiki_control_file(&wiki, "_index.md").is_err());
        assert!(open_control_file_nofollow(&wiki, "_log.md", true).is_err());
        assert!(open_control_file_nofollow(&wiki, "_index.lock", false).is_err());
        assert!(atomic_write_resolved_checked(&page, "attack", || Ok(())).is_err());
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }
}
