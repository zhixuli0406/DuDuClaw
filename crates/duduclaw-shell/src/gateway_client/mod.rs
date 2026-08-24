// Gateway client — Shell-S4 (2026-08-22, WP-S4-notif): real gateway RPC for
// the Notifications overlay's approval cards. Four submodules, each a
// separate concern (this crate's established "many small files, low
// coupling" convention — see e.g. `Cargo.toml`'s own `serde`/`zbus`
// comments for the same reasoning applied elsewhere):
//
//   `session`   — `POST /api/session/local` (sync hand-rolled HTTP/1.1,
//                  mirrors `oobe::claim`'s own client — see that module's
//                  header comment for why no `reqwest`; this is a SEPARATE
//                  small client, not a shared one, deliberately — see
//                  `session.rs`'s own header comment).
//   `login`     — `POST /api/login` (WP-lock-pw, 2026-08-22): the
//                  lockscreen's password-verify call — see `login.rs`'s own
//                  header comment for why it doesn't share code with
//                  `session`/`claim`'s similarly-shaped clients either.
//   `ws_rpc`    — one-shot round trip over `/ws` (the gateway has NO REST
//                  surface for `approvals.list`/`approvals.decide` — see
//                  that module's header comment for the dependency-tree
//                  finding that makes `tokio`/`tokio-tungstenite` free to
//                  use here, and `Cargo.toml`'s own comment for the same).
//   `approvals` — typed `ApprovalItem` + `list_approvals`/`decide_approval`,
//                  the only two RPCs this round needs.
//   `tasks`     — A1 result-loopback (2026-08-24): `AgentRef`/`CreatedGoal`/
//                  `TaskSnapshot` + `list_agents`/`pick_default_agent`/
//                  `create_goal`/`list_tasks`/`decide_goal_task` — the
//                  Launcher's 交辦 card and its own agent-scoped poll loop.
//   `task_progress` — A4 (2026-08-24): typed `TaskProgressItem` +
//                  `list_in_progress_tasks`, over the SAME `ws_rpc::
//                  call_once` — the dock badge / Notifications panel's
//                  "進行中任務" section. A SEPARATE module from `tasks`
//                  above on purpose — see `task_progress.rs`'s own header
//                  comment for why the two `tasks.list` callers don't share
//                  one function.
//
// Every function in this module tree is a PLAIN BLOCKING call. Callers run
// it from a `std::thread::spawn` and bridge the result back to gpui via
// `std::sync::mpsc` + a `cx.spawn` poll loop — the exact split
// `oobe/claim.rs` (pure blocking client) / `oobe/steps/account.rs` (thread +
// channel + poll glue) already establish for the OOBE claim flow; see the
// latter's own header comment. Nothing in this module tree touches gpui,
// so it's usable — and tested — without a live window.

pub mod approvals;
mod login;
/// ICON-3 (2026-08-23): `device.power_local`, the lockscreen's restart/
/// shut-down control — see that module's own header comment for the wire
/// contract and for why a pre-auth surface may call it at all.
mod power;
pub mod session;
/// A1 result-loopback (2026-08-24): agent listing + goal-task submit/poll/
/// decide for the Launcher's 交辦 card — see this file's own module-table
/// comment above.
pub mod tasks;
/// A4 (2026-08-24): `tasks.list(status="in_progress")` — the in-progress
/// task count/list the dock badge and Notifications panel's "進行中任務"
/// section this round need. See this file's own module-table comment above
/// for why it is not folded into `tasks` above.
pub mod task_progress;
mod ws_rpc;

pub use approvals::{decide_approval, list_approvals, ApprovalItem};
// `AgentRef`/`CreatedGoal` are only ever named through inference at their two
// call sites (`launcher.rs`'s `pick_default_agent(&agents)` /
// `create_goal(...)`'s return value, never spelled `gateway_client::AgentRef`
// literally) — same "kept for whichever future caller needs it by name"
// allowance `duduclaw-native-gui/src/rpc.rs::CallError::Rejected`'s own doc
// comment gives for the identical situation, not dead code to delete.
#[allow(unused_imports)]
pub use tasks::{create_goal, decide_goal_task, list_agents, list_tasks, pick_default_agent, AgentRef, CreatedGoal, TaskSnapshot};
pub use task_progress::{list_in_progress_tasks, TaskProgressItem};
pub(crate) use login::{verify_password, LoginError};
pub(crate) use power::{power_local, PowerAction, PowerError};
pub use session::{bootstrap_local_session, SessionError};
pub use ws_rpc::RpcError;
/// D4b (2026-08-23): `crate::settings::client` drives the whole `device.*` /
/// `network.*` / `users.*` admin surface through the SAME one-shot round trip
/// the approvals feed uses. Re-exported under a name that says what it is at
/// the call site rather than widening `ws_rpc`'s own visibility — there is
/// still exactly one WS client in this crate, and this is it.
pub(crate) use ws_rpc::call_once as call_settings_rpc;

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
