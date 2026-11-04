//! Only sanitized screen cells cross into the host backend. Never VT formatted bytes.
use crate::{blocks::Pane, sanitize::TerminalText, settings::Settings};
use ratatui::{
    Frame,
    buffer::{Buffer, CellDiffOption},
    layout::Rect,
    style::{Color, Modifier, Style},
};
use std::num::NonZeroU16;
fn color(color: vt100::Color) -> Color {
    match color {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}
/// Map rendered host columns to one-based VT columns, including wide-cell tails.
pub(super) fn host_to_vt_columns(screen: &crate::pty::Screen, row: u16, columns: u16, settings: &Settings) -> Vec<u16> {
    let mut mapping = Vec::new();
    for col in 0..screen.size().1 {
        let Some(cell) = screen.cell(row, col) else { break; };
        if cell.is_wide_continuation() { continue; }
        let safe = TerminalText::new(cell.contents().to_owned()).plain().replace('\n', "");
        let width = super::text::columns(if safe.is_empty() { " " } else { &safe }, settings.ambiguous_width);
        if mapping.len() + width > usize::from(columns) { break; }
        let vt_tail = u16::from(screen.cell(row, col + 1).is_some_and(|next| next.is_wide_continuation()));
        // Ambiguous-width expansion may use two host cells for one VT cell.
        mapping.extend((0..width).map(|offset| col + 1 + (offset as u16).min(vt_tail)));
    }
    mapping
}

pub(super) fn cells(
    screen: &crate::pty::Screen,
    area: Rect,
    buffer: &mut Buffer,
    settings: &Settings,
    cursor: bool,
) {
    for row in 0..area.height.min(screen.size().0) {
        let mut x = area.x;
        for col in 0..screen.size().1 {
            let Some(cell) = screen.cell(row, col) else {
                continue;
            };
            if cell.is_wide_continuation() {
                continue;
            }
            let safe = TerminalText::new(cell.contents().to_owned())
                .plain()
                .replace('\n', "");
            let symbol = if safe.is_empty() { " " } else { &safe };
            let width = super::text::columns(symbol, settings.ambiguous_width);
            let Some(width) = u16::try_from(width).ok().and_then(NonZeroU16::new) else {
                continue;
            };
            if x.saturating_add(width.get()) > area.right() {
                break;
            }
            let mut style = Style::default()
                .fg(crate::theme::adapt(color(cell.fgcolor()), settings.colors.capability))
                .bg(crate::theme::adapt(color(cell.bgcolor()), settings.colors.capability));
            if cell.inverse() { style = style.add_modifier(Modifier::REVERSED); }
            if cell.bold() {
                style = style.add_modifier(Modifier::BOLD);
            }
            // A drawn cursor belongs to the local model, never the host cursor.
            if cursor
                && !screen.hide_cursor()
                && screen.scrollback() == 0
                && screen.cursor_position() == (row, col)
            {
                style = if cell.inverse() { style.remove_modifier(Modifier::REVERSED) }
                    else { style.add_modifier(Modifier::REVERSED) };
            }
            buffer[(x, area.y + row)]
                .set_symbol(symbol)
                .set_style(style)
                .set_diff_option(CellDiffOption::ForcedWidth(width));
            for dx in 1..width.get() {
                buffer[(x + dx, area.y + row)].reset();
                buffer[(x + dx, area.y + row)].set_style(style);
            }
            x += width.get();
        }
    }
}
pub(super) fn draw(
    frame: &mut Frame,
    area: Rect,
    pane: &mut Pane,
    settings: &Settings,
    title: &str,
    focused: bool,
) {
    pane.pty_area = Rect::default();
    let waiting = pane.pty.as_ref().is_some_and(|p| p.lock().unwrap_or_else(|e| e.into_inner()).waiting());
    let title = if waiting { format!("{title} · {}", crate::strings::PTY_WAITING) } else { title.into() };
    let inner = super::widgets::panel_focus(frame, area, &title, settings, false, focused);
    if inner.is_empty() {
        return;
    }
    let Some(pty) = &pane.pty else {
        return;
    };
    let mut pty = pty.lock().unwrap_or_else(|e| e.into_inner());
    let screen_area = Rect::new(
        inner.x,
        inner.y,
        inner.width,
        inner.height.saturating_sub(1),
    );
    pane.pty_area = screen_area;
    if !screen_area.is_empty() {
        pty.resize(screen_area.height, screen_area.width);
        cells(
            pty.screen(),
            screen_area,
            frame.buffer_mut(),
            settings,
            focused,
        );
    }
    let status = if pty.screen().scrollback() > 0 {
        format!(
            "{} | PgUp/PgDn -{}",
            pty.status,
            pty.screen().scrollback()
        )
    } else {
        pty.status.clone()
    };
    super::widgets::text(
        frame,
        Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
        TerminalText::new(status),
        settings,
        false,
        false,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::Backend;
    #[test]
    fn no_color_preserves_inverse_and_cursor_without_color() {
        let mut parser = vt100::Parser::new(2, 20, 0);
        parser.process(b"\x1b[38;2;200;30;40m\x1b[7mA\x1b[0mB");
        let settings = Settings { colors: crate::theme::Theme::new(crate::theme::ThemeMode::Dark, crate::theme::Capability::NoColor), ..Settings::test_defaults() };
        let mut buffer = Buffer::empty(Rect::new(0,0,20,2));
        cells(&crate::pty::Screen::capture(parser.screen()), buffer.area, &mut buffer, &settings, true);
        assert!(buffer.content.iter().all(|cell| cell.fg == Color::Reset && cell.bg == Color::Reset));
        assert!(buffer[(0,0)].modifier.contains(Modifier::REVERSED));
        assert!(!buffer[(1,0)].modifier.contains(Modifier::REVERSED));
        assert!(buffer[(2,0)].modifier.contains(Modifier::REVERSED));
        let mut backend = super::super::backend::ScreenBackend(Vec::new());
        backend.draw(buffer.content.iter().enumerate().map(|(i,c)| ((i % 20) as u16,(i / 20) as u16,c))).unwrap();
        let mut host = vt100::Parser::new(2,20,0); host.process(&backend.0);
        assert!(host.screen().cell(0,0).unwrap().inverse());
        assert!(host.screen().cell(0,2).unwrap().inverse());
    }
    #[test]
    fn hostile_child_bytes_never_reach_host_backend() {
        let mut parser = vt100::Parser::new(4, 40, 10);
        parser.process(b"ok\x1b]52;c;CLIP_CANARY\x07\x1b]0;TITLE_CANARY\x07\x1b]2;TITLE2_CANARY\x1b\\\x1b]8;;LINK_CANARY\x1b\\link\x1b]8;;\x1b\\\x1bPqSIXEL_CANARY\x1b\\\x1bP1;2|DCS_CANARY\x1b\\\x1b[?2004h\x1b[1;31mRED");
        parser.process("\u{202e}\u{200b}e\u{301}".as_bytes());
        let mut buffer = Buffer::empty(Rect::new(0, 0, 40, 4));
        cells(
            &crate::pty::Screen::capture(parser.screen()),
            buffer.area,
            &mut buffer,
            &Settings::default(),
            false,
        );
        let mut backend = super::super::backend::ScreenBackend(Vec::new());
        backend
            .draw(
                buffer
                    .content
                    .iter()
                    .enumerate()
                    .map(|(i, c)| ((i % 40) as u16, (i / 40) as u16, c)),
            )
            .unwrap();
        let output = String::from_utf8(backend.0).unwrap();
        for forbidden in [
            "CANARY",
            "\x1b]",
            "\x1bP",
            "\x1b[?2004h",
            "\u{202e}",
            "\u{200b}",
        ] {
            assert!(!output.contains(forbidden), "{forbidden:?}");
        }
        assert!(output.contains("e\u{301}"));
        // The only text emitted is sanitized cells; all escape sequences belong
        // to our own backend's position/color/bold drawing protocol.
        let mut rendered = vt100::Parser::new(4, 40, 0);
        rendered.process(output.as_bytes());
        assert_eq!(rendered.screen().contents().trim_end(), "oklinkREDe\u{301}");
        assert!(rendered.screen().cell(0, 6).unwrap().bold());
    }
}
