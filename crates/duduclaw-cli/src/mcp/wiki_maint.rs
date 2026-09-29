use super::*;

pub(crate) async fn handle_wiki_dedup(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let agent_id = args
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or(default_agent);
    if !is_valid_agent_id(agent_id) {
        return tool_error("Invalid agent_id format");
    }

    let wiki_dir = match resolve_wiki_dir(home_dir, agent_id) {
        Ok(d) => d,
        Err(e) => return tool_error(&e),
    };
    if !wiki_dir.exists() {
        return tool_text("No wiki found.");
    }

    let store = duduclaw_memory::WikiStore::new(wiki_dir);
    match store.detect_duplicates() {
        Ok(candidates) if candidates.is_empty() => tool_text("No duplicate candidates found."),
        Ok(candidates) => {
            let mut output = format!("Found {} potential duplicate pairs:\n\n", candidates.len());
            for c in &candidates {
                output.push_str(&format!(
                    "- **{}** (trust: {:.1}) ↔ **{}** (trust: {:.1})\n  Reason: {}\n  Suggestion: keep the page with higher trust\n\n",
                    c.page_a, c.trust_a, c.page_b, c.trust_b, c.reason
                ));
            }
            tool_text(&output)
        }
        Err(e) => tool_error(&format!("Dedup detection failed: {e}")),
    }
}

pub(crate) async fn handle_wiki_graph(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let agent_id = args
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or(default_agent);
    if !is_valid_agent_id(agent_id) {
        return tool_error("Invalid agent_id format");
    }

    let wiki_dir = match resolve_wiki_dir(home_dir, agent_id) {
        Ok(d) => d,
        Err(e) => return tool_error(&e),
    };
    if !wiki_dir.exists() {
        return tool_text("No wiki found.");
    }

    let center = args.get("center").and_then(|v| v.as_str());
    let depth: usize = args
        .get("depth")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse().ok())
        .unwrap_or(2);

    let store = duduclaw_memory::WikiStore::new(wiki_dir);
    match store.export_mermaid(center, depth) {
        Ok(mermaid) => tool_text(&format!("```mermaid\n{mermaid}```")),
        Err(e) => tool_error(&format!("Graph export failed: {e}")),
    }
}

pub(crate) async fn handle_wiki_rebuild_fts(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let agent_id = args
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or(default_agent);
    if !is_valid_agent_id(agent_id) {
        return tool_error("Invalid agent_id format");
    }

    let wiki_dir = match resolve_wiki_dir(home_dir, agent_id) {
        Ok(d) => d,
        Err(e) => return tool_error(&e),
    };
    if !wiki_dir.exists() {
        return tool_text("No wiki found.");
    }

    let store = duduclaw_memory::WikiStore::new(wiki_dir);
    match store.rebuild_fts() {
        Ok(count) => tool_text(&format!("FTS index rebuilt: {} pages indexed.", count)),
        Err(e) => tool_error(&format!("FTS rebuild failed: {e}")),
    }
}

pub(crate) async fn handle_wiki_trust_audit(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let agent_id = args
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or(default_agent);
    if !is_valid_agent_id(agent_id) {
        return tool_error("Invalid agent_id format");
    }
    let max_trust = args
        .get("max_trust")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.3) as f32;
    let limit = (args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize).min(500);

    // Lazy init: if the global store hasn't been created (CLI invocation
    // outside the gateway), open it on demand at the standard path.
    let store = match duduclaw_memory::trust_store::global_trust_store() {
        Some(s) => s,
        None => match duduclaw_memory::trust_store::init_global_trust_store(
            home_dir.join("wiki_trust.db"),
        ) {
            Ok(s) => s,
            Err(e) => return tool_error(&format!("Trust store init failed: {e}")),
        },
    };

    match store.list_low_trust(agent_id, max_trust, limit) {
        Ok(rows) => {
            if rows.is_empty() {
                return tool_text(&format!(
                    "No pages below trust ≤ {max_trust:.2} for agent '{agent_id}'."
                ));
            }
            let mut lines = Vec::with_capacity(rows.len() + 2);
            lines.push(format!(
                "## Wiki trust audit — agent '{agent_id}' (trust ≤ {max_trust:.2})\n"
            ));
            lines.push(
                "| Page | Trust | Cite | Err | OK | DNI | Last signal |\n|---|---|---|---|---|---|---|".into(),
            );
            for s in &rows {
                lines.push(format!(
                    "| `{}` | {:.3} | {} | {} | {} | {} | {} |",
                    s.page_path,
                    s.trust,
                    s.citation_count,
                    s.error_signal_count,
                    s.success_signal_count,
                    if s.do_not_inject { "yes" } else { "no" },
                    s.last_signal_at
                        .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or_default(),
                ));
            }
            tool_text(&lines.join("\n"))
        }
        Err(e) => tool_error(&format!("trust audit failed: {e}")),
    }
}

pub(crate) async fn handle_wiki_trust_history(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let agent_id = args
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or(default_agent);
    let page_path = args.get("page_path").and_then(|v| v.as_str()).unwrap_or("");
    let limit = (args.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize).min(500);

    if !is_valid_agent_id(agent_id) {
        return tool_error("Invalid agent_id format");
    }
    if page_path.is_empty() {
        return tool_error("Missing 'page_path' parameter");
    }
    // (review MED R2 N-2) Defence in depth: bad page_path can't reach SQL
    // injection (parameterised queries) but might leak via audit log strings
    // or future filesystem ops. Reject traversal / NUL / non-md paths.
    if page_path.len() > 512
        || page_path.contains("..")
        || page_path.starts_with('/')
        || page_path.starts_with('\\')
        || page_path.contains('\0')
        || !page_path.ends_with(".md")
    {
        return tool_error("Invalid page_path format");
    }

    let store = match duduclaw_memory::trust_store::global_trust_store() {
        Some(s) => s,
        None => match duduclaw_memory::trust_store::init_global_trust_store(
            home_dir.join("wiki_trust.db"),
        ) {
            Ok(s) => s,
            Err(e) => return tool_error(&format!("Trust store init failed: {e}")),
        },
    };

    match store.history(agent_id, page_path, limit) {
        Ok(rows) => {
            if rows.is_empty() {
                return tool_text(&format!(
                    "No trust history for `{page_path}` (agent '{agent_id}')."
                ));
            }
            let mut lines = Vec::with_capacity(rows.len() + 2);
            lines.push(format!(
                "## Trust history — `{page_path}` (agent '{agent_id}')\n"
            ));
            lines.push(
                "| Time | Old → New | Δ | Trigger | Signal | Composite Err |\n|---|---|---|---|---|---|".into(),
            );
            for h in &rows {
                lines.push(format!(
                    "| {} | {:.3} → {:.3} | {:+.3} | {} | {} | {} |",
                    h.ts.format("%Y-%m-%d %H:%M:%S"),
                    h.old_trust,
                    h.new_trust,
                    h.applied_delta,
                    h.trigger,
                    h.signal_kind,
                    h.composite_error
                        .map(|e| format!("{:.2}", e))
                        .unwrap_or_default(),
                ));
            }
            tool_text(&lines.join("\n"))
        }
        Err(e) => tool_error(&format!("trust history failed: {e}")),
    }
}
