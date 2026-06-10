//! Machine fingerprint generation.
//!
//! Computes a deterministic SHA-256 hash of the machine's hostname
//! combined with the primary MAC address, truncated to 128 bits
//! and hex-encoded.

use sha2::{Digest, Sha256};

/// Generate a machine fingerprint string.
///
/// The fingerprint is a hex-encoded 128-bit truncation of
/// `SHA-256(hostname::mac_address)`. This provides a stable,
/// privacy-preserving machine identifier suitable for license binding.
///
/// The MAC address component prevents trivial spoofing by hostname
/// alone, providing a stronger hardware binding.
pub fn generate_fingerprint() -> String {
    let hostname = gethostname::gethostname().to_string_lossy().to_string();

    let mac = get_primary_mac().unwrap_or_else(|| "00:00:00:00:00:00".to_string());

    let combined = format!("{}::{}", hostname, mac);
    compute_fingerprint(&combined)
}

/// Compute fingerprint from a given input string.
/// Exposed for testability.
pub(crate) fn compute_fingerprint(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let hash = hasher.finalize();
    // Truncate to 128 bits (16 bytes) for a compact fingerprint
    hex::encode(&hash[..16])
}

/// Retrieve the primary (first non-loopback) MAC address.
///
/// Uses the `mac_address` crate for cross-platform detection.
/// Returns `None` if no MAC address can be determined.
fn get_primary_mac() -> Option<String> {
    mac_address::get_mac_address()
        .ok()
        .flatten()
        .map(|mac| mac.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_32_hex_chars() {
        let fp = generate_fingerprint();
        assert_eq!(fp.len(), 32, "fingerprint should be 32 hex characters");
        assert!(
            fp.chars().all(|c| c.is_ascii_hexdigit()),
            "fingerprint should contain only hex characters"
        );
    }

    #[test]
    fn fingerprint_is_deterministic() {
        let fp1 = compute_fingerprint("test-host::AA:BB:CC:DD:EE:FF");
        let fp2 = compute_fingerprint("test-host::AA:BB:CC:DD:EE:FF");
        assert_eq!(fp1, fp2);
    }

    #[test]
    fn different_inputs_produce_different_fingerprints() {
        let fp1 = compute_fingerprint("host-a::AA:BB:CC:DD:EE:FF");
        let fp2 = compute_fingerprint("host-b::AA:BB:CC:DD:EE:FF");
        assert_ne!(fp1, fp2);
    }

    #[test]
    fn different_macs_produce_different_fingerprints() {
        let fp1 = compute_fingerprint("same-host::AA:BB:CC:DD:EE:FF");
        let fp2 = compute_fingerprint("same-host::11:22:33:44:55:66");
        assert_ne!(fp1, fp2);
    }

    #[test]
    fn mac_address_included_in_fingerprint() {
        // Same hostname but different MAC should yield different fingerprints
        let fp_with_mac = compute_fingerprint("hello::AA:BB:CC:DD:EE:FF");
        let fp_hostname_only = compute_fingerprint("hello");
        assert_ne!(fp_with_mac, fp_hostname_only);
    }

    #[test]
    fn get_primary_mac_returns_some_on_real_machine() {
        // On any real machine with network interfaces, this should succeed.
        // In CI/containers, it may return None — that's acceptable.
        let mac = get_primary_mac();
        if let Some(ref addr) = mac {
            // MAC addresses are in format XX:XX:XX:XX:XX:XX (17 chars)
            assert_eq!(addr.len(), 17, "MAC address should be 17 characters");
        }
    }
}
