//! Unit tests for [`super`] (`network/iwd.rs`) — split into a sibling file
//! purely to keep `iwd.rs` inside this project's 800-line hard cap; included
//! via `#[path = "tests_iwd.rs"] mod tests;` in `iwd.rs`, which makes this a
//! literal CHILD module (`network::iwd::tests`), not an independent
//! sibling — `use super::*` below therefore reaches every private item in
//! `network/iwd.rs` exactly as it would if this were still inline. Same
//! split technique this crate already uses for `codrive/registry.rs` /
//! `codrive/tests_registry.rs`. Linux-only by inheritance: `iwd` itself is
//! only declared as `#[cfg(target_os = "linux")] mod iwd;` in `network/
//! mod.rs`, so this file is never even parsed off-Linux — no separate cfg
//! needed here.

use super::*;
use zbus::zvariant::Value;

fn owned(v: Value<'_>) -> OwnedValue {
    OwnedValue::try_from(v).expect("test fixture value must convert")
}

fn props_with(pairs: &[(&str, OwnedValue)]) -> HashMap<String, OwnedValue> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

// ── prop_string / prop_bool ────────────────────────────────────────

#[test]
fn prop_string_reads_known_key() {
    let props = props_with(&[("Name", owned(Value::from("DuDuClaw-Spike")))]);
    assert_eq!(
        prop_string(&props, "Name").as_deref(),
        Some("DuDuClaw-Spike")
    );
}

#[test]
fn prop_string_missing_key_is_none() {
    let props = props_with(&[]);
    assert_eq!(prop_string(&props, "Name"), None);
}

#[test]
fn prop_string_wrong_type_is_none_not_panic() {
    let props = props_with(&[("Name", owned(Value::from(42u32)))]);
    assert_eq!(prop_string(&props, "Name"), None);
}

#[test]
fn prop_bool_reads_known_key() {
    let props = props_with(&[("Connected", owned(Value::from(true)))]);
    assert_eq!(prop_bool(&props, "Connected"), Some(true));
}

#[test]
fn prop_bool_missing_or_wrong_type_is_none() {
    assert_eq!(prop_bool(&props_with(&[]), "Connected"), None);
    let props = props_with(&[("Connected", owned(Value::from("not-a-bool")))]);
    assert_eq!(prop_bool(&props, "Connected"), None);
}

// ── build_wifi_network ──────────────────────────────────────────────

#[test]
fn build_wifi_network_typical_open_entry() {
    let props = props_with(&[
        ("Name", owned(Value::from("Open-Net"))),
        ("Type", owned(Value::from("open"))),
        ("Connected", owned(Value::from(false))),
    ]);
    let objects = ManagedObjects::new();
    let net = build_wifi_network(&objects, &props, -5500).unwrap();
    assert_eq!(net.ssid, "Open-Net");
    assert_eq!(net.security, "open");
    assert!(!net.connected);
    assert!(!net.known, "no KnownNetwork property present");
    assert!(!net.hidden);
    assert_eq!(net.signal_bars, 3);
}

#[test]
fn build_wifi_network_known_and_hidden_via_known_network_reference() {
    let known_path = OwnedObjectPath::try_from("/net/connman/iwd/0/1/known_abc123").unwrap();
    let mut objects = ManagedObjects::new();
    let mut known_ifaces = HashMap::new();
    known_ifaces.insert(
        KNOWN_NETWORK_IFACE.to_string(),
        props_with(&[
            ("Name", owned(Value::from("HiddenNet"))),
            ("Hidden", owned(Value::from(true))),
        ]),
    );
    objects.insert(known_path.clone(), known_ifaces);

    let props = props_with(&[
        ("Name", owned(Value::from("HiddenNet"))),
        ("Type", owned(Value::from("psk"))),
        ("Connected", owned(Value::from(false))),
        (
            "KnownNetwork",
            owned(Value::from(
                ObjectPath::try_from(known_path.as_str()).unwrap(),
            )),
        ),
    ]);

    let net = build_wifi_network(&objects, &props, -4000).unwrap();
    assert!(net.known);
    assert!(
        net.hidden,
        "must follow the KnownNetwork reference to read Hidden"
    );
    assert_eq!(net.signal_bars, 4);
}

#[test]
fn build_wifi_network_known_without_resolvable_reference_defaults_hidden_false() {
    // KnownNetwork points somewhere not present in `objects` — must
    // degrade to `hidden = false`, never guess `true`.
    let props = props_with(&[
        ("Name", owned(Value::from("Net"))),
        ("Type", owned(Value::from("psk"))),
        (
            "KnownNetwork",
            owned(Value::from(
                ObjectPath::try_from("/net/connman/iwd/0/1/missing").unwrap(),
            )),
        ),
    ]);
    let objects = ManagedObjects::new();
    let net = build_wifi_network(&objects, &props, -4000).unwrap();
    assert!(net.known);
    assert!(!net.hidden);
}

#[test]
fn build_wifi_network_missing_name_is_none() {
    let props = props_with(&[("Type", owned(Value::from("psk")))]);
    let objects = ManagedObjects::new();
    assert!(build_wifi_network(&objects, &props, -4000).is_none());
}

// ── find_network_by_ssid / find_known_network_by_ssid: exact match ──

fn objects_with_network(ssid: &str, path: &str) -> ManagedObjects {
    let mut objects = ManagedObjects::new();
    let mut ifaces = HashMap::new();
    ifaces.insert(
        NETWORK_IFACE.to_string(),
        props_with(&[("Name", owned(Value::from(ssid)))]),
    );
    objects.insert(OwnedObjectPath::try_from(path).unwrap(), ifaces);
    objects
}

#[test]
fn find_network_by_ssid_exact_match_only() {
    let objects = objects_with_network("MyNetwork", "/net/connman/iwd/0/1/net1");
    assert!(find_network_by_ssid(&objects, "MyNetwork").is_some());
    // Substring / prefix must NOT match — coding convention 2.
    assert!(find_network_by_ssid(&objects, "MyNetwork2").is_none());
    assert!(find_network_by_ssid(&objects, "My").is_none());
    assert!(find_network_by_ssid(&objects, "").is_none());
}

// ── PassphraseGuard: panic-safety of the clear-on-drop discipline ───

#[test]
fn passphrase_guard_clears_the_slot_on_drop() {
    let slot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(Some("hunter2".to_string())));
    {
        let _guard = PassphraseGuard(Arc::clone(&slot));
        assert_eq!(slot.lock().unwrap().as_deref(), Some("hunter2"));
    }
    assert_eq!(
        *slot.lock().unwrap(),
        None,
        "the slot must be cleared once the guard drops"
    );
}

#[test]
fn passphrase_guard_clears_even_on_panic_unwind() {
    let slot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(Some("hunter2".to_string())));
    let slot_for_panic = Arc::clone(&slot);
    let result = std::panic::catch_unwind(move || {
        let _guard = PassphraseGuard(Arc::clone(&slot_for_panic));
        panic!("simulated failure between storing and using the passphrase");
    });
    assert!(result.is_err(), "sanity: the closure really did panic");
    assert_eq!(
        *slot.lock().unwrap(),
        None,
        "Drop must still run during unwind and clear the slot"
    );
}

// ── dbus_error_name ───────────────────────────────────────────────────

#[test]
fn dbus_error_name_non_method_error_is_empty() {
    let err = zbus::Error::InputOutput(std::sync::Arc::new(std::io::Error::other("boom")));
    assert_eq!(dbus_error_name(&err), "");
}
