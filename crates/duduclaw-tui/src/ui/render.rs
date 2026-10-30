use super::{
    app::{App, Selection},
    text::SafeText,
};
use crate::{
    client::ConnectionState, sanitize::TerminalText, settings::Settings, state::Model, strings,
};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
};

use super::widgets::{panel, style, text};
pub fn draw(
    frame: &mut Frame,
    model: &Model,
    connection: &ConnectionState,
    home: &TerminalText,
    app: &mut App,
    settings: &Settings,
) {
    let area = frame.area();
    if area.width < 24 || area.height < 12 {
        if let Some(card) = &mut app.card { card.viewport(1, 0); }
        text(
            frame,
            area,
            TerminalText::new(strings::TERMINAL_SMALL.into()),
            settings,
            false,
            true,
        );
        return;
    }
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(4),
            Constraint::Length(if app.notice.is_some() { 5 } else { 4 }),
        ])
        .split(area);
    let (status, version) = match connection {
        ConnectionState::Connecting => (strings::CONNECTING, "?".into()),
        ConnectionState::Disconnected => (strings::DISCONNECTED, "?".into()),
        ConnectionState::LoginRequired(_) => (strings::DISCONNECTED, "?".into()),
        ConnectionState::Connected(info) => (
            strings::CONNECTED,
            info.version
                .as_ref()
                .map(TerminalText::plain)
                .unwrap_or_else(|| "?".into()),
        ),
    };
    text(
        frame,
        Rect::new(parts[0].x, parts[0].y, parts[0].width, 1),
        TerminalText::new(format!(
            "{} | {}",
            strings::TITLE,
            home.plain().replace('\n', " ")
        )),
        settings,
        false,
        false,
    );
    text(
        frame,
        Rect::new(parts[0].x, parts[0].y + 1, parts[0].width, 1),
        TerminalText::new(format!(
            "{} {} | {}",
            strings::GATEWAY,
            version.replace('\n', " "),
            status
        )),
        settings,
        false,
        false,
    );
    if let Some(autonomy) = &model.autonomy {
        text(
            frame,
            Rect::new(parts[0].x, parts[0].y + 2, parts[0].width, 1),
            TerminalText::new(format!(
                "{}: {}",
                strings::AUTONOMY,
                autonomy.plain().replace('\n', " ")
            )),
            settings,
            model.employees.count().is_none(),
            false,
        );
    }
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(48), Constraint::Percentage(52)])
        .split(parts[1]);
    super::panels::draw(frame, columns[0], model, app, settings);
    let stale_inbox = model.inbox.count().is_none();
    let stale_employees = model.employees.count().is_none() || stale_inbox;
    let stale_schedules = model.schedules.count().is_none();
    let detail_stale = !app.help
        && match app.selected {
            Some(Selection::Inbox(_)) => stale_inbox,
            Some(Selection::Employee(_)) => stale_employees,
            Some(Selection::Schedule(_)) => stale_schedules,
            None => false,
        };
    let inside = panel(frame, columns[1], strings::DETAIL, settings, detail_stale);
    let detail = app.detail(model);
    frame.render_widget(
        SafeText {
            text: &detail,
            width: settings.ambiguous_width,
            visible: true,
            wrap: true,
            scroll: if app.help { 0 } else { app.detail_scroll },
            style: style(detail_stale),
        },
        inside,
    );
    let footer = parts[2];
    text(
        frame,
        Rect::new(footer.x, footer.y, footer.width, 1),
        TerminalText::new(format!(
            "[{}]{}",
            if app.card.is_some() { strings::CARD_FOCUS } else if app.help { strings::HELP_FOCUS } else { strings::LIST_FOCUS },
            if detail_stale {
                format!(" {}", strings::STALE)
            } else {
                String::new()
            }
        )),
        settings,
        false,
        false,
    );
    text(
        frame,
        Rect::new(footer.x, footer.y + 1, footer.width, 2),
        TerminalText::new((if app.card.is_some() { strings::CARD_KEY_HINTS } else if app.help { strings::HELP_KEY_HINTS } else { strings::LIST_KEY_HINTS }).into()),
        settings,
        false,
        true,
    );
    if let Some(notice) = &app.notice {
        text(
            frame,
            Rect::new(footer.x, footer.y + 3, footer.width, 2),
            TerminalText::new(notice.clone()),
            settings,
            false,
            true,
        );
    }
    if let Some(card) = &mut app.card {
        super::card_render::draw(frame, columns[1], card, settings,
            !stale_inbox && app.waiting_refresh.is_none());
    }
}
