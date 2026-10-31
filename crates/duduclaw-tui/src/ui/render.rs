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
            Constraint::Length(3),
            Constraint::Min(6),
            Constraint::Length(footer_height),
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
            super::text::left_ellipsis(&home.plain().replace('\n', " "), usize::from(area.width).saturating_sub(super::text::columns(strings::TITLE, settings.ambiguous_width) + 3), settings.ambiguous_width)
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
    let detail_stale = match app.selected {
            Some(Selection::Inbox(_)) => stale_inbox,
            Some(Selection::Employee(_)) => stale_employees,
            Some(Selection::Schedule(_)) => stale_schedules,
            None => false,
        };
    super::block_render::draw(frame, columns[1], model, app, settings, detail_stale);
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
    text(
        frame,
        Rect::new(footer.x, footer.y + 1, footer.width, footer.height.saturating_sub(if app.notice.is_some() { 3 } else { 1 }).min(2)),
        TerminalText::new((if app.palette.is_some() { strings::PALETTE_KEYS } else if app.control_card.is_some() { strings::CONTROL_KEY_HINTS } else if app.card.is_some() { strings::CARD_KEY_HINTS } else if app.help { strings::HELP_KEY_HINTS } else { strings::LIST_KEY_HINTS }).into()),
        settings,
        false,
        true,
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
    if let Some(card) = &mut app.card {
        super::card_render::draw(frame, columns[1], card, settings,
            !stale_inbox && app.waiting_refresh.is_none() && (!app.task_card || !app.blocks.panes[app.blocks.focused].data.stale));
    }
    let control_fresh = app.control_card.as_ref().is_some_and(|c| app.control_subject(&c.source, model).as_ref() == Some(&c.fingerprint)) && app.waiting_refresh.is_none();
    if let Some(card) = &mut app.control_card { super::control_card::draw(frame, columns[1], card, settings, control_fresh); }

    super::palette::draw(frame, model, app, settings);
}
