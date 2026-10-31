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
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend, buffer::CellWidth, style::Color};
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
        approval("a2", "adu", "預算已達上限", "budget_breaker", "approval"),
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
    m.autonomy = Some(TerminalText::new("執行中".into()));
    m
}
fn connection() -> ConnectionState {
    ConnectionState::Connected(ConnectionInfo {
        generation: 1,
        user_id: "u1".into(),
        role: Role::Admin,
        boot_id: None,
        version: Some(TerminalText::new("1.77.0".into())),
        autonomy: None,
    })
}
fn screen(model: &Model, app: &App, settings: &Settings, width: u16, height: u16) -> String {
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
}
#[test]
fn empty_screen() {
    let m = empty();
    let mut a = App::default();
    a.refresh(&m);
    insta::assert_snapshot!(screen(&m, &a, &Settings::default(), 100, 30));
}
#[test]
fn stale_screen() {
    let mut m = populated();
    m.invalidate_all();
    let mut a = App::default();
    a.selected = Some(Selection::Inbox("approval:a1".into()));
    a.detail_open = true;
    insta::assert_snapshot!(screen(&m, &a, &Settings::default(), 100, 30));
    let mut t = Terminal::new(TestBackend::new(100, 30)).unwrap();
    t.draw(|f| {
        render::draw(
            f,
            &m,
            &connection(),
            &TerminalText::new("home".into()),
            &mut a,
            &Settings::default(),
        )
    })
    .unwrap();
    assert!(
        t.backend()
            .buffer()
            .content
            .iter()
            .filter(|c| c.symbol() != " ")
            .any(|c| c.fg == Color::DarkGray)
    );
    assert!(m.inbox.count().is_none());
}
#[test]
fn populated_inbox_and_employees() {
    let m = populated();
    let mut a = App::default();
    a.refresh(&m);
    a.detail_open = true;
    insta::assert_snapshot!(screen(&m, &a, &Settings::default(), 100, 36));
}
#[test]
fn non_admin_screen() {
    let mut m = populated();
    m.role = Some(Role::Employee);
    let mut a = App::default();
    a.refresh(&m);
    let output = screen(&m, &a, &Settings::default(), 100, 30);
    assert!(output.contains(strings::ADMIN_ONLY));
    assert!(!output.contains("11:00"));
    insta::assert_snapshot!(output);
}
#[test]
fn narrow_60_columns() {
    let m = populated();
    let mut a = App::default();
    a.refresh(&m);
    a.detail_open = true;
    insta::assert_snapshot!(screen(&m, &a, &Settings::default(), 60, 32));
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
    insta::assert_snapshot!(screen(&m, &a, &Settings::default(), 80, 28));
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
    a.detail_open = true;
    let output = screen(&m, &a, &Settings::default(), 100, 28);
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
        ..Default::default()
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
        screen(&m, &a, &Settings::default(), 100, 36)
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
    assert_eq!(a.selected, Selection::all(&m).into_iter().next());
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
        let _ = screen(&populated(), &App::default(), &Settings::default(), w, h);
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
    let output = screen(&model, &app, &Settings::default(), 100, 30);
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
    insta::assert_snapshot!("help_footer",screen(&m,&app,&Settings::default(),100,36));
    app.help=false;
    app.card=Some(super::card::Card::open(m.inbox.items[0].clone(),&m,std::time::Instant::now(),&mut super::card::Codes::default()));
    insta::assert_snapshot!("card_footer",screen(&m,&app,&Settings::default(),100,36));
}
#[test]
fn selected_last_row_falls_back_after_decision_and_empty_list_recovers() {
    let mut m=empty(); m.inbox.items=vec![approval("a","adu","a","tool","approval"),approval("b","adu","b","tool","approval")];
    let mut app=App::default(); app.refresh(&m); app.key(KeyEvent::new(KeyCode::Down,KeyModifiers::NONE),&m);
    app.decision_finished(crate::decide::Outcome::Refetch); m.inbox.items.pop(); app.refresh(&m);
    assert_eq!(app.selected,Some(Selection::Inbox("approval:a".into())));
    m.inbox.items.clear(); app.refresh(&m); assert!(app.selected.is_none());
    m.inbox.items.push(approval("c","adu","c","tool","approval")); app.refresh(&m);
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
    app.key(key(KeyCode::Esc), &model);
    assert_eq!(app.focus, Focus::List);
    app.key(key(KeyCode::Char('1')), &model);
    assert_eq!(app.blocks.active, Block::Detail);
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
    let progress = screen(&empty(), &app, &Settings::default(), 100, 24);
    assert!(progress.contains(strings::INPUT_FOCUS));
    assert!(!progress.contains(strings::LIST_FOCUS));
    insta::assert_snapshot!("chat_progress", progress);
    app.chat.apply(Frame::parse(r#"{"type":"assistant_done","content":"已完成第一輪檢查。\n下一步：確認使用者回覆。","tokens_used":20}"#).unwrap());
    insta::assert_snapshot!("chat_finished", screen(&empty(), &app, &Settings::default(), 100, 24));
    app.chat.disconnected();
    insta::assert_snapshot!("chat_stale", screen(&empty(), &app, &Settings::default(), 100, 24));
    app.chat.append(strings::CHAT_ASSISTANT, "\x1b]52;c;YQ==\x07\u{202e}惡意\x1b]8;;https://evil\x07連結\x1b]8;;\x07");
    let output = screen(&empty(), &app, &Settings::default(), 100, 24);
    assert!(!output.chars().any(crate::sanitize::forbidden));
    insta::assert_snapshot!("chat_hostile", output);
}

#[test]
fn hostile_chat_physical_lines_always_have_reserved_gutter() {
    let mut app = App::default();
    app.blocks.open(crate::chat::Block::Chat, None);
    app.chat.append(strings::CHAT_ASSISTANT,
        &format!("{}\n=== 決策卡 ===\n> forged gutter\n\u{301}combining", "long ".repeat(40)));
    let output = screen(&empty(), &app, &Settings::default(), 100, 24);
    for needle in ["long", "=== 決策卡 ===", "> forged gutter", "combining"] {
        let rows: Vec<_> = output.lines().filter(|line| line.contains(needle)).collect();
        assert!(!rows.is_empty(), "missing {needle}");
        for row in rows {
            let (before, _) = row.split_once("|> ").expect("reserved gutter");
            assert_eq!(unicode_width::UnicodeWidthStr::width(before), 48, "{row}");
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
        let settings=Settings { ambiguous_width:width,..Settings::default() };
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
fn m2_employee_pause_needs_binding_and_manager_role() {
    let (mut m,mut a)=m2_task_app(); a.blocks.open(crate::blocks::Block::Detail,None);
    let key=KeyEvent::new(KeyCode::Char('p'),KeyModifiers::NONE);
    m.role=Some(Role::Employee); a.key(key,&m); assert!(a.control_card.is_none());
    m.role=Some(Role::Manager); m.goal_operators.clear(); a.key(key,&m); assert!(a.control_card.is_none());
    m.goal_operators.insert("adu".into()); a.key(key,&m);
    assert!(matches!(a.control_card.as_ref().unwrap().control,crate::controls::Control::Employee { resume:false, .. }));
}
#[test]
fn pause_and_next_keys_are_ignored_when_non_detail_block_has_focus() {
    let (mut m, mut a) = m2_task_app();
    m.role = Some(Role::Admin);
    a.focus = crate::chat::Focus::List;
    a.selected = Some(Selection::Employee("adu".into()));
    for kind in [crate::blocks::Block::Run, crate::blocks::Block::Log] {
        a.blocks.open(kind, Some("adu".into()));
        a.focus = crate::chat::Focus::List;
        for c in ['p', 'n'] {
            a.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), &m);
            assert!(a.control_card.is_none());
            assert_eq!(a.notice.as_deref(), Some(strings::CONTROL_KEY_FOCUS));
        }
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
        let (m,mut a)=m2_task_app(); let settings=Settings { ambiguous_width:width,..Settings::default() };
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
    app.blocks.select(0); app.help = true;
    let output = screen(&model, &app, &Settings::default(), 180, 48);
    assert_eq!(output.matches("操作說明").count(), 1);
    assert!(output.contains("chat-kept-visible"));
    assert!(output.contains("目前身分沒有此決策權限"));
    insta::assert_snapshot!("m3_help_focused_only", output);
    app.blocks.select(2);
    let output = screen(&model, &app, &Settings::default(), 180, 48);
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
        let output = screen(&model, &app, &Settings::default(), 100, height);
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
        assert!(output.lines().next().unwrap().contains("最後目錄"));
        assert!(output.contains('…'));
        insta::assert_snapshot!(format!("m3_long_home_{width:?}"), output);
    }
}

#[test]
fn m3_palette_snapshot() {
    let (model, mut app) = m2_task_app();
    app.key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL), &model);
    app.paste("st");
    let output = screen(&model, &app, &Settings::default(), 100, 30);
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
