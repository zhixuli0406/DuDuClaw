use super::*;
use super::super::notices::{Filter, Kind, Mailbox};
use std::time::{Duration, Instant};

fn key(code: KeyCode) -> KeyEvent { KeyEvent::new(code, KeyModifiers::NONE) }
fn setup() -> (Model, App) {
    let mut model = empty();
    model.employees = fresh(vec![employee("adu", "阿嘟", "active")]);
    model.goal_operators.insert("adu".into());
    model.inbox = fresh(vec![approval("a1", "adu", "PRIVATE TASK CONTENT", "tool", "approval")]);
    let mut app = App::default(); app.refresh(&model);
    (model, app)
}
fn notice(app: &mut App, kind: Kind, name: &str, inbox: Option<&str>, now: Instant) {
    app.notices.push(kind, TerminalText::new(name.into()), Some("adu".into()), inbox.map(str::to_owned), now);
    app.notices.items.front_mut().unwrap().timestamp = "2026-11-04T03:59:40Z".into();
}
fn settings(glyphs: Glyphs) -> Settings {
    let mut s = Settings::test_defaults(); s.glyphs = glyphs;
    s.colors = crate::theme::Theme::new(crate::theme::ThemeMode::Dark, crate::theme::Capability::TrueColor);
    s
}
#[test]
fn toast_and_mailbox_snapshots() {
    for glyphs in [Glyphs::Unicode, Glyphs::Ascii] {
        let (model, mut app) = setup();
        let now = Instant::now();
        notice(&mut app, Kind::NeedYou, "阿嘟", Some("approval:a1"), now - Duration::from_secs(60));
        notice(&mut app, Kind::Done, "研究員", None, now);
        let output = screen(&model, &app, &settings(glyphs), 110, 26);
        assert!(output.contains("完成 · 研究員") && output.contains("需要你 · 阿嘟"));
        assert!(!output.contains("PRIVATE TASK CONTENT"));
        insta::assert_snapshot!(format!("round3_toasts_stacked_{glyphs:?}"), output);
        app.notices.dismiss(app.notices.items[0].id);
        insta::assert_snapshot!(format!("round3_toast_persistent_{glyphs:?}"), screen(&model, &app, &settings(glyphs), 110, 26));
        app.key(key(KeyCode::Char('M')), &model);
        // The open action acknowledges existing notices; subsequent arrivals stay unread.
        notice(&mut app, Kind::Error, "阿嘟", None, now);
        for filter in [Filter::All, Filter::Unread, Filter::Errors] {
            app.mailbox = Some(Mailbox { filter, selected: None });
            insta::assert_snapshot!(format!("round3_mailbox_{filter:?}_{glyphs:?}"), screen(&model, &app, &settings(glyphs), 110, 26));
        }
    }
}
#[test]
fn zoom_preserves_blocks_across_resize_and_restore() {
    for glyphs in [Glyphs::Unicode, Glyphs::Ascii] {
        let (model, mut app) = setup();
        app.blocks.add(crate::blocks::Split::Vertical);
        app.blocks.open(crate::blocks::Block::Task, Some("adu".into()));
        app.blocks.panes[1].data.stale = false;
        let ids: Vec<_> = app.blocks.panes.iter().map(|p| p.id.clone()).collect();
        insta::assert_snapshot!(format!("round3_zoom_off_{glyphs:?}"), screen(&model, &app, &settings(glyphs), 110, 26));
        assert_eq!(app.key(key(KeyCode::Char('z')), &model), Action::None);
        assert!(app.zoomed);
        for (width, height) in [(110,26), (80,24), (140,40), (24,12), (23,11)] {
            let output = screen(&model, &app, &settings(glyphs), width, height);
            if width >= 24 { assert!(output.contains("放大中 · z 還原")); assert!(!output.contains("DuDuClaw 控制室")); }
            else { assert!(output.contains("終端太小")); }
        }
        insta::assert_snapshot!(format!("round3_zoom_on_{glyphs:?}"), screen(&model, &app, &settings(glyphs), 110, 26));
        app.key(key(KeyCode::Char('z')), &model);
        assert!(!app.zoomed);
        assert_eq!(ids, app.blocks.panes.iter().map(|p| p.id.clone()).collect::<Vec<_>>());
        assert_eq!(app.blocks.focused, 1);
    }
}
#[test]
fn mailbox_key_routing_is_local_and_preserves_goal_shortcut() {
    let (model, mut app) = setup();
    notice(&mut app, Kind::NeedYou, "阿嘟", Some("approval:a1"), Instant::now());
    let selected = app.selected.clone();
    assert_eq!(app.key(key(KeyCode::Char('M')), &model), Action::None);
    assert!(app.mailbox.is_some()); assert_eq!(app.selected, selected);
    assert!(app.notices.filtered(Filter::Unread).is_empty());
    let count = app.blocks.panes.len();
    assert_eq!(app.key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL), &model), Action::None);
    assert_eq!(app.blocks.panes.len(), count);
    app.key(key(KeyCode::Tab), &model);
    assert_eq!(app.mailbox.as_ref().unwrap().filter, Filter::Unread);
    app.key(key(KeyCode::Tab), &model);
    assert_eq!(app.mailbox.as_ref().unwrap().filter, Filter::Errors);
    app.key(key(KeyCode::Tab), &model);
    app.zoomed = true;
    assert_eq!(app.key(key(KeyCode::Enter), &model), Action::None);
    assert!(app.card.is_none()); assert!(app.mailbox.is_none()); assert!(!app.zoomed);
    assert_eq!(app.selected, Some(Selection::Inbox("approval:a1".into())));
    app.leave_list(); app.selected = Some(Selection::Employee("adu".into()));
    app.key(key(KeyCode::Char('N')), &model);
    assert!(app.control_card.is_some()); assert!(app.mailbox.is_none());
}
#[test]
fn notifications_never_steal_keys_and_cards_hide_toasts() {
    let (model, mut app) = setup();
    let selected = app.selected.clone();
    app.observe_notices(&model, Instant::now());
    assert_eq!(app.selected, selected); assert!(app.toast_focus.is_none());
    let now = Instant::now();
    assert_eq!(app.key(key(KeyCode::F(8)), &model), Action::None);
    assert!(app.toast_focus.is_some());
    let count = app.blocks.panes.len();
    app.key(key(KeyCode::Char('x')), &model);
    assert_eq!(app.blocks.panes.len(), count);
    assert!(app.notices.visible(now).is_empty());
    notice(&mut app, Kind::NeedYou, "NOTIFICATION ONLY", Some("approval:a1"), now);
    app.key(key(KeyCode::Char('I')), &model);
    app.key(key(KeyCode::Enter), &model);
    assert!(app.card.is_some());
    // Other queued toasts must not cover an unrelated decision card either.
    notice(&mut app, Kind::Error, "NOTIFICATION ONLY", None, now);
    app.zoomed = true;
    let shown = screen(&model, &app, &settings(Glyphs::Unicode), 110, 26);
    assert!(!shown.contains("NOTIFICATION ONLY"));
    let before = app.zoomed;
    app.key_at(key(KeyCode::Char('z')), &model, now + Duration::from_secs(2), 0);
    assert_eq!(app.zoomed, before);
    app.card = None;
    app.focus = crate::chat::Focus::Input;
    app.key(key(KeyCode::Char('M')), &model); app.key(key(KeyCode::Char('z')), &model);
    assert!(app.input.ends_with("Mz")); assert!(app.mailbox.is_none()); assert_eq!(app.zoomed, before);
}
#[test]
fn zoom_inbox_and_palette_keep_visible_targets() {
    let (model, mut app) = setup();
    app.key(key(KeyCode::Char('z')), &model);
    app.key(key(KeyCode::Char('I')), &model);
    assert!(!app.zoomed);
    assert!(screen(&model, &app, &settings(Glyphs::Ascii), 110,26).contains("PRIVATE TASK CONTENT"));
    app.key(key(KeyCode::Char('z')), &model);
    app.key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL), &model);
    assert!(app.palette.is_some()); assert!(app.zoomed);
    assert!(screen(&model, &app, &settings(Glyphs::Ascii), 110,26).contains("命令"));
}
#[test]
fn mailbox_and_toast_focus_do_not_answer_hidden_downstream_prompt() {
    let (model, mut app) = setup();
    app.chat.connected = true;
    app.chat.prepare(crate::chat::Submission { agent: "adu".into(), content: "hello".into() }).unwrap();
    let msg_id = app.chat.msg_id.clone(); let conv = app.chat.conv.clone();
    app.chat.apply(serde_json::from_value(json!({"type":"error","code":"cancelled","msg_id":msg_id,"conv":conv,"downstream_ref":"ref-123","downstream_running":1})).unwrap());
    for focused_toast in [false, true] {
        if focused_toast { notice(&mut app, Kind::NeedYou, "阿嘟", None, Instant::now()); app.key(key(KeyCode::F(8)), &model); }
        else { app.key(key(KeyCode::Char('M')), &model); }
        assert_eq!(app.key(key(KeyCode::Char('y')), &model), Action::None);
        assert_eq!(app.key(key(KeyCode::Esc), &model), Action::None);
        assert!(app.downstream_reference(Instant::now()).is_some());
    }
}
#[test]
fn snapshot_transitions_do_not_invent_results_or_repeat_notifications() {
    let (mut model, mut app) = setup();
    let now = Instant::now();
    model.recent_activity.insert("adu".into(), Activity { finished_runs: vec![("old".into(), false)], ..Default::default() });
    app.observe_notices(&model, now);
    assert_eq!(app.notices.items.len(), 1); // Pending approval only, not historical completion.
    model.inbox.stale = true;
    app.observe_notices(&model, now);
    assert_eq!(app.notices.items.len(), 1);
    model.inbox = fresh(model.inbox.items.clone());
    model.recent_activity.get_mut("adu").unwrap().finished_runs.insert(0, ("new-error".into(), true));
    app.observe_notices(&model, now);
    assert_eq!(app.notices.items.len(), 2);
    assert_eq!(app.notices.items[0].kind, Kind::Error);
    for _ in 0..110 { app.observe_notices(&model, now); }
    assert_eq!(app.notices.items.len(), 2);
    app.read.mark("approval:a1".into());
    app.observe_notices(&model, now);
    assert!(app.notices.visible(now).iter().all(|n| n.kind != Kind::NeedYou));
}
#[test]
fn new_surfaces_preserve_physical_width_for_hostile_cjk() {
    let (model, mut app) = setup();
    for _ in 0..40 { app.chat.append("員工", &"界·".repeat(70)); }
    notice(&mut app, Kind::NeedYou, &format!("\x1b]52;c;bad\x07\n{}", "界·".repeat(1000)), Some("approval:a1"), Instant::now());
    for width in [AmbiguousWidth::Narrow, AmbiguousWidth::Wide] {
        for glyphs in [Glyphs::Ascii, Glyphs::Unicode] {
            let mut settings = settings(glyphs); settings.ambiguous_width = width;
            for (columns, rows) in [(80,24), (110,26), (140,40)] {
                let output = screen(&model, &app, &settings, columns, rows);
                assert!(!output.contains('\x1b') && !output.contains('\x07'));
            }
        }
    }
}

#[test]
fn explicit_toast_focus_survives_expiry_without_splitting_workspace() {
    let (model, mut app) = setup();
    let now = Instant::now();
    notice(&mut app, Kind::Done, "阿嘟", None, now);
    app.key_at(key(KeyCode::F(8)), &model, now, 0);
    let count = app.blocks.panes.len();
    app.observe_notices(&model, now + Duration::from_secs(7));
    app.key_at(key(KeyCode::Char('x')), &model, now + Duration::from_secs(7), 0);
    assert_eq!(app.blocks.panes.len(), count);
    assert!(app.toast_focus.is_none());
}
#[test]
fn light_and_no_color_notice_snapshots_and_semantic_styles() {
    for (mode, capability) in [
        (crate::theme::ThemeMode::Light, crate::theme::Capability::TrueColor),
        (crate::theme::ThemeMode::Dark, crate::theme::Capability::NoColor),
    ] {
        let (model, mut app) = setup();
        let mut s = settings(Glyphs::Unicode);
        s.colors = crate::theme::Theme::new(mode, capability);
        notice(&mut app, Kind::NeedYou, "阿嘟", None, Instant::now());
        notice(&mut app, Kind::Error, "研究員", None, Instant::now());
        insta::assert_snapshot!(format!("round3_notice_{mode:?}_{capability:?}"), screen(&model, &app, &s, 110,26));
        app.key(key(KeyCode::Char('M')), &model);
        insta::assert_snapshot!(format!("round3_mail_{mode:?}_{capability:?}"), screen(&model, &app, &s, 110,26));
    }
    for (kind, symbol) in [(Kind::Done, "完"), (Kind::NeedYou, "需"), (Kind::Error, "錯")] {
        let (model, mut app) = setup();
        let s = settings(Glyphs::Unicode);
        notice(&mut app, kind, "阿嘟", None, Instant::now());
        let mut terminal = Terminal::new(TestBackend::new(110,26)).unwrap();
        terminal.draw(|f| render::draw(f, &model, &connection(), &TerminalText::default(), &mut app, &s)).unwrap();
        let expected = match kind { Kind::Done => s.colors.success, Kind::NeedYou => s.colors.warning, Kind::Error => s.colors.danger };
        assert!(terminal.backend().buffer().content.iter().any(|cell| cell.symbol() == symbol && cell.fg == expected && cell.modifier.contains(Modifier::BOLD)));
    }
}

#[test]
fn breaker_reopen_notifies_once_per_sequence_and_keeps_admin_gate() {
    let (mut model, mut app) = setup();
    model.inbox = fresh(vec![]);
    let now = Instant::now();
    let mut event = crate::client::ServerEvent { name: "budget.breaker_open".into(), seq: Some(1), boot_id: Some("boot".into()),
        payload: json!({"agent_id":"adu","message":"PRIVATE EVENT CONTENT"}) };
    app.notices.event(&event, &model, now);
    assert_eq!(app.notices.items.len(), 1);
    model.inbox.items.push(InboxItem::Breaker(crate::state::Breaker { key: "budget.breaker_open:adu".into(), autopilot: false,
        subject: TerminalText::new("adu".into()), agent: TerminalText::new("adu".into()), title: TerminalText::new("PRIVATE BREAKER TITLE".into()) }));
    app.observe_notices(&model, now);
    assert_eq!(app.notices.items.len(), 1);
    app.notices.event(&event, &model, now);
    assert_eq!(app.notices.items.len(), 1);
    event.seq = Some(2);
    app.notices.event(&event, &model, now);
    assert_eq!(app.notices.items.len(), 2);
    app.observe_notices(&model, now);
    assert_eq!(app.notices.items.len(), 2);
    event.seq = Some(1);
    app.notices.event(&event, &model, now);
    assert_eq!(app.notices.items.len(), 2);
    event.boot_id = Some("new-boot".into());
    app.notices.event(&event, &model, now);
    assert_eq!(app.notices.items.len(), 3);
    model.role = Some(Role::Manager); event.seq = Some(3);
    app.notices.event(&event, &model, now);
    assert_eq!(app.notices.items.len(), 3);
    assert!(app.notices.items.iter().all(|n| n.name.plain() == "阿嘟"));
}
