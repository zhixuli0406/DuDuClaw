//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

/// WP-7G (batch 2 tail work): three small backend gaps left over from batch
/// 1 — `task_row_to_json` never surfaced the `archived`/`pinned` columns,
/// `tools.catalog` never got a `search.query` entry, and `mail.decide` had
/// no way to record the decider's own reason.
use super::*;

fn frame_ok(f: &WsFrame) -> bool {
    matches!(f, WsFrame::Response { ok: true, .. })
}

fn frame_data(f: &WsFrame) -> Value {
    match f {
        WsFrame::Response {
            payload: Some(p), ..
        } => p.clone(),
        other => panic!("expected a payload: {other:?}"),
    }
}

// ── #1: task_row_to_json must surface archived/pinned ──────────────
#[test]
fn task_row_to_json_surfaces_archived_and_pinned() {
    let mut row = TaskRow::new(
        "t-wp7g".into(),
        "title".into(),
        "desc".into(),
        "medium".into(),
        "agent-a".into(),
        "agent-a".into(),
    );
    let v = task_row_to_json(&row);
    assert_eq!(
        v["archived"],
        json!(false),
        "default archived must reach the JSON: {v}"
    );
    assert_eq!(
        v["pinned"],
        json!(false),
        "default pinned must reach the JSON: {v}"
    );

    row.archived = true;
    row.pinned = true;
    let v = task_row_to_json(&row);
    assert_eq!(
        v["archived"],
        json!(true),
        "archived=true must reach the dashboard: {v}"
    );
    assert_eq!(
        v["pinned"],
        json!(true),
        "pinned=true must reach the dashboard: {v}"
    );
}

// task_row_to_json must surface the canonical task kind: the dashboard locks
// discovery rows read-only and keeps them off the goal board from it.
#[test]
fn task_row_to_json_surfaces_canonical_kind() {
    let mut row = TaskRow::new(
        "t-kind".into(),
        "title".into(),
        "desc".into(),
        "medium".into(),
        "agent-a".into(),
        "agent-a".into(),
    );
    assert_eq!(task_row_to_json(&row)["kind"], json!("task"));
    row.kind = crate::task_store::TaskKind::Goal;
    assert_eq!(task_row_to_json(&row)["kind"], json!("goal"));
    row.kind = crate::task_store::TaskKind::Discovery;
    assert_eq!(task_row_to_json(&row)["kind"], json!("discovery"));
    // Same spelling as the store's own serde form (one mapping, not two).
    assert_eq!(serde_json::to_value(row.kind).unwrap(), task_row_to_json(&row)["kind"]);
}

// ── #2: tools.catalog must list search.query (drift guard) ─────────
#[tokio::test]
async fn tools_catalog_lists_search_query() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler.handle_tools_catalog(json!({}));
    assert!(frame_ok(&frame), "{frame:?}");
    let data = frame_data(&frame);
    let tools = data["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(
        names.contains(&"search.query"),
        "search.query must be listed in tools.catalog: {names:?}"
    );
    // Catch a future copy-paste duplicate at the same time — cheap
    // insurance against the exact kind of drift this entry fixes.
    let mut sorted = names.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        names.len(),
        "tools.catalog must not list the same RPC name twice: {names:?}"
    );
}

// ── #3: mail.decide's optional `note` reaches the ledger ───────────
async fn drafted_mail(home: &std::path::Path) -> (crate::approval::ApprovalBroker, String) {
    let broker = crate::approval::ApprovalBroker::open(home).unwrap();
    let approval = broker
        .request(
            "sales",
            crate::mail::OUTBOUND_ACTION_KIND,
            "寄信",
            json!({}),
            3600,
        )
        .await
        .unwrap();
    let cfg = crate::mail::MailConfig::default();
    let mail_id = crate::mail::record_outbox_draft(
        home,
        &cfg,
        "sales",
        "client@example.com",
        "報價回覆",
        "附上報價單。",
        approval.as_str(),
        None,
    );
    (broker, mail_id)
}

#[tokio::test]
async fn mail_decide_reject_note_lands_in_the_settle_row() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = UserContext::admin_fallback();
    let (_broker, mail_id) = drafted_mail(home.path()).await;

    let frame = handler
        .handle(
            "mail.decide",
            json!({ "mail_id": mail_id, "approve": false, "note": "客戶尚未簽約，先不寄。" }),
            &ctx,
        )
        .await;
    assert!(frame_ok(&frame), "{frame:?}");
    assert_eq!(frame_data(&frame)["note"], "客戶尚未簽約，先不寄。");

    // The reject settles immediately (nothing left to transmit) and the
    // operator's own reason — not the fixed system copy — lands in the
    // ledger's terminal note.
    let item = &crate::mail::list_outbox(home.path(), None, None, 10)[0];
    assert_eq!(item.status, crate::mail::OutboxStatus::Rejected);
    assert_eq!(
        item.note.as_deref(),
        Some("客戶尚未簽約，先不寄。"),
        "the operator's reason must reach the settle row, not the system copy"
    );
    assert_eq!(
        item.decision_note.as_deref(),
        Some("客戶尚未簽約，先不寄。")
    );
}

#[tokio::test]
async fn mail_decide_approve_note_is_kept_separate_from_the_deferred_settle_note() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = UserContext::admin_fallback();
    let (_broker, mail_id) = drafted_mail(home.path()).await;

    let frame = handler
        .handle(
            "mail.decide",
            json!({ "mail_id": mail_id, "approve": true, "note": "已電話確認，可以寄。" }),
            &ctx,
        )
        .await;
    assert!(frame_ok(&frame), "{frame:?}");

    // Transmission is still the worker's job — the row must stay pending
    // (mirrors the pre-existing `mail_decide_reports_queued_not_sent_*`
    // invariant for the approve path).
    let item = &crate::mail::list_outbox(home.path(), None, None, 10)[0];
    assert_eq!(item.status, crate::mail::OutboxStatus::Pending);
    assert!(
        item.note.is_none(),
        "the settle note must not be invented before the worker actually sends"
    );
    assert_eq!(
        item.decision_note.as_deref(),
        Some("已電話確認，可以寄。"),
        "the approver's note must still be recorded on the ledger"
    );
}

#[tokio::test]
async fn mail_decide_without_a_note_keeps_the_fixed_system_copy() {
    // Backward-compat: an omitted `note` must not regress the pre-WP-7G
    // reject-path behavior of a system-authored reason.
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = UserContext::admin_fallback();
    let (_broker, mail_id) = drafted_mail(home.path()).await;

    let frame = handler
        .handle(
            "mail.decide",
            json!({ "mail_id": mail_id, "approve": false }),
            &ctx,
        )
        .await;
    assert!(frame_ok(&frame), "{frame:?}");
    assert!(frame_data(&frame)["note"].is_null());

    let item = &crate::mail::list_outbox(home.path(), None, None, 10)[0];
    assert_eq!(item.status, crate::mail::OutboxStatus::Rejected);
    assert_eq!(item.note.as_deref(), Some("已由人工拒絕，未寄出。"));
    assert!(
        item.decision_note.is_none(),
        "no note param ⇒ no decision_note row"
    );
}
