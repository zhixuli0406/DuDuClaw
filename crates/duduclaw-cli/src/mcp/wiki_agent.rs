use super::*;

pub(crate) async fn handle_wiki_ls(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let agent_id = args
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or(default_agent);

    // Visibility check for cross-agent access
    if agent_id != default_agent {
        match check_wiki_visibility(home_dir, agent_id, default_agent) {
            Ok(false) => {
                return tool_error(&format!(
                    "Agent '{}' wiki is not visible to '{}'. Ask the owner to add you to wiki_visible_to.",
                    agent_id, default_agent
                ));
            }
            Err(e) => return tool_error(&format!("Visibility check failed: {e}")),
            _ => {}
        }
    }

    let wiki_dir = match resolve_wiki_dir(home_dir, agent_id) {
        Ok(d) => d,
        Err(e) => return tool_error(&e),
    };

    if !wiki_dir.exists() {
        return tool_text(&format!(
            "No wiki found for agent '{}'. Use wiki_write to create the first page.",
            agent_id
        ));
    }

    let pages = collect_md_files(&wiki_dir, &wiki_dir);
    if pages.is_empty() {
        return tool_text("Wiki directory exists but contains no pages.");
    }

    let mut lines = Vec::with_capacity(pages.len() + 1);
    lines.push(format!(
        "Wiki for agent '{}' ({} pages):\n",
        agent_id,
        pages.len()
    ));

    for rel_path in &pages {
        let full_path = wiki_dir.join(rel_path);
        let content = std::fs::read_to_string(&full_path).unwrap_or_default();
        let title = extract_frontmatter_title(&content)
            .unwrap_or_else(|| rel_path.to_string_lossy().to_string());
        let updated = extract_frontmatter_updated(&content).unwrap_or_else(|| "?".to_string());
        lines.push(format!(
            "  {} — {} (updated: {})",
            rel_path.display(),
            title,
            updated
        ));
    }

    tool_text(&lines.join("\n"))
}

pub(crate) async fn handle_wiki_read(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let agent_id = args
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or(default_agent);
    let page_path = match args.get("page_path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return tool_error("Missing required parameter: page_path"),
    };

    // Visibility check for cross-agent access
    if agent_id != default_agent {
        match check_wiki_visibility(home_dir, agent_id, default_agent) {
            Ok(false) => {
                return tool_error(&format!(
                    "Agent '{}' wiki is not visible to '{}'. Ask the owner to add you to wiki_visible_to.",
                    agent_id, default_agent
                ));
            }
            Err(e) => return tool_error(&format!("Visibility check failed: {e}")),
            _ => {}
        }
    }

    // Shared validator, before the path touches the disk. Reserved files
    // (_index.md, _schema.md) may be read; only writes refuse them.
    if let Err(e) = validate_wiki_read_path(page_path) {
        return tool_error(&e);
    }

    let wiki_dir = match resolve_wiki_dir(home_dir, agent_id) {
        Ok(d) => d,
        Err(e) => return tool_error(&e),
    };

    // Keep the raw read, parsed-page check, and live trust check in one
    // cooperative snapshot. A verified CCR route independently reacquires
    // its own delivery lease after the MCP response arrives. W2-B: the fence
    // is the Wiki root being read, so another agent's Wiki is unaffected.
    let _wiki_read_lease =
        match duduclaw_memory::WikiDeliveryFence::for_wiki_dir(&wiki_dir).try_shared() {
            Ok(lease) => lease,
            Err(_) => return tool_error("Wiki is busy with a source update; retry the read"),
        };

    // Fail-closed, no-follow read: the page must resolve inside the wiki
    // directory and be a regular file opened with O_NOFOLLOW (a canonicalize
    // failure used to skip the escape check).
    let content = match duduclaw_memory::wiki_path::read_wiki_file_nofollow(&wiki_dir, page_path)
    {
        Ok(content) => content,
        Err(e) => return tool_error(&wiki_read_error(page_path, &e)),
    };
    let file_name = Path::new(page_path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if !WIKI_RESERVED.contains(&file_name)
        && let Some(reason) = wiki_page_withheld(home_dir, &wiki_dir, agent_id, page_path, &content)
    {
        return tool_error(reason);
    }
    tool_text(&content)
}

/// Why a (non-reserved) page's content must not leave the wiki, or `None`.
///
/// Shared by `wiki_read` and `wiki_share`, so sharing a page cannot publish
/// what reading it would withhold: the parsed page must match the raw bytes
/// just read, must not be `do_not_inject` or below trust 0.1, and the live
/// trust store (when present) must not quarantine it. Fail closed: an
/// unreadable page or trust store withholds.
pub(crate) fn wiki_page_withheld(
    home_dir: &Path,
    wiki_dir: &Path,
    agent_id: &str,
    page_path: &str,
    raw: &str,
) -> Option<&'static str> {
    let store = duduclaw_memory::WikiStore::new(wiki_dir.to_path_buf());
    let (page, parsed_raw) = match store.read_page_with_raw(page_path) {
        Ok(page) => page,
        Err(_) => return Some("Wiki page is unavailable or invalid"),
    };
    if parsed_raw != raw || page.do_not_inject || !page.trust.is_finite() || page.trust < 0.1 {
        return Some("Wiki page is quarantined or changed; content withheld");
    }
    let db = home_dir.join("wiki_trust.db");
    if db.exists() {
        let trust_state = (|| -> std::result::Result<Option<(f64, i64)>, rusqlite::Error> {
            let conn = rusqlite::Connection::open_with_flags(
                &db,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            conn.query_row(
                "SELECT trust,do_not_inject FROM wiki_trust_state
                 WHERE page_path=?1 AND agent_id=?2",
                rusqlite::params![page_path, agent_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
        })();
        match trust_state {
            Ok(Some((trust, do_not_inject)))
                if !trust.is_finite() || trust < 0.1 || do_not_inject != 0 =>
            {
                return Some("Wiki page is quarantined; content withheld");
            }
            Err(_) => return Some("Wiki trust state unavailable; content withheld"),
            _ => {}
        }
    }
    None
}

/// Best-effort activity-feed trace for knowledge writes. Wiki pages used to
/// land with zero dashboard footprint — the owner had no way to see "the agent
/// filed something" without opening the wiki itself. Mirrors
/// `handle_activity_post`'s storage path; failure never fails the tool.
pub(crate) async fn post_wiki_activity(home_dir: &Path, agent_id: &str, summary: String, page_path: &str) {
    let actor = if is_valid_agent_id(agent_id) {
        agent_id
    } else {
        "system"
    };
    let store = match duduclaw_gateway::task_store::TaskStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(error = %e, "wiki activity skipped: task store open failed");
            return;
        }
    };
    let row = duduclaw_gateway::task_store::ActivityRow {
        id: uuid::Uuid::new_v4().to_string(),
        event_type: "wiki_written".to_string(),
        agent_id: actor.to_string(),
        task_id: None,
        summary,
        timestamp: chrono::Utc::now().to_rfc3339(),
        metadata: Some(serde_json::json!({ "page_path": page_path }).to_string()),
    };
    if store.append_activity(&row).await.is_ok() {
        append_bus_event(home_dir, "activity.new", &activity_row_to_json(&row)).await;
    }
}

pub(crate) async fn handle_wiki_write(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let agent_id = args
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or(default_agent);
    let page_path = match args.get("page_path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return tool_error("Missing required parameter: page_path"),
    };
    let content = match args.get("content").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return tool_error("Missing required parameter: content"),
    };

    // M5: cross-agent visibility check (parity with the read/search paths) —
    // an agent must be in the target's wiki_visible_to to write into its wiki.
    if agent_id != default_agent {
        match check_wiki_visibility(home_dir, agent_id, default_agent) {
            Ok(false) => {
                return tool_error(&format!(
                    "Agent '{}' wiki is not visible to '{}'. Ask the owner to add you to wiki_visible_to.",
                    agent_id, default_agent
                ));
            }
            Err(e) => return tool_error(&format!("Visibility check failed: {e}")),
            _ => {}
        }
    }

    if let Err(e) = validate_wiki_page_path(page_path) {
        return tool_error(&e);
    }

    if content.len() > WIKI_MAX_PAGE_SIZE {
        return tool_error(&format!(
            "Content too large: {} bytes (max {})",
            content.len(),
            WIKI_MAX_PAGE_SIZE
        ));
    }

    let wiki_dir = match resolve_wiki_dir(home_dir, agent_id) {
        Ok(d) => d,
        Err(e) => return tool_error(&e),
    };

    // This MCP handler writes directly instead of using WikiStore. Hold the
    // same cooperative fence as WikiStore so a source-bound CCR reply cannot
    // be sent while this page and its index are being replaced. W2-B: the
    // fence is this Wiki root, taken with the bounded write wait so a short
    // delivery read delays the write instead of rejecting it.
    let wiki_mutation =
        match duduclaw_memory::WikiDeliveryFence::for_wiki_dir(&wiki_dir).exclusive_for_write() {
            Ok(guard) => guard,
            Err(_) => return tool_error("Wiki is busy with an active delivery; retry the write"),
        };

    // Ensure wiki scaffold exists
    if let Err(e) = ensure_wiki_dir(&wiki_dir) {
        return tool_error(&e);
    }

    // Validate and confine BEFORE creating anything: the nearest existing
    // ancestor must resolve inside the canonical wiki dir (fail closed), so a
    // symlinked subdirectory cannot make this create directories outside the
    // wiki. Checked again after the parents are created.
    let full_path = match duduclaw_memory::wiki_path::prepare_wiki_write_target(&wiki_dir, page_path)
    {
        Ok(p) => p,
        Err(e) => {
            return tool_error(&format!(
                "Page path refused: {e}. It must resolve inside the wiki directory."
            ));
        }
    };

    let is_new = std::fs::symlink_metadata(&full_path).is_err();

    // P2-B H-4: record where this write came from (host env only).
    let existing = duduclaw_memory::wiki_path::read_wiki_file_nofollow(&wiki_dir, page_path).ok();
    let stamped = match crate::mcp_memory_handlers::host_wiki_stamp(content, existing.as_deref()) {
        Ok(s) => s,
        Err(e) => return tool_error(&e),
    };
    let content = stamped.as_str();

    // Atomic write: O_EXCL temp file + rename (never through a planted link).
    if let Err(e) = duduclaw_memory::wiki_path::atomic_write_nofollow(&full_path, content) {
        return tool_error(&format!("Failed to write page: {e}"));
    }

    // Update _index.md
    let update_index = crate::mcp_args::arg_bool(args, "update_index").unwrap_or(true);

    if update_index {
        let title = extract_frontmatter_title(content).unwrap_or_else(|| page_path.to_string());
        if let Err(e) = update_wiki_index(&wiki_dir, page_path, &title) {
            warn!("Failed to update wiki index: {e}");
        }
    }

    // Append to _log.md
    let action = if is_new { "create" } else { "update" };
    if let Err(e) = append_wiki_log(&wiki_dir, action, page_path) {
        warn!("Failed to append wiki log: {e}");
    }

    drop(wiki_mutation);

    let verb = if is_new { "Created" } else { "Updated" };
    let title = extract_frontmatter_title(content).unwrap_or_else(|| page_path.to_string());
    post_wiki_activity(
        home_dir,
        default_agent,
        format!("寫入知識庫「{title}」"),
        page_path,
    )
    .await;
    tool_text(&format!("{} wiki page: {}", verb, page_path))
}

pub(crate) async fn handle_wiki_search(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let agent_id = args
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or(default_agent);

    // Visibility check for cross-agent access
    if agent_id != default_agent {
        match check_wiki_visibility(home_dir, agent_id, default_agent) {
            Ok(false) => {
                return tool_error(&format!(
                    "Agent '{}' wiki is not visible to '{}'. Ask the owner to add you to wiki_visible_to.",
                    agent_id, default_agent
                ));
            }
            Err(e) => return tool_error(&format!("Visibility check failed: {e}")),
            _ => {}
        }
    }

    let query = match args.get("query").and_then(|v| v.as_str()) {
        Some(q) if !q.is_empty() => q,
        _ => return tool_error("Missing required parameter: query"),
    };
    let limit: usize = crate::mcp_args::arg_u64(args, "limit")
        .map_or(10, |n| n as usize)
        .min(100);
    let min_trust: Option<f32> = crate::mcp_args::arg_f64(args, "min_trust").map(|t| t as f32);
    let layer_filter: Option<duduclaw_memory::WikiLayer> = args
        .get("layer")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok());
    let expand: bool = crate::mcp_args::arg_bool(args, "expand").unwrap_or(false);

    let wiki_dir = match resolve_wiki_dir(home_dir, agent_id) {
        Ok(d) => d,
        Err(e) => return tool_error(&e),
    };

    if !wiki_dir.exists() {
        return tool_text("No wiki found. Use wiki_write to create the first page.");
    }

    let store = duduclaw_memory::WikiStore::new(wiki_dir);
    let hits = match store.search_filtered(query, limit, min_trust, layer_filter, expand) {
        Ok(h) => h,
        Err(e) => return tool_error(&format!("Wiki search failed: {e}")),
    };

    if hits.is_empty() {
        return tool_text(&format!("No wiki pages match query: '{}'", query));
    }

    let mut output = format!("Found {} matching pages for '{}':\n\n", hits.len(), query);
    for h in &hits {
        let expanded_tag = if h.score == 0 { " [expanded]" } else { "" };
        output.push_str(&format!(
            "**{}** ({}) — relevance: {} | trust: {:.1} | layer: {}{}\n",
            h.title, h.path, h.score, h.trust, h.layer, expanded_tag
        ));
        for line in &h.context_lines {
            output.push_str(&format!("  {}\n", line));
        }
        output.push('\n');
    }

    tool_text(&output)
}

pub(crate) async fn handle_wiki_lint(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let agent_id = args
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or(default_agent);

    let wiki_dir = match resolve_wiki_dir(home_dir, agent_id) {
        Ok(d) => d,
        Err(e) => return tool_error(&e),
    };

    if !wiki_dir.exists() {
        return tool_text("No wiki found. Use wiki_write to create the first page.");
    }

    let store = duduclaw_memory::WikiStore::new(wiki_dir);
    match store.lint() {
        Ok(report) => {
            let mut output = format!("Wiki Lint Report for '{}'\n\n", agent_id);
            output.push_str(&format!("Total pages: {}\n", report.total_pages));
            output.push_str(&format!("Index entries: {}\n\n", report.index_entries));

            if report.orphan_pages.is_empty()
                && report.broken_links.is_empty()
                && report.stale_pages.is_empty()
            {
                output.push_str("All clear — no issues found.\n");
            } else {
                if !report.orphan_pages.is_empty() {
                    output.push_str(&format!("Orphan pages ({}):\n", report.orphan_pages.len()));
                    for p in &report.orphan_pages {
                        output.push_str(&format!("  - {}\n", p));
                    }
                    output.push('\n');
                }

                if !report.broken_links.is_empty() {
                    output.push_str(&format!("Broken links ({}):\n", report.broken_links.len()));
                    for (from, to) in &report.broken_links {
                        output.push_str(&format!("  - {} -> {} (not found)\n", from, to));
                    }
                    output.push('\n');
                }

                if !report.stale_pages.is_empty() {
                    output.push_str(&format!(
                        "Stale pages (>30 days) ({}):\n",
                        report.stale_pages.len()
                    ));
                    for p in &report.stale_pages {
                        output.push_str(&format!("  - {}\n", p));
                    }
                }
            }

            tool_text(&output)
        }
        Err(e) => tool_error(&format!("Lint failed: {e}")),
    }
}

pub(crate) async fn handle_wiki_stats(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let agent_id = args
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or(default_agent);

    let wiki_dir = match resolve_wiki_dir(home_dir, agent_id) {
        Ok(d) => d,
        Err(e) => return tool_error(&e),
    };

    if !wiki_dir.exists() {
        return tool_text(&format!("No wiki found for agent '{}'.", agent_id));
    }

    let store = duduclaw_memory::WikiStore::new(wiki_dir.clone());
    let pages = match store.list_pages() {
        Ok(p) => p,
        Err(e) => return tool_error(&format!("Failed to list pages: {e}")),
    };

    let index_content = std::fs::read_to_string(wiki_dir.join("_index.md")).unwrap_or_default();
    let index_entries = index_content
        .lines()
        .filter(|l| l.starts_with("- ["))
        .count();

    let log_content = std::fs::read_to_string(wiki_dir.join("_log.md")).unwrap_or_default();
    let log_entries = log_content
        .lines()
        .filter(|l| l.starts_with("## ["))
        .count();

    // Count by directory
    let mut by_dir: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for page in &pages {
        let dir = std::path::Path::new(&page.path)
            .parent()
            .and_then(|p| p.to_str())
            .unwrap_or("root")
            .to_string();
        *by_dir.entry(dir).or_insert(0) += 1;
    }

    let most_recent = pages
        .first()
        .map(|p| format!("{} ({})", p.title, p.updated.format("%Y-%m-%d")))
        .unwrap_or_else(|| "none".to_string());

    let mut output = format!("Wiki Stats for '{}'\n\n", agent_id);
    output.push_str(&format!("Total pages: {}\n", pages.len()));
    output.push_str(&format!("Index entries: {}\n", index_entries));
    output.push_str(&format!("Log entries: {}\n", log_entries));
    output.push_str(&format!("Most recent: {}\n\n", most_recent));

    output.push_str("By directory:\n");
    let mut dirs: Vec<_> = by_dir.into_iter().collect();
    dirs.sort_by(|a, b| b.1.cmp(&a.1));
    for (dir, count) in &dirs {
        output.push_str(&format!("  {}: {} pages\n", dir, count));
    }

    tool_text(&output)
}

pub(crate) async fn handle_wiki_export(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let agent_id = args
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or(default_agent);
    let format = args
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("html");

    // Visibility check for cross-agent access
    if agent_id != default_agent {
        match check_wiki_visibility(home_dir, agent_id, default_agent) {
            Ok(false) => {
                return tool_error(&format!(
                    "Agent '{}' wiki is not visible to '{}'. Ask the owner to add you to wiki_visible_to.",
                    agent_id, default_agent
                ));
            }
            Err(e) => return tool_error(&format!("Visibility check failed: {e}")),
            _ => {}
        }
    }

    let wiki_dir = match resolve_wiki_dir(home_dir, agent_id) {
        Ok(d) => d,
        Err(e) => return tool_error(&e),
    };

    if !wiki_dir.exists() {
        return tool_text("No wiki found. Nothing to export.");
    }

    let store = duduclaw_memory::WikiStore::new(wiki_dir);

    match format {
        "obsidian" => {
            let export_dir = home_dir
                .join("exports")
                .join(format!("{}-wiki-obsidian", agent_id));
            if let Err(e) = std::fs::create_dir_all(&export_dir) {
                return tool_error(&format!("Failed to create export directory: {e}"));
            }
            match store.export_obsidian(&export_dir) {
                Ok(count) => tool_text(&format!(
                    "Exported {} pages as Obsidian vault to:\n{}",
                    count,
                    export_dir.display()
                )),
                Err(e) => tool_error(&format!("Export failed: {e}")),
            }
        }
        "html" => match store.export_html() {
            Ok(html) => {
                let export_path = home_dir
                    .join("exports")
                    .join(format!("{}-wiki.html", agent_id));
                if let Some(parent) = export_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                match std::fs::write(&export_path, &html) {
                    Ok(()) => tool_text(&format!(
                        "Exported wiki as HTML ({} bytes) to:\n{}",
                        html.len(),
                        export_path.display()
                    )),
                    Err(e) => tool_error(&format!("Failed to write HTML: {e}")),
                }
            }
            Err(e) => tool_error(&format!("Export failed: {e}")),
        },
        _ => tool_error(&format!(
            "Unknown format '{}'. Use 'obsidian' or 'html'.",
            format
        )),
    }
}

// ---------------------------------------------------------------------------
// Shared Wiki handlers
// ---------------------------------------------------------------------------
