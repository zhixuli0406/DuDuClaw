//! RM-143 P18: session-only settings, with acknowledged widening and durable CAS.
use super::{config_commit, MethodHandler, UserContext, UserDb, UserRole, WsFrame};
use crate::approval::chat_admin::{card, config, ChatAdminConfig, StepMethod, STEP_UP_KIND_KEYS};
use duduclaw_security::audit::{AuditEvent, Severity};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;

const LOGIN_REQUIRED: &str = "請使用管理員帳號登入儀表板後，再設定聊天核准。";
const UNREADABLE: &str = "目前無法讀取設定，請先修復設定檔再儲存。";
const UNAVAILABLE: &str = "目前無法讀取帳號資料，請重新整理後再試。";
const DEDICATED: &str = "請到安全設定的「在聊天軟體核准」區塊變更設定並確認風險。";

pub(super) fn minimum_role(method: &str) -> Option<UserRole> {
    match method {
        "approvals.chat_admin.get" | "approvals.chat_admin.set" => Some(UserRole::Admin),
        _ => None,
    }
}

fn require_session(ctx: &UserContext) -> Result<(), String> {
    if ctx.user_id.is_empty() || ctx.user_id == "system" || ctx.member_key_id.is_some()
        || ctx.role != UserRole::Admin || ctx.must_change_password {
        return Err(LOGIN_REQUIRED.into());
    }
    crate::google_personal::session_for(&ctx.user_id).map_err(|_| LOGIN_REQUIRED)?;
    Ok(())
}

fn require_live(db: &UserDb, ctx: &UserContext) -> Result<(), String> {
    let live = db.live_user_context(&ctx.user_id).map_err(|_| UNAVAILABLE)?
        .ok_or(LOGIN_REQUIRED)?;
    if live.role != UserRole::Admin || live.must_change_password
        || live.credential_epoch != ctx.credential_epoch {
        return Err(LOGIN_REQUIRED.into());
    }
    Ok(())
}

fn subtable(table: &toml::Table) -> Result<Option<&toml::Value>, String> {
    match table.get("approval") {
        None => Ok(None),
        Some(value) => Ok(value.as_table().ok_or(UNREADABLE)?.get("chat_admin")),
    }
}

pub(super) fn refuse_subtable_change(before: &toml::Table, after: &toml::Table) -> Result<(), String> {
    if subtable(before)? != subtable(after)? { return Err(DEDICATED.into()); }
    Ok(())
}

/// Generic updates are patches. Reject every supplied protected value which
/// differs, including null/deletion and flat dotted paths; unchanged echoes pass.
pub(super) fn refuse_generic_change(before: &toml::Table, params: &Value) -> Result<(), String> {
    fn visit<'a>(before: &toml::Table, value: &'a Value, path: Vec<&'a str>) -> Result<(), String> {
        if path.len() >= 2 && path[0] == "approval" && path[1] == "chat_admin" {
            let existing = config_commit::toml_at_json(before, &path.join("."));
            if value != &existing { return Err(DEDICATED.into()); }
            return Ok(());
        }
        if path.is_empty() || path == ["approval"] {
            if let Some(object) = value.as_object() {
                for (key, value) in object {
                    let mut child = path.clone();
                    child.extend(key.split('.'));
                    visit(before, value, child)?;
                }
            } else if path == ["approval"] && subtable(before)?.is_some() {
                return Err(DEDICATED.into());
            }
        }
        Ok(())
    }
    visit(before, params, Vec::new())
}

fn read(home: &Path) -> Result<(String, toml::Table, ChatAdminConfig), String> {
    // Missing files also fail closed, unlike the generic writer's empty-file fallback.
    let text = std::fs::read_to_string(home.join("config.toml")).map_err(|_| UNREADABLE)?;
    let table: toml::Table = text.parse().map_err(|_| UNREADABLE)?;
    let cfg = config::parse(subtable(&table)?).map_err(|_| UNREADABLE)?;
    Ok((text, table, cfg))
}

fn effective_enabled(cfg: &ChatAdminConfig, db: &UserDb) -> bool {
    cfg.enabled && cfg.enabled_ack.as_ref().is_some_and(|ack| {
        db.live_user_context(&ack.by.to_string()).ok().flatten()
            .is_some_and(|user| user.role == UserRole::Admin)
    })
}

/// Explicit projection: audit only settings from this schema, never raw config.
fn settings(cfg: &ChatAdminConfig) -> Value {
    json!({
        "enabled": cfg.enabled,
        "channels": cfg.channels.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
        "step_up_methods": cfg.step_up_methods.iter().map(|m| match m {
            StepMethod::Web => "web", StepMethod::DmTotp => "dm_totp",
        }).collect::<Vec<_>>(),
        "card_valid_hours": cfg.card_valid_hours,
        "step_up_exempt_kinds": cfg.step_up_exempt_kinds,
        "enabled_ack": cfg.enabled_ack.as_ref().map(|ack| json!({"by": ack.by, "at": ack.at.to_rfc3339()})),
    })
}

fn kind(key: &str) -> Value {
    use crate::trigger_route::texts::Lang;
    let impact = match key {
        "knowledge_quarantine" => "擱置的說法升為採用內容，影響該員工之後的回答",
        "knowledge_add_source" => "文件加入共享集合，能讀集合的員工都會引用",
        "expert_skills_enable" => "員工獲得新工具能力，啟用期間的行為不可逆",
        "workflow_activation" => "工作流取得長期自動行動權限",
        "agent_file_change" => "改寫 AI 員工人格或工作規則，影響之後所有對話",
        crate::admin_ai::card::ACTION_KIND => "一次套用多位 AI 員工的設定變更，影響之後的行為",
        "org_change" => "匯報線改變，連動委派範圍",
        "autopilot_action" => "執行一次凍結的自動化動作，多半不可逆",
        "cli_egress" | "egress_guard" => "讀過敏感資料後把內容送出系統，不可逆",
        _ => "請到儀表板確認影響",
    };
    json!({"key": key, "label": {
        "zh-TW": card::kind_label_for_key(key, Lang::ZhTw),
        "en": card::kind_label_for_key(key, Lang::En),
        "ja": card::kind_label_for_key(key, Lang::Ja),
    }, "impact": impact})
}

fn admin_status(home: &Path, db: &UserDb, agents: &[String]) -> Result<Vec<Value>, String> {
    let accounts: Vec<_> = ["telegram", "googlechat"].into_iter()
        .flat_map(|channel| crate::chat_link_intercept::configured_accounts(home, channel, agents)
            .unwrap_or_default()).collect();
    let mut admins = Vec::new();
    for user in db.list_users().map_err(|_| UNAVAILABLE)? {
        if user.role != UserRole::Admin { continue; }
        let active = db.live_user_context(&user.id).map_err(|_| UNAVAILABLE)?
            .is_some_and(|live| live.role == UserRole::Admin);
        let mut proven = Vec::new();
        if active {
            for proof in db.approval_proofs(&user.id).map_err(|_| UNAVAILABLE)? {
                if accounts.iter().any(|a| a.channel == proof.channel && a.account == proof.proof_account)
                    && !proven.contains(&proof.channel) { proven.push(proof.channel); }
            }
        }
        let has_totp = if active {
            db.mfa_factors(&user.id).map_err(|_| UNAVAILABLE)?["factors"]["totp"].as_bool()
                .ok_or(UNAVAILABLE)?
        } else { false };
        admins.push(json!({"user_id": user.id, "name": duduclaw_core::plain_line(&user.display_name, 200),
            "proven_channels": proven, "has_totp": has_totp}));
    }
    Ok(admins)
}

fn web_link_available(home: &Path) -> bool {
    crate::deep_link::configured_public_base(home).ok().flatten()
        .is_some_and(|url| crate::google_authorization_requests::url::external_base(&url))
}

fn get(home: &Path, db: &UserDb, agents: &[String]) -> Result<Value, String> {
    let snapshot = read(home);
    let readable = snapshot.is_ok();
    let (hash, table, cfg) = match snapshot {
        Ok((text, table, cfg)) => (Some(config_commit::content_hash(&text)), table, cfg),
        Err(_) => (None, toml::Table::new(), ChatAdminConfig::default()),
    };
    let mut result = settings(&cfg);
    result.as_object_mut().unwrap().remove("step_up_exempt_kinds");
    result["readable"] = json!(readable);
    result["hash"] = json!(hash);
    result["enabled"] = json!(effective_enabled(&cfg, db));
    result["enabled_ack"] = match &cfg.enabled_ack {
        Some(ack) => {
            let name = db.get_user(&ack.by.to_string()).map_err(|_| UNAVAILABLE)?
                .map(|u| duduclaw_core::plain_line(&u.display_name, 200)).unwrap_or_else(|| "已移除的成員".into());
            json!({"by_name": name, "at": ack.at.to_rfc3339()})
        }
        None => Value::Null,
    };
    result["kinds"] = json!(STEP_UP_KIND_KEYS.iter().map(|key| {
        let mut row = kind(key);
        row["tier"] = json!("T2");
        row["step_up_required"] = json!(!cfg.step_up_exempt_kinds.iter().any(|k| k.as_str() == *key));
        row
    }).collect::<Vec<_>>());
    result["t1_kinds"] = json!([kind("knowledge_quarantine"), kind("knowledge_add_source")]);
    result["admins"] = json!(admin_status(home, db, agents)?);
    result["web_link_available"] = json!(readable && web_link_available(home));
    result["unsupported_channels_ignored"] = json!(config_commit::toml_at(&table, "approval.chat_admin.channels")
        .and_then(toml::Value::as_array).is_some_and(|channels| channels.iter()
            .any(|c| c.as_str().is_some_and(|c| !["telegram", "googlechat"].contains(&c)))));
    Ok(result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetRequest {
    expected_hash: String,
    enabled: bool,
    channels: Vec<String>,
    step_up_methods: Vec<String>,
    card_valid_hours: u16,
    step_up_exempt_kinds: Vec<String>,
    acknowledge_risk: Option<bool>,
    acknowledge_exempt_risk: Option<Vec<String>>,
}

fn request(params: Value) -> Result<SetRequest, String> {
    if params.as_object().is_some_and(|p| !p.contains_key("expected_hash")) {
        return Err("請重新整理設定頁面後再儲存。".into());
    }
    // Optional means omitted, not null or a coerced type.
    if params.as_object().is_none_or(|p| p.values().any(Value::is_null)) {
        return Err("聊天核准設定格式不正確。".into());
    }
    serde_json::from_value(params).map_err(|_| "聊天核准設定包含不支援的欄位或格式。".into())
}

fn set(home: &Path, db: &UserDb, ctx: &UserContext, req: SetRequest,
    sweep: impl FnOnce()) -> Result<Value, String> {
    set_with_commit(home, db, ctx, req, sweep, |path, hash, render| {
        config_commit::commit_rendered_locked(path, hash, render)
    })
}

fn set_with_commit(home: &Path, db: &UserDb, ctx: &UserContext, req: SetRequest,
    sweep: impl FnOnce(),
    commit: impl FnOnce(&Path, &str, &mut dyn FnMut(&str) -> Result<String, String>) -> Result<(), String>,
) -> Result<Value, String> {
    require_live(db, ctx)?;
    let (text, mut table, before) = read(home)?;
    let hash = config_commit::content_hash(&text);
    if req.expected_hash != hash {
        return Err("設定已變更，請重新整理後再試。".into());
    }
    if req.channels.iter().any(|c| !["telegram", "googlechat"].contains(&c.as_str()))
        || (req.enabled && req.channels.is_empty()) {
        return Err("請選擇 Telegram 或 Google Chat。".into());
    }
    // An invalid/revoked acknowledgement also makes this a fresh enable.
    let enabling = req.enabled && !effective_enabled(&before, db);
    if enabling && req.acknowledge_risk != Some(true) {
        return Err("開啟聊天核准前，請確認已了解風險。".into());
    }
    let mut candidate = json!({"enabled": req.enabled, "channels": req.channels,
        "step_up_methods": req.step_up_methods, "card_valid_hours": req.card_valid_hours,
        "step_up_exempt_kinds": req.step_up_exempt_kinds});
    if req.enabled {
        candidate["enabled_ack"] = if enabling {
            json!({"by": ctx.user_id, "at": chrono::Utc::now().to_rfc3339()})
        } else { settings(&before)["enabled_ack"].clone() };
    }
    let value = toml::Value::try_from(&candidate).map_err(|_| "聊天核准設定格式不正確。")?;
    let after = config::parse(Some(&value)).map_err(|_| "聊天核准設定格式不正確。")?;
    let newly_exempted: Vec<_> = after.step_up_exempt_kinds.iter()
        .filter(|k| !before.step_up_exempt_kinds.contains(k)).cloned().collect();
    let newly_required: Vec<_> = before.step_up_exempt_kinds.iter()
        .filter(|k| !after.step_up_exempt_kinds.contains(k)).cloned().collect();
    let ack = req.acknowledge_exempt_risk.unwrap_or_default();
    if ack.len() != newly_exempted.len() || newly_exempted.iter().any(|k| !ack.contains(k)) {
        return Err("取消加強確認前，請逐項確認新增豁免的風險。".into());
    }
    config_commit::table_at_mut(&mut table, &["approval"])?.insert("chat_admin".into(), value);
    let mut event = AuditEvent::new("approval_chat_admin_settings_changed", "dashboard", Severity::Warning,
        json!({"phase": "intent", "change_id": uuid::Uuid::new_v4().to_string(),
            "by": ctx.user_id, "at": chrono::Utc::now().to_rfc3339(),
            "before": settings(&before), "after": settings(&after),
            "newly_exempted": newly_exempted, "newly_required": newly_required}));
    // Fail closed: durable audit intent must precede any config publication.
    duduclaw_security::audit::try_append_audit_event(home, &event)
        .map_err(|_| "稽核紀錄寫入失敗，設定未儲存，請聯絡管理員。")?;
    let mut saved_hash = String::new();
    let published = commit(&home.join("config.toml"), &hash, &mut |current| {
        require_live(db, ctx)?;
        // Check again under the commit lock. Never bless a missing file as empty.
        if !home.join("config.toml").is_file() { return Err(UNREADABLE.into()); }
        let rendered = config_commit::render_preserving(current, &table)?;
        let artifact: toml::Table = rendered.parse().map_err(|_| UNREADABLE)?;
        let parsed = config::parse(subtable(&artifact)?).map_err(|_| UNREADABLE)?;
        if parsed != after { return Err(UNREADABLE.into()); }
        saved_hash = config_commit::content_hash(&rendered);
        Ok(rendered)
    });
    let durability_unconfirmed = published.is_err();
    if let Err(error) = published {
        // Rename may have succeeded before a later durability check failed.
        // Reconcile the effective settings, not the proposed bytes or hash.
        if !read(home).is_ok_and(|(_, _, actual)| actual == after) {
            event.details["phase"] = json!("aborted");
            let _ = duduclaw_security::audit::try_append_audit_event(home, &event);
            return Err(error);
        }
    }
    // Schedule after confirmed publication, even if durability or auditing fails.
    if !after.enabled { sweep(); }
    duduclaw_security::audit::log_config_changed(home, &ctx.email, "admin",
        &["approval.chat_admin".into()]);
    crate::security_autopilot::emit_config_changed();
    event.details["phase"] = json!(if durability_unconfirmed { "committed_unconfirmed" } else { "committed" });
    let audit_result = duduclaw_security::audit::try_append_audit_event(home, &event);
    if !durability_unconfirmed {
        audit_result.as_ref().map_err(|_| "設定已儲存，但稽核紀錄寫入失敗，請重新整理並聯絡管理員。")?;
    }
    crate::security_autopilot::emit_security_autopilot_event(&event.severity, &event.event_type,
        Some(&event.agent_id), crate::security_autopilot::SOURCE_AUDIT);
    if durability_unconfirmed {
        if audit_result.is_err() {
            return Err("設定可能已經生效，但無法確認已持久寫入，而且這次變更的稽核紀錄也沒有寫進去。請重新整理確認設定，並檢查安全紀錄。".into());
        }
        return Err("設定已儲存，但無法確認資料已持久寫入，請重新整理確認設定。".into());
    }
    Ok(json!({"success": true, "hash": saved_hash}))
}

/// Blocking status projection: does not expose identities to security.status.
pub(super) fn status(home: &Path, db: Option<&UserDb>, agents: &[String]) -> Value {
    let cfg = read(home).ok().map(|(_, _, cfg)| cfg);
    let enabled = db.zip(cfg.as_ref()).is_some_and(|(db, cfg)| effective_enabled(cfg, db));
    let admins = db.and_then(|db| admin_status(home, db, agents).ok());
    let proven = admins.as_ref().map(|admins| admins.iter()
        .filter(|a| a["proven_channels"].as_array().is_some_and(|a| !a.is_empty())).count());
    let web = cfg.is_some() && web_link_available(home);
    let count = proven.map(|n| format!("{n} 位管理員已完成本人連結"))
        .unwrap_or_else(|| "目前無法確認管理員連結狀態".into());
    json!({"enabled": enabled, "readable": cfg.is_some(), "proven_admins": proven,
        "web_link_available": web, "message": format!("聊天核准{}；{}；網頁連結{}。",
            if enabled { "已開啟" } else { "已關閉" }, count, if web { "可用" } else { "不可用" })})
}

impl MethodHandler {
    pub(crate) async fn handle_chat_admin_settings_rpc(&self, method: &str, params: Value, ctx: &UserContext) -> WsFrame {
        if let Err(error) = require_session(ctx) { return WsFrame::error_response("", &error); }
        let req = match method {
            "approvals.chat_admin.get" if params.as_object().is_some_and(|p| p.is_empty()) => None,
            "approvals.chat_admin.set" => match request(params) {
                Ok(req) => Some(req), Err(error) => return WsFrame::error_response("", &error),
            },
            _ => return WsFrame::error_response("", "不支援此聊天核准操作或欄位。"),
        };
        let Some(db) = self.user_db.read().await.clone() else { return WsFrame::error_response("", UNAVAILABLE); };
        let agents = self.registry.read().await.list().iter().map(|a| a.config.agent.name.clone()).collect::<Vec<_>>();
        let home = self.home_dir.clone();
        let ctx = ctx.clone();
        let runtime = tokio::runtime::Handle::current();
        let result = tokio::task::spawn_blocking(move || {
            require_live(&db, &ctx)?;
            match req {
                None => get(&home, &db, &agents),
                Some(req) => set(&home, &db, &ctx, req, || {
                    let home = home.clone();
                    runtime.spawn(async move { crate::approval::chat_admin::sync::sweep_feature_off(&home).await; });
                }),
            }
        }).await;
        match result {
            Ok(Ok(value)) => WsFrame::ok_response("", value),
            Ok(Err(error)) => WsFrame::error_response("", &error),
            Err(_) => WsFrame::error_response("", UNAVAILABLE),
        }
    }
}

#[cfg(test)]
#[path = "chat_admin_settings_rpc_tests.rs"]
mod tests;
