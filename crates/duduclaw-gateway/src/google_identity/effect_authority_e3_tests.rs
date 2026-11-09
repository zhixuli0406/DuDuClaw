//! v1.81 E3: pinned Personal refresh (`access::resolve_pinned`, used by the
//! effect / remote-MCP authorities). The provider result is injected through
//! `pinned_test`; the agent has `fallback='home'` configured. Not compiled or
//! run here (Linux builder).
use super::*;
use crate::isolation_connector_events::{Target, e3_test_support::{company, personal, selection}};
use crate::isolation_connectors::{ConnectorCatalog, ConnectorKind};

fn provider_attempts(f: &Fixture) -> usize {
    access::pinned_test::events(f.home.path()).iter().filter(|&&e| e == "provider").count()
}
fn revision(f: &Fixture) -> u64 {
    vault::get(f.home.path(), &f.user).unwrap().unwrap().metadata.revision
}

#[cfg(unix)]
#[tokio::test]
async fn e3_pinned_invalid_grant_revokes_routes_without_fallback() {
    let f = Fixture::gmail(1);
    let catalog = ConnectorCatalog::open(f.home.path()).unwrap();
    let own = catalog.register("alice gmail", "google", ConnectorKind::Personal, &personal(&f.user)).unwrap();
    let home = catalog.register("home google", "google", ConnectorKind::SharedHome, &company()).unwrap();
    let target = Target::personal(&f.user, "google").unwrap();
    access::pinned_test::refresh_result(f.home.path(), Err(crate::google_personal::oauth::Error::InvalidGrant));

    let result = resolve_pinned(f.dedicated("gmail_read"), f.selected("gmail_read")).await;
    // The call fails; configured `fallback='home'` is never used.
    assert!(result.is_err_and(|e| e.code == "needs_reconnect" && !e.fallback_eligible));
    assert_eq!(provider_attempts(&f), 1, "one attempt, no retry with another account");
    // Catalog first: the owner's Personal google route is revoked; the home
    // route is not (a Personal consent revoke never touches SharedHome).
    assert!(!catalog.route(&own).unwrap().unwrap().active);
    assert!(catalog.route(&home).unwrap().unwrap().active);
    assert_eq!(catalog.pending_relinks(target.scope()).unwrap(), 1);
    // The pinned path never marks or reselects: the credential is unchanged.
    assert_eq!(revision(&f), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn e3_revoke_during_refresh_discards_new_token() {
    let f = Fixture::gmail(1);
    let catalog = ConnectorCatalog::open(f.home.path()).unwrap();
    let account = catalog.register("alice gmail", "google", ConnectorKind::Personal, &personal(&f.user)).unwrap();
    let selected = selection(f.home.path(), &account, &f.user, "gmail_search", false);
    access::pinned_test::refresh_result(
        f.home.path(),
        Ok(crate::google_personal::oauth::Tokens {
            secret: Secret {
                access_token: "fixture-fresh-never-output".into(),
                refresh_token: None,
                expires_at: Some(4_000_000_000),
            },
            granted_scopes: None,
        }),
    );
    // The route is revoked after the token exchange returned and before the
    // new token is stored (matrix 8: revoke during Google refresh).
    let home = f.home.path().to_path_buf();
    let revoked = account.clone();
    access::pinned_test::set_before_publish(
        f.home.path(),
        Box::new(move || ConnectorCatalog::open(&home).unwrap().revoke(&revoked).unwrap()),
    );
    let result = duduclaw_mcp_boundary::io_authority::scope(
        selected.io_context(),
        resolve_pinned(f.dedicated("gmail_read"), f.selected("gmail_read")),
    )
    .await;
    assert!(result.is_err_and(|e| !e.fallback_eligible), "uncertain/refused, never a fallback");
    // The new token was discarded: not published, not returned.
    assert_eq!(revision(&f), 1);
    let events = access::pinned_test::events(f.home.path());
    assert!(events.contains(&"provider") && !events.contains(&"refresh_published"), "{events:?}");
    assert!(selected.recheck().is_err());

    // Next page: refused before any provider attempt.
    let next = duduclaw_mcp_boundary::io_authority::scope(
        selected.io_context(),
        resolve_pinned(f.dedicated("gmail_read"), f.selected("gmail_read")),
    )
    .await;
    assert!(next.is_err());
    assert_eq!(provider_attempts(&f), 1);
    assert_eq!(revision(&f), 1);
}
