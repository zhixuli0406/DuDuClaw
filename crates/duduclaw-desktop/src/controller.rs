//! Desktop controller — cross-platform mouse/keyboard/screenshot operations.

use base64::Engine;
use enigo::{Axis, Button, Coordinate, Direction, Enigo, Keyboard, Mouse, Settings};
use tracing::{info, warn};

/// Errors from desktop control operations.
#[derive(Debug)]
pub enum DesktopError {
    /// Screenshot capture failed.
    ScreenshotFailed(String),
    /// Input simulation failed.
    InputFailed(String),
    /// Platform not supported for this operation.
    Unsupported(String),
}

impl std::fmt::Display for DesktopError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ScreenshotFailed(msg) => write!(f, "screenshot failed: {msg}"),
            Self::InputFailed(msg) => write!(f, "input failed: {msg}"),
            Self::Unsupported(msg) => write!(f, "unsupported: {msg}"),
        }
    }
}

impl std::error::Error for DesktopError {}

/// Mouse button types.
#[derive(Debug, Clone, Copy)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// Scroll direction.
#[derive(Debug, Clone, Copy)]
pub enum ScrollDirection {
    Up,
    Down,
    Left,
    Right,
}

/// Abstract desktop controller interface.
///
/// Implemented by `NativeDesktopController` (host) and by the container
/// orchestrator (which delegates to docker exec + xdotool).
///
/// Note: NOT `Send` / `Sync` because `Enigo` on macOS wraps CGEventSource
/// which cannot cross thread boundaries. Use from a single thread/task.
pub trait DesktopController {
    /// Capture a full-screen screenshot, returning PNG bytes.
    fn screenshot(&self) -> Result<Vec<u8>, DesktopError>;

    /// Capture a screenshot and return as base64-encoded PNG.
    fn screenshot_base64(&self) -> Result<String, DesktopError> {
        let png = self.screenshot()?;
        Ok(base64::engine::general_purpose::STANDARD.encode(&png))
    }

    /// Move the mouse to absolute coordinates.
    fn mouse_move(&mut self, x: i32, y: i32) -> Result<(), DesktopError>;

    /// Click at the specified coordinates.
    fn click(&mut self, x: i32, y: i32, button: MouseButton) -> Result<(), DesktopError>;

    /// Double-click at the specified coordinates.
    fn double_click(&mut self, x: i32, y: i32) -> Result<(), DesktopError>;

    /// Type text (simulates keyboard input).
    fn type_text(&mut self, text: &str) -> Result<(), DesktopError>;

    /// Press a key combination (e.g., "ctrl+s", "Return", "Tab").
    fn key_press(&mut self, key: &str) -> Result<(), DesktopError>;

    /// Scroll at the specified coordinates.
    fn scroll(
        &mut self,
        x: i32,
        y: i32,
        direction: ScrollDirection,
        amount: i32,
    ) -> Result<(), DesktopError>;
}

/// Native desktop controller using `enigo` (input) + OS-native screen capture.
///
/// Directly controls the host machine's display. Requires:
/// - macOS: Accessibility API permission (System Settings → Privacy → Accessibility)
/// - Windows: no special permissions
/// - Linux: X11 (Wayland support is experimental in enigo)
pub struct NativeDesktopController {
    enigo: Enigo,
}

impl NativeDesktopController {
    /// Create a new native controller.
    ///
    /// # Errors
    /// Returns error if the input simulation backend cannot be initialized
    /// (e.g., Accessibility API not granted on macOS).
    pub fn new() -> Result<Self, DesktopError> {
        let enigo = Enigo::new(&Settings::default())
            .map_err(|e| DesktopError::InputFailed(format!("Failed to init enigo: {e}")))?;
        info!("NativeDesktopController initialized");
        Ok(Self { enigo })
    }
}

impl DesktopController for NativeDesktopController {
    fn screenshot(&self) -> Result<Vec<u8>, DesktopError> {
        // Capture the primary monitor as PNG via the OS-native tool (no `xcap`).
        crate::capture::capture_primary_monitor_png().map_err(DesktopError::ScreenshotFailed)
    }

    fn mouse_move(&mut self, x: i32, y: i32) -> Result<(), DesktopError> {
        self.enigo
            .move_mouse(x, y, Coordinate::Abs)
            .map_err(|e| DesktopError::InputFailed(format!("mouse_move: {e}")))
    }

    fn click(&mut self, x: i32, y: i32, button: MouseButton) -> Result<(), DesktopError> {
        self.mouse_move(x, y)?;
        let btn = match button {
            MouseButton::Left => Button::Left,
            MouseButton::Right => Button::Right,
            MouseButton::Middle => Button::Middle,
        };
        self.enigo
            .button(btn, Direction::Click)
            .map_err(|e| DesktopError::InputFailed(format!("click: {e}")))
    }

    fn double_click(&mut self, x: i32, y: i32) -> Result<(), DesktopError> {
        self.mouse_move(x, y)?;
        self.enigo
            .button(Button::Left, Direction::Click)
            .map_err(|e| DesktopError::InputFailed(format!("double_click(1): {e}")))?;
        self.enigo
            .button(Button::Left, Direction::Click)
            .map_err(|e| DesktopError::InputFailed(format!("double_click(2): {e}")))
    }

    fn type_text(&mut self, text: &str) -> Result<(), DesktopError> {
        self.enigo
            .text(text)
            .map_err(|e| DesktopError::InputFailed(format!("type_text: {e}")))
    }

    fn key_press(&mut self, key_str: &str) -> Result<(), DesktopError> {
        // Parse key combinations like "ctrl+s", "Return", "Tab", "ctrl++".
        // `parse_key_combo` handles a literal '+' as the final key (which a naive
        // `split('+')` would turn into empty segments and silently drop).
        let (modifiers, final_key) = parse_key_combo(key_str);

        // Press modifier keys
        for part in &modifiers {
            if let Some(key) = parse_enigo_key(part) {
                self.enigo
                    .key(key, Direction::Press)
                    .map_err(|e| DesktopError::InputFailed(format!("key press {part}: {e}")))?;
            }
        }

        // Press and release the final key
        if let Some(last) = &final_key {
            if let Some(key) = parse_enigo_key(last) {
                self.enigo
                    .key(key, Direction::Click)
                    .map_err(|e| DesktopError::InputFailed(format!("key click {last}: {e}")))?;
            }
        }

        // Release modifier keys (reverse order)
        for part in modifiers.iter().rev() {
            if let Some(key) = parse_enigo_key(part) {
                self.enigo
                    .key(key, Direction::Release)
                    .map_err(|e| DesktopError::InputFailed(format!("key release {part}: {e}")))?;
            }
        }

        Ok(())
    }

    fn scroll(
        &mut self,
        x: i32,
        y: i32,
        direction: ScrollDirection,
        amount: i32,
    ) -> Result<(), DesktopError> {
        self.mouse_move(x, y)?;
        let (axis, clicks) = match direction {
            ScrollDirection::Up => (Axis::Vertical, amount),
            ScrollDirection::Down => (Axis::Vertical, -amount),
            ScrollDirection::Left => (Axis::Horizontal, -amount),
            ScrollDirection::Right => (Axis::Horizontal, amount),
        };
        self.enigo
            .scroll(clicks, axis)
            .map_err(|e| DesktopError::InputFailed(format!("scroll: {e}")))
    }
}

/// Split a key-combination string into (modifier tokens, final key token).
///
/// Splitting naively on '+' is wrong for combos whose final key is itself a
/// literal '+': `"ctrl++"` -> `["ctrl", "", ""]`, where the empty trailing
/// segments cause the actual '+' key to never be pressed. This parser treats a
/// trailing '+' (or a combo that is just "+") as a literal plus key, and drops
/// empty/whitespace-only modifier tokens.
///
/// Examples:
/// - `"ctrl+s"`  -> (["ctrl"], Some("s"))
/// - `"ctrl++"`  -> (["ctrl"], Some("+"))
/// - `"+"`        -> ([], Some("+"))
/// - `"Return"`  -> ([], Some("Return"))
pub fn parse_key_combo(key_str: &str) -> (Vec<String>, Option<String>) {
    let trimmed = key_str.trim();

    // A bare "+" means the literal plus key with no modifiers.
    if trimmed == "+" {
        return (Vec::new(), Some("+".to_string()));
    }

    // A trailing '+' (e.g. "ctrl++") means the final key is the literal '+'.
    // Everything before the last '+' is the modifier portion.
    let (modifier_str, final_key) = if let Some(prefix) = trimmed.strip_suffix("++") {
        (prefix, "+".to_string())
    } else {
        match trimmed.rsplit_once('+') {
            Some((prefix, last)) => (prefix, last.trim().to_string()),
            None => return (Vec::new(), Some(trimmed.to_string())),
        }
    };

    let modifiers: Vec<String> = modifier_str
        .split('+')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();

    (modifiers, Some(final_key))
}

/// Parse a key name string to an enigo Key.
pub fn parse_enigo_key(name: &str) -> Option<enigo::Key> {
    use enigo::Key;
    match name.to_lowercase().as_str() {
        "ctrl" | "control" => Some(Key::Control),
        "alt" | "option" => Some(Key::Alt),
        "shift" => Some(Key::Shift),
        "super" | "meta" | "cmd" | "command" => Some(Key::Meta),
        "return" | "enter" => Some(Key::Return),
        "tab" => Some(Key::Tab),
        "escape" | "esc" => Some(Key::Escape),
        "backspace" => Some(Key::Backspace),
        "delete" => Some(Key::Delete),
        "space" => Some(Key::Space),
        "up" => Some(Key::UpArrow),
        "down" => Some(Key::DownArrow),
        "left" => Some(Key::LeftArrow),
        "right" => Some(Key::RightArrow),
        "home" => Some(Key::Home),
        "end" => Some(Key::End),
        "pageup" => Some(Key::PageUp),
        "pagedown" => Some(Key::PageDown),
        "f1" => Some(Key::F1),
        "f2" => Some(Key::F2),
        "f3" => Some(Key::F3),
        "f4" => Some(Key::F4),
        "f5" => Some(Key::F5),
        "f6" => Some(Key::F6),
        "f7" => Some(Key::F7),
        "f8" => Some(Key::F8),
        "f9" => Some(Key::F9),
        "f10" => Some(Key::F10),
        "f11" => Some(Key::F11),
        "f12" => Some(Key::F12),
        // Single character keys
        s if s.len() == 1 => {
            let ch = s.chars().next()?;
            Some(Key::Unicode(ch))
        }
        other => {
            warn!(key = other, "Unknown key name, skipping");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_key_modifiers() {
        assert!(matches!(parse_enigo_key("ctrl"), Some(enigo::Key::Control)));
        assert!(matches!(parse_enigo_key("Shift"), Some(enigo::Key::Shift)));
        assert!(matches!(parse_enigo_key("CMD"), Some(enigo::Key::Meta)));
        assert!(matches!(parse_enigo_key("alt"), Some(enigo::Key::Alt)));
    }

    #[test]
    fn parse_key_special() {
        assert!(matches!(parse_enigo_key("Return"), Some(enigo::Key::Return)));
        assert!(matches!(parse_enigo_key("tab"), Some(enigo::Key::Tab)));
        assert!(matches!(parse_enigo_key("escape"), Some(enigo::Key::Escape)));
        assert!(matches!(parse_enigo_key("space"), Some(enigo::Key::Space)));
    }

    #[test]
    fn parse_key_single_char() {
        assert!(matches!(parse_enigo_key("s"), Some(enigo::Key::Unicode('s'))));
        assert!(matches!(parse_enigo_key("a"), Some(enigo::Key::Unicode('a'))));
    }

    #[test]
    fn parse_key_function() {
        assert!(matches!(parse_enigo_key("f1"), Some(enigo::Key::F1)));
        assert!(matches!(parse_enigo_key("F12"), Some(enigo::Key::F12)));
    }

    #[test]
    fn parse_key_unknown() {
        assert!(parse_enigo_key("unknownkey").is_none());
    }

    #[test]
    fn combo_simple_with_modifier() {
        let (mods, key) = parse_key_combo("ctrl+s");
        assert_eq!(mods, vec!["ctrl".to_string()]);
        assert_eq!(key.as_deref(), Some("s"));
    }

    #[test]
    fn combo_single_key_no_modifier() {
        let (mods, key) = parse_key_combo("Return");
        assert!(mods.is_empty());
        assert_eq!(key.as_deref(), Some("Return"));
    }

    #[test]
    fn combo_literal_plus_with_modifier() {
        // "ctrl++" must resolve to ctrl + literal '+', not a dropped empty key.
        let (mods, key) = parse_key_combo("ctrl++");
        assert_eq!(mods, vec!["ctrl".to_string()]);
        assert_eq!(key.as_deref(), Some("+"));
        // And the literal '+' must be a recognized key.
        assert!(matches!(
            parse_enigo_key(&key.unwrap()),
            Some(enigo::Key::Unicode('+'))
        ));
    }

    #[test]
    fn combo_bare_plus() {
        let (mods, key) = parse_key_combo("+");
        assert!(mods.is_empty());
        assert_eq!(key.as_deref(), Some("+"));
    }

    #[test]
    fn combo_multiple_modifiers() {
        let (mods, key) = parse_key_combo("ctrl+shift+s");
        assert_eq!(mods, vec!["ctrl".to_string(), "shift".to_string()]);
        assert_eq!(key.as_deref(), Some("s"));
    }

    #[test]
    fn combo_multiple_modifiers_literal_plus() {
        let (mods, key) = parse_key_combo("ctrl+shift++");
        assert_eq!(mods, vec!["ctrl".to_string(), "shift".to_string()]);
        assert_eq!(key.as_deref(), Some("+"));
    }

    #[test]
    fn combo_trims_whitespace() {
        let (mods, key) = parse_key_combo(" ctrl + s ");
        assert_eq!(mods, vec!["ctrl".to_string()]);
        assert_eq!(key.as_deref(), Some("s"));
    }
}
