//! Browser-only per-employee Odoo consent, separate from connection secrets.
use super::*;
use duduclaw_odoo::access_policy::PermissionOverride;
use duduclaw_security::audit::{AuditEvent, Severity, try_append_audit_event};
use serde::Deserialize;

const LOGIN: &str = "請以管理員帳號登入瀏覽器後管理 Odoo 權限。";
const INVALID: &str = "AI 的 Odoo 權限參數無效。";
const RISK: &str = "放寬 AI 的 Odoo 權限或使用請求者帳號，須確認已了解風險。";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetAgentAccess {
    agent_id: String,
    permission_override: Option<String>,
    act_as_requester: Option<bool>,
    #[serde(default)]
    acknowledge_risk: bool,
}

/// Keep unknown stored strings visible; the runtime interprets them as
/// read_only. Only new input is restricted to the three supported strings.
pub(super) fn summary(table: &toml::Table) -> Result<Value, String> {
    if table.get("odoo").is_some_and(|v| !v.is_table()) {
        return Err(INVALID.into());
    }
    let mode = match super::config_commit::toml_at(table, "odoo.permission_override") {
        None => "inherit",
        Some(toml::Value::String(s)) => s,
        Some(_) => return Err(INVALID.into()),
    };
    let requester = match super::config_commit::toml_at(table, "odoo.act_as_requester") {
        None => false,
        Some(toml::Value::Boolean(b)) => *b,
        Some(_) => return Err(INVALID.into()),
    };
    Ok(json!({"permission_override":mode,"act_as_requester":requester}))
}

fn require_ack(before: &Value, after: &Value, acknowledged: bool) -> Result<(), String> {
    let rank = |value: &Value| PermissionOverride::from_config(value["permission_override"].as_str()).restriction_rank();
    if !acknowledged && ((after["act_as_requester"] == true && before["act_as_requester"] != true)
        || rank(after) < rank(before)) {
        return Err(RISK.into());
    }
    Ok(())
}

impl MethodHandler {
    pub(crate) async fn handle_odoo_agent_access_set(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let result = self.set_odoo_agent_access(params, ctx).await;
        match result {
            Ok(value) => WsFrame::ok_response("", value),
            Err(error) => WsFrame::error_response("", &error),
        }
    }

    async fn set_odoo_agent_access(&self, params: Value, ctx: &UserContext) -> Result<Value, String> {
        // An AI/internal context or an API key cannot manufacture browser
        // provenance by supplying a session id in RPC parameters.
        if ctx.origin != duduclaw_auth::acl::ContextOrigin::DashboardSession {
            return Err(LOGIN.into());
        }
        let session = crate::google_personal::session_for(&ctx.user_id).map_err(|_| LOGIN.to_owned())?;
        let db = self.user_db.read().await.clone().ok_or(LOGIN)?;
        super::odoo_access_rpc::fresh(&db, ctx, &session)?;
        if params.as_object().is_some_and(|p| p.values().any(Value::is_null)) {
            return Err(INVALID.into());
        }
        let args: SetAgentAccess = serde_json::from_value(params).map_err(|_| INVALID.to_owned())?;
        if !is_valid_agent_id(&args.agent_id)
            || (args.permission_override.is_none() && args.act_as_requester.is_none())
            || args.permission_override.as_deref().is_some_and(|s| !matches!(s, "inherit" | "engine" | "read_only"))
        {
            return Err(INVALID.into());
        }
        let path = crate::agent_files::file_path(&self.home_dir, &args.agent_id, &crate::agent_files::FileKey::AgentToml)?;
        {
            let registry = self.registry.read().await;
            let agent = registry.get(&args.agent_id).ok_or("Agent not found")?;
            if agent.dir.join("agent.toml") != path { return Err("Agent directory mismatch".into()); }
        }
        let raw = super::config_commit::read_regular_config(&path).map_err(|_| INVALID.to_owned())?;
        let mut table: toml::Table = raw.parse().map_err(|_| INVALID.to_owned())?;
        let before = summary(&table)?;
        let section = super::config_commit::table_at_mut(&mut table, &["odoo"])?;
        if let Some(mode) = &args.permission_override {
            section.insert("permission_override".into(), toml::Value::String(mode.clone()));
        }
        if let Some(requester) = args.act_as_requester {
            section.insert("act_as_requester".into(), toml::Value::Boolean(requester));
        }
        let after = summary(&table)?;
        require_ack(&before, &after, args.acknowledge_risk)?;
        let hash = super::config_commit::content_hash(&raw);
        let commit = crate::agent_files::writer::Commit {
            home: &self.home_dir,
            agent_id: &args.agent_id,
            key: crate::agent_files::FileKey::AgentToml,
            expected_hash: Some(&hash),
            meta: crate::agent_files::writer::CommitMeta::new(&ctx.user_id, "odoo.agent_access_set"),
            baseline: crate::agent_files::writer::Baseline::Accept,
        };
        let outcome = crate::agent_files::writer::commit_odoo_agent_access(
            &self.registry, commit, &table, |current, rendered| {
                let actor = super::odoo_access_rpc::fresh(&db, ctx, &session)?;
                crate::agent_files::file_path(&self.home_dir, &args.agent_id, &crate::agent_files::FileKey::AgentToml)?;
                // Audit the actual artifact validated by the writer under its
                // lock, never merely the caller's proposed table.
                let before = summary(&current.parse::<toml::Table>().map_err(|_| INVALID.to_owned())?)?;
                let after = summary(&rendered.parse::<toml::Table>().map_err(|_| INVALID.to_owned())?)?;
                require_ack(&before, &after, args.acknowledge_risk)?;
                try_append_audit_event(&self.home_dir, &AuditEvent::new(
                    "odoo_agent_access_changed", &args.agent_id, Severity::Info,
                    json!({"actor_user_id":actor.user_id,"agent_id":args.agent_id,
                        "before":before,"after":after,"acknowledged":args.acknowledge_risk,"phase":"before_commit"}),
                )).map_err(|_| "無法寫入 Odoo 權限稽核，這次沒有變更設定。".to_owned())
            },
            || {
                super::odoo_access_rpc::fresh(&db, ctx, &session)?;
                crate::agent_files::file_path(&self.home_dir, &args.agent_id, &crate::agent_files::FileKey::AgentToml)?;
                Ok(())
            },
        ).await.map_err(|e| e.message())?;
        if outcome.registry_reloaded && !outcome.unchanged {
            let _ = crate::channel_reply::agent_config_events().send(args.agent_id.clone());
        }
        Ok(json!({"success":true,"agent_id":args.agent_id,
            "permission_override":after["permission_override"],"act_as_requester":after["act_as_requester"],
            "hot_reloaded":outcome.registry_reloaded}))
    }
}
