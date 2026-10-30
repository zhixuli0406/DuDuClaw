use super::{
    app::{App, Selection, employee_status},
    widgets::{Row, panel, rows, text, title},
};
use crate::{
    auth::Role, sanitize::TerminalText, settings::Settings, state::Model,
    state_extra::ScheduleSource, strings,
};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
};
pub(super) fn draw(frame: &mut Frame, area: Rect, model: &Model, app: &App, settings: &Settings) {
    let compact = area.height < 9;
    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            if compact { Constraint::Length(3) } else { Constraint::Percentage(35) },
            if compact { Constraint::Length(3) } else { Constraint::Percentage(40) },
            if compact { Constraint::Min(0) } else { Constraint::Percentage(25) },
        ])
        .split(area);
    let stale_inbox = model.inbox.count().is_none();
    let inside = panel(
        frame,
        left[0],
        &title(strings::INBOX, model.inbox.count()),
        settings,
        stale_inbox,
    );
    if model.inbox.items.is_empty() {
        text(
            frame,
            inside,
            TerminalText::new(
                if stale_inbox {
                    strings::STALE
                } else {
                    strings::EMPTY_INBOX
                }
                .into(),
            ),
            settings,
            stale_inbox,
            true,
        );
    } else {
        rows(
            frame,
            inside,
            model
                .inbox
                .items
                .iter()
                .map(|i| Row {
                    selection: Selection::Inbox(i.key()),
                    text: TerminalText::new(format!(
                        "[{}] {}{}{}",
                        i.source_label(),
                        i.title().plain(),
                        if app.read.contains(&i.key()) {
                            format!(" ({})", strings::READ)
                        } else {
                            String::new()
                        },
                        if app.read.resurfaced(i, crate::snooze::epoch_now()) {
                            format!(" ({})", strings::RESURFACED)
                        } else { String::new() }
                    )),
                })
                .collect(),
            app,
            settings,
            stale_inbox,
        );
    }
    let stale_employees = model.employees.count().is_none() || stale_inbox;
    let inside = panel(
        frame,
        left[1],
        &title(
            strings::EMPLOYEES,
            if stale_employees {
                None
            } else {
                model.employees.count()
            },
        ),
        settings,
        stale_employees,
    );
    if model.employees.items.is_empty() {
        text(
            frame,
            inside,
            TerminalText::new(
                if stale_employees {
                    strings::STALE
                } else {
                    strings::EMPTY_EMPLOYEES
                }
                .into(),
            ),
            settings,
            stale_employees,
            true,
        );
    } else {
        rows(
            frame,
            inside,
            model
                .employees
                .items
                .iter()
                .map(|e| {
                    let s = employee_status(e, model, &app.read);
                    Row {
                        selection: Selection::Employee(e.id().into()),
                        text: TerminalText::new(format!(
                            "{} {} {}",
                            s.glyph(settings.glyphs),
                            if e.display_name.plain().is_empty() {
                                e.name_text().plain()
                            } else {
                                e.display_name.plain()
                            },
                            s.label()
                        )),
                    }
                })
                .collect(),
            app,
            settings,
            stale_employees,
        );
    }
    let admin = super::m2::manager(model);
    let stale_schedules = admin && model.schedules.count().is_none();
    let inside = panel(
        frame,
        left[2],
        &if stale_schedules {
            format!("{} {}", strings::NEXT_RUNS, strings::STALE)
        } else {
            strings::NEXT_RUNS.into()
        },
        settings,
        stale_schedules,
    );
    if !admin || model.schedules.items.is_empty() {
        text(
            frame,
            inside,
            TerminalText::new(
                if !admin {
                    strings::ADMIN_ONLY
                } else if stale_schedules {
                    strings::STALE
                } else {
                    strings::NO_SCHEDULE
                }
                .into(),
            ),
            settings,
            stale_schedules,
            true,
        );
    } else {
        let inside = if model.role != Some(Role::Admin) {
            text(frame, ratatui::layout::Rect::new(inside.x, inside.y, inside.width, 1), TerminalText::new(strings::CRON_ADMIN_ONLY.into()), settings, false, false);
            ratatui::layout::Rect::new(inside.x, inside.y + 1, inside.width, inside.height.saturating_sub(1))
        } else { inside };
        rows(
            frame,
            inside,
            model
                .schedules
                .items
                .iter()
                .map(|s| Row {
                    selection: Selection::Schedule(s.id.clone()),
                    text: TerminalText::new(format!(
                        "{} {} {} / {}: {}",
                        match s.source {
                            ScheduleSource::Heartbeat => strings::HEARTBEAT,
                            ScheduleSource::Cron => strings::CRON,
                        },
                        s.agent.plain(),
                        s.next_run.plain(), strings::LAST_RESULT,
                        crate::blocks::field(&s.detail, "last_status")
                    )),
                })
                .collect(),
            app,
            settings,
            stale_schedules,
        );
    }
}
