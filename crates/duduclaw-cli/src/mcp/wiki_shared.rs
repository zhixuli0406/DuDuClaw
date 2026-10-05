use super::*;

/// Resolve the shared wiki directory.
pub(crate) fn resolve_shared_wiki_dir(home_dir: &Path) -> std::path::PathBuf {
    home_dir.join("shared").join("wiki")
}

/// Ensure the shared wiki scaffold exists.
pub(crate) fn ensure_shared_wiki_dir(wiki_dir: &Path) -> std::result::Result<(), String> {
    let subdirs = ["entities", "concepts", "sources", "synthesis"];
    for sub in &subdirs {
        let p = wiki_dir.join(sub);
        std::fs::create_dir_all(&p).map_err(|e| format!("create dir {}: {e}", p.display()))?;
    }
    // Scaffold reserved files
    let scaffold: &[(&str, &str)] = &[
        (
            "_schema.md",
            "# Shared Wiki Schema\n\nThis is the shared knowledge base accessible to all agents.\n\n## Subdirectories\n- `entities/` — people, products, organizations\n- `concepts/` — procedures, policies, domain knowledge\n- `sources/` — shared pages from agent wikis\n- `synthesis/` — cross-agent analysis and summaries\n",
        ),
        (
            "_index.md",
            "# Shared Wiki Index\n\n<!-- Auto-maintained. One entry per page. -->\n",
        ),
        (
            "_log.md",
            "# Shared Wiki Log\n\n<!-- Append-only operation log with author attribution. -->\n",
        ),
    ];
    for (name, content) in scaffold {
        let path = wiki_dir.join(name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => {
                use std::io::Write;
                f.write_all(content.as_bytes())
                    .map_err(|e| format!("write {name}: {e}"))?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("create {name}: {e}")),
        }
    }
    Ok(())
}

/// Detect sensitive patterns in content (secret scanner for wiki writes).
pub(crate) fn contains_sensitive_pattern(content: &str) -> Option<&'static str> {
    let patterns: &[(&str, &str)] = &[
        ("sk-ant-", "Anthropic API key"),
        ("sk-proj-", "OpenAI API key"),
        ("api_key=", "API key assignment"),
        ("password=", "password assignment"),
        ("PRIVATE KEY", "private key"),
        ("ghp_", "GitHub personal access token"),
        ("gho_", "GitHub OAuth token"),
        ("xoxb-", "Slack bot token"),
        ("xoxp-", "Slack user token"),
    ];
    for (pattern, label) in patterns {
        if content.contains(pattern) {
            return Some(label);
        }
    }
    None
}

/// WP7: resolve an agent's department from its agent.toml. Returns `None` when
/// the agent has no department, an invalid one, or the config can't be read
/// (fail-safe: a caller with no resolvable department sees only the company
/// layer, never another team's pages).
pub(crate) fn resolve_agent_department(home_dir: &Path, agent_id: &str) -> Option<String> {
    if !is_valid_agent_id(agent_id) {
        return None;
    }
    // W3-3b (a): every call site passes the CALLER (`caller_agent`), which may
    // be an `eph-*` role member under `agents/.ephemeral/<id>/`.
    let toml_path = agent_dir_for_id(home_dir, agent_id).join("agent.toml");
    let content = std::fs::read_to_string(&toml_path).ok()?;
    // Parse only the `[agent].department` field — robust to any other missing
    // config fields, and cheap. An invalid / traversal-shaped value is dropped
    // (treated as "no department"), so a path is never built from it.
    let value: toml::Value = content.parse().ok()?;
    let dept = value.get("agent")?.get("department")?.as_str()?.trim();
    if dept.is_empty() || !duduclaw_core::is_valid_department(dept) {
        return None;
    }
    Some(dept.to_string())
}

/// Single shared-wiki page-enumeration predicate honouring department read
/// isolation (F4/F5). Every read-side tool (`ls`, `stats`, `lint`) enumerates
/// through this so other departments' pages never leak into a listing, a
/// contributor count, or a lint report. Paths are returned wiki-relative and
/// `/`-normalized: page paths are platform-independent identifiers (the same
/// form `page_path` takes on input), so Windows must not surface `\` here —
/// `Path::join` accepts `/` on every platform, so joining back is safe.
pub(crate) fn collect_visible_shared_pages(
    home_dir: &Path,
    wiki_dir: &Path,
    caller_agent: &str,
) -> Vec<String> {
    let visibility = DeptVisibility::for_agent(home_dir, caller_agent);
    collect_md_files(wiki_dir, wiki_dir)
        .into_iter()
        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
        .filter(|rel| visibility.allows(rel))
        .collect()
}

pub(crate) async fn handle_shared_wiki_ls(home_dir: &Path, caller_agent: &str) -> Value {
    let wiki_dir = resolve_shared_wiki_dir(home_dir);
    if !wiki_dir.exists() {
        return tool_text("No shared wiki found. Use wiki_write with scope=\"shared\" to create the first page.");
    }

    // WP7 / F4: hide other departments' pages (read isolation always on).
    let pages = collect_visible_shared_pages(home_dir, &wiki_dir, caller_agent);
    if pages.is_empty() {
        return tool_text("Shared wiki directory exists but contains no pages visible to you.");
    }

    let mut lines = Vec::with_capacity(pages.len() + 1);
    lines.push(format!("Shared wiki ({} pages):\n", pages.len()));

    for rel_path in &pages {
        let full_path = wiki_dir.join(rel_path);
        let content = std::fs::read_to_string(&full_path).unwrap_or_default();
        let title = extract_frontmatter_title(&content).unwrap_or_else(|| rel_path.clone());
        let updated = extract_frontmatter_updated(&content).unwrap_or_else(|| "?".to_string());
        let author =
            extract_frontmatter_field(&content, "author").unwrap_or_else(|| "unknown".to_string());
        lines.push(format!(
            "  {rel_path} — {title} (by: {author}, updated: {updated})"
        ));
    }

    tool_text(&lines.join("\n"))
}

pub(crate) async fn handle_shared_wiki_read(args: &Value, home_dir: &Path, caller_agent: &str) -> Value {
    let page_path = match args.get("page_path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return tool_error("Missing required parameter: page_path"),
    };

    if page_path.contains("..") || page_path.starts_with('/') || page_path.starts_with('\\') {
        return tool_error("Path traversal is not allowed");
    }

    // WP7 / F4: an agent may only read its own department's pages (company
    // layer is open to all). Fail-closed for other departments / no-department
    // callers — always on, independent of the `.scope.toml` write policy.
    if !DeptVisibility::for_agent(home_dir, caller_agent).allows(page_path) {
        return tool_error(&format!(
            "Shared wiki read denied: '{page_path}' is restricted to another department \
             (department page or a namespace with visible_to_departments)."
        ));
    }

    let wiki_dir = resolve_shared_wiki_dir(home_dir);
    let full_path = wiki_dir.join(page_path);

    // Symlink protection
    if full_path.exists()
        && let (Ok(canon_wiki), Ok(canon_page)) =
            (wiki_dir.canonicalize(), full_path.canonicalize())
        && !canon_page.starts_with(&canon_wiki)
    {
        return tool_error("Path escapes shared wiki directory");
    }

    match std::fs::read_to_string(&full_path) {
        Ok(content) => tool_text(&content),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tool_error(&format!("Page not found: {}", page_path))
        }
        Err(e) => tool_error(&format!("Failed to read page: {e}")),
    }
}

pub(crate) async fn handle_shared_wiki_write(args: &Value, home_dir: &Path, caller_agent: &str) -> Value {
    let page_path = match args.get("page_path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return tool_error("Missing required parameter: page_path"),
    };
    let content = match args.get("content").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return tool_error("Missing required parameter: content"),
    };

    if let Err(e) = validate_wiki_page_path(page_path) {
        return tool_error(&e);
    }

    // RFC-21 §3: shared-wiki SoT namespace policy. Loaded fresh on every
    // call (≤ a few KB on disk) so operator edits to .scope.toml take
    // effect immediately. Absent / malformed file ⇒ empty policy ⇒ all
    // namespaces writable (no regression vs. v1.10.1).
    let scope_policy = crate::wiki_scope::WikiScopePolicy::load_for(home_dir);

    // WP7: department isolation. An agent may only write to its own
    // department's `departments/<dept>/…` sub-tree; the company layer stays
    // governed by `.scope.toml` below. Deferred when the operator explicitly
    // declared the `departments` namespace (explicit policy wins).
    if !scope_policy.has_explicit_namespace(duduclaw_core::DEPARTMENTS_NAMESPACE) {
        let dept = resolve_agent_department(home_dir, caller_agent);
        if let Err(deny) = crate::wiki_scope::check_department_access(page_path, dept.as_deref()) {
            return tool_error(&format!("Shared wiki write denied: {deny}"));
        }
    }

    let caller_capability = crate::wiki_scope::WriterCapability::for_agent(caller_agent);
    if let Err(deny) = scope_policy.check_write(page_path, &caller_capability) {
        return tool_error(&format!("Shared wiki write denied: {deny}"));
    }

    if content.len() > WIKI_MAX_PAGE_SIZE {
        return tool_error(&format!(
            "Content too large: {} bytes (max {})",
            content.len(),
            WIKI_MAX_PAGE_SIZE
        ));
    }

    // Secret scanner
    if let Some(label) = contains_sensitive_pattern(content) {
        return tool_error(&format!(
            "Content contains sensitive data ({label}). Remove it before writing to shared wiki."
        ));
    }

    // Karpathy-schema frontmatter guard (shared wiki is strict — violations reject).
    if let Err(e) = validate_wiki_frontmatter(content) {
        return tool_error(&format!("Shared wiki schema check failed: {e}"));
    }

    // Fallback-content guard: shared wiki refuses pages authored from stale
    // LLM priors (e.g. web_search failure). Callers that must preserve such a
    // record can write it to their own agent wiki with a low trust score, but
    // the shared wiki stays clean per design: "有 fallback 的資料不應該混入共
    // 用 wiki 中產生雜訊".
    let body = extract_frontmatter_body(content);
    if let Some(marker) = detect_fallback_content(&body) {
        // Allow explicit opt-in via `fallback-mode` tag so a human can
        // deliberately archive a fallback record (e.g. for post-mortem).
        let tags = extract_frontmatter_field(content, "tags").unwrap_or_default();
        let opt_in = tags.to_lowercase().contains("fallback-mode");
        if !opt_in {
            return tool_error(&format!(
                "Fallback content detected (marker: '{marker}'). Refusing to \
                 write to shared wiki. If this record is intentional, add the \
                 `fallback-mode` tag to frontmatter and set `trust: 0.2` or \
                 lower. Otherwise, re-run the source fetch before writing."
            ));
        }
    }

    let wiki_dir = resolve_shared_wiki_dir(home_dir);
    if let Err(e) = ensure_shared_wiki_dir(&wiki_dir) {
        return tool_error(&e);
    }

    // P2-B H-4: record where this write came from (host env only).
    let stamped = match crate::mcp_memory_handlers::host_wiki_stamp(
        content,
        std::fs::read_to_string(wiki_dir.join(page_path)).ok().as_deref(),
    ) {
        Ok(s) => s,
        Err(e) => return tool_error(&e),
    };
    let content = stamped.as_str();
    let store = duduclaw_memory::WikiStore::new_shared(home_dir);
    let write_result = store.write_page_with_author(page_path, content, caller_agent);

    // RFC-22 Decision 4-D (Phase 3 W2): record an authorship audit alongside
    // the standard tool_call entry so post-hoc analysis can detect cases
    // where a single caller wrote a multi-agent page.  The 5/5 trace had
    // agnes write a "## DuDuClaw PM 觀點" section after pm spawn failed —
    // wiki content claims pm authored part of it but the only caller was
    // agnes.  Surfacing this as `matches_caller=false` lets the dashboard
    // (or future reviewers) flag the page even when the LLM ignores
    // the CONTRACT.toml `must_not` rule.
    let claimed_authors = detect_claimed_authors_in_wiki(content);
    let matches_caller =
        claimed_authors.is_empty() || claimed_authors.iter().any(|a| a == caller_agent);
    duduclaw_security::audit::append_tool_call_with_extras(
        home_dir,
        caller_agent,
        "wiki_write",
        &format!("path={page_path} size={}", content.len()),
        write_result.is_ok(),
        &[
            ("scope", "shared".into()),
            (
                "claimed_authors_in_content",
                serde_json::Value::Array(
                    claimed_authors.iter().map(|a| a.clone().into()).collect(),
                ),
            ),
            ("matches_caller", matches_caller.into()),
            ("actual_caller", caller_agent.into()),
        ],
    );

    match write_result {
        Ok(()) => {
            post_wiki_activity(
                home_dir,
                caller_agent,
                format!("寫入共享知識庫「{page_path}」"),
                page_path,
            )
            .await;
            tool_text(&format!(
                "Written shared wiki page: {} (by: {})",
                page_path, caller_agent
            ))
        }
        Err(e) => tool_error(&format!("Failed to write shared wiki page: {e}")),
    }
}

// ── Live Canvas tools (G15) ──────────────────────────────────────
//
// The canvas is an XSS-adjacent surface: agent-authored HTML rendered in the
// operator's dashboard. Both tools write through
// `duduclaw_gateway::canvas::CanvasStore`, whose `push` sanitizes with the
// ammonia canvas profile at WRITE time (fail-closed — a sanitizer rejection
// stores nothing). The dashboard additionally renders inside
// `<iframe sandbox="">`. Live `canvas.updated` WS events are emitted by the
// gateway's canvas broadcast bridge (it polls canvas.db), so no bus append is
// needed here. Both tools are in the `is_state_changing` audit list.

/// Push a sanitized HTML canvas for the calling agent.
pub(crate) async fn handle_canvas_push(args: &Value, home_dir: &Path, caller_agent: &str) -> Value {
    // SEC: caller identity is the storage key — validate before any I/O.
    if !is_valid_agent_id(caller_agent) {
        return tool_error("Invalid agent ID");
    }
    let html = match args.get("html").and_then(|v| v.as_str()) {
        Some(h) => h,
        None => return tool_error("Missing required parameter: html"),
    };
    let title = args.get("title").and_then(|v| v.as_str()).unwrap_or("");
    let store = match duduclaw_gateway::canvas::CanvasStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return tool_error(&format!("Canvas store unavailable: {e}")),
    };
    match store.push(caller_agent, title, html).await {
        Ok(row) => tool_text(&format!(
            "Canvas updated (version {}, {} bytes after sanitization). The user can view it on the dashboard Canvas page.",
            row.seq,
            row.html.len()
        )),
        // Fail-closed: oversize / empty-after-sanitization pushes are
        // rejected with the sanitizer's reason so the agent can fix and retry.
        Err(e) => tool_error(&format!("Canvas push rejected: {e}")),
    }
}

/// Clear the calling agent's canvas (appends an empty tombstone version).
pub(crate) async fn handle_canvas_clear(home_dir: &Path, caller_agent: &str) -> Value {
    if !is_valid_agent_id(caller_agent) {
        return tool_error("Invalid agent ID");
    }
    let store = match duduclaw_gateway::canvas::CanvasStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return tool_error(&format!("Canvas store unavailable: {e}")),
    };
    match store.clear(caller_agent).await {
        Ok(_) => tool_text(
            "Canvas cleared. The dashboard now shows the empty state; previous versions remain in history.",
        ),
        Err(e) => tool_error(&format!("Canvas clear failed: {e}")),
    }
}

/// Detect agent names claimed as authors within a markdown wiki page.
///
/// RFC-22 Decision 4-D (Phase 3 W2): callers (typically agnes) sometimes
/// produce wiki pages structured as multi-agent meeting notes, with sections
/// like `## DuDuClaw PM 的觀點` followed by content that claims to be
/// authored by `duduclaw-pm`.  We extract those claimed names so the audit
/// trail can flag the page when the actual MCP caller does NOT match.
///
/// Patterns recognized (case-sensitive on the agent token; any of these
/// signal a claimed author):
///
/// - Markdown heading: `## <agent> 的觀點` / `## <agent> 觀點`
/// - Bold reply attribution: `**回覆人**：<agent>` / `**Author**: <agent>`
/// - Trailing signature: `*<agent> | <date>*` (loose match — last segment
///   before the pipe is treated as the agent name).
/// - Frontmatter `claimed_authors: [a, b]` (explicit declaration).
///
/// Names are filtered to look like duduclaw agent ids (lowercase
/// alphanumeric + hyphens, length 2..=64). Returns deduplicated list.
pub(crate) fn detect_claimed_authors_in_wiki(content: &str) -> Vec<String> {
    use std::collections::BTreeSet;
    let mut found = BTreeSet::new();

    // Frontmatter explicit declaration.
    if let Some(claimed) = extract_frontmatter_field(content, "claimed_authors") {
        // Tolerate `[a, b]` or `a,b` styles.
        for raw in claimed
            .trim_start_matches('[')
            .trim_end_matches(']')
            .split(',')
        {
            let name = raw.trim().trim_matches('"').trim_matches('\'').to_string();
            if is_agent_id_shape(&name) {
                found.insert(name);
            }
        }
    }

    // Heading: ## <agent> 的觀點  /  ## <agent> 觀點
    // We don't use the `regex` crate here to keep dependencies lean for the
    // mcp.rs module — substring scanning is sufficient given the bounded
    // input size (already capped by WIKI_MAX_PAGE_SIZE).
    for line in content.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("## ") {
            // Try patterns "<name> 的觀點", "<name> 觀點"
            for suffix in [" 的觀點", " 觀點", " 的觀點：", " 觀點："] {
                if let Some(name_part) = rest.strip_suffix(suffix) {
                    let name = name_part.trim().to_string();
                    if is_agent_id_shape(&name) {
                        found.insert(name);
                    }
                }
            }
        }
    }

    // Bold attribution: **回覆人**：<agent>  or  **Author**: <agent>
    for marker in ["**回覆人**：", "**回覆人**:", "**Author**:", "**author**:"] {
        for chunk in content.split(marker).skip(1) {
            // Take everything until newline / end / next markdown control
            let candidate: String = chunk
                .chars()
                .take_while(|c| !matches!(c, '\n' | '<' | '|' | '\r'))
                .collect();
            let name = candidate.trim().to_string();
            if is_agent_id_shape(&name) {
                found.insert(name);
            }
        }
    }

    // Trailing signature: *<agent> | <date>*
    for line in content.lines() {
        let t = line.trim();
        if let Some(inner) = t.strip_prefix('*').and_then(|s| s.strip_suffix('*')) {
            if let Some((name_raw, _)) = inner.split_once('|') {
                let name = name_raw.trim().to_string();
                if is_agent_id_shape(&name) {
                    found.insert(name);
                }
            }
        }
    }

    found.into_iter().collect()
}

/// True when the string looks like a duduclaw agent id (per
/// `is_valid_agent_id`-equivalent shape: lowercase alphanumeric + `-`,
/// 2..=64 chars).  Used as a filter before treating a markdown token as
/// a claimed author.
///
/// WP-4I (2026-08) confirmed this is intentionally NOT unified with
/// `duduclaw_core::is_valid_agent_id` / `is_valid_new_agent_id`: this is a
/// markdown-authorship heuristic (also requires at least one alphabetic char,
/// which neither core validator does), not a security gate. Do not use it to
/// authorize or route based on agent identity — use the core validators for
/// that.
pub(crate) fn is_agent_id_shape(s: &str) -> bool {
    let len = s.len();
    if !(2..=64).contains(&len) {
        return false;
    }
    s.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && s.chars().any(|c| c.is_ascii_alphabetic())
}
