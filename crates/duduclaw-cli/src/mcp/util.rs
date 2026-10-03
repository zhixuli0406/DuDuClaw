use super::*;

/// Detect the host system's IANA timezone name (e.g. `"Asia/Taipei"`,
/// `"America/New_York"`, `"UTC"`).
///
/// Uses `iana_time_zone::get_timezone()` which reads `/etc/localtime` on
/// Unix / the registry on Windows. Validated against `chrono-tz` so we
/// never return a name the scheduler would reject — defensive because
/// `iana-time-zone` is allowed to surface historical aliases that
/// `chrono-tz`'s database has dropped.
///
/// Returns `None` when the host has no discoverable TZ (extremely rare
/// on real machines — typical in minimal Docker images with no
/// `/etc/localtime`). Callers should fall back to UTC.
///
/// Used so a cron schedule created without `cron_timezone` is evaluated in
/// the host's local zone rather than silently in UTC.
pub(crate) fn detect_local_timezone() -> Option<String> {
    let name = iana_time_zone::get_timezone().ok()?;
    // Round-trip through chrono-tz so we only ever hand back names the
    // scheduler's `duduclaw_core::parse_timezone` will accept.
    if duduclaw_core::parse_timezone(&name).is_some() {
        Some(name)
    } else {
        None
    }
}

/// Validate agent ID is safe for filesystem paths (no traversal).
///
/// WP-4I (2026-08): this hand-rolled copy had drifted from its two siblings
/// in `duduclaw-gateway::handlers` and `duduclaw-cli::lib` — it was missing
/// the leading/trailing-hyphen guard they both had (a leading hyphen risks
/// being misread as a flag by any downstream command that forwards the id as
/// a bare positional argument). Now delegates to
/// [`duduclaw_core::is_valid_new_agent_id`], the single authoritative copy;
/// this closes the drift (behavior change: an id like `-agent` or `agent-`,
/// previously accepted here, is now rejected — see the WP-4I report).
pub(crate) fn is_valid_agent_id(id: &str) -> bool {
    duduclaw_core::is_valid_new_agent_id(id)
}

/// Count existing agents (directories under `<home>/agents/` that carry an
/// `agent.toml`) — the denominator for the agent-count cap in
/// `handle_create_agent`. Mirrors what the gateway registry would list.
pub(crate) fn count_existing_agents(home_dir: &Path) -> usize {
    std::fs::read_dir(home_dir.join("agents"))
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().join("agent.toml").is_file())
                .count()
        })
        .unwrap_or(0)
}

/// WP22 T4 — collect every identifier that already resolves to an agent: each
/// existing agent's directory name AND its `[agent] name` field. The two
/// namespaces are supposed to coincide (create always sets them equal) but
/// nothing enforces that after the fact — a hand-edited `agent.toml` or a
/// renamed directory can drift them apart. A new agent whose `name` collides
/// with either would make the registry's `name → LoadedAgent` map (last-wins,
/// see `AgentRegistry::scan`) or the delegation `name → dir` resolver pick one
/// of the two silently, mis-routing delegation/channel traffic to the wrong
/// agent. One directory scan, not per-check re-reads.
pub(crate) fn collect_existing_agent_identifiers(home_dir: &Path) -> std::collections::HashSet<String> {
    let mut ids = std::collections::HashSet::new();
    let entries = match std::fs::read_dir(home_dir.join("agents")) {
        Ok(e) => e,
        Err(_) => return ids,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if let Some(dir_name) = path.file_name().and_then(|n| n.to_str()) {
            ids.insert(dir_name.to_string());
        }
        // Shared typed parse point (R2 unification): absent file / malformed
        // TOML / absent `[agent]` table / absent-or-wrong-typed `name` all
        // still contribute nothing, exactly as the raw walk did.
        if let Some(name) = duduclaw_core::agent_toml::load(&path)
            .agent
            .and_then(|a| a.name)
        {
            ids.insert(name);
        }
    }
    ids
}

/// Maximum JSONL queue file size (10 MB).
pub(crate) const MAX_QUEUE_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// Maximum allowed byte length for an `agent_id` parameter in MCP handlers.
/// Prevents excessively long inputs that could cause DoS or log-flooding.
pub(crate) const MAX_AGENT_ID_LEN: usize = 128;

/// Append a line to a JSONL file with size limit check.
///
/// **Concurrency (project convention #3, 2026-07 MED)**: the whole
/// check+append runs under `duduclaw_core::with_file_lock`. `O_APPEND` alone
/// is not enough — the gateway dispatcher REWRITES `bus_queue.jsonl`
/// (read-modify-write) under the same lock, and a bare append racing that
/// rewrite is silently lost; oversized records can also interleave.
pub(crate) fn append_to_jsonl_sync(path: &std::path::Path, line: &str) -> bool {
    use std::io::Write;
    duduclaw_core::with_file_lock(path, || {
        // Check size limit (inside the lock so it can't race the append).
        if let Ok(meta) = std::fs::metadata(path)
            && meta.len() > MAX_QUEUE_FILE_SIZE
        {
            tracing::warn!("Queue file {} exceeds size limit", path.display());
            return Ok(false);
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        writeln!(f, "{line}")?;
        Ok(true)
    })
    .unwrap_or(false)
}

// ── Namespace-aware wiki agent resolver (W19-P0 M2) ──────────────────────────
/// Resolve the effective wiki agent for this principal from the namespace context.
///
/// Rules:
/// - External clients (`write_namespace = "external/{client_id}"`): wiki
///   operations are scoped to `{client_id}`.  The dispatcher has already
///   stripped any user-supplied `agent_id` argument, so the fallback in every
///   wiki handler lands here instead of on `default_agent`.
/// - Internal clients: preserve existing behaviour — fall back to
///   `default_agent`, which is the agent configured in config.toml.
pub(crate) fn wiki_agent_from_ns<'a>(
    ns_ctx: &'a crate::mcp_namespace::NamespaceContext,
    default_agent: &'a str,
) -> &'a str {
    ns_ctx
        .write_namespace
        .strip_prefix("external/")
        .unwrap_or(default_agent)
}

/// Resolve the agent identity to stamp on execution-attribution records for
/// this MCP call — `tool_calls.jsonl` audit rows and dashboard live-feedback
/// events. NOT for delegation-policy decisions (see below).
///
/// `DUDUCLAW_DELEGATION_SENDER` (`duduclaw_core::ENV_DELEGATION_SENDER`)
/// answers a *policy* question — "who delegated this work?" — injected by
/// `dispatcher.rs`'s `DelegationEnv` (and `cron_scheduler.rs`) into every
/// spawned Claude CLI subprocess so the WP21 delegation gate can judge the
/// request. Every scheduler-driven dispatch path (goal-loop, cron, heartbeat,
/// autopilot) as well as the two human-interface paths (dashboard, webhook)
/// stamps one of the six `duduclaw_core::SYSTEM_SENDERS` ids as the sender —
/// these are schedulers / human interfaces, never agents, and never call an
/// MCP tool themselves. The *worker* agent's own subprocess — whose MCP
/// server resolves `default_agent` from `DUDUCLAW_AGENT_ID` — is what
/// actually dials `tools/call`.
///
/// Before this fix, execution attribution stamped the sender verbatim, so
/// every tool call a goal-loop worker made was recorded under
/// `goal-loop-driver` in `tool_calls.jsonl` instead of the worker's own id.
/// That starved `task_observe`/A3/A4 of any evidence (every round settled
/// `Unobservable`, so `task_prediction_log` never closed and no task-rule was
/// ever induced), left the B3 grounding pre-check permanently `Skip`, and
/// made the MAV acceptance judge treat honest tool-backed work as an
/// unverifiable claim — see `wiki/reports/memory-quality/2026-08/wp-a10-live-test-2026-08-06.md`
/// BUG-1. Because all of those dispatch paths funnel through this single
/// read site, fixing it here covers every one of them; `goal_loop.rs`,
/// `cron_scheduler.rs`, `dispatcher.rs` and friends are unchanged.
///
/// A genuine agent-to-agent delegation (the sender is a real agent id, not a
/// reserved system sender) is unaffected: the sender IS the actual caller in
/// that case, so it is still what gets stamped. Delegation-POLICY decisions
/// (e.g. `handle_spawn_ephemeral`'s capability envelope / org-placement
/// `parent`) intentionally do NOT go through this helper — they must keep
/// reading `DUDUCLAW_DELEGATION_SENDER` verbatim, because "who is allowed to
/// delegate here" and "who executed this tool call" are different questions.
pub(crate) fn resolve_audit_agent(fallback: impl FnOnce() -> String) -> String {
    match std::env::var(duduclaw_core::ENV_DELEGATION_SENDER) {
        Ok(sender) if !sender.is_empty() && !duduclaw_core::is_system_sender(&sender) => sender,
        _ => fallback(),
    }
}

/// Which agent is actually acting behind `caller_client_id`?
///
/// Every agent the gateway spawns authenticates with ONE shared MCP key whose
/// client_id is [`duduclaw_gateway::mcp_internal_key::INTERNAL_CLIENT_ID`]
/// ("gateway-internal"), while the agent it is running for arrives separately
/// as `DUDUCLAW_AGENT_ID` → `default_agent`. A handler that treats the
/// client_id as an agent name therefore looks up `agents/gateway-internal/…`,
/// finds nothing, and denies everything — which is exactly how the native
/// `db_*` tools shipped broken in 1.64.0: `agent_update db_sources_add=crm`
/// wrote the grant to `agents/main/agent.toml`, the dispatch gate (which
/// already does this mapping, `mcp_dispatch.rs` §3.6) passed, and then
/// `db_sources` answered 「此代理沒有任何資料庫來源授權」. `mcp_dispatch`'s own
/// `gate_agent` is the same fix one layer up.
///
/// Mapping is by **exact** equality on the internal client_id (plus the legacy
/// empty-client_id stdio case) — never a prefix or substring test. Any other
/// client_id is an external client and is returned unchanged: an external
/// caller must NEVER inherit the process's default agent, or one API key would
/// read every internal agent's grants, files and recordings.
pub(crate) fn acting_agent_id<'a>(caller_client_id: &'a str, default_agent: &'a str) -> &'a str {
    if caller_client_id.is_empty()
        || caller_client_id == duduclaw_gateway::mcp_internal_key::INTERNAL_CLIENT_ID
    {
        default_agent
    } else {
        caller_client_id
    }
}

/// Extract the human-readable text portion of an MCP tool result envelope
/// (`{"content": [{"type": "text", "text": "..."}], "isError": bool}`) for
/// B3b audit-trail capture (`duduclaw_security::audit::append_tool_call_with_input`'s
/// `result_text` parameter). Joins every `type: "text"` block with `\n` — a
/// result can carry more than one text block; non-text blocks (e.g.
/// computer-use screenshots) are silently skipped since there is no text to
/// ground a claim against. Masking and size-capping happen inside the audit
/// helper, not here — this only extracts the raw text.
pub(crate) fn extract_tool_result_text(result: &Value) -> Option<String> {
    let blocks = result.get("content")?.as_array()?;
    let mut parts = Vec::new();
    for block in blocks {
        if block.get("type").and_then(|t| t.as_str()) == Some("text")
            && let Some(text) = block.get("text").and_then(|t| t.as_str())
        {
            parts.push(text);
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

/// Build a short summary of tool call parameters for audit logging.
/// Avoids logging full payloads (which may contain sensitive data).
/// The tool arguments as they may be written to an audit row. `computer_type`
/// is reduced to `{"chars": n}`: the text typed inside the computer-use
/// container (a password, say) must never reach `tool_calls.jsonl`, from
/// where the recent-actions feed can replay it into prompts. Every other
/// tool's arguments are returned unchanged (`computer_key` carries a key
/// name, which is fine).
pub(crate) fn audit_safe_arguments(tool_name: &str, args: &Value) -> Value {
    match tool_name {
        "computer_type" => {
            let chars = args.get("text").and_then(|v| v.as_str()).map(|t| t.chars().count()).unwrap_or(0);
            serde_json::json!({ "chars": chars })
        }
        "computer_navigate" => {
            let (host, path_len) = navigate_url_summary(args);
            serde_json::json!({ "host": host, "path_len": path_len })
        }
        _ => args.clone(),
    }
}

/// `computer_navigate`'s URL reduced to its host and path length: the query
/// string (tokens, search terms) and the path itself never reach
/// `tool_calls.jsonl`. An unparseable URL gives `None` / 0.
fn navigate_url_summary(args: &Value) -> (Option<String>, usize) {
    let parsed = args.get("url").and_then(|v| v.as_str()).and_then(|u| reqwest::Url::parse(u).ok());
    match parsed {
        Some(url) => (
            url.host_str().map(|h| duduclaw_core::truncate_chars(h, 80).to_string()),
            url.path().len(),
        ),
        None => (None, 0),
    }
}

pub(crate) fn build_params_summary(tool_name: &str, args: &Value) -> String {
    match tool_name {
        // Computer use: coordinates / sizes only. Typed text is summarised by
        // its length and a screenshot by its name (the image never lands in
        // the audit).
        "computer_click" | "computer_scroll" => {
            let n = |k: &str| args.get(k).map(|v| v.to_string()).unwrap_or_default();
            format!("x={} y={}", n("x"), n("y"))
        }
        "computer_type" => {
            let chars = args.get("text").and_then(|v| v.as_str()).map(|t| t.chars().count()).unwrap_or(0);
            format!("chars={chars}")
        }
        "computer_key" => {
            let key = args.get("key").and_then(|v| v.as_str()).unwrap_or("");
            format!("key={}", duduclaw_core::truncate_chars(key, 32))
        }
        "computer_screenshot" => "screenshot".to_string(),
        "computer_navigate" => {
            let (host, path_len) = navigate_url_summary(args);
            format!("host={} path_len={path_len}", host.as_deref().unwrap_or("?"))
        }
        "computer_session_start" | "computer_session_stop" => {
            let id = args.get("session_id").and_then(|v| v.as_str()).unwrap_or("");
            format!("session_id={}", duduclaw_core::truncate_chars(id, 64))
        }
        "create_agent" => {
            let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let display = args
                .get("display_name")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            format!("name={name} display_name={display}")
        }
        "agent_remove" => {
            // The tool's parameter is `agent_id`; this used to read `name`,
            // so every removal was summarised as `name=?`.
            let agent = args.get("agent_id").and_then(|v| v.as_str()).unwrap_or("?");
            format!("agent_id={}", duduclaw_core::truncate_chars(agent, 64))
        }
        "agent_update" => {
            let agent = args.get("agent_id").and_then(|v| v.as_str()).unwrap_or("?");
            let field = args.get("field").and_then(|v| v.as_str()).unwrap_or("?");
            format!("agent_id={agent} field={field}")
        }
        "agent_update_soul" => {
            let agent = args.get("agent_id").and_then(|v| v.as_str()).unwrap_or("?");
            format!("agent_id={agent}")
        }
        "spawn_agent" | "send_to_agent" => {
            let agent = args.get("agent_id").and_then(|v| v.as_str()).unwrap_or("?");
            format!("agent_id={agent}")
        }
        "spawn_ephemeral" => {
            let tier = args
                .get("tier")
                .and_then(|v| v.as_str())
                .unwrap_or("standard");
            let tools = args
                .get("tools")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            format!("tier={tier} tools={tools}")
        }
        "update_cron_task" => {
            let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("?");
            let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            format!("id={id} name={name}")
        }
        "delete_cron_task" => {
            let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("?");
            let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            format!("id={id} name={name}")
        }
        "pause_cron_task" => {
            let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("?");
            let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let enabled = args
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            format!("id={id} name={name} enabled={enabled}")
        }
        "run_cron_task" => {
            let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("?");
            let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            format!("id={id} name={name}")
        }
        _ => {
            let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let id = args.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
            format!("name={name} agent_id={id}")
        }
    }
}

// ── Voice / ASR / TTS handlers ─────────────────────────────────

#[cfg(test)]
mod computer_navigate_audit_tests {
    use super::*;

    #[test]
    fn navigate_audit_keeps_host_and_path_length_only() {
        let args = serde_json::json!({"url": "https://Example.com/a/b?token=secret#frag"});
        let safe = audit_safe_arguments("computer_navigate", &args);
        assert_eq!(safe, serde_json::json!({"host": "example.com", "path_len": 4}));
        let summary = build_params_summary("computer_navigate", &args);
        assert_eq!(summary, "host=example.com path_len=4");
        assert!(!safe.to_string().contains("secret") && !summary.contains("secret"));
        let bad = serde_json::json!({"url": "not a url"});
        assert_eq!(audit_safe_arguments("computer_navigate", &bad), serde_json::json!({"host": null, "path_len": 0}));
        assert_eq!(build_params_summary("computer_navigate", &bad), "host=? path_len=0");
    }
}
