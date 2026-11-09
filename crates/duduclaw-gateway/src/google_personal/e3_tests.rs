//! v1.81 E3: Personal Google refresh / consent-revoke recheck on the legacy
//! resolver (`access::resolve`), against the owned loopback provider of
//! `tests.rs` (`refresh_token = "invalid"` ⇒ `invalid_grant`). Not compiled or
//! run here (Linux builder).
use super::tests::Fixture;
use super::{access, lifecycle};
use crate::google_identity::requester_gate::UserDbPeople;
use crate::google_identity::resolve::{Resolved, resolve_person_for_tool};
use crate::isolation_connector_events::{Target, e3_test_support::{company, live_context, personal}};
use crate::isolation_connectors::{ConnectorCatalog, ConnectorKind};
use crate::person_credentials::{self as vault, Status};
use duduclaw_knowledge::requester::RequesterIdentity;

/// Make the stored access token expired so the next call must refresh with
/// `refresh` (the loopback answers `invalid_grant` for "invalid").
fn expire_with(f: &Fixture, refresh: &str) {
    let key = crate::google_identity::protection_gate(&f.home).unwrap();
    let old = vault::get(&f.home, &f.alice.id).unwrap().unwrap();
    let secret = vault::Secret {
        access_token: "expired".into(),
        refresh_token: Some(refresh.into()),
        expires_at: Some(0),
    };
    assert!(vault::finish_refresh(&f.home, &key, &old.metadata, &secret).unwrap());
}

#[cfg(unix)]
#[tokio::test]
async fn e3_personal_invalid_grant_revokes_routes() {
    let f = Fixture::new();
    f.connect().await;
    f.db.set_google_consent(&f.alice.id, "bot", true, &f.alice.id).unwrap();
    let owner = f.alice.id.as_str();
    let catalog = ConnectorCatalog::open(&f.home).unwrap();
    let google = catalog.register("alice gmail", "google", ConnectorKind::Personal, &personal(owner)).unwrap();
    let database = catalog.register("alice db", "database", ConnectorKind::Personal, &personal(owner)).unwrap();
    let other = catalog.register("bob gmail", "google", ConnectorKind::Personal, &personal("bob")).unwrap();
    let home_google = catalog.register("home google", "google", ConnectorKind::SharedHome, &company()).unwrap();
    let target = Target::personal(owner, "google").unwrap();

    // A dead grant of a SUPERSEDED credential (a reconnect published while
    // the refresh was in flight) never fences the routes of the new one.
    let mut stale = vault::get(&f.home, owner).unwrap().unwrap().metadata;
    stale.credential_id = uuid::Uuid::new_v4().to_string();
    lifecycle::consent_revoked(&f.home, &stale);
    assert!(catalog.route(&google).unwrap().unwrap().active);

    expire_with(&f, "invalid");
    let people = UserDbPeople(f.db.clone());
    let requester = RequesterIdentity::DashboardUser { user_id: f.alice.id.clone() };
    let effective = duduclaw_core::google_identity::effective_for(&f.home, Some("bot"));
    let refusal = access::resolve(&f.home, "bot", &requester, &people, &effective, "gmail_read")
        .await
        .err()
        .expect("a revoked consent fails the call");
    assert_eq!(refusal.code, "needs_reconnect");

    // Catalog: the owner's Personal google routes are revoked (relinkable by
    // reconnecting the same Google account); nothing else is touched.
    assert!(!catalog.route(&google).unwrap().unwrap().active);
    for untouched in [&database, &other, &home_google] {
        assert!(catalog.route(untouched).unwrap().unwrap().active);
    }
    assert!(catalog.active_in_scope(target.scope()).unwrap().is_empty());
    assert_eq!(catalog.pending_relinks(target.scope()).unwrap(), 1);
    // Material: marked for reconnect after the fence.
    assert_eq!(vault::get(&f.home, owner).unwrap().unwrap().metadata.status, Status::NeedsReconnect);
    let audit = std::fs::read_to_string(f.home.join("security_audit.jsonl")).unwrap_or_default();
    assert!(audit.contains("personal_google_consent_revoked"));
    for leak in ["sub-alice", "alice@c.test", "alice gmail", "refresh-", "access-"] {
        assert!(!audit.contains(leak), "{leak}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn e3_refresh_never_switches_account() {
    let f = Fixture::new();
    f.connect().await;
    f.db.set_google_consent(&f.alice.id, "bot", true, &f.alice.id).unwrap();
    // The administrator configured the home account as fallback.
    std::fs::write(
        f.home.join("agents/bot/agent.toml"),
        "[agent]\nname='bot'\n[google]\nidentity='owner'\nfallback='home'\n",
    )
    .unwrap();
    expire_with(&f, "invalid");
    let people = UserDbPeople(f.db.clone());
    let requester = RequesterIdentity::DashboardUser { user_id: f.alice.id.clone() };
    let isolated = live_context(&f.alice.id);

    // Inside an isolated request, the call that observes the revoked consent
    // fails; it never continues on the configured home account.
    let first = duduclaw_mcp_boundary::io_authority::scope(
        isolated.clone(),
        resolve_person_for_tool(&f.home, "bot", &requester, &people, "gmail_read"),
    )
    .await;
    match first {
        Err(refusal) => assert!(refusal.code == "needs_reconnect" && !refusal.fallback_eligible, "{refusal:?}"),
        Ok(Resolved::UseHome { .. }) => panic!("consent revoke must not switch to the home account"),
        Ok(_) => panic!("a revoked Personal grant must not resolve"),
    }
    // The next page (credential now NeedsReconnect) is refused as well.
    let next = duduclaw_mcp_boundary::io_authority::scope(
        isolated,
        resolve_person_for_tool(&f.home, "bot", &requester, &people, "gmail_read"),
    )
    .await;
    assert!(matches!(next, Err(ref refusal) if !refusal.fallback_eligible), "next page refused, no fallback");
    // Exactly one provider attempt was made for the dead grant: the status
    // gate refuses the second call before any refresh.
    assert_eq!(vault::get(&f.home, &f.alice.id).unwrap().unwrap().metadata.status, Status::NeedsReconnect);
}
