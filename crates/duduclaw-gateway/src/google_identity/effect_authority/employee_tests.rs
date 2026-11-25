use super::*;
use crate::google_identity::owner_test_support::{Fixture, AGENT};
use crate::google_workspace::native_call::{NativeCallTemplate, testing};
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use serde_json::json;

#[tokio::test]
async fn revocation_before_send_or_after_headers_has_honest_outcome() {
    for (stage, expected_steps) in [("before_builder", 0), ("after_send", 1)] {
        let f = Fixture::new();
        let count = Arc::new(AtomicUsize::new(0));
        let served = count.clone();
        let app = axum::Router::new().fallback(move || {
            let served = served.clone();
            async move {
                served.fetch_add(1, Ordering::SeqCst);
                axum::Json(json!({"id":"message", "snippet":"never-return-stale-result"}))
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
        let _fixture = testing::install(f.home.path(), &format!("http://{address}/"));
        let args = json!({"message_id":"message"});
        let context = GoogleCallContext::employee(f.home.path(), AGENT,
            crate::knowledge::turn_requesters::lookup(AGENT, &f.turn).unwrap(),
            crate::google_identity::requester_gate::people_for(f.home.path(), Some(&f.db)),
            "gmail_read", &args, Some(f.turn.clone()), None).unwrap();
        let snapshot = f.snapshot();
        let db = f.db.clone();
        let admin = f.admin.clone();
        testing::hook(f.home.path(), stage, Box::new(move || {
            db.revoke_google_delegate_attestation(AGENT, &admin).unwrap();
        }));
        // Private fixture material models a completed mint; it cannot be
        // constructed by a route or returned to any caller.
        let material = Resolved { access: zeroize::Zeroizing::new("fixture-token".into()), subject:snapshot.email.clone() };
        let progress = AtomicUsize::new(0);
        let permit = Permit { context:&context, snapshot:&snapshot, progress:&progress };
        let template = NativeCallTemplate::prepare("gmail_read", &args).unwrap();
        let result = Bearer { material:&material, permit }.execute_native(&template).await;
        assert_eq!(progress.load(Ordering::SeqCst), expected_steps);
        assert_eq!(count.load(Ordering::SeqCst), expected_steps);
        assert!(testing::observations(f.home.path()).is_empty());
        match result {
            Err(ClosedNativeFailure::Denied(_)) if expected_steps == 0 => {},
            Err(ClosedNativeFailure::Unconfirmed) if expected_steps == 1 => {},
            _ => panic!("revoked calls must not deliver a result or claim not-sent after headers"),
        }
        server.abort();
    }
}
