use duduclaw_core::error::{DuDuClawError, Result};

/// Simple token-based authentication manager.
///
/// When a token is configured, clients must provide a matching token to
/// authenticate.  Ed25519 challenge-response authentication is planned for
/// Phase 5.
pub struct AuthManager {
    token: Option<String>,
}

impl AuthManager {
    /// Create a new [`AuthManager`].
    ///
    /// If `token` is `None`, authentication is disabled and all connections are
    /// accepted.
    pub fn new(token: Option<String>) -> Self {
        Self { token }
    }

    /// Validate a provided token against the configured token.
    ///
    /// Uses constant-time comparison to prevent timing attacks.
    /// Returns `Ok(())` when authentication succeeds, or an error when the
    /// token does not match or no token is configured but one was expected.
    pub fn validate(&self, provided_token: &str) -> Result<()> {
        match &self.token {
            Some(expected) => {
                // Constant-time comparison to prevent timing attacks.
                // Both operands are compared byte-by-byte regardless of
                // where a mismatch occurs.
                let expected_bytes = expected.as_bytes();
                let provided_bytes = provided_token.as_bytes();
                if constant_time_eq(expected_bytes, provided_bytes) {
                    Ok(())
                } else {
                    Err(DuDuClawError::Security(
                        "invalid authentication token".to_owned(),
                    ))
                }
            }
            None => {
                // No token configured — accept everything.
                Ok(())
            }
        }
    }

    /// Returns `true` when a token has been configured and authentication is
    /// required.
    pub fn is_auth_required(&self) -> bool {
        self.token.is_some()
    }
}

/// Constant-time byte-slice equality check.
///
/// Always examines every byte of both slices so that the execution time does
/// not leak information about where a mismatch occurs. Returns `false` when
/// the slices differ in length.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut acc: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        acc |= x ^ y;
    }
    acc == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_auth_required() {
        let mgr = AuthManager::new(None);
        assert!(!mgr.is_auth_required());
        assert!(mgr.validate("anything").is_ok());
    }

    #[test]
    fn test_valid_token() {
        let mgr = AuthManager::new(Some("secret".to_owned()));
        assert!(mgr.is_auth_required());
        assert!(mgr.validate("secret").is_ok());
    }

    #[test]
    fn test_invalid_token() {
        let mgr = AuthManager::new(Some("secret".to_owned()));
        assert!(mgr.validate("wrong").is_err());
    }
}
