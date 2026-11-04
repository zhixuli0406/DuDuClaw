//! Dashboard-aligned chrome. All strings cross the same width-aware text renderer.
use super::{app::{App, Selection}, panels::line, text::columns, widgets};
use crate::{client::ConnectionState, sanitize::TerminalText, settings::{Glyphs, Settings}, state::Model, strings};
use ratatui::{Frame, layout::Rect};

pub(super) fn sidebar(frame: &mut Frame, area: Rect, model: &Model, connection: &ConnectionState, app: &App, settings: &Settings) {
    if area.width < 4 || area.height < 5 { return; }
    let settings = &Settings { colors: settings.colors.on(settings.colors.app_shell), ..settings.clone() };
    let inner = Rect::new(area.x + 1, area.y, area.width.saturating_sub(2), area.height);
    let colors = settings.colors;
    let unicode = settings.glyphs == Glyphs::Unicode;
    let brand = if unicode { "◆" } else { "*" };
    let brand_width = columns(brand, settings.ambiguous_width) as u16;
    line(frame, Rect::new(inner.x, inner.y, brand_width, 1), brand.into(), settings, colors.foreground(colors.brand));
    line(frame, Rect::new(inner.x + brand_width + 1, inner.y, inner.width.saturating_sub(brand_width + 1), 1),
        "DuDuClaw".into(), settings, colors.base());
    if inner.width >= 23 {
        line(frame, Rect::new(inner.right() - 6, inner.y, 6, 1), "控制室".into(), settings, colors.muted());
    }
    let search = Rect::new(inner.x, inner.y + 2, inner.width, 1);
    frame.buffer_mut().set_style(search, colors.on(colors.surface).base());
    let hint_width = 6.min(search.width);
    line(frame, Rect::new(search.x, search.y, search.width.saturating_sub(hint_width + 1), 1),
        if unicode { "⌕ 搜尋員工或指令 …" } else { "/ 搜尋員工或指令 ..." }.into(), settings,
        colors.on(colors.surface).muted());
    line(frame, Rect::new(search.right() - hint_width, search.y, hint_width, 1), "Ctrl-P".into(), settings,
        colors.on(colors.surface).muted());

    let (status, connected) = match connection {
        ConnectionState::Connecting => (strings::CONNECTING, false),
        ConnectionState::Disconnected | ConnectionState::LoginRequired(_) => (strings::DISCONNECTED, false),
        ConnectionState::Connected(_) => (strings::CONNECTED, true),
    };
    let dot = if unicode { "●" } else { "*" };
    let dot_width = columns(dot, settings.ambiguous_width) as u16;
    // The model records permission/refusal as None, never a fabricated $0.00.
    let cost = model.cost_millicents.filter(|_| model.role == Some(crate::auth::Role::Admin))
        .map(|cost| if model.cost_today { strings::cost_today(cost) } else { strings::cost_window(cost) });
    let connection_width = dot_width + 1 + columns(status, settings.ambiguous_width) as u16;
    let narrow = cost.as_ref().is_some_and(|cost| columns(cost, settings.ambiguous_width) + usize::from(connection_width + 1) > usize::from(inner.width));
    let footer_height = (3 + u16::from(super::m2::manager(model)) + u16::from(narrow)).min(inner.height.saturating_sub(4));
    let footer_y = inner.bottom() - footer_height;
    super::panels::draw(frame, Rect::new(inner.x, inner.y + 4, inner.width, footer_y.saturating_sub(inner.y + 4)), model, app, settings);
    nav_row(frame, Rect::new(inner.x, footer_y, inner.width, 1), "I 收件匣", model.inbox.count(),
        matches!(app.selected, Some(Selection::InboxEntry | Selection::Inbox(_))), true, settings);
    let mut y = footer_y + 1;
    if super::m2::manager(model) && y < inner.bottom() {
        nav_row(frame, Rect::new(inner.x, y, inner.width, 1), "S 排程", model.schedules.count(),
            matches!(app.selected, Some(Selection::ScheduleEntry | Selection::Schedule(_))), false, settings);
        y += 1;
    }
    if y < inner.bottom() { widgets::separator(frame, Rect::new(inner.x, y, inner.width, 1), settings); y += 1; }
    if y >= inner.bottom() { return; }
    line(frame, Rect::new(inner.x, y, dot_width, 1), dot.into(), settings,
        colors.foreground(if connected { colors.success } else { colors.warning }));
    line(frame, Rect::new(inner.x + dot_width + 1, y, inner.width.saturating_sub(dot_width + 1), 1),
        status.into(), settings, colors.muted());
    if let Some(cost) = cost {
        let cost = if columns(&cost, settings.ambiguous_width) > usize::from(inner.width) {
            let prefix = if model.cost_today { "今日 $" } else { "24h $" };
            let amount = cost.split_once('$').map(|(_,amount)| amount).unwrap_or("");
            let budget = usize::from(inner.width).saturating_sub(columns(prefix, settings.ambiguous_width));
            format!("{prefix}{}", super::text::middle_ellipsis(amount, budget, settings.ambiguous_width))
        } else { cost };
        let width = columns(&cost, settings.ambiguous_width) as u16;
        let cost_y = if narrow { y + 1 } else { y };
        if cost_y < inner.bottom() {
            line(frame, Rect::new(inner.right() - width, cost_y, width, 1), cost, settings, colors.muted());
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn nav_row(frame: &mut Frame, area: Rect, label: &str, count: Option<usize>, selected: bool, pill: bool, settings: &Settings) {
    if area.is_empty() { return; }
    let base = if selected { settings.colors.selected() } else { settings.colors.muted() };
    frame.buffer_mut().set_style(area, base);
    let value = format!(" {} ", count.map(|n| n.to_string()).unwrap_or_else(|| "?".into()));
    let width = (columns(&value, settings.ambiguous_width) as u16).min(area.width);
    line(frame, Rect::new(area.x, area.y, area.width.saturating_sub(width + 1), 1), label.into(), settings, base);
    let style = if pill { settings.colors.on(settings.colors.hover).base() } else { base };
    line(frame, Rect::new(area.right() - width, area.y, width, 1), value, settings, style);
}

pub(super) fn version(connection: &ConnectionState) -> Option<String> {
    match connection {
        ConnectionState::Connected(info) => info.version.as_ref().map(TerminalText::plain)
            .filter(|v| !v.trim().is_empty()).map(|v| {
                let v = v.replace('\n', " ");
                if v.starts_with('v') { v } else { format!("v{v}") }
            }),
        _ => None,
    }
}

/// Shared with the command list where these navigation commands are declared.
pub(super) const HELP_KEY: &str = "?";
pub(super) const ZOOM_KEY: &str = "z";
pub(super) fn idle_hints(app: &App, width: u16) -> String {
    if width < 26 { return format!("{HELP_KEY} {ZOOM_KEY} q"); }
    if width < 38 { return format!("{HELP_KEY} 說明  {ZOOM_KEY} {}  q 離開", if app.zoomed { "還原" } else { "放大" }); }
    let pause = if matches!(app.selected, Some(Selection::Employee(_)))
        && app.blocks.active != crate::blocks::Block::Task { "  p 暫停員工" } else { "" };
    format!("{HELP_KEY} 快捷鍵{pause}  {ZOOM_KEY} {}  q 離開", if app.zoomed { "還原" } else { "放大" })
}
