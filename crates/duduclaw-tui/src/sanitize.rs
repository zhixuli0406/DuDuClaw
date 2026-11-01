//! Terminal safety at the presentation boundary, never for identity comparison.
use std::fmt::Write;

pub(crate) fn forbidden(c: char) -> bool {
    // Explicit default-ignorable/bidi-format ranges, including reserved tags.
    // Intentionally keep variation selectors U+FE00..U+FE0F for emoji and CJK
    // display. The requested tag-plane range E0000..E0FFF is removed
    // in full, including supplementary variation selectors.
    matches!(c,
        '\u{0000}'..='\u{0009}' | '\u{000b}'..='\u{001f}' |
        '\u{007f}'..='\u{009f}' | '\u{00ad}' | '\u{034f}' | '\u{061c}' |
        '\u{115f}'..='\u{1160}' | '\u{17b4}'..='\u{17b5}' |
        '\u{180b}'..='\u{180f}' | '\u{200b}'..='\u{200f}' |
        '\u{2028}'..='\u{202e}' | '\u{2060}'..='\u{206f}' |
        '\u{3164}' | '\u{feff}' | '\u{ffa0}' | '\u{fff0}'..='\u{fff8}' |
        '\u{1bca0}'..='\u{1bca3}' | '\u{1d173}'..='\u{1d17a}' |
        '\u{e0000}'..='\u{e0fff}')
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

/// Render each key in place: two keys with the same visible spelling must not
/// overwrite one another in a rebuilt JSON map and hide part of a payload.
pub(crate) fn visible_json(value: &serde_json::Value) -> TerminalText {
    fn render(value: &serde_json::Value, depth: usize) -> String {
        use serde_json::Value;
        let indent = "  ".repeat(depth + 1);
        let close_indent = "  ".repeat(depth);
        match value {
            Value::String(s) => serde_json::to_string(&sanitize_for_terminal_visible(s)).unwrap_or_default(),
            Value::Object(o) if !o.is_empty() => format!("{{\n{}\n{close_indent}}}", o.iter().map(|(k,v)| {
                let key = render(&Value::String(k.clone()), depth + 1);
                format!("{indent}{key}: {}", render(v, depth + 1))
            }).collect::<Vec<_>>().join(",\n")),
            Value::Array(a) if !a.is_empty() => format!("[\n{}\n{close_indent}]", a.iter().map(|v|
                format!("{indent}{}", render(v, depth + 1))).collect::<Vec<_>>().join(",\n")),
            other => other.to_string(),
        }
    }
    TerminalText::new(render(value, 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn visible_json_preserves_colliding_display_keys_and_their_values() {
        let value = serde_json::json!({"a\u{202e}": "first", "a⟨U+202E⟩": "second"});
        let text = visible_json(&value).visible();
        assert!(text.contains("first") && text.contains("second"));
        assert_eq!(text.matches("a⟨U+202E⟩").count(), 2);
    }
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
                let visible = sanitize_for_terminal_visible(&input);
                assert!(!visible.chars().any(forbidden));
                assert_eq!(sanitize_for_terminal_visible(&visible), visible);
                assert_eq!(sanitize_for_terminal(&plain), plain);
            }
        }
    }
    #[test]
    fn explicit_ignorable_and_bidi_regressions() {
        for n in [0xad, 0x34f, 0x61c, 0x115f, 0x1160, 0x17b4, 0x17b5,
            0x180b, 0x180e, 0x180f, 0x200e, 0x200f, 0x2060, 0x206a,
            0x206b, 0x206c, 0x206d, 0x206e, 0x206f, 0x3164, 0xffa0,
            0xfff0, 0xfff8, 0x1bca0, 0x1bca3, 0x1d173, 0x1d17a,
            0xe0000, 0xe0020, 0xe007f, 0xe0080, 0xe0100, 0xe01f0, 0xe0fff] {
            let input = format!("甲{}乙", char::from_u32(n).unwrap());
            assert_eq!(sanitize_for_terminal(&input), "甲乙", "U+{n:X}");
            assert_eq!(sanitize_for_terminal_visible(&input), format!("甲⟨U+{n:04X}⟩乙"));
        }
        let variation = "❤️漢\u{fe00}";
        assert_eq!(sanitize_for_terminal(variation), variation);
        assert_eq!(sanitize_for_terminal_visible(variation), variation);
    }
    #[test]
    fn preserves_layout_newline_and_normal_unicode() {
        let input = "你好 café 🙂\n第二行";
        assert_eq!(sanitize_for_terminal(input), input);
        assert_eq!(sanitize_for_terminal("\t\r\u{2066}\u{2069}\u{feff}"), "");
    }
}
