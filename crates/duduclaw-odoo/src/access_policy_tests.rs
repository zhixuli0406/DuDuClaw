use super::*;

fn config(body: &str) -> Result<OdooAccessConfig, String> {
    OdooAccessConfig::parse_config_toml(&format!("[odoo_access]\n{body}"))
}

fn follow_config() -> OdooAccessConfig {
    config("permission_mode = 'follow_odoo_account'\npermission_mode_ack = { by = 'owner', at = '2026-11-27T12:00:00+08:00' }").unwrap()
}

fn op<'a>(tool: ToolKind, model: &'a str, method: &'a str, verb: Verb) -> OdooOp<'a> {
    OdooOp { tool, model, method, verb }
}

#[test]
fn config_defaults_and_explicit_empty_are_distinct() {
    let defaults = OdooAccessConfig::default();
    assert_eq!(OdooAccessConfig::parse_config_toml("").unwrap(), defaults);
    assert_eq!(config("").unwrap(), defaults);
    assert_eq!(config("attachment_max_mb = 25").unwrap().always_refuse,
        DEFAULT_ALWAYS_REFUSE.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    assert!(config("always_refuse = []").unwrap().always_refuse.is_empty());
    assert_eq!(defaults.attachment_max_mb, 10);
    assert_eq!(defaults.permission_mode, PermissionMode::Engine);
    assert!(defaults.permission_mode_ack.is_none());
    assert!(defaults.company_key_agents.is_empty());
    assert!(defaults.company_key_agents_migrated.is_empty());
    assert!(config("attachment_max_mb = 1").is_ok());
    assert!(config("company_key_agents = ['Agent-01', 'agent_02']").is_ok());
    let table: toml::Table = "[unrelated]\nvalue = 42\n[odoo_access]\nalways_refuse = []".parse().unwrap();
    assert!(OdooAccessConfig::from_table(&table).unwrap().always_refuse.is_empty());
}

#[test]
fn invalid_config_is_an_error() {
    for body in [
        "typo = true", "permission_mode = 'follow'", "permission_mode = 1",
        "always_refuse = ['read:*']", "always_refuse = ['write:ir..model']",
        "always_refuse = ['method:']", "always_refuse = false", "always_refuse = [2]",
        "attachment_max_mb = 0", "attachment_max_mb = 26", "attachment_max_mb = -1",
        "attachment_max_mb = '10'", "attachment_max_mb = 10.0",
        "company_key_agents = ['../alice']", "company_key_agents = ['']",
        "company_key_agents = ['員工']", "company_key_agents = [1]",
        "company_key_agents = 'alice'", "company_key_agents_migrated = true",
        "permission_mode_ack = { by = 'owner', at = '2026-11-27T00:00:00Z', typo = true }",
    ] {
        assert!(config(body).is_err(), "{body}");
    }
    for raw in ["[odoo_access", "odoo_access = 1", "odoo_access = []"] {
        assert!(OdooAccessConfig::parse_config_toml(raw).is_err(), "{raw}");
    }
    assert!(config(&format!("company_key_agents = ['{}']", "a".repeat(65))).is_err());
}

#[test]
fn missing_or_malformed_ack_downgrades_to_engine() {
    for ack in [
        "", "permission_mode_ack = 'bad'", "permission_mode_ack = {}",
        "permission_mode_ack = { by = 'owner' }",
        "permission_mode_ack = { by = '', at = '2026-11-27T00:00:00Z' }",
        "permission_mode_ack = { by = 42, at = '2026-11-27T00:00:00Z' }",
        "permission_mode_ack = { by = 'owner', at = 'yesterday' }",
        "permission_mode_ack = { by = 'owner', at = '2026-11-27' }",
    ] {
        let access = config(&format!("permission_mode = 'follow_odoo_account'\n{ack}")).unwrap();
        assert_eq!(access.permission_mode, PermissionMode::Engine, "{ack}");
        assert_eq!(effective_mode(IdentityKind::Personal, &access, PermissionOverride::Inherit), EffectiveMode::Engine);
    }
    assert_eq!(follow_config().permission_mode, PermissionMode::FollowOdooAccount);
}

#[test]
fn refuse_grammar_is_strict() {
    for entry in [
        "create:crm.lead", "write:ir.*", "unlink:*", "execute:a.b.*",
        "activity:crm.lead", "note:crm.lead", "message:crm.lead", "attach:crm.lead",
        "*:res.users", "*:*", "method:action_Send_123", "method:_private", "write:0._1",
    ] {
        assert!(RefuseRule::parse(entry).is_ok(), "{entry}");
    }
    for entry in [
        "", "read:*", "search:*", "action:*", "Write:crm.lead", "write:",
        "write:IR.model", "write:ir..model", "write:.ir", "write:ir.",
        "write:ir*", "write:ir.*.x", "write:*.model", "write:**", "write:ir.-x",
        "write:crm.lead:more", " write:crm.lead", "write:crm.lead ",
        "method:*", "method:send.mail", "method:a-b", "method:", "method:寄信",
    ] {
        assert!(RefuseRule::parse(entry).is_err(), "{entry}");
    }
}

#[test]
fn refuse_matching_is_exact_and_segment_aware() {
    let prefix = RefuseRule::parse("write:ir.*").unwrap();
    for (model, expected) in [
        ("ir.model", true), ("ir.attachment", true), ("ir.model.fields", true),
        ("irx.foo", false), ("x.ir.model", false), ("ir", false),
    ] {
        assert_eq!(prefix.matches(&op(ToolKind::Generic, model, "write", Verb::Write)), expected);
    }
    let exact = RefuseRule::parse("write:crm.lead").unwrap();
    assert!(!exact.matches(&op(ToolKind::Generic, "crm.lead.extra", "write", Verb::Write)));
    assert!(!exact.matches(&op(ToolKind::Generic, "crm.lead", "create", Verb::Create)));
    let method = RefuseRule::parse("method:send_mail").unwrap();
    assert!(method.matches(&op(ToolKind::Generic, "crm.lead", "send_mail", Verb::Execute)));
    assert!(!method.matches(&op(ToolKind::Generic, "crm.lead", "xsend_mail", Verb::Execute)));
    assert!(RefuseRule::parse("*:*").unwrap().matches(&op(ToolKind::Generic, "crm.lead", "read", Verb::Read)));
}

#[test]
fn method_verbs_cover_crud_and_workflow() {
    for method in ["read", "search", "search_read", "search_count", "fields_get", "name_get", "name_search", "default_get", "read_group"] {
        assert_eq!(method_verb(method), Verb::Read);
    }
    for (method, verb) in [
        ("create", Verb::Create), ("copy", Verb::Create), ("write", Verb::Write),
        ("update", Verb::Write), ("unlink", Verb::Unlink),
        ("action_confirm", Verb::Execute), ("button_confirm", Verb::Execute),
        ("toggle_active", Verb::Execute), ("custom", Verb::Execute), ("", Verb::Execute),
    ] {
        assert_eq!(method_verb(method), verb, "{method}");
    }
}

#[test]
fn fixed_refusals_and_exemptions() {
    use ToolKind::{Activity, Attachment, Dedicated, Generic, Message, Note};
    for tool in [Generic, Dedicated, Activity, Note, Message, Attachment] {
        for method in ["write", "update", "unlink"] {
            assert_eq!(fixed_refusal(&op(tool, "ir.attachment", method, method_verb(method))), Some(FixedRefusal::F1));
        }
        assert_eq!(fixed_refusal(&op(tool, "ir.attachment", "create", Verb::Create)),
            if tool == Attachment { None } else { Some(FixedRefusal::F1) });
    }
    for (models, refusal) in [
        (&["ir.model", "ir.model.fields", "ir.model.access", "ir.rule"][..], FixedRefusal::F2),
        (&["res.users.apikeys", "res.users.apikeys.extra", "res.users.apikeys.a.b"][..], FixedRefusal::F3),
        (&["mail.mail", "mail.compose.message", "mail.followers", "mail.template"][..], FixedRefusal::F4),
    ] {
        for model in models {
            for verb in [Verb::Create, Verb::Write, Verb::Unlink, Verb::Execute, Verb::Activity, Verb::Note, Verb::Message, Verb::Attach] {
                assert_eq!(fixed_refusal(&op(Generic, model, "custom", verb)), Some(refusal), "{model} {verb:?}");
            }
            assert_eq!(fixed_refusal(&op(Generic, model, "read", Verb::Read)), None);
        }
    }
    for model in ["res.users.apikeysx", "res.users.apikeys_extra", "x.res.users.apikeys", "ir.modelx", "mail.mailx"] {
        assert_eq!(fixed_refusal(&op(Generic, model, "write", Verb::Write)), None);
    }
    for method in ["message_post", "message_notify", "message_subscribe", "send_mail", "action_send_mail"] {
        assert_eq!(fixed_refusal(&op(Generic, "crm.lead", method, Verb::Execute)), Some(FixedRefusal::F4));
    }
    for tool in [Note, Message] {
        assert_eq!(fixed_refusal(&op(tool, "crm.lead", "message_post", Verb::Note)), None);
        assert_eq!(fixed_refusal(&op(tool, "mail.mail", "message_post", Verb::Note)), Some(FixedRefusal::F4));
    }
    for method in ["_private", "_", "init", "uninstall_hook"] {
        assert_eq!(fixed_refusal(&op(Generic, "crm.lead", method, Verb::Execute)), Some(FixedRefusal::F5));
    }
    for method in ["initialize", "uninstall_hook_extra", "x_private", "xmessage_post"] {
        assert_eq!(fixed_refusal(&op(Generic, "crm.lead", method, Verb::Execute)), None);
    }
    assert_eq!(fixed_refusal(&op(Generic, "ir.attachment", "read", Verb::Read)), None);
    assert_eq!(fixed_refusal(&op(Generic, "ir.model", "write", Verb::Read)), Some(FixedRefusal::F2));
}

#[test]
fn overrides_only_narrow() {
    for identity in [IdentityKind::Personal, IdentityKind::Agent, IdentityKind::Company, IdentityKind::NotConnected] {
        for raw in [None, Some("inherit"), Some("engine"), Some("read_only"), Some("follow_odoo_account"), Some("")] {
            let override_ = PermissionOverride::from_config(raw);
            let engine = effective_mode(identity, &OdooAccessConfig::default(), override_);
            assert_ne!(engine, EffectiveMode::Follow);
            let follow = effective_mode(identity, &follow_config(), override_);
            let expected = if override_ == PermissionOverride::ReadOnly {
                EffectiveMode::ReadOnly
            } else if identity == IdentityKind::Personal && override_ == PermissionOverride::Inherit {
                EffectiveMode::Follow
            } else {
                EffectiveMode::Engine
            };
            assert_eq!(follow, expected, "{identity:?} {raw:?}");
        }
    }
}

#[test]
fn agent_new_keys_and_dedicated_opt_ins() {
    let default = AgentOdooConfig::default();
    assert!(!default.act_as_requester);
    assert_eq!(default.permission_override, None);
    assert!(default.is_empty());
    for token in ["activity", "note", "message", "attach"] {
        for (actions, expected) in [
            (vec![], false), (vec![token.to_string()], false),
            (vec![format!("{token}:*")], false), (vec![format!("{token}:crm.lead")], true),
            (vec![format!("{token}:crm.lead.extra")], false),
        ] {
            let agent = AgentOdooConfig { allowed_actions: actions, ..Default::default() };
            assert_eq!(agent.dedicated_opt_in(token, "crm.lead"), expected, "{token}");
            assert!(!agent.dedicated_opt_in(token, "crm..lead"));
        }
    }
    for body in ["permission_override = 'inherit'", "permission_override = 'unknown'", "act_as_requester = true"] {
        let agent = AgentOdooConfig::parse_agent_toml(&format!("[odoo]\n{body}")).unwrap().unwrap();
        assert!(!agent.is_empty());
        let roundtrip: AgentOdooConfig = toml::from_str(&toml::to_string(&agent).unwrap()).unwrap();
        assert_eq!(agent, roundtrip);
    }
    assert!(AgentOdooConfig::parse_agent_toml("[odoo]\npermission_override = 1").is_err());
    assert!(AgentOdooConfig::parse_agent_toml("[odoo]\nact_as_requester = 'yes'").is_err());
}

fn listed(
    tool: ToolClass, model: Option<&str>, method: Option<&str>,
    agent: &AgentOdooConfig, access: &OdooAccessConfig, identities: PossibleIdentities,
) -> bool {
    odoo_tool_listable(&ListableInput { tool, model, method, agent_id: "worker", agent, access, identities })
}

#[test]
fn listable_identity_and_mode_matrix() {
    let agent = AgentOdooConfig::default();
    let own = PossibleIdentities { has_agent_credential: true, ..Default::default() };
    let avatar = PossibleIdentities { avatar_has_owner: true, owner_has_personal_credential: true, ..Default::default() };
    let missing = PossibleIdentities { avatar_has_owner: true, ..Default::default() };
    let none = PossibleIdentities::default();
    for access in [OdooAccessConfig::default(), follow_config()] {
        for (identities, read, write) in [
            (none, false, false), (own, true, false), (missing, true, false),
            (avatar, true, access.permission_mode == PermissionMode::FollowOdooAccount),
        ] {
            assert_eq!(listed(ToolClass::Read, None, None, &agent, &access, identities), read);
            assert_eq!(listed(ToolClass::GenericWrite, None, None, &agent, &access, identities), write);
        }
    }
    let company = config("company_key_agents = ['worker']").unwrap();
    assert!(listed(ToolClass::Read, None, None, &agent, &company, none));
    assert!(!listed(ToolClass::Read, None, None, &agent, &config("company_key_agents = ['worker-extra']").unwrap(), none));
    let requester = AgentOdooConfig { act_as_requester: true, ..Default::default() };
    assert!(listed(ToolClass::Read, None, None, &requester, &follow_config(), none));
    assert!(!listed(ToolClass::Message, None, None, &requester, &follow_config(), none));
    let requester_key = PossibleIdentities { requester_has_personal_credential: true, ..Default::default() };
    assert!(listed(ToolClass::Message, None, None, &requester, &follow_config(), requester_key));
    assert!(!listed(ToolClass::Message, None, None, &agent, &follow_config(), requester_key));
    for override_ in ["read_only", "invalid"] {
        let restricted = AgentOdooConfig { permission_override: Some(override_.into()), ..Default::default() };
        assert!(listed(ToolClass::Read, None, None, &restricted, &follow_config(), avatar));
        for tool in [ToolClass::GenericCreate, ToolClass::GenericWrite, ToolClass::Activity, ToolClass::Note, ToolClass::Message, ToolClass::Attach, ToolClass::Execute] {
            assert!(!listed(tool, None, None, &restricted, &follow_config(), avatar), "{tool:?}");
        }
    }
}

#[test]
fn listable_grants_and_denials_matrix() {
    let own = PossibleIdentities { has_agent_credential: true, ..Default::default() };
    let avatar = PossibleIdentities { avatar_has_owner: true, owner_has_personal_credential: true, ..Default::default() };
    for (tool, actions) in [
        (ToolClass::GenericCreate, vec!["create", "generic_create:crm.lead"]),
        (ToolClass::GenericWrite, vec!["write:crm.lead", "generic_write:crm.lead"]),
        (ToolClass::Activity, vec!["activity:crm.lead"]),
        (ToolClass::Note, vec!["note:crm.lead"]),
        (ToolClass::Message, vec!["message:crm.lead"]),
        (ToolClass::Attach, vec!["attach:crm.lead"]),
        (ToolClass::Execute, vec!["action:crm.lead"]),
    ] {
        let agent = AgentOdooConfig {
            allowed_models: vec!["crm.lead".into()],
            allowed_actions: actions.into_iter().map(str::to_owned).collect(),
            ..Default::default()
        };
        assert!(listed(tool, None, None, &agent, &OdooAccessConfig::default(), own), "{tool:?}");
        assert!(!listed(tool, Some("sale.order"), None, &agent, &OdooAccessConfig::default(), own), "{tool:?}");
        assert!(listed(tool, Some("sale.order"), None, &agent, &follow_config(), avatar), "{tool:?}");
        assert!(!listed(tool, None, None, &agent, &config("always_refuse = ['*:*']").unwrap(), own));
    }
    let agent = AgentOdooConfig::default();
    let follow = follow_config();
    for (tool, model, method) in [
        (ToolClass::GenericWrite, "ir.attachment", "write"),
        (ToolClass::GenericWrite, "ir.model.fields", "write"),
        (ToolClass::GenericCreate, "res.users.apikeys", "create"),
        (ToolClass::Execute, "crm.lead", "message_post"),
        (ToolClass::Execute, "crm.lead", "_private"),
        (ToolClass::Execute, "crm.lead", "execute_import"),
        (ToolClass::Execute, "ir.model", "action_confirm"),
        (ToolClass::Read, "crm.lead", "write"),
        (ToolClass::Note, "crm.lead", "send_mail"),
    ] {
        assert!(!listed(tool, Some(model), Some(method), &agent, &follow, avatar), "{tool:?} {model} {method}");
    }
    assert!(listed(ToolClass::Attach, Some("crm.lead"), None, &agent, &follow, avatar));
    assert!(listed(ToolClass::Note, Some("crm.lead"), None, &agent, &follow, avatar));
    assert!(!listed(ToolClass::Read, None, None, &agent, &config("always_refuse = ['*:*']").unwrap(), own));
    assert!(!listed(ToolClass::Read, Some("bad..model"), None, &agent, &follow, avatar));
}

#[test]
fn readonly_and_missing_credentials_do_not_bypass_engine_or_refuse_rules() {
    let agent = AgentOdooConfig {
        permission_override: Some("read_only".into()),
        allowed_models: vec!["sale.order".into()], allowed_actions: vec!["write".into()],
        ..Default::default()
    };
    let own = PossibleIdentities { has_agent_credential: true, ..Default::default() };
    let missing = PossibleIdentities { avatar_has_owner: true, ..Default::default() };
    assert!(!listed(ToolClass::Read, None, None, &agent, &OdooAccessConfig::default(), own));
    assert!(!listed(ToolClass::Read, None, None, &agent, &OdooAccessConfig::default(), missing));
    assert!(listed(ToolClass::Read, None, None, &agent, &follow_config(), missing));
    let missing_with_own = PossibleIdentities { has_agent_credential: true, ..missing };
    assert!(!listed(ToolClass::Read, None, None, &agent, &follow_config(), missing_with_own));
    let blocked = config("permission_mode = 'follow_odoo_account'\npermission_mode_ack = { by = 'owner', at = '2026-11-27T00:00:00Z' }\nalways_refuse = ['*:*']").unwrap();
    assert!(!listed(ToolClass::Read, None, None, &agent, &blocked, missing));
}

#[test]
fn administrator_rules_apply_to_dedicated_verbs_and_all_modes() {
    let own = PossibleIdentities { has_agent_credential: true, ..Default::default() };
    let avatar = PossibleIdentities { avatar_has_owner: true, owner_has_personal_credential: true, ..Default::default() };
    for (tool, token, method) in [
        (ToolClass::Activity, "activity", "activity_schedule"),
        (ToolClass::Note, "note", "message_post"),
        (ToolClass::Message, "message", "message_post"),
        (ToolClass::Attach, "attach", "create"),
    ] {
        let agent = AgentOdooConfig {
            allowed_models: vec!["crm.lead".into()],
            allowed_actions: vec![format!("{token}:crm.lead")],
            ..Default::default()
        };
        for rule in [format!("{token}:crm.*"), format!("method:{method}")] {
            for (mut access, identities) in [(OdooAccessConfig::default(), own), (follow_config(), avatar)] {
                access.always_refuse = vec![rule.clone()];
                assert!(!listed(tool, Some("crm.lead"), Some(method), &agent, &access, identities));
            }
        }
    }
    let mut access = follow_config();
    access.always_refuse = vec!["execute:*".into()];
    assert!(!listed(ToolClass::Execute, Some("crm.lead"), Some("action_confirm"), &AgentOdooConfig::default(), &access, avatar));
    access.always_refuse = vec!["broken".into()];
    assert!(access.always_refuses(&op(ToolKind::Generic, "crm.lead", "read", Verb::Read)));
    assert!(!listed(ToolClass::Read, None, None, &AgentOdooConfig::default(), &access, avatar));
}

#[test]
fn existential_listing_has_no_hardcoded_business_model_or_method() {
    let avatar = PossibleIdentities { avatar_has_owner: true, owner_has_personal_credential: true, ..Default::default() };
    let mut access = follow_config();
    access.always_refuse = vec![
        "*:crm.*".into(), "*:sale.*".into(), "*:policy_model_0".into(),
        "*:policy_model_1.*".into(), "method:policy_execute_0".into(),
        "method:action_policy_0".into(),
    ];
    let agent = AgentOdooConfig {
        allowed_models: vec!["crm.lead".into()],
        allowed_actions: vec!["action:crm.lead".into()], ..Default::default()
    };
    assert!(listed(ToolClass::GenericWrite, None, None, &agent, &access, avatar));
    assert!(listed(ToolClass::Execute, None, None, &agent, &access, avatar));
    assert!(!listed(ToolClass::GenericWrite, Some("crm.lead"), None, &agent, &access, avatar));
    let own = PossibleIdentities { has_agent_credential: true, ..Default::default() };
    assert!(!listed(ToolClass::Execute, None, None, &agent, &access, own));
}

#[test]
fn user_messages_are_plain_and_stable() {
    for refusal in [FixedRefusal::F1, FixedRefusal::F2, FixedRefusal::F3, FixedRefusal::F4, FixedRefusal::F5] {
        assert_eq!(refusal.code(), format!("{refusal:?}"));
        assert_eq!(fixed_refusal_message(refusal), format!("DuDuClaw 不讓 AI 做這件事：{}。", refusal.reason()));
    }
    assert_eq!(list_refusal_message("刪除資料"), "公司設定的『一律不允許』清單不允許 AI 做這件事（刪除資料）。");
    assert_eq!(not_connected_message(true), "你還沒有連接自己的 Odoo 帳號，請到『我的連線』連接。");
    assert_eq!(not_connected_message(false), "這位 AI 員工沒有連接 Odoo，請管理員到 Odoo 設定指定帳號。");
}
