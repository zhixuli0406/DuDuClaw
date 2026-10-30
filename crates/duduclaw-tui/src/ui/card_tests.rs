use super::{card::{Card, Codes, Effect}, card_render, app::{App, Action, Selection}};
use crate::{auth::Role, decide::{Decision, GoalAction}, sanitize::TerminalText,
    settings::{Settings, AmbiguousWidth}, snooze::Snooze, state::{Approval, HumanTask, InboxItem, Model}, strings};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend, buffer::CellWidth};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

fn approval(extra: Value) -> InboxItem {
    let mut v = json!({"id":"a1","agent_id":"adu","kind":"tool","summary":"確認操作",
        "request_kind":"approval","status":"pending","may_decide":true,"expires_at":2000,
        "decided_in_dashboard_only":false,"effect_class":"modify","payload":{"text":"完整內容"}});
    for (k, v2) in extra.as_object().unwrap() { v[k] = v2.clone(); }
    let mut a: Approval = serde_json::from_value(v.clone()).unwrap();
    a.detail = TerminalText::new(v.to_string());
    InboxItem::Approval(a)
}
fn goal(status: &str) -> InboxItem {
    let v = json!({"id":"t1","assigned_to":"adu","title":"目標","status":status,"goal_mode":true});
    let mut t: HumanTask = serde_json::from_value(v.clone()).unwrap();
    t.detail = TerminalText::new(v.to_string()); InboxItem::Task(t)
}
fn model() -> Model {
    let mut m = Model::default(); m.role = Some(Role::Admin);
    m.goal_operators.insert("adu".into());
    m.inbox.stale = false; m.inbox.refetch_pending = false;
    m.employees.stale = false; m.employees.refetch_pending = false; m
}
fn open(item: InboxItem, model: &Model, now: Instant) -> Card {
    Card::open(item, model, now, &mut Codes::default())
}
fn key(c: char) -> KeyEvent { KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE) }
fn press(card: &mut Card, k: KeyEvent, now: Instant) -> Effect {
    card.key(k, now + Duration::from_secs(1), 1000, true)
}
fn enter(card: &mut Card, now: Instant) -> Effect {
    press(card, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), now)
}
fn type_code(card: &mut Card, now: Instant) {
    for c in card.code.clone().chars() { assert_eq!(press(card, key(c), now), Effect::None); }
}
#[test]
fn first_second_ignored_wrong_code_then_exact_code_only_once() {
    let now = Instant::now(); let mut c = open(approval(json!({"digest":"seen"})), &model(), now);
    for k in [key('n'), key('s'), key('0'), KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)] {
        assert_eq!(c.key(k, now + Duration::from_millis(999), 1000, true), Effect::None);
    }
    c.code = "23".into();
    press(&mut c, key('9'), now); press(&mut c, key('9'), now);
    assert_eq!(enter(&mut c, now), Effect::None);
    assert_eq!(c.hint, Some(strings::CARD_WRONG_CODE));
    assert!(c.input.is_empty()); type_code(&mut c, now);
    assert_eq!(enter(&mut c, now), Effect::Send(Decision::Approval {id:"a1".into(),approve:true,digest:Some("seen".into())}));
    assert_eq!(press(&mut c, key('n'), now), Effect::None);
}
#[test]
fn code_changes_on_every_opening_including_reopening_same_row() {
    let now = Instant::now(); let m = model(); let item = approval(json!({}));
    let mut codes = Codes::default();
    let mut previous = Card::open(item.clone(), &m, now, &mut codes).code;
    for _ in 0..200 {
        let next = Card::open(item.clone(), &m, now, &mut codes).code;
        assert_eq!(next.len(), 2); assert_ne!(previous, next); previous = next;
    }
}
#[test]
fn high_risk_requires_real_wrapped_bottom_and_resize_relocks() {
    let now = Instant::now();
    let mut c = open(approval(json!({"effect_class":"admin","payload":{"command":"x".repeat(300)}})), &model(), now);
    assert!(c.high_risk);
    let lines = super::text::wrapped_lines(&c.body, 20, AmbiguousWidth::Wide);
    c.viewport(lines, 5); type_code(&mut c, now); assert!(c.input.is_empty());
    assert_eq!(enter(&mut c, now), Effect::None);
    for _ in 0..lines { press(&mut c, KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE), now); }
    type_code(&mut c, now); assert_eq!(c.input.len(), 2);
    c.viewport(lines * 2, 5); assert!(c.input.is_empty()); assert_eq!(enter(&mut c, now), Effect::None);
    assert!(matches!(press(&mut c, key('n'), now), Effect::Send(Decision::Approval {approve:false,..})));
}
#[test]
fn terminal_actions_require_consecutive_distinct_presses_without_countdown() {
    for (k, action) in [('a',GoalAction::Abort),('d',GoalAction::Done),('t',GoalAction::Takeover)] {
        let now = Instant::now(); let mut c = open(goal("needs_human"), &model(), now);
        assert_eq!(press(&mut c, key(k), now), Effect::None);
        let mut repeat = key(k); repeat.kind = KeyEventKind::Repeat;
        assert_eq!(press(&mut c, repeat, now), Effect::None);
        press(&mut c, key('x'), now); assert_eq!(press(&mut c, key(k), now), Effect::None);
        assert_eq!(c.key(key(k),now+Duration::from_secs(3600),1000,true),
            Effect::Send(Decision::Goal {task_id:"t1".into(),action,message:String::new()}));
    }
    let now = Instant::now(); let mut c = open(goal("needs_human"), &model(), now);
    assert!(matches!(press(&mut c,key('r'),now),Effect::Send(Decision::Goal{action:GoalAction::Retry,..})));
}
#[test]
fn continue_requires_finished_goal_and_explicit_message_as_real_handler_does() {
    let now = Instant::now(); let mut c = open(goal("done"), &model(), now);
    assert_eq!(press(&mut c,key('c'),now),Effect::None);
    press(&mut c,key('i'),now); for ch in "繼續下一項".chars() { press(&mut c,key(ch),now); }
    press(&mut c,KeyEvent::new(KeyCode::Esc,KeyModifiers::NONE),now);
    assert_eq!(press(&mut c,key('c'),now),Effect::Send(Decision::Goal{task_id:"t1".into(),action:GoalAction::Continue,message:"繼續下一項".into()}));
}
#[test]
fn read_only_dashboard_expired_missing_ttl_and_stale_never_send() {
    for extra in [json!({"may_decide":false,"reason":"只有擁有者可操作"}),
        json!({"decided_in_dashboard_only":true}),json!({"expires_at":1000}),json!({"expires_at":null})] {
        let now = Instant::now(); let mut c = open(approval(extra), &model(), now);
        type_code(&mut c,now); assert_eq!(enter(&mut c,now),Effect::None);
        assert_eq!(press(&mut c,key('n'),now),Effect::None);
        assert!(!c.controls(now+Duration::from_secs(1),1000,true).plain().contains(strings::CARD_CODE));
    }
    let now = Instant::now(); let mut c = open(approval(json!({})), &model(), now);
    assert_eq!(c.key(key('n'),now+Duration::from_secs(1),1000,false),Effect::None);
    let mut m = model(); m.goal_operators.clear();
    let mut c = open(goal("needs_human"), &m, now); assert_eq!(press(&mut c,key('r'),now),Effect::None);
}
#[test]
fn changed_payload_removed_row_and_revoked_access_block_open_cards() {
    let now = Instant::now();
    for change in 0..3 {
        let mut m = model(); m.inbox.items = vec![goal("needs_human")];
        let mut app = App::default();
        app.card = Some(open(m.inbox.items[0].clone(), &m, now));
        match change {
            0 => m.inbox.items = vec![goal("done")],
            1 => m.inbox.items.clear(),
            _ => m.goal_operators.clear(),
        }
        app.refresh(&m);
        assert_eq!(app.key_at(key('r'), &m, now + Duration::from_secs(1), 1000), Action::None);
        assert!(app.card.as_ref().unwrap().blocked.is_some());
    }
}
#[test]
fn extra_code_character_never_approves_and_board_gate_hides_manager_actions() {
    let now = Instant::now(); let mut c = open(approval(json!({})), &model(), now);
    type_code(&mut c, now); press(&mut c, key('0'), now);
    assert_eq!(enter(&mut c, now), Effect::None);
    let mut m = model(); m.role = Some(Role::Manager);
    let mut c = open(approval(json!({"kind":"strategic_plan"})), &m, now);
    assert!(c.blocked.is_some()); assert_eq!(press(&mut c, key('n'), now), Effect::None);
}
#[test]
fn snooze_cap_resurface_expiry_and_local_roundtrip() {
    for (expiry, expected) in [(1100,1080),(2000,1800),(11000,9000)] {
        let item = approval(json!({"expires_at":expiry}));
        let s = Snooze::new(&item,1000,100000).unwrap();
        assert!(s.hidden(&item,expected-1)); assert!(!s.hidden(&item,expected));
        assert!(!s.hidden(&item,expiry));
    }
    let item = approval(json!({"expires_at":2000})); let s = Snooze::new(&item,1000,100000).unwrap();
    assert!(!s.hidden(&approval(json!({"expires_at":1100})),1001));
    assert!(Snooze::new(&item,2000,600).is_none());
    let home = tempfile::tempdir().unwrap(); let mut r = crate::read_state::ReadState::default();
    assert!(r.snooze(&item,1000)); r.save(home.path()).unwrap();
    let (r,notice) = crate::read_state::ReadState::load(home.path()); assert!(notice.is_none());
    assert!(r.hidden(&item,1599)); assert!(r.resurfaced(&item,1600));
}
#[test]
fn arrival_never_opens_card_or_moves_selection_and_sending_waits_for_refetch() {
    let mut m = model(); m.inbox.items.push(approval(json!({})));
    let now = Instant::now(); let mut app = App::default(); app.refresh(&m);
    let selected = app.selected.clone(); m.inbox.items.insert(0,goal("needs_human")); app.refresh(&m);
    assert_eq!(app.selected,selected); assert!(app.card.is_none());
    assert_eq!(app.key_at(KeyEvent::new(KeyCode::Enter,KeyModifiers::NONE),&m,now,1000),Action::None);
    let card_key = app.card.as_ref().unwrap().item.key(); app.refresh(&m);
    assert_eq!(card_key,"approval:a1");
    assert!(matches!(app.key_at(key('n'),&m,now+Duration::from_secs(1),1000),Action::Decide(_)));
    assert!(app.card.is_none()); assert_eq!(app.waiting_refresh,Some(0));
    app.decision_finished(crate::decide::Outcome::Changed);
    assert_eq!(app.notice.as_deref(), Some(strings::CARD_CHANGED));
    app.refresh(&m); assert_eq!(app.waiting_refresh,Some(0));
    m.local_refresh=1; app.refresh(&m); assert!(app.waiting_refresh.is_none());
    assert_eq!(app.selected,Some(Selection::Inbox("approval:a1".into())));
}

fn snapshot(extra: Value) -> String {
    let now = Instant::now(); let mut c = open(approval(extra), &model(), now); c.code="42".into();
    let mut terminal = Terminal::new(TestBackend::new(88,32)).unwrap();
    terminal.draw(|f| card_render::draw_at(f,f.area(),&mut c,&Settings::default(),true,now+Duration::from_secs(1),1000)).unwrap();
    let mut out=String::new(); let b=terminal.backend().buffer();
    for y in 0..32 { let mut line=String::new(); let mut x=0; while x<88 {
        let cell=&b[(x,y)]; line.push_str(cell.symbol()); x+=cell.cell_width().max(1);
    } out.push_str(line.trim_end());out.push('\n'); } out
}
#[test] fn card_normal() { insta::assert_snapshot!(snapshot(json!({}))); }
#[test] fn card_high_risk() { insta::assert_snapshot!(snapshot(json!({"effect_class":"admin","payload":{"command":"完整指令\n".repeat(40)}}))); }
#[test] fn card_read_only() { insta::assert_snapshot!(snapshot(json!({"may_decide":false,"reason":"此身分只有檢視權限"}))); }
#[test] fn card_dashboard_only() { insta::assert_snapshot!(snapshot(json!({"decided_in_dashboard_only":true}))); }
#[test] fn card_hostile_payload() {
    let out = snapshot(json!({"effect_class":"shell","payload":{"\u{202e}command":"甲\u{1b}]52;c;secret\u{7}\u{200d}乙"}}));
    assert!(out.contains("␛")); assert!(out.contains("⟨U+202E⟩")); assert!(!out.contains('\u{1b}'));
    insta::assert_snapshot!(out);
}
