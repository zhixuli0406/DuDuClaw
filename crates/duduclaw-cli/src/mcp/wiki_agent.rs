use super::*;

use crate::mcp_memory_handlers::ReadScope;

// ---------------------------------------------------------------------------
// v1.84.0 (RM-200 item 1): conversation scope of auto-written pages
// ---------------------------------------------------------------------------

/// The read scope of this process's wiki tools (same turn identity as the
/// memory tools; an agent id is never an `external/` namespace).
fn wiki_read_scope(agent: &str) -> ReadScope {
    crate::mcp_memory_handlers::mcp_read_scope(agent)
}

/// Where a page path sits relative to the auto namespace (v1.84.0), judged
/// on the components the file system will open: normal components only
/// (`./auto/x.md` and `auto//x.md` are `auto/x.md`), the namespace segment
/// as `auto_wiki_page::is_auto_namespace_segment` folds it (`AUTO/`,
/// `auto./`, `auto /` all open `auto/` somewhere).
enum AutoNamespace<'a> {
    Outside,
    /// Inside: every normal component, the namespace segment first (as given).
    Inside(Vec<&'a str>),
    /// A component that cannot be judged (not UTF-8, a root, a prefix, `..`):
    /// treated as inside (fail closed).
    Unusable,
}

/// Classify `page_path` (string only — never touches the file system).
fn auto_namespace_of(page_path: &str) -> AutoNamespace<'_> {
    let mut parts: Vec<&str> = Vec::new();
    for component in Path::new(page_path).components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::Normal(s) => match s.to_str() {
                Some(s) => parts.push(s),
                None => return AutoNamespace::Unusable,
            },
            _ => return AutoNamespace::Unusable,
        }
    }
    match parts.first() {
        Some(first) if duduclaw_gateway::auto_wiki_page::is_auto_namespace_segment(first) => {
            AutoNamespace::Inside(parts)
        }
        _ => AutoNamespace::Outside,
    }
}

/// The auto-page registry a read in `scope` of `agent_id`'s wiki judges
/// against, loaded ONCE per tool call (review H1 item 3). Employee-wide
/// reads do not consult it (empty).
pub(crate) fn auto_registry_for(
    home_dir: &Path,
    agent_id: &str,
    scope: &ReadScope,
) -> duduclaw_gateway::auto_wiki_page::AutoPageRegistry {
    match scope {
        ReadScope::Employee => Default::default(),
        ReadScope::Conversation(_) => duduclaw_gateway::auto_wiki_page::load_registry(home_dir, agent_id),
    }
}

/// Whether the page `page_path` of `wiki_dir` must look like a page that does
/// not exist to this read: on a channel turn, every page under `auto/` that
/// `auto_wiki_page::page_visible_in_conversation` does not grant this
/// conversation — judged from the host-recorded provenance and the host
/// promotion mark, and only when `registry` (the gateway's record, see
/// [`auto_registry_for`]) holds exactly the page's current bytes; never from
/// `sources` / `author` / `layer`. Employee-wide reads see everything, as
/// before. Fail closed: an unusable component, an unreadable page or a page
/// the registry did not record hides it.
pub(crate) fn auto_page_hidden(
    wiki_dir: &Path,
    page_path: &str,
    scope: &ReadScope,
    registry: &duduclaw_gateway::auto_wiki_page::AutoPageRegistry,
) -> bool {
    auto_page_hidden_with(page_path, scope, registry, |rel| {
        duduclaw_memory::wiki_path::read_wiki_file_nofollow(wiki_dir, rel).ok()
    })
}

/// [`auto_page_hidden`] with the page's raw content supplied by `read`
/// (called with the path as it will be opened, only for a page under
/// `auto/` on a channel turn) — e.g. the content a search already read.
pub(crate) fn auto_page_hidden_with(
    page_path: &str,
    scope: &ReadScope,
    registry: &duduclaw_gateway::auto_wiki_page::AutoPageRegistry,
    read: impl FnOnce(&str) -> Option<String>,
) -> bool {
    let ReadScope::Conversation(session) = scope else {
        return false;
    };
    let parts = match auto_namespace_of(page_path) {
        AutoNamespace::Outside => return false,
        AutoNamespace::Unusable => return true,
        AutoNamespace::Inside(parts) => parts,
    };
    let raw = read(&parts.join("/"));
    let namespace = duduclaw_gateway::auto_wiki_page::AUTO_NAMESPACE;
    // An alias spelling (`AUTO/`, `auto./`) is looked up under the
    // canonical name; the registry records only the gateway's own paths, so
    // a differently spelled file name finds no record and stays hidden.
    let scoped_path = format!("{namespace}/{}", parts[1..].join("/"));
    !duduclaw_gateway::auto_wiki_page::page_visible_in_conversation(
        &scoped_path,
        raw.as_deref(),
        session.as_deref(),
        registry,
    )
}

/// The one answer every MCP wiki mutation gives when its source or
/// destination is inside `auto/` (v1.84.0, review H1), for every AI-callable
/// scope — a channel conversation turn and an employee-wide call alike.
/// Fixed text, decided from the path string before any file-system access,
/// so it is the same whether or not the page exists.
pub(crate) const WIKI_AUTO_NAMESPACE_REFUSED: &str =
    "Not available to AI tools: pages under auto/ are filed automatically from conversations and \
     are changed only from the curation station. Use a path outside auto/, or ask an administrator.";

/// Whether an MCP wiki mutation of `page_path` (its source or destination)
/// must be refused with [`WIKI_AUTO_NAMESPACE_REFUSED`]: any path inside
/// `auto/` of any agent's wiki (an unusable path counts as inside), whatever
/// the read scope — the employee-wide scope included (review H1 round 2: an
/// employee-wide `wiki_write` could otherwise overwrite a conversation's
/// page). Only the gateway's auto writer (`auto_wiki_page::write_auto_page`)
/// and the dashboard's human actions (promote, archive, share-out) write
/// there.
pub(crate) fn auto_namespace_mutation_refused(page_path: &str) -> bool {
    !matches!(auto_namespace_of(page_path), AutoNamespace::Outside)
}

/// The fixed `wiki_export` answer on a channel conversation turn (v1.84.0).
pub(crate) const WIKI_EXPORT_CHANNEL_TURN_REFUSED: &str =
    "Not available from a conversation: a wiki export covers the pages of every conversation \
     of this employee, so it cannot be used on a chat turn. Ask an administrator.";

/// On a channel turn, `_index.md` / `_log.md` without the lines that name a
/// page of the auto namespace (they carry the titles and paths of other
/// conversations' pages). Fail closed: every such line is dropped, this
/// conversation's own auto pages included (they stay reachable through
/// `wiki_ls` / `wiki_search`).
fn redact_reserved_listing(content: &str, scope: &ReadScope) -> String {
    if *scope == ReadScope::Employee {
        return content.to_string();
    }
    let marker = format!("{}/", duduclaw_gateway::auto_wiki_page::AUTO_NAMESPACE);
    let mut out: String = content
        .lines()
        .filter(|line| !line.to_ascii_lowercase().contains(&marker))
        .collect::<Vec<_>>()
        .join("\n");
    if content.ends_with('\n') {
        out.push('\n');
    }
    out
}

pub(crate) async fn handle_wiki_ls(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let scope = wiki_read_scope(default_agent);
    handle_wiki_ls_in(args, home_dir, default_agent, &scope).await
}

/// [`handle_wiki_ls`] with an explicit [`ReadScope`] (v1.84.0: hidden auto
/// pages are not listed nor counted).
pub(crate) async fn handle_wiki_ls_in(
    args: &Value,
    home_dir: &Path,
    default_agent: &str,
    scope: &ReadScope,
) -> Value {
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

    let registry = auto_registry_for(home_dir, agent_id, scope);
    let pages: Vec<std::path::PathBuf> = collect_md_files(&wiki_dir, &wiki_dir)
        .into_iter()
        .filter(|rel| !auto_page_hidden(&wiki_dir, &rel.to_string_lossy(), scope, &registry))
        .collect();
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
    let scope = wiki_read_scope(default_agent);
    handle_wiki_read_in(args, home_dir, default_agent, &scope).await
}

/// [`handle_wiki_read`] with an explicit [`ReadScope`] (v1.84.0: a hidden
/// auto page answers exactly like a missing page).
pub(crate) async fn handle_wiki_read_in(
    args: &Value,
    home_dir: &Path,
    default_agent: &str,
    scope: &ReadScope,
) -> Value {
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
    // v1.84.0: another conversation's auto page is "not found" — the same
    // text a missing page gets, before any other check can tell them apart.
    let registry = auto_registry_for(home_dir, agent_id, scope);
    if auto_page_hidden(&wiki_dir, page_path, scope, &registry) {
        let missing = std::io::Error::from(std::io::ErrorKind::NotFound);
        return tool_error(&wiki_read_error(page_path, &missing));
    }
    if WIKI_RESERVED.contains(&file_name) {
        return tool_text(&redact_reserved_listing(&content, scope));
    }
    if let Some(reason) = wiki_page_withheld(home_dir, &wiki_dir, agent_id, page_path, &content) {
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
    if duduclaw_mcp_boundary::io_authority::verify_before_io().is_err() { return; }
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
    if store.append_activity(&row).await.is_ok()
        && duduclaw_mcp_boundary::io_authority::verify_before_io().is_ok() {
        append_bus_event(home_dir, "activity.new", &activity_row_to_json(&row)).await;
    }
}

pub(crate) async fn handle_wiki_write(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let scope = wiki_read_scope(default_agent);
    handle_wiki_write_in(args, home_dir, default_agent, &scope).await
}

/// [`handle_wiki_write`] with an explicit [`ReadScope`] (v1.84.0: nothing
/// under `auto/` is written, in any scope — review H1 round 2; the scope is
/// kept in the signature for the callers and tests that pass one).
pub(crate) async fn handle_wiki_write_in(
    args: &Value,
    home_dir: &Path,
    default_agent: &str,
    _scope: &ReadScope,
) -> Value {
    if let Err(error) = duduclaw_mcp_boundary::io_authority::verify_before_io() {
        return tool_error(&error.to_string());
    }
    let agent_id = args
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or(default_agent);
    let page_path = match args.get("page_path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return tool_error("Missing required parameter: page_path"),
    };
    // v1.84.0 (review H1): before any file-system access, one fixed answer
    // whether or not the page exists (no existence oracle, no overwrite or
    // forgery of a conversation's auto page), for every scope.
    if auto_namespace_mutation_refused(page_path) {
        return tool_error(WIKI_AUTO_NAMESPACE_REFUSED);
    }
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

    // The delivery fence may block after dispatch authorization. Recheck the
    // live run before any page/scaffold mutation once that wait has finished.
    if let Err(error) = duduclaw_mcp_boundary::io_authority::verify_before_io() {
        return tool_error(&error.to_string());
    }
    // Ensure wiki scaffold exists
    if let Err(e) = ensure_wiki_dir(&wiki_dir) {
        return tool_error(&e);
    }

    // Validate and confine BEFORE creating anything: the nearest existing
    // ancestor must resolve inside the canonical wiki dir (fail closed), so a
    // symlinked subdirectory cannot make this create directories outside the
    // wiki. Checked again after the parents are created.
    if let Err(error) = duduclaw_mcp_boundary::io_authority::verify_before_io() {
        return tool_error(&error.to_string());
    }
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
    if let Err(error) = duduclaw_mcp_boundary::io_authority::verify_before_io() {
        return tool_error(&error.to_string());
    }
    if let Err(e) = duduclaw_memory::wiki_path::atomic_write_nofollow_checked(&full_path, content, || {
        duduclaw_mcp_boundary::io_authority::verify_before_io()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::PermissionDenied, e.to_string()))
    }) {
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
    let scope = wiki_read_scope(default_agent);
    handle_wiki_search_in(args, home_dir, default_agent, &scope).await
}

/// [`handle_wiki_search`] with an explicit [`ReadScope`] (v1.84.0: hidden
/// auto pages are neither returned nor counted, and do not use up `limit`).
///
/// Review L1: on a channel turn the visibility rule runs inside the search
/// (`WikiStore::search_filtered_visible`), on every matching page before the
/// ranking cut-off, so any number of better-ranked hidden pages cannot crowd
/// this conversation's pages out. The search's hard cap on judged hits
/// (`wiki::MAX_VISIBLE_SEARCH_SCAN`) is logged when reached.
pub(crate) async fn handle_wiki_search_in(
    args: &Value,
    home_dir: &Path,
    default_agent: &str,
    scope: &ReadScope,
) -> Value {
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
    let searched = match scope {
        ReadScope::Employee => store.search_filtered(query, limit, min_trust, layer_filter, expand),
        // Hidden pages are dropped inside the search, before its ranking
        // cut-off (direct hits and expanded neighbours alike), judged on the
        // content the search itself read and the registry loaded once here.
        ReadScope::Conversation(_) => {
            let registry = auto_registry_for(home_dir, agent_id, scope);
            let visible = |rel: &str, raw: &str| {
                !auto_page_hidden_with(rel, scope, &registry, |_| Some(raw.to_string()))
            };
            store.search_filtered_visible(query, limit, min_trust, layer_filter, expand, &visible)
        }
    };
    let hits = match searched {
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

    let store = duduclaw_memory::WikiStore::new(wiki_dir.clone());
    // v1.84.0: on a channel turn other conversations' auto pages are neither
    // listed nor counted.
    let scope = wiki_read_scope(default_agent);
    match store.lint() {
        Ok(mut report) => {
            if scope != ReadScope::Employee {
                let registry = auto_registry_for(home_dir, agent_id, &scope);
                let hidden = |p: &str| auto_page_hidden(&wiki_dir, p, &scope, &registry);
                let hidden_total = collect_md_files(&wiki_dir, &wiki_dir)
                    .iter()
                    .filter(|rel| hidden(rel.to_string_lossy().as_ref()))
                    .count();
                report.total_pages = report.total_pages.saturating_sub(hidden_total);
                report.orphan_pages.retain(|p| !hidden(p.as_str()));
                report.stale_pages.retain(|p| !hidden(p.as_str()));
                report.broken_links.retain(|(from, _)| !hidden(from.as_str()));
                let index = std::fs::read_to_string(wiki_dir.join("_index.md")).unwrap_or_default();
                report.index_entries = redact_reserved_listing(&index, &scope)
                    .lines()
                    .filter(|l| l.starts_with("- ["))
                    .count();
            }
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

    // v1.84.0: on a channel turn other conversations' auto pages are neither
    // counted nor named (pages, index and log lines alike).
    let scope = wiki_read_scope(default_agent);
    let registry = auto_registry_for(home_dir, agent_id, &scope);
    let store = duduclaw_memory::WikiStore::new(wiki_dir.clone());
    let pages = match store.list_pages() {
        Ok(p) => p
            .into_iter()
            .filter(|p| !auto_page_hidden(&wiki_dir, &p.path, &scope, &registry))
            .collect::<Vec<_>>(),
        Err(e) => return tool_error(&format!("Failed to list pages: {e}")),
    };

    let index_content = redact_reserved_listing(
        &std::fs::read_to_string(wiki_dir.join("_index.md")).unwrap_or_default(),
        &scope,
    );
    let index_entries = index_content
        .lines()
        .filter(|l| l.starts_with("- ["))
        .count();

    let log_content = redact_reserved_listing(
        &std::fs::read_to_string(wiki_dir.join("_log.md")).unwrap_or_default(),
        &scope,
    );
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

    // v1.84.0: an export copies (and counts) every conversation's auto pages,
    // so it is not available on a channel conversation turn.
    if wiki_read_scope(default_agent) != ReadScope::Employee {
        return tool_error(WIKI_EXPORT_CHANNEL_TURN_REFUSED);
    }

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
                match duduclaw_memory::wiki_path::atomic_write_nofollow_checked(&export_path, &html, || {
                    duduclaw_mcp_boundary::io_authority::verify_before_io()
                        .map_err(|e| std::io::Error::new(std::io::ErrorKind::PermissionDenied, e.to_string()))
                }) {
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

#[cfg(test)]
mod conversation_scope_tests {
    use super::*;

    fn text(v: &Value) -> String {
        v["content"][0]["text"].as_str().unwrap_or("").to_string()
    }

    /// File an auto page the way the knowledge route does.
    fn file_page(home: &Path, slug: &str, original: &str, source_id: &str) -> String {
        let store = duduclaw_memory::WikiStore::new(home.join("agents").join("alice").join("wiki"));
        duduclaw_gateway::auto_wiki_page::write_auto_page(
            &store,
            home,
            "alice",
            &duduclaw_gateway::auto_wiki_page::AutoPageRequest {
                doc_type: duduclaw_gateway::knowledge_route::DocType::Sop,
                title: "退款流程".into(),
                slug: slug.into(),
                summary: "退款流程摘要".into(),
                original: original.into(),
                source_label: "Telegram 對話".into(),
                source_id: source_id.into(),
            },
        )
        .unwrap()
        .path()
        .to_string()
    }

    /// RM-200 item 1: on a channel turn in A, conversation B's auto page —
    /// filed under the same slug, which now gives B its own page — reads
    /// exactly like a missing page, is not listed, counted or found, and its
    /// title is not in `_index.md`; A's own auto page and a curated page stay
    /// visible; an unknown conversation sees no auto page; an employee-wide
    /// read sees everything.
    #[tokio::test]
    async fn other_conversations_auto_pages_look_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let agent_dir = home.join("agents").join("alice");
        std::fs::create_dir_all(agent_dir.join("wiki")).unwrap();
        std::fs::write(agent_dir.join("agent.toml"), "[agent]\nname = \"alice\"\n").unwrap();
        let a_page = file_page(home, "refunds", "kestrelmoor 退款流程：A 客戶的版本。", "conversation:telegram:A:m:1");
        let b_page = file_page(home, "refunds", "kestrelmoor 退款流程：B 客戶的秘密版本。", "conversation:telegram:B:m:2");
        assert_ne!(a_page, b_page, "no cross-conversation merge");
        duduclaw_memory::WikiStore::new(agent_dir.join("wiki"))
            .write_page(
                "concepts/policy.md",
                "---\ntitle: \"公開政策\"\nauthor: \"operator\"\nlayer: core\ntrust: 0.900\n---\n\nkestrelmoor 公開政策。",
            )
            .unwrap();

        let in_a = ReadScope::Conversation(Some("telegram:A".into()));
        let read = |path: String, scope: ReadScope| async move {
            handle_wiki_read_in(&serde_json::json!({ "page_path": path }), home, "alice", &scope).await
        };
        let hidden = read(b_page.clone(), in_a.clone()).await;
        let missing = read("auto/sop/no-such-page.md".into(), in_a.clone()).await;
        assert_eq!(hidden["isError"], true, "{hidden}");
        assert_eq!(text(&hidden), format!("Page not found: {b_page}"));
        assert_eq!(text(&missing), "Page not found: auto/sop/no-such-page.md");
        assert_eq!(hidden.as_object().map(|o| o.len()), missing.as_object().map(|o| o.len()));
        for alias in [format!("./{b_page}"), b_page.replacen("auto/", "AUTO/", 1)] {
            let r = read(alias.clone(), in_a.clone()).await;
            assert!(!text(&r).contains("秘密"), "{alias}: {r}");
        }
        assert!(text(&read(a_page.clone(), in_a.clone()).await).contains("A 客戶"));
        assert!(text(&read("concepts/policy.md".into(), in_a.clone()).await).contains("公開政策"));
        let index = text(&read("_index.md".into(), in_a.clone()).await);
        assert!(!index.contains(&b_page), "{index}");

        let ls = text(&handle_wiki_ls_in(&serde_json::json!({}), home, "alice", &in_a).await);
        assert!(ls.contains("(2 pages)") && ls.contains(&a_page) && !ls.contains(&b_page), "{ls}");
        let found = text(&handle_wiki_search_in(&serde_json::json!({ "query": "kestrelmoor" }), home, "alice", &in_a).await);
        assert!(!found.contains(&b_page) && !found.contains("秘密"), "{found}");
        assert!(found.starts_with("Found 2 matching pages"), "{found}");

        let unknown = ReadScope::Conversation(None);
        let r = read(a_page.clone(), unknown.clone()).await;
        assert_eq!(text(&r), format!("Page not found: {a_page}"));
        let ls = text(&handle_wiki_ls_in(&serde_json::json!({}), home, "alice", &unknown).await);
        assert!(ls.contains("(1 pages)"), "{ls}");

        let ls = text(&handle_wiki_ls_in(&serde_json::json!({}), home, "alice", &ReadScope::Employee).await);
        assert!(ls.contains("(3 pages)") && ls.contains(&b_page), "{ls}");
        assert!(text(&read(b_page.clone(), ReadScope::Employee).await).contains("秘密"));
    }

    fn setup(home: &Path) -> std::path::PathBuf {
        let agent_dir = home.join("agents").join("alice");
        std::fs::create_dir_all(agent_dir.join("wiki")).unwrap();
        std::fs::write(agent_dir.join("agent.toml"), "[agent]\nname = \"alice\"\n").unwrap();
        agent_dir.join("wiki")
    }

    /// Review H1: every MCP mutation of the agent wiki (`wiki_write`;
    /// `wiki_share`, whose source is the agent wiki — the agent wiki has no
    /// MCP append / delete / move / import tool) is refused inside `auto/`
    /// with ONE fixed text, in every AI-callable scope (a channel turn, an
    /// unknown conversation, and — review H1 round 2 — the employee-wide
    /// scope), identical for an existing page (B's), a missing page and every
    /// alias of the namespace, decided before any file-system access; B's
    /// page is untouched.
    #[tokio::test]
    async fn no_scope_can_mutate_the_auto_namespace() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let wiki = setup(home);
        let b_page = file_page(home, "refunds", "kestrelmoor 退款流程：B 客戶的秘密版本。", "conversation:telegram:B:m:2");
        let b_before = std::fs::read(wiki.join(&b_page)).unwrap();
        let in_a = ReadScope::Conversation(Some("telegram:A".into()));
        let refused = tool_error(WIKI_AUTO_NAMESPACE_REFUSED);

        let forged = "---\ntitle: \"forged\"\nauthor: \"operator\"\nlayer: core\n\
                      sources: [\"conversation:telegram:B:m:9\"]\nhost_promoted: true\n---\n\nforged\n";
        let targets = [
            b_page.clone(),
            "auto/sop/no-such-page.md".to_string(),
            format!("./{b_page}"),
            b_page.replacen("auto/", "AUTO/", 1),
            b_page.replacen("auto/", "auto./", 1),
            b_page.replacen("auto/", "auto /", 1),
            b_page.replacen("auto/", "au\u{200d}to/", 1),
            "auto".to_string(),
        ];
        let unknown = ReadScope::Conversation(None);
        for scope in [&in_a, &unknown, &ReadScope::Employee] {
            for target in &targets {
                for update_index in [true, false] {
                    let args = serde_json::json!({
                        "page_path": target, "content": forged, "update_index": update_index,
                    });
                    let got = handle_wiki_write_in(&args, home, "alice", scope).await;
                    assert_eq!(got, refused, "wiki_write {target:?} {scope:?}");
                }
                let got = handle_wiki_share_in(&serde_json::json!({ "page_path": target }), home, "alice", scope).await;
                assert_eq!(got, refused, "wiki_share {target:?} {scope:?}");
            }
            // Another agent's wiki is refused alike.
            let args = serde_json::json!({ "page_path": b_page, "content": forged, "agent_id": "bob" });
            assert_eq!(handle_wiki_write_in(&args, home, "alice", scope).await, refused, "{scope:?}");
        }

        assert_eq!(std::fs::read(wiki.join(&b_page)).unwrap(), b_before, "B's page untouched");
        for missing in ["auto/sop/no-such-page.md", "auto/sop/manual.md"] {
            assert!(!wiki.join(missing).exists(), "{missing}");
        }
        let index = std::fs::read_to_string(wiki.join("_index.md")).unwrap_or_default();
        assert!(!index.contains("no-such-page"), "{index}");
        assert!(!home.join("shared/wiki/sources").join("alice--refunds.md").exists());

        // Outside auto/ every scope still writes.
        for (scope, page) in [(&in_a, "concepts/notes.md"), (&ReadScope::Employee, "concepts/employee-notes.md")] {
            let args = serde_json::json!({ "page_path": page, "content": "---\ntitle: n\n---\n\nn\n" });
            let got = text(&handle_wiki_write_in(&args, home, "alice", scope).await);
            assert!(got.starts_with("Created"), "{scope:?}: {got}");
        }
        // The Employee-scope write that used to land under auto/ (and keep
        // B's host stamp on an overwrite of B's page) is refused.
        let args = serde_json::json!({ "page_path": "auto/sop/manual.md", "content": forged });
        assert_eq!(handle_wiki_write_in(&args, home, "alice", &ReadScope::Employee).await, refused);
    }

    /// Review H1: visibility of an auto page is decided by the host-recorded
    /// provenance and the host promotion mark only. A page planted under
    /// `auto/` whose `sources` name B, whose `author` / `layer` claim a
    /// promotion, but without host provenance, is invisible to B, to A and to
    /// an unknown conversation; a page `promote_page` promoted is visible in
    /// every conversation.
    #[tokio::test]
    async fn forged_content_fields_do_not_grant_visibility() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let wiki = setup(home);
        let store = duduclaw_memory::WikiStore::new(wiki.clone());
        store
            .write_page(
                "auto/sop/planted.md",
                "---\ntitle: \"planted\"\nauthor: \"operator\"\nlayer: core\ntrust: 0.900\n\
                 sources: [\"conversation:telegram:B:m:2\"]\n---\n\nkestrelmoor planted text\n",
            )
            .unwrap();
        // Review H1 item 3: host fields that DO parse, written straight to
        // disk (as an employee's own file tool could) — B's host source, the
        // promotion mark, the legacy marker — grant nothing without the
        // gateway's registry record.
        for (name, host, promoted) in [
            ("forged-own", "conversation:telegram:B:m:2", false),
            ("forged-promoted", "conversation:telegram:B:m:2", true),
            ("forged-legacy", duduclaw_gateway::wiki_host_sources::LEGACY_PROMOTION_SOURCE, true),
        ] {
            let body = duduclaw_gateway::wiki_host_sources::set_host_fields(
                "---\ntitle: \"planted\"\nauthor: \"auto-distill\"\nlayer: context\ntrust: 0.900\n---\n\nkestrelmoor planted text\n",
                &[host.to_string()],
                promoted,
            );
            std::fs::write(wiki.join(format!("auto/sop/{name}.md")), body).unwrap();
        }
        let promoted = file_page(home, "policy", "kestrelmoor 已確認的公開流程。", "conversation:telegram:B:m:3");
        duduclaw_gateway::auto_wiki_page::promote_page(&store, &promoted).unwrap();

        for scope in [
            ReadScope::Conversation(Some("telegram:B".into())),
            ReadScope::Conversation(Some("telegram:A".into())),
            ReadScope::Conversation(None),
        ] {
            let read = |path: &str| {
                let scope = scope.clone();
                let args = serde_json::json!({ "page_path": path });
                async move { handle_wiki_read_in(&args, home, "alice", &scope).await }
            };
            for name in ["planted", "forged-own", "forged-promoted", "forged-legacy"] {
                let path = format!("auto/sop/{name}.md");
                let planted = read(path.as_str()).await;
                assert_eq!(text(&planted), format!("Page not found: {path}"), "{scope:?}");
            }
            assert!(text(&read(promoted.as_str()).await).contains("已確認"), "{scope:?}");
            let ls = text(&handle_wiki_ls_in(&serde_json::json!({}), home, "alice", &scope).await);
            assert!(!ls.contains("planted") && ls.contains(&promoted), "{scope:?}: {ls}");
            let found =
                text(&handle_wiki_search_in(&serde_json::json!({ "query": "kestrelmoor" }), home, "alice", &scope).await);
            assert!(!found.contains("planted") && found.contains(&promoted), "{scope:?}: {found}");
        }
        let employee = handle_wiki_read_in(&serde_json::json!({ "page_path": "auto/sop/planted.md" }), home, "alice", &ReadScope::Employee).await;
        assert!(text(&employee).contains("planted text"));
    }

    /// Review L1: more than 2,000 better-ranked hidden auto pages do not
    /// crowd this conversation's page out of a scoped search (`limit = 1`).
    #[tokio::test]
    async fn scoped_search_is_not_crowded_out_by_hidden_pages() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let wiki = setup(home);
        std::fs::create_dir_all(wiki.join("auto/sop")).unwrap();
        let page = |title: &str, trust: &str, session: &str, body: &str| {
            duduclaw_gateway::wiki_host_sources::set_host_fields(
                &format!(
                    "---\ntitle: \"{title}\"\nauthor: \"auto-distill\"\nlayer: context\ntrust: {trust}\n---\n\n{body}\n"
                ),
                &[format!("conversation:{session}:m:1")],
                false,
            )
        };
        for i in 0..2_050 {
            std::fs::write(
                wiki.join(format!("auto/sop/hidden-{i}.md")),
                page(&format!("hidden {i}"), "0.900", "telegram:B", "gannetcliff ledger hidden"),
            )
            .unwrap();
        }
        // Only `own.md` is recorded by the gateway's registry (as the auto
        // writer would); the hidden pages are B's and unrecorded alike.
        let own = page("own", "0.300", "telegram:A", "gannetcliff ledger own");
        std::fs::write(wiki.join("auto/sop/own.md"), &own).unwrap();
        duduclaw_memory::wiki_auto_registry::record_page(
            home,
            "alice",
            "auto/sop/own.md",
            &own,
            &["conversation:telegram:A:m:1".to_string()],
            false,
        )
        .unwrap();

        let in_a = ReadScope::Conversation(Some("telegram:A".into()));
        let args = serde_json::json!({ "query": "gannetcliff ledger", "limit": 1 });
        let found = text(&handle_wiki_search_in(&args, home, "alice", &in_a).await);
        assert!(found.starts_with("Found 1 matching pages"), "{found}");
        assert!(found.contains("auto/sop/own.md") && !found.contains("hidden-"), "{found}");
        // Employee-wide the hidden pages rank first (they really outrank it).
        let employee = text(&handle_wiki_search_in(&args, home, "alice", &ReadScope::Employee).await);
        assert!(employee.contains("hidden-") && !employee.contains("own.md"), "{employee}");
    }
}

// ---------------------------------------------------------------------------
// Shared Wiki handlers
// ---------------------------------------------------------------------------
