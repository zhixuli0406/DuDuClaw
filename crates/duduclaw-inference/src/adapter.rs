//! Adapter surface for embedding hosts (the DuDuClaw gateway) that want to
//! drive the local inference stack through their own chat-provider
//! abstraction (e.g. `duduclaw_llm::ChatProvider`).
//!
//! Design decision (2026-07): `duduclaw-inference` and `duduclaw-llm` stay
//! **decoupled** — neither crate depends on the other. This module only
//! *exposes* what an external adapter needs to make its delegation decision:
//!
//! - whether the currently-active local backend is an OpenAI-compatible HTTP
//!   endpoint (llamafile / Exo via the [`crate::manager::InferenceManager`],
//!   or a configured `[openai_compat]` server), and if so its base URL,
//!   model, and resolved API key ([`CompatEndpoint`]);
//! - whether the operator allows tool calling against that endpoint
//!   (`inference.toml [router] local_tools`, default **true**).
//!
//! The decision itself is a pure function ([`resolve_compat_endpoint`]) so it
//! is table-testable offline; `InferenceEngine::compat_endpoint` supplies the
//! live inputs.

use crate::config::InferenceConfig;

/// Snapshot of the active OpenAI-compatible HTTP endpoint.
///
/// Handed to external adapters so they can point their own OpenAI-compat
/// client (with tool-calling support) at the same server the engine uses.
#[derive(Clone)]
pub struct CompatEndpoint {
    /// Base URL, e.g. `http://localhost:8080/v1`.
    pub base_url: String,
    /// Model name the server expects.
    pub model: String,
    /// Resolved API key (decrypted), `None` when the server needs none.
    pub api_key: Option<String>,
}

impl std::fmt::Debug for CompatEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompatEndpoint")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

/// Where a resolved compat endpoint came from — decides API-key attachment
/// (manager-discovered llamafile/Exo servers are keyless local processes;
/// only the `[openai_compat]` config carries a key).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompatSource {
    /// Discovered by the `InferenceManager` (Exo cluster / llamafile).
    Manager,
    /// Declared in `inference.toml [openai_compat]`.
    Config,
}

/// Pure endpoint-resolution decision. Mirrors `InferenceEngine::init`
/// precedence for the *active* backend: a manager-discovered server
/// (Exo/llamafile) replaces any configured `[openai_compat]` backend, so the
/// manager URL wins here too. Returns `None` when local inference is disabled
/// or no HTTP endpoint is in play (in-process llama.cpp / mistral.rs).
pub fn resolve_compat_endpoint(
    enabled: bool,
    manager_url: Option<String>,
    manager_model: Option<String>,
    config_compat: Option<(&str, &str)>,
) -> Option<(String, String, CompatSource)> {
    if !enabled {
        return None;
    }
    if let Some(url) = manager_url.filter(|u| !u.trim().is_empty()) {
        // Mirror `InferenceEngine::init`: manager model falls back to "default".
        let model = manager_model
            .filter(|m| !m.trim().is_empty())
            .unwrap_or_else(|| "default".to_string());
        return Some((url, model, CompatSource::Manager));
    }
    let (base_url, model) = config_compat?;
    if base_url.trim().is_empty() {
        return None;
    }
    Some((base_url.to_string(), model.to_string(), CompatSource::Config))
}

/// Whether the operator allows an external adapter to run tool calling
/// against the local endpoint. `[router] local_tools` absent ⇒ **true**
/// (most OpenAI-compat servers handle tool JSON; the host's tool loop is
/// fail-soft on malformed calls).
pub fn local_tools_enabled(config: &InferenceConfig) -> bool {
    config
        .router
        .as_ref()
        .and_then(|r| r.local_tools)
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_engine_resolves_no_endpoint() {
        assert!(resolve_compat_endpoint(
            false,
            Some("http://localhost:8080/v1".into()),
            Some("m".into()),
            Some(("http://localhost:9090/v1", "cfg-model")),
        )
        .is_none());
    }

    #[test]
    fn manager_url_wins_over_config_compat() {
        let (url, model, source) = resolve_compat_endpoint(
            true,
            Some("http://localhost:8080/v1".into()),
            Some("exo-model".into()),
            Some(("http://localhost:9090/v1", "cfg-model")),
        )
        .expect("endpoint");
        assert_eq!(url, "http://localhost:8080/v1");
        assert_eq!(model, "exo-model");
        assert_eq!(source, CompatSource::Manager);
    }

    #[test]
    fn manager_model_falls_back_to_default() {
        // llamafile mode: manager reports a URL but no model name.
        let (_, model, _) =
            resolve_compat_endpoint(true, Some("http://localhost:8080/v1".into()), None, None)
                .expect("endpoint");
        assert_eq!(model, "default");
    }

    #[test]
    fn config_compat_used_when_no_manager_url() {
        let (url, model, source) = resolve_compat_endpoint(
            true,
            None,
            None,
            Some(("http://localhost:9090/v1", "cfg-model")),
        )
        .expect("endpoint");
        assert_eq!(url, "http://localhost:9090/v1");
        assert_eq!(model, "cfg-model");
        assert_eq!(source, CompatSource::Config);
    }

    #[test]
    fn empty_urls_resolve_to_none() {
        // Blank manager URL must not shadow config, blank config is rejected.
        assert!(resolve_compat_endpoint(true, Some("  ".into()), None, Some(("", "m"))).is_none());
        assert!(resolve_compat_endpoint(true, None, None, None).is_none());
    }

    // ── [router] local_tools config parsing ────────────────────────────

    fn parse(toml_str: &str) -> InferenceConfig {
        toml::from_str::<InferenceConfig>(toml_str).expect("parse inference.toml")
    }

    #[test]
    fn local_tools_defaults_to_enabled() {
        // No [router] section at all.
        assert!(local_tools_enabled(&parse("enabled = true")));
        // [router] present but local_tools absent.
        assert!(local_tools_enabled(&parse("enabled = true\n[router]\nenabled = true")));
    }

    #[test]
    fn local_tools_explicit_values_parse() {
        assert!(!local_tools_enabled(&parse(
            "enabled = true\n[router]\nlocal_tools = false"
        )));
        assert!(local_tools_enabled(&parse(
            "enabled = true\n[router]\nlocal_tools = true"
        )));
    }

    #[test]
    fn debug_redacts_api_key() {
        let ep = CompatEndpoint {
            base_url: "http://localhost:8080/v1".into(),
            model: "m".into(),
            api_key: Some("sk-secret".into()),
        };
        let dbg = format!("{ep:?}");
        assert!(!dbg.contains("sk-secret"));
        assert!(dbg.contains("[REDACTED]"));
    }
}
