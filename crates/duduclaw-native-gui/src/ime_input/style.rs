// D3-b (2026-08-23) — the per-instance presentation/behaviour knobs that
// turn `ImeTextInput` from "the chat composer" into a widget two crates can
// share.
//
// Why this file exists: `duduclaw-shell` (DuDuClaw OS's session shell) needs
// the SAME `EntityInputHandler` composition path this module already
// implements, but with three differences the original chat-only version had
// hardcoded:
//
//   1. Colors. The chat composer reads `theme::BRAND`/`theme::
//      MUTED_FOREGROUND` — single, fixed values. The shell resolves every
//      color through a runtime `ShellPalette` that flips with the operator's
//      light/dark choice, so the caret/selection/placeholder colors have to
//      be supplied per instance, not baked in.
//   2. Masking. Three of the shell's five text inputs are passwords (OOBE
//      account password, Wi-Fi PSK, lockscreen unlock). See `masked` below
//      for the full treatment — it is deliberately a DISPLAY transform, not
//      an "IME off" switch.
//   3. Single-line. The chat composer is multi-line (Shift+Enter); every
//      shell field is one line and must never grow a second row.
//
// `Default` reproduces the chat composer's pre-D3-b values EXACTLY, so
// `duduclaw-native-gui`'s own composer is byte-identical after this refactor
// — the shell is the only caller that passes a non-default style.

use gpui::{px, Hsla, Pixels};

use crate::theme;

/// The glyph a masked field paints in place of every real grapheme. Matches
/// the character `duduclaw-shell/src/oobe/widgets.rs`'s pre-D3-b
/// `OobeTextField` already displayed (`"•".repeat(...)`), so the visual is
/// unchanged for the operator — only the machinery underneath it is new.
///
/// Kept as a `char` (not a `&str`) because every offset conversion in
/// [`crate::ime_input::TextEngine`] needs its exact UTF-8 byte length, and
/// deriving that from a `char` is a compile-time-checkable invariant rather
/// than an assumption about a string literal.
pub const MASK_CHAR: char = '•';

/// Presentation + behaviour configuration for one [`super::ImeTextInput`]
/// instance. Cheap (`Copy`) — the shell re-pushes it on every render pass so
/// a theme flip takes effect on the next frame without any subscription
/// machinery.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextInputStyle {
    /// Committed (non-placeholder) text color.
    pub text: Hsla,
    /// Placeholder text color, used only while the field is empty.
    pub placeholder: Hsla,
    /// Caret color.
    pub cursor: Hsla,
    /// Selection highlight fill (callers are expected to pass an already-
    /// alpha'd color; nothing here multiplies it further).
    pub selection: Hsla,
    pub text_size: Pixels,
    pub line_height: Pixels,
    /// Render every grapheme as [`MASK_CHAR`] — including the in-progress
    /// IME preedit.
    ///
    /// This is a DISPLAY transform, not an "IME off" switch, and that is a
    /// deliberate safety call rather than an oversight. gpui's Wayland
    /// backend turns `zwp_text_input_v3` off entirely when a handler reports
    /// `accepts_text_input() == false` (`gpui_linux/src/linux/wayland/
    /// window.rs::update_ime_enabled`, pinned rev `7a7c3e1`). On DuDuClaw OS
    /// fcitx5 holds a `grabKeyboard()` on the seat, so with text-input
    /// disabled the field would receive NOTHING at all — an operator locked
    /// out of their own machine. Masking the display keeps the field
    /// typeable under every input path while still meeting the shell's "no
    /// plaintext anywhere it can be observed" rule:
    ///   - the painted glyphs are bullets, preedit included;
    ///   - `text_for_range` (the IME's window onto surrounding text — the
    ///     one place a plaintext password could be handed to an external
    ///     input-method process, cloud-pinyin included) answers with bullets
    ///     too, never the real characters;
    ///   - nothing on this path logs field content in any form.
    pub masked: bool,
    /// `false` collapses the field to exactly one row: Shift+Enter inserts
    /// nothing and any newline arriving through the IME/paste path is
    /// stripped before it reaches the buffer.
    pub multi_line: bool,
    /// Emit [`super::ImeTextInputEvent::Submit`] on a bare Enter. The shell
    /// leaves this off — `enter` is a globally bound action there
    /// (`OobeNext`), and a bound keystroke never reaches a raw `on_key_down`
    /// listener at all, so an "Enter" arm here would be dead code pretending
    /// to be behaviour.
    pub submit_on_enter: bool,
}

impl Default for TextInputStyle {
    fn default() -> Self {
        Self {
            text: theme::alpha(theme::FOREGROUND, 1.0).into(),
            placeholder: theme::alpha(theme::MUTED_FOREGROUND, 1.0).into(),
            cursor: theme::alpha(theme::BRAND, 1.0).into(),
            selection: theme::alpha(theme::BRAND, 0.20).into(),
            text_size: px(theme::TEXT_SM),
            line_height: px(theme::TEXT_SM * 1.4),
            masked: false,
            multi_line: true,
            submit_on_enter: true,
        }
    }
}

impl TextInputStyle {
    /// Single-line, non-submitting — the shape every `duduclaw-shell` field
    /// wants before it overrides colors. Colors still come from
    /// [`Default`]; callers layer [`Self::with_colors`] on top.
    pub fn single_line() -> Self {
        Self { multi_line: false, submit_on_enter: false, ..Self::default() }
    }

    pub fn with_colors(mut self, text: Hsla, placeholder: Hsla, cursor: Hsla, selection: Hsla) -> Self {
        self.text = text;
        self.placeholder = placeholder;
        self.cursor = cursor;
        self.selection = selection;
        self
    }

    pub fn with_metrics(mut self, text_size: Pixels, line_height: Pixels) -> Self {
        self.text_size = text_size;
        self.line_height = line_height;
        self
    }

    pub fn masked(mut self, masked: bool) -> Self {
        self.masked = masked;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The chat composer's pre-D3-b hardcoded values, asserted here so a
    /// future edit to `Default` cannot silently restyle it.
    #[test]
    fn default_matches_the_chat_composers_original_values() {
        let s = TextInputStyle::default();
        assert_eq!(s.text, theme::alpha(theme::FOREGROUND, 1.0).into());
        assert_eq!(s.placeholder, theme::alpha(theme::MUTED_FOREGROUND, 1.0).into());
        assert_eq!(s.cursor, theme::alpha(theme::BRAND, 1.0).into());
        assert_eq!(s.selection, theme::alpha(theme::BRAND, 0.20).into());
        assert_eq!(s.text_size, px(theme::TEXT_SM));
        assert_eq!(s.line_height, px(theme::TEXT_SM * 1.4));
        assert!(!s.masked);
        assert!(s.multi_line);
        assert!(s.submit_on_enter);
    }

    #[test]
    fn single_line_preset_turns_off_multiline_and_submit_but_keeps_colors() {
        let d = TextInputStyle::default();
        let s = TextInputStyle::single_line();
        assert!(!s.multi_line);
        assert!(!s.submit_on_enter);
        assert_eq!(s.text, d.text);
        assert_eq!(s.cursor, d.cursor);
    }

    /// `MASK_CHAR`'s UTF-8 width is load-bearing: every masked-offset
    /// conversion in `TextEngine` divides/multiplies by it.
    #[test]
    fn mask_char_is_three_utf8_bytes() {
        assert_eq!(MASK_CHAR.len_utf8(), 3);
    }
}
