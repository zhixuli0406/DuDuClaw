//! Incremental Wiki traversal. It never materializes every page before cancellation.
use super::source::{Table, in_range, push};
use super::writer::{Cell, LogicalRow};
use super::*;
pub(super) fn collect(
    home: &Path,
    ctx: &UserContext,
    request: &ExportRequest,
    table: &mut Table,
    bytes: &mut usize,
    checkpoint: impl FnMut() -> ExportResult<()>,
) -> ExportResult<()> {
    let checkpoint = std::cell::RefCell::new(checkpoint);
    table.headers = vec![
        "page_path",
        "scope",
        "agent_id",
        "title",
        "markdown",
        "updated_at_utc",
    ];
    let shared = request.scope.as_deref() == Some("shared");
    let root = if shared {
        home.join("shared/wiki")
    } else {
        home.join("agents")
            .join(
                request
                    .agent_id
                    .as_deref()
                    .ok_or_else(|| "EXPORT_INVALID_REQUEST".to_string())?,
            )
            .join("wiki")
    };
    let metadata = match std::fs::symlink_metadata(&root) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err("EXPORT_SOURCE_UNAVAILABLE".into()),
    };
    if duduclaw_memory::wiki_path::is_link_metadata(&metadata) || !metadata.is_dir() {
        return Err("EXPORT_SOURCE_UNAVAILABLE".into());
    }
    let store = if shared {
        duduclaw_memory::WikiStore::new_shared(home)
    } else {
        duduclaw_memory::WikiStore::new(root.clone())
    };
    let mut entries = 0;
    let mut pages = 0;
    visit(
        &root,
        &root,
        0,
        &mut entries,
        &mut |relative| {
            (&mut *checkpoint.borrow_mut())()?;
            let live = authorize(home, ctx, request)?;
            if shared {
                let reader = crate::handlers::SharedWikiReader::new(home, &live)
                    .map_err(|_| "EXPORT_PERMISSION_DENIED".to_string())?;
                if !reader.page_visible(relative) {
                    return Ok(());
                }
            }
            pages += 1;
            if pages > MAX_ROWS {
                return Err("EXPORT_TOO_LARGE".into());
            }
            let (page, raw) = store
                .read_export_page(relative, MAX_BYTES.saturating_sub(*bytes as u64))
                .map_err(|error| match error {
                    duduclaw_core::error::DuDuClawError::Memory(code)
                        if code == "EXPORT_TOO_LARGE" =>
                    {
                        code
                    }
                    _ => "EXPORT_SOURCE_UNAVAILABLE".into(),
                })?;
            if in_range(request, &page.updated.to_rfc3339())? {
                let id = format!(
                    "wiki:{}:{}:{}",
                    if shared { "shared" } else { "agent" },
                    request.agent_id.as_deref().unwrap_or(""),
                    relative
                );
                let cells = vec![
                    Cell::from(relative),
                    Cell::from(if shared { "shared" } else { "agent" }),
                    Cell::from(request.agent_id.clone().unwrap_or_default()),
                    Cell::from(page.title),
                    Cell::from(raw),
                    Cell::from(page.updated.to_rfc3339()),
                ];
                push(table, LogicalRow { id, cells }, bytes)?;
            }
            Ok(())
        },
        &mut || (&mut *checkpoint.borrow_mut())(),
    )?;
    table.rows.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(())
}
fn visit(
    root: &Path,
    directory: &Path,
    depth: usize,
    entries: &mut usize,
    visitor: &mut impl FnMut(&str) -> ExportResult<()>,
    checkpoint: &mut impl FnMut() -> ExportResult<()>,
) -> ExportResult<duduclaw_memory::wiki_scan::ScanStats> {
    let _ = depth; // Depth is derived from the root by the shared walker.
    let stats = duduclaw_memory::wiki_scan::visit(root, directory,
        &mut |id| visitor(id).map_err(std::io::Error::other),
        &mut || {
            checkpoint().map_err(std::io::Error::other)?;
            *entries += 1;
            if *entries > MAX_ROWS * 2 { return Err(std::io::Error::other("EXPORT_TOO_LARGE")); }
            Ok(())
        },
    ).map_err(|error| {
        if error.kind() == std::io::ErrorKind::Other { error.to_string() }
        else { "EXPORT_SOURCE_UNAVAILABLE".to_string() }
    })?;
    if stats.invalid_ids > 0 {
        tracing::warn!(invalid_ids = stats.invalid_ids, "wiki export skipped invalid ids");
    }
    Ok(stats)
}


#[cfg(all(test, unix))]
#[test]
fn export_walker_skips_directory_and_file_links() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("ordinary.md"), "ordinary").unwrap();
    std::fs::write(outside.path().join("secret.md"), "secret").unwrap();
    symlink(outside.path(), root.path().join("alias")).unwrap();
    symlink(outside.path().join("secret.md"), root.path().join("alias.md")).unwrap();
    let mut found = Vec::new();
    visit(root.path(), root.path(), 0, &mut 0, &mut |id| {
        found.push(id.to_string());
        Ok(())
    }, &mut || Ok(())).unwrap();
    assert_eq!(found, vec!["ordinary.md".to_string()]);
}

#[cfg(test)]
#[test]
fn rm209_export_scan_matches_store_and_counts_invalid_ids() {
    let root = tempfile::tempdir().unwrap();
    for id in ["notes/a.md", "_archive/old.md", "notes/_INDEX.md", "notes/_archive/kept.md"] {
        let path = root.path().join(id);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "---\ntitle: Page\n---\nbody").unwrap();
    }
    #[cfg(unix)]
    std::fs::write(root.path().join("bad\\name.md"), "body").unwrap();
    let mut exported = Vec::new();
    let stats = visit(root.path(), root.path(), 0, &mut 0, &mut |id| {
        exported.push(id.to_string()); Ok(())
    }, &mut || Ok(())).unwrap();
    exported.sort();
    let store = duduclaw_memory::WikiStore::new(root.path().to_path_buf());
    let mut listed: Vec<_> = store.list_pages().unwrap().into_iter().map(|p| p.path).collect();
    listed.sort();
    assert_eq!(exported, vec!["notes/_archive/kept.md", "notes/a.md"]);
    assert_eq!(exported, listed);
    assert_eq!(stats.invalid_ids, if cfg!(unix) { 1 } else { 0 });
}

#[cfg(all(test, unix))]
#[test]
fn rm209_export_refuses_linked_start() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let alias = root.path().join("linked");
    std::os::unix::fs::symlink(outside.path(), &alias).unwrap();
    assert!(visit(&alias, &alias, 0, &mut 0, &mut |_| Ok(()), &mut || Ok(())).is_err());
}
