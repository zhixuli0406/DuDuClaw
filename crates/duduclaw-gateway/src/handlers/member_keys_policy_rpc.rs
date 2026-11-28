//! Browser-only, live-admin creation policy with write-ahead audit.
use super::*;
use duduclaw_core::mcp_scopes::PERSON_KEY_MCP_SCOPE_STRINGS;
use duduclaw_security::audit::{AuditEvent, Severity, try_append_audit_event};
use serde::Deserialize;

const INVALID: &str = "無法讀取本人金鑰設定，請管理員檢查設定後再試。";
const LOGIN: &str = "請以管理員帳號登入瀏覽器後管理本人金鑰的開放範圍。";
const STORE: &str = "無法儲存本人金鑰的開放範圍，請重新整理後再試。";
const CONVERSATION_RISK: &str = "開放後，成員可讓新金鑰讀取自己的對話內容，持有金鑰的外部系統也能讀取。請確認已了解風險。";
const SCOPES_RISK: &str = "開放後，成員可讓新金鑰使用選取的外部工具功能，持有金鑰的外部系統可能讀取資料或執行寫入。請確認已了解風險。";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetPolicy {
    allow_conversation_content: Option<bool>,
    allow_mcp_scopes: Option<Vec<String>>,
    #[serde(default)]
    acknowledge_risk: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

fn fresh(db: &UserDb, ctx: &UserContext, session: &str) -> Result<UserContext, String> {
    super::odoo_access_rpc::fresh(db, ctx, session).map_err(|_| LOGIN.to_owned())
}

fn summary(config: &crate::member_keys::Config) -> Value {
    json!({"allow_conversation_content":config.allow_conversation_content,
        "allow_mcp_scopes":config.allow_mcp_scopes})
}
fn response(config: &crate::member_keys::Config) -> Value {
    json!({"readable":true,"enabled":config.enabled,
        "allow_conversation_content":config.allow_conversation_content,"allow_mcp_scopes":config.allow_mcp_scopes,
        "available_mcp_scopes":PERSON_KEY_MCP_SCOPE_STRINGS,
        "risk":{"conversation":CONVERSATION_RISK,"mcp_scopes":SCOPES_RISK}})
}

impl MethodHandler {
    pub(crate) async fn handle_member_keys_policy_rpc(&self, method: &str, params: Value, ctx: &UserContext) -> WsFrame {
        if ctx.member_key_id.is_some() || ctx.user_id == "system" { return WsFrame::error_response("", LOGIN); }
        let session = match crate::google_personal::session_for(&ctx.user_id) {
            Ok(session) => session, Err(_) => return WsFrame::error_response("", LOGIN),
        };
        let Some(db) = self.user_db.read().await.clone() else { return WsFrame::error_response("", LOGIN); };
        let home = self.home_dir.clone();
        let ctx = ctx.clone();
        let method = method.to_owned();
        let result = tokio::task::spawn_blocking(move || -> Result<Value, String> {
            fresh(&db, &ctx, &session)?;
            if !params.is_object() { return Err("本人金鑰設定參數無效。".into()); }
            if method == "member_keys.policy_get" {
                serde_json::from_value::<Empty>(params).map_err(|_| "此操作不接受參數。".to_owned())?;
                return Ok(match crate::member_keys::read_config(&home) {
                    Ok(config) => response(&config),
                    Err(_) => json!({"readable":false,"message":INVALID}),
                });
            }
            if method != "member_keys.policy_set" { return Err("沒有這項本人金鑰設定操作。".into()); }
            if params.as_object().is_some_and(|p| p.values().any(Value::is_null)) {
                return Err("本人金鑰設定參數不能是空值。".into());
            }
            let args: SetPolicy = serde_json::from_value(params).map_err(|_| "本人金鑰設定參數無效。".to_owned())?;
            // Parse the exact snapshot whose hash is checked under the commit lock.
            // Missing files are unreadable, never implicit permission to create one.
            let raw = std::fs::read_to_string(home.join("config.toml")).map_err(|_| INVALID.to_owned())?;
            let before = crate::member_keys::Config::parse(&raw).ok_or(INVALID)?;
            let mut table: toml::Table = raw.parse().map_err(|_| INVALID.to_owned())?;
            if args.allow_conversation_content == Some(true) && !before.allow_conversation_content && !args.acknowledge_risk {
                return Err(CONVERSATION_RISK.into());
            }
            if let Some(scopes) = &args.allow_mcp_scopes {
                if scopes.iter().any(|s| !PERSON_KEY_MCP_SCOPE_STRINGS.contains(&s.as_str())) {
                    return Err("選取的外部工具使用範圍無效，請重新選擇。".into());
                }
                if !args.acknowledge_risk && scopes.iter().any(|s| !before.allow_mcp_scopes.contains(s)) {
                    return Err(SCOPES_RISK.into());
                }
            }
            // An empty update need not create an empty section.
            if args.allow_conversation_content.is_some() || args.allow_mcp_scopes.is_some() {
                let section = super::config_commit::table_at_mut(&mut table, &["member_api_keys"]).map_err(|_| INVALID.to_owned())?;
                if let Some(value) = args.allow_conversation_content {
                    section.insert("allow_conversation_content".into(), toml::Value::Boolean(value));
                }
                if let Some(scopes) = &args.allow_mcp_scopes {
                    let mut scopes = scopes.clone();
                    scopes.sort();
                    scopes.dedup();
                    section.insert("allow_mcp_scopes".into(), toml::Value::Array(scopes.into_iter().map(toml::Value::String).collect()));
                }
            }
            let rendered = super::config_commit::render_preserving(&raw, &table).map_err(|_| STORE.to_owned())?;
            let after = crate::member_keys::Config::parse(&rendered).ok_or(INVALID)?;
            super::config_commit::commit_member_keys_policy_table(
                &home, &super::config_commit::content_hash(&raw), &table, || {
                    let actor = fresh(&db, &ctx, &session)?;
                    try_append_audit_event(&home, &AuditEvent::new(
                        "member_keys_policy_changed", "", Severity::Info,
                        json!({"actor_user_id":actor.user_id,"before":summary(&before),"after":summary(&after),
                            "acknowledged":args.acknowledge_risk,"phase":"before_commit"}),
                    )).map_err(|_| STORE.to_owned())
                },
            ).map_err(|_| STORE.to_owned())?;
            Ok(response(&after))
        }).await;
        match result {
            Ok(Ok(value)) => WsFrame::ok_response("", value),
            Ok(Err(error)) => WsFrame::error_response("", &error),
            Err(_) => WsFrame::error_response("", STORE),
        }
    }
}

#[cfg(test)]
#[path = "tests/member_keys_policy_rpc_tests.rs"]
mod tests;
