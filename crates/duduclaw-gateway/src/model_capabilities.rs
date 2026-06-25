//! Model capability detection — currently just vision/multimodal support.
//!
//! The Dashboard WebChat needs to tell the user whether the agent's configured
//! model can *understand* uploaded images (vision), versus only reading the text
//! content of documents. Image understanding requires a multimodal model; plain
//! documents (PDF/txt/csv/code) are read as text by the agent regardless, so this
//! gate is specifically about images.
//!
//! Judgement is by `(runtime provider, preferred model id)` — the primary brain
//! used for channel/WebChat replies. Fail-closed: unknown providers/models return
//! `false` so the UI warns rather than silently dropping images on a model that
//! can't see them.

use duduclaw_core::types::RuntimeType;

/// Whether the given runtime + model can interpret image input (vision).
///
/// - **Claude**: every current cloud Claude model (`claude-*` — 3.x, 4.x, Fable)
///   supports vision. A non-`claude-` id (e.g. a local GGUF) is treated as no
///   vision.
/// - **Gemini**: Gemini models are multimodal.
/// - **Codex (OpenAI)**: only the known vision-capable families
///   (`gpt-4o`, `gpt-5`, `o1`/`o3`/`o4`, `gpt-4-*vision*`).
/// - **OpenAI-compat**: custom/local endpoints — unknown, fail closed.
pub fn supports_vision(provider: RuntimeType, model_id: &str) -> bool {
    let m = model_id.trim().to_ascii_lowercase();
    match provider {
        RuntimeType::Claude => m.starts_with("claude"),
        RuntimeType::Gemini => m.starts_with("gemini"),
        // Antigravity (`agy`) multiplexes Gemini + Claude models; both families
        // are multimodal. GPT-OSS / other text-only ids fail closed.
        RuntimeType::Antigravity => m.starts_with("gemini") || m.starts_with("claude"),
        RuntimeType::Codex => {
            m.contains("gpt-4o")
                || m.contains("gpt-5")
                || m.contains("vision")
                || m.starts_with("o1")
                || m.starts_with("o3")
                || m.starts_with("o4")
        }
        RuntimeType::OpenAiCompat => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_cloud_models_support_vision() {
        for id in [
            "claude-opus-4-8",
            "claude-sonnet-4-6",
            "claude-haiku-4-5",
            "claude-fable-5",
            "Claude-Opus-4-8", // case-insensitive
        ] {
            assert!(supports_vision(RuntimeType::Claude, id), "{id}");
        }
    }

    #[test]
    fn claude_runtime_with_local_gguf_has_no_vision() {
        assert!(!supports_vision(RuntimeType::Claude, "qwen2.5-7b-instruct-q4_k_m.gguf"));
        assert!(!supports_vision(RuntimeType::Claude, ""));
    }

    #[test]
    fn gemini_supports_vision() {
        assert!(supports_vision(RuntimeType::Gemini, "gemini-2.0-flash"));
        assert!(!supports_vision(RuntimeType::Gemini, "not-gemini"));
    }

    #[test]
    fn codex_only_known_vision_families() {
        assert!(supports_vision(RuntimeType::Codex, "gpt-4o"));
        assert!(supports_vision(RuntimeType::Codex, "gpt-5-codex"));
        assert!(supports_vision(RuntimeType::Codex, "o3-mini"));
        assert!(!supports_vision(RuntimeType::Codex, "gpt-3.5-turbo"));
    }

    #[test]
    fn openai_compat_fails_closed() {
        assert!(!supports_vision(RuntimeType::OpenAiCompat, "llava-1.6"));
        assert!(!supports_vision(RuntimeType::OpenAiCompat, "anything"));
    }
}
