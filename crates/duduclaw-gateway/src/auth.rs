use duduclaw_core::error::{DuDuClawError, Result};

/// Authentication manager for the gateway's legacy pre-shared admin token.
///
/// Dashboard sign-in is JWT account login (`duduclaw-auth`, `/api/login`);
/// this type only carries the optional `config.toml` admin token that the
/// WebSocket handshake accepts as `{"token": "..."}`. (An Ed25519
/// challenge-response mode used to live here; nothing ever configured a public
/// key and no client implemented signing, so it was removed.)
pub struct AuthManager {
    token: Option<String>,
}

impl AuthManager {
    /// Create a new [`AuthManager`] with optional token auth.
    pub fn new(token: Option<String>) -> Self {
        Self { token }
    }

    /// Returns `true` when the admin token is configured.
    pub fn is_auth_required(&self) -> bool {
        self.token.is_some()
    }

    /// Validate a provided bearer token against the configured token.
    ///
    /// Uses constant-time comparison to prevent timing attacks.
    pub fn validate(&self, provided_token: &str) -> Result<()> {
        match &self.token {
            Some(expected) => {
                if constant_time_eq(expected.as_bytes(), provided_token.as_bytes()) {
                    Ok(())
                } else {
                    Err(DuDuClawError::Security(
                        "invalid authentication token".to_owned(),
                    ))
                }
            }
            None => Ok(()), // No token required
        }
    }
}

/// Constant-time byte-slice equality check.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
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
