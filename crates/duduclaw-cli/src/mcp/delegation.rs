use super::*;

/// Send a message to another agent via the bus queue.
pub(crate) async fn handle_send_to_agent(params: &Value, home_dir: &Path, caller: &str) -> Value {
    send_to_agent_with_ctx(params, home_dir, caller, DelegationContext::from_env()).await
}

/// Core implementation with injectable delegation context.
/// Production callers use `DelegationContext::from_env()`;
/// tests inject a specific context to avoid unsafe env var mutation.
pub(crate) async fn send_to_agent_with_ctx(
    params: &Value,
    home_dir: &Path,
    caller: &str,
    ctx: DelegationContext,
) -> Value {
    let target = params
        .get("agent_id")
        .or_else(|| params.get("agent"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let prompt = params
        .get("prompt")
        .or_else(|| params.get("message"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if target.is_empty() || prompt.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: agent_id and prompt are required"}],
            "isError": true
        });
    }

    // Validate agent_id format
    if !is_valid_agent_id(target) {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: agent_id must be lowercase alphanumeric with hyphens"}],
            "isError": true
        });
    }

    // ── WP21 C2: department × hierarchy delegation gate ────────
    if let Err(reason) = check_delegation_allowed(home_dir, caller, target, "send_to_agent").await {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: {reason}")}],
            "isError": true
        });
    }

    // ── Delegation depth tracking ──────────────────────────────
    let incoming_depth = ctx.depth;
    let outgoing_depth = incoming_depth.saturating_add(1);

    if outgoing_depth >= duduclaw_core::MAX_DELEGATION_DEPTH {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "Error: delegation depth limit ({}) would be exceeded. \
                 Current depth: {incoming_depth}, chain origin: {}. \
                 Cannot delegate further to prevent infinite loops.",
                duduclaw_core::MAX_DELEGATION_DEPTH,
                ctx.origin.as_deref().unwrap_or("unknown"),
            )}],
            "isError": true
        });
    }

    let origin = ctx.origin.as_deref().unwrap_or(caller);

    let msg_id = uuid::Uuid::new_v4().to_string();

    // v1.8.18: SQLite `message_queue.db` is the authoritative dispatch rail.
    // Writing to `bus_queue.jsonl` as well created a dual-rail race: the
    // legacy `poll_and_dispatch` loop tokio::spawn's its own Claude CLI
    // task (which DROPS task-local REPLY_CHANNEL), so whichever side won
    // the race determined whether sub-agents inherited channel context.
    // When legacy won, the v1.8.16 reply_channel propagation was silently
    // defeated — nested sub-agent callbacks never registered, replies
    // were silently dropped. Fix: stop writing to bus_queue.jsonl here.
    // (Orphan-response recovery / task_created signals / spawn_agent
    // entries still use bus_queue.jsonl — those are untouched.)
    //
    // v1.8.16 behaviour preserved: propagate `DUDUCLAW_REPLY_CHANNEL`
    // into the row so the dispatcher can scope REPLY_CHANNEL around the
    // target agent's Claude CLI when it spawns. Best-effort: if the
    // ALTER TABLE migration hasn't run yet the fallback INSERT (without
    // reply_channel) succeeds on the legacy schema.
    let queued = {
        let db_path = home_dir.join("message_queue.db");
        let msg_id_cl = msg_id.clone();
        let caller_cl = caller.to_string();
        let target_cl = target.to_string();
        let prompt_cl = prompt.to_string();
        let origin_cl = origin.to_string();
        let ts_now = chrono::Utc::now().to_rfc3339();
        let reply_channel = std::env::var(duduclaw_core::ENV_REPLY_CHANNEL)
            .ok()
            .filter(|s| !s.is_empty());
        // v1.10: Forward wiki RL trust feedback context so the dispatcher
        // can re-establish task_locals around the sub-agent dispatch and
        // sub-agent RAG citations attribute back to the originating turn.
        let trust_turn_id = std::env::var(duduclaw_core::ENV_TRUST_TURN_ID)
            .ok()
            .filter(|s| !s.is_empty());
        let trust_session_id = std::env::var(duduclaw_core::ENV_TRUST_SESSION_ID)
            .ok()
            .filter(|s| !s.is_empty());
        tokio::task::spawn_blocking(move || -> bool {
            let Ok(conn) = rusqlite::Connection::open(&db_path) else {
                return false;
            };
            let _ = conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;");
            let inserted = conn.execute(
                "INSERT OR IGNORE INTO message_queue \
                 (id, sender, target, payload, status, retry_count, delegation_depth, \
                  origin_agent, sender_agent, created_at, reply_channel, turn_id, session_id) \
                 VALUES (?1, ?2, ?3, ?4, 'pending', 0, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                rusqlite::params![
                    msg_id_cl,
                    caller_cl,
                    target_cl,
                    prompt_cl,
                    outgoing_depth,
                    origin_cl,
                    caller_cl,
                    ts_now,
                    reply_channel,
                    trust_turn_id,
                    trust_session_id,
                ],
            );
            if let Ok(rows) = inserted {
                return rows > 0;
            }
            // Legacy schema fallback (pre-v1.8.16 — no reply_channel,
            // turn_id, session_id columns). Gateway migrates on next start.
            conn.execute(
                "INSERT OR IGNORE INTO message_queue \
                 (id, sender, target, payload, status, retry_count, delegation_depth, \
                  origin_agent, sender_agent, created_at) \
                 VALUES (?1, ?2, ?3, ?4, 'pending', 0, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    msg_id_cl,
                    caller_cl,
                    target_cl,
                    prompt_cl,
                    outgoing_depth,
                    origin_cl,
                    caller_cl,
                    ts_now,
                ],
            )
            .map(|rows| rows > 0)
            .unwrap_or(false)
        })
        .await
        .unwrap_or(false)
    };

    // Register delegation callback if running inside a channel context.
    // The dispatcher will use this to forward the sub-agent's response
    // back to the originating channel (Telegram/LINE/Discord/etc.).
    if let Ok(reply_channel) = std::env::var(duduclaw_core::ENV_REPLY_CHANNEL) {
        let db_path = home_dir.join("message_queue.db");
        let msg_id_cb = msg_id.clone();
        let caller_cb = caller.to_string();
        let channel_str = reply_channel;
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(conn) = rusqlite::Connection::open(&db_path) {
                let _ = conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;");
                // Ensure table exists (MCP process may open DB before gateway).
                // Schema must match message_queue.rs init_schema — keep in sync.
                let _ = conn.execute_batch(
                    "CREATE TABLE IF NOT EXISTS delegation_callbacks (
                         message_id   TEXT PRIMARY KEY,
                         agent_id     TEXT NOT NULL,
                         channel_type TEXT NOT NULL,
                         channel_id   TEXT NOT NULL,
                         thread_id    TEXT,
                         retry_count  INTEGER NOT NULL DEFAULT 0,
                         created_at   TEXT NOT NULL
                     );
                     CREATE INDEX IF NOT EXISTS idx_dc_agent ON delegation_callbacks(agent_id);"
                );
                // Parse channel context. Supported formats:
                //   "telegram:12345"            → chat, no thread
                //   "telegram:12345:6789"       → chat + topic/thread
                //   "discord:<channel_id>"      → main channel
                //   "discord:thread:<thread_id>" → Discord thread (thread IS a
                //                                  channel to the Discord API;
                //                                  the literal token "thread"
                //                                  is a marker, not an ID)
                //   "line:<user_id>"            → LINE user
                //   "slack:<channel_id>"        → Slack channel
                //   "slack:<channel_id>:<ts>"   → Slack thread (ts = parent timestamp)
                let parts: Vec<&str> = channel_str.splitn(3, ':').collect();
                if parts.len() >= 2 && duduclaw_core::SUPPORTED_CHANNEL_TYPES.contains(&parts[0]) {
                    // Rate limit: max 100 pending callbacks per agent to prevent DoS
                    let count: i64 = conn.query_row(
                        "SELECT COUNT(*) FROM delegation_callbacks WHERE agent_id = ?1",
                        rusqlite::params![caller_cb],
                        |r| r.get(0),
                    ).unwrap_or(0);
                    if count >= 100 {
                        tracing::warn!(agent = %caller_cb, "delegation_callbacks per-agent limit (100) reached");
                    } else {
                    let ch_type = parts[0];
                    // Special case: `<type>:thread:<id>` — "thread" is a marker
                    // word, not the channel_id. Collapse to (ch_id=<id>,
                    // thread_id=None) because Discord's API treats a thread as
                    // a regular channel endpoint. Storing "thread" as ch_id
                    // makes validate_channel_id reject the forward as non-
                    // numeric and the sub-agent's reply never reaches the user.
                    let (ch_id, thread) = if parts.len() == 3 && parts[1] == "thread" {
                        (parts[2], None)
                    } else {
                        (parts[1], parts.get(2).map(|s| s.to_string()))
                    };
                    let now = chrono::Utc::now().to_rfc3339();
                    let _ = conn.execute(
                        "INSERT OR IGNORE INTO delegation_callbacks \
                         (message_id, agent_id, channel_type, channel_id, thread_id, created_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        rusqlite::params![msg_id_cb, caller_cb, ch_type, ch_id, thread, now],
                    );
                    }
                }
            }
        }).await;
    }

    let ts = chrono::Utc::now().to_rfc3339();
    if queued {
        serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "Receipt: message_id={msg_id}, target={target}, depth={outgoing_depth}, \
                 status=queued, timestamp={ts}. \
                 The gateway dispatcher will deliver this message."
            )}],
            "_receipt": {
                "message_id": msg_id,
                "target": target,
                "status": "queued",
                "depth": outgoing_depth,
                "timestamp": ts,
            }
        })
    } else {
        serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "Failed to queue message for agent '{target}'"
            )}],
            "isError": true
        })
    }
}

/// Cascade **hop depth** for the feedback path (P3, paper 2607.01641). Read from
/// the dispatcher-injected env var only — untrusted tool params are ignored,
/// same threat model as [`DelegationContext::from_env`]. Distinct from
/// `delegation_depth`: hop_depth rides the bus task across the dispatcher's
/// re-spawn boundary so a re-generating feedback loop inherits (never resets)
/// its depth.
pub(crate) fn incoming_hop_depth() -> u8 {
    std::env::var(duduclaw_core::ENV_HOP_DEPTH)
        .ok()
        .and_then(|v| v.parse::<u8>().ok())
        .unwrap_or(0)
}

/// P3 runaway guard for a bus delegation enqueue. Two independent bounds,
/// both fail-visible (a denied dispatch returns a concrete MCP error — never
/// silently dropped):
///
/// 1. **Cascade hop-depth** — inherit the dispatcher-injected `hop_depth`,
///    increment, and reject once it exceeds `[dispatch_guard] max_hop_depth`
///    (default [`duduclaw_core::DEFAULT_MAX_HOP_DEPTH`]). Bounds delegation-chain
///    explosion across re-spawn boundaries.
/// 2. **Sliding-window circuit breaker** — a cross-process rate limiter keyed on
///    `(path_kind, agent_id)` (`<home>/dispatch_guard.json`). Bounds a runaway
///    that spams delegations faster than the chain deepens.
///
/// `agent_id` is the *originator* of the dispatches (the calling agent), so one
/// runaway agent cannot starve another's budget. Returns the outgoing hop_depth
/// to stamp onto the new bus task, or `Err(response)` to reject the call.
pub(crate) fn check_dispatch_runaway(
    home_dir: &Path,
    path_kind: &str,
    agent_id: &str,
) -> std::result::Result<u8, Value> {
    let cfg = duduclaw_core::DispatchGuardConfig::from_home(home_dir);

    let outgoing_hop = incoming_hop_depth().saturating_add(1);
    if outgoing_hop > cfg.max_hop_depth {
        return Err(mcp_error(&format!(
            "委派鏈過深:hop_depth {outgoing_hop} 超過上限 {} — 已中止以防失控迴圈(runaway loop)。",
            cfg.max_hop_depth
        )));
    }

    match duduclaw_core::dispatch_guard_check(home_dir, path_kind, agent_id, &cfg) {
        duduclaw_core::DispatchGuardDecision::Allow => Ok(outgoing_hop),
        duduclaw_core::DispatchGuardDecision::Trip {
            reason,
            retry_after_secs,
        } => Err(mcp_error(&format!(
            "派工斷路器已跳閘,拒絕本次委派({reason})。請於約 {retry_after_secs}s 後重試 \
                 — 此為防止失控迴圈(runaway)的保護機制。"
        ))),
    }
}

/// Normalize reports_to: both "" and "none" mean root (no parent).
pub(crate) fn normalize_reports_to(value: &str) -> &str {
    if value.is_empty() || value == "none" {
        ""
    } else {
        value
    }
}

/// WP21 — snapshot the slice of the org tree a delegation decision needs.
///
/// Only the named agents and their `reports_to` ancestor chains are read
/// (≤ `MAX_ANCESTOR_HOPS` hops each), never the whole registry: the predicate
/// asks exactly two questions — "is either party an ancestor of the other?" and
/// "are their departments equal?" — and both are answerable from those chains.
/// An agent with no `agent.toml` is simply absent from the snapshot, which the
/// core predicate treats as an unproven relation (fail-closed).
///
/// WP22 T1 — the org fields are read from [`duduclaw_core::org_store`] first:
/// `<home>/org.toml` sits outside every agent's working directory, so an agent
/// that rewrites its own `agent.toml` (by Bash, or from a runtime with no
/// `PreToolUse` hook at all) no longer moves itself in the tree. An agent with
/// **no store entry** still resolves from `agent.toml`, which keeps
/// pre-WP22 installs, hand-created agents and every existing fixture working.
pub(crate) async fn org_snapshot(home_dir: &Path, agents: &[&str]) -> duduclaw_core::MapOrgView {
    let agents_dir = home_dir.join("agents");
    let store = duduclaw_core::org_store::load(home_dir);
    let mut view = duduclaw_core::MapOrgView::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for start in agents {
        let mut current = start.trim().to_string();
        // +1 so the starting node itself does not consume a hop budget that
        // belongs to the ancestor walk.
        for _ in 0..=duduclaw_core::MAX_ANCESTOR_HOPS {
            if current.is_empty() || !seen.insert(current.clone()) {
                break; // root reached, or this chain was already walked
            }
            let node = match store.get(&current) {
                // Authoritative record — the mirror is not consulted at all.
                Some(entry) => (entry.reports_to.clone(), entry.department.clone()),
                None => {
                    let Some(cfg) = read_agent_config(&agents_dir, &current).await else {
                        break; // unknown agent — leave it out of the snapshot
                    };
                    (
                        normalize_reports_to(cfg.agent.reports_to.trim()).to_string(),
                        cfg.agent.department.trim().to_string(),
                    )
                }
            };
            let (parent, department) = node;
            view.insert(current.as_str(), parent.as_str(), department.as_str());
            if parent.is_empty() {
                break;
            }
            current = parent;
        }
    }
    view
}

/// WP21 C2 — may `sender` delegate work to `target`?
///
/// Replaces the pre-WP21 `check_supervisor_relation`, which recognised exactly
/// one shape (a *direct* parent↔child pair) and so could not express skip-level
/// command or same-department peer collaboration. The rule now lives in
/// `duduclaw-core::delegation_policy` so this front door, the bus dispatcher and
/// the task board all answer the question identically; this wrapper supplies
/// only the two things core cannot fetch itself — the configured policy and an
/// org snapshot read from `agent.toml` on disk.
///
/// Every DENY writes a `delegation_denied` audit row and returns the zh-TW
/// explanation for the agent to read.
///
/// WP21 T11: reads the full [`duduclaw_core::DelegationRules`] (policy +
/// `[delegation] allow` whitelist), not just the bare policy, so an explicit
/// cross-department/cross-hierarchy pair (design doc §2.1 rule 5b) is honored
/// at this front door exactly as it is at the C1 dispatcher gate.
pub(crate) async fn check_delegation_allowed(
    home_dir: &Path,
    sender: &str,
    target: &str,
    path_kind: &str,
) -> std::result::Result<(), String> {
    let rules = duduclaw_core::delegation_rules_from_home(home_dir);
    let org = org_snapshot(home_dir, &[sender, target]).await;
    match duduclaw_core::can_delegate_rules(&rules, &org, sender, target) {
        Ok(()) => Ok(()),
        Err(denied) => {
            duduclaw_security::audit::append_tool_call_with_extras(
                home_dir,
                sender,
                "delegation_denied",
                &format!(
                    "{path_kind}: '{sender}' -> '{target}' denied ({})",
                    denied.reason.as_str()
                ),
                false,
                &[
                    ("target", serde_json::json!(target)),
                    ("reason", serde_json::json!(denied.reason.as_str())),
                    ("policy", serde_json::json!(denied.policy.as_str())),
                    ("path_kind", serde_json::json!(path_kind)),
                ],
            );
            Err(denied.message_zh())
        }
    }
}

/// WP21 T6 (design doc §2.5) — may `caller` see `target` in `list_agents` /
/// `agent_status`?
///
/// "A can delegate to B" and "A can see B" deliberately share the same
/// hierarchy/department test, plus the whitelist: the design doc's own
/// rationale for folding whitelist partners into the visible set is that a
/// caller who can delegate to someone but cannot see them is a
/// self-contradicting UX. `Open` policy, system senders, and `caller ==
/// target` are each call site's responsibility to short-circuit *before*
/// reaching this — `list_agents` always lists the caller unconditionally, and
/// neither call site pays for an org read when the policy is `open`.
pub(crate) fn org_visible(
    rules: &duduclaw_core::DelegationRules,
    org: &impl duduclaw_core::OrgView,
    caller: &str,
    target: &str,
) -> bool {
    if caller == target {
        return true;
    }
    if duduclaw_core::is_org_ancestor(org, caller, target)
        || duduclaw_core::is_org_ancestor(org, target, caller)
    {
        return true;
    }
    if rules.policy == duduclaw_core::DelegationPolicy::Department {
        let caller_dept = org.department(caller).unwrap_or_default();
        let target_dept = org.department(target).unwrap_or_default();
        let caller_dept = caller_dept.trim();
        let target_dept = target_dept.trim();
        if !caller_dept.is_empty() && !target_dept.is_empty() && caller_dept == target_dept {
            return true;
        }
    }
    rules.allows_pair(caller, target)
}

/// WP21 T6 — the single "not found or not visible" response for
/// `agent_status`. Both cases return byte-identical text so the tool cannot be
/// used to probe which agent ids exist versus which are merely hidden from
/// the caller.
pub(crate) fn agent_not_visible_error(agent_id: &str) -> Value {
    serde_json::json!({
        "content": [{"type": "text", "text": format!("Error: agent '{agent_id}' not found or not visible")}],
        "isError": true
    })
}

/// WP21 C4 — the org-placement authorization rule shared by `create_agent` and
/// `agent_update`.
///
/// Attaching (or moving) an agent anywhere outside the caller's own subtree is
/// self-service privilege escalation: hang a node under the CEO and the
/// delegation predicate above will happily say "yes" to it forever after. The
/// only nodes a caller may touch are therefore itself and the nodes it already
/// commands.
///
/// `Ok(None)` = allowed. `Ok(Some(policy))` = the caller is not entitled, with
/// the policy that decided it (the caller renders the message it needs, since
/// "you may not attach *under* X" and "you may not reorganise X" are different
/// sentences). Skipped for `DelegationPolicy::Open` (the documented escape
/// hatch) and for system / human-interface senders — the dashboard and gateway
/// build agents through their own handlers, not this MCP path, but an
/// operator-driven caller id must never be mistaken for an agent promoting
/// itself.
pub(crate) async fn org_subtree_check(
    home_dir: &Path,
    caller: &str,
    node: &str,
) -> Option<duduclaw_core::DelegationPolicy> {
    if duduclaw_core::is_system_sender(caller) {
        return None; // human interface, not an agent in the org tree
    }
    let policy = duduclaw_core::delegation_policy_from_home(home_dir);
    if policy == duduclaw_core::DelegationPolicy::Open {
        return None;
    }

    let caller = caller.trim();
    let node = normalize_reports_to(node.trim());
    if node.is_empty() {
        return Some(policy); // root placement is outside every caller's subtree
    }
    if node == caller {
        return None;
    }
    let org = org_snapshot(home_dir, &[caller, node]).await;
    if duduclaw_core::is_org_ancestor(&org, caller, node) {
        None
    } else {
        Some(policy)
    }
}

pub(crate) fn audit_org_placement_denied(
    home_dir: &Path,
    caller: &str,
    node: &str,
    policy: duduclaw_core::DelegationPolicy,
    what: &str,
) {
    duduclaw_security::audit::append_tool_call_with_extras(
        home_dir,
        caller,
        "org_placement_denied",
        &format!("{what}: '{caller}' -> '{node}' denied (outside caller subtree)"),
        false,
        &[
            ("node", serde_json::json!(node)),
            ("policy", serde_json::json!(policy.as_str())),
            ("path_kind", serde_json::json!(what)),
        ],
    );
}

/// WP21 C4 — may `caller` place an agent *under* `parent`?
pub(crate) async fn check_org_placement_allowed(
    home_dir: &Path,
    caller: &str,
    parent: &str,
    what: &str,
) -> std::result::Result<(), String> {
    let Some(policy) = org_subtree_check(home_dir, caller, parent).await else {
        return Ok(());
    };
    audit_org_placement_denied(home_dir, caller, parent, policy, what);

    let caller_short = duduclaw_core::truncate_chars(caller.trim(), 64);
    let parent = normalize_reports_to(parent.trim());
    let parent_label = if parent.is_empty() {
        "最上層(無主管)".to_string()
    } else {
        format!("「{}」", duduclaw_core::truncate_chars(parent, 64))
    };
    Err(format!(
        "{what}遭拒:「{caller_short}」只能將 AI 員工掛在自己或自己團隊之下,\
         不能掛到 {parent_label} 之下(委派政策:{})。\
         可行處理:① 把 reports_to 設為「{caller_short}」或你團隊內既有的成員;\
         ② 請該主管自行建立/調整;③ 需要完全開放時,在 config.toml 將 \
         [delegation] policy 設為 \"open\"。",
        policy.label_zh()
    ))
}

/// WP21 C4 — may `caller` reorganise `node` at all? (`node` must be the caller
/// itself or someone the caller already commands.)
pub(crate) async fn check_org_subject_allowed(
    home_dir: &Path,
    caller: &str,
    node: &str,
    what: &str,
) -> std::result::Result<(), String> {
    let Some(policy) = org_subtree_check(home_dir, caller, node).await else {
        return Ok(());
    };
    audit_org_placement_denied(home_dir, caller, node, policy, what);

    let caller_short = duduclaw_core::truncate_chars(caller.trim(), 64);
    let node_short = duduclaw_core::truncate_chars(node.trim(), 64);
    Err(format!(
        "{what}遭拒:「{caller_short}」只能調整自己或自己團隊成員的從屬關係,\
         「{node_short}」不在你的團隊之內(委派政策:{})。\
         可行處理:① 請「{node_short}」的主管自行調整;\
         ② 需要完全開放時,在 config.toml 將 [delegation] policy 設為 \"open\"。",
        policy.label_zh()
    ))
}

/// Validate that a `reports_to` value references an existing agent (or is empty
/// for root agents) and does not create a cycle.
pub(crate) async fn validate_reports_to(
    home_dir: &Path,
    agent_name: &str,
    reports_to: &str,
) -> std::result::Result<(), String> {
    if reports_to.is_empty() || reports_to == "none" {
        return Ok(()); // root agent
    }

    // Cannot report to self
    if reports_to == agent_name {
        return Err(format!("Agent '{agent_name}' cannot report to itself"));
    }

    let agents_dir = home_dir.join("agents");

    // Target must exist
    if !agents_dir.join(reports_to).join("agent.toml").exists() {
        return Err(format!(
            "reports_to '{reports_to}' does not exist. \
             Create the agent first or use an empty string for root."
        ));
    }

    // Walk up the chain to detect cycles (max 20 hops as safety bound)
    let store = duduclaw_core::org_store::load(home_dir);
    let mut current = reports_to.to_string();
    let mut visited = std::collections::HashSet::new();
    visited.insert(agent_name.to_string());

    for _ in 0..20 {
        if !visited.insert(current.clone()) {
            return Err(format!(
                "Circular reports_to detected: setting '{agent_name}'.reports_to='{reports_to}' \
                 would create a cycle involving '{current}'"
            ));
        }
        // WP22 T1: walk the *authoritative* chain (`org.toml` where a record
        // exists, `agent.toml` otherwise). Reading only the mirror would let a
        // cycle that exists in the authority slip through this check.
        let next = match store.get(&current) {
            Some(entry) => entry.reports_to.clone(),
            None => match read_agent_config(&agents_dir, &current).await {
                Some(cfg) => normalize_reports_to(cfg.agent.reports_to.trim()).to_string(),
                None => break, // dangling reference — not our problem here
            },
        };
        if next.is_empty() {
            break; // reached root
        }
        current = next;
    }

    Ok(())
}
