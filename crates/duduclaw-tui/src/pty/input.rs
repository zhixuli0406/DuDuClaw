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
