use super::*;

/// RM-027 round 6: how long a CLI writer that holds the global org lock waits
/// for an `agent.toml` (or other per-agent file) lock —
/// [`duduclaw_core::org_store::MIRROR_LOCK_WAIT`], the bound the gateway's
/// org-lock holders use. Tests shorten it per home.
pub(crate) fn org_locked_file_wait(home: &Path) -> std::time::Duration {
    #[cfg(test)]
    {
        if let Some(d) = org_locked_wait_override::get(home) {
            return d;
        }
    }
    let _ = home;
    duduclaw_core::org_store::MIRROR_LOCK_WAIT
}

/// Test seam: a shorter [`org_locked_file_wait`] for one home.
#[cfg(test)]
pub(crate) mod org_locked_wait_override {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::time::Duration;

    static WAITS: Mutex<BTreeMap<PathBuf, Duration>> = Mutex::new(BTreeMap::new());

    pub(crate) fn set(home: &Path, wait: Duration) {
        WAITS.lock().unwrap().insert(home.to_path_buf(), wait);
    }

    pub(crate) fn clear(home: &Path) {
        WAITS.lock().unwrap().remove(home);
    }

    pub(super) fn get(home: &Path) -> Option<Duration> {
        WAITS.lock().unwrap().get(home).copied()
    }
}

/// The tool error for a per-agent file lock that was not acquired within
/// [`org_locked_file_wait`] while the org lock was held: nothing was written.
pub(crate) fn org_locked_wait_timeout_error(what: &str, detail: &str) -> Value {
    serde_json::json!({
        "content": [{"type": "text", "text": format!(
            "Error: lock_wait_timeout — {what} 正被其他寫入佔用，等待後仍無法取得寫入鎖，沒有做任何變更（組織資料也沒有改動）；請稍後再試。（{detail}）"
        )}],
        "isError": true
    })
}

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

/// RM-149 round 2: `agent_update` refuses a `status` change for an archived
/// or soft-deleted employee (the Admin-only lifecycle).
pub(crate) const ADMIN_LIFECYCLE_STATUS_REFUSED: &str = "Error: 這位 AI 員工已封存或已刪除，狀態只能由管理員在儀表板變更（解除封存，或新建一位 AI 員工）。未做任何變更。";

/// Runtime writes are not supported by this MCP tool; never silently accept
/// these parameters alongside an otherwise valid day-to-day edit.
const UNSUPPORTED_RUNTIME_PARAMS: &[&str] = &["runtime", "timeout_secs", "max_turns"];
const AI_AUTHORITY_REFUSED: &str = "這項設定只能由管理員在管理介面變更，未做任何變更。請向提出要求的人說明，並請他聯絡管理員處理。";

fn audit_ai_authority_refusal(home: &Path, caller: &str, target: &str, field: &str, class: &str) {
    duduclaw_security::audit::append_tool_call_with_extras(
        home, caller, "agent_authority_refused", "agent_update authority change refused", false,
        &[
            ("caller", serde_json::json!(caller)),
            ("target", serde_json::json!(target)),
            ("agent", serde_json::json!(target)),
            ("field", serde_json::json!(field)),
            ("keys", serde_json::json!([field])),
            ("attempted_value_class", serde_json::json!(class)),
            ("source", serde_json::json!("mcp.agent_update")),
            ("reason", serde_json::json!("authority_widening")),
        ],
    );
}

fn check_ai_authority(
    home: &Path, actor: RecordActor<'_>, target: &str,
    before: &toml::Table, after: &toml::Table, approved_db: Option<&[String]>,
) -> std::result::Result<(), String> {
    let Some(caller) = actor.agent() else { return Ok(()); };
    let changes: Vec<_> = duduclaw_core::agent_toml::authority::authority_diff(before, after)
        .into_iter().filter(|change| !(change.key == "capabilities.db_sources"
            && approved_db.is_some_and(|ids| change.after == serde_json::json!(ids))))
        .collect();
    for change in &changes {
        let class = match change.key.as_str() {
            "budget.monthly_limit_cents" | "budget.daily_cap_cents" => {
                if change.after.as_u64() == Some(0) { "limit_removed" } else { "limit_increase" }
            }
            "budget.hard_stop" => "hard_stop_disabled",
            "budget.when_unavailable" => "failure_policy_loosened",
            "capabilities.db_sources" => "grant_expansion",
            _ => "protected_value_change",
        };
        audit_ai_authority_refusal(home, caller, target, &change.key, class);
    }
    if changes.is_empty() { Ok(()) } else { Err(AI_AUTHORITY_REFUSED.into()) }
}

/// RM-137 H4: every `agent_update` parameter that creates, resumes or
/// reschedules background work for the target employee. The handler accepts
/// no other proactive / tick / schedule field (`[heartbeat]` has only
/// `enabled` and `cron` here; `[agent] status` is what the heartbeat scheduler
/// and the proactive invoker check before running an employee). Shared with
/// [`changes_background_work`].
pub(crate) const BACKGROUND_WORK_PARAMS: &[&str] = &["status", "heartbeat_enabled", "heartbeat_cron"];

/// RM-137 H4: the AI employee that must pass the delegation predicate for
/// this `agent_update` — `Some(caller)` when an AI employee changes a
/// [`BACKGROUND_WORK_PARAMS`] field of ANOTHER employee. Operators and
/// self-edits keep today's rule (`None`).
pub(crate) fn background_delegation_sender<'a>(
    actor: RecordActor<'a>,
    target: &str,
    params: &Value,
) -> Option<&'a str> {
    let me = actor.agent()?;
    (me.trim() != target.trim()
        && BACKGROUND_WORK_PARAMS.iter().any(|k| params.get(*k).is_some()))
    .then_some(me)
}

/// Update one or more fields of an existing agent's `agent.toml` (see the
/// notes at the top of this file for the org gate).
///
/// AI callers share the dashboard's non-admin authority rules, for both
/// self-edits and other employees. Delegation policies never widen authority.
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

    // RM-137 H4: starting, resuming, pausing or rescheduling another
    // employee's background work (the heartbeat scheduler and the proactive
    // invoker run it afterwards) is delegation, so it must pass the delegation
    // predicate as well — refused under `none`. Re-checked inside the locked
    // write below.
    let background_sender = background_delegation_sender(actor, agent_id, params);
    if let Some(me) = background_sender
        && let Err(reason) =
            check_delegation_allowed(home_dir, me, agent_id, "agent_update_background").await
    {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: {reason}")}],
            "isError": true
        });
    }

    if let Some(me) = actor.agent() {
        let keys: Vec<_> = UNSUPPORTED_RUNTIME_PARAMS.iter()
            .filter(|key| params.get(**key).is_some()).collect();
        if !keys.is_empty() {
            for key in keys {
                audit_ai_authority_refusal(home_dir, me, agent_id, key, "unsupported_runtime_write");
            }
            return serde_json::json!({
                "content": [{"type":"text", "text":AI_AUTHORITY_REFUSED}], "isError":true
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
    // The typed config as read, so only the keys this call changes are
    // written back (format-preserving, see the write below).
    let config_before = config.clone();

    // RM-149 round 2: `archived` and `deleted` are the Admin-only lifecycle
    // (a deleted employee holds no licence seat). `status` here cannot move
    // an employee out of either — that would resurrect it without the seat
    // admission and around the dashboard's restore. Re-checked on the file
    // as read under its lock below.
    let status_requested = params.get("status").and_then(|v| v.as_str()).is_some();
    if status_requested
        && matches!(
            config_before.agent.status,
            duduclaw_core::types::AgentStatus::Archived | duduclaw_core::types::AgentStatus::Deleted
        )
    {
        return serde_json::json!({
            "content": [{"type": "text", "text": ADMIN_LIFECYCLE_STATUS_REFUSED}],
            "isError": true
        });
    }

    let mut changes = Vec::new();

    // -- Agent identity fields --
    if let Some(v) = params.get("display_name").and_then(|v| v.as_str()) {
        // Written into SOUL.md / IDENTITY.md by the rename sync: plain text only.
        if let Err(e) = duduclaw_core::validate_display_name(v).and_then(|_| {
            duduclaw_gateway::agent_files::external::rename_is_safe("", v)
                .map_err(|cats| format!("display_name looks like an instruction to the AI ({cats}); choose a plain name"))
        }) {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error: {e}")}],
                "isError": true
            });
        }
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
        let before: toml::Table = match content.parse() {
            Ok(table) => table,
            Err(_) => return serde_json::json!({"content":[{"type":"text", "text":"無法確認目前設定，未做任何變更。"}], "isError":true}),
        };
        let mut after = before.clone();
        if let Some(agent) = after.get_mut("agent").and_then(toml::Value::as_table_mut) {
            agent.insert("reports_to".into(), toml::Value::String(v.into()));
        }
        if let Err(reason) = check_ai_authority(home_dir, actor, agent_id, &before, &after, None) {
            return serde_json::json!({"content":[{"type":"text", "text":reason}], "isError":true});
        }
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

    // -- Reply language ([prompt] reply_language) --
    // Fixed list (auto / zh-TW / zh-CN / en / ja-JP); empty string clears the
    // override so the employee inherits `[general] default_language`.
    if let Some(v) = params.get("reply_language").and_then(|v| v.as_str()) {
        let v = v.trim();
        if v.is_empty() {
            config.prompt.reply_language = None;
            changes.push("prompt.reply_language cleared".to_string());
        } else if duduclaw_core::profile_sections::is_valid_reply_language(v) {
            config.prompt.reply_language = Some(v.to_string());
            changes.push(format!("prompt.reply_language = \"{v}\""));
        } else {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!(
                    "Error: invalid reply_language '{v}'. Valid: {} (empty string = inherit the global setting)",
                    duduclaw_core::profile_sections::REPLY_LANGUAGE_CHOICES.join(", ")
                )}],
                "isError": true
            });
        }
    }
    if let Some(value) = params.get("memory_language") {
        let Some(v) = value.as_str() else {
            return serde_json::json!({"content": [{"type":"text", "text":"Error: memory_language must be a string"}], "isError":true});
        };
        let v = v.trim();
        if v.is_empty() {
            config.memory.language = None;
            changes.push("memory.language cleared".to_string());
        } else if duduclaw_core::profile_sections::is_valid_reply_language(v) {
            config.memory.language = Some(v.to_string());
            changes.push(format!("memory.language = \"{v}\""));
        } else {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!(
                    "Error: invalid memory_language '{v}'. Valid: {} (empty string = inherit the global setting)",
                    duduclaw_core::profile_sections::REPLY_LANGUAGE_CHOICES.join(", ")
                )}],
                "isError": true
            });
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
    // AI expansions for another employee require a bound Admin approval.
    // Self-expansions remain refused; revocations apply directly.
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
            "content": [{"type": "text", "text": "Error: no valid fields to update. Supported fields: display_name, role, status, trigger, icon, reports_to, model, fallback_model, api_mode, reply_language, memory_language, budget_cents, max_concurrent, heartbeat_enabled, heartbeat_cron, db_sources, db_sources_add, db_sources_remove"}],
            "isError": true
        });
    }

    // A status stop takes the exclusive lease in the profile writer. Never
    // retain a shared lease while waiting for that exclusive barrier.
    let stopping = params.get("status").and_then(Value::as_str)
        .is_some_and(|status| status.to_lowercase() != "active");
    let background_edit = changes_background_work("agent_update", params);
    // Acquire the shared lease inside the file lock too: the reverse order
    // would deadlock against a lifecycle writer waiting for admissions.
    let mut admission = None;

    // Write through the gateway's profile file writer: under the shared
    // cross-process lock, only the changed keys (comments and the keys the
    // typed form does not know survive), a version and the
    // `agent_file_changed` audit row.
    let typed = |c: &duduclaw_core::types::AgentConfig| -> std::result::Result<toml::Table, String> {
        toml::Table::try_from(c).map_err(|e| format!("Error serializing agent.toml: {e}"))
    };
    let (before_t, after_t) = match (typed(&config_before), typed(&config)) {
        (Ok(b), Ok(a)) => (b, a),
        (Err(e), _) | (_, Err(e)) => {
            return serde_json::json!({ "content": [{"type": "text", "text": e}], "isError": true });
        }
    };
    // Refuse unrelated authority changes before filing a card. Only this
    // other-employee database expansion is eligible for the Admin exception.
    let needs_db_approval = actor.agent().is_some_and(|me| me != agent_id)
        && !db_sources_added.is_empty();
    let approved_sources = if needs_db_approval { db_sources_after.as_deref() } else { None };
    if let Err(reason) = check_ai_authority(home_dir, actor, agent_id, &before_t, &after_t, approved_sources) {
        return serde_json::json!({"content":[{"type":"text", "text":reason}], "isError":true});
    }
    let db_approval = if let Some(sources) = approved_sources {
        match super::approval::approve_db_source_grant(home_dir, caller, agent_id, sources).await {
            Ok(proof) => Some(proof),
            Err(reason) => return serde_json::json!({"content":[{"type":"text", "text":reason}], "isError":true}),
        }
    } else { None };
    // RM-027 review: a re-parenting holds the global org mutation lock
    // (shared with the dashboard `org.*` RPCs, `agents.update` and
    // `duduclaw org sync`), taken BEFORE the agent.toml lock — the order every
    // org writer uses — so validation and commit see one tree.
    let reports_to_param = params.get("reports_to").and_then(Value::as_str);
    let org_lock = match reports_to_param {
        Some(_) => match duduclaw_core::org_store::lock_org_mutations(home_dir) {
            Ok(g) => Some(g),
            Err(e) => {
                return serde_json::json!({
                    "content": [{"type": "text", "text": format!("Error: 暫時無法取得組織資料的寫入鎖，沒有做任何變更：{e}")}],
                    "isError": true
                });
            }
        },
        None => None,
    };
    // The authority entry before this write, once it was replaced.
    let mut org_prior: Option<Option<duduclaw_core::OrgEntry>> = None;
    // RM-027 round 6: while the org lock is held the agent.toml lock wait is
    // bounded — a stuck writer fails this update (nothing written, the
    // authority untouched: the guard below never ran) instead of pinning the
    // org lock for every other org writer.
    let lock_wait = org_lock.as_ref().map(|_| org_locked_file_wait(home_dir));
    let written = duduclaw_gateway::agent_files::external::edit_agent_toml_guarded_within(home_dir, agent_id, "mcp", caller, |raw| {
        // RM-149 round 2: the lifecycle check again, on the file as read
        // under its lock (an Admin may have archived / deleted it meanwhile).
        if status_requested
            && matches!(
                raw.get("agent").and_then(|a| a.get("status")).and_then(toml::Value::as_str),
                Some(duduclaw_core::agent_seats::ARCHIVED_STATUS | duduclaw_core::agent_seats::DELETED_STATUS)
            )
        {
            return Err(ADMIN_LIFECYCLE_STATUS_REFUSED.to_string());
        }
        let authority_before = raw.clone();
        duduclaw_gateway::agent_files::external::apply_typed_changes(raw, &before_t, &after_t);
        // A requested stop must reach the fresh artifact even if the initial
        // snapshot had that status already and another writer resumed it.
        if stopping {
            let status = after_t.get("agent").and_then(|a| a.get("status")).cloned()
                .ok_or_else(|| "agent.toml missing status".to_string())?;
            raw.get_mut("agent").and_then(toml::Value::as_table_mut)
                .ok_or_else(|| "agent.toml missing [agent] section".to_string())?
                .insert("status".into(), status);
        }
        // Compare the artifact read under the cross-process lock with the
        // final artifact. A concurrent admin tightening cannot be overwritten
        // using an older, more permissive baseline.
        if let Some(sources) = approved_sources {
            // A concurrent revocation must not be undone by an old approval.
            let current = authority_before.get("capabilities").and_then(|v| v.get("db_sources"));
            let held: Option<Vec<&str>> = match current {
                None => Some(Vec::new()),
                Some(toml::Value::Array(ids)) => ids.iter().map(toml::Value::as_str).collect(),
                _ => None,
            };
            let baseline: Vec<&str> = config_before.capabilities.db_sources.iter().map(String::as_str).collect();
            if held.as_ref() != Some(&baseline) { return Err("資料庫授權已變更，請重新提出核准。".into()); }
            let final_list = raw.get("capabilities").and_then(|v| v.get("db_sources"))
                .and_then(toml::Value::as_array).ok_or("核准清單不符。")?;
            let exact: Option<Vec<&str>> = final_list.iter().map(toml::Value::as_str).collect();
            if exact.as_deref() != Some(sources.iter().map(String::as_str).collect::<Vec<_>>().as_slice()) {
                return Err("核准清單不符。".into());
            }
        }
        check_ai_authority(home_dir, actor, agent_id, &authority_before, raw, approved_sources)?;
        Ok(())
    }, || {
        // RM-137 H4: the policy / org tree may have changed since the front
        // check; decide again under the agent.toml lock.
        if let Some(me) = background_sender {
            check_delegation_allowed_blocking(home_dir, me, agent_id, "agent_update_background")?;
        }
        if background_edit {
            if stopping {
                check_background_caller(home_dir, actor, "agent_update")?;
            } else if let Some(agent) = actor.agent() {
                admission = Some(duduclaw_core::autonomy_gate::admit_blocking(
                    home_dir, agent, duduclaw_core::autonomy_gate::AutonomyPath::Background,
                ).map_err(|info| background_caller_refusal("agent_update", &info))?);
            }
        }
        // Keep the authoritative org write after the lifecycle barrier and
        // before its TOML mirror. A drain timeout must have no side effects.
        // Inside the org lock: re-read the authority strictly (a seeded but
        // lost / corrupt / newer `org.toml` refuses), re-validate the whole
        // tree, commit. Any failure aborts the write — the mirror never moves
        // without the authority.
        if let (Some(v), Some(lock)) = (reports_to_param, org_lock.as_ref()) {
            let store = lock.load().map_err(|e| e.to_string())?;
            duduclaw_core::org_change::validate_org_change_in(&store, home_dir, agent_id, Some(v), None)
                .map_err(|e| e.to_string())?;
            let current = duduclaw_core::org_change::effective_in(&store, home_dir, agent_id).unwrap_or_default();
            lock.upsert(agent_id, duduclaw_core::OrgEntry::new(v, &current.department))
                .map_err(|e| e.to_string())?;
            org_prior = Some(store.get(agent_id).cloned());
        }
        Ok(())
    }, stopping, lock_wait);
    drop(admission);
    if let Err(e) = written {
        // The mirror was not written: put the authority back (still inside
        // the org lock) so a failed request moves nobody. (On a lock-wait
        // timeout the guard never ran, so there is nothing to put back.)
        if let (Some(lock), Some(prior)) = (org_lock.as_ref(), org_prior.take())
            && let Err(re) = lock.restore(agent_id, prior)
        {
            tracing::warn!(agent = %agent_id, error = %re, "org.toml rollback failed on agent_update");
        }
        drop(org_lock);
        if e == AI_AUTHORITY_REFUSED {
            return serde_json::json!({
                "content":[{"type":"text", "text":AI_AUTHORITY_REFUSED}], "isError":true
            });
        }
        if duduclaw_gateway::agent_files::external::is_lock_wait_timeout(&e) {
            return org_locked_wait_timeout_error("agent.toml", &e);
        }
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error writing agent.toml: {e}")}],
            "isError": true
        });
    }
    drop(org_lock);

    if let Some((approval_id, user_id)) = &db_approval {
        duduclaw_security::audit::append_tool_call_with_extras(
            home_dir, caller, "db_source_grant_applied", "Admin approved database source grant applied", true,
            &[("target", serde_json::json!(agent_id)), ("added", serde_json::json!(db_sources_added)),
              ("approval_id", serde_json::json!(approval_id)), ("decider_user_id", serde_json::json!(user_id))],
        );
    }

    if config_before.agent.status != config.agent.status {
        duduclaw_gateway::dashboard_feedback::emit(home_dir, "agent.status_changed",
            serde_json::json!({ "agent_id": agent_id, "status": config.agent.status })).await;
    }

    // WP: sync SOUL.md / IDENTITY.md self-introduction text to the new
    // display_name (see comment above the capture site). Best-effort: a
    // missing file is skipped, an IO error is logged but does not fail the
    // already-committed agent.toml write.
    let mut soul_sync_changes: Vec<String> = Vec::new();
    if old_display_name != config.agent.display_name && !old_display_name.is_empty() {
        let new_display_name = &config.agent.display_name;
        use duduclaw_gateway::agent_files::external::{ExternalBaseline, ExternalFile, rename_is_safe, write_file};
        for (fname, file) in [("SOUL.md", ExternalFile::Soul), ("IDENTITY.md", ExternalFile::Identity)] {
            if !agent_dir.join(fname).exists() {
                continue; // file doesn't exist — nothing to sync
            }
            // Through the shared writer (lock, version, audit). The SOUL
            // baseline follows only when the file was the approved version
            // before the rename (a rename must not bless an out-of-band edit,
            // nor leave a legitimate one drifted), and the renamed text must
            // pass the fixed rules — nobody reviews an automatic edit.
            let synced = write_file(home_dir, agent_id, file, "rename", caller, ExternalBaseline::AcceptIfWasIntact, |current| {
                let renamed = duduclaw_core::rename_in_markdown(current, &old_display_name, new_display_name).0;
                rename_is_safe(current, &renamed).map_err(|cats| format!("not synced, the result needs a review ({cats})"))?;
                Ok(renamed)
            });
            match synced {
                Ok(o) if o.changed => {}
                Ok(_) => continue,
                Err(e) => {
                    tracing::warn!(agent_id, file = fname, error = %e, "New name not synced into a profile file");
                    changes.push(format!("{fname} not synced: {e}"));
                    continue;
                }
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

#[cfg(test)]
mod skill_budget_authority_tests {
    use super::*;

    #[test]
    fn ai_skill_budget_unlimited_enabling_is_refused_and_audited() {
        let tmp = tempfile::tempdir().unwrap();
        let before = toml::Table::new();
        let after: toml::Table = "[evolution]\nskill_budget_unlimited = true\n".parse().unwrap();
        for caller in ["rep", "lead"] {
            assert_eq!(check_ai_authority(
                tmp.path(), RecordActor::Agent(caller), "rep", &before, &after, None,
            ), Err(AI_AUTHORITY_REFUSED.into()));
        }
        let log = std::fs::read_to_string(tmp.path().join("tool_calls.jsonl")).unwrap();
        let rows: Vec<Value> = log.lines().map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|row| row["tool_name"] == "agent_authority_refused").collect();
        assert_eq!(rows.len(), 2);
        for (row, caller) in rows.iter().zip(["rep", "lead"]) {
            assert_eq!(row["caller"], caller);
            assert_eq!(row["target"], "rep");
            assert_eq!(row["field"], "evolution.skill_budget_unlimited");
            assert_eq!(row["keys"], serde_json::json!(["evolution.skill_budget_unlimited"]));
            assert_eq!(row["source"], "mcp.agent_update");
            assert_eq!(row["reason"], "authority_widening");
            assert_eq!(row["attempted_value_class"], "protected_value_change");
        }
    }

    #[test]
    fn ai_skill_budget_unlimited_disabling_is_allowed_without_refusal_audit() {
        let tmp = tempfile::tempdir().unwrap();
        let before: toml::Table = "[evolution]\nskill_budget_unlimited = true\n".parse().unwrap();
        let off: toml::Table = "[evolution]\nskill_budget_unlimited = false\n".parse().unwrap();
        let absent = toml::Table::new();
        // Seed a refusal so successful narrowing must leave the audit unchanged.
        assert!(check_ai_authority(
            tmp.path(), RecordActor::Agent("rep"), "rep", &absent, &before, None,
        ).is_err());
        let audit_path = tmp.path().join("tool_calls.jsonl");
        let audit_before = std::fs::read(&audit_path).unwrap();
        for caller in ["rep", "lead"] {
            for after in [&off, &absent] {
                assert_eq!(check_ai_authority(
                    tmp.path(), RecordActor::Agent(caller), "rep", &before, after, None,
                ), Ok(()));
                assert_eq!(std::fs::read(&audit_path).unwrap(), audit_before);
            }
        }
    }
}

#[cfg(test)]
mod db_source_grant_binding_tests {
    use super::*;

    #[test]
    fn approved_list_cannot_exempt_a_different_artifact() {
        let tmp = tempfile::tempdir().unwrap();
        let before: toml::Table = toml::from_str("[capabilities]\ndb_sources=[]\n").unwrap();
        let after: toml::Table = toml::from_str("[capabilities]\ndb_sources=['crm','hr']\n").unwrap();
        assert!(check_ai_authority(tmp.path(), RecordActor::Agent("lead"), "rep", &before, &after, Some(&["crm".into()])).is_err());
        assert!(check_ai_authority(tmp.path(), RecordActor::Agent("lead"), "rep", &before, &after, Some(&["crm".into(), "hr".into()])).is_ok());
    }
}
