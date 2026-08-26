// WP-comp-shell-ipc — wire protocol for the shell-control socket.
//
// Transport: one JSON object per line (newline-delimited), ONE request per
// connection — the client connects, writes exactly one `ShellControlRequest`
// line, reads exactly one `ShellControlResponse` line, then the connection
// closes. Unlike `codrive`'s injection socket (one long-lived, stateful,
// multi-command session — see `codrive::listener`'s module doc), there is no
// session to keep alive here: `list_windows`/`focus_window` are each
// independent, idempotent-to-retry queries/actions with no freeze/terminated
// state machine attached (see `shell_control/mod.rs`'s module doc for why).
// A dock polling `list_windows` every few seconds is exactly this shape —
// simple connect/request/response/close, same as `duduclaw-sysd`'s protocol
// (`duduclaw-sysd/src/protocol.rs`, which this module's shape deliberately
// mirrors: closed `#[serde(tag = ..., deny_unknown_fields)]` enum, one flat
// response envelope with `Option` fields).
//
// `deny_unknown_fields` (unlike `codrive::protocol::InjectCmd`, which
// predates this convention): an attacker appending stray fields to a
// well-formed op must fail to parse, not be silently ignored. This is also
// why `ShellControlRequest` is ADJACENTLY tagged (`tag = "op", content =
// "params"`, exactly `duduclaw-sysd::protocol::SysdRequest`'s own shape)
// rather than internally tagged like `codrive::protocol::InjectCmd`
// (`tag = "op"` alone): found empirically, not assumed — an internally
// tagged enum's `deny_unknown_fields` does not reliably reject a stray
// top-level key next to a unit variant's tag (serde buffers the whole
// object as generic `Content` to peek the tag first, and that buffering
// step does not re-validate "was every key consumed" the way a normal
// struct visitor does). Adjacent tagging sidesteps this: a variant's own
// fields, if any, live under a nested `"params"` object with its own
// ordinary (and therefore `deny_unknown_fields`-honoring) struct
// deserialization pass, and the outer envelope has exactly two legal keys
// (`op`, `params`) enforced the same way. A unit variant like `ListWindows`
// still serializes with no `params` key at all — see the `list_windows_
// wire_shape_has_no_extra_fields` test below.

use serde::{Deserialize, Serialize};
use smithay::output::Mode;

use crate::cursor::CursorSourceInfo;

use super::codrive_ops::CodriveStatusInfo;

/// Socket file name, relative to `$XDG_RUNTIME_DIR` — see task brief.
/// Deliberately a DIFFERENT file than `codrive`'s `duduclaw-codrive.sock`
/// (`codrive/mod.rs::init`) — two sockets, two trust boundaries, see
/// `shell_control/mod.rs`'s module doc.
pub const SOCKET_FILE_NAME: &str = "duduclaw-shell.sock";

/// Audit log file name, relative to `$XDG_RUNTIME_DIR`. Separate file from
/// `codrive`'s `duduclaw-codrive-audit.jsonl` — see `audit.rs`'s module doc
/// for why a shared file was rejected.
pub const AUDIT_FILE_NAME: &str = "duduclaw-shell-control-audit.jsonl";

/// Same bound `codrive::listener::MAX_LINE_BYTES` uses, same reasoning: a
/// local control channel, not a network API, but an unbounded read on a
/// line nobody terminates would still be an easy local DoS against the one
/// thread that serves every shell-control connection.
pub const MAX_REQUEST_LINE_BYTES: usize = 4096;

/// Hard cap on `focus_window`'s `query` field, bytes. Same value and same
/// "reject, don't truncate" reasoning as `codrive::protocol::
/// MAX_ACTIVATE_WINDOW_QUERY_BYTES` (this crate has no CJK-safe byte-
/// truncation helper — see that constant's own doc comment) — real xdg-shell
/// app_ids/titles this short a query is meant to match are short strings.
pub const MAX_QUERY_BYTES: usize = 255;

/// CUR-2: hard cap on `set_cursor_source`'s `source` field, bytes. The legal
/// values are `"system"` / `"brand"` / `"duduclaw"`; anything remotely near
/// this bound is already a bug or an attack, and rejecting it here means the
/// strict parser never sees a pathological string.
pub const MAX_CURSOR_SOURCE_BYTES: usize = 32;

/// WP-comp-shell-display: hard cap on `set_output_mode`/`set_output_scale`'s
/// `output` field, bytes. Same "reject, don't truncate" reasoning as
/// [`MAX_QUERY_BYTES`] — real connector names this crate ever produces
/// (`udev_backend::build_surfaces`'s `"{interface}-{interface_id}"`, or
/// winit's fixed `"winit"`) are well under 32 bytes; 128 leaves generous
/// headroom for an exotic real-world connector name without coming anywhere
/// near [`MAX_REQUEST_LINE_BYTES`].
pub const MAX_OUTPUT_NAME_BYTES: usize = 128;

/// WP-comp-shell-display: the closed set `set_output_scale` accepts,
/// matching the shell's own five-segment 顯示 › 縮放 control. Same
/// "closed set, refuse rather than coerce" policy CUR-3's
/// `crate::cursor::source::CURSOR_SIZE_STEPS` established for pointer size —
/// see that constant's doc for why a settings page must never be handed a
/// value none of its buttons can represent.
pub const OUTPUT_SCALE_STEPS: [i64; 5] = [100, 125, 150, 175, 200];

/// D2: hard cap on `set_theme`'s `theme` field, bytes. The only legal values
/// are `"light"`/`"dark"`; same "reject before the strict parser ever sees a
/// pathological string" reasoning as [`MAX_CURSOR_SOURCE_BYTES`], just a
/// smaller bound because this field's whole vocabulary is two five-byte
/// words.
pub const MAX_THEME_BYTES: usize = 16;

/// A1: bound on [`crate::state::DuduclawComp::pending_shell_intents`]. A
/// shell that stops polling `take_shell_intents` (crashed, restarting) must
/// not let the human's own global-hotkey presses accumulate without limit —
/// see `DuduclawComp::push_shell_intent`'s doc for the drop-oldest-and-warn
/// policy this bounds.
pub const MAX_PENDING_SHELL_INTENTS: usize = 8;

/// A1: the closed set of "the compositor is telling the shell something
/// happened" signals `take_shell_intents` can drain. One value today —
/// [`ShellIntent::GlobalTaskBar`] — but a closed `enum` rather than a bare
/// `&'static str` so a second signal (if one is ever added) cannot silently
/// diverge in spelling between the push side (`input.rs`) and the drain side
/// (`shell_control::mod`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellIntent {
    /// Super+K was pressed with real human-keyboard provenance anywhere in
    /// the session (the compositor holds the only global hotkey — an
    /// individual client with keyboard focus never sees this combo at all).
    /// The shell should open its global task-bar / 交辦欄 overlay.
    GlobalTaskBar,
}

impl ShellIntent {
    /// The exact wire token — also what a `take_shell_intents` audit line
    /// records. Lower-snake-case, matching every other op-name/enum-string
    /// convention on this socket (`ShellControlRequest::op_name`,
    /// `crate::cursor::source::CursorSource::as_str`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GlobalTaskBar => "global_task_bar",
        }
    }
}

/// The closed op set this socket accepts. See this file's module doc for
/// the wire shape convention (mirrors `duduclaw-sysd::protocol::SysdRequest`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", content = "params", rename_all = "snake_case", deny_unknown_fields)]
pub enum ShellControlRequest {
    /// `{"op":"list_windows"}` — every currently mapped toplevel's
    /// app_id/title/focused-state. Read-only: never touches `self.space`
    /// mutably, never audited (same "queries aren't audited, actions are"
    /// precedent `codrive::listener`'s own `status`/`resume` handling
    /// already established — see `mod.rs`'s module doc).
    ListWindows,
    /// `{"op":"focus_window","params":{"query":"foot-A"}}` — raises/focuses a mapped
    /// toplevel by exact xdg-shell app_id, falling back to a title-prefix
    /// match. Identical matching POLICY to `codrive`'s `activate_window`
    /// (reuses `codrive::window_target::find_target_window` — see that
    /// module's own doc for the exact-app_id-then-title-prefix priority
    /// order and z-order tie-break), but reached over a different socket,
    /// under a different auth model, audited to a different file, and
    /// applied to the HUMAN seat (`DuduclawComp::seat`), not the agent seat
    /// — see `mod.rs`'s module doc for why that seat choice matters.
    FocusWindow { query: String },
    /// CUR-2. `{"op":"get_cursor_source"}` — what the human pointer is
    /// currently drawn from. Read-only, never audited (same "queries aren't
    /// audited, actions are" split as `list_windows`; a settings page will
    /// call this on every open).
    ///
    /// Answers with the `cursor` block: effective source, requested source,
    /// theme name, where the value came from, whether an operator env var
    /// pins it, and (CUR-3) the cursor size plus the size actually being
    /// drawn. See `crate::cursor::CursorSourceInfo` for the field semantics —
    /// in particular why `source`/`requested` and `size`/`effective_size` are
    /// each two fields rather than one.
    ///
    /// The op name kept its CUR-2 spelling after CUR-3 widened the answer to
    /// cover size: renaming it to something like `get_cursor_config` would
    /// break every already-shipped caller for a cosmetic gain, and the reply
    /// is additive (a CUR-2-era client ignores the new keys).
    GetCursorSource,
    /// CUR-2. `{"op":"set_cursor_source","params":{"source":"brand"}}` —
    /// switch the human pointer's artwork **live**, no compositor restart.
    ///
    /// `source` is `"system"` / `"brand"` (`"duduclaw"` is accepted as a
    /// synonym of `brand`, matching the env var). Anything else is REFUSED
    /// with `invalid_cursor_source` rather than silently coerced — see
    /// `CursorSource::parse_strict`'s doc for why this parser is stricter
    /// than the env one.
    ///
    /// This is an ACTION, so it is audited, and it also writes the value to
    /// the stored preference so it survives a restart. A persistence failure
    /// does not fail the op — the switch is already live — but it is
    /// reported honestly as `cursor.persisted: false` plus
    /// `cursor.persist_error`.
    ///
    /// Why this socket and not `codrive`'s: choosing a pointer style is a
    /// HUMAN preference, not an agent action. Routing it through the agent's
    /// injection channel would attribute a person's settings change to the
    /// agent in the codrive audit trail — the exact audit-poisoning this
    /// module exists to avoid (`mod.rs`'s doc). The same-uid `SO_PEERCRED`
    /// boundary is also the right one: only a process running as this kiosk
    /// session's own user may change how that session looks.
    SetCursorSource { source: String },
    /// CUR-3. `{"op":"set_cursor_size","params":{"size":32}}` — change the
    /// human pointer's size **live**, no compositor restart.
    ///
    /// `size` must be one of `crate::cursor::source::CURSOR_SIZE_STEPS`
    /// (24 / 32 / 48 / 64 / 96 — the five segments the design canvas settled
    /// on for 協助工具 › 指向與點按). Anything else is REFUSED with
    /// `invalid_cursor_size`; it is never clamped to the nearest step, for the
    /// same reason `set_cursor_source` refuses `"brnad"` instead of coercing
    /// it (`CursorSource::parse_strict`'s doc) — a settings page must never be
    /// handed a value none of its buttons can represent.
    ///
    /// Typed `i64` rather than `u32` so that `{"size":-5}` and `{"size":9e18}`
    /// come back as `invalid_cursor_size` — an honest statement about the
    /// value — instead of `parse_error`, which would blame the JSON. A
    /// non-integer (`{"size":3.5}`, `{"size":"32"}`) is still `parse_error`:
    /// that genuinely IS a schema violation, not an out-of-range size.
    ///
    /// The reply is the same `cursor` block `get_cursor_source` returns, with
    /// the new `size` — and, when the loaded theme has no image at that size,
    /// an `effective_size` that differs from it. **This is a real outcome, not
    /// an error**: nothing upscales, so a 96 request against a theme whose
    /// largest image is 64 draws 64 px and says so. See
    /// `crate::cursor::theme::CursorThemeStore::effective_size`.
    ///
    /// Like `set_cursor_source` this is an ACTION: audited, and written to the
    /// stored preference so it survives a restart (a persistence failure is
    /// reported as `cursor.persisted: false` + `cursor.persist_error`, never
    /// swallowed). It lives on this socket for the same reason — pointer size
    /// is an accessibility preference belonging to the HUMAN at the keyboard,
    /// and routing it through the agent's injection channel would attribute a
    /// person's settings change to the agent.
    SetCursorSize { size: i64 },
    /// WP-comp-shell-display (2026-08-23). `{"op":"get_outputs"}` — every
    /// real output's current mode/scale/physical properties, for the
    /// shell's 顯示 (display) settings page. Read-only: never touches
    /// `self.space` mutably, never audited (same "queries aren't audited,
    /// actions are" rule as `list_windows`/`get_cursor_source`).
    ///
    /// The CD-2 shadow workspace's headless output (`DuduclawComp::
    /// shadow_output`) is always excluded — it is not a screen a human can
    /// see, same exclusion `state.rs::primary_output` already applies. See
    /// `mod.rs`'s module doc for the full wire shape and for why
    /// `mode_switch_supported` is always `false` on this build.
    GetOutputs,
    /// `{"op":"set_output_mode","params":{"output":"Virtual-1","width":1920,
    /// "height":1080,"refresh_mhz":60000}}` — switch an output to one of its
    /// own already-known modes.
    ///
    /// `width`/`height`/`refresh_mhz` are typed `i64`, not `u32`, so an
    /// out-of-range or negative value comes back as `invalid_mode` — an
    /// honest statement about the *value* — instead of `parse_error`, which
    /// would blame the JSON. Same reasoning CUR-3's `SetCursorSize` doc
    /// comment gives for its own `size: i64` field. A non-integer
    /// (`{"width":3.5}`, `{"width":"1920"}`) is still `parse_error`: that
    /// genuinely IS a schema violation, not an out-of-range mode.
    ///
    /// This ACTION is always audited — but on this build it always answers
    /// `mode_switch_unsupported` for an otherwise-valid request. See
    /// `mod.rs`'s module doc for exactly why (a real modeset would need
    /// backend-lifecycle restructuring out of scope for this socket) and
    /// why that is the honest answer rather than a coerced success.
    SetOutputMode { output: String, width: i64, height: i64, refresh_mhz: i64 },
    /// `{"op":"set_output_scale","params":{"output":"Virtual-1","scale_pct":125}}`
    /// — switch an output's UI scale factor.
    ///
    /// `scale_pct` must be one of [`OUTPUT_SCALE_STEPS`] (100 / 125 / 150 /
    /// 175 / 200); anything else is REFUSED with `invalid_scale`, never
    /// clamped to the nearest step — same "closed set, refuse rather than
    /// coerce" policy `SetCursorSize` established for pointer size. Typed
    /// `i64` for the same `invalid_scale`-not-`parse_error` reason
    /// `SetOutputMode`'s fields are.
    ///
    /// Always audited. WP-comp-shell-display D4b-3 (2026-08-24): this now
    /// APPLIES for real — validated request → live `change_current_state` →
    /// layer/window re-layout → persisted to `output_prefs` → refreshed
    /// `outputs` echoed back. See `mod.rs`'s module doc ("Scale, real as of
    /// D4b-3") for what unblocked this (every custom render element used to
    /// hardcode `Scale::from(1.0)`; consolidated onto `render::
    /// output_render_scale`) and for the one disclosed gap (the three
    /// fractional steps were not independently live-verified, only 100%/
    /// 200%).
    SetOutputScale { output: String, scale_pct: i64 },
    /// D2. `{"op":"set_theme","params":{"theme":"dark"}}` — switch comp's own
    /// server-side decorations (title bar / border / shadow / Alt-Tab
    /// switcher) between light and dark **live**, no compositor restart.
    ///
    /// `theme` is `"light"` / `"dark"` (trim + case-insensitive, same
    /// leniency as `SetCursorSource`'s `source`). Anything else is REFUSED
    /// with `invalid_theme` rather than defaulted to either side — see
    /// `crate::decor::Theme::parse_strict`'s doc.
    ///
    /// This is an ACTION, so it is audited. Unlike the cursor preferences
    /// this socket also carries, there is **no persistence** on comp's side:
    /// `duduclaw-shell` is the single source of truth for the theme choice
    /// and re-announces it at every boot, so comp only tracks the live value
    /// for this process's lifetime — see `crate::decor::Theme`'s own doc.
    ///
    /// Why this socket and not `codrive`'s: same reasoning as every other
    /// appearance preference here — a person's own theme choice must not be
    /// attributed to the agent in the codrive audit trail.
    SetTheme { theme: String },
    /// A1. `{"op":"take_shell_intents"}` — **drains** (read-and-clear, not a
    /// re-readable snapshot) the queue of global compositor-level gestures
    /// the shell has not yet been told about (today: Super+K, i.e.
    /// [`ShellIntent::GlobalTaskBar`]).
    ///
    /// Read-only in the sense that it never touches `self.space`/`self.seat`,
    /// but it DOES mutate `DuduclawComp::pending_shell_intents` (drain, not a
    /// re-readable snapshot). Audited only when it actually drains something:
    /// the shell polls this every ~200ms (task brief's contract), and the
    /// overwhelming majority of polls find the queue empty — auditing every
    /// one would be exactly the noise `list_windows`/`get_cursor_source` stay
    /// unaudited to avoid. A poll that DOES drain ≥1 intent is, by
    /// construction, reporting a real human gesture (Super+K has real-keyboard
    /// provenance — see `input.rs`'s `is_task_bar_keysym` call site), so that
    /// one line still leaves a genuine trace despite the frequent-poll shape
    /// of this op. See `shell_control::mod`'s handler for the exact rule.
    ///
    /// Answers `{"ok":true,"intents":[...]}`, `intents` always present (an
    /// empty array on a quiet poll, never omitted) — see
    /// [`ShellControlResponse::intents`].
    TakeShellIntents,
    /// A2. `{"op":"codrive_status"}` — who is driving this desktop right now
    /// (`human` / `codrive` / `handover`), plus the raw flags that answer was
    /// derived from. Read-only, never audited (same rule as `list_windows` /
    /// `get_cursor_source` / `get_outputs` — a shell painting a status pill
    /// polls this).
    ///
    /// Answers with the `codrive` block; see
    /// [`super::codrive_ops::CodriveStatusInfo`] for the field semantics and
    /// that module's own doc for the wire examples.
    CodriveStatus,
    /// A2. `{"op":"codrive_drive","params":{"action":"take_wheel"}}` — the
    /// human takes the wheel back from the agent, or gives it back.
    ///
    /// `action` is the closed set `"take_wheel"` / `"hand_back"`
    /// (`super::codrive_ops::CodriveDriveAction`); anything else is REFUSED
    /// with `invalid_codrive_action`, never coerced. An ACTION, so it is
    /// audited; the reply is the same `codrive` block `codrive_status`
    /// returns, reflecting the state AFTER the action ran.
    ///
    /// Lives on this socket, not `codrive`'s, for the same reason every other
    /// op here does — and with one consequence worth stating plainly: on the
    /// appliance the agent runs as a different system user and structurally
    /// cannot reach this socket, while on a same-uid development machine it
    /// could. `codrive_ops.rs`'s module doc has the full trust boundary,
    /// including why Super+Esc remains the only agent-unreachable stop.
    CodriveDrive { action: String },
    /// D9-bug3/D9-bug4 (2026-08-24).
    /// `{"op":"set_session_locked","params":{"locked":true}}` — the session
    /// shell tells comp whether its lock screen is up.
    ///
    /// comp has no way to work this out for itself (a lock screen is just
    /// pixels on a layer surface), and three compositor-owned behaviours
    /// depend on it: ordinary windows are not painted, only layer surfaces can
    /// take pointer input, and keys bypass the input method's keyboard grab.
    /// See `crate::session_lock`'s module doc for the full rule and for the
    /// smithay source readings behind the keyboard half.
    ///
    /// This is an ACTION with a real, security-relevant effect, so it is
    /// always audited — including the no-op case where the shell re-announces
    /// a state comp already holds (the shell announces `false` at boot, and a
    /// re-announcement after a comp restart is exactly the case an audit
    /// reader wants to see).
    ///
    /// Like [`Self::SetTheme`] there is **no persistence** on comp's side: the
    /// shell owns the credential check and re-announces at every boot, so comp
    /// only tracks the live value for this process's lifetime. A shell that
    /// dies while locked leaves comp locked, which is the safe direction —
    /// see `session_lock`'s "Fail-closed choices".
    SetSessionLocked { locked: bool },
}

impl ShellControlRequest {
    /// Stable short name for tracing/audit fields — same motive as
    /// `duduclaw-sysd::protocol::SysdRequest::verb_name`.
    pub fn op_name(&self) -> &'static str {
        match self {
            ShellControlRequest::ListWindows => "list_windows",
            ShellControlRequest::FocusWindow { .. } => "focus_window",
            ShellControlRequest::GetCursorSource => "get_cursor_source",
            ShellControlRequest::SetCursorSource { .. } => "set_cursor_source",
            ShellControlRequest::SetCursorSize { .. } => "set_cursor_size",
            ShellControlRequest::GetOutputs => "get_outputs",
            ShellControlRequest::SetOutputMode { .. } => "set_output_mode",
            ShellControlRequest::SetOutputScale { .. } => "set_output_scale",
            ShellControlRequest::SetTheme { .. } => "set_theme",
            ShellControlRequest::TakeShellIntents => "take_shell_intents",
            ShellControlRequest::CodriveStatus => "codrive_status",
            ShellControlRequest::CodriveDrive { .. } => "codrive_drive",
            ShellControlRequest::SetSessionLocked { .. } => "set_session_locked",
        }
    }

    /// A7c: true iff this op may be reached by an AGENT-authority peer
    /// (`listener::PeerAuthority::Agent` — comp's own uid still authorizes
    /// the full set as `Human`, unchanged). Closed allowlist, deliberately
    /// narrower than the full op set: exactly the appearance-preference ops
    /// A7a's design doc (`commercial/docs/DESIGN-os-self-drive-2026-08.md`
    /// §5) already rated `requires_approval=false` (cursor size/source,
    /// theme, output scale) plus their read-only counterpart
    /// `get_outputs`. Everything else stays Human-only:
    /// `list_windows`/`focus_window` are a HUMAN-facing dock surface,
    /// `codrive_status`/`codrive_drive` are the co-drive handover verbs
    /// (agent reaching those would let it hand the wheel to/from itself),
    /// `take_shell_intents` drains a HUMAN global-hotkey queue, and
    /// `set_session_locked` is a security-relevant signal the session shell
    /// alone should ever send. `set_output_mode` is also excluded even
    /// though it always refuses today (see `mod.rs`'s doc) — narrowest
    /// allowlist that satisfies the task, not "whatever happens to be
    /// harmless right now".
    pub fn agent_allowed(&self) -> bool {
        matches!(
            self,
            ShellControlRequest::GetCursorSource
                | ShellControlRequest::SetCursorSource { .. }
                | ShellControlRequest::SetCursorSize { .. }
                | ShellControlRequest::GetOutputs
                | ShellControlRequest::SetOutputScale { .. }
                | ShellControlRequest::SetTheme { .. }
        )
    }
}

/// WP-comp-shell-display: one entry in a `get_outputs` output's `modes`
/// list — a mode that output has actually reported (via
/// `smithay::output::Output::modes()`), never a synthesized one. See
/// `mod.rs`'s module doc for what the two backends actually populate this
/// with.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ShellOutputMode {
    pub width: i64,
    pub height: i64,
    pub refresh_mhz: i64,
    /// True iff this is the output's `Output::preferred_mode()`.
    pub preferred: bool,
    /// True iff this is the output's `Output::current_mode()` — the one
    /// actually being scanned out right now.
    pub current: bool,
}

/// WP-comp-shell-display: one `get_outputs` row. `make`/`model`/
/// `description`/`physical_*_mm` come straight from `Output::
/// physical_properties()` — real hardware reports real values, the winit
/// backend's synthetic output reports `0`/`0` and the fixed strings
/// `winit_backend.rs::init_winit` sets, honestly.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ShellOutputInfo {
    pub name: String,
    pub description: String,
    pub make: String,
    pub model: String,
    /// The output's *current* mode's dimensions/refresh — mirrors the
    /// matching entry in `modes` (the one with `current: true`), duplicated
    /// at the top level so a settings page doesn't have to search the list
    /// just to show "what is this screen doing right now".
    pub width: i64,
    pub height: i64,
    pub refresh_mhz: i64,
    /// `Output::current_scale()`'s fractional scale × 100, rounded to the
    /// nearest integer — never a float on the wire, same reasoning
    /// `SetCursorSize` gives for using `i64` throughout this socket.
    pub scale_pct: i64,
    pub physical_width_mm: i64,
    pub physical_height_mm: i64,
    /// Exactly `Output::modes()`, mapped field-for-field. Empty if and only
    /// if the real `Output::modes()` was empty — see `mod.rs`'s module doc
    /// for what this crate's two backends actually populate it with (found
    /// by reading `Output::change_current_state`/`set_preferred`'s own
    /// source, not assumed).
    pub modes: Vec<ShellOutputMode>,
    /// Whether `set_output_mode` can actually apply a mode change to this
    /// output on this build. **Always `false`** today — see `mod.rs`'s
    /// module doc for the concrete reason. Per-output (not a global
    /// constant) so a future round that lands real modesetting for one
    /// backend does not have to lie about the other.
    pub mode_switch_supported: bool,
}

/// True iff `(width, height, refresh_mhz)` exactly matches one of `modes` —
/// the "is this a real, known mode for this output" check `set_output_mode`
/// needs before it can even consider a switch. Pure and typed against
/// `smithay::output::Mode` directly (a plain, GPU-free struct — both
/// backends construct it the same way, e.g. `winit_backend.rs`'s
/// `Mode { size: backend.window_size(), refresh: 60_000 }`), so this is
/// testable with no real backend, same reasoning `codrive::window_target`'s
/// pure matching functions already established for this crate.
///
/// `i64 -> i32` uses `try_from`, never `as`: a caller-sent value outside
/// `i32`'s range (only pre-filtered to `> 0` by `listener::validate`, which
/// has no access to the real output and so cannot bound the top) must read
/// as "not a match" — never wrap into a bogus in-range value via a lossy
/// cast, which could otherwise coincide with a real mode by accident.
pub(crate) fn mode_request_matches(width: i64, height: i64, refresh_mhz: i64, modes: &[Mode]) -> bool {
    let (Ok(w), Ok(h), Ok(refresh)) =
        (i32::try_from(width), i32::try_from(height), i32::try_from(refresh_mhz))
    else {
        return false;
    };
    modes.iter().any(|m| m.size.w == w && m.size.h == h && m.refresh == refresh)
}

/// One `list_windows` row. `app_id`/`title` mirror
/// `codrive::window_target::window_identity`'s own return shape exactly
/// (both are `None` whenever a real client never set that xdg-shell
/// property — an honest gap, not a placeholder string).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShellWindowInfo {
    pub app_id: Option<String>,
    pub title: Option<String>,
    /// True iff this window currently holds the HUMAN seat's keyboard
    /// focus (`DuduclawComp::seat`, never the agent seat — a dock is a
    /// human-facing surface, so "which window is focused" must answer the
    /// human's own question, not report the agent's).
    pub focused: bool,
    /// WM-3: true iff this window is minimized (`crate::minimize`) — alive and
    /// switchable, but not on screen.
    ///
    /// **Additive, and deliberately so.** Minimized windows are now *in* the
    /// `list_windows` answer, because a dock that cannot see them cannot bring
    /// them back and this compositor has no other task bar. The op's semantics
    /// are otherwise unchanged, and the field is safe to add without touching
    /// `duduclaw-shell`: its `comp_client::CompWindow` derives a plain
    /// `Deserialize`, which ignores unknown fields — so the shipped shell
    /// simply does not see this yet, and shows a minimized window in its dock
    /// exactly as it shows a mapped one. Rendering it *differently* is a
    /// shell-side change for a later round.
    pub minimized: bool,
}

/// Response envelope — one flat struct with `Option` fields
/// (`#[serde(skip_serializing_if)]` trims absent ones from the wire), same
/// shape convention as `duduclaw-sysd::protocol::SysdResponse`. Exactly one
/// of `windows` / (`matched_app_id` or `matched_title_prefix` or neither,
/// on a `focus_window` miss) / `error` is meaningfully populated per op —
/// see the three constructors below for the three real shapes this crate
/// ever emits.
#[derive(Debug, Clone, Serialize)]
pub struct ShellControlResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub windows: Option<Vec<ShellWindowInfo>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_app_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_title_prefix: Option<String>,
    /// CUR-2: populated by `get_cursor_source` / `set_cursor_source` only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<CursorSourceInfo>,
    /// WP-comp-shell-display: populated by `get_outputs` only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outputs: Option<Vec<ShellOutputInfo>>,
    /// A1: populated by `take_shell_intents` only. Always `Some` (even
    /// `Some(vec![])`) on that op's response — see [`Self::intents`]'s own
    /// doc for why the empty case must serialize as `"intents":[]` rather
    /// than being skipped like every other `Option` field here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intents: Option<Vec<String>>,
    /// A2: populated by `codrive_status` / `codrive_drive` only. Additive —
    /// every other constructor leaves it `None`, so no already-shipped
    /// response shape changed a single byte.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codrive: Option<CodriveStatusInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ShellControlResponse {
    pub fn windows(windows: Vec<ShellWindowInfo>) -> Self {
        Self { ok: true, windows: Some(windows), matched_app_id: None, matched_title_prefix: None, cursor: None, outputs: None, intents: None, codrive: None, error: None }
    }

    /// CUR-2: the `get_cursor_source` / `set_cursor_source` success shape.
    pub fn cursor(info: CursorSourceInfo) -> Self {
        Self { ok: true, windows: None, matched_app_id: None, matched_title_prefix: None, cursor: Some(info), outputs: None, intents: None, codrive: None, error: None }
    }

    /// A `focus_window` hit — exactly one of `matched_app_id`/
    /// `matched_title_prefix` is `Some`, mirroring `codrive::window_target::
    /// WindowMatch`'s own two variants (never both, never neither).
    pub fn focused_by_app_id(app_id: String) -> Self {
        Self { ok: true, windows: None, matched_app_id: Some(app_id), matched_title_prefix: None, cursor: None, outputs: None, intents: None, codrive: None, error: None }
    }

    pub fn focused_by_title_prefix(title: String) -> Self {
        Self { ok: true, windows: None, matched_app_id: None, matched_title_prefix: Some(title), cursor: None, outputs: None, intents: None, codrive: None, error: None }
    }

    /// WP-comp-shell-display: the `get_outputs` success shape.
    pub fn outputs(outputs: Vec<ShellOutputInfo>) -> Self {
        Self { ok: true, windows: None, matched_app_id: None, matched_title_prefix: None, cursor: None, outputs: Some(outputs), intents: None, codrive: None, error: None }
    }

    /// D2: the bare `set_theme` success shape — `{"ok":true}` and nothing
    /// else. `duduclaw-shell`'s own client (`comp_client.rs`) only reads
    /// `ok`, so there is nothing this op needs to echo back; keeping the
    /// response minimal is the exact wire contract the shell side was
    /// written against.
    pub fn ok() -> Self {
        Self { ok: true, windows: None, matched_app_id: None, matched_title_prefix: None, cursor: None, outputs: None, intents: None, codrive: None, error: None }
    }

    /// A1: the `take_shell_intents` success shape. Always `Some(intents)`,
    /// even when `intents` is empty — an EMPTY vec still serializes as
    /// `"intents":[]` because the field type is `Option<Vec<_>>` with
    /// `skip_serializing_if = "Option::is_none"` (which only checks the
    /// outer `Option`, never the inner `Vec`'s length). That distinction is
    /// load-bearing: the shell's short-poll contract expects the key to
    /// always be present on this op's response, unlike every other optional
    /// field on this envelope.
    pub fn intents(intents: Vec<String>) -> Self {
        Self { ok: true, windows: None, matched_app_id: None, matched_title_prefix: None, cursor: None, outputs: None, intents: Some(intents), codrive: None, error: None }
    }

    /// A2: the `codrive_status` / `codrive_drive` success shape. Both ops
    /// answer with the same block, so a caller never has to branch on which
    /// one it sent to read the state back.
    pub fn codrive(info: CodriveStatusInfo) -> Self {
        Self { ok: true, windows: None, matched_app_id: None, matched_title_prefix: None, cursor: None, outputs: None, intents: None, codrive: Some(info), error: None }
    }

    pub fn err(error: impl Into<String>) -> Self {
        Self { ok: false, windows: None, matched_app_id: None, matched_title_prefix: None, cursor: None, outputs: None, intents: None, codrive: None, error: Some(error.into()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_windows_wire_shape_has_no_extra_fields() {
        let s = serde_json::to_string(&ShellControlRequest::ListWindows).unwrap();
        assert_eq!(s, r#"{"op":"list_windows"}"#);
        let back: ShellControlRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(back, ShellControlRequest::ListWindows);
    }

    #[test]
    fn focus_window_wire_shape_round_trips_with_query() {
        let req = ShellControlRequest::FocusWindow { query: "foot-A".to_string() };
        let s = serde_json::to_string(&req).unwrap();
        assert_eq!(s, r#"{"op":"focus_window","params":{"query":"foot-A"}}"#);
        let back: ShellControlRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn unknown_op_fails_to_parse() {
        let r: Result<ShellControlRequest, _> = serde_json::from_str(r#"{"op":"shutdown"}"#);
        assert!(r.is_err());
    }

    #[test]
    fn unknown_field_is_rejected() {
        let r: Result<ShellControlRequest, _> =
            serde_json::from_str(r#"{"op":"list_windows","extra":"field"}"#);
        assert!(r.is_err(), "deny_unknown_fields must reject a stray extra key");
    }

    #[test]
    fn malformed_json_fails_to_parse() {
        let r: Result<ShellControlRequest, _> = serde_json::from_str("{not json");
        assert!(r.is_err());
    }

    #[test]
    fn op_name_is_stable_and_does_not_leak_query_value() {
        assert_eq!(ShellControlRequest::ListWindows.op_name(), "list_windows");
        assert_eq!(
            ShellControlRequest::FocusWindow { query: "secret-ish-title".into() }.op_name(),
            "focus_window"
        );
    }

    #[test]
    fn windows_response_omits_matched_and_error_fields() {
        let resp = ShellControlResponse::windows(vec![ShellWindowInfo {
            app_id: Some("foot-A".into()),
            title: Some("foot".into()),
            focused: true,
            minimized: false,
        }]);
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains(r#""ok":true"#));
        assert!(s.contains(r#""windows""#));
        assert!(!s.contains("matched_app_id"));
        assert!(!s.contains("matched_title_prefix"));
        assert!(!s.contains("\"error\""));
    }

    #[test]
    fn a_window_row_carries_the_wm3_minimized_flag_on_the_wire() {
        // The dock resolves `focus_window` against this list, so a minimized
        // window has to be IN it — and has to be distinguishable, or a dock can
        // never render the two states differently.
        let resp = ShellControlResponse::windows(vec![
            ShellWindowInfo {
                app_id: Some("foot-A".into()),
                title: Some("visible".into()),
                focused: true,
                minimized: false,
            },
            ShellWindowInfo {
                app_id: Some("foot-B".into()),
                title: Some("parked".into()),
                focused: false,
                minimized: true,
            },
        ]);
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains(r#""minimized":false"#));
        assert!(s.contains(r#""minimized":true"#));
    }

    #[test]
    fn focused_by_app_id_response_omits_windows_and_title_prefix() {
        let resp = ShellControlResponse::focused_by_app_id("foot-A".into());
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains(r#""matched_app_id":"foot-A""#));
        assert!(!s.contains("\"windows\""));
        assert!(!s.contains("matched_title_prefix"));
    }

    #[test]
    fn err_response_omits_every_success_field() {
        let resp = ShellControlResponse::err("not_found");
        let s = serde_json::to_string(&resp).unwrap();
        assert_eq!(s, r#"{"ok":false,"error":"not_found"}"#);
    }

    // ── CUR-2 cursor ops ─────────────────────────────────────────────────

    #[test]
    fn get_cursor_source_wire_shape_has_no_params() {
        let s = serde_json::to_string(&ShellControlRequest::GetCursorSource).unwrap();
        assert_eq!(s, r#"{"op":"get_cursor_source"}"#);
        let back: ShellControlRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(back, ShellControlRequest::GetCursorSource);
    }

    #[test]
    fn set_cursor_source_wire_shape_round_trips() {
        let req = ShellControlRequest::SetCursorSource { source: "brand".to_string() };
        let s = serde_json::to_string(&req).unwrap();
        assert_eq!(s, r#"{"op":"set_cursor_source","params":{"source":"brand"}}"#);
        let back: ShellControlRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn cursor_ops_reject_stray_fields_like_every_other_op() {
        let bad: Result<ShellControlRequest, _> =
            serde_json::from_str(r#"{"op":"get_cursor_source","params":{}}"#);
        assert!(bad.is_err(), "a unit variant must not accept a params object");
        let bad: Result<ShellControlRequest, _> = serde_json::from_str(
            r#"{"op":"set_cursor_source","params":{"source":"brand","persist":false}}"#,
        );
        assert!(bad.is_err(), "deny_unknown_fields must reject an extra param");
        let bad: Result<ShellControlRequest, _> =
            serde_json::from_str(r#"{"op":"set_cursor_source"}"#);
        assert!(bad.is_err(), "the source param is not optional");
    }

    #[test]
    fn cursor_op_names_are_stable_and_do_not_leak_the_value() {
        assert_eq!(ShellControlRequest::GetCursorSource.op_name(), "get_cursor_source");
        assert_eq!(
            ShellControlRequest::SetCursorSource { source: "brand".into() }.op_name(),
            "set_cursor_source"
        );
    }

    #[test]
    fn cursor_response_carries_the_block_and_omits_the_window_fields() {
        let resp = ShellControlResponse::cursor(CursorSourceInfo {
            source: "system".into(),
            requested: "brand".into(),
            theme: "Adwaita".into(),
            origin: "runtime".into(),
            size: 24,
            effective_size: 24,
            size_env_pinned: false,
            env_pinned: false,
            persisted: Some(true),
            persist_error: None,
        });
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains(r#""ok":true"#));
        assert!(s.contains(r#""source":"system""#));
        assert!(s.contains(r#""requested":"brand""#));
        assert!(s.contains(r#""origin":"runtime""#));
        assert!(s.contains(r#""env_pinned":false"#));
        assert!(s.contains(r#""persisted":true"#));
        assert!(!s.contains("persist_error"), "absent on success");
        assert!(!s.contains("\"windows\""));
        assert!(!s.contains("matched_app_id"));
        assert!(!s.contains("\"error\""));
    }

    #[test]
    fn a_get_reply_omits_the_set_only_fields() {
        // `persisted`/`persist_error` are meaningless for a query and must
        // not appear as `null` — a settings UI reading `persisted === false`
        // out of a GET would wrongly warn "this will not survive a restart".
        let resp = ShellControlResponse::cursor(CursorSourceInfo {
            source: "brand".into(),
            requested: "brand".into(),
            theme: "DuDuClaw".into(),
            origin: "persisted".into(),
            size: 48,
            effective_size: 48,
            size_env_pinned: false,
            env_pinned: true,
            persisted: None,
            persist_error: None,
        });
        let s = serde_json::to_string(&resp).unwrap();
        // Matched as a JSON KEY, not as a substring — `"origin":"persisted"`
        // legitimately contains the word.
        assert!(!s.contains(r#""persisted":"#), "unexpected: {s}");
        assert!(!s.contains(r#""persist_error":"#), "unexpected: {s}");
        assert!(s.contains(r#""origin":"persisted""#));
        assert!(s.contains(r#""env_pinned":true"#));
    }

    // ── CUR-3 cursor size ────────────────────────────────────────────────

    #[test]
    fn set_cursor_size_wire_shape_round_trips() {
        let req = ShellControlRequest::SetCursorSize { size: 32 };
        let s = serde_json::to_string(&req).unwrap();
        assert_eq!(s, r#"{"op":"set_cursor_size","params":{"size":32}}"#);
        let back: ShellControlRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn set_cursor_size_parses_the_shapes_the_shell_can_send() {
        // The shell sends one of the five segment values. Each must reach the
        // validator as a SIZE, not die at the JSON layer.
        for n in [24, 32, 48, 64, 96] {
            let raw = format!(r#"{{"op":"set_cursor_size","params":{{"size":{n}}}}}"#);
            let parsed: ShellControlRequest = serde_json::from_str(&raw).unwrap();
            assert_eq!(parsed, ShellControlRequest::SetCursorSize { size: n });
        }
        // An out-of-range or negative value must PARSE (so `validate` can
        // answer `invalid_cursor_size`) rather than fail as a type error —
        // this is exactly why the field is `i64`.
        for raw in [
            r#"{"op":"set_cursor_size","params":{"size":-5}}"#,
            r#"{"op":"set_cursor_size","params":{"size":0}}"#,
            r#"{"op":"set_cursor_size","params":{"size":100000}}"#,
        ] {
            assert!(
                serde_json::from_str::<ShellControlRequest>(raw).is_ok(),
                "{raw} must parse so the validator can refuse it as a size"
            );
        }
    }

    #[test]
    fn set_cursor_size_rejects_non_integer_and_stray_fields() {
        // A float or a string genuinely IS a schema violation, not an
        // out-of-range size — `parse_error` is the honest answer there.
        for raw in [
            r#"{"op":"set_cursor_size","params":{"size":3.5}}"#,
            r#"{"op":"set_cursor_size","params":{"size":"32"}}"#,
            r#"{"op":"set_cursor_size","params":{"size":null}}"#,
            r#"{"op":"set_cursor_size","params":{"size":32,"persist":false}}"#,
            r#"{"op":"set_cursor_size","params":{}}"#,
            r#"{"op":"set_cursor_size"}"#,
        ] {
            assert!(
                serde_json::from_str::<ShellControlRequest>(raw).is_err(),
                "{raw} must not parse"
            );
        }
    }

    #[test]
    fn set_cursor_size_op_name_is_stable_and_does_not_leak_the_value() {
        assert_eq!(
            ShellControlRequest::SetCursorSize { size: 96 }.op_name(),
            "set_cursor_size"
        );
    }

    #[test]
    fn the_cursor_block_carries_size_and_effective_size_as_the_shell_expects() {
        // The contract the shell was written against: `cursor.size` is an
        // integer, and `effective_size` rides alongside it (the shell tolerates
        // the extra key, and needs it to avoid claiming 96 px when 64 is drawn).
        let resp = ShellControlResponse::cursor(CursorSourceInfo {
            source: "system".into(),
            requested: "system".into(),
            theme: "SparseTheme".into(),
            origin: "default".into(),
            size: 96,
            effective_size: 64,
            size_env_pinned: true,
            env_pinned: false,
            persisted: Some(true),
            persist_error: None,
        });
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains(r#""size":96"#), "unexpected: {s}");
        assert!(s.contains(r#""effective_size":64"#), "unexpected: {s}");
        assert!(s.contains(r#""size_env_pinned":true"#), "unexpected: {s}");
        // Never omitted, unlike `persisted`: a settings page reading a missing
        // `size` would have nothing to highlight.
        let always_present = ShellControlResponse::cursor(CursorSourceInfo {
            source: "system".into(),
            requested: "system".into(),
            theme: "Adwaita".into(),
            origin: "default".into(),
            size: 24,
            effective_size: 24,
            size_env_pinned: false,
            env_pinned: false,
            persisted: None,
            persist_error: None,
        });
        let s = serde_json::to_string(&always_present).unwrap();
        assert!(s.contains(r#""size":24"#), "unexpected: {s}");
        assert!(s.contains(r#""effective_size":24"#), "unexpected: {s}");
        assert!(s.contains(r#""size_env_pinned":false"#), "unexpected: {s}");
    }

    // ── WP-comp-shell-display: get_outputs / set_output_mode / set_output_scale ──

    #[test]
    fn get_outputs_wire_shape_has_no_params() {
        let s = serde_json::to_string(&ShellControlRequest::GetOutputs).unwrap();
        assert_eq!(s, r#"{"op":"get_outputs"}"#);
        let back: ShellControlRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(back, ShellControlRequest::GetOutputs);
    }

    #[test]
    fn set_output_mode_wire_shape_round_trips() {
        let req = ShellControlRequest::SetOutputMode {
            output: "Virtual-1".to_string(),
            width: 1920,
            height: 1080,
            refresh_mhz: 60000,
        };
        let s = serde_json::to_string(&req).unwrap();
        assert_eq!(
            s,
            r#"{"op":"set_output_mode","params":{"output":"Virtual-1","width":1920,"height":1080,"refresh_mhz":60000}}"#
        );
        let back: ShellControlRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn set_output_scale_wire_shape_round_trips() {
        let req = ShellControlRequest::SetOutputScale { output: "Virtual-1".to_string(), scale_pct: 125 };
        let s = serde_json::to_string(&req).unwrap();
        assert_eq!(s, r#"{"op":"set_output_scale","params":{"output":"Virtual-1","scale_pct":125}}"#);
        let back: ShellControlRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn output_ops_reject_stray_fields_and_missing_params_like_every_other_op() {
        let bad: Result<ShellControlRequest, _> =
            serde_json::from_str(r#"{"op":"get_outputs","params":{}}"#);
        assert!(bad.is_err(), "a unit variant must not accept a params object");

        for raw in [
            r#"{"op":"set_output_mode","params":{"output":"Virtual-1","width":1920,"height":1080,"refresh_mhz":60000,"extra":1}}"#,
            r#"{"op":"set_output_mode","params":{"width":1920,"height":1080,"refresh_mhz":60000}}"#,
            r#"{"op":"set_output_mode","params":{"output":"Virtual-1","height":1080,"refresh_mhz":60000}}"#,
            r#"{"op":"set_output_mode","params":{"output":"Virtual-1","width":1920,"refresh_mhz":60000}}"#,
            r#"{"op":"set_output_mode","params":{"output":"Virtual-1","width":1920,"height":1080}}"#,
            r#"{"op":"set_output_mode","params":{}}"#,
            r#"{"op":"set_output_mode"}"#,
            r#"{"op":"set_output_scale","params":{"output":"Virtual-1","scale_pct":125,"extra":1}}"#,
            r#"{"op":"set_output_scale","params":{"scale_pct":125}}"#,
            r#"{"op":"set_output_scale","params":{"output":"Virtual-1"}}"#,
            r#"{"op":"set_output_scale","params":{}}"#,
            r#"{"op":"set_output_scale"}"#,
        ] {
            assert!(
                serde_json::from_str::<ShellControlRequest>(raw).is_err(),
                "{raw} must not parse"
            );
        }
    }

    #[test]
    fn set_output_mode_parses_out_of_range_numbers_so_the_validator_can_refuse_them() {
        // Same reasoning as `set_cursor_size`'s equivalent test: these are
        // schema-valid JSON, so `invalid_mode` (not `parse_error`) must be
        // the eventual answer, which means the parse itself must succeed.
        for raw in [
            r#"{"op":"set_output_mode","params":{"output":"Virtual-1","width":-1,"height":1080,"refresh_mhz":60000}}"#,
            r#"{"op":"set_output_mode","params":{"output":"Virtual-1","width":1920,"height":0,"refresh_mhz":60000}}"#,
            r#"{"op":"set_output_mode","params":{"output":"Virtual-1","width":1920,"height":1080,"refresh_mhz":0}}"#,
            r#"{"op":"set_output_mode","params":{"output":"Virtual-1","width":9999999999999,"height":1080,"refresh_mhz":60000}}"#,
        ] {
            assert!(
                serde_json::from_str::<ShellControlRequest>(raw).is_ok(),
                "{raw} must parse so the validator can refuse it as a mode"
            );
        }
        for raw in [
            r#"{"op":"set_output_scale","params":{"output":"Virtual-1","scale_pct":0}}"#,
            r#"{"op":"set_output_scale","params":{"output":"Virtual-1","scale_pct":-100}}"#,
            r#"{"op":"set_output_scale","params":{"output":"Virtual-1","scale_pct":999}}"#,
        ] {
            assert!(
                serde_json::from_str::<ShellControlRequest>(raw).is_ok(),
                "{raw} must parse so the validator can refuse it as a scale"
            );
        }
    }

    #[test]
    fn set_output_mode_and_scale_reject_non_integer_numbers() {
        // A float or a string genuinely IS a schema violation — same split
        // `set_cursor_size` makes for its own numeric field.
        for raw in [
            r#"{"op":"set_output_mode","params":{"output":"Virtual-1","width":1920.5,"height":1080,"refresh_mhz":60000}}"#,
            r#"{"op":"set_output_mode","params":{"output":"Virtual-1","width":"1920","height":1080,"refresh_mhz":60000}}"#,
            r#"{"op":"set_output_mode","params":{"output":"Virtual-1","width":1920,"height":null,"refresh_mhz":60000}}"#,
            r#"{"op":"set_output_scale","params":{"output":"Virtual-1","scale_pct":100.0}}"#,
            r#"{"op":"set_output_scale","params":{"output":"Virtual-1","scale_pct":"100"}}"#,
            r#"{"op":"set_output_scale","params":{"output":"Virtual-1","scale_pct":null}}"#,
        ] {
            assert!(
                serde_json::from_str::<ShellControlRequest>(raw).is_err(),
                "{raw} must not parse"
            );
        }
    }

    #[test]
    fn output_op_names_are_stable_and_do_not_leak_values() {
        assert_eq!(ShellControlRequest::GetOutputs.op_name(), "get_outputs");
        assert_eq!(
            ShellControlRequest::SetOutputMode {
                output: "secret-panel-name".into(),
                width: 1920,
                height: 1080,
                refresh_mhz: 60000
            }
            .op_name(),
            "set_output_mode"
        );
        assert_eq!(
            ShellControlRequest::SetOutputScale { output: "secret-panel-name".into(), scale_pct: 125 }
                .op_name(),
            "set_output_scale"
        );
    }

    #[test]
    fn outputs_response_carries_the_wire_shape_the_shell_was_written_against() {
        let resp = ShellControlResponse::outputs(vec![ShellOutputInfo {
            name: "Virtual-1".into(),
            description: "DuDuClaw - duduclaw-comp (winit) - Virtual-1".into(),
            make: "DuDuClaw".into(),
            model: "duduclaw-comp (winit)".into(),
            width: 1920,
            height: 1080,
            refresh_mhz: 60000,
            scale_pct: 100,
            physical_width_mm: 0,
            physical_height_mm: 0,
            modes: vec![ShellOutputMode {
                width: 1920,
                height: 1080,
                refresh_mhz: 60000,
                preferred: true,
                current: true,
            }],
            mode_switch_supported: false,
        }]);
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains(r#""ok":true"#));
        assert!(s.contains(r#""name":"Virtual-1""#));
        assert!(s.contains(r#""width":1920"#));
        assert!(s.contains(r#""height":1080"#));
        assert!(s.contains(r#""refresh_mhz":60000"#));
        assert!(s.contains(r#""scale_pct":100"#));
        assert!(s.contains(r#""physical_width_mm":0"#));
        assert!(s.contains(r#""physical_height_mm":0"#));
        assert!(s.contains(r#""preferred":true"#));
        assert!(s.contains(r#""current":true"#));
        assert!(s.contains(r#""mode_switch_supported":false"#));
        // Envelope discipline: the other ops' fields must not leak in.
        assert!(!s.contains("\"windows\""));
        assert!(!s.contains("matched_app_id"));
        assert!(!s.contains("matched_title_prefix"));
        assert!(!s.contains("\"cursor\""));
        assert!(!s.contains("\"error\""));
    }

    #[test]
    fn an_empty_modes_list_is_emitted_honestly_not_padded() {
        // See mod.rs's module doc: `Output::modes()` returning empty is the
        // honest answer for a not-yet-configured output, never synthesized.
        let resp = ShellControlResponse::outputs(vec![ShellOutputInfo {
            name: "Virtual-1".into(),
            description: String::new(),
            make: String::new(),
            model: String::new(),
            width: 1920,
            height: 1080,
            refresh_mhz: 60000,
            scale_pct: 100,
            physical_width_mm: 0,
            physical_height_mm: 0,
            modes: vec![],
            mode_switch_supported: false,
        }]);
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains(r#""modes":[]"#), "unexpected: {s}");
    }

    #[test]
    fn non_output_responses_omit_the_outputs_field() {
        // Regression guard for the additive field: every other constructor
        // must still omit `outputs` entirely, never emit `"outputs":null`.
        for s in [
            serde_json::to_string(&ShellControlResponse::windows(vec![])).unwrap(),
            serde_json::to_string(&ShellControlResponse::focused_by_app_id("x".into())).unwrap(),
            serde_json::to_string(&ShellControlResponse::err("not_found")).unwrap(),
        ] {
            assert!(!s.contains("outputs"), "unexpected: {s}");
        }
    }

    // ── D2 set_theme ─────────────────────────────────────────────────────

    #[test]
    fn set_theme_wire_shape_round_trips() {
        let req = ShellControlRequest::SetTheme { theme: "dark".to_string() };
        let s = serde_json::to_string(&req).unwrap();
        assert_eq!(s, r#"{"op":"set_theme","params":{"theme":"dark"}}"#);
        let back: ShellControlRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn set_theme_rejects_stray_fields_and_a_missing_theme() {
        for raw in [
            r#"{"op":"set_theme","params":{"theme":"dark","persist":false}}"#,
            r#"{"op":"set_theme","params":{}}"#,
            r#"{"op":"set_theme"}"#,
            r#"{"op":"set_theme","params":{"theme":123}}"#,
            r#"{"op":"set_theme","params":{"theme":null}}"#,
        ] {
            assert!(
                serde_json::from_str::<ShellControlRequest>(raw).is_err(),
                "{raw} must not parse"
            );
        }
    }

    #[test]
    fn set_theme_op_name_is_stable_and_does_not_leak_the_value() {
        assert_eq!(
            ShellControlRequest::SetTheme { theme: "dark".into() }.op_name(),
            "set_theme"
        );
    }

    #[test]
    fn ok_response_is_the_bare_wire_shape_the_shell_was_written_against() {
        let s = serde_json::to_string(&ShellControlResponse::ok()).unwrap();
        assert_eq!(s, r#"{"ok":true}"#);
    }

    // ── A1 take_shell_intents ───────────────────────────────────────────

    #[test]
    fn take_shell_intents_wire_shape_has_no_params() {
        let s = serde_json::to_string(&ShellControlRequest::TakeShellIntents).unwrap();
        assert_eq!(s, r#"{"op":"take_shell_intents"}"#);
        let back: ShellControlRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(back, ShellControlRequest::TakeShellIntents);
    }

    #[test]
    fn take_shell_intents_rejects_a_stray_params_object() {
        let bad: Result<ShellControlRequest, _> =
            serde_json::from_str(r#"{"op":"take_shell_intents","params":{}}"#);
        assert!(bad.is_err(), "a unit variant must not accept a params object");
    }

    #[test]
    fn take_shell_intents_op_name_is_stable() {
        assert_eq!(ShellControlRequest::TakeShellIntents.op_name(), "take_shell_intents");
    }

    // ── D9-bug3/D9-bug4 set_session_locked ──────────────────────────────

    /// Pinned to the LITERAL wire line the shell's hand-mirrored client
    /// builds (`duduclaw-shell/src/comp_client.rs::set_session_locked`),
    /// for the same reason `params.rs`'s reserved-band test pins literal
    /// numbers: the two crates cannot depend on each other, so this string
    /// IS the contract.
    #[test]
    fn set_session_locked_wire_shape_is_what_the_shell_sends() {
        for (locked, raw) in [
            (true, r#"{"op":"set_session_locked","params":{"locked":true}}"#),
            (false, r#"{"op":"set_session_locked","params":{"locked":false}}"#),
        ] {
            let req = ShellControlRequest::SetSessionLocked { locked };
            assert_eq!(serde_json::to_string(&req).unwrap(), raw);
            let back: ShellControlRequest = serde_json::from_str(raw).unwrap();
            assert_eq!(back, req);
        }
    }

    #[test]
    fn set_session_locked_refuses_anything_that_is_not_a_bool() {
        for raw in [
            r#"{"op":"set_session_locked"}"#,
            r#"{"op":"set_session_locked","params":{}}"#,
            r#"{"op":"set_session_locked","params":{"locked":"true"}}"#,
            r#"{"op":"set_session_locked","params":{"locked":1}}"#,
            r#"{"op":"set_session_locked","params":{"locked":null}}"#,
            // `deny_unknown_fields`: a typo'd extra key must not be ignored,
            // or a caller could believe it had asked for something it hadn't.
            r#"{"op":"set_session_locked","params":{"locked":true,"reason":"idle"}}"#,
        ] {
            assert!(
                serde_json::from_str::<ShellControlRequest>(raw).is_err(),
                "{raw} must not parse"
            );
        }
    }

    #[test]
    fn set_session_locked_op_name_is_stable() {
        assert_eq!(
            ShellControlRequest::SetSessionLocked { locked: true }.op_name(),
            "set_session_locked"
        );
        assert_eq!(
            ShellControlRequest::SetSessionLocked { locked: false }.op_name(),
            "set_session_locked"
        );
    }

    #[test]
    fn shell_intent_wire_token_is_the_documented_spelling() {
        assert_eq!(ShellIntent::GlobalTaskBar.as_str(), "global_task_bar");
    }

    #[test]
    fn an_empty_drain_serializes_the_key_as_an_empty_array_not_omitted() {
        // The whole point of `intents` being `Option<Vec<_>>` rather than a
        // plain `Vec<_>` with `skip_serializing_if = "Vec::is_empty"`: the
        // shell's short-poll contract wants `"intents":[]` on every quiet
        // poll, never a response with no `intents` key at all.
        let s = serde_json::to_string(&ShellControlResponse::intents(vec![])).unwrap();
        assert_eq!(s, r#"{"ok":true,"intents":[]}"#);
    }

    #[test]
    fn a_non_empty_drain_carries_the_intent_tokens() {
        let s = serde_json::to_string(&ShellControlResponse::intents(vec![
            ShellIntent::GlobalTaskBar.as_str().to_string(),
        ]))
        .unwrap();
        assert_eq!(s, r#"{"ok":true,"intents":["global_task_bar"]}"#);
    }

    #[test]
    fn non_intents_responses_omit_the_intents_field() {
        for s in [
            serde_json::to_string(&ShellControlResponse::windows(vec![])).unwrap(),
            serde_json::to_string(&ShellControlResponse::ok()).unwrap(),
            serde_json::to_string(&ShellControlResponse::err("not_found")).unwrap(),
        ] {
            assert!(!s.contains("intents"), "unexpected: {s}");
        }
    }

    // ── `mode_request_matches` — pure, no real backend needed ──────────────

    #[test]
    fn mode_request_matches_an_exact_known_mode() {
        let modes = [Mode { size: (1920, 1080).into(), refresh: 60000 }];
        assert!(mode_request_matches(1920, 1080, 60000, &modes));
    }

    #[test]
    fn mode_request_rejects_a_size_only_match_with_a_different_refresh() {
        // All three fields must match — a mode is the whole triple, not
        // just its resolution.
        let modes = [Mode { size: (1920, 1080).into(), refresh: 60000 }];
        assert!(!mode_request_matches(1920, 1080, 144000, &modes));
    }

    #[test]
    fn mode_request_rejects_anything_not_in_the_list() {
        let modes = [Mode { size: (1920, 1080).into(), refresh: 60000 }];
        assert!(!mode_request_matches(1280, 720, 60000, &modes));
    }

    #[test]
    fn mode_request_against_an_empty_list_never_matches() {
        assert!(!mode_request_matches(1920, 1080, 60000, &[]));
    }

    #[test]
    fn mode_request_out_of_i32_range_never_panics_and_never_matches() {
        let modes = [Mode { size: (1920, 1080).into(), refresh: 60000 }];
        assert!(!mode_request_matches(9_999_999_999_999, 1080, 60000, &modes));
        assert!(!mode_request_matches(1920, i64::MIN, 60000, &modes));
        assert!(!mode_request_matches(1920, 1080, i64::MAX, &modes));
    }

    // ── A7c: agent_allowed — the closed allowlist an Agent-authority peer
    // (root during Yocto bring-up, or a future explicitly-configured
    // `agent_uid` once gateway de-roots — see `listener::PeerAuthority`)
    // may reach. Enumerated exhaustively both ways so a future op added to
    // this enum forces a conscious decision here instead of silently
    // inheriting `false` (or `true`) by omission. ──────────────────────────

    #[test]
    fn agent_allowed_covers_exactly_the_appearance_ops() {
        let allowed = [
            ShellControlRequest::GetCursorSource,
            ShellControlRequest::SetCursorSource { source: "brand".into() },
            ShellControlRequest::SetCursorSize { size: 48 },
            ShellControlRequest::GetOutputs,
            ShellControlRequest::SetOutputScale { output: "Virtual-1".into(), scale_pct: 150 },
            ShellControlRequest::SetTheme { theme: "dark".into() },
        ];
        for req in &allowed {
            assert!(req.agent_allowed(), "{:?} must be agent-allowed", req.op_name());
        }
    }

    #[test]
    fn agent_allowed_excludes_every_human_only_op() {
        let denied = [
            ShellControlRequest::ListWindows,
            ShellControlRequest::FocusWindow { query: "foot-A".into() },
            ShellControlRequest::SetOutputMode {
                output: "Virtual-1".into(),
                width: 1920,
                height: 1080,
                refresh_mhz: 60000,
            },
            ShellControlRequest::TakeShellIntents,
            ShellControlRequest::CodriveStatus,
            ShellControlRequest::CodriveDrive { action: "take_wheel".into() },
            ShellControlRequest::SetSessionLocked { locked: true },
        ];
        for req in &denied {
            assert!(!req.agent_allowed(), "{:?} must NOT be agent-allowed", req.op_name());
        }
    }
}
