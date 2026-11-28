//! RM-141 permission-mode contracts. Gateway decisions alone select the mode.
use super::odoo_access::*;
use super::odoo_identity_client::{IdentityKind, IdentityPermission, OdooIdentity};
use duduclaw_odoo::access_policy::*;
use serde_json::json;

fn identity(permission: IdentityPermission) -> OdooIdentity {
    OdooIdentity { kind: IdentityKind::Personal, permission, user_id: Some("alice".into()),
        url: None, db: None, server_fp: None, odoo_uid: Some(7), login: Some("alice".into()), rev: Some(1), secret: None }
}

fn policy() -> ToolPolicy {
    ToolPolicy {
        person_key: false,
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
        for model in ["ir.attachment", "ir.model.fields", "ir.model", "ir.model.access", "ir.model.data", "ir.model.constraint", "mail.template", "mail.message", "ir.rule", "res.users.apikeys"] {
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
fn bound_preconnect_intersects_all_engine_gates_even_with_possible_follow() {
    let mut p = policy();
    // Exercise the explicit bound API without relying on an isolation task local.
    let read = json!({"model":"crm.lead","method":"read"});
    assert!(p.data_preconnect_engine("odoo_execute", &read).is_ok());
    for (model, method) in [("crm.lead", "unlink"), ("res.partner", "read"),
        ("ir.model", "write"), ("res.users.apikeys", "create"), ("crm.lead", "message_post")] {
        assert!(p.data_preconnect_engine("odoo_execute", &json!({"model":model,"method":method})).is_err());
    }
    p.agent.allowed_models = vec!["sale.order".into()];
    assert!(p.data_preconnect("odoo_execute", &read, None).is_ok());
    assert!(p.data_preconnect_engine("odoo_execute", &read).is_err());
    p.agent.allowed_models.clear();
    p.agent.allowed_actions = vec!["write:crm.lead".into()];
    assert!(p.data_preconnect_engine("odoo_execute", &read).is_err());
    p.agent.allowed_actions.clear();
    p.access.always_refuse = vec!["read:crm.lead".into()];
    assert!(p.data_preconnect_engine("odoo_execute", &read).unwrap_err().contains("一律不允許"));
    p.access.always_refuse.clear();
    p.global.features_crm = false;
    assert!(p.data_preconnect_engine("odoo_crm_leads", &json!({})).is_err());
    p.global.features_crm = true;
    let attach = json!({"model":"crm.lead","res_id":9,"file":"x.pdf"});
    assert!(p.data_preconnect("odoo_attachment_add", &attach, None).is_ok());
    assert!(p.data_preconnect_engine("odoo_attachment_add", &attach).is_err());
    p.agent.allowed_actions = vec!["attach:crm.lead".into(), "attach:res.partner".into()];
    assert!(p.data_preconnect_engine("odoo_attachment_add", &attach).is_ok());
    assert!(p.data_preconnect_engine("odoo_attachment_add",
        &json!({"model":"res.partner","res_id":9,"file":"x.pdf"})).is_err());
    assert!(p.data_preconnect_engine("odoo_unknown", &json!({})).is_err());
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
                    for tool in ["odoo_search", "odoo_create", "odoo_write", "odoo_execute",
                        "odoo_activity_create", "odoo_note_post", "odoo_message_send", "odoo_attachment_add"] {
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
fn writeback_policy_uses_semantic_verbs_and_exact_dedicated_tokens() {
    let params = json!({"model":"crm.lead", "res_id":1, "summary":"test", "body":"test"});
    for (tool, verb) in [("odoo_activity_create", "activity"), ("odoo_note_post", "note"), ("odoo_message_send", "message"), ("odoo_attachment_add", "attach")] {
        let mut p = policy();
        assert!(p.listed(tool, None));
        assert!(p.check_call(tool, &params, Some(&identity(IdentityPermission::Follow))).is_ok());
        p.access.permission_mode = PermissionMode::Engine;
        p.agent.allowed_models = vec!["crm.lead".into()];
        for token in [verb.to_string(), format!("{verb}:*"), format!("{verb}:crm.lead.extra")] {
            p.agent.allowed_actions = vec![token];
            assert!(!p.listed(tool, None));
            assert!(p.check_call(tool, &params, Some(&identity(IdentityPermission::Engine))).is_err());
        }
        p.agent.allowed_actions = vec![format!("{verb}:crm.lead")];
        assert!(p.listed(tool, None));
        assert!(p.check_call(tool, &params, Some(&identity(IdentityPermission::Engine))).is_ok());
        assert!(p.check_call(tool, &params, Some(&identity(IdentityPermission::ReadOnly))).is_err());
        p.access.permission_mode = PermissionMode::FollowOdooAccount;
        p.access.always_refuse = vec![format!("{verb}:*")];
        assert!(!p.listed(tool, None));
        assert!(p.check_call(tool, &params, Some(&identity(IdentityPermission::Follow))).unwrap_err().contains("一律不允許"));
    }
    let p = policy();
    assert!(p.check_call("odoo_execute", &json!({"model":"crm.lead","method":"message_post"}),
        Some(&identity(IdentityPermission::Follow))).unwrap_err().contains("DuDuClaw 不讓"));
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

#[test]
fn person_policy_never_uses_employee_tables_or_engine() {
    let mut p = policy();
    p.person_key = true;
    p.agent.allowed_models = vec!["employee.only".into()];
    p.agent.allowed_actions = vec!["read:employee.only".into()];
    p.access.always_refuse = vec!["unlink:*".into()];
    for permission in [IdentityPermission::Follow, IdentityPermission::ReadOnly] {
        let decision = identity(permission);
        assert!(p.check_call("odoo_search", &json!({"model":"res.partner"}), Some(&decision)).is_ok());
        for tool in ["odoo_create", "odoo_write", "odoo_activity_create", "odoo_note_post"] {
            assert_eq!(p.check_call(tool, &json!({"model":"res.partner"}), Some(&decision)).is_ok(),
                permission == IdentityPermission::Follow, "{tool}");
        }
        assert!(p.common("res.partner", "unlink").unwrap_err().contains("一律不允許"));
        for tool in ["odoo_execute", "odoo_report", "odoo_connect", "odoo_message_send", "odoo_attachment_add"] {
            assert!(p.check_call(tool, &json!({"model":"res.partner"}), Some(&decision)).is_err());
        }
    }
    let follow = identity(IdentityPermission::Follow);
    for model in ["ir.model", "ir.model.fields", "ir.model.access", "ir.model.data", "ir.model.constraint", "mail.template", "mail.message", "ir.rule"] {
        assert!(p.check_call("odoo_write", &json!({"model":model}), Some(&follow)).unwrap_err()
            .starts_with("DuDuClaw 不讓 AI 做這件事："));
    }
    assert!(p.check_call("odoo_search", &json!({"model":"res.partner"}), None).is_err());
    assert!(p.check_call("odoo_search", &json!({"model":"res.partner"}), Some(&identity(IdentityPermission::Engine))).is_err());
}


#[test]
fn preconnect_distinguishes_member_credentials_off_from_missing_connection() {
    let mut p = policy();
    p.identities.owner_has_personal_credential = false;
    let params = json!({"model":"crm.lead"});
    assert_eq!(p.data_preconnect("odoo_search", &params, None).unwrap_err(), MEMBER_CREDENTIALS_OFF);
    p.global.member_credentials = true;
    assert_eq!(p.data_preconnect("odoo_search", &params, None).unwrap_err(),
        format!("Odoo not connected. {}", not_connected_message(true)));
    p.global.member_credentials = false;
    p.identities.avatar_has_owner = false;
    p.agent.act_as_requester = true;
    assert_eq!(p.data_preconnect("odoo_search", &params, None).unwrap_err(), MEMBER_CREDENTIALS_OFF);
    p.agent.act_as_requester = false;
    assert_eq!(p.data_preconnect("odoo_search", &params, None).unwrap_err(),
        format!("Odoo not connected. {}", not_connected_message(false)));
}
