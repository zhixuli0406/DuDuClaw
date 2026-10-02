use super::*;

pub(crate) async fn handle_transcribe_audio(args: &Value) -> Value {
    use base64::Engine;

    let audio_b64 = match args.get("audio_base64").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return tool_error("Missing required parameter: audio_base64"),
    };

    // Limit input size: 34MB base64 ≈ 25MB decoded
    const MAX_B64_LEN: usize = 34 * 1024 * 1024;
    if audio_b64.len() > MAX_B64_LEN {
        return tool_error(&format!(
            "Audio too large: {} bytes (max 25MB)",
            audio_b64.len() * 3 / 4
        ));
    }

    let language = args
        .get("language")
        .and_then(|v| v.as_str())
        .unwrap_or("zh");

    let audio_bytes = match base64::engine::general_purpose::STANDARD.decode(audio_b64) {
        Ok(b) => b,
        Err(e) => return tool_error(&format!("Invalid base64: {e}")),
    };

    // Transcribe via Whisper API (sends raw audio bytes, format auto-detected)
    match duduclaw_inference::whisper::transcribe(
        &audio_bytes,
        Some(language),
        &duduclaw_inference::whisper::WhisperMode::Api,
    )
    .await
    {
        Ok(text) => tool_text(&text),
        Err(e) => tool_error(&format!("Transcription failed: {e}")),
    }
}

pub(crate) async fn handle_synthesize_speech(args: &Value) -> Value {
    use base64::Engine;
    use duduclaw_gateway::tts::TtsProvider;

    let text = match args.get("text").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s,
        _ => return tool_error("Missing required parameter: text"),
    };
    let voice = args.get("voice").and_then(|v| v.as_str()).unwrap_or("");

    let provider = duduclaw_gateway::tts::EdgeTtsProvider::new();
    match provider.synthesize(text, voice).await {
        Ok(audio_bytes) => {
            let b64 = base64::engine::general_purpose::STANDARD.encode(&audio_bytes);
            serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": format!("Audio synthesized ({} bytes). Base64 data:\n{}", audio_bytes.len(), b64)
                }]
            })
        }
        Err(e) => tool_error(&format!("Speech synthesis failed: {e}")),
    }
}

// ── Channel settings tool handlers ──────────────────────────────
//
// Channel-type list, key allowlists, and value validation are centralized in
// `duduclaw_gateway::channel_settings` (W2-2) so this MCP tool and the
// dashboard `channels.config_*`/`access_*` RPCs can never drift into
// accepting different input — one shared `ChannelSettingsManager` access
// layer, one validator. `VALID_KEYS` here intentionally excludes
// `admin_users`: an in-channel agent must never grant itself `!STOP`
// authority (dashboard-only via `channels.access_set`, see handlers.rs).

// The `computer_*` tools live in `computer_use_client.rs` (a thin client of
// the gateway's `POST /api/internal/computer-use`).

// ─────────────────────────────────────────────────────────────────
// Google Workspace native tool handlers (Gmail + Calendar).
//
// These consume the OAuth vault token via the gateway `google_workspace`
// module. The google:read / google:write scope gates are enforced upstream in
// mcp_dispatch; auth/API failures degrade to clear tool_error text that guides
// the user back to the dashboard Integrations → Google page.
// ─────────────────────────────────────────────────────────────────
