use std::collections::HashMap;
use std::path::Path;

use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

use crate::models::{AccessLevel, User};

/// JWT configuration.
pub struct JwtConfig {
    encoding_key: EncodingKey,
    decoding_key: DecodingKey,
    /// Access token time-to-live in seconds (default: 30 minutes).
    /// Shortened from 8h to limit exposure window for suspended accounts (C4 fix).
    pub access_ttl_secs: u64,
    /// Refresh token time-to-live in seconds (default: 7 days).
    pub refresh_ttl_secs: u64,
}

/// Claims embedded in the JWT access token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// Subject (user ID).
    pub sub: String,
    /// User email.
    pub email: String,
    /// User role (admin / manager / employee).
    pub role: String,
    /// List of agent names the user is bound to.
    pub bound_agents: Vec<String>,
    /// Agent name → access level mapping.
    pub access_levels: HashMap<String, String>,
    /// Expiration time (Unix timestamp).
    pub exp: u64,
    /// Issued at (Unix timestamp).
    pub iat: u64,
    /// Token type — "access" or "refresh".
    pub token_type: String,
}

impl JwtConfig {
    /// Create a JwtConfig from a raw secret (at least 32 bytes recommended).
    pub fn new(secret: &[u8]) -> Self {
        Self {
            encoding_key: EncodingKey::from_secret(secret),
            decoding_key: DecodingKey::from_secret(secret),
            access_ttl_secs: 30 * 60,    // 30 minutes (C4: reduced from 8h)
            refresh_ttl_secs: 7 * 86400, // 7 days
        }
    }

    /// Load JWT secret from `~/.duduclaw/jwt_secret`, generating a random
    /// 64-byte secret if the file does not exist. Uses atomic write (temp + rename)
    /// to prevent partial writes on crash (MEDIUM-4 fix).
    pub fn load_or_generate(home_dir: &Path) -> Result<Self, String> {
        let secret_path = home_dir.join("jwt_secret");

        let secret = if secret_path.exists() {
            let bytes = std::fs::read(&secret_path)
                .map_err(|e| format!("failed to read jwt_secret: {e}"))?;
            if bytes.len() < 32 {
                return Err("jwt_secret is too short (corrupted?), delete and restart".to_string());
            }
            bytes
        } else {
            // Generate random 64-byte secret
            let mut bytes = vec![0u8; 64];
            use ring::rand::SecureRandom;
            ring::rand::SystemRandom::new()
                .fill(&mut bytes)
                .map_err(|_| "RNG failed".to_string())?;

            // Atomic write: temp file → set permissions → rename
            let tmp_path = secret_path.with_extension("tmp");
            std::fs::write(&tmp_path, &bytes)
                .map_err(|e| format!("failed to write jwt_secret.tmp: {e}"))?;

            // Set restrictive permissions BEFORE rename (R2 fix: no permission window)
            duduclaw_core::platform::set_owner_only(&tmp_path).ok();

            std::fs::rename(&tmp_path, &secret_path)
                .map_err(|e| format!("failed to rename jwt_secret: {e}"))?;

            tracing::info!("Generated new JWT secret at {}", secret_path.display());
            bytes
        };

        Ok(Self::new(&secret))
    }

    /// Strict JWT validation: locked to HS256, zero leeway (C3 fix).
    fn strict_validation() -> Validation {
        let mut v = Validation::new(Algorithm::HS256);
        v.leeway = 0;
        v
    }

    /// Issue an access token for a user.
    pub fn issue_access_token(
        &self,
        user: &User,
        agent_access: &[(String, AccessLevel)],
    ) -> Result<String, String> {
        let now = chrono::Utc::now().timestamp() as u64;
        let bound_agents: Vec<String> = agent_access.iter().map(|(a, _)| a.clone()).collect();
        let access_levels: HashMap<String, String> = agent_access
            .iter()
            .map(|(a, l)| (a.clone(), l.to_string()))
            .collect();

        let claims = Claims {
            sub: user.id.clone(),
            email: user.email.clone(),
            role: user.role.to_string(),
            bound_agents,
            access_levels,
            exp: now + self.access_ttl_secs,
            iat: now,
            token_type: "access".to_string(),
        };

        encode(&Header::default(), &claims, &self.encoding_key)
            .map_err(|e| format!("JWT encode error: {e}"))
    }

    /// Issue a refresh token (minimal claims).
    pub fn issue_refresh_token(&self, user_id: &str) -> Result<String, String> {
        let now = chrono::Utc::now().timestamp() as u64;
        let claims = Claims {
            sub: user_id.to_string(),
            email: String::new(),
            role: String::new(),
            bound_agents: Vec::new(),
            access_levels: HashMap::new(),
            exp: now + self.refresh_ttl_secs,
            iat: now,
            token_type: "refresh".to_string(),
        };

        encode(&Header::default(), &claims, &self.encoding_key)
            .map_err(|e| format!("JWT encode error: {e}"))
    }

    /// Internal: verify and decode a JWT token with strict validation.
    fn verify_token_inner(&self, token: &str) -> Result<Claims, String> {
        let data = decode::<Claims>(token, &self.decoding_key, &Self::strict_validation())
            .map_err(|e| format!("JWT verify error: {e}"))?;
        Ok(data.claims)
    }

    /// Verify an access token. Rejects refresh tokens.
    pub fn verify_access_token(&self, token: &str) -> Result<Claims, String> {
        let claims = self.verify_token_inner(token)?;
        if claims.token_type != "access" {
            return Err("not an access token".to_string());
        }
        Ok(claims)
    }

    /// Verify a refresh token. Rejects access tokens.
    pub fn verify_refresh_token(&self, token: &str) -> Result<Claims, String> {
        let claims = self.verify_token_inner(token)?;
        if claims.token_type != "refresh" {
            return Err("not a refresh token".to_string());
        }
        Ok(claims)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{UserRole, UserStatus};

    fn test_user() -> User {
        User {
            id: "user-123".to_string(),
            email: "test@example.com".to_string(),
            display_name: "Test".to_string(),
            role: UserRole::Employee,
            status: UserStatus::Active,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            last_login: None,
        }
    }

    #[test]
    fn access_token_roundtrip() {
        let config = JwtConfig::new(b"test-secret-at-least-32-bytes-long!!");
        let agent_access = vec![
            ("my-agent".to_string(), AccessLevel::Owner),
        ];

        let token = config.issue_access_token(&test_user(), &agent_access).unwrap();
        let claims = config.verify_access_token(&token).unwrap();

        assert_eq!(claims.sub, "user-123");
        assert_eq!(claims.email, "test@example.com");
        assert_eq!(claims.role, "employee");
        assert_eq!(claims.bound_agents, vec!["my-agent"]);
        assert_eq!(claims.token_type, "access");
    }

    #[test]
    fn refresh_token_roundtrip() {
        let config = JwtConfig::new(b"test-secret-at-least-32-bytes-long!!");
        let token = config.issue_refresh_token("user-123").unwrap();
        let claims = config.verify_refresh_token(&token).unwrap();

        assert_eq!(claims.sub, "user-123");
        assert_eq!(claims.token_type, "refresh");
    }

    #[test]
    fn access_token_rejected_as_refresh() {
        let config = JwtConfig::new(b"test-secret-at-least-32-bytes-long!!");
        let token = config.issue_access_token(&test_user(), &[]).unwrap();
        assert!(config.verify_refresh_token(&token).is_err());
    }

    #[test]
    fn refresh_token_rejected_as_access() {
        let config = JwtConfig::new(b"test-secret-at-least-32-bytes-long!!");
        let token = config.issue_refresh_token("user-123").unwrap();
        assert!(config.verify_access_token(&token).is_err());
    }

    #[test]
    fn expired_token_rejected() {
        let config = JwtConfig::new(b"test-secret-at-least-32-bytes-long!!");
        // Manually craft a token with exp in the past
        let now = chrono::Utc::now().timestamp() as u64;
        let claims = Claims {
            sub: "user-123".to_string(),
            email: "test@example.com".to_string(),
            role: "employee".to_string(),
            bound_agents: Vec::new(),
            access_levels: HashMap::new(),
            exp: now.saturating_sub(60), // 60 seconds in the past
            iat: now.saturating_sub(120),
            token_type: "access".to_string(),
        };
        let token = encode(
            &Header::default(),
            &claims,
            &config.encoding_key,
        ).unwrap();

        let result = config.verify_access_token(&token);
        assert!(result.is_err(), "expired token should be rejected with zero leeway");
    }

    #[test]
    fn wrong_secret_rejected() {
        let config1 = JwtConfig::new(b"secret-one-at-least-32-bytes-long!!");
        let config2 = JwtConfig::new(b"secret-two-at-least-32-bytes-long!!");
        let token = config1.issue_access_token(&test_user(), &[]).unwrap();
        let result = config2.verify_access_token(&token);
        assert!(result.is_err());
    }
}
