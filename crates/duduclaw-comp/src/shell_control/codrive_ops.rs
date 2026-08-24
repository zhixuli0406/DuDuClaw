//! A2 共駕復活 (2026-08-24) — the HUMAN side of the driving-mode contract.
//!
//! Two ops on the shell-control socket (A2 contract §4), so a person at the
//! keyboard can see who is driving and take the wheel back with a button
//! instead of a keyboard shortcut:
//!
//! ```text
//! -> {"op":"codrive_status"}
//! <- {"ok":true,"codrive":{"mode":"codrive","handover_reason":null,
//!                          "session_active":true,"frozen":false,
//!                          "terminated":false,"takeover":false,
//!                          "shadow":false,"watch_active":false,
//!                          "watch_paused":false}}
//!
//! -> {"op":"codrive_drive","params":{"action":"take_wheel"}}
//! <- {"ok":true,"codrive":{"mode":"handover",
//!                          "handover_reason":"shell_take_wheel",…}}
//!
//! -> {"op":"codrive_drive","params":{"action":"hand_back"}}
//! <- {"ok":true,"codrive":{"mode":"codrive","handover_reason":null,…}}
//!
//! -> {"op":"codrive_drive","params":{"action":"park"}}
//! <- {"ok":false,"error":"invalid_codrive_action"}
//! ```
//!
//! `codrive_status` is a READ — unaudited, like `list_windows` /
//! `get_cursor_source` / `get_outputs` (a shell polling "who is driving" to
//! paint a status pill would otherwise flood the trail). `codrive_drive` is
//! an ACTION — always audited, on both trails: this module's own
//! `duduclaw-shell-control-audit.jsonl` line says a HUMAN pressed the button,
//! and the `freeze`/`resume`/`driving_mode` lines it causes land in
//! `codrive`'s own trail where the agent-facing state machine's history
//! lives.
//!
//! ## The two actions
//! - **`take_wheel`** freezes the agent seat with `reason=shell_take_wheel`.
//!   It is **always allowed** — never gated on the current mode, never
//!   refused. That direction is the fail-safe one: anything that can reach a
//!   human's own session must be able to say "stop". It deliberately does NOT
//!   route through `on_human_input`, even though the freeze it performs is
//!   the same: `on_human_input`'s first act is to let a watch-mode idle pause
//!   interpret the event as proof of presence and **un**freeze, which is the
//!   exact opposite of what this button means.
//! - **`hand_back`** is `human_resume()` — the button form of Super+Enter,
//!   which DESIGN §3.1 names literally ("『交還』是明確動作（按鈕/
//!   Super+Enter）"). It reuses that method rather than re-implementing the
//!   un-freeze, so the shadow hand-back, takeover teardown and watch-pause
//!   clearing it already performs cannot drift out of sync with the keyboard
//!   path.
//!
//! Both actions refresh `codrive_last_human_activity`: a person clicking a
//! button in the shell IS human presence, and without it a `hand_back` could
//! be followed immediately by a watch-mode idle re-pause measured from a
//! stale clock. The keyboard path gets this for free (the Super+Enter key
//! event itself runs `on_human_input` first); the socket path has no such
//! event, so it says so explicitly.
//!
//! ## Trust boundary — why a human-side "stop the agent" button may live here
//! On the appliance these are two DIFFERENT system users, kernel-enforced:
//! the gateway (and therefore every agent CLI subprocess) runs
//! `User=duduclaw`, while the shell and this compositor run
//! `User=duduclaw-kiosk` — read from
//! `appliance/mkosi.extra/etc/systemd/system/duduclaw-gateway.service` and
//! `duduclaw-kiosk.service` directly, not assumed. This socket authenticates
//! by `SO_PEERCRED` same-uid (`listener::is_authorized_peer`), so an agent
//! process **structurally cannot open it** — it is not a matter of policy or
//! of a secret it might read.
//!
//! **A same-uid development machine does not have that protection.** There,
//! comp, the shell and any agent process can all run as one user, and
//! anything that can call `codrive_status` can also call `codrive_drive`. Two
//! things follow, and neither is hedged:
//! 1. `codrive_drive` is written so that the *dangerous* direction is the one
//!    that does not exist. `take_wheel` only ever STOPS the agent. `hand_back`
//!    resumes it — but resuming was already reachable by anything with the
//!    codrive token (an agent could simply not have frozen itself), and the
//!    codrive injection socket's own `resume` op stays unconditionally denied
//!    exactly as CD-1 left it. This op adds no new way for an agent to clear
//!    a freeze a HUMAN caused on a machine where it could not already do so.
//! 2. **Super+Esc remains the only stop that is structurally unreachable by
//!    the agent** — it is detected in the compositor's own human keyboard
//!    filter, which no injected event can enter. That red line is unchanged
//!    by this module and must stay that way; a button on a socket is a
//!    convenience, never a replacement for it.

use serde::Serialize;

use crate::codrive::{self, CodriveStatusSnapshot, DrivingMode, HandoverReason};
use crate::state::DuduclawComp;

use super::protocol::ShellControlResponse;

/// Hard cap on `codrive_drive`'s `action` field, bytes. The legal values are
/// `"take_wheel"` / `"hand_back"`; anything remotely near this bound is
/// already a bug or an attack. Same "check the length before the strict
/// parser ever sees a pathological string" ordering — and the same value — as
/// `protocol::MAX_CURSOR_SOURCE_BYTES`.
pub(super) const MAX_CODRIVE_ACTION_BYTES: usize = 32;

/// The closed set `codrive_drive` accepts (A2 contract §4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CodriveDriveAction {
    /// The human takes the wheel — freeze the agent seat.
    TakeWheel,
    /// The human gives it back — `human_resume()`.
    HandBack,
}

impl CodriveDriveAction {
    /// Exact-match only, and deliberately stricter than the appearance ops on
    /// this socket: `set_cursor_source`/`set_theme` trim and case-fold
    /// because a person may type those into a settings field, whereas this
    /// one is only ever sent by the shell's own button handler from a fixed
    /// string. A near-miss here is a bug in the caller, so it is reported as
    /// one rather than guessed at — the same reasoning
    /// `CursorSource::parse_strict` gives for refusing `"brnad"` instead of
    /// coercing it, taken one step further.
    pub(super) fn parse_strict(raw: &str) -> Option<Self> {
        match raw {
            "take_wheel" => Some(CodriveDriveAction::TakeWheel),
            "hand_back" => Some(CodriveDriveAction::HandBack),
            _ => None,
        }
    }

    /// The wire token — also what the audit line records.
    pub(super) fn as_str(self) -> &'static str {
        match self {
            CodriveDriveAction::TakeWheel => "take_wheel",
            CodriveDriveAction::HandBack => "hand_back",
        }
    }
}

/// The `codrive` block both A2 shell-control ops answer with (contract §4.1).
///
/// Every field is always present. `handover_reason` in particular is NOT
/// `skip_serializing_if`-elided: the contract says it serializes as `null`
/// outside `handover`, and a shell reading a missing key would have to guess
/// whether that meant "no reason" or "old compositor".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CodriveStatusInfo {
    /// `human` / `codrive` / `handover` — see `codrive::DrivingMode`.
    pub mode: String,
    /// Only ever `Some` while `mode == "handover"`.
    pub handover_reason: Option<String>,
    /// An authenticated codrive connection exists.
    pub session_active: bool,
    /// The raw agent-seat freeze flag, reported honestly even when the mode
    /// is `human` (an emergency stop leaves it latched — see
    /// `codrive::derive_mode`'s doc).
    pub frozen: bool,
    pub terminated: bool,
    pub takeover: bool,
    /// CD-2 shadow workspace — deliberately NOT folded into `mode`, see
    /// `codrive/mode.rs`'s module doc.
    pub shadow: bool,
    pub watch_active: bool,
    pub watch_paused: bool,
}

impl CodriveStatusInfo {
    fn from_snapshot(snap: &CodriveStatusSnapshot) -> Self {
        // Named rather than inlined so the two `String` fields below carry
        // their real types at the one place they stop being types: both are
        // the WIRE ENCODING of a closed enum, and nothing on this socket may
        // ever put anything else in them.
        let mode: DrivingMode = snap.mode;
        let reason: Option<HandoverReason> = snap.handover_reason;
        Self {
            mode: mode.as_str().to_string(),
            handover_reason: reason.map(|r| r.as_str().to_string()),
            session_active: snap.session_active,
            frozen: snap.frozen,
            terminated: snap.terminated,
            takeover: snap.takeover,
            shadow: snap.shadow,
            watch_active: snap.watch_active,
            watch_paused: snap.watch_paused,
        }
    }
}

impl DuduclawComp {
    /// The `codrive` block for right now, built from the same
    /// `codrive::status_snapshot` the agent-side injection socket answers
    /// `status` from — one derivation, two channels, so the human's view and
    /// the agent's view of one desktop can never disagree.
    fn shell_control_codrive_info(&self) -> CodriveStatusInfo {
        CodriveStatusInfo::from_snapshot(&codrive::status_snapshot(&self.codrive))
    }

    /// `{"op":"codrive_status"}` — READ, never audited (see this module's
    /// doc).
    pub(super) fn shell_control_codrive_status(&self) -> ShellControlResponse {
        ShellControlResponse::codrive(self.shell_control_codrive_info())
    }

    /// `{"op":"codrive_drive","params":{"action":…}}` — ACTION, always
    /// audited, answers with the state AFTER the action ran.
    ///
    /// `action` has already been through `listener::validate`, so
    /// `parse_strict` here cannot fail; it is re-parsed rather than passed as
    /// an enum because the wire type is a string and the parse is the
    /// boundary — the same defensive shape `shell_control_set_cursor_source`
    /// and `shell_control_set_theme` both use, so a validation gap lands on a
    /// real error response instead of a panic or an unvalidated action.
    pub(super) fn shell_control_codrive_drive(&mut self, action: &str) -> ShellControlResponse {
        let Some(action) = CodriveDriveAction::parse_strict(action) else {
            tracing::error!(
                "shell_control: codrive_drive reached the main thread with a value \
                 listener::validate should have refused — refusing here too"
            );
            self.shell_control.record(
                "codrive_drive_failed",
                Some("invalid_codrive_action".to_string()),
            );
            return ShellControlResponse::err("invalid_codrive_action");
        };

        let before = codrive::status_snapshot(&self.codrive);

        // A human pressed a button — that IS presence. See this module's doc
        // for why the socket path has to say so explicitly while the
        // Super+Enter path gets it for free.
        self.codrive_last_human_activity = std::time::Instant::now();

        match action {
            // The freeze itself lives in `codrive/mode.rs`, not here: the
            // co-drive state machine owns its own transitions (and its own
            // audit vocabulary and push events), and this module is only the
            // human-side front door onto it.
            CodriveDriveAction::TakeWheel => self.codrive_shell_take_wheel(),
            // The button form of Super+Enter. `human_resume` itself ends any
            // shadow session / takeover / watch pause and syncs the mode.
            CodriveDriveAction::HandBack => self.human_resume(),
        }

        let info = self.shell_control_codrive_info();
        self.shell_control.record(
            "codrive_drive",
            Some(format!(
                "action={} mode_before={} mode_after={} reason={}",
                action.as_str(),
                before.mode.as_str(),
                info.mode,
                info.handover_reason.as_deref().unwrap_or("none")
            )),
        );
        ShellControlResponse::codrive(info)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell_control::protocol::ShellControlRequest;

    // ── The action parser ────────────────────────────────────────────────

    #[test]
    fn parse_strict_accepts_exactly_the_two_contract_actions() {
        assert_eq!(
            CodriveDriveAction::parse_strict("take_wheel"),
            Some(CodriveDriveAction::TakeWheel)
        );
        assert_eq!(
            CodriveDriveAction::parse_strict("hand_back"),
            Some(CodriveDriveAction::HandBack)
        );
    }

    #[test]
    fn parse_strict_refuses_everything_else_including_near_misses() {
        for bad in [
            "",
            "   ",
            "TAKE_WHEEL",
            " take_wheel",
            "take_wheel ",
            "takewheel",
            "take-wheel",
            "handback",
            "resume",
            "stop",
            "park",
            "🐾",
        ] {
            assert_eq!(
                CodriveDriveAction::parse_strict(bad),
                None,
                "{bad:?} must be refused, never coerced"
            );
        }
    }

    #[test]
    fn action_tokens_round_trip() {
        for a in [CodriveDriveAction::TakeWheel, CodriveDriveAction::HandBack] {
            assert_eq!(CodriveDriveAction::parse_strict(a.as_str()), Some(a));
        }
    }

    #[test]
    fn the_action_cap_is_the_cursor_source_cap() {
        assert_eq!(
            MAX_CODRIVE_ACTION_BYTES,
            super::super::protocol::MAX_CURSOR_SOURCE_BYTES
        );
        // Both legal values must comfortably clear it, or the length check
        // would refuse a valid request before the parser ever ran.
        for a in [CodriveDriveAction::TakeWheel, CodriveDriveAction::HandBack] {
            assert!(a.as_str().len() < MAX_CODRIVE_ACTION_BYTES);
        }
    }

    /// `shell_control::listener::validate` under a clearer local name — the
    /// A2 ops' validation tests live here, with the rest of A2's shell-side
    /// tests, rather than in `listener.rs`, which was already past the
    /// 800-line cap before this round.
    use super::super::listener::validate as listener_validate;

    // ── `shell_control::listener::validate` for the two A2 ops ──────────

    #[test]
    fn validate_accepts_codrive_status() {
        assert!(listener_validate(&ShellControlRequest::CodriveStatus).is_ok());
    }

    #[test]
    fn validate_accepts_the_two_legal_drive_actions() {
        for v in ["take_wheel", "hand_back"] {
            let req = ShellControlRequest::CodriveDrive {
                action: v.to_string(),
            };
            assert!(listener_validate(&req).is_ok(), "{v:?} should be accepted");
        }
    }

    #[test]
    fn validate_refuses_an_unknown_drive_action_and_does_not_case_fold() {
        // Deliberately stricter than `set_cursor_source`/`set_theme`: this
        // value comes from the shell's own button handler, never a text
        // field, so a near-miss is a caller bug to report, not typing to
        // forgive.
        for v in [
            "",
            "   ",
            "TAKE_WHEEL",
            " take_wheel ",
            "takewheel",
            "resume",
            "stop",
            "🐾",
        ] {
            let req = ShellControlRequest::CodriveDrive {
                action: v.to_string(),
            };
            assert_eq!(
                listener_validate(&req).unwrap_err(),
                "invalid_codrive_action",
                "{v:?} should be refused"
            );
        }
    }

    #[test]
    fn validate_rejects_an_oversized_drive_action() {
        let req = ShellControlRequest::CodriveDrive {
            action: "t".repeat(MAX_CODRIVE_ACTION_BYTES + 1),
        };
        let err = listener_validate(&req).unwrap_err();
        assert!(err.contains(&MAX_CODRIVE_ACTION_BYTES.to_string()), "{err}");
    }

    #[test]
    fn an_invalid_drive_action_error_does_not_echo_the_callers_value() {
        let req = ShellControlRequest::CodriveDrive {
            action: "<script>".to_string(),
        };
        let err = listener_validate(&req).unwrap_err();
        assert_eq!(err, "invalid_codrive_action");
        assert!(!err.contains("script"));
    }

    // ── Wire shape: request ──────────────────────────────────────────────

    #[test]
    fn codrive_status_wire_shape_has_no_params() {
        let s = serde_json::to_string(&ShellControlRequest::CodriveStatus).unwrap();
        assert_eq!(s, r#"{"op":"codrive_status"}"#);
        let back: ShellControlRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(back, ShellControlRequest::CodriveStatus);
    }

    #[test]
    fn codrive_drive_wire_shape_round_trips() {
        let req = ShellControlRequest::CodriveDrive {
            action: "take_wheel".to_string(),
        };
        let s = serde_json::to_string(&req).unwrap();
        assert_eq!(
            s,
            r#"{"op":"codrive_drive","params":{"action":"take_wheel"}}"#
        );
        let back: ShellControlRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn codrive_ops_reject_stray_fields_and_missing_params_like_every_other_op() {
        for raw in [
            r#"{"op":"codrive_status","params":{}}"#,
            r#"{"op":"codrive_drive","params":{"action":"take_wheel","force":true}}"#,
            r#"{"op":"codrive_drive","params":{}}"#,
            r#"{"op":"codrive_drive"}"#,
            r#"{"op":"codrive_drive","params":{"action":123}}"#,
            r#"{"op":"codrive_drive","params":{"action":null}}"#,
        ] {
            assert!(
                serde_json::from_str::<ShellControlRequest>(raw).is_err(),
                "{raw} must not parse"
            );
        }
    }

    #[test]
    fn codrive_op_names_are_stable_and_do_not_leak_the_action() {
        assert_eq!(
            ShellControlRequest::CodriveStatus.op_name(),
            "codrive_status"
        );
        assert_eq!(
            ShellControlRequest::CodriveDrive {
                action: "take_wheel".into()
            }
            .op_name(),
            "codrive_drive"
        );
    }

    // ── Wire shape: response ─────────────────────────────────────────────

    fn info(mode: DrivingMode, reason: Option<HandoverReason>) -> CodriveStatusInfo {
        CodriveStatusInfo::from_snapshot(&CodriveStatusSnapshot {
            mode,
            handover_reason: reason,
            session_active: mode != DrivingMode::Human,
            frozen: mode == DrivingMode::Handover,
            terminated: false,
            takeover: false,
            shadow: false,
            watch_active: false,
            watch_paused: false,
        })
    }

    #[test]
    fn the_codrive_block_is_the_exact_contract_shape() {
        let resp = ShellControlResponse::codrive(info(DrivingMode::CoDrive, None));
        let s = serde_json::to_string(&resp).unwrap();
        assert_eq!(
            s,
            r#"{"ok":true,"codrive":{"mode":"codrive","handover_reason":null,"session_active":true,"frozen":false,"terminated":false,"takeover":false,"shadow":false,"watch_active":false,"watch_paused":false}}"#
        );
    }

    #[test]
    fn handover_reason_is_null_not_omitted_outside_handover() {
        // A shell reading a MISSING key could not tell "no reason" from "old
        // compositor" — the contract requires an explicit null.
        let s = serde_json::to_string(&ShellControlResponse::codrive(info(
            DrivingMode::Human,
            None,
        )))
        .unwrap();
        assert!(s.contains(r#""handover_reason":null"#), "unexpected: {s}");
    }

    #[test]
    fn a_handover_block_carries_its_trigger_token() {
        let s = serde_json::to_string(&ShellControlResponse::codrive(info(
            DrivingMode::Handover,
            Some(HandoverReason::ShellTakeWheel),
        )))
        .unwrap();
        assert!(s.contains(r#""mode":"handover""#), "unexpected: {s}");
        assert!(
            s.contains(r#""handover_reason":"shell_take_wheel""#),
            "unexpected: {s}"
        );
        assert!(s.contains(r#""frozen":true"#), "unexpected: {s}");
    }

    #[test]
    fn the_codrive_response_omits_every_other_ops_fields() {
        let s = serde_json::to_string(&ShellControlResponse::codrive(info(
            DrivingMode::Human,
            None,
        )))
        .unwrap();
        for key in [
            "\"windows\"",
            "matched_app_id",
            "matched_title_prefix",
            "\"cursor\"",
            "\"outputs\"",
            "intents",
            "\"error\"",
        ] {
            assert!(!s.contains(key), "{key} leaked into a codrive reply: {s}");
        }
    }

    #[test]
    fn non_codrive_responses_omit_the_codrive_field() {
        // Regression guard for the additive envelope field: every other
        // constructor must still omit it entirely, never emit `"codrive":null`.
        for s in [
            serde_json::to_string(&ShellControlResponse::windows(vec![])).unwrap(),
            serde_json::to_string(&ShellControlResponse::ok()).unwrap(),
            serde_json::to_string(&ShellControlResponse::intents(vec![])).unwrap(),
            serde_json::to_string(&ShellControlResponse::err("not_found")).unwrap(),
        ] {
            assert!(!s.contains("codrive"), "unexpected: {s}");
        }
    }

    #[test]
    fn every_mode_serializes_as_valid_json_with_the_derived_token() {
        for (mode, reason) in [
            (DrivingMode::Human, None),
            (DrivingMode::CoDrive, None),
            (DrivingMode::Handover, Some(HandoverReason::WatchIdle)),
        ] {
            let s =
                serde_json::to_string(&ShellControlResponse::codrive(info(mode, reason))).unwrap();
            let v: serde_json::Value = serde_json::from_str(&s).unwrap();
            assert_eq!(v["codrive"]["mode"], mode.as_str());
        }
    }
}
