use super::*;

pub(crate) async fn handle_shared_wiki_search(args: &Value, home_dir: &Path, caller_agent: &str) -> Value {
    let query = match args.get("query").and_then(|v| v.as_str()) {
        Some(q) if !q.is_empty() => q,
        _ => return tool_error("Missing required parameter: query"),
    };
    let limit: usize = args
        .get("limit")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or(10)
        .min(100);
    let min_trust: Option<f32> = args
        .get("min_trust")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok());
    let layer_filter: Option<duduclaw_memory::WikiLayer> = args
        .get("layer")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok());

    let wiki_dir = resolve_shared_wiki_dir(home_dir);
    if !wiki_dir.exists() {
        return tool_text("No shared wiki found. Use wiki_write with scope=\"shared\" to create the first page.");
    }

    let store = duduclaw_memory::WikiStore::new_shared(home_dir);
    let hits = match store.search_filtered(query, limit, min_trust, layer_filter, false) {
        Ok(h) => h,
        Err(e) => return tool_error(&format!("Shared wiki search failed: {e}")),
    };

    // WP7 / F4: drop hits from other departments before rendering.
    let visibility = DeptVisibility::for_agent(home_dir, caller_agent);
    let hits: Vec<_> = hits
        .into_iter()
        .filter(|h| visibility.allows(&h.path.replace('\\', "/")))
        .collect();

    if hits.is_empty() {
        return tool_text(&format!("No shared wiki pages match '{}'.", query));
    }

    let mut output = format!(
        "Found {} shared wiki results for '{}':\n\n",
        hits.len(),
        query
    );
    for h in &hits {
        output.push_str(&format!(
            "📄 {} — {} (trust: {:.1} | layer: {} | relevance: {})\n",
            h.path, h.title, h.trust, h.layer, h.score
        ));
        for line in &h.context_lines {
            output.push_str(&format!("  {}\n", line));
        }
        output.push('\n');
    }

    tool_text(&output)
}

pub(crate) async fn handle_shared_wiki_delete(args: &Value, home_dir: &Path, caller_agent: &str) -> Value {
    let page_path = match args.get("page_path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return tool_error("Missing required parameter: page_path"),
    };

    if let Err(e) = validate_wiki_page_path(page_path) {
        return tool_error(&e);
    }

    // RFC-21 §3: deletes on read_only / operator_only namespaces are denied
    // even for the original page author — the namespace policy is the
    // authority, not the per-page ACL.
    let scope_policy = crate::wiki_scope::WikiScopePolicy::load_for(home_dir);

    // WP7: an agent may only delete within its own department's sub-tree.
    if !scope_policy.has_explicit_namespace(duduclaw_core::DEPARTMENTS_NAMESPACE) {
        let dept = resolve_agent_department(home_dir, caller_agent);
        if let Err(deny) = crate::wiki_scope::check_department_access(page_path, dept.as_deref()) {
            return tool_error(&format!("Shared wiki delete denied: {deny}"));
        }
    }

    let caller_capability = crate::wiki_scope::WriterCapability::for_agent(caller_agent);
    if let Err(deny) = scope_policy.check_write(page_path, &caller_capability) {
        return tool_error(&format!("Shared wiki delete denied: {deny}"));
    }

    let wiki_dir = resolve_shared_wiki_dir(home_dir);
    let full_path = wiki_dir.join(page_path);

    if !full_path.exists() {
        return tool_error(&format!("Page not found: {}", page_path));
    }

    // ACL: only author or main agent can delete
    let content = std::fs::read_to_string(&full_path).unwrap_or_default();
    let page_author = extract_frontmatter_field(&content, "author").unwrap_or_default();

    // Check if caller is the main agent. Must parse the typed [agent] role —
    // an unanchored substring scan over the whole file would let any agent
    // whose SOUL/comments contain the literal `role = "main"` spoof main-agent
    // deletion rights (coding convention 2: no unanchored contains for authz).
    let is_main = duduclaw_core::agent_toml::load_for_agent(home_dir, caller_agent)
        .agent
        .and_then(|a| a.role)
        .as_deref()
        == Some("main");

    if page_author != caller_agent && !is_main {
        return tool_error(&format!(
            "Permission denied: page was authored by '{}'. Only the author or a main agent can delete shared wiki pages.",
            page_author
        ));
    }

    let store = duduclaw_memory::WikiStore::new_shared(home_dir);
    match store.delete_page(page_path) {
        Ok(()) => tool_text(&format!(
            "Deleted shared wiki page: {} (by: {})",
            page_path, caller_agent
        )),
        Err(e) => tool_error(&format!("Failed to delete: {e}")),
    }
}

pub(crate) async fn handle_shared_wiki_stats(home_dir: &Path, caller_agent: &str) -> Value {
    let wiki_dir = resolve_shared_wiki_dir(home_dir);
    if !wiki_dir.exists() {
        return tool_text("No shared wiki found.");
    }

    // F5: only enumerate pages the caller may read — other departments' path
    // names and author counts must not leak through stats.
    let pages = collect_visible_shared_pages(home_dir, &wiki_dir, caller_agent);
    if pages.is_empty() {
        return tool_text("Shared wiki exists but has no pages visible to you.");
    }

    let mut author_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut dir_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut latest_updated = String::new();

    for rel_path in &pages {
        let full_path = wiki_dir.join(rel_path);
        let content = std::fs::read_to_string(&full_path).unwrap_or_default();

        let author =
            extract_frontmatter_field(&content, "author").unwrap_or_else(|| "unknown".to_string());
        *author_counts.entry(author).or_default() += 1;

        let dir = match rel_path.rsplit_once('/') {
            Some((parent, _)) if !parent.is_empty() => parent.to_string(),
            _ => "root".to_string(),
        };
        *dir_counts.entry(dir).or_default() += 1;

        let updated = extract_frontmatter_updated(&content).unwrap_or_default();
        if updated > latest_updated {
            latest_updated = updated;
        }
    }

    let mut output = format!(
        "Shared Wiki Stats\n\nTotal pages: {}\nLast updated: {}\n\n",
        pages.len(),
        latest_updated
    );

    output.push_str("Contributors:\n");
    let mut authors: Vec<_> = author_counts.into_iter().collect();
    authors.sort_by(|a, b| b.1.cmp(&a.1));
    for (author, count) in &authors {
        output.push_str(&format!("  {} — {} pages\n", author, count));
    }

    output.push_str("\nBy directory:\n");
    let mut dirs: Vec<_> = dir_counts.into_iter().collect();
    dirs.sort_by(|a, b| b.1.cmp(&a.1));
    for (dir, count) in &dirs {
        output.push_str(&format!("  {} — {} pages\n", dir, count));
    }

    tool_text(&output)
}

/// RFC-21 §3: Inspect the shared-wiki namespace policy (`.scope.toml`).
/// Returns the configured namespaces and their modes plus a hint about the
/// fallback ("agent_writable") behaviour for namespaces not listed.
pub(crate) async fn handle_wiki_namespace_status(home_dir: &Path, caller_agent: &str) -> Value {
    let policy = crate::wiki_scope::WikiScopePolicy::load_for(home_dir);
    let snapshot = policy.snapshot();

    // WP7: surface the caller's department + the built-in department isolation
    // rule so the dashboard can render "which departments can I see".
    let caller_department = resolve_agent_department(home_dir, caller_agent);
    let departments_explicit = policy.has_explicit_namespace(duduclaw_core::DEPARTMENTS_NAMESPACE);

    // WP2.3: namespaces declaring `visible_to_departments = [...]` (read-side
    // visibility, orthogonal to the write mode above). Surface them so the
    // dashboard/agent can see which namespaces are department-restricted.
    let vis_policy = duduclaw_core::DepartmentVisibilityPolicy::load_for_home(home_dir);
    let visible_to_departments: serde_json::Value = vis_policy
        .snapshot()
        .iter()
        .map(|(ns, depts)| (ns.clone(), serde_json::json!(depts)))
        .collect::<serde_json::Map<_, _>>()
        .into();

    let payload = serde_json::json!({
        "policy_file": policy.loaded_from()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| crate::wiki_scope::scope_file_path(home_dir).display().to_string()),
        "policy_loaded": policy.loaded_from().is_some(),
        "default_mode": "agent_writable",
        "namespaces": snapshot,
        "department": {
            "caller_agent": caller_agent,
            "caller_department": caller_department,
            "namespace": duduclaw_core::DEPARTMENTS_NAMESPACE,
            // F4: department READ isolation is ALWAYS enforced — an agent can
            // only read `departments/<own-dept>/…` plus the open company layer,
            // regardless of `.scope.toml`. Report it honestly so this snapshot
            // never contradicts the actual enforcement.
            "read_isolation_enforced": true,
            // The `.scope.toml` flag only affects the WRITE policy for the
            // `departments` namespace (who may write there); it does NOT relax
            // read isolation. When true, the operator's write policy governs
            // writes to `departments/…`; when false, the built-in rule limits
            // writes to `departments/<own-dept>/…`.
            "write_policy_from_scope_toml": departments_explicit,
        },
        // WP2.3 read-visibility filter: namespace → departments allowed to see
        // it (prompt injection + shared wiki_search/read). Fail-closed for any
        // declared namespace; unlisted namespaces stay visible to all.
        "visible_to_departments": visible_to_departments,
    });

    let pretty = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string());

    let header = if policy.is_empty() {
        "Shared wiki namespace policy: none configured (every namespace is agent_writable).\n\n"
    } else {
        "Shared wiki namespace policy:\n\n"
    };
    tool_text(&format!("{header}{pretty}"))
}

/// Audit shared wiki for Karpathy-schema compliance: missing frontmatter
/// fields, fallback-content markers, orphan pages, broken links, stale pages.
pub(crate) async fn handle_shared_wiki_lint(home_dir: &Path, caller_agent: &str) -> Value {
    let wiki_dir = resolve_shared_wiki_dir(home_dir);
    if !wiki_dir.exists() {
        return tool_text("No shared wiki found.");
    }

    // F5: only lint pages the caller may read — other departments' page paths
    // must not surface in the report.
    let pages = collect_visible_shared_pages(home_dir, &wiki_dir, caller_agent);
    if pages.is_empty() {
        return tool_text("Shared wiki exists but has no pages visible to you.");
    }

    // Schema compliance + fallback scan
    let mut schema_violations: Vec<(String, String)> = Vec::new();
    let mut fallback_pages: Vec<(String, &'static str)> = Vec::new();
    for rel_path in &pages {
        let full_path = wiki_dir.join(rel_path);
        let content = std::fs::read_to_string(&full_path).unwrap_or_default();
        let rel_str = rel_path.clone();

        if let Err(e) = validate_wiki_frontmatter(&content) {
            schema_violations.push((rel_str.clone(), e));
        }

        let body = extract_frontmatter_body(&content);
        if let Some(marker) = detect_fallback_content(&body) {
            let tags = extract_frontmatter_field(&content, "tags").unwrap_or_default();
            if !tags.to_lowercase().contains("fallback-mode") {
                fallback_pages.push((rel_str, marker));
            }
        }
    }

    // Delegate graph-level checks to WikiStore::lint. F5: the graph lint scans
    // the whole store, so filter its path-bearing results through the same
    // department read-isolation predicate — other departments' orphan/broken/
    // stale pages must not surface in this caller's report.
    let store = duduclaw_memory::WikiStore::new_shared(home_dir);
    let graph = store.lint().ok().map(|mut r| {
        let vis = DeptVisibility::for_agent(home_dir, caller_agent);
        let visible = |p: &str| vis.allows(&p.replace('\\', "/"));
        r.orphan_pages.retain(|p| visible(p));
        r.stale_pages.retain(|p| visible(p));
        r.broken_links.retain(|(from, _to)| visible(from));
        r
    });

    let mut output = format!("Shared Wiki Lint Report\n\nTotal pages: {}\n", pages.len());
    if let Some(ref r) = graph {
        output.push_str(&format!("Index entries: {}\n", r.index_entries));
    }
    output.push('\n');

    let clean = schema_violations.is_empty()
        && fallback_pages.is_empty()
        && graph.as_ref().is_none_or(|r| {
            r.orphan_pages.is_empty() && r.broken_links.is_empty() && r.stale_pages.is_empty()
        });

    if clean {
        output.push_str("All clear — shared wiki is Karpathy-schema compliant.\n");
        return tool_text(&output);
    }

    if !schema_violations.is_empty() {
        output.push_str(&format!(
            "Schema violations ({}):\n",
            schema_violations.len()
        ));
        for (path, err) in &schema_violations {
            output.push_str(&format!("  - {}: {}\n", path, err));
        }
        output.push('\n');
    }

    if !fallback_pages.is_empty() {
        output.push_str(&format!(
            "Fallback-content pages ({}) — likely authored without live evidence:\n",
            fallback_pages.len()
        ));
        for (path, marker) in &fallback_pages {
            output.push_str(&format!("  - {} (marker: '{}')\n", path, marker));
        }
        output.push_str("  → Remove, re-run source fetch, or add `fallback-mode` tag + `trust: 0.2` to opt in.\n\n");
    }

    if let Some(r) = graph {
        if !r.orphan_pages.is_empty() {
            output.push_str(&format!(
                "Orphan pages ({}) — not in _index.md:\n",
                r.orphan_pages.len()
            ));
            for p in &r.orphan_pages {
                output.push_str(&format!("  - {}\n", p));
            }
            output.push('\n');
        }
        if !r.broken_links.is_empty() {
            output.push_str(&format!("Broken links ({}):\n", r.broken_links.len()));
            for (from, to) in &r.broken_links {
                output.push_str(&format!("  - {} -> {} (not found)\n", from, to));
            }
            output.push('\n');
        }
        if !r.stale_pages.is_empty() {
            output.push_str(&format!(
                "Stale pages (>30 days) ({}):\n",
                r.stale_pages.len()
            ));
            for p in &r.stale_pages {
                output.push_str(&format!("  - {}\n", p));
            }
        }
    }

    tool_text(&output)
}

pub(crate) async fn handle_wiki_share(args: &Value, home_dir: &Path, caller_agent: &str) -> Value {
    let page_path = match args.get("page_path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return tool_error("Missing required parameter: page_path"),
    };
    let custom_summary = args.get("summary").and_then(|v| v.as_str());

    // Read source page from caller's wiki
    let wiki_dir = match resolve_wiki_dir(home_dir, caller_agent) {
        Ok(d) => d,
        Err(e) => return tool_error(&e),
    };

    let full_path = wiki_dir.join(page_path);
    let source_content = match std::fs::read_to_string(&full_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return tool_error(&format!("Page not found in your wiki: {}", page_path));
        }
        Err(e) => return tool_error(&format!("Failed to read source page: {e}")),
    };

    let source_title =
        extract_frontmatter_title(&source_content).unwrap_or_else(|| page_path.to_string());
    let source_body = extract_frontmatter_body(&source_content);

    // Generate summary
    let summary = match custom_summary {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => {
            let chars: String = source_body.chars().take(500).collect();
            if source_body.chars().count() > 500 {
                format!("{}...", chars)
            } else {
                chars
            }
        }
    };

    // Secret scanner on summary
    if let Some(label) = contains_sensitive_pattern(&summary) {
        return tool_error(&format!(
            "Summary contains sensitive data ({label}). Redact before sharing."
        ));
    }

    // Build shared page name: sources/{caller}--{page_stem}.md
    let page_stem = std::path::Path::new(page_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("page");
    let shared_page_path = format!("sources/{}--{}.md", caller_agent, page_stem);

    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let shared_content = format!(
        "---\ntitle: \"{}\"\nauthor: \"{}\"\nsource_agent: \"{}\"\nsource_page: \"{}\"\nshared_at: \"{}\"\nupdated: \"{}\"\ntags: [shared, from-{}]\n---\n\n{}\n\n---\n*Shared from {}'s wiki (`{}`)*\n",
        source_title,
        caller_agent,
        caller_agent,
        page_path,
        now,
        now,
        caller_agent,
        summary,
        caller_agent,
        page_path,
    );

    // Write to shared wiki
    let shared_wiki_dir = resolve_shared_wiki_dir(home_dir);
    if let Err(e) = ensure_shared_wiki_dir(&shared_wiki_dir) {
        return tool_error(&e);
    }

    let store = duduclaw_memory::WikiStore::new_shared(home_dir);
    match store.write_page_with_author(&shared_page_path, &shared_content, caller_agent) {
        Ok(()) => tool_text(&format!(
            "Shared '{}' to shared wiki as '{}' (by: {})",
            page_path, shared_page_path, caller_agent
        )),
        Err(e) => tool_error(&format!("Failed to write shared page: {e}")),
    }
}
