use std::sync::Arc;

use chrono::Utc;
use rusqlite::params;

use super::*;
use crate::approval::{ApprovalBroker, ApprovalStore, EXPECTED_DECISION_DIGEST, decision_digest};
use crate::approval::chat_admin::{DecisionActor, receipts::{self, Receipt, TerminalStatus}};

const USER: &str = "3f2afc12-4f13-4a28-8b7f-df31eadbb013";

fn record(kind: &str) -> ApprovalRecord {
    serde_json::from_value(serde_json::json!({
        "id": Uuid::new_v4().to_string(), "agent_id": "test",
        "action_kind": kind, "summary": "test", "payload": {},
        "status": "pending", "created_at": Utc::now().to_rfc3339(),
        "decided_at": null, "decided_by": null, "ttl_seconds": 3600
    })).unwrap()
}

fn entry(rec: &ApprovalRecord) -> LedgerEntry {
    LedgerEntry {
        user_id: USER.parse().unwrap(), channel: ChatChannel::Telegram,
        step: Step::Tap, step_required: false, card_id: Uuid::new_v4().to_string(),
        digest: decision_digest(rec),
    }
}

fn receipt(rec: &ApprovalRecord, entry: &LedgerEntry) -> Receipt {
    Receipt {
        card_id: entry.card_id.clone(), approval_id: rec.id.to_string(), purpose: "rm143".into(),
        user_id: entry.user_id.to_string(), channel: entry.channel.as_str().into(),
        account: "bot-1".into(), principal: "12345".into(), dm: "12345".into(),
        message_id: Some(Uuid::new_v4().to_string()), digest: entry.digest.clone(),
        tier: "T2".into(), valid_until: (Utc::now() + chrono::Duration::minutes(30)).to_rfc3339(),
        status: "live".into(), sent_at: Utc::now().to_rfc3339(),
    }
}

async fn fixture() -> (Arc<ApprovalStore>, ApprovalRecord, LedgerEntry, Receipt) {
    let store = Arc::new(ApprovalStore::open_in_memory().unwrap());
    let rec = record("expert_skills_enable");
    let entry = entry(&rec);
    let receipt = receipt(&rec, &entry);
    store.insert(&rec).await.unwrap();
    receipts::insert(&store, &receipt).await.unwrap();
    (store, rec, entry, receipt)
}

async fn fresh_receipt(store: &ApprovalStore, card: &Receipt) -> Receipt {
    receipts::lookup_message(store, &card.channel, &card.account, &card.dm,
        card.message_id.as_deref().unwrap()).await.unwrap().unwrap()
}

#[test]
fn decided_by_parse_table() {
    for channel in [ChatChannel::Telegram, ChatChannel::Googlechat, ChatChannel::Line,
        ChatChannel::Slack, ChatChannel::Discord] {
        for step in [Step::Tap, Step::Web, Step::Totp] {
            let by = DecidedBy::new(USER.parse().unwrap(), channel, step);
            let text = format!("chat-admin:{USER}:{}:{}", channel.as_str(), step.as_str());
            assert_eq!(by.to_string(), text);
            assert_eq!(text.parse::<DecidedBy>().unwrap(), by);
        }
    }
    for text in [
        "chat-admin:system:telegram:tap".to_string(),
        format!("chat-admin:{USER}:telegram:tap:extra"),
        format!("chat-admin:{USER}:unknown:tap"),
        format!("chat-admin:{USER}:Telegram:tap"),
        format!("chat-admin:{USER}:telegram:unknown"),
        format!("dashboard:{USER}:telegram:tap"),
        format!("chat-admin-extra:{USER}:telegram:tap"),
        "chat-admin::telegram:tap".to_string(),
        format!("chat-admin:{USER}::tap"),
        format!("chat-admin:{USER}:telegram:"),
        format!(":{USER}:telegram:tap"),
        format!("chat-admin:{USER}:telegram"),
        String::new(),
    ] {
        assert!(text.parse::<DecidedBy>().is_err(), "{text}");
    }
}

#[tokio::test]
async fn ledger_status_and_receipt_commit_together() {
    for (approve, step, step_required) in [(true, Step::Tap, false),
        (true, Step::Web, true), (true, Step::Totp, true), (false, Step::Tap, false)] {
        let (store, rec, mut entry, card) = fixture().await;
        entry.step = step;
        entry.step_required = step_required;
        let broker = ApprovalBroker::new(store.clone());
        broker.decide_as(&rec.id, approve, &DecisionActor::ChatAdmin(entry.clone())).await.unwrap();
        let decided = store.get(&rec.id).await.unwrap().unwrap();
        assert_eq!(decided.status, if approve { ApprovalStatus::Approved } else { ApprovalStatus::Denied });
        assert_eq!(decided.decided_by, Some(entry.decided_by().to_string()));
        assert_eq!(fresh_receipt(&store, &card).await.status, "consumed");
        assert!(receipts::list_live(&store, &rec.id).await.unwrap().is_empty());
        let conn = store.conn.lock().await;
        assert!(accepts(&conn, &decided));
        let evidence: (String, String, String) = conn.query_row(
            "SELECT card_id, digest, decided_at FROM chat_admin_decisions WHERE approval_id = ?1",
            [rec.id.as_str()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).unwrap();
        assert_eq!(evidence, (entry.card_id, entry.digest, decided.decided_at.unwrap()));
    }
}

#[tokio::test]
async fn ledger_insert_failure_rolls_back_status_and_receipt() {
    let (store, rec, entry, card) = fixture().await;
    {
        let conn = store.conn.lock().await;
        conn.execute("INSERT INTO chat_admin_decisions
            (approval_id, card_id, user_id, channel, step, step_required, digest, approve, decided_at)
            VALUES (?1, 'existing', ?2, 'telegram', 'tap', 0, 'existing', 0, ?3)",
            params![rec.id.as_str(), USER, Utc::now().to_rfc3339()]).unwrap();
    }
    let broker = ApprovalBroker::new(store.clone());
    let error = broker.decide_as(&rec.id, true, &DecisionActor::ChatAdmin(entry)).await.unwrap_err();
    assert!(error.contains("insert chat-admin decision"), "{error}");
    let unchanged = store.get(&rec.id).await.unwrap().unwrap();
    assert_eq!(unchanged.status, ApprovalStatus::Pending);
    assert_eq!(unchanged.decided_by, None);
    assert_eq!(unchanged.decided_at, None);
    assert_eq!(fresh_receipt(&store, &card).await.status, "live");
    let conn = store.conn.lock().await;
    let card_id: String = conn.query_row("SELECT card_id FROM chat_admin_decisions WHERE approval_id = ?1",
        [rec.id.as_str()], |row| row.get(0)).unwrap();
    assert_eq!(card_id, "existing");
}

#[tokio::test]
async fn double_press_executes_once() {
    let (store, rec, entry, card) = fixture().await;
    let now = Utc::now().to_rfc3339();
    assert_eq!(store.decide_if_pending_with_ledger(&rec.id, true, &entry, &now).await.unwrap(), 1);
    assert_eq!(store.decide_if_pending_with_ledger(&rec.id, false, &entry, &now).await.unwrap(), 0);
    let broker = ApprovalBroker::new(store.clone());
    assert!(broker.decide_as(&rec.id, true, &DecisionActor::ChatAdmin(entry)).await.is_err());
    assert_eq!(store.get(&rec.id).await.unwrap().unwrap().status, ApprovalStatus::Approved);
    assert_eq!(fresh_receipt(&store, &card).await.status, "consumed");
    let conn = store.conn.lock().await;
    let count: i64 = conn.query_row("SELECT count(*) FROM chat_admin_decisions", [], |row| row.get(0)).unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn decided_by_without_ledger_refused() {
    let (store, rec, entry, card) = fixture().await;
    let broker = ApprovalBroker::new(store.clone());
    broker.decide(&rec.id, true, &entry.decided_by().to_string()).await.unwrap();
    let decided = store.get(&rec.id).await.unwrap().unwrap();
    assert_eq!(fresh_receipt(&store, &card).await.status, "live");
    let conn = store.conn.lock().await;
    assert!(!accepts(&conn, &decided));
}

#[tokio::test]
async fn t3_record_refused_by_decide_as() {
    let (store, rec, entry, card) = fixture().await;
    // Change the persisted row after delivery: the transaction must classify it again.
    {
        let conn = store.conn.lock().await;
        conn.execute("UPDATE approvals SET action_kind = 'unknown' WHERE id = ?1", [rec.id.as_str()]).unwrap();
    }
    let broker = ApprovalBroker::new(store.clone());
    let error = broker.decide_as(&rec.id, true, &DecisionActor::ChatAdmin(entry.clone())).await.unwrap_err();
    assert!(error.contains("T3"), "{error}");
    let error = store.decide_if_pending_with_ledger(&rec.id, true, &entry, &Utc::now().to_rfc3339())
        .await.unwrap_err();
    assert!(error.contains("T3"), "{error}");
    assert_eq!(store.get(&rec.id).await.unwrap().unwrap().status, ApprovalStatus::Pending);
    assert_eq!(fresh_receipt(&store, &card).await.status, "live");
}

#[tokio::test]
async fn accepts_requires_exact_evidence_and_decided_status() {
    let (store, rec, entry, _) = fixture().await;
    ApprovalBroker::new(store.clone()).decide_as(&rec.id, true, &DecisionActor::ChatAdmin(entry))
        .await.unwrap();
    let decided = store.get(&rec.id).await.unwrap().unwrap();
    let conn = store.conn.lock().await;
    for status in [ApprovalStatus::Pending, ApprovalStatus::Denied, ApprovalStatus::Expired,
        ApprovalStatus::Answered, ApprovalStatus::Invalidated] {
        let mut changed = decided.clone();
        changed.status = status;
        assert!(!accepts(&conn, &changed));
    }
    for value in [format!("chat-admin:{}:telegram:tap", Uuid::new_v4()),
        format!("chat-admin:{}:telegram:tap", USER.to_uppercase()),
        format!("chat-admin:{USER}:slack:tap"), format!("chat-admin:{USER}:telegram:web")] {
        let mut changed = decided.clone();
        changed.decided_by = Some(value);
        assert!(!accepts(&conn, &changed));
    }
    let mut changed = decided.clone();
    changed.id = ApprovalId::new();
    assert!(!accepts(&conn, &changed));
    changed = decided.clone();
    changed.action_kind = "unknown".into();
    assert!(!accepts(&conn, &changed));
    conn.execute("UPDATE chat_admin_decisions SET step_required = 1", []).unwrap();
    assert!(!accepts(&conn, &decided), "required step-up cannot be satisfied by tap");
    conn.execute("DROP TABLE chat_admin_decisions", []).unwrap();
    assert!(!accepts(&conn, &decided), "database errors fail closed");
}

#[tokio::test]
async fn required_tap_and_digest_drift_leave_pending() {
    let (store, rec, mut entry, card) = fixture().await;
    let broker = ApprovalBroker::new(store.clone());
    entry.step_required = true;
    assert!(broker.decide_as(&rec.id, true, &DecisionActor::ChatAdmin(entry.clone())).await.is_err());
    entry.step_required = false;
    let error = EXPECTED_DECISION_DIGEST.scope(Some((rec.id.to_string(), "wrong".into())),
        broker.decide_as(&rec.id, true, &DecisionActor::ChatAdmin(entry.clone())))
        .await.unwrap_err();
    assert!(error.contains("digest mismatch"), "{error}");
    entry.digest = "wrong".into();
    assert!(broker.decide_as(&rec.id, true, &DecisionActor::ChatAdmin(entry)).await.is_err());
    assert_eq!(store.get(&rec.id).await.unwrap().unwrap().status, ApprovalStatus::Pending);
    assert_eq!(fresh_receipt(&store, &card).await.status, "live");
}

#[tokio::test]
async fn unavailable_receipt_rolls_back_decision() {
    let (store, rec, entry, card) = fixture().await;
    receipts::mark(&store, &card.card_id, TerminalStatus::Retired).await.unwrap();
    let error = ApprovalBroker::new(store.clone()).decide_as(&rec.id, true, &DecisionActor::ChatAdmin(entry))
        .await.unwrap_err();
    assert!(error.contains("receipt"), "{error}");
    assert_eq!(store.get(&rec.id).await.unwrap().unwrap().status, ApprovalStatus::Pending);
    let conn = store.conn.lock().await;
    let count: i64 = conn.query_row("SELECT count(*) FROM chat_admin_decisions", [], |row| row.get(0)).unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn receipt_unique_exact_lookups_and_terminal_transitions() {
    let (store, rec, entry, card) = fixture().await;
    let mut duplicate = card.clone();
    duplicate.card_id = Uuid::new_v4().to_string();
    assert!(receipts::insert(&store, &duplicate).await.is_err());
    assert_eq!(fresh_receipt(&store, &card).await, card);
    assert!(receipts::lookup_message(&store, "telegram-extra", &card.account, &card.dm,
        card.message_id.as_deref().unwrap()).await.unwrap().is_none());
    for status in [TerminalStatus::Consumed, TerminalStatus::Retired, TerminalStatus::Expired] {
        let mut line = receipt(&rec, &entry);
        line.card_id = Uuid::new_v4().to_string();
        line.channel = "line".into();
        line.message_id = None;
        receipts::insert(&store, &line).await.unwrap();
        let matches = receipts::lookup_without_message_id(&store, &rec.id, USER, &line.dm).await.unwrap();
        assert!(matches.contains(&line));
        assert!(receipts::lookup_without_message_id(&store, &ApprovalId::new(), USER, &line.dm)
            .await.unwrap().is_empty());
        assert!(receipts::mark(&store, &line.card_id, status).await.unwrap());
        assert!(!receipts::mark(&store, &line.card_id, TerminalStatus::Retired).await.unwrap());
        assert!(!receipts::list_live(&store, &rec.id).await.unwrap().iter()
            .any(|receipt| receipt.card_id == line.card_id));
    }
    assert_eq!(receipts::list_live(&store, &rec.id).await.unwrap(), vec![card]);
}

#[tokio::test]
async fn dashboard_actor_preserves_existing_path() {
    let (store, rec, _, card) = fixture().await;
    ApprovalBroker::new(store.clone()).decide_as(&rec.id, true,
        &DecisionActor::Dashboard { user_id: USER.into() }).await.unwrap();
    let decided = store.get(&rec.id).await.unwrap().unwrap();
    assert_eq!(decided.decided_by, Some(format!("dashboard:{USER}")));
    assert_eq!(fresh_receipt(&store, &card).await.status, "live");
    let conn = store.conn.lock().await;
    let count: i64 = conn.query_row("SELECT count(*) FROM chat_admin_decisions", [], |row| row.get(0)).unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn chat_admin_egress_requires_the_new_actor_path() {
    let store = Arc::new(ApprovalStore::open_in_memory().unwrap());
    let mut rec = record("ordinary");
    rec.payload = serde_json::json!({"egress_guard": {}});
    let entry = entry(&rec);
    store.insert(&rec).await.unwrap();
    receipts::insert(&store, &receipt(&rec, &entry)).await.unwrap();
    let broker = ApprovalBroker::new(store.clone());
    assert!(broker.decide(&rec.id, true, &entry.decided_by().to_string()).await.is_err());
    assert!(broker.decide_as(&rec.id, true, &DecisionActor::Dashboard { user_id: USER.into() })
        .await.is_err());
    broker.decide_as(&rec.id, true, &DecisionActor::ChatAdmin(entry)).await.unwrap();
    let decided = store.get(&rec.id).await.unwrap().unwrap();
    let conn = store.conn.lock().await;
    assert!(accepts(&conn, &decided));
}

#[test]
fn readonly_acceptance_missing_database_fails_closed_without_creating_it() {
    let home = tempfile::tempdir().unwrap();
    assert!(!accepts_readonly(home.path(), &record("expert_skills_enable")));
    assert!(!home.path().join("approvals.db").exists());
}
