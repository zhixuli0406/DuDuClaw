//! v1.81 E3: Google home source refresh (`oauth_refresh_io`) against the owned
//! loopback `refresh_io::testing::Wire`. Not compiled or run here (Linux
//! builder).
use super::*;
use crate::isolation_connector_events::{Target, e3_test_support::{company, personal, selection}};
use crate::isolation_connectors::{ConnectorCatalog, ConnectorKind};

#[cfg(unix)]
#[tokio::test]
async fn e3_home_invalid_grant_revokes_shared_home() {
    let d = fixture(true, true);
    let catalog = ConnectorCatalog::open(d.path()).unwrap();
    let home_google = catalog.register("home google", "google", ConnectorKind::SharedHome, &company()).unwrap();
    let home_notion = catalog.register("home notion", "notion", ConnectorKind::SharedHome, &company()).unwrap();
    let alice = catalog.register("alice gmail", "google", ConnectorKind::Personal, &personal("alice")).unwrap();
    let target = Target::home("google").unwrap();
    let receipt = prepare_home_selection(d.path(), &[]).unwrap();
    let before = raw(d.path());
    let wire = refresh_io::testing::Wire::scenario(d.path(), "invalid_grant");

    assert!(resolve_home_oauth_pinned(d.path(), &receipt, &[]).await.is_err());
    assert_eq!(wire.requests(), 1, "one attempt, never retried");
    // Catalog: SharedHome google routes revoked (relinkable by a home
    // reconnect); other providers and Personal routes untouched.
    assert!(!catalog.route(&home_google).unwrap().unwrap().active);
    assert!(catalog.route(&home_notion).unwrap().unwrap().active);
    assert!(catalog.route(&alice).unwrap().unwrap().active);
    assert!(catalog.active_in_scope(target.scope()).unwrap().is_empty());
    assert_eq!(catalog.pending_relinks(target.scope()).unwrap(), 1);
    // Material: marked for reconnect after the fence, never deleted.
    assert_eq!(raw(d.path()), before);
    assert_eq!(crate::mcp_oauth::google_setup_status(d.path())["needs_reconnect"], true);
    let audit = std::fs::read_to_string(d.path().join("security_audit.jsonl")).unwrap_or_default();
    assert!(audit.contains("home_oauth_consent_revoked"));
    assert!(!audit.contains("legacy-refresh") && !audit.contains("secret-provider-description"));

    // A later home reconnect recreates the route under a NEW id.
    crate::mcp_oauth::publish_home_credential(d.path(), "google", || Ok::<_, String>(())).unwrap();
    let active = catalog.active_in_scope(target.scope()).unwrap();
    assert_eq!(active.len(), 1);
    assert_ne!(active[0].account, home_google);
}

#[cfg(unix)]
#[tokio::test]
async fn e3_revoke_during_home_refresh_discards_new_token() {
    let d = fixture(true, true);
    let catalog = ConnectorCatalog::open(d.path()).unwrap();
    let account = catalog.register("home google", "google", ConnectorKind::SharedHome, &company()).unwrap();
    // A Personal employee using the explicit home fallback route.
    let selected = selection(d.path(), &account, "alice", "gmail_search", true);
    let receipt = prepare_home_selection(d.path(), &[]).unwrap();
    let before = raw(d.path());
    // The route is revoked while Google is answering the refresh (matrix 8).
    let home = d.path().to_path_buf();
    let revoked = account.clone();
    let wire = refresh_io::testing::Wire::with_hook(d.path(), move || {
        ConnectorCatalog::open(&home).unwrap().revoke(&revoked).unwrap();
    });

    let result = duduclaw_mcp_boundary::io_authority::scope(
        selected.io_context(),
        resolve_home_oauth_pinned(d.path(), &receipt, &[]),
    )
    .await;
    assert!(result.is_err(), "uncertain/refused");
    assert_eq!(wire.requests(), 1);
    // The provider's new token was discarded: nothing was published.
    assert_eq!(raw(d.path()), before);
    assert!(selected.recheck().is_err());

    // Next page: refused before any provider attempt.
    let next = duduclaw_mcp_boundary::io_authority::scope(
        selected.io_context(),
        resolve_home_oauth_pinned(d.path(), &receipt, &[]),
    )
    .await;
    assert!(next.is_err());
    assert_eq!(wire.requests(), 1);
    assert_eq!(raw(d.path()), before);
}
