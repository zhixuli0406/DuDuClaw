//! Runtime feature gating based on license tier.
//!
//! Reads `~/.duduclaw/license.key` and enforces tier-based limits on agents,
//! channels, and feature access. Defaults to `Community` when no license is
//! present, the file cannot be parsed, or the signature is invalid.

use ring::signature;
use serde::Serialize;
use serde_json::Value;
use std::path::PathBuf;
use tracing::warn;

/// License tier levels (ordered: Community < Pro < Enterprise < Oem).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    Community,
    Pro,
    Enterprise,
    Oem,
}

impl Tier {
    /// Parse a tier string (case-insensitive) into a `Tier` variant.
    fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "pro" => Tier::Pro,
            "enterprise" => Tier::Enterprise,
            "oem" => Tier::Oem,
            _ => Tier::Community,
        }
    }

    /// Display name for the tier.
    pub fn as_str(&self) -> &'static str {
        match self {
            Tier::Community => "Community",
            Tier::Pro => "Pro",
            Tier::Enterprise => "Enterprise",
            Tier::Oem => "OEM",
        }
    }
}

/// Runtime feature gate — reads license.key and enforces limits.
pub struct FeatureGate {
    tier: Tier,
}

impl FeatureGate {
    /// Load from `~/.duduclaw/license.key`, defaults to Community if missing
    /// or unparseable.
    pub fn load() -> Self {
        let tier = Self::read_tier_from_license();
        Self { tier }
    }

    /// Create a `FeatureGate` for a specific tier (useful for testing).
    pub fn with_tier(tier: Tier) -> Self {
        Self { tier }
    }

    /// Current tier.
    pub fn tier(&self) -> Tier {
        self.tier
    }

    /// Check if a feature is available at the current tier.
    ///
    /// Feature names correspond to keys in `features.toml`.
    pub fn check(&self, feature: &str) -> bool {
        let min_tier = Self::min_tier_for_feature(feature);
        self.tier >= min_tier
    }

    /// Max agents allowed (0 = unlimited).
    /// Open Core: No artificial limits on core features — always unlimited.
    pub fn max_agents(&self) -> usize {
        0 // unlimited for all tiers
    }

    /// Max channels allowed (0 = unlimited).
    /// Open Core: No artificial limits on core features — always unlimited.
    pub fn max_channels(&self) -> usize {
        0 // unlimited for all tiers
    }

    /// Generate a license suggestion message.
    pub fn upgrade_message(&self, feature: &str) -> String {
        let min_tier = Self::min_tier_for_feature(feature);
        format!(
            "Value-add service '{}' requires {} license (current: {}). \
             Visit https://duduclaw.dev/pricing for details.",
            feature,
            min_tier.as_str(),
            self.tier.as_str(),
        )
    }

    // ── Private helpers ──────────────────────────────────────────

    /// Read the tier from `~/.duduclaw/license.key` with full integrity checks.
    ///
    /// Verifies Ed25519 signature, expiry date, and machine fingerprint before
    /// trusting the tier field. Returns `Community` if any check fails.
    fn read_tier_from_license() -> Tier {
        let license_path = Self::license_path();
        let content = match std::fs::read_to_string(&license_path) {
            Ok(c) => c,
            Err(_) => return Tier::Community,
        };

        let license: Value = match serde_json::from_str(&content) {
            Ok(v) => v,
            Err(e) => {
                warn!(path = %license_path.display(), error = %e, "failed to parse license.key");
                return Tier::Community;
            }
        };

        // Load public key — if missing, we cannot verify any license
        let pubkey_bytes = match Self::load_public_key() {
            Some(k) => k,
            None => {
                warn!("license public key not found at ~/.duduclaw/.license_pubkey — treating as Community");
                return Tier::Community;
            }
        };

        // Extract and verify Ed25519 signature
        let sig_b64 = match license.get("signature").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => {
                warn!("license.key missing signature field");
                return Tier::Community;
            }
        };

        let sig_bytes = match base64_decode(sig_b64) {
            Some(b) => b,
            None => {
                warn!("license.key has invalid base64 signature");
                return Tier::Community;
            }
        };

        let canonical = match build_canonical_payload(&license) {
            Some(c) => c,
            None => {
                warn!("license.key missing required fields for canonical payload");
                return Tier::Community;
            }
        };

        if !verify_ed25519_signature(&pubkey_bytes, &canonical, &sig_bytes) {
            warn!("license.key Ed25519 signature verification failed");
            return Tier::Community;
        }

        // Check expiry
        if let Some(exp_str) = license.get("expires_at").and_then(|v| v.as_str()) {
            if let Ok(exp) = chrono::DateTime::parse_from_rfc3339(exp_str) {
                if exp.with_timezone(&chrono::Utc) < chrono::Utc::now() {
                    warn!("license.key has expired");
                    return Tier::Community;
                }
            }
        }

        // Check machine fingerprint (MUST match — empty fingerprint is rejected)
        let fp = license
            .get("machine_fingerprint")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let local_fp = Self::machine_fingerprint();
        if fp.is_empty() || fp != local_fp {
            warn!("license.key machine fingerprint mismatch or empty");
            return Tier::Community;
        }

        let tier_str = license
            .get("tier")
            .and_then(|v| v.as_str())
            .unwrap_or("community");

        Tier::from_str(tier_str)
    }

    /// Resolve the DuDuClaw home directory.
    fn duduclaw_home() -> PathBuf {
        if let Ok(custom) = std::env::var("DUDUCLAW_HOME") {
            return PathBuf::from(custom);
        }
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| ".".into());
        PathBuf::from(home).join(".duduclaw")
    }

    /// Return the path to the license file.
    fn license_path() -> PathBuf {
        Self::duduclaw_home().join("license.key")
    }

    /// Return the path to the public key file.
    fn pubkey_path() -> PathBuf {
        Self::duduclaw_home().join(".license_pubkey")
    }

    /// Generate machine fingerprint: SHA-256(hostname::mac)[..16] as 32 hex chars.
    ///
    /// MUST match `build_machine_fingerprint()` in duduclaw-cli/src/main.rs
    /// and `cmd_fingerprint()` in tools/license-keygen/src/main.rs.
    /// Public alias for use by handlers that need fingerprint comparison.
    pub fn machine_fingerprint_static() -> String {
        Self::machine_fingerprint()
    }

    fn machine_fingerprint() -> String {
        let hostname = gethostname::gethostname().to_string_lossy().to_string();
        let mac = mac_address::get_mac_address()
            .ok()
            .flatten()
            .map(|m| m.to_string())
            .unwrap_or_else(|| "00:00:00:00:00:00".into());
        let combined = format!("{hostname}::{mac}");
        let hash = ring::digest::digest(&ring::digest::SHA256, combined.as_bytes());
        hex::encode(&hash.as_ref()[..16])
    }

    /// Load the Ed25519 public key for license verification.
    ///
    /// Priority: embedded compile-time key > file fallback (debug only).
    /// The public key is embedded at build time via `DUDUCLAW_LICENSE_PUBKEY_HEX`
    /// env var. In debug builds, falls back to `~/.duduclaw/.license_pubkey` for
    /// development convenience.
    pub fn load_public_key() -> Option<Vec<u8>> {
        // 1. Compile-time embedded key (production)
        let embedded = option_env!("DUDUCLAW_LICENSE_PUBKEY_HEX").unwrap_or("");
        if !embedded.is_empty() {
            return hex::decode(embedded.trim()).ok().filter(|b: &Vec<u8>| b.len() == 32);
        }

        // 2. File fallback — debug/development only
        #[cfg(debug_assertions)]
        {
            let path = Self::pubkey_path();
            if duduclaw_core::platform::has_loose_permissions(&path) {
                warn!(
                    path = %path.display(),
                    "License public key file has overly permissive permissions — rejecting"
                );
                return None;
            }
            let hex_str = std::fs::read_to_string(&path).ok()?;
            return hex::decode(hex_str.trim()).ok().filter(|b: &Vec<u8>| b.len() == 32);
        }

        #[cfg(not(debug_assertions))]
        {
            warn!("No embedded license public key — set DUDUCLAW_LICENSE_PUBKEY_HEX at build time");
            None
        }
    }

    /// Return the minimum tier required for a given feature.
    ///
    /// Open Core (Apache-2.0): ALL core features are available to all tiers.
    /// Pro/Enterprise add NEW features via the private repo's `GatewayExtension`,
    /// not by gating existing CE features.
    ///
    /// Only OEM-exclusive features (white-label, redistribution) remain gated
    /// here as they represent contractual rights, not functionality.
    fn min_tier_for_feature(feature: &str) -> Tier {
        match feature {
            // OEM-only: contractual rights, not functionality
            "white_label" => Tier::Oem,
            "redistribution" => Tier::Oem,

            // Everything else is open to all tiers (Apache-2.0 Community)
            _ => Tier::Community,
        }
    }
}

// ── Shared license verification helpers ──────────────────────────────

/// Decode a base64 string (standard encoding) into bytes.
pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
    BASE64.decode(s.trim()).ok()
}

/// Verify an Ed25519 signature using `ring`.
///
/// `pubkey_bytes` must be 32 bytes (raw Ed25519 public key).
/// `message` is the canonical payload.
/// `sig_bytes` must be 64 bytes (Ed25519 signature).
pub fn verify_ed25519_signature(pubkey_bytes: &[u8], message: &[u8], sig_bytes: &[u8]) -> bool {
    let peer_public_key =
        signature::UnparsedPublicKey::new(&signature::ED25519, pubkey_bytes);
    peer_public_key.verify(message, sig_bytes).is_ok()
}

/// Build the canonical payload from a license JSON `Value`.
///
/// Matches the field order and **types** used by the `duduclaw-license` crate's
/// `CanonicalPayload` struct: tier, customer_name, machine_fingerprint,
/// issued_at (DateTime<Utc>), expires_at (Option<DateTime<Utc>>).
///
/// IMPORTANT: `issued_at` and `expires_at` are parsed into `DateTime<Utc>` before
/// serialization to ensure byte-identical output with the signing side, which uses
/// `chrono::DateTime<Utc>` (not raw strings). This avoids nanosecond format mismatches.
pub fn build_canonical_payload(license: &Value) -> Option<Vec<u8>> {
    use chrono::{DateTime, Utc};

    // All fields except `signature` must be present for canonical payload.
    let tier = license.get("tier").and_then(|v| v.as_str())?;
    let customer_name = license.get("customer_name").and_then(|v| v.as_str())?;
    let machine_fingerprint = license
        .get("machine_fingerprint")
        .and_then(|v| v.as_str())?;
    let issued_at_str = license.get("issued_at").and_then(|v| v.as_str())?;

    // Parse dates into DateTime<Utc> to match the license crate's CanonicalPayload type.
    // This ensures chrono's serialization format is used, not the raw JSON string.
    let issued_at: DateTime<Utc> = DateTime::parse_from_rfc3339(issued_at_str)
        .ok()?
        .with_timezone(&Utc);

    // expires_at may be null (perpetual license)
    let expires_at: Option<DateTime<Utc>> = license
        .get("expires_at")
        .and_then(|v| v.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc));

    // CANONICAL ORDER — DO NOT REORDER FIELDS.
    // serde_json serializes structs in declaration order.
    // Changing field order will invalidate ALL existing license signatures.
    // Must match CanonicalPayload in tools/license-keygen/src/main.rs.
    // NOTE: `features` field is NOT signed — it is informational only.
    #[derive(Serialize)]
    struct Canonical<'a> {
        tier: &'a str,
        customer_name: &'a str,
        machine_fingerprint: &'a str,
        issued_at: DateTime<Utc>,
        expires_at: Option<DateTime<Utc>>,
    }

    let canonical = Canonical {
        tier,
        customer_name,
        machine_fingerprint,
        issued_at,
        expires_at,
    };

    serde_json::to_vec(&canonical).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn community_tier_no_limits() {
        // Open Core: all tiers get unlimited agents/channels
        let gate = FeatureGate::with_tier(Tier::Community);
        assert_eq!(gate.max_agents(), 0); // 0 = unlimited
        assert_eq!(gate.max_channels(), 0); // 0 = unlimited
        assert_eq!(gate.tier(), Tier::Community);
    }

    #[test]
    fn pro_tier_unlimited() {
        let gate = FeatureGate::with_tier(Tier::Pro);
        assert_eq!(gate.max_agents(), 0);
        assert_eq!(gate.max_channels(), 0);
    }

    #[test]
    fn enterprise_tier_unlimited() {
        let gate = FeatureGate::with_tier(Tier::Enterprise);
        assert_eq!(gate.max_agents(), 0);
        assert_eq!(gate.max_channels(), 0);
    }

    #[test]
    fn community_all_core_features_open() {
        // Open Core: Community tier has ALL core features (Apache-2.0)
        let gate = FeatureGate::with_tier(Tier::Community);
        assert!(gate.check("evolution_enabled"));
        assert!(gate.check("security_input_guard"));
        assert!(gate.check("multi_runtime"));
        assert!(gate.check("account_rotation"));
        assert!(gate.check("federated_memory"));
        assert!(gate.check("odoo"));
        assert!(gate.check("browser_automation"));
        assert!(gate.check("rbac"));
        assert!(gate.check("prometheus_metrics"));
        assert!(gate.check("cost_telemetry"));
        assert!(gate.check("heartbeat"));
        assert!(gate.check("skill_ecosystem"));
        assert!(gate.check("hosted_service"));
        // Only OEM contractual rights are gated
        assert!(!gate.check("white_label"));
        assert!(!gate.check("redistribution"));
    }

    #[test]
    fn oem_has_everything() {
        let gate = FeatureGate::with_tier(Tier::Oem);
        assert!(gate.check("multi_runtime"));
        assert!(gate.check("odoo"));
        assert!(gate.check("browser_automation"));
        assert!(gate.check("white_label"));
        assert!(gate.check("redistribution"));
        assert!(gate.check("hosted_service"));
    }

    #[test]
    fn tier_ordering() {
        assert!(Tier::Community < Tier::Pro);
        assert!(Tier::Pro < Tier::Enterprise);
        assert!(Tier::Enterprise < Tier::Oem);
    }

    #[test]
    fn tier_from_str_case_insensitive() {
        assert_eq!(Tier::from_str("pro"), Tier::Pro);
        assert_eq!(Tier::from_str("PRO"), Tier::Pro);
        assert_eq!(Tier::from_str("Pro"), Tier::Pro);
        assert_eq!(Tier::from_str("enterprise"), Tier::Enterprise);
        assert_eq!(Tier::from_str("oem"), Tier::Oem);
        assert_eq!(Tier::from_str("unknown"), Tier::Community);
        assert_eq!(Tier::from_str(""), Tier::Community);
    }

    #[test]
    fn upgrade_message_includes_tier_info() {
        let gate = FeatureGate::with_tier(Tier::Community);
        // Open Core: core features show Community tier (always allowed)
        let msg = gate.upgrade_message("multi_runtime");
        assert!(msg.contains("Community"));
        assert!(msg.contains("multi_runtime"));

        // OEM-only features still show OEM tier requirement
        let msg2 = gate.upgrade_message("white_label");
        assert!(msg2.contains("OEM"));
    }

    #[test]
    fn unknown_feature_defaults_to_open() {
        // Open Core: unknown features default to Community (open)
        let gate = FeatureGate::with_tier(Tier::Community);
        assert!(gate.check("some_unknown_feature"));
        assert!(gate.check("any_new_feature"));
    }
}
