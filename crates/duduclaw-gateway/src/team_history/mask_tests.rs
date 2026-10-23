use super::*;
use crate::session::SessionMessage;
use std::collections::BTreeMap;

fn row(role: &str, content: &str) -> SessionMessage {
    SessionMessage {
        role: role.into(),
        content: content.into(),
        tokens: 0,
        timestamp: "2026-10-23T00:00:00Z".into(),
    }
}

#[test]
fn external_transcript_masks_pii_on_both_sides_with_stable_numbers() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "").unwrap();
    let masker = ViewMasker::for_home(home.path()).unwrap();
    let rows = [
        row(
            "user",
            "[sender_id: U0123456789abcdef0123456789abcdef]\n電話0912-345-678，email a@example.com，身分證 A123456789",
        ),
        row("assistant", "再確認0912345678和a@example.com"),
    ];
    let out = masker
        .mask_transcript(&rows, &BTreeMap::new(), "業務")
        .unwrap();
    let json = serde_json::to_value(&out).unwrap();
    let text = json.to_string();
    assert!(
        !text.contains("0912")
            && !text.contains("a@example.com")
            && !text.contains("A123456789")
            && !text.contains("U012345"),
        "{text}"
    );
    assert_eq!(json["messages"][0]["sender_label"], "客戶 1");
    assert_eq!(json["messages"][1]["sender_label"], "業務");
    let phones: Vec<_> = json["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| m["parts"].as_array().unwrap())
        .filter(|p| p["cat"] == "PHONE")
        .collect();
    assert_eq!(phones.len(), 2);
    assert!(phones.iter().all(|p| p["n"] == 1));
    assert!(!out.ner);
}

#[test]
fn filenames_paths_links_tokens_and_system_rows_do_not_escape() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "").unwrap();
    let masker = ViewMasker::for_home(home.path()).unwrap();
    let out = masker.mask_transcript(&[
        row("assistant", "📎DELIVER:/private/customer-name.txt\n![customer](https://host/secret-name.png)\n[report](file:///secret/report.pdf)\n<REDACT:EMAIL:abcd>"),
        row("system", "DO-NOT-RELEASE-SYSTEM-CONTENT"),
    ], &BTreeMap::new(), "業務").unwrap();
    let text = serde_json::to_string(&out).unwrap();
    for secret in [
        "customer-name",
        "secret-name",
        "file://",
        "/private/",
        "DO-NOT-RELEASE",
        "<REDACT:",
    ] {
        assert!(!text.contains(secret), "{text}");
    }
    assert!(text.contains("attachment"));
}

#[test]
fn custom_rules_apply_even_when_model_redaction_is_off_and_bad_rules_refuse() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "[redaction]\nenabled=false\n[redaction.rules.order]\ntype='regex'\npattern='ORDER-[0-9]+'\ncategory='ORDER'\npriority=200\n").unwrap();
    let masker = ViewMasker::for_home(home.path()).unwrap();
    let out = masker
        .mask_transcript(&[row("user", "ORDER-123")], &BTreeMap::new(), "業務")
        .unwrap();
    assert!(!serde_json::to_string(&out).unwrap().contains("ORDER-123"));
    std::fs::write(
        home.path().join("config.toml"),
        "[redaction.rules.bad]\ntype='regex'\npattern='['\ncategory='BAD'\npriority=1\n",
    )
    .unwrap();
    assert!(ViewMasker::for_home(home.path()).is_err());
}

#[test]
fn partial_high_priority_rules_cannot_uncover_the_rest_of_a_sensitive_match() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        "[redaction.rules.partial]\ntype='regex'\npattern='@'\ncategory='CUSTOM'\npriority=200\n",
    )
    .unwrap();
    let out = ViewMasker::for_home(home.path())
        .unwrap()
        .mask_transcript(
            &[row("user", "alice@example.com")],
            &BTreeMap::new(),
            "業務",
        )
        .unwrap();
    let text = serde_json::to_string(&out).unwrap();
    assert!(
        !text.contains("alice") && !text.contains("example.com"),
        "{text}"
    );
}

#[test]
fn fullwidth_pii_and_cjk_adjacent_identifiers_are_masked() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "").unwrap();
    let out = ViewMasker::for_home(home.path())
        .unwrap()
        .mask_transcript(
            &[
                row(
                    "user",
                    "身分證Ａ１２３４５６７８９電話０９１２－３４５－６７８",
                ),
                row("assistant", "電話0912345678"),
            ],
            &BTreeMap::new(),
            "業務",
        )
        .unwrap();
    let value = serde_json::to_value(&out).unwrap();
    let text = value.to_string();
    for secret in ["Ａ１２３", "A123", "０９１２", "0912"] {
        assert!(!text.contains(secret), "{text}");
    }
    let phones: Vec<_> = value["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| m["parts"].as_array().unwrap())
        .filter(|p| p["cat"] == "PHONE")
        .collect();
    assert_eq!(phones.len(), 2);
    assert!(phones.iter().all(|p| p["n"] == 1));
}

#[test]
fn nested_attachment_destinations_and_malformed_operator_metadata_are_never_text() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "").unwrap();
    let out=ViewMasker::for_home(home.path()).unwrap().mask_transcript(&[row("user","![x](https://host/(2026)/王小明.pdf)\n[report](file:///folder\\(x\\)/林大明.xlsx)\n<system_operator_pending>CONTROL-NONCE")],&BTreeMap::new(),"業務").unwrap();
    let text = serde_json::to_string(&out).unwrap();
    for secret in [
        "王小明",
        "林大明",
        "CONTROL-NONCE",
        "system_operator_pending",
        "https://",
        "file://",
    ] {
        assert!(!text.contains(secret), "{text}");
    }
    assert_eq!(
        out.messages[0]
            .parts
            .iter()
            .filter(|p| matches!(p, Part::Attachment { .. }))
            .count(),
        2
    );
}

#[test]
fn recognized_platform_identifiers_are_masked_without_a_model() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "").unwrap();
    let identifiers = [
        "U123ABCDEF",
        "123456789012345678901",
        "ou_0123456789abcdef0123456789abcdef",
        "01234567-89ab-cdef-0123-456789abcdef",
    ];
    let out = ViewMasker::for_home(home.path())
        .unwrap()
        .mask_transcript(
            &[row("assistant", &identifiers.join(" "))],
            &BTreeMap::new(),
            "業務",
        )
        .unwrap();
    let text = serde_json::to_string(&out).unwrap();
    for id in identifiers {
        assert!(!text.contains(id), "{text}");
    }
}

#[test]
fn original_custom_rules_survive_nfkc_value_and_boundary_changes() {
    for rule in [
        "type='keyword'\nvalues=['ＡＢＣ']",
        "type='regex'\npattern='ＡＢＣ'",
        "type='regex'\npattern='\\bSECRET\\b'",
        "type='keyword'\nvalues=['A']",
    ] {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            format!("[redaction.rules.private]\n{rule}\ncategory='PRIVATE'\npriority=300\n"),
        )
        .unwrap();
        let input = if rule.contains("SECRET") {
            "℠SECRET"
        } else if rule.contains("values=['A']") {
            "A\u{030a} B"
        } else {
            "預約ＡＢＣ公司"
        };
        let out = ViewMasker::for_home(home.path())
            .unwrap()
            .mask_transcript(&[row("user", input)], &BTreeMap::new(), "業務")
            .unwrap();
        let text = serde_json::to_string(&out).unwrap();
        for secret in ["ＡＢＣ", "ABC", "SECRET", "Å"] {
            assert!(!text.contains(secret), "rule={rule} {text}");
        }
        assert!(
            out.messages[0]
                .parts
                .iter()
                .any(|p| matches!(p,Part::Mask{cat,..}if cat=="PRIVATE"||cat=="REDACTED")),
            "{text}"
        );
    }
}

#[test]
fn split_wide_matches_share_numbers_and_refresh_keeps_visible_numbers_stable() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"),"[redaction.rules.whole]\ntype='regex'\npattern='(?s)BEGIN.*END'\ncategory='WHOLE'\npriority=1\n").unwrap();
    let masker = ViewMasker::for_home(home.path()).unwrap();
    let input = format!("BEGIN\n{}END", "電話0912-345-678\n".repeat(128));
    let rows = [row("user", &input)];
    let first = masker
        .mask_transcript(&rows, &BTreeMap::new(), "業務")
        .unwrap();
    assert!(
        first.messages[0]
            .parts
            .iter()
            .all(|p| matches!(p, Part::Mask { n: 1, .. }))
    );
    assert_eq!(first.masked_counts.get("PHONE"), Some(&128));
    let second = masker
        .mask_transcript(&rows, &BTreeMap::new(), "業務")
        .unwrap();
    assert_eq!(
        serde_json::to_value(first).unwrap(),
        serde_json::to_value(second).unwrap()
    );
}

#[test]
fn equal_priority_labels_do_not_depend_on_rule_map_iteration_order() {
    use duduclaw_redaction::rules::RuleSpec;
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "").unwrap();
    let a: RuleSpec =
        toml::from_str("id='a'\ntype='keyword'\nvalues=['Alice']\ncategory='A'\npriority=50")
            .unwrap();
    let z: RuleSpec =
        toml::from_str("id='z'\ntype='keyword'\nvalues=['Alice']\ncategory='Z'\npriority=50")
            .unwrap();
    let mut first = ViewMasker::for_home(home.path()).unwrap();
    first.engine = RuleEngine::from_specs(vec![a.clone(), z.clone()]).unwrap();
    let mut second = ViewMasker::for_home(home.path()).unwrap();
    second.engine = RuleEngine::from_specs(vec![z, a]).unwrap();
    let rows = [row("user", "Alice")];
    let a = first
        .mask_transcript(&rows, &BTreeMap::new(), "業務")
        .unwrap();
    let b = second
        .mask_transcript(&rows, &BTreeMap::new(), "業務")
        .unwrap();
    assert_eq!(
        serde_json::to_value(a).unwrap(),
        serde_json::to_value(b).unwrap()
    );
}
