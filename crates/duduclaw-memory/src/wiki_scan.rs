//! One incremental scanner for listing, search, injection, CLI and exports.
use std::io;
use std::path::Path;

pub const RESERVED_FILES: &[&str] = &["_schema.md", "_index.md", "_log.md"];
pub const MAX_DEPTH: usize = 20;

pub fn is_reserved(name: &str) -> bool {
    RESERVED_FILES.iter().any(|reserved| duduclaw_core::wiki_auto_namespace::segment_opens_as(name, reserved))
}

#[derive(Debug, Default)]
pub struct ScanStats {
    pub entries: usize,
    pub invalid_ids: usize,
}

/// Refuse linked start directories and skip internal links. Visitors and
/// checkpoints run incrementally so exports retain cancellation and row caps.
pub fn visit(
    base: &Path, directory: &Path,
    visitor: &mut impl FnMut(&str) -> io::Result<()>,
    checkpoint: &mut impl FnMut() -> io::Result<()>,
) -> io::Result<ScanStats> {
    for root in [base, directory] {
        let meta = std::fs::symlink_metadata(root)?;
        if crate::wiki_path::is_link_metadata(&meta) || !meta.is_dir() {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "wiki scan root is not a real directory"));
        }
    }
    let relative = directory.strip_prefix(base)
        .map_err(|_| io::Error::new(io::ErrorKind::PermissionDenied, "scan outside wiki"))?;
    if relative.components().any(|part| !matches!(part, std::path::Component::Normal(_) | std::path::Component::CurDir)) {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "invalid scan subtree"));
    }
    // Also refuse links in the chain to a requested subtree.
    let mut ancestor = base.to_path_buf();
    for part in relative.components() {
        ancestor.push(part);
        if crate::wiki_path::is_link_metadata(&std::fs::symlink_metadata(&ancestor)?) {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "linked scan ancestor"));
        }
    }
    let mut stats = ScanStats::default();
    if relative.components().next().and_then(|c| c.as_os_str().to_str())
        .is_some_and(|name| duduclaw_core::wiki_auto_namespace::segment_opens_as(name, "_archive")) {
        return Ok(stats);
    }
    walk(base, directory, relative.components().count(), &mut stats, visitor, checkpoint)?;
    Ok(stats)
}

fn walk(
    base: &Path, directory: &Path, depth: usize, stats: &mut ScanStats,
    visitor: &mut impl FnMut(&str) -> io::Result<()>,
    checkpoint: &mut impl FnMut() -> io::Result<()>,
) -> io::Result<()> {
    if depth > MAX_DEPTH { return Ok(()); }
    let metadata = std::fs::symlink_metadata(directory)?;
    if crate::wiki_path::is_link_metadata(&metadata) || !metadata.is_dir() {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "linked scan directory"));
    }
    for entry in std::fs::read_dir(directory)? {
        checkpoint()?;
        stats.entries += 1;
        let entry = entry?;
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path)?;
        if crate::wiki_path::is_link_metadata(&meta) { continue; }
        if meta.is_dir() {
            if depth == 0 && entry.file_name().to_str().is_some_and(|name|
                duduclaw_core::wiki_auto_namespace::segment_opens_as(name, "_archive")) { continue; }
            walk(base, &path, depth + 1, stats, visitor, checkpoint)?;
        } else if meta.is_file() && path.extension().is_some_and(|ext| ext == "md") {
            if entry.file_name().to_str().is_some_and(is_reserved) { continue; }
            let id = path.strip_prefix(base).ok().and_then(crate::wiki_path::page_id_from_relative_path);
            let resolved = id.and_then(|id| crate::wiki_path::resolve_wiki_page(base, &id, false).ok());
            match resolved {
                Some(page) => visitor(&page.id)?,
                None => stats.invalid_ids += 1,
            }
        }
    }
    Ok(())
}

pub fn collect(base: &Path, directory: &Path) -> Vec<String> {
    let mut pages = Vec::new();
    match visit(base, directory, &mut |id| { pages.push(id.to_string()); Ok(()) }, &mut || Ok(())) {
        Ok(stats) => {
            if stats.invalid_ids > 0 { tracing::warn!(invalid_ids = stats.invalid_ids, "wiki scan skipped invalid ids"); }
            pages.sort();
            pages
        }
        Err(error) => {
            tracing::warn!(%error, "wiki scan refused");
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rm209_scan_matches_store_and_caps_depth() {
        let root = tempfile::tempdir().unwrap();
        for id in ["notes/a.md", "_archive/old.md", "_ARCHIVE/other.md", "notes/_INDEX.md", "notes/_archive/kept.md"] {
            let path = root.path().join(id);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "---\ntitle: Page\n---\nbody").unwrap();
        }
        let mut deep = root.path().to_path_buf();
        for _ in 0..21 { deep.push("d"); }
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("too-deep.md"), "body").unwrap();
        let expected = vec!["notes/_archive/kept.md".to_string(), "notes/a.md".to_string()];
        assert_eq!(collect(root.path(), root.path()), expected);
        let store = crate::WikiStore::new(root.path().to_path_buf());
        let mut listed: Vec<_> = store.list_pages().unwrap().into_iter().map(|p| p.path).collect();
        listed.sort();
        assert_eq!(listed, expected);
        assert!(collect(root.path(), &root.path().join("_archive")).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn rm209_scan_refuses_linked_start_and_counts_invalid_ids() {
        let root = tempfile::tempdir().unwrap();
        let alias = root.path().join("alias");
        let wiki = root.path().join("wiki");
        std::fs::create_dir(&wiki).unwrap();
        std::fs::write(wiki.join("bad\\name.md"), "body").unwrap();
        std::fs::write(wiki.join("good.md"), "body").unwrap();
        std::os::unix::fs::symlink(&wiki, &alias).unwrap();
        assert!(visit(&alias, &alias, &mut |_| Ok(()), &mut || Ok(())).is_err());
        let mut ids = Vec::new();
        let stats = visit(&wiki, &wiki, &mut |id| { ids.push(id.to_string()); Ok(()) }, &mut || Ok(())).unwrap();
        assert_eq!(stats.invalid_ids, 1);
        assert_eq!(ids, vec!["good.md"]);
    }
}
