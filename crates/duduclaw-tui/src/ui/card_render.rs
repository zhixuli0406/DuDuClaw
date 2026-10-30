use super::{card::Card, text::{SafeText, wrapped_lines}, widgets::{panel, style}};
use crate::{settings::Settings, strings};
use ratatui::{Frame, layout::Rect};
use std::time::Instant;

pub(super) fn draw(frame: &mut Frame, area: Rect, card: &mut Card, settings: &Settings, fresh: bool) {
    draw_at(frame, area, card, settings, fresh, Instant::now(), crate::snooze::epoch_now());
}
pub(super) fn draw_at(frame: &mut Frame, area: Rect, card: &mut Card, settings: &Settings, fresh: bool, now: Instant, epoch: i64) {
    frame.render_widget(ratatui::widgets::Clear, area);
    let inner = panel(frame, area, strings::CARD_TITLE, settings, !fresh);
    let controls_height = 8.min(inner.height.saturating_sub(1));
    let body = Rect::new(inner.x, inner.y, inner.width, inner.height.saturating_sub(controls_height));
    // All payload fields are present; controls are separate and cannot be forged
    // by a payload that scrolls off screen. No server-derived links are opened.
    card.viewport(wrapped_lines(&card.body, body.width, settings.ambiguous_width), usize::from(body.height));
    frame.render_widget(SafeText { text: &card.body, width: settings.ambiguous_width,
        visible: true, wrap: true, scroll: card.scroll, style: style(!fresh) }, body);
    let controls = card.controls(now, epoch, fresh);
    frame.render_widget(SafeText { text: &controls, width: settings.ambiguous_width,
        visible: true, wrap: true, scroll: 0, style: style(!fresh) },
        Rect::new(inner.x, body.bottom(), inner.width, controls_height));
}
