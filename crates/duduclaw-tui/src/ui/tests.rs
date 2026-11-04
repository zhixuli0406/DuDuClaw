use super::{
    app::{Action, App, EmployeeStatus, Selection, employee_status},
    render,
};
use crate::{
    auth::Role,
    client::{ConnectionInfo, ConnectionState},
    inbox::{json_snapshot, text_snapshot},
    read_state::ReadState,
    sanitize::TerminalText,
    settings::{AmbiguousWidth, Glyphs, Settings},
    state::{Approval, Employee, HumanTask, InboxItem, Model, Snapshot},
    state_extra::{Activity, ScheduleSource, ScheduledRun},
    strings,
};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend, buffer::CellWidth, style::{Color, Modifier}};
use serde_json::json;
use unicode_width::UnicodeWidthStr;

fn fresh<T>(items: Vec<T>) -> Snapshot<T> {
    Snapshot {
        items,
        stale: false,
        refetch_pending: false,
        last_success: None,
        error: None,
    }
}
fn employee(id: &str, name: &str, status: &str) -> Employee {
    serde_json::from_value(json!({"name":id,"display_name":name,"status":status,"archived":false}))
        .unwrap()
}
fn approval(id: &str, agent: &str, summary: &str, kind: &str, request: &str) -> InboxItem {
    let value = json!({"id":id,"agent_id":agent,"summary":summary,"kind":kind,"request_kind":request,"status":"pending","decided_in_dashboard_only":true});
    let mut a: Approval = serde_json::from_value(value.clone()).unwrap();
    a.detail = TerminalText::new(value.to_string());
    InboxItem::Approval(a)
}
fn empty() -> Model {
    let mut m = Model::default();
    m.role = Some(Role::Admin);
    m.inbox = fresh(vec![]);
    m.employees = fresh(vec![]);
    m.schedules = fresh(vec![]);
    m.autonomy = fresh(vec![crate::autonomy::Status::default()]);
    m
}
fn populated() -> Model {
    let mut m = empty();
    let value = json!({"id":"t1","title":"週報待確認","assigned_to":"writer","status":"needs_human","pause_reason":"需要補充資料","goal_mode":true});
    let mut task: HumanTask = serde_json::from_value(value.clone()).unwrap();
    task.detail = TerminalText::new(value.to_string());
    m.inbox = fresh(vec![
        approval("a1", "adu", "確認資料匯出", "tool", "approval"),
        InboxItem::Task(task),
        InboxItem::Breaker(crate::state::Breaker { key: "budget.breaker_open:adu".into(), autopilot: false,
            subject: TerminalText::new("adu".into()), agent: TerminalText::new("adu".into()), title: TerminalText::new(strings::BREAKER_EVENT.into()) }),
        approval("a3", "adu", "請選擇時段", "tool", "question"),
    ]);
    m.employees = fresh(vec![
        employee("adu", "阿嘟", "active"),
        employee("worker", "小林", "working"),
        employee("done", "研究員", "active"),
        employee("idle", "會計", "active"),
        employee("paused", "小藍", "paused"),
        employee("ended", "舊員工", "ended"),
        employee("error", "檢查員", "error"),
    ]);
    m.activity.insert(
        "done".into(),
        Activity {
            completed_id: Some("run:done:123".into()),
            ..Default::default()
        },
    );
    m.schedules = fresh(vec![ScheduledRun {
        id: "heartbeat:adu".into(),
        agent: TerminalText::new("阿嘟".into()),
        next_run: TerminalText::new("11:00".into()),
        source: ScheduleSource::Heartbeat,
        detail: json!({}),
    }]);
    m
}
fn connection() -> ConnectionState {
    ConnectionState::Connected(ConnectionInfo {
        generation: 1,
        user_id: "u1".into(),
        role: Role::Admin,
        boot_id: None,
        version: Some(TerminalText::new("1.77.0".into())),
    })
}
fn screen(model: &Model, app: &App, settings: &Settings, width: u16, height: u16) -> String {
    crate::human_time::with_test_clock("2026-11-04T12:00:00+08:00", || {
    let mut app = app.clone();
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|f| {
            render::draw(
                f,
                model,
                &connection(),
                &TerminalText::new("/home/使用者/.duduclaw".into()),
                &mut app,
                settings,
            )
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    let mut output = String::new();
    for y in 0..height {
        let mut row = String::new();
        let mut x = 0;
        while x < width {
            let cell = &buffer[(x, y)];
            row.push_str(cell.symbol());
            x += cell.cell_width().max(1);
        }
        // Simulate the terminal's physical columns, not just Ratatui's buffer.
        let physical = match settings.ambiguous_width {
            AmbiguousWidth::Narrow => row.width(),
            AmbiguousWidth::Wide => row.width_cjk(),
        };
        assert_eq!(physical, usize::from(width), "row {y} physical width");
        output.push_str(row.trim_end());
        output.push('\n');
    }
    output
    })
}
#[test]
fn empty_screen() {
    let m = empty();
    let mut a = App::default();
    a.refresh(&m);
    insta::assert_snapshot!(screen(&m, &a, &Settings::test_defaults(), 100, 30));
}
#[test]
fn stale_screen() {
    let mut m = populated();
    m.invalidate_all();
    let mut a = App::default();
    a.selected = Some(Selection::Inbox("approval:a1".into()));
    a.detail_open = true;
    insta::assert_snapshot!(screen(&m, &a, &Settings::test_defaults(), 100, 30));
    let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
    t.draw(|f| {
        render::draw(
            f,
            &m,
            &connection(),
            &TerminalText::new("home".into()),
            &mut a,
            &Settings::test_defaults(),
        )
    })
    .unwrap();
    assert!(
        t.backend()
            .buffer()
            .content
            .iter()
            .filter(|c| c.symbol() != " ")
            .any(|c| c.fg == Settings::test_defaults().colors.muted)
    );
    assert!(m.inbox.count().is_none());
}

#[test]
fn selected_employee_row_uses_semantic_selection_tokens() {
    let mut m = empty();
    m.employees = fresh(vec![employee("adu", "阿嘟", "active")]);
    let mut a = App::default();
    a.refresh(&m);
    a.selected = Some(Selection::Employee("adu".into()));
    let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
    t.draw(|f| render::draw(f, &m, &connection(), &TerminalText::new("home".into()), &mut a, &Settings::test_defaults())).unwrap();
    let buffer = t.backend().buffer();
    let (name_y, name_x) = (0..buffer.area.height)
        .find_map(|y| {
            (0..28)
                .find(|&x| buffer[(x, y)].symbol() == "阿")
                .map(|x| (y, x))
        })
        .unwrap();
    let marker = &buffer[(name_x, name_y)];
    assert_ne!(marker.fg, Color::Yellow);
    assert_eq!(marker.fg, Settings::test_defaults().colors.selection_fg);
    assert!(marker.modifier.contains(Modifier::BOLD));
    assert_eq!(marker.bg, Settings::test_defaults().colors.selection_bg);

    let name = (0..buffer.area.width)
        .map(|x| &buffer[(x, name_y)])
        .find(|cell| cell.symbol() == "阿")
        .unwrap();
    assert_eq!(name.fg, Settings::test_defaults().colors.selection_fg);
    assert!(name.modifier.contains(Modifier::BOLD));
}
#[test]
fn populated_inbox_and_employees() {
    let m = populated();
    let mut a = App::default();
    a.refresh(&m);
    a.detail_open = true;
    insta::assert_snapshot!(screen(&m, &a, &Settings::test_defaults(), 100, 36));
}
#[test]
fn non_admin_screen() {
    let mut m = populated();
    m.role = Some(Role::Employee);
    let mut a = App::default();
    a.refresh(&m);
    let output = screen(&m, &a, &Settings::test_defaults(), 100, 30);
    assert!(!output.contains(strings::NEXT_RUNS));
    assert!(!output.contains("11:00"));
    insta::assert_snapshot!(output);
}
#[test]
fn narrow_60_columns() {
    let m = populated();
    let mut a = App::default();
    a.refresh(&m);
    a.detail_open = true;
    insta::assert_snapshot!(screen(&m, &a, &Settings::test_defaults(), 60, 32));
}
#[test]
fn cjk_names() {
    let mut m = empty();
    m.employees = fresh(vec![
        employee("cjk", "王小明與日本語あいうえお", "working"),
        employee("mixed", "研發 A組 café", "paused"),
    ]);
    let mut a = App::default();
    a.refresh(&m);
    a.detail_open = true;
    insta::assert_snapshot!(screen(&m, &a, &Settings::test_defaults(), 80, 28));
}
#[test]
fn hostile_name_visible_only_in_detail() {
    let mut m = empty();
    m.employees = fresh(vec![employee(
        "hostile",
        "阿嘟\x1b]52;c;evil\x07\x1b[2K\u{202e}甲",
        "active",
    )]);
    let mut a = App::default();
    a.refresh(&m);
    a.key(ordinary_key('D'), &m);
    a.detail_open = true;
    let output = screen(&m, &a, &Settings::test_defaults(), 100, 28);
    assert!(!output.contains('\x1b'));
    assert!(!output.contains('\u{202e}'));
    assert!(output.contains("U+202E"));
    assert!(output.contains('␛'));
    insta::assert_snapshot!(output);
}
#[test]
fn ambiguous_narrow() {
    ambiguous(AmbiguousWidth::Narrow);
}
#[test]
fn ambiguous_wide() {
    ambiguous(AmbiguousWidth::Wide);
}
fn ambiguous(width: AmbiguousWidth) {
    let mut m = populated();
    m.employees.items[0] = employee("adu", "阿嘟 Ω·α café", "active");
    let mut a = App::default();
    a.refresh(&m);
    a.selected = Some(Selection::Employee("adu".into()));
    a.detail_open = true;
    let s = Settings {
        glyphs: Glyphs::Unicode,
        ambiguous_width: width,
        ..Settings::test_defaults()
    };
    let output = screen(&m, &a, &s, 100, 36);
    match width {
        AmbiguousWidth::Narrow => insta::assert_snapshot!("ambiguous_width_narrow", output),
        AmbiguousWidth::Wide => insta::assert_snapshot!("ambiguous_width_wide", output),
    }
}
#[test]
fn selection_survives_refresh_and_removal_without_focus_theft() {
    let mut m = populated();
    let mut a = App::default();
    a.refresh(&m);
    a.key(ordinary_key('I'), &m);
    a.key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE), &m);
    let selected = a.selected.clone();
    a.detail_open = true;
    m.inbox
        .items
        .insert(0, approval("new", "adu", "新項目", "tool", "approval"));
    m.inbox.items.reverse();
    a.refresh(&m);
    assert_eq!(a.selected, selected);
    assert!(a.detail_open);
    insta::assert_snapshot!(
        "selection_after_refresh",
        screen(&m, &a, &Settings::test_defaults(), 100, 36)
    );
    m.inbox
        .items
        .retain(|i| Some(Selection::Inbox(i.key())) != selected);
    a.refresh(&m);
    assert_eq!(a.selected, Some(Selection::Inbox("approval:a1".into())));
    assert_ne!(a.detail(&m).plain(), strings::REMOVED);
    let mut a = App::default();
    a.refresh(&empty());
    a.refresh(&m);
    assert_eq!(a.selected, Some(Selection::InboxEntry)); // New employees do not steal the entry selection.
}
#[test]
fn badges_priority_done_receipts_and_identity_not_display_normalization() {
    let mut m = populated();
    let e = &m.employees.items[0];
    let mut read = ReadState::default();
    m.activity.insert(
        "adu".into(),
        Activity {
            working: true,
            error: true,
            completed_id: Some("run:r1".into()),
            ..Default::default()
        },
    );
    assert_eq!(employee_status(e, &m, &read), EmployeeStatus::Awaiting);
    m.inbox.items.clear();
    assert_eq!(employee_status(e, &m, &read), EmployeeStatus::Error);
    m.activity.get_mut("adu").unwrap().error = false;
    assert_eq!(employee_status(e, &m, &read), EmployeeStatus::Done);
    read.mark("run:r1".into());
    assert_eq!(employee_status(e, &m, &read), EmployeeStatus::Working);
    m.activity.get_mut("adu").unwrap().working = false;
    assert_eq!(employee_status(e, &m, &read), EmployeeStatus::Idle);
    m.inbox.items.push(approval(
        "x",
        "ad\u{202e}u",
        "malicious",
        "tool",
        "approval",
    ));
    assert_eq!(employee_status(e, &m, &read), EmployeeStatus::Idle);
}
#[test]
fn mark_read_is_local_and_keys_keep_list_focus() {
    let m = populated();
    let mut a = App::default();
    a.refresh(&m);
    a.key(ordinary_key('I'), &m);
    assert_eq!(
        a.key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE), &m),
        Action::Save
    );
    assert!(a.read.contains("approval:a1"));
    assert_eq!(m.inbox.items.len(), 4);
    assert_eq!(
        employee_status(&m.employees.items[0], &m, &a.read),
        EmployeeStatus::Awaiting
    );
    a.selected = Some(Selection::Employee("done".into()));
    a.key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE), &m);
    assert_eq!(
        employee_status(&m.employees.items[2], &m, &a.read),
        EmployeeStatus::Idle
    );
    a.key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE), &m);
    assert!(a.help);
    a.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &m);
    assert!(a.detail_open);
    assert!(!a.help);
    assert_eq!(
        a.key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE), &m),
        Action::Quit
    );
}
#[test]
fn json_preserves_raw_data_while_text_is_safe() {
    let mut m = empty();
    let hostile = "確認\x1b[2K\u{202e}\n次行";
    m.inbox
        .items
        .push(approval("x", "adu", hostile, "tool", "question"));
    let v = json_snapshot(&m, &ReadState::default());
    let encoded = serde_json::to_string(&v).unwrap();
    assert!(!encoded.contains('\x1b'));
    assert_eq!(encoded.lines().count(), 1);
    assert_eq!(v["items"][0]["data"]["summary"], hostile);
    let text = text_snapshot(&m, &ReadState::default());
    assert!(!text.contains('\x1b'));
    assert!(!text.contains('\u{202e}'));
    m.invalidate_all();
    let stale = json_snapshot(&m, &ReadState::default());
    assert_eq!(stale["items"].as_array().unwrap().len(), 1);
    assert_eq!(stale["stale"], true);
    assert!(stale["count"].is_null());
}
#[test]
fn narrow_and_tiny_do_not_panic() {
    for (w, h) in [(1, 1), (24, 12), (60, 12)] {
        let _ = screen(&populated(), &App::default(), &Settings::test_defaults(), w, h);
    }
}

#[test]
fn completed_history_survives_restart_and_read_receipt_roundtrip() {
    let mut model = empty();
    model.employees = fresh(vec![employee("adu", "阿嘟", "active")]);
    model.recent_activity.insert(
        "adu".into(),
        Activity {
            completed_id: Some("run:adu:r1".into()),
            ..Default::default()
        },
    );
    let mut app = App::default();
    app.refresh(&model);
    assert_eq!(
        employee_status(&model.employees.items[0], &model, &app.read),
        EmployeeStatus::Done
    );
    assert_eq!(
        app.key(
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
            &model
        ),
        Action::Save
    );
    let home = tempfile::tempdir().unwrap();
    app.read.save(home.path()).unwrap();
    let (read, notice) = ReadState::load(home.path());
    assert!(notice.is_none());
    assert_eq!(
        employee_status(&model.employees.items[0], &model, &read),
        EmployeeStatus::Idle
    );
}
#[test]
fn configuration_and_corrupt_state_notices_reach_status_line() {
    let model = empty();
    let mut app = App::default();
    app.notice = Some(format!(
        "{} / {}",
        strings::SETTINGS_NOTICE,
        strings::READ_STATE_NOTICE
    ));
    let output = screen(&model, &app, &Settings::test_defaults(), 100, 30);
    assert!(output.contains(strings::SETTINGS_NOTICE));
    assert!(output.contains(strings::READ_STATE_NOTICE));
}
#[test]
fn loss_discards_live_hints_and_keeps_counts_unknown() {
    use crate::client::ServerEvent;
    let mut model = populated();
    model.activity.insert(
        "worker".into(),
        Activity {
            working: true,
            ..Default::default()
        },
    );
    model.event(&ServerEvent {
        name: "events_lost".into(),
        payload: json!({}),
        seq: Some(3),
        boot_id: None,
    });
    assert!(model.activity.is_empty());
    assert!(model.employees.count().is_none());
}

#[test]
fn wide_glyph_refresh_preserves_visible_columns() {
    let settings = Settings {
        glyphs: Glyphs::Unicode,
        ambiguous_width: AmbiguousWidth::Wide,
        ..Default::default()
    };
    let mut model = empty();
    model.employees = fresh(vec![employee("adu", "Ωα中", "working")]);
    let mut app = App::default();
    app.refresh(&model);
    let home = TerminalText::new("home".into());
    let mut reused = Terminal::new(TestBackend::new(60, 25)).unwrap();
    reused
        .draw(|f| render::draw(f, &model, &connection(), &home, &mut app, &settings))
        .unwrap();
    model.employees.items[0] = employee("adu", "甲", "active");
    reused
        .draw(|f| render::draw(f, &model, &connection(), &home, &mut app, &settings))
        .unwrap();
    let mut fresh = Terminal::new(TestBackend::new(60, 25)).unwrap();
    fresh
        .draw(|f| render::draw(f, &model, &connection(), &home, &mut app, &settings))
        .unwrap();
    // TestBackend stores only emitted cells and does not emulate terminal
    // erasure underneath wide glyphs. Compare visible cells, skipping their
    // covered columns (which may retain an intentionally undrawn old symbol).
    for y in 0..25 {
        let mut x = 0;
        while x < 60 {
            let expected = &fresh.backend().buffer()[(x, y)];
            assert_eq!(
                &reused.backend().buffer()[(x, y)],
                expected,
                "visible cell {x},{y}"
            );
            x += expected.cell_width().max(1);
        }
    }
}

#[test]
fn focus_specific_footer_snapshots() {
    let m=populated(); let mut app=App::default(); app.refresh(&m); app.help=true;
    insta::assert_snapshot!("help_footer",screen(&m,&app,&Settings::test_defaults(),100,36));
    app.help=false;
    app.card=Some(super::card::Card::open(m.inbox.items[0].clone(),&m,std::time::Instant::now(),&mut super::card::Codes::default()));
    insta::assert_snapshot!("card_footer",screen(&m,&app,&Settings::test_defaults(),100,36));
}
#[test]
fn selected_last_row_falls_back_after_decision_and_empty_list_recovers() {
    let mut m=empty(); m.inbox.items=vec![approval("a","adu","a","tool","approval"),approval("b","adu","b","tool","approval")];
    let mut app=App::default(); app.refresh(&m); app.key(ordinary_key('I'), &m); app.key(KeyEvent::new(KeyCode::Down,KeyModifiers::NONE),&m);
    app.decision_finished(crate::decide::Outcome::Refetch); m.inbox.items.pop(); app.refresh(&m);
    assert_eq!(app.selected,Some(Selection::Inbox("approval:a".into())));
    m.inbox.items.clear(); app.refresh(&m); assert_eq!(app.selected,Some(Selection::InboxEntry));
    m.inbox.items.push(approval("c","adu","c","tool","approval")); app.refresh(&m);
    assert_eq!(app.selected,Some(Selection::InboxEntry));
    app.key(KeyEvent::new(KeyCode::Enter,KeyModifiers::NONE), &m);
    assert_eq!(app.selected,Some(Selection::Inbox("approval:c".into())));
}

#[test]
fn input_focus_ime_paste_and_list_actions() {
    use crate::chat::{Focus, Block};
    let model = populated();
    let mut app = App::default();
    app.selected = Some(Selection::Employee("adu".into()));
    app.chat.connected = true;
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    assert_eq!(app.key(key(KeyCode::Char('i')), &model), Action::None);
    assert_eq!(app.focus, Focus::Input);
    assert_eq!(app.blocks.active, Block::Chat);
    for c in "你好qrs12".chars() { assert_eq!(app.key(key(KeyCode::Char(c)), &model), Action::None); }
    app.paste("注音提交🙂");
    assert_eq!(app.input, "你好qrs12注音提交🙂");
    app.key(key(KeyCode::Backspace), &model);
    assert_eq!(app.input, "你好qrs12注音提交");
    let Action::Chat(sent) = app.key(key(KeyCode::Enter), &model) else { panic!("expected send"); };
    assert_eq!(sent.agent, "adu"); assert_eq!(sent.content, app.input);
    app.input.clear();
    app.key(key(KeyCode::Esc), &model);
    assert_eq!(app.focus, Focus::List);
    app.key(key(KeyCode::Char('1')), &model);
    assert_eq!(app.blocks.active, Block::Chat);
    app.input.clear();
    app.key(key(KeyCode::Char('/')), &model);
    app.paste("pause");
    assert_eq!(crate::chat::command_note(&app.input), Some(strings::COMPUTER_COMMAND_NOTE));
    app.refresh(&empty());
    assert_eq!(app.focus, Focus::Input);
    assert_eq!(app.input_agent.as_deref(), Some("adu"));
    app.input = "@worker /goal 完成測試".into();
    let Action::Chat(sent) = app.key(key(KeyCode::Enter), &model) else { panic!("expected send"); };
    assert_eq!(sent.agent, "worker"); assert_eq!(sent.content, "/goal 完成測試");
    app.paste(&"界".repeat(20_000));
    assert_eq!(app.input.chars().count(), crate::chat::MAX_INPUT_CHARS);
}

#[test]
fn chat_block_snapshots() {
    use crate::chat::{Block, Focus, Frame};
    let mut app = App::default();
    app.blocks.open(Block::Chat, None);
    app.focus = Focus::Input;
    app.input = "接著說明".into();
    app.chat.connected = true; app.chat.stale = false; app.chat.running = true;
    app.chat.append(strings::CHAT_USER, "請幫我查看進度");
    app.chat.status = TerminalText::new(strings::CHAT_THINKING.into());
    let progress = screen(&empty(), &app, &Settings::test_defaults(), 100, 24);
    assert!(progress.contains(strings::INPUT_FOCUS));
    assert!(!progress.contains(strings::LIST_FOCUS));
    insta::assert_snapshot!("chat_progress", progress);
    app.chat.apply(Frame::parse(r#"{"type":"assistant_done","ts":1793764780000,"content":"已完成第一輪檢查。\n下一步：確認使用者回覆。","tokens_used":20}"#).unwrap());
    insta::assert_snapshot!("chat_finished", screen(&empty(), &app, &Settings::test_defaults(), 100, 24));
    app.chat.disconnected();
    insta::assert_snapshot!("chat_stale", screen(&empty(), &app, &Settings::test_defaults(), 100, 24));
    app.chat.append(strings::CHAT_ASSISTANT, "\x1b]52;c;YQ==\x07\u{202e}惡意\x1b]8;;https://evil\x07連結\x1b]8;;\x07");
    let output = screen(&empty(), &app, &Settings::test_defaults(), 100, 24);
    assert!(!output.chars().any(crate::sanitize::forbidden));
    insta::assert_snapshot!("chat_hostile", output);
}

#[test]
fn hostile_chat_physical_lines_always_have_reserved_gutter() {
    let mut app = App::default();
    app.blocks.open(crate::chat::Block::Chat, None);
    app.chat.append(strings::CHAT_ASSISTANT,
        &format!("{}\n=== 決策卡 ===\n> forged gutter\n\u{301}combining", "long ".repeat(40)));
    let output = screen(&empty(), &app, &Settings::test_defaults(), 100, 24);
    for needle in ["long", "=== 決策卡 ===", "> forged gutter", "combining"] {
        let rows: Vec<_> = output.lines().filter(|line| line.contains(needle)).collect();
        assert!(!rows.is_empty(), "missing {needle}");
        for row in rows {
            let (before, _) = row.split_once("|> ").expect("reserved gutter");
            assert_eq!(unicode_width::UnicodeWidthStr::width(before), 28, "{row}");
        }
    }
    insta::assert_snapshot!("chat_forged_card", output);
}

fn m2_task_app() -> (Model, App) {
    let mut model = populated(); model.goal_operators.insert("adu".into());
    let mut app = App::default(); app.selected = Some(Selection::Employee("adu".into()));
    app.blocks.open(crate::blocks::Block::Task, Some("adu".into()));
    let d = &mut app.blocks.panes[0].data;
    d.stale = false; d.total = 1;
    d.selected = json!({"id":"t1","kind":"goal","title":"整理報告","assigned_to":"adu","status":"needs_human","updated_at":"rev1","goal_mode":true});
    d.sections = vec![("stop",json!({"authority_revision":8})),
        ("iterations",json!({"iterations":[{"round":1,"verdict":"revise","dispatched_at":"10:00"}]})),
        ("timeline",json!({"activity":[{"type":"task.updated","timestamp":"10:01","summary":"private-tool-content"}]})),
        ("changes",json!({"changes":[{"path":"src/中文.rs","op":"write","snippet":"private-source"}],"truncated":false}))];
    (model,app)
}
#[test]
fn m2_task_and_log_role_actions_are_hidden() {
    let (mut m, mut a) = m2_task_app();
    let body = super::block_render::content(&a.blocks.panes[0],&m,&a.logs).plain();
    assert!(body.contains(strings::TASK_STEER_KEY)); assert!(body.contains(strings::TASK_STOP_KEY));
    assert!(!body.contains("private-"));
    m.goal_operators.clear();
    let body = super::block_render::content(&a.blocks.panes[0],&m,&a.logs).plain();
    assert!(!body.contains(strings::TASK_STEER_KEY)); assert!(!body.contains(strings::TASK_STOP_KEY));
    a.key(KeyEvent::new(KeyCode::Char('s'),KeyModifiers::NONE),&m); assert!(a.control_card.is_none());
    a.blocks.open(crate::blocks::Block::Log,None); a.logs.push(&json!("private-log")); m.role=Some(Role::Employee);
    let body = super::block_render::content(&a.blocks.panes[0],&m,&a.logs).plain();
    assert!(body.contains(strings::MANAGER_ONLY)); assert!(!body.contains("private-log"));
}
#[test]
fn m2_stop_confirmation_waits_for_viewport_time_and_authoritative_refresh() {
    let (m, mut a) = m2_task_app(); let now=std::time::Instant::now();
    let key=|c| KeyEvent::new(KeyCode::Char(c),KeyModifiers::NONE);
    a.key_at(key('s'),&m,now,0);
    let code = a.control_card.as_ref().unwrap().code.clone();
    for c in code.chars() { a.key_at(key(c),&m,now,0); }
    assert!(a.control_card.as_ref().unwrap().input.is_empty());
    let later=now+std::time::Duration::from_secs(2);
    for c in code.chars() { a.key_at(key(c),&m,later,0); }
    assert!(a.control_card.as_ref().unwrap().input.is_empty(),"not rendered yet");
    a.control_card.as_mut().unwrap().viewport(4,8);
    for c in code.chars() { a.key_at(key(c),&m,later,0); }
    let action=a.key_at(KeyEvent::new(KeyCode::Enter,KeyModifiers::NONE),&m,later,0);
    assert_eq!(action,Action::Control(crate::controls::Control::Stop{task:"t1".into(),revision:8}));
    assert!(a.blocks.panes[0].data.stale); assert!(a.waiting_refresh.is_some());
    assert_eq!(a.blocks.panes[0].data.selected["status"],"needs_human");
    a.decision_finished(crate::decide::Outcome::Refetch);
    assert!(a.waiting_refresh.is_some()); a.refresh(&m); assert!(a.waiting_refresh.is_some());
}
#[test]
fn m2_card_changes_or_revoked_binding_refuse_control() {
    for changed in [true,false] {
        let (mut m,mut a)=m2_task_app(); let now=std::time::Instant::now();
        a.key_at(KeyEvent::new(KeyCode::Char('s'),KeyModifiers::NONE),&m,now,0);
        let code=a.control_card.as_ref().unwrap().code.clone();
        if changed { a.blocks.panes[0].data.sections[0].1["authority_revision"]=json!(9); } else { m.goal_operators.clear(); }
        a.refresh(&m); a.control_card.as_mut().unwrap().viewport(4,8);
        for c in code.chars() { assert_eq!(a.key_at(KeyEvent::new(KeyCode::Char(c),KeyModifiers::NONE),&m,now+std::time::Duration::from_secs(2),0),Action::None); }
        assert_eq!(a.key_at(KeyEvent::new(KeyCode::Enter,KeyModifiers::NONE),&m,now+std::time::Duration::from_secs(2),0),Action::None);
        assert!(a.waiting_refresh.is_none());
    }
}
#[test]
fn m2_goal_card_uses_task_block_revision_and_blocks_changes() {
    let (m,mut a)=m2_task_app(); let now=std::time::Instant::now();
    a.key_at(KeyEvent::new(KeyCode::Enter,KeyModifiers::NONE),&m,now,0); a.refresh(&m);
    assert!(a.card.as_ref().unwrap().blocked.is_none());
    a.card.as_mut().unwrap().viewport(5,20);
    let action=a.key_at(KeyEvent::new(KeyCode::Char('r'),KeyModifiers::NONE),&m,now+std::time::Duration::from_secs(2),0);
    assert!(matches!(action,Action::Decide(crate::decide::Decision::Goal { expected_updated_at, .. }) if expected_updated_at == "rev1"));
}
#[test]
fn m2_content_warning_is_explicit_and_default_hides_previews() {
    let m=empty(); let mut a=App::default(); a.blocks.open(crate::blocks::Block::Run,Some("adu".into()));
    let p=&mut a.blocks.panes[0]; p.data.stale=false;
    p.data.sections.push(("events",json!([{"kind":"tool_step","preview":"private-key","label":"private-label"}])));
    assert!(!super::block_render::content(p,&m,&a.logs).plain().contains("private-"));
    let key=KeyEvent::new(KeyCode::Char('c'),KeyModifiers::NONE);
    a.key(key,&m); assert!(!a.blocks.panes[0].content); assert!(a.content_warning);
    a.key(KeyEvent::new(KeyCode::Esc,KeyModifiers::NONE),&m); assert!(!a.blocks.panes[0].content);
    a.key(key,&m); a.key(key,&m); assert!(a.blocks.panes[0].content);
    assert!(super::block_render::content(&a.blocks.panes[0],&m,&a.logs).plain().contains("private-key"));
    a.key(key,&m); assert!(!a.blocks.panes[0].content);
}
#[test]
fn m2_blocks_and_four_way_snapshots_in_both_width_modes() {
    use crate::blocks::{Block,Split};
    let (m,base)=m2_task_app();
    for (name,width) in [("narrow",AmbiguousWidth::Narrow),("wide",AmbiguousWidth::Wide)] {
        let settings=Settings { ambiguous_width:width,..Settings::test_defaults() };
        for kind in [Block::Detail,Block::Chat,Block::Run,Block::Task,Block::Log] {
            let mut a=base.clone();
            if kind!=Block::Task { a.blocks.open(kind,Some("adu".into())); }
            a.blocks.panes[0].data.stale=false;
            if kind==Block::Run {
                let d=&mut a.blocks.panes[0].data; d.total=1;
                d.runtime=Some(TerminalText::new("codex".into()));
                d.selected=json!({"started_at":"10:00","agent_id":"adu","status":"completed"});
                d.sections.push(("events",json!([{"kind":"text","ts":"10:01","preview":"private-content"}])));
            }
            a.logs.push(&json!({"level":"INFO","message":"中文 · safe\u{1b}"})); a.detail_open=true;
            let output=screen(&m,&a,&settings,140,36);
            assert!(!output.contains("private-content")); assert!(!output.contains('\u{1b}'));
            insta::assert_snapshot!(format!("m2_{kind:?}_{name}"),output);
        }
        let mut a=base.clone();
        for kind in [Block::Run,Block::Chat,Block::Log] { a.blocks.add(Split::Vertical); a.blocks.open(kind,Some("adu".into())); }
        insta::assert_snapshot!(format!("m2_four_{name}"),screen(&m,&a,&settings,180,48));
        let narrow=screen(&m,&a,&settings,60,24);
        assert!(narrow.contains("3 個區塊"));
        insta::assert_snapshot!(format!("m2_four_compact_{name}"),narrow);
    }
}
#[test]
fn m2_steering_edit_does_not_send_before_second_confirmation_and_resize() {
    let (m,mut a)=m2_task_app(); let now=std::time::Instant::now(); let later=now+std::time::Duration::from_secs(2);
    a.key_at(KeyEvent::new(KeyCode::Char('e'),KeyModifiers::NONE),&m,now,0);
    a.paste("先檢查中文\u{1b}\u{202e}");
    let card=a.control_card.as_ref().unwrap(); assert!(card.editing);
    assert!(matches!(&card.control,crate::controls::Control::Steer { body,.. } if body=="先檢查中文"));
    a.key_at(KeyEvent::new(KeyCode::Enter,KeyModifiers::NONE),&m,later,0);
    assert!(!a.control_card.as_ref().unwrap().editing); assert!(a.waiting_refresh.is_none());
    a.control_card.as_mut().unwrap().viewport(4,8);
    let code=a.control_card.as_ref().unwrap().code.clone();
    for c in code.chars() { a.key_at(KeyEvent::new(KeyCode::Char(c),KeyModifiers::NONE),&m,later+std::time::Duration::from_secs(2),0); }
    a.control_card.as_mut().unwrap().resize();
    assert_eq!(a.key_at(KeyEvent::new(KeyCode::Enter,KeyModifiers::NONE),&m,later+std::time::Duration::from_secs(2),0),Action::None);
    assert!(a.waiting_refresh.is_none());
}
#[test]
fn m2_responsibility_open_occurrence_uses_server_task_id() {
    let (m,mut a)=m2_task_app();
    let p=&mut a.blocks.panes[0]; p.responsibilities=true;
    p.data.sections.push(("summary",json!({"summary":{"open_occurrence":{"task_id":"occurrence-42"}}})));
    a.key(KeyEvent::new(KeyCode::Char('o'),KeyModifiers::NONE),&m);
    assert_eq!(a.blocks.panes[0].task_id.as_deref(),Some("occurrence-42"));
    assert!(!a.blocks.panes[0].responsibilities); assert!(a.blocks.panes[0].data.stale);
}
#[test]
fn m2_responsibility_and_confirmation_snapshots() {
    use crate::controls::Control;
    for (name,width) in [("narrow",AmbiguousWidth::Narrow),("wide",AmbiguousWidth::Wide)] {
        let (m,mut a)=m2_task_app(); let settings=Settings { ambiguous_width:width,..Settings::test_defaults() };
        let p=&mut a.blocks.panes[0]; p.responsibilities=true;
        p.data.selected=json!({"responsibility_id":"resp-1","objective":"每週整理週報","owner_agent_id":"adu","state":"active","control_epoch":3,"stop_at":"2026-11-30"});
        p.data.sections=vec![("summary",json!({"summary":{"next_due_at":"11:00","open_occurrence":{"task_id":"task-1"}}})),
            ("fires",json!({"fires":[{"observed_at":"10:00","state":"consumed","reason":"schedule"}]}))];
        insta::assert_snapshot!(format!("m2_responsibility_{name}"),screen(&m,&a,&settings,140,36));
        let fingerprint=a.control_subject(&super::control_card::Source::Pane(0),&m).unwrap();
        a.control_card=Some(super::control_card::ControlCard::new(Control::Responsibility { id:"resp-1".into(),epoch:3,disable:false },
            super::control_card::Source::Pane(0),fingerprint,"42".into(),std::time::Instant::now()-std::time::Duration::from_secs(2)));
        insta::assert_snapshot!(format!("m2_confirmation_{name}"),screen(&m,&a,&settings,140,36));
    }
}

#[test]
fn m3_palette_filters_roles_focus_and_current_binding() {
    use crate::{blocks::Block, chat::Focus};
    let (mut model, mut app) = m2_task_app();
    let has = |a: &App, m: &Model, code| a.commands(m).iter().any(|c| c.key.code == code && c.key.modifiers.is_empty());
    assert!(has(&app, &model, KeyCode::Char('e')));
    assert!(has(&app, &model, KeyCode::Char('s')));
    assert!(has(&app, &model, KeyCode::Char('L')));
    model.role = Some(Role::Employee); model.goal_operators.clear();
    for c in ['e', 's', 'L', 'p', 'n', 'd'] { assert!(!has(&app, &model, KeyCode::Char(c)), "{c}"); }
    app.blocks.open(Block::Run, Some("adu".into()));
    assert!(has(&app, &model, KeyCode::Char('c')));
    assert!(!has(&app, &model, KeyCode::Char('b')));
    app.focus = Focus::Input;
    app.chat.connected = true; app.input_agent = Some("adu".into()); app.input = "你好".into();
    assert!(has(&app, &model, KeyCode::Enter));
    assert!(!has(&app, &model, KeyCode::Char('c')));
    assert!(!has(&app, &model, KeyCode::Char('s')));
    app.chat.running = true;
    assert!(!has(&app, &model, KeyCode::Enter));
}

#[test]
fn m3_palette_dispatch_cannot_bypass_stop_confirmation_or_changed_role() {
    let (mut model, mut app) = m2_task_app();
    let now = std::time::Instant::now();
    let open = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL);
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.key_at(open, &model, now, 0), Action::None);
    app.paste("stop");
    assert_eq!(app.palette_commands(&model).len(), 1);
    assert_eq!(app.key_at(enter, &model, now, 0), Action::None);
    assert!(app.palette.is_none()); assert!(app.control_card.is_some());
    assert!(app.waiting_refresh.is_none());
    assert_eq!(app.key_at(open, &model, now, 0), Action::None);
    assert!(app.palette.is_none(), "card owns keyboard");
    let later = now + std::time::Duration::from_secs(2);
    assert_eq!(app.key_at(enter, &model, later, 0), Action::None);
    assert!(app.waiting_refresh.is_none(), "no code or viewport");
    app.control_card = None;
    app.key_at(open, &model, now, 0); app.paste("stop");
    model.goal_operators.clear();
    assert_eq!(app.key_at(enter, &model, now, 0), Action::None);
    assert!(app.control_card.is_none()); assert!(app.waiting_refresh.is_none());
}

#[test]
fn m3_palette_fuzzy_search_ime_and_key_event_kinds() {
    let (model, mut app) = m2_task_app();
    let open = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL);
    app.key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL), &model);
    assert!(app.palette.is_none(), "Ctrl-K is reserved");
    app.key(open, &model); app.paste("STP");
    assert_eq!(app.palette_commands(&model).len(), 1);
    let mut enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    enter.kind = crossterm::event::KeyEventKind::Release;
    assert_eq!(app.key(enter, &model), Action::None);
    assert!(app.palette.is_some()); assert!(app.control_card.is_none());
    app.palette.as_mut().unwrap().query.clear(); app.paste("調方向");
    assert_eq!(app.palette_commands(&model)[0].label, strings::CMD_STEER);
    app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &model);
    assert!(app.palette.is_none());
    app.focus = crate::chat::Focus::Input; app.input = "草稿".into();
    app.key(open, &model); app.paste("查詢");
    assert_eq!(app.input, "草稿");
    app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &model);
    assert_eq!(app.focus, crate::chat::Focus::Input);
}

#[test]
fn m3_help_only_covers_focused_block_and_keeps_chat() {
    use crate::blocks::{Block, Split};
    let model = populated(); let mut app = App::default();
    app.selected = Some(Selection::Employee("adu".into())); app.detail_open = true;
    app.blocks.add(Split::Vertical);
    app.blocks.add(Split::Horizontal); app.blocks.open(Block::Chat, None);
    app.chat.append(strings::CHAT_ASSISTANT, "chat-kept-visible");
    // Use the operator path so the departing chat is saved into its pane.
    app.key(ordinary_key('1'), &model); app.help = true;
    let output = screen(&model, &app, &Settings::test_defaults(), 180, 48);
    assert_eq!(output.matches("操作說明").count(), 1);
    assert!(output.contains("chat-kept-visible"));
    assert!(output.contains("目前身分沒有此決策權限"));
    insta::assert_snapshot!("m3_help_focused_only", output);
    app.key(ordinary_key('?'), &model);
    app.key(ordinary_key('3'), &model);
    assert_eq!(app.blocks.focused, 2);
    app.key(ordinary_key('?'), &model);
    let output = screen(&model, &app, &Settings::test_defaults(), 180, 48);
    assert_eq!(output.matches("操作說明").count(), 1);
    assert!(!output.contains("chat-kept-visible"));
    insta::assert_snapshot!("m3_help_chat_focused", output);
}

#[test]
fn m3_small_height_keeps_inbox_and_employee_rows() {
    let model = populated(); let app = App::default();
    for height in [12, 13, 14, 16] {
        let mut app = app.clone();
        app.notice = Some(strings::SETTINGS_NOTICE.into());
        let output = screen(&model, &app, &Settings::test_defaults(), 100, height);
        assert!(output.contains("確認資料匯出"), "inbox row missing at {height}");
        assert!(output.contains("阿嘟"), "employee row missing at {height}");
        insta::assert_snapshot!(format!("m3_small_height_{height}"), output);
    }
}

#[test]
fn m3_long_home_keeps_tail_in_both_width_modes() {
    for width in [AmbiguousWidth::Narrow, AmbiguousWidth::Wide] {
        let settings = Settings { ambiguous_width: width, ..Default::default() };
        let model = empty(); let mut app = App::default();
        let home = TerminalText::new(format!("/Users/{}/最後目錄", "very-long-home-".repeat(20)));
        let mut terminal = Terminal::new(TestBackend::new(60, 24)).unwrap();
        terminal.draw(|f| render::draw(f, &model, &connection(), &home, &mut app, &settings)).unwrap();
        let buffer = terminal.backend().buffer();
        let output = (0..24).map(|y| {
            let mut row = String::new(); let mut x = 0;
            while x < 60 { let cell = &buffer[(x,y)]; row.push_str(cell.symbol()); x += cell.cell_width().max(1); }
            assert_eq!(super::text::columns(&row, width), 60);
            row.trim_end().to_string()
        }).collect::<Vec<_>>().join("\n");
        assert!(app.help_text().plain().contains(&home.plain()));
        assert!(output.lines().next().unwrap().contains(strings::CONNECTED));
        // The single-line header may shorten the basename on very narrow screens.
        assert!(output.contains('…'));
        insta::assert_snapshot!(format!("m3_long_home_{width:?}"), output);
    }
}

#[test]
fn m3_palette_snapshot() {
    let (model, mut app) = m2_task_app();
    app.key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL), &model);
    app.paste("st");
    let output = screen(&model, &app, &Settings::test_defaults(), 100, 30);
    assert!(output.contains("[s]")); assert!(output.contains("停止任務"));
    assert!(output.contains("[e]")); assert!(output.contains("調整任務方向"));
    insta::assert_snapshot!("m3_palette", output);
}

#[test]
fn m3_empty_task_palette_has_no_enter_command() {
    let (model, mut app) = m2_task_app();
    app.blocks.panes[0].data = crate::blocks::Data::default();
    assert!(!app.commands(&model).iter().any(|c| c.key.code == KeyCode::Enter));
}

#[cfg(unix)]
#[test]
fn m4_pty_focus_owns_escape_ctrl_c_palette_and_close_confirmation() {
    let model=empty(); let mut app=App::default();
    assert_eq!(app.key(KeyEvent::new(KeyCode::Char('P'),KeyModifiers::NONE),&model),Action::None);
    assert_eq!(app.blocks.active,crate::blocks::Block::Pty); assert!(app.pty_focus);
    for key in [KeyEvent::new(KeyCode::Esc,KeyModifiers::NONE),KeyEvent::new(KeyCode::Char('c'),KeyModifiers::CONTROL),KeyEvent::new(KeyCode::Char('p'),KeyModifiers::CONTROL),KeyEvent::new(KeyCode::Char('q'),KeyModifiers::NONE)] {
        assert_eq!(app.key(key,&model),Action::None); assert!(app.pty_focus); assert!(app.palette.is_none());
    }
    app.key(KeyEvent::new(KeyCode::Char('\\'),KeyModifiers::CONTROL),&model);
    assert!(!app.pty_focus);
    app.key(KeyEvent::new(KeyCode::Char('w'),KeyModifiers::CONTROL),&model);
    assert!(app.pty_confirm.is_some()); assert_eq!(app.blocks.active,crate::blocks::Block::Pty);
    app.paste("y"); assert!(app.pty_confirm.is_some());
    app.key(KeyEvent::new(KeyCode::Esc,KeyModifiers::NONE),&model);
    assert!(app.pty_confirm.is_none());
    // Replacing a running/pending PTY is also a close operation.
    app.key(KeyEvent::new(KeyCode::Char('D'),KeyModifiers::NONE),&model);
    assert!(app.pty_confirm.is_some());
    app.key(KeyEvent::new(KeyCode::Char('y'),KeyModifiers::NONE),&model);
    assert_eq!(app.blocks.active,crate::blocks::Block::Detail);
}

#[test]
fn m4_pty_cjk_and_combining_snapshots_in_both_width_modes() {
    let mut app=App::default(); app.blocks.open(crate::blocks::Block::Pty,None); app.pty_focus=true;
    {
        let mut pty=app.blocks.panes[0].pty.as_ref().unwrap().lock().unwrap();
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process("繁體中文 cafe\u{301}\r\n\x1b[1;32m彩色粗體\x1b[0m\r\n·αΩ end".as_bytes());
        pty.set_screen_for_test(parser.screen());
        pty.status=crate::strings::PTY_RUNNING.into();
    }
    for width in [AmbiguousWidth::Narrow,AmbiguousWidth::Wide] {
        let settings=Settings { ambiguous_width:width,..Settings::test_defaults() };
        insta::assert_snapshot!(format!("m4_pty_cjk_{width:?}"),screen(&empty(),&app,&settings,100,24));
    }
}


#[cfg(not(unix))]
#[test]
fn m4_unsupported_platform_never_enters_child_input() {
    let mut app = App::default();
    app.key(KeyEvent::new(KeyCode::Char('P'), KeyModifiers::NONE), &empty());
    assert!(!app.pty_focus);
    let mut session = app.blocks.panes[0].pty.as_ref().unwrap().lock().unwrap();
    session.start();
    assert_eq!(session.status, strings::PTY_DISABLED);
    assert!(!session.pending_or_running());
}

// Run the public crossterm event reader in a fresh process so its global input
// reader/raw-mode state cannot interfere with the parallel library tests.
#[cfg(unix)]
#[test]
fn m4_real_crossterm_parser_probe() {
    use std::{io::Write, time::Duration};
    if std::env::var_os("TUI_RAW_PARSER_PROBE").is_none() { return; }
    crossterm::terminal::enable_raw_mode().unwrap();
    println!("PARSER_READY");
    std::io::stdout().flush().unwrap();
    let model = empty();
    let mut app = App::default();
    app.blocks.open(crate::blocks::Block::Pty, None);
    let read_key = || {
        assert!(crossterm::event::poll(Duration::from_secs(5)).unwrap());
        let crossterm::event::Event::Key(key) = crossterm::event::read().unwrap() else {
            panic!("expected parsed key");
        };
        key
    };
    // Raw 0x1c, CSI-u backslash, CSI-u raw control char, then F12.
    for expected in [
        KeyEvent::new(KeyCode::Char('4'), KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Char('\u{1c}'), KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char('\u{1c}'), KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE),
    ] {
        let key = read_key();
        assert_eq!(key, expected);
        app.pty_focus = true;
        assert_eq!(app.key(key, &model), Action::None);
        assert!(!app.pty_focus, "escape must leave terminal focus: {key:?}");
    }
    // Verify the other app control bindings from their actual legacy bytes.
    let close = read_key();
    assert_eq!(close, KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
    assert_eq!(crate::pty::key_bytes(close, false), b"\x17");
    app.key(close, &model);
    assert!(app.pty_confirm.is_some());
    app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &model);
    let palette = read_key();
    assert_eq!(palette, KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
    assert_eq!(crate::pty::key_bytes(palette, false), b"\x10");
    app.key(palette, &model);
    assert!(app.palette.is_some());
    crossterm::terminal::disable_raw_mode().unwrap();
}

#[cfg(unix)]
#[test]
fn m4_escape_and_control_bindings_through_real_terminal_parser() {
    use std::{io::{Read, Write}, sync::mpsc, time::Duration};
    let pair = portable_pty::native_pty_system().openpty(portable_pty::PtySize {
        rows: 24, cols: 80, pixel_width: 0, pixel_height: 0,
    }).unwrap();
    let mut reader = pair.master.try_clone_reader().unwrap();
    let mut writer = pair.master.take_writer().unwrap();
    let mut cmd = portable_pty::CommandBuilder::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", "ui::tests::m4_real_crossterm_parser_probe", "--nocapture"]);
    cmd.env("TUI_RAW_PARSER_PROBE", "1");
    let mut child = pair.slave.spawn_command(cmd).unwrap();
    drop(pair.slave);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = [0; 4096];
        while let Ok(n) = reader.read(&mut bytes) {
            if n == 0 || tx.send(bytes[..n].to_vec()).is_err() { break; }
        }
    });
    let mut output = Vec::new();
    let result = (|| {
        while !String::from_utf8_lossy(&output).contains("PARSER_READY") {
            output.extend(rx.recv_timeout(Duration::from_secs(5)).map_err(|e| e.to_string())?);
        }
        writer.write_all(b"\x1c\x1b[92;5u\x1b[28u\x1b[28;5u\x1b[24~\x17\x10").map_err(|e| e.to_string())?;
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                return if status.success() { Ok(()) } else { Err(format!("probe exit {status:?}")) };
            }
            if std::time::Instant::now() >= deadline { return Err("parser probe timeout".into()); }
            if let Ok(bytes) = rx.recv_timeout(Duration::from_millis(20)) { output.extend(bytes); }
        }
    })();
    if result.is_err() { let _ = child.kill(); }
    let _ = child.wait();
    assert!(result.is_ok(), "{result:?}: {}", String::from_utf8_lossy(&output));
}

#[test]
fn full_stalled_child_queue_still_reads_escape_resize_and_quit() {
    use std::{io::{self, Write}, sync::mpsc, time::{Duration, Instant}};
    struct Stalled { gate: mpsc::Receiver<()>, started: mpsc::Sender<()> }
    impl Write for Stalled {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let _ = self.started.send(());
            let _ = self.gate.recv_timeout(Duration::from_secs(5));
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }
    for escape in [KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE), KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::CONTROL)] {
        let (release, gate) = mpsc::channel();
        let (started, writing) = mpsc::channel();
        let mut app = App::default();
        app.blocks.open(crate::blocks::Block::Pty, None);
        app.pty_focus = true;
        let pty = app.blocks.panes[0].pty.as_ref().unwrap().clone();
        {
            let mut pty = pty.lock().unwrap();
            pty.set_writer_for_test(Stalled { gate, started });
            pty.send(vec![b'x'; crate::pty::MAX_INPUT]);
        }
        writing.recv_timeout(Duration::from_secs(5)).unwrap();
        let start = Instant::now();
        let events = [
            Event::Paste("refuse this entire chunk".into()),
            Event::Resize(100, 30),
            Event::Key(escape),
            Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
        ];
        let mut resized = false;
        let mut quit = false;
        for event in events {
            let event = super::read_host_event(&mut app, || Ok(Some(event))).unwrap().unwrap();
            match event {
                Event::Paste(text) => {
                    app.paste(&text);
                    assert_eq!(pty.lock().unwrap().status, strings::PTY_INPUT_FULL);
                }
                Event::Resize(_, _) => { resized = true; }
                Event::Key(key) => { quit |= app.key(key, &empty()) == Action::Quit; }
                _ => unreachable!(),
            }
        }
        assert!(resized && quit && !app.pty_focus);
        assert!(start.elapsed() < Duration::from_secs(1));
        release.send(()).unwrap();
    }
}


fn downstream_app(now: std::time::Instant) -> App {
    let mut app = App::default();
    app.blocks.open(crate::chat::Block::Chat, None);
    app.chat.connected = true;
    app.chat.stale = false;
    app.chat.running = true;
    app.chat.apply_at(crate::chat::Frame::parse(
        r#"{"type":"error","ts":1793764780000,"code":"cancelled","downstream_running":2,"downstream_ref":"opaque-never-render"}"#
    ).unwrap(), now);
    app
}

#[test]
fn downstream_answer_requires_list_focus_and_is_single_use() {
    use crate::chat::Focus;
    let now = std::time::Instant::now();
    let model = empty();
    let mut app = downstream_app(now);
    app.focus = Focus::Input;
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    for c in ['y', 'n'] {
        assert_eq!(app.key_at(key(KeyCode::Char(c)), &model, now, 0), Action::None);
    }
    assert_eq!(app.input, "yn");
    assert!(app.downstream_prompt(now).is_some());
    assert!(!app.commands(&model).iter().any(|c| c.key.code == KeyCode::Char('y')));
    app.key_at(key(KeyCode::Esc), &model, now, 0);
    assert_eq!(app.focus, Focus::List);
    assert!(app.commands(&model).iter().any(|c| c.key.code == KeyCode::Char('y')));
    assert_eq!(app.key_at(key(KeyCode::Char('y')), &model, now, 0), Action::CancelDownstream("opaque-never-render".into()));
    assert!(app.downstream_prompt(now).is_none());
    // A watch update can still contain the offer while the command is queued.
    app.chat = downstream_app(now).chat;
    assert_eq!(app.key_at(key(KeyCode::Char('y')), &model, now, 0), Action::None);
    assert!(app.downstream_prompt(now).is_none());
}

#[test]
fn downstream_decline_escape_expiry_and_disconnect_send_no_stop() {
    let now = std::time::Instant::now();
    let model = empty();
    for code in [KeyCode::Char('n'), KeyCode::Esc] {
        let mut app = downstream_app(now);
        assert_eq!(app.key_at(KeyEvent::new(code, KeyModifiers::NONE), &model, now, 0),
            Action::DismissDownstream("opaque-never-render".into()));
        assert!(app.downstream_prompt(now).is_none());
    }
    let mut app = downstream_app(now);
    let expired = now + crate::chat::DOWNSTREAM_TTL;
    assert_eq!(app.key_at(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE), &model, expired, 0), Action::None);
    assert!(app.downstream_prompt(expired).is_none());
    app.chat.disconnected();
    assert_eq!(app.key_at(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE), &model, now, 0), Action::None);
}

#[test]
fn downstream_prompt_snapshots_preserve_focus_and_hide_reference() {
    let now = std::time::Instant::now();
    let mut app = downstream_app(now);
    app.focus = crate::chat::Focus::Input;
    assert!(app.downstream_prompt(now).is_some());
    let output = screen(&empty(), &app, &Settings::test_defaults(), 240, 24);
    assert!(output.contains(strings::DOWNSTREAM_INPUT_HINT));
    assert!(output.contains(&strings::downstream_prompt(2)));
    assert!(!output.contains("opaque-never-render"));
    assert_eq!(app.focus, crate::chat::Focus::Input);
    insta::assert_snapshot!("downstream_prompt_input", output);
    app.focus = crate::chat::Focus::List;
    insta::assert_snapshot!("downstream_prompt_list", screen(&empty(), &app, &Settings::test_defaults(), 240, 24));
}

fn autonomy_model(value: serde_json::Value) -> Model {
    let mut m = empty();
    m.autonomy = fresh(vec![crate::autonomy::Status::parse(value).unwrap()]);
    m.employees = fresh(vec![employee("adu", "阿嘟", "active")]);
    m.goal_operators.insert("adu".into());
    m
}
fn pause_entry() -> serde_json::Value { json!({"reason":"operator", "who":"u1", "at":"2026-11-02T00:00:00Z"}) }
fn ordinary_key(c: char) -> KeyEvent { KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE) }

fn assert_autonomy_unconfirmed(m: &Model) {
    let read = ReadState::default();
    let mut without_approvals = m.clone();
    without_approvals.inbox = fresh(vec![]);
    for status in ["active", "working"] {
        assert_eq!(employee_status(&employee("adu", "阿嘟", status), &without_approvals, &read), EmployeeStatus::AutonomyUnknown);
    }
    let line = crate::autonomy::status_line(m).unwrap().plain();
    assert!(line.contains(strings::AUTONOMY_UNKNOWN));
    assert!(!line.contains(strings::AUTONOMY_PAUSED));
    assert!(!line.contains(strings::AUTONOMY_ALL));
    let output = screen(m, &App::default(), &Settings::test_defaults(), 180, 24);
    assert!(output.contains(strings::AUTONOMY_UNKNOWN));
    assert!(!output.contains(strings::AUTONOMY_ROW_PAUSED));
}

#[test]
fn autonomy_resume_then_invalidation_and_failed_refetch_hide_cached_pause() {
    let mut m = autonomy_model(json!({"global":null,"agents":{"adu":pause_entry()},"state_error":null}));
    assert_eq!(employee_status(&m.employees.items[0], &m, &ReadState::default()), EmployeeStatus::AutonomyPaused);
    m.event(&crate::client::ServerEvent {
        name: "autonomy.changed".into(),
        payload: json!({"agent_id":"adu","all":false,"paused":false}),
        seq: Some(1), boot_id: None,
    });
    assert!(m.autonomy.paused("adu"));
    assert_autonomy_unconfirmed(&m);
    // A failed status refetch retains the old items and invalidation flags.
    m.autonomy.error = Some(crate::Error::Network);
    assert_autonomy_unconfirmed(&m);
    m.inbox = fresh(vec![approval("pending", "adu", "review", "tool", "approval")]);
    assert_autonomy_unconfirmed(&m);
}

#[test]
fn autonomy_pause_then_invalidation_never_displays_confirmed_state() {
    for value in [
        json!({"global":null,"agents":{},"state_error":null}),
        json!({"global":null,"agents":{"adu":pause_entry()},"state_error":null}),
        json!({"global":pause_entry(),"agents":{},"state_error":null}),
    ] {
        let mut m = autonomy_model(value);
        m.event(&crate::client::ServerEvent {
            name: "autonomy.changed".into(),
            payload: json!({"agent_id":"adu","all":false,"paused":true}),
            seq: Some(1), boot_id: None,
        });
        assert_autonomy_unconfirmed(&m);
        // Either flag alone must hide cached pauses, including priority badges.
        for (stale, pending) in [(true, false), (false, true)] {
            m.autonomy.stale = stale;
            m.autonomy.refetch_pending = pending;
            for status in ["active", "awaiting", "error", "paused", "stopped"] {
                m.employees.items[0] = employee("adu", "阿嘟", status);
                assert_autonomy_unconfirmed(&m);
            }
        }
    }
}

#[test]
fn autonomy_status_line_snapshots_and_hostile_names() {
    let pause = pause_entry();
    for (name, value) in [
        ("one", json!({"global":null,"agents":{"adu":pause},"state_error":null})),
        ("several", json!({"global":null,"agents":{"adu":pause,"b":pause,"c":pause,"d":pause},"state_error":null})),
        ("all", json!({"global":pause,"agents":{},"state_error":null})),
        ("none", json!({"global":null,"agents":{},"state_error":null})),
        ("state_error", json!({"global":null,"agents":{},"state_error":"unreadable"})),
    ] {
        let m = autonomy_model(value);
        let rendered = screen(&m, &App::default(), &Settings::test_defaults(), 120, 16);
        insta::assert_snapshot!(format!("autonomy_{name}"), rendered.lines().find(|line| line.starts_with("[焦點：")).unwrap().trim_end());
    }
    let mut m = autonomy_model(json!({"global":null,"agents":{"adu":pause},"state_error":null}));
    m.autonomy.error = Some(crate::Error::Timeout); m.autonomy.stale = true;
    insta::assert_snapshot!("autonomy_unreadable", screen(&m,&App::default(),&Settings::test_defaults(),120,16).lines().find(|line| line.starts_with("[焦點：")).unwrap().trim_end());
    m.autonomy = Default::default();
    insta::assert_snapshot!("autonomy_unknown", screen(&m,&App::default(),&Settings::test_defaults(),120,16).lines().find(|line| line.starts_with("[焦點：")).unwrap().trim_end());
    m.autonomy = fresh(vec![crate::autonomy::Status::parse(json!({"global":null,"agents":{"adu":pause},"state_error":null})).unwrap()]);
    m.employees.items[0].display_name = TerminalText::new("阿\u{1b}[2K\u{202e}\u{200b}嘟\nhostile".into());
    let line = crate::autonomy::status_line(&m).unwrap().plain();
    assert!(!line.chars().any(crate::sanitize::forbidden));
    assert!(!line.contains('\n'));
    insta::assert_snapshot!("autonomy_hostile", line);
}

#[tokio::test]
async fn autonomy_keys_send_exact_requests_immediately_without_optimism() {
    use crate::{blocks::fetch::Gateway, client::RpcReply, controls::Control};
    struct Fake(std::sync::Mutex<Vec<(String, serde_json::Value)>>, bool);
    #[async_trait::async_trait]
    impl Gateway for Fake {
        async fn request(&self, method: &str, params: serde_json::Value) -> crate::Result<RpcReply> {
            self.0.lock().unwrap().push((method.into(), params.clone()));
            if self.1 { return Ok(RpcReply::Rejected { message: TerminalText::new("permission denied".into()), digest_mismatch: false }); }
            let paused = method == "autonomy.pause";
            Ok(RpcReply::Accepted(json!({"agent_id":params["agent_id"],"all":false,"paused":paused,
                "pause":if paused { pause_entry() } else { serde_json::Value::Null },"applied":true})))
        }
    }
    let m = autonomy_model(json!({"global":null,"agents":{},"state_error":null}));
    let gateway = Fake(Default::default(), false);
    for kind in [crate::blocks::Block::Detail, crate::blocks::Block::Run, crate::blocks::Block::Log, crate::blocks::Block::Chat, crate::blocks::Block::Task] {
        let mut app = App::default(); app.selected = Some(Selection::Employee("adu".into()));
        app.blocks.open(kind, Some("adu".into()));
        // Even an outstanding unrelated refetch cannot delay the emergency key.
        app.waiting_refresh = Some(0);
        for (c, method) in [('p', "autonomy.pause"), ('u', "autonomy.resume")] {
            let Action::Control(control @ Control::Autonomy { .. }) = app.key(ordinary_key(c), &m) else { panic!("immediate control expected"); };
            assert!(app.control_card.is_none()); assert!(app.card.is_none());
            let outcome = crate::controls::send(&gateway, &control).await;
            assert!(matches!(outcome, crate::decide::Outcome::Notice(_)));
            app.decision_finished(outcome);
            assert_eq!(gateway.0.lock().unwrap().last().unwrap(), &(method.into(), json!({"agent_id":"adu"})));
            assert!(!m.autonomy.paused("adu"));
        }
        app.key(ordinary_key('i'), &m);
        for c in ['p','u','n'] { assert_eq!(app.key(ordinary_key(c), &m), Action::None); }
        assert_eq!(app.input, "pun"); assert!(app.control_card.is_none());
    }
    let mut app = App::default(); app.selected = Some(Selection::Employee("adu".into()));
    let Action::Control(control) = app.key(ordinary_key('p'), &m) else { panic!() };
    let refused = Fake(Default::default(), true);
    app.decision_finished(crate::controls::send(&refused, &control).await);
    assert!(app.notice.as_ref().unwrap().contains("權限不足"));
    assert!(!app.notice.as_ref().unwrap().contains(strings::AUTONOMY_APPLIED));
    assert!(!m.autonomy.paused("adu"));
}

#[test]
fn autonomy_palette_permissions_all_shape_and_employee_status_cards() {
    let mut m = autonomy_model(json!({"global":null,"agents":{},"state_error":null}));
    let mut app = App::default(); app.selected = Some(Selection::Employee("adu".into()));
    for role in [Role::Employee, Role::Manager] {
        m.role = Some(role);
        assert!(!app.commands(&m).iter().any(|c| c.label == strings::AUTONOMY_PAUSE_ALL || c.label == strings::AUTONOMY_RESUME_ALL));
    }
    m.role = Some(Role::Admin);
    for (label, method) in [(strings::AUTONOMY_PAUSE_ALL,"autonomy.pause"),(strings::AUTONOMY_RESUME_ALL,"autonomy.resume")] {
        app.key(KeyEvent::new(KeyCode::Char('p'),KeyModifiers::CONTROL),&m);
        app.paste(label);
        let Action::Control(control) = app.key(KeyEvent::new(KeyCode::Enter,KeyModifiers::NONE),&m) else { panic!() };
        assert_eq!(control.request(),(method,json!({"all":true})));
        assert!(app.control_card.is_none());
    }
    for label in [strings::EMPLOYEE_PAUSE,strings::EMPLOYEE_RESUME] {
        app.key(KeyEvent::new(KeyCode::Char('p'),KeyModifiers::CONTROL),&m); app.paste(label);
        assert_eq!(app.key(KeyEvent::new(KeyCode::Enter,KeyModifiers::NONE),&m),Action::None);
        assert!(matches!(app.control_card.as_ref().unwrap().control,crate::controls::Control::Employee { .. }));
        app.control_card = None;
    }
    m.role = Some(Role::Employee);
    assert_eq!(app.key(ordinary_key('p'),&m),Action::Control(crate::controls::Control::Autonomy { agent: Some("adu".into()), resume: false }));
    m.role = Some(Role::Manager); m.goal_operators.clear();
    assert_eq!(app.key(ordinary_key('p'),&m),Action::Control(crate::controls::Control::Autonomy { agent: Some("adu".into()), resume: false }));
    assert_eq!(app.key(ordinary_key('u'),&m),Action::Control(crate::controls::Control::Autonomy { agent: Some("adu".into()), resume: true }));
    // Ctrl-P opens the palette; exercise the all-agent permission gate directly.
    for role in [Some(Role::Admin), None] {
        m.role = role;
        for resume in [false, true] {
            assert_eq!(app.autonomy_control(None,resume,&m),Action::Control(crate::controls::Control::Autonomy { agent: None, resume }));
        }
    }
    for role in [Role::Employee, Role::Manager] {
        m.role = Some(role);
        for resume in [false, true] {
            assert_eq!(app.autonomy_control(None,resume,&m),Action::None);
        }
    }
    // Ctrl-P is the palette even before users.me resolves; it never pauses all.
    for role in [None, Some(Role::Manager)] {
        m.role = role;
        assert_eq!(app.key(KeyEvent::new(KeyCode::Char('p'),KeyModifiers::CONTROL),&m),Action::None);
        assert!(app.palette.is_some());
        assert!(!app.palette_commands(&m).iter().any(|c| c.label == strings::AUTONOMY_PAUSE_ALL || c.label == strings::AUTONOMY_RESUME_ALL));
        app.key(KeyEvent::new(KeyCode::Esc,KeyModifiers::NONE),&m);
    }
}

#[test]
fn autonomy_employee_states_are_distinct() {
    let pause = pause_entry();
    let mut m = autonomy_model(json!({"global":null,"agents":{"adu":pause},"state_error":null}));
    let read = ReadState::default();
    for status in ["active","working"] {
        assert_eq!(employee_status(&employee("adu","阿嘟",status),&m,&read), EmployeeStatus::AutonomyPaused);
    }
    m.inbox = fresh(vec![approval("pending", "adu", "review", "tool", "approval")]);
    let output = screen(&m,&App::default(),&Settings::test_defaults(),180,24);
    assert!(output.contains("需要你"));
    assert!(output.contains(strings::AUTONOMY_PAUSED));
    m.inbox = fresh(vec![]);
    assert_eq!(employee_status(&employee("adu","阿嘟","paused"),&m,&read),EmployeeStatus::Paused);
    assert_eq!(employee_status(&employee("adu","阿嘟","stopped"),&m,&read),EmployeeStatus::Ended);
    m.autonomy.items[0].global = Some(serde_json::from_value(pause).unwrap());
    assert_eq!(employee_status(&employee("other","其他","active"),&m,&read),EmployeeStatus::AutonomyPaused);
    m.autonomy.error = Some(crate::Error::Network);
    assert_eq!(employee_status(&m.employees.items[0],&m,&read),EmployeeStatus::AutonomyUnknown);
}

#[test]
fn responsibility_cron_heartbeat_keys_use_real_handler_and_keep_cards() {
    let (mut m, base) = m2_task_app(); m.role = Some(Role::Admin);
    let mut app = base.clone();
    app.blocks.panes[0].responsibilities = true;
    app.blocks.panes[0].data.selected = json!({"responsibility_id":"resp1","control_epoch":4,"owner_agent_id":"adu"});
    assert_eq!(app.key(ordinary_key('p'),&m),Action::None);
    assert_eq!(app.control_card.as_ref().unwrap().control.request(),("responsibilities.pause",json!({"responsibility_id":"resp1","expected_control_epoch":4})));
    for block in [crate::blocks::Block::Detail,crate::blocks::Block::Run,crate::blocks::Block::Log,crate::blocks::Block::Task] {
        for (source,key,enabled,method) in [
            (ScheduleSource::Cron,'p',true,"cron.pause"),
            (ScheduleSource::Cron,'p',false,"cron.resume"),
            (ScheduleSource::Cron,'n',true,"cron.run_now"),
            (ScheduleSource::Heartbeat,'n',true,"heartbeat.trigger"),
        ] {
            let mut app = base.clone(); app.blocks.open(block,None);
            m.schedules = fresh(vec![ScheduledRun { id:"schedule".into(), agent:TerminalText::new("adu".into()), next_run:TerminalText::new("10:00".into()), source,
                detail:json!({"id":"cron1","agent_id":"adu","enabled":enabled}) }]);
            app.selected = Some(Selection::Schedule("schedule".into()));
            let original_pane = app.blocks.focused;
            app.key(ordinary_key('i'), &m);
            assert_eq!(app.key(ordinary_key(key),&m),Action::None);
            assert_eq!(app.input, key.to_string()); assert!(app.control_card.is_none());
            app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &m);
            app.key(ordinary_key(char::from(b'1' + original_pane as u8)), &m);
            app.key(ordinary_key('S'), &m); // Schedule controls belong to the visible schedule view.
            assert_eq!(app.key(ordinary_key(key),&m),Action::None);
            let (actual,params) = app.control_card.as_ref().unwrap().control.request();
            assert_eq!(actual,method);
            if method.starts_with("cron.") { assert_eq!(params["id"],"cron1"); assert!(params["request_id"].as_str().unwrap().starts_with("tui-")); }
            else { assert_eq!(params,json!({"agent_id":"adu"})); }
        }
    }
}

#[test]
fn header_cost_refusal_footer_selection_and_run_round_are_visible() {
    let mut model=empty(); model.employees=fresh(vec![employee("adu","阿嘟","active")]);
    let mut app=App::default(); app.selected=Some(Selection::Employee("adu".into()));
    app.blocks.open(crate::blocks::Block::Run,Some("adu".into()));
    app.blocks.panes[0].data=crate::blocks::Data { stale:false,total:1,selected:json!({"id":"dispatch:1","agent_id":"adu","channel":"goal","round":2,"status":"running"}), ..Default::default() };
    model.cost_millicents=Some(123_000);
    let output=screen(&model,&app,&Settings::test_defaults(),140,30);
    assert!(output.contains("$1.23")); assert!(output.contains("近 24 小時"));
    assert!(output.contains("goal · 回合 2")); assert!(output.contains("阿嘟"));
    assert!(output.contains("Ctrl-P")); assert!(output.contains("q 離開"));
    insta::assert_snapshot!("autonomy_header_cost",output);
    model.cost_millicents=None;
    let output=screen(&model,&app,&Settings::test_defaults(),140,30);
    assert!(!output.contains('$'));
    insta::assert_snapshot!("autonomy_header_cost_hidden",output);
}

#[test]
fn heartbeat_uses_last_run_and_cron_uses_gateway_next_run_without_prediction() {
    let mut model=empty();
    model.schedules=fresh(vec![
        ScheduledRun { id:"heartbeat:adu".into(),agent:TerminalText::new("adu".into()),next_run:TerminalText::new("2026-11-03T01:00:00Z".into()),source:ScheduleSource::Heartbeat,detail:json!({"last_run":"2026-11-02T01:00:00Z"}) },
        ScheduledRun { id:"cron:c1".into(),agent:TerminalText::new("adu".into()),next_run:TerminalText::new(strings::NOT_SCHEDULED.into()),source:ScheduleSource::Cron,detail:json!({"next_run":null,"last_status":"success"}) },
    ]);
    let mut app=App::default(); app.key(ordinary_key('S'), &model);
    let output=screen(&model,&app,&Settings::test_defaults(),200,40);
    assert!(output.contains("上次執行: 11/02 09:00"));
    assert!(output.contains("11/03 09:00")); assert!(output.contains(strings::NOT_SCHEDULED));
    assert!(!output.contains("2026-11-03T"));
    insta::assert_snapshot!("autonomy_schedule_times",output);
}

#[test]
fn chat_block_without_a_room_shows_connected_after_the_handshake() {
    use crate::chat::{ChatRooms, Frame};

    let model = populated();
    let mut app = App::default();
    app.selected = Some(Selection::Employee("adu".into()));
    app.key(KeyEvent::new(KeyCode::Char('C'), KeyModifiers::NONE), &model);
    let pane_id = app.blocks.panes[app.blocks.focused].id.clone();
    let mut rooms = ChatRooms::default();

    app.sync_chats(&rooms);
    assert_eq!(app.chat.status.visible(), strings::CONNECTING);
    assert!(!rooms.rooms.contains_key(&pane_id));

    rooms.apply(Frame::parse(r#"{"type":"session_info","session_id":"base"}"#).unwrap());
    app.sync_chats(&rooms);

    assert!(app.chat.connected);
    assert_eq!(app.chat.status.visible(), strings::CONNECTED);
    assert!(!rooms.rooms.contains_key(&pane_id));

    rooms.disconnected();
    app.sync_chats(&rooms);

    assert!(!app.chat.connected);
    assert_eq!(app.chat.status.visible(), strings::CONNECTING);
}

#[test]
fn concurrent_chat_real_keys_keep_drafts_scrollback_cancel_and_prompt_in_own_pane() {
    use crate::chat::{ChatRooms, Focus, Frame};
    let model = populated();
    let mut app = App::default();
    let mut rooms = ChatRooms::default();
    rooms.apply(Frame::parse(r#"{"type":"session_info","session_id":"base"}"#).unwrap());
    let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    app.selected = Some(Selection::Employee("adu".into()));
    app.key(key('C'), &model);
    app.sync_chats(&rooms);
    app.key(key('i'), &model); app.paste("first");
    let Action::Chat(input) = app.key(enter, &model) else { panic!("first submission"); };
    let a_id = app.blocks.panes[0].id.clone();
    let a = serde_json::to_value(rooms.room(&a_id).unwrap().prepare(input).unwrap()).unwrap();
    app.input = "draft A".into(); app.chat_scroll = 7;
    app.key(esc, &model);
    app.selected = Some(Selection::Employee("worker".into()));
    app.key(key('C'), &model);
    app.sync_chats(&rooms);
    assert!(app.input.is_empty());
    app.key(key('i'), &model); app.paste("second");
    let Action::Chat(input) = app.key(enter, &model) else { panic!("second submission"); };
    assert_eq!(input.agent, "worker");
    let b_id = app.blocks.panes[1].id.clone();
    let b = serde_json::to_value(rooms.room(&b_id).unwrap().prepare(input).unwrap()).unwrap();
    app.input = "draft B".into();
    app.key(esc, &model);
    for (sent, content) in [(&a, "A working"), (&b, "B working")] {
        rooms.apply(Frame::parse(&json!({"type":"progress","conv":sent["conv"],"msg_id":sent["msg_id"],"content":content}).to_string()).unwrap());
    }
    app.sync_chats(&rooms);
    assert_eq!(app.blocks.focused, 1);
    app.key(key('1'), &model);
    assert_eq!(app.input, "draft A"); assert_eq!(app.chat_scroll, 7);
    assert_eq!(app.key(key('X'), &model), Action::CancelChat);
    rooms.apply(Frame::parse(&json!({"type":"error","ts":1793764780000_i64,"code":"cancelled","conv":a["conv"],"msg_id":a["msg_id"],"downstream_ref":"private-reference","downstream_running":2}).to_string()).unwrap());
    app.sync_chats(&rooms);
    assert_eq!(app.blocks.focused, 0);
    assert!(app.downstream_prompt(std::time::Instant::now()).is_some());
    app.key(key('2'), &model);
    assert_eq!(app.input, "draft B"); assert!(app.chat.running);
    assert!(app.downstream_prompt(std::time::Instant::now()).is_none());
    assert_eq!(app.key(key('y'), &model), Action::None);
    let output = screen(&model, &app, &Settings::test_defaults(), 140, 36);
    assert!(!output.contains("private-reference"));
    insta::assert_snapshot!("two_chat_interleaved", output);
    app.key(key('1'), &model); app.key(key('i'), &model);
    assert_eq!(app.focus, Focus::Input);
    assert_eq!(app.key(key('y'), &model), Action::None);
    assert_eq!(app.input, "draft Ay");
    app.key(esc, &model);
    assert_eq!(app.key(key('y'), &model), Action::CancelDownstream("private-reference".into()));
    app.key(key('C'), &model); app.key(key('C'), &model); app.key(key('C'), &model);
    assert_eq!(app.blocks.panes.len(), 4);
}

#[test]
fn navigation_hints_are_passive_and_only_accept_known_inbox_paths() {
    let model = populated();
    let mut app = App::default();
    let original = Some(Selection::Employee("adu".into()));
    app.selected = original.clone();
    for path in ["//evil", "/inbox?item=unknown", "/inbox?item=a1&execute=yes", "/inbox?item=a1\u{1b}]52;bad", "/agents/adu/../worker", "/agents/worker", "/agents/adu", "/inbox?item=%61%31"] {
        app.navigate_hint(&json!({"path":path}), &model);
        assert_eq!(app.selected, original);
        assert!(app.notice.is_none());
    }
    app.focus = crate::chat::Focus::Input;
    app.input = "draft".into();
    app.navigate_hint(&json!({"path":"/inbox?item=a1"}), &model);
    assert_eq!(app.selected, original); assert_eq!(app.input, "draft");
    assert!(app.notice.is_none());
    app.focus = crate::chat::Focus::List;
    app.detail_scroll = 7;
    app.navigate_hint(&json!({"path":"/inbox?item=a1", "command":"approve", "focus":true}), &model);
    assert_eq!(app.selected, original);
    assert_eq!(app.notice.as_deref(), Some(strings::NAVIGATE_INBOX_HINT));
    assert_eq!(app.detail_scroll, 7);
    assert_eq!(app.focus, crate::chat::Focus::List);
    assert!(app.card.is_none());
}

#[test]
fn queued_navigation_cannot_retarget_pause_or_resume() {
    let mut model = autonomy_model(json!({"global":null,"agents":{},"state_error":null}));
    model.employees.items.push(employee("worker", "Worker", "active"));
    model.goal_operators.insert("worker".into());
    model.inbox = populated().inbox;
    for path in ["/agents/worker", "/inbox?item=a1", "/inbox?item=approval:a1"] {
        for (key, resume) in [('p', false), ('u', true)] {
            let mut app = App::default();
            app.selected = Some(Selection::Employee("adu".into()));
            // The event queue is drained immediately before the queued key.
            app.navigate_hint(&json!({"path":path,"agent_id":"worker","focus":true}), &model);
            app.refresh(&model);
            assert_eq!(app.key(ordinary_key(key), &model), Action::Control(
                crate::controls::Control::Autonomy { agent: Some("adu".into()), resume }
            ));
        }
    }
}

#[cfg(unix)]
#[test]
fn pty_mouse_uses_focused_viewport_and_never_focuses_another_pane() {
    use crossterm::event::{MouseEvent, MouseEventKind, MouseButton};
    use std::sync::{Arc, Mutex};
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> { self.0.lock().unwrap().extend_from_slice(bytes); Ok(bytes.len()) }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    let mut app = App::default();
    app.blocks.open(crate::blocks::Block::Pty, None);
    let pty = app.blocks.panes[0].pty.clone().unwrap();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let mut parser = vt100::Parser::new(4, 20, 0);
    parser.process("\x1b[?1000;1006h界x".as_bytes());
    { let mut session = pty.lock().unwrap(); session.set_screen_for_test(parser.screen()); session.set_writer_for_test(Capture(captured.clone())); }
    app.blocks.panes[0].pty_area = ratatui::layout::Rect::new(40, 10, 20, 4);
    let click = MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: 42, row: 11, modifiers: KeyModifiers::NONE };
    app.mouse(click, &Settings::test_defaults());
    assert!(captured.lock().unwrap().is_empty());
    app.pty_focus = true;
    app.mouse(MouseEvent { column: 39, ..click }, &Settings::test_defaults());
    app.mouse(MouseEvent { row: 14, ..click }, &Settings::test_defaults());
    assert!(captured.lock().unwrap().is_empty());
    app.mouse(click, &Settings::test_defaults());
    for column in [40, 41, 42] {
        app.mouse(MouseEvent { column, row: 10, ..click }, &Settings::test_defaults());
    }
    let expected = b"\x1b[<0;3;2M\x1b[<0;1;1M\x1b[<0;2;1M\x1b[<0;3;1M";
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    while captured.lock().unwrap().len() < expected.len() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert_eq!(*captured.lock().unwrap(), expected);
    assert_eq!(app.blocks.focused, 0);
    app.pty_focus = false;
    app.blocks.add(crate::blocks::Split::Vertical);
    app.mouse(click, &Settings::test_defaults());
    assert_eq!(app.blocks.focused, 1);
    assert_eq!(*captured.lock().unwrap(), expected);
}

#[test]
fn input_focus_cannot_send_or_paste_into_a_non_chat_pane() {
    let mut app = App::default();
    app.focus = crate::chat::Focus::Input;
    app.chat.connected = true;
    app.input_agent = Some("adu".into());
    app.paste("must not enter another pane");
    assert!(app.input.is_empty());
    app.input = "stale draft".into();
    assert_eq!(app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &empty()), Action::None);
    assert_eq!(app.focus, crate::chat::Focus::List);
}

#[test]
fn downstream_prompt_keeps_answer_keys_and_count_in_narrow_chat() {
    for width in [18, 24, 32, 48] {
        let mut app = downstream_app(std::time::Instant::now());
        let backend = ratatui::backend::TestBackend::new(width, 12);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| {
            super::chat_render::draw(frame, frame.area(), &mut app, &Settings::test_defaults(), strings::CHAT_ASSISTANT);
        }).unwrap();
        let buffer = terminal.backend().buffer();
        let mut output = String::new();
        for row in 0..12 {
            let mut column = 0;
            while column < width {
                let cell = &buffer[(column, row)];
                output.push_str(cell.symbol());
                column += cell.cell_width().max(1);
            }
            output.push('\n');
        }
        assert!(output.contains("y 停止"), "{width}: {output}");
        assert!(output.contains("n 繼續"), "{width}: {output}");
        assert!(output.contains('2'), "{width}: {output}");
        assert!(!output.contains("opaque-never-render"));
    }
}

#[test]
fn round1_visual_modes() {
    use crate::theme::{Capability, Theme, ThemeMode};
    for (name, mode, glyphs, capability) in [
        ("round1_dark", ThemeMode::Dark, Glyphs::Unicode, Capability::TrueColor),
        ("round1_light", ThemeMode::Light, Glyphs::Unicode, Capability::TrueColor),
        ("round1_ascii", ThemeMode::Dark, Glyphs::Ascii, Capability::Ansi16),
        ("round1_no_color", ThemeMode::Dark, Glyphs::Unicode, Capability::NoColor),
    ] {
        let settings = Settings { glyphs, theme: mode, colors: Theme::new(mode, capability), ..Settings::test_defaults() };
        let model = populated();
        let mut app = App::default(); app.refresh(&model);
        app.selected = Some(Selection::Employee("adu".into()));
        let mut terminal = Terminal::new(TestBackend::new(110, 38)).unwrap();
        crate::human_time::with_test_clock("2026-11-04T12:00:00+08:00", || {
            terminal.draw(|f| render::draw(f, &model, &connection(), &TerminalText::new("~/.duduclaw".into()), &mut app, &settings)).unwrap();
        });
        let buffer = terminal.backend().buffer();
        let mut output = String::new();
        for y in 0..38 {
            let mut row = String::new(); let mut x = 0;
            while x < 110 { let cell = &buffer[(x,y)]; row.push_str(cell.symbol()); x += cell.cell_width().max(1); }
            output.push_str(row.trim_end()); output.push('\n');
        }
        output.push_str("\nPainted semantic cells (symbol, foreground, background, modifiers):\n");
        for symbol in ["◉", "●", "○", "✓", "✕", "⏸", ">", "╭", "+", "C", "命"] {
            if let Some(cell) = buffer.content.iter().find(|c| c.symbol() == symbol) {
                output.push_str(&format!("{symbol}: {:?} / {:?} / {:?}\n", cell.fg, cell.bg, cell.modifier));
            }
        }
        let footer_key = buffer.content.iter().enumerate().find(|(i,c)| *i / 110 >= 34 && c.symbol() == "C").map(|(_,c)| c).unwrap();
        output.push_str(&format!("footer Ctrl-P: {:?} / {:?}\n", footer_key.fg, footer_key.modifier));
        assert_eq!(footer_key.fg, settings.colors.accent);
        if capability == Capability::NoColor {
            assert!(buffer.content.iter().all(|c| c.fg == Color::Reset && c.bg == Color::Reset));
            assert!(buffer.content.iter().any(|c| c.modifier.contains(Modifier::REVERSED)));
        }
        insta::assert_snapshot!(name, output);
    }
}

#[test]
fn round1_unicode_wide_cards_and_palette_preserve_physical_columns() {
    let settings = Settings { glyphs: Glyphs::Unicode, ambiguous_width: AmbiguousWidth::Wide, ..Settings::test_defaults() };
    let model = populated(); let mut app = App::default(); app.refresh(&model);
    for width in [99, 100] { screen(&model, &app, &settings, width, 36); }
    app.card = Some(super::card::Card::open(model.inbox.items[0].clone(), &model, std::time::Instant::now(), &mut app.codes));
    for width in [99, 100] { screen(&model, &app, &settings, width, 36); }
    app.card = None;
    app.key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL), &model);
    for width in [99, 100] { screen(&model, &app, &settings, width, 36); }
}

#[test]
fn round1_header_hides_default_home_and_unknown_counts() {
    let model = Model::default(); let mut app = App::default();
    let mut terminal = Terminal::new(TestBackend::new(100,30)).unwrap();
    terminal.draw(|f| render::draw(f, &model, &ConnectionState::Connecting,
        &TerminalText::new("~/.duduclaw".into()), &mut app, &Settings::test_defaults())).unwrap();
    let header: String = (0..3).flat_map(|y| (0..100).map(move |x| (x,y)))
        .map(|p| terminal.backend().buffer()[p].symbol()).collect();
    assert!(!header.contains('?')); assert!(!header.contains(".duduclaw"));
    assert!(!header.contains("執行中"));
}

fn round2_fixture() -> (Model, App) {
    let mut model = empty();
    model.cost_millicents = Some(42_000);
    model.cost_today = true;
    model.employees = fresh(vec![
        employee("ops", "Ops", "paused"),
        employee("writer", "Writer 中文編輯", "active"),
        employee("prod", "Prod-shaped", "working"),
        employee("plain", "Plain Test Employee", "active"),
        employee("error", "Error 檢查員", "error"),
    ]);
    model.inbox = fresh(vec![approval("quote", "plain", "寄出報價", "寄信", "approval")]);
    model.schedules = fresh(vec![ScheduledRun {
        id: "heartbeat:writer".into(), agent: TerminalText::new("writer".into()),
        next_run: TerminalText::new("2026-11-04T04:12:00Z".into()), source: ScheduleSource::Heartbeat,
        detail: json!({"agent_id":"writer","last_run":null,"enabled":true}),
    }]);
    model.autonomy.items[0].agents.insert("ops".into(), serde_json::from_value(json!({
        "reason":"維護資料庫", "who":"u1", "at":"2026-11-04T03:00:00Z"
    })).unwrap());
    let mut app = App::default();
    app.refresh(&model);
    app.chat.connected = true;
    app.chat.stale = false;
    app.chat.status = TerminalText::new(strings::CONNECTED.into());
    app.chat.append(strings::CHAT_USER, "幫我整理本週訂單");
    app.chat.append(strings::CHAT_ASSISTANT, "本週共 12 筆，已整理好，請確認報價。");
    let mut task = crate::blocks::Blocks::default();
    task.open(crate::blocks::Block::Task, Some("prod".into()));
    task.panes[0].data = crate::blocks::Data { stale: false,
        selected: json!({"id":"goal","assigned_to":"prod","goal_mode":true,"status":"doing","iteration_cap":5}),
        sections: vec![("iterations",json!({"iterations":[{"round":2,"dispatched_at":"2026-11-04T03:57:00Z"}]}))],
        ..Default::default() };
    app.blocks.parked.push(task.panes.remove(0));
    (model, app)
}

#[test]
fn round2_sidebar_sizes_unicode_and_ascii() {
    let (model, app) = round2_fixture();
    for glyphs in [Glyphs::Unicode, Glyphs::Ascii] {
        for (width, height) in [(80,24), (100,30), (140,40), (200,50)] {
            let mut settings = Settings::test_defaults();
            settings.glyphs = glyphs;
            let output = screen(&model, &app, &settings, width, height);
            assert!(output.contains("收件匣 1"));
            assert!(output.contains("對話"));
            assert!(output.lines().next().unwrap().contains("$0.42"));
            assert!(!output.chars().any(crate::sanitize::forbidden));
            if width >= 100 {
                assert!(output.contains("第 2/5 回合 · 已 3 分"));
                assert!(output.contains("下次心跳 12 分鐘後"));
                assert!(output.contains("暫停：維護資料庫"));
            } else { assert!(!output.contains("下次心跳")); }
            insta::assert_snapshot!(format!("round2_sidebar_{width}x{height}_{glyphs:?}"), output);
        }
    }
}

#[test]
fn round2_sidebar_light_no_color_and_wide_ambiguous() {
    let (model, app) = round2_fixture();
    for (name, theme, capability, width) in [
        ("light", crate::theme::ThemeMode::Light, crate::theme::Capability::TrueColor, AmbiguousWidth::Narrow),
        ("no_color", crate::theme::ThemeMode::Dark, crate::theme::Capability::NoColor, AmbiguousWidth::Narrow),
        ("wide", crate::theme::ThemeMode::Dark, crate::theme::Capability::TrueColor, AmbiguousWidth::Wide),
    ] {
        let mut settings = Settings::test_defaults();
        settings.glyphs = Glyphs::Unicode;
        settings.ambiguous_width = width;
        settings.theme = theme;
        settings.colors.capability = capability;
        settings.resolve_theme(None);
        let output = screen(&model, &app, &settings, 140,40);
        insta::assert_snapshot!(format!("round2_sidebar_{name}"), output);
    }
}

#[test]
fn round2_order_uses_display_name_and_actual_read_receipts() {
    let mut model = empty();
    model.employees = fresh(vec![employee("z", "A worker", "working"), employee("idle", "A idle", "active"),
        employee("pause", "A pause", "paused"), employee("err", "Z error", "error"),
        employee("b", "B needs", "awaiting"), employee("a", "A needs", "awaiting")]);
    model.recent_activity.insert("z".into(), Activity { completed_id:Some("run:z:1".into()), ..Default::default() });
    let mut read = ReadState::default(); read.mark("run:z:1".into());
    let ordered: Vec<_> = super::workspace::employees(&model, &read).into_iter().map(Employee::id).collect();
    assert_eq!(ordered, ["a", "b", "err", "z", "idle", "pause"]);
}

#[test]
fn round2_context_precedence_freshness_and_missing_fields() {
    let (mut model, mut app) = round2_fixture();
    let prod = model.employees.items.iter().find(|e| e.id() == "prod").unwrap().clone();
    crate::human_time::with_test_clock("2026-11-04T12:00:00+08:00", || {
        assert_eq!(super::workspace::context(&prod, &model, &app).plain(), "第 2/5 回合 · 已 3 分");
        app.blocks.parked[0].data.selected.as_object_mut().unwrap().remove("iteration_cap");
        assert_eq!(super::workspace::context(&prod, &model, &app).plain(), "第 2 回合 · 已 3 分");
        model.inbox.items.push(approval("prod", "prod", "需要補件\x1b]52;bad\x07", "tool", "question"));
        let context = super::workspace::context(&prod, &model, &app).plain();
        assert!(context.starts_with("員工提問：需要補件"));
        assert!(!context.contains('\x1b'));
        model.inbox.items.pop(); app.blocks.parked[0].data.stale = true;
        assert_eq!(super::workspace::context(&prod, &model, &app).plain(), strings::IDLE);
        let writer = model.employees.items.iter().find(|e| e.id() == "writer").unwrap();
        assert_eq!(super::workspace::context(writer, &model, &app).plain(), "下次心跳 12 分鐘後");
        model.schedules.stale = true;
        assert_eq!(super::workspace::context(writer, &model, &app).plain(), strings::IDLE);
    });
    assert_eq!(crate::blocks::field(&json!({}), "last_run"), "尚未執行");
    assert_eq!(crate::blocks::field(&json!({"last_run_at":null}), "last_run_at"), "尚未執行");
    assert_eq!(crate::blocks::field(&json!({"last_run":"bad"}), "last_run"), crate::human_time::UNKNOWN);
}

#[test]
fn round2_tabs_keep_live_chat_draft_and_input_keys() {
    let model = populated(); let mut app = App::default(); app.refresh(&model);
    let id = app.blocks.panes[app.blocks.focused].id.clone();
    app.chat.running = true; app.chat.connected = true;
    app.input = "未送出的草稿".into(); app.chat_scroll = 4;
    let tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
    app.key(tab, &model); assert_eq!(app.blocks.active, crate::blocks::Block::Task);
    app.key(tab, &model); assert_eq!(app.blocks.active, crate::blocks::Block::Run);
    app.key(tab, &model); assert_eq!(app.blocks.active, crate::blocks::Block::Chat);
    assert_eq!(app.blocks.panes[app.blocks.focused].id, id);
    assert!(app.chat.running); assert_eq!(app.input, "未送出的草稿"); assert_eq!(app.chat_scroll, 4);
    app.key(ordinary_key('i'), &model);
    app.key(ordinary_key('I'), &model); app.key(tab, &model);
    assert_eq!(app.input, "未送出的草稿I");
    assert_eq!(app.main_view, super::workspace::MainView::Workspace);
    assert_eq!(app.blocks.active, crate::blocks::Block::Chat);
}

#[test]
fn round2_lists_do_not_operate_hidden_task_chat_or_pty() {
    use crate::blocks::Block;
    let mut model = populated();
    let mut app = App::default(); app.selected = Some(Selection::Employee("adu".into()));
    app.blocks.open(Block::Task, Some("adu".into()));
    app.blocks.panes[0].data = crate::blocks::Data { stale:false, selected:json!({"id":"hidden","assigned_to":"adu","status":"needs_human","goal_mode":true}), ..Default::default() };
    app.key(ordinary_key('I'), &model);
    assert_eq!(app.key(ordinary_key('s'), &model), Action::None); // Approval is not snoozable.
    assert!(app.control_card.is_none());
    app.key(KeyEvent::new(KeyCode::Enter,KeyModifiers::NONE), &model);
    assert_eq!(app.card.as_ref().unwrap().item.key(), "approval:a1");
    assert!(!app.task_card);
    app.card = None;
    app.blocks.open(Block::Pty, None);
    let pty_id = app.blocks.panes[0].id.clone();
    app.key(ordinary_key('I'), &model);
    app.key(KeyEvent::new(KeyCode::Enter,KeyModifiers::NONE), &model);
    assert!(app.card.is_some()); assert!(!app.pty_focus);
    app.card = None;
    app.key(ordinary_key('P'), &model);
    assert_eq!(app.blocks.panes[0].id, pty_id);
    assert_eq!(app.pty_focus, crate::pty::SUPPORTED);
    app.pty_focus = false;
    model.schedules.items[0].detail = json!({"agent_id":"adu","cron":"0 * * * *","last_run":null});
    app.key(ordinary_key('S'), &model);
    app.key(KeyEvent::new(KeyCode::Enter,KeyModifiers::NONE), &model);
    assert_eq!(app.blocks.panes[0].id, pty_id); assert!(app.pty_confirm.is_none());
    let output = screen(&model,&app,&Settings::test_defaults(),100,30);
    assert!(output.contains("尚未執行")); assert!(output.contains("0 * * * *"));
    app.key(ordinary_key('I'), &model);
    app.selected = Some(Selection::Inbox(model.inbox.items[2].key()));
    app.key(KeyEvent::new(KeyCode::Enter,KeyModifiers::NONE), &model);
    if crate::pty::SUPPORTED { assert!(app.pty_confirm.is_some()); assert_eq!(app.blocks.panes[0].id, pty_id); }
}

#[test]
fn round2_initial_layout_and_many_employee_navigation_keep_bindings() {
    for layout in [crate::settings::DefaultLayout::Single, crate::settings::DefaultLayout::Vertical, crate::settings::DefaultLayout::Horizontal] {
        let mut model = empty();
        model.employees = fresh((0..8).map(|i| employee(&format!("e{i}"),&format!("員工{i}"),"active")).collect());
        let mut app = App::default(); app.blocks = layout.blocks(); app.blocks.invalidate(); app.refresh(&model);
        assert_eq!(app.blocks.active, crate::blocks::Block::Chat);
        for i in 1..8 {
            app.key(ordinary_key('j'), &model);
            assert_eq!(app.selected, Some(Selection::Employee(format!("e{i}"))));
            assert_eq!(app.blocks.panes[app.blocks.focused].agent.as_deref(), Some(format!("e{i}").as_str()));
            assert!(app.blocks.chat_ids().len() <= 4);
        }
    }
}

#[test]
fn round2_cost_precision_and_help_home() {
    assert_eq!(strings::cost_today(0), "今日 $0.00");
    assert_eq!(strings::cost_today(999), "今日 $0.0100");
    assert_eq!(strings::cost_today(500), "今日 $0.0050");
    assert_eq!(strings::cost_today(1000), "今日 $0.01");
    assert_eq!(strings::cost_today(42_000), "今日 $0.42");
    let mut app = App::default(); let mut terminal = Terminal::new(TestBackend::new(140,40)).unwrap();
    terminal.draw(|f| render::draw(f,&empty(),&connection(),&TerminalText::new("~/.duduclaw".into()),&mut app,&Settings::test_defaults())).unwrap();
    assert!(app.help_text().plain().contains("~/.duduclaw"));
}

#[test]
fn round2_pane_focus_synchronizes_employee_before_tab_or_pause() {
    let mut model = populated();
    model.goal_operators.extend(["adu".into(), "worker".into()]);
    let mut app = App::default();
    app.selected = Some(Selection::Employee("adu".into())); app.key(ordinary_key('C'), &model);
    app.selected = Some(Selection::Employee("worker".into())); app.key(ordinary_key('C'), &model);
    app.key(ordinary_key('1'), &model);
    assert_eq!(app.selected, Some(Selection::Employee("adu".into())));
    app.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &model);
    assert_eq!(app.blocks.panes[app.blocks.focused].agent.as_deref(), Some("adu"));
    assert!(matches!(app.key(ordinary_key('p'), &model), Action::Control(crate::controls::Control::Autonomy { agent:Some(id),resume:false }) if id == "adu"));
}

#[test]
fn round2_same_employee_split_chats_return_to_their_own_tab() {
    let model = populated(); let mut app = App::default();
    app.selected = Some(Selection::Employee("adu".into()));
    app.key(ordinary_key('C'), &model);
    let first = app.blocks.panes[app.blocks.focused].id.clone(); app.input = "first draft".into();
    app.key(ordinary_key('C'), &model);
    let second = app.blocks.panes[app.blocks.focused].id.clone(); app.input = "second draft".into(); app.chat.running = true;
    let tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
    for _ in 0..3 { app.key(tab, &model); }
    assert_eq!(app.blocks.panes[app.blocks.focused].id, second);
    assert_eq!(app.input, "second draft"); assert!(app.chat.running);
    app.key(ordinary_key('1'), &model);
    for _ in 0..3 { app.key(tab, &model); }
    assert_eq!(app.blocks.panes[app.blocks.focused].id, first);
    assert_eq!(app.input, "first draft"); assert!(!app.chat.running);
    app.key(ordinary_key('2'), &model);
    assert_eq!(app.blocks.panes[app.blocks.focused].id, second);
    assert_eq!(app.input, "second draft");
}

#[test]
fn round2_detail_then_new_chat_keeps_independent_tab_groups() {
    let model = populated(); let mut app = App::default(); app.refresh(&model);
    let first = app.blocks.panes[0].id.clone(); app.input = "old draft".into();
    let tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
    app.key(tab, &model); app.key(ordinary_key('D'), &model); app.key(ordinary_key('C'), &model);
    let second = app.blocks.panes[app.blocks.focused].id.clone(); app.input = "new draft".into();
    assert_ne!(first, second); assert_eq!(app.blocks.panes.len(), 2);
    for _ in 0..3 { app.key(tab, &model); }
    assert_eq!(app.blocks.panes[app.blocks.focused].id, second); assert_eq!(app.input, "new draft");
    app.key(ordinary_key('1'), &model);
    app.key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT), &model);
    app.key(tab, &model);
    assert_eq!(app.blocks.panes[app.blocks.focused].id, first); assert_eq!(app.input, "old draft");
}

#[test]
fn round2_removed_employee_keeps_draft_and_defers_switch_during_input() {
    let mut model = empty(); model.employees = fresh(vec![employee("a", "A", "active"), employee("b", "B", "active")]);
    let mut app = App::default(); app.refresh(&model); app.key(ordinary_key('i'), &model); app.input = "draft A".into();
    model.employees.items.remove(0); app.refresh(&model);
    assert_eq!(app.selected, Some(Selection::Employee("a".into())));
    assert_eq!(app.blocks.panes[app.blocks.focused].agent.as_deref(), Some("a"));
    app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &model); app.refresh(&model);
    assert_eq!(app.selected, Some(Selection::Employee("b".into())));
    assert_eq!(app.blocks.panes[app.blocks.focused].agent.as_deref(), Some("b"));
    assert!(app.blocks.parked.iter().any(|p| p.agent.as_deref() == Some("a") && p.chat_view.input == "draft A"));
}

#[test]
fn round2_chat_capacity_applies_to_all_openers_and_close_preserves_other_employees() {
    let mut model = empty();
    model.employees = fresh((0..5).map(|i| employee(&format!("e{i}"), &format!("E{i}"), "active")).collect());
    let mut app = App::default(); app.refresh(&model);
    for i in 0..4 {
        app.input = format!("draft {i}");
        app.key(ordinary_key('j'), &model);
    }
    assert_eq!(app.blocks.chat_ids().len(), 4);
    assert_eq!(app.blocks.active, crate::blocks::Block::Task);
    assert_eq!(app.selected, Some(Selection::Employee("e4".into())));
    app.key(ordinary_key('i'), &model); assert_eq!(app.focus, crate::chat::Focus::List);
    app.key(ordinary_key('C'), &model); assert_eq!(app.blocks.chat_ids().len(), 4);
    app.key(ordinary_key('k'), &model);
    app.key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT), &model);
    assert_eq!(app.input, "draft 3");
    app.key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL), &model);
    assert_eq!(app.blocks.chat_ids().len(), 3);
    assert!(app.blocks.parked.iter().any(|p| p.chat_view.input == "draft 0"));
}

#[test]
fn round2_removed_employee_waits_for_help_or_palette_to_close() {
    for help in [true, false] {
        let mut model = empty(); model.employees = fresh(vec![employee("a", "A", "active"), employee("b", "B", "active")]);
        let mut app = App::default(); app.refresh(&model);
        if help { app.help = true; } else { app.palette = Some(super::palette::Palette::default()); }
        model.employees.items.remove(0); app.refresh(&model);
        assert_eq!(app.selected, Some(Selection::Employee("a".into())));
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &model); app.refresh(&model);
        assert_eq!(app.selected, Some(Selection::Employee("b".into())));
        assert_eq!(app.blocks.panes[app.blocks.focused].agent.as_deref(), Some("b"));
    }
}

#[path = "round3_tests.rs"]
mod round3;
