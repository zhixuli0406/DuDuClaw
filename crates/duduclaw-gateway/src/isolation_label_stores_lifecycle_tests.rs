//! F6h (design r5 §3.2 step 4, §3.3; H5, A11, S9) against a real confirmed
//! employee: the per-employee channel bot gate and the tombstone hook. A
//! child of the lifecycle test module, whose real-store fixture it reuses
//! (the root allocator is the `Root` fixture).
use super::*;
use crate::isolation_connector_details::{AccountDetailRecord, ChannelTokenRef, DetailReference};
use crate::isolation_connectors::{ConnectorCatalog, ConnectorKind};
use crate::isolation_label_stores as stores;

fn channel_detail(employee: EmployeeId, agent: &str, channel: &str, token: ChannelTokenRef) -> AccountDetailRecord {
    AccountDetailRecord { employee, agent: agent.to_owned(), reference: DetailReference::Channel {
        channel: channel.to_owned(), scope: "12345".into(), status_label: format!("{channel}:{agent}"), token, extra_id: None } }
}

/// H5 / A11: a protected employee's own bot starts only with a live
/// lifecycle AND an active `channel` catalog route whose C6 detail names
/// this employee, this label, this channel and the employee's own token.
#[test]
fn channel_bot_needs_live_lifecycle_and_route() {
    let f = fixture();
    let home = f.home();
    let (roots, held) = personal_employee(&f);
    assert!(!stores::bot_gate(home, LABEL, "telegram", &roots), "no catalog: no route");
    let catalog = ConnectorCatalog::open(home).unwrap();
    // Routes that never qualify: another employee id, another label.
    catalog.register_with_details("other id", "channel", ConnectorKind::Personal, &held.binding,
        &channel_detail(EmployeeId([9; 16]), LABEL, "telegram", ChannelTokenRef::Agent)).unwrap();
    catalog.register_with_details("other label", "channel", ConnectorKind::Personal, &held.binding,
        &channel_detail(held.employee, "alice-other", "telegram", ChannelTokenRef::Agent)).unwrap();
    assert!(!stores::bot_gate(home, LABEL, "telegram", &roots));
    let route = catalog.register_with_details("own bot", "channel", ConnectorKind::Personal, &held.binding,
        &channel_detail(held.employee, LABEL, "telegram", ChannelTokenRef::Agent)).unwrap();
    assert!(stores::bot_gate(home, LABEL, "telegram", &roots));
    assert!(!stores::bot_gate(home, LABEL, "discord", &roots), "exact channel only");
    // A stale lifecycle (root readback disagrees) refuses.
    let mut stale = roots.0[0].clone();
    stale.uid += 1;
    assert!(!stores::bot_gate(home, LABEL, "telegram", &Root(vec![stale])));
    // A revoked route refuses.
    catalog.revoke(&route).unwrap();
    assert!(!stores::bot_gate(home, LABEL, "telegram", &roots));
    catalog.register_with_details("own bot again", "channel", ConnectorKind::Personal, &held.binding,
        &channel_detail(held.employee, LABEL, "telegram", ChannelTokenRef::Agent)).unwrap();
    assert!(stores::bot_gate(home, LABEL, "telegram", &roots));
    // Tombstoned (row disabled, quarantine row open): refused although the route is active.
    drop(catalog);
    tombstone_personal_employee(home, LABEL, held.employee).unwrap();
    assert!(!stores::bot_gate(home, LABEL, "telegram", &roots));
}

/// §3.2 step 4 (S9) through the real tombstone: routes to the label are
/// stripped into the quarantine row, other labels keep theirs, and every
/// writer of the label is refused afterwards (protected mode).
#[test]
fn tombstone_strips_global_routes_and_gates_writers() {
    let f = fixture();
    let home = f.home();
    let (_, held) = personal_employee(&f);
    let path = home.join("agent_bindings.json");
    std::fs::write(&path, serde_json::json!({"bindings": {"telegram": {"u1": LABEL, "u2": "bob-desk"}}}).to_string()).unwrap();
    set_mode(&path, 0o600);
    tombstone_personal_employee(home, LABEL, held.employee).unwrap();
    let db = Connection::open(home.join(APPLY_DB)).unwrap();
    let row = crate::isolation_quarantine::read_quarantine(&db, held.employee, 0).unwrap().unwrap();
    assert_eq!(row.stripped["global_routes"]["channel_routes"], serde_json::json!(["binding:telegram:u1"]));
    assert!(row.stripped["deferred"].as_array().is_some_and(|items| !items.is_empty()), "unmodelled routes are listed");
    assert!(!row.pending_fences.iter().any(|fence| fence.starts_with(stores::STORE_FENCE_GROUP)), "{:?}", row.pending_fences);
    let marks: Option<String> = db.query_row("SELECT store_marks FROM employee_quarantine WHERE label=?1", [LABEL],
        |r| r.get(0)).unwrap();
    let marks: serde_json::Value = serde_json::from_str(&marks.unwrap()).unwrap();
    for store in ["memory", "sessions", "tasks", "cron"] { assert_eq!(marks[store], 0, "{store}"); }
    assert_eq!(stores::LABEL_KEYED_STORES.iter().find(|s| s.name == "channel_routes").unwrap().count_under(home, "bob-desk"), Ok(1));
    stores::TEST_PROTECTED.with(|cell| cell.set(Some(true)));
    let refused = stores::check_label_writable(home, LABEL);
    stores::TEST_PROTECTED.with(|cell| cell.set(None));
    assert_eq!(refused, Err(stores::GateRefusal::Quarantined));
    assert_eq!(stores::check_label_not_quarantined(home, LABEL, None), Err(stores::GateRefusal::Quarantined));
}
