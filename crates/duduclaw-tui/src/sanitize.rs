//! Terminal safety at the presentation boundary, never for identity comparison.
use std::fmt::Write;

fn forbidden(c: char) -> bool {
    matches!(c, '\u{0000}'..='\u{0009}' | '\u{000b}'..='\u{001f}' |
        '\u{007f}'..='\u{009f}' | '\u{202a}'..='\u{202e}' |
        '\u{2066}'..='\u{2069}' | '\u{200b}'..='\u{200d}' | '\u{feff}')
}

pub fn sanitize_for_terminal(input: &str) -> String {
    input.chars().filter(|c| !forbidden(*c)).collect()
}

/// Exposes removed code points without emitting executable terminal controls.
pub fn sanitize_for_terminal_visible(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for c in input.chars() {
        if !forbidden(c) {
            output.push(c);
        } else if (c as u32) < 32 {
            if let Some(symbol) = char::from_u32(0x2400 + c as u32) {
                output.push(symbol);
            }
        } else if c == '\u{007f}' {
            output.push('␡');
        } else {
            let _ = write!(output, "⟨U+{:04X}⟩", c as u32);
        }
    }
    output
}

/// Keep raw text private so renderers must choose a safe view explicitly.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct TerminalText(String);
impl TerminalText {
    pub fn new(text: String) -> Self {
        Self(text)
    }
    /// Compare opaque identities without applying display normalization.
    pub fn is(&self, value: &str) -> bool { self.0 == value }
    pub(crate) fn json_value(&self) -> Option<serde_json::Value> { serde_json::from_str(&self.0).ok() }
    pub fn plain(&self) -> String {
        sanitize_for_terminal(&self.0)
    }
    pub fn visible(&self) -> String {
        sanitize_for_terminal_visible(&self.0)
    }
}
impl std::fmt::Debug for TerminalText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.plain().fmt(f)
    }
}
impl<'de> serde::Deserialize<'de> for TerminalText {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        <String as serde::Deserialize>::deserialize(d).map(Self)
    }
}

// JSON preserves raw data; consumers must not write deserialized text raw to a terminal.
impl serde::Serialize for TerminalText {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn controls_and_mixed_cjk() {
        let input = "繁體\x07\x1b]52;c;secret\x07\x1b[2K\u{202e}中文\u{200d}\n下一行";
        assert_eq!(
            sanitize_for_terminal(input),
            "繁體]52;c;secret[2K中文\n下一行"
        );
        assert_eq!(
            sanitize_for_terminal_visible("中\x1b\u{202e}\x7f\u{85}"),
            "中␛⟨U+202E⟩␡⟨U+0085⟩"
        );
    }
    #[test]
    fn complete_ranges_and_idempotence() {
        for n in 0..=0x10ffff {
            if let Some(c) = char::from_u32(n) {
                let input = c.to_string();
                let plain = sanitize_for_terminal(&input);
                assert_eq!(plain.is_empty(), forbidden(c));
                assert!(!sanitize_for_terminal_visible(&input).chars().any(forbidden));
                assert_eq!(sanitize_for_terminal(&plain), plain);
            }
        }
    }
    #[test]
    fn preserves_layout_newline_and_normal_unicode() {
        let input = "你好 café 🙂\n第二行";
        assert_eq!(sanitize_for_terminal(input), input);
        assert_eq!(sanitize_for_terminal("\t\r\u{2066}\u{2069}\u{feff}"), "");
    }
}
