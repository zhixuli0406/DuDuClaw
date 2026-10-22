use super::*;
use crate::channel_ingress::AcceptedEvent;

fn text_event(text: &str) -> serde_json::Value {
    serde_json::json!({"type": "message", "replyToken": format!("rt-{text}"),
        "source": {"type": "user", "userId": "U1"},
        "message": {"type": "text", "id": "1", "text": text}})
}

fn accepted(id: &str, conversation: &str, revision: &str, text: &str) -> AcceptedEvent {
    AcceptedEvent {
        decision_fastlane: false,
        decision_binding: None,
        event_id: id.into(),
        account: "account".into(),
        revision: revision.into(),
        authorization_revision: "a1".into(),
        conversation: conversation.into(),
        payload: serde_json::json!({"destination": "D", "event": text_event(text)}).to_string(),
    }
}

fn plain(e: &serde_json::Value) -> bool {
    !e["message"]["text"].as_str().unwrap_or("").contains("STOP")
}

async fn store_with(events: &[AcceptedEvent]) -> (tempfile::TempDir, IngressStore) {
    let dir = tempfile::tempdir().unwrap();
    let s = IngressStore::open(dir.path()).unwrap();
    s.append(events, 10).await.unwrap();
    (dir, s)
}

async fn status_of(s: &IngressStore, event_id: &str) -> String {
    s.list()
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.event_id == event_id)
        .unwrap()
        .status
}

#[tokio::test]
async fn waiting_messages_fold_into_the_head_in_order_and_stop_at_the_first_misfit() {
    let (_d, s) = store_with(&[
        accepted("m1", "c", "r1", "第一則"),
        accepted("m2", "c", "r1", "第二則"),
        accepted("m3", "c", "r1", "第三則"),
        accepted("m4", "c", "r1", "STOP 指令"),
        accepted("m5", "c", "r1", "第五則"),
        accepted("other", "c2", "r1", "別的對話"),
    ])
    .await;
    let head = s.claim(10).await.unwrap().unwrap();
    assert_eq!(head.event_id, "m1");
    let merged = s.absorb_followers(&head, &plain).await.unwrap();
    let texts: Vec<_> = merged.iter().map(|m| m.event["message"]["text"].clone()).collect();
    assert_eq!(texts, vec!["第二則", "第三則"]);
    assert_eq!(status_of(&s, "m2").await, MERGED_STATUS);
    assert_eq!(status_of(&s, "m3").await, MERGED_STATUS);
    assert_eq!(status_of(&s, "m4").await, "ready", "a misfit stops folding");
    assert_eq!(status_of(&s, "m5").await, "ready", "nothing after the misfit is taken");
    assert_eq!(status_of(&s, "other").await, "ready", "other conversations untouched");
    // The head payload now lists them (a retry / rerun covers every message).
    let stored = s.list().await.unwrap().into_iter().find(|r| r.event_id == "m1").unwrap();
    let payload: serde_json::Value = serde_json::from_str(stored.payload.as_deref().unwrap()).unwrap();
    assert_eq!(merged_from_payload(&payload), merged);
    // Merged rows do not hold the conversation: once the head finishes the
    // next waiting message is claimable.
    s.transition(&head, "claimed", "dispatching", None).await.unwrap();
    s.transition(&head, "dispatching", "completed", None).await.unwrap();
    let next = s.claim(10).await.unwrap().unwrap();
    assert_eq!(next.event_id, "m4");
}

#[tokio::test]
async fn a_different_snapshot_or_a_rerun_is_never_folded() {
    let (_d, s) = store_with(&[
        accepted("m1", "c", "r1", "第一則"),
        accepted("m2", "c", "r2", "換了路由之後"),
    ])
    .await;
    let head = s.claim(10).await.unwrap().unwrap();
    assert!(s.absorb_followers(&head, &plain).await.unwrap().is_empty());
    assert_eq!(status_of(&s, "m2").await, "ready");

    let (_d2, s2) = store_with(&[
        accepted("m1", "c", "r1", "第一則"),
        accepted("m2", "c", "r1", "第二則"),
    ])
    .await;
    let mut head = s2.claim(10).await.unwrap().unwrap();
    head.run_authorization_id = Some("rerun".into());
    assert!(s2.absorb_followers(&head, &plain).await.unwrap().is_empty());
    assert_eq!(status_of(&s2, "m2").await, "ready");
}

#[tokio::test]
async fn folding_is_bounded_and_needs_the_live_lease() {
    let events: Vec<_> = (0..8)
        .map(|i| accepted(&format!("m{i}"), "c", "r1", &format!("訊息{i}")))
        .collect();
    let (_d, s) = store_with(&events).await;
    let head = s.claim(10).await.unwrap().unwrap();
    let mut stale = head.clone();
    stale.lease_id = Some("someone-else".into());
    assert!(s.absorb_followers(&stale, &plain).await.unwrap().is_empty());
    let merged = s.absorb_followers(&head, &plain).await.unwrap();
    assert_eq!(merged.len(), MAX_MERGED - 1);
    // A second call keeps the list and adds nothing past the bound.
    assert_eq!(s.absorb_followers(&head, &plain).await.unwrap(), merged);
    assert_eq!(status_of(&s, "m5").await, "ready");
}

#[tokio::test]
async fn a_one_shot_head_does_not_take_the_next_messages() {
    let (_d, s) = store_with(&[
        accepted("m1", "c", "r1", "交給業務的那則"),
        accepted("m2", "c", "r1", "之後的訊息"),
    ])
    .await;
    let head = s.claim(10).await.unwrap().unwrap();
    s.store_route_agent(&head.id, "sales", true).await.unwrap();
    assert!(s.absorb_followers(&head, &plain).await.unwrap().is_empty());
    assert_eq!(status_of(&s, "m2").await, "ready");
}
