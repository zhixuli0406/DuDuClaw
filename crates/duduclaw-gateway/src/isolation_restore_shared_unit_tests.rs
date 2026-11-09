//! Pure parts of `isolation_restore_shared` (no stores): tick parsing, the
//! ticked route source, the gate's early exits, stable codes. The store-backed
//! tests (`shared_restore_*`) are in `isolation_restore_shared_tests.rs`.
use super::*;
use std::cell::RefCell;

fn offered() -> Vec<TenantRoute> {
    vec![
        TenantRoute { account: "acct-a".into(), generation: 3, provider: "google".into(), kind: ConnectorKind::SharedTenant },
        TenantRoute { account: "acct-b".into(), generation: 1, provider: "github".into(), kind: ConnectorKind::SharedHome },
    ]
}

#[test]
fn ticks_must_match_offered_route_and_generation_once() {
    let chosen = parse_ticks(&json!([{"account": "acct-b", "generation": 1}, {"account": "acct-a", "generation": 3}]), &offered())
        .unwrap();
    assert_eq!(chosen.iter().map(|r| r.account.as_str()).collect::<Vec<_>>(), ["acct-a", "acct-b"], "sorted");
    assert_eq!(parse_ticks(&json!([]), &offered()).unwrap(), Vec::new());
    for bad in [json!({}), json!([{"account": "acct-a"}]), json!([{"account": "acct-a", "generation": 3, "kind": "x"}]),
        json!([{"account": "", "generation": 3}]), json!([{"account": "acct-a", "generation": -1}]),
        json!([{"account": "acct-a", "generation": 3}, {"account": "acct-a", "generation": 3}])] {
        assert_eq!(parse_ticks(&bad, &offered()).err(), Some(SharedError::Invalid), "{bad}");
    }
    for stale in [json!([{"account": "acct-a", "generation": 2}]), json!([{"account": "ACCT-A", "generation": 3}])] {
        assert_eq!(parse_ticks(&stale, &offered()).err(), Some(SharedError::RouteNotOffered), "{stale}");
    }
}

struct Inner(RefCell<Vec<(String, u64)>>);
impl RouteSource for Inner {
    fn routes_for(&self, _: &EmployeeBinding, _: &str) -> Result<Vec<(String, u64)>, ()> { Ok(self.0.borrow().clone()) }
    fn detail_routes_naming(&self, _: EmployeeId) -> Result<Vec<String>, ()> { Ok(vec!["d".into()]) }
}

#[test]
fn ticked_source_sees_only_ticked_accounts() {
    let inner = Inner(RefCell::new(vec![("acct-a".into(), 3), ("acct-b".into(), 1), ("acct-c".into(), 9)]));
    let ticked = Ticked { inner: &inner, accounts: ["acct-a".to_owned()].into_iter().collect() };
    let binding = EmployeeBinding::Shared(Text::new("t".into()).unwrap());
    assert_eq!(ticked.routes_for(&binding, "t").unwrap(), vec![("acct-a".to_owned(), 3)]);
    // A re-generated ticked route changes the digest the engine compares.
    let before = engine::route_digest(&ticked.routes_for(&binding, "t").unwrap());
    inner.0.borrow_mut()[0].1 = 4;
    assert_ne!(engine::route_digest(&ticked.routes_for(&binding, "t").unwrap()), before);
    assert_eq!(ticked.detail_routes_naming(EmployeeId([1; 16])).unwrap(), vec!["d".to_owned()], "fences pass through");
}

#[test]
fn gate_exits_early_only_for_unrestricted_cases_and_fails_closed() {
    let missing = Path::new("/nonexistent/duduclaw-f6k-home");
    let shared = EmployeeBinding::Shared(Text::new("t".into()).unwrap());
    let personal = EmployeeBinding::Personal(Text::new("u".into()).unwrap());
    let employee = EmployeeId([7; 16]);
    // Never restored, Personal, or not a tenant-wide kind: no record needed.
    assert_eq!(tenant_route_gate(missing, employee, 0, &shared, "a", ConnectorKind::SharedTenant), Ok(()));
    assert_eq!(tenant_route_gate(missing, employee, 3, &personal, "a", ConnectorKind::Personal), Ok(()));
    assert_eq!(tenant_route_gate(missing, employee, 3, &shared, "a", ConnectorKind::Personal), Ok(()));
    // A restored Shared incarnation without a readable record refuses.
    assert_eq!(tenant_route_gate(missing, employee, 1, &shared, "a", ConnectorKind::SharedTenant), Err(VerificationError::Unavailable));
    assert_eq!(tenant_route_gate(missing, employee, 1, &shared, "a", ConnectorKind::SharedHome), Err(VerificationError::Unavailable));
}

#[test]
fn shared_error_codes_are_stable_and_distinct() {
    let all = [SharedError::MembersInvalid, SharedError::LevelRaised, SharedError::NoMembers, SharedError::MembersChanged,
        SharedError::RouteNotOffered, SharedError::Invalid];
    let codes: BTreeSet<&str> = all.iter().map(|e| e.code()).collect();
    assert_eq!(codes.len(), all.len());
    assert_eq!(SharedError::LevelRaised.code(), "ACCESS_LEVEL_RAISED");
    assert_eq!(SharedError::Restore(Refusal::TenantMismatch).code(), "TENANT_MISMATCH");
    assert!(all.iter().all(|e| !e.message().is_empty()));
    // The RPC error keeps engine refusals as engine refusals.
    use crate::isolation_restore_rpc::RpcError;
    assert_eq!(RpcError::from(SharedError::Restore(Refusal::Expired)), RpcError::Restore(Refusal::Expired));
    assert_eq!(RpcError::from(SharedError::MembersChanged).code(), "MEMBERS_CHANGED");
}
