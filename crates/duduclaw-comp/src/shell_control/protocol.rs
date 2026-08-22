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
}

impl ShellControlRequest {
    /// Stable short name for tracing/audit fields — same motive as
    /// `duduclaw-sysd::protocol::SysdRequest::verb_name`.
    pub fn op_name(&self) -> &'static str {
        match self {
            ShellControlRequest::ListWindows => "list_windows",
            ShellControlRequest::FocusWindow { .. } => "focus_window",
        }
    }
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ShellControlResponse {
    pub fn windows(windows: Vec<ShellWindowInfo>) -> Self {
        Self { ok: true, windows: Some(windows), matched_app_id: None, matched_title_prefix: None, error: None }
    }

    /// A `focus_window` hit — exactly one of `matched_app_id`/
    /// `matched_title_prefix` is `Some`, mirroring `codrive::window_target::
    /// WindowMatch`'s own two variants (never both, never neither).
    pub fn focused_by_app_id(app_id: String) -> Self {
        Self { ok: true, windows: None, matched_app_id: Some(app_id), matched_title_prefix: None, error: None }
    }

    pub fn focused_by_title_prefix(title: String) -> Self {
        Self { ok: true, windows: None, matched_app_id: None, matched_title_prefix: Some(title), error: None }
    }

    pub fn err(error: impl Into<String>) -> Self {
        Self { ok: false, windows: None, matched_app_id: None, matched_title_prefix: None, error: Some(error.into()) }
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
        }]);
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains(r#""ok":true"#));
        assert!(s.contains(r#""windows""#));
        assert!(!s.contains("matched_app_id"));
        assert!(!s.contains("matched_title_prefix"));
        assert!(!s.contains("\"error\""));
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
}
