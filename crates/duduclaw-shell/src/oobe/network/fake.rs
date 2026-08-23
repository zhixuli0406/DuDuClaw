// Deterministic in-memory Wi-Fi backend — every platform, always compiled.
// See `mod.rs`'s header comment for the three situations that select this:
// Mac dev loop (no `nm` module compiled at all), `DUDUCLAW_SHELL_FAKE_NET=1`
// forced override, and Linux with NetworkManager's D-Bus unreachable.
//
// Data comes from `oobe::fake_data::FAKE_WIFI_NETWORKS` — the SAME
// canonical example list Shell-S1 shipped (see that module's own header
// comment), now reached through the `NetworkBackend` trait instead of read
// directly by `steps::network`. Shell-S1's original behavior ("選取→假連線
// 成功", no failure path at all) is deliberately NOT preserved verbatim:
// `connect()` now honors the PSK-length precondition a secured network
// needs, so the demo backend still exercises the real `AwaitingPsk` ->
// `Connecting` -> `Failed`/success UI states `steps::network` implements
// for the real backend — a fake backend that could never fail would leave
// two of those four states permanently unreachable in a dev build.

use super::{AccessPoint, ConnStatus, NetError, NetworkBackend};
use crate::oobe::fake_data::FAKE_WIFI_NETWORKS;

pub(crate) struct FakeNetworkBackend;

impl FakeNetworkBackend {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl NetworkBackend for FakeNetworkBackend {
    fn scan(&self) -> Result<Vec<AccessPoint>, NetError> {
        Ok(FAKE_WIFI_NETWORKS
            .iter()
            .map(|net| AccessPoint {
                ssid: net.ssid.to_string(),
                signal_bars: net.signal_bars,
                security: if net.secured { "psk" } else { "open" }.to_string(),
                // D4a-5: this demo backend never claims a saved credential.
                // `known: true` would make `steps::network`'s row click
                // skip the PSK prompt entirely (see `AccessPoint::known`'s
                // own doc comment) and go straight to `connect(ssid, None)`
                // — but `FakeNetworkBackend::connect` below STILL enforces
                // the 8..=63 PSK-length precondition for every `secured`
                // entry, so a `None` psk against a secured fake network
                // would just fail every time. Keeping `known: false` is
                // what keeps the demo build exercising the same
                // `AwaitingPsk` -> `Connecting` -> success/`Failed` states a
                // real unknown network goes through.
                known: false,
            })
            .collect())
    }

    fn connect(&self, ssid: &str, psk: Option<&str>) -> Result<(), NetError> {
        let Some(net) = FAKE_WIFI_NETWORKS.iter().find(|n| n.ssid == ssid) else {
            return Err(NetError::NotFound);
        };
        if net.secured {
            // Same 8-char floor `steps::network`'s own client-side PSK
            // gate already enforces before ever calling `connect()` — this
            // is a defense-in-depth re-check inside the backend, not the
            // primary gate (mirrors `oobe::claim::create_account_at`'s own
            // password-length re-check against the gateway's rule).
            let len = psk.map(|p| p.chars().count()).unwrap_or(0);
            if !(8..=63).contains(&len) {
                return Err(NetError::ConnectFailed("demo backend: psk length out of range".to_string()));
            }
        }
        Ok(())
    }

    fn status(&self) -> ConnStatus {
        // The fake backend keeps no connection state of its own — see this
        // struct's own header comment. `steps::network` derives "currently
        // connected" from `OobeSelections::network_ssid`/`network_connected`
        // (the real, PERSISTED source of truth) instead of consulting this
        // method for the demo backend, so always reporting `Disconnected`
        // here can never contradict what the screen actually shows.
        ConnStatus::Disconnected
    }

    fn forget(&self, ssid: &str) -> Result<(), NetError> {
        if FAKE_WIFI_NETWORKS.iter().any(|n| n.ssid == ssid) {
            Ok(())
        } else {
            Err(NetError::NotFound)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn scan_returns_every_fake_network_with_matching_fields() {
        let backend = FakeNetworkBackend::new();
        let aps = backend.scan().expect("fake scan never fails");
        assert_eq!(aps.len(), FAKE_WIFI_NETWORKS.len());
        for (ap, net) in aps.iter().zip(FAKE_WIFI_NETWORKS.iter()) {
            assert_eq!(ap.ssid, net.ssid);
            assert_eq!(ap.signal_bars, net.signal_bars);
            assert_eq!(ap.secured(), net.secured);
            assert!(!ap.known, "the demo backend never claims a saved credential — see this scan()'s own doc comment");
        }
    }

    #[test]
    fn connect_to_an_open_network_always_succeeds() {
        let backend = FakeNetworkBackend::new();
        let open = FAKE_WIFI_NETWORKS.iter().find(|n| !n.secured).expect("fixture must contain an open network");
        assert_eq!(backend.connect(open.ssid, None), Ok(()));
    }

    #[test]
    fn connect_to_a_secured_network_requires_a_long_enough_psk() {
        let backend = FakeNetworkBackend::new();
        let secured = FAKE_WIFI_NETWORKS.iter().find(|n| n.secured).expect("fixture must contain a secured network");
        assert!(backend.connect(secured.ssid, Some("short")).is_err());
        assert!(backend.connect(secured.ssid, None).is_err());
        assert_eq!(backend.connect(secured.ssid, Some("a-long-enough-passphrase")), Ok(()));
    }

    #[test]
    fn connect_to_an_unknown_ssid_is_not_found() {
        let backend = FakeNetworkBackend::new();
        assert_eq!(backend.connect("no-such-network", None), Err(NetError::NotFound));
    }

    #[test]
    fn status_is_always_disconnected() {
        let backend = FakeNetworkBackend::new();
        assert_eq!(backend.status(), ConnStatus::Disconnected);
    }

    #[test]
    fn forget_a_known_ssid_succeeds_an_unknown_one_is_not_found() {
        let backend = FakeNetworkBackend::new();
        let known = FAKE_WIFI_NETWORKS[0].ssid;
        assert_eq!(backend.forget(known), Ok(()));
        assert_eq!(backend.forget("no-such-network"), Err(NetError::NotFound));
    }

    #[test]
    fn fake_networks_have_no_duplicate_ssids_in_scan_output() {
        // Guards the assumption `steps::network`'s row rendering leans on
        // (one row per distinct SSID) — a regression in `fake_data.rs`
        // would otherwise only surface as a visual duplicate row, not a
        // test failure.
        let backend = FakeNetworkBackend::new();
        let aps = backend.scan().unwrap();
        let unique: HashSet<&str> = aps.iter().map(|ap| ap.ssid.as_str()).collect();
        assert_eq!(unique.len(), aps.len());
    }
}
