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
            TerminalText::new(format!("[{}] {}", app.focus_label(), strings::TERMINAL_SMALL)),
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
    if app.blocks.active == crate::chat::Block::Chat && !app.help {
        super::chat_render::draw(frame, columns[1], app, settings);
    } else {
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
    }
    let footer = parts[2];
    text(
        frame,
        Rect::new(footer.x, footer.y, footer.width, 1),
        TerminalText::new(format!(
            "[{}]{}",
            app.focus_label(),
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
    if app.focus == crate::chat::Focus::Input && app.card.is_none() {
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
    text(
        frame,
        Rect::new(footer.x, footer.y + 1, footer.width, 2),
        TerminalText::new((if app.card.is_some() { strings::CARD_KEY_HINTS } else if app.help { strings::HELP_KEY_HINTS } else { strings::LIST_KEY_HINTS }).into()),
        settings,
        false,
        true,
    );
    }
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
