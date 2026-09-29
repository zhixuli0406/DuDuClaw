use super::*;

/// Remove an agent directory after safety checks.
///
/// Refuses to remove the main agent. Moves to `_trash/{name}_{timestamp}` instead
/// of hard-deleting, so recovery is possible.
///
/// WP21 C4: removal is the same escalation-adjacent primitive as `reports_to`
/// reparenting — deleting a node you don't command is not something a caller
/// should be able to do just because it knows the agent id. Gated by the same
/// `check_org_subject_allowed` the `reports_to` branch of `agent_update` uses:
/// only the caller's own subtree (or itself) is fair game. System senders and
/// `DelegationPolicy::Open` remain exempt (baked into the helper).
pub(crate) async fn handle_agent_remove(params: &Value, home_dir: &Path, caller: &str) -> Value {
    let agent_id = params
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if agent_id.is_empty() || !is_valid_agent_id(agent_id) {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: valid agent_id is required"}],
            "isError": true
        });
    }

    if let Err(reason) = check_org_subject_allowed(home_dir, caller, agent_id, "移除 AI 員工").await
    {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: {reason}")}],
            "isError": true
        });
    }

    let agent_dir = home_dir.join("agents").join(agent_id);
    let toml_path = agent_dir.join("agent.toml");

    // Verify agent exists
    let content = match tokio::fs::read_to_string(&toml_path).await {
        Ok(c) => c,
        Err(_) => {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error: agent '{agent_id}' not found")}],
                "isError": true
            });
        }
    };

    // Refuse to remove main agent
    if let Ok(config) = toml::from_str::<duduclaw_core::types::AgentConfig>(&content)
        && config.agent.role == duduclaw_core::types::AgentRole::Main
    {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: cannot remove main agent '{agent_id}'. Change its role first if you really mean to.")}],
            "isError": true
        });
    }

    // Move to trash instead of hard delete
    let trash_dir = home_dir.join("agents").join("_trash");
    let _ = tokio::fs::create_dir_all(&trash_dir).await;
    let timestamp = chrono::Utc::now().format("%Y%m%d%H%M%S");
    let trash_name = format!("{agent_id}_{timestamp}");
    let trash_path = trash_dir.join(&trash_name);

    if let Err(e) = tokio::fs::rename(&agent_dir, &trash_path).await {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error moving agent to trash: {e}")}],
            "isError": true
        });
    }

    // WP22 T1 — drop the authoritative record only *after* the agent is really
    // gone. This is the one write that must not go store-first: had the trash
    // move failed, an already-cleared entry would leave a still-live agent
    // governed by its own (writable) `agent.toml` mirror again.
    if let Err(e) = duduclaw_core::org_store::remove(home_dir, agent_id) {
        tracing::warn!(agent = %agent_id, error = %e, "org.toml removal failed on agent_remove");
    }

    serde_json::json!({
        "content": [{"type": "text", "text": format!(
            "Agent '{agent_id}' removed (moved to trash).\n\
             Recovery path: {}\n\n\
             To permanently delete: rm -rf {}",
            trash_path.display(),
            trash_path.display()
        )}]
    })
}

/// WP1.1 C4 (`DESIGN-evolution-v3-aee.md` §1.9.2) — `[permissions]
/// can_modify_own_soul` read fresh, in isolation, for the C2 gate below.
///
/// Deserializes only the `[permissions]` table so an unrelated malformed
/// section elsewhere in `agent.toml` cannot silently widen this to `true`
/// (same pattern as `mcp_dispatch::PolicyOnlyConfig`). Fail-closed: a missing
/// file, missing `[permissions]` section, missing key, or malformed TOML all
/// resolve to `false` — this flag is the *only* escape hatch the C2 gate
/// honours, so an unreadable value must never be treated as an implicit
/// grant. `rbac.rs` (the previous, never-called reader of this field) is
/// deleted as part of this WP — see the B3 verdict in
/// `commercial/docs/TODO-evolution-v3-2026-08.md` §5.5.
pub(crate) async fn can_modify_own_soul(home_dir: &Path, agent_id: &str) -> bool {
    #[derive(serde::Deserialize, Default)]
    struct PermsOnly {
        #[serde(default)]
        can_modify_own_soul: bool,
    }
    #[derive(serde::Deserialize, Default)]
    struct Wrap {
        #[serde(default)]
        permissions: PermsOnly,
    }
    let path = home_dir.join("agents").join(agent_id).join("agent.toml");
    let Ok(content) = tokio::fs::read_to_string(&path).await else {
        return false;
    };
    toml::from_str::<Wrap>(&content)
        .map(|w| w.permissions.can_modify_own_soul)
        .unwrap_or(false)
}

/// WP1.1 C2 (`DESIGN-evolution-v3-aee.md` §1.9.2) — the MCP front-door
/// identity gate for `agent_update_soul`.
///
/// Every legitimate agent's own `.mcp.json` injects `DUDUCLAW_AGENT_ID`
/// (`identity_token::agent_identity_env_vars`), so a non-empty claim in this
/// process's environment means the call arrived from an agent's own spawned
/// MCP subprocess, not from an operator. An absent claim matches the
/// `HookCaller::Absent` convention already used by `org_field_guard`:
/// nobody but an operator (dashboard / `templates.*` build flow / a human
/// running tooling by hand) reaches this server with no agent identity at
/// all, so that caller is unrestricted here too.
///
/// Returns `Some(caller_id)` when the call must be DENIED (an agent caller
/// that is not the target with `can_modify_own_soul = true`), `None` when it
/// may proceed. `caller_id` is the *verified* identity used in the audit
/// row — under `[delegation] require_identity_token = true` an unverifiable
/// claim collapses to `UNTRUSTED_AGENT_ID`, which can never equal `agent_id`
/// and is therefore always denied, matching every other WP21 gate's
/// fail-closed contract.
pub(crate) async fn soul_write_denial(home_dir: &Path, agent_id: &str) -> Option<String> {
    let claimed = std::env::var(duduclaw_core::ENV_AGENT_ID).unwrap_or_default();
    let claimed = claimed.trim();
    if claimed.is_empty() {
        // Operator / dashboard convention — unrestricted.
        return None;
    }
    let caller_id = if caller_identity_verdict(home_dir) == duduclaw_core::IdentityVerdict::Rejected
    {
        duduclaw_core::UNTRUSTED_AGENT_ID.to_string()
    } else {
        claimed.to_string()
    };
    let is_self = caller_id.eq_ignore_ascii_case(agent_id);
    let allowed = is_self && can_modify_own_soul(home_dir, agent_id).await;
    if allowed { None } else { Some(caller_id) }
}

/// Update SOUL.md for an agent via the trusted MCP channel.
///
/// This bypasses the `agent-file-guard` PreToolUse hook (which blocks
/// Write/Edit/MultiEdit on SOUL.md) because MCP tools are a trusted code path
/// in the DuDuClaw architecture — the authorization decision is made here
/// instead, by `can_modify_own_soul`.
///
/// Post-write, this fn calls `soul_guard::store_hash` to keep the integrity
/// fingerprint in sync (otherwise `check_soul_integrity` would forever
/// report drift after every legitimate update — observed on agnes
/// 2026-05-19 02:27Z) and `audit::append_tool_call` for traceability.
///
/// WP1.1 (SOUL.md 唯讀化, `DESIGN-evolution-v3-aee.md` §1.9): the doc comment
/// above ("Bypasses file-protect hooks") used to make this the one code path
/// through which an agent's own in-process MCP principal — which holds
/// `Scope::Admin` by default — could rewrite its own SOUL.md wholesale. The
/// C2 gate below closes that: an agent-identified caller is denied unless it
/// is the target and `[permissions] can_modify_own_soul = true`. Operator /
/// dashboard callers (no agent identity in the environment) are unaffected.
pub(crate) async fn handle_agent_update_soul(params: &Value, home_dir: &Path) -> Value {
    let agent_id = params
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if agent_id.is_empty() || !is_valid_agent_id(agent_id) {
        // Audit even validation rejections so operators can spot agents
        // probing the SOUL.md backdoor with malformed inputs.
        duduclaw_security::audit::append_tool_call(
            home_dir,
            "",
            "agent_update_soul",
            &format!("REJECTED: invalid agent_id={agent_id:?}"),
            false,
        );
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: valid agent_id is required"}],
            "isError": true
        });
    }

    // C2 — identity gate. Runs before the content/existence checks below so
    // an agent probing this tool learns "denied" rather than incidentally
    // fingerprinting which agent ids exist via the "not found" branch.
    if let Some(caller_id) = soul_write_denial(home_dir, agent_id).await {
        duduclaw_security::audit::append_tool_call(
            home_dir,
            agent_id,
            "agent_update_soul",
            &format!(
                "DENIED: soul_write_denied caller='{caller_id}' target='{agent_id}' \
                 (agent principal, not operator; can_modify_own_soul not granted for self-write)"
            ),
            false,
        );
        return serde_json::json!({
            "content": [{"type": "text", "text":
                "Error: SOUL.md 由 operator 透過儀表板管理，AI 員工無法經此工具改寫人格檔（自己或其他 agent 皆同）。\
                 如需為此 agent 開放自我修改的實驗性逃生艙，請管理員在其 agent.toml 的 [permissions] \
                 將 can_modify_own_soul 設為 true。"
            }],
            "isError": true
        });
    }

    let soul_content = params.get("content").and_then(|v| v.as_str()).unwrap_or("");
    if soul_content.is_empty() {
        duduclaw_security::audit::append_tool_call(
            home_dir,
            agent_id,
            "agent_update_soul",
            "REJECTED: empty content",
            false,
        );
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: content is required (the new SOUL.md text)"}],
            "isError": true
        });
    }

    let agent_dir = home_dir.join("agents").join(agent_id);

    // Verify agent exists
    if !agent_dir.join("agent.toml").exists() {
        duduclaw_security::audit::append_tool_call(
            home_dir,
            agent_id,
            "agent_update_soul",
            &format!("REJECTED: agent '{agent_id}' not found"),
            false,
        );
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: agent '{agent_id}' not found")}],
            "isError": true
        });
    }

    let soul_path = agent_dir.join("SOUL.md");

    // Read old content for SHA-256 fingerprint comparison
    let old_content = tokio::fs::read_to_string(&soul_path)
        .await
        .unwrap_or_default();
    let old_hash = {
        let digest = <sha2::Sha256 as sha2::Digest>::digest(old_content.as_bytes());
        format!("{:x}", digest)
    };

    // Atomic write: temp file + rename
    let tmp_path = soul_path.with_extension("md.tmp");
    if let Err(e) = tokio::fs::write(&tmp_path, soul_content).await {
        duduclaw_security::audit::append_tool_call(
            home_dir,
            agent_id,
            "agent_update_soul",
            &format!("FAILED: write tmp: {e}"),
            false,
        );
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error writing SOUL.md: {e}")}],
            "isError": true
        });
    }
    if let Err(e) = tokio::fs::rename(&tmp_path, &soul_path).await {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        duduclaw_security::audit::append_tool_call(
            home_dir,
            agent_id,
            "agent_update_soul",
            &format!("FAILED: rename: {e}"),
            false,
        );
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error committing SOUL.md: {e}")}],
            "isError": true
        });
    }

    let new_hash = {
        let digest = <sha2::Sha256 as sha2::Digest>::digest(soul_content.as_bytes());
        format!("{:x}", digest)
    };

    // Refresh the soul_guard integrity hash. Without this, the next
    // `check_soul_integrity` call (and the new heartbeat check added in
    // 2026-05-20) would flag every legitimate `agent_update_soul` call as
    // tampering. Failure here is logged but does not fail the tool call —
    // the SOUL.md was already updated and the heartbeat drift warning is
    // a recoverable signal, not a security violation we can roll back.
    if let Err(e) = duduclaw_security::soul_guard::accept_soul_change(agent_id, &agent_dir) {
        tracing::warn!(
            agent = %agent_id,
            "Failed to refresh soul_guard hash after agent_update_soul: {e} — \
             next integrity check will flag drift until manually re-accepted"
        );
    }

    duduclaw_security::audit::append_tool_call(
        home_dir,
        agent_id,
        "agent_update_soul",
        &format!(
            "ok: old_hash={}, new_hash={}, size={}",
            &old_hash[..16.min(old_hash.len())],
            &new_hash[..16.min(new_hash.len())],
            soul_content.len()
        ),
        true,
    );

    serde_json::json!({
        "content": [{"type": "text", "text": format!(
            "SOUL.md updated for agent '{agent_id}'.\n\
             Old SHA-256: {old_hash}\n\
             New SHA-256: {new_hash}\n\
             Size: {} bytes",
            soul_content.len()
        )}]
    })
}

pub(crate) async fn handle_submit_feedback(params: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let signal_type = params
        .get("signal_type")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let detail = params.get("detail").and_then(|v| v.as_str()).unwrap_or("");
    let agent_id = params
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or(default_agent);

    if signal_type.is_empty() || detail.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: signal_type and detail are required"}],
            "isError": true
        });
    }

    if !["positive", "negative", "correction"].contains(&signal_type) {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: signal_type must be positive, negative, or correction"}],
            "isError": true
        });
    }

    match duduclaw_gateway::external_factors::submit_feedback(
        home_dir,
        agent_id,
        signal_type,
        "mcp",
        detail,
    )
    .await
    {
        Ok(()) => serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "Feedback recorded: [{signal_type}] for agent '{agent_id}'. This will be included in the next evolution reflection."
            )}]
        }),
        Err(e) => serde_json::json!({
            "content": [{"type": "text", "text": format!("Error submitting feedback: {e}")}],
            "isError": true
        }),
    }
}

// ── Evolution control handlers ──────────────────────────────
