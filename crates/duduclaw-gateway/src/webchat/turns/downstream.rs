//! Single-use, bounded downstream offers. References never encode an identity.
use super::*;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

const OFFER_TTL: Duration = Duration::from_secs(600);
const MAX_OFFERS: usize = 1024;
const MAX_USER_OFFERS: usize = 16;

/// Use the same requester mapping and principal encoder as the reply pipeline.
pub(super) fn lineage_principal(home: &std::path::Path, user: &str, sid: &str) -> String {
    let requester = crate::knowledge::requester_map::map_requester(sid, user, Some(user), |_, _| None);
    crate::on_behalf_of::principal_for_requester(home, &requester)
        .unwrap_or_else(|| "lineage-unavailable".into())
}

/// Validate before credential I/O or spawning; the owned permit releases on panic.
pub(in crate::webchat) fn admit_stop(
    semaphore: &std::sync::Arc<tokio::sync::Semaphore>, reference: &str, request_id: Option<&str>,
) -> Result<tokio::sync::OwnedSemaphorePermit, serde_json::Value> {
    if uuid::Uuid::parse_str(reference).is_err() { return Err(unknown(request_id)); }
    semaphore.clone().try_acquire_owned().map_err(|_| serde_json::json!({
        "type":"error", "code":"downstream_stop_busy", "retryable":true, "request_id":request_id,
    }))
}

pub(super) struct Offer {
    reference: String,
    user: String,
    principal: String,
    sid: String,
    turn: String,
    expires: Instant,
}
#[derive(Default)]
pub(super) struct Offers(VecDeque<Offer>);
impl Registry {
    pub(super) fn offer(&self, user: &str, principal: &str, sid: &str, turn: &str) -> Option<String> {
        if is_widget_visitor(user) { return None; }
        let now = Instant::now();
        let mut offers = self.offers.lock().unwrap();
        offers.0.retain(|o| o.expires > now);
        while offers.0.iter().filter(|o| o.user == user).count() >= MAX_USER_OFFERS {
            let index = offers.0.iter().position(|o| o.user == user).unwrap();
            offers.0.remove(index);
        }
        while offers.0.len() >= MAX_OFFERS { offers.0.pop_front(); }
        let reference = uuid::Uuid::new_v4().to_string();
        offers.0.push_back(Offer { reference: reference.clone(), user: user.into(), principal: principal.into(),
            sid: sid.into(), turn: turn.into(), expires: now + OFFER_TTL });
        Some(reference)
    }
    fn consume_offer(&self, user: &str, principal: &str, reference: &str) -> Option<Offer> {
        if is_widget_visitor(user) { return None; }
        let mut offers = self.offers.lock().unwrap();
        offers.0.retain(|o| o.expires > Instant::now());
        let index = offers.0.iter().position(|o|
            o.reference == reference && o.user == user && o.principal == principal)?;
        offers.0.remove(index)
    }
}

pub(in crate::webchat) fn unknown(request_id: Option<&str>) -> serde_json::Value {
    serde_json::json!({"type":"error", "code":"downstream_ref_unknown",
        "message":"Downstream offer unavailable", "request_id":request_id})
}

pub(in crate::webchat) async fn stop_request(
    state: &WebChatState, credential: Option<&duduclaw_auth::jwt::Claims>, user: &str,
    reference: &str, request_id: Option<&str>,
) -> serde_json::Value {
    let validity = state.credential_validity(credential, true).await;
    handle_request(&state.turns, &state.ctx.home_dir, user, reference, request_id, validity).await
}

async fn handle_request(
    registry: &Registry, home: &std::path::Path, user: &str, reference: &str,
    request_id: Option<&str>, validity: crate::handlers::live_context::SocketValidity,
) -> serde_json::Value {
    use crate::handlers::live_context::SocketValidity;
    if is_widget_visitor(user) { return unknown(request_id); }
    let principal = format!("user:{user}");
    match validity {
        SocketValidity::Revoked => {
            if let Some(offer) = registry.consume_offer(user, &principal, reference) {
                crate::downstream_runs::stop(home, &offer.sid, &offer.turn, &offer.principal,
                    crate::downstream_runs::STOP_WAIT, "credential_revoked_offer").await;
            }
            return serde_json::json!({"type":"error", "code":"credential_revoked",
                "message":"Credential revoked", "request_id":request_id});
        },
        SocketValidity::Unavailable => return serde_json::json!({"type":"error", "code":"credential_check_unavailable",
            "message":"Credential check unavailable; retry later", "retryable":true, "request_id":request_id}),
        SocketValidity::Valid => {},
    }
    let Some(offer) = registry.consume_offer(user, &principal, reference) else { return unknown(request_id); };
    let result = crate::downstream_runs::stop(home, &offer.sid, &offer.turn, &offer.principal,
        crate::downstream_runs::STOP_WAIT, "person").await;
    serde_json::json!({"type":"downstream_stopped", "request_id":request_id,
        "stopped":result.stopped, "still_stopping":result.still_stopping})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::live_context::SocketValidity;
    #[tokio::test]
    async fn downstream_offers_are_single_use_private_expiring_and_bounded() {
        let registry = Registry::default();
        let home = tempfile::tempdir().unwrap();
        let first = registry.offer("alice", "user:alice", "s", "t").unwrap();
        let foreign = handle_request(&registry, home.path(), "bob", &first, None, SocketValidity::Valid).await;
        let absent = handle_request(&registry, home.path(), "alice", "unknown", None, SocketValidity::Valid).await;
        assert_eq!(foreign, absent);
        assert_eq!(handle_request(&registry, home.path(), "alice", &first, None, SocketValidity::Unavailable).await["retryable"], true);
        assert_eq!(handle_request(&registry, home.path(), "alice", &first, None, SocketValidity::Valid).await["type"], "downstream_stopped");
        assert_eq!(handle_request(&registry, home.path(), "alice", &first, None, SocketValidity::Valid).await, absent);
        let expired = registry.offer("alice", "user:alice", "s", "t").unwrap();
        registry.offers.lock().unwrap().0[0].expires = Instant::now();
        assert_eq!(handle_request(&registry, home.path(), "alice", &expired, None, SocketValidity::Valid).await, absent);
        for _ in 0..MAX_USER_OFFERS + 1 { registry.offer("alice", "user:alice", "s", "t"); }
        assert_eq!(registry.offers.lock().unwrap().0.len(), MAX_USER_OFFERS);
        for n in 0..MAX_OFFERS + 1 { registry.offer(&format!("u{n}"), "p", "s", "t"); }
        assert_eq!(registry.offers.lock().unwrap().0.len(), MAX_OFFERS);
        assert!(registry.offer("widget-visitor:test", "p", "s", "t").is_none());
        assert_eq!(handle_request(&registry, home.path(), "widget-visitor:test", "x", None, SocketValidity::Valid).await, absent);
    }
}

#[cfg(test)]
mod security_tests {
    use super::*;
    use crate::handlers::live_context::SocketValidity;

    #[tokio::test]
    async fn downstream_revoked_offer_stops_without_disclosing_reference_existence() {
        let home = tempfile::tempdir().unwrap();
        let registry = Registry::default();
        let offer = registry.offer("alice", "user:alice", "s", "t").unwrap();
        let guard = crate::downstream_runs::register(home.path(), Some("s"), Some("t"), Some("user:alice")).unwrap().unwrap();
        let worker = tokio::spawn(async move { guard.run(std::future::pending::<()>()).await });
        let known = handle_request(&registry, home.path(), "alice", &offer, Some("r"), SocketValidity::Revoked).await;
        let absent = handle_request(&registry, home.path(), "alice", &uuid::Uuid::new_v4().to_string(), Some("r"), SocketValidity::Revoked).await;
        assert_eq!(known, absent);
        assert_eq!(known["code"], "credential_revoked");
        worker.await.unwrap().unwrap_err();
        assert!(registry.consume_offer("alice", "user:alice", &offer).is_none());
        assert!(std::fs::read_to_string(home.path().join("tool_calls.jsonl")).unwrap().contains("credential_revoked_offer"));
    }

    #[tokio::test]
    async fn downstream_socket_admission_is_bounded_and_releases_after_panic() {
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        assert_eq!(admit_stop(&slots, "garbage", Some("r")).unwrap_err(), unknown(Some("r")));
        assert_eq!(slots.available_permits(), 1);
        let reference = uuid::Uuid::new_v4().to_string();
        let permit = admit_stop(&slots, &reference, Some("first")).unwrap();
        assert_eq!(admit_stop(&slots, &reference, Some("second")).unwrap_err(),
            serde_json::json!({"type":"error", "code":"downstream_stop_busy", "retryable":true,"request_id":"second"}));
        assert_eq!(admit_stop(&slots, "garbage", Some("r")).unwrap_err(), unknown(Some("r")));
        assert!(tokio::spawn(async move { let _permit = permit; panic!("fixture"); }).await.unwrap_err().is_panic());
        assert!(admit_stop(&slots, &reference, None).is_ok());
    }
}
