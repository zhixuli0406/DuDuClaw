use super::{
    app::{App, EmployeeStatus, Selection, employee_status},
    text::{SafeText, columns, middle_ellipsis},
    widgets::{Row, section, rows, text, title},
};
use crate::{auth::Role, sanitize::TerminalText, settings::{Glyphs, Settings}, state::Model, state_extra::ScheduleSource, strings};
use ratatui::{Frame, layout::Rect, style::{Modifier, Style}};

pub(super) fn line(frame: &mut Frame, area: Rect, value: String, settings: &Settings, style: Style) {
    let style = settings.colors.readable_style(style);
    frame.render_widget(SafeText { text: &TerminalText::new(value), width: settings.ambiguous_width,
        visible: false, wrap: false, scroll: 0, style }, area);
}

fn group(status: EmployeeStatus) -> &'static str {
    match status {
        EmployeeStatus::Awaiting | EmployeeStatus::Error => "需要你",
        EmployeeStatus::Working => "執行中",
        _ => "閒置",
    }
}

/// Grouped, borderless employee rows. Navigation order is still workspace::employees.
pub(super) fn draw(frame: &mut Frame, area: Rect, model: &Model, app: &App, settings: &Settings) {
    if area.is_empty() { return; }
    let stale = model.employees.count().is_none() || model.inbox.count().is_none();
    let compact = frame.area().width < 100 || area.height < 7;
    let row_height = if compact { 1 } else { 2 };
    let employees = super::workspace::employees(model, &app.read);
    if employees.is_empty() {
        text(frame, area, TerminalText::new(if stale { strings::STALE } else { strings::EMPTY_EMPLOYEES }.into()), settings, stale, true);
        return;
    }
    let mut positions = Vec::new();
    let mut headings = Vec::new();
    let mut previous = "";
    let mut height = 0u16;
    for employee in &employees {
        let label = group(employee_status(employee, model, &app.read));
        if label != previous {
            if !previous.is_empty() { height = height.saturating_add(1); }
            headings.push((height, label));
            height = height.saturating_add(1);
            previous = label;
        }
        positions.push(height);
        height = height.saturating_add(row_height);
    }
    let overflow = height > area.height;
    let available = area.height.saturating_sub(u16::from(overflow));
    let selected = employees.iter().position(|e| app.selected == Some(Selection::Employee(e.id().into())));
    let offset = selected.or(Some(0)).map(|i| positions[i].saturating_add(row_height).saturating_sub(available)).unwrap_or(0);
    for (y, label) in headings {
        if y >= offset && y < offset.saturating_add(available) {
            line(frame, Rect::new(area.x + 1, area.y + y - offset, area.width.saturating_sub(1), 1),
                label.into(), settings, settings.colors.muted());
        }
    }
    let mut visible = 0;
    for (index, employee) in employees.iter().enumerate() {
        let position = positions[index];
        if position < offset || position.saturating_add(row_height) > offset.saturating_add(available) { continue; }
        visible += 1;
        let y = area.y + position - offset;
        let selected = app.selected == Some(Selection::Employee(employee.id().into()));
        let focused = app.focus == crate::chat::Focus::List && !app.help && app.card.is_none()
            && app.control_card.is_none() && app.palette.is_none() && app.mailbox.is_none() && !app.pty_focus;
        let base = if selected { settings.colors.selection(focused) } else { settings.colors.base() };
        frame.buffer_mut().set_style(Rect::new(area.x, y, area.width, row_height), base);
        let status = employee_status(employee, model, &app.read);
        let unread = model.inbox.items.iter().any(|item| item.agent().is(employee.id()) && !app.read.contains(&item.key()));
        let pointer = if selected { if settings.glyphs == Glyphs::Unicode { "›" } else { ">" } }
            else if unread { if settings.glyphs == Glyphs::Unicode { "✦" } else { "*" } } else { " " };
        line(frame, Rect::new(area.x, y, 1, 1), pointer.into(), settings, base.fg(settings.colors.accent));
        let glyph = status.glyph(settings.glyphs);
        let glyph_width = columns(glyph, settings.ambiguous_width) as u16;
        line(frame, Rect::new(area.x + 1, y, glyph_width.min(area.width.saturating_sub(1)), 1), glyph.into(), settings,
            base.fg(if stale { settings.colors.muted } else { status.color(&settings.colors) }));
        let name_x = (area.x + glyph_width + 2).min(area.right());
        let pill = matches!(status, EmployeeStatus::Awaiting | EmployeeStatus::Paused | EmployeeStatus::AutonomyPaused);
        let label = match status {
            EmployeeStatus::Awaiting => "待審",
            EmployeeStatus::Paused | EmployeeStatus::AutonomyPaused => "暫停",
            _ => status.label(),
        };
        // Keep the pause word even in a collapsed sidebar; color is never the only channel.
        let meta = if compact && !pill { String::new() } else if pill && !selected {
            format!("({label})")
        } else { format!(" {label} ") };
        let meta_width = (columns(&meta, settings.ambiguous_width) as u16).min(area.right().saturating_sub(name_x));
        let name_width = area.right().saturating_sub(name_x + meta_width + u16::from(meta_width > 0));
        let name = middle_ellipsis(&super::workspace::name(employee), usize::from(name_width), settings.ambiguous_width);
        let name_style = base.fg(if selected { settings.colors.text } else { settings.colors.secondary });
        line(frame, Rect::new(name_x, y, name_width, 1), name, settings,
            if selected { name_style.add_modifier(Modifier::BOLD) } else { name_style });
        if meta_width > 0 {
            let badge_style = if pill && !stale {
                if selected && settings.colors.capability != crate::theme::Capability::NoColor {
                    settings.colors.warning_pill(true).remove_modifier(Modifier::REVERSED)
                } else { base.fg(settings.colors.warning) }
            } else { base.fg(settings.colors.muted) };
            line(frame, Rect::new(area.right() - meta_width, y, meta_width, 1), meta, settings, badge_style);
        }
        if !compact {
            let context = super::workspace::context(employee, model, app);
            line(frame, Rect::new(name_x, y + 1, area.right().saturating_sub(name_x), 1),
                context.plain(), settings, base.fg(settings.colors.muted));
        }
    }
    if overflow {
        let marker = if settings.glyphs == Glyphs::Unicode { "…" } else { "..." };
        line(frame, Rect::new(area.x + 1, area.bottom() - 1, area.width.saturating_sub(1), 1),
            format!("{marker} 另 {} 位 · ↑↓", employees.len() - visible), settings, settings.colors.muted());
    }
}

pub(super) fn inbox(frame: &mut Frame, area: Rect, model: &Model, app: &App, settings: &Settings) {
    let stale = model.inbox.count().is_none();
    let inside = section(frame, area, &format!("{}{} · Esc 返回側欄", title(strings::INBOX, model.inbox.actionable_count()),
        model.inbox.informational_count().filter(|n| *n > 0).map(|n| format!(" · {}", strings::inbox_information(n))).unwrap_or_default()), settings, stale, false);
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
    let inside = section(frame, area, &format!("{} · Esc 返回側欄", strings::NEXT_RUNS), settings, stale, false);
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
    if area.is_empty() { return; }
    let pane = &app.blocks.panes[app.blocks.focused];
    let agent = pane.agent.as_deref().or(match &app.selected { Some(Selection::Employee(id)) => Some(id.as_str()), _ => None });
    let employee = agent.and_then(|id| model.employees.items.iter().find(|e| e.id() == id));
    let name = employee.map(super::workspace::name)
        .or_else(|| agent.map(|id| TerminalText::new(id.into()).plain())).unwrap_or_else(|| "控制室".into());
    let labels = ["對話", "任務", "紀錄"];
    let tabs_width: u16 = labels.iter().map(|label| columns(&format!(" {label} "), settings.ambiguous_width) as u16).sum();
    let show_tabs = area.width >= tabs_width + 8;
    let tabs_x = area.right().saturating_sub(if show_tabs { tabs_width } else { 0 });
    let title_width = tabs_x.saturating_sub(area.x + u16::from(show_tabs));
    let mut x = area.x;
    if let Some(employee) = employee {
        let status = employee_status(employee, model, &app.read);
        let glyph = status.glyph(settings.glyphs);
        let width = columns(glyph, settings.ambiguous_width) as u16;
        line(frame, Rect::new(x, area.y, width.min(title_width), 1), glyph.into(), settings,
            settings.colors.foreground(status.color(&settings.colors)));
        x = x.saturating_add(width + 1).min(tabs_x);
    }
    let name = middle_ellipsis(&name, usize::from(title_width.saturating_sub(x - area.x)).min(24), settings.ambiguous_width);
    let name_width = columns(&name, settings.ambiguous_width) as u16;
    line(frame, Rect::new(x, area.y, name_width, 1), name, settings, settings.colors.base().add_modifier(Modifier::BOLD));
    x = x.saturating_add(name_width + 2).min(tabs_x);
    // These are configured employee metadata from the existing agents.list response,
    // not a claim about the model actually used after a per-run fallback.
    let runtime = employee.and_then(|e| e.runtime.as_ref()).and_then(|r| r.provider.as_ref())
        .map(TerminalText::plain).unwrap_or_default();
    let model_name = employee.and_then(|e| e.model.as_ref()).and_then(|m| m.preferred.as_ref())
        .map(TerminalText::plain).unwrap_or_default();
    let runtime = [runtime.as_str(), model_name.as_str()].into_iter().filter(|s| !s.trim().is_empty())
        .collect::<Vec<_>>().join(" · ").replace('\n', " ");
    line(frame, Rect::new(x, area.y, tabs_x.saturating_sub(x + u16::from(show_tabs)), 1), runtime, settings, settings.colors.muted());
    if show_tabs {
        x = tabs_x;
        for (index, label) in labels.iter().enumerate() {
            let active = app.workspace_tab() == index && matches!(app.blocks.active, crate::blocks::Block::Chat | crate::blocks::Block::Task | crate::blocks::Block::Run | crate::blocks::Block::Log);
            let value = format!(" {label} ");
            let width = columns(&value, settings.ambiguous_width) as u16;
            let style = if active {
                if matches!(settings.colors.capability, crate::theme::Capability::NoColor | crate::theme::Capability::Ansi16) {
                    settings.colors.selected()
                } else { settings.colors.on(settings.colors.hover).base() }
            } else { settings.colors.muted() };
            line(frame, Rect::new(x, area.y, width, 1), value, settings, style);
            x += width;
        }
    }
}
