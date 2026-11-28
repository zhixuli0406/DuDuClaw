//! Browser-only, live-admin Odoo policy editing with write-ahead audit.
use super::*;
use duduclaw_odoo::access_policy::{DEFAULT_ALWAYS_REFUSE, FixedRefusal, OdooAccessConfig, PermissionMode, RefuseRule};
use duduclaw_security::audit::{AuditEvent, Severity, try_append_audit_event};
use serde::Deserialize;
use crate::odoo_access_migration::{Employee, employees};

const INVALID: &str = "[odoo_access] 權限設定無效，請管理員修正設定後再試。";
const LOGIN: &str = "請以管理員帳號登入瀏覽器後管理 Odoo 權限。";
const STORE: &str = "無法儲存 Odoo 權限設定，請重新整理後再試。";
const FOLLOW_RISK: &str = "依 Odoo 帳號模式會略過員工的引擎權限表與模型封鎖；一律不允許清單、固定拒絕與必要核准仍生效。請確認已了解風險。";
const REFUSE_RISK: &str = "移除一律不允許項目會讓 AI 可以嘗試原本禁止的操作。請確認已了解風險。";
const COMPANY_RISK: &str = "新增 AI 會允許該 AI 使用全公司 Odoo 帳號。請確認已了解風險。";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetAccess {
    permission_mode: Option<PermissionMode>,
    #[serde(default)]
    acknowledge_risk: bool,
    company_key_agents: Option<Vec<String>>,
    always_refuse: Option<Vec<String>>,
    attachment_max_mb: Option<u8>,
    #[serde(default)]
    dismiss_upgrade_notice: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

pub(super) fn fresh(db: &UserDb, ctx: &UserContext, session: &str) -> Result<UserContext, String> {
    if ctx.member_key_id.is_some() || ctx.user_id == "system" { return Err(LOGIN.into()); }
    let live = db.live_user_context(&ctx.user_id).map_err(|_| LOGIN.to_owned())?
        .filter(|u| u.credential_epoch == ctx.credential_epoch && !u.must_change_password)
        .ok_or(LOGIN)?;
    acl::require_role(&live, UserRole::Admin).map_err(|_| LOGIN.to_owned())?;
    if !db.remember_session_valid(Some(session), &live.user_id, Utc::now().timestamp())
        .map_err(|_| LOGIN.to_owned())? {
        return Err(LOGIN.into());
    }
    Ok(live)
}

fn summary(access: &OdooAccessConfig) -> Value {
    json!({
        "permission_mode":match access.permission_mode {
            PermissionMode::Engine => "engine", PermissionMode::FollowOdooAccount => "follow_odoo_account",
        },
        "permission_mode_ack":access.permission_mode_ack.as_ref().map(|a| json!({"by":a.by,"at":a.at})),
        "company_key_agents":access.company_key_agents,
        "company_key_agents_migrated":access.company_key_agents_migrated,
        "always_refuse":access.always_refuse,
        "attachment_max_mb":access.attachment_max_mb,
        "upgrade_notice_pending":access.upgrade_notice_pending,
    })
}
fn response(access: &OdooAccessConfig, rows: &[Employee]) -> Value {
    let fixed: Vec<_> = [FixedRefusal::F1, FixedRefusal::F2, FixedRefusal::F3, FixedRefusal::F4, FixedRefusal::F5]
        .into_iter().map(|f| json!({"code":f.code(),"reason":f.reason()})).collect();
    json!({"config":summary(access),"defaults":{"always_refuse":DEFAULT_ALWAYS_REFUSE},
        "fixed_refusals":fixed,"upgrade_notice_pending":access.upgrade_notice_pending,"employees":rows,
        "risk_text":{"permission_mode":FOLLOW_RISK,"always_refuse":REFUSE_RISK,"company_key_agents":COMPANY_RISK}})
}

fn apply(table: &mut toml::Table, args: &SetAccess, actor: &str, rows: &[Employee]) -> Result<OdooAccessConfig, String> {
    let before = OdooAccessConfig::from_table(table).map_err(|_| INVALID.to_owned())?;
    if let Some(ids) = &args.company_key_agents {
        if ids.iter().any(|id| !duduclaw_core::is_valid_agent_id(id) || !rows.iter().any(|e| &e.id == id)) {
            return Err("請選擇存在的正式 AI 員工。".into());
        }
        if !args.acknowledge_risk && ids.iter().any(|id| !before.company_key_agents.contains(id)) {
            return Err(COMPANY_RISK.into());
        }
    }
    if let Some(rules) = &args.always_refuse {
        for rule in rules { RefuseRule::parse(rule)?; }
        if !args.acknowledge_risk && before.always_refuse.iter().any(|r| !rules.contains(r)) {
            return Err(REFUSE_RISK.into());
        }
    }
    if args.attachment_max_mb.is_some_and(|n| !(1..=25).contains(&n)) {
        return Err("附件大小必須介於 1 到 25 MB。".into());
    }
    if args.permission_mode == Some(PermissionMode::FollowOdooAccount) && !args.acknowledge_risk {
        return Err(FOLLOW_RISK.into());
    }
    let section = super::config_commit::table_at_mut(table, &["odoo_access"])?;
    if let Some(mode) = args.permission_mode {
        let value = match mode {
            PermissionMode::Engine => {
                section.remove("permission_mode_ack");
                "engine"
            }
            PermissionMode::FollowOdooAccount => {
                let mut ack = toml::Table::new();
                ack.insert("by".into(), toml::Value::String(actor.into()));
                ack.insert("at".into(), toml::Value::String(Utc::now().to_rfc3339()));
                section.insert("permission_mode_ack".into(), toml::Value::Table(ack));
                "follow_odoo_account"
            }
        };
        section.insert("permission_mode".into(), toml::Value::String(value.into()));
    }
    if let Some(ids) = &args.company_key_agents {
        let mut ids = ids.clone();
        ids.sort();
        ids.dedup();
        section.insert("company_key_agents".into(), toml::Value::Array(ids.into_iter().map(toml::Value::String).collect()));
    }
    if let Some(rules) = &args.always_refuse {
        section.insert("always_refuse".into(), toml::Value::Array(rules.iter().cloned().map(toml::Value::String).collect()));
    }
    if let Some(n) = args.attachment_max_mb {
        section.insert("attachment_max_mb".into(), toml::Value::Integer(i64::from(n)));
    }
    if args.dismiss_upgrade_notice {
        section.insert("upgrade_notice_pending".into(), toml::Value::Boolean(false));
    }
    OdooAccessConfig::from_table(table).map_err(|_| INVALID.to_owned())
}

impl MethodHandler {
    pub(crate) async fn handle_odoo_access_rpc(&self, method: &str, params: Value, ctx: &UserContext) -> WsFrame {
        if ctx.member_key_id.is_some() || ctx.user_id == "system" { return WsFrame::error_response("", LOGIN); }
        let session = match crate::google_personal::session_for(&ctx.user_id) {
            Ok(session) => session, Err(_) => return WsFrame::error_response("", LOGIN),
        };
        let Some(db) = self.user_db.read().await.clone() else { return WsFrame::error_response("", LOGIN); };
        let home = self.home_dir.clone();
        let ctx = ctx.clone();
        let method = method.to_owned();
        let result = tokio::task::spawn_blocking(move || -> Result<Value, String> {
            let live = fresh(&db, &ctx, &session)?;
            let path = home.join("config.toml");
            let raw = super::config_commit::read_text_or_empty(&path).map_err(|_| INVALID.to_owned())?;
            let mut table: toml::Table = raw.parse().map_err(|_| INVALID.to_owned())?;
            let before = OdooAccessConfig::from_table(&table).map_err(|_| INVALID.to_owned())?;
            let rows = employees(&home)?;
            match method.as_str() {
                "odoo.access_get" => {
                    serde_json::from_value::<Empty>(params).map_err(|_| "此操作不接受參數。".to_owned())?;
                    Ok(response(&before, &rows))
                }
                "odoo.access_set" => {
                    if params.as_object().is_some_and(|p| p.values().any(Value::is_null)) {
                        return Err("Odoo 權限參數不能是 null。".into());
                    }
                    let args: SetAccess = serde_json::from_value(params).map_err(|_| "Odoo 權限參數無效。".to_owned())?;
                    let after = apply(&mut table, &args, &live.user_id, &rows)?;
                    let before_summary = summary(&before);
                    let after_summary = summary(&after);
                    super::config_commit::commit_odoo_table(
                        &home, &super::config_commit::content_hash(&raw), &table, super::config_commit::OdooWrite::Access, || {
                            let actor = fresh(&db, &ctx, &session)?;
                            // Recheck employee existence immediately before publication.
                            if let Some(ids) = &args.company_key_agents {
                                let current = employees(&home)?;
                                if ids.iter().any(|id| !current.iter().any(|e| &e.id == id)) {
                                    return Err("AI 員工清單已變更，請重新整理。".into());
                                }
                            }
                            try_append_audit_event(&home, &AuditEvent::new(
                                "odoo_access_changed", "", Severity::Info,
                                json!({"actor_user_id":actor.user_id,"before":before_summary,"after":after_summary,
                                    "acknowledged":args.acknowledge_risk,"phase":"before_commit"}),
                            )).map_err(|_| STORE.to_owned())
                        },
                    ).map_err(|_| STORE.to_owned())?;
                    Ok(response(&after, &rows))
                }
                _ => Err("沒有這項 Odoo 權限操作。".into()),
            }
        }).await;
        match result {
            Ok(Ok(value)) => WsFrame::ok_response("", value),
            Ok(Err(error)) => WsFrame::error_response("", &error),
            Err(_) => WsFrame::error_response("", STORE),
        }
    }
}

#[cfg(test)]
#[path = "tests/odoo_access_rpc_tests.rs"]
mod tests;
