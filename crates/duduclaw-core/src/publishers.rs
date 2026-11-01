//! Publisher identities are pinned locally, independently of artifact hosts.
use std::{collections::BTreeMap, path::Path};
use serde::{Deserialize, Serialize};

/// The release verification key, embedded in the binary (never downloaded).
pub const OFFICIAL_PUBLISHER_KEY: &str = "RWTh5pOpk0YmdBgm3VyB2bzxFtajNLXr7zFDhbcc75TgM8YfeV+NSzXh";
pub const TRUST_STORE_FILE: &str = "trusted_publishers.toml";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Publisher { pub keys: Vec<String> }
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustStore {
    #[serde(default)]
    pub publishers: BTreeMap<String, Publisher>,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureState { Verified, #[default] Unsigned, Invalid }
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignatureStatus {
    pub state: SignatureState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,
}
impl SignatureStatus {
    pub fn unsigned(publisher: Option<&str>) -> Self {
        Self { state: SignatureState::Unsigned, publisher: publisher.map(str::to_owned) }
    }
}
pub fn validate_publisher_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 64 || !id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        || !id.as_bytes()[0].is_ascii_alphanumeric() {
        return Err("publisher id must be 1-64 lowercase ASCII letters, digits or hyphens".into());
    }
    Ok(())
}
pub fn validate_pubkey(key: &str) -> Result<(), String> {
    minisign_verify::PublicKey::from_base64(key).map(|_| ()).map_err(|e| format!("invalid minisign public key: {e}"))
}
pub fn verify_minisig(data: &[u8], signature: &str, key: &str) -> Result<(), String> {
    let key = minisign_verify::PublicKey::from_base64(key).map_err(|e| format!("invalid minisign public key: {e}"))?;
    let signature = minisign_verify::Signature::decode(signature).map_err(|e| format!("invalid minisign signature: {e}"))?;
    key.verify(data, &signature, false).map_err(|e| format!("signature verification failed: {e}"))
}
impl TrustStore {
    pub fn parse(text: &str) -> Result<Self, String> {
        let store: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        for (id, publisher) in &store.publishers {
            validate_publisher_id(id)?;
            if id == "duduclaw" { return Err("the built-in duduclaw key cannot be overridden".into()); }
            if publisher.keys.is_empty() { return Err(format!("publisher {id} has no keys")); }
            for key in &publisher.keys { validate_pubkey(key)?; }
        }
        Ok(store)
    }
    /// Only an absent store is empty. Existing invalid state must never downgrade trust.
    pub fn load(home: &Path) -> Result<Self, String> {
        let path = home.join(TRUST_STORE_FILE);
        let invalid = |reason| format!("invalid publisher trust store {}: {reason}; refusing provenance decision", path.display());
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).map_err(invalid),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // A dangling symlink is existing broken state, not an absent store.
                match std::fs::symlink_metadata(&path) {
                    Err(meta_err) if meta_err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
                    _ => Err(invalid(e.to_string())),
                }
            }
            Err(e) => Err(invalid(e.to_string())),
        }
    }
    pub fn keys(&self, id: &str) -> Vec<&str> {
        if id == "duduclaw" { return vec![OFFICIAL_PUBLISHER_KEY]; }
        self.publishers.get(id).map(|p| p.keys.iter().map(String::as_str).collect()).unwrap_or_default()
    }
    pub fn is_pinned(&self, id: &str) -> bool { !self.keys(id).is_empty() }
    /// Origin only: callers must still scan, quarantine and seek approval.
    pub fn verify_content(&self, publisher: Option<&str>, signature: Option<&str>, data: &[u8]) -> Result<SignatureStatus, String> {
        let Some(publisher) = publisher else { return Ok(SignatureStatus::unsigned(None)); };
        let keys = self.keys(publisher);
        if keys.is_empty() { return Ok(SignatureStatus::unsigned(Some(publisher))); }
        let signature = signature.ok_or_else(|| format!("pinned publisher {publisher} is missing its signature"))?;
        if !keys.iter().any(|key| verify_minisig(data, signature, key).is_ok()) {
            return Err(format!("invalid signature for pinned publisher {publisher}"));
        }
        Ok(SignatureStatus { state: SignatureState::Verified, publisher: Some(publisher.into()) })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    const SIG: &str = "untrusted comment: signature from minisign secret key\nRUTh5pOpk0YmdButYZG9tlcekzhpZGpz83+E62dF1rtB5NtAoxXBenf0jZrluKejtL1R5cHtL60+75EzvH/EorwGHcc2aZlwkwQ=\ntrusted comment: duduclaw test\nmRGk2RUiXNVr9zzLXu6BI9+0URr0xlBwS3rMMXD3smkK7rcMrajd/tMz7jhWxQkiPzmWe4pdxFiIG1Vx2JdFAQ==\n";
    #[test]
    fn pinned_signature_origin_and_tamper() {
        let store = TrustStore::default();
        assert_eq!(store.verify_content(Some("duduclaw"), Some(SIG), b"FAKE_BINARY").unwrap().state, SignatureState::Verified);
        assert!(store.verify_content(Some("duduclaw"), Some(SIG), b"tampered").is_err());
        assert!(store.verify_content(Some("duduclaw"), None, b"FAKE_BINARY").is_err());
        assert_eq!(store.verify_content(Some("unknown"), Some(SIG), b"x").unwrap().state, SignatureState::Unsigned);
    }
    #[test]
    fn missing_store_is_empty_but_invalid_store_is_an_error() {
        let home = tempfile::tempdir().unwrap();
        assert!(TrustStore::load(home.path()).unwrap().publishers.is_empty());
        let path = home.path().join(TRUST_STORE_FILE);
        for text in ["[broken", "[publishers.alice]\nkeys=['broken']"] {
            std::fs::write(&path, text).unwrap();
            assert!(TrustStore::load(home.path()).unwrap_err().contains(&path.display().to_string()));
        }
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(TrustStore::load(home.path()).unwrap_err().contains(TRUST_STORE_FILE));
    }
    #[cfg(unix)]
    #[test]
    fn dangling_store_symlink_is_invalid_not_missing() {
        let home = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("absent", home.path().join(TRUST_STORE_FILE)).unwrap();
        assert!(TrustStore::load(home.path()).unwrap_err().contains(TRUST_STORE_FILE));
    }
    #[test]
    fn malformed_store_is_untrusted() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join(TRUST_STORE_FILE), "[publishers.alice]\nkeys=['broken']").unwrap();
        assert!(TrustStore::load(home.path()).unwrap_err().contains(TRUST_STORE_FILE));
        assert!(TrustStore::parse("[publishers.duduclaw]\nkeys=[]").is_err());
    }
}
