use super::{
    app::App,
    text::{SafeText, wrapped_lines},
    widgets::{panel, style, text},
};
use crate::{
    sanitize::TerminalText,
    settings::{Glyphs, Settings},
    strings,
};
use ratatui::{Frame, layout::Rect};
pub(super) fn draw(frame: &mut Frame, area: Rect, app: &mut App, settings: &Settings) {
    let inside = panel(frame, area, strings::CHAT_TITLE, settings, app.chat.stale);
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
    let transcript = TerminalText::new(
        app.chat
            .lines
            .iter()
            .map(TerminalText::plain)
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let max_scroll = wrapped_lines(&transcript, body.width, settings.ambiguous_width)
        .saturating_sub(usize::from(body.height));
    app.chat_scroll = app.chat_scroll.min(max_scroll);
    frame.render_widget(
        SafeText {
            text: &transcript,
            width: settings.ambiguous_width,
            visible: false,
            wrap: true,
            scroll: max_scroll.saturating_sub(app.chat_scroll),
            style: style(app.chat.stale),
        },
        body,
    );
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
