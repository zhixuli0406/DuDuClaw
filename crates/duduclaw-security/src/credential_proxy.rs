use std::collections::HashMap;
use std::sync::Arc;

use duduclaw_core::error::{DuDuClawError, Result};
use tokio::sync::RwLock;
use tracing::info;

use crate::crypto::CryptoEngine;

/// Credential proxy that never exposes real keys to containers.
pub struct CredentialProxy {
    credentials: Arc<RwLock<HashMap<String, EncryptedCredential>>>,
    crypto: CryptoEngine,
}

/// A credential stored in encrypted form.
pub struct EncryptedCredential {
    pub id: String,
    pub encrypted_value: Vec<u8>,
    pub credential_type: CredentialType,
}

/// The kind of credential being stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialType {
    ApiKey,
    OAuthToken,
    ChannelToken,
}

impl CredentialProxy {
    /// Create a new proxy backed by the given crypto engine.
    pub fn new(crypto: CryptoEngine) -> Self {
        Self {
            credentials: Arc::new(RwLock::new(HashMap::new())),
            crypto,
        }
    }

    /// Encrypt and store a credential.
    pub async fn store(
        &self,
        id: &str,
        value: &str,
        cred_type: CredentialType,
    ) -> Result<()> {
        let encrypted_value = self.crypto.encrypt(value.as_bytes())?;
        let credential = EncryptedCredential {
            id: id.to_string(),
            encrypted_value,
            credential_type: cred_type,
        };
        self.credentials
            .write()
            .await
            .insert(id.to_string(), credential);
        info!(credential_id = id, "credential stored");
        Ok(())
    }

    /// Decrypt and return a credential value.
    pub async fn retrieve(&self, id: &str) -> Result<String> {
        let guard = self.credentials.read().await;
        let credential = guard
            .get(id)
            .ok_or_else(|| DuDuClawError::Security(format!("credential not found: {id}")))?;
        let decrypted = self.crypto.decrypt(&credential.encrypted_value)?;
        String::from_utf8(decrypted)
            .map_err(|e| DuDuClawError::Security(format!("invalid UTF-8 in credential: {e}")))
    }

    /// Remove a credential.
    pub async fn remove(&self, id: &str) -> Result<()> {
        let removed = self.credentials.write().await.remove(id);
        if removed.is_none() {
            return Err(DuDuClawError::Security(format!(
                "credential not found: {id}"
            )));
        }
        info!(credential_id = id, "credential removed");
        Ok(())
    }

    /// List all stored credential IDs.
    pub async fn list_ids(&self) -> Vec<String> {
        self.credentials
            .read()
            .await
            .keys()
            .cloned()
            .collect()
    }

    /// Check whether a credential with the given ID exists.
    pub async fn has(&self, id: &str) -> bool {
        self.credentials.read().await.contains_key(id)
    }

    /// Constant-time comparison to prevent timing attacks.
    #[allow(dead_code)]
    fn constant_time_compare(a: &[u8], b: &[u8]) -> bool {
        if a.len() != b.len() {
            return false;
        }
        let mut diff = 0u8;
        for (x, y) in a.iter().zip(b.iter()) {
            diff |= x ^ y;
        }
        diff == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::CryptoEngine;

    fn make_proxy() -> CredentialProxy {
        let key = CryptoEngine::generate_key().unwrap();
        let engine = CryptoEngine::new(&key).unwrap();
        CredentialProxy::new(engine)
    }

    #[tokio::test]
    async fn store_and_retrieve() {
        let proxy = make_proxy();
        proxy
            .store("key1", "secret-value", CredentialType::ApiKey)
            .await
            .unwrap();
        let value = proxy.retrieve("key1").await.unwrap();
        assert_eq!(value, "secret-value");
    }

    #[tokio::test]
    async fn retrieve_missing_credential() {
        let proxy = make_proxy();
        assert!(proxy.retrieve("nonexistent").await.is_err());
    }

    #[tokio::test]
    async fn remove_credential() {
        let proxy = make_proxy();
        proxy
            .store("key1", "val", CredentialType::OAuthToken)
            .await
            .unwrap();
        proxy.remove("key1").await.unwrap();
        assert!(!proxy.has("key1").await);
    }

    #[tokio::test]
    async fn list_ids_returns_all() {
        let proxy = make_proxy();
        proxy
            .store("a", "v1", CredentialType::ApiKey)
            .await
            .unwrap();
        proxy
            .store("b", "v2", CredentialType::ChannelToken)
            .await
            .unwrap();
        let mut ids = proxy.list_ids().await;
        ids.sort();
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[test]
    fn constant_time_compare_works() {
        assert!(CredentialProxy::constant_time_compare(b"abc", b"abc"));
        assert!(!CredentialProxy::constant_time_compare(b"abc", b"abd"));
        assert!(!CredentialProxy::constant_time_compare(b"abc", b"ab"));
    }
}
