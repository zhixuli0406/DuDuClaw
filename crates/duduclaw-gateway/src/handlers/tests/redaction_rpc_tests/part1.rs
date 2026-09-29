//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

//! Handler-level coverage for the §12 poison surface and the §11.2 editor
//! RPCs. Same harness as the other async RPC test modules in this file.
use super::*;
use serde_json::json;

pub(super) fn payload(frame: &WsFrame) -> Value {
    match frame {
        WsFrame::Response {
            ok: true,
            payload: Some(p),
            ..
        } => p.clone(),
        WsFrame::Response {
            ok: false, error, ..
        } => {
            panic!("RPC returned an error frame: {error:?}")
        }
        other => panic!("unexpected frame shape: {other:?}"),
    }
}

pub(super) fn error_text(frame: &WsFrame) -> String {
    match frame {
        WsFrame::Response { error: Some(e), .. } => e.to_string(),
        _ => String::new(),
    }
}

pub(super) async fn handler_with_config(home: &std::path::Path, config_toml: &str) -> MethodHandler {
    std::fs::write(home.join("config.toml"), config_toml).unwrap();
    MethodHandler::new(home.to_path_buf()).await
}

#[tokio::test]
pub(super) async fn data_sources_round_trip_through_get_and_update() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_config(home.path(), "[redaction]\nenabled = false\n").await;

    // `get` always carries the built-ins, even with nothing configured.
    let p = payload(&handler.handle_redaction_get().await);
    let names: Vec<String> = p["data_sources"]
        .as_array()
        .expect("data_sources array")
        .iter()
        .map(|s| s["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["odoo", "duduclaw_db", "duduclaw_files"]);

    // An update adds a custom source AND the rule that uses it in one go.
    let frame = handler
        .handle_redaction_update(json!({
            "data_sources": { "crm_pg": {
                "label": "客戶 CRM 資料庫",
                "tools": ["pg_select"],
                "table_arg": "table",
                "record_paths": ["$.rows[*]"],
            } },
            "field_rules": { "crm_customers": {
                "type": "db_field", "category": "CUSTOMER_PII",
                "source": "crm_pg", "fields": ["customers.name"],
            } },
        }))
        .await;
    assert_eq!(payload(&frame)["success"], true);

    let p = payload(&handler.handle_redaction_get().await);
    let pg = p["data_sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "crm_pg")
        .expect("custom source listed");
    assert_eq!(pg["builtin"], false);
    assert_eq!(pg["record_paths"], json!(["$.rows[*]"]));
    assert_eq!(p["field_rules"][0]["connector"], "crm_pg");

    // Deleting the source while the rule still points at it is refused,
    // and nothing is written.
    let frame = handler
        .handle_redaction_update(json!({ "data_sources": { "crm_pg": null } }))
        .await;
    assert!(
        error_text(&frame).contains("still referenced by rule crm_customers"),
        "{}",
        error_text(&frame)
    );
    let p = payload(&handler.handle_redaction_get().await);
    assert!(
        p["data_sources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["name"] == "crm_pg")
    );

    // A built-in name is never writable.
    let frame = handler
        .handle_redaction_update(
            json!({ "data_sources": { "odoo": { "tools": ["x"], "table_arg": "t" } } }),
        )
        .await;
    assert!(
        error_text(&frame).contains("built-in"),
        "{}",
        error_text(&frame)
    );
}

#[tokio::test]
pub(super) async fn get_and_policy_status_report_no_poison_by_default() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_config(home.path(), "[redaction]\nenabled = false\n").await;

    let p = payload(&handler.handle_redaction_get().await);
    assert_eq!(p["poisoned"], Value::Null);
    assert!(p["field_rules"].is_array());

    let p = payload(&handler.handle_redaction_policy_status().await);
    assert_eq!(p["poisoned"], Value::Null);
}

#[tokio::test]
pub(super) async fn poison_state_surfaces_on_both_rpcs() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_config(home.path(), "[redaction]\nenabled = false\n").await;
    handler
        .set_redaction_poison(Some(RedactionPoison::new("rule 'x' failed to compile")))
        .await;

    let p = payload(&handler.handle_redaction_get().await);
    assert_eq!(p["poisoned"]["reason"], "rule 'x' failed to compile");
    assert!(p["poisoned"]["since"].as_str().unwrap().contains('T'));

    // The manager-absent fallback shape is exactly where a poisoned boot
    // lands — it must carry the field too.
    let p = payload(&handler.handle_redaction_policy_status().await);
    assert_eq!(p["poisoned"]["reason"], "rule 'x' failed to compile");
}

#[tokio::test]
pub(super) async fn successful_hot_reload_clears_the_poison() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_config(home.path(), "[redaction]\nenabled = false\n").await;
    handler
        .set_redaction_poison(Some(RedactionPoison::new("boot failed")))
        .await;

    let frame = handler
        .handle_redaction_update(json!({ "enabled": true, "profiles": ["general"] }))
        .await;
    let p = payload(&frame);
    assert_eq!(p["applied"], true, "{p}");
    assert!(handler.get_redaction_poison().await.is_none());
    assert!(handler.get_redaction_manager().await.is_some());
}

#[tokio::test]
pub(super) async fn failed_hot_reload_keeps_the_poison_and_refreshes_the_reason() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_config(home.path(), "[redaction]\nenabled = false\n").await;
    handler
        .set_redaction_poison(Some(RedactionPoison::new("boot failed")))
        .await;

    // A profile that does not exist fails `RedactionManager::open`.
    let frame = handler
        .handle_redaction_update(json!({ "enabled": true, "profiles": ["no_such_profile"] }))
        .await;
    let p = payload(&frame);
    assert_eq!(p["applied"], false, "{p}");
    assert!(p["warning"].as_str().unwrap().contains("no_such_profile"));
    let poison = handler
        .get_redaction_poison()
        .await
        .expect("still poisoned");
    assert!(
        poison.reason.contains("no_such_profile"),
        "reason should be refreshed: {}",
        poison.reason
    );
}

#[tokio::test]
pub(super) async fn disabling_redaction_also_clears_the_poison() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_config(home.path(), "[redaction]\nenabled = true\n").await;
    handler
        .set_redaction_poison(Some(RedactionPoison::new("boot failed")))
        .await;

    let p = payload(
        &handler
            .handle_redaction_update(json!({ "enabled": false }))
            .await,
    );
    assert_eq!(p["applied"], true, "{p}");
    assert!(handler.get_redaction_poison().await.is_none());
}

#[tokio::test]
pub(super) async fn update_refuses_to_write_a_rule_that_does_not_compile() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_config(
        home.path(),
        "[redaction]\nenabled = true\nprofiles = [\"general\"]\n",
    )
    .await;

    let frame = handler
        .handle_redaction_update(json!({
            "field_rules": {
                "bad": { "type": "json_path", "category": "X", "paths": ["not-a-path"] }
            }
        }))
        .await;
    assert!(error_text(&frame).contains("試編"), "{:?}", frame);
    // Nothing was written.
    let saved = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert!(!saved.contains("[redaction.rules.bad]"), "{saved}");
}

#[tokio::test]
pub(super) async fn update_writes_a_valid_field_rule_and_get_lists_it() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_config(
        home.path(),
        "[redaction]\nenabled = true\nprofiles = [\"general\"]\n",
    )
    .await;

    let frame = handler
        .handle_redaction_update(json!({
            "field_rules": {
                "partner": {
                    "type": "db_field",
                    "category": "CUSTOMER_PII",
                    "fields": ["res.partner.name"],
                }
            }
        }))
        .await;
    let p = payload(&frame);
    assert_eq!(p["applied"], true, "{p}");

    let p = payload(&handler.handle_redaction_get().await);
    let rules = p["field_rules"].as_array().unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0]["id"], "partner");
    assert_eq!(rules[0]["kind"], "db_field");
}

#[tokio::test]
pub(super) async fn dry_run_errors_when_redaction_is_off() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_config(home.path(), "[redaction]\nenabled = false\n").await;
    let frame = handler
        .handle_redaction_dry_run(json!({ "sample_json": "{\"a\":1}" }))
        .await;
    assert!(error_text(&frame).contains("未啟用"), "{:?}", frame);
}

#[tokio::test]
pub(super) async fn dry_run_error_names_the_poison_state() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_config(home.path(), "[redaction]\nenabled = false\n").await;
    handler
        .set_redaction_poison(Some(RedactionPoison::new("rule 'x' failed to compile")))
        .await;
    let frame = handler
        .handle_redaction_dry_run(json!({ "sample_json": "{\"a\":1}" }))
        .await;
    let err = error_text(&frame);
    assert!(err.contains("毒化"), "{err}");
    assert!(err.contains("rule 'x' failed to compile"), "{err}");
}

#[tokio::test]
pub(super) async fn dry_run_reports_hits_without_original_values() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_config(
        home.path(),
        "[redaction]\nenabled = true\nprofiles = [\"general\"]\n\
             [redaction.sources]\ntool_results = \"on\"\n\
             [redaction.rules.partner]\ntype = \"db_field\"\ncategory = \"CUSTOMER_PII\"\n\
             fields = [\"res.partner.name\", \"res.partner.street\"]\n",
    )
    .await;
    // Install the live manager the way boot does.
    let cfg = duduclaw_redaction::RedactionConfig {
        enabled: true,
        profiles: vec!["general".into()],
        ..Default::default()
    };
    let mut cfg = cfg;
    cfg.rules.insert(
        "partner".into(),
        duduclaw_redaction::RuleSpec {
            id: "partner".into(),
            category: "CUSTOMER_PII".into(),
            restore_scope: duduclaw_redaction::RestoreScope::Owner,
            priority: 50,
            cross_session_stable: false,
            apply_to_system_prompt: false,
            enabled: true,
            kind: duduclaw_redaction::RuleKind::DbField {
                source: Some("odoo".into()),
                connector: None,
                fields: vec!["res.partner.name".into(), "res.partner.street".into()],
            },
        },
    );
    cfg.sources.tool_results = duduclaw_redaction::SourceMode::On.into();
    let manager =
        crate::redaction_integration::build_manager_from_home(home.path(), cfg).unwrap();
    handler.swap_redaction_manager(Some(manager)).await;

    let sample = r#"[{"id": 7, "name": "王小明", "street": "台北市信義路 1 號"}]"#;
    let frame = handler
        .handle_redaction_dry_run(json!({
            "sample_json": sample,
            "tool": "odoo_search",
            "args": { "model": "res.partner" },
        }))
        .await;
    let p = payload(&frame);
    let hits = p["hits"].as_array().expect("hits array");
    assert!(!hits.is_empty(), "expected structured hits: {p}");
    assert_eq!(p["token_count"].as_u64().unwrap() as usize, hits.len());
    assert_eq!(p["restored_ok"].as_u64().unwrap() as usize, hits.len());

    let rendered = serde_json::to_string(&p).unwrap();
    assert!(
        !rendered.contains("王小明"),
        "originals must never be returned: {rendered}"
    );
    assert!(
        !rendered.contains("信義路"),
        "originals must never be returned: {rendered}"
    );
    for hit in hits {
        assert!(
            hit["pointer"]
                .as_str()
                .unwrap()
                .starts_with("/content/0/text#")
        );
        assert_eq!(hit["rule_id"], "partner");
        assert_eq!(hit["category"], "CUSTOMER_PII");
        assert!(hit["token"].as_str().unwrap().starts_with("<REDACT:"));
        assert!(hit.get("masked").is_none(), "no masked column either");
        assert!(hit.get("original").is_none());
    }
}

#[tokio::test]
pub(super) async fn dry_run_rejects_bad_input() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_config(home.path(), "[redaction]\nenabled = false\n").await;
    assert!(!error_text(&handler.handle_redaction_dry_run(json!({})).await).is_empty());
    assert!(
        !error_text(
            &handler
                .handle_redaction_dry_run(json!({ "sample_json": "   " }))
                .await
        )
        .is_empty()
    );
    assert!(
        !error_text(
            &handler
                .handle_redaction_dry_run(json!({ "sample_json": "{", "args": 5 }))
                .await
        )
        .is_empty()
    );
}

// ── §13.2 「我的規則」 custom rules + rule-pack import ───────────────

/// A config with redaction enabled so the hot-reload arm actually runs.
pub(super) const ENABLED_CONFIG: &str = "[redaction]\nenabled = true\nprofiles = [\"general\"]\n";

pub(super) fn listed_profiles(home: &std::path::Path) -> Vec<String> {
    let body = std::fs::read_to_string(home.join("config.toml")).unwrap();
    let table: toml::Table = toml::from_str(&body).unwrap();
    crate::redaction_custom_rules::listed_profiles(&table)
}

/// `MethodHandler::new` does not build the redaction manager — `server.rs`
/// injects it at boot. This does the same thing through the shared hot
/// reload, so a test can call `dry_run` without first writing a rule.
pub(super) async fn handler_with_live_redaction(
    home: &std::path::Path,
    config_toml: &str,
) -> MethodHandler {
    let handler = handler_with_config(home, config_toml).await;
    let table = handler.read_config_table(&home.join("config.toml")).await;
    let (applied, warning) = handler.apply_redaction_hot_reload(&table).await;
    assert!(applied, "test setup: redaction must load — {warning:?}");
    handler
}

#[tokio::test]
pub(super) async fn custom_rules_create_list_toggle_remove() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_config(home.path(), ENABLED_CONFIG).await;

    // Empty to start with — an absent custom.toml is a list, not an error.
    let p = payload(&handler.handle_redaction_custom_rules_list().await);
    assert_eq!(p["rules"].as_array().unwrap().len(), 0);

    let created = payload(
        &handler
            .handle_redaction_custom_rules_upsert(json!({
                "label": "Employee ID",
                "kind": "regex",
                "pattern": r"EMP-\d{4}-\d{4}",
            }))
            .await,
    );
    assert_eq!(created["id"], "employee_id");
    assert_eq!(created["category"], "CUSTOM_EMPLOYEE_ID");
    assert_eq!(created["example"], "EMP-0000-0000");
    assert_eq!(created["enabled"], true);
    assert_eq!(
        created["applied"], true,
        "hot reload must have run: {created}"
    );

    // The profile is now listed in config.toml — without that the rule
    // file on disk would never be resolved.
    assert!(listed_profiles(home.path()).contains(&"custom".to_string()));

    // ...and the LIVE engine is carrying it.
    let manager = handler
        .get_redaction_manager()
        .await
        .expect("manager rebuilt");
    let hits = manager.engine().apply(
        "工號 EMP-2024-0133",
        &duduclaw_redaction::Source::ToolResult {
            tool_name: "x".into(),
        },
    );
    assert!(
        hits.iter()
            .any(|h| h.rule.category() == "CUSTOM_EMPLOYEE_ID"),
        "custom rule must reach the live engine"
    );

    let p = payload(&handler.handle_redaction_custom_rules_list().await);
    assert_eq!(p["rules"].as_array().unwrap().len(), 1);

    let toggled = payload(
        &handler
            .handle_redaction_custom_rules_set_enabled(
                json!({ "id": "employee_id", "enabled": false }),
            )
            .await,
    );
    assert_eq!(toggled["enabled"], false);
    let manager = handler.get_redaction_manager().await.unwrap();
    assert!(
        !manager
            .engine()
            .rule_catalogue()
            .iter()
            .any(|(id, _)| id == "employee_id"),
        "a disabled rule must leave the live engine"
    );

    let removed = payload(
        &handler
            .handle_redaction_custom_rules_remove(json!({ "id": "employee_id" }))
            .await,
    );
    assert_eq!(removed["removed"], true);
    let p = payload(&handler.handle_redaction_custom_rules_list().await);
    assert!(p["rules"].as_array().unwrap().is_empty());
}

#[tokio::test]
pub(super) async fn custom_rule_labels_surface_in_redaction_get() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_config(home.path(), ENABLED_CONFIG).await;
    payload(
        &handler
            .handle_redaction_custom_rules_upsert(json!({
                "label": "內部專案代號",
                "kind": "keyword",
                "keywords": ["獵鷹專案", "北極星"],
            }))
            .await,
    );

    let p = payload(&handler.handle_redaction_get().await);
    assert_eq!(p["category_labels"]["CUSTOM_01"], "內部專案代號");
    let custom = p["available_profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["name"] == "custom")
        .expect("custom profile listed");
    assert_eq!(custom["custom"], true);
    assert_eq!(custom["builtin"], false);
    // Built-ins report the inverse.
    let general = p["available_profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["name"] == "general")
        .unwrap();
    assert_eq!(general["custom"], false);
}

#[tokio::test]
pub(super) async fn custom_rules_reject_invalid_payloads() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_config(home.path(), ENABLED_CONFIG).await;
    for bad in [
        json!({ "kind": "keyword", "keywords": ["ab"] }),
        json!({ "label": "a", "kind": "keyword", "keywords": ["x"] }),
        json!({ "label": "a", "kind": "regex", "pattern": "[bad" }),
        json!({ "label": "a", "kind": "identity" }),
    ] {
        let frame = handler
            .handle_redaction_custom_rules_upsert(bad.clone())
            .await;
        assert!(!error_text(&frame).is_empty(), "should reject {bad}");
    }
    // Missing params on the other two methods.
    assert!(
        !error_text(
            &handler
                .handle_redaction_custom_rules_remove(json!({}))
                .await
        )
        .is_empty()
    );
    assert!(
        !error_text(
            &handler
                .handle_redaction_custom_rules_set_enabled(json!({ "id": "x" }))
                .await
        )
        .is_empty()
    );
    // Toggling a rule that does not exist is an error, not a silent no-op.
    assert!(
        !error_text(
            &handler
                .handle_redaction_custom_rules_set_enabled(
                    json!({ "id": "nope", "enabled": false })
                )
                .await
        )
        .is_empty()
    );
}

#[tokio::test]
pub(super) async fn removing_a_rule_that_never_existed_does_not_poison_the_pipeline() {
    // `custom.toml` has never been written. Listing the profile anyway
    // would make the next resolve fail with "profile 'custom' not found",
    // i.e. a self-inflicted poison on a no-op delete.
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_config(home.path(), ENABLED_CONFIG).await;
    let p = payload(
        &handler
            .handle_redaction_custom_rules_remove(json!({ "id": "nope" }))
            .await,
    );
    assert_eq!(p["removed"], false);
    assert_eq!(p["applied"], true, "{p}");
    assert!(!listed_profiles(home.path()).contains(&"custom".to_string()));
    assert!(handler.get_redaction_poison().await.is_none());
    assert!(handler.get_redaction_manager().await.is_some());
}

#[tokio::test]
pub(super) async fn unreadable_custom_profile_surfaces_as_an_error() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_config(home.path(), ENABLED_CONFIG).await;
    let dir = crate::redaction_custom_rules::profiles_dir(home.path());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("custom.toml"), "not toml [[[").unwrap();

    let frame = handler.handle_redaction_custom_rules_list().await;
    assert!(
        error_text(&frame).contains("無法解析"),
        "fail closed, got: {}",
        error_text(&frame)
    );
}

pub(super) const PACK_TOML: &str = r#"
[meta]
name = "Acme Pack"

[meta.labels]
CUSTOM_ACME_ID = "Acme 編號"

[rules.acme_id]
type = "regex"
pattern = 'ACME-\d{5}'
category = "CUSTOM_ACME_ID"

[rules.broken]
type = "regex"
pattern = '[unclosed'
category = "CUSTOM_ACME_ID"
"#;

#[tokio::test]
pub(super) async fn profile_import_dry_run_then_write() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_config(home.path(), ENABLED_CONFIG).await;

    let dry = payload(
        &handler
            .handle_redaction_profiles_import(json!({ "toml": PACK_TOML, "dry_run": true }))
            .await,
    );
    assert_eq!(dry["name"], "acme-pack");
    assert_eq!(dry["imported"], 1);
    assert_eq!(dry["skipped"].as_array().unwrap().len(), 1);
    assert_eq!(dry["skipped"][0]["rule_id"], "broken");
    assert!(dry["skipped"][0]["line"].as_u64().is_some());
    assert_eq!(dry["dry_run"], true);
    // Nothing written, nothing listed.
    assert!(!listed_profiles(home.path()).contains(&"acme-pack".to_string()));

    let wet = payload(
        &handler
            .handle_redaction_profiles_import(json!({ "toml": PACK_TOML }))
            .await,
    );
    assert_eq!(wet["imported"], 1);
    assert_eq!(wet["applied"], true);
    assert!(listed_profiles(home.path()).contains(&"acme-pack".to_string()));

    let p = payload(&handler.handle_redaction_get().await);
    assert_eq!(p["category_labels"]["CUSTOM_ACME_ID"], "Acme 編號");

    // Remove it again: file gone, unlisted, reloaded.
    let gone = payload(
        &handler
            .handle_redaction_profiles_remove(json!({ "name": "acme-pack" }))
            .await,
    );
    assert_eq!(gone["removed"], true);
    assert!(!listed_profiles(home.path()).contains(&"acme-pack".to_string()));
}
