//! A2 §3.1 read-only driving-state query — the gateway half of the
//! `codrive_status` MCP tool.
//!
//! One question, no side effects: "which seat is driving the shared
//! desktop right now, and is watch supervision armed?" It opens a
//! connection, authenticates, sends the existing `{"op":"status"}` op, and
//! closes. It sends no injection op, changes nothing, and writes no audit
//! row on success — a read, treated like one (A2 §4.1's `list_windows`
//! precedent on the shell socket).
//!
//! ## Authorization is NOT this module's job
//!
//! This function is the trusted executor; the trust boundary is the MCP
//! front door, exactly like [`super::driver::run_script`]'s. The caller
//! (`duduclaw-cli::mcp::handle_codrive_status`) must have already passed:
//! `Scope::Admin` (`mcp_auth::tool_requires_scope`), the deny-by-default
//! `[capabilities] codrive` dispatch gate (`mcp_dispatch::CODRIVE_TOOLS`),
//! and the in-handler re-check via
//! [`super::identity::resolve_run_identity`]. Do not call this from a path
//! that has not.
//!
//! ## The seat is exclusive, and this query queues behind it
//!
//! comp accepts co-drive connections one at a time (its `accept_loop` is
//! serialized), so while a `codrive_run` session holds the socket, this
//! query's connection sits in the kernel backlog: the connect itself
//! succeeds immediately, the auth line is written into the socket buffer,
//! and the auth ACK never comes until comp gets around to accepting.
//! That read is bounded by `[codrive] connect_timeout_secs`
//! ([`super::client::CodriveClient::connect`] wraps every read in it), so
//! this returns an honest timeout rather than hanging — and
//! `describe_error` spells out the most likely cause instead of leaving an
//! operator staring at "timed out".

use std::path::Path;
use std::time::Duration;

use serde::Serialize;

use super::client::{CodriveAck, CodriveClient, CodriveClientError, CodriveCmd};
use super::config::CodriveConfig;
use super::driver::resolve_endpoint;
use super::mode::{CodriveDrivingMode, CodriveHandoverReason};

/// The A2 §3.1 driving-state block, lifted out of a `status` ack. Every
/// field is optional because every field is optional on the wire — a comp
/// that predates A2 yields an all-`None` block, which is honest ("this
/// compositor does not report a driving mode") rather than a guess.
///
/// Lives here rather than beside [`CodriveAck`] because this query is its
/// only consumer, and `client.rs` is at this project's per-file size
/// convention.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CodriveDrivingState {
    pub mode: Option<CodriveDrivingMode>,
    pub handover_reason: Option<CodriveHandoverReason>,
    pub frozen: Option<bool>,
    pub terminated: Option<bool>,
    pub takeover: Option<bool>,
    pub shadow: Option<bool>,
    pub watch_active: Option<bool>,
    pub watch_paused: Option<bool>,
}

impl CodriveDrivingState {
    /// Project a `status` ack onto the driving-state block. Pure — it
    /// copies what comp reported and derives nothing (A2 §2: comp owns the
    /// only state machine, and this side never grows a second one).
    pub fn from_ack(ack: &CodriveAck) -> Self {
        Self {
            mode: ack.mode.clone(),
            handover_reason: ack.handover_reason.clone(),
            frozen: ack.frozen,
            terminated: ack.terminated,
            takeover: ack.takeover,
            shadow: ack.shadow,
            watch_active: ack.watch_active,
            watch_paused: ack.watch_paused,
        }
    }
}

/// `codrive_status`'s JSON response shape. Mirrors the A2 §4.1 shell-side
/// envelope (`{"ok":…, "codrive":{…}}`) so both human-side and agent-side
/// readers see the same block under the same key.
#[derive(Debug, Clone, Serialize)]
pub struct CodriveStatusReport {
    pub ok: bool,
    /// Operator-facing failure reason. `None` iff `ok`.
    pub error: Option<String>,
    /// The driving-state block comp reported. `None` iff `!ok`. Note that
    /// an all-`None` block inside a successful reply is itself meaningful:
    /// comp answered, but predates A2 and reports no mode.
    pub codrive: Option<CodriveDrivingState>,
}

impl CodriveStatusReport {
    fn ok(state: CodriveDrivingState) -> Self {
        Self {
            ok: true,
            error: None,
            codrive: Some(state),
        }
    }

    fn failed(error: String) -> Self {
        Self {
            ok: false,
            error: Some(error),
            codrive: None,
        }
    }
}

/// Ask comp for the current driving state. Never panics; every failure
/// (no endpoint configured, socket missing, auth rejected, timeout,
/// session terminated) comes back as an honest `ok:false` report — "空結果
/// 優於假結果", never a fabricated `human`/idle block.
pub async fn query_status(home_dir: &Path) -> CodriveStatusReport {
    let cfg = CodriveConfig::from_home(home_dir);
    let (socket_path, token) = match resolve_endpoint(&cfg).await {
        Ok(v) => v,
        Err(e) => return CodriveStatusReport::failed(e),
    };
    let timeout = Duration::from_secs(cfg.connect_timeout_secs.max(1));

    let mut client = match CodriveClient::connect(&socket_path, &token, timeout).await {
        Ok(c) => c,
        Err(e) => return CodriveStatusReport::failed(describe_error(&e, timeout)),
    };

    match client.send(&CodriveCmd::Status).await {
        Ok(ack) if ack.ok => CodriveStatusReport::ok(CodriveDrivingState::from_ack(&ack)),
        Ok(ack) => CodriveStatusReport::failed(format!(
            "共駕狀態查詢被拒：{}",
            ack.error.as_deref().unwrap_or("unknown_error")
        )),
        Err(e) => CodriveStatusReport::failed(describe_error(&e, timeout)),
    }
}

/// Turn a client error into something an operator can act on. The timeout
/// arm is the one that earns its keep: on this socket a timeout during the
/// handshake almost always means the single driving seat is already taken,
/// which "comp call timed out" alone does not convey.
fn describe_error(err: &CodriveClientError, timeout: Duration) -> String {
    match err {
        CodriveClientError::Timeout(_) => format!(
            "共駕狀態查詢逾時（{}秒）——共駕連線一次只接受一條，駕駛權可能正被另一個共駕 session 佔用；\
             也可能是 duduclaw-comp 沒有在回應。",
            timeout.as_secs()
        ),
        CodriveClientError::Connect { path, source } => {
            format!("無法連線到共駕 socket（{path}）：{source}")
        }
        CodriveClientError::Auth(e) => format!("共駕鑑別失敗：{e}"),
        CodriveClientError::Terminated => "共駕連線已結束（可能剛觸發急停）。".to_string(),
        other => format!("共駕狀態查詢失敗：{other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_message_names_the_exclusive_seat_as_the_likely_cause() {
        let msg = describe_error(
            &CodriveClientError::Timeout(Duration::from_secs(5)),
            Duration::from_secs(5),
        );
        assert!(msg.contains("駕駛權"), "msg: {msg}");
        assert!(msg.contains("5秒"), "msg: {msg}");
    }

    #[test]
    fn every_error_variant_produces_a_non_empty_operator_message() {
        let timeout = Duration::from_secs(3);
        for err in [
            CodriveClientError::Auth("auth_failed".into()),
            CodriveClientError::Terminated,
            CodriveClientError::Frozen,
            CodriveClientError::Decode("bad json".into()),
            CodriveClientError::Timeout(timeout),
        ] {
            let msg = describe_error(&err, timeout);
            assert!(!msg.trim().is_empty(), "empty message for {err:?}");
        }
    }

    #[test]
    fn driving_state_projection_copies_and_derives_nothing() {
        let ack: CodriveAck = serde_json::from_value(serde_json::json!({
            "ok": true, "frozen": true, "terminated": false, "takeover": true,
            "mode": "handover", "handover_reason": "agent_take_over",
            "shadow": false, "watch_active": true, "watch_paused": true
        }))
        .unwrap();
        let state = CodriveDrivingState::from_ack(&ack);
        assert_eq!(state.mode, Some(CodriveDrivingMode::Handover));
        assert_eq!(
            state.handover_reason,
            Some(CodriveHandoverReason::AgentTakeOver)
        );
        assert_eq!(state.frozen, Some(true));
        assert_eq!(state.terminated, Some(false));
        assert_eq!(state.takeover, Some(true));
        assert_eq!(state.shadow, Some(false));
        assert_eq!(state.watch_active, Some(true));
        assert_eq!(state.watch_paused, Some(true));
    }

    /// A pre-A2 ack projects to an all-`None` block — "this compositor does
    /// not report a driving mode", not a fabricated `human` default.
    #[test]
    fn driving_state_projection_of_a_pre_a2_ack_is_all_none() {
        let ack: CodriveAck =
            serde_json::from_value(serde_json::json!({"ok": true, "frozen": false})).unwrap();
        let state = CodriveDrivingState::from_ack(&ack);
        assert_eq!(state.mode, None);
        assert_eq!(state.handover_reason, None);
        assert_eq!(state.frozen, Some(false));
        assert_eq!(state.terminated, None);
    }

    /// No endpoint configured (no `[codrive] socket_path`, no
    /// `XDG_RUNTIME_DIR`) must be an honest refusal, never a fabricated
    /// "everything is human-driven" answer.
    #[tokio::test]
    async fn unconfigured_home_reports_failure_not_a_fabricated_state() {
        let home = std::env::temp_dir().join(format!(
            "codrive-status-unset-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&home).unwrap();
        // Point at a socket path that does not exist, so the answer is
        // deterministic regardless of the test host's XDG_RUNTIME_DIR.
        std::fs::write(
            home.join("config.toml"),
            "[codrive]\nsocket_path = \"/tmp/codrive-does-not-exist-1a2b3c.sock\"\nconnect_timeout_secs = 1\n",
        )
        .unwrap();

        let report = query_status(&home).await;
        assert!(!report.ok);
        assert!(report.codrive.is_none(), "must not invent a state block");
        assert!(report.error.is_some());
        let _ = std::fs::remove_dir_all(&home);
    }
}
