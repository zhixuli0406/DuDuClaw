use super::*;
use std::path::Path;
use std::sync::Arc;

use duduclaw_redaction::RedactionManager;

const KEY: &str = "sk-ant-api03-AbCdEfGhIjKlMnOpQrStUvWxYz0123456789abcdefABCDEF-QQ";

fn manager(home: &Path, tables: &str) -> Arc<RedactionManager> {
    let src = format!("enabled = true\nprofiles = [\"general\"]\n{tables}");
    let cfg: duduclaw_redaction::RedactionConfig = toml::from_str(&src).unwrap();
    Arc::new(
        RedactionManager::open(cfg, duduclaw_redaction::ManagerPaths::under_home(home)).unwrap(),
    )
}

fn ctx<'a>(home: &'a Path, engine: Option<&'a RuleEngine>) -> SanitizeCtx<'a> {
    SanitizeCtx {
        rules: std::sync::Arc::new(crate::channel_reply::OutboundRules::load(home, "agnes")),
        engine,
    }
}

fn text_of(s: Sanitized) -> (String, bool) {
    match s {
        Sanitized::Text {
            text, truncated, ..
        } => (text, truncated),
        Sanitized::Blocked(b) => panic!("unexpected block: {b:?}"),
    }
}

#[test]
fn vault_tokens_are_masked_and_never_restored() {
    let home = tempfile::tempdir().unwrap();
    let m = manager(home.path(), "");
    // A real vault-backed token for this agent/session: restore WOULD give
    // the e-mail back. The thinking path must not.
    let tokenized = crate::redaction_sources::redact_text(
        Some(&m),
        "agnes",
        Some("webchat:x:1"),
        "聯絡 alice@example.com 確認",
        &duduclaw_redaction::Source::ToolResult {
            tool_name: "crm".into(),
        },
    )
    .unwrap();
    assert!(
        tokenized.contains(duduclaw_redaction::token::TOKEN_PREFIX),
        "{tokenized}"
    );
    let restored = crate::redaction_sources::restore_for_user(
        Some(&m),
        "agnes",
        Some("webchat:x:1"),
        tokenized.clone(),
    );
    assert!(
        restored.contains("alice@example.com"),
        "fixture sanity: {restored}"
    );

    let thinking = format!("我要先寫信給 {tokenized} 再整理");
    let (out, _) = text_of(sanitize_for_display(
        &thinking,
        &ctx(home.path(), Some(m.engine())),
        1_500,
    ));
    assert!(!out.contains("alice@example.com"), "{out}");
    assert!(
        !out.contains(duduclaw_redaction::token::TOKEN_PREFIX),
        "{out}"
    );
    assert!(out.contains("〔已遮蔽·"), "{out}");
}

#[test]
fn rule_engine_spans_are_masked_without_a_vault_write() {
    let home = tempfile::tempdir().unwrap();
    let rule = "[rules.order_no]\ntype = \"regex\"\npattern = 'ORD-[0-9]{6}'\ncategory = \"ORDER\"\npriority = 60\n";
    let m = manager(home.path(), rule);
    let (out, _) = text_of(sanitize_for_display(
        "客戶的訂單 ORD-123456 還沒出貨，email bob@example.com",
        &ctx(home.path(), Some(m.engine())),
        1_500,
    ));
    assert!(out.contains("〔已遮蔽·ORDER〕"), "{out}");
    assert!(!out.contains("ORD-123456"), "{out}");
    assert!(!out.contains("bob@example.com"), "{out}");
    // Display masking never writes the vault: nothing maps back to a value.
    let back = crate::redaction_sources::restore_for_user(
        Some(&m),
        "agnes",
        Some("webchat:x:1"),
        out.clone(),
    );
    assert_eq!(
        back, out,
        "nothing in the display text maps back to a value"
    );
}

#[test]
fn anthropic_keys_are_replaced_whole_even_next_to_cjk() {
    let home = tempfile::tempdir().unwrap();
    for text in [
        format!("先用金鑰 {KEY} 呼叫"),
        format!("金鑰是{KEY}，然後呼叫"),
        format!("Authorization: Bearer {KEY}"),
    ] {
        let (out, _) = text_of(sanitize_for_display(&text, &ctx(home.path(), None), 1_500));
        assert!(!out.contains("AbCdEfGh"), "{out}");
        assert!(!out.contains("-QQ"), "no tail characters either: {out}");
        assert!(out.contains(SECRET_PLACEHOLDER), "{out}");
    }
    // Ordinary text is untouched by the secret pass.
    let plain = "比對 Q3 營收 與 Q2 差異";
    assert_eq!(mask_text(plain, None), plain);
}

#[test]
fn layered_masking_preserves_placeholders_in_both_orders() {
    for marker in ["[REDACTED]", "[已遮罩的 API key cred_test]", "[已遮罩的密碼]", SECRET_PLACEHOLDER] {
        let text = format!("金鑰是{marker}，然後呼叫");
        assert_eq!(mask_text(&text, None), text);
        assert_eq!(mask_secret_words(&text), text);
    }
    let home = tempfile::tempdir().unwrap();
    let m = manager(home.path(), "[rules.marker_word]\ntype = \"regex\"\npattern = 'cred_test'\ncategory = \"TEST\"\npriority = 60\n");
    let marker = "[已遮罩的 API key cred_test]";
    assert_eq!(mask_text(marker, Some(m.engine())), marker);
    let forged = format!("〔已遮蔽·AKIA{}〕", "A".repeat(16));
    assert!(!mask_text(&forged, None).contains(&"A".repeat(16)));
    let raw = format!("金鑰是{KEY}，然後呼叫");
    let once = mask_text(&raw, None);
    assert_eq!(once, format!("金鑰是{SECRET_PLACEHOLDER}，然後呼叫"));
    assert_eq!(mask_text(&once, None), once);
    let first = duduclaw_security::credential_detect::mask(&raw);
    assert_eq!(mask_text(&first, None), first);
}

#[test]
fn control_chars_and_think_tags_are_cleaned() {
    let out = mask_text("<think>想一下\u{7}\r\n\n\n\n下一步</think>\t好", None);
    assert_eq!(out, "想一下\n\n下一步\t好");
}

#[test]
fn truncation_is_by_chars_and_cjk_emoji_safe() {
    let home = tempfile::tempdir().unwrap();
    let text = "思考🙂".repeat(40); // 120 chars, multi-byte
    let (out, truncated) = text_of(sanitize_for_display(&text, &ctx(home.path(), None), 50));
    assert!(truncated);
    assert_eq!(out.chars().count(), 50);
    let (out, truncated) = text_of(sanitize_for_display("短句", &ctx(home.path(), None), 50));
    assert!(!truncated);
    assert_eq!(out, "短句");
    assert_eq!(
        last_line("第一行\n\n  第二行 🙂  \n\n"),
        Some("第二行 🙂".to_string())
    );
    assert_eq!(
        last_line(&"字".repeat(200)).unwrap().chars().count(),
        LINE_CHARS
    );
    assert_eq!(last_line("  \n "), None);
}

#[test]
fn hold_back_keeps_the_unfinished_tail() {
    // The tail after the last whitespace waits for more text.
    let (shown, tail) = split_hold_back("checking the key sk-ant-api0", 64);
    assert_eq!(shown, "checking the key ");
    assert_eq!(tail, "sk-ant-api0");
    // Whitespace-free CJK still flows: only the last 64 chars wait.
    let cjk = "字".repeat(100);
    let (shown, tail) = split_hold_back(&cjk, 64);
    assert_eq!(shown.chars().count(), 36);
    assert_eq!(tail.chars().count(), 64);
    // A long whitespace-free ASCII word (a key mid-stream) is held whole,
    // even past the 64-char window, so no head of it is shown.
    let long_key = format!("token: {}", "A1b2C3d4".repeat(12)); // 96-char word
    let (shown, tail) = split_hold_back(&long_key, 64);
    assert_eq!(shown, "token: ");
    assert_eq!(tail.len(), 96);
    // …but CJK right before it still flows.
    let mixed = format!("{}{}", "字".repeat(10), "Z".repeat(70));
    let (shown, tail) = split_hold_back(&mixed, 64);
    assert_eq!(shown, "字".repeat(10));
    assert_eq!(tail, "Z".repeat(70));
    // Sentence cut (used on top of this when redaction is on).
    assert_eq!(split_at_sentence("一句。二句還在"), ("一句。", "二句還在"));
    assert_eq!(
        split_at_sentence("line one\nline tw"),
        ("line one\n", "line tw")
    );
    assert_eq!(split_at_sentence("Done! next"), ("Done!", " next"));
    assert_eq!(split_at_sentence("沒有結尾"), ("", "沒有結尾"));
    // Short text: everything waits until whitespace or the end.
    assert_eq!(split_hold_back("abc", 64), ("", "abc"));
    assert_eq!(split_hold_back("", 64), ("", ""));
}

fn write_contract(home: &Path, must_not: &str) {
    let dir = home.join("agents").join("agnes");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("CONTRACT.toml"),
        format!("[boundaries]\nmust_not = [\"{must_not}\"]\n"),
    )
    .unwrap();
}

#[test]
fn contract_and_guardrail_refusals_are_returned_not_applied() {
    let home = tempfile::tempdir().unwrap();
    write_contract(home.path(), "reveal api keys");
    match sanitize_for_display(
        "I could reveal api keys here",
        &ctx(home.path(), None),
        1_500,
    ) {
        Sanitized::Blocked(OutboundBlock::Contract(rules)) => assert!(!rules.is_empty()),
        other => panic!("expected a contract block, got {other:?}"),
    }
    // A refusal here never writes the audit by itself (the gate's owner does).
    assert!(!home.path().join("security_audit.jsonl").exists());

    let dir = home.path().join("agents").join("agnes");
    std::fs::write(
        dir.join("agent.toml"),
        "[guardrails]\nenabled = true\nredact_pii = true\ndeny_phrases = [\"project zebra\"]\n",
    )
    .unwrap();
    match sanitize_for_display("about project zebra", &ctx(home.path(), None), 1_500) {
        Sanitized::Blocked(OutboundBlock::Guardrail(_)) => {}
        other => panic!("expected a guardrail block, got {other:?}"),
    }
    let (out, _) = text_of(sanitize_for_display(
        "mail carol@example.com later",
        &ctx(home.path(), None),
        1_500,
    ));
    assert!(
        !out.contains("carol@example.com"),
        "guardrail PII redaction applies: {out}"
    );
}

// Written for F1; not run (the review task prohibits builds/tests).
#[test]
fn display_sink_masks_context_passwords_without_operator_redaction() {
    let masked = mask_text("密碼：fixture-thinking", None);
    assert!(!masked.contains("fixture-thinking"));
    assert!(!mask_text("password: fixture-thinking", None).contains("fixture-thinking"));
}
