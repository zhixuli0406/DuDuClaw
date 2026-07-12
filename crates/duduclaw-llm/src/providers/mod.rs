//! Provider implementations — each module translates the normalized types
//! to/from one native wire format via pure `build_request_body` /
//! `parse_response` functions (offline-testable) plus a thin HTTP shell.
//!
//! | module          | API                        | streaming (v1)        |
//! |-----------------|----------------------------|-----------------------|
//! | `anthropic`     | Messages API               | real SSE              |
//! | `openai`        | Responses API              | buffered (Done-only)  |
//! | `gemini`        | native generateContent     | buffered (Done-only)  |
//! | `openai_compat` | legacy chat/completions    | real SSE              |

pub mod anthropic;
pub mod gemini;
pub mod openai;
pub mod openai_compat;

pub use anthropic::AnthropicProvider;
pub use gemini::GeminiProvider;
pub use openai::OpenAiProvider;
pub use openai_compat::{preset, CompatPreset, OpenAiCompatProvider, COMPAT_PRESETS};

use crate::provider::{ApiAuth, ChatProvider};

/// Build a boxed [`ChatProvider`] for a provider id + credentials.
///
/// Native protocols route to their dedicated provider; every other id is
/// treated as an OpenAI-compatible preset (`deepseek` / `qwen` / `xai` / …).
/// Unknown ids fail closed with `None` — no guessed base URL. Used by the
/// local reverse-proxy (`duduclaw proxy`) to forward a rotator-selected
/// account to its real upstream.
///
/// `auth.base_url`, when set, overrides the provider's default endpoint (a
/// self-hosted / relayed compat server); for the native providers it is
/// honored by the underlying provider constructor.
pub fn build_provider(provider_id: &str, auth: ApiAuth) -> Option<Box<dyn ChatProvider>> {
    match provider_id {
        "anthropic" => Some(Box::new(AnthropicProvider::new(auth))),
        "openai" => Some(Box::new(OpenAiProvider::new(auth))),
        "gemini" | "google" => Some(Box::new(GeminiProvider::new(auth))),
        other => {
            OpenAiCompatProvider::from_preset(other, auth).map(|p| Box::new(p) as Box<dyn ChatProvider>)
        }
    }
}

#[cfg(test)]
mod build_provider_tests {
    use super::*;

    #[test]
    fn native_providers_resolve() {
        assert_eq!(
            build_provider("anthropic", ApiAuth::new("k")).unwrap().id(),
            "anthropic"
        );
        assert_eq!(build_provider("openai", ApiAuth::new("k")).unwrap().id(), "openai");
        assert_eq!(build_provider("gemini", ApiAuth::new("k")).unwrap().id(), "gemini");
        // Google alias maps to the Gemini provider.
        assert_eq!(build_provider("google", ApiAuth::new("k")).unwrap().id(), "gemini");
    }

    #[test]
    fn compat_preset_resolves_by_id() {
        assert_eq!(
            build_provider("deepseek", ApiAuth::new("k")).unwrap().id(),
            "deepseek"
        );
    }

    #[test]
    fn unknown_provider_fails_closed() {
        assert!(build_provider("not-a-provider", ApiAuth::new("k")).is_none());
    }
}
