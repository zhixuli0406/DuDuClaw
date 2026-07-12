//! A2A v1.0 Signed Agent Card — domain-holder signatures (G6).
//!
//! The A2A (Agent2Agent) v1.0 spec adds a `signatures` array to the Agent Card
//! so a receiving party can verify the card was issued by the holder of a given
//! Ed25519 key. Each signature is a JWS in **detached-payload** form (RFC 7515
//! Appendix F): the payload — the canonical JSON of the card *without* its
//! `signatures` field — is not embedded in the JWS; the verifier reconstructs it
//! from the card itself.
//!
//! Design mirrors the project's existing Ed25519 conventions:
//! - `duduclaw-license` uses `ed25519-dalek` v2 (`SigningKey`/`VerifyingKey`).
//! - `updater.rs` pins a minisign public key for release verification.
//!
//! Key material lives at `<duduclaw_home>/keys/a2a-signing.ed25519` (32 raw
//! private-key bytes, `chmod 600`). Missing key ⇒ generated on first use.
//!
//! **Fail-closed:** every function here returns `Result`/`Option`; a key-load or
//! generation failure never panics. The HTTP layer falls back to an unsigned
//! card (see `server.rs`) rather than returning 500.
//!
//! All the wire-format helpers (canonical JSON, JWS assembly, base64url) are
//! pure functions with unit tests, including a sign→verify roundtrip.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// JWS `alg` for Ed25519 (RFC 8037).
const ALG_EDDSA: &str = "EdDSA";

/// A domain-holder signer for A2A Agent Cards.
pub struct A2aSigner {
    signing_key: SigningKey,
    /// Key ID advertised in JWKS and the JWS protected header (`kid`).
    kid: String,
}

impl A2aSigner {
    /// Build a signer from raw 32-byte private-key material.
    pub fn from_secret_bytes(secret: [u8; 32]) -> Self {
        let signing_key = SigningKey::from_bytes(&secret);
        let kid = key_fingerprint(&signing_key.verifying_key());
        Self { signing_key, kid }
    }

    /// Load the signing key from `path`, generating (and persisting) it on first
    /// run. Returns an error string on any IO / format failure so the caller can
    /// fall back to serving an unsigned card.
    pub fn load_or_generate(path: &Path) -> Result<(Self, bool), String> {
        match std::fs::read(path) {
            Ok(bytes) => {
                let secret: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
                    format!(
                        "A2A 簽章私鑰長度不符（預期 32 bytes，實際 {}）",
                        bytes.len()
                    )
                })?;
                Ok((Self::from_secret_bytes(secret), false))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let signer = generate_and_persist(path)?;
                Ok((signer, true))
            }
            Err(e) => Err(format!("讀取 A2A 簽章私鑰失敗（{}）：{e}", path.display())),
        }
    }

    /// The advertised key id (`kid`).
    pub fn kid(&self) -> &str {
        &self.kid
    }

    /// The public verifying key.
    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }

    /// SHA-256 fingerprint of the public key (for operator-facing logs).
    pub fn fingerprint(&self) -> String {
        self.kid.clone()
    }

    /// Sign `card`, inserting/overwriting its `signatures` array with a single
    /// detached-payload JWS. Any pre-existing `signatures` field is stripped from
    /// the canonicalized payload so the signature covers the substantive card.
    pub fn sign_card(&self, card: &mut Value) {
        let payload_b64 = URL_SAFE_NO_PAD.encode(canonical_card_bytes(card));
        let protected = build_protected_header(&self.kid);
        let protected_b64 = URL_SAFE_NO_PAD.encode(protected.as_bytes());
        let signing_input = jws_signing_input(&protected_b64, &payload_b64);
        let sig: Signature = self.signing_key.sign(signing_input.as_bytes());
        let sig_b64 = URL_SAFE_NO_PAD.encode(sig.to_bytes());

        if let Some(obj) = card.as_object_mut() {
            obj.insert(
                "signatures".to_string(),
                json!([{
                    "protected": protected_b64,
                    "signature": sig_b64,
                }]),
            );
        }
    }

    /// The JWKS document (`/.well-known/jwks.json`) advertising this public key.
    pub fn jwks(&self) -> Value {
        jwks_document(&self.verifying_key(), &self.kid)
    }
}

/// Generate a fresh key, persist it with `0600` perms, return the signer.
fn generate_and_persist(path: &Path) -> Result<A2aSigner, String> {
    use rand::rngs::OsRng;
    let signing_key = SigningKey::generate(&mut OsRng);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("建立 A2A 金鑰目錄失敗（{}）：{e}", dir.display()))?;
    }
    write_key_owner_only(path, &signing_key.to_bytes())?;
    let kid = key_fingerprint(&signing_key.verifying_key());
    Ok(A2aSigner { signing_key, kid })
}

/// Persist the raw private key so it is **never** briefly world/group-readable.
///
/// On Unix the file is created atomically with mode `0600` (the mode is applied
/// at `open` time, closing the window that `write()` + later `chmod` leaves — a
/// local user could read the key in between). `set_owner_only_permissions` is
/// still called afterwards to re-assert `0600` in the rare case the file
/// pre-existed with looser perms (where the create-time mode is ignored).
fn write_key_owner_only(path: &Path, bytes: &[u8]) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| format!("開啟 A2A 私鑰檔失敗（{}）：{e}", path.display()))?;
        f.write_all(bytes)
            .map_err(|e| format!("寫入 A2A 簽章私鑰失敗（{}）：{e}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes)
            .map_err(|e| format!("寫入 A2A 簽章私鑰失敗（{}）：{e}", path.display()))?;
    }
    set_owner_only_permissions(path)
}

/// Restrict the key file to owner read/write (`chmod 600` on Unix).
fn set_owner_only_permissions(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("設定 A2A 私鑰權限失敗（{}）：{e}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// Canonical path for the A2A signing key.
pub fn default_key_path() -> PathBuf {
    duduclaw_core::duduclaw_home()
        .join("keys")
        .join("a2a-signing.ed25519")
}

// ── Pure helpers (unit-tested) ────────────────────────────────────────────

/// SHA-256 fingerprint of a public key, rendered as `sha256:<hex16>` for use as
/// a stable, non-secret key id.
pub fn key_fingerprint(vk: &VerifyingKey) -> String {
    let digest = Sha256::digest(vk.to_bytes());
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

/// JWS protected header JSON for an EdDSA detached-payload signature.
///
/// `b64: false` + `crit: ["b64"]` (RFC 7797) signals the payload is not
/// base64url-encoded inside the JWS — but A2A uses standard detached JWS where
/// the payload *is* base64url. We therefore emit a plain compact-serialization
/// header (`alg`, `kid`) matching the A2A reference implementations.
pub fn build_protected_header(kid: &str) -> String {
    // Deterministic key order for stable bytes across runs.
    let mut map = Map::new();
    map.insert("alg".to_string(), Value::String(ALG_EDDSA.to_string()));
    map.insert("kid".to_string(), Value::String(kid.to_string()));
    canonical_json(&Value::Object(map))
}

/// The JWS signing input: `BASE64URL(protected) || '.' || BASE64URL(payload)`.
pub fn jws_signing_input(protected_b64: &str, payload_b64: &str) -> String {
    format!("{protected_b64}.{payload_b64}")
}

/// Canonical UTF-8 bytes of a card with its `signatures` field removed.
pub fn canonical_card_bytes(card: &Value) -> Vec<u8> {
    let mut clone = card.clone();
    if let Some(obj) = clone.as_object_mut() {
        obj.remove("signatures");
    }
    canonical_json(&clone).into_bytes()
}

/// Deterministic JSON serialization: object keys sorted lexicographically,
/// compact (no insignificant whitespace). Arrays preserve order.
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            out.push('{');
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                // serde_json produces a spec-correct JSON string literal.
                out.push_str(&Value::String((*k).clone()).to_string());
                out.push(':');
                write_canonical(&map[*k], out);
            }
            out.push('}');
        }
        Value::Array(arr) => {
            out.push('[');
            for (i, v) in arr.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(v, out);
            }
            out.push(']');
        }
        // Scalars: serde_json's Display is already compact and canonical.
        other => out.push_str(&other.to_string()),
    }
}

/// Build the JWKS document advertising an Ed25519 public key (RFC 8037 OKP).
pub fn jwks_document(vk: &VerifyingKey, kid: &str) -> Value {
    let x = URL_SAFE_NO_PAD.encode(vk.to_bytes());
    json!({
        "keys": [{
            "kty": "OKP",
            "crv": "Ed25519",
            "x": x,
            "use": "sig",
            "alg": ALG_EDDSA,
            "kid": kid,
        }]
    })
}

/// Verify that a signed `card` carries at least one valid detached-payload JWS
/// under `vk`. Used by tests and any future inbound-card verification.
pub fn verify_card(card: &Value, vk: &VerifyingKey) -> bool {
    let Some(sigs) = card.get("signatures").and_then(|s| s.as_array()) else {
        return false;
    };
    let payload_b64 = URL_SAFE_NO_PAD.encode(canonical_card_bytes(card));
    for sig in sigs {
        let Some(protected_b64) = sig.get("protected").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(signature_b64) = sig.get("signature").and_then(|v| v.as_str()) else {
            continue;
        };
        let Ok(sig_bytes) = URL_SAFE_NO_PAD.decode(signature_b64) else {
            continue;
        };
        let Ok(sig_arr): Result<[u8; 64], _> = sig_bytes.as_slice().try_into() else {
            continue;
        };
        let signing_input = jws_signing_input(protected_b64, &payload_b64);
        let signature = Signature::from_bytes(&sig_arr);
        if vk.verify(signing_input.as_bytes(), &signature).is_ok() {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_signer() -> A2aSigner {
        // Deterministic key from fixed bytes — reproducible tests.
        A2aSigner::from_secret_bytes([7u8; 32])
    }

    #[test]
    fn canonical_json_sorts_keys_and_is_compact() {
        let v = json!({ "b": 1, "a": { "d": 4, "c": 3 }, "arr": [3, 1, 2] });
        // Keys sorted at every level; array order preserved; no whitespace.
        assert_eq!(
            canonical_json(&v),
            r#"{"a":{"c":3,"d":4},"arr":[3,1,2],"b":1}"#
        );
    }

    #[test]
    fn canonical_json_escapes_strings() {
        let v = json!({ "k": "a\"b\n" });
        assert_eq!(canonical_json(&v), r#"{"k":"a\"b\n"}"#);
    }

    #[test]
    fn canonical_card_bytes_strips_signatures() {
        let card = json!({ "name": "x", "signatures": [{"protected": "p", "signature": "s"}] });
        let bytes = canonical_card_bytes(&card);
        assert_eq!(String::from_utf8(bytes).unwrap(), r#"{"name":"x"}"#);
    }

    #[test]
    fn protected_header_is_stable_and_sorted() {
        let h = build_protected_header("sha256:abcd");
        assert_eq!(h, r#"{"alg":"EdDSA","kid":"sha256:abcd"}"#);
    }

    #[test]
    fn jws_signing_input_joins_with_dot() {
        assert_eq!(jws_signing_input("aaa", "bbb"), "aaa.bbb");
    }

    #[test]
    fn fingerprint_is_deterministic_and_prefixed() {
        let s = test_signer();
        let fp = s.fingerprint();
        assert!(fp.starts_with("sha256:"), "fingerprint: {fp}");
        // 16 hex chars after the prefix.
        assert_eq!(fp.len(), "sha256:".len() + 16);
        // Deterministic for a fixed key.
        assert_eq!(fp, test_signer().fingerprint());
    }

    #[test]
    fn sign_card_inserts_signatures_array() {
        let signer = test_signer();
        let mut card = json!({ "name": "DuDuClaw Agent", "version": "1.0.0" });
        signer.sign_card(&mut card);
        let sigs = card
            .get("signatures")
            .and_then(|s| s.as_array())
            .expect("signatures");
        assert_eq!(sigs.len(), 1);
        assert!(sigs[0].get("protected").and_then(|v| v.as_str()).is_some());
        assert!(sigs[0].get("signature").and_then(|v| v.as_str()).is_some());
        // Original fields untouched (only-add invariant).
        assert_eq!(card["name"], json!("DuDuClaw Agent"));
        assert_eq!(card["version"], json!("1.0.0"));
    }

    #[test]
    fn sign_then_verify_roundtrip() {
        let signer = test_signer();
        let mut card = json!({
            "name": "DuDuClaw Agent",
            "version": "1.0.0",
            "capabilities": { "streaming": true },
        });
        signer.sign_card(&mut card);
        assert!(
            verify_card(&card, &signer.verifying_key()),
            "own key must verify"
        );
    }

    #[test]
    fn verify_fails_under_wrong_key() {
        let signer = test_signer();
        let mut card = json!({ "name": "DuDuClaw Agent" });
        signer.sign_card(&mut card);
        let other = A2aSigner::from_secret_bytes([9u8; 32]);
        assert!(
            !verify_card(&card, &other.verifying_key()),
            "foreign key must not verify"
        );
    }

    #[test]
    fn verify_fails_on_tampered_card() {
        let signer = test_signer();
        let mut card = json!({ "name": "DuDuClaw Agent", "version": "1.0.0" });
        signer.sign_card(&mut card);
        // Mutate a signed field after signing.
        card["version"] = json!("6.6.6");
        assert!(
            !verify_card(&card, &signer.verifying_key()),
            "tampered card must not verify"
        );
    }

    #[test]
    fn verify_fails_without_signatures() {
        let signer = test_signer();
        let card = json!({ "name": "DuDuClaw Agent" });
        assert!(!verify_card(&card, &signer.verifying_key()));
    }

    #[test]
    fn jwks_advertises_okp_ed25519() {
        let signer = test_signer();
        let jwks = signer.jwks();
        let key = &jwks["keys"][0];
        assert_eq!(key["kty"], json!("OKP"));
        assert_eq!(key["crv"], json!("Ed25519"));
        assert_eq!(key["alg"], json!("EdDSA"));
        assert_eq!(key["use"], json!("sig"));
        assert_eq!(key["kid"], json!(signer.kid()));
        // x is base64url of the 32-byte public key ⇒ decodes to 32 bytes.
        let x = key["x"].as_str().unwrap();
        assert_eq!(URL_SAFE_NO_PAD.decode(x).unwrap().len(), 32);
    }

    #[test]
    fn load_or_generate_creates_then_reuses_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys").join("a2a-signing.ed25519");
        let (signer1, generated) = A2aSigner::load_or_generate(&path).unwrap();
        assert!(generated, "first call generates");
        assert!(path.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "key must be owner-only");
        }
        let (signer2, generated2) = A2aSigner::load_or_generate(&path).unwrap();
        assert!(!generated2, "second call reuses");
        // Same key material ⇒ same fingerprint.
        assert_eq!(signer1.fingerprint(), signer2.fingerprint());
    }

    #[test]
    fn load_rejects_wrong_length_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.ed25519");
        std::fs::write(&path, b"too short").unwrap();
        assert!(A2aSigner::load_or_generate(&path).is_err());
    }
}
