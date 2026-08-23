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
//
// CD-1 additions (BUILD.md "CD-1 comp-side additions" has the full wire
// table): `key_name` (named functional keys), `status` (read-only query),
// `highlight` (target highlight box). `auth` is deliberately NOT a variant
// here — see `AuthLine` below.
//
// CD-3 additions (task brief items 1/3, DESIGN §5's "接手/交還＋watch mode"
// row — full behavior lives in `codrive/takeover.rs` and `codrive/watch.rs`):
// `take_over` (agent-initiated hand-off to the human, e.g. a login step) and
// `watch` (toggles idle-based auto-pause supervision for the rest of this
// session's trajectory).
//
// WP-CD4a-COMP addition (B-line CD-4a, multi-window targeting): `activate_
// window` — raises/focuses a toplevel by xdg-shell app_id (or a title-prefix
// fallback). Full behavior lives in `codrive/window_target.rs`.
//
// WP-CD4b-fix addition (B3, AT-SPI2 coordinate translation): `window_
// geometry` — a READ-ONLY query returning where a client's visible window
// sits in compositor global coordinates, so the gateway can convert an
// AT-SPI `CoordType::Window` offset into a real click point (GTK4 hard-codes
// `CoordType::Screen` to zeros — see `codrive/window_geometry.rs`'s module
// doc for the first-hand GTK source citations). Answered by a oneshot
// request/response round trip to the calloop main thread, like
// `crate::shell_control`'s ops, NOT over the fire-and-forget `InjectCmd`
// channel.

use serde::Deserialize;

/// Hard cap on `take_over`'s `reason` field, bytes. This crate has no
/// `duduclaw_core::truncate_chars` dependency (deliberately dependency-thin,
/// see Cargo.toml's workspace-detach comment) — reject-not-truncate avoids
/// needing a CJK-safe byte-truncation helper here for what's already bounded
/// by `listener.rs::MAX_LINE_BYTES` anyway; this is a tighter, more honest
/// limit specific to one human-readable field.
pub const MAX_TAKE_OVER_REASON_BYTES: usize = 500;

/// Hard cap on `activate_window`'s `app_id` field, bytes (WP-CD4a-COMP).
/// Real xdg-shell app_ids are short reverse-DNS-style strings and titles
/// rarely run long either — this is a generous-but-bounded query length,
/// same "reject, don't truncate" reasoning as [`MAX_TAKE_OVER_REASON_BYTES`]
/// (this crate has no CJK-safe byte-truncation helper to reach for; see
/// that constant's own doc for why).
pub const MAX_ACTIVATE_WINDOW_QUERY_BYTES: usize = 255;

/// One line of the injection protocol, `{"op": "...", ...}`.
///
/// Wire shape (see DESIGN §3.3.1 CD-0 bullet 2, extended for CD-1):
/// - `{"op":"move","x":100.0,"y":200.0}` — absolute logical-space position.
/// - `{"op":"button","btn":"left","state":"press"}` — press/release.
/// - `{"op":"key","keycode":38,"state":"press"}` — raw XKB keycode
///   (evdev keycode + 8, matching what `smithay::input::keyboard::Keycode`
///   represents), press/release.
/// - `{"op":"key_name","name":"enter","state":"press"}` — a named
///   functional key from a fixed allowlist (see `keymap_ascii::
///   key_name_to_xkb`) that has no printable-ASCII representation for
///   `text` to reach — press/release.
/// - `{"op":"text","s":"hello"}` — synthesized key-by-key from an ASCII-only
///   table (see `keymap_ascii.rs` for the honest limitation).
/// - `{"op":"resume"}` — CD-1: always denied over this channel now
///   (`resume_is_human_only`) — "交還" moved to the human side (Super+Enter,
///   `DuduclawComp::human_resume`). Kept as a variant so a caller still
///   trying the CD-0-era path gets a specific, named denial.
/// - `{"op":"status"}` — CD-1: read-only query, answered directly by the
///   listener thread (never reaches the main-thread channel), allowed even
///   while frozen.
/// - `{"op":"highlight","x":0.0,"y":0.0,"w":100.0,"h":40.0,"ms":800}` —
///   CD-1: draws a hollow border around this rectangle for `ms`
///   milliseconds (optional, default 800, clamped to [100, 5000]).
/// - `{"op":"rotate_token"}` — CD-2 (DESIGN §9 CD-1 carry-forward "socket
///   rotation"): asks this run to generate a fresh socket-auth token right
///   now and write it over `$XDG_RUNTIME_DIR/duduclaw-codrive.token`,
///   without restarting the process. Like `status`, this is answered
///   directly by the listener thread (`listener.rs`) and never reaches the
///   main-thread channel — see `CodriveShared::rotate_token`. Only reachable
///   AFTER the auth handshake, same as every other op here, so requesting a
///   rotation already proves the caller held the token being replaced.
/// - `{"op":"shadow","enable":true|false}` — CD-2 shadow workspace
///   (DESIGN §3.3.4 / task brief "WP-CD2-shadow"): toggles whether this
///   session's agent-driven window(s) run on the headless shadow output
///   (invisible on the main desktop, PiP-previewed) or the main output.
///   Unlike `status`/`resume`/`rotate_token`, this DOES reach the main
///   thread (`codrive::handle_agent_inject` → `DuduclawComp::
///   codrive_set_shadow`, `codrive/shadow.rs`) — it moves real windows in
///   `self.space`, so it's subject to the same frozen/terminated gates as
///   `move`/`button`/`key`/`text`/`highlight`.
/// - `{"op":"take_over","reason":"登入頁需要人工輸入密碼"}` — CD-3: the agent
///   proactively yields the desktop to the human (DESIGN §3.1 "agent 出
///   take_over 動作 → 同上（主動交棒）"). Reaches the main thread
///   (`codrive::handle_agent_inject` → `DuduclawComp::codrive_take_over`,
///   `codrive/takeover.rs`) — it freezes the seat exactly like human input
///   does, plus marks the freeze as agent-initiated (blocks the shadow-
///   bypass exception; see `takeover.rs`'s module doc) and audits
///   `takeover_started`. Ends the same way any freeze ends — human-side
///   Super+Enter (`human_resume`) — which additionally audits
///   `takeover_ended`.
/// - `{"op":"watch","enable":true|false}` — CD-3: toggles watch-mode idle
///   supervision (DESIGN §3.4 "watch mode…人離開即自動暫停") for the rest of
///   this session. Reaches the main thread (`DuduclawComp::codrive_set_watch`,
///   `codrive/watch.rs`).
/// - `{"op":"activate_window","app_id":"..."}` — WP-CD4a-COMP (B-line CD-4a,
///   multi-window targeting): raises and focuses (via the same shared
///   `DuduclawComp::focus_window` helper the human click-to-focus / agent
///   click-to-focus / Super+Tab paths already use — see `state.rs`, WP-A1)
///   the toplevel whose xdg-shell app_id matches `app_id` exactly, or —
///   fallback, when no exact app_id match exists — whose title starts with
///   `app_id` (some real clients don't expose the app_id a caller might
///   expect; a title-prefix fallback gives a usable path instead of a hard
///   failure). Reaches the main thread (`codrive::handle_agent_inject` →
///   `DuduclawComp::codrive_activate_window`, `codrive/window_target.rs`) —
///   subject to the same frozen/terminated gates as `move`/`button`/… and,
///   like `Shadow`/`TakeOver`/`Watch`, never shadow-bypass-eligible (see
///   `shadow.rs`'s `freeze_bypass_decision`). No match found is answered
///   honestly with an `activate_window_failed` audit line, never a silent
///   no-op.
/// - `{"op":"window_geometry","pid":1234,"app_id":"…"}` — WP-CD4b-fix (B3):
///   READ-ONLY query for a client's visible-window origin + size in global
///   logical coordinates. At least one of `pid`/`app_id` is required
///   (`listener::validate`). Like `status`/`rotate_token` it is answered
///   *outside* the frozen/terminated gates (it mutates nothing) — but unlike
///   them it needs `self.space`, so `listener.rs` bridges it to the calloop
///   main thread over a separate oneshot-reply channel and blocks, bounded,
///   for the answer. Full behavior + the GTK4 `CoordType::Screen`-is-hard-
///   coded-to-zero background it exists to work around live in
///   `codrive/window_geometry.rs`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum InjectCmd {
    Move { x: f64, y: f64 },
    Button { btn: String, state: String },
    Key { keycode: u32, state: String },
    KeyName { name: String, state: String },
    Text { s: String },
    Resume,
    Status,
    Highlight { x: f64, y: f64, w: f64, h: f64, ms: Option<u64> },
    RotateToken,
    Shadow { enable: bool },
    TakeOver { reason: String },
    Watch { enable: bool },
    ActivateWindow { app_id: String },
    /// WP-CD4b-fix (B3). Both fields optional at the serde layer so a caller
    /// may send either or both; `listener::validate` rejects the
    /// all-absent case rather than letting an identity-less query through.
    WindowGeometry {
        #[serde(default)]
        app_id: Option<String>,
        #[serde(default)]
        pid: Option<u32>,
    },
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
            InjectCmd::KeyName { .. } => ("key_name", None, None),
            InjectCmd::Text { .. } => ("text", None, None),
            InjectCmd::Resume => ("resume", None, None),
            InjectCmd::Status => ("status", None, None),
            InjectCmd::Highlight { x, y, .. } => ("highlight", Some(*x), Some(*y)),
            InjectCmd::RotateToken => ("rotate_token", None, None),
            InjectCmd::Shadow { .. } => ("shadow", None, None),
            InjectCmd::TakeOver { .. } => ("take_over", None, None),
            InjectCmd::Watch { .. } => ("watch", None, None),
            InjectCmd::ActivateWindow { .. } => ("activate_window", None, None),
            InjectCmd::WindowGeometry { .. } => ("window_geometry", None, None),
        }
    }

    /// Does executing this command put a key on the agent seat's keyboard?
    ///
    /// D3-c: exactly the ops an input-method keyboard grab on the agent seat
    /// would swallow. Written as an exhaustive match rather than
    /// `matches!(…, Key | KeyName | Text)` so that adding a new keyboard op
    /// later is a compile error here instead of a silent hole in the gate —
    /// the same fail-closed shape `describe` above uses.
    pub fn is_keyboard_op(&self) -> bool {
        match self {
            InjectCmd::Key { .. } | InjectCmd::KeyName { .. } | InjectCmd::Text { .. } => true,
            InjectCmd::Move { .. }
            | InjectCmd::Button { .. }
            | InjectCmd::Resume
            | InjectCmd::Status
            | InjectCmd::Highlight { .. }
            | InjectCmd::RotateToken
            | InjectCmd::Shadow { .. }
            | InjectCmd::TakeOver { .. }
            | InjectCmd::Watch { .. }
            | InjectCmd::ActivateWindow { .. }
            | InjectCmd::WindowGeometry { .. } => false,
        }
    }
}

/// The mandatory first line of every new connection (CD-1, DESIGN §3.3.1's
/// "EIS 界線"): `{"op":"auth","token":"<hex>"}`. Deliberately NOT a variant
/// of `InjectCmd` — auth only ever applies to exactly the first line of a
/// connection (`listener.rs`'s `authenticate`), never to the ongoing
/// per-command loop, so keeping it a separate, narrower type makes "an
/// `auth` op showing up mid-stream" a plain parse error instead of a case
/// every `InjectCmd` match arm has to consider. `token` is a required field
/// (not `Option`/`#[serde(default)]`): a missing token is exactly as much
/// an auth failure as a wrong one, and letting serde reject it outright
/// keeps `listener.rs::authenticate` from having to special-case "absent"
/// vs. "present but wrong."
#[derive(Debug, Deserialize)]
pub struct AuthLine {
    pub op: String,
    pub token: String,
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
