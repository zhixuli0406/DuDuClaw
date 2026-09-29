//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

#[tokio::test]
pub(super) async fn profile_import_of_a_cjk_pack_derives_its_own_slug() {
    // Live-test regression: a Taiwanese pack named 「製造業客戶包」 sent
    // with no `name` param used to fail with 「請自行指定」, and the
    // approved import dialog has no name field to specify one from.
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_config(home.path(), ENABLED_CONFIG).await;
    const CJK_PACK: &str = r#"
[meta]
name = "製造業客戶包"

[meta.labels]
CUSTOM_ACME_ID = "客戶編號"

[rules.acme_id]
type = "regex"
pattern = 'ACME-\d{5}'
category = "CUSTOM_ACME_ID"
"#;

    let dry = payload(
        &handler
            .handle_redaction_profiles_import(json!({ "toml": CJK_PACK, "dry_run": true }))
            .await,
    );
    let slug = dry["name"].as_str().expect("name").to_string();
    assert!(slug.starts_with("pack_"), "{slug}");
    assert_eq!(slug.chars().count(), 13, "{slug}");
    assert_eq!(dry["imported"], 1);
    assert!(!listed_profiles(home.path()).contains(&slug));

    let wet = payload(
        &handler
            .handle_redaction_profiles_import(json!({ "toml": CJK_PACK }))
            .await,
    );
    assert_eq!(wet["name"], slug.as_str(), "the slug must be stable");
    assert_eq!(wet["applied"], true, "{wet}");
    assert!(listed_profiles(home.path()).contains(&slug));

    // The readable name survives as the profile's label, and the pack's
    // own category label reaches `redaction.get`.
    let p = payload(&handler.handle_redaction_get().await);
    let row = p["available_profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["name"] == slug.as_str())
        .expect("imported profile listed");
    assert_eq!(row["label"], "製造業客戶包");
    assert_eq!(row["custom"], true);
    assert_eq!(p["category_labels"]["CUSTOM_ACME_ID"], "客戶編號");

    // Re-import overwrites: still exactly one file, still listed once.
    payload(
        &handler
            .handle_redaction_profiles_import(json!({ "toml": CJK_PACK }))
            .await,
    );
    assert_eq!(
        listed_profiles(home.path())
            .iter()
            .filter(|n| *n == &slug)
            .count(),
        1
    );
}

#[tokio::test]
pub(super) async fn profile_import_refuses_reserved_and_builtin_names() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_config(home.path(), ENABLED_CONFIG).await;
    for name in ["custom", "general", "taiwan_strict"] {
        let frame = handler
            .handle_redaction_profiles_import(
                json!({ "toml": PACK_TOML, "name": name, "dry_run": true }),
            )
            .await;
        assert!(
            error_text(&frame).contains("保留"),
            "{name}: {}",
            error_text(&frame)
        );
    }
    // And a built-in can never be deleted.
    let frame = handler
        .handle_redaction_profiles_remove(json!({ "name": "general" }))
        .await;
    assert!(
        error_text(&frame).contains("內建規則集"),
        "{}",
        error_text(&frame)
    );
}

#[tokio::test]
pub(super) async fn profile_import_with_no_usable_rules_is_an_error() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_config(home.path(), ENABLED_CONFIG).await;
    let frame = handler
        .handle_redaction_profiles_import(json!({
            "toml": "[meta]\nname = \"x\"\n\n[rules.a]\ntype = \"regex\"\npattern = '[bad'\ncategory = \"X\"\n",
            "dry_run": true,
        }))
        .await;
    assert!(
        error_text(&frame).contains("沒有任何可用的規則"),
        "{}",
        error_text(&frame)
    );
    // Missing param.
    assert!(!error_text(&handler.handle_redaction_profiles_import(json!({})).await).is_empty());
}

// ── dry_run: unsaved draft rules + plain-text samples ──────────────

#[tokio::test]
pub(super) async fn dry_run_previews_an_unsaved_draft_rule_over_plain_text() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_live_redaction(home.path(), ENABLED_CONFIG).await;

    let p = payload(
        &handler
            .handle_redaction_dry_run(json!({
                "sample_text": "工號 EMP-2024-0133 與 EMP-2025-0007 都已離職",
                "draft_rules": [{
                    "id": "employee_id",
                    "label": "員工編號",
                    "category": "CUSTOM_EMPLOYEE_ID",
                    "kind": "regex",
                    "pattern": r"EMP-\d{4}-\d{4}",
                }],
            }))
            .await,
    );
    let hits = p["hits"].as_array().expect("hits array");
    let mine: Vec<&Value> = hits
        .iter()
        .filter(|h| h["rule_id"] == "employee_id")
        .collect();
    assert_eq!(mine.len(), 2, "two hits expected, got {hits:?}");
    for h in mine {
        assert_eq!(h["category"], "CUSTOM_EMPLOYEE_ID");
        assert!(h["token"].as_str().unwrap().starts_with("<REDACT:"));
    }

    // Nothing was persisted: the draft must not appear in the saved rules
    // and must not be listed as a profile.
    assert!(
        crate::redaction_custom_rules::list_rules(home.path())
            .unwrap()
            .is_empty()
    );
    assert!(!listed_profiles(home.path()).contains(&"custom".to_string()));
}

#[tokio::test]
pub(super) async fn dry_run_draft_keyword_rule_also_previews() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_live_redaction(home.path(), ENABLED_CONFIG).await;
    let p = payload(
        &handler
            .handle_redaction_dry_run(json!({
                "sample_text": "客戶是台積電，聯絡人待補",
                "draft_rules": [{
                    "id": "customer_code",
                    "category": "CUSTOM_01",
                    "kind": "keyword",
                    "keywords": ["台積電"],
                }],
            }))
            .await,
    );
    assert!(
        p["hits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["rule_id"] == "customer_code" && h["category"] == "CUSTOM_01"),
        "{p}"
    );
}

#[tokio::test]
pub(super) async fn dry_run_draft_overrides_a_saved_rule_with_the_same_id() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_config(home.path(), ENABLED_CONFIG).await;
    // Save a rule that does NOT match the sample...
    payload(
        &handler
            .handle_redaction_custom_rules_upsert(json!({
                "label": "Employee ID",
                "kind": "regex",
                "pattern": r"OLD-\d{4}",
            }))
            .await,
    );
    // ...then preview an edit to it under the same id.
    let p = payload(
        &handler
            .handle_redaction_dry_run(json!({
                "sample_text": "工號 EMP-2024-0133",
                "draft_rules": [{
                    "id": "employee_id",
                    "category": "CUSTOM_EMPLOYEE_ID",
                    "kind": "regex",
                    "pattern": r"EMP-\d{4}-\d{4}",
                }],
            }))
            .await,
    );
    assert!(
        p["hits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["rule_id"] == "employee_id"),
        "the draft must shadow the saved rule: {p}"
    );
    // The saved rule is untouched.
    let saved = crate::redaction_custom_rules::list_rules(home.path()).unwrap();
    assert_eq!(saved[0]["pattern"], r"OLD-\d{4}");
}

#[tokio::test]
pub(super) async fn dry_run_rejects_a_bad_draft_without_running_anything() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_config(home.path(), ENABLED_CONFIG).await;
    for bad in [
        json!([{ "id": "x", "category": "CUSTOM_X", "kind": "regex", "pattern": "[unclosed" }]),
        json!([{ "id": "x", "category": "CUSTOM_X", "kind": "keyword", "keywords": ["a"] }]),
        json!([{ "id": "X", "category": "CUSTOM_X", "kind": "regex", "pattern": "a" }]),
        json!([{ "id": "x", "category": "bad-cat", "kind": "regex", "pattern": "a" }]),
        json!([{ "id": "x", "kind": "regex", "pattern": "a" }]),
        json!([{ "category": "CUSTOM_X", "kind": "regex", "pattern": "a" }]),
        json!([{ "id": "x", "category": "CUSTOM_X", "kind": "ner" }]),
        json!("not-an-array"),
        json!([
            { "id": "x", "category": "CUSTOM_X", "kind": "regex", "pattern": "a" },
            { "id": "x", "category": "CUSTOM_Y", "kind": "regex", "pattern": "b" },
        ]),
    ] {
        let frame = handler
            .handle_redaction_dry_run(json!({
                "sample_text": "anything",
                "draft_rules": bad,
            }))
            .await;
        assert!(
            !error_text(&frame).is_empty(),
            "should reject draft_rules = {bad}"
        );
    }
}

#[tokio::test]
pub(super) async fn dry_run_without_drafts_is_unchanged_and_sample_json_wins() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_live_redaction(home.path(), ENABLED_CONFIG).await;

    // No drafts, plain text: still runs against the live rule set.
    let p = payload(
        &handler
            .handle_redaction_dry_run(json!({ "sample_text": "mail me at a@b.com" }))
            .await,
    );
    assert!(
        p["hits"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["category"] == "EMAIL"),
        "the general profile's email rule should fire: {p}"
    );

    // Both given ⇒ sample_json wins.
    let p = payload(
        &handler
            .handle_redaction_dry_run(json!({
                "sample_json": "{\"email\":\"a@b.com\"}",
                "sample_text": "no email here at all",
            }))
            .await,
    );
    assert_eq!(p["token_count"], 1, "{p}");

    // Neither given ⇒ the same error as before this parameter existed.
    assert!(!error_text(&handler.handle_redaction_dry_run(json!({})).await).is_empty());
    assert!(
        !error_text(
            &handler
                .handle_redaction_dry_run(json!({ "sample_text": "   " }))
                .await
        )
        .is_empty()
    );
}

#[tokio::test]
pub(super) async fn suggest_pattern_validates_before_calling_any_engine() {
    let home = tempfile::tempdir().unwrap();
    let handler = handler_with_config(home.path(), ENABLED_CONFIG).await;
    for bad in [
        json!({ "examples": ["only-one"] }),
        json!({ "examples": [] }),
        json!({ "examples": "not-an-array" }),
    ] {
        let frame = handler
            .handle_redaction_suggest_pattern(bad.clone(), "tester")
            .await;
        assert!(!error_text(&frame).is_empty(), "should reject {bad}");
    }
}
