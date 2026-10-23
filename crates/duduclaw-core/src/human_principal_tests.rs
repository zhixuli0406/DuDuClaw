use super::*;

#[test]
fn user_principals_round_trip() {
    let p = HumanPrincipal::parse("user:u-123_AB").unwrap();
    assert_eq!(p, HumanPrincipal::User("u-123_AB".into()));
    assert_eq!(p.to_string(), "user:u-123_AB");
    assert_eq!(p.user_id(), Some("u-123_AB"));
}

#[test]
fn unmapped_principals_round_trip() {
    let p = HumanPrincipal::parse("unmapped:line:0123456789abcdef").unwrap();
    assert_eq!(p.to_string(), "unmapped:line:0123456789abcdef");
    assert!(p.user_id().is_none());
    assert!(HumanPrincipal::parse("unmapped:telegram:unavailable").is_ok());
}

#[test]
fn system_sender_names_are_refused() {
    for name in ["dashboard", "cron", "heartbeat", "webhook", "autopilot", "goal-loop-driver", "system", "default", "a2a-client", "__deferred_gvu__", "heartbeat-scheduler"] {
        let err = HumanPrincipal::parse(&format!("user:{name}")).unwrap_err();
        assert_eq!(err.reason, "system_sender", "{name}");
        assert!(HumanPrincipal::user(name).is_none(), "{name}");
    }
}

#[test]
fn malformed_values_are_refused_with_a_reason() {
    let cases = [
        ("u1", "missing_prefix"),
        ("dashboard", "missing_prefix"),
        ("user:", "invalid_user_id"),
        ("user:a b", "invalid_user_id"),
        ("user:王小明", "invalid_user_id"),
        ("user:x\ny", "invalid_user_id"),
        ("unmapped:line", "invalid_unmapped"),
        ("unmapped:LINE:0123456789abcdef", "invalid_unmapped"),
        ("unmapped:line:0123456789ABCDEF", "invalid_unmapped"),
        ("unmapped:line:0123", "invalid_unmapped"),
        ("unmapped::0123456789abcdef", "invalid_unmapped"),
    ];
    for (raw, reason) in cases {
        assert_eq!(HumanPrincipal::parse(raw).unwrap_err().reason, reason, "{raw}");
    }
    let long = format!("user:{}", "a".repeat(MAX_USER_ID_LEN + 1));
    assert!(HumanPrincipal::parse(&long).is_err());
    assert!(HumanPrincipal::parse(&format!("user:{}", "a".repeat(MAX_USER_ID_LEN))).is_ok());
}

#[test]
fn rejection_preview_is_char_safe_and_capped() {
    let raw = format!("user:{}", "測".repeat(100));
    let err = HumanPrincipal::parse(&raw).unwrap_err();
    assert_eq!(err.preview.chars().count(), REJECTED_PREVIEW_CHARS);
}

#[test]
fn mode_defaults_to_record_and_typos_enforce() {
    assert_eq!(HumanPrincipalMode::from_config_value(None).0, HumanPrincipalMode::Record);
    assert_eq!(HumanPrincipalMode::from_config_value(Some("record")).0, HumanPrincipalMode::Record);
    assert_eq!(HumanPrincipalMode::from_config_value(Some("ENFORCE")).0, HumanPrincipalMode::Enforce);
    let (mode, warning) = HumanPrincipalMode::from_config_value(Some("enforced"));
    assert_eq!(mode, HumanPrincipalMode::Enforce);
    assert!(warning.unwrap().contains("enforce"));
    let table: toml::Table = "[delegation]\nhuman_principal = 3\n".parse().unwrap();
    assert_eq!(HumanPrincipalMode::from_table(&table).0, HumanPrincipalMode::Enforce);
    let table: toml::Table = "[delegation]\npolicy = \"open\"\n".parse().unwrap();
    assert_eq!(HumanPrincipalMode::from_table(&table).0, HumanPrincipalMode::Record);
    for raw in ["delegation = 'bad'", "delegation = []", "delegation = false"] {
        let table: toml::Table = raw.parse().unwrap();
        let (mode, warning) = HumanPrincipalMode::from_table(&table);
        assert_eq!(mode, HumanPrincipalMode::Enforce, "{raw}");
        assert!(warning.is_some(), "{raw}");
    }
}

#[test]
fn mode_from_home_fails_closed_on_a_broken_config() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(HumanPrincipalMode::from_home(home.path()), HumanPrincipalMode::Record);
    std::fs::write(home.path().join("config.toml"), "[delegation\n").unwrap();
    assert_eq!(HumanPrincipalMode::from_home(home.path()), HumanPrincipalMode::Enforce);
    std::fs::write(home.path().join("config.toml"), "[delegation]\nhuman_principal = \"enforce\"\n").unwrap();
    assert_eq!(HumanPrincipalMode::from_home(home.path()), HumanPrincipalMode::Enforce);
}

#[test]
fn delegation_paths_map_to_rights() {
    assert_eq!(human_right_for_path("send_to_agent"), HumanRight::Call);
    assert_eq!(human_right_for_path("spawn_agent"), HumanRight::Call);
    assert_eq!(human_right_for_path("sqlite_queue"), HumanRight::Call);
    assert_eq!(human_right_for_path("tasks_create"), HumanRight::Assign);
    assert_eq!(human_right_for_path("mail_read"), HumanRight::Assign);
    assert_eq!(human_right_for_path("something_new"), HumanRight::Assign);
}

#[test]
fn the_variable_is_not_on_the_spawn_allowlist() {
    // The gateway sets it per spawn; an ambient value on the gateway's own
    // environment must never reach an employee CLI.
    assert!(!crate::AGENT_CLI_ENV_ALLOWLIST.contains(&ENV_ON_BEHALF_OF));
}

#[test]
fn the_denial_names_the_person_not_the_employee() {
    let denied = crate::DelegationDenied {
        sender: "sales-lead".into(),
        target: "finance".into(),
        policy: crate::DelegationPolicy::Department,
        reason: crate::DelegationDenyReason::HumanPrincipalNotPermitted,
    };
    assert_eq!(denied.reason.as_str(), "human_principal_not_permitted");
    let text = denied.message_zh();
    assert!(text.contains("發出這個指令的人沒有權限把工作交給「finance」"), "{text}");
}
