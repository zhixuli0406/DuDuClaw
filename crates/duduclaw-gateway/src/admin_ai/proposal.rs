//! Read-only proposal preparation. Only the approval card is persisted here.
use std::{collections::{BTreeMap, BTreeSet}, path::Path};
use duduclaw_agent::{registry::AgentRegistry, skill_state};
use duduclaw_core::{admin_ai_settings::{AdminAiField, AdminAiRefusal, Bounds, Item, Proposal, Schedule},
    authority_coordinator::AuthorityCoordinator};
use serde_json::{json, Value};
use crate::{approval::{payload_hash, ConversationAudience}, on_behalf_of::admin_turn::{admin_turn, AdminTurn, AdminTurnLevel}};
use super::route;

pub(crate) type Result<T> = std::result::Result<T, AdminAiRefusal>;
pub(super) fn refused(code: &'static str) -> AdminAiRefusal {
    AdminAiRefusal { code, message: match code {
        "admin_ai_pending_limit" => "已有三張待確認卡，請先到收件匣處理。",
        "admin_ai_hourly_limit" => "一小時內已提出二十張確認卡，請稍後再試。",
        "admin_ai_model_refused" => "這個模型無法由員工目前的執行引擎執行，請在儀表板設定。",
        "admin_ai_duplicate_trigger" => "呼叫名稱與其他員工或同批變更重複。",
        "admin_ai_schedule_refused" => "只能修改沒有工作流綁定的純提示型時間排程。",
        "admin_ai_prompt_blocked" => "排程名稱或提示未通過內容檢查。",
        "admin_ai_target_unavailable" => "目標員工不在可修改範圍內。",
        "admin_ai_duplicate_item" => "同一項設定在這批變更中出現多次，請合併後重試。",
        "admin_ai_state_changed" => "設定或身分已變動，請重新讀取後再提出。",
        _ => "無法確認這批設定變更，請重新讀取或到儀表板處理。",
    }.into() }
}
fn failure<T>(_: T) -> AdminAiRefusal { refused("admin_ai_settings_unavailable") }
pub(crate) fn hash_text(text: &str) -> String { crate::agent_files::hash_of(text) }

// Shared by proposal, execution and the checks inside the committing locks.
pub(crate) const STALE: &str = "admin_ai_state_changed";
pub(crate) fn hash_schedule(row: &crate::cron_store::CronTaskRow) -> Result<String> {
    Ok(payload_hash(&serde_json::to_value(row).map_err(failure)?))
}
pub(crate) fn hash_skills(state: &skill_state::DisabledSkills) -> Result<String> {
    if state.error().is_some() { return Err(refused(STALE)); }
    Ok(payload_hash(&json!(state.names())))
}
fn canonical_trigger(trigger: &str) -> String { trigger.trim().to_lowercase() }
pub(crate) fn trigger_namespace(home: &Path) -> Result<BTreeMap<String, String>> {
    let _authority = AuthorityCoordinator::new(home).read().map_err(failure)?;
    trigger_namespace_locked(home)
}

// The caller holds a shared or exclusive authority lease.
fn trigger_namespace_locked(home: &Path) -> Result<BTreeMap<String, String>> {
    let mut triggers = BTreeMap::new();
    for entry in std::fs::read_dir(home.join("agents")).map_err(failure)? {
        let entry = entry.map_err(failure)?;
        let dir = entry.path();
        let name = entry.file_name();
        let name = name.to_str().ok_or_else(|| refused(STALE))?;
        if duduclaw_core::is_internal_agent_dir(name) { continue; }
        if !std::fs::metadata(&dir).map_err(failure)?.is_dir() { continue; }
        match std::fs::symlink_metadata(dir.join("agent.toml")) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(failure(e)),
            Ok(_) => {},
        }
        // preset::sanitize_preset_table removes the entire [agent] table before
        // resolve_for_agent merges it: name/display_name/trigger/status are raw
        // identity, never preset overrides. A resolved artifact can lag a commit.
        let table: toml::Value = std::fs::read_to_string(dir.join("agent.toml"))
            .map_err(failure)?.parse().map_err(failure)?;
        if string_leaf(&table, AdminAiField::Status)? == "terminated" { continue; }
        let id = table.get("agent").and_then(|a| a.get("name")).and_then(toml::Value::as_str)
            .ok_or_else(|| refused(STALE))?;
        let trigger = canonical_trigger(&string_leaf(&table, AdminAiField::Trigger)?);
        if triggers.insert(id.to_owned(), trigger).is_some() { return Err(refused(STALE)); }
    }
    Ok(triggers)
}
pub(crate) fn hash_triggers(triggers: &BTreeMap<String, String>) -> String {
    // BTreeMap binds both ownership and canonical names in stable order.
    payload_hash(&json!(triggers))
}
pub(crate) fn check_live_trigger(home: &Path, id: &str, table: &toml::Table) -> Result<()> {
    let trigger = table.get("agent").and_then(|a| a.get("trigger")).and_then(toml::Value::as_str)
        .ok_or_else(|| refused(STALE))?;
    check_trigger(id, trigger, &trigger_namespace_locked(home)?, &mut BTreeMap::new())
}

pub(super) struct Prepared {
    pub items: Vec<Value>,
    pub target_hashes: BTreeMap<String, String>,
    pub environment_hash: String,
    pub employee_count: usize,
    pub reason: String,
    pub admin_name: String,
    pub avatar_name: String,
}

pub(super) fn environment(home: &Path, turn: &AdminTurn) -> Result<(String, Bounds, String)> {
    let config: toml::Table = std::fs::read_to_string(home.join("config.toml")).map_err(failure)?
        .parse().map_err(failure)?;
    if !duduclaw_core::admin_ai_settings::enabled_from_table(&config) {
        return Err(refused("admin_ai_disabled"));
    }
    let timeout = crate::runtime_config::TimeoutPolicy::from_table(&config).map_err(failure)?;
    let turns = duduclaw_core::turn_limits::TurnPolicy::validate(&config).map_err(failure)?;
    let db = rusqlite::Connection::open_with_flags(home.join("users.db"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(failure)?;
    let (account, name) = db.query_row(
        "SELECT role,status,must_change_password,avatar_agent_id,email,display_name FROM users WHERE id=?1",
        [&turn.user_id], |row| Ok((json!({"role": row.get::<_, String>(0)?, "status": row.get::<_, String>(1)?,
            "must_change_password": row.get::<_, bool>(2)?, "avatar": row.get::<_, Option<String>>(3)?,
            "email": row.get::<_, String>(4)?}), row.get::<_, String>(5)?))).map_err(failure)?;
    if account["role"] != "admin" || account["status"] != "active"
        || account["must_change_password"] != false || account["avatar"] != turn.agent {
        return Err(refused("admin_ai_state_changed"));
    }
    let epoch = AuthorityCoordinator::new(home).epoch().map_err(failure)?;
    let hash = payload_hash(&json!({"admin_ai": config.get("admin_ai"),
        "runtime_limits": {"default_timeout_secs": timeout.default_secs, "max_timeout_secs": timeout.max_secs,
            "idle_timeout_min": 60, "idle_timeout_max": timeout.max_secs, "idle_timeout_allow_zero": true,
            "default_idle_timeout_secs": crate::runtime_config::DEFAULT_IDLE_TIMEOUT_SECS,
            "configured_idle_timeout_secs": config.get("runtime").and_then(|r| r.get("idle_timeout_secs"))},
        "turn_policy": {"default": turns.default, "limit": turns.limit}, "admin_account": account,
        "authority_epoch": epoch}));
    Ok((hash, Bounds { timeout_max: timeout.max_secs, turn_limit: turns.limit,
        idle_timeout_min: 60, idle_timeout_max: timeout.max_secs, idle_timeout_allow_zero: true }, name))
}

fn leaf(table: &toml::Value, field: AdminAiField) -> Result<Value> {
    let value = field.toml_path().split('.').try_fold(table, |v, key| v.get(key))
        .map(serde_json::to_value).transpose().map_err(failure)?.unwrap_or(Value::Null);
    if value.is_array() || value.is_object() { return Err(refused("admin_ai_settings_unavailable")); }
    Ok(value)
}
fn string_leaf(table: &toml::Value, field: AdminAiField) -> Result<String> {
    Ok(leaf(table, field)?.as_str().unwrap_or("").to_owned())
}
fn check_trigger(id: &str, trigger: &str, existing: &BTreeMap<String, String>, proposed: &mut BTreeMap<String, String>) -> Result<()> {
    let key = canonical_trigger(trigger);
    if existing.iter().any(|(other, value)| other != id && canonical_trigger(value) == key)
        || proposed.iter().any(|(other, value)| other != id && canonical_trigger(value) == key) {
        return Err(refused("admin_ai_duplicate_trigger"));
    }
    proposed.insert(id.to_owned(), trigger.to_owned());
    Ok(())
}
fn scan_schedule(home: &Path, agent: &str, name: &str, task: &str) -> Result<()> {
    let prompt = format!("[Scheduled Task: {name}] {task}");
    if duduclaw_security::input_guard::scan_input_with_audit(&prompt,
        duduclaw_security::input_guard::DEFAULT_BLOCK_THRESHOLD, home, agent).blocked {
        return Err(refused("admin_ai_prompt_blocked"));
    }
    Ok(())
}
fn schedule_values(row: &crate::cron_store::CronTaskRow) -> Value {
    json!({"name": row.name, "cron": row.cron, "timezone": row.cron_timezone,
        "task": row.task, "enabled": row.enabled})
}
fn check_model(agent: &str, model: &str, provider: duduclaw_core::types::RuntimeType, models: &[Value]) -> Result<()> {
    if !crate::runtime_config::model_matches_provider(model, provider)
        || crate::runtime_config::runnable_model(agent, model, provider).as_ref() != model
        || !models.iter().any(|row| row["id"].as_str() == Some(model)) {
        return Err(refused("admin_ai_model_refused"));
    }
    Ok(())
}

pub(super) async fn prepare(home: &Path, turn: &AdminTurn, proposal: Proposal) -> Result<Prepared> {
    if turn.audience != ConversationAudience::Private { return Err(refused("admin_ai_private_audience_required")); }
    let (environment_hash, bounds, admin_name) = environment(home, turn)?;
    let normalized = proposal.validate(&bounds)?;
    let user = route::fresh_user(home, &turn.user_id).map_err(refused)?;
    let mut registry = AgentRegistry::new(home.join("agents"));
    registry.scan().await.map_err(failure)?;
    let store = crate::cron_store::CronStore::open(home).map_err(failure)?;
    let mut schedules = BTreeMap::new();
    let mut owners = BTreeMap::new();
    for item in &normalized {
        if let Some(id) = item.as_item().schedule_id() {
            let row = store.get(id).await.map_err(failure)?.ok_or_else(|| refused("admin_ai_schedule_refused"))?;
            if !route::prompt_only(&row) || store.workflow_binding(id).await.map_err(failure)?.is_some() {
                return Err(refused("admin_ai_schedule_refused"));
            }
            owners.insert(id.to_owned(), row.agent_id.clone());
            schedules.insert(id.to_owned(), row);
        }
    }
    proposal.validate_resolved_agents(&owners)?;
    let ids: BTreeSet<String> = normalized.iter().map(|item| item.as_item().agent_id().map(str::to_owned)
        .or_else(|| item.as_item().schedule_id().and_then(|id| owners.get(id).cloned()))
        .ok_or_else(|| refused("admin_ai_target_unavailable"))).collect::<Result<_>>()?;
    let mut tables = BTreeMap::new();
    let mut all_hashes = BTreeMap::new();
    for id in &ids {
        let agent = registry.get(id).ok_or_else(|| refused("admin_ai_target_unavailable"))?;
        let text = route::effective_text(&agent.dir).map_err(refused)?;
        let table: toml::Value = text.parse().map_err(failure)?;
        if !route::target_allowed(home, &user, id, &agent.dir, &table) { return Err(refused("admin_ai_target_unavailable")); }
        all_hashes.insert(format!("agent:{id}"), hash_text(&text));
        let raw = std::fs::read_to_string(agent.dir.join("agent.toml")).map_err(failure)?;
        all_hashes.insert(format!("agent_raw:{id}"), hash_text(&raw));
        tables.insert(id.clone(), table);
    }
    let models = crate::runtime_models::merged_models(&crate::runtime_models::load_cache(home).await.unwrap_or_default());
    let policy = crate::product_policy::load(home).map_err(failure)?;
    let models: Vec<Value> = models.into_iter().filter(|row| row["provider"].as_str().is_some_and(|provider|
        policy.require_runtime(provider).is_ok() && policy.require_provider(provider).is_ok())).collect();
    let changes_names = normalized.iter().any(|i| matches!(i.as_item(),
        Item::AgentField { field: AdminAiField::Trigger | AdminAiField::DisplayName, .. }));
    let existing_triggers = if changes_names { trigger_namespace(home)? } else { BTreeMap::new() };
    let explicit_triggers: BTreeMap<_, _> = normalized.iter().filter_map(|i| match i.as_item() {
        Item::AgentField { agent_id, field: AdminAiField::Trigger, value } => Some((agent_id.clone(), value.clone())), _ => None,
    }).collect();
    let mut proposed_triggers = BTreeMap::new();
    let mut seen = BTreeSet::new();
    let mut items = Vec::new();
    let mut target_hashes = BTreeMap::new();
    let mut changed_agents = BTreeSet::new();
    for normalized in normalized {
        let item = normalized.into_item();
        let id = item.agent_id().map(str::to_owned).or_else(|| item.schedule_id().and_then(|id| owners.get(id).cloned()))
            .ok_or_else(|| refused("admin_ai_target_unavailable"))?;
        let table = &tables[&id];
        let dir = home.join("agents").join(&id);
        let name = string_leaf(table, AdminAiField::DisplayName)?;
        let employee = if name.is_empty() { id.clone() } else { name };
        let (key, label, before, after) = match &item {
            Item::AgentField { field, value, .. } => {
                let before = leaf(table, *field)?;
                match field {
                    AdminAiField::DisplayName => {
                        crate::agent_files::external::rename_is_safe("", value.as_str().unwrap_or("")).map_err(failure)?;
                        if !explicit_triggers.contains_key(&id) {
                            if let Some(trigger) = duduclaw_core::synced_trigger(&string_leaf(table, AdminAiField::Trigger)?,
                                before.as_str().unwrap_or(""), value.as_str().unwrap_or("")) {
                                let derived = Item::AgentField { agent_id: id.clone(), field: AdminAiField::Trigger, value: json!(trigger) };
                                duduclaw_core::admin_ai_settings::validate_item(&derived, &bounds)?;
                                check_trigger(&id, &trigger, &existing_triggers, &mut proposed_triggers)?;
                                // The rename handler synchronizes this trigger; show and bind that effect too.
                                if before != *value {
                                    items.push(json!({"seq": items.len() + 1, "target": id, "employee": employee,
                                        "label": AdminAiField::Trigger.label("zh-TW"), "before": leaf(table, AdminAiField::Trigger)?,
                                        "after": trigger, "item": derived, "derived": true}));
                                }
                            }
                        }
                    }
                    AdminAiField::Trigger => check_trigger(&id, value.as_str().unwrap_or(""), &existing_triggers, &mut proposed_triggers)?,
                    AdminAiField::Status if !matches!(before.as_str(), Some("active" | "paused")) && !before.is_null() =>
                        return Err(refused("admin_ai_status_refused")),
                    AdminAiField::Model | AdminAiField::FallbackModel => {
                        let model = value.as_str().ok_or_else(|| refused("admin_ai_model_refused"))?;
                        if !model.is_empty() {
                            let runtime = crate::runtime_config::load_runtime_settings_strict(&dir).map_err(failure)?;
                            check_model(&id, model, runtime.provider, &models)?;
                        }
                    }
                    AdminAiField::IdleTimeoutSecs => {
                        crate::runtime_config::validate_idle_timeout(value.as_i64().ok_or_else(|| refused("invalid_value"))?, bounds.timeout_max).map_err(failure)?;
                    }
                    _ => {},
                }
                (format!("field:{id}:{}", field.tool_name()), field.label("zh-TW").to_owned(), before, value.clone())
            }
            Item::Skill { skill, enabled, .. } => {
                let skills = route::skills(home, &id, &dir, false).await.map_err(refused)?;
                let installed = skills.as_array().and_then(|rows| rows.iter().find(|row|
                    row["name"].as_str().is_some_and(|name| name.eq_ignore_ascii_case(skill))))
                    .ok_or_else(|| refused("admin_ai_skill_not_installed"))?;
                let state = skill_state::read_disabled(home, &id);
                if state.error().is_some() { return Err(refused("admin_ai_skills_unavailable")); }
                all_hashes.insert(format!("skills:{id}"), hash_skills(&state)?);
                (format!("skill:{id}:{}", skill.to_ascii_lowercase()), format!("技能「{skill}」"), installed["enabled"].clone(), json!(enabled))
            }
            Item::Schedule(Schedule::Create { name, cron, timezone, task, .. }) => {
                scan_schedule(home, &id, name, task)?;
                (format!("create:{}", items.len()), "新增排程".into(), Value::Null,
                    json!({"name": name, "cron": cron, "timezone": timezone, "task": task, "enabled": true}))
            }
            Item::Schedule(op) => {
                let schedule_id = item.schedule_id().ok_or_else(|| refused("admin_ai_schedule_refused"))?;
                let row = &schedules[schedule_id];
                let before = schedule_values(row);
                let mut after = before.clone();
                let label = match op {
                    Schedule::Update { name, cron, timezone, task, .. } => {
                        if let Some(v) = name { after["name"] = json!(v); }
                        if let Some(v) = cron { after["cron"] = json!(v); }
                        if let Some(v) = timezone { after["timezone"] = json!(v); }
                        if let Some(v) = task { after["task"] = json!(v); }
                        scan_schedule(home, &id, after["name"].as_str().unwrap_or(""), after["task"].as_str().unwrap_or(""))?;
                        "修改排程"
                    }
                    Schedule::Pause { .. } => { after["enabled"] = json!(false); "暫停排程" }
                    Schedule::Resume { .. } => { after["enabled"] = json!(true); "恢復排程" }
                    Schedule::Delete { .. } => { after = Value::Null; "刪除排程" }
                    Schedule::Create { .. } => unreachable!(),
                };
                all_hashes.insert(format!("schedule:{schedule_id}"), hash_schedule(row)?);
                (format!("schedule:{schedule_id}"), label.into(), before, after)
            }
        };
        if !seen.insert(key) { return Err(refused("admin_ai_duplicate_item")); }
        if before == after { continue; }
        changed_agents.insert(id.clone());
        target_hashes.insert(format!("agent:{id}"), all_hashes[&format!("agent:{id}")].clone());
        target_hashes.insert(format!("agent_raw:{id}"), all_hashes[&format!("agent_raw:{id}")].clone());
        let extra = match &item {
            Item::Skill { .. } => Some(format!("skills:{id}")),
            _ => item.schedule_id().map(|id| format!("schedule:{id}")),
        };
        if let Some(key) = extra { target_hashes.insert(key.clone(), all_hashes[&key].clone()); }
        let mut entry = json!({"seq": items.len() + 1, "target": id, "employee": employee,
            "label": label, "before": before, "after": after, "item": item});
        if matches!(&item, Item::AgentField { field: AdminAiField::DisplayName, .. }) {
            if let Some(trigger) = explicit_triggers.get(&id) {
                // Even an unchanged explicit trigger suppresses rename auto-sync.
                // P7 must pass this override with the grouped employee update.
                entry["trigger_override"] = trigger.clone();
            }
        }
        items.push(entry);
    }
    if items.iter().any(|v| matches!(v["item"]["field"].as_str(), Some("trigger" | "display_name"))) {
        target_hashes.insert("triggers".into(), hash_triggers(&existing_triggers));
    }
    // All field updates invoke the same handler, which aligns even an unchanged
    // preferred model. Refuse an already mismatched model before filing any card.
    for id in &changed_agents {
        if items.iter().any(|v| v["target"] == *id && v["item"]["kind"] == "agent_field") {
            let runtime = crate::runtime_config::load_runtime_settings_strict(&home.join("agents").join(id)).map_err(failure)?;
            let preferred = items.iter().find(|v| v["target"] == *id && v["item"]["field"] == "model")
                .and_then(|v| v["after"].as_str()).map(str::to_owned)
                .unwrap_or(string_leaf(&tables[id], AdminAiField::Model)?);
            if !preferred.is_empty() && !crate::runtime_config::model_matches_provider(&preferred, runtime.provider) {
                return Err(refused("admin_ai_model_refused"));
            }
        }
    }
    if items.len() > duduclaw_core::admin_ai_settings::MAX_ITEMS {
        return Err(refused("item_limit"));
    }
    // Do not present a mixed snapshot after asynchronous skill/schedule reads.
    let fresh_user = route::fresh_user(home, &turn.user_id).map_err(refused)?;
    for id in &changed_agents {
        let dir = home.join("agents").join(id);
        let text = route::effective_text(&dir).map_err(refused)?;
        let table = text.parse::<toml::Value>().map_err(failure)?;
        if !route::target_allowed(home, &fresh_user, id, &dir, &table)
            || target_hashes.get(&format!("agent:{id}")) != Some(&hash_text(&text)) {
            return Err(refused("admin_ai_state_changed"));
        }
    }
    for (key, expected) in &target_hashes {
        if key == "triggers" {
            if hash_triggers(&trigger_namespace(home)?) != *expected { return Err(refused(STALE)); }
            continue;
        }
        let (kind, id) = key.split_once(':').ok_or_else(|| refused("admin_ai_state_changed"))?;
        let actual = match kind {
            "agent" => continue,
            "agent_raw" => hash_text(&std::fs::read_to_string(home.join("agents").join(id).join("agent.toml"))
                .map_err(failure)?),
            "schedule" => {
                let row = store.get(id).await.map_err(failure)?.ok_or_else(|| refused("admin_ai_state_changed"))?;
                if !route::prompt_only(&row) || store.workflow_binding(id).await.map_err(failure)?.is_some() {
                    return Err(refused("admin_ai_state_changed"));
                }
                hash_schedule(&row)?
            }
            "skills" => {
                let state = skill_state::read_disabled(home, id);
                if state.error().is_some() { return Err(refused("admin_ai_skills_unavailable")); }
                hash_skills(&state)?
            }
            _ => return Err(refused("admin_ai_state_changed")),
        };
        if &actual != expected { return Err(refused("admin_ai_state_changed")); }
    }
    let avatar = registry.get(&turn.agent).ok_or_else(|| refused("admin_ai_target_unavailable"))?;
    let avatar_name = string_leaf(&route::effective(&avatar.dir).map_err(refused)?, AdminAiField::DisplayName)?;
    if environment(home, turn)?.0 != environment_hash { return Err(refused("admin_ai_state_changed")); }
    Ok(Prepared { items, target_hashes, environment_hash, employee_count: changed_agents.len(),
        reason: proposal.reason, admin_name, avatar_name })
}

pub(super) async fn propose(home: &Path, turn: &AdminTurn, context: &str, proposal: Proposal, executor: Option<super::execute::Executor>) -> Result<Value> {
    let prepared = prepare(home, turn, proposal).await?;
    let fresh = admin_turn(home, &turn.agent, context, AdminTurnLevel::Write).map_err(|e| refused(e.code()))?;
    if fresh.user_id != turn.user_id || fresh.turn_id != turn.turn_id || fresh.session_id != turn.session_id
        || fresh.decision_context != turn.decision_context || fresh.audience != turn.audience {
        return Err(refused("admin_ai_state_changed"));
    }
    if prepared.items.is_empty() { return Ok(json!({"message": "沒有需要變更的內容", "item_count": 0})); }
    super::card::create(home, &fresh, prepared, executor).await
}
