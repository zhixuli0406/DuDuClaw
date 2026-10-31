use super::{
    app::App,
    text::SafeText,
    widgets::{panel, style, text},
};
use crate::{
    blocks::{Block, Pane, field},
    sanitize::TerminalText,
    settings::Settings,
    state::Model,
    strings,
};
use ratatui::{Frame, layout::Rect};

pub(super) fn content(pane: &Pane, model: &Model, logs: &crate::blocks::LogTail) -> TerminalText {
    let d = &pane.data;
    let mut lines = Vec::new();
    if d.stale {
        lines.push(strings::STALE.into());
    }
    if let Some(error) = &d.error {
        lines.push(error.plain());
    }
    match pane.kind {
        Block::Run => {
            lines.push(
                if d.subscribed {
                    strings::WATCH_SUBSCRIBED
                } else if d.subscription_checked {
                    strings::WATCH_POLLING
                } else {
                    strings::RUN_WAIT
                }
                .into(),
            );
            if d.runtime.as_ref().is_some_and(|r| !r.is("claude"))
                && !d.section("events").as_array().is_some_and(|events| {
                    events.iter().any(|e| {
                        matches!(
                            e["kind"].as_str(),
                            Some("tool_step" | "tool_use" | "todo_update")
                        )
                    })
                })
            {
                lines.push(strings::RUNTIME_ENDPOINTS.into());
            }
            lines.push(strings::RUN_KEYS.into());
            lines.push(if d.stale {
                "?".into()
            } else {
                format!(
                    "{}/{}",
                    pane.index.min(d.total.saturating_sub(1)) + usize::from(d.total > 0),
                    d.total
                )
            });
            if d.total >= 50 {
                lines.push(strings::RUN_LIMIT.into());
            }
            if !d.stale && d.selected.is_null() {
                lines.push(strings::EMPTY_RUNS.into());
            } else {
                lines.push(crate::watch::format_run(&d.selected, pane.content));
            }
            if let Some(events) = d.section("events").as_array() {
                for event in events {
                    lines.push(crate::watch::format_event(event, pane.content));
                }
            }
        }
        Block::Task if pane.responsibilities => {
            lines.push(strings::RESP_LIST.into());
            lines.push(strings::ROW_KEYS.into());
            if d.section("summary")["summary"]["open_occurrence"]["task_id"]
                .as_str()
                .is_some()
            {
                lines.push(strings::OPEN_OCCURRENCE.into());
            }
            lines.push(if d.stale {
                "?".into()
            } else {
                format!(
                    "{}/{}",
                    pane.index.min(d.total.saturating_sub(1)) + usize::from(d.total > 0),
                    d.total
                )
            });
            for key in [
                "responsibility_id",
                "objective",
                "state",
                "owner_agent_id",
                "stop_at",
            ] {
                lines.push(format!(
                    "{}: {}",
                    strings::field_label(key),
                    field(&d.selected, key)
                ));
            }
            lines.push(format!(
                "{}: {}",
                strings::NEXT_RUNS,
                field(&d.section("summary")["summary"], "next_due_at")
            ));
            if let Some(fires) = d.section("fires")["fires"].as_array() {
                for fire in fires {
                    lines.push(format!(
                        "{} {} {}",
                        field(fire, "observed_at"),
                        field(fire, "state"),
                        field(fire, "reason")
                    ));
                }
            }
            if super::m2::operator(model, d.selected["owner_agent_id"].as_str()) && !d.stale {
                lines.push(strings::RESP_KEYS.into());
            } else {
                lines.push(strings::CARD_NO_PERMISSION.into());
            }
        }
        Block::Task => {
            lines.push(strings::TASK_KEYS.into());
            lines.push(if d.stale {
                "?".into()
            } else {
                format!(
                    "{}/{}",
                    pane.index.min(d.total.saturating_sub(1)) + usize::from(d.total > 0),
                    d.total
                )
            });
            for key in [
                "title",
                "status",
                "assigned_to",
                "pause_reason",
                "updated_at",
            ] {
                lines.push(format!(
                    "{}: {}",
                    strings::field_label(key),
                    field(&d.selected, key)
                ));
            }
            if super::m2::operator(model, d.selected["assigned_to"].as_str())
                && !d.stale
                && d.selected["kind"] != "discovery"
            {
                if d.selected["goal_mode"] == true
                    && !matches!(
                        d.selected["status"].as_str(),
                        Some("done" | "cancelled" | "failed")
                    )
                {
                    lines.push(strings::TASK_STEER_KEY.into());
                }
                if d.section("stop")["authority_revision"].as_i64().is_some() {
                    lines.push(strings::TASK_STOP_KEY.into());
                }
            } else {
                lines.push(strings::CARD_NO_PERMISSION.into());
            }
            lines.push(strings::ITERATIONS.into());
            if let Some(rows) = d.section("iterations")["iterations"].as_array() {
                for row in rows {
                    lines.push(format!(
                        "{} {} {}",
                        field(row, "round"),
                        field(row, "verdict"),
                        field(row, "dispatched_at")
                    ));
                }
            }
            lines.push(strings::TIMELINE.into());
            if let Some(rows) = d.section("timeline")["activity"].as_array() {
                // Never expose activity metadata or summaries that can embed tool payloads.
                for row in rows {
                    lines.push(format!(
                        "{} {}",
                        field(row, "timestamp"),
                        field(row, "type")
                    ));
                }
            }
            lines.push(strings::CHANGES.into());
            if let Some(rows) = d.section("changes")["changes"].as_array() {
                for row in rows {
                    lines.push(format!("{} {}", field(row, "op"), field(row, "path")));
                }
            }
            if d.section("changes")["truncated"] == true {
                lines.push(strings::BLOCK_TRUNCATED.into());
            }
        }
        Block::Log => {
            if !super::m2::manager(model) {
                lines.push(strings::MANAGER_ONLY.into());
            } else {
                if logs.lost {
                    lines.push(strings::LOG_GAP.into());
                }
                lines.extend(logs.lines.iter().map(TerminalText::plain));
            }
        }
        _ => {}
    }
    TerminalText::new(lines.join("\n"))
}
pub(super) fn draw(
    frame: &mut Frame,
    area: Rect,
    model: &Model,
    app: &mut App,
    settings: &Settings,
    detail_stale: bool,
) {
    let (regions, hidden) = app.blocks.areas(area);
    let content_area = if hidden > 0 {
        text(
            frame,
            Rect::new(area.x, area.y, area.width, 1),
            TerminalText::new(strings::hidden_blocks(hidden)),
            settings,
            false,
            false,
        );
        Rect::new(
            area.x,
            area.y + 1,
            area.width,
            area.height.saturating_sub(1),
        )
    } else {
        area
    };
    let regions = if hidden > 0 {
        vec![(app.blocks.focused, content_area)]
    } else {
        regions
    };
    for (index, region) in regions {
        let kind = app.blocks.panes[index].kind;
        let title = format!(
            "{}{} {}",
            if index == app.blocks.focused {
                "> "
            } else {
                ""
            },
            index + 1,
            kind.label()
        );
        if kind == Block::Chat && !(app.help && index == app.blocks.focused) {
            super::chat_render::draw(frame, region, app, settings, &title);
            continue;
        }
        if kind == Block::Pty && !(app.help && index == app.blocks.focused) {
            super::pty_render::draw(frame, region, &app.blocks.panes[index], settings, &title, app.pty_focus && index == app.blocks.focused);
            continue;
        }
        let pane = &app.blocks.panes[index];
        let stale = if app.help && index == app.blocks.focused {
            false
        } else if kind == Block::Detail {
            detail_stale
        } else {
            pane.data.stale
        };
        let inner = panel(frame, region, &title, settings, stale);
        let body = if app.help && index == app.blocks.focused {
            TerminalText::new(strings::HELP.into())
        } else if kind == Block::Detail {
            app.selection_detail(model)
        } else {
            content(pane, model, &app.logs)
        };
        frame.render_widget(
            SafeText {
                text: &body,
                width: settings.ambiguous_width,
                visible: true,
                wrap: true,
                scroll: if app.help && index == app.blocks.focused {
                    app.help_scroll
                } else if kind == Block::Detail {
                    app.detail_scroll
                } else {
                    pane.scroll
                },
                style: style(stale),
            },
            inner,
        );
    }
}
