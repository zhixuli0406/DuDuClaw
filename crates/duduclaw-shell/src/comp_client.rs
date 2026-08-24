//! Blocking Unix-socket client for `duduclaw-comp`'s shell-control socket
//! (WP-comp-shell-ipc, 2026-08-22) — the dock's window list/switch IPC.
//!
//! Wire contract mirrored here BY HAND (comp side: `duduclaw-comp/src/
//! shell_control/protocol.rs`) — this crate cannot depend on
//! `duduclaw-comp` (Linux-only, detached workspace, own `Cargo.lock` — see
//! that crate's own `Cargo.toml` header comment), same reasoning
//! `duduclaw-gateway/src/codrive/client.rs`'s own header comment gives for
//! hand-mirroring `codrive`'s wire types instead of sharing a crate. One
//! request per connection (connect → one JSON line out → one JSON line in
//! → close) — see comp's own `shell_control/protocol.rs` module doc for why
//! there is no persistent session here, unlike `codrive`'s own client.
//!
//! Every function here is a PLAIN BLOCKING call — same "callers run it from
//! a `std::thread::spawn` and bridge the result back to gpui via
//! `std::sync::mpsc` + a `cx.spawn` poll loop" contract `gateway_client`'s
//! own module doc establishes (`home/running_windows.rs` is this module's
//! one caller, following that exact pattern).
//!
//! Not `#[cfg(target_os = "linux")]`-gated: `std::os::unix::net::UnixStream`
//! also exists on macOS (this crate's own dev-iteration platform — see
//! `BUILD-LINUX.md`'s header comment on why Docker is needed for the
//! LINUX-specific `gpui_linux` backend, a SEPARATE concern from this plain
//! socket client), so this compiles unconditionally on both. On a dev Mac
//! there is never a real `duduclaw-comp` process — `socket_path()`/`call()`
//! degrade to the ordinary `NotAvailable`/`Io` error paths below, exactly
//! the same "comp isn't running" case a Linux box without the compositor up
//! would also hit; no platform-specific branch is needed.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

/// Same value and file name as comp's own `shell_control::protocol::
/// SOCKET_FILE_NAME` — kept as a literal here (not imported — see this
/// file's own module doc on why nothing is shared across the two crates).
const SOCKET_FILE_NAME: &str = "duduclaw-shell.sock";

/// Bounds both the connect-and-request-write phase and the response read —
/// this is a local, same-host round trip (comp's own `MAIN_THREAD_REPLY_
/// TIMEOUT`/`REQUEST_READ_TIMEOUT` are both 3s), so a dock caller waiting
/// this long already means something is badly wrong on the comp side, not
/// ordinary load.
const CALL_TIMEOUT: Duration = Duration::from_secs(3);

/// `$XDG_RUNTIME_DIR/duduclaw-shell.sock`. `None` when `XDG_RUNTIME_DIR`
/// isn't set at all (e.g. a plain `cargo test` invocation, or a dev Mac
/// shell run outside any session manager) — a real kiosk session always
/// has it set (`duduclaw-kiosk.service`'s own `Environment=XDG_RUNTIME_
/// DIR=...`, `appliance/mkosi.extra/etc/systemd/system/duduclaw-kiosk.
/// service`).
pub fn socket_path() -> Option<PathBuf> {
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")?;
    Some(PathBuf::from(runtime_dir).join(SOCKET_FILE_NAME))
}

/// One `list_windows` row — mirrors comp's own `shell_control::protocol::
/// ShellWindowInfo` field-for-field.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CompWindow {
    pub app_id: Option<String>,
    pub title: Option<String>,
    pub focused: bool,
}

/// Every failure this client can produce. Deliberately does NOT try to
/// distinguish "comp isn't running" from "comp is running but refused the
/// call" beyond `NotAvailable` vs. everything else — `home/running_
/// windows.rs`'s `RunningWindowsFeed` collapses all of these to the same
/// "offline" presentation either way (same pattern `gateway_client::
/// GatewayError`'s own doc comment documents and justifies for the
/// Notifications feed).
#[derive(Debug, Clone)]
pub enum CompClientError {
    /// No socket to even try connecting to (`XDG_RUNTIME_DIR` unset, or the
    /// socket file doesn't exist yet — comp not running, or running an old
    /// build without this channel).
    NotAvailable(String),
    /// A real I/O failure during connect/write/read.
    Io(String),
    /// Connected and sent a request, but no response line arrived within
    /// `CALL_TIMEOUT`.
    Timeout,
    /// The response line wasn't valid JSON in the expected shape.
    Protocol(String),
    /// comp answered with a structured `{"ok":false,"error":"..."}` — the
    /// call reached comp and comp explicitly declined it (e.g.
    /// `focus_window` found no match: `"not_found"`).
    Comp(String),
}

impl std::fmt::Display for CompClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompClientError::NotAvailable(s) => write!(f, "shell-control socket not available: {s}"),
            CompClientError::Io(s) => write!(f, "shell-control I/O error: {s}"),
            CompClientError::Timeout => write!(f, "shell-control call timed out"),
            CompClientError::Protocol(s) => write!(f, "shell-control protocol error: {s}"),
            CompClientError::Comp(s) => write!(f, "shell-control rejected: {s}"),
        }
    }
}

/// The compositor's current cursor configuration — ICON-3 (2026-08-23), the
/// pointer-settings surface. Mirrors comp's `cursor` object field-for-field,
/// but every field except `source` is `Option` on purpose: this shell has to
/// keep working against a comp build that predates any given field.
///
/// `size` in particular is NEW this round. A comp that has not been upgraded
/// answers `get_cursor_source` with no `size` key at all, which arrives here
/// as `None` — the honest signal the size控制 must degrade rather than
/// guess a number and then show it as the current one.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CursorState {
    /// What is ACTUALLY being drawn — `"system"` or `"brand"`. Kept as the
    /// raw string rather than an enum: comp owns this vocabulary, and a
    /// value this build has never heard of must render as "not brand", not
    /// fail to parse the whole response.
    pub source: String,
    /// What the operator ASKED for. Comp draws `source`; when the two
    /// disagree it means the requested theme is not installed and system
    /// cursors are being drawn instead — an honest signal, not an error
    /// (comp's own `shell_control` module doc spells this out). `None` on a
    /// comp build that predates the field, which is why every reader goes
    /// through `requested_source()` below.
    #[serde(default)]
    pub requested: Option<String>,
    #[serde(default)]
    pub theme: Option<String>,
    /// The size that is STORED. `effective_size` is what is drawn.
    #[serde(default)]
    pub size: Option<u32>,
    /// The size actually drawn. An XCursor theme holds a fixed set of image
    /// sizes and the nearest is used at its own size — nothing is upscaled —
    /// so a 96 request against a theme whose largest image is 64 really
    /// draws 64, and comp says so rather than claiming 96. On the
    /// appliance's own Adwaita the two are equal at all five offered steps.
    /// `None` on a comp build that predates the field.
    #[serde(default)]
    pub effective_size: Option<u32>,
    /// An operator pinned `DUDUCLAW_COMP_CURSOR_SOURCE` in comp's spawn
    /// environment, so a stored preference will not apply at the next start.
    #[serde(default)]
    pub env_pinned: bool,
    /// The size-side twin: `XCURSOR_SIZE` is pinned in comp's environment.
    #[serde(default)]
    pub size_env_pinned: bool,
}

impl CursorState {
    /// Whether the brand (paw) theme is what is being DRAWN. Anything else —
    /// including `"system"` and including a value from a future comp this
    /// build doesn't know — is not brand.
    pub fn is_brand(&self) -> bool {
        self.source == "brand"
    }

    /// The value a style RADIO should follow: what the operator chose, which
    /// is not always what is drawn. Falls back to `source` on a comp that
    /// does not report `requested` — there, the two are the same thing as
    /// far as this shell can tell, and pretending otherwise would invent a
    /// distinction.
    pub fn requested_is_brand(&self) -> bool {
        match self.requested.as_deref() {
            Some(requested) => requested == "brand",
            None => self.is_brand(),
        }
    }

    /// True when comp is drawing something OTHER than what was asked for —
    /// i.e. the requested cursor theme is not installed. Only ever true when
    /// comp actually reported `requested`; a build that does not is silent
    /// rather than assumed fine… which is the same thing here, because a
    /// build that cannot report the difference also cannot have one to
    /// report.
    pub fn theme_missing(&self) -> bool {
        self.requested.as_deref().is_some_and(|requested| requested != self.source)
    }

    /// The size a segment control should show as selected: what is DRAWN,
    /// not what was stored. Falls back to `size` on a comp that predates
    /// `effective_size`.
    pub fn drawn_size(&self) -> Option<u32> {
        self.effective_size.or(self.size)
    }

    /// Whether either half of the cursor configuration is pinned by comp's
    /// spawn environment, in which case a choice made here will not survive
    /// the next start and the UI has to say so.
    pub fn any_env_pinned(&self) -> bool {
        self.env_pinned || self.size_env_pinned
    }
}

/// Permissive ack struct — every field but `ok` is `Option`, same
/// "shape varies by op" convention `duduclaw-gateway/src/codrive/
/// client.rs::CodriveAck`'s own doc comment establishes for the sibling
/// codrive client.
#[derive(Debug, Clone, Default, Deserialize)]
struct CompResponse {
    ok: bool,
    #[serde(default)]
    windows: Option<Vec<CompWindow>>,
    #[serde(default)]
    matched_app_id: Option<String>,
    #[serde(default)]
    matched_title_prefix: Option<String>,
    #[serde(default)]
    cursor: Option<CursorState>,
    /// A1 (2026-08-23) — `take_shell_intents`' drained queue. `Option` for
    /// the same reason every other op-specific field here is: a comp build
    /// that predates the op does not send the key at all, and that must
    /// parse as "nothing pending", not as a protocol error.
    #[serde(default)]
    intents: Option<Vec<String>>,
    /// D4b (2026-08-23) — `get_outputs`' screen list, for the settings app's
    /// 顯示 page. `Option` for the same reason as every sibling above.
    #[serde(default)]
    outputs: Option<Vec<CompOutput>>,
    #[serde(default)]
    error: Option<String>,
}

/// One blocking connect → write one line → read one line → close round
/// trip. `req_line` is a complete, already-serialized JSON request WITHOUT
/// a trailing newline (this fn adds it) — callers build it with
/// `serde_json::json!` rather than a hand-formatted string, so a query
/// value containing `"` or `\` round-trips correctly (unlike comp's own
/// `codrive/mod.rs` debug tooling, which gets away with raw string
/// literals only because none of ITS values are caller-supplied free text).
fn call(req_line: &str) -> Result<CompResponse, CompClientError> {
    let line = call_raw(req_line)?;
    serde_json::from_str::<CompResponse>(line.trim()).map_err(|e| CompClientError::Protocol(e.to_string()))
}

/// The SOCKET half of [`call`], split out (A2 共駕, 2026-08-23) so a sibling
/// client module can parse the very same round trip into its OWN response
/// type — see `crate::codrive_client`, which does exactly that.
///
/// The alternative was hanging yet another op-specific `Option` field off
/// [`CompResponse`] (that struct already carries five). This way each family
/// of ops owns its own reply type and there is still exactly ONE piece of
/// socket code in this crate, which is the property that actually matters:
/// timeouts, the missing-socket classification and the "connection closed
/// with no response" case are all decided here, once, for every caller.
///
/// Returns the raw response LINE (newline trimmed by the caller's parser).
/// Blocking; see this file's own module doc for the threading contract.
pub(crate) fn call_raw(req_line: &str) -> Result<String, CompClientError> {
    let Some(path) = socket_path() else {
        return Err(CompClientError::NotAvailable("XDG_RUNTIME_DIR is not set".to_string()));
    };
    if !path.exists() {
        return Err(CompClientError::NotAvailable(format!("no shell-control socket at {}", path.display())));
    }

    let mut stream = UnixStream::connect(&path).map_err(|e| CompClientError::Io(e.to_string()))?;
    let _ = stream.set_read_timeout(Some(CALL_TIMEOUT));
    let _ = stream.set_write_timeout(Some(CALL_TIMEOUT));

    if let Err(e) = writeln!(stream, "{req_line}") {
        return Err(classify_io_error(e));
    }

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) => return Err(CompClientError::Protocol("connection closed with no response".to_string())),
        Ok(_) => {}
        Err(e) => return Err(classify_io_error(e)),
    }

    Ok(line)
}

fn classify_io_error(e: std::io::Error) -> CompClientError {
    match e.kind() {
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => CompClientError::Timeout,
        _ => CompClientError::Io(e.to_string()),
    }
}

/// `{"op":"list_windows"}` — every currently mapped toplevel comp knows
/// about. Blocking; see this file's own module doc for the threading
/// contract.
pub fn list_windows() -> Result<Vec<CompWindow>, CompClientError> {
    let resp = call(r#"{"op":"list_windows"}"#)?;
    if !resp.ok {
        return Err(CompClientError::Comp(resp.error.unwrap_or_else(|| "unknown error".to_string())));
    }
    Ok(resp.windows.unwrap_or_default())
}

/// Outcome of a successful `focus_window` call — which criterion comp
/// matched on, mirroring `codrive::window_target::WindowMatch`'s own two
/// variants (never both, never neither — see comp's own `protocol.rs`
/// doc). Not currently rendered anywhere (the dock only needs pass/fail),
/// but kept typed rather than discarded so a future caller (a debug
/// overlay, say) doesn't have to re-parse the raw response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FocusMatch {
    AppId(String),
    TitlePrefix(String),
}

/// `{"op":"focus_window","params":{"query":"..."}}` — raises/focuses the
/// matching toplevel on the compositor's HUMAN seat (comp's own `shell_
/// control` module doc explains why, and why this is a DIFFERENT socket
/// from the agent's `codrive` channel). A query matching nothing comes back
/// as `Err(CompClientError::Comp("not_found"))`, never a silent success.
pub fn focus_window(query: &str) -> Result<FocusMatch, CompClientError> {
    let req = serde_json::json!({ "op": "focus_window", "params": { "query": query } }).to_string();
    let resp = call(&req)?;
    if !resp.ok {
        return Err(CompClientError::Comp(resp.error.unwrap_or_else(|| "unknown error".to_string())));
    }
    match (resp.matched_app_id, resp.matched_title_prefix) {
        (Some(id), _) => Ok(FocusMatch::AppId(id)),
        (None, Some(title)) => Ok(FocusMatch::TitlePrefix(title)),
        (None, None) => Err(CompClientError::Protocol("ok response carried neither matched_app_id nor matched_title_prefix".to_string())),
    }
}

// ── Cursor configuration (ICON-3, 2026-08-23) ───────────────────────────
// Three ops behind the pointer-settings surface. The compositor side is
// implemented by a SEPARATE work package this round; this client is written
// against the agreed wire contract and, crucially, degrades cleanly against
// a comp that has only half of it:
//
//   {"op":"get_cursor_source"}                        -> {"ok":true,"cursor":{…}}
//   {"op":"set_cursor_source","params":{"source":…}}  -> {"ok":true[,"cursor":{…}]}
//   {"op":"set_cursor_size","params":{"size":N}}      -> {"ok":true,"cursor":{…}}
//
// `set_cursor_source` predates this round on the comp side and its response
// shape was never specified to include the cursor object, so both setters
// return `Option<CursorState>` — `None` means "accepted, but told us
// nothing", and the caller re-reads. Inventing the post-set state locally
// would be this client asserting something it did not observe.

/// `{"op":"get_cursor_source"}`. Blocking; see this file's module doc for
/// the threading contract.
///
/// A response with `ok:true` but no `cursor` object is a `Protocol` error,
/// not a silent default: the whole point of this call is to learn the
/// current state, and having no answer is different from having a default
/// one.
pub fn get_cursor_source() -> Result<CursorState, CompClientError> {
    let resp = call(r#"{"op":"get_cursor_source"}"#)?;
    if !resp.ok {
        return Err(CompClientError::Comp(resp.error.unwrap_or_else(|| "unknown error".to_string())));
    }
    resp.cursor.ok_or_else(|| CompClientError::Protocol("ok response carried no cursor object".to_string()))
}

/// `{"op":"set_cursor_source","params":{"source":"system"|"brand"}}`.
///
/// `source` is `&str` rather than an enum for the same reason
/// `CursorState::source` is: comp owns the vocabulary. Call sites pass one
/// of `CURSOR_SOURCE_SYSTEM`/`CURSOR_SOURCE_BRAND` below so the two spellings
/// live in exactly one place.
pub fn set_cursor_source(source: &str) -> Result<Option<CursorState>, CompClientError> {
    let req = serde_json::json!({ "op": "set_cursor_source", "params": { "source": source } }).to_string();
    let resp = call(&req)?;
    if !resp.ok {
        return Err(CompClientError::Comp(resp.error.unwrap_or_else(|| "unknown error".to_string())));
    }
    Ok(resp.cursor)
}

/// `{"op":"set_cursor_size","params":{"size":N}}`.
///
/// A comp that predates this op answers `{"ok":false,"error":"..."}`, which
/// arrives as `CompClientError::Comp` — the caller's cue to disable the size
/// control and say so, rather than to show an error dialog for something the
/// operator cannot fix.
pub fn set_cursor_size(size: u32) -> Result<Option<CursorState>, CompClientError> {
    let req = serde_json::json!({ "op": "set_cursor_size", "params": { "size": size } }).to_string();
    let resp = call(&req)?;
    if !resp.ok {
        return Err(CompClientError::Comp(resp.error.unwrap_or_else(|| "unknown error".to_string())));
    }
    Ok(resp.cursor)
}

/// The two `source` values comp accepts. Consts, not an enum, so this
/// client never has to decide what to do with a third value it might one day
/// be told about (see `CursorState::source`'s own doc comment).
pub const CURSOR_SOURCE_SYSTEM: &str = "system";
pub const CURSOR_SOURCE_BRAND: &str = "brand";

/// Comp's own refusal code for a size outside its five-step set
/// (`duduclaw-comp/src/shell_control/listener.rs`). It means "that VALUE is
/// not offered", NOT "this build cannot change the size" — which is why the
/// pointer surface must not treat it as evidence the op is missing. This
/// shell only ever sends the five, so seeing it at all would mean the two
/// sides' step lists have drifted apart.
/// (Comp's style axis has the same distinction, `invalid_cursor_source`, but
/// no consumer here: the style control has no "disable it" state to protect,
/// so every style refusal renders the same way regardless of code.)
pub const CURSOR_ERR_INVALID_SIZE: &str = "invalid_cursor_size";

// ── Outputs / screens (D4b, 2026-08-23) ─────────────────────────────────
// Three ops behind the settings app's 顯示 page. The compositor is the ONLY
// process that manages outputs, so this is the only way a settings surface
// can know what screens exist or change one.
//
//   {"op":"get_outputs"}                                        -> {"ok":true,"outputs":[…]}
//   {"op":"set_output_mode","params":{"output":…,"width":…,…}}   -> {"ok":true[,"outputs":[…]]}
//   {"op":"set_output_scale","params":{"output":…,"scale_pct":…}}-> {"ok":true[,"outputs":[…]]}
//
// Same degradation discipline the cursor ops above document: every field on
// `CompOutput` except `name` is optional, so a comp build that predates any
// one of them parses fine and the page renders that fact rather than a
// guess. A comp that predates the whole op answers `{"ok":false,…}`, which
// arrives as `CompClientError::Comp` — the page's cue to say the display
// settings are unavailable, not to show an error dialog.

/// One mode a screen reports it can drive.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CompOutputMode {
    pub width: u32,
    pub height: u32,
    /// Millihertz, comp's own unit (60000 = 60 Hz), passed through
    /// unconverted so no rounding happens on this side of the socket.
    #[serde(default)]
    pub refresh_mhz: Option<u32>,
    #[serde(default)]
    pub preferred: bool,
    #[serde(default)]
    pub current: bool,
}

impl CompOutputMode {
    /// `1920 × 1080 · 60 Hz`, with the refresh clause dropped when comp did
    /// not report one — never a fabricated "60 Hz".
    pub fn label(&self) -> String {
        match self.refresh_mhz {
            Some(mhz) => format!("{} × {} · {}", self.width, self.height, format_refresh(mhz)),
            None => format!("{} × {}", self.width, self.height),
        }
    }
}

/// One screen.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CompOutput {
    /// The connector name (`eDP-1`, `Virtual-1`). The stable identity every
    /// setter addresses a screen by.
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub make: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub refresh_mhz: Option<u32>,
    /// Current scale × 100. Integer on the wire on purpose — same reasoning
    /// `set_cursor_size` uses `i64`: a settings page must never be handed a
    /// value none of its segments can represent.
    #[serde(default)]
    pub scale_pct: Option<u32>,
    #[serde(default)]
    pub physical_width_mm: Option<u32>,
    #[serde(default)]
    pub physical_height_mm: Option<u32>,
    /// Whatever comp's own mode list holds. An EMPTY list is a real and
    /// expected answer (a virtio screen under QEMU reports none), and means
    /// "this screen offers no choices" — it is never padded with a synthetic
    /// entry built from the current resolution.
    #[serde(default)]
    pub modes: Vec<CompOutputMode>,
    /// Whether `set_output_mode` will actually do something on this build.
    /// Absent ⇒ `false`: a comp that does not say so must not be assumed
    /// capable, or the page would offer a control that always refuses.
    #[serde(default)]
    pub mode_switch_supported: bool,
}

impl CompOutput {
    /// The human-facing screen name: comp's description if it has one, else
    /// make+model, else the connector name. Never empty.
    pub fn display_name(&self) -> String {
        if let Some(desc) = self.description.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
            return desc.to_string();
        }
        let make = self.make.as_deref().map(str::trim).unwrap_or_default();
        let model = self.model.as_deref().map(str::trim).unwrap_or_default();
        match (make.is_empty(), model.is_empty()) {
            (false, false) => format!("{make} {model}"),
            (false, true) => make.to_string(),
            (true, false) => model.to_string(),
            (true, true) => self.name.clone(),
        }
    }

    /// The current resolution line, or `None` when comp reported no size at
    /// all (an older build) — the page then says so rather than showing 0×0.
    pub fn current_mode_label(&self) -> Option<String> {
        let (w, h) = (self.width?, self.height?);
        Some(match self.refresh_mhz {
            Some(mhz) => format!("{w} × {h} · {}", format_refresh(mhz)),
            None => format!("{w} × {h}"),
        })
    }
}

/// Millihertz -> a human refresh string. `60000` -> `60 Hz`, `59940` ->
/// `59.94 Hz` (trailing zeros trimmed, so real CVT/EDID rates read correctly
/// instead of all collapsing to the same integer).
pub fn format_refresh(mhz: u32) -> String {
    if mhz.is_multiple_of(1000) {
        return format!("{} Hz", mhz / 1000);
    }
    let text = format!("{:.2}", mhz as f64 / 1000.0);
    let trimmed = text.trim_end_matches('0').trim_end_matches('.');
    format!("{trimmed} Hz")
}

/// The scale steps the settings page offers, as percentages. A closed set,
/// exactly like the cursor's five sizes — comp refuses anything else with
/// `OUTPUT_ERR_INVALID_SCALE` rather than clamping.
pub const OUTPUT_SCALE_STEPS: [u32; 5] = [100, 125, 150, 175, 200];

/// comp's refusal codes for the three output ops. Named so the one place
/// these literals live is greppable against comp's own listener.
pub const OUTPUT_ERR_UNKNOWN_OUTPUT: &str = "unknown_output";
pub const OUTPUT_ERR_MODE_UNSUPPORTED: &str = "mode_switch_unsupported";
pub const OUTPUT_ERR_SCALE_UNSUPPORTED: &str = "scale_change_unsupported";

/// `{"op":"get_outputs"}`. Blocking; see this file's module doc.
///
/// An `ok:true` response with no `outputs` key is a `Protocol` error, not an
/// empty screen list — same reasoning `get_cursor_source` gives: "no answer"
/// and "an answer that is empty" are different facts, and a machine with
/// zero screens is not a state this shell can be running in.
pub fn get_outputs() -> Result<Vec<CompOutput>, CompClientError> {
    let resp = call(r#"{"op":"get_outputs"}"#)?;
    if !resp.ok {
        return Err(CompClientError::Comp(resp.error.unwrap_or_else(|| "unknown error".to_string())));
    }
    resp.outputs.ok_or_else(|| CompClientError::Protocol("ok response carried no outputs list".to_string()))
}

/// `{"op":"set_output_mode","params":{…}}`. Returns the refreshed screen
/// list when comp echoed one; `None` means "accepted, but told us nothing"
/// and the caller re-reads — the same contract the cursor setters have, and
/// for the same reason (this client must never assert a state it did not
/// observe).
pub fn set_output_mode(output: &str, width: u32, height: u32, refresh_mhz: u32) -> Result<Option<Vec<CompOutput>>, CompClientError> {
    let req = serde_json::json!({
        "op": "set_output_mode",
        "params": { "output": output, "width": width, "height": height, "refresh_mhz": refresh_mhz }
    })
    .to_string();
    let resp = call(&req)?;
    if !resp.ok {
        return Err(CompClientError::Comp(resp.error.unwrap_or_else(|| "unknown error".to_string())));
    }
    Ok(resp.outputs)
}

/// `{"op":"set_output_scale","params":{"output":…,"scale_pct":…}}`.
pub fn set_output_scale(output: &str, scale_pct: u32) -> Result<Option<Vec<CompOutput>>, CompClientError> {
    let req = serde_json::json!({ "op": "set_output_scale", "params": { "output": output, "scale_pct": scale_pct } }).to_string();
    let resp = call(&req)?;
    if !resp.ok {
        return Err(CompClientError::Comp(resp.error.unwrap_or_else(|| "unknown error".to_string())));
    }
    Ok(resp.outputs)
}

// ── Theme (D2) and global intents (A1) — both 2026-08-23 ────────────────
// Two ops added by the same round that migrated this shell's chrome onto
// real layer surfaces. Comp side lives in `duduclaw-comp/src/shell_control/`.
//
//   {"op":"set_theme","params":{"theme":"dark"|"light"}} -> {"ok":true}
//   {"op":"take_shell_intents"}                          -> {"ok":true,"intents":[…]}
//
// Both degrade the same way every op above does: a comp build that predates
// them answers `{"ok":false,"error":…}`, which arrives as
// `CompClientError::Comp` and is the caller's cue to stop asking — NOT to
// show the operator an error for something they cannot fix.

/// The two `theme` values comp accepts. Consts rather than an enum, for the
/// same reason [`CURSOR_SOURCE_SYSTEM`]/[`CURSOR_SOURCE_BRAND`] are: comp
/// owns this vocabulary, and the two spellings should exist in exactly one
/// place on this side of the wire.
pub const THEME_DARK: &str = "dark";
/// See [`THEME_DARK`].
pub const THEME_LIGHT: &str = "light";

/// `{"op":"set_theme","params":{"theme":"dark"|"light"}}` — tells comp which
/// palette to draw its SERVER-SIDE decorations in (title bars, borders,
/// shadows, the Alt-Tab switcher), so an application window's frame matches
/// the shell around it.
///
/// One-way on purpose: the shell is the authority on which theme the
/// operator chose (it is the half that persists the choice — `oobe`'s theme
/// step), so there is no `get_theme`. Comp is told, it does not vote.
///
/// Returns `Ok(())` rather than any state object: comp's answer to this op
/// is a bare ack, and inventing a returned "current theme" from the value we
/// just sent would be this client asserting something it never observed
/// (same reasoning the cursor setters' doc comments give for returning
/// `Option<CursorState>` instead of a fabricated one).
///
/// Blocking; see this file's module doc for the threading contract.
pub fn set_theme(theme: &str) -> Result<(), CompClientError> {
    let req = serde_json::json!({ "op": "set_theme", "params": { "theme": theme } }).to_string();
    let resp = call(&req)?;
    if !resp.ok {
        return Err(CompClientError::Comp(resp.error.unwrap_or_else(|| "unknown error".to_string())));
    }
    Ok(())
}

/// D9-bug3/D9-bug4 (2026-08-24): tell comp whether this shell's lock screen
/// is up.
///
/// comp cannot work that out for itself — a lock screen is just pixels on a
/// layer surface — and three compositor-owned behaviours depend on knowing:
/// ordinary windows are not painted at all (which is what stops a locked
/// screen showing the browser that was open behind it), only layer surfaces
/// can take pointer input, and keys are delivered straight to the shell
/// instead of through the input method's keyboard grab (which is what makes
/// "press any key to wake" work while fcitx5 is in 注音 mode). See comp's own
/// `session_lock.rs` module doc for the whole rule.
///
/// Returns `Ok(())` rather than an echoed state, for exactly the reason
/// [`set_theme`] gives: comp answers with a bare ack, and reporting a
/// "current lock state" we never observed would be this client asserting
/// something it did not see.
///
/// Blocking; see this file's module doc for the threading contract. The one
/// caller (`crate::notify_comp_session_locked`) runs it detached, so a comp
/// that is slow or absent can never stall the lock/unlock itself.
pub fn set_session_locked(locked: bool) -> Result<(), CompClientError> {
    let req = serde_json::json!({ "op": "set_session_locked", "params": { "locked": locked } }).to_string();
    let resp = call(&req)?;
    if !resp.ok {
        return Err(CompClientError::Comp(resp.error.unwrap_or_else(|| "unknown error".to_string())));
    }
    Ok(())
}

/// One thing comp is asking this shell to do, because comp saw a global
/// hotkey the shell could not have seen itself.
///
/// The vocabulary is comp's (same convention as `CursorState::source`), but
/// unlike a cursor style — which is only ever *rendered* — an intent
/// *triggers an action*. So this is a CLOSED enum and an unrecognized wire
/// value is DROPPED rather than surfaced: a shell that acts on a token it
/// does not understand is strictly worse than one that ignores it. See
/// [`take_shell_intents`] for where the drop is counted and reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellIntent {
    /// Super+K was pressed. Comp intercepts it (an application window
    /// normally holds keyboard focus, so this shell's own `cmd-k` binding
    /// never sees it — the compositor owning global hotkeys is the standard
    /// wlroots-ecosystem split) and queues this for the shell to act on by
    /// raising the global task bar.
    GlobalTaskBar,
}

impl ShellIntent {
    /// Every intent this build understands. Exists so the unrecognized-token
    /// log below can say what the vocabulary IS, not just that something was
    /// dropped — the whole point of that log is diagnosing a comp/shell
    /// version mismatch, and "dropped `foo`" without the known set makes the
    /// reader go read the source.
    pub const ALL: [ShellIntent; 1] = [ShellIntent::GlobalTaskBar];

    /// Comp's wire spelling for this intent.
    pub const fn wire_name(self) -> &'static str {
        match self {
            ShellIntent::GlobalTaskBar => "global_task_bar",
        }
    }

    /// Parses one wire token. `None` for anything this build does not know —
    /// see this type's own doc comment for why that is a drop and not an
    /// error.
    pub fn from_wire(raw: &str) -> Option<Self> {
        match raw {
            "global_task_bar" => Some(ShellIntent::GlobalTaskBar),
            _ => None,
        }
    }
}

/// `{"op":"take_shell_intents"}` — **drains** comp's pending global-hotkey
/// queue (taking them clears them, so two pollers would steal from each
/// other; there is exactly one caller by construction).
///
/// Polled rather than pushed: comp's shell-control socket is a one-shot
/// connect/request/response/close channel with a single sequential listener
/// thread (see comp's own `shell_control/listener.rs` module doc), so a
/// long-poll would wedge every other caller behind it. The cost is polling
/// latency on the ⌘K path, which is a known, recorded tradeoff rather than
/// an oversight.
///
/// Unrecognized intent tokens are dropped and reported on stderr once per
/// batch — never acted on, never turned into an `Err` that would also throw
/// away the intents alongside them that this build DOES understand.
///
/// Blocking; see this file's module doc for the threading contract.
pub fn take_shell_intents() -> Result<Vec<ShellIntent>, CompClientError> {
    let resp = call(r#"{"op":"take_shell_intents"}"#)?;
    if !resp.ok {
        return Err(CompClientError::Comp(resp.error.unwrap_or_else(|| "unknown error".to_string())));
    }
    let raw = resp.intents.unwrap_or_default();
    let mut known: Vec<ShellIntent> = Vec::with_capacity(raw.len());
    let mut unknown: Vec<&str> = Vec::new();
    for token in &raw {
        match ShellIntent::from_wire(token.as_str()) {
            Some(intent) => known.push(intent),
            None => unknown.push(token.as_str()),
        }
    }
    if !unknown.is_empty() {
        let known: Vec<&str> = ShellIntent::ALL.iter().map(|i| i.wire_name()).collect();
        eprintln!(
            "[comp_client] take_shell_intents: dropped {} unrecognized intent(s) {unknown:?}; this build understands {known:?}",
            unknown.len()
        );
    }
    Ok(known)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three tests below all mutate the SAME process-wide environment
    /// variable, and `cargo test` runs them on concurrent threads — so one
    /// test's `remove_var` could land between another's `set_var` and its
    /// assertion. That is not theoretical: measured at **14 failures in 40
    /// runs** of `cargo test comp_client::` alone (2026-08-22, macOS), with
    /// no other test in the binary involved. It went unnoticed until APP-1
    /// added enough sibling tests to change the scheduling, which is exactly
    /// how a latent race announces itself.
    ///
    /// Serializing them on one mutex is the fix rather than
    /// `--test-threads=1` (which would slow the whole suite for three tests)
    /// or `#[serial_test::serial]` (a new dependency, which this crate's
    /// `Cargo.toml` header comment sets a deliberately high bar for). The
    /// guard is taken for the WHOLE save/mutate/assert/restore span, so a
    /// panicking assertion cannot leak a mutated environment either — a
    /// poisoned mutex is recovered rather than cascading into a second
    /// failure that hides the first.
    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        static ENV_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        ENV_LOCK.get_or_init(|| std::sync::Mutex::new(())).lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn socket_path_none_when_xdg_runtime_dir_unset() {
        let _guard = env_guard();
        // SAFETY: test-only env mutation, serialized by `env_guard` above so
        // no sibling test can observe the intermediate state.
        let saved = std::env::var_os("XDG_RUNTIME_DIR");
        unsafe {
            std::env::remove_var("XDG_RUNTIME_DIR");
        }
        assert_eq!(socket_path(), None);
        unsafe {
            if let Some(v) = saved {
                std::env::set_var("XDG_RUNTIME_DIR", v);
            }
        }
    }

    #[test]
    fn socket_path_joins_the_fixed_file_name_under_xdg_runtime_dir() {
        let _guard = env_guard();
        let saved = std::env::var_os("XDG_RUNTIME_DIR");
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", "/tmp/some-runtime-dir");
        }
        assert_eq!(socket_path(), Some(PathBuf::from("/tmp/some-runtime-dir/duduclaw-shell.sock")));
        unsafe {
            match saved {
                Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
                None => std::env::remove_var("XDG_RUNTIME_DIR"),
            }
        }
    }

    #[test]
    fn list_windows_against_a_missing_socket_is_not_available_not_a_panic() {
        let _guard = env_guard();
        let saved = std::env::var_os("XDG_RUNTIME_DIR");
        let dir = std::env::temp_dir().join(format!("duduclaw-shell-comp-client-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", &dir);
        }
        let err = list_windows().expect_err("no comp process is listening in this test dir");
        assert!(matches!(err, CompClientError::NotAvailable(_)), "unexpected error variant: {err:?}");
        unsafe {
            match saved {
                Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
                None => std::env::remove_var("XDG_RUNTIME_DIR"),
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compresponse_deserializes_windows_shape() {
        let json = r#"{"ok":true,"windows":[{"app_id":"foot-A","title":"foot","focused":true}]}"#;
        let resp: CompResponse = serde_json::from_str(json).unwrap();
        assert!(resp.ok);
        let windows = resp.windows.unwrap();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].app_id.as_deref(), Some("foot-A"));
        assert!(windows[0].focused);
    }

    #[test]
    fn compresponse_deserializes_err_shape() {
        let json = r#"{"ok":false,"error":"not_found"}"#;
        let resp: CompResponse = serde_json::from_str(json).unwrap();
        assert!(!resp.ok);
        assert_eq!(resp.error.as_deref(), Some("not_found"));
        assert!(resp.windows.is_none());
    }

    // ── ICON-3 (2026-08-23): cursor ops ──────────────────────────────────

    /// Comp's own documented reply shape, copied from
    /// `duduclaw-comp/src/shell_control/mod.rs`'s module doc rather than
    /// invented here.
    #[test]
    fn compresponse_deserializes_a_full_cursor_object() {
        let json = r#"{"ok":true,"cursor":{"source":"brand","requested":"brand","theme":"DuDuClaw","origin":"runtime",
                        "size":32,"effective_size":32,"size_env_pinned":false,"env_pinned":false,"persisted":true}}"#;
        let resp: CompResponse = serde_json::from_str(json).unwrap();
        let cursor = resp.cursor.expect("cursor object must parse");
        assert_eq!(cursor.source, "brand");
        assert!(cursor.is_brand());
        assert!(cursor.requested_is_brand());
        assert!(!cursor.theme_missing());
        assert_eq!(cursor.theme.as_deref(), Some("DuDuClaw"));
        assert_eq!(cursor.drawn_size(), Some(32));
        assert!(!cursor.any_env_pinned());
        // Fields this shell does not read (`origin`, `persisted`) must not
        // break the parse — that is what makes comp free to add more.
    }

    /// The degradation this whole `Option<u32>` exists for: a comp that
    /// predates `set_cursor_size` reports no `size` key at all. That must
    /// parse fine and come back as `None` — NOT as a default number the UI
    /// would then present as the machine's actual setting.
    #[test]
    fn a_cursor_object_without_size_parses_with_size_none() {
        let json = r#"{"ok":true,"cursor":{"source":"system","theme":"Adwaita"}}"#;
        let resp: CompResponse = serde_json::from_str(json).unwrap();
        let cursor = resp.cursor.expect("cursor object must parse");
        assert_eq!(cursor.size, None);
        assert_eq!(cursor.drawn_size(), None);
        assert!(!cursor.is_brand());
        // No `requested` either — the radio falls back to `source`, and
        // "the theme is missing" is not claimed on evidence that doesn't
        // exist.
        assert!(!cursor.requested_is_brand());
        assert!(!cursor.theme_missing());
    }

    /// The honest-signal case comp's own doc names: the brand theme was
    /// asked for but is not installed, so system cursors are drawn. The
    /// radio follows the CHOICE; the screen says what is actually drawn.
    #[test]
    fn a_requested_theme_that_is_not_installed_is_reported_not_hidden() {
        let json = r#"{"ok":true,"cursor":{"source":"system","requested":"brand","theme":"Adwaita","size":24,"effective_size":24}}"#;
        let cursor = serde_json::from_str::<CompResponse>(json).unwrap().cursor.expect("cursor");
        assert!(!cursor.is_brand(), "what is DRAWN is the system theme");
        assert!(cursor.requested_is_brand(), "what was CHOSEN is the paw theme");
        assert!(cursor.theme_missing());
    }

    /// The size-side honest signal: a step the installed theme has no image
    /// for is drawn at the nearest one it does have, and the selection has
    /// to follow what is drawn.
    #[test]
    fn a_size_the_theme_cannot_draw_reports_the_size_it_actually_drew() {
        let json = r#"{"ok":true,"cursor":{"source":"system","requested":"system","size":96,"effective_size":64}}"#;
        let cursor = serde_json::from_str::<CompResponse>(json).unwrap().cursor.expect("cursor");
        assert_eq!(cursor.size, Some(96));
        assert_eq!(cursor.drawn_size(), Some(64), "the selection must follow the drawn size, not the stored one");
    }

    #[test]
    fn an_env_pinned_cursor_is_reported_on_either_axis() {
        let source_pinned = serde_json::from_str::<CompResponse>(r#"{"ok":true,"cursor":{"source":"system","env_pinned":true}}"#)
            .unwrap()
            .cursor
            .expect("cursor");
        assert!(source_pinned.any_env_pinned());
        let size_pinned = serde_json::from_str::<CompResponse>(r#"{"ok":true,"cursor":{"source":"system","size_env_pinned":true}}"#)
            .unwrap()
            .cursor
            .expect("cursor");
        assert!(size_pinned.any_env_pinned());
        let neither =
            serde_json::from_str::<CompResponse>(r#"{"ok":true,"cursor":{"source":"system"}}"#).unwrap().cursor.expect("cursor");
        assert!(!neither.any_env_pinned());
    }

    /// A `source` value from a future comp must not fail the whole parse —
    /// it reads as "not brand", which is what the system card honestly
    /// describes.
    #[test]
    fn an_unknown_source_value_still_parses_and_is_not_brand() {
        let json = r#"{"ok":true,"cursor":{"source":"something-new"}}"#;
        let resp: CompResponse = serde_json::from_str(json).unwrap();
        let cursor = resp.cursor.expect("cursor object must parse");
        assert!(!cursor.is_brand());
        assert_eq!(cursor.theme, None);
        assert_eq!(cursor.size, None);
    }

    #[test]
    fn cursor_requests_are_well_formed_json() {
        let src = serde_json::json!({ "op": "set_cursor_source", "params": { "source": CURSOR_SOURCE_BRAND } });
        let back: serde_json::Value = serde_json::from_str(&src.to_string()).unwrap();
        assert_eq!(back["op"], "set_cursor_source");
        assert_eq!(back["params"]["source"], "brand");

        let size = serde_json::json!({ "op": "set_cursor_size", "params": { "size": 96u32 } });
        let back: serde_json::Value = serde_json::from_str(&size.to_string()).unwrap();
        assert_eq!(back["op"], "set_cursor_size");
        assert_eq!(back["params"]["size"], 96);
    }

    /// D9-bug3/D9-bug4: pinned to the LITERAL line comp's own protocol test
    /// (`duduclaw-comp/src/shell_control/protocol.rs`,
    /// `set_session_locked_wire_shape_is_what_the_shell_sends`) asserts
    /// against. The two crates cannot depend on each other — see this file's
    /// module doc — so these two string literals ARE the contract, and this is
    /// the same "pin wire formats to literal JSON" discipline the cursor ops
    /// above already follow.
    #[test]
    fn set_session_locked_request_is_the_exact_line_comp_parses() {
        for locked in [true, false] {
            let req = serde_json::json!({ "op": "set_session_locked", "params": { "locked": locked } });
            let expected = if locked {
                r#"{"op":"set_session_locked","params":{"locked":true}}"#
            } else {
                r#"{"op":"set_session_locked","params":{"locked":false}}"#
            };
            assert_eq!(req.to_string(), expected);
        }
    }

    /// The dev-Mac / compositor-down path for the lock announcement: no
    /// compositor must never be a panic, and (through
    /// `crate::notify_comp_session_locked`, which discards the result) must
    /// never be able to block a lock from happening.
    #[test]
    fn set_session_locked_against_a_missing_socket_is_not_available_not_a_panic() {
        let _guard = env_guard();
        let saved = std::env::var_os("XDG_RUNTIME_DIR");
        let dir = std::env::temp_dir().join(format!("duduclaw-shell-lock-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", &dir);
        }
        assert!(matches!(set_session_locked(true), Err(CompClientError::NotAvailable(_))));
        assert!(matches!(set_session_locked(false), Err(CompClientError::NotAvailable(_))));
        unsafe {
            match saved {
                Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
                None => std::env::remove_var("XDG_RUNTIME_DIR"),
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The dev-Mac / compositor-down path, end to end: no socket means
    /// `NotAvailable`, never a panic and never a fabricated `CursorState`.
    #[test]
    fn cursor_ops_against_a_missing_socket_are_not_available_not_a_panic() {
        let _guard = env_guard();
        let saved = std::env::var_os("XDG_RUNTIME_DIR");
        let dir = std::env::temp_dir().join(format!("duduclaw-shell-cursor-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", &dir);
        }
        let get = get_cursor_source().expect_err("no comp process is listening in this test dir");
        assert!(matches!(get, CompClientError::NotAvailable(_)), "unexpected error variant: {get:?}");
        let set_source = set_cursor_source(CURSOR_SOURCE_BRAND).expect_err("no comp process");
        assert!(matches!(set_source, CompClientError::NotAvailable(_)), "unexpected error variant: {set_source:?}");
        let set_size = set_cursor_size(48).expect_err("no comp process");
        assert!(matches!(set_size, CompClientError::NotAvailable(_)), "unexpected error variant: {set_size:?}");
        unsafe {
            match saved {
                Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
                None => std::env::remove_var("XDG_RUNTIME_DIR"),
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn focus_window_request_is_well_formed_json() {
        // Not a wire round-trip (no live comp) — just confirms the request
        // builder produces valid, correctly-shaped JSON (adjacent tagging,
        // matching comp's own `ShellControlRequest` shape) rather than a
        // hand-formatted string that could break on a query containing `"`.
        let req = serde_json::json!({ "op": "focus_window", "params": { "query": "a \"quoted\" id" } });
        let s = req.to_string();
        let back: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(back["op"], "focus_window");
        assert_eq!(back["params"]["query"], "a \"quoted\" id");
    }

    /// Live-fire, `#[ignore]`d — same "never run by a bare `cargo test`"
    /// contract `oobe::claim`/`gateway_client`'s own live tests establish.
    /// Run against a REAL `duduclaw-comp` (this file's own module doc's
    /// "one-shot" contract means any comp instance works, no special
    /// fixture needed) with `XDG_RUNTIME_DIR` pointed at that comp's own
    /// runtime dir:
    ///   `XDG_RUNTIME_DIR=/tmp/xdg-runtime cargo test -- --ignored \
    ///    live_list_windows_against_real_comp --nocapture`
    #[test]
    #[ignore = "requires a live duduclaw-comp with its shell-control socket up — see doc comment"]
    fn live_list_windows_against_real_comp() {
        match list_windows() {
            Ok(windows) => eprintln!("[live] {} window(s): {windows:?}", windows.len()),
            Err(e) => panic!("list_windows failed against a supposedly-live comp: {e}"),
        }
    }

    /// Same live-fire contract as `live_list_windows_against_real_comp`
    /// above. Expects at least one window whose app_id is `foot-A` to be
    /// mapped in the live comp instance (this round's own container
    /// verification launches exactly that — see `BUILD.md`'s
    /// "WP-comp-shell-ipc" section for the reproducible command):
    ///   `XDG_RUNTIME_DIR=/tmp/xdg-runtime cargo test -- --ignored \
    ///    live_focus_window_against_real_comp --nocapture`
    #[test]
    #[ignore = "requires a live duduclaw-comp with a foot-A window mapped — see doc comment"]
    fn live_focus_window_against_real_comp() {
        match focus_window("foot-A") {
            Ok(m) => eprintln!("[live] focus_window(\"foot-A\") matched: {m:?}"),
            Err(e) => panic!("focus_window(\"foot-A\") failed against a supposedly-live comp: {e}"),
        }
        // A query that should never exist proves the honest not_found path
        // is reachable through THIS client too, not just comp's own side.
        match focus_window("definitely-does-not-exist-xyz") {
            Err(CompClientError::Comp(e)) => {
                assert_eq!(e, "not_found");
                eprintln!("[live] focus_window on a bogus query correctly returned Comp(\"not_found\")");
            }
            other => panic!("expected Comp(\"not_found\") for a bogus query, got {other:?}"),
        }
    }

    // ── Theme (D2) + intents (A1), 2026-08-23 ───────────────────────────
    // Pure wire-shape tests only. The two ops' round trips need a live comp
    // and are covered by the `#[ignore]`d live-fire tests' own convention
    // above, not simulated here.

    #[test]
    fn theme_consts_are_comps_two_spellings() {
        assert_eq!(THEME_DARK, "dark");
        assert_eq!(THEME_LIGHT, "light");
    }

    #[test]
    fn set_theme_request_serializes_to_the_agreed_wire_shape() {
        let req = serde_json::json!({ "op": "set_theme", "params": { "theme": THEME_DARK } }).to_string();
        assert_eq!(req, r#"{"op":"set_theme","params":{"theme":"dark"}}"#);
    }

    #[test]
    fn every_known_intent_round_trips_through_its_wire_name() {
        // Iterating `ALL` (rather than a hand-written list) is what makes
        // this test keep covering a SECOND intent the day one is added.
        for intent in ShellIntent::ALL {
            assert_eq!(ShellIntent::from_wire(intent.wire_name()), Some(intent));
        }
    }

    #[test]
    fn the_known_intent_vocabulary_has_no_duplicate_wire_names() {
        let mut names: Vec<&str> = ShellIntent::ALL.iter().map(|i| i.wire_name()).collect();
        let before = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), before, "two intents share a wire name: {names:?}");
    }

    #[test]
    fn shell_intent_rejects_unknown_tokens() {
        // Deliberately includes near-misses: a closed vocabulary must not be
        // matched by prefix, case-insensitively, or with surrounding space.
        for raw in ["", "GLOBAL_TASK_BAR", "global_task", "global_task_bar ", "reboot", "global-task-bar"] {
            assert_eq!(ShellIntent::from_wire(raw), None, "unexpectedly accepted {raw:?}");
        }
    }

    #[test]
    fn response_without_an_intents_key_parses_as_nothing_pending() {
        // The compatibility case: a comp build predating `take_shell_intents`
        // sends no `intents` key at all. That must be "nothing pending", not
        // a parse failure.
        let resp: CompResponse = serde_json::from_str(r#"{"ok":true}"#).expect("must parse");
        assert!(resp.ok);
        assert_eq!(resp.intents, None);
        assert!(resp.intents.unwrap_or_default().is_empty());
    }

    #[test]
    fn response_carries_an_intent_list_when_present() {
        let resp: CompResponse =
            serde_json::from_str(r#"{"ok":true,"intents":["global_task_bar"]}"#).expect("must parse");
        assert_eq!(resp.intents.as_deref(), Some(["global_task_bar".to_string()].as_slice()));
    }

    #[test]
    fn an_unknown_intent_is_dropped_without_discarding_the_known_ones_beside_it() {
        // Mirrors what `take_shell_intents` does with a mixed batch — the
        // partition/filter_map logic, exercised without a socket.
        let raw = vec![
            "global_task_bar".to_string(),
            "some_future_intent".to_string(),
            "global_task_bar".to_string(),
        ];
        let known: Vec<ShellIntent> = raw.iter().filter_map(|s| ShellIntent::from_wire(s.as_str())).collect();
        assert_eq!(known, vec![ShellIntent::GlobalTaskBar, ShellIntent::GlobalTaskBar]);
    }
}
