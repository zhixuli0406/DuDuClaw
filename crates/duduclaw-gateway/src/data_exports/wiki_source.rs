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
    if !root.exists() {
        return Ok(());
    }
    if std::fs::symlink_metadata(&root)
        .map_err(|_| "EXPORT_SOURCE_UNAVAILABLE".to_string())?
        .file_type()
        .is_symlink()
    {
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
            authorize(home, ctx, request)?;
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
) -> ExportResult<()> {
    if depth > 20 {
        return Err("EXPORT_TOO_LARGE".into());
    }
    for entry in
        std::fs::read_dir(directory).map_err(|_| "EXPORT_SOURCE_UNAVAILABLE".to_string())?
    {
        checkpoint()?;
        *entries += 1;
        if *entries > MAX_ROWS * 2 {
            return Err("EXPORT_TOO_LARGE".into());
        }
        let entry = entry.map_err(|_| "EXPORT_SOURCE_UNAVAILABLE".to_string())?;
        let kind = entry
            .file_type()
            .map_err(|_| "EXPORT_SOURCE_UNAVAILABLE".to_string())?;
        let path = entry.path();
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            if depth == 0 && entry.file_name() == "_archive" {
                continue;
            }
            visit(root, &path, depth + 1, entries, visitor, checkpoint)?;
        } else if kind.is_file() && path.extension().is_some_and(|extension| extension == "md") {
            if ["_schema.md", "_index.md", "_log.md"]
                .iter()
                .any(|reserved| entry.file_name() == *reserved)
            {
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .map_err(|_| "EXPORT_SOURCE_UNAVAILABLE".to_string())?
                .components()
                .map(|component| {
                    component
                        .as_os_str()
                        .to_str()
                        .ok_or_else(|| "EXPORT_INVALID_UTF8".to_string())
                })
                .collect::<ExportResult<Vec<_>>>()?
                .join("/");
            visitor(&relative)?;
        }
    }
    Ok(())
}
