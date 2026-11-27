//! RM-141 permission-mode contracts. Gateway decisions alone select the mode.
use super::odoo_access::*;
use super::odoo_identity_client::{IdentityKind, IdentityPermission, OdooIdentity};
use duduclaw_odoo::access_policy::*;
use serde_json::json;

fn identity(permission: IdentityPermission) -> OdooIdentity {
    OdooIdentity { kind: IdentityKind::Personal, permission, user_id: Some("alice".into()),
        odoo_uid: Some(7), login: Some("alice".into()), rev: Some(1), secret: None }
}

fn policy() -> ToolPolicy {
    ToolPolicy {
        agent_id: "bot".into(), agent: duduclaw_odoo::AgentOdooConfig::default(),
        access: OdooAccessConfig::parse_config_toml("[odoo_access]\npermission_mode='follow_odoo_account'\npermission_mode_ack={by='owner',at='2026-11-01T00:00:00Z'}\nalways_refuse=[]").unwrap(),
        global: duduclaw_odoo::OdooConfig::default(),
        identities: PossibleIdentities { avatar_has_owner: true, owner_has_personal_credential: true, ..Default::default() },
    }
}

#[test]
fn all_modes_enforce_fixed_and_admin_refusals() {
    for permission in [IdentityPermission::Follow, IdentityPermission::Engine, IdentityPermission::ReadOnly] {
        let mut p = policy();
        for model in ["ir.attachment", "ir.model.fields", "ir.model", "ir.model.access", "ir.rule", "res.users.apikeys"] {
            let error = p.check_call("odoo_write", &json!({"model":model}), Some(&identity(permission))).unwrap_err();
            assert!(error.starts_with("DuDuClaw 不讓 AI 做這件事："), "{error}");
        }
        p.access.always_refuse = vec!["write:res.partner".into()];
        let error = p.check_call("odoo_write", &json!({"model":"res.partner"}), Some(&identity(permission))).unwrap_err();
        assert!(error.contains("一律不允許"), "{error}");
    }
}

#[test]
fn decision_engine_never_inherits_follow_and_read_only_refuses_write() {
    let p = policy();
    let params = json!({"model":"res.partner"});
    assert!(p.check_call("odoo_write", &params, Some(&identity(IdentityPermission::Follow))).is_ok());
    assert!(p.check_call("odoo_write", &params, Some(&identity(IdentityPermission::Engine))).is_err());
    assert!(p.check_call("odoo_write", &params, Some(&identity(IdentityPermission::ReadOnly))).unwrap_err().contains("唯讀"));
}

#[test]
fn listing_and_dispatch_share_possible_identity_matrix() {
    for personal in [false, true] {
        for own in [false, true] {
            for company in [false, true] {
                for readonly in [false, true] {
                    let mut p = policy();
                    p.identities.owner_has_personal_credential = personal;
                    p.identities.has_agent_credential = own;
                    p.access.company_key_agents = if company { vec!["bot".into()] } else { vec![] };
                    p.agent.permission_override = readonly.then(|| "read_only".into());
                    for tool in ["odoo_search", "odoo_create", "odoo_write", "odoo_execute"] {
                        let params = json!({"model":"res.partner", "method":"write"});
                        assert_eq!(p.listed(tool, Some(&params)), p.precheck(tool, &params).is_ok(),
                            "{tool}: personal={personal}, own={own}, company={company}, readonly={readonly}");
                    }
                    assert!(p.listed("odoo_search", None));
                    assert_eq!(p.listed("odoo_write", None), personal && !readonly);
                }
            }
        }
    }
}

#[test]
fn broken_access_and_unreadable_database_fail_closed() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "[odoo_access]\nalways_refuse=42").unwrap();
    assert!(ToolPolicy::load(home.path(), "bot").is_err());
    let facts = personal_facts(home.path(), "bot");
    assert!(!facts.avatar_has_owner && !facts.owner_has_personal_credential && !facts.requester_has_personal_credential);
    assert!(!home.path().join("users.db").exists());
}

#[test]
fn visitor_never_receives_operator_errors() {
    for session in ["odoolivechat:site:42", "widget-visitor:42"] {
        let value = sanitize_for_session(super::mcp_error("你的 Odoo 帳號（alice）沒有權限；請找管理員"), Some(session));
        let text = value.to_string();
        assert!(!text.contains("alice") && !text.contains("管理員"));
        assert_eq!(value["isError"], true);
    }
    assert!(sanitize_for_session(super::mcp_error("operator"), Some("webchat:owner")).to_string().contains("operator"));
}

/// Deliberately no secret_ct column: availability queries must not need it.
pub(super) fn personal_db(home: &std::path::Path, agent: &str, credential: bool) {
    let db = rusqlite::Connection::open(home.join("users.db")).unwrap();
    db.execute_batch("CREATE TABLE users(id TEXT PRIMARY KEY,status TEXT,avatar_agent_id TEXT,credential_epoch INTEGER);
        CREATE TABLE user_agent_bindings(user_id TEXT,agent_name TEXT,access_level TEXT);
        CREATE TABLE member_odoo_credentials(user_id TEXT,state TEXT,credential_epoch INTEGER);").unwrap();
    db.execute("INSERT INTO users VALUES('alice','active',?1,1)", [agent]).unwrap();
    db.execute("INSERT INTO user_agent_bindings VALUES('alice',?1,'owner')", [agent]).unwrap();
    if credential {
        db.execute("INSERT INTO member_odoo_credentials VALUES('alice','ok',1)", []).unwrap();
    }
}

#[test]
fn availability_uses_active_avatar_binding_and_credential_metadata_only() {
    let home = tempfile::tempdir().unwrap();
    personal_db(home.path(), "bot", true);
    let facts = personal_facts(home.path(), "bot");
    assert!(facts.avatar_has_owner && facts.owner_has_personal_credential);
    assert!(!facts.requester_has_personal_credential);
    let db = rusqlite::Connection::open(home.path().join("users.db")).unwrap();
    db.execute("UPDATE member_odoo_credentials SET credential_epoch=0", []).unwrap();
    assert!(!personal_facts(home.path(), "bot").owner_has_personal_credential);
    db.execute("UPDATE users SET status='suspended'", []).unwrap();
    assert!(!personal_facts(home.path(), "bot").avatar_has_owner);
}

#[test]
fn read_only_intersects_personal_policy_and_hides_execute_even_for_read_method() {
    let mut p = policy();
    p.agent.allowed_models = vec!["crm.lead".into()];
    p.agent.permission_override = Some("read_only".into());
    let read = json!({"model":"res.partner"});
    assert!(p.listed("odoo_search", Some(&read)));
    assert!(p.check_call("odoo_search", &read, Some(&identity(IdentityPermission::ReadOnly))).is_ok());
    assert!(p.check_call("odoo_execute", &json!({"model":"res.partner","method":"read"}),
        Some(&identity(IdentityPermission::ReadOnly))).is_err());
}

#[test]
fn exact_and_segment_refusals_and_feature_switches_apply_in_follow() {
    let mut p = policy();
    p.access.always_refuse = vec!["write:res.partner.*".into()];
    for (model, allowed) in [("res.partner", true), ("res.partner.child", false), ("res.partnership", true)] {
        assert_eq!(p.check_call("odoo_write", &json!({"model":model}),
            Some(&identity(IdentityPermission::Follow))).is_ok(), allowed);
    }
    p.global.features_crm = false;
    assert!(!p.listed("odoo_crm_leads", None));
    assert!(p.check_call("odoo_crm_leads", &json!({}), Some(&identity(IdentityPermission::Follow))).is_err());
}
