use super::*;

// Update one or more fields in an existing agent's agent.toml.
//
// Reads the current config, applies the requested changes, and writes back.
// Uses `toml::to_string_pretty` for consistent formatting.
//
// `caller` is the MCP caller identity (`RecordActor::id`); WP21 C4 uses it to
// gate `reports_to` re-parenting.
//
// WP21 debt ⑥ — the C4 gate used to sit *inside* the `reports_to` branch only,
// which left the rest of the tool wide open: the same call could flip another
// department's agent to `status = "terminated"`, repoint its `model`, zero its
// budget or rewrite its heartbeat schedule without ever touching `reports_to`.
// Owning a node's settings is the same authority as owning its position in the
// tree, so `check_org_subject_allowed` ("whose settings may I touch?") is now a
// front gate over **every** field. Editing yourself stays free (the helper
// short-circuits on `node == caller`), as do system senders and the `open`
// policy escape hatch; changing your *own* `reports_to` still additionally
// needs the placement check below, which is the half this front gate does not
// cover.
/// Render a source-id list the way it reads in `agent.toml`.
pub(crate) fn render_db_source_list(ids: &[String]) -> String {
    format!(
        "[{}]",
        ids.iter()
            .map(|s| format!("\"{s}\""))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// The refusal text for an id that is not a configured `[db_sources.<id>]`.
///
/// Lists the configured **ids only** — never a driver, a URL, an
/// `allowed_tables` entry, or a loader's raw error text. A caller that
/// mistypes a source name needs the vocabulary, not the customer's connection
/// details.
pub(crate) fn db_source_unknown_message(wanted: &str, loaded: &duduclaw_db::LoadedDbSources) -> String {
    let shown = duduclaw_core::truncate_chars(wanted.trim(), 64);
    // "configured but broken" and "no such source" are different problems, and
    // telling them apart saves an operator from renaming a source that is
    // really just misconfigured.
    if loaded
        .errors
        .iter()
        .any(|e| e.name.trim().eq_ignore_ascii_case(wanted.trim()))
    {
        return format!(
            "資料來源「{shown}」的設定目前有誤而無法載入，未做任何變更。\
             請先到儀表板 設定 → 去識別化 →「外部系統與資料來源」修正該來源，再回來授權。"
        );
    }
    let configured = loaded.names();
    if configured.is_empty() {
        format!(
            "資料來源「{shown}」不存在，而且目前尚未設定任何資料來源，未做任何變更。\
             請先到儀表板 設定 → 去識別化 →「外部系統與資料來源」新增資料來源，再回來授權。"
        )
    } else {
        format!(
            "資料來源「{shown}」不存在，未做任何變更。目前已設定的資料來源 id：{}。\
             請填 [db_sources.<id>] 的 id（儀表板 設定 → 去識別化 →「外部系統與資料來源」卡片上的 id），不是顯示名稱。",
            configured.join("、")
        )
    }
}

/// Parse the comma-separated `db_sources_remove` parameter.
///
/// Deliberately NOT validated against `config.toml`, unlike its grant-side
/// siblings. Revocation only ever *reduces* authority, and the case that most
/// needs it is precisely the one config lookup cannot serve: an operator
/// deleted `[db_sources.old]` while an agent still held `"old"`, and that
/// stale grant must stay revocable. The id is compared against the agent's own
/// held list and never sent anywhere, so nothing is at risk.
///
/// What is still enforced is shape — the same character rules as a
/// `[db_sources.<id>]` key ([`duduclaw_db::config::is_valid_source_name`],
/// applied case-insensitively so `CRM` is accepted like everywhere else) — so
/// arbitrary caller text cannot reach the change log or the audit row.
pub(crate) fn parse_db_source_removals(raw: &str) -> std::result::Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for part in raw.split(',') {
        let id = part.trim();
        if id.is_empty() {
            continue;
        }
        if !duduclaw_db::config::is_valid_source_name(&id.to_ascii_lowercase()) {
            let shown = duduclaw_core::truncate_chars(id, 64);
            return Err(format!(
                "「{shown}」不是合法的資料來源 id，未做任何變更。\
                 資料來源 id 只允許英文字母、數字與底線（最長 64 字元）。"
            ));
        }
        // Exact, case-insensitive dedupe — never a substring test.
        if !out
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(id))
        {
            out.push(id.to_string());
        }
    }
    Ok(out)
}

/// Canonicalize one comma-separated `db_sources*` parameter into configured
/// source ids, or explain which entry could not be honoured.
///
/// Trims, drops empty entries, de-duplicates keeping first-seen order, and
/// rewrites every entry to the exact `[db_sources.<id>]` key it matched — so a
/// caller that types `CRM` stores `crm` and the grant compares equal wherever
/// it is read. Matching goes through [`duduclaw_db::LoadedDbSources::get`]
/// (exact, trimmed, ASCII-case-insensitive), never a substring test.
pub(crate) fn canonicalize_db_source_ids(
    raw: &str,
    loaded: &duduclaw_db::LoadedDbSources,
) -> std::result::Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for part in raw.split(',') {
        let wanted = part.trim();
        if wanted.is_empty() {
            continue;
        }
        let Some(entry) = loaded.get(wanted) else {
            return Err(db_source_unknown_message(wanted, loaded));
        };
        if !out.iter().any(|existing| existing == &entry.name) {
            out.push(entry.name.clone());
        }
    }
    Ok(out)
}

/// `agent_update` parameters that change an employee's authority: the MCP
/// counterparts of the keys the dashboard's `agents.update` treats as
/// admin-only (`AUTHORITY_KEYS` in `duduclaw-gateway/src/handlers/
/// agents_update_v168.rs`: `[agent] reports_to`, all of `[capabilities]` —
/// here the `db_sources*` grant params), plus the budget and `[agent] role`
/// (`role = "main"` is what the shared-wiki delete check and the
/// dispatcher's `default` alias read as the main agent). An AI employee may
/// not send any of these about itself.
pub(crate) const SELF_AUTHORITY_PARAMS: &[&str] = &[
    "reports_to",
    "db_sources",
    "db_sources_add",
    "db_sources_remove",
    "budget_cents",
    "role",
];

/// Update one or more fields of an existing agent's `agent.toml` (see the
/// notes at the top of this file for the org gate).
///
/// Self-edit authority guard: when an AI-employee caller targets itself, any
/// [`SELF_AUTHORITY_PARAMS`] key refuses the whole call and is audited as
/// `agent_authority_refused` — the org gate's `node == caller` short-circuit
/// would otherwise let an employee grant itself a database source, raise its
/// own budget or re-parent itself. Operators are not restricted; editing a
/// subordinate is unchanged.
pub(crate) async fn handle_agent_update(
    params: &Value,
    home_dir: &Path,
    actor: RecordActor<'_>,
) -> Value {
    let caller = actor.id();
    if let Err(reason) = check_actor_identity(home_dir, actor, "", "agent_update") {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: {reason}")}],
            "isError": true
        });
    }
    let agent_id = params
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if agent_id.is_empty() || !is_valid_agent_id(agent_id) {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: valid agent_id is required (lowercase alphanumeric with hyphens, max 64 chars)"}],
            "isError": true
        });
    }

    // Front gate, evaluated before the agent.toml read so an unauthorized
    // caller cannot use the "not found" vs "denied" split to probe which agent
    // ids exist (same anti-probing reasoning as WP21 T6's
    // `agent_not_visible_error`).
    // The audit `path_kind` still distinguishes a re-parenting attempt from an
    // ordinary settings edit, so widening the gate does not blur the log.
    // Three labels, most-sensitive-first: re-parenting moves org authority,
    // a database grant hands an agent a customer datastore, everything else is
    // an ordinary settings edit. The label lands in the audit `path_kind`, so
    // keeping them distinct is what lets "who opened the CRM to whom" be
    // greppable after the fact.
    let touches_db_sources = ["db_sources", "db_sources_add", "db_sources_remove"]
        .iter()
        .any(|k| params.get(*k).and_then(|v| v.as_str()).is_some());
    let subject_what = if params.get("reports_to").and_then(|v| v.as_str()).is_some() {
        "調整組織從屬"
    } else if touches_db_sources {
        "調整資料庫來源授權"
    } else {
        "調整 AI 員工設定"
    };
    if let Err(reason) = check_org_subject_allowed(home_dir, caller, agent_id, subject_what).await {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: {reason}")}],
            "isError": true
        });
    }

    if let Some(me) = actor.agent() {
        let is_self = agent_id == me.trim();
        let keys: Vec<&str> = SELF_AUTHORITY_PARAMS
            .iter()
            .copied()
            .filter(|k| params.get(*k).is_some())
            .collect();
        if is_self && !keys.is_empty() {
            duduclaw_security::audit::append_tool_call_with_extras(
                home_dir,
                me,
                "agent_authority_refused",
                &format!("agent_update: '{agent_id}' may not change its own {}", keys.join(", ")),
                false,
                &[
                    ("agent", serde_json::json!(agent_id)),
                    ("keys", serde_json::json!(keys)),
                    ("source", serde_json::json!("mcp.agent_update")),
                    ("reason", serde_json::json!("self_authority_change")),
                ],
            );
            return serde_json::json!({
                "content": [{"type": "text", "text": format!(
                    "Error: 不能調整自己的權限類設定（{}），未做任何變更。這些設定只能由主管（委派政策允許的上級）或操作者在儀表板調整。",
                    keys.join("、")
                )}],
                "isError": true
            });
        }
    }

    let agent_dir = home_dir.join("agents").join(agent_id);
    let toml_path = agent_dir.join("agent.toml");

    let content = match tokio::fs::read_to_string(&toml_path).await {
        Ok(c) => c,
        Err(_) => {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error: agent '{agent_id}' not found")}],
                "isError": true
            });
        }
    };

    let mut config: duduclaw_core::types::AgentConfig = match toml::from_str(&content) {
        Ok(c) => c,
        Err(e) => {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error parsing agent.toml: {e}")}],
                "isError": true
            });
        }
    };

    // WP: capture the pre-update display_name / trigger so a rename can
    // sync SOUL.md / IDENTITY.md self-introduction text and the default
    // `@trigger` afterward. Root cause: the agent's system-prompt self-name
    // comes 100% from literal text burned into SOUL.md at creation time —
    // agent.toml's display_name never reached the prompt on its own, so a
    // rename left the agent introducing itself with its old name forever.
    let old_display_name = config.agent.display_name.clone();
    let old_trigger = config.agent.trigger.clone();

    let mut changes = Vec::new();

    // -- Agent identity fields --
    if let Some(v) = params.get("display_name").and_then(|v| v.as_str()) {
        config.agent.display_name = v.to_string();
        changes.push(format!("display_name = \"{v}\""));

        // Auto-sync the default `@{old_name}` mention trigger to the new
        // name, unless the caller also set `trigger` explicitly in this same
        // request (that wins, and is applied below) or the existing trigger
        // was already customized away from the default pattern.
        if params.get("trigger").and_then(|t| t.as_str()).is_none() {
            if let Some(new_trigger) =
                duduclaw_core::synced_trigger(&old_trigger, &old_display_name, v)
            {
                changes.push(format!("trigger synced -> \"{new_trigger}\""));
                config.agent.trigger = new_trigger;
            }
        }
    }
    if let Some(v) = params.get("role").and_then(|v| v.as_str()) {
        use std::str::FromStr;
        let role = match duduclaw_core::types::AgentRole::from_str(v) {
            Ok(r) => r,
            Err(_) => {
                return serde_json::json!({
                    "content": [{"type": "text", "text": format!(
                        "Error: invalid role '{v}'. Valid: {}",
                        duduclaw_core::types::AgentRole::valid_values_help()
                    )}],
                    "isError": true
                });
            }
        };
        let canonical = role.as_str().to_string();
        config.agent.role = role;
        changes.push(format!("role = \"{canonical}\""));
    }
    if let Some(v) = params.get("status").and_then(|v| v.as_str()) {
        let status = match v.to_lowercase().as_str() {
            "active" => duduclaw_core::types::AgentStatus::Active,
            "paused" => duduclaw_core::types::AgentStatus::Paused,
            "terminated" => duduclaw_core::types::AgentStatus::Terminated,
            _ => {
                return serde_json::json!({
                    "content": [{"type": "text", "text": format!("Error: invalid status '{v}'. Valid: active, paused, terminated")}],
                    "isError": true
                });
            }
        };
        config.agent.status = status;
        changes.push(format!("status = \"{v}\""));
    }
    if let Some(v) = params.get("trigger").and_then(|v| v.as_str()) {
        config.agent.trigger = v.to_string();
        changes.push(format!("trigger = \"{v}\""));
    }
    if let Some(v) = params.get("icon").and_then(|v| v.as_str()) {
        config.agent.icon = v.to_string();
        changes.push(format!("icon = \"{v}\""));
    }
    if let Some(v) = params.get("reports_to").and_then(|v| v.as_str()) {
        // Validate reports_to references an existing agent and won't create a cycle
        if let Err(reason) = validate_reports_to(home_dir, agent_id, v).await {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error: {reason}")}],
                "isError": true
            });
        }
        // WP21 C4: re-parenting is the same escalation primitive as creating an
        // agent under someone else's manager, so it takes the same gate — twice.
        // The subject half ("may I reorganise *this* node at all?") is already
        // enforced as the whole-tool front gate above (debt ⑥); what remains
        // here is the destination half — the *new parent* must be the caller or
        // inside its subtree, so nobody promotes themselves under the CEO.
        // Evaluated against the org tree as it stands before this edit.
        if let Err(reason) = check_org_placement_allowed(home_dir, caller, v, "調整組織從屬").await
        {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error: {reason}")}],
                "isError": true
            });
        }
        config.agent.reports_to = v.to_string();
        changes.push(format!("reports_to = \"{v}\""));
    }

    // -- Model fields --
    if let Some(v) = params.get("model").and_then(|v| v.as_str()) {
        config.model.preferred = v.to_string();
        changes.push(format!("model.preferred = \"{v}\""));
    }
    if let Some(v) = params.get("fallback_model").and_then(|v| v.as_str()) {
        config.model.fallback = v.to_string();
        changes.push(format!("model.fallback = \"{v}\""));
    }
    if let Some(v) = params.get("api_mode").and_then(|v| v.as_str()) {
        match v {
            "cli" | "direct" | "auto" => {
                config.model.api_mode = v.to_string();
                changes.push(format!("model.api_mode = \"{v}\""));
            }
            _ => {
                return serde_json::json!({
                    "content": [{"type": "text", "text": format!("Error: invalid api_mode '{v}'. Valid: cli, direct, auto")}],
                    "isError": true
                });
            }
        }
    }

    // -- Budget fields --
    if let Some(v) = params.get("budget_cents").and_then(|v| v.as_u64()) {
        config.budget.monthly_limit_cents = v;
        changes.push(format!("budget.monthly_limit_cents = {v}"));
    }

    // -- Container fields --
    if let Some(v) = params.get("max_concurrent").and_then(|v| v.as_u64()) {
        config.container.max_concurrent = v as u32;
        changes.push(format!("container.max_concurrent = {v}"));
    }

    // -- Heartbeat fields --
    if let Some(v) = params.get("heartbeat_enabled")
        && let Some(b) = v.as_bool()
    {
        config.heartbeat.enabled = b;
        changes.push(format!("heartbeat.enabled = {b}"));
    }
    if let Some(v) = params.get("heartbeat_cron").and_then(|v| v.as_str()) {
        config.heartbeat.cron = v.to_string();
        changes.push(format!("heartbeat.cron = \"{v}\""));
    }

    // -- Capability fields: read-only database source grants (WP-B) --
    //
    // `[capabilities] db_sources` is deny-by-default: an agent with no grant
    // cannot even list the configured sources, let alone query one. Until now
    // the only way to open a source was hand-editing agent.toml, so a manager
    // could not say 「把客戶 CRM 資料庫開給小美」 and have it happen. These three
    // params are the conversational route to the same authority the dashboard
    // writes, behind the `check_org_subject_allowed` front gate this handler
    // already applied (you may only edit yourself or your own subtree).
    //
    // Fail-closed on the granting side: every id named by `db_sources` /
    // `db_sources_add` is validated against the configured
    // `[db_sources.<id>]` blocks BEFORE `config.capabilities.db_sources` is
    // touched, so a call naming one good and one bad id writes nothing at all.
    // `db_sources_remove` is shape-checked only — revocation reduces authority
    // and must keep working for a source config no longer declares.
    let mut db_sources_after: Option<Vec<String>> = None;
    let mut db_sources_added: Vec<String> = Vec::new();
    let mut db_sources_removed: Vec<String> = Vec::new();
    if touches_db_sources {
        let loaded = duduclaw_db::load_db_sources(home_dir).await;
        // A config.toml that does not parse degrades to "no sources
        // configured", which would turn every id into a bogus "does not
        // exist". Refuse loudly instead of guessing.
        if loaded.errors.iter().any(|e| e.name == "config.toml") {
            return serde_json::json!({
                "content": [{"type": "text", "text":
                    "Error: 無法讀取 config.toml 的資料來源設定，未做任何變更。請先修正 config.toml 後再授權資料庫來源。"}],
                "isError": true
            });
        }

        // Validate up front — nothing is written on error.
        let parse_param = |key: &str| -> std::result::Result<Option<Vec<String>>, String> {
            match params.get(key).and_then(|v| v.as_str()) {
                Some(raw) => canonicalize_db_source_ids(raw, &loaded).map(Some),
                None => Ok(None),
            }
        };
        let replace_ids = match parse_param("db_sources") {
            Ok(v) => v,
            Err(msg) => {
                return serde_json::json!({
                    "content": [{"type": "text", "text": format!("Error: {msg}")}],
                    "isError": true
                });
            }
        };
        let add_ids = match parse_param("db_sources_add") {
            Ok(v) => v,
            Err(msg) => {
                return serde_json::json!({
                    "content": [{"type": "text", "text": format!("Error: {msg}")}],
                    "isError": true
                });
            }
        };
        // Revocation is intentionally config-free — see
        // `parse_db_source_removals`.
        let remove_ids = match params.get("db_sources_remove").and_then(|v| v.as_str()) {
            Some(raw) => match parse_db_source_removals(raw) {
                Ok(ids) => Some(ids),
                Err(msg) => {
                    return serde_json::json!({
                        "content": [{"type": "text", "text": format!("Error: {msg}")}],
                        "isError": true
                    });
                }
            },
            None => None,
        };

        let before = config.capabilities.db_sources.clone();
        let mut current = before.clone();

        // Precedence: replace, then add, then remove.
        if let Some(ids) = replace_ids {
            changes.push(format!(
                "capabilities.db_sources = {}",
                render_db_source_list(&ids)
            ));
            current = ids;
        }

        let mut newly_added: Vec<String> = Vec::new();
        let mut already_held: Vec<String> = Vec::new();
        for id in add_ids.unwrap_or_default() {
            // Exact (trimmed, case-insensitive) equality — never substring.
            if current
                .iter()
                .any(|held| held.trim().eq_ignore_ascii_case(&id))
            {
                already_held.push(id);
            } else {
                current.push(id.clone());
                newly_added.push(id);
            }
        }
        if !newly_added.is_empty() {
            changes.push(format!(
                "capabilities.db_sources += {}",
                render_db_source_list(&newly_added)
            ));
        }
        if !already_held.is_empty() {
            changes.push(format!(
                "capabilities.db_sources 未變更（已持有：{}）",
                already_held.join("、")
            ));
        }

        let mut dropped: Vec<String> = Vec::new();
        let mut not_held: Vec<String> = Vec::new();
        for id in remove_ids.unwrap_or_default() {
            let len_before = current.len();
            current.retain(|held| !held.trim().eq_ignore_ascii_case(&id));
            if current.len() == len_before {
                not_held.push(id);
            } else {
                dropped.push(id);
            }
        }
        if !dropped.is_empty() {
            changes.push(format!(
                "capabilities.db_sources -= {}",
                render_db_source_list(&dropped)
            ));
        }
        if !not_held.is_empty() {
            changes.push(format!(
                "capabilities.db_sources 未變更（未持有：{}）",
                not_held.join("、")
            ));
        }

        // Audit fields describe the NET effect against the pre-call list, so a
        // replace that happens to drop two sources is recorded as two
        // revocations rather than as an opaque "= [...]".
        db_sources_added = current
            .iter()
            .filter(|id| {
                !before
                    .iter()
                    .any(|old| old.trim().eq_ignore_ascii_case(id.trim()))
            })
            .cloned()
            .collect();
        db_sources_removed = before
            .iter()
            .filter(|id| {
                !current
                    .iter()
                    .any(|new| new.trim().eq_ignore_ascii_case(id.trim()))
            })
            .cloned()
            .collect();

        config.capabilities.db_sources = current.clone();
        db_sources_after = Some(current);
    }

    if changes.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: no valid fields to update. Supported fields: display_name, role, status, trigger, icon, reports_to, model, fallback_model, api_mode, budget_cents, max_concurrent, heartbeat_enabled, heartbeat_cron, db_sources, db_sources_add, db_sources_remove"}],
            "isError": true
        });
    }

    // WP22 T1 — re-parenting lands in the authoritative store first, then in
    // the `agent.toml` mirror below (see the ordering note in
    // `handle_create_agent`). Only written when this call actually touched
    // `reports_to`: an unrelated edit (icon, model, …) must not quietly adopt
    // a mirror value the operator has not synced.
    if let Some(v) = params.get("reports_to").and_then(|v| v.as_str()) {
        if let Err(e) = duduclaw_core::org_store::upsert(
            home_dir,
            agent_id,
            duduclaw_core::OrgEntry::new(v, &config.agent.department),
        ) {
            tracing::warn!(agent = %agent_id, error = %e, "org.toml upsert failed on agent_update");
        }
    }

    // Serialize and write atomically (temp + rename)
    let updated_toml = match toml::to_string_pretty(&config) {
        Ok(s) => s,
        Err(e) => {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error serializing agent.toml: {e}")}],
                "isError": true
            });
        }
    };

    let tmp_path = toml_path.with_extension("toml.tmp");
    if let Err(e) = tokio::fs::write(&tmp_path, &updated_toml).await {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error writing agent.toml: {e}")}],
            "isError": true
        });
    }
    if let Err(e) = tokio::fs::rename(&tmp_path, &toml_path).await {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error committing agent.toml: {e}")}],
            "isError": true
        });
    }

    // WP: sync SOUL.md / IDENTITY.md self-introduction text to the new
    // display_name (see comment above the capture site). Best-effort: a
    // missing file is skipped, an IO error is logged but does not fail the
    // already-committed agent.toml write.
    let mut soul_sync_changes: Vec<String> = Vec::new();
    if old_display_name != config.agent.display_name && !old_display_name.is_empty() {
        let new_display_name = &config.agent.display_name;
        for fname in ["SOUL.md", "IDENTITY.md"] {
            let path = agent_dir.join(fname);
            let content = match tokio::fs::read_to_string(&path).await {
                Ok(c) => c,
                Err(_) => continue, // file doesn't exist — nothing to sync
            };
            let (new_content, changed) =
                duduclaw_core::rename_in_markdown(&content, &old_display_name, new_display_name);
            if !changed {
                continue;
            }
            let tmp_path = path.with_extension("md.tmp");
            if let Err(e) = tokio::fs::write(&tmp_path, &new_content).await {
                tracing::warn!(agent_id, file = fname, error = %e, "Failed to write identity-rename tmp file");
                continue;
            }
            if let Err(e) = tokio::fs::rename(&tmp_path, &path).await {
                let _ = tokio::fs::remove_file(&tmp_path).await;
                tracing::warn!(agent_id, file = fname, error = %e, "Failed to commit identity-rename");
                continue;
            }
            soul_sync_changes.push(format!(
                "{fname} self-name synced \"{old_display_name}\" -> \"{new_display_name}\""
            ));
        }
    }
    changes.extend(soul_sync_changes);

    // Audit: a database grant hands an agent read access to a customer
    // datastore, so it gets its own row rather than living only inside the
    // generic `agent_update` tool-call record (whose params_summary does not
    // carry the resulting list). Written after the commit, so the log records
    // what is actually on disk.
    let db_summary = match &db_sources_after {
        Some(after) => {
            let resulting = if after.is_empty() {
                "（無）".to_string()
            } else {
                after.join("、")
            };
            duduclaw_security::audit::append_tool_call_with_extras(
                home_dir,
                caller,
                "db_sources_grant_changed",
                &format!(
                    "{subject_what}: '{caller}' -> '{agent_id}' now grants [{}]",
                    after.join(", ")
                ),
                true,
                &[
                    ("agent", serde_json::json!(agent_id)),
                    ("added", serde_json::json!(db_sources_added)),
                    ("removed", serde_json::json!(db_sources_removed)),
                    ("resulting", serde_json::json!(after)),
                    ("path_kind", serde_json::json!(subject_what)),
                ],
            );
            format!("\n\n目前資料庫來源授權：{resulting}")
        }
        None => String::new(),
    };

    serde_json::json!({
        "content": [{"type": "text", "text": format!(
            "Agent '{agent_id}' updated successfully.\n\nChanges:\n{}{db_summary}",
            changes.iter().map(|c| format!("  • {c}")).collect::<Vec<_>>().join("\n")
        )}]
    })
}
