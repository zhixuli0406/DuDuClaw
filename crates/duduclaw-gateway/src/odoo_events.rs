//! Odoo ERP change bridge — `duduclaw-odoo`'s event layer wired to the
//! autopilot broadcast bus (G4, 2026-09 feature audit).
//!
//! ## What this closes
//!
//! `duduclaw_odoo::events` has shipped `PollTracker::poll_model`,
//! `classify_event` and `parse_webhook` since the Odoo bridge landed, but
//! nothing in the gateway ever called them: there was no `/webhook/odoo`
//! route and no polling task, while the dashboard's Odoo page happily wrote
//! `poll_enabled` / `poll_models` / `webhook_enabled` into `config.toml`.
//! Operators could configure event synchronisation and get nothing. This
//! module is the missing half.
//!
//! ## Two transports, one event
//!
//! - **Polling** (`config.toml [odoo] poll_enabled`, default **off**): one
//!   background task per gateway. Every `poll_interval_seconds` it asks Odoo
//!   for records in `poll_models` whose `write_date` moved since the last
//!   cutoff, classifies each through [`duduclaw_odoo::events::classify_event`],
//!   de-duplicates, and publishes.
//! - **Webhook** (`[odoo] webhook_enabled`, default off): `POST /webhook/odoo`
//!   with a shared secret in the body. Fail-closed at every step — the route
//!   404s while disabled, 401s on a missing/wrong secret, and
//!   [`duduclaw_odoo::events::parse_webhook`] itself refuses an empty expected
//!   secret so "enabled but unconfigured" can never accept anything.
//!
//! Both converge on [`AutopilotEvent::OdooEvent`], so an autopilot rule reads
//! the same fields whichever transport delivered the change.
//!
//! ## Why the polling task uses the global `[odoo]` credentials
//!
//! Per-agent credentials live in `duduclaw_cli::odoo_pool::OdooConnectorPool`,
//! and `duduclaw-cli` depends on `duduclaw-gateway`, not the other way round —
//! the pool is unreachable from here. That is also the right answer on the
//! merits: `poll_models` / `poll_interval_seconds` are **global** config keys
//! describing one shared ERP feed, and the resulting event is fanned out to
//! agents by autopilot rules, not by whoever's credentials read it. Per-agent
//! credentials stay what they always were: the identity an agent's own MCP
//! tool calls authenticate as.
//!
//! ## Fail-safe posture
//!
//! Nothing here can take the gateway down. An unconfigured / unreachable Odoo
//! logs and retries on the next tick; a malformed webhook body is refused, not
//! panicked on; a full broadcast channel drops the event (the same back-
//! pressure every other producer on this bus has).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use duduclaw_odoo::{OdooConfig, OdooConnector};
use duduclaw_odoo::events::{OdooEvent, PollTracker, WebhookPayload, classify_event, dedup_events};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::autopilot_engine::AutopilotEvent;

/// Largest webhook body accepted. Odoo automated actions post a single record;
/// 256 KiB is generous and bounds the JSON parse.
const WEBHOOK_MAX_BYTES: usize = 256 * 1024;

/// Fields fetched per polled model. Deliberately a fixed, small set: these are
/// exactly the columns [`classify_event`] reads plus the ones
/// `format_proactive_notification` renders, so a poll never drags a whole
/// customer row through the bus.
const POLL_FIELDS: &[&str] = &[
    "id",
    "name",
    "create_date",
    "write_date",
    "state",
    "stage_id",
    "payment_state",
    "amount_total",
    "partner_id",
];

/// How long a `(event_type, model, record_id)` stays de-duplicated. The
/// poll cutoff is deliberately sampled *before* the query (see
/// `PollTracker::poll_model`), so the same record legitimately re-appears in
/// the next window; this window is what keeps that overlap from becoming a
/// duplicate event.
const DEDUP_HOURS: u32 = 1;

/// Floor on the polling interval, mirroring the dashboard's own clamp
/// (`odoo.configure` stores `poll_interval_seconds.clamp(60, 86400)`), so a
/// hand-edited `config.toml` cannot turn the poller into a hot loop.
const POLL_INTERVAL_MIN_SECS: u64 = 60;
const POLL_INTERVAL_MAX_SECS: u64 = 86_400;

/// Cap on models polled per tick, mirroring `odoo.configure`'s `take(50)`.
const POLL_MODELS_MAX: usize = 50;

// ── Config ──────────────────────────────────────────────────────

/// Read `config.toml [odoo]` from `home_dir`. Missing / malformed ⇒ default
/// (which is *not* configured, so every caller below no-ops).
pub fn load_odoo_config(home_dir: &Path) -> OdooConfig {
    let Ok(raw) = std::fs::read_to_string(home_dir.join("config.toml")) else {
        return OdooConfig::default();
    };
    let Ok(table) = raw.parse::<toml::Table>() else {
        return OdooConfig::default();
    };
    OdooConfig::from_toml(&table)
}

/// Resolve the Odoo credential (api key or password, per `auth_method`) the
/// same way `handlers::resolve_odoo_credential` does, but as a free function
/// over `home_dir` so the poll task needs no handler.
///
/// Returns `None` rather than the ciphertext when decryption fails — a
/// `secret://` reference or an undecryptable blob must never be sent to Odoo
/// as if it were the credential.
async fn resolve_credential(home_dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(home_dir.join("config.toml")).ok()?;
    let table = raw.parse::<toml::Table>().ok()?;
    let auth_method = table
        .get("odoo")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("auth_method"))
        .and_then(|v| v.as_str())
        .unwrap_or("api_key");
    let field_base = if auth_method == "password" {
        "password"
    } else {
        "api_key"
    };
    crate::config_crypto::decrypt_config_field_async(&table, "odoo", field_base, home_dir)
        .await
        .map(|s| s.expose().to_string())
        .filter(|s| !s.is_empty())
}

/// Resolve the webhook shared secret (`[odoo] webhook_secret_enc`, falling
/// back to the plaintext twin). Empty ⇒ the webhook must refuse everything.
async fn resolve_webhook_secret(home_dir: &Path) -> String {
    let Ok(raw) = std::fs::read_to_string(home_dir.join("config.toml")) else {
        return String::new();
    };
    let Ok(table) = raw.parse::<toml::Table>() else {
        return String::new();
    };
    crate::config_crypto::decrypt_config_field_async(&table, "odoo", "webhook_secret", home_dir)
        .await
        .map(|s| s.expose().to_string())
        .unwrap_or_default()
}

/// The models this gateway should poll, after validation and capping.
///
/// A model name that is not a plausible Odoo model (`a-z0-9._`) is dropped —
/// it would be interpolated into a JSON-RPC call, and a config file is not a
/// trusted input just because it is local.
pub fn effective_poll_models(cfg: &OdooConfig) -> Vec<String> {
    cfg.poll_models
        .iter()
        .filter(|m| is_valid_odoo_model(m))
        .take(POLL_MODELS_MAX)
        .cloned()
        .collect()
}

/// Mirror of `handlers::is_valid_odoo_model` (a private associated fn there).
fn is_valid_odoo_model(name: &str) -> bool {
    !name.is_empty()
        && name.len() < 100
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
}

/// Clamp the configured interval into the same range the dashboard enforces.
pub fn effective_poll_interval(cfg: &OdooConfig) -> Duration {
    Duration::from_secs(
        cfg.poll_interval_seconds
            .clamp(POLL_INTERVAL_MIN_SECS, POLL_INTERVAL_MAX_SECS),
    )
}

/// Is the background poller supposed to run for this config?
///
/// Three independent conditions, all fail-closed: the operator turned polling
/// on, Odoo is configured at all, and at least one valid model is listed.
pub fn polling_active(cfg: &OdooConfig) -> bool {
    cfg.poll_enabled && cfg.is_configured() && !effective_poll_models(cfg).is_empty()
}

// ── Bus publishing ──────────────────────────────────────────────

/// Convert an [`OdooEvent`] into the bus variant and publish it.
///
/// Returns `true` when the event reached at least one subscriber. A `send`
/// error means "no subscribers right now", which is normal during shutdown
/// and never an error worth failing a webhook over.
pub fn publish(tx: &broadcast::Sender<AutopilotEvent>, event: &OdooEvent) -> bool {
    tx.send(AutopilotEvent::OdooEvent {
        event_type: event.event_type.clone(),
        model: event.model.clone(),
        record_id: event.record_id,
        record: event.data.clone(),
    })
    .is_ok()
}

// ── Webhook route ───────────────────────────────────────────────

#[derive(Clone)]
struct OdooWebhookState {
    home_dir: PathBuf,
    tx: broadcast::Sender<AutopilotEvent>,
}

/// Mount `POST /webhook/odoo`.
///
/// Always mounted; the handler self-gates on `[odoo] webhook_enabled` and
/// 404s while off, so a stock install exposes nothing and leaks no evidence
/// the endpoint exists — the same posture `miniapp::router` uses.
pub fn router(home_dir: PathBuf, tx: broadcast::Sender<AutopilotEvent>) -> Router {
    Router::new()
        .route("/webhook/odoo", post(webhook_handler))
        .layer(DefaultBodyLimit::max(WEBHOOK_MAX_BYTES))
        .with_state(OdooWebhookState { home_dir, tx })
}

/// `config.toml [odoo] webhook_enabled`. Missing / unreadable ⇒ false.
pub fn webhook_enabled(home_dir: &Path) -> bool {
    load_odoo_config(home_dir).webhook_enabled
}

/// What [`decide_webhook`] made of one request. The axum handler is a thin
/// shell over this so every branch — including the two fail-closed ones — is
/// testable without a running server.
///
/// Deliberately no `PartialEq`: `OdooEvent` carries a free-form `data: Value`
/// and a wall-clock timestamp, so equality on the accepted variant would be
/// meaningless. Callers match on the variant.
#[derive(Debug)]
pub enum WebhookOutcome {
    /// `[odoo] webhook_enabled` is off ⇒ 404, no evidence the route exists.
    Disabled,
    /// Body is not a `WebhookPayload` ⇒ 400. The body is never echoed back:
    /// a webhook body can carry customer data.
    Malformed,
    /// Missing / wrong secret, **or an empty expected secret** ⇒ 401.
    Unauthorized(String),
    Accepted(Box<OdooEvent>),
}

/// The pure half of the webhook: config state + body in, decision out.
pub fn decide_webhook(enabled: bool, expected_secret: &str, body: &str) -> WebhookOutcome {
    if !enabled {
        return WebhookOutcome::Disabled;
    }
    let Ok(payload) = serde_json::from_str::<WebhookPayload>(body) else {
        return WebhookOutcome::Malformed;
    };
    // `parse_webhook` refuses an empty expected secret outright, so "enabled
    // but no secret configured" denies rather than accepting anything.
    match duduclaw_odoo::events::parse_webhook(&payload, expected_secret) {
        Ok(e) => WebhookOutcome::Accepted(Box::new(e)),
        Err(e) => WebhookOutcome::Unauthorized(e),
    }
}

async fn webhook_handler(State(state): State<OdooWebhookState>, body: String) -> Response {
    let enabled = webhook_enabled(&state.home_dir);
    // Resolving the secret is skipped entirely while disabled — no decryption
    // work, no keyfile touch, for a route that is 404 anyway.
    let expected = if enabled {
        resolve_webhook_secret(&state.home_dir).await
    } else {
        String::new()
    };
    match decide_webhook(enabled, &expected, &body) {
        WebhookOutcome::Disabled => StatusCode::NOT_FOUND.into_response(),
        WebhookOutcome::Malformed => (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({ "ok": false, "error": "malformed payload" })),
        )
            .into_response(),
        WebhookOutcome::Unauthorized(reason) => {
            warn!(reason = %reason, "odoo webhook rejected");
            (
                StatusCode::UNAUTHORIZED,
                axum::Json(json!({ "ok": false, "error": "unauthorized" })),
            )
                .into_response()
        }
        WebhookOutcome::Accepted(event) => {
            let delivered = publish(&state.tx, &event);
            info!(
                event_type = %event.event_type,
                model = %event.model,
                record_id = event.record_id,
                delivered,
                "odoo webhook accepted"
            );
            (
                StatusCode::OK,
                axum::Json(json!({ "ok": true, "delivered": delivered })),
            )
                .into_response()
        }
    }
}

// ── Polling task ────────────────────────────────────────────────

/// Spawn the background Odoo poll loop, or `None` when there is no Odoo
/// connection configured at all. Mirrors `tick_source::spawn_tick_sources`:
/// the caller pushes the handle onto the gateway's background-task list and
/// aborting it is what stops the loop.
///
/// The gate here is **`is_configured()`, not `poll_enabled`**, on purpose: the
/// loop re-reads `config.toml` every tick, so an operator who flips the
/// dashboard's polling switch on an already-connected Odoo gets polling within
/// one interval with no gateway restart. An install with no `[odoo]` section
/// spawns nothing.
///
/// Honest limitation: connecting Odoo *for the first time* on a running
/// gateway does still need a restart before the poller exists — there is no
/// task to notice the new section. The webhook half has no such caveat (its
/// route is always mounted and self-gates per request).
pub fn spawn_odoo_poller(
    home_dir: &Path,
    tx: broadcast::Sender<AutopilotEvent>,
) -> Option<JoinHandle<()>> {
    let cfg = load_odoo_config(home_dir);
    if !cfg.is_configured() {
        debug!("Odoo event polling not started ([odoo] has no url/db)");
        return None;
    }
    if polling_active(&cfg) {
        info!(
            models = effective_poll_models(&cfg).len(),
            interval_secs = effective_poll_interval(&cfg).as_secs(),
            "Odoo event polling started"
        );
    } else {
        info!(
            "Odoo connected but event polling is off ([odoo] poll_enabled) — \
             watching for the setting to change"
        );
    }
    let home = home_dir.to_path_buf();
    Some(tokio::spawn(async move {
        run_poll_loop(home, tx).await;
    }))
}

/// The poll loop. Never returns — aborting the task stops it.
async fn run_poll_loop(home_dir: PathBuf, tx: broadcast::Sender<AutopilotEvent>) {
    let mut tracker = PollTracker::new();
    let mut seen: HashMap<String, String> = HashMap::new();
    let mut connector: Option<Arc<OdooConnector>> = None;

    loop {
        // Re-read config every tick: the dashboard can flip `poll_enabled` or
        // edit `poll_models` without a gateway restart, and a poller that
        // cached its config at spawn would silently ignore both.
        let cfg = load_odoo_config(&home_dir);
        let interval = effective_poll_interval(&cfg);
        if !polling_active(&cfg) {
            // Turned off under us — drop the connection and idle. We keep the
            // task alive rather than exiting so turning it back on does not
            // need a restart either.
            connector = None;
            tokio::time::sleep(interval).await;
            continue;
        }

        let conn = match connector.clone() {
            Some(c) => c,
            None => match connect(&home_dir, &cfg).await {
                Some(c) => {
                    connector = Some(c.clone());
                    c
                }
                None => {
                    tokio::time::sleep(interval).await;
                    continue;
                }
            },
        };

        let mut events = Vec::new();
        let mut failed = false;
        for model in effective_poll_models(&cfg) {
            match tracker.poll_model(&conn, &model, POLL_FIELDS).await {
                Ok(records) => {
                    for record in &records {
                        if let Some(ev) = classify_event(&model, record, None) {
                            events.push(ev);
                        }
                    }
                }
                Err(e) => {
                    warn!(model = %model, error = %e, "Odoo poll failed");
                    failed = true;
                }
            }
        }
        if failed {
            // Force a reconnect on the next tick: an expired session is the
            // most common cause and a cached connector would keep failing.
            connector = None;
        }

        let fresh = dedup_events(&events, &mut seen, DEDUP_HOURS);
        for ev in &fresh {
            publish(&tx, ev);
        }
        if !fresh.is_empty() {
            info!(count = fresh.len(), "Odoo events published to autopilot bus");
        }

        tokio::time::sleep(interval).await;
    }
}

/// Build a connector from the global `[odoo]` block. `None` on any missing
/// credential or failed authentication — the caller retries next tick.
async fn connect(home_dir: &Path, cfg: &OdooConfig) -> Option<Arc<OdooConnector>> {
    let Some(credential) = resolve_credential(home_dir).await else {
        warn!("Odoo poll: no usable credential in [odoo] — skipping this tick");
        return None;
    };
    match OdooConnector::connect(cfg, &credential).await {
        Ok(c) => Some(Arc::new(c)),
        Err(e) => {
            warn!(error = %e, "Odoo poll: connection failed — retrying next tick");
            None
        }
    }
}

/// Classify a batch of polled records for one model. Extracted so the
/// poll → event step is testable without an Odoo server.
pub fn classify_batch(model: &str, records: &[Value]) -> Vec<OdooEvent> {
    records
        .iter()
        .filter_map(|r| classify_event(model, r, None))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with(poll_enabled: bool, models: &[&str]) -> OdooConfig {
        OdooConfig {
            url: "https://odoo.example.com".into(),
            db: "prod".into(),
            poll_enabled,
            poll_models: models.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn polling_is_off_by_default() {
        // Regression: `OdooConfig::default()` used to ship `poll_enabled =
        // true`, so wiring the poller would have started polling for every
        // operator who merely *configured* Odoo without asking for events.
        let cfg = OdooConfig::default();
        assert!(!cfg.poll_enabled, "unset poll_enabled must read as off");
        assert!(!polling_active(&cfg));
    }

    #[test]
    fn polling_requires_enabled_configured_and_a_model() {
        assert!(polling_active(&cfg_with(true, &["crm.lead"])));
        assert!(!polling_active(&cfg_with(false, &["crm.lead"])));
        assert!(!polling_active(&cfg_with(true, &[])));

        let mut unconfigured = cfg_with(true, &["crm.lead"]);
        unconfigured.url = String::new();
        assert!(!polling_active(&unconfigured));
    }

    #[test]
    fn invalid_model_names_are_dropped_not_sent_to_odoo() {
        let cfg = cfg_with(true, &["crm.lead", "bad model", "sale.order", "x'; DROP"]);
        assert_eq!(
            effective_poll_models(&cfg),
            vec!["crm.lead".to_string(), "sale.order".to_string()]
        );
    }

    #[test]
    fn poll_interval_is_clamped_like_the_dashboard() {
        let mut cfg = cfg_with(true, &["crm.lead"]);
        cfg.poll_interval_seconds = 1;
        assert_eq!(effective_poll_interval(&cfg).as_secs(), 60);
        cfg.poll_interval_seconds = 999_999;
        assert_eq!(effective_poll_interval(&cfg).as_secs(), 86_400);
        cfg.poll_interval_seconds = 300;
        assert_eq!(effective_poll_interval(&cfg).as_secs(), 300);
    }

    #[test]
    fn one_poll_round_classifies_and_publishes_events() {
        // A poll round's pure half: records in, bus events out. Proves the
        // wiring G4 added actually produces `AutopilotEvent::OdooEvent`.
        let records = vec![
            json!({
                "id": 7,
                "name": "New lead",
                "create_date": "2026-09-29 10:00:00",
                "write_date": "2026-09-29 10:00:00",
            }),
            json!({
                "id": 8,
                "name": "Moved lead",
                "create_date": "2026-09-01 10:00:00",
                "write_date": "2026-09-29 11:30:00",
            }),
        ];
        let events = classify_batch("crm.lead", &records);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type, "odoo.crm.lead_created");
        assert_eq!(events[1].event_type, "odoo.crm.stage_changed");

        let (tx, mut rx) = broadcast::channel(8);
        let mut seen = HashMap::new();
        let fresh = dedup_events(&events, &mut seen, DEDUP_HOURS);
        assert_eq!(fresh.len(), 2);
        for ev in &fresh {
            assert!(publish(&tx, ev));
        }
        let got = rx.try_recv().expect("first event on the bus");
        match got {
            AutopilotEvent::OdooEvent {
                event_type,
                model,
                record_id,
                ..
            } => {
                assert_eq!(event_type, "odoo.crm.lead_created");
                assert_eq!(model, "crm.lead");
                assert_eq!(record_id, 7);
            }
            other => panic!("expected OdooEvent, got {:?}", other.event_name()),
        }

        // Second round over the same records must publish nothing.
        let repeat = dedup_events(&events, &mut seen, DEDUP_HOURS);
        assert!(repeat.is_empty(), "dedup must suppress a re-seen record");
    }

    #[test]
    fn webhook_secret_mismatch_is_refused_and_empty_secret_refuses_everything() {
        let payload = WebhookPayload {
            event: "odoo.sale.order_confirmed".into(),
            model: Some("sale.order".into()),
            record_id: Some(42),
            data: None,
            secret: Some("wrong".into()),
        };
        assert!(duduclaw_odoo::events::parse_webhook(&payload, "right").is_err());
        assert!(
            duduclaw_odoo::events::parse_webhook(&payload, "").is_err(),
            "an unconfigured secret must refuse, never accept"
        );

        let ok = WebhookPayload {
            secret: Some("right".into()),
            ..payload
        };
        let event = duduclaw_odoo::events::parse_webhook(&ok, "right").expect("correct secret");
        assert_eq!(event.model, "sale.order");
        assert_eq!(event.record_id, 42);
    }

    #[test]
    fn config_round_trips_from_toml() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("config.toml"),
            r#"
[odoo]
url = "https://odoo.example.com"
db = "prod"
poll_enabled = true
poll_interval_seconds = 120
poll_models = ["crm.lead", "sale.order"]
webhook_enabled = true
"#,
        )
        .unwrap();
        let cfg = load_odoo_config(tmp.path());
        assert!(cfg.poll_enabled);
        assert!(cfg.webhook_enabled);
        assert_eq!(cfg.poll_interval_seconds, 120);
        assert_eq!(effective_poll_models(&cfg).len(), 2);
        assert!(polling_active(&cfg));
        assert!(webhook_enabled(tmp.path()));
    }

    #[test]
    fn missing_config_reads_as_fully_off() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = load_odoo_config(tmp.path());
        assert!(!polling_active(&cfg));
        assert!(!webhook_enabled(tmp.path()));

        // Malformed TOML must not be louder than missing: still off.
        std::fs::write(tmp.path().join("config.toml"), "[odoo\nbroken").unwrap();
        assert!(!polling_active(&load_odoo_config(tmp.path())));
        assert!(!webhook_enabled(tmp.path()));
    }

    const WEBHOOK_BODY: &str = r#"{"event":"odoo.invoice.overdue","model":"account.move","record_id":9,"data":{"partner_name":"Acme"},"secret":"s3cret"}"#;

    #[test]
    fn webhook_is_fail_closed_while_disabled_or_unconfigured() {
        // Disabled ⇒ 404-equivalent, and the secret is irrelevant.
        assert!(matches!(
            decide_webhook(false, "s3cret", WEBHOOK_BODY),
            WebhookOutcome::Disabled
        ));
        // Enabled but NO secret configured must refuse — this is the branch a
        // half-finished setup lands in, and accepting there would make the
        // endpoint an unauthenticated event injector.
        assert!(matches!(
            decide_webhook(true, "", WEBHOOK_BODY),
            WebhookOutcome::Unauthorized(_)
        ));
        // Wrong secret ⇒ refused.
        assert!(matches!(
            decide_webhook(true, "other", WEBHOOK_BODY),
            WebhookOutcome::Unauthorized(_)
        ));
        // Malformed body ⇒ refused without echoing it back.
        assert!(matches!(
            decide_webhook(true, "s3cret", "not json"),
            WebhookOutcome::Malformed
        ));
    }

    #[test]
    fn webhook_accepts_the_right_secret_and_puts_it_on_the_bus() {
        let WebhookOutcome::Accepted(event) = decide_webhook(true, "s3cret", WEBHOOK_BODY) else {
            panic!("correct secret must be accepted");
        };
        let (tx, mut rx) = broadcast::channel(8);
        assert!(publish(&tx, &event));

        match rx.try_recv().expect("event on the bus") {
            AutopilotEvent::OdooEvent {
                event_type,
                model,
                record_id,
                record,
            } => {
                assert_eq!(event_type, "odoo.invoice.overdue");
                assert_eq!(model, "account.move");
                assert_eq!(record_id, 9);
                assert_eq!(record["partner_name"], "Acme");
            }
            other => panic!("expected OdooEvent, got {:?}", other.event_name()),
        }
    }

    #[test]
    fn webhook_route_is_mountable() {
        // Cheap structural check: the router builds with the same state the
        // gateway hands it. (Behaviour is covered by `decide_webhook` above —
        // the handler is a thin shell over it.)
        let tmp = tempfile::tempdir().unwrap();
        let (tx, _rx) = broadcast::channel(8);
        let _app: Router = router(tmp.path().to_path_buf(), tx);
    }

    #[tokio::test]
    async fn no_poller_is_spawned_without_an_odoo_connection() {
        let tmp = tempfile::tempdir().unwrap();
        let (tx, _rx) = broadcast::channel(8);
        assert!(
            spawn_odoo_poller(tmp.path(), tx.clone()).is_none(),
            "no [odoo] section ⇒ no task at all"
        );

        // Connected but polling off: the loop IS spawned, so flipping the
        // dashboard switch later takes effect within one interval instead of
        // needing a gateway restart. It publishes nothing while off.
        std::fs::write(
            tmp.path().join("config.toml"),
            "[odoo]\nurl = \"https://o.example.com\"\ndb = \"p\"\npoll_enabled = false\n",
        )
        .unwrap();
        let handle = spawn_odoo_poller(tmp.path(), tx).expect("connected ⇒ loop is spawned");
        handle.abort();
    }
}
