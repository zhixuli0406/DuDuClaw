// Local power control — ICON-3 (2026-08-23), the lockscreen's bottom-centre
// power button.
//
// ── Why this exists at all ───────────────────────────────────────────────
// The lock screen is the ONE surface an operator can reach without
// credentials, and a duty machine sitting in an office needs a way to be
// restarted or shut down cleanly by whoever is standing in front of it —
// that is exactly the situation every mainstream lock screen already covers
// (see `research/native-os-2026-08/lockscreen-oobe-icons-2026-08.md` §A.1:
// a power control on the lock surface is the cross-OS norm, not a
// DuDuClaw invention). Doing it any other way would mean pulling the plug.
//
// ── The wire contract (owned by the gateway, mirrored here) ──────────────
// `device.power_local`, one WS RPC, `params: {"action":"reboot"|"shutdown"}`,
// over a PRE-AUTH connection (`ws_rpc::call_once_pre_auth` — see that
// section's own comment for the handshake). The gateway enforces the fences
// that make this safe to expose without a login: appliance mode, a loopback
// peer, a closed two-value action set, and a rate limit. Nothing in this
// file decides whether the machine MAY power off; it only asks, and renders
// what it is told.
//
// ── Why NOT the local-session path ──────────────────────────────────────
// An earlier draft bootstrapped a JWT through `POST /api/session/local` and
// called the RPC as an authenticated client. That works on a Personal-edition
// box with `local_auto_login` on, and returns 403 everywhere else — leaving
// the power button dead on exactly the Enterprise/Pro appliances this
// feature is for. The pre-auth handshake depends on neither the edition nor
// any login state, which is why it is the only path here.
//
// ── Blocking, like every other client in this module tree ───────────────
// One `std::thread::spawn` per attempt, bridged back through
// `std::sync::mpsc` + a `cx.spawn` poll loop by the caller
// (`lockscreen::render::dispatch_power_action`), same contract
// `gateway_client`'s own module doc states for all of its siblings.

use serde_json::{json, Value};

use super::ws_rpc::{self, PreAuthOutcome, RpcError};

/// The two actions this surface offers. Deliberately a closed enum rather
/// than a `&str` threaded from the UI: the wire value is decided HERE, in
/// one place, so no call site can invent a third action or typo one of these
/// two into a silent no-op. (The gateway independently refuses anything
/// outside its own closed set — `invalid_action` — so this is the near half
/// of a defence in depth, not the only guard.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PowerAction {
    Reboot,
    Shutdown,
}

impl PowerAction {
    /// The `action` param's wire value. Matches the gateway's own contract
    /// exactly; changing either side without the other is an
    /// `invalid_action` rejection, not a silent misfire.
    fn wire(self) -> &'static str {
        match self {
            PowerAction::Reboot => "reboot",
            PowerAction::Shutdown => "shutdown",
        }
    }
}

/// Why a power request did not land. Two variants, not five, because the
/// operator has exactly two different responses available:
///
/// * `Unsupported` — this machine will not do it, and no amount of retrying
///   changes that: an older gateway that has never heard of the method, or a
///   live one that refused because this is not an appliance / not a local
///   caller / not a legal action. All four are "stop asking", so they share
///   one message.
/// * `Failed` — transport trouble on the way there: the gateway could not be
///   reached, the handshake was refused, the frame was malformed, or the
///   request was rate-limited. Retrying is the sensible response. Same
///   collapse `lockscreen::UnlockFailureKind::Unreachable` already makes for
///   the unlock path, and for the same reason: an operator standing at a
///   locked screen cannot act on the difference between a timeout and a
///   malformed frame.
///
/// Both carry the underlying detail as text purely for the stderr
/// diagnostic at the call site — it is never rendered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PowerError {
    Unsupported(String),
    Failed(String),
}

/// The gateway's own denial codes for this method
/// (`duduclaw-gateway/src/power_local.rs::PowerLocalDenial::code`), and the
/// one that is NOT a denial at all but an older build answering a method it
/// does not have.
///
/// Read STRUCTURALLY — the refusal frame's `error` is a JSON object
/// (`{"code":…,"message":…}`), so this parses it and compares the `code`
/// field, never a substring of the whole payload. That matters beyond
/// tidiness: the sibling `message` is free Chinese prose, and a `contains`
/// against the serialized object would let prose decide a branch (this
/// crate's coding convention 2, applied to a UI decision rather than a
/// security one).
const CODE_NOT_APPLIANCE: &str = "not_appliance";
const CODE_NOT_LOCAL: &str = "not_local";
const CODE_INVALID_ACTION: &str = "invalid_action";
const CODE_RATE_LIMITED: &str = "rate_limited";

/// An older gateway answers an unrecognised method with a plain STRING error
/// (`WsFrame::error_response(…, "Unknown method: …")`), not the object above.
/// `ws_rpc` hands the `error` field back as its JSON text, so a string value
/// arrives quoted — hence the leading quote. Anchored with `starts_with`, so
/// a live gateway's own prose can never masquerade as "this build is too
/// old" by mentioning the phrase.
const UNKNOWN_METHOD_PREFIX: &str = "\"Unknown method:";

/// Classifies one `RpcError` into this module's two-way vocabulary. Pure —
/// `power_local` below is the only caller, but keeping it separate is what
/// makes the classification testable without a gateway.
fn classify(error: &RpcError) -> PowerError {
    let RpcError::Rejected(text) = error else {
        return PowerError::Failed(format!("{error:?}"));
    };
    if text.starts_with(UNKNOWN_METHOD_PREFIX) {
        return PowerError::Unsupported(text.clone());
    }
    match denial_code(text) {
        // "This machine will not do this, ever" — a stopped condition, not a
        // transient one.
        Some(CODE_NOT_APPLIANCE) | Some(CODE_NOT_LOCAL) | Some(CODE_INVALID_ACTION) => PowerError::Unsupported(text.clone()),
        // Rate limiting IS transient: the operator pressed it twice, and the
        // right message is "try again", not "this machine can't".
        Some(CODE_RATE_LIMITED) => PowerError::Failed(text.clone()),
        // An unrecognised rejection falls to the retryable side on purpose:
        // between "tell them to give up" and "tell them to try again", the
        // second is the one that cannot strand an operator in front of a
        // machine that would in fact have restarted.
        _ => PowerError::Failed(text.clone()),
    }
}

/// The `code` field of a structured refusal, if the payload is one.
fn denial_code(error_text: &str) -> Option<&'static str> {
    let value: Value = serde_json::from_str(error_text).ok()?;
    let code = value.get("code")?.as_str()?;
    // Mapped back onto this module's own consts rather than returned as a
    // borrowed slice, so the caller matches on values that exist in the
    // source rather than on ad-hoc literals.
    [CODE_NOT_APPLIANCE, CODE_NOT_LOCAL, CODE_INVALID_ACTION, CODE_RATE_LIMITED].into_iter().find(|known| *known == code)
}

/// Asks the gateway to reboot or shut the machine down. BLOCKING.
///
/// `Ok(())` means the request reached the gateway — either it answered, or
/// it went silent AFTER the frame was on the wire, which for this method is
/// the ordinary shape of success (a machine that is rebooting takes the
/// socket with it). The distinction that matters is the one
/// `PreAuthOutcome::SentButNoAnswer` draws: a failure BEFORE the write is
/// still a real `Err`, so a button that never sent anything can never report
/// success.
///
/// The trade-off that buys: a gateway that accepted the frame and then hung
/// without acting would also read as `Ok`. That is accepted deliberately —
/// the alternative is showing a red error over a screen that is about to go
/// black, which is the failure mode operators actually hit.
pub(crate) fn power_local(action: PowerAction) -> Result<(), PowerError> {
    match ws_rpc::call_once_pre_auth("device.power_local", json!({ "action": action.wire() })) {
        // Defence in depth against the 2026-08-23 appliance regression: a
        // gateway older than `power_local_result_frame` answers a
        // ran-but-failed shell-out (polkit `Access denied`) with `ok:true`
        // and `success:false` in the payload — believing that frame's `ok`
        // alone left the operator on "正在送出…" forever. An explicit
        // `success:false` is a failure no matter what the envelope says; an
        // absent/malformed `success` field stays success, because the happy
        // path's whole contract is "the machine is about to go down, don't
        // second-guess it".
        Ok(PreAuthOutcome::Answered(payload)) => {
            if answered_payload_is_failure(&payload) {
                return Err(PowerError::Failed("gateway ran the command but it failed".to_string()));
            }
            Ok(())
        }
        Ok(PreAuthOutcome::SentButNoAnswer) => Ok(()),
        Err(e) => Err(classify(&e)),
    }
}

/// `true` only for an explicit `"success": false` in an answered payload —
/// see `power_local`'s own comment for why absent/malformed stays success.
fn answered_payload_is_failure(payload: &Value) -> bool {
    payload.get("success").and_then(Value::as_bool) == Some(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_values_match_the_gateway_contract() {
        assert_eq!(PowerAction::Reboot.wire(), "reboot");
        assert_eq!(PowerAction::Shutdown.wire(), "shutdown");
    }

    /// The 2026-08-23 appliance regression, client side: an `ok:true` frame
    /// whose payload says `"success": false` (a pre-fix gateway laundering a
    /// polkit `Access denied` shell-out) must read as failure. Only the
    /// explicit boolean does — absent, null, or non-boolean stays success,
    /// per `power_local`'s own contract comment.
    #[test]
    fn an_answered_payload_with_success_false_reads_as_failure() {
        assert!(answered_payload_is_failure(&serde_json::json!({"success": false, "stderr": "Access denied"})));
        assert!(!answered_payload_is_failure(&serde_json::json!({"success": true})));
        assert!(!answered_payload_is_failure(&serde_json::json!({})));
        assert!(!answered_payload_is_failure(&serde_json::json!({"success": "false"})));
        assert!(!answered_payload_is_failure(&Value::Null));
    }

    /// The whole reason `Unsupported` exists as its own variant: an older
    /// gateway that has never heard of this method must produce a DIFFERENT
    /// operator-facing message from a machine that is merely unreachable,
    /// because retrying fixes one and not the other.
    #[test]
    fn an_unknown_method_rejection_classifies_as_unsupported() {
        let err = RpcError::Rejected("\"Unknown method: device.power_local\"".to_string());
        assert!(matches!(classify(&err), PowerError::Unsupported(_)), "{:?}", classify(&err));
    }

    /// Anchored, not `contains` — a rejection whose text merely MENTIONS the
    /// phrase (e.g. a validation message quoting it back) is a plain
    /// failure, not evidence the gateway is too old.
    #[test]
    fn a_rejection_that_only_mentions_the_phrase_is_not_unsupported() {
        let err = RpcError::Rejected("\"action must be one of reboot|shutdown (not Unknown method:)\"".to_string());
        assert!(matches!(classify(&err), PowerError::Failed(_)));
    }

    /// The gateway's three permanent denials, read out of the structured
    /// `error` object's `code` field.
    #[test]
    fn the_gateways_permanent_denials_classify_as_unsupported() {
        for code in [CODE_NOT_APPLIANCE, CODE_NOT_LOCAL, CODE_INVALID_ACTION] {
            let err = RpcError::Rejected(format!(r#"{{"code":"{code}","message":"某段使用者視角中文"}}"#));
            assert!(matches!(classify(&err), PowerError::Unsupported(_)), "{code} should be Unsupported");
        }
    }

    /// …and the one transient denial, which must NOT tell the operator to
    /// give up: they pressed the button twice, that's all.
    #[test]
    fn rate_limiting_classifies_as_retryable_not_unsupported() {
        let err = RpcError::Rejected(format!(r#"{{"code":"{CODE_RATE_LIMITED}","message":"剛剛已經送出電源指令，請稍候再試一次。"}}"#));
        assert!(matches!(classify(&err), PowerError::Failed(_)));
    }

    /// The refusal object's Chinese `message` must never decide the branch —
    /// only `code` does. A message that happens to contain another code's
    /// name changes nothing.
    #[test]
    fn only_the_code_field_decides_the_branch_never_the_message_prose() {
        let err = RpcError::Rejected(format!(r#"{{"code":"{CODE_RATE_LIMITED}","message":"not_local not_appliance invalid_action"}}"#));
        assert!(matches!(classify(&err), PowerError::Failed(_)), "prose must not promote this to Unsupported");
    }

    /// An unrecognised rejection falls to the RETRYABLE side — see
    /// `classify`'s own comment on why that direction is the safe one.
    #[test]
    fn an_unrecognised_rejection_code_falls_back_to_retryable() {
        let err = RpcError::Rejected(r#"{"code":"something_new","message":"…"}"#.to_string());
        assert!(matches!(classify(&err), PowerError::Failed(_)));
    }

    #[test]
    fn every_transport_failure_classifies_as_failed() {
        for err in [
            RpcError::Unreachable("connection refused".to_string()),
            RpcError::Timeout,
            RpcError::AuthRejected,
            RpcError::RateLimited,
            RpcError::Malformed("not json".to_string()),
        ] {
            assert!(matches!(classify(&err), PowerError::Failed(_)), "{err:?} should be Failed");
        }
    }

    /// A gateway that never runs is the ordinary case on a dev machine —
    /// this must be an `Err`, never a panic and never a false success.
    #[test]
    fn a_dead_gateway_reports_failed_rather_than_panicking() {
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prev = std::env::var("DUDUCLAW_SHELL_GATEWAY_URL").ok();
        // SAFETY: test-only env mutation, serialized by ENV_LOCK above and
        // restored before this test returns — same discipline every other
        // env-touching test in this crate follows.
        unsafe { std::env::set_var("DUDUCLAW_SHELL_GATEWAY_URL", "http://127.0.0.1:1") };
        let result = power_local(PowerAction::Reboot);
        unsafe {
            match prev {
                Some(v) => std::env::set_var("DUDUCLAW_SHELL_GATEWAY_URL", v),
                None => std::env::remove_var("DUDUCLAW_SHELL_GATEWAY_URL"),
            }
        }
        assert!(matches!(result, Err(PowerError::Failed(_))), "{result:?}");
    }
}
