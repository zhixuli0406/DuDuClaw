//! `config.toml [knowledge.embedding]`.
//!
//! ```toml
//! [knowledge.embedding]
//! provider = "local"                     # local | openai | gemini | none
//! local_model = "multilingual-e5-small"  # catalog id
//! cloud_model = ""                       # empty = the provider's default
//! cloud_account = ""                     # account id to use (empty = rotator / env)
//! cloud_base_url = ""                    # empty = the provider's official endpoint
//! allow_private_endpoint = false         # Admin-only: accept a private / loopback endpoint
//! batch_size = 16
//! threads = 0                            # 0 = half the cores
//! max_vector_rows = 200000
//! idle_unload_minutes = 10
//! # cloud_disclosure_ack is written by the dashboard, never by hand
//! ```
//!
//! An unknown `provider` value means no vectors (keyword-only, shown as
//! such), never a guess. A cloud provider is only used when
//! `cloud_disclosure_ack` names exactly the provider, host and model in
//! effect; changing any of them needs a new acknowledgement.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::catalog::DEFAULT_MODEL;
use super::cloud::{CloudProvider, validate_base_url};

/// Accepted `provider` values.
pub const PROVIDER_VALUES: &[&str] = &["local", "openai", "gemini", "none"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Local,
    Cloud(CloudKind),
    None,
}

/// Serializable mirror of [`CloudProvider`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudKind {
    Openai,
    Gemini,
}

impl CloudKind {
    pub fn provider(self) -> CloudProvider {
        match self {
            CloudKind::Openai => CloudProvider::OpenAi,
            CloudKind::Gemini => CloudProvider::Gemini,
        }
    }
}

impl ProviderKind {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "local" => Self::Local,
            "openai" => Self::Cloud(CloudKind::Openai),
            "gemini" => Self::Cloud(CloudKind::Gemini),
            "none" => Self::None,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Cloud(CloudKind::Openai) => "openai",
            Self::Cloud(CloudKind::Gemini) => "gemini",
            Self::None => "none",
        }
    }
}

/// Who acknowledged sending de-identified passages to which provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisclosureAck {
    pub by: String,
    pub at: String,
    pub provider: String,
    pub host: String,
    /// Port of the acknowledged endpoint. Records written before ports
    /// were recorded have none and cover only 443 (the official endpoints).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingConfig {
    pub provider: ProviderKind,
    /// The raw value when it was not one of [`PROVIDER_VALUES`].
    pub unknown_provider: Option<String>,
    pub local_model: String,
    pub cloud_model: String,
    pub cloud_account: String,
    pub cloud_base_url: String,
    /// Admin-only switch: accept a cloud endpoint on a private, loopback,
    /// link-local or metadata address (a local proxy). Off by default.
    pub allow_private_endpoint: bool,
    pub batch_size: usize,
    pub threads: usize,
    pub max_vector_rows: usize,
    pub idle_unload_minutes: u64,
    pub ack: Option<DisclosureAck>,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            provider: ProviderKind::Local,
            unknown_provider: None,
            local_model: DEFAULT_MODEL.to_string(),
            cloud_model: String::new(),
            cloud_account: String::new(),
            cloud_base_url: String::new(),
            allow_private_endpoint: false,
            batch_size: 16,
            threads: 0,
            max_vector_rows: 200_000,
            idle_unload_minutes: 10,
            ack: None,
        }
    }
}

fn s(t: &toml::Table, k: &str) -> Option<String> {
    t.get(k)
        .and_then(toml::Value::as_str)
        .map(|v| v.trim().to_string())
}

fn n(t: &toml::Table, k: &str, default: u64, lo: u64, hi: u64) -> u64 {
    match t.get(k).and_then(toml::Value::as_integer) {
        Some(v) if v >= 0 => (v as u64).clamp(lo, hi),
        _ => default,
    }
}

/// A cloud model id that is safe inside a URL path: 1–64 of `[A-Za-z0-9._-]`,
/// not starting with `.` and without `..`.
pub fn valid_model_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && !s.starts_with('.')
        && !s.contains("..")
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// The resolved cloud target: provider, model, base URL and host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudTarget {
    pub provider: CloudProvider,
    pub model: String,
    pub base_url: String,
    pub host: String,
    /// Effective port (scheme default when the URL names none).
    pub port: u16,
}

impl EmbeddingConfig {
    pub fn from_toml_str(text: &str) -> Self {
        let root: toml::Table = text.parse().unwrap_or_default();
        let t = root
            .get("knowledge")
            .and_then(toml::Value::as_table)
            .and_then(|k| k.get("embedding"))
            .and_then(toml::Value::as_table);
        match t {
            Some(t) => Self::from_table(t),
            None => Self::default(),
        }
    }

    pub fn from_home(home: &Path) -> Self {
        Self::from_toml_str(&std::fs::read_to_string(home.join("config.toml")).unwrap_or_default())
    }

    fn from_table(t: &toml::Table) -> Self {
        let d = Self::default();
        let raw_provider = s(t, "provider").unwrap_or_else(|| "local".into());
        let (provider, unknown_provider) = match ProviderKind::parse(&raw_provider) {
            Some(p) => (p, None),
            None => (ProviderKind::None, Some(raw_provider)),
        };
        let ack = t
            .get("cloud_disclosure_ack")
            .cloned()
            .and_then(|v| v.try_into::<DisclosureAck>().ok());
        Self {
            provider,
            unknown_provider,
            local_model: s(t, "local_model")
                .filter(|v| !v.is_empty())
                .unwrap_or(d.local_model),
            cloud_model: s(t, "cloud_model").unwrap_or_default(),
            cloud_account: s(t, "cloud_account").unwrap_or_default(),
            cloud_base_url: s(t, "cloud_base_url").unwrap_or_default(),
            allow_private_endpoint: t
                .get("allow_private_endpoint")
                .and_then(toml::Value::as_bool)
                .unwrap_or(false),
            batch_size: n(t, "batch_size", 16, 1, 64) as usize,
            threads: n(t, "threads", 0, 0, 64) as usize,
            max_vector_rows: n(t, "max_vector_rows", 200_000, 1_000, 2_000_000) as usize,
            idle_unload_minutes: n(t, "idle_unload_minutes", 10, 0, 24 * 60),
            ack,
        }
    }

    /// ORT intra-op threads to use.
    pub fn effective_threads(&self) -> usize {
        if self.threads > 0 {
            return self.threads;
        }
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2);
        (cores / 2).max(1)
    }

    /// The cloud target in effect, when the provider is a cloud one.
    pub fn cloud_target(&self) -> Option<Result<CloudTarget, String>> {
        let ProviderKind::Cloud(kind) = self.provider else {
            return None;
        };
        let provider = kind.provider();
        let base = if self.cloud_base_url.is_empty() {
            provider.default_base_url().to_string()
        } else {
            self.cloud_base_url.clone()
        };
        let model = if self.cloud_model.is_empty() {
            provider.default_model().to_string()
        } else {
            self.cloud_model.clone()
        };
        if !valid_model_id(&model) {
            return Some(Err(
                "雲端模型名稱只能包含英數字、點、連字號與底線".to_string()
            ));
        }
        Some(
            validate_base_url(&base, self.allow_private_endpoint).and_then(|(base_url, host)| {
                let port = super::cloud::effective_port(&base_url)
                    .ok_or_else(|| "雲端端點網址格式不正確".to_string())?;
                Ok(CloudTarget {
                    provider,
                    model,
                    base_url,
                    host,
                    port,
                })
            }),
        )
    }

    /// Whether the stored acknowledgement covers `target` exactly.
    pub fn ack_covers(&self, target: &CloudTarget) -> bool {
        self.ack.as_ref().is_some_and(|a| {
            a.provider == target.provider.as_str()
                && a.host == target.host
                && a.port.unwrap_or(443) == target.port
                && a.model == target.model
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_local_e5() {
        let c = EmbeddingConfig::from_toml_str("");
        assert_eq!(c.provider, ProviderKind::Local);
        assert_eq!(c.local_model, DEFAULT_MODEL);
        assert!(c.cloud_target().is_none());
        assert!(c.effective_threads() >= 1);
    }

    #[test]
    fn unknown_provider_means_no_vectors() {
        let c = EmbeddingConfig::from_toml_str("[knowledge.embedding]\nprovider = \"voyage\"\n");
        assert_eq!(c.provider, ProviderKind::None);
        assert_eq!(c.unknown_provider.as_deref(), Some("voyage"));
    }

    #[test]
    fn ack_must_match_provider_host_and_model() {
        let text = r#"
[knowledge.embedding]
provider = "openai"
cloud_disclosure_ack = { by = "admin", at = "2026-10-22T00:00:00Z", provider = "openai", host = "api.openai.com", model = "text-embedding-3-small" }
"#;
        let c = EmbeddingConfig::from_toml_str(text);
        let t = c.cloud_target().unwrap().unwrap();
        assert_eq!(t.host, "api.openai.com");
        assert!(c.ack_covers(&t));
        let moved = EmbeddingConfig::from_toml_str(&text.replace(
            "provider = \"openai\"\ncloud",
            "provider = \"openai\"\ncloud_base_url = \"https://proxy.example.com\"\ncloud",
        ));
        let t2 = moved.cloud_target().unwrap().unwrap();
        assert!(
            !moved.ack_covers(&t2),
            "a new host needs a new acknowledgement"
        );
        let other_model = EmbeddingConfig::from_toml_str(&text.replace(
            "provider = \"openai\"\ncloud",
            "provider = \"openai\"\ncloud_model = \"text-embedding-3-large\"\ncloud",
        ));
        assert!(!other_model.ack_covers(&other_model.cloud_target().unwrap().unwrap()));
        let bad = EmbeddingConfig::from_toml_str(
            "[knowledge.embedding]\nprovider = \"gemini\"\ncloud_base_url = \"http://evil.example.com\"\n",
        );
        assert!(bad.cloud_target().unwrap().is_err());
    }

    #[test]
    fn hand_edited_model_ids_cannot_reach_the_url_path() {
        for bad in ["../v1/x", "a/b", "m?key=1", ".hidden", "x..y", ""] {
            assert!(!valid_model_id(bad), "{bad}");
        }
        assert!(valid_model_id("text-embedding-3-small"));
        let c = EmbeddingConfig::from_toml_str(
            "[knowledge.embedding]\nprovider = \"gemini\"\ncloud_model = \"../evil\"\n",
        );
        assert!(c.cloud_target().unwrap().is_err());
    }

    #[test]
    fn ack_binds_the_port_and_old_records_cover_only_443() {
        let base = |url: &str, port: &str| {
            format!(
                "[knowledge.embedding]\nprovider = \"openai\"\ncloud_base_url = \"{url}\"\ncloud_disclosure_ack = {{ by = \"a\", at = \"t\", provider = \"openai\", host = \"api.example.com\", model = \"text-embedding-3-small\"{port} }}\n"
            )
        };
        let covers = |text: String| {
            let c = EmbeddingConfig::from_toml_str(&text);
            c.ack_covers(&c.cloud_target().unwrap().unwrap())
        };
        assert!(
            covers(base("https://api.example.com", "")),
            "no port recorded = 443"
        );
        assert!(
            !covers(base("https://api.example.com:8443", "")),
            "a record without a port never covers 8443"
        );
        assert!(covers(base(
            "https://api.example.com:8443",
            ", port = 8443"
        )));
        assert!(!covers(base("https://api.example.com", ", port = 8443")));
    }

    #[test]
    fn numbers_are_clamped() {
        let c = EmbeddingConfig::from_toml_str(
            "[knowledge.embedding]\nbatch_size = 999\nmax_vector_rows = 5\nthreads = -1\n",
        );
        assert_eq!(c.batch_size, 64);
        assert_eq!(c.max_vector_rows, 1_000);
        assert_eq!(c.threads, 0);
    }
}
