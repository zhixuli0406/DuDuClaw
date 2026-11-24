//! One incremental scanner for listing, search, injection, CLI and exports.
use std::io;
use std::path::{Component, Path};
use std::sync::Arc;
use crate::wiki_path::{directory::{Directory, Kind}, FoldedNames, ResolvedWikiPage, MAX_DIRECTORY_ENTRIES};

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
    visit_resolved(base, directory, &mut |page| visitor(&page.id), checkpoint)
}

/// Visit classified pages incrementally without resolving them a second time.
pub fn visit_resolved(
    base: &Path, directory: &Path,
    visitor: &mut impl FnMut(ResolvedWikiPage) -> io::Result<()>,
    checkpoint: &mut impl FnMut() -> io::Result<()>,
) -> io::Result<ScanStats> {
    let relative = directory.strip_prefix(base)
        .map_err(|_| io::Error::new(io::ErrorKind::PermissionDenied, "scan outside wiki"))?;
    let root = Arc::new(Directory::root(base)?);
    let mut directory = root.clone();
    // A scan can start below the root. Resolve those ancestors once too,
    // before issuing typed pages whose ids must use unambiguous disk names.
    for component in relative.components() {
        let name = match component {
            Component::Normal(name) => name.to_str(),
            Component::CurDir => continue,
            _ => None,
        }.ok_or_else(|| io::Error::new(io::ErrorKind::PermissionDenied, "invalid scan directory"))?;
        let names = directory.names(MAX_DIRECTORY_ENTRIES, checkpoint)?;
        let name = FoldedNames::new(&names).resolve(name)?;
        directory = Arc::new(directory.child(&name, false)?);
    }
    let mut stats = ScanStats::default();
    if relative.components().next().and_then(|c| c.as_os_str().to_str())
        .is_some_and(|name| duduclaw_core::wiki_auto_namespace::segment_opens_as(name, "_archive")) {
        return Ok(stats);
    }
    walk(&root, &directory, relative.components().count(), &mut stats, visitor, checkpoint)?;
    Ok(stats)
}

fn walk(
    root: &Arc<Directory>, directory: &Arc<Directory>, depth: usize, stats: &mut ScanStats,
    visitor: &mut impl FnMut(ResolvedWikiPage) -> io::Result<()>,
    checkpoint: &mut impl FnMut() -> io::Result<()>,
) -> io::Result<()> {
    if depth > MAX_DEPTH { return Ok(()); }
    let names = directory.names(MAX_DIRECTORY_ENTRIES, checkpoint)?;
    let folded = FoldedNames::new(&names);
    for name in names {
        checkpoint()?;
        stats.entries += 1;
        let path = directory.path.join(&name);
        // One snapshot/map per directory, including directory aliases. Never
        // invoke the single-page resolver once per scanned file.
        let unique = name.to_str().is_some_and(|text| folded.resolve(text).is_ok());
        if !unique { stats.invalid_ids += 1; continue; }
        match directory.kind(&name)? {
            Kind::Directory => {
                if depth == 0 && name.to_str().is_some_and(|name|
                    duduclaw_core::wiki_auto_namespace::segment_opens_as(name, "_archive")) { continue; }
                let child = Arc::new(directory.child(&name, false)?);
                walk(root, &child, depth + 1, stats, visitor, checkpoint)?;
            }
            Kind::File if path.extension().is_some_and(|ext| ext == "md") => {
                if name.to_str().is_some_and(is_reserved) { continue; }
                match path.strip_prefix(&root.path).ok().and_then(crate::wiki_path::page_id_from_relative_path) {
                    Some(id) => visitor(ResolvedWikiPage::from_scan(root.clone(), directory.clone(), id)?)?,
                    None => stats.invalid_ids += 1,
                }
            }
            _ => {},
        }
    }
    Ok(())
}

pub fn collect(base: &Path, directory: &Path) -> Vec<String> {
    collect_resolved(base, directory).into_iter().map(|page| page.id).collect()
}

/// Keep the scanner's on-disk identities and shared parent handles so readers
/// do not resolve (and list the parent of) every page again.
pub(crate) fn collect_resolved(base: &Path, directory: &Path) -> Vec<ResolvedWikiPage> {
    let mut pages = Vec::new();
    match visit_resolved(base, directory, &mut |page| { pages.push(page); Ok(()) }, &mut || Ok(())) {
        Ok(stats) => {
            if stats.invalid_ids > 0 { tracing::warn!(invalid_ids = stats.invalid_ids, "wiki scan skipped invalid ids"); }
            pages.sort_by(|a, b| a.id.cmp(&b.id));
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
    fn scanner_lists_each_directory_once() {
        let root = tempfile::tempdir().unwrap();
        for directory in ["a", "b", "b/c"] {
            std::fs::create_dir_all(root.path().join(directory)).unwrap();
            for index in 0..32 {
                std::fs::write(root.path().join(directory).join(format!("{index}.md")), "body").unwrap();
            }
        }
        let mut pages = Vec::new();
        let before = crate::wiki_path::directory::LISTING_COUNT.with(|count| count.get());
        visit(root.path(), root.path(), &mut |id| {
            pages.push(id.to_owned()); Ok(())
        }, &mut || Ok(())).unwrap();
        assert_eq!(pages.len(), 96);
        let after = crate::wiki_path::directory::LISTING_COUNT.with(|count| count.get());
        assert_eq!(after - before, 4, "one real directory listing per directory");
    }

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
