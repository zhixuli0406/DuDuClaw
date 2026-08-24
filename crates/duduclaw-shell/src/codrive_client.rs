//! Co-driving (共駕) ops on `duduclaw-comp`'s shell-control socket — A2
//! (2026-08-23).
//!
//! Two ops, both added by the A2 round on the comp side:
//!
//! ```text
//! {"op":"codrive_status"}                                  -> {"ok":true,"codrive":{…}}
//! {"op":"codrive_drive","params":{"action":"take_wheel"}}  -> {"ok":true,"codrive":{…}}
//! ```
//!
//! Its own file rather than more lines in [`crate::comp_client`]: that
//! module is already over this crate's 800-line ceiling, and these ops have
//! their own reply object (`codrive`) with nine fields of its own. The
//! SOCKET is still shared — everything here goes through
//! `comp_client::call_raw`, so there is exactly one piece of Unix-socket
//! code in this crate (see that function's own doc comment for why the
//! split is a raw-line one rather than another `Option` field on
//! `CompResponse`). Errors are `comp_client::CompClientError`, unchanged.
//!
//! Every function here is a PLAIN BLOCKING call, same contract
//! `comp_client`'s own module doc states: callers run it from a
//! `std::thread::spawn` and bridge the result back to gpui. This module's
//! one caller (`overlay::codrive_row`) does exactly that.
//!
//! ── Trust boundary (do not paraphrase this loosely) ─────────────────────
//! The shell-control socket is the HUMAN side. On the appliance the gateway
//! runs as `User=duduclaw` while comp and this shell run as
//! `User=duduclaw-kiosk` (`appliance/mkosi.extra/etc/systemd/system/
//! duduclaw-gateway.service` / `duduclaw-kiosk.service`), so the uids
//! differ and an agent process structurally cannot reach this socket —
//! comp checks `SO_PEERCRED` for the same uid. A same-uid DEVELOPMENT
//! machine has no such protection: there, an agent running as the same user
//! could talk to this socket like anything else. That is why **Super+Esc
//! remains the only stop that is enforced inside the compositor itself and
//! is structurally unreachable by an agent**; nothing in this file changes
//! or replaces that red line.
//!
//! ── Version skew is the normal case, not an edge case ───────────────────
//! comp and this shell are separately-deployed binaries and WILL run at
//! different versions. So: every field below is `#[serde(default)]`, and a
//! comp that predates these ops answers `{"ok":false,"error":…}` which
//! arrives as `CompClientError::Comp` — the caller's cue to say "this
//! machine has no co-driving", NOT to show an error dialog and NOT to
//! pretend the human is driving. Likewise an unrecognized `mode` token
//! becomes [`DriveMode::Unknown`], which is a THIRD answer ("cannot tell"),
//! never silently folded into [`DriveMode::Human`]: claiming the human has
//! the wheel when we do not know is exactly the failure this whole state
//! machine exists to prevent.

use serde::{Deserialize, Deserializer};

use crate::comp_client::{call_raw, CompClientError};

/// Which of the three driving states the shared desktop is in.
///
/// A CLOSED enum with an explicit unknown arm, the same shape
/// `comp_client::ShellIntent` uses and for the same reason: comp owns the
/// vocabulary, but this value drives what a person is told and which button
/// they get, so a token this build has never heard of must land somewhere
/// visible rather than being coerced into a neighbouring meaning.
///
/// Note what is NOT here: shadow. An agent working in a shadow output does
/// not hold the shared desktop's wheel, so that case is `Human` + the
/// separate `shadow` flag on [`CodriveState`] — see the A2 contract §1.
/// Adding a fourth variant for it would make the shell disagree with comp
/// about what "driving" means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriveMode {
    /// No authenticated co-driving session (or it was stopped). The agent
    /// has zero driving authority over this desktop.
    Human,
    /// A session exists and the agent's seat is live — **the agent is
    /// driving and the person is watching**.
    CoDrive,
    /// A session exists but the agent's seat is frozen — **the wheel is back
    /// in the person's hands**. Paused, not ended.
    Handover,
    /// comp reported a token this build does not know, or reported none at
    /// all. Carries the raw string (empty when the field was absent) so the
    /// diagnostic can name it. Deliberately NOT treated as `Human`.
    Unknown(String),
}

impl Default for DriveMode {
    /// "We were told nothing", not "the human is driving" — see this type's
    /// own doc comment.
    fn default() -> Self {
        DriveMode::Unknown(String::new())
    }
}

impl DriveMode {
    /// Every token this build understands. Exists so the unknown-token
    /// diagnostic can say what the vocabulary IS rather than only that
    /// something was not recognized — the same reason
    /// `comp_client::ShellIntent::ALL` exists.
    pub const KNOWN_WIRE_NAMES: [&'static str; 3] = ["human", "codrive", "handover"];

    /// Parses one wire token. Never fails: an unrecognized value becomes
    /// [`DriveMode::Unknown`] carrying the original string.
    pub fn from_wire(raw: &str) -> Self {
        match raw {
            "human" => DriveMode::Human,
            "codrive" => DriveMode::CoDrive,
            "handover" => DriveMode::Handover,
            other => DriveMode::Unknown(other.to_string()),
        }
    }

    /// comp's spelling for this mode; for [`DriveMode::Unknown`] this is
    /// whatever comp actually sent (possibly empty).
    pub fn wire_name(&self) -> &str {
        match self {
            DriveMode::Human => "human",
            DriveMode::CoDrive => "codrive",
            DriveMode::Handover => "handover",
            DriveMode::Unknown(raw) => raw.as_str(),
        }
    }

    /// Whether this build could actually identify what comp said.
    pub fn is_known(&self) -> bool {
        !matches!(self, DriveMode::Unknown(_))
    }
}

impl<'de> Deserialize<'de> for DriveMode {
    /// Deserializes through `Option<String>` rather than `String` on
    /// purpose: an explicit `"mode": null` from a future comp then degrades
    /// to [`DriveMode::Unknown`] like every other unusable value, instead of
    /// failing the whole response parse and throwing away the eight other
    /// fields beside it.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = Option::<String>::deserialize(deserializer)?;
        Ok(raw.map(|s| DriveMode::from_wire(&s)).unwrap_or_default())
    }
}

/// comp's `codrive` status object — the A2 contract §4.1 field list,
/// mirrored here BY HAND for the reason `comp_client`'s own module doc gives
/// for hand-mirroring every other wire type (this crate cannot depend on
/// `duduclaw-comp`).
///
/// EVERY field is `#[serde(default)]`, including `mode`: comp and this shell
/// are separately deployed and a partial object has to parse into an honest
/// "cannot tell", never into a fabricated state. See this module's own
/// header comment.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct CodriveState {
    /// The one field a person's screen is driven by. Absent/unknown ⇒
    /// [`DriveMode::Unknown`].
    #[serde(default)]
    pub mode: DriveMode,
    /// Why the wheel came back to the person — comp's own closed vocabulary
    /// (`human_input` / `agent_take_over` / `watch_idle` /
    /// `shell_take_wheel`), meaningful only while `mode` is
    /// [`DriveMode::Handover`] and `null` otherwise.
    ///
    /// Kept as a raw `Option<String>` rather than an enum, the same call
    /// `comp_client::CursorState::source` makes: nothing in this shell
    /// BRANCHES on it today (the row shows the state, not the reason), so a
    /// value from a future comp must round-trip harmlessly instead of
    /// needing a new arm here first.
    #[serde(default)]
    pub handover_reason: Option<String>,
    /// There is a co-driving connection that passed authentication.
    #[serde(default)]
    pub session_active: bool,
    /// The agent's seat is frozen (this is what makes a live session read as
    /// [`DriveMode::Handover`] rather than [`DriveMode::CoDrive`]).
    #[serde(default)]
    pub frozen: bool,
    /// The session was stopped outright (Super+Esc, or comp's own emergency
    /// stop path).
    #[serde(default)]
    pub terminated: bool,
    #[serde(default)]
    pub takeover: bool,
    /// The agent is working in a SHADOW output. Reported alongside `mode`,
    /// never merged into it — a shadow session leaves the shared desktop's
    /// wheel with the person (contract §1).
    #[serde(default)]
    pub shadow: bool,
    #[serde(default)]
    pub watch_active: bool,
    #[serde(default)]
    pub watch_paused: bool,
}

/// Permissive reply envelope for both ops — same "shape varies by op"
/// convention `comp_client::CompResponse` documents for its own family.
#[derive(Debug, Clone, Default, Deserialize)]
struct CodriveResponse {
    ok: bool,
    #[serde(default)]
    codrive: Option<CodriveState>,
    #[serde(default)]
    error: Option<String>,
}

/// The two `action` values comp accepts. Consts, not an enum, for the same
/// reason `comp_client::CURSOR_SOURCE_SYSTEM`/`_BRAND` are: comp owns the
/// vocabulary, and the two spellings should exist in exactly one place on
/// this side of the wire.
///
/// `take_wheel` freezes the agent's seat — the contract makes it **always
/// allowed**, on the fail-safe principle that anything at all must be able
/// to tell the agent to stop.
pub const CODRIVE_ACTION_TAKE_WHEEL: &str = "take_wheel";
/// `hand_back` resumes the agent's seat — the button twin of the Super+Enter
/// gesture. See [`CODRIVE_ACTION_TAKE_WHEEL`].
pub const CODRIVE_ACTION_HAND_BACK: &str = "hand_back";

/// comp's refusal code for an `action` outside its closed set. Named so the
/// one place this literal lives is greppable against comp's own listener —
/// and so a caller can tell "the two sides' vocabularies have drifted apart"
/// (a bug worth a distinct log line) from an ordinary refusal.
pub const CODRIVE_ERR_INVALID_ACTION: &str = "invalid_codrive_action";

/// The status request line, as a const so the wire-shape test below asserts
/// against the string this module ACTUALLY sends rather than a copy of it
/// that could drift.
const STATUS_REQUEST: &str = r#"{"op":"codrive_status"}"#;

/// `{"op":"codrive_status"}` — a READ, unaudited on the comp side (same
/// class as `list_windows`/`get_cursor_source`).
///
/// An `ok:true` response with no `codrive` object is a `Protocol` error, not
/// a defaulted state: the entire point of the call is to learn what is
/// happening, and "no answer" is a different fact from "an answer that says
/// nothing" — the same call [`crate::comp_client::get_cursor_source`] makes.
///
/// Blocking; see this module's own header comment for the threading
/// contract.
pub fn codrive_status() -> Result<CodriveState, CompClientError> {
    let state = parse(call_raw(STATUS_REQUEST)?)?;
    note_unknown_mode("codrive_status", &state);
    Ok(state)
}

/// `{"op":"codrive_drive","params":{"action":"take_wheel"|"hand_back"}}` —
/// an ACTION, audited on the comp side. Returns the state AFTER the action,
/// which is what makes this a real observation rather than an assumption:
/// this client never repaints a mode it merely requested.
///
/// `action` is `&str` rather than an enum for the same reason
/// [`CODRIVE_ACTION_TAKE_WHEEL`] is a const — call sites pass one of the two
/// consts above so the spellings live in exactly one place. Anything else
/// comes back as `Err(CompClientError::Comp("invalid_codrive_action"))`,
/// never a silent success.
pub fn codrive_drive(action: &str) -> Result<CodriveState, CompClientError> {
    let req = serde_json::json!({ "op": "codrive_drive", "params": { "action": action } }).to_string();
    let state = parse(call_raw(&req)?)?;
    note_unknown_mode("codrive_drive", &state);
    Ok(state)
}

/// Shared reply handling for both ops — identical apart from the request
/// line, so it lives here once.
fn parse(line: String) -> Result<CodriveState, CompClientError> {
    let resp: CodriveResponse =
        serde_json::from_str(line.trim()).map_err(|e| CompClientError::Protocol(e.to_string()))?;
    if !resp.ok {
        return Err(CompClientError::Comp(resp.error.unwrap_or_else(|| "unknown error".to_string())));
    }
    resp.codrive.ok_or_else(|| CompClientError::Protocol("ok response carried no codrive object".to_string()))
}

/// Reports a driving mode this build cannot interpret, once per call.
///
/// Not an `Err`: the rest of the object is still perfectly usable, and
/// failing the whole read would leave the surface with nothing to say at
/// all. Same "drop the token, keep everything beside it, and say so on
/// stderr" handling `comp_client::take_shell_intents` applies to an
/// unrecognized intent.
fn note_unknown_mode(op: &str, state: &CodriveState) {
    if state.mode.is_known() {
        return;
    }
    eprintln!(
        "[codrive_client] {op}: comp reported driving mode {:?}, which this build does not understand \
         (known: {:?}) — presenting it as \"cannot tell\", never as human control",
        state.mode.wire_name(),
        DriveMode::KNOWN_WIRE_NAMES
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    // Wire-shape and parse tests only. The socket round trip belongs to
    // `comp_client::call_raw` and is covered by that module's own tests
    // (including the missing-socket -> `NotAvailable` path, which is where
    // every function here inherits its dev-Mac behaviour from); duplicating
    // it here would also mean a SECOND `XDG_RUNTIME_DIR` mutex racing that
    // module's own — the exact failure its `env_guard` doc comment records.

    fn parse_ok(json: &str) -> CodriveState {
        parse(json.to_string()).expect("must parse")
    }

    #[test]
    fn the_status_request_is_the_agreed_wire_shape() {
        let back: serde_json::Value = serde_json::from_str(STATUS_REQUEST).expect("the request this module sends must be valid JSON");
        assert_eq!(back["op"], "codrive_status");
        assert!(back.get("params").is_none(), "the status op takes no params");
    }

    #[test]
    fn the_drive_request_serializes_to_the_agreed_wire_shape() {
        for action in [CODRIVE_ACTION_TAKE_WHEEL, CODRIVE_ACTION_HAND_BACK] {
            let req = serde_json::json!({ "op": "codrive_drive", "params": { "action": action } }).to_string();
            let back: serde_json::Value = serde_json::from_str(&req).unwrap();
            assert_eq!(back["op"], "codrive_drive");
            assert_eq!(back["params"]["action"], action);
        }
    }

    /// The contract's own §4.1 example, copied verbatim rather than invented
    /// here.
    #[test]
    fn a_full_status_object_parses_field_for_field() {
        let state = parse_ok(
            r#"{"ok":true,"codrive":{"mode":"codrive","handover_reason":null,
                "session_active":true,"frozen":false,"terminated":false,"takeover":false,
                "shadow":false,"watch_active":false,"watch_paused":false}}"#,
        );
        assert_eq!(state.mode, DriveMode::CoDrive);
        assert_eq!(state.handover_reason, None);
        assert!(state.session_active);
        assert!(!state.frozen);
        assert!(!state.terminated);
        assert!(!state.takeover);
        assert!(!state.shadow);
        assert!(!state.watch_active);
        assert!(!state.watch_paused);
    }

    #[test]
    fn a_handover_object_carries_its_reason() {
        let state = parse_ok(
            r#"{"ok":true,"codrive":{"mode":"handover","handover_reason":"shell_take_wheel","session_active":true,"frozen":true}}"#,
        );
        assert_eq!(state.mode, DriveMode::Handover);
        assert_eq!(state.handover_reason.as_deref(), Some("shell_take_wheel"));
        assert!(state.frozen);
    }

    /// Contract §1: shadow is deliberately NOT a fourth mode. An agent
    /// working in a shadow output leaves the shared desktop's wheel with the
    /// person, so the mode stays `human` and the flag rides alongside it.
    #[test]
    fn shadow_is_reported_beside_the_mode_not_folded_into_it() {
        let state = parse_ok(r#"{"ok":true,"codrive":{"mode":"human","shadow":true,"session_active":true}}"#);
        assert_eq!(state.mode, DriveMode::Human);
        assert!(state.shadow);
    }

    /// The version-skew case this whole module is written around: a comp
    /// that answers with fewer fields than this build expects must parse,
    /// and the missing mode must read as "cannot tell" — NOT as human
    /// control.
    #[test]
    fn a_status_object_missing_every_optional_field_is_unknown_not_human() {
        let state = parse_ok(r#"{"ok":true,"codrive":{}}"#);
        assert_eq!(state.mode, DriveMode::Unknown(String::new()));
        assert_ne!(state.mode, DriveMode::Human, "silence must never be read as human control");
        assert!(!state.mode.is_known());
        assert!(!state.session_active);
        assert_eq!(state.handover_reason, None);
    }

    /// A token from a FUTURE comp: kept verbatim, still not human, still not
    /// a parse failure that would discard the eight fields beside it.
    #[test]
    fn an_unknown_mode_token_is_preserved_and_is_not_human() {
        let state = parse_ok(r#"{"ok":true,"codrive":{"mode":"supervising","session_active":true,"shadow":true}}"#);
        assert_eq!(state.mode, DriveMode::Unknown("supervising".to_string()));
        assert_eq!(state.mode.wire_name(), "supervising");
        assert!(!state.mode.is_known());
        assert!(state.session_active, "the fields beside an unknown mode still have to survive");
        assert!(state.shadow);
    }

    /// An explicit `null` degrades the same way an absent key does.
    #[test]
    fn an_explicitly_null_mode_degrades_instead_of_failing_the_whole_parse() {
        let state = parse_ok(r#"{"ok":true,"codrive":{"mode":null,"session_active":true}}"#);
        assert_eq!(state.mode, DriveMode::default());
        assert!(state.session_active);
    }

    /// Fields a FUTURE comp adds must not break this build's parse — that is
    /// what makes the wire additive.
    #[test]
    fn unknown_extra_fields_are_ignored() {
        let state = parse_ok(r#"{"ok":true,"codrive":{"mode":"human","some_future_field":42,"another":{"nested":true}}}"#);
        assert_eq!(state.mode, DriveMode::Human);
    }

    /// A comp build that predates these ops entirely. This must surface as
    /// an honest refusal the caller can render as "no co-driving on this
    /// machine", not as a defaulted state.
    #[test]
    fn a_comp_that_does_not_know_the_op_is_a_refusal_not_a_default_state() {
        let err = parse(r#"{"ok":false,"error":"unknown_op"}"#.to_string()).expect_err("must not parse as a state");
        match err {
            CompClientError::Comp(code) => assert_eq!(code, "unknown_op"),
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    #[test]
    fn an_ok_response_with_no_codrive_object_is_a_protocol_error() {
        let err = parse(r#"{"ok":true}"#.to_string()).expect_err("no state was actually reported");
        assert!(matches!(err, CompClientError::Protocol(_)), "unexpected error variant: {err:?}");
    }

    #[test]
    fn a_non_json_line_is_a_protocol_error_not_a_panic() {
        let err = parse("not json at all".to_string()).expect_err("must not parse");
        assert!(matches!(err, CompClientError::Protocol(_)), "unexpected error variant: {err:?}");
    }

    /// Iterating `KNOWN_WIRE_NAMES` (rather than a hand-written list) is what
    /// keeps this test covering a FOURTH mode the day one is added.
    #[test]
    fn every_known_mode_round_trips_through_its_wire_name() {
        for name in DriveMode::KNOWN_WIRE_NAMES {
            let mode = DriveMode::from_wire(name);
            assert!(mode.is_known(), "{name} must be a known mode");
            assert_eq!(mode.wire_name(), name);
        }
    }

    /// A closed vocabulary must not be matched by prefix, case-insensitively
    /// or with surrounding space — same near-miss battery
    /// `comp_client::ShellIntent`'s own test uses.
    #[test]
    fn near_miss_mode_tokens_do_not_match() {
        for raw in ["", "HUMAN", "Human", "hum", "human ", "co-drive", "co_drive", "hand_over"] {
            assert!(!DriveMode::from_wire(raw).is_known(), "unexpectedly accepted {raw:?}");
        }
    }

    /// The two action spellings are comp's, verbatim. A silent edit here
    /// would send a token comp refuses.
    #[test]
    fn the_action_consts_are_comps_two_spellings() {
        assert_eq!(CODRIVE_ACTION_TAKE_WHEEL, "take_wheel");
        assert_eq!(CODRIVE_ACTION_HAND_BACK, "hand_back");
        assert_eq!(CODRIVE_ERR_INVALID_ACTION, "invalid_codrive_action");
    }

    /// Live-fire, `#[ignore]`d — same "never run by a bare `cargo test`"
    /// contract `comp_client`'s own live tests establish. Run against a REAL
    /// `duduclaw-comp` with `XDG_RUNTIME_DIR` pointed at its runtime dir:
    ///   `XDG_RUNTIME_DIR=/tmp/xdg-runtime cargo test -- --ignored \
    ///    live_codrive_status_against_real_comp --nocapture`
    ///
    /// Deliberately READ-only: `codrive_drive` changes who is holding the
    /// wheel on a live machine, which is not something a test suite should
    /// do to whoever is sitting at it.
    #[test]
    #[ignore = "requires a live duduclaw-comp with its shell-control socket up — see doc comment"]
    fn live_codrive_status_against_real_comp() {
        match codrive_status() {
            Ok(state) => eprintln!("[live] codrive_status -> {state:?}"),
            Err(e) => panic!("codrive_status failed against a supposedly-live comp: {e}"),
        }
    }
}
