//! Wi-Fi adapter presence probing — backs [`crate::network::WifiErrorCode::
//! NoAdapter`] / [`crate::network::WifiErrorCode::DriverMissing`] (design
//! §5.3). Compiled on every target (not Linux-gated): off-Linux the `/sys`
//! reads simply find nothing, so [`detect_adapter_absence`] degrades to
//! [`crate::network::WifiErrorCode::NoAdapter`] rather than failing to
//! build — the parsing logic below is exercised by `cargo test` on every
//! host this crate compiles for, matching `device.rs`'s existing split of
//! "thin OS-facing collector feeding a pure, tested parser".
//!
//! iwd.rs calls [`detect_adapter_absence`] exactly once, only after it has
//! already confirmed (via `net.connman.iwd`'s `ObjectManager`) that no
//! `Station` object exists at all — i.e. only on the "no adapter" path, not
//! on every scan/connect/status call.

use crate::network::{WifiErrorCode, classify_adapter_absence};

/// Number of wireless PHYs the kernel currently exposes. Zero on a machine
/// with no Wi-Fi hardware (or off-Linux, where the directory doesn't exist).
pub fn phy_count() -> usize {
    std::fs::read_dir("/sys/class/ieee80211")
        .map(|entries| entries.filter_map(Result::ok).count())
        .unwrap_or(0)
}

/// Pure: does a raw `/sys/bus/pci/devices/*/class` value (e.g. `"0x028000\n"`
/// or `"0x020000\n"`) identify a Wi-Fi ("Network controller: Other")
/// adapter? PCI class codes are 6 hex digits — base class (2) + subclass (2)
/// + prog-if (2); network controllers are base class `0x02`, and Wi-Fi is
/// subclass `0x80` within that, so the wireless signature is the 4-hex-digit
/// prefix `0280` (Ethernet, by contrast, is `0200`).
///
/// Uses `str::get` rather than a raw byte-index slice so a garbage/short
/// value can never panic (coding convention: no raw byte-index string
/// slicing) — `get(..4)` returns `None` for a too-short string or one whose
/// 4th byte isn't a char boundary, both of which this function then treats
/// as "not wireless" rather than crashing on.
///
/// The `0x`/`0X` prefix strip is checked in both cases explicitly — a plain
/// `.strip_prefix("0x")` is case-SENSITIVE and would silently fail to strip
/// an uppercase `"0X0280AB"` value, leaving the stray `X` inside the
/// compared 4-char window and reporting a real wireless class as not
/// wireless (caught live by `wireless_class_is_case_insensitive` failing
/// against exactly this input before this fix).
pub fn pci_class_is_wireless(raw: &str) -> bool {
    let trimmed = raw.trim();
    let hex = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    match hex.get(..4) {
        Some(prefix) => prefix.eq_ignore_ascii_case("0280"),
        None => false,
    }
}

/// Is there at least one PCI device on this machine whose class identifies
/// it as a Wi-Fi adapter? Used to distinguish "no Wi-Fi hardware at all"
/// from "Wi-Fi hardware present but its driver/firmware never bound a phy"
/// (design §1.1's `driver_missing` scenario: the real hardware target for
/// this appliance ships with wireless kernel modules but zero firmware
/// packages, so the phy never appears even though the PCI device does).
///
/// PCI-only is a deliberate, honest scope limit: it does not probe USB Wi-Fi
/// dongles. Every appliance target this design doc names (N305 / 8845HS) is
/// PCI(e)-attached Wi-Fi, so this is not a gap for the M1 hardware — see
/// module-level "honest disclosure" in the D4a report for the explicit call
/// on USB not being covered.
pub fn wireless_pci_present() -> bool {
    let Ok(entries) = std::fs::read_dir("/sys/bus/pci/devices") else {
        return false;
    };
    entries.filter_map(Result::ok).any(|entry| {
        std::fs::read_to_string(entry.path().join("class"))
            .map(|raw| pci_class_is_wireless(&raw))
            .unwrap_or(false)
    })
}

/// Combine the two probes above into the closed [`WifiErrorCode`] the
/// design's error taxonomy wants — the only call site `iwd.rs` needs.
pub fn detect_adapter_absence() -> WifiErrorCode {
    classify_adapter_absence(phy_count(), wireless_pci_present())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── pci_class_is_wireless ─────────────────────────────────────────────

    #[test]
    fn wireless_class_with_0x_prefix() {
        assert!(pci_class_is_wireless("0x028000\n"));
    }

    #[test]
    fn wireless_class_without_prefix() {
        assert!(pci_class_is_wireless("028000"));
    }

    #[test]
    fn wireless_class_is_case_insensitive() {
        assert!(pci_class_is_wireless("0x0280AB"));
        assert!(pci_class_is_wireless("0X0280ab"));
    }

    #[test]
    fn ethernet_class_is_not_wireless() {
        assert!(!pci_class_is_wireless("0x020000\n"));
    }

    #[test]
    fn unrelated_class_is_not_wireless() {
        // 0x030000 — display controller, a real value seen on real machines.
        assert!(!pci_class_is_wireless("0x030000\n"));
    }

    #[test]
    fn garbage_input_is_not_wireless_never_panics() {
        assert!(!pci_class_is_wireless(""));
        assert!(!pci_class_is_wireless("x"));
        assert!(!pci_class_is_wireless("notahexvalue"));
        assert!(!pci_class_is_wireless("\n"));
        // Multi-byte content near the truncation boundary must not panic
        // (this is exactly the class of input `str::get` exists to guard).
        assert!(!pci_class_is_wireless("測試內容"));
    }

    #[test]
    fn whitespace_is_trimmed() {
        assert!(pci_class_is_wireless("  0x028000  \n"));
    }

    // ── phy_count / wireless_pci_present (collectors — smoke only) ───────

    #[test]
    fn phy_count_never_panics() {
        // No assertion on the value itself — this is a live `/sys` read and
        // its result is host-dependent (0 on this dev machine, off-Linux,
        // or a CI runner with no wireless hardware). The contract under
        // test is "never panics, never hangs".
        let _ = phy_count();
    }

    #[test]
    fn wireless_pci_present_never_panics() {
        let _ = wireless_pci_present();
    }

    #[test]
    fn detect_adapter_absence_never_panics_when_phy_absent() {
        // `detect_adapter_absence` -> `classify_adapter_absence` debug-
        // asserts `phy_count == 0` (it is only ever meant to be called on
        // the "no Station object" path — see this file's module doc). A
        // real dev/CI machine with actual Wi-Fi hardware has `phy_count() >
        // 0`, so calling it unconditionally would trip that assertion in a
        // debug/test build — this guard is what keeps the test honest about
        // its own precondition instead of getting lucky on a Wi-Fi-less
        // runner and panicking on the next one that has a real adapter.
        if phy_count() == 0 {
            let _ = detect_adapter_absence();
        }
    }
}
