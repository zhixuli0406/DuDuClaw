use super::audit::*;
use serde_json::Value;

fn record() -> DriveWriteAudit {
    DriveWriteAudit {
        call_id: "call-1".into(), google_turn_id: "registered-turn-1".into(), id_source: "turn",
        run_id: Some("run-1".into()), agent_id: "bot".into(), identity_kind: "personal".into(),
        account_email: "holder@c.test".into(), requester: AuditRequester::from_identity(&duduclaw_knowledge::requester::RequesterIdentity::DashboardUser { user_id: "holder".into() }), action: "create".into(),
        tool: "drive_create".into(), file_id: Some("file-1".into()), file_name: Some("檔😀".repeat(60)),
        parents_before: vec![], parents_after: vec!["folder-1".into()],
        destination_folder_id: Some("folder-1".into()), folder_rule: Some("global".into()),
        approval: Approval { mode: "member".into(), approval_ids: vec!["approval-1".into()], decided_by: vec!["holder".into()] },
        bytes: Some(100), content_sha256: Some("a".repeat(64)), outcome: Outcome::Confirmed,
        reason: None, post_check: PostCheck::Ok,
    }
}

#[test]
fn audit_has_all_fields_preserves_google_turn_and_omits_content_and_tokens() {
    let home = tempfile::tempdir().unwrap();
    let record = record();
    append(home.path(), &record);
    let raw = std::fs::read_to_string(home.path().join("tool_calls.jsonl")).unwrap();
    assert_eq!(raw.lines().count(), 1);
    let row: Value = serde_json::from_str(raw.lines().next().unwrap()).unwrap();
    assert_eq!(row["requester"], serde_json::json!({"kind":"dashboard_user","user_id":"holder","channel":null}));
    assert_eq!(row["tool_name"], "google_drive_write");
    assert_eq!(row["agent_id"], "bot");
    assert_eq!(row["params_summary"], "drive_create");
    assert_eq!(row["google_turn_id"], "registered-turn-1");
    assert_eq!(row["id_source"], "turn");
    assert_eq!(row["success"], true);
    assert_eq!(row["file_name"], duduclaw_core::truncate_chars(record.file_name.as_deref().unwrap(), 80));
    assert_eq!(row["file_name"].as_str().unwrap().chars().count(), 80);
    for field in ["call_id", "google_turn_id", "id_source", "run_id", "agent_id", "identity_kind", "account_email",
        "requester", "action", "tool", "file_id", "file_name", "parents_before", "parents_after",
        "destination_folder_id", "folder_rule", "approval", "bytes", "content_sha256", "outcome", "reason", "post_check"] {
        assert!(row.get(field).is_some(), "missing {field}");
    }
    for field in ["mode", "approval_ids", "decided_by"] { assert!(row["approval"].get(field).is_some()); }
    for field in ["content", "text", "arguments", "token", "access_token", "refresh_token", "bearer"] {
        assert!(row.get(field).is_none(), "unexpected {field}");
    }
    assert_eq!(row["outcome"], "confirmed");
    assert_eq!(row["post_check"], "ok");
}

#[test]
fn audit_outcomes_postchecks_and_reason_are_closed() {
    assert!(Reason::from_code("provider error with secret material").is_none());
    let home = tempfile::tempdir().unwrap();
    let mut record = record();
    for (outcome, code, post_check, expected_post) in [
        (Outcome::NotSent, "not_sent", PostCheck::None, "none"),
        (Outcome::MayHaveSent, "may_have_sent", PostCheck::Incomplete, "incomplete"),
        (Outcome::Confirmed, "confirmed", PostCheck::Violation, "violation"),
        (Outcome::Refused, "refused", PostCheck::None, "none"),
    ] {
        record.outcome = outcome;
        record.reason = Some(Reason::from_code("changed").unwrap());
        record.post_check = post_check;
        append(home.path(), &record);
        let raw = std::fs::read_to_string(home.path().join("tool_calls.jsonl")).unwrap();
        let row: Value = serde_json::from_str(raw.lines().last().unwrap()).unwrap();
        assert_eq!(row["outcome"], code);
        assert_eq!(row["post_check"], expected_post);
        assert_eq!(row["reason"], "changed");
        assert_eq!(row["success"], false);
    }
}

#[test]
fn notice_limiter_is_per_key_and_expires_at_its_ttl() {
    use std::{collections::HashMap, sync::Mutex, time::{Duration, Instant}};
    let ttl = Duration::from_secs(24 * 3600);
    let entries = Mutex::new(HashMap::new());
    let claim = |key| super::credential::claim_notice(&entries, key, ttl);
    assert!(claim(("bot", "file-a")));
    assert!(!claim(("bot", "file-a")));
    assert!(claim(("bot", "file-b")));
    assert!(claim(("other", "file-a")));
    entries.lock().unwrap().insert(("bot", "file-a"), Instant::now() - ttl);
    assert!(claim(("bot", "file-a")));
}

#[test]
fn audit_requester_uses_only_registered_identity_fields() {
    use duduclaw_knowledge::requester::RequesterIdentity;
    use serde_json::json;
    for (identity, expected) in [
        (RequesterIdentity::DashboardUser {user_id:"holder".into()}, json!({"kind":"dashboard_user","user_id":"holder","channel":null})),
        (RequesterIdentity::ChannelUser {user_id:Some("holder".into()),channel:"line".into(),chat:"private-chat".into(),sender:"private-name".into(),attested_private:true}, json!({"kind":"channel_user","user_id":"holder","channel":"line"})),
        (RequesterIdentity::ChannelUser {user_id:None,channel:"line".into(),chat:"private-chat".into(),sender:"private-name".into(),attested_private:false}, json!({"kind":"channel_unbound","user_id":null,"channel":"line"})),
        (RequesterIdentity::ChannelUser {user_id:Some("visitor-name".into()),channel:"widget".into(),chat:"public-chat".into(),sender:"visitor-name".into(),attested_private:false}, json!({"kind":"channel_unbound","user_id":null,"channel":"widget"})),
        (RequesterIdentity::System {kind:"cron".into(),on_behalf_of:Some("holder".into())}, json!({"kind":"system","user_id":"holder","channel":null})),
        (RequesterIdentity::System {kind:"cron".into(),on_behalf_of:None}, json!({"kind":"system","user_id":null,"channel":null})),
        (RequesterIdentity::Unknown, json!({"kind":"unknown","user_id":null,"channel":null})),
    ] {
        let mut record = record();
        record.requester = AuditRequester::from_identity(&identity);
        let home = tempfile::tempdir().unwrap();
        append(home.path(), &record);
        let raw = std::fs::read_to_string(home.path().join("tool_calls.jsonl")).unwrap();
        let row: Value = serde_json::from_str(raw.lines().next().unwrap()).unwrap();
        assert_eq!(row["requester"], expected);
        for private in ["private-chat", "private-name", "visitor-name", "public-chat"] {
            assert!(!row["requester"].to_string().contains(private));
        }
    }
}
