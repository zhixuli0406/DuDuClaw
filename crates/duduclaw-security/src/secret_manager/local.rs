//! In-process AES-256-GCM encrypted secret adapter.
//!
//! Suitable for development, testing, and single-process deployments.
//! Secrets are stored in memory; they do **not** survive process restarts.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use duduclaw_core::error::{DuDuClawError, Result};
use tokio::sync::RwLock;
use tracing::debug;

use crate::crypto::CryptoEngine;
use super::SecretManager;

/// An in-memory, AES-256-GCM encrypted secret store.
///
/// Each `LocalSecretAdapter` instance has its own encryption key generated at
/// construction time.  The encrypted blobs are held in a `HashMap` protected
/// by a `tokio::sync::RwLock`.
pub struct LocalSecretAdapter {
    store: Arc<RwLock<HashMap<String, Vec<u8>>>>,
    crypto: CryptoEngine,
}

impl LocalSecretAdapter {
    /// Create a new adapter with a freshly generated random key.
    ///
    /// This is the primary constructor for ephemeral (non-persistent) use.
    pub fn new_ephemeral() -> Self {
        let key = CryptoEngine::generate_key().expect("system RNG should not fail");
        let crypto = CryptoEngine::new(&key).expect("key material is always valid here");
        Self {
            store: Arc::new(RwLock::new(HashMap::new())),
            crypto,
        }
    }

    /// Create an adapter from an existing 32-byte key (allows re-attaching to
    /// a previously seeded store, e.g. loaded from disk).
    pub fn with_key(key: &[u8; 32]) -> Result<Self> {
        let crypto = CryptoEngine::new(key)?;
        Ok(Self {
            store: Arc::new(RwLock::new(HashMap::new())),
            crypto,
        })
    }
}

#[async_trait]
impl SecretManager for LocalSecretAdapter {
    async fn get(&self, name: &str) -> Result<String> {
        let guard = self.store.read().await;
        let ciphertext = guard.get(name).ok_or_else(|| {
            DuDuClawError::Security(format!("secret not found: {name}"))
        })?;
        let plaintext = self.crypto.decrypt(ciphertext)?;
        String::from_utf8(plaintext)
            .map_err(|e| DuDuClawError::Security(format!("secret '{name}' is not valid UTF-8: {e}")))
    }

    async fn put(&self, name: &str, value: &str) -> Result<()> {
        if name.is_empty() {
            return Err(DuDuClawError::Security(
                "secret name must not be empty".to_string(),
            ));
        }
        if value.is_empty() {
            return Err(DuDuClawError::Security(
                "secret value must not be empty".to_string(),
            ));
        }
        let ciphertext = self.crypto.encrypt(value.as_bytes())?;
        self.store.write().await.insert(name.to_string(), ciphertext);
        debug!(secret_name = name, "secret stored (local adapter)");
        Ok(())
    }

    async fn delete(&self, name: &str) -> Result<()> {
        let removed = self.store.write().await.remove(name);
        if removed.is_none() {
            return Err(DuDuClawError::Security(format!(
                "secret not found for deletion: {name}"
            )));
        }
        debug!(secret_name = name, "secret deleted (local adapter)");
        Ok(())
    }

    async fn exists(&self, name: &str) -> Result<bool> {
        Ok(self.store.read().await.contains_key(name))
    }
}
