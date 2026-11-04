use super::app::{App, Selection};
use crate::{client::ConnectionState, sanitize::TerminalText, settings::Settings, state::Model, strings};
use ratatui::{Frame, layout::Rect};
use super::{panels::line, widgets::text};

pub fn draw(frame: &mut Frame, model: &Model, connection: &ConnectionState, home: &TerminalText, app: &mut App, settings: &Settings) {
    let area = frame.area();
    app.home = home.clone();
    frame.buffer_mut().set_style(area, settings.colors.on(settings.colors.app_shell).base());
    if area.width < 24 || area.height < 12 {
        if let Some(card) = &mut app.card { card.viewport(1, 0); }
        text(frame, area, TerminalText::new(format!("[{}] {}", app.focus_label(), strings::TERMINAL_SMALL)), settings, false, true);
        return;
    }
    let sidebar_width = if app.zoomed { 0 } else if area.width < 100 { (area.width / 4).clamp(12, 22) } else { 34 };
    let inset = Rect::new(area.x + 1, area.y + 1, area.width.saturating_sub(2), area.height.saturating_sub(2));
    if !app.zoomed {
        super::shell::sidebar(frame, Rect::new(inset.x, inset.y, sidebar_width, inset.height), model, connection, app, settings);
    }
    let panel = Rect::new(inset.x + sidebar_width, inset.y, inset.width.saturating_sub(sidebar_width), inset.height);
    let inner = super::widgets::panel_focus(frame, panel, "", settings, false, false);
    let inner = Rect::new(inner.x + 1.min(inner.width), inner.y, inner.width.saturating_sub(2), inner.height);
    let stale_inbox = model.inbox.count().is_none();
    let detail_stale = match app.selected {
        Some(Selection::Inbox(_) | Selection::InboxEntry) => stale_inbox,
        Some(Selection::Employee(_)) => model.employees.count().is_none() || stale_inbox,
        Some(Selection::Schedule(_) | Selection::ScheduleEntry) => model.schedules.count().is_none(),
        None => false,
    };
    let input = app.focus == crate::chat::Focus::Input && app.card.is_none() && app.control_card.is_none();
    let hints = contextual_hints(app, model);
    let caption = caption(app, model, detail_stale, hints.is_some() || input);
    let hint_rows = if hints.is_some() { 2 } else { 1 };
    let notice_rows = app.notice.as_ref().map_or(0, |notice| super::text::wrapped_lines(&TerminalText::new(notice.clone()), inner.width, settings.ambiguous_width).min(6) as u16);
    let footer_height = (hint_rows + if input { 2 } else { 0 } + u16::from(!caption.is_empty())
        + notice_rows).min(inner.height.saturating_sub(2));
    let footer = Rect::new(inner.x, inner.bottom() - footer_height, inner.width, footer_height);
    let main = Rect::new(inner.x, inner.y, inner.width, inner.height.saturating_sub(footer_height));
    let showing_inbox = app.main_view == super::workspace::MainView::Inbox;
    let showing_schedule = app.main_view == super::workspace::MainView::Schedules;
    // Decision cards and overlays keep their existing renderers, freshness checks and guards.
    if app.mailbox.is_some() {
        super::notices::mailbox(frame, main, app, settings);
    } else if showing_inbox && !app.help && !input && !app.pty_focus {
        super::panels::inbox(frame, main, model, app, settings);
    } else if showing_schedule && !app.help && !input && !app.pty_focus {
        super::panels::schedules(frame, main, model, app, settings);
    } else if app.zoomed {
        super::block_render::draw(frame, main, model, app, settings, detail_stale);
    } else {
        super::panels::tabs(frame, Rect::new(main.x, main.y, main.width, 1.min(main.height)), model, app, settings);
        if main.height > 1 { super::widgets::separator(frame, Rect::new(main.x, main.y + 1, main.width, 1), settings); }
        super::block_render::draw(frame, Rect::new(main.x, main.y + 2.min(main.height), main.width, main.height.saturating_sub(2)), model, app, settings, detail_stale);
    }
    draw_footer(frame, footer, connection, app, settings, &caption, hints, input);
    super::notices::toasts(frame, main, app, settings, std::time::Instant::now());
    if let Some(card) = &mut app.card {
        super::card_render::draw(frame, main, card, settings,
            !stale_inbox && app.waiting_refresh.is_none() && (!app.task_card || !app.blocks.panes[app.blocks.focused].data.stale));
    }
    let control_fresh = app.control_card.as_ref().is_some_and(|c| app.control_subject(&c.source, model).as_ref() == Some(&c.fingerprint)) && app.waiting_refresh.is_none();
    if let Some(card) = &mut app.control_card { super::control_card::draw(frame, main, card, settings, control_fresh); }
    super::palette::draw(frame, model, app, settings);
}

fn contextual_hints(app: &App, model: &Model) -> Option<&'static str> {
    Some(if app.pty_confirm.is_some() { strings::PTY_CONFIRM }
    else if app.pty_focus { strings::PTY_KEYS }
    else if app.palette.is_some() { strings::PALETTE_KEYS }
    else if app.control_card.is_some() { strings::CONTROL_KEY_HINTS }
    else if app.card.is_some() { strings::CARD_KEY_HINTS }
    else if app.mailbox.is_some() { super::notices::MAIL_KEYS }
    else if app.toast_focus.is_some() { super::notices::TOAST_KEYS }
    else if app.help { strings::HELP_KEY_HINTS }
    else if app.focus == crate::chat::Focus::Input { strings::INPUT_HINT }
    else if !app.list_view() && app.blocks.active == crate::blocks::Block::Pty { strings::PTY_LIST_KEYS }
    else if app.context_hints(model) != strings::LIST_KEY_HINTS { app.context_hints(model) }
    else { return None })
}

fn caption(app: &App, model: &Model, stale: bool, focused: bool) -> String {
    let mut parts = Vec::new();
    if focused { parts.push(app.focus_label().to_owned()); }
    if app.zoomed { parts.push("放大中 · z 還原".into()); }
    if stale { parts.push(strings::STALE.into()); }
    if let Some(status) = crate::autonomy::status_line(model) { parts.push(status.plain()); }
    parts.join(" · ")
}

#[allow(clippy::too_many_arguments)]
fn draw_footer(frame: &mut Frame, footer: Rect, connection: &ConnectionState, app: &App, settings: &Settings, caption: &str, hints: Option<&str>, input: bool) {
    if footer.is_empty() { return; }
    let mut y = footer.y;
    if !caption.is_empty() {
        line(frame, Rect::new(footer.x, y, footer.width, 1), caption.into(), settings, settings.colors.muted());
        y += 1;
    }
    if input && y + 1 < footer.bottom() {
        // Preserve the insertion tail at grapheme boundaries, including CJK.
        use unicode_segmentation::UnicodeSegmentation;
        let budget = usize::from(footer.width.saturating_sub(4));
        let safe = TerminalText::new(app.input.clone()).plain();
        let mut used = 0;
        let tail: Vec<_> = safe.graphemes(true).rev().take_while(|g| {
            used += super::text::columns(g, settings.ambiguous_width);
            used <= budget
        }).collect();
        text(frame, Rect::new(footer.x, y, footer.width, 1), TerminalText::new(format!("> {}_", tail.into_iter().rev().collect::<String>())), settings, false, false);
        y += 1;
        text(frame, Rect::new(footer.x, y, footer.width, 1), TerminalText::new(crate::chat::command_note(&app.input).unwrap_or(strings::INPUT_HINT).into()), settings, false, false);
        y += 1;
    }
    if let Some(notice) = &app.notice {
        let height = (super::text::wrapped_lines(&TerminalText::new(notice.clone()), footer.width, settings.ambiguous_width) as u16)
            .min(footer.bottom().saturating_sub(y + 1));
        text(frame, Rect::new(footer.x, y, footer.width, height), TerminalText::new(notice.clone()), settings, false, true);
        y += height;
    }
    if y >= footer.bottom() { return; }
    let version = super::shell::version(connection).unwrap_or_default();
    let measured_version = super::text::columns(&version, settings.ambiguous_width);
    let version_width = if measured_version <= usize::from(footer.width / 3) { measured_version as u16 } else { 0 };
    let hints_width = footer.width.saturating_sub(version_width + u16::from(version_width > 0));
    let value = hints.map(str::to_owned).unwrap_or_else(|| super::shell::idle_hints(app, hints_width));
    super::widgets::key_hints(frame, Rect::new(footer.x, y, hints_width, footer.bottom() - y), TerminalText::new(value), settings);
    line(frame, Rect::new(footer.right() - version_width, footer.bottom() - 1, version_width, 1), version, settings, settings.colors.muted());
}
