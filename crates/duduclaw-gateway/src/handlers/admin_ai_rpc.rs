//! Administrator AI settings: explicit acknowledgement and server-owned evidence.
use super::*;
use super::config_commit::{content_hash, read_text_or_empty, commit_table_locked_with_guard};

const CONFIG_UNREADABLE: &str = "設定檔目前無法讀取，請先修正設定檔再變更管理員 AI 設定。";

fn stamp_ack(section: &mut toml::Table, user_id: &str) {
    let mut ack = toml::Table::new();
    ack.insert("by".into(), toml::Value::String(user_id.into()));
    ack.insert("at".into(), toml::Value::String(chrono::Utc::now().to_rfc3339()));
    section.insert("enabled_ack".into(), toml::Value::Table(ack));
}

pub(super) fn prepare_admin_ai_change(
    before: &toml::Table, after: &mut toml::Table, acknowledged: bool, user_id: &str,
) -> Result<bool, String> {
    if before.get("admin_ai") == after.get("admin_ai") { return Ok(false); }
    if !acknowledged {
        return Err("變更管理員 AI 設定前，請明確確認風險（acknowledge_risk = true）。".into());
    }
    let enabled = duduclaw_core::admin_ai_settings::enabled_from_table(after);
    if let Some(section) = after.get_mut("admin_ai") {
        let section = section.as_table_mut().ok_or("[admin_ai] 必須是設定區段。")?;
        // A client may neither forge nor delete the recorded acknowledgement.
        section.remove("enabled_ack");
        if enabled {
            stamp_ack(section, user_id);
        } else if let Some(ack) = before.get("admin_ai").and_then(|s| s.get("enabled_ack")) {
            section.insert("enabled_ack".into(), ack.clone());
        }
    }
    Ok(true)
}

pub(super) fn audit_admin_ai_toggle(home: &Path, user_id: &str, before: &toml::Table, after: &toml::Table) {
    crate::security_autopilot::audit_and_emit(home,
        &duduclaw_security::audit::AuditEvent::new("admin_ai_settings_toggled", user_id,
            duduclaw_security::audit::Severity::Warning,
            json!({"user_id": user_id,
                "before": duduclaw_core::admin_ai_settings::enabled_from_table(before),
                "after": duduclaw_core::admin_ai_settings::enabled_from_table(after)})));
}

impl MethodHandler {
    pub(crate) async fn handle_admin_ai_get(&self, ctx: &UserContext) -> WsFrame {
        if let Err(e) = acl::require_role(ctx, UserRole::Admin) { return WsFrame::error_response("", &e); }
        let (table, config_state) = match read_text_or_empty(&self.home_dir.join("config.toml")) {
            Err(_) => (toml::Table::new(), "unreadable"),
            Ok(text) => match text.parse::<toml::Table>() {
                Err(_) => (toml::Table::new(), "malformed"),
                Ok(table) => {
                    let valid = table.get("admin_ai").is_none_or(|section| section.as_table()
                        .is_some_and(|section| section.get("enabled").is_none_or(|value| value.as_bool().is_some())));
                    (table, if valid { "ok" } else { "malformed" })
                }
            },
        };
        let mut payload = json!({"enabled": config_state == "ok"
            && duduclaw_core::admin_ai_settings::enabled_from_table(&table), "config_state": config_state});
        if let Some(ack) = table.get("admin_ai").and_then(|s| s.get("enabled_ack")).and_then(toml::Value::as_table) {
            if let (Some(by), Some(at)) = (ack.get("by").and_then(toml::Value::as_str), ack.get("at").and_then(toml::Value::as_str)) {
                let by_name = match self.user_db().await {
                    Some(db) => db.get_user(by).ok().flatten()
                        .map(|user| duduclaw_core::plain_line(&user.display_name, 200)),
                    None => None,
                };
                payload["enabled_ack"] = json!({"by": by, "by_name": by_name, "at": at});
            }
        }
        WsFrame::ok_response("", payload)
    }

    pub(crate) async fn handle_admin_ai_set(&self, params: Value, ctx: &UserContext) -> WsFrame {
        if let Err(e) = acl::require_role(ctx, UserRole::Admin) { return WsFrame::error_response("", &e); }
        let Some(enabled) = params.get("enabled").and_then(Value::as_bool) else {
            return WsFrame::error_response("", "enabled 必須是布林值。");
        };
        let acknowledged = params.get("acknowledge_risk").and_then(Value::as_bool) == Some(true);
        if enabled && !acknowledged {
            return WsFrame::error_response("", "開啟管理員 AI 前，請明確確認風險（acknowledge_risk = true）。");
        }
        let path = self.home_dir.join("config.toml");
        let original = match read_text_or_empty(&path) { Ok(text) => text, Err(_) => return WsFrame::error_response("", CONFIG_UNREADABLE) };
        let before: toml::Table = match original.parse() { Ok(table) => table, Err(_) => return WsFrame::error_response("", CONFIG_UNREADABLE) };
        let mut after = before.clone();
        let section = after.entry("admin_ai").or_insert_with(|| toml::Value::Table(toml::Table::new()));
        let Some(section) = section.as_table_mut() else { return WsFrame::error_response("", "[admin_ai] 必須是設定區段。"); };
        section.insert("enabled".into(), toml::Value::Boolean(enabled));
        // Force server-generated evidence even when explicitly re-enabling an enabled setting.
        if enabled { stamp_ack(section, &ctx.user_id); }
        if let Err(e) = prepare_admin_ai_change(&before, &mut after, acknowledged || !enabled, &ctx.user_id) {
            return WsFrame::error_response("", &e);
        }
        let home = self.home_dir.clone();
        let actor = ctx.clone();
        if let Err(e) = commit_table_locked_with_guard(&path, content_hash(&original), &after,
            move || super::system_update_config::require_live_admin_at_commit(&home, &actor)).await {
            return WsFrame::error_response("", &e);
        }
        audit_admin_ai_toggle(&self.home_dir, &ctx.user_id, &before, &after);
        self.handle_admin_ai_get(ctx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(frame: &WsFrame) -> bool { matches!(frame, WsFrame::Response { ok: true, .. }) }
    fn payload(frame: &WsFrame) -> Value {
        match frame { WsFrame::Response { payload, .. } => payload.clone().unwrap_or(Value::Null), _ => panic!("{frame:?}") }
    }

    #[tokio::test]
    async fn admin_ai_get_reports_config_state_and_set_sanitizes_read_errors() {
        let home = tempfile::tempdir().unwrap();
        let h = MethodHandler::new(home.path().to_owned()).await;
        let admin = UserContext::admin_fallback();
        let path = home.path().join("config.toml");
        assert_eq!(payload(&h.handle_admin_ai_get(&admin).await), json!({"enabled": false, "config_state": "ok"}));
        for (text, state, enabled) in [
            ("[admin_ai]\nenabled=true\n", "ok", true),
            ("[admin_ai]\nenabled=false\n", "ok", false),
            ("broken=[", "malformed", false),
            ("[admin_ai]\nenabled='yes'\n", "malformed", false),
            ("admin_ai=true\n", "malformed", false),
        ] {
            std::fs::write(&path, text).unwrap();
            assert_eq!(payload(&h.handle_admin_ai_get(&admin).await),
                json!({"enabled": enabled, "config_state": state}));
        }
        std::fs::write(&path, "broken=[").unwrap();
        let expected = "設定檔目前無法讀取，請先修正設定檔再變更管理員 AI 設定。";
        let frame = h.handle_admin_ai_set(json!({"enabled": false}), &admin).await;
        assert!(matches!(frame, WsFrame::Response { ok: false, error: Some(ref e), .. } if e == &json!(expected)));
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert_eq!(payload(&h.handle_admin_ai_get(&admin).await), json!({"enabled": false, "config_state": "unreadable"}));
        let frame = h.handle_admin_ai_set(json!({"enabled": false}), &admin).await;
        assert!(matches!(frame, WsFrame::Response { ok: false, error: Some(ref e), .. } if e == &json!(expected)));
    }

    #[tokio::test]
    async fn admin_ai_get_ack_name_resolves_and_is_null_for_deleted_user() {
        let home = tempfile::tempdir().unwrap();
        let db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
        let user = db.create_user("admin@example.invalid", "王管理員", "test-password", UserRole::Admin).unwrap();
        let h = MethodHandler::new(home.path().to_owned()).await;
        std::fs::write(home.path().join("config.toml"), format!(
            "[admin_ai]\nenabled=true\nenabled_ack={{by='{}',at='2026-11-28T01:02:03Z'}}\n", user.id)).unwrap();
        let admin = UserContext::admin_fallback();
        let data = payload(&h.handle_admin_ai_get(&admin).await);
        assert_eq!(data["enabled_ack"], json!({"by": user.id, "by_name": "王管理員", "at": "2026-11-28T01:02:03Z"}));
        rusqlite::Connection::open(home.path().join("users.db")).unwrap()
            .execute("DELETE FROM users WHERE id=?1", [&user.id]).unwrap();
        let data = payload(&h.handle_admin_ai_get(&admin).await);
        assert_eq!(data["enabled_ack"], json!({"by": user.id, "by_name": null, "at": "2026-11-28T01:02:03Z"}));
        assert_eq!(data["enabled"], true);
    }

    #[tokio::test]
    async fn admin_ai_rpc_roles_ack_and_server_evidence() {
        let home = tempfile::tempdir().unwrap();
        let _db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
        let h = MethodHandler::new(home.path().to_owned()).await;
        let admin = UserContext::admin_fallback();
        let mut non_admin = admin.clone();
        for role in [UserRole::Manager, UserRole::Employee] {
            non_admin.role = role;
            for method in ["admin_ai.get", "admin_ai.set"] {
                assert!(!ok(&h.handle(method, json!({"enabled": true, "acknowledge_risk": true}), &non_admin).await));
            }
        }
        assert_eq!(payload(&h.handle("admin_ai.get", json!({}), &admin).await)["enabled"], false);
        for ack in [json!(null), json!(false), json!("true")] {
            assert!(!ok(&h.handle("admin_ai.set", json!({"enabled": true, "acknowledge_risk": ack}), &admin).await));
        }
        let frame = h.handle("admin_ai.set", json!({"enabled": true, "acknowledge_risk": true,
            "enabled_ack": {"by": "forged", "at": "forged"}}), &admin).await;
        assert!(ok(&frame), "{frame:?}");
        let enabled = payload(&frame);
        assert_eq!(enabled["enabled"], true);
        assert_eq!(enabled["enabled_ack"]["by"], admin.user_id);
        assert!(chrono::DateTime::parse_from_rfc3339(enabled["enabled_ack"]["at"].as_str().unwrap()).is_ok());
        let frame = h.handle("admin_ai.set", json!({"enabled": false}), &admin).await;
        assert!(ok(&frame), "{frame:?}");
        assert_eq!(payload(&frame)["enabled_ack"], enabled["enabled_ack"]);
    }

    #[tokio::test]
    async fn raw_editor_requires_ack_and_overwrites_client_ack() {
        let home = tempfile::tempdir().unwrap();
        let _db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
        let h = MethodHandler::new(home.path().to_owned()).await;
        let admin = UserContext::admin_fallback();
        let path = home.path().join("config.toml");
        let original = "[admin_ai]\nenabled=false\n";
        std::fs::write(&path, original).unwrap();
        let mut params = json!({"file": "config", "base_hash": content_hash(original),
            "content": "[admin_ai]\nenabled=true\nenabled_ack={by='forged',at='forged'}\n"});
        let frame = h.handle_config_raw_set(params.clone(), &admin).await;
        assert!(!ok(&frame));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        params["acknowledge_risk"] = json!(true);
        let frame = h.handle_config_raw_set(params, &admin).await;
        assert!(ok(&frame), "{frame:?}");
        let stored: toml::Table = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        assert_eq!(stored["admin_ai"]["enabled_ack"]["by"].as_str(), Some(admin.user_id.as_str()));
        assert_ne!(stored["admin_ai"]["enabled_ack"]["at"].as_str(), Some("forged"));
    }
}
