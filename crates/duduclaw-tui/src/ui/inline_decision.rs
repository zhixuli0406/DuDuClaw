//! Presentation only: Enter opens Card::open; numbered labels never send a decision.
use super::{app::{App, Selection}, text::{columns, middle_ellipsis}, widgets::styled};
use crate::{blocks::Block, sanitize::TerminalText, settings::Settings, state::{InboxItem, Model}};
use ratatui::{Frame, layout::Rect};

pub(super) fn item<'a>(app: &App, model: &'a Model) -> Option<&'a InboxItem> {
    if app.list_view() || app.blocks.active != Block::Chat { return None; }
    let Some(Selection::Employee(agent)) = &app.selected else { return None; };
    if app.blocks.panes[app.blocks.focused].agent.as_ref().is_some_and(|id| id != agent) { return None; }
    model.inbox.items.iter().find(|item| item.agent().is(agent) && match item {
        InboxItem::Approval(a) => a.status.is("pending"),
        InboxItem::Task(t) => t.goal_mode && t.status.is("needs_human"),
        InboxItem::Breaker(_) => false,
    })
}
pub(super) fn draw(frame: &mut Frame, area: Rect, item: &InboxItem, settings: &Settings) {
    if area.is_empty() { return; }
    let colors = settings.colors.on(settings.colors.surface_raised);
    frame.buffer_mut().set_style(area, colors.base());
    let inner = Rect::new(area.x + 1, area.y, area.width.saturating_sub(2), area.height);
    let (badge, actions) = match item {
        InboxItem::Approval(_) => (" 待審 ", ["1 核准", "2 拒絕", "3 檢視內容"]),
        InboxItem::Task(_) if item.data()["plan_pending"].as_str().is_some_and(|s| !s.is_empty()) =>
            (" 暫停 ", ["1 核准計畫", "2 中止", "3 檢視選項"]),
        _ => (" 暫停 ", ["1 重試", "2 中止", "3 檢視選項"]),
    };
    let badge_width = columns(badge, settings.ambiguous_width) as u16;
    styled(frame, Rect::new(inner.x, inner.y, badge_width.min(inner.width), 1), badge, settings, colors.warning_pill(true));
    let title = TerminalText::new(match item {
        InboxItem::Approval(a) => if a.request_kind.as_ref().is_some_and(|kind| kind.is("question")) {
            "回覆問題".into()
        } else { "審核操作".into() },
        _ => item.title().plain().replace('\n', " "),
    });
    styled(frame, Rect::new(inner.x + (badge_width + 1).min(inner.width), inner.y, inner.width.saturating_sub(badge_width + 1), 1), &title.plain(), settings, colors.base());
    if inner.height > 1 {
        let data = item.data();
        let summary = match item {
            InboxItem::Approval(a) => a.summary.plain(),
            InboxItem::Task(t) => t.pause_reason.as_ref().map(TerminalText::plain)
                .unwrap_or_else(|| data["summary"].as_str().unwrap_or("等待你決定下一步").into()),
            _ => String::new(),
        };
        let safe = TerminalText::new(summary).plain().replace('\n', " ");
        styled(frame, Rect::new(inner.x, inner.y + 1, inner.width, 1), &middle_ellipsis(&safe, usize::from(inner.width), settings.ambiguous_width), settings, colors.muted());
    }
    if inner.height > 2 {
        let mut x = inner.x;
        for (index, label) in actions.iter().enumerate() {
            let label = format!(" {label} ");
            let width = columns(&label, settings.ambiguous_width) as u16;
            if x + width > inner.right() { break; }
            let style = match index { 0 => colors.primary(), 1 => colors.on(colors.hover).base(), _ => colors.base() };
            styled(frame, Rect::new(x, inner.y + 2, width, 1), &label, settings, style);
            x += width + 1;
        }
    }
    if inner.height > 3 {
        styled(frame, Rect::new(inner.x, inner.y + 3, inner.width, 1), "Enter 開啟決策卡 · 數字為選項標籤", settings, colors.muted());
    }
}
