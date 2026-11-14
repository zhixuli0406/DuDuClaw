use super::*;

use serde_json::json;

const AGENT: &str = "emp";
const SESSION: &str = "line:U1";
const TURN: &str = "line:U1#turn-now";

fn fresh_home() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let p = std::env::temp_dir().join(format!(
        "dudu-claims-{}-{}-{nanos}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn ts(now: chrono::DateTime<chrono::Utc>, minutes_ago: i64) -> String {
    (now - chrono::Duration::minutes(minutes_ago)).to_rfc3339()
}

/// A successful row of `tool` for AGENT in SESSION, `minutes_ago` old, on an
/// earlier turn (so only the time window can make it count).
fn row(now: chrono::DateTime<chrono::Utc>, tool: &str, minutes_ago: i64) -> Value {
    json!({
        "timestamp": ts(now, minutes_ago),
        "agent_id": AGENT,
        "tool_name": tool,
        "params_summary": "",
        "success": true,
        "session_id": SESSION,
        "turn_id": "line:U1#turn-earlier",
    })
}

fn write_rows(path: &std::path::Path, rows: &[Value]) {
    let body: String = rows.iter().map(|r| format!("{r}\n")).collect();
    std::fs::write(path, body).unwrap();
}

fn detect(home: &std::path::Path, output: &str, now: chrono::DateTime<chrono::Utc>) -> Vec<UnbackedClaim> {
    detect_unbacked_workspace_claims(home, AGENT, Some(SESSION), Some(TURN), output, now).unwrap()
}

fn claim(claim_type: ClaimType) -> ActionClaim {
    ActionClaim { claim_type, target_id: String::new(), matched_text: "x".into() }
}

fn types(output: &str) -> Vec<ClaimType> {
    extract_workspace_claims(output).into_iter().map(|c| c.claim_type).collect()
}

// ── Extraction ───────────────────────────────────────────

#[test]
fn workspace_patterns_all_compile() {
    let specs = workspace_specs();
    for (pat, _) in &specs {
        assert!(Regex::new(pat).is_ok(), "pattern does not compile: {pat}");
    }
    assert_eq!(WORKSPACE_PATTERNS.len(), specs.len());
    let set = WORKSPACE_SET.as_ref().expect("the prefilter set compiles");
    assert_eq!(set.len(), specs.len());
    for family in WORKSPACE_FAMILIES {
        assert!(family.is_workspace());
        assert!(specs.iter().any(|(_, t)| *t == family), "{family:?} has no pattern");
    }
}

#[test]
fn each_family_extracts_positive_phrases() {
    let cases: &[(&str, ClaimType)] = &[
        ("好的，已寄出。", ClaimType::MailSent),
        ("已寄送給王經理。", ClaimType::MailSent),
        ("信已寄給客戶了。", ClaimType::MailSent),
        ("郵件已發送。", ClaimType::MailSent),
        ("我已發信通知大家。", ClaimType::MailSent),
        ("已經幫您把信寄出了。", ClaimType::MailSent),
        ("I've sent the email to Bob.", ClaimType::MailSent),
        ("Email sent.", ClaimType::MailSent),
        ("The email has been sent.", ClaimType::MailSent),
        ("已建立行程：週一 10:00 開會。", ClaimType::CalendarEventCreated),
        ("已排入行事曆。", ClaimType::CalendarEventCreated),
        ("已新增活動「季度檢討」。", ClaimType::CalendarEventCreated),
        ("已經幫您把會議排入行事曆。", ClaimType::CalendarEventCreated),
        ("行程已建立。", ClaimType::CalendarEventCreated),
        ("Event created for Monday.", ClaimType::CalendarEventCreated),
        ("I've added it to your calendar.", ClaimType::CalendarEventCreated),
        ("Added to your calendar.", ClaimType::CalendarEventCreated),
        ("已寫入試算表。", ClaimType::SheetWritten),
        ("已新增到試算表第 5 列。", ClaimType::SheetWritten),
        ("已更新試算表。", ClaimType::SheetWritten),
        ("試算表已更新完成。", ClaimType::SheetWritten),
        ("已經幫您把這筆資料寫入試算表。", ClaimType::SheetWritten),
        ("I've added a row to the sheet.", ClaimType::SheetWritten),
        ("Appended to the sheet.", ClaimType::SheetWritten),
        ("I appended the data to your spreadsheet.", ClaimType::SheetWritten),
        ("已寫入文件。", ClaimType::DocWritten),
        ("已更新文件。", ClaimType::DocWritten),
        ("已經把會議紀錄附加到 Google 文件。", ClaimType::DocWritten),
        ("Appended to the doc.", ClaimType::DocWritten),
        ("I've appended the summary to the Google Doc.", ClaimType::DocWritten),
        ("已建立任務。", ClaimType::TaskCreated),
        ("已新增待辦：買咖啡。", ClaimType::TaskCreated),
        ("已加入任務看板。", ClaimType::TaskCreated),
        ("任務已建立。", ClaimType::TaskCreated),
        ("Task created.", ClaimType::TaskCreated),
        ("I've created a new task for this.", ClaimType::TaskCreated),
    ];
    for (text, expected) in cases {
        let found = types(text);
        assert!(found.contains(expected), "{text:?} should extract {expected:?}, got {found:?}");
        let claims = extract_workspace_claims(text);
        assert!(claims.iter().all(|c| c.target_id.is_empty() && !c.matched_text.is_empty()), "{text:?}");
    }
}

#[test]
fn negations_plans_and_questions_extract_nothing() {
    for text in [
        // The phrases the spec lists, bare.
        "尚未寄出",
        "還沒寄出",
        "不會寄出",
        "是否要寄出",
        "寄出後",
        "將會寄出",
        "待確認後才會寄出",
        "已排入待確認",
        "草稿已建立",
        "not sent",
        "will send",
        "once sent",
        // The same in sentences an employee actually writes.
        "這封信尚未寄出，需要管理員確認。",
        "郵件已排入待確認，確認後才會寄出。",
        "草稿已建立，尚未寄出。",
        "請問是否已寄出？",
        "信已寄出了嗎？",
        "已寄出後我會通知你。",
        "不要說已寄出。",
        "是否已建立行程？",
        "任務已建立了嗎？",
        "行程將會建立在下週。",
        "Email not sent yet.",
        "I will send the email once you confirm.",
        "Once sent, I'll let you know.",
        "Has the email been sent?",
        "Was the email sent?",
        "Once the email was sent, nothing else is needed.",
        "The event will be created after approval.",
        "It will be added to your calendar once approved.",
        "Should I add a row to the sheet?",
        "Was the task created?",
    ] {
        let found = types(text);
        assert!(found.is_empty(), "{text:?} must not extract a claim, got {found:?}");
    }
}

#[test]
fn guards_cover_the_whole_sentence() {
    for text in [
        // The two review examples.
        "Was the email sent to Bob and Carol yesterday afternoon?",
        "Once, the email was sent, nothing else is needed.",
        // Long questions and markers far before the match.
        "Has the quarterly summary email for the whole regional sales team been sent?",
        "請問給王經理和李經理的那封季度報告郵件已發送了嗎",
        "郵件已發送給王經理和李經理以及所有區域業務同仁？",
        "有沒有確認一下，信已寄給客戶",
        "如果主管同意，郵件已發送給所有人。",
        "一旦確認，我就會說已寄出。",
        "若需要的話，已建立行程這件事可以再改。",
        "等主管核准，任務已建立就會通知你。",
        "這筆資料將在明天，已寫入試算表的部分不變。",
        "我們之後，已更新文件。",
        "If you approve, then, the event was created for Monday.",
        "When you are ready, I've sent the email to Bob.",
        "I could confirm, I think, that the email was sent.",
        "Not yet, but the task was created in draft.",
        "The email is going to be sent, I've sent the email in my head.",
        "It is to be added, added to your calendar.",
    ] {
        let found = types(text);
        assert!(found.is_empty(), "{text:?} must not extract a claim, got {found:?}");
    }
}

#[test]
fn newline_ends_a_sentence() {
    // A question or condition ending in a newline still guards its own line.
    for text in [
        "Quick check:\nwas the email sent to Bob and Carol yesterday afternoon?\nThanks",
        "請問\n信已寄出了嗎？",
        "如果主管同意，郵件已發送\n",
        "Once, the email was sent,\nnothing else is needed.",
        "是否\n\n已寄出？",
    ] {
        let found = types(text);
        assert!(found.is_empty(), "{text:?} must not extract a claim, got {found:?}");
    }
    // A marker on an earlier line does not guard a later line.
    for (text, expected) in [
        ("這件事還沒處理\n已寄出給王經理", ClaimType::MailSent),
        ("Not ready yesterday\nEmail sent.\n", ClaimType::MailSent),
        ("要不要排會議嗎\n已寫入試算表", ClaimType::SheetWritten),
        ("如果還有問題請告訴我。\n已建立任務。", ClaimType::TaskCreated),
    ] {
        let found = types(text);
        assert_eq!(found, vec![expected.clone()], "{text:?}");
    }
}

#[test]
fn scan_is_bounded_to_the_first_max_scan_chars() {
    let padding = "好。".repeat(MAX_SCAN_CHARS / 2);
    assert_eq!(padding.chars().count(), MAX_SCAN_CHARS);
    assert!(types(&format!("{padding}已寄出。")).is_empty(), "a claim past the bound is not scanned");
    let inside = "好。".repeat(MAX_SCAN_CHARS / 2 - 4);
    assert_eq!(types(&format!("{inside}已寄出。")), vec![ClaimType::MailSent]);
}

#[test]
fn workspace_phrases_are_not_agent_management_claims() {
    // Existing agent-management extraction is unchanged: it never returns a
    // workspace family, so the cron and bus paths behave exactly as before.
    let text = "好的，已寄出，也已建立行程、已寫入試算表、已更新文件、已建立任務。";
    assert!(super::super::extract_action_claims(text).iter().all(|c| !c.claim_type.is_workspace()));
    assert_eq!(types(text).len(), 5, "one claim per workspace family");
}

// ── Evidence logic (rows already scoped) ─────────────────

#[test]
fn evidence_set_per_family() {
    let now = chrono::Utc::now();
    for (family, tools) in [
        (ClaimType::CalendarEventCreated, &["calendar_create_event"][..]),
        (ClaimType::SheetWritten, &["sheets_append"][..]),
        (ClaimType::DocWritten, &["docs_append"][..]),
        (ClaimType::TaskCreated, &["tasks_create", "gtasks_create"][..]),
    ] {
        assert_eq!(family.evidence_tools(), tools);
        for tool in tools {
            let rows = vec![row(now, tool, 1)];
            assert!(verify_workspace_claims(&[claim(family.clone())], &rows).is_empty(), "{tool} backs {family:?}");
        }
        // A different family's tool is not evidence.
        let rows = vec![row(now, "gtasks_complete", 1), row(now, "sheets_read", 1)];
        let unbacked = verify_workspace_claims(&[claim(family.clone())], &rows);
        assert_eq!(unbacked.len(), 1, "{family:?}");
        assert_eq!(unbacked[0].kind, UnbackedKind::NoRecord);
    }
    let rows = vec![row(now, "calendar_create_event", 1)];
    let unbacked = verify_workspace_claims(&[claim(ClaimType::SheetWritten)], &rows);
    assert_eq!(unbacked[0].kind, UnbackedKind::NoRecord);
    // Agent-management claims are not handled here.
    assert!(verify_workspace_claims(&[claim(ClaimType::AgentCreated)], &[]).is_empty());
}

#[test]
fn failed_and_usage_only_rows_are_not_evidence() {
    let now = chrono::Utc::now();
    let mut failed = row(now, "calendar_create_event", 1);
    failed["success"] = json!(false);
    let mut usage = row(now, "calendar_create_event", 1);
    usage["usage_only"] = json!(true);
    let unbacked = verify_workspace_claims(&[claim(ClaimType::CalendarEventCreated)], &[failed, usage]);
    assert_eq!(unbacked.len(), 1);
    assert_eq!(unbacked[0].kind, UnbackedKind::NoRecord);
}

#[test]
fn mail_with_only_draft_or_pending_rows_is_draft_only() {
    let now = chrono::Utc::now();
    let mut pending = row(now, "mail_send", 1);
    pending["state"] = json!("pending_confirmation");
    pending["sent"] = json!(false);
    let mut draft = row(now, "gmail_create_draft", 1);
    draft["state"] = json!("draft");
    for rows in [vec![pending.clone()], vec![draft.clone()], vec![pending, draft]] {
        let unbacked = verify_workspace_claims(&[claim(ClaimType::MailSent)], &rows);
        assert_eq!(unbacked.len(), 1);
        assert_eq!(unbacked[0].kind, UnbackedKind::DraftOnly);
    }
    let unbacked = verify_workspace_claims(&[claim(ClaimType::MailSent)], &[]);
    assert_eq!(unbacked[0].kind, UnbackedKind::NoRecord);
}

#[test]
fn mail_backed_only_by_sent_true_row() {
    let now = chrono::Utc::now();
    let mut sent = row(now, "mail_send", 1);
    sent["state"] = json!("sent");
    sent["sent"] = json!(true);
    assert!(verify_workspace_claims(&[claim(ClaimType::MailSent)], &[sent.clone()]).is_empty());
    // `sent` must be the boolean true, on a successful row.
    let mut text_true = sent.clone();
    text_true["sent"] = json!("true");
    let mut failed = sent;
    failed["success"] = json!(false);
    for r in [text_true, failed] {
        assert_eq!(verify_workspace_claims(&[claim(ClaimType::MailSent)], &[r]).len(), 1);
    }
}

#[test]
fn non_mail_row_with_sent_true_does_not_back_mail_claim() {
    let now = chrono::Utc::now();
    for tool in ["calendar_create_event", "sheets_append", "send_to_agent", "mail_read", "gmail_read", "web_fetch"] {
        let mut r = row(now, tool, 1);
        r["sent"] = json!(true);
        r["state"] = json!("sent");
        let unbacked = verify_workspace_claims(&[claim(ClaimType::MailSent)], &[r]);
        assert_eq!(unbacked.len(), 1, "{tool} with sent=true must not back a mail claim");
        assert_eq!(unbacked[0].kind, UnbackedKind::NoRecord, "{tool}");
    }
    // Every tool of the evidence set does, with `sent == true`.
    for tool in MAIL_SENT_EVIDENCE_TOOLS {
        let mut r = row(now, tool, 1);
        r["sent"] = json!(true);
        assert!(verify_workspace_claims(&[claim(ClaimType::MailSent)], &[r]).is_empty(), "{tool}");
    }
}

// ── Session-scoped reader ────────────────────────────────

#[test]
fn row_from_another_session_is_not_evidence() {
    let home = fresh_home();
    let now = chrono::Utc::now();
    let mut other = row(now, "calendar_create_event", 1);
    other["session_id"] = json!("line:U2");
    write_rows(&home.join("tool_calls.jsonl"), &[other]);
    let unbacked = detect(&home, "已建立行程。", now);
    assert_eq!(unbacked.len(), 1);
    assert_eq!(unbacked[0].kind, UnbackedKind::NoRecord);
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn row_from_another_employee_is_not_evidence() {
    let home = fresh_home();
    let now = chrono::Utc::now();
    let mut other = row(now, "calendar_create_event", 1);
    other["agent_id"] = json!("someone-else");
    write_rows(&home.join("tool_calls.jsonl"), &[other]);
    assert_eq!(detect(&home, "已建立行程。", now).len(), 1);
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn rows_without_session_id_never_count() {
    let home = fresh_home();
    let now = chrono::Utc::now();
    let mut bare = row(now, "calendar_create_event", 1);
    bare.as_object_mut().unwrap().remove("session_id");
    bare["turn_id"] = json!(TURN);
    write_rows(&home.join("tool_calls.jsonl"), &[bare]);
    assert_eq!(detect(&home, "已建立行程。", now).len(), 1);
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn window_29_minutes_counts_31_minutes_does_not() {
    let now = chrono::Utc::now();
    let home = fresh_home();
    write_rows(&home.join("tool_calls.jsonl"), &[row(now, "sheets_append", 29)]);
    assert!(detect(&home, "已寫入試算表。", now).is_empty(), "29 minutes old backs the claim");
    write_rows(&home.join("tool_calls.jsonl"), &[row(now, "sheets_append", 31)]);
    assert_eq!(detect(&home, "已寫入試算表。", now).len(), 1, "31 minutes old does not");
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn current_turn_row_counts_beyond_window() {
    let now = chrono::Utc::now();
    let home = fresh_home();
    let mut long_turn = row(now, "docs_append", 45);
    long_turn["turn_id"] = json!(TURN);
    write_rows(&home.join("tool_calls.jsonl"), &[long_turn]);
    assert!(detect(&home, "已寫入文件。", now).is_empty());
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn no_session_id_means_not_evaluated() {
    let now = chrono::Utc::now();
    let home = fresh_home();
    // No rows at all: with a session this is NoRecord, without one nothing.
    assert_eq!(detect(&home, "已寄出。", now).len(), 1);
    for session in [None, Some("")] {
        assert!(detect_unbacked_workspace_claims(&home, AGENT, session, Some(TURN), "已寄出。", now).unwrap().is_empty());
    }
    assert!(detect_unbacked_workspace_claims(&home, "", Some(SESSION), Some(TURN), "已寄出。", now).unwrap().is_empty());
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn held_log_lock_is_an_error_not_an_unbacked_claim() {
    let now = chrono::Utc::now();
    let home = fresh_home();
    let path = home.join("tool_calls.jsonl");
    write_rows(&path, &[row(now, "sheets_append", 1)]);
    let held = duduclaw_core::fs_lock::lock_exclusive(&path).unwrap();
    let claims = extract_workspace_claims("已寫入試算表。");
    let started = std::time::Instant::now();
    let result = verify_claims_in_session(
        &home,
        AGENT,
        Some(SESSION),
        Some(TURN),
        &claims,
        now,
        std::time::Duration::from_millis(100),
    );
    let err = result.expect_err("a held lock must be an error, never 'no rows'");
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    drop(held);
    let backed = verify_claims_in_session(
        &home,
        AGENT,
        Some(SESSION),
        Some(TURN),
        &claims,
        now,
        std::time::Duration::from_millis(100),
    );
    assert!(backed.unwrap().is_empty(), "once the lock is free the row backs the claim");
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn rotated_backup_is_read_when_live_file_is_small() {
    let now = chrono::Utc::now();
    let home = fresh_home();
    write_rows(&home.join("tool_calls.jsonl.old"), &[row(now, "tasks_create", 2)]);
    write_rows(&home.join("tool_calls.jsonl"), &[row(now, "sheets_read", 1)]);
    assert!(detect(&home, "已建立任務。", now).is_empty());
    std::fs::remove_dir_all(home).unwrap();
}
