use super::{
    app::App,
    text::SafeText,
    widgets::{panel, style, text},
};
use crate::{
    sanitize::TerminalText,
    settings::{Glyphs, Settings},
    strings,
};
use ratatui::{Frame, layout::Rect};
use crate::settings::AmbiguousWidth;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Default, Clone)]
pub(super) struct TranscriptCache {
    key: Option<(u16, AmbiguousWidth, u64)>,
    lines: Vec<TerminalText>,
}
impl TranscriptCache {
    fn update(&mut self, chat: &crate::chat::ChatState, columns: u16, width: AmbiguousWidth) {
        let key = (columns, width, chat.revision);
        if self.key == Some(key) { return; }
        self.key = Some(key);
        self.lines.clear();
        if columns == 0 { return; }
        for source in &chat.lines {
            let mut line = String::new();
            let mut used = 0;
            for g in source.plain().graphemes(true) {
                let n = match width { AmbiguousWidth::Narrow => g.width(), AmbiguousWidth::Wide => g.width_cjk() };
                if n == 0 || n > usize::from(columns) { continue; }
                if used + n > usize::from(columns) {
                    self.lines.push(TerminalText::new(std::mem::take(&mut line)));
                    used = 0;
                }
                line.push_str(g);
                used += n;
            }
            self.lines.push(TerminalText::new(line));
        }
    }
}

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &mut App, settings: &Settings, title: &str) {
    let inside = panel(frame, area, title, settings, app.chat.stale);
    if inside.is_empty() {
        return;
    }
    let spinner = if app.chat.running {
        match settings.glyphs {
            Glyphs::Ascii => ["|", "/", "-", "\\"][app.spinner % 4],
            Glyphs::Unicode => ["◐", "◓", "◑", "◒"][app.spinner % 4],
        }
    } else {
        ""
    };
    text(
        frame,
        Rect::new(inside.x, inside.y, inside.width, 1),
        TerminalText::new(format!(
            "{} {} {}",
            spinner,
            app.chat.status.plain(),
            if app.chat.stale { strings::STALE } else { "" }
        )),
        settings,
        app.chat.stale,
        false,
    );
    let body = Rect::new(
        inside.x,
        inside.y + 1,
        inside.width,
        inside.height.saturating_sub(2),
    );
    // Reserve real terminal columns for the gutter; content never renders there,
    // including continuation lines and text that imitates a decision card.
    let gutter = 2.min(body.width);
    let columns = body.width.saturating_sub(gutter);
    app.chat_cache.update(&app.chat, columns, settings.ambiguous_width);
    let max_scroll = app.chat_cache.lines.len().saturating_sub(usize::from(body.height));
    app.chat_scroll = app.chat_scroll.min(max_scroll);
    for (row, line) in app.chat_cache.lines.iter()
        .skip(max_scroll.saturating_sub(app.chat_scroll))
        .take(usize::from(body.height)).enumerate() {
        let y = body.y + row as u16;
        text(frame, Rect::new(body.x, y, gutter, 1),
            TerminalText::new(strings::CHAT_GUTTER.into()), settings, app.chat.stale, false);
        frame.render_widget(SafeText { text: line, width: settings.ambiguous_width,
            visible: false, wrap: false, scroll: 0, style: style(app.chat.stale) },
            Rect::new(body.x + gutter, y, columns, 1));
    }
    if app.chat.running && inside.height > 1 {
        text(
            frame,
            Rect::new(inside.x, inside.bottom() - 1, inside.width, 1),
            TerminalText::new(
                if app.chat.can_cancel() {
                    strings::CHAT_CANCEL_HINT
                } else {
                    strings::CHAT_CANCEL_UNAVAILABLE
                }
                .into(),
            ),
            settings,
            false,
            false,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cache_tracks_width_revision_and_ambiguous_width() {
        let mut chat = crate::chat::ChatState::default();
        chat.append("A", "界abcdef");
        let mut cache = TranscriptCache::default();
        cache.update(&chat, 5, AmbiguousWidth::Narrow);
        let original = cache.lines.clone();
        let allocation = cache.lines.as_ptr();
        cache.update(&chat, 5, AmbiguousWidth::Narrow);
        assert_eq!(cache.lines.as_ptr(), allocation);
        assert_eq!(cache.lines, original);
        cache.update(&chat, 20, AmbiguousWidth::Narrow);
        assert!(cache.lines.len() < original.len());
        chat.append("B", "more");
        cache.update(&chat, 20, AmbiguousWidth::Narrow);
        assert_eq!(cache.lines.len(), 2);
        cache.update(&chat, 20, AmbiguousWidth::Wide);
        assert_eq!(cache.key, Some((20, AmbiguousWidth::Wide, chat.revision)));
    }
}
