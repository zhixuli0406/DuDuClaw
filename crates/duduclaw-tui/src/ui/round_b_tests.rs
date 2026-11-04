use super::*;
use ratatui::layout::Rect;
use super::super::{inline_decision, notices::{Kind, Mailbox}, widgets};
use crate::{chat::Frame, theme::{Capability, Theme, ThemeMode}};
use std::time::{Duration, Instant};

fn fixture() -> (Model, App) {
    let mut model = empty();
    model.employees = fresh(vec![employee("sales", "業務助理", "active")]);
    model.goal_operators.insert("sales".into());
    let mut app = App::default(); app.refresh(&model);
    app.chat.connected = true; app.chat.stale = false;
    app.chat.status = TerminalText::new(strings::CONNECTED.into());
    app.chat.prepare(crate::chat::Submission { agent: "sales".into(), content: "幫我整理這週的訂單，並準備給客戶 A 的報價。".into() }).unwrap();
    for (tool, summary, start, end) in [("Read", "orders/2026-11.csv", 1793757600000u64, 1793757600400u64), ("Write", "quote-A.pdf", 1793757600400, 1793757602700)] {
        app.chat.apply(Frame::parse(&json!({"type":"step","phase":"start","tool":tool,"summary":summary,"ts":start}).to_string()).unwrap());
        app.chat.apply(Frame::parse(&json!({"type":"step","phase":"completed","tool":tool,"ts":end}).to_string()).unwrap());
    }
    app.chat.apply(Frame::parse(r#"{"type":"assistant_done","ts":1793757603000,"content":"本週訂單已整理完成，報價單 quote-A.pdf 已備妥。"}"#).unwrap());
    for message in &mut app.chat.messages { message.time = "10:02".into(); }
    (model, app)
}
fn pending(model: &mut Model, goal: bool) {
    let value = if goal {
        json!({"id":"goal","assigned_to":"sales","title":"整理本週訂單","status":"needs_human","goal_mode":true,
            "updated_at":"revision-1","pause_reason":"缺少客戶 A 的報價條件\n請確認後再繼續"})
    } else {
        json!({"id":"approval","agent_id":"sales","summary":"寄給客戶 A · 主旨〈11 月報價〉 · quote-A.pdf","kind":"tool",
            "request_kind":"approval","status":"pending","may_decide":true,"expires_at":2000,"digest":"bound-digest","decided_in_dashboard_only":false})
    };
    let item = if goal {
        let mut task: HumanTask = serde_json::from_value(value.clone()).unwrap(); task.detail = TerminalText::new(value.to_string()); InboxItem::Task(task)
    } else {
        let mut approval: Approval = serde_json::from_value(value.clone()).unwrap(); approval.detail = TerminalText::new(value.to_string()); InboxItem::Approval(approval)
    };
    model.inbox.items = vec![item];
}
fn settings(mode: ThemeMode, capability: Capability) -> Settings {
    Settings { glyphs: Glyphs::Unicode, colors: Theme::new(mode, capability), ..Settings::test_defaults() }
}
fn painted(model: &Model, app: &App, settings: &Settings, width: u16, height: u16) -> String {
    let mut output = screen(model, app, settings, width, height);
    let mut app = app.clone();
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| render::draw(f, model, &connection(), &TerminalText::default(), &mut app, settings)).unwrap();
    output.push_str("\nPainted roles (fg / bg / modifiers):\n");
    for (role, symbol) in [("success", "✓"), ("pending", "待"), ("primary", "E"), ("user", "幫"), ("employee", "本"), ("selection", "›"), ("overlay", "╭")] {
        if let Some(cell) = terminal.backend().buffer().content.iter().rev().find(|cell| cell.symbol() == symbol) {
            output.push_str(&format!("{role}: {:?} / {:?} / {:?}\n", cell.fg, cell.bg, cell.modifier));
        }
    }
    output
}
#[test]
fn content_and_overlay_snapshots() {
    for (label, mode, capability) in [("dark", ThemeMode::Dark, Capability::TrueColor), ("light", ThemeMode::Light, Capability::TrueColor),
        ("ansi", ThemeMode::Ansi, Capability::Ansi16), ("no_color", ThemeMode::Dark, Capability::NoColor)] {
        let settings = settings(mode, capability);
        let (model, app) = fixture();
        for (width,height) in [(140,40),(80,24)] {
            insta::assert_snapshot!(format!("round_b_conversation_{label}_{width}x{height}"), painted(&model,&app,&settings,width,height));
            for goal in [false,true] {
                let mut model = model.clone(); pending(&mut model,goal);
                insta::assert_snapshot!(format!("round_b_decision_{}_{label}_{width}x{height}", if goal {"goal"} else {"approval"}), painted(&model,&app,&settings,width,height));
            }
        }
        for focused in [false,true] {
            let mut app = app.clone();
            app.focus = if focused { crate::chat::Focus::Input } else { crate::chat::Focus::List };
            insta::assert_snapshot!(format!("round_b_input_{}_{label}_140x40", if focused {"focused"} else {"unfocused"}), painted(&model,&app,&settings,140,40));
        }
        let mut palette = app.clone(); palette.palette = Some(super::super::palette::Palette { query: "員工".into(), selected: 0 });
        insta::assert_snapshot!(format!("round_b_palette_{label}_140x40"), painted(&model,&palette,&settings,140,40));
        let mut notices = app.clone();
        for (kind,name) in [(Kind::Done,"研究員"),(Kind::NeedYou,"業務助理")] {
            notices.notices.push(kind,TerminalText::new(name.into()),None,None,Instant::now());
            notices.notices.items.front_mut().unwrap().timestamp = "2026-11-04T03:59:40Z".into();
        }
        insta::assert_snapshot!(format!("round_b_toasts_{label}_140x40"), painted(&model,&notices,&settings,140,40));
        notices.mailbox = Some(Mailbox::default());
        insta::assert_snapshot!(format!("round_b_mailbox_{label}_140x40"), painted(&model,&notices,&settings,140,40));
    }
}
#[test]
fn inline_labels_open_existing_guard_code_and_revision_flow() {
    let now = Instant::now();
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    for goal in [false,true] {
        let (mut model, mut app) = fixture(); pending(&mut model,goal);
        let original = inline_decision::item(&app,&model).unwrap().key();
        for label in ['1','2','3'] {
            assert_eq!(app.key_at(key(KeyCode::Char(label)),&model,now,1000),Action::None);
            assert!(app.card.is_none());
        }
        assert_eq!(app.key_at(key(KeyCode::Enter),&model,now,1000),Action::None);
        let card = app.card.as_mut().unwrap();
        assert_eq!(card.item.key(), original); card.viewport(1,20); card.code = "23".into();
        for code in [KeyCode::Char('r'),KeyCode::Char('n'),KeyCode::Char('2'),KeyCode::Enter] {
            assert_eq!(app.key_at(key(code),&model,now+Duration::from_millis(999),1000),Action::None);
        }
        let ready = now+Duration::from_secs(1);
        if goal {
            assert_eq!(app.key_at(key(KeyCode::Char('r')),&model,ready,1000), Action::Decide(crate::decide::Decision::Goal {
                task_id:"goal".into(), action:crate::decide::GoalAction::Retry, message:String::new(), expected_updated_at:"revision-1".into() }));
        } else {
            for ch in ['9','9'] { app.key_at(key(KeyCode::Char(ch)),&model,ready,1000); }
            assert_eq!(app.key_at(key(KeyCode::Enter),&model,ready,1000),Action::None);
            assert_eq!(app.card.as_ref().unwrap().hint,Some(strings::CARD_WRONG_CODE));
            for ch in ['2','3'] { app.key_at(key(KeyCode::Char(ch)),&model,ready,1000); }
            assert_eq!(app.key_at(key(KeyCode::Enter),&model,ready,1000),Action::Decide(crate::decide::Decision::Approval {
                id:"approval".into(), approve:true, digest:Some("bound-digest".into()) }));
        }
        assert!(app.card.is_none());
        assert!(app.waiting_refresh.is_some());
        assert_eq!(app.key_at(key(KeyCode::Enter),&model,ready,1000),Action::None);
    }
}
#[test]
fn inline_stale_permission_and_changed_subject_remain_blocked() {
    let now = Instant::now();
    for changed in [false,true] {
        let (mut model, mut app) = fixture(); pending(&mut model,true);
        app.key_at(KeyEvent::new(KeyCode::Enter,KeyModifiers::NONE),&model,now,1000);
        app.card.as_mut().unwrap().viewport(1,20);
        if changed {
            if let InboxItem::Task(task) = &mut model.inbox.items[0] {
                let mut value = task.detail.json_value().unwrap(); value["updated_at"] = json!("revision-2");
                task.detail = TerminalText::new(value.to_string());
            }
        } else { model.goal_operators.clear(); model.role = Some(Role::Employee); }
        app.refresh(&model);
        assert_eq!(app.key_at(ordinary_key('r'),&model,now+Duration::from_secs(2),1000),Action::None);
        assert!(app.card.as_ref().unwrap().blocked.is_some());
    }
}
#[test]
fn overlay_repaints_background_and_clears_wide_glyph_edges() {
    for width in [AmbiguousWidth::Narrow,AmbiguousWidth::Wide] {
        let mut settings = settings(ThemeMode::Dark,Capability::TrueColor); settings.ambiguous_width = width;
        let mut terminal = Terminal::new(TestBackend::new(30,10)).unwrap();
        terminal.draw(|f| {
            widgets::styled(f,Rect::new(0,0,30,10),"界".repeat(15).as_str(),&settings,settings.colors.emphasis(settings.colors.success));
            widgets::styled(f,Rect::new(0,3,30,1),"界".repeat(15).as_str(),&settings,settings.colors.base());
            widgets::dim(f,&settings);
            let raised = widgets::overlay_settings(&settings);
            widgets::overlay(f,Rect::new(3,2,20,5),"",&raised);
        }).unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0,0)].fg,settings.colors.muted);
        assert!(!buffer[(0,0)].modifier.contains(Modifier::BOLD));
        assert_eq!(buffer[(2,3)].cell_width(),1);
        assert_eq!(buffer[(23,3)].cell_width(),1);
        for y in 0..10 {
            let mut row = String::new(); let mut x=0;
            while x<30 { let cell=&buffer[(x,y)]; row.push_str(cell.symbol()); x+=cell.cell_width().max(1); }
            assert_eq!(super::super::text::columns(&row,width),30,"{y}: {row}");
        }
    }
}

#[test]
fn downstream_stop_prompt_is_fixed_above_scrollback() {
    let mut app = super::downstream_app(Instant::now());
    for _ in 0..100 { app.chat.append(strings::CHAT_ASSISTANT, "history fills the screen"); }
    for scroll in [0, 50, 500] {
        app.chat_scroll = scroll;
        let output = screen(&empty(), &app, &Settings::test_defaults(), 80, 24);
        assert!(output.contains("y 停止"));
        assert!(output.contains("n 繼續"));
    }
}
#[test]
fn input_keeps_command_note_draft_and_focus_ring() {
    let (model, mut app) = fixture();
    app.focus = crate::chat::Focus::Input;
    app.input = "/pause".into();
    let output = screen(&model,&app,&Settings::test_defaults(),140,40);
    assert!(output.contains(strings::COMPUTER_COMMAND_NOTE));
    app.key(KeyEvent::new(KeyCode::Esc,KeyModifiers::NONE),&model);
    assert_eq!(app.input,"/pause");
    app.focus = crate::chat::Focus::Input;
    let settings = settings(ThemeMode::Light,Capability::TrueColor);
    let mut terminal = Terminal::new(TestBackend::new(140,40)).unwrap();
    terminal.draw(|frame| render::draw(frame,&model,&connection(),&TerminalText::default(),&mut app,&settings)).unwrap();
    assert!(terminal.backend().buffer().content.iter().any(|cell| cell.fg == settings.colors.border_focus
        && cell.modifier.contains(Modifier::UNDERLINED)));
}

#[test]
fn system_notice_does_not_move_the_completed_turn_tool_card() {
    let (model,mut app) = fixture();
    app.chat.append(strings::CHAT_SYSTEM, "SYSTEM_NOTICE_AFTER_REPLY");
    let output = screen(&model,&app,&Settings::test_defaults(),140,40);
    assert!(output.find("orders/2026-11.csv").unwrap() < output.find("SYSTEM_NOTICE_AFTER_REPLY").unwrap());
}

#[test]
fn palette_is_the_only_foreground_overlay_and_escape_restores_help() {
    let (model, mut app) = fixture();
    app.help = true;
    app.key(KeyEvent::new(KeyCode::Char('p'),KeyModifiers::CONTROL), &model);
    let output = screen(&model,&app,&Settings::test_defaults(),140,40);
    assert!(output.contains(strings::PALETTE_TITLE));
    assert!(!output.contains("操作說明"));
    app.key(KeyEvent::new(KeyCode::Esc,KeyModifiers::NONE),&model);
    assert!(app.help);
    assert!(screen(&model,&app,&Settings::test_defaults(),140,40).contains("操作說明"));
}
