use super::*;
use duduclaw_tui::{decide::{self, Decision, GoalAction, Outcome}, strings,
    ui::{app::{Action, App, Selection}, card::{Card, Codes}}, state::{Approval, InboxItem}, sanitize::TerminalText};
use crossterm::event::{KeyEvent, KeyCode, KeyModifiers};

async fn approval_wire(digest: Value) {
    let Some(gateway) = Gateway::start(false).await else { return; };
    let client = gateway.client().await; connected(&client,1).await;
    let mut feed = StateFeed::start(client.clone(),Duration::from_secs(3600));
    model_until(&mut feed.receiver,|m|m.inbox.count().is_some()).await;
    let mut model = feed.receiver.borrow().clone();
    model.role = Some(duduclaw_tui::auth::Role::Manager);
    let mut value = json!({"id":"a1","agent_id":"adu","kind":"tool","summary":"approve",
        "request_kind":"approval","status":"pending","may_decide":true,"expires_at":9999999999_i64,
        "decided_in_dashboard_only":false,"effect_class":"modify","payload":{}});
    if digest != "absent" { value["digest"]=digest; }
    let mut a: Approval=serde_json::from_value(value.clone()).unwrap(); a.detail=TerminalText::new(value.to_string());
    let now=std::time::Instant::now(); let item=InboxItem::Approval(a);
    model.inbox.items=vec![item.clone()];
    let mut app=App::default(); app.selected=Some(Selection::Inbox(item.key()));
    app.card=Some(Card::open(item,&model,now,&mut Codes::default()));
    let code=app.card.as_ref().unwrap().code.clone();
    for c in code.chars() { app.key_at(KeyEvent::new(KeyCode::Char(c),KeyModifiers::NONE),&model,now+Duration::from_secs(1),1000); }
    let Action::Decide(request)=app.key_at(KeyEvent::new(KeyCode::Enter,KeyModifiers::NONE),&model,now+Duration::from_secs(1),1000) else { panic!("expected decision"); };
    assert!(app.card.is_none());
    assert!(matches!(decide::send(&client,&request).await,Outcome::Refetch));
    let calls=gateway.fixture.decisions.lock().unwrap().clone();
    assert_eq!(calls.len(),1); assert_eq!(calls[0].0,"approvals.decide");
    let mut expected=json!({"id":"a1","approve":true});
    if let Some(d)=value.get("digest").and_then(Value::as_str) { expected["expected_digest"]=json!(d); }
    assert_eq!(calls[0].1,expected);
    feed.refetch(); model_until(&mut feed.receiver,|m|m.local_refresh>0).await;
    app.refresh(&feed.receiver.borrow()); assert!(app.waiting_refresh.is_none());
    // The fixture keeps the card pending: never manufacture an optimistic removal.
    assert_eq!(feed.receiver.borrow().inbox.count(),Some(2));
}
#[tokio::test] async fn approve_with_digest() { approval_wire(json!("shown-digest")).await; }
#[tokio::test] async fn approve_digest_omitted_null() { approval_wire(Value::Null).await; }
#[tokio::test] async fn approve_digest_omitted_absent() { approval_wire(json!("absent")).await; }
#[tokio::test]
async fn digest_mismatch_closes_refetches_and_uses_fixed_message() {
    let Some(gateway)=Gateway::start(false).await else{return;};
    gateway.fixture.digest_mismatch.store(true,Ordering::SeqCst);
    let client=gateway.client().await; connected(&client,1).await;
    let mut feed=StateFeed::start(client.clone(),Duration::from_secs(3600));
    model_until(&mut feed.receiver,|m|m.inbox.count().is_some()).await;
    let mut app=App::default(); app.waiting_refresh=Some(0);
    app.card=Some(Card::open(feed.receiver.borrow().inbox.items[0].clone(),&feed.receiver.borrow(),std::time::Instant::now(),&mut Codes::default()));
    let out=decide::send(&client,&Decision::Approval{id:"a1".into(),approve:true,digest:Some("old".into())}).await;
    assert!(matches!(out,Outcome::Changed)); assert_eq!(out.notice(),strings::CARD_CHANGED);
    // Same completion path as the screen: close and refetch even on rejection.
    app.decision_finished(out);
    assert!(app.card.is_none()); assert_eq!(app.notice.as_deref(),Some(strings::CARD_CHANGED));
    feed.refetch();
    model_until(&mut feed.receiver,|m|m.local_refresh>0).await;
    app.refresh(&feed.receiver.borrow()); assert!(app.card.is_none()); assert!(app.waiting_refresh.is_none());
    assert_eq!(gateway.fixture.decisions.lock().unwrap().len(),1);
}
#[tokio::test]
async fn goal_decide_real_params_and_response() {
    let Some(gateway)=Gateway::start(false).await else{return;};
    let client=gateway.client().await; connected(&client,1).await;
    for action in [GoalAction::Retry,GoalAction::Abort,GoalAction::Done,GoalAction::Takeover,GoalAction::Continue] {
        let request=Decision::Goal{task_id:"t1".into(),action,message:"下一項".into()};
        assert!(matches!(decide::send(&client,&request).await,Outcome::Refetch));
        let mut expected=json!({"task_id":"t1","action":action.wire()});
        if action==GoalAction::Continue {expected["message"]=json!("下一項");}
        assert_eq!(gateway.fixture.decisions.lock().unwrap().last().unwrap(),&("tasks.goal_decide".into(),expected));
    }
}
#[tokio::test]
async fn rpc_error_preserves_text_only_through_terminal_text() {
    let Some(gateway)=Gateway::start(false).await else{return;};
    let client=gateway.client().await; connected(&client,1).await;
    let reply=client.call_detailed("reject",json!({}),Duration::from_secs(2)).await.unwrap();
    let duduclaw_tui::client::RpcReply::Rejected{message,..}=reply else {panic!("expected rejection");};
    assert_eq!(message.visible(),"untrusted-secret␛");
}
#[tokio::test]
async fn employee_operator_keeps_goal_inbox_without_manager_approval_rpc() {
    let Some(gateway)=Gateway::start(false).await else{return;};
    gateway.fixture.employee_role.store(true,Ordering::SeqCst);
    let client=gateway.client().await; connected(&client,1).await;
    let mut feed=StateFeed::start(client,Duration::from_secs(3600));
    model_until(&mut feed.receiver,|m|m.inbox.count().is_some()).await;
    assert_eq!(feed.receiver.borrow().inbox.count(),Some(1));
    assert!(feed.receiver.borrow().goal_operators.contains("adu"));
    assert_eq!(gateway.fixture.list_calls.load(Ordering::SeqCst),0);
}
