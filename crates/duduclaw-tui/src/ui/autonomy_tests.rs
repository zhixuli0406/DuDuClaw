//! Recorded gateway shapes exercised without binding sockets.
use super::{app::{Action, App, Selection}, block_render};
use crate::{auth::Role, blocks::{Block, Data, fetch::{self, Gateway}}, client::RpcReply,
    decide::{Decision, GoalAction}, sanitize::TerminalText, state::{Employee, Model}, strings};
use async_trait::async_trait;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use serde_json::{Value, json};
use std::{sync::Mutex, time::{Duration, Instant}};

#[derive(Clone, Copy, Debug, PartialEq)]
enum Fixture { Populated, Empty, Refused, Unavailable, Hostile }
struct FakeGateway { state: Fixture, fail_at: Option<&'static str>, task_present: bool, calls: Mutex<Vec<(String, Value)>> }
impl FakeGateway {
    fn new(state: Fixture) -> Self { Self { state, fail_at: None, task_present: false, calls: Mutex::new(vec![]) } }
}
#[async_trait]
impl Gateway for FakeGateway {
    async fn request(&self, method: &str, params: Value) -> crate::Result<RpcReply> {
        self.calls.lock().unwrap().push((method.into(), params.clone()));
        if self.state == Fixture::Refused && self.fail_at.is_none_or(|m| m == method) {
            return Ok(RpcReply::Rejected { message: TerminalText::new("permission denied\u{1b}\u{202e}".into()), digest_mismatch: false });
        }
        if self.state == Fixture::Unavailable && method != "autopilot.list" && self.fail_at.is_none_or(|m| m == method) {
            return if method == "autopilot.breakers" {
                Ok(RpcReply::Rejected { message: TerminalText::new("Autopilot engine unavailable".into()), digest_mismatch: false })
            } else { Err(crate::Error::Disconnected) };
        }
        let empty = self.state == Fixture::Empty;
        let name = if self.state == Fixture::Hostile { format!("惡意\u{1b}]52;clipboard\u{7}\u{202e}{}", "長".repeat(300)) } else { "每週報告".into() };
        let list = |value: Value| if empty { json!([]) } else { json!([value]) };
        let run = json!({"run_id":"w1","workflow_id":name,"revision":2,"activation_id":"a1","status":"running","error_code":null,"created_at":"2026-11-02T08:00:00Z","deadline_at":"2026-11-03T08:00:00Z","steps":[{"step_id":name,"status":"pending","error_code":null}]});
        let value = match method {
            "workflow_drafts.list" => {
                assert_eq!(params, json!({"task_id":"t1"}));
                json!({"drafts":list(json!({"draft":{"draft_id":"d1","revision":2,"owner":"adu"}})),"next_cursor":null})
            }
            "workflow_runs.list" => {
                assert_eq!(params,json!({"draft_id":"d1","revision":2,"limit":20}));
                json!({"runs":list(run.clone())})
            }
            "workflow_runs.get" => { assert_eq!(params,json!({"run_id":"w1"})); run }
            "autopilot.list" => json!({"rules":list(json!({"id":"rule1","name":name,"enabled":true,"trigger_event":"task_created"}))}),
            "autopilot.breakers" => json!({"breakers":list(json!({"rule_id":"rule1","state":"half_open","opened_at":"2026-11-02T08:00:00Z"}))}),
            "evolution.status" => json!({"enabled":!empty,"mode":"prediction_driven","total_rounds":4,"applied_rounds":2,"agents":list(json!({"agent_id":name,"gvu_enabled":true,"cognitive_memory":true}))}),
            "evolution.stagnation" => {
                assert_eq!(params,json!({"agent_id":"adu"}));
                json!({"snapshots":list(json!({"agent_id":"adu","is_stagnant":true,"summary":name,"checked_at":"2026-11-02T08:00:00Z"}))})
            }
            "playbook.list" => { assert_eq!(params,json!({"agent_id":"adu"})); json!({"agent_id":"adu","entries":list(json!({"id":"p1","state":"active","category":"repair","content":name}))}) }
            "budget.incidents" => { assert_eq!(params,json!({"limit":50})); json!({"incidents":list(json!({"ts":"2026-11-02T08:00:00Z","agent_id":name,"event":"budget_breaker_open","scope":"daily","spent_cents":100,"cap_cents":100})),"by_agent":[]}) }
            "cost.summary" => { assert_eq!(params,json!({"hours":24})); json!({"available":!empty,"total_cost_millicents":123_000}) }
            "digest.latest" => json!({"enabled":true,"digest":if empty { Value::Null } else { json!({"date":"2026-11-02","since":"2026-11-01T00:00:00Z","generated_at":"2026-11-02T08:00:00Z","agents":[{"agent_id":"adu","finished_total":1,"pending_approvals":1,"runs_done":1,"runs_not_done":2,"spend_usd":null,"finished":[{"title":name,"completed_at":"2026-11-02T08:00:00Z"}],"artifacts":[]}]}) },"feedback":{}}),
            "tasks.list_page" => json!({"tasks":if self.task_present { json!([task()]) } else { list(task()) },"total":if empty && !self.task_present {0} else {1}}),
            "tasks.timeline" => { assert_eq!(params,json!({"task_id":"t1"})); json!({"task":task(),"activity":[],"runs":[],"criteria_ledger":{"mode":"enforce","last_report_round":2,"invalid_reports":0,"units":list(json!({"handle":"C1","text":name,"status":"confirmed","evidence":name,"unresolved":null,"updated_round":2}))}}) }
            "tasks.role_turns" => { assert_eq!(params,json!({"task_id":"t1"})); json!({"turns":list(json!({"round":2,"role":"verifier","runtime":"codex","provider":name,"outcome":"ok","timestamp":"2026-11-02T08:00:00Z"}))}) }
            "tasks.iterations" => json!({"iterations":[]}),
            "tasks.changes" => json!({"changes":[],"truncated":false}),
            "tasks.stop_status" => json!({"stop":null,"authority_revision":3}),
            "workflow_runs.cancel" | "tasks.goal_create" => json!({"ok":true}),
            _ => panic!("unexpected method {method}"),
        };
        Ok(RpcReply::Accepted(value))
    }
}
fn task() -> Value { json!({"id":"t1","title":"目標","assigned_to":"adu","status":"needs_human","goal_mode":true,"updated_at":"revision"}) }
fn model() -> Model {
    let mut model = Model::default();
    model.role = Some(Role::Admin);
    model.goal_operators.insert("adu".into());
    model.inbox.stale = false; model.inbox.refetch_pending = false;
    model.employees.stale = false; model.employees.refetch_pending = false;
    model.employees.items.push(serde_json::from_value::<Employee>(json!({"name":"adu","display_name":"阿嘟","status":"active","archived":false})).unwrap());
    model
}
fn app(kind: Block) -> App {
    let mut app = App::default();
    app.selected = Some(Selection::Employee("adu".into()));
    app.blocks.open(kind, Some("adu".into()));
    app.blocks.panes[0].task_id = Some("t1".into());
    app
}
fn key(code: KeyCode) -> KeyEvent { KeyEvent::new(code, KeyModifiers::NONE) }
fn press(app: &mut App, c: char, model: &Model, now: Instant) -> Action { app.key_at(key(KeyCode::Char(c)),model,now,1000) }

#[tokio::test]
async fn subsystem_views_real_shapes_all_states_and_sanitized_snapshots() {
    let model = model();
    for kind in [Block::Workflow,Block::Autopilot,Block::Evolution,Block::Budget,Block::Digest,Block::Task] {
        for state in [Fixture::Populated,Fixture::Empty,Fixture::Refused,Fixture::Unavailable,Fixture::Hostile] {
            let fake = FakeGateway::new(state);
            let mut app = app(kind);
            // Empty task pages have no selected ID; explicit selection still asks for detail.
            if state == Fixture::Empty { app.blocks.panes[0].task_id = if kind == Block::Task { None } else { Some("t1".into()) }; }
            app.blocks.panes[0].data = match fetch::pane(&fake,&app.blocks.panes[0],true).await {
                Ok(data) => data,
                Err(e) => Data { error:Some(TerminalText::new(e.to_string())), ..Data::default() },
            };
            let body = block_render::content(&app.blocks.panes[0], &model, &app.logs).plain();
            assert!(!body.chars().any(crate::sanitize::forbidden));
            if matches!(state, Fixture::Refused | Fixture::Unavailable) {
                assert!(app.blocks.panes[0].data.stale);
                assert!(!body.contains(strings::VIEW_EMPTY));
                assert!(!body.contains("closed"));
            }
            if state == Fixture::Hostile { assert!(body.contains(strings::BLOCK_TRUNCATED)); }
            if kind == Block::Autopilot && state == Fixture::Unavailable { assert!(body.contains("Autopilot engine unavailable")); }
            if state == Fixture::Empty { assert!(body.contains(strings::VIEW_EMPTY)); }
            insta::assert_snapshot!(format!("autonomous_{kind:?}_{state:?}"),body);
            assert!(fake.calls.lock().unwrap().iter().all(|(method,_)| !method.ends_with("cancel") && !method.ends_with("create")));
        }
    }
}
#[test]
fn role_refusals_are_explained_and_no_stale_payload_is_displayed() {
    for (kind, label) in [(Block::Autopilot,strings::ADMIN_ONLY),(Block::Evolution,strings::MANAGER_ONLY),(Block::Budget,strings::MANAGER_ONLY)] {
        let mut model = model(); model.role = Some(Role::Employee);
        let mut app = app(kind); app.blocks.panes[0].data.selected = json!({"private":"never display"});
        let body = block_render::content(&app.blocks.panes[0],&model,&app.logs).plain();
        assert!(body.contains(label)); assert!(!body.contains("never display"));
    }
}
#[tokio::test]
async fn workflow_cancel_real_key_path_requires_fresh_review_code_and_sends_once() {
    let model = model(); let now = Instant::now(); let mut app = app(Block::Workflow);
    let fake = FakeGateway::new(Fixture::Populated);
    app.blocks.panes[0].data = fetch::pane(&fake,&app.blocks.panes[0],true).await.unwrap();
    assert_eq!(press(&mut app,'s',&model,now),Action::None);
    let card = app.control_card.as_mut().expect("confirmation"); card.code = "42".into(); card.viewport(20,4);
    assert_eq!(press(&mut app,'4',&model,now),Action::None);
    let ready = now + Duration::from_secs(2);
    press(&mut app,'4',&model,ready); press(&mut app,'2',&model,ready);
    assert_eq!(app.key_at(key(KeyCode::Enter),&model,ready,1000),Action::None);
    assert!(fake.calls.lock().unwrap().iter().all(|(m,_)| m != "workflow_runs.cancel"));
    let card = app.control_card.as_mut().unwrap(); card.viewport(1,20);
    press(&mut app,'4',&model,ready); press(&mut app,'2',&model,ready);
    let Action::Control(control) = app.key_at(key(KeyCode::Enter),&model,ready,1000) else { panic!("confirmed cancel"); };
    assert_eq!(control.request(),("workflow_runs.cancel",json!({"run_id":"w1"})));
    crate::controls::send(&fake,&control).await;
    assert_eq!(fake.calls.lock().unwrap().iter().filter(|(m,_)| m == "workflow_runs.cancel").count(),1);
    assert!(app.control_card.is_none());
}
#[test]
fn goal_create_palette_opens_edit_then_plan_toggle_then_confirmation() {
    let model = model(); let now = Instant::now(); let mut app = app(Block::Detail);
    app.key_at(KeyEvent::new(KeyCode::Char('p'),KeyModifiers::CONTROL),&model,now,1000);
    app.paste("goal create");
    assert_eq!(app.palette_commands(&model).len(),1);
    app.key_at(key(KeyCode::Enter),&model,now,1000);
    assert!(app.control_card.as_ref().unwrap().editing);
    let ready=now+Duration::from_secs(2);
    app.paste("建立測試報告\u{1b}\u{202e}");
    app.key_at(key(KeyCode::Tab),&model,ready,1000);
    app.key_at(key(KeyCode::Enter),&model,ready,1000);
    let card=app.control_card.as_mut().unwrap(); assert!(!card.editing); card.code="42".into(); card.viewport(1,20);
    let ready=ready+Duration::from_secs(2);
    press(&mut app,'4',&model,ready); press(&mut app,'2',&model,ready);
    let Action::Control(control)=app.key_at(key(KeyCode::Enter),&model,ready,1000) else { panic!("goal create"); };
    assert_eq!(control.request(),("tasks.goal_create",json!({"agent_id":"adu","description":"建立測試報告","plan_first":false})));
}
#[test]
fn plan_approval_uses_real_decision_card_rules_and_never_unlabelled_retry() {
    let model=model(); let now=Instant::now(); let mut app=app(Block::Task);
    app.blocks.panes[0].data=Data { stale:false, selected:task(), ..Data::default() };
    app.blocks.panes[0].data.selected["plan_pending"]=json!("先核對來源，再產生報告");
    app.key_at(key(KeyCode::Enter),&model,now,1000);
    let card=app.card.as_mut().unwrap(); card.code="42".into(); card.viewport(20,3);
    let ready=now+Duration::from_secs(2);
    assert!(app.card.as_ref().unwrap().controls(ready,1000,true).plain().contains(strings::PLAN_APPROVE));
    assert_eq!(press(&mut app,'r',&model,ready),Action::None);
    press(&mut app,'4',&model,ready); press(&mut app,'2',&model,ready);
    assert_eq!(app.key_at(key(KeyCode::Enter),&model,ready,1000),Action::None);
    app.card.as_mut().unwrap().viewport(1,20);
    let mut repeat=key(KeyCode::Char('4')); repeat.kind=KeyEventKind::Repeat;
    app.key_at(repeat,&model,ready,1000); assert!(app.card.as_ref().unwrap().input.is_empty());
    press(&mut app,'4',&model,ready); press(&mut app,'2',&model,ready);
    let Action::Decide(decision)=app.key_at(key(KeyCode::Enter),&model,ready,1000) else { panic!("plan decision"); };
    assert_eq!(decision,Decision::Goal { task_id:"t1".into(),action:GoalAction::Retry,message:String::new(),expected_updated_at:"revision".into() });
}
#[test]
fn finished_goal_single_c_focuses_required_message_no_empty_send() {
    let model=model(); let now=Instant::now(); let mut app=app(Block::Task);
    let mut row=task(); row["status"]=json!("done");
    app.blocks.panes[0].data=Data { stale:false,selected:row,..Data::default() };
    press(&mut app,'c',&model,now);
    assert!(app.card.is_some());
    app.paste("下一步\u{1b}");
    let ready=now+Duration::from_secs(2);
    app.key_at(key(KeyCode::Enter),&model,ready,1000);
    let Action::Decide(Decision::Goal { action:GoalAction::Continue,message,.. })=press(&mut app,'c',&model,ready) else { panic!("continue message"); };
    assert_eq!(message,"下一步");
}
#[test]
fn all_new_blocks_are_reachable_through_palette_and_keyboard() {
    let model=model(); let now=Instant::now();
    for (key_char,kind,query) in [('W',Block::Workflow,"workflow"),('A',Block::Autopilot,"autopilot"),('E',Block::Evolution,"evolution"),('B',Block::Budget,"budget"),('G',Block::Digest,"digest")] {
        let mut app=app(Block::Task); app.blocks.panes[0].data.selected=task();
        press(&mut app,key_char,&model,now); assert_eq!(app.blocks.active,kind);
        app.key_at(KeyEvent::new(KeyCode::Char('p'),KeyModifiers::CONTROL),&model,now,1000); app.paste(query);
        assert_eq!(app.palette_commands(&model).len(),1);
        app.key_at(key(KeyCode::Enter),&model,now,1000); assert_eq!(app.blocks.active,kind);
    }
}

#[test]
fn breaker_events_are_bounded_informational_and_open_the_matching_view() {
    let now=Instant::now(); let mut model=model(); let mut app=app(Block::Detail);
    for (name,payload,kind) in [("budget.breaker_open",json!({"agent_id":"adu"}),Block::Budget),
        ("autopilot.breaker_open",json!({"rule_id":"rule\u{1b}\u{202e}"}),Block::Autopilot)] {
        model.event(&crate::client::ServerEvent { name:name.into(),payload,seq:None,boot_id:None });
        let item=model.inbox.items.last().unwrap();
        assert!(matches!(item,crate::state::InboxItem::Breaker(_)));
        assert!(!item.context().chars().any(crate::sanitize::forbidden));
        app.selected=Some(Selection::Inbox(item.key()));
        app.key_at(key(KeyCode::Enter),&model,now,1000);
        assert_eq!(app.blocks.active,kind); assert!(app.card.is_none()); assert!(app.control_card.is_none());
    }
    for i in 0..200 { model.event(&crate::client::ServerEvent { name:"autopilot.breaker_open".into(),payload:json!({"rule_id":format!("r{i}")}),seq:None,boot_id:None }); }
    assert_eq!(model.breakers.len(),128);
    model.role=Some(Role::Employee);
    model.event(&crate::client::ServerEvent { name:"budget.breaker_open".into(),payload:json!({"agent_id":"other"}),seq:None,boot_id:None });
    assert_eq!(model.breakers.len(),128);
}
#[test]
fn real_turn_events_link_tasks_without_inventing_outcomes_and_preserve_parallel_work() {
    let now=Instant::now(); let mut model=model(); let mut app=app(Block::Detail);
    let event=|name:&str,seq,payload| crate::client::ServerEvent { name:name.into(),payload,seq:Some(seq),boot_id:Some("boot".into()) };
    let goal=json!({"agent_id":"adu","source":"goal","runtime":"codex","task_id":"t1"});
    model.event(&event("agent.turn_started",1,goal.clone()));
    model.event(&event("agent.turn_started",1,goal.clone()));
    assert_eq!(model.activity["adu"].turns.len(),1);
    press(&mut app,'T',&model,now); assert_eq!(app.blocks.panes[0].task_id.as_deref(),Some("t1"));
    let chat=json!({"agent_id":"adu","source":"channel","runtime":"claude"});
    model.event(&event("agent.turn_started",2,chat.clone()));
    assert_eq!(model.activity["adu"].turns.len(),2);
    assert!(model.activity["adu"].task_id.is_none());
    press(&mut app,'T',&model,now); assert!(app.blocks.panes[0].task_id.is_none());
    model.event(&event("agent.turn_finished",3,chat));
    assert!(model.activity["adu"].working); assert_eq!(model.activity["adu"].task_id.as_deref(),Some("t1"));
    model.event(&event("agent.turn_finished",4,goal));
    assert!(!model.activity["adu"].working); assert!(model.activity["adu"].completed_id.is_none()); assert!(!model.activity["adu"].error);
}
#[test]
fn plan_stale_changed_missing_revision_and_long_payload_cannot_authorize() {
    for variant in ["stale","changed","revision","long"] {
        let mut model=model(); let now=Instant::now(); let mut app=app(Block::Task);
        let mut row=task(); row["plan_pending"]=json!(if variant=="long" {"x".repeat(17_000)} else {"plan".into()});
        if variant=="revision" { row.as_object_mut().unwrap().remove("updated_at"); }
        app.blocks.panes[0].data=Data { stale:false,selected:row,..Data::default() };
        app.key_at(key(KeyCode::Enter),&model,now,1000);
        app.card.as_mut().unwrap().viewport(1,20);
        if variant=="stale" { model.inbox.stale=true; }
        if variant=="changed" { app.blocks.panes[0].data.selected["updated_at"]=json!("new"); app.refresh(&model); }
        let ready=now+Duration::from_secs(2);
        for c in app.card.as_ref().unwrap().code.clone().chars() { assert_eq!(press(&mut app,c,&model,ready),Action::None); }
        assert_eq!(app.key_at(key(KeyCode::Enter),&model,ready,1000),Action::None);
    }
}

#[tokio::test]
async fn cost_refused_or_unavailable_is_hidden_never_zero() {
    assert_eq!(crate::state_extra::cost_summary(&FakeGateway::new(Fixture::Populated)).await,Some(123_000));
    for fixture in [Fixture::Refused,Fixture::Unavailable,Fixture::Empty] {
        assert_eq!(crate::state_extra::cost_summary(&FakeGateway::new(fixture)).await,None);
    }
    for malformed in [json!({}),json!({"available":true}),json!({"available":true,"total_cost_millicents":-1}),json!({"available":false,"total_cost_millicents":0})] {
        assert_eq!(crate::state::cost_amount(&malformed),None);
    }
    assert_eq!(crate::state::cost_amount(&json!({"available":true,"total_cost_millicents":0})),Some(0));
}

#[tokio::test]
async fn known_role_refusal_does_not_make_an_admin_request() {
    for kind in [Block::Autopilot,Block::Evolution,Block::Budget] {
        let fake=FakeGateway::new(Fixture::Populated); let mut app=app(kind);
        app.blocks.panes[0].data=fetch::pane_for_role(&fake,&app.blocks.panes[0],Some(Role::Employee)).await.unwrap();
        assert!(app.blocks.panes[0].data.error.is_some()); assert!(fake.calls.lock().unwrap().is_empty());
    }
}
#[test]
fn workflow_record_switch_keeps_source_task_and_invalidates_card_subject() {
    let model=model(); let now=Instant::now(); let mut app=app(Block::Workflow);
    app.blocks.panes[0].data=Data { stale:false,total:2,selected:json!({"run_id":"w1","_tui_owner":"adu"}),..Data::default() };
    press(&mut app,']',&model,now);
    assert_eq!(app.blocks.panes[0].index,1); assert_eq!(app.blocks.panes[0].task_id.as_deref(),Some("t1"));
    press(&mut app,'s',&model,now); assert!(app.control_card.is_none());
}

#[tokio::test]
async fn role_turns_failure_and_empty_criteria_keep_task_detail_and_never_infer_pass() {
    for state in [Fixture::Empty,Fixture::Refused,Fixture::Unavailable] {
        let mut fake=FakeGateway::new(state); fake.fail_at=Some("tasks.role_turns"); fake.task_present=true;
        let mut app=app(Block::Task);
        app.blocks.panes[0].data=fetch::pane(&fake,&app.blocks.panes[0],true).await.unwrap();
        let body=block_render::content(&app.blocks.panes[0],&model(),&app.logs).plain();
        assert!(body.contains(strings::CRITERIA)); assert!(body.contains(strings::ROLES));
        assert!(body.contains("目標"));
        if state != Fixture::Empty { assert!(app.blocks.panes[0].data.section("roles_error").is_string()); }
        insta::assert_snapshot!(format!("role_detail_{state:?}"),body);
    }
}

#[test]
fn pending_plan_extra_digit_is_not_silently_accepted() {
    let model=model(); let now=Instant::now(); let mut app=app(Block::Task);
    let mut row=task(); row["plan_pending"]=json!("plan");
    app.blocks.panes[0].data=Data { stale:false,selected:row,..Data::default() };
    app.key_at(key(KeyCode::Enter),&model,now,1000);
    let card=app.card.as_mut().unwrap(); card.code="42".into(); card.viewport(1,20);
    let ready=now+Duration::from_secs(2);
    for c in "420".chars() { press(&mut app,c,&model,ready); }
    assert_eq!(app.key_at(key(KeyCode::Enter),&model,ready,1000),Action::None);
    assert_eq!(app.card.as_ref().unwrap().hint,Some(strings::CARD_WRONG_CODE));
}
#[test]
fn no_observation_or_no_task_link_is_unknown_not_zero_or_fabricated() {
    let now=Instant::now(); let mut model=model(); let mut app=app(Block::Detail);
    assert_eq!(model.observed_working(),None);
    model.event(&crate::client::ServerEvent { name:"agent.turn_started".into(),payload:json!({"agent_id":"adu","source":"channel","runtime":"codex"}),seq:None,boot_id:None });
    assert_eq!(model.observed_working(),None);
    model.employees.stale=false; model.employees.refetch_pending=false;
    assert_eq!(model.observed_working(),Some(1));
    press(&mut app,'T',&model,now);
    assert_eq!(app.notice.as_deref(),Some(strings::WORKING_TASK_UNKNOWN));
    assert!(app.blocks.panes[0].task_id.is_none());
}

#[test]
fn workflow_cancel_extra_digit_does_not_confirm() {
    let model=model(); let now=Instant::now(); let mut app=app(Block::Workflow);
    app.blocks.panes[0].data=Data { stale:false,selected:json!({"run_id":"w1","_tui_owner":"adu"}),..Data::default() };
    press(&mut app,'s',&model,now);
    let card=app.control_card.as_mut().unwrap(); card.code="42".into(); card.viewport(1,20);
    let ready=now+Duration::from_secs(2);
    for c in "420".chars() { press(&mut app,c,&model,ready); }
    assert_eq!(app.key_at(key(KeyCode::Enter),&model,ready,1000),Action::None);
    assert!(app.control_card.is_some());
}
