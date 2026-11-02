//! Translate host key events into child input, never host terminal output.
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers as M};
pub fn escape(key: KeyEvent) -> bool {
    key.kind != KeyEventKind::Release
        && ((key.modifiers == M::CONTROL
            && matches!(key.code, KeyCode::Char('\\' | '4' | '\u{1c}')))
            || (key.modifiers.is_empty()
                && matches!(key.code, KeyCode::F(12) | KeyCode::Char('\u{1c}'))))
}
pub fn key_bytes(key: KeyEvent, application_cursor: bool) -> Vec<u8> {
    if key.kind == KeyEventKind::Release {
        return vec![];
    }
    let modifiers = key.modifiers;
    let parameter = 1
        + u8::from(modifiers.contains(M::SHIFT))
        + 2 * u8::from(modifiers.contains(M::ALT))
        + 4 * u8::from(modifiers.contains(M::CONTROL));
    let cursor = |suffix: char| {
        if parameter > 1 {
            format!("\x1b[1;{parameter}{suffix}")
        } else {
            format!("\x1b{}{suffix}", if application_cursor { 'O' } else { '[' })
        }
    };
    let tilde = |n| {
        if parameter > 1 {
            format!("\x1b[{n};{parameter}~")
        } else {
            format!("\x1b[{n}~")
        }
    };
    let mut text = match key.code {
        KeyCode::Char(c) if modifiers.contains(M::CONTROL) => {
            let c = c.to_ascii_uppercase();
            match c {
                '@'..='_' => ((c as u8) & 0x1f).into(),
                ' ' | '2' => '\0',
                '3' => '\x1b',
                '4' => '\x1c',
                '5' => '\x1d',
                '6' => '\x1e',
                '7' => '\x1f',
                '8' | '?' => '\x7f',
                _ => c,
            }
            .to_string()
        }
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Enter => "\r".into(),
        KeyCode::Tab => "\t".into(),
        KeyCode::BackTab => "\x1b[Z".into(),
        KeyCode::Backspace => "\x7f".into(),
        KeyCode::Esc => "\x1b".into(),
        KeyCode::Up => cursor('A'),
        KeyCode::Down => cursor('B'),
        KeyCode::Right => cursor('C'),
        KeyCode::Left => cursor('D'),
        KeyCode::Home => cursor('H'),
        KeyCode::End => cursor('F'),
        KeyCode::Insert => tilde(2),
        KeyCode::Delete => tilde(3),
        KeyCode::PageUp => tilde(5),
        KeyCode::PageDown => tilde(6),
        KeyCode::F(n @ 1..=4) => {
            let suffix = char::from(b'P' + n - 1);
            if parameter > 1 {
                format!("\x1b[1;{parameter}{suffix}")
            } else {
                format!("\x1bO{suffix}")
            }
        }
        KeyCode::F(n @ 5..=12) => tilde([15, 17, 18, 19, 20, 21, 23, 24][usize::from(n - 5)]),
        _ => String::new(),
    };
    if modifiers.contains(M::ALT)
        && matches!(
            key.code,
            KeyCode::Char(_) | KeyCode::Enter | KeyCode::Backspace | KeyCode::Tab | KeyCode::Esc
        )
    {
        text.insert(0, '\x1b');
    }
    text.into_bytes()
}
pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    // Treat paste as text. In particular, embedded ESC cannot terminate the wrapper.
    let text: String = text
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'))
        .collect();
    if bracketed {
        format!("\x1b[200~{text}\x1b[201~").into_bytes()
    } else {
        text.into_bytes()
    }
}

/// Child encoding modes are independent; the parser filter retains every set bit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseEncoding {
    #[default]
    Default,
    Utf8,
    Urxvt,
    Sgr,
    SgrPixels,
}
impl From<vt100::MouseProtocolEncoding> for MouseEncoding {
    fn from(encoding: vt100::MouseProtocolEncoding) -> Self {
        match encoding {
            vt100::MouseProtocolEncoding::Utf8 => Self::Utf8,
            vt100::MouseProtocolEncoding::Sgr => Self::Sgr,
            _ => Self::Default,
        }
    }
}

/// Estimate the top-left pixel of the host cell, relative to the pane.
/// Mouse events have no subcell geometry; absent host dimensions use 8x16 cells.
pub fn mouse_pixels(event: crossterm::event::MouseEvent, area: ratatui::layout::Rect,
    window: Option<crossterm::terminal::WindowSize>) -> Option<(u32, u32)> {
    if !area.contains(ratatui::layout::Position::new(event.column, event.row)) { return None; }
    let (width, height) = window.and_then(|size| {
        if size.columns == 0 || size.rows == 0 { return None; }
        let cell = (size.width / size.columns, size.height / size.rows);
        (cell.0 > 0 && cell.1 > 0).then_some(cell)
    }).unwrap_or((8, 16));
    let axis = |offset: u16, cells: u16, pixels: u16| {
        (u32::from(offset) * u32::from(pixels) + 1)
            .clamp(1, u32::from(cells) * u32::from(pixels))
    };
    Some((axis(event.column - area.x, area.width, width),
        axis(event.row - area.y, area.height, height)))
}

/// Encode only host mouse input for the child. Coordinates are one-based cells, or pane pixels for SGR-Pixels.
pub fn mouse_bytes(event: crossterm::event::MouseEvent, x: u32, y: u32,
    mode: vt100::MouseProtocolMode, encoding: impl Into<MouseEncoding>) -> Vec<u8> {
    use crossterm::event::{MouseButton as B, MouseEventKind as K};
    use vt100::MouseProtocolMode as Mode;
    use MouseEncoding as Encoding;
    let encoding = encoding.into();
    let sgr = matches!(encoding, Encoding::Sgr | Encoding::SgrPixels);
    if mode == Mode::None || x == 0 || y == 0 { return vec![]; }
    let button = |b| match b { B::Left => 0u16, B::Middle => 1, B::Right => 2 };
    let release = matches!(event.kind, K::Up(_));
    let mut code = match event.kind {
        K::Down(b) => button(b),
        K::Up(b) if mode != Mode::Press => if sgr { button(b) } else { 3 },
        K::Drag(b) if matches!(mode, Mode::ButtonMotion | Mode::AnyMotion) => 32 + button(b),
        K::Moved if mode == Mode::AnyMotion => 35,
        K::ScrollUp => 64, K::ScrollDown => 65, K::ScrollLeft => 66, K::ScrollRight => 67,
        _ => return vec![],
    };
    if mode != Mode::Press {
        if event.modifiers.contains(M::SHIFT) { code += 4; }
        if event.modifiers.contains(M::ALT) { code += 8; }
        if event.modifiers.contains(M::CONTROL) { code += 16; }
    }
    match encoding {
        Encoding::Sgr | Encoding::SgrPixels => format!("\x1b[<{code};{x};{y}{}", if release { 'm' } else { 'M' }).into_bytes(),
        Encoding::Urxvt => format!("\x1b[{};{x};{y}M", code + 32).into_bytes(),
        Encoding::Default if x <= 223 && y <= 223 => vec![27, b'[', b'M', (code + 32) as u8, (x + 32) as u8, (y + 32) as u8],
        Encoding::Utf8 if x <= 2015 && y <= 2015 => {
            let mut out = String::from("\x1b[M");
            for n in [u32::from(code) + 32, x + 32, y + 32] { out.push(char::from_u32(n).unwrap()); }
            out.into_bytes()
        }
        _ => vec![],
    }
}
