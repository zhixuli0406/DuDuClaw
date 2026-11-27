use super::*;

fn record() -> ApprovalRecord {
    serde_json::from_value(json!({"id":"01234567-89ab-4def-8123-456789abcdef", "agent_id":"avatar", "action_kind":ACTION_KIND,
        "summary":"管理員 AI 設定變更確認", "status":"pending", "created_at":chrono::Utc::now().to_rfc3339(), "ttl_seconds":600,
        "payload":{"admin_name":"管理員\u{202e}", "avatar_name":"分身\u{200b}", "employee_count":1, "reason":"說明\u{1b}",
            "items":[{"seq":1, "target":"worker", "employee":"員工\u{2066}", "label":"主要模型",
                "before":"舊\u{7f}", "after":"新\u{200b}"}]}})).unwrap()
}

#[test]
fn card_escapes_invisible_characters() {
    let rec = record();
    let raw = rec.payload.clone();
    let body = card_body(&rec, Some("A9b2"));
    for c in ['\u{202e}', '\u{200b}', '\u{2066}', '\u{1b}', '\u{7f}'] { assert!(!body.contains(c)); }
    for s in ["\\u{202e}", "\\u{200b}", "\\u{2066}", "\\u{1b}", "\\u{7f}"] { assert!(body.contains(s)); }
    assert!(body.contains("確認 01234567-89ab-4def-8123-456789abcdef A9b2"));
    assert_eq!(rec.payload, raw);
    assert!(!confirmation_display(&rec).to_string().contains("action_grant"));
}

#[test]
fn long_schedule_prompt_stays_complete_in_inbox_and_channel_gets_notice() {
    let mut rec = record();
    let prompt = format!("{}最後一句", "完整提示".repeat(800));
    rec.payload["items"][0]["label"] = json!("修改排程");
    rec.payload["items"][0]["after"] = json!({"name":"每日", "cron":"0 9 * * *", "task":prompt});
    assert!(card_body(&rec, None).contains(&prompt));
    assert_eq!(channel_body(&rec, "A9b2"), NOTICE);
    assert!(channel_body(&rec, "A9b2").chars().count() <= 1800);
    assert!(!channel_body(&rec, "A9b2").contains(rec.id.as_str()));
}

#[test]
fn check_code_hash_is_salted_with_approval_id() {
    assert_ne!(check_code_hash("first", "A9b2"), check_code_hash("second", "A9b2"));
    assert_ne!(check_code_hash("first", "A9b2"), check_code_hash("first", "A9b3"));
    assert_eq!(check_code_hash("first", "A9b2").len(), 64);
}

#[test]
fn unreceipted_notice_never_advertises_text_confirmation() {
    let rec = record();
    let notice = channel_body(&rec, "A9b2");
    assert!(notice.contains("儀表板收件匣"));
    assert!(!notice.contains("A9b2"));
    assert!(!notice.contains(rec.id.as_str()));
}
