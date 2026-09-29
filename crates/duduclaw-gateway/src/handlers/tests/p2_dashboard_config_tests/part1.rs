//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

use super::*;

// ── ODO: qualified-action parse ──────────────────────────────────────────

#[test]
pub(super) fn odo_action_accepts_bare_and_qualified() {
    assert!(odo_valid_action("read"));
    assert!(odo_valid_action("write"));
    assert!(odo_valid_action("write:crm.lead"));
    assert!(odo_valid_action("execute:sale.order"));
}

#[test]
pub(super) fn odo_action_rejects_bad_verb_and_model() {
    assert!(!odo_valid_action("destroy"));
    assert!(!odo_valid_action("write:"));
    assert!(!odo_valid_action("write:bad model!"));
    assert!(!odo_valid_action(""));
}

#[test]
pub(super) fn odo_apply_encrypts_secret_and_keeps_qualified_action() {
    let tmp = std::env::temp_dir().join(format!("ddc-odo-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).unwrap();
    let mut table = toml::Table::new();
    let params = json!({
        "odoo": {
            "profile": "sales",
            "allowed_actions": ["read", "write:crm.lead"],
            "company_ids": [1, 2],
            "api_key": "super-secret",
        }
    });
    let changes = apply_odoo_to_table(&mut table, &params, &tmp).expect("apply");
    let odoo = table.get("odoo").unwrap().as_table().unwrap();
    // Cleartext api_key must NOT be present; only api_key_enc.
    assert!(odoo.get("api_key").is_none());
    assert!(odoo.get("api_key_enc").is_some());
    // Qualified action preserved.
    let actions: Vec<&str> = odoo
        .get("allowed_actions")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(actions.contains(&"write:crm.lead"));
    assert_eq!(
        odoo.get("company_ids").unwrap().as_array().unwrap().len(),
        2
    );
    assert!(changes.iter().any(|c| c.contains("[ENCRYPTED]")));
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
pub(super) fn odo_apply_stores_secret_reference_raw() {
    // A `secret://` value is a pointer, not a secret — it must be stored
    // verbatim into `*_enc` (NOT AES-encrypted), so the connector pool can
    // resolve it via the SecretManager at connect time.
    let tmp = std::env::temp_dir().join(format!("ddc-odo-secret-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).unwrap();
    let mut table = toml::Table::new();
    let params = json!({
        "odoo": {
            "api_key": "secret://vault/odoo-api-key",
            "password": "secret://vault/odoo-password",
        }
    });
    let changes = apply_odoo_to_table(&mut table, &params, &tmp).expect("apply");
    let odoo = table.get("odoo").unwrap().as_table().unwrap();
    // Stored raw, NOT encrypted.
    assert_eq!(
        odoo.get("api_key_enc").and_then(|v| v.as_str()),
        Some("secret://vault/odoo-api-key")
    );
    assert_eq!(
        odoo.get("password_enc").and_then(|v| v.as_str()),
        Some("secret://vault/odoo-password")
    );
    // No cleartext mirror left behind.
    assert!(odoo.get("api_key").is_none());
    assert!(odoo.get("password").is_none());
    assert!(changes.iter().any(|c| c.contains("[SECRET REF]")));
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
pub(super) fn odo_apply_rejects_bad_action() {
    let mut table = toml::Table::new();
    let params = json!({ "odoo": { "allowed_actions": ["nuke:crm.lead"] } });
    let tmp = std::env::temp_dir();
    assert!(apply_odoo_to_table(&mut table, &params, &tmp).is_err());
}

// ── ODO §2: per-agent override url/db validation + mask non-overwrite ──────

#[test]
pub(super) fn odo_apply_rejects_ssrf_url_and_bad_db() {
    let tmp = std::env::temp_dir();
    // http:// to a non-localhost host must be refused (SSRF/HTTPS gate).
    let mut t1 = toml::Table::new();
    assert!(
        apply_odoo_to_table(
            &mut t1,
            &json!({ "odoo": { "url": "http://169.254.169.254/latest" } }),
            &tmp,
        )
        .is_err()
    );
    // https:// to a private IP must be refused.
    let mut t2 = toml::Table::new();
    assert!(
        apply_odoo_to_table(
            &mut t2,
            &json!({ "odoo": { "url": "https://10.0.0.5" } }),
            &tmp,
        )
        .is_err()
    );
    // Bad db name must be refused.
    let mut t3 = toml::Table::new();
    assert!(
        apply_odoo_to_table(&mut t3, &json!({ "odoo": { "db": "bad db!" } }), &tmp,).is_err()
    );
    // A safe https url + clean db round-trips.
    let mut t4 = toml::Table::new();
    let changes = apply_odoo_to_table(
        &mut t4,
        &json!({ "odoo": { "url": "https://odoo.example.com", "db": "prod_db" } }),
        &tmp,
    )
    .expect("safe config applies");
    let odoo = t4.get("odoo").unwrap().as_table().unwrap();
    assert_eq!(
        odoo.get("url").unwrap().as_str(),
        Some("https://odoo.example.com")
    );
    assert_eq!(odoo.get("db").unwrap().as_str(), Some("prod_db"));
    assert!(changes.iter().any(|c| c.contains("odoo.url")));
}

#[test]
pub(super) fn odo_apply_masked_placeholder_does_not_overwrite_real_secret() {
    let tmp = std::env::temp_dir().join(format!("ddc-odo-mask-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).unwrap();
    // 1) Store a real secret.
    let mut table = toml::Table::new();
    apply_odoo_to_table(
        &mut table,
        &json!({ "odoo": { "api_key": "real-secret-value" } }),
        &tmp,
    )
    .expect("store real");
    let enc_before = table
        .get("odoo")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("api_key_enc"))
        .and_then(|v| v.as_str())
        .unwrap()
        .to_string();
    assert!(!enc_before.is_empty());

    // 2) Re-apply with the masked placeholder — the stored ciphertext must
    //    NOT be replaced (mask is not a real secret).
    apply_odoo_to_table(
        &mut table,
        &json!({ "odoo": { "api_key": SECRET_MASK_SET } }),
        &tmp,
    )
    .expect("mask no-op");
    let enc_after = table
        .get("odoo")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("api_key_enc"))
        .and_then(|v| v.as_str())
        .unwrap();
    assert_eq!(
        enc_before, enc_after,
        "masked placeholder must not clobber real secret"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

// ── CAP §6: native_sandbox + Progent policy round-trip / fail-closed ──────

#[test]
pub(super) fn cap_native_sandbox_and_policy_round_trip() {
    let mut table = toml::Table::new();
    let params = json!({
        "capabilities": {
            "native_sandbox": true,
            "policy": [
                {
                    "tool": "shell_exec",
                    "effect": "forbid",
                    "when": [{ "arg": "command", "op": "contains", "value": "rm -rf" }],
                },
                { "tool": "*", "effect": "allow" },
            ],
        }
    });
    let changes = apply_capabilities_to_table(&mut table, &params).expect("apply");
    let cap = table.get("capabilities").unwrap().as_table().unwrap();
    assert_eq!(cap.get("native_sandbox").unwrap().as_bool(), Some(true));
    let policy = cap.get("policy").unwrap().as_array().unwrap();
    assert_eq!(policy.len(), 2);
    let first = policy[0].as_table().unwrap();
    assert_eq!(first.get("tool").unwrap().as_str(), Some("shell_exec"));
    assert_eq!(first.get("effect").unwrap().as_str(), Some("forbid"));
    let when = first.get("when").unwrap().as_array().unwrap();
    assert_eq!(
        when[0].as_table().unwrap().get("op").unwrap().as_str(),
        Some("contains")
    );
    assert!(changes.iter().any(|c| c.contains("native_sandbox")));
    assert!(changes.iter().any(|c| c.contains("policy = [2 rules]")));

    // The written section must deserialize back into a real
    // CapabilitiesConfig (proves the on-disk shape is valid, not just JSON).
    let cfg: duduclaw_core::types::CapabilitiesConfig = cap
        .clone()
        .try_into()
        .expect("deserializes into CapabilitiesConfig");
    assert!(cfg.native_sandbox);
    assert_eq!(cfg.policy.len(), 2);
}

#[test]
pub(super) fn cap_policy_fail_closed_on_bad_effect_op_and_missing_tool() {
    // Unknown effect.
    let mut t1 = toml::Table::new();
    assert!(
        apply_capabilities_to_table(
            &mut t1,
            &json!({ "capabilities": { "policy": [{ "tool": "x", "effect": "nuke" }] } }),
        )
        .is_err()
    );
    // Unknown op.
    let mut t2 = toml::Table::new();
    assert!(apply_capabilities_to_table(
        &mut t2,
        &json!({ "capabilities": { "policy": [
            { "tool": "x", "effect": "allow", "when": [{ "arg": "a", "op": "regex", "value": "b" }] }
        ] } }),
    )
    .is_err());
    // Missing/empty tool.
    let mut t3 = toml::Table::new();
    assert!(
        apply_capabilities_to_table(
            &mut t3,
            &json!({ "capabilities": { "policy": [{ "tool": "", "effect": "allow" }] } }),
        )
        .is_err()
    );
}

// ── v1.39: os_native capability + [os_watch] table ───────────────────────

#[test]
pub(super) fn cap_os_native_round_trips_into_capabilities_config() {
    let mut table = toml::Table::new();
    let changes = apply_capabilities_to_table(
        &mut table,
        &json!({ "capabilities": { "os_native": true } }),
    )
    .expect("apply");
    let cap = table.get("capabilities").unwrap().as_table().unwrap();
    assert_eq!(cap.get("os_native").unwrap().as_bool(), Some(true));
    assert!(changes.iter().any(|c| c.contains("os_native = true")));
    // Must deserialize back into a real CapabilitiesConfig.
    let cfg: duduclaw_core::types::CapabilitiesConfig = cap
        .clone()
        .try_into()
        .expect("deserializes into CapabilitiesConfig");
    assert!(cfg.os_native);
}

// ── WP3.3: recording capability (dashboard toggle) ───────────────────────

#[test]
pub(super) fn cap_recording_round_trips_into_capabilities_config() {
    let mut table = toml::Table::new();
    let changes = apply_capabilities_to_table(
        &mut table,
        &json!({ "capabilities": { "recording": true } }),
    )
    .expect("apply");
    let cap = table.get("capabilities").unwrap().as_table().unwrap();
    assert_eq!(cap.get("recording").unwrap().as_bool(), Some(true));
    assert!(changes.iter().any(|c| c.contains("recording = true")));
    // Must deserialize back into a real CapabilitiesConfig.
    let cfg: duduclaw_core::types::CapabilitiesConfig = cap
        .clone()
        .try_into()
        .expect("deserializes into CapabilitiesConfig");
    assert!(cfg.recording);

    // Explicit false is also written (operator turning it off).
    let mut t2 = toml::Table::new();
    let changes2 = apply_capabilities_to_table(
        &mut t2,
        &json!({ "capabilities": { "recording": false } }),
    )
    .expect("apply");
    assert!(changes2.iter().any(|c| c.contains("recording = false")));

    // Serialization of CapabilitiesConfig carries `recording` so
    // agents.inspect exposes it to the dashboard.
    let json = serde_json::to_value(duduclaw_core::types::CapabilitiesConfig::default())
        .expect("serialize");
    assert_eq!(json.get("recording"), Some(&serde_json::Value::Bool(false)));
}

// ── git_credentials (dashboard toggle, WP-10A follow-up) ──────────────────

#[test]
pub(super) fn cap_git_credentials_round_trips_into_capabilities_config() {
    let mut table = toml::Table::new();
    let changes = apply_capabilities_to_table(
        &mut table,
        &json!({ "capabilities": { "git_credentials": true } }),
    )
    .expect("apply");
    let cap = table.get("capabilities").unwrap().as_table().unwrap();
    assert_eq!(cap.get("git_credentials").unwrap().as_bool(), Some(true));
    assert!(changes.iter().any(|c| c.contains("git_credentials = true")));
    // Must deserialize back into a real CapabilitiesConfig.
    let cfg: duduclaw_core::types::CapabilitiesConfig = cap
        .clone()
        .try_into()
        .expect("deserializes into CapabilitiesConfig");
    assert!(cfg.git_credentials);

    // Explicit false is also written (operator turning it back off).
    let mut t2 = toml::Table::new();
    let changes2 = apply_capabilities_to_table(
        &mut t2,
        &json!({ "capabilities": { "git_credentials": false } }),
    )
    .expect("apply");
    assert!(
        changes2
            .iter()
            .any(|c| c.contains("git_credentials = false"))
    );

    // Serialization of CapabilitiesConfig carries `git_credentials` so
    // agents.inspect exposes it to the dashboard (the switch must be able
    // to reflect the agent's real on-disk state, not just write it).
    let json = serde_json::to_value(duduclaw_core::types::CapabilitiesConfig::default())
        .expect("serialize");
    assert_eq!(
        json.get("git_credentials"),
        Some(&serde_json::Value::Bool(false))
    );
}

// ── system_operator (dashboard toggle, O-4 follow-up) ─────────────────────

#[test]
pub(super) fn cap_system_operator_round_trips_into_capabilities_config() {
    let mut table = toml::Table::new();
    let changes = apply_capabilities_to_table(
        &mut table,
        &json!({ "capabilities": { "system_operator": true } }),
    )
    .expect("apply");
    let cap = table.get("capabilities").unwrap().as_table().unwrap();
    assert_eq!(cap.get("system_operator").unwrap().as_bool(), Some(true));
    assert!(changes.iter().any(|c| c.contains("system_operator = true")));
    // Must deserialize back into a real CapabilitiesConfig.
    let cfg: duduclaw_core::types::CapabilitiesConfig = cap
        .clone()
        .try_into()
        .expect("deserializes into CapabilitiesConfig");
    assert!(cfg.system_operator);

    // Explicit false is also written (operator turning it back off).
    let mut t2 = toml::Table::new();
    let changes2 = apply_capabilities_to_table(
        &mut t2,
        &json!({ "capabilities": { "system_operator": false } }),
    )
    .expect("apply");
    assert!(
        changes2
            .iter()
            .any(|c| c.contains("system_operator = false"))
    );

    // Serialization of CapabilitiesConfig carries `system_operator` so
    // agents.inspect exposes it to the dashboard (the switch must be able
    // to reflect the agent's real on-disk state, not just write it).
    let json = serde_json::to_value(duduclaw_core::types::CapabilitiesConfig::default())
        .expect("serialize");
    assert_eq!(
        json.get("system_operator"),
        Some(&serde_json::Value::Bool(false))
    );
}

// ── codrive (dashboard toggle, CD-1) ───────────────────────────────────────

#[test]
pub(super) fn cap_codrive_round_trips_into_capabilities_config() {
    let mut table = toml::Table::new();
    let changes = apply_capabilities_to_table(
        &mut table,
        &json!({ "capabilities": { "codrive": true } }),
    )
    .expect("apply");
    let cap = table.get("capabilities").unwrap().as_table().unwrap();
    assert_eq!(cap.get("codrive").unwrap().as_bool(), Some(true));
    assert!(changes.iter().any(|c| c.contains("codrive = true")));
    // Must deserialize back into a real CapabilitiesConfig.
    let cfg: duduclaw_core::types::CapabilitiesConfig = cap
        .clone()
        .try_into()
        .expect("deserializes into CapabilitiesConfig");
    assert!(cfg.codrive);

    // Explicit false is also written (operator turning it back off).
    let mut t2 = toml::Table::new();
    let changes2 =
        apply_capabilities_to_table(&mut t2, &json!({ "capabilities": { "codrive": false } }))
            .expect("apply");
    assert!(changes2.iter().any(|c| c.contains("codrive = false")));

    // Serialization of CapabilitiesConfig carries `codrive` so
    // agents.inspect exposes it to the dashboard.
    let json = serde_json::to_value(duduclaw_core::types::CapabilitiesConfig::default())
        .expect("serialize");
    assert_eq!(json.get("codrive"), Some(&serde_json::Value::Bool(false)));
}

// ── autonomy_level (goal-loop dashboard editor) ───────────────────────────

#[test]
pub(super) fn cap_autonomy_level_writes_valid_values_and_reads_back_via_goal_loop() {
    for level in [
        "operator",
        "collaborator",
        "consultant",
        "approver",
        "observer",
    ] {
        let mut table = toml::Table::new();
        let changes = apply_capabilities_to_table(
            &mut table,
            &json!({ "capabilities": { "autonomy_level": level } }),
        )
        .expect("apply");
        let cap = table.get("capabilities").unwrap().as_table().unwrap();
        assert_eq!(cap.get("autonomy_level").unwrap().as_str(), Some(level));
        assert!(changes.iter().any(|c| c.contains("autonomy_level")));

        // Round-trips through the exact reader `goal_loop::AutonomyLevel::
        // for_agent` uses at dispatch time — a mismatch here would mean the
        // dashboard writes a value the goal loop can't parse.
        let dir = tempfile::tempdir().unwrap();
        let agent_dir = dir.path().join("agents").join("alice");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(
            agent_dir.join("agent.toml"),
            toml::to_string(&toml::Value::Table(table)).unwrap(),
        )
        .unwrap();
        let parsed = crate::goal_loop::AutonomyLevel::for_agent(dir.path(), "alice");
        assert_eq!(parsed.as_str(), level);
    }
}

#[test]
pub(super) fn cap_autonomy_level_rejects_unknown_value_fail_closed() {
    let mut table = toml::Table::new();
    let err = apply_capabilities_to_table(
        &mut table,
        &json!({ "capabilities": { "autonomy_level": "god_mode" } }),
    )
    .expect_err("unknown autonomy_level must be rejected, not silently defaulted");
    assert!(err.contains("god_mode"));
    // The invalid value itself was never written.
    let cap = table.get("capabilities").and_then(|v| v.as_table());
    assert!(cap.is_none_or(|c| c.get("autonomy_level").is_none()));
}

#[test]
pub(super) fn os_watch_apply_writes_all_fields() {
    let mut table = toml::Table::new();
    let changes = apply_os_watch_to_table(
        &mut table,
        &json!({
            "os_watch": {
                "paths": ["~/Downloads", "/abs/inbox"],
                "ignore": ["*.part"],
                "debounce_ms": 500,
                "max_events_per_min": 12,
            }
        }),
    )
    .expect("apply");
    let ow = table.get("os_watch").unwrap().as_table().unwrap();
    assert_eq!(ow.get("paths").unwrap().as_array().unwrap().len(), 2);
    assert_eq!(ow.get("ignore").unwrap().as_array().unwrap().len(), 1);
    assert_eq!(ow.get("debounce_ms").unwrap().as_integer(), Some(500));
    assert_eq!(ow.get("max_events_per_min").unwrap().as_integer(), Some(12));
    assert!(changes.iter().any(|c| c.contains("os_watch.paths")));
    // Absent os_watch object ⇒ no-op, empty change list.
    let mut empty = toml::Table::new();
    assert!(
        apply_os_watch_to_table(&mut empty, &json!({}))
            .expect("no-op")
            .is_empty()
    );
    assert!(empty.get("os_watch").is_none());
}

#[test]
pub(super) fn os_watch_apply_rejects_bad_values() {
    // Empty path string.
    let mut t1 = toml::Table::new();
    assert!(
        apply_os_watch_to_table(&mut t1, &json!({ "os_watch": { "paths": ["ok", "  "] } }),)
            .is_err()
    );
    // Non-string path entry.
    let mut t2 = toml::Table::new();
    assert!(
        apply_os_watch_to_table(&mut t2, &json!({ "os_watch": { "paths": [123] } }),).is_err()
    );
    // debounce_ms out of range (0).
    let mut t3 = toml::Table::new();
    assert!(
        apply_os_watch_to_table(&mut t3, &json!({ "os_watch": { "debounce_ms": 0 } }),)
            .is_err()
    );
    // max_events_per_min out of range.
    let mut t4 = toml::Table::new();
    assert!(
        apply_os_watch_to_table(
            &mut t4,
            &json!({ "os_watch": { "max_events_per_min": 2_000_000 } }),
        )
        .is_err()
    );
}

#[test]
pub(super) fn os_watch_apply_writes_goal_template_and_acceptance() {
    let mut table = toml::Table::new();
    let changes = apply_os_watch_to_table(
        &mut table,
        &json!({
            "os_watch": {
                "goal_template": "整理 {file_name}（{kind}）到月報",
                "goal_acceptance": "月報已含 {file_name} 的資料",
            }
        }),
    )
    .expect("apply");
    let ow = table.get("os_watch").unwrap().as_table().unwrap();
    assert_eq!(
        ow.get("goal_template").unwrap().as_str(),
        Some("整理 {file_name}（{kind}）到月報")
    );
    assert_eq!(
        ow.get("goal_acceptance").unwrap().as_str(),
        Some("月報已含 {file_name} 的資料")
    );
    assert!(changes.iter().any(|c| c.contains("goal_template")));
    assert!(changes.iter().any(|c| c.contains("goal_acceptance")));
}

#[test]
pub(super) fn os_watch_apply_goal_template_null_clears() {
    let mut table = toml::Table::new();
    apply_os_watch_to_table(
        &mut table,
        &json!({ "os_watch": { "goal_template": "do {path}" } }),
    )
    .expect("apply");
    assert!(
        table
            .get("os_watch")
            .unwrap()
            .as_table()
            .unwrap()
            .contains_key("goal_template")
    );
    let changes = apply_os_watch_to_table(
        &mut table,
        &json!({ "os_watch": { "goal_template": null } }),
    )
    .expect("apply clear");
    assert!(
        !table
            .get("os_watch")
            .unwrap()
            .as_table()
            .unwrap()
            .contains_key("goal_template")
    );
    assert!(changes.iter().any(|c| c.contains("cleared")));
}

#[test]
pub(super) fn os_watch_apply_goal_template_rejects_bad_values() {
    // Empty string (use null to clear instead).
    let mut t1 = toml::Table::new();
    assert!(
        apply_os_watch_to_table(&mut t1, &json!({ "os_watch": { "goal_template": "" } }),)
            .is_err()
    );
    // Non-string.
    let mut t2 = toml::Table::new();
    assert!(
        apply_os_watch_to_table(&mut t2, &json!({ "os_watch": { "goal_template": 123 } }),)
            .is_err()
    );
    // Over the 2000-char cap.
    let mut t3 = toml::Table::new();
    let long = "x".repeat(2001);
    assert!(
        apply_os_watch_to_table(&mut t3, &json!({ "os_watch": { "goal_acceptance": long } }),)
            .is_err()
    );
}

#[test]
pub(super) fn os_watch_apply_writes_footprint_flag() {
    let mut table = toml::Table::new();
    let changes =
        apply_os_watch_to_table(&mut table, &json!({ "os_watch": { "footprint": true } }))
            .expect("apply");
    let ow = table.get("os_watch").unwrap().as_table().unwrap();
    assert_eq!(ow.get("footprint").unwrap().as_bool(), Some(true));
    assert!(changes.iter().any(|c| c.contains("footprint = true")));

    // Explicit false also round-trips (not a "clear", a real write).
    let changes2 =
        apply_os_watch_to_table(&mut table, &json!({ "os_watch": { "footprint": false } }))
            .expect("apply");
    assert_eq!(
        table
            .get("os_watch")
            .unwrap()
            .as_table()
            .unwrap()
            .get("footprint")
            .unwrap()
            .as_bool(),
        Some(false)
    );
    assert!(changes2.iter().any(|c| c.contains("footprint = false")));

    // Non-bool value is silently ignored (matches the `.and_then(as_bool)`
    // guard — same convention as `capabilities.os_native`), not an error.
    let mut t3 = toml::Table::new();
    let changes3 =
        apply_os_watch_to_table(&mut t3, &json!({ "os_watch": { "footprint": "yes" } }))
            .expect("apply");
    assert!(changes3.is_empty());
}
