use super::{
    app::{App, Selection, employee_status},
    text::{SafeText, columns, middle_ellipsis},
    widgets::{Row, panel, rows, text, title},
};
use crate::{auth::Role, sanitize::TerminalText, settings::{Glyphs, Settings}, state::Model, state_extra::ScheduleSource, strings};
use ratatui::{Frame, layout::Rect, style::Style};

fn line(frame: &mut Frame, area: Rect, value: String, settings: &Settings, style: Style) {
    frame.render_widget(SafeText { text: &TerminalText::new(value), width: settings.ambiguous_width,
        visible: false, wrap: false, scroll: 0, style }, area);
}

pub(super) fn draw(frame: &mut Frame, area: Rect, model: &Model, app: &App, settings: &Settings) {
    let stale = model.employees.count().is_none() || model.inbox.count().is_none();
    let inside = panel(frame, area, &title(strings::EMPLOYEES, if stale { None } else { model.employees.count() }), settings, stale);
    if inside.is_empty() { return; }
    let compact = frame.area().width < 100;
    let card_height = if compact { 1 } else { 2 };
    let footer_height = if super::m2::manager(model) { 2 } else { 1 };
    let available = inside.height.saturating_sub(footer_height);
    let employees = super::workspace::employees(model, &app.read);
    let selected = employees.iter().position(|e| app.selected == Some(Selection::Employee(e.id().into())));
    let count = usize::from(available / card_height);
    let offset = selected.map(|i| (i + 1).saturating_sub(count)).unwrap_or(0);
    if employees.is_empty() {
        text(frame, Rect::new(inside.x, inside.y, inside.width, available),
            TerminalText::new(if stale { strings::STALE } else { strings::EMPTY_EMPLOYEES }.into()), settings, stale, true);
    }
    for (index, employee) in employees.iter().skip(offset).take(count).enumerate() {
        let y = inside.y + index as u16 * card_height;
        let selected = app.selected == Some(Selection::Employee(employee.id().into()));
        let base = if selected { settings.colors.selected() } else { settings.colors.base() };
        frame.buffer_mut().set_style(Rect::new(inside.x, y, inside.width, card_height), base);
        let status = employee_status(employee, model, &app.read);
        let unread = model.inbox.items.iter().any(|i| i.agent().is(employee.id()) && !app.read.contains(&i.key()));
        let marker = if unread { if settings.glyphs == Glyphs::Unicode { "▎" } else { "|" } } else { " " };
        let marker_width = columns(marker, settings.ambiguous_width) as u16;
        line(frame, Rect::new(inside.x, y, marker_width, 1), marker.into(), settings, base.fg(settings.colors.accent));
        let glyph = status.glyph(settings.glyphs);
        let glyph_width = columns(glyph, settings.ambiguous_width) as u16;
        line(frame, Rect::new(inside.x + marker_width, y, glyph_width, 1), glyph.into(), settings,
            base.fg(if stale { settings.colors.muted } else { status.color(&settings.colors) }));
        let name_x = inside.x + marker_width + glyph_width + 1;
        let status_width = if compact { 0 } else { columns(status.label(), settings.ambiguous_width) as u16 + 1 };
        let name_width = inside.right().saturating_sub(name_x + status_width);
        let name = middle_ellipsis(&super::workspace::name(employee), usize::from(name_width), settings.ambiguous_width);
        line(frame, Rect::new(name_x, y, name_width, 1), name, settings, base);
        if !compact {
            line(frame, Rect::new(inside.right().saturating_sub(status_width), y, status_width, 1),
                format!(" {}", status.label()), settings, base.fg(if stale { settings.colors.muted } else { status.color(&settings.colors) }));
            let context = super::workspace::context(employee, model, app);
            line(frame, Rect::new(inside.x + marker_width + 1, y + 1, inside.width.saturating_sub(marker_width + 1), 1),
                context.plain(), settings, base.fg(settings.colors.muted));
        }
    }
    let inbox_y = inside.bottom().saturating_sub(footer_height);
    let count = model.inbox.count().map(|n| n.to_string()).unwrap_or_else(|| "?".into());
    let arrow = if settings.glyphs == Glyphs::Unicode { "▸" } else { ">" };
    let inbox_selected = matches!(app.selected, Some(Selection::InboxEntry | Selection::Inbox(_)));
    let base = if inbox_selected { settings.colors.selected() } else { settings.colors.base() };
    let color = if model.inbox.count().is_some_and(|n| n > 0) { settings.colors.accent } else { settings.colors.muted };
    let rect = Rect::new(inside.x, inbox_y, inside.width, 1);
    frame.buffer_mut().set_style(rect, base);
    line(frame, rect, format!("收件匣 {count} {arrow}  I"), settings, base.fg(color));
    if footer_height > 1 {
        let selected = matches!(app.selected, Some(Selection::ScheduleEntry | Selection::Schedule(_)));
        let base = if selected { settings.colors.selected() } else { settings.colors.muted() };
        let count = model.schedules.count().map(|n| n.to_string()).unwrap_or_else(|| "?".into());
        let rect = Rect::new(inside.x, inbox_y + 1, inside.width, 1);
        frame.buffer_mut().set_style(rect, base);
        line(frame, rect, format!("排程 {count} {arrow}  S"), settings, base);
    }
}

pub(super) fn inbox(frame: &mut Frame, area: Rect, model: &Model, app: &App, settings: &Settings) {
    let stale = model.inbox.count().is_none();
    let inside = panel(frame, area, &format!("{}{} · Esc 返回側欄", title(strings::INBOX, model.inbox.actionable_count()),
        model.inbox.informational_count().filter(|n| *n > 0).map(|n| format!(" · {}", strings::inbox_information(n))).unwrap_or_default()), settings, stale);
    if model.inbox.items.is_empty() {
        text(frame, inside, TerminalText::new(if stale { strings::STALE } else { strings::EMPTY_INBOX_GUIDE }.into()), settings, stale, true);
    } else {
        rows(frame, inside, model.inbox.items.iter().map(|i| Row {
            selection: Selection::Inbox(i.key()), status: None,
            text: TerminalText::new(format!("[{}] {} · {}{}{}", i.source_label(), i.context(), i.title().plain(),
                if app.read.contains(&i.key()) { format!(" ({})", strings::READ) } else { String::new() },
                if app.read.resurfaced(i, crate::snooze::epoch_now()) { format!(" ({})", strings::RESURFACED) } else { String::new() })),
        }).collect(), app, settings, stale);
    }
}

pub(super) fn schedules(frame: &mut Frame, area: Rect, model: &Model, app: &App, settings: &Settings) {
    let manager = super::m2::manager(model);
    let stale = model.schedules.count().is_none();
    let inside = panel(frame, area, &format!("{} · Esc 返回側欄", strings::NEXT_RUNS), settings, stale);
    if !manager || model.schedules.items.is_empty() {
        text(frame, inside, TerminalText::new(if !manager { strings::ADMIN_ONLY } else if stale { strings::STALE } else { strings::NO_SCHEDULE }.into()), settings, stale, true);
        return;
    }
    if app.detail_open {
        frame.render_widget(SafeText { text: &app.selection_detail(model), width: settings.ambiguous_width,
            visible: true, wrap: true, scroll: app.detail_scroll, style: super::widgets::style(settings, stale) }, inside);
        return;
    }
    let inside = if model.role != Some(Role::Admin) {
        text(frame, Rect::new(inside.x, inside.y, inside.width, 1), TerminalText::new(strings::CRON_ADMIN_ONLY.into()), settings, false, false);
        Rect::new(inside.x, inside.y + 1, inside.width, inside.height.saturating_sub(1))
    } else { inside };
    rows(frame, inside, model.schedules.items.iter().map(|s| Row {
        selection: Selection::Schedule(s.id.clone()), status: None,
        text: TerminalText::new(format!("{} {} {} / {}: {}",
            match s.source { ScheduleSource::Heartbeat => strings::HEARTBEAT, ScheduleSource::Cron => strings::CRON },
            s.agent.plain(), crate::human_time::schedule(&s.next_run),
            match s.source { ScheduleSource::Heartbeat => strings::field_label("last_run"), ScheduleSource::Cron => strings::LAST_RESULT },
            crate::blocks::field(&s.detail, match s.source { ScheduleSource::Heartbeat => "last_run", ScheduleSource::Cron => "last_status" }))),
    }).collect(), app, settings, stale);
}

pub(super) fn tabs(frame: &mut Frame, area: Rect, model: &Model, app: &App, settings: &Settings) {
    let agent = app.blocks.panes[app.blocks.focused].agent.as_deref();
    let name = agent.and_then(|id| model.employees.items.iter().find(|e| e.id() == id)).map(super::workspace::name)
        .or_else(|| agent.map(|id| TerminalText::new(id.into()).plain())).unwrap_or_default();
    let labels = ["對話", "任務", "紀錄"];
    let arrow = if settings.glyphs == Glyphs::Unicode { "▸" } else { ">" };
    let tab_width: usize = labels.iter().map(|label| columns(&format!("{arrow} {label}  "), settings.ambiguous_width)).sum();
    let name_budget = usize::from(area.width).saturating_sub(tab_width + 1).min(24);
    let name = middle_ellipsis(&name, name_budget, settings.ambiguous_width);
    let name_width = columns(&name, settings.ambiguous_width) as u16;
    line(frame, Rect::new(area.x, area.y, name_width, 1), name, settings, settings.colors.muted());
    let mut x = area.x + name_width + u16::from(name_width > 0);
    for (index, label) in labels.iter().enumerate() {
        let active = app.workspace_tab() == index && matches!(app.blocks.active, crate::blocks::Block::Chat | crate::blocks::Block::Task | crate::blocks::Block::Run | crate::blocks::Block::Log);
        let value = format!("{} {label}  ", if active { arrow } else { " " });
        let width = columns(&value, settings.ambiguous_width) as u16;
        line(frame, Rect::new(x, area.y, width.min(area.right().saturating_sub(x)), 1), value, settings,
            if active { settings.colors.emphasis(settings.colors.accent) } else { settings.colors.muted() });
        x = x.saturating_add(width).min(area.right());
    }
}
