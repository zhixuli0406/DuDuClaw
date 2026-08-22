// Typed wrappers over `ws_rpc::call_once` for the two RPCs the Notifications
// overlay needs this round — `approvals.list` / `approvals.decide`
// (`duduclaw-gateway/src/handlers.rs::handle_approvals_list` /
// `::handle_approvals_decide`, read-only reference, not modified from here).
// Field names and JSON shape are copied from `handle_approvals_list`'s own
// response-building code, not guessed.

use serde::Deserialize;
use serde_json::json;

use super::ws_rpc::{self, RpcError};

/// One pending approval row, as the Notifications overlay's approval card
/// renders it. A SUBSET of `handle_approvals_list`'s full response shape —
/// only the fields this round's card actually uses. `simulation` mirrors
/// `crate::approval::SimulationNarrative`'s JSON form
/// (`{"world_state_change": "...", "risk_points": [...]}`) when the
/// ActionGuard judge ran for this approval; `null` for the overwhelming
/// majority of kinds (per that handler's own doc comment), hence `Option`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ApprovalItem {
    pub id: String,
    pub agent_id: String,
    pub kind: String,
    pub summary: String,
    pub created_at: String,
    #[serde(default)]
    pub simulation: Option<serde_json::Value>,
}

/// Lists every currently-pending approval (optionally scoped by
/// `agent_id` — `None` here means "every agent", matching
/// `handle_approvals_list`'s own `agent_id` param being optional). Blocking;
/// callers run this from a `std::thread::spawn`, same convention this whole
/// module tree follows (see `gateway_client`'s own header comment).
pub fn list_approvals(jwt: &str) -> Result<Vec<ApprovalItem>, RpcError> {
    let payload = ws_rpc::call_once(jwt, "approvals.list", json!({}))?;
    let items = payload.get("approvals").cloned().unwrap_or(serde_json::Value::Array(Vec::new()));
    serde_json::from_value(items).map_err(|e| RpcError::Malformed(format!("approvals.list payload did not match the expected shape: {e}")))
}

/// Approves or rejects one pending approval by id. `Ok(())` means the
/// gateway's `approvals.decide` RPC itself succeeded (`ok:true`) — this
/// module does not inspect the RPC's own response body beyond that, since
/// the caller (`overlay::notifications_feed`) only needs the pass/fail
/// outcome to settle the card's local state; the gateway's own audit trail
/// is the durable record.
pub fn decide_approval(jwt: &str, id: &str, approve: bool) -> Result<(), RpcError> {
    ws_rpc::call_once(jwt, "approvals.decide", json!({ "id": id, "approve": approve }))?;
    Ok(())
}
