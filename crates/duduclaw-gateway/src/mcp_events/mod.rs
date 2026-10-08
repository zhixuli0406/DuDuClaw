//! MCP Events receiver (2026-10-08): an upstream MCP server's event wakes
//! work in DuDuClaw.
//!
//! ## What the draft says, and what was verified
//!
//! The source is the Triggers & Events working group's design sketch
//! `docs/design-sketch-proposal.md` in
//! `github.com/modelcontextprotocol/experimental-ext-triggers-events`
//! ("Status: Draft proposal", dated 2026-02-19), read on 2026-10-08. OpenAI's
//! page on ChatGPT's support (webhook delivery and callback verification
//! from "the draft MCP Events specification", protocol 2026-07-28) could not
//! be fetched from this environment; only search-result summaries of it were
//! seen. Implemented from the sketch, webhook mode only:
//!
//! - capability: the server's `initialize` result has `capabilities.events`;
//! - `events/subscribe { name, arguments, delivery: { mode: "webhook", url,
//!   secret }, cursor: null, ttlMs }` ⇒ `{ id, refreshBefore, cursor,
//!   truncated }`; refreshed by calling it again before `refreshBefore`;
//!   `events/unsubscribe { name, arguments, delivery: { url } }`;
//! - the secret is generated here (client side): `whsec_` + base64 of 32
//!   random bytes;
//! - deliveries are Standard Webhooks: `webhook-id`, `webhook-timestamp`
//!   (Unix seconds), `webhook-signature: v1,<base64 HMAC-SHA256(secret,
//!   id + "." + timestamp + "." + raw body)>` (several space-separated
//!   signatures accepted), plus `X-MCP-Subscription-Id`; deliveries older
//!   than 5 minutes are refused and `webhook-id` is de-duplicated;
//! - control bodies (a top-level `type`): `verification` (echo
//!   `{"challenge": …}` with a 2xx), `gap`, `terminated`;
//! - `410 Gone` stops retries of one delivery.
//!
//! Assumed / not implemented: poll and push modes, cursor persistence and
//! replay (subscriptions always start with `cursor: null`), `deliveryStatus`,
//! `maxAgeMs`, `notifications/events/list_changed`, the optional asymmetric
//! `v1a` server signature, subscription `arguments` (always `{}`; one
//! upstream subscription per event name). Nothing has been tried against a
//! real provider; tests use a loopback fake.
//!
//! ## Shape in DuDuClaw
//!
//! - [`store`]: `<home>/mcp_events/subscriptions.json` (0600, lock), the
//!   signing secret encrypted with the per-machine keyfile like
//!   `remote_mcp/store.rs`. Each local subscription has a random id
//!   (`mev_` + 32 hex) that is the path of its callback URL.
//! - [`signature`]: secret generation and constant-time verification.
//! - [`receiver`]: `POST /webhook/mcp-events/{id}` — unknown id ⇒ 404;
//!   256 KiB body cap; 120 deliveries per minute per subscription; replay
//!   cache; accepted events become `events.db` rows `mcp.event`
//!   (payload scanned by `input_guard`, data capped at 16 KiB, marked
//!   `suspicious`), which autopilot (`mcp_event` trigger) and
//!   responsibilities (event source `mcp.event`) read.
//! - [`upstream`]: the minimal MCP client the subscribe / refresh /
//!   unsubscribe calls use (remote servers connected through
//!   [`crate::remote_mcp`] only; a stdio server cannot reach a webhook).
//! - [`service`]: the four admin RPCs and the refresh sweep.
//!
//! Work started by an event runs in the read-only explore lane
//! ([`crate::explore_lane`]) unless the operator created the subscription
//! with `mode = "normal"`.

pub mod receiver;
pub mod service;
pub mod signature;
pub mod store;
pub mod upstream;

/// Audit event types.
pub const AUDIT_CREATED: &str = "mcp_event_subscription_created";
pub const AUDIT_ROTATED: &str = "mcp_event_subscription_rotated";
pub const AUDIT_REVOKED: &str = "mcp_event_subscription_revoked";
pub const AUDIT_REFRESH_FAILED: &str = "mcp_event_subscription_refresh_failed";
pub const AUDIT_DELIVERED: &str = "mcp_event_delivered";
pub const AUDIT_REJECTED: &str = "mcp_event_delivery_rejected";
pub const AUDIT_CONTROL: &str = "mcp_event_control";

/// Write one audit row (ids, names, counts; never secrets or payloads).
pub fn audit(home: &std::path::Path, event: &str, agent_id: &str, details: serde_json::Value) {
    let severity = if event == AUDIT_REJECTED || event == AUDIT_REFRESH_FAILED {
        duduclaw_security::audit::Severity::Warning
    } else {
        duduclaw_security::audit::Severity::Info
    };
    duduclaw_security::audit::append_audit_event(
        home,
        &duduclaw_security::audit::AuditEvent::new(event, agent_id, severity, details),
    );
}
