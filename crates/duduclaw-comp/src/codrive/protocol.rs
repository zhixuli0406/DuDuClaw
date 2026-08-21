// CD-0 codrive spike — injection channel wire protocol.
// See DESIGN-codrive-desktop-2026-08.md §3.3.1/§5 (CD-0 line item) and
// BUILD.md's "CD-0 codrive spike verification" section.
//
// Deliberately NOT a Wayland protocol (no virtual-keyboard-unstable-v1 /
// wlr-virtual-pointer-unstable-v1 wire objects): this is a private,
// same-host, JSON-lines control channel that calls straight into the agent
// seat's `KeyboardHandle`/`PointerHandle` Rust API from `codrive::exec`.
// That sidesteps needing to implement any wlr protocol for CD-0 (per the
// task brief) and, more importantly, means nothing here is GPL-derived:
// niri's `virtual_pointer.rs` (GPL-3.0) was read only as background on how
// the *protocol* shapes itself for the wlr world, never as source to copy —
// this module's shapes were designed from scratch against DESIGN
// §3.3.1's op list, not transcribed from any GPL source.

use serde::Deserialize;

/// One line of the injection protocol, `{"op": "...", ...}`.
///
/// Wire shape (see DESIGN §3.3.1 CD-0 bullet 2):
/// - `{"op":"move","x":100.0,"y":200.0}` — absolute logical-space position.
/// - `{"op":"button","btn":"left","state":"press"}` — press/release.
/// - `{"op":"key","keycode":38,"state":"press"}` — raw XKB keycode
///   (evdev keycode + 8, matching what `smithay::input::keyboard::Keycode`
///   represents), press/release.
/// - `{"op":"text","s":"hello"}` — synthesized key-by-key from an ASCII-only
///   table (see `keymap_ascii.rs` for the honest limitation).
/// - `{"op":"resume"}` — clears the freeze set by the most recent human
///   input; the only op accepted while frozen.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum InjectCmd {
    Move { x: f64, y: f64 },
    Button { btn: String, state: String },
    Key { keycode: u32, state: String },
    Text { s: String },
    Resume,
}

impl InjectCmd {
    /// Short op name + optional (x, y) for audit/log purposes. Never panics
    /// on malformed data — this only reads fields already validated by
    /// `serde` at parse time.
    pub fn describe(&self) -> (&'static str, Option<f64>, Option<f64>) {
        match self {
            InjectCmd::Move { x, y } => ("move", Some(*x), Some(*y)),
            InjectCmd::Button { .. } => ("button", None, None),
            InjectCmd::Key { .. } => ("key", None, None),
            InjectCmd::Text { .. } => ("text", None, None),
            InjectCmd::Resume => ("resume", None, None),
        }
    }
}

/// `"press"` / `"release"` → pressed?
pub fn parse_press_state(s: &str) -> Result<bool, String> {
    match s {
        "press" => Ok(true),
        "release" => Ok(false),
        other => Err(format!(
            "invalid state {other:?}, expected \"press\" or \"release\""
        )),
    }
}

/// `"left"` / `"right"` / `"middle"` → Linux evdev `BTN_*` code
/// (`linux/input-event-codes.h`), matching the constant already used by
/// `grabs/move_grab.rs` and `grabs/resize_grab.rs` (`BTN_LEFT = 0x110`).
pub fn parse_button_code(btn: &str) -> Result<u32, String> {
    match btn {
        "left" => Ok(0x110),
        "right" => Ok(0x111),
        "middle" => Ok(0x112),
        other => Err(format!(
            "unknown button {other:?}, expected \"left\"/\"right\"/\"middle\""
        )),
    }
}
