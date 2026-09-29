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

/// Handle all computer_* MCP tool calls.
///
/// These tools provide Computer Use capabilities to sub-agents via MCP.
/// They route commands through the orchestrator's global session registry
/// for action execution, or return structured commands for the orchestrator.
pub(crate) async fn handle_computer_use_tool(tool_name: &str, args: &Value) -> Value {
    // Check for active sessions
    let sessions = duduclaw_gateway::computer_use_orchestrator::list_sessions().await;
    let active = !sessions.is_empty();

    match tool_name {
        "computer_screenshot" => {
            let display = args
                .get("display")
                .and_then(|v| v.as_str())
                .unwrap_or("container");
            tool_text(
                &serde_json::json!({
                    "action": "screenshot",
                    "display": display,
                    "active_sessions": sessions,
                    "status": if active { "executing" } else { "no_active_session" },
                })
                .to_string(),
            )
        }
        "computer_click" => {
            let x = args.get("x").and_then(|v| v.as_u64()).unwrap_or(0);
            let y = args.get("y").and_then(|v| v.as_u64()).unwrap_or(0);
            let button = args
                .get("button")
                .and_then(|v| v.as_str())
                .unwrap_or("left");
            let double = args
                .get("double")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let action_name = match (button, double) {
                ("right", _) => "right_click",
                (_, true) => "double_click",
                _ => "left_click",
            };
            tool_text(
                &serde_json::json!({
                    "action": action_name,
                    "coordinate": [x, y],
                    "status": if active { "executing" } else { "no_active_session" },
                })
                .to_string(),
            )
        }
        "computer_type" => {
            let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
            tool_text(
                &serde_json::json!({
                    "action": "type",
                    "text": text,
                    "status": if active { "executing" } else { "no_active_session" },
                })
                .to_string(),
            )
        }
        "computer_key" => {
            let key = args.get("key").and_then(|v| v.as_str()).unwrap_or("");
            tool_text(
                &serde_json::json!({
                    "action": "key",
                    "text": key,
                    "status": if active { "executing" } else { "no_active_session" },
                })
                .to_string(),
            )
        }
        "computer_scroll" => {
            let x = args.get("x").and_then(|v| v.as_u64()).unwrap_or(0);
            let y = args.get("y").and_then(|v| v.as_u64()).unwrap_or(0);
            let direction = args
                .get("direction")
                .and_then(|v| v.as_str())
                .unwrap_or("down");
            let amount = args.get("amount").and_then(|v| v.as_u64()).unwrap_or(3);
            tool_text(
                &serde_json::json!({
                    "action": "scroll",
                    "coordinate": [x, y],
                    "direction": direction,
                    "amount": amount,
                    "status": if active { "executing" } else { "no_active_session" },
                })
                .to_string(),
            )
        }
        "computer_session_start" => {
            let task = args
                .get("task")
                .and_then(|v| v.as_str())
                .unwrap_or("Computer use session");
            let width = args.get("width").and_then(|v| v.as_u64()).unwrap_or(1280);
            let height = args.get("height").and_then(|v| v.as_u64()).unwrap_or(800);
            let session_id = uuid::Uuid::new_v4().to_string();
            tool_text(
                &serde_json::json!({
                    "session_id": session_id,
                    "task": task,
                    "display": format!("{width}x{height}"),
                    "status": "started",
                    "active_sessions": sessions.len() + 1,
                })
                .to_string(),
            )
        }
        "computer_session_stop" => {
            let session_id = args
                .get("session_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            // Stop via the session registry
            if let Some(control) =
                duduclaw_gateway::computer_use_orchestrator::get_session_control(session_id).await
            {
                control
                    .stopped
                    .store(true, std::sync::atomic::Ordering::Release);
                // NOTE: Do NOT call unregister_session() here — the run_loop in
                // channel_reply.rs is the sole owner of session lifecycle cleanup.
                // Setting stopped=true causes run_loop to exit, which then calls
                // unregister_session(). Calling it here too would create a race.
                tool_text(&serde_json::json!({
                    "session_id": session_id,
                    "status": "stopping",
                    "note": "Stop signal sent. Session will be cleaned up when the orchestrator loop exits.",
                }).to_string())
            } else {
                tool_text(
                    &serde_json::json!({
                        "session_id": session_id,
                        "status": "not_found",
                        "active_sessions": sessions,
                    })
                    .to_string(),
                )
            }
        }
        _ => tool_error(&format!("Unknown computer use tool: {tool_name}")),
    }
}

// ─────────────────────────────────────────────────────────────────
// Google Workspace native tool handlers (Gmail + Calendar).
//
// These consume the OAuth vault token via the gateway `google_workspace`
// module. The google:read / google:write scope gates are enforced upstream in
// mcp_dispatch; auth/API failures degrade to clear tool_error text that guides
// the user back to the dashboard Integrations → Google page.
// ─────────────────────────────────────────────────────────────────
