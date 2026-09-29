//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

use super::{apply_redaction_to_table, redaction_table_to_response};
use serde_json::{Value, json};

#[test]
fn sources_round_trip_string_and_detail_forms() {
    let mut table = toml::Table::new();
    let params = json!({
        "enabled": true,
        "sources": {
            "user_input": "off",
            "tool_results": {
                "mode": "on",
                "only_categories": ["TW_ID", "CREDIT_CARD"],
            },
            "cron_context": {
                "mode": "on",
                "exclude_categories": ["EMAIL"],
            },
        },
    });
    let changes = apply_redaction_to_table(&mut table, &params).unwrap();
    assert!(
        changes.iter().any(|c| c.contains("tool_results")),
        "{changes:?}"
    );

    // The written TOML must parse under the crate's SourceSetting (both forms).
    let toml_str = toml::to_string(&table).unwrap();
    #[derive(serde::Deserialize)]
    struct W {
        redaction: duduclaw_redaction::config::RedactionConfig,
    }
    let w: W = toml::from_str(&toml_str).unwrap();
    assert_eq!(
        w.redaction.sources.tool_results.only_categories,
        ["TW_ID", "CREDIT_CARD"]
    );
    assert_eq!(
        w.redaction.sources.cron_context.exclude_categories,
        ["EMAIL"]
    );
    assert!(w.redaction.sources.user_input.is_mode_only());

    // Response always uses the detail-object form.
    let resp = redaction_table_to_response(&table);
    let tr = &resp["sources"]["tool_results"];
    assert_eq!(tr["mode"], "on");
    assert_eq!(tr["only_categories"][1], "CREDIT_CARD");
    assert_eq!(resp["sources"]["user_input"]["mode"], "off");
    assert_eq!(
        resp["sources"]["user_input"]["only_categories"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn detail_form_with_empty_lists_collapses_to_string() {
    let mut table = toml::Table::new();
    let params = json!({
        "sources": { "tool_results": { "mode": "selective", "only_categories": [] } }
    });
    apply_redaction_to_table(&mut table, &params).unwrap();
    let written = table["redaction"]["sources"]["tool_results"].clone();
    assert_eq!(written, toml::Value::String("selective".into()));
}

#[test]
fn invalid_detail_forms_rejected() {
    let mut table = toml::Table::new();
    for bad in [
        json!({ "sources": { "tool_results": { "mode": "sideways" } } }),
        json!({ "sources": { "tool_results": { "only_categories": ["X"] } } }), // mode missing
        json!({ "sources": { "tool_results": { "mode": "on", "only_categories": [""] } } }),
        json!({ "sources": { "tool_results": 42 } }),
    ] {
        assert!(apply_redaction_to_table(&mut table, &bad).is_err(), "{bad}");
    }
}

#[test]
fn builtin_profile_catalogue_lists_categories() {
    let tmp = tempfile::tempdir().unwrap();
    let profiles = super::redaction_available_profiles(tmp.path());
    assert!(
        profiles.len() >= 5,
        "expected the 5 built-ins, got {}",
        profiles.len()
    );
    let general = profiles
        .iter()
        .find(|p| p["name"] == "general")
        .expect("general profile present");
    assert!(general["builtin"].as_bool().unwrap());
    assert!(
        general["categories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c == "EMAIL")
    );
}

// ── §11.2 structured-field rule editor ───────────────────

fn db_field_rule() -> serde_json::Value {
    json!({
        "type": "db_field",
        "category": "CUSTOMER_PII",
        "fields": ["res.partner.name", "res.partner.street"],
    })
}

#[test]
fn get_lists_only_field_rules() {
    let mut table = toml::Table::new();
    let params = json!({
        "enabled": true,
        "field_rules": {
            "partner": db_field_rule(),
            "orders": {
                "type": "json_path",
                "category": "ORDER",
                "paths": ["$[*].partner_id"],
                "match_tool": "odoo_*",
                "match_args": { "model": "sale.order" },
                "exclude_keys": ["id"],
            },
        },
    });
    apply_redaction_to_table(&mut table, &params).unwrap();
    // A regex rule written straight into TOML must NOT be listed.
    table["redaction"]["rules"].as_table_mut().unwrap().insert(
        "email".into(),
        toml::Value::Table(
            toml::toml! { type = "regex" category = "EMAIL" pattern = "a@b" }.clone(),
        ),
    );

    let listed = super::redaction_field_rules(&table);
    let ids: Vec<&str> = listed.iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["orders", "partner"], "regex rule must not be listed");

    let partner = listed.iter().find(|r| r["id"] == "partner").unwrap();
    assert_eq!(partner["kind"], "db_field");
    assert_eq!(partner["connector"], "odoo");
    assert_eq!(partner["fields"][1], "res.partner.street");
    assert_eq!(partner["restore_scope"]["kind"], "owner");
    assert_eq!(partner["priority"], 50);
    assert_eq!(partner["cross_session_stable"], false);

    let orders = listed.iter().find(|r| r["id"] == "orders").unwrap();
    assert_eq!(orders["kind"], "json_path");
    assert_eq!(orders["match_tool"], "odoo_*");
    assert_eq!(orders["match_args"]["model"], "sale.order");
    assert_eq!(orders["exclude_keys"][0], "id");
}

#[test]
fn field_rules_upsert_null_and_absent() {
    let mut table = toml::Table::new();
    apply_redaction_to_table(
        &mut table,
        &json!({ "field_rules": { "a": db_field_rule(), "b": db_field_rule() } }),
    )
    .unwrap();
    assert_eq!(super::redaction_field_rules(&table).len(), 2);

    // Upsert `a`, delete `b`, leave `a`'s sibling keys alone.
    let changes = apply_redaction_to_table(
        &mut table,
        &json!({
            "field_rules": {
                "a": { "type": "db_field", "category": "OTHER", "fields": ["hr.employee.*"] },
                "b": null,
            }
        }),
    )
    .unwrap();
    assert!(
        changes
            .iter()
            .any(|c| c.contains("redaction.rules.b removed")),
        "{changes:?}"
    );
    let listed = super::redaction_field_rules(&table);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["id"], "a");
    assert_eq!(listed[0]["category"], "OTHER");
    assert_eq!(listed[0]["fields"][0], "hr.employee.*");

    // An absent id is untouched.
    apply_redaction_to_table(&mut table, &json!({ "enabled": true })).unwrap();
    assert_eq!(super::redaction_field_rules(&table).len(), 1);
}

#[test]
fn field_rules_written_toml_round_trips_through_the_crate() {
    let mut table = toml::Table::new();
    apply_redaction_to_table(
        &mut table,
        &json!({ "enabled": true, "field_rules": { "partner": db_field_rule() } }),
    )
    .unwrap();
    // The atomic write serialises the whole table — prove it survives that
    // trip and deserialises back into a real RuleSpec.
    let raw = toml::to_string(&table).expect("serialisable");
    #[derive(serde::Deserialize)]
    struct W {
        redaction: duduclaw_redaction::config::RedactionConfig,
    }
    let w: W = toml::from_str(&raw).unwrap();
    let spec = w.redaction.rules.get("partner").expect("rule present");
    assert!(matches!(
        spec.kind,
        duduclaw_redaction::RuleKind::DbField { .. }
    ));
}

#[test]
fn field_rules_validation_rejects_bad_input() {
    // Bad id charset / shape.
    for bad_id in [
        "Partner",
        "1partner",
        "-partner",
        "",
        "par tner",
        &"a".repeat(65),
    ] {
        let mut table = toml::Table::new();
        let params = json!({ "field_rules": { bad_id: db_field_rule() } });
        assert!(
            apply_redaction_to_table(&mut table, &params).is_err(),
            "id {bad_id:?} should be refused"
        );
    }
    // Wrong kind — the editor owns db_field / json_path only.
    let mut table = toml::Table::new();
    assert!(
        apply_redaction_to_table(
            &mut table,
            &json!({ "field_rules": { "x": { "type": "regex", "category": "E", "pattern": "a" } } })
        )
        .is_err()
    );
    // Unknown rule type entirely.
    let mut table = toml::Table::new();
    assert!(
        apply_redaction_to_table(
            &mut table,
            &json!({ "field_rules": { "x": { "type": "nope", "category": "E" } } })
        )
        .is_err()
    );
    // Not an object.
    let mut table = toml::Table::new();
    assert!(
        apply_redaction_to_table(&mut table, &json!({ "field_rules": { "x": 42 } })).is_err()
    );
}

#[test]
fn field_rules_refuse_to_touch_a_non_field_rule_id() {
    let mut table = toml::Table::new();
    apply_redaction_to_table(&mut table, &json!({ "enabled": true })).unwrap();
    let red = table["redaction"].as_table_mut().unwrap();
    let mut rules = toml::map::Map::new();
    rules.insert(
        "email".into(),
        toml::Value::Table(
            toml::toml! { type = "regex" category = "EMAIL" pattern = "a@b" }.clone(),
        ),
    );
    red.insert("rules".into(), toml::Value::Table(rules));

    // Overwriting a regex rule id with a db_field rule is refused …
    let err = apply_redaction_to_table(
        &mut table,
        &json!({ "field_rules": { "email": db_field_rule() } }),
    )
    .unwrap_err();
    assert!(err.contains("regex"), "{err}");
    // … and so is deleting it through this path.
    assert!(
        apply_redaction_to_table(&mut table, &json!({ "field_rules": { "email": null } }))
            .is_err()
    );
    // The rule itself is untouched.
    assert_eq!(
        table["redaction"]["rules"]["email"]["type"].as_str(),
        Some("regex")
    );
}

#[test]
fn dry_compile_rejects_malformed_paths_and_unknown_connectors() {
    let tmp = tempfile::tempdir().unwrap();

    // Malformed json_path expression.
    let mut table = toml::Table::new();
    apply_redaction_to_table(
        &mut table,
        &json!({
            "enabled": true,
            "profiles": ["general"],
            "field_rules": { "bad": { "type": "json_path", "category": "X", "paths": ["not-a-path"] } },
        }),
    )
    .unwrap();
    let err = super::dry_compile_redaction_table(&table, tmp.path()).unwrap_err();
    assert!(!err.is_empty(), "compile error text must be surfaced");

    // Unknown db_field connector.
    let mut table = toml::Table::new();
    apply_redaction_to_table(
        &mut table,
        &json!({
            "enabled": true,
            "profiles": ["general"],
            "field_rules": {
                "bad": { "type": "db_field", "category": "X", "connector": "sap", "fields": ["a.b"] }
            },
        }),
    )
    .unwrap();
    assert!(super::dry_compile_redaction_table(&table, tmp.path()).is_err());

    // Malformed model.field entry.
    let mut table = toml::Table::new();
    apply_redaction_to_table(
        &mut table,
        &json!({
            "enabled": true,
            "profiles": ["general"],
            "field_rules": {
                "bad": { "type": "db_field", "category": "X", "fields": ["NotAModel"] }
            },
        }),
    )
    .unwrap();
    assert!(super::dry_compile_redaction_table(&table, tmp.path()).is_err());

    // A good pair compiles.
    let mut table = toml::Table::new();
    apply_redaction_to_table(
        &mut table,
        &json!({
            "enabled": true,
            "profiles": ["general"],
            "field_rules": { "ok": db_field_rule() },
        }),
    )
    .unwrap();
    super::dry_compile_redaction_table(&table, tmp.path()).expect("valid rules compile");
}

#[test]
fn get_output_can_be_posted_straight_back_to_update() {
    // The editor's round trip: `redaction.get` renders `kind`, and
    // `redaction.update` must accept that body verbatim (minus the id).
    let mut table = toml::Table::new();
    apply_redaction_to_table(
        &mut table,
        &json!({ "field_rules": { "partner": db_field_rule() } }),
    )
    .unwrap();
    let listed = super::redaction_field_rules(&table);
    let mut body = listed[0].as_object().unwrap().clone();
    body.remove("id");
    assert_eq!(body["kind"], "db_field", "get renders `kind`");

    let mut table2 = toml::Table::new();
    apply_redaction_to_table(
        &mut table2,
        &json!({ "field_rules": { "partner": serde_json::Value::Object(body) } }),
    )
    .expect("a `kind`-shaped body must be accepted");
    assert_eq!(
        super::redaction_field_rules(&table2),
        listed,
        "round trip must be lossless"
    );

    // A body carrying both spellings in disagreement is refused.
    let mut table3 = toml::Table::new();
    assert!(
        apply_redaction_to_table(
            &mut table3,
            &json!({ "field_rules": { "x": {
                "type": "db_field", "kind": "json_path",
                "category": "X", "fields": ["a.b"]
            } } })
        )
        .is_err()
    );
}

// ── §13.5 data-source registry ──────────────────────────────────────────

fn pg_source() -> serde_json::Value {
    json!({
        "label": "客戶 CRM 資料庫",
        "tools": ["pg_query", "pg_select"],
        "table_arg": "table",
        "key_alias": { "name": "customer_name" },
    })
}

#[test]
fn get_lists_builtin_and_custom_data_sources() {
    let mut table = toml::Table::new();
    apply_redaction_to_table(
        &mut table,
        &json!({ "data_sources": { "crm_pg": pg_source() } }),
    )
    .unwrap();

    let listed = super::redaction_data_sources(&table);
    let names: Vec<&str> = listed.iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        ["odoo", "duduclaw_db", "duduclaw_files", "crm_pg"],
        "built-ins first"
    );

    let odoo = &listed[0];
    assert_eq!(odoo["builtin"], true);
    assert_eq!(odoo["tools"].as_array().unwrap().len(), 9);
    // Odoo mixes per-tool tables and aliases — the simple form reports
    // nothing it cannot prove rather than inventing a summary.
    assert_eq!(odoo["table"], Value::Null);
    assert_eq!(odoo["table_arg"], Value::Null);
    assert_eq!(odoo["table_result"], Value::Null);
    assert_eq!(odoo["free_form_names"], false);
    assert_eq!(odoo["record_paths"], json!(["$[*]", "$"]));

    let db = &listed[1];
    assert_eq!(db["builtin"], true);
    assert_eq!(db["tools"], json!(["db_select"]));
    assert_eq!(db["table_arg"], "table");
    assert_eq!(db["table_result"], Value::Null);
    assert_eq!(db["free_form_names"], false);
    assert_eq!(db["record_paths"], json!(["$.rows[*]"]));

    // The local data-file readers: the table comes from the RESULT and the
    // names are free-form (file names / spreadsheet headers).
    let files = &listed[2];
    assert_eq!(files["builtin"], true);
    assert_eq!(files["tools"], json!(["csv_read", "xlsx_read"]));
    assert_eq!(files["table_arg"], Value::Null);
    assert_eq!(files["table"], Value::Null);
    assert_eq!(files["table_result"], "/table");
    assert_eq!(files["free_form_names"], true);
    assert_eq!(files["record_paths"], json!(["$.rows[*]"]));

    let pg = &listed[3];
    assert_eq!(pg["builtin"], false);
    assert_eq!(pg["label"], "客戶 CRM 資料庫");
    assert_eq!(pg["tools"], json!(["pg_query", "pg_select"]));
    assert_eq!(pg["table_arg"], "table");
    assert_eq!(pg["table"], Value::Null);
    assert_eq!(pg["table_result"], Value::Null);
    assert_eq!(pg["free_form_names"], false);
    // Omitted record_paths come back as the defaults that take effect.
    assert_eq!(pg["record_paths"], json!(["$.rows[*]", "$[*]", "$"]));
    assert_eq!(pg["key_alias"]["name"], "customer_name");
}

#[test]
fn data_sources_upsert_null_and_absent() {
    let mut table = toml::Table::new();
    apply_redaction_to_table(
        &mut table,
        &json!({ "data_sources": { "crm_pg": pg_source(), "erp": {
            "tools": ["erp_customers"], "table": "customers"
        } } }),
    )
    .unwrap();
    let builtins = duduclaw_redaction::BUILTIN_SOURCE_NAMES.len();
    assert_eq!(super::redaction_data_sources(&table).len(), builtins + 2);

    // Upsert one, delete the other; an absent name is untouched.
    let changes = apply_redaction_to_table(
        &mut table,
        &json!({ "data_sources": {
            "crm_pg": { "label": "改名", "tools": ["pg_select"], "table_arg": "table" },
            "erp": null,
        } }),
    )
    .unwrap();
    assert!(
        changes
            .iter()
            .any(|c| c.contains("redaction.data_sources.erp removed")),
        "{changes:?}"
    );
    let listed = super::redaction_data_sources(&table);
    assert_eq!(listed.len(), builtins + 1);
    let pg = listed.iter().find(|s| s["name"] == "crm_pg").unwrap();
    assert_eq!(pg["label"], "改名");
    assert_eq!(pg["tools"], json!(["pg_select"]));
    // The replaced entry no longer carries the old alias.
    assert_eq!(pg["key_alias"], json!({}));

    apply_redaction_to_table(&mut table, &json!({ "enabled": true })).unwrap();
    assert_eq!(super::redaction_data_sources(&table).len(), builtins + 1);
}

#[test]
fn data_sources_refuse_builtin_names() {
    for builtin in duduclaw_redaction::BUILTIN_SOURCE_NAMES {
        let builtin = *builtin;
        let mut table = toml::Table::new();
        let err = apply_redaction_to_table(
            &mut table,
            &json!({ "data_sources": { builtin: pg_source() } }),
        )
        .unwrap_err();
        assert!(err.contains("built-in"), "{err}");
        // …and deleting one is refused through the same door.
        let mut table = toml::Table::new();
        assert!(
            apply_redaction_to_table(&mut table, &json!({ "data_sources": { builtin: null } }))
                .is_err()
        );
    }
}

#[test]
fn data_sources_validation_rejects_bad_input() {
    for bad_name in ["Crm", "1crm", "-crm", "", "crm.pg", &"a".repeat(65)] {
        let mut table = toml::Table::new();
        assert!(
            apply_redaction_to_table(
                &mut table,
                &json!({ "data_sources": { bad_name: pg_source() } })
            )
            .is_err(),
            "name {bad_name:?} should be refused"
        );
    }
    // Semantic refusals, each with its own reason.
    for (body, needle) in [
        (json!({ "tools": [], "table_arg": "table" }), "no tools"),
        (json!({ "tools": ["pg_select"] }), "exactly one"),
        // The XOR error names all three options, whichever pair collided.
        (
            json!({ "tools": ["pg_select"], "table_arg": "table", "table": "customers" }),
            "table_result",
        ),
        (
            json!({ "tools": ["pg_select"], "table_arg": "table", "table": "customers" }),
            "sets table_arg and table",
        ),
        (
            json!({ "tools": ["csv_read"], "table_arg": "table", "table_result": "/table" }),
            "sets table_arg and table_result",
        ),
        (
            json!({ "tools": ["csv_read"], "table_result": "table" }),
            "JSON pointer",
        ),
        (
            json!({ "tools": ["pg_select"], "table_arg": "table", "record_paths": ["rows[*]"] }),
            "rows[*]",
        ),
    ] {
        let mut table = toml::Table::new();
        let err = apply_redaction_to_table(
            &mut table,
            &json!({ "data_sources": { "crm_pg": body } }),
        )
        .unwrap_err();
        assert!(err.contains(needle), "expected {needle:?} in: {err}");
    }
    // Not an object.
    let mut table = toml::Table::new();
    assert!(
        apply_redaction_to_table(&mut table, &json!({ "data_sources": { "crm_pg": 42 } }))
            .is_err()
    );
}

#[test]
fn data_sources_accept_table_result_and_free_form_names() {
    let mut table = toml::Table::new();
    apply_redaction_to_table(
        &mut table,
        &json!({ "data_sources": { "excel_drop": {
            "label": "匯入的試算表",
            "tools": ["sheet_read"],
            "table_result": "/table",
            "record_paths": ["$.rows[*]"],
            "free_form_names": true,
        } } }),
    )
    .unwrap();

    let listed = super::redaction_data_sources(&table);
    let src = listed
        .iter()
        .find(|s| s["name"] == "excel_drop")
        .expect("custom source listed");
    assert_eq!(src["table_result"], "/table");
    assert_eq!(src["table_arg"], Value::Null);
    assert_eq!(src["table"], Value::Null);
    assert_eq!(src["free_form_names"], true);
    assert_eq!(src["record_paths"], json!(["$.rows[*]"]));

    // …and the written TOML really carries them (the wire is rendered from
    // the stored entry, so a serialisation slip would be invisible above).
    let stored = table["redaction"]["data_sources"]["excel_drop"]
        .as_table()
        .unwrap();
    assert_eq!(stored["table_result"].as_str(), Some("/table"));
    assert_eq!(stored["free_form_names"].as_bool(), Some(true));
    assert!(!stored.contains_key("table_arg"));
}

#[test]
fn a_referenced_data_source_cannot_be_deleted() {
    let mut table = toml::Table::new();
    apply_redaction_to_table(
        &mut table,
        &json!({
            "data_sources": { "crm_pg": { "tools": ["pg_select"], "table_arg": "table" } },
            "field_rules": { "crm_customers": {
                "type": "db_field", "category": "CUSTOMER_PII",
                "source": "crm_pg", "fields": ["customers.name"],
            } },
        }),
    )
    .unwrap();

    let err =
        apply_redaction_to_table(&mut table, &json!({ "data_sources": { "crm_pg": null } }))
            .unwrap_err();
    assert!(
        err.contains("still referenced by rule crm_customers"),
        "{err}"
    );
    let builtins = duduclaw_redaction::BUILTIN_SOURCE_NAMES.len();
    assert_eq!(
        super::redaction_data_sources(&table).len(),
        builtins + 1,
        "nothing removed"
    );

    // Retiring the rule and the source in ONE call works — field_rules are
    // applied first, so the reference is already gone.
    apply_redaction_to_table(
        &mut table,
        &json!({ "field_rules": { "crm_customers": null }, "data_sources": { "crm_pg": null } }),
    )
    .unwrap();
    assert_eq!(super::redaction_data_sources(&table).len(), builtins);

    // The deprecated `connector` spelling pins its source just as well.
    let mut table = toml::Table::new();
    apply_redaction_to_table(
        &mut table,
        &json!({
            "data_sources": { "crm_pg": { "tools": ["pg_select"], "table_arg": "table" } },
            "field_rules": { "legacy": {
                "type": "db_field", "category": "X",
                "connector": "crm_pg", "fields": ["customers.name"],
            } },
        }),
    )
    .unwrap();
    assert!(
        apply_redaction_to_table(&mut table, &json!({ "data_sources": { "crm_pg": null } }))
            .is_err()
    );
}

#[test]
fn dry_compile_covers_custom_sources_in_both_directions() {
    let tmp = tempfile::tempdir().unwrap();

    // A rule naming a source nobody defined must not reach the disk.
    let mut table = toml::Table::new();
    apply_redaction_to_table(
        &mut table,
        &json!({
            "enabled": true,
            "profiles": ["general"],
            "field_rules": { "crm": {
                "type": "db_field", "category": "X",
                "source": "crm_pg", "fields": ["customers.name"],
            } },
        }),
    )
    .unwrap();
    let err = super::dry_compile_redaction_table(&table, tmp.path()).unwrap_err();
    assert!(err.contains("crm_pg"), "{err}");

    // Defining the source makes the same rule compile.
    apply_redaction_to_table(
        &mut table,
        &json!({ "data_sources": { "crm_pg": { "tools": ["pg_select"], "table_arg": "table" } } }),
    )
    .unwrap();
    super::dry_compile_redaction_table(&table, tmp.path()).expect("a defined source compiles");

    // `source` and `connector` disagreeing is a compile error, not a
    // silent winner.
    let mut table = toml::Table::new();
    apply_redaction_to_table(
        &mut table,
        &json!({
            "enabled": true,
            "profiles": ["general"],
            "field_rules": { "conflict": {
                "type": "db_field", "category": "X",
                "source": "duduclaw_db", "connector": "odoo",
                "fields": ["customers.name"],
            } },
        }),
    )
    .unwrap();
    assert!(super::dry_compile_redaction_table(&table, tmp.path()).is_err());
}

#[test]
fn db_field_rule_wire_reports_the_resolved_source() {
    let mut table = toml::Table::new();
    apply_redaction_to_table(
        &mut table,
        &json!({
            "data_sources": { "crm_pg": { "tools": ["pg_select"], "table_arg": "table" } },
            "field_rules": {
                // No source / connector at all ⇒ the historical default.
                "legacy": { "type": "db_field", "category": "X", "fields": ["res.partner.name"] },
                "modern": { "type": "db_field", "category": "X", "source": "crm_pg", "fields": ["customers.name"] },
            },
        }),
    )
    .unwrap();
    let listed = super::redaction_field_rules(&table);
    let by = |id: &str| -> Value { listed.iter().find(|r| r["id"] == id).unwrap().clone() };
    assert_eq!(by("legacy")["connector"], "odoo");
    assert_eq!(by("modern")["connector"], "crm_pg");

    // Posting the rendered body straight back stays lossless.
    let mut body = by("modern").as_object().unwrap().clone();
    body.remove("id");
    let mut table2 = toml::Table::new();
    apply_redaction_to_table(
        &mut table2,
        &json!({ "field_rules": { "modern": Value::Object(body) } }),
    )
    .unwrap();
    assert_eq!(
        super::redaction_field_rules(&table2)[0]["connector"],
        "crm_pg"
    );
}

#[test]
fn field_rule_id_charset_is_anchored() {
    assert!(super::is_valid_field_rule_id("a"));
    assert!(super::is_valid_field_rule_id("partner_pii-2"));
    assert!(!super::is_valid_field_rule_id("Partner"));
    assert!(!super::is_valid_field_rule_id("2partner"));
    assert!(!super::is_valid_field_rule_id("partner."));
    assert!(!super::is_valid_field_rule_id("partner/../x"));
    assert!(!super::is_valid_field_rule_id(""));
}
