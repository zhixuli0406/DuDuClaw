// D4b — the settings app's gateway bridge.
//
// Every page in `crate::settings` reads or writes real system state, and all
// of it lives behind the gateway's ADMIN WS RPC surface (`/ws`, the
// `require_admin!() + require_appliance!()` family). This module is the one
// place that knows how to get an authenticated call through, so no page ever
// has to think about sessions.
//
// ── Why a process-wide cached JWT, not one per page ────────────────────
// `gateway_client::bootstrap_local_session()` is an HTTP round trip that
// mints a session for the local operator. Seven pages each holding their own
// copy would mean seven bootstraps on the first visit to each, and seven
// independent staleness stories. There is exactly one operator sitting at
// exactly one machine, so there is exactly one session — kept in a
// `Mutex<Option<String>>` here. `overlay::notifications_feed` keeps its own
// (it predates this module and its state machine is tested around owning
// one); converging the two is a later cleanup, not this round's job, and
// two sessions for the same local operator is harmless — the gateway mints
// them independently.
//
// ── Why one retry, and only on AuthRejected ────────────────────────────
// A cached JWT expires. The honest recovery for "the gateway says this
// token is no longer good" is to mint a new one and try the SAME call once
// more; the honest recovery for anything else (unreachable, rate-limited,
// rejected on the merits) is to report it. Retrying a *rejected* call would
// double every write.
//
// ── Threading ──────────────────────────────────────────────────────────
// Everything here is PLAIN BLOCKING, same contract `gateway_client`'s own
// module doc states: callers run it from `std::thread::spawn` and bridge the
// result back through `settings::spawn_rpc`. Nothing here touches gpui.

use std::sync::Mutex;

use serde_json::Value;

use crate::gateway_client::{bootstrap_local_session, RpcError};

/// The one local operator session, shared by every settings page.
static SESSION: Mutex<Option<String>> = Mutex::new(None);

/// Every way a settings call can fail, in the vocabulary the PAGES need —
/// which is not the same as `RpcError`'s. A page cares about "is this
/// something the operator can act on" (`Rejected` carries a gateway code and
/// a zh-TW message written by the gateway) versus "the machinery is not
/// reachable" (everything else, which all render as one honest line).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SettingsRpcError {
    /// No local session could be minted at all — the gateway is down, or it
    /// refused to issue one (e.g. auto-login is off on this edition).
    NoSession(String),
    /// A session existed and the gateway still refused it, twice. Distinct
    /// from `NoSession` because the repair is different: this one means the
    /// account this shell logs in as is not an admin.
    NotAuthorized,
    Unreachable(String),
    RateLimited,
    Timeout,
    /// The gateway answered `ok:false`. `code` is its closed error code when
    /// it sent a structured error, empty when it sent a bare string.
    Rejected { code: String, message: String },
    Malformed(String),
}

impl SettingsRpcError {
    /// One zh-TW line for the operator. Deliberately does NOT include the
    /// technical detail for the transport variants — an operator standing at
    /// a duty box can act on "服務未啟動", not on a tungstenite `Display`
    /// string (which still reaches the journal via the `eprintln!` at the
    /// call site).
    pub(crate) fn user_message(&self) -> String {
        match self {
            SettingsRpcError::NoSession(_) => "無法連上本機服務，設定暫時無法讀取。".to_string(),
            SettingsRpcError::NotAuthorized => "目前登入的帳號沒有系統設定權限。".to_string(),
            SettingsRpcError::Unreachable(_) => "無法連上本機服務，設定暫時無法讀取。".to_string(),
            SettingsRpcError::RateLimited => "操作太頻繁，請稍候再試。".to_string(),
            SettingsRpcError::Timeout => "本機服務沒有在時限內回應。".to_string(),
            // The gateway writes these in zh-TW for exactly this purpose
            // (see `network_error_frame` / the `device.*` refusal frames),
            // so they are shown verbatim rather than re-worded here.
            SettingsRpcError::Rejected { message, .. } => message.clone(),
            SettingsRpcError::Malformed(_) => "本機服務回覆了無法解讀的內容。".to_string(),
        }
    }

    /// The gateway's own error code, when it sent a structured one. Pages
    /// branch on this for the few codes that change what they OFFER (e.g.
    /// `not_appliance` means the whole page is meaningless on this machine,
    /// which is a different screen from a failed call).
    pub(crate) fn code(&self) -> Option<&str> {
        match self {
            SettingsRpcError::Rejected { code, .. } if !code.is_empty() => Some(code),
            _ => None,
        }
    }

    /// True when this machine is not a DuDuClaw OS appliance at all — the
    /// gateway's `require_appliance!()` gate. Every `device.*` / `network.*`
    /// page renders a single "此功能僅在值班機上可用" line for it instead of
    /// an error, because nothing is broken; the feature simply does not
    /// apply here (the ordinary case on a dev Mac).
    pub(crate) fn is_not_appliance(&self) -> bool {
        self.code() == Some(NOT_APPLIANCE_CODE)
    }
}

/// The gateway's own code for `require_appliance!()`
/// (`duduclaw-gateway/src/handlers.rs::device_not_appliance_frame`). Named
/// rather than inlined so the one place this literal lives is greppable
/// against the gateway.
pub(crate) const NOT_APPLIANCE_CODE: &str = "not_appliance";

/// Turns the `error` payload of an `ok:false` response into a code/message
/// pair. `RpcError::Rejected` carries the raw JSON TEXT of that payload
/// (`ws_rpc` stringifies it rather than parsing, because it has no schema to
/// parse against) — which is either a JSON object `{"code","message"}` from
/// a structured refusal, or a JSON string from `WsFrame::error_response`.
/// Pure, so both shapes are covered by tests below rather than by hoping.
pub(crate) fn split_rejection(raw: &str) -> (String, String) {
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::Object(map)) => {
            let code = map.get("code").and_then(Value::as_str).unwrap_or_default().to_string();
            let message = map
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_string)
                // A structured error with no message is still an error; show
                // the code rather than an empty bubble.
                .unwrap_or_else(|| if code.is_empty() { raw.to_string() } else { code.clone() });
            (code, message)
        }
        Ok(Value::String(text)) => (String::new(), text),
        // Not JSON at all, or a JSON scalar we have no shape for — show it
        // verbatim rather than inventing a friendlier lie.
        _ => (String::new(), raw.to_string()),
    }
}

fn classify(e: RpcError) -> SettingsRpcError {
    match e {
        RpcError::Unreachable(s) => SettingsRpcError::Unreachable(s),
        RpcError::RateLimited => SettingsRpcError::RateLimited,
        RpcError::AuthRejected => SettingsRpcError::NotAuthorized,
        RpcError::Timeout => SettingsRpcError::Timeout,
        RpcError::Malformed(s) => SettingsRpcError::Malformed(s),
        RpcError::Rejected(raw) => {
            let (code, message) = split_rejection(&raw);
            SettingsRpcError::Rejected { code, message }
        }
    }
}

/// The cached JWT, minting one if there is none. Returns the token by value
/// so the lock is never held across the (blocking) RPC itself.
fn session_token() -> Result<String, SettingsRpcError> {
    // A poisoned lock means a previous holder panicked mid-mutation. The
    // only thing under it is an `Option<String>`, so the worst case is a
    // stale token — recoverable by treating it as absent, which is strictly
    // safer than propagating a panic into a settings page.
    let mut guard = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(jwt) = guard.as_ref() {
        return Ok(jwt.clone());
    }
    match bootstrap_local_session() {
        Ok(jwt) => {
            *guard = Some(jwt.clone());
            Ok(jwt)
        }
        Err(e) => Err(SettingsRpcError::NoSession(format!("{e:?}"))),
    }
}

fn forget_session() {
    let mut guard = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    *guard = None;
}

/// One authenticated admin RPC round trip. Blocking — see this module's
/// header comment for the threading contract.
///
/// On `AuthRejected` the cached session is dropped and the call is retried
/// exactly once with a freshly-minted one; a second refusal is reported as
/// `NotAuthorized` (the account genuinely lacks the role, and re-minting
/// forever would just hammer the gateway).
pub(crate) fn call(method: &str, params: Value) -> Result<Value, SettingsRpcError> {
    let jwt = session_token()?;
    match crate::gateway_client::call_settings_rpc(&jwt, method, params.clone()) {
        Ok(v) => Ok(v),
        Err(RpcError::AuthRejected) => {
            forget_session();
            let jwt = session_token()?;
            crate::gateway_client::call_settings_rpc(&jwt, method, params).map_err(|e| match e {
                RpcError::AuthRejected => SettingsRpcError::NotAuthorized,
                other => classify(other),
            })
        }
        Err(other) => Err(classify(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_structured_refusal_splits_into_code_and_message() {
        let (code, message) = split_rejection(r#"{"code":"not_appliance","message":"此功能僅限值班機。"}"#);
        assert_eq!(code, "not_appliance");
        assert_eq!(message, "此功能僅限值班機。");
    }

    #[test]
    fn a_bare_string_error_has_no_code_and_keeps_its_text() {
        let (code, message) = split_rejection(r#""permission denied""#);
        assert!(code.is_empty(), "a bare string carries no code to branch on");
        assert_eq!(message, "permission denied");
    }

    #[test]
    fn a_structured_refusal_without_a_message_falls_back_to_its_code() {
        let (code, message) = split_rejection(r#"{"code":"apply_failed"}"#);
        assert_eq!(code, "apply_failed");
        assert_eq!(message, "apply_failed", "an empty bubble would be worse than the code");
    }

    /// Never panic, never invent: an unparseable payload is shown verbatim.
    #[test]
    fn an_unparseable_error_payload_is_passed_through_not_swallowed() {
        let (code, message) = split_rejection("not json at all");
        assert!(code.is_empty());
        assert_eq!(message, "not json at all");
    }

    #[test]
    fn the_not_appliance_code_is_recognised_and_others_are_not() {
        let err = SettingsRpcError::Rejected { code: NOT_APPLIANCE_CODE.to_string(), message: "x".into() };
        assert!(err.is_not_appliance());
        let err = SettingsRpcError::Rejected { code: "apply_failed".to_string(), message: "x".into() };
        assert!(!err.is_not_appliance());
        assert!(!SettingsRpcError::Timeout.is_not_appliance(), "a transport failure says nothing about the machine");
    }

    /// The gateway writes its refusal copy in zh-TW for the operator; this
    /// layer must not re-word it (that would produce two different messages
    /// for the same condition depending on which side rendered it).
    #[test]
    fn a_gateway_rejection_shows_the_gateways_own_wording() {
        let err = SettingsRpcError::Rejected { code: "invalid_address".into(), message: "IP 位址格式不正確。".into() };
        assert_eq!(err.user_message(), "IP 位址格式不正確。");
    }

    /// Transport detail belongs in the journal, not on a settings card.
    #[test]
    fn transport_failures_do_not_leak_their_detail_into_the_operator_message() {
        let err = SettingsRpcError::Unreachable("tcp connect: ECONNREFUSED 127.0.0.1:18789".into());
        assert!(!err.user_message().contains("ECONNREFUSED"));
        assert!(!err.user_message().is_empty());
    }

    #[test]
    fn every_variant_has_a_non_empty_operator_message() {
        for err in [
            SettingsRpcError::NoSession("x".into()),
            SettingsRpcError::NotAuthorized,
            SettingsRpcError::Unreachable("x".into()),
            SettingsRpcError::RateLimited,
            SettingsRpcError::Timeout,
            SettingsRpcError::Rejected { code: "c".into(), message: "m".into() },
            SettingsRpcError::Malformed("x".into()),
        ] {
            assert!(!err.user_message().trim().is_empty(), "{err:?} has no operator-facing message");
        }
    }
}
