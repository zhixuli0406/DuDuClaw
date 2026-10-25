//! Per-employee Google identity (B2a) — dashboard RPCs.
//!
//! | method | who | what |
//! |---|---|---|
//! | `google.delegation.get` | Admin | protection state, sealed key info (no secret), domain settings, every employee's identity |
//! | `google.delegation.import_key {key_json}` | Admin | validate + seal a service-account key with the protected key |
//! | `google.delegation.remove_key {confirm}` | Admin | delete the sealed key |
//! | `google.delegation.settings_set {allowed_subject_domains?, default_identity?}` | Admin | `[integrations.google_identity]` |
//! | `agents.google_identity_get {agent_id}` | Viewer of the employee | the identity in effect and whether it can be used now |
//! | `agents.google_identity_set {agent_id, identity, fallback?, requesters?}` | Admin | write `[google]` (B2a: `home`, `none`, `delegate:<email>`) |
//! | `google.identity.test {agent_id}` | Admin | mint for the subject and read one profile; sends nothing |
//!
//! No response ever carries key material or an access token.

#[allow(unused_imports)]
use super::*;

use duduclaw_core::google_identity::{self as gi, Effective, Requesters, Selector};
use duduclaw_security::audit::{AuditEvent, Severity};

use crate::google_identity::{delegation_key, requester_gate, resolve};

const ADMIN_CONSOLE_PATH: &str =
    "Google 管理控制台 → 安全性 → 存取權與資料控管 → API 控制項 → 管理全網域委派";

fn sec_audit(home: &Path, event: &str, severity: Severity, ctx: &UserContext, details: Value) {
    let mut d = details;
    if let Some(obj) = d.as_object_mut() {
        obj.insert("actor_user_id".into(), json!(ctx.user_id));
        obj.insert("actor_email".into(), json!(ctx.email));
    }
    crate::security_autopilot::audit_and_emit(
        home,
        &AuditEvent::new(event, "dashboard", severity, d),
    );
}

/// The identity of one employee as the dashboard shows it, plus whether it
/// can be used right now (local checks only — no network, no unsealing).
fn identity_view(home: &Path, agent: &str, people: &dyn requester_gate::People) -> Value {
    let section = gi::read_agent_section(home, agent);
    let eff = gi::combine(&section, &gi::read_home_config(home));
    let (state, message) = match &eff.effective {
        Effective::Home => ("home", String::new()),
        Effective::Disabled => ("none", String::new()),
        Effective::Delegate { email } => match resolve::precheck(home, email, people) {
            Err(r) => ("blocked", r.message_zh),
            Ok(()) => match delegation_key::info(home) {
                Ok(Some(_)) => ("ready", String::new()),
                Ok(None) => ("blocked", "公司授權代表還沒匯入服務帳號金鑰。".into()),
                Err(e) => ("blocked", e),
            },
        },
        Effective::Personal { selector } => match people.user_db() {
            None => ("blocked", "人員資料尚未就緒，請聯絡管理員。".into()),
            Some(db) => match crate::google_personal::access::select_user(db, agent, selector) {
                Err(r) => (r.code, r.message_zh),
                Ok(user) if user.status != duduclaw_auth::UserStatus::Active => ("person_inactive", "負責人目前不是有效帳號。".into()),
                Ok(user) => match db.google_consent(&user.id, agent) {
                    Ok(Some(false)) => ("consent_declined", "帳號本人已關閉這位員工的使用權。".into()),
                    Ok(None) => ("awaiting_consent", "等待帳號本人到「我的連線」允許。".into()),
                    Err(_) => ("blocked", "本人同意紀錄無法讀取。".into()),
                    Ok(Some(true)) => match crate::person_credentials::get(home, &user.id) {
                        Ok(Some(record)) if record.metadata.status == crate::person_credentials::Status::Connected => ("configured", "已保存個人連線；實際使用仍會檢查有效狀態與可用服務。".into()),
                        Ok(Some(_)) => ("needs_reconnect", "請本人到「我的連線」重新連結。".into()),
                        Ok(None) => ("not_connected", "本人尚未連結 Google。".into()),
                        Err(e) => ("blocked", e),
                    }
                },
            }
        },
        other => (
            "blocked",
            resolve::refusal_for(other)
                .map(|r| r.message_zh)
                .unwrap_or_default(),
        ),
    };
    let email = match &eff.effective {
        Effective::Delegate { email } => Some(email.clone()),
        _ => None,
    };
    let selected_person = match (&eff.effective, people.user_db()) {
        (Effective::Personal { selector }, Some(db)) => crate::google_personal::access::select_user(db, agent, selector).ok().map(|u| json!({"user_id":u.id,"name":u.display_name,"login_email":u.email})),
        _ => None,
    };
    let owners = people.user_db().and_then(|db| db.get_agent_users(agent).ok().map(|bindings| bindings.into_iter()
        .filter(|b| b.access_level == AccessLevel::Owner).filter_map(|b| db.get_user(&b.user_id).ok().flatten().map(|u| json!({"user_id":u.id,"name":u.display_name,"email":u.email,"is_primary":b.is_primary}))).collect::<Vec<_>>()));
    let bound_people = people.user_db().and_then(|db| db.get_agent_users(agent).ok().map(|bindings| bindings.into_iter()
        .filter_map(|b| db.get_user(&b.user_id).ok().flatten()).filter(|u| u.status == duduclaw_auth::UserStatus::Active)
        .map(|u| json!({"user_id":u.id,"name":u.display_name,"email":u.email})).collect::<Vec<_>>()));
    let allowed_channels = std::fs::read_to_string(home.join("agents").join(agent).join("agent.toml")).ok()
        .and_then(|s| s.parse::<toml::Table>().ok()).and_then(|t| t.get("permissions").and_then(toml::Value::as_table).and_then(|p| p.get("allowed_channels")).cloned());
    json!({
        "agent_id": agent,
        "kind": eff.effective.kind(),
        "email": email,
        "selected_person": selected_person,
        "owners": owners,
        "bound_people": bound_people,
        "allowed_channels": allowed_channels,
        "identity": match &eff.effective { Effective::Personal { selector } => Some(selector.to_config_value()), _ => None },
        "explicit": eff.explicit,
        "fallback": eff.fallback.as_str(),
        "requesters": eff.requesters.as_str(),
        "state": state,
        "message": message,
    })
}

impl MethodHandler {
    async fn people(&self) -> Box<dyn requester_gate::People> {
        let users = self.user_db.read().await.clone();
        requester_gate::people_for(&self.home_dir, users.as_ref())
    }

    /// `google.delegation.get`.
    pub(crate) async fn handle_google_delegation_get(&self) -> WsFrame {
        let home = self.home_dir.clone();
        let protection = tokio::task::spawn_blocking({
            let home = home.clone();
            move || crate::handlers::credential_protection_rpc::protection_for(&home).status()
        })
        .await;
        let protection = match protection {
            Ok(s) => crate::handlers::credential_protection_rpc::status_json(&s),
            Err(e) => return WsFrame::error_response("", &format!("憑證保護狀態讀取中斷：{e}")),
        };
        let (key, key_error) = match delegation_key::info(&home) {
            Ok(k) => (k, None),
            Err(e) => (None, Some(e)),
        };
        let home_cfg = gi::read_home_config(&home);
        let people = self.people().await;
        if let Some(db) = people.user_db() {
            if let Ok(records) = crate::person_credentials::all_metadata(&home) {
                for record in records {
                    if let Ok(user) = db.get_user(&record.owner_id) {
                        let _ = crate::person_credentials::sync_user_status(&home, &record.owner_id, user.is_some_and(|u| u.status == duduclaw_auth::UserStatus::Active));
                    }
                }
            }
        }
        let agents: Vec<Value> = {
            let reg = self.registry.read().await;
            let mut names: Vec<String> = reg
                .list()
                .iter()
                .map(|a| a.config.agent.name.clone())
                .collect();
            names.sort();
            names
                .iter()
                .map(|n| identity_view(&home, n, people.as_ref()))
                .collect()
        };
        WsFrame::ok_response(
            "",
            json!({
                "protection": protection,
                "key": key,
                "key_error": key_error,
                "settings": match &home_cfg {
                    Ok(c) => json!({
                        "default_identity": c.default_identity.to_config_value(),
                        "allowed_subject_domains": c.allowed_subject_domains,
                        "allowed_domains": c.allowed_domains,
                    }),
                    Err(_) => Value::Null,
                },
                "settings_error": home_cfg.err(),
                "home_subject": crate::google_identity::home_subject(&home),
                "required_scopes": crate::google_workspace::REQUIRED_SCOPES,
                "admin_console_path": ADMIN_CONSOLE_PATH,
                "agents": agents,
                "personal_connections": crate::person_credentials::all_metadata(&home).ok(),
            }),
        )
    }

    /// `google.delegation.import_key {key_json}`.
    pub(crate) async fn handle_google_delegation_import_key(
        &self,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        let Some(raw) = params
            .get("key_json")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            return WsFrame::error_response("", "請提供服務帳號金鑰檔的內容（key_json）。");
        };
        let home = self.home_dir.clone();
        let result = tokio::task::spawn_blocking(move || {
            let key = crate::google_identity::protection_gate(&home).map_err(|r| r.message_zh)?;
            delegation_key::import(&home, &key, &raw)
        })
        .await
        .unwrap_or_else(|e| Err(format!("匯入中斷：{e}")));
        match result {
            Ok(info) => {
                sec_audit(
                    &self.home_dir,
                    "google_delegation_key_imported",
                    Severity::Warning,
                    ctx,
                    json!({ "client_email": info.client_email, "client_id": info.client_id, "key_id": info.key_id }),
                );
                WsFrame::ok_response("", json!({ "key": info }))
            }
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    /// `google.delegation.remove_key {confirm: true}`.
    pub(crate) async fn handle_google_delegation_remove_key(
        &self,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        if params.get("confirm").and_then(Value::as_bool) != Some(true) {
            return WsFrame::error_response(
                "",
                "移除後所有「代表某位同事」的 AI 員工會停止使用 Google，需要明確確認（confirm: true）。",
            );
        }
        match delegation_key::remove(&self.home_dir) {
            Ok(()) => {
                sec_audit(
                    &self.home_dir,
                    "google_delegation_key_removed",
                    Severity::Warning,
                    ctx,
                    json!({}),
                );
                WsFrame::ok_response("", json!({ "removed": true }))
            }
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    /// `google.delegation.settings_set {allowed_subject_domains?, default_identity?}`.
    pub(crate) async fn handle_google_delegation_settings_set(
        &self,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        let mut section = toml::Table::new();
        let current = match gi::read_home_config(&self.home_dir) { Ok(c) => c, Err(e) => return WsFrame::error_response("", &e) };
        let default_identity = match params.get("default_identity") {
            None => current.default_identity.to_config_value(),
            Some(Value::String(s)) => match Selector::parse(s) {
                Ok(sel @ (Selector::Home | Selector::None)) => sel.to_config_value(),
                _ => return WsFrame::error_response("", "default_identity 只能是 home 或 none"),
            },
            Some(_) => return WsFrame::error_response("", "default_identity 必須是字串"),
        };
        let domains = match params.get("allowed_subject_domains") {
            None => current.allowed_subject_domains.clone(),
            Some(Value::Array(items)) => {
                let mut out = Vec::new();
                for v in items {
                    let Some(d) = v
                        .as_str()
                        .map(|d| d.trim().trim_start_matches('@').to_ascii_lowercase())
                    else {
                        return WsFrame::error_response(
                            "",
                            "allowed_subject_domains 必須是字串陣列",
                        );
                    };
                    if !gi::is_domain(&d) {
                        return WsFrame::error_response(
                            "",
                            &format!("「{}」不是網域", duduclaw_core::truncate_chars(&d, 80)),
                        );
                    }
                    if !out.contains(&d) {
                        out.push(d);
                    }
                }
                out
            }
            Some(_) => {
                return WsFrame::error_response("", "allowed_subject_domains 必須是字串陣列");
            }
        };
        if default_identity != "home" {
            section.insert(
                "default_identity".into(),
                toml::Value::String(default_identity.clone()),
            );
        }
        if !domains.is_empty() {
            section.insert(
                "allowed_subject_domains".into(),
                toml::Value::Array(domains.iter().cloned().map(toml::Value::String).collect()),
            );
        }
        let personal_domains = match params.get("allowed_domains") {
            None => current.allowed_domains,
            Some(Value::Array(items)) => {
                let mut domains = Vec::new();
                for value in items {
                    let Some(domain) = value.as_str().map(|s| s.trim().trim_start_matches('@').to_ascii_lowercase()).filter(|s| gi::is_domain(s)) else {
                        return WsFrame::error_response("", "同事可連結的網域必須是有效的網域清單。");
                    };
                    if !domains.contains(&domain) { domains.push(domain); }
                }
                domains
            }
            Some(_) => return WsFrame::error_response("", "allowed_domains 必須是字串陣列"),
        };
        if !personal_domains.is_empty() { section.insert("allowed_domains".into(), toml::Value::Array(personal_domains.iter().cloned().map(toml::Value::String).collect())); }
        let config_path = self.home_dir.join("config.toml");
        let mut table = self.read_config_table(&config_path).await;
        let mut integrations = table
            .get("integrations")
            .and_then(|v| v.as_table())
            .cloned()
            .unwrap_or_default();
        if section.is_empty() {
            integrations.remove(gi::HOME_SECTION);
        } else {
            integrations.insert(gi::HOME_SECTION.into(), toml::Value::Table(section));
        }
        table.insert("integrations".into(), toml::Value::Table(integrations));
        if let Err(e) = self.write_config_table(&config_path, &table).await {
            return WsFrame::error_response("", &format!("無法寫入 config.toml：{e}"));
        }
        sec_audit(
            &self.home_dir,
            "google_identity_settings_set",
            Severity::Info,
            ctx,
            json!({ "default_identity": default_identity, "allowed_subject_domains": domains, "allowed_domains": personal_domains }),
        );
        WsFrame::ok_response(
            "",
            json!({ "default_identity": default_identity, "allowed_subject_domains": domains, "allowed_domains": personal_domains }),
        )
    }

    /// `agents.google_identity_get {agent_id}`.
    pub(crate) async fn handle_agents_google_identity_get(&self, agent_id: &str) -> WsFrame {
        if !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Missing or invalid 'agent_id' parameter");
        }
        let people = self.people().await;
        WsFrame::ok_response("", identity_view(&self.home_dir, agent_id, people.as_ref()))
    }

    /// `agents.google_identity_set {agent_id, identity, fallback?, requesters?}`.
    pub(crate) async fn handle_agents_google_identity_set(
        &self,
        mut params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        let agent_id = params
            .get("agent_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if !is_valid_agent_id(&agent_id)
            || !self
                .home_dir
                .join("agents")
                .join(&agent_id)
                .join("agent.toml")
                .is_file()
        {
            return WsFrame::error_response("", "Missing or invalid 'agent_id' parameter");
        }
        let before = gi::read_agent_section(&self.home_dir, &agent_id);
        if let gi::SectionRead::Present(previous) = &before {
            // Omitted policy fields retain their previous restriction. An
            // ordinary identity/fallback edit must not widen owner_only.
            if params.get("requesters").is_none() {
                params["requesters"] = json!(previous.requesters.as_str());
            }
            let unchanged_identity = previous.identity.as_ref().is_some_and(|identity| {
                params
                    .get("identity")
                    .and_then(Value::as_str)
                    .and_then(|s| Selector::parse(s).ok())
                    .as_ref()
                    == Some(identity)
            });
            if unchanged_identity
                && previous.requesters == Requesters::Anyone
                && params.get("requesters").and_then(Value::as_str) == Some("anyone")
            {
                params["confirm_anyone"] = json!(true);
            }
        }
        let mut section = match parse_identity_params(&params) {
            Ok(s) => s,
            Err(e) => return WsFrame::error_response("", &e),
        };
        // A non-admin disabling Google retains the validated requester boundary.
        // Admin Home/None edits keep the parser's existing policy-clearing behavior.
        if !ctx.is_admin()
            && section.get("identity").and_then(toml::Value::as_str) == Some("none")
            && let Some(requesters) = params.get("requesters").and_then(Value::as_str)
        {
            section.insert(
                "requesters".into(),
                toml::Value::String(requesters.into()),
            );
        }
        let parsed = match gi::section_from_table(&section) { Ok(p) => p, Err(e) => return WsFrame::error_response("", &e) };
        let personal_selector = parsed.identity.as_ref().filter(|s| matches!(s, Selector::Owner | Selector::User(_)));
        let db = self.user_db.read().await.clone();
        let selected = if let Some(selector) = personal_selector {
            if self.resolve_edition_profile().await.is_personal() { return super::gates::enterprise_only_reject_frame(); }
            let Some(db) = &db else { return WsFrame::error_response("", "人員資料尚未就緒。"); };
            match crate::google_personal::access::select_user(db, &agent_id, selector) {
                Ok(user) if user.status == duduclaw_auth::UserStatus::Active => Some(user.id),
                Ok(_) => return WsFrame::error_response("", "指定的同事目前不是有效帳號。"),
                Err(r) => return WsFrame::error_response("", &r.message_zh),
            }
        } else { None };
        let mut self_consent = false;
        if !ctx.is_admin() {
            let before_policy = match &before { gi::SectionRead::Present(p) => p.clone(), gi::SectionRead::Absent => gi::AgentGoogleSection::default(), gi::SectionRead::Invalid(_) => return WsFrame::error_response("", "原本的 Google 設定無法辨識，請管理員修正。") };
            match crate::google_personal::owner_change::validate(&before_policy, &parsed, &ctx.user_id, selected.as_deref(), params.get("confirm_shared_access").and_then(Value::as_bool) == Some(true)) {
                Ok(own) => self_consent = own,
                Err(e) => return WsFrame::error_response("", &e),
            }
        }
        if let Some(Selector::Delegate(email)) = section
            .get("identity")
            .and_then(|v| v.as_str())
            .and_then(|s| Selector::parse(s).ok())
        {
            let cfg = match gi::read_home_config(&self.home_dir) { Ok(c) => c, Err(e) => return WsFrame::error_response("", &e) };
            if !gi::delegate_domain_allowed(
                &email,
                &cfg,
                crate::google_identity::home_subject(&self.home_dir).as_deref(),
            ) {
                return WsFrame::error_response(
                    "",
                    &format!(
                        "{email} 不在允許的網域。請先在「整合 → Google → 公司授權代表」設定允許的網域。"
                    ),
                );
            }
        }
        let new_section = section.clone();
        let expected_before = before.clone();
        let own_home = self.home_dir.clone();
        let own_agent = agent_id.clone();
        let restrict_caller = !ctx.is_admin();
        let result = self
            .update_agent_toml_as(
                &agent_id,
                "agents.google_identity_set",
                Some(ctx),
                if ctx.is_admin() { None } else { Some(ctx) },
                move |table| {
                    if restrict_caller && gi::read_agent_section(&own_home, &own_agent) != expected_before { return Err("Google 設定已變更，請重新開啟後再儲存。".into()); }
                    if new_section.is_empty() {
                        table.remove(gi::AGENT_SECTION);
                    } else {
                        table.insert(gi::AGENT_SECTION.into(), toml::Value::Table(new_section));
                    }
                    Ok(())
                },
            )
            .await;
        if let Err(e) = result {
            return WsFrame::error_response("", &e);
        }
        if self_consent {
            let Some(db) = &db else { return WsFrame::error_response("", "人員資料尚未就緒，尚未允許帳號使用。"); };
            if let Err(e) = db.set_google_consent(&ctx.user_id, &agent_id, true, &ctx.user_id) { return WsFrame::error_response("", &e); }
        }
        let after = gi::read_agent_section(&self.home_dir, &agent_id);
        let describe = |s: &gi::SectionRead| match s {
            gi::SectionRead::Absent => json!(null),
            gi::SectionRead::Invalid(_) => json!("invalid"),
            gi::SectionRead::Present(p) => json!({
                "identity": p.identity.as_ref().map(Selector::to_config_value),
                "fallback": p.fallback.as_str(),
                "requesters": p.requesters.as_str(),
            }),
        };
        let detail =
            json!({ "agent_id": agent_id, "before": describe(&before), "after": describe(&after) });
        if let Some(db) = self.user_db.read().await.clone() {
            let _ = db.log_action(
                Some(&ctx.user_id),
                "agent.google_identity_set",
                Some(&agent_id),
                Some(&detail.to_string()),
                None,
            );
        }
        sec_audit(
            &self.home_dir,
            "agent_google_identity_set",
            Severity::Warning,
            ctx,
            detail,
        );
        let people = self.people().await;
        WsFrame::ok_response(
            "",
            identity_view(&self.home_dir, &agent_id, people.as_ref()),
        )
    }

    /// `google.identity.test {agent_id}`.
    pub(crate) async fn handle_google_identity_test(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(Value::as_str).unwrap_or("");
        if !is_valid_agent_id(agent_id) {
            return WsFrame::error_response("", "Missing or invalid 'agent_id' parameter");
        }
        let people = self.people().await;
        let (email, steps) =
            crate::google_identity::check::run(&self.home_dir, agent_id, people.as_ref()).await;
        let ok = steps.iter().all(|s| s.ok);
        WsFrame::ok_response(
            "",
            json!({ "agent_id": agent_id, "email": email, "ok": ok, "steps": steps }),
        )
    }
}

/// Validate `agents.google_identity_set` params into the `[google]` table
/// to write (the identity is always written explicitly). B2a accepts
/// `home`, `none` and `delegate:<email>`; a delegate with `requesters =
/// "anyone"` needs `confirm_anyone: true`. Leaving a person identity drops
/// that policy and never asks the operator to confirm a widening.
pub(crate) fn parse_identity_params(params: &Value) -> Result<toml::Table, String> {
    for field in ["fallback", "requesters"] {
        if params.get(field).is_some_and(|value| !value.is_string()) {
            return Err(format!("{field} 必須是字串"));
        }
    }
    let identity = params
        .get("identity")
        .and_then(Value::as_str)
        .ok_or("請提供 identity")?;
    let sel = Selector::parse(identity)?;
    let mut t = toml::Table::new();
    let fallback = match params
        .get("fallback")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_ascii_lowercase())
    {
        None => "none".to_string(),
        Some(v) if v == "none" || v == "home" => v,
        Some(_) => return Err("fallback 只能是 none 或 home".into()),
    };
    let requesters = match params.get("requesters").and_then(Value::as_str) {
        None => Requesters::default(),
        Some(r) => Requesters::parse(r)?,
    };
    if requesters == Requesters::Anyone
        && matches!(sel, Selector::Delegate(_) | Selector::Owner | Selector::User(_))
        && params.get("confirm_anyone").and_then(Value::as_bool) != Some(true)
    {
        return Err("「任何人都能透過這位員工使用」需要明確確認（confirm_anyone: true）。公開訪客仍然一律不行。".into());
    }
    t.insert(
        "identity".into(),
        toml::Value::String(sel.to_config_value()),
    );
    if matches!(sel, Selector::Delegate(_) | Selector::Owner | Selector::User(_)) {
        if fallback != "none" {
            t.insert("fallback".into(), toml::Value::String(fallback));
        }
        if requesters != Requesters::default() {
            t.insert(
                "requesters".into(),
                toml::Value::String(requesters.as_str().into()),
            );
        }
    }
    Ok(t)
}
