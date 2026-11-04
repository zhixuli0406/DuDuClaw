use super::{app::App, text::{SafeText, columns, middle_ellipsis}, widgets::section};
use crate::{sanitize::TerminalText, settings::{AmbiguousWidth, Settings}, strings};
use ratatui::{Frame, layout::Rect, style::Style};
use unicode_segmentation::UnicodeSegmentation;

pub(super) fn wrap(value: &TerminalText, budget: u16, width: AmbiguousWidth) -> Vec<TerminalText> {
    if budget == 0 { return Vec::new(); }
    let mut result = Vec::new();
    for source in value.plain().split('\n') {
        let mut line = String::new();
        let mut used = 0;
        for g in source.graphemes(true) {
            let n = columns(g, width);
            if n == 0 || n > usize::from(budget) { continue; }
            if used + n > usize::from(budget) {
                result.push(TerminalText::new(std::mem::take(&mut line)));
                used = 0;
            }
            line.push_str(g); used += n;
        }
        result.push(TerminalText::new(line));
    }
    result
}
#[derive(Clone)]
struct MessageLines {
    user: bool,
    label: TerminalText,
    time: String,
    turn: u64,
    lines: Vec<TerminalText>,
    width: u16,
}
#[derive(Default, Clone)]
pub(super) struct TranscriptCache {
    key: Option<(u16, AmbiguousWidth, u64)>,
    messages: Vec<MessageLines>,
}
impl TranscriptCache {
    fn update(&mut self, chat: &crate::chat::ChatState, available: u16, width: AmbiguousWidth) {
        let key = (available, width, chat.revision);
        if self.key == Some(key) { return; }
        self.key = Some(key);
        self.messages = chat.messages.iter().map(|message| {
            let budget = if message.user { available.saturating_mul(7) / 10 } else { available };
            let lines = wrap(&message.content, budget.saturating_sub(if message.user { 2 } else { 0 }), width);
            let measured = lines.iter().map(|l| columns(&l.plain(), width)).max().unwrap_or(0) as u16;
            MessageLines { user: message.user, label: message.label.clone(), time: message.time.clone(),
                turn: message.turn, lines, width: (measured + if message.user { 2 } else { 0 }).min(available) }
        }).collect();
    }
}
struct PaintLine { text: TerminalText, x: u16, width: u16, style: Style, glyph: Option<(&'static str, Style)>, state: String }
fn plain(text: TerminalText, width: u16, style: Style) -> PaintLine {
    PaintLine { text, x: 0, width, style, glyph: None, state: String::new() }
}
#[cfg(test)]
pub(super) fn draw(frame: &mut Frame, area: Rect, app: &mut App, settings: &Settings, title: &str) {
    draw_pane(frame, area, app, settings, title, app.blocks.focused, None);
}
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_pane(frame: &mut Frame, area: Rect, app: &mut App, settings: &Settings, title: &str, pane_index: usize, name: Option<&str>) {
    let inside = section(frame, area, title, settings, app.chat.stale, title.starts_with("> "));
    if inside.is_empty() { return; }
    let mut lines = Vec::new();
    let base = if app.chat.stale { settings.colors.muted() } else { settings.colors.base() };
    let width = inside.width;
    let now = std::time::Instant::now();
    let downstream = app.downstream_prompt(now).map(|prompt| {
        if columns(&prompt.plain(), settings.ambiguous_width) > usize::from(width) {
            app.downstream_prompt_compact(now).unwrap_or(prompt)
        } else { prompt }
    });
    if app.chat.stale || app.chat.running || app.chat.messages.is_empty() || downstream.is_some() {
        let status = if downstream.is_some() && app.focus == crate::chat::Focus::Input {
            TerminalText::new(strings::DOWNSTREAM_INPUT_HINT.into())
        } else { app.chat.status.clone() };
        lines.push(plain(status, width, settings.colors.muted()));
    }
    if let Some(prompt) = downstream {
        for line in wrap(&prompt, width, settings.ambiguous_width) { lines.push(plain(line, width, base)); }
    }
    let header_rows = lines.len().min(usize::from(inside.height));
    for (index, row) in lines.drain(..).take(header_rows).enumerate() {
        put(frame, Rect::new(inside.x, inside.y + index as u16, width, 1), &row.text, settings, row.style);
    }
    let inside = Rect::new(inside.x, inside.y + header_rows as u16, width, inside.height.saturating_sub(header_rows as u16));
    if inside.is_empty() { return; }
    super::timeline::reconcile_chat(&app.chat.activity, &mut app.blocks.panes[pane_index].timeline);
    app.chat_cache.update(&app.chat, width, settings.ambiguous_width);
    let messages = &app.chat_cache.messages;
    let mut selected_line = None;
    // A later system notice (for example downstream cancellation) must not steal
    // a completed employee message's tool card.
    let mut anchors = std::collections::HashMap::new();
    for (index, message) in messages.iter().enumerate() {
        let priority = if message.label.is(strings::CHAT_ASSISTANT) { 2 } else { u8::from(!message.user) };
        let anchor = anchors.entry(message.turn).or_insert((priority, index));
        if priority >= anchor.0 { *anchor = (priority, index); }
    }
    for (index, message) in messages.iter().enumerate() {
        if !message.user {
            let name = if message.label.is(strings::CHAT_ASSISTANT) {
                name.or(app.blocks.panes[pane_index].agent.as_deref()).unwrap_or(strings::CHAT_ASSISTANT).to_owned()
            } else { message.label.plain() };
            lines.push(plain(TerminalText::new(format!("{name} · {}", message.time)), width, settings.colors.muted()));
        }
        for line in &message.lines {
            let mut row = plain(line.clone(), if message.user { message.width } else { width },
                if message.user {
                    let style = settings.colors.on(settings.colors.hover).base();
                    if matches!(settings.colors.capability, crate::theme::Capability::Ansi16 | crate::theme::Capability::NoColor) {
                        style.add_modifier(ratatui::style::Modifier::REVERSED)
                    } else { style }
                } else { base });
            if message.user {
                row.x = width.saturating_sub(message.width);
                row.text = TerminalText::new(format!(" {} ", line.plain()));
            }
            lines.push(row);
        }
        if anchors.get(&message.turn).is_some_and(|(_, anchor)| *anchor == index) {
            let group = super::timeline::chat_lines(&app.chat.activity, &mut app.blocks.panes[pane_index].timeline,
                message.turn, width, settings, app.spinner, app.chat.running);
            if message.user && !group.is_empty() {
                lines.push(plain(TerminalText::new(format!("{} · {}", name.or(app.blocks.panes[pane_index].agent.as_deref()).unwrap_or(strings::CHAT_ASSISTANT), message.time)), width, settings.colors.muted()));
            }
            for step in group {
                if step.selected { selected_line = Some(lines.len()); }
                let colors = settings.colors.on(settings.colors.surface);
                let style = if step.selected { colors.selected() } else { colors.base() };
                lines.push(PaintLine { text: step.text, x: 0, width, style,
                    glyph: Some((step.glyph, style.fg(colors.readable(step.color, style.bg.unwrap_or(colors.surface))))), state: step.state });
            }
        }
        lines.push(plain(TerminalText::default(), width, base));
    }
    if messages.is_empty() {
        for line in wrap(&TerminalText::new(strings::EMPTY_CHAT.into()), width, settings.ambiguous_width) {
            lines.push(plain(line, width, settings.colors.muted()));
        }
    }
    let cancel = app.chat.running && inside.height > 1;
    let height = inside.height.saturating_sub(u16::from(cancel));
    let max_scroll = lines.len().saturating_sub(usize::from(height));
    app.chat_scroll = app.chat_scroll.min(max_scroll);
    let offset = app.blocks.panes[pane_index].timeline.conversation_offset(lines.len(), selected_line,
        usize::from(height), app.chat_scroll);
    for (index, row) in lines.iter().skip(offset).take(usize::from(height)).enumerate() {
        let area = Rect::new(inside.x + row.x, inside.y + index as u16, row.width, 1);
        frame.buffer_mut().set_style(area, row.style);
        if let Some((glyph, glyph_style)) = row.glyph {
            let glyph_width = columns(glyph, settings.ambiguous_width) as u16;
            let left = 1 + glyph_width + 1;
            let state_width = columns(&row.state, settings.ambiguous_width).min(usize::from(area.width / 2)) as u16;
            put(frame, Rect::new(area.x + 1.min(area.width), area.y, glyph_width.min(area.width.saturating_sub(1)), 1), &TerminalText::new(glyph.into()), settings, glyph_style);
            let budget = area.width.saturating_sub(left + state_width + 2);
            let target = TerminalText::new(middle_ellipsis(&row.text.plain(), usize::from(budget), settings.ambiguous_width));
            put(frame, Rect::new(area.x + left.min(area.width), area.y, budget, 1), &target, settings, row.style);
            put(frame, Rect::new(area.right().saturating_sub(state_width + 1), area.y, state_width, 1), &TerminalText::new(row.state.clone()), settings,
                settings.colors.on(row.style.bg.unwrap_or(settings.colors.surface)).muted());
        } else { put(frame, area, &row.text, settings, row.style); }
    }
    if cancel {
        put(frame, Rect::new(inside.x, inside.bottom() - 1, width, 1),
            &TerminalText::new(if app.chat.can_cancel() { strings::CHAT_CANCEL_HINT } else { strings::CHAT_CANCEL_UNAVAILABLE }.into()), settings, settings.colors.muted());
    }
}
fn put(frame: &mut Frame, area: Rect, text: &TerminalText, settings: &Settings, style: Style) {
    frame.render_widget(SafeText { text, width: settings.ambiguous_width, visible: false, wrap: false, scroll: 0, style }, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn user_blocks_wrap_cjk_by_display_width_and_cache_revision() {
        let mut chat = crate::chat::ChatState::default();
        chat.append("你", "界Ω訂單·資料".repeat(9).as_str());
        let mut cache = TranscriptCache::default();
        for width in [AmbiguousWidth::Narrow, AmbiguousWidth::Wide] {
            cache.update(&chat, 30, width);
            let message = &cache.messages[0];
            assert!(message.user);
            assert!(message.width <= 21);
            assert!(message.lines.iter().all(|line| columns(&line.plain(), width) <= 19));
            assert_eq!(message.lines.iter().map(TerminalText::plain).collect::<String>(), chat.messages[0].content.plain());
            let ptr = cache.messages.as_ptr();
            cache.update(&chat, 30, width);
            assert_eq!(ptr, cache.messages.as_ptr());
        }
        chat.append("員工", "已完成");
        cache.update(&chat, 30, AmbiguousWidth::Narrow);
        assert_eq!(cache.messages.len(), 2);
    }
}
