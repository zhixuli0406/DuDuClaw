// Gateway client — Shell-S4 (2026-08-22, WP-S4-notif): real gateway RPC for
// the Notifications overlay's approval cards. Three submodules, each a
// separate concern (this crate's established "many small files, low
// coupling" convention — see e.g. `Cargo.toml`'s own `serde`/`zbus`
// comments for the same reasoning applied elsewhere):
//
//   `session`   — `POST /api/session/local` (sync hand-rolled HTTP/1.1,
//                  mirrors `oobe::claim`'s own client — see that module's
//                  header comment for why no `reqwest`; this is a SEPARATE
//                  small client, not a shared one, deliberately — see
//                  `session.rs`'s own header comment).
//   `ws_rpc`    — one-shot round trip over `/ws` (the gateway has NO REST
//                  surface for `approvals.list`/`approvals.decide` — see
//                  that module's header comment for the dependency-tree
//                  finding that makes `tokio`/`tokio-tungstenite` free to
//                  use here, and `Cargo.toml`'s own comment for the same).
//   `approvals` — typed `ApprovalItem` + `list_approvals`/`decide_approval`,
//                  the only two RPCs this round needs.
//
// Every function in this module tree is a PLAIN BLOCKING call. Callers run
// it from a `std::thread::spawn` and bridge the result back to gpui via
// `std::sync::mpsc` + a `cx.spawn` poll loop — the exact split
// `oobe/claim.rs` (pure blocking client) / `oobe/steps/account.rs` (thread +
// channel + poll glue) already establish for the OOBE claim flow; see the
// latter's own header comment. Nothing in this module tree touches gpui,
// so it's usable — and tested — without a live window.

pub mod approvals;
pub mod session;
mod ws_rpc;

pub use approvals::{decide_approval, list_approvals, ApprovalItem};
pub use session::{bootstrap_local_session, SessionError};
pub use ws_rpc::RpcError;

/// Unifies every failure this module tree can produce — the shape
/// `overlay::notifications_feed`'s state machine actually stores, since its
/// UI collapses "couldn't bootstrap a session" and "the RPC itself failed"
/// to the same Offline presentation either way (see that module's own doc
/// comment). Individual callers that DO care about the distinction (e.g. to
/// decide whether to retry with a fresh session) can still match on
/// `SessionError`/`RpcError` directly before it gets here.
#[derive(Debug, Clone)]
pub enum GatewayError {
    Session(SessionError),
    Rpc(RpcError),
}

impl From<SessionError> for GatewayError {
    fn from(e: SessionError) -> Self {
        GatewayError::Session(e)
    }
}

impl From<RpcError> for GatewayError {
    fn from(e: RpcError) -> Self {
        GatewayError::Rpc(e)
    }
}

#[cfg(test)]
mod live_tests {
    use super::*;

    /// Live-fire pairing check against a REAL gateway with at least one real
    /// pending approval already seeded — `#[ignore]`d so `cargo test` never
    /// depends on one being up, same convention `oobe/claim.rs`'s own
    /// `live_first_run_claim_against_env_gateway` establishes. Exercises the
    /// FULL chain this module tree exists for: session bootstrap ->
    /// `approvals.list` -> `approvals.decide` -> `approvals.list` again to
    /// confirm the decided row is gone.
    ///
    /// Verification playbook (WP-S4-notif, 2026-08-22):
    ///   1. start a gateway with a FRESH (unclaimed, Personal-edition) home,
    ///      e.g. `DUDUCLAW_HOME=$(mktemp -d) DUDUCLAW_PORT=28793 duduclaw run`
    ///   2. seed at least one pending approval into that home's
    ///      `approvals.db` (e.g. via `ApprovalBroker::request` — this crate
    ///      has no CLI/MCP path of its own to create one, since approvals
    ///      are normally filed by an agent's tool call)
    ///   3. `DUDUCLAW_SHELL_GATEWAY_URL=http://127.0.0.1:28793 cargo test \
    ///      -p duduclaw-shell -- --ignored live_full_round_trip --nocapture`
    #[test]
    #[ignore = "requires a live gateway with at least one seeded pending approval — see doc comment"]
    fn live_full_round_trip_against_env_gateway() {
        let Some(base) = std::env::var("DUDUCLAW_SHELL_GATEWAY_URL").ok().filter(|v| !v.trim().is_empty()) else {
            eprintln!("[live] DUDUCLAW_SHELL_GATEWAY_URL not set — skipping");
            return;
        };

        let jwt = bootstrap_local_session().expect("session bootstrap must succeed against a fresh Personal-edition home");
        eprintln!("[live] bootstrapped session, jwt len={}", jwt.len());

        let before = list_approvals(&jwt).expect("approvals.list must succeed");
        eprintln!("[live] {} pending approval(s) before decide", before.len());
        let target = before.first().unwrap_or_else(|| panic!("expected at least one seeded pending approval at {base}, found none"));
        let target_id = target.id.clone();
        eprintln!("[live] deciding id={target_id} summary={:?}", target.summary);

        decide_approval(&jwt, &target_id, true).expect("approvals.decide must succeed");

        let after = list_approvals(&jwt).expect("approvals.list (post-decide) must succeed");
        assert!(!after.iter().any(|a| a.id == target_id), "decided approval must no longer be listed as pending");
        eprintln!("[live] confirmed: {target_id} no longer pending ({} remain)", after.len());
    }
}
