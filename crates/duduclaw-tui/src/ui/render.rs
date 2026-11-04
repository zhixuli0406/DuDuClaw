use super::{
    app::{App, Selection},
};
use crate::{
    client::ConnectionState, sanitize::TerminalText, settings::Settings, state::Model, strings,
};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
};

use super::widgets::text;
pub fn draw(
    frame: &mut Frame,
    model: &Model,
    connection: &ConnectionState,
    home: &TerminalText,
    app: &mut App,
    settings: &Settings,
) {
    let area = frame.area();
    app.home = home.clone();
    frame.buffer_mut().set_style(area, settings.colors.base());
    if area.width < 24 || area.height < 12 {
        if let Some(card) = &mut app.card { card.viewport(1, 0); }
        text(
            frame,
            area,
            TerminalText::new(format!("[{}] {}", app.focus_label(), strings::TERMINAL_SMALL)),
            settings,
            false,
            true,
        );
        return;
    }
    let footer_height = (if app.notice.is_some() { 5 } else { 4 }).min(area.height.saturating_sub(9));
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(if app.zoomed { 0 } else { 1 }),
            Constraint::Min(6),
            Constraint::Length(footer_height),
        ])
        .split(area);
    let (status, version, connected) = match connection {
        ConnectionState::Connecting => (strings::CONNECTING, None, false),
        ConnectionState::Disconnected | ConnectionState::LoginRequired(_) => (strings::DISCONNECTED, None, false),
        ConnectionState::Connected(info) => (strings::CONNECTED, info.version.as_ref().map(TerminalText::plain), true),
    };
    if !app.zoomed { header(frame, parts[0], model, status, version, connected, home, settings); }
    let sidebar_width = if app.zoomed { 0 } else if area.width < 100 { (area.width / 4).clamp(12, 22) } else { (area.width / 4).clamp(28, 34) };
    let columns = Layout::default().direction(Direction::Horizontal)
        .constraints([Constraint::Length(sidebar_width), Constraint::Min(1)]).split(parts[1]);
    if !app.zoomed { super::panels::draw(frame, columns[0], model, app, settings); }
    let stale_inbox = model.inbox.count().is_none();
    let stale_employees = model.employees.count().is_none() || stale_inbox;
    let stale_schedules = model.schedules.count().is_none();
    let detail_stale = match app.selected {
            Some(Selection::Inbox(_)) => stale_inbox,
            Some(Selection::Employee(_)) => stale_employees,
            Some(Selection::Schedule(_)) => stale_schedules,
            Some(Selection::InboxEntry) => stale_inbox,
            Some(Selection::ScheduleEntry) => stale_schedules,
            None => false,
        };
    let main = columns[1];
    let showing_inbox = app.main_view == super::workspace::MainView::Inbox;
    let showing_schedule = app.main_view == super::workspace::MainView::Schedules;
    // Overlays and input retain their existing focused block and key guards.
    if app.mailbox.is_some() {
        super::notices::mailbox(frame, main, app, settings);
    } else if showing_inbox && !app.help && app.focus != crate::chat::Focus::Input && !app.pty_focus {
        super::panels::inbox(frame, main, model, app, settings);
    } else if showing_schedule && !app.help && app.focus != crate::chat::Focus::Input && !app.pty_focus {
        super::panels::schedules(frame, main, model, app, settings);
    } else if app.zoomed {
        super::block_render::draw(frame, main, model, app, settings, detail_stale);
    } else {
        super::panels::tabs(frame, Rect::new(main.x, main.y, main.width, 1), model, app, settings);
        super::block_render::draw(frame, Rect::new(main.x, main.y + 1, main.width, main.height.saturating_sub(1)), model, app, settings, detail_stale);
    }
    let footer = parts[2];
    text(
        frame,
        Rect::new(footer.x, footer.y, footer.width, 1),
        TerminalText::new(format!(
            "{}[{}]{}{}{}",
            if app.zoomed { "放大中 · z 還原  " } else { "" },
            app.focus_label(),
            if app.focus == crate::chat::Focus::List && app.card.is_none() && app.control_card.is_none() && app.palette.is_none() {
                let name = match &app.selected {
                    Some(Selection::Employee(id)) => model.employees.items.iter().find(|e| e.id() == id).map(|e| if e.display_name.plain().is_empty() { e.name_text().plain() } else { e.display_name.plain() }),
                    Some(Selection::Inbox(id)) => model.inbox.items.iter().find(|i| i.key() == *id).map(|i| i.title().plain()),
                    Some(Selection::Schedule(id)) => model.schedules.items.iter().find(|s| &s.id == id).map(|s| s.agent.plain()),
                    Some(Selection::InboxEntry | Selection::ScheduleEntry) | None => None,
                };
                name.map(|n| format!(" {}", n.replace('\n', " "))).unwrap_or_default()
            } else { String::new() },
            if detail_stale {
                format!(" {}", strings::STALE)
            } else {
                String::new()
            },
            crate::autonomy::status_line(model).map(|s| format!(" · {}", s.plain())).unwrap_or_default()
        )),
        settings,
        false,
        false,
    );
    if app.focus == crate::chat::Focus::Input && app.card.is_none() && app.control_card.is_none()
        && (app.notice.is_none() || footer.height >= 5) {
        // Keep the insertion end visible at grapheme boundaries, including CJK.
        use unicode_segmentation::UnicodeSegmentation;
        use unicode_width::UnicodeWidthStr;
        let budget = usize::from(footer.width.saturating_sub(4));
        let safe = TerminalText::new(app.input.clone()).plain();
        let mut used = 0;
        let tail: Vec<_> = safe.graphemes(true).rev().take_while(|g| {
            used += match settings.ambiguous_width {
                crate::settings::AmbiguousWidth::Narrow => g.width(),
                crate::settings::AmbiguousWidth::Wide => g.width_cjk(),
            };
            used <= budget
        }).collect();
        text(frame, Rect::new(footer.x, footer.y + 1, footer.width, 1),
            TerminalText::new(format!("> {}_", tail.into_iter().rev().collect::<String>())), settings, false, false);
        text(frame, Rect::new(footer.x, footer.y + 2, footer.width, 1),
            TerminalText::new(crate::chat::command_note(&app.input).unwrap_or(strings::INPUT_HINT).into()), settings, false, false);
    } else {
    super::widgets::key_hints(
        frame,
        Rect::new(footer.x, footer.y + 1, footer.width, footer.height.saturating_sub(if app.notice.is_some() { 3 } else { 1 }).min(2)),
        TerminalText::new((if app.pty_confirm.is_some() { strings::PTY_CONFIRM } else if app.pty_focus { strings::PTY_KEYS } else if app.palette.is_some() { strings::PALETTE_KEYS } else if app.control_card.is_some() { strings::CONTROL_KEY_HINTS } else if app.card.is_some() { strings::CARD_KEY_HINTS } else if app.mailbox.is_some() { super::notices::MAIL_KEYS } else if app.toast_focus.is_some() { super::notices::TOAST_KEYS } else if app.help { strings::HELP_KEY_HINTS } else if !app.list_view() && app.blocks.active == crate::blocks::Block::Pty { strings::PTY_LIST_KEYS } else { app.context_hints(model) }).to_string() + if app.card.is_none() && app.control_card.is_none() && app.palette.is_none() && !app.pty_focus && !app.help && app.context_hints(model) != strings::LIST_KEY_HINTS { strings::GLOBAL_HINT_SUFFIX } else { "" }),
        settings,
    );
    }
    if let Some(notice) = &app.notice {
        text(
            frame,
            Rect::new(footer.x, footer.bottom().saturating_sub(2), footer.width, 2),
            TerminalText::new(notice.clone()),
            settings,
            false,
            true,
        );
    }
    super::notices::toasts(frame, main, app, settings, std::time::Instant::now());
    if let Some(card) = &mut app.card {
        super::card_render::draw(frame, columns[1], card, settings,
            !stale_inbox && app.waiting_refresh.is_none() && (!app.task_card || !app.blocks.panes[app.blocks.focused].data.stale));
    }
    let control_fresh = app.control_card.as_ref().is_some_and(|c| app.control_subject(&c.source, model).as_ref() == Some(&c.fingerprint)) && app.waiting_refresh.is_none();
    if let Some(card) = &mut app.control_card { super::control_card::draw(frame, columns[1], card, settings, control_fresh); }

    super::palette::draw(frame, model, app, settings);
}

fn display_home(home: &TerminalText, budget: usize, settings: &Settings) -> Option<String> {
    let path = home.plain().replace('\n', " ");
    let os_home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" });
    let default = os_home.as_ref().map(|h| std::path::PathBuf::from(h).join(".duduclaw"));
    if path == "~/.duduclaw" || default.as_ref().is_some_and(|d| std::path::Path::new(&path) == d) { return None; }
    let shortened = os_home.as_ref().and_then(|h| std::path::Path::new(&path).strip_prefix(h).ok())
        .map(|p| format!("~/{}", p.display())).unwrap_or(path);
    Some(super::text::middle_ellipsis(&shortened, budget, settings.ambiguous_width))
}

#[allow(clippy::too_many_arguments)]
fn header(frame: &mut Frame, area: Rect, model: &Model, status: &str, version: Option<String>, connected: bool, home: &TerminalText, settings: &Settings) {
    use super::text::{SafeText, columns};
    let unicode = settings.glyphs == crate::settings::Glyphs::Unicode;
    let (left, dash, right, dot, separator) = if unicode { ("╭", "─", "╮", "●", " · ") } else { ("+", "-", "+", "*", " | ") };
    let mut state = format!("{dot} {status}");
    if let Some(version) = version.filter(|v| !v.trim().is_empty()) { state.push_str(&format!("{separator}{}", version.replace('\n', " "))); }
    if let Some(cost) = model.cost_millicents { state.push_str(&format!("{separator}{}", if model.cost_today { strings::cost_today(cost) } else { strings::cost_window(cost) })); }
    if let Some(count) = model.observed_working() { state.push_str(&format!("{separator}{}", strings::working_runs(Some(count)))); }
    let width = settings.ambiguous_width;
    let fixed = columns(&format!("{left}{dash} {}  {state} {dash}{right}", strings::TITLE), width);
    let budget = usize::from(area.width).saturating_sub(fixed + 3);
    let path = display_home(home, budget, settings).filter(|s| !s.is_empty()).map(|s| format!(" | {s}")).unwrap_or_default();
    let prefix = format!("{left}{dash} {}{path} ", strings::TITLE);
    let suffix = format!(" {state} {dash}{right}");
    let fill = usize::from(area.width).saturating_sub(columns(&prefix, width) + columns(&suffix, width));
    let rail_width = columns(dash, width).max(1);
    let fill_text = format!("{}{}", dash.repeat(fill / rail_width), " ".repeat(fill % rail_width));
    let dot_x = area.x + (columns(&prefix, width) + fill + 1).min(usize::from(area.width)) as u16;
    frame.render_widget(SafeText { text: &TerminalText::new(format!("{prefix}{fill_text}{suffix}")), width,
        visible: false, wrap: false, scroll: 0, style: settings.colors.base() }, area);
    if dot_x + columns(dot, width) as u16 <= area.right() {
        frame.render_widget(SafeText { text: &TerminalText::new(dot.into()), width, visible: false, wrap: false, scroll: 0,
            style: settings.colors.emphasis(if connected { settings.colors.success } else { settings.colors.warning }) },
            Rect::new(dot_x, area.y, columns(dot, width) as u16, 1));
    }
}
