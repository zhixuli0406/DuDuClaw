//! Run proof bound to a gateway-recorded OS process tree.
//! Environment variables transport the token and local endpoint, never identity.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use zeroize::Zeroizing;

pub mod discovery;
pub mod ipc;
pub use duduclaw_execution::process_identity::{ProcessIdentity, RootProcess, is_descendant};
pub use duduclaw_execution::spawn_binding::{SpawnBinding, MAX_ANCESTRY_DEPTH};

pub const ENV_RUN_CREDENTIAL: &str = "DUDUCLAW_RUN_CREDENTIAL";
/// Legacy transport key, retained only to scrub or refuse it. Never connect to it.
pub const ENV_IDENTITY_URL: &str = "DUDUCLAW_IDENTITY_URL";
/// Versioned local routing hint; never an identity claim.
pub const ENV_IDENTITY_ENDPOINT: &str = "DUDUCLAW_IDENTITY_ENDPOINT";
pub const IDENTITY_ENDPOINT_FILE: &str = "run/identity-endpoint";
pub const PREFIX: &str = "ddc_run_";

/// Remove whole run proofs, including when embedded in JSON or shell text.
/// Apply before any log, transcript or error truncation to avoid leaking fragments.
pub fn redact(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains(PREFIX) { return std::borrow::Cow::Borrowed(text); }
    let mut result = String::with_capacity(text.len());
    let mut remaining = text;
    while let Some((before, after)) = remaining.split_once(PREFIX) {
        result.push_str(before);
        result.push_str("[REDACTED]");
        remaining = after.trim_start_matches(|c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    }
    result.push_str(remaining);
    std::borrow::Cow::Owned(result)
}

pub fn redact_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => {
            if let std::borrow::Cow::Owned(masked) = redact(text) { *text = masked; }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(redact_value),
        serde_json::Value::Object(map) => {
            let original = std::mem::take(map);
            for (key, mut value) in original {
                redact_value(&mut value);
                map.insert(redact(&key).into_owned(), value);
            }
        }
        _ => {}
    }
}

pub struct RunCredential(Zeroizing<String>);

/// Bridge for lower-layer host schedulers that cannot depend on gateway.
/// Only the gateway installs an issuer; an absent issuer denies the spawn.
pub trait RunLease: Send + Sync {
    fn credential(&self) -> &RunCredential;
    fn spawn_binding(&self) -> SpawnBinding;
    fn bind_spawn(&self, root: RootProcess) -> std::io::Result<()> {
        self.spawn_binding().bind_spawn(root)
    }
    fn identity_endpoint(&self) -> Option<&str> { None }
}

pub type BackgroundIssuer = dyn Fn(&std::path::Path, &str, &str, &str, u64)
    -> Result<Box<dyn RunLease>, String> + Send + Sync;
static BACKGROUND_ISSUER: std::sync::OnceLock<Box<BackgroundIssuer>> = std::sync::OnceLock::new();

pub fn install_background_issuer(issuer: Box<BackgroundIssuer>) -> Result<(), String> {
    BACKGROUND_ISSUER.set(issuer).map_err(|_| "Run identity issuer already installed".into())
}

pub fn background_lease(home: &std::path::Path, agent: &str, runtime: &str, kind: &str, timeout: u64)
    -> Result<Box<dyn RunLease>, String>
{
    BACKGROUND_ISSUER.get().ok_or("Run identity issuer unavailable")?(home, agent, runtime, kind, timeout)
}

impl RunCredential {
    pub fn from_bytes(bytes: &[u8; 32]) -> Self {
        Self(Zeroizing::new(format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes))))
    }

    pub fn parse(value: String) -> Option<Self> {
        let value = Zeroizing::new(value);
        let encoded = value.strip_prefix(PREFIX)?;
        if encoded.len() != 43 { return None; }
        let bytes = Zeroizing::new(URL_SAFE_NO_PAD.decode(encoded).ok()?);
        if bytes.len() != 32 || URL_SAFE_NO_PAD.encode(&*bytes) != encoded { return None; }
        Some(Self(value))
    }

    /// Only for the child environment or a local introspection request.
    pub fn expose(&self) -> &str { &self.0 }
}

impl std::fmt::Debug for RunCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RunCredential([REDACTED])")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shape_and_debug_are_safe() {
        let credential = RunCredential::from_bytes(&[19; 32]);
        assert!(RunCredential::parse(credential.expose().to_owned()).is_some());
        assert_eq!(format!("{credential:?}"), "RunCredential([REDACTED])");
        for value in ["", "ddc_run_", "ddc_run_偽造", "ddc_run_!!!!"] {
            assert!(RunCredential::parse(value.to_owned()).is_none());
        }
        let redacted = crate::secret_redact::redact_secrets(credential.expose());
        assert!(!redacted.contains(credential.expose()));
    }

    #[test]
    fn redaction_removes_the_entire_proof_before_truncation() {
        let credential = RunCredential::from_bytes(&[77; 32]);
        for text in [credential.expose().to_string(), format!("中文({})!", credential.expose()),
            format!("{{\"credential\":\"{}\"}}", credential.expose()),
            format!("DUDUCLAW_RUN_CREDENTIAL={}", credential.expose())] {
            let masked = crate::secret_redact::redact_secrets(&text);
            assert!(!masked.contains(PREFIX));
            assert!(!masked.contains(credential.expose()));
            assert!(masked.contains("[REDACTED]"));
        }
        assert!(crate::spawn_env::AGENT_CLI_ENV_ALLOWLIST.contains(&ENV_RUN_CREDENTIAL));
        assert!(crate::spawn_env::AGENT_CLI_ENV_ALLOWLIST.contains(&ENV_IDENTITY_ENDPOINT));
        assert!(crate::mcp_proxy_rewrite::is_proxy_private_env_key("duduclaw_run_credential"));
    }
}
