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
