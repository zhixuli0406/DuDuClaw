//! Minimal full-screen crossterm adapter, authored here. Explicit cursor moves
//! honor the widget's ForcedWidth cells, including East Asian ambiguous glyphs.
use crossterm::{
    cursor, queue,
    style::{self, Attribute},
    terminal,
};
use ratatui::{
    backend::{Backend, ClearType, WindowSize},
    buffer::Cell,
    layout::{Position, Size},
    style::{Color, Modifier},
};
use std::io::{self, Write};

pub struct ScreenBackend<W: Write>(pub W);
fn color(c: Color) -> style::Color {
    match c {
        Color::Reset => style::Color::Reset,
        Color::Black => style::Color::Black,
        Color::Red => style::Color::DarkRed,
        Color::Green => style::Color::DarkGreen,
        Color::Yellow => style::Color::DarkYellow,
        Color::Blue => style::Color::DarkBlue,
        Color::Magenta => style::Color::DarkMagenta,
        Color::Cyan => style::Color::DarkCyan,
        Color::Gray => style::Color::Grey,
        Color::DarkGray => style::Color::DarkGrey,
        Color::LightRed => style::Color::Red,
        Color::LightGreen => style::Color::Green,
        Color::LightYellow => style::Color::Yellow,
        Color::LightBlue => style::Color::Blue,
        Color::LightMagenta => style::Color::Magenta,
        Color::LightCyan => style::Color::Cyan,
        Color::White => style::Color::White,
        Color::Rgb(r, g, b) => style::Color::Rgb { r, g, b },
        Color::Indexed(n) => style::Color::AnsiValue(n),
    }
}
impl<W: Write> Backend for ScreenBackend<W> {
    type Error = io::Error;
    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        for (x, y, cell) in content {
            queue!(
                self.0,
                cursor::MoveTo(x, y),
                style::SetAttribute(Attribute::Reset),
                style::SetForegroundColor(color(cell.fg)),
                style::SetBackgroundColor(color(cell.bg))
            )?;
            if cell.modifier.contains(Modifier::BOLD) { queue!(self.0, style::SetAttribute(Attribute::Bold))?; }
            queue!(self.0,
                style::Print(cell.symbol())
            )?;
        }
        Ok(())
    }
    fn hide_cursor(&mut self) -> io::Result<()> {
        queue!(self.0, cursor::Hide)
    }
    fn show_cursor(&mut self) -> io::Result<()> {
        queue!(self.0, cursor::Show)
    }
    fn get_cursor_position(&mut self) -> io::Result<Position> {
        cursor::position().map(Into::into)
    }
    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        let p = position.into();
        queue!(self.0, cursor::MoveTo(p.x, p.y))
    }
    fn clear(&mut self) -> io::Result<()> {
        self.clear_region(ClearType::All)
    }
    fn clear_region(&mut self, kind: ClearType) -> io::Result<()> {
        let kind = match kind {
            ClearType::All => terminal::ClearType::All,
            ClearType::AfterCursor => terminal::ClearType::FromCursorDown,
            ClearType::BeforeCursor => terminal::ClearType::FromCursorUp,
            ClearType::CurrentLine => terminal::ClearType::CurrentLine,
            ClearType::UntilNewLine => terminal::ClearType::UntilNewLine,
        };
        queue!(self.0, terminal::Clear(kind))
    }
    fn size(&self) -> io::Result<Size> {
        terminal::size().map(|(w, h)| Size::new(w, h))
    }
    fn window_size(&mut self) -> io::Result<WindowSize> {
        let s = terminal::window_size()?;
        Ok(WindowSize {
            columns_rows: Size::new(s.columns, s.rows),
            pixels: Size::new(s.width, s.height),
        })
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}
