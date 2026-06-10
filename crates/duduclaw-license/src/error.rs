//! License error types.

use thiserror::Error;

/// Errors that can occur during license operations.
#[derive(Debug, Error)]
pub enum LicenseError {
    /// The license has passed its expiration date.
    #[error("license has expired")]
    Expired,

    /// The license was revoked by control-plane (e.g. subscription cancelled).
    #[error("license has been revoked: {0}")]
    Revoked(String),

    /// The license file claims a `valid_from` in the future.
    #[error("license is not yet valid (valid_from in the future)")]
    NotYetValid,

    /// The license has exceeded its offline grace period — must phone home.
    #[error("license grace period exceeded ({0} days since last phone-home)")]
    GracePeriodExceeded(i64),

    /// The license signature is valid but phone-home should be attempted soon.
    /// Not fatal — caller should attempt refresh but may continue operating.
    #[error("license needs phone-home (last refresh {0} days ago)")]
    NeedsPhoneHome(i64),

    /// The Ed25519 signature on the license is invalid.
    #[error("invalid license signature")]
    InvalidSignature,

    /// The machine fingerprint does not match the licensed machine.
    #[error("machine fingerprint mismatch")]
    InvalidFingerprint,

    /// The requested feature is not available at the current license tier.
    #[error("feature not available: {0}")]
    FeatureNotAvailable(String),

    /// The license file could not be found at the expected path.
    #[error("license file not found: {0}")]
    FileNotFound(String),

    /// The license data could not be parsed.
    #[error("failed to parse license: {0}")]
    ParseError(String),

    /// The license schema version is unknown to this build of the binary.
    #[error("unsupported license schema version: {0}")]
    UnsupportedVersion(u32),

    /// The license refers to a public key ID this build does not recognize.
    #[error("unknown public key id: {0}")]
    UnknownPublicKeyId(String),

    /// A tier-mode mismatch — e.g. a cloud-only tier issued for a self-host machine.
    #[error("license tier mode mismatch: {0}")]
    TierModeMismatch(String),
}

/// Convenience alias for `Result<T, LicenseError>`.
pub type Result<T> = std::result::Result<T, LicenseError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display_messages() {
        assert_eq!(LicenseError::Expired.to_string(), "license has expired");
        assert_eq!(
            LicenseError::InvalidSignature.to_string(),
            "invalid license signature"
        );
        assert_eq!(
            LicenseError::InvalidFingerprint.to_string(),
            "machine fingerprint mismatch"
        );
        assert_eq!(
            LicenseError::FeatureNotAvailable("odoo".into()).to_string(),
            "feature not available: odoo"
        );
        assert_eq!(
            LicenseError::FileNotFound("/tmp/license".into()).to_string(),
            "license file not found: /tmp/license"
        );
        assert_eq!(
            LicenseError::ParseError("bad json".into()).to_string(),
            "failed to parse license: bad json"
        );
        assert_eq!(
            LicenseError::Revoked("subscription_cancelled".into()).to_string(),
            "license has been revoked: subscription_cancelled"
        );
        assert_eq!(
            LicenseError::NotYetValid.to_string(),
            "license is not yet valid (valid_from in the future)"
        );
        assert_eq!(
            LicenseError::GracePeriodExceeded(45).to_string(),
            "license grace period exceeded (45 days since last phone-home)"
        );
        assert_eq!(
            LicenseError::NeedsPhoneHome(8).to_string(),
            "license needs phone-home (last refresh 8 days ago)"
        );
        assert_eq!(
            LicenseError::UnsupportedVersion(99).to_string(),
            "unsupported license schema version: 99"
        );
        assert_eq!(
            LicenseError::UnknownPublicKeyId("v9".into()).to_string(),
            "unknown public key id: v9"
        );
        assert_eq!(
            LicenseError::TierModeMismatch(
                "Solo is cloud-only but was issued to a self-host machine".into()
            )
            .to_string(),
            "license tier mode mismatch: Solo is cloud-only but was issued to a self-host machine"
        );
    }

    #[test]
    fn error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<LicenseError>();
    }
}
