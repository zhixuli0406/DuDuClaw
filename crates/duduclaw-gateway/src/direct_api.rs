//! Direct Anthropic Messages API client — bypasses Claude CLI for pure chat.
//!
//! By calling the API directly, DuDuClaw gains full control over `cache_control`
//! breakpoint placement and deterministic serialization, achieving 95%+ cache hit
//! rates (vs. 25% through the CLI).
//!
//! Reference: <https://docs.anthropic.com/en/api/messages>

use duduclaw_core::truncate_bytes;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::cost_telemetry::TokenUsage;

// ---------------------------------------------------------------------------
// API types
// ---------------------------------------------------------------------------

/// Anthropic Messages API request body.
#[derive(Debug, Serialize)]
struct MessagesRequest {
    model: String,
    max_tokens: u32,
    system: Vec<SystemBlock>,
    messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
}

/// A system prompt block with optional cache_control.
#[derive(Debug, Serialize)]
struct SystemBlock {
    #[serde(rename = "type")]
    block_type: String,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<CacheControl>,
}

/// Cache control marker.
#[derive(Debug, Serialize)]
struct CacheControl {
    #[serde(rename = "type")]
    control_type: String,
}

/// A conversation message.
#[derive(Debug, Serialize)]
struct Message {
    role: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<CacheControl>,
}

/// Anthropic Messages API response.
#[derive(Debug, Deserialize)]
struct MessagesResponse {
    content: Vec<ContentBlock>,
    usage: Option<ApiUsage>,
}

#[derive(Debug, Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    #[serde(default)]
    text: String,
}

/// Usage information from the API response.
#[derive(Debug, Deserialize)]
struct ApiUsage {
    input_tokens: u64,
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
}

impl From<ApiUsage> for TokenUsage {
    fn from(u: ApiUsage) -> Self {
        Self {
            input_tokens: u.input_tokens,
            cache_read_tokens: u.cache_read_input_tokens,
            cache_creation_tokens: u.cache_creation_input_tokens,
            output_tokens: u.output_tokens,
        }
    }
}

/// Error response from Anthropic API.
#[derive(Debug, Deserialize)]
struct ApiError {
    error: ApiErrorDetail,
}

#[derive(Debug, Deserialize)]
struct ApiErrorDetail {
    message: String,
    #[serde(rename = "type")]
    error_type: String,
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

const API_BASE: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u32 = 4096;

/// Marker that splits the system prompt into separately-cached blocks.
///
/// Prompt assemblers can layer the system prompt by change frequency —
/// static (SOUL/CLAUDE.md/capabilities) `‖` semi-stable (wiki/skills/summary)
/// — so a semi-stable edit only invalidates its own block instead of the
/// whole prefix. Anthropic allows 4 cache breakpoints; we spend at most 3 on
/// system blocks (the conversation-history breakpoint uses the 4th), so at
/// most 3 segments are honored and any extras merge into the last one.
/// Prompts without the marker behave exactly as before (single cached block).
pub const CACHE_SPLIT_MARKER: &str = "<!-- duduclaw:cache-split -->";

/// Max system segments that receive their own cache breakpoint.
const MAX_SYSTEM_SEGMENTS: usize = 3;

/// Shared HTTP client singleton — avoids rebuilding connection pool per request.
static HTTP_CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();

fn http_client() -> &'static reqwest::Client {
    HTTP_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("Failed to build HTTP client")
    })
}

/// Direct API response with text and optional usage telemetry.
pub struct DirectApiResponse {
    pub text: String,
    pub usage: Option<TokenUsage>,
}

/// Call Anthropic Messages API directly with precise cache_control placement.
///
/// Cache strategy ("system_and_3"):
/// 1. System prompt → single block with `cache_control: ephemeral`
/// 2. Conversation history → included as prior turns, with cache breakpoint
///    on the 3rd-to-last message for optimal cache hit rate
/// 3. Current user message → no cache_control (changes every time)
///
/// This achieves 95%+ cache efficiency on system prompt + stable history.
pub async fn call_direct_api(
    api_key: &str,
    model: &str,
    system_prompt: &str,
    user_prompt: &str,
    conversation_history: &[(String, String)], // (role, content) pairs
) -> Result<DirectApiResponse, String> {
    call_direct_api_with_dynamic(
        api_key,
        model,
        system_prompt,
        None,
        user_prompt,
        conversation_history,
    )
    .await
}

/// Variant of [`call_direct_api`] that accepts an optional **dynamic system
/// suffix** which is appended as a *second* system block **without**
/// `cache_control`.
///
/// Motivation: the agent's pending Task Queue changes every turn, so
/// appending it to the cached system block would invalidate the entire
/// static prefix (Soul/Identity/Skills/Contract — usually 5–20k tokens).
/// Keeping it in a separate uncached block preserves 95%+ cache efficiency
/// on the static prefix while still letting the agent see the live queue.
///
/// Callers that don't need dynamic content should keep using
/// [`call_direct_api`].
pub async fn call_direct_api_with_dynamic(
    api_key: &str,
    model: &str,
    system_prompt: &str,
    dynamic_system_suffix: Option<&str>,
    user_prompt: &str,
    conversation_history: &[(String, String)],
) -> Result<DirectApiResponse, String> {
    call_direct_api_attributed(
        None,
        api_key,
        model,
        system_prompt,
        dynamic_system_suffix,
        user_prompt,
        conversation_history,
    )
    .await
}

/// Full-control variant with cache-invalidation attribution.
///
/// `scope` identifies a stable request stream (e.g. `"{agent_id}:{model}"`).
/// When set, the cached system-block hashes of each request are compared to
/// the previous request in the same scope, and the *cause* of any prefix
/// invalidation (`system_block_N` / `layout` / `cold`) is logged and counted —
/// see [`cache_attribution_snapshot`]. CostTelemetry can tell you cache
/// efficiency is low; this tells you *which block keeps breaking it*.
pub async fn call_direct_api_attributed(
    scope: Option<&str>,
    api_key: &str,
    model: &str,
    system_prompt: &str,
    dynamic_system_suffix: Option<&str>,
    user_prompt: &str,
    conversation_history: &[(String, String)],
) -> Result<DirectApiResponse, String> {
    let client = http_client();

    // Build system blocks: cached segments (split on CACHE_SPLIT_MARKER),
    // then the uncached dynamic suffix.
    let segments = split_system_segments(system_prompt);
    let mut system: Vec<SystemBlock> = segments
        .iter()
        .map(|seg| SystemBlock {
            block_type: "text".to_string(),
            text: seg.clone(),
            cache_control: Some(CacheControl {
                control_type: "ephemeral".to_string(),
            }),
        })
        .collect();

    if let Some(scope_key) = scope {
        note_cache_attribution(scope_key, &segments);
    }
    if let Some(suffix) = dynamic_system_suffix.filter(|s| !s.trim().is_empty()) {
        system.push(SystemBlock {
            block_type: "text".to_string(),
            text: suffix.to_string(),
            cache_control: None, // intentionally uncached — changes per-turn
        });
    }

    // Build messages with conversation history ("system_and_3" cache strategy)
    let mut messages: Vec<Message> = Vec::with_capacity(conversation_history.len() + 1);

    // Add conversation history with the "system_and_3" cache strategy:
    // place cache breakpoint on the 3rd-to-last message so the last 3 messages
    // (which change each turn) are re-sent, while everything before is cached.
    // Only set breakpoint when history has enough depth to benefit from caching.
    let cache_breakpoint_idx = if conversation_history.len() >= 4 {
        Some(conversation_history.len() - 4)
    } else {
        None // too short for meaningful caching
    };
    for (i, (role, content)) in conversation_history.iter().enumerate() {
        messages.push(Message {
            role: role.clone(),
            content: content.clone(),
            cache_control: if cache_breakpoint_idx == Some(i) {
                Some(CacheControl { control_type: "ephemeral".to_string() })
            } else {
                None
            },
        });
    }

    // Current user message — no cache (changes every call)
    messages.push(Message {
        role: "user".to_string(),
        content: user_prompt.to_string(),
        cache_control: None,
    });

    let body = MessagesRequest {
        model: model.to_string(),
        max_tokens: DEFAULT_MAX_TOKENS,
        system,
        messages,
        stream: None,
    };

    let response = client
        .post(API_BASE)
        .header("x-api-key", api_key)
        .header("anthropic-version", API_VERSION)
        .header("anthropic-beta", "prompt-caching-2024-07-31")
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("HTTP request failed: {e}"))?;

    let status = response.status();
    let response_text = response
        .text()
        .await
        .map_err(|e| format!("Failed to read response body: {e}"))?;

    if !status.is_success() {
        // Try to parse error
        if let Ok(err) = serde_json::from_str::<ApiError>(&response_text) {
            return Err(format!(
                "Anthropic API error ({}): {} - {}",
                status, err.error.error_type, err.error.message
            ));
        }
        return Err(format!("Anthropic API error ({}): {}", status, truncate_bytes(&response_text, 200)));
    }

    let resp: MessagesResponse = serde_json::from_str(&response_text)
        .map_err(|e| format!("Failed to parse API response: {e}"))?;

    let text = resp
        .content
        .iter()
        .filter(|b| b.block_type == "text")
        .map(|b| b.text.as_str())
        .collect::<Vec<_>>()
        .join("");

    let usage = resp.usage.map(TokenUsage::from);

    if let Some(ref u) = usage {
        info!(
            input = u.input_tokens,
            cache_read = u.cache_read_tokens,
            cache_write = u.cache_creation_tokens,
            output = u.output_tokens,
            cache_eff = format!("{:.1}%", u.cache_efficiency() * 100.0),
            "Direct API call completed"
        );
        // Detailed cache metrics at debug level for monitoring dashboards
        let total = u.input_tokens + u.cache_read_tokens + u.cache_creation_tokens;
        if total > 0 {
            let efficiency = u.cache_read_tokens as f64 / total as f64;
            debug!(
                cache_efficiency = %format!("{:.1}%", efficiency * 100.0),
                cache_read = u.cache_read_tokens,
                cache_creation = u.cache_creation_tokens,
                input = u.input_tokens,
                "Anthropic API cache metrics"
            );
        }
    }

    if text.is_empty() {
        return Err("Empty response from Anthropic API".to_string());
    }

    Ok(DirectApiResponse { text, usage })
}

// ---------------------------------------------------------------------------
// Cache segmentation + invalidation attribution
// ---------------------------------------------------------------------------

/// Split the system prompt on [`CACHE_SPLIT_MARKER`] into normalized,
/// non-empty segments, capped at [`MAX_SYSTEM_SEGMENTS`] (extras merge into
/// the last segment so the 4-breakpoint API budget is never exceeded).
fn split_system_segments(system_prompt: &str) -> Vec<String> {
    let mut segments: Vec<String> = system_prompt
        .split(CACHE_SPLIT_MARKER)
        .map(normalize_system_prompt)
        .filter(|s| !s.trim().is_empty())
        .collect();

    if segments.is_empty() {
        // Preserve the pre-split behavior of always sending one system block.
        segments.push(normalize_system_prompt(system_prompt));
    }
    while segments.len() > MAX_SYSTEM_SEGMENTS {
        let tail = segments.pop().expect("len checked above");
        let last = segments.last_mut().expect("len checked above");
        last.push_str(&tail);
    }
    segments
}

/// Non-cryptographic 64-bit content hash for prefix-change detection.
fn hash64(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// Last-seen system-segment hashes per attribution scope.
static ATTRIBUTION_STATE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, Vec<u64>>>,
> = std::sync::OnceLock::new();

/// Counters of invalidation causes per scope: (scope, cause) → count.
static ATTRIBUTION_COUNTS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<(String, String), u64>>,
> = std::sync::OnceLock::new();

/// Compare this request's cached-prefix hashes with the previous request in
/// the same scope and record why (or whether) the prefix was invalidated.
fn note_cache_attribution(scope: &str, segments: &[String]) {
    let hashes: Vec<u64> = segments.iter().map(|s| hash64(s)).collect();

    let cause: String = {
        let state = ATTRIBUTION_STATE.get_or_init(Default::default);
        let mut state = match state.lock() {
            Ok(g) => g,
            Err(_) => return, // poisoned — attribution is best-effort
        };
        match state.insert(scope.to_string(), hashes.clone()) {
            None => "cold".to_string(),
            Some(prev) if prev == hashes => "none".to_string(),
            Some(prev) if prev.len() != hashes.len() => "layout".to_string(),
            Some(prev) => {
                let first_diff = prev
                    .iter()
                    .zip(&hashes)
                    .position(|(a, b)| a != b)
                    .unwrap_or(0);
                format!("system_block_{first_diff}")
            }
        }
    };

    if cause != "none" && cause != "cold" {
        info!(scope, %cause, "Direct API cached prefix invalidated");
    }

    let counts = ATTRIBUTION_COUNTS.get_or_init(Default::default);
    if let Ok(mut counts) = counts.lock() {
        *counts.entry((scope.to_string(), cause)).or_insert(0) += 1;
    }
}

/// Snapshot of cache-invalidation causes: `(scope, cause, count)` rows.
///
/// `cause` ∈ `cold` (first request), `none` (full prefix hit expected),
/// `system_block_N` (block N changed — earlier blocks still cache-hit),
/// `layout` (segment count changed — whole prefix invalidated).
pub fn cache_attribution_snapshot() -> Vec<(String, String, u64)> {
    let counts = ATTRIBUTION_COUNTS.get_or_init(Default::default);
    match counts.lock() {
        Ok(counts) => {
            let mut rows: Vec<(String, String, u64)> = counts
                .iter()
                .map(|((scope, cause), n)| (scope.clone(), cause.clone(), *n))
                .collect();
            rows.sort();
            rows
        }
        Err(_) => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// MoA virtual models (`moa:<name>`) — API-mode-only routing
// ---------------------------------------------------------------------------

/// Collect the distinct provider ids referenced by a MoA spec's members
/// (proposers + aggregator). Members without a `provider/` prefix yield
/// nothing here — `complete_moa_model`'s own `provider_for` lookup then
/// fails closed with an explicit error, never a silent fallback.
pub fn moa_member_providers(spec: &duduclaw_llm::MoaSpec) -> Vec<String> {
    let mut out: Vec<String> = spec
        .proposers
        .iter()
        .chain(std::iter::once(&spec.aggregator))
        .filter_map(|m| duduclaw_llm::split_model_id(m).0.map(str::to_string))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Execute a `moa:<name>` virtual model through the duduclaw-llm MoA
/// executor (API mode only — MoA has no CLI equivalent).
///
/// Provider clients for every member provider are pre-resolved here (the
/// executor's lookup closure is sync): API key from the account rotator's
/// pool for that provider first, then the provider's standard env var.
/// Providers with no key are simply absent from the map — the executor
/// fails closed per member with an explicit error.
///
/// 2026-07 HIGH-B fixes:
/// - **Cost visibility**: every member client is wrapped in
///   [`crate::claude_runner::RecordingProvider`], so each proposer AND the
///   aggregator call is billed to CostTelemetry under `agent_id` with the
///   member model id (previously MoA usage was invisible).
/// - **Multi-turn context**: `conversation_history` (same `(role, content)`
///   pairs the non-MoA [`call_direct_api`] path threads) is replayed into the
///   request instead of collapsing every turn into per-turn amnesia.
///
/// **Streaming (2026-07, deliberately unwired):** `duduclaw_llm` ships a
/// streaming twin (`stream_moa_model`), but no gateway surface consumes
/// `duduclaw_llm::StreamEvent` streams today — channel replies and webchat
/// deliver buffered text (webchat's `assistant_chunk` protocol variant has no
/// producer), and the gateway's only `stream()` impls are pass-through trait
/// completeness (`local_llm` / `RecordingProvider`). The sole live
/// `StreamEvent` consumer in the workspace is the `duduclaw proxy` SSE
/// endpoint (duduclaw-cli `proxy.rs`), which is not a gateway path and does
/// not route `moa:` ids. Wire `stream_moa_model` here (mirroring this
/// function's RecordingProvider telemetry + history handling) only once a
/// real gateway streaming surface exists — do not invent one for MoA alone.
pub async fn call_moa_model(
    home_dir: &std::path::Path,
    agent_id: &str,
    request_type: crate::cost_telemetry::RequestType,
    model_id: &str,
    system_prompt: &str,
    user_message: &str,
    conversation_history: &[(String, String)],
) -> Result<String, String> {
    use std::sync::Arc;

    let name = duduclaw_llm::moa_name(model_id)
        .ok_or_else(|| format!("`{model_id}` 不是 MoA 模型 id（格式應為 `moa:<name>`）"))?;
    let registry = crate::cost_telemetry::model_registry();
    let spec = registry.moa_spec(name).ok_or_else(|| {
        format!(
            "未定義的 MoA ensemble `{name}` — 請在 ~/.duduclaw/models.toml 加入 [moa.{name}] 區段"
        )
    })?;

    // Pre-resolve one client per member provider (rotator pool → env var).
    let rotator = crate::claude_runner::get_rotator_cached(home_dir).await.ok();
    let mut providers: std::collections::HashMap<String, Arc<dyn duduclaw_llm::ChatProvider>> =
        std::collections::HashMap::new();
    for provider_id in moa_member_providers(spec) {
        let key = match &rotator {
            Some(r) => match r.select_for_provider(&provider_id).await.and_then(|env| env.raw_key)
            {
                Some(k) => Some(k),
                None => duduclaw_llm::resolve_env_key(&provider_id),
            },
            None => duduclaw_llm::resolve_env_key(&provider_id),
        };
        let Some(key) = key else {
            debug!(provider = %provider_id, "MoA member provider has no API key — member will fail closed");
            continue;
        };
        if let Some(client) =
            duduclaw_llm::providers::build_provider(&provider_id, duduclaw_llm::ApiAuth::new(key))
        {
            // Telemetry wrapper: bills each member round-trip under the
            // calling agent, model = the member model actually used.
            providers.insert(
                provider_id,
                Arc::new(crate::claude_runner::RecordingProvider::new(
                    client,
                    agent_id,
                    request_type,
                )),
            );
        }
    }

    // Normalized request: system prompt (cache markers stripped — member
    // providers each have their own caching semantics) + the real session
    // history + the current user turn (mirrors `call_direct_api`'s message
    // construction).
    let system_text = system_prompt.replace(CACHE_SPLIT_MARKER, "");
    let mut req = duduclaw_llm::ChatRequest::new(model_id);
    if !system_text.trim().is_empty() {
        req.system = vec![duduclaw_llm::SystemBlock::uncached(system_text)];
    }
    req.messages = build_moa_messages(conversation_history, user_message);
    req.max_tokens = 8192;

    // `complete_moa_model` borrows a `&dyn Fn` lookup across its awaits, which
    // makes its future non-Send — run it on a blocking thread via
    // `Handle::block_on` (no Send bound) so this outer future stays Send for
    // the channel-reply pipeline.
    let model_id_owned = model_id.to_string();
    let handle = tokio::runtime::Handle::current();
    let moa = tokio::task::spawn_blocking(move || {
        let lookup = move |pid: &str| providers.get(pid).cloned();
        handle.block_on(duduclaw_llm::complete_moa_model(
            &model_id_owned,
            &req,
            crate::cost_telemetry::model_registry(),
            &lookup,
        ))
    })
    .await
    .map_err(|e| format!("MoA 執行緒失敗：{e}"))?
    .map_err(|e| format!("MoA ensemble `{name}` 執行失敗：{e}"))?;

    if moa.degraded {
        for (member, err) in &moa.proposer_errors {
            debug!(ensemble = %moa.ensemble, member = %member, error = %err, "MoA proposer failed");
        }
    }
    info!(
        ensemble = %moa.ensemble,
        proposals = moa.proposals_used,
        degraded = moa.degraded,
        input_tokens = moa.response.usage.input_tokens,
        output_tokens = moa.response.usage.output_tokens,
        "MoA ensemble completed"
    );
    Ok(moa.response.text())
}

/// Build the MoA request's message list from the session history plus the
/// current user turn — the same `(role, content)` pairs and ordering the
/// non-MoA [`call_direct_api`] path sends. Pure (unit-tested below).
///
/// Role mapping is fail-safe: `"assistant"` (case-insensitive) maps to the
/// assistant role; anything else — including unknown strings — is treated as
/// user content, never dropped.
fn build_moa_messages(
    conversation_history: &[(String, String)],
    user_message: &str,
) -> Vec<duduclaw_llm::ChatMessage> {
    let mut messages = Vec::with_capacity(conversation_history.len() + 1);
    for (role, content) in conversation_history {
        if role.eq_ignore_ascii_case("assistant") {
            messages.push(duduclaw_llm::ChatMessage::assistant(content.clone()));
        } else {
            messages.push(duduclaw_llm::ChatMessage::user(content.clone()));
        }
    }
    messages.push(duduclaw_llm::ChatMessage::user(user_message));
    messages
}

// ---------------------------------------------------------------------------
// System prompt normalization
// ---------------------------------------------------------------------------

/// Normalize system prompt for deterministic serialization.
///
/// Eliminates sources of byte-level variation that break cache prefix matching:
/// - Trailing whitespace
/// - Multiple consecutive blank lines → single blank line
/// - Trailing newline normalization
///
/// Does NOT remove content — only normalizes formatting.
fn normalize_system_prompt(prompt: &str) -> String {
    let mut result = String::with_capacity(prompt.len());
    let mut blank_count = 0u32;

    for line in prompt.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            blank_count += 1;
            if blank_count <= 1 {
                result.push('\n');
            }
        } else {
            blank_count = 0;
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str(trimmed);
        }
    }

    // Ensure single trailing newline
    if !result.ends_with('\n') {
        result.push('\n');
    }
    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_removes_trailing_whitespace() {
        let input = "Hello   \nWorld  \n";
        let result = normalize_system_prompt(input);
        assert_eq!(result, "Hello\nWorld\n");
    }

    #[test]
    fn normalize_collapses_blank_lines() {
        let input = "Section 1\n\n\n\nSection 2\n";
        let result = normalize_system_prompt(input);
        assert_eq!(result, "Section 1\n\nSection 2\n");
    }

    #[test]
    fn normalize_ensures_trailing_newline() {
        let input = "No trailing newline";
        let result = normalize_system_prompt(input);
        assert!(result.ends_with('\n'));
    }

    #[test]
    fn normalize_is_deterministic() {
        let input1 = "# Soul\nHello world  \n\n\n# Skills\nTranslate\n";
        let input2 = "# Soul\nHello world\n\n# Skills\nTranslate\n";
        assert_eq!(normalize_system_prompt(input1), normalize_system_prompt(input2));
    }

    #[test]
    fn split_without_marker_is_single_block() {
        let segs = split_system_segments("# Soul\nHello\n");
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0], normalize_system_prompt("# Soul\nHello\n"));
    }

    #[test]
    fn split_on_marker_yields_layered_blocks() {
        let prompt = format!("# Static\nsoul\n{CACHE_SPLIT_MARKER}\n# Semi\nwiki\n");
        let segs = split_system_segments(&prompt);
        assert_eq!(segs.len(), 2);
        assert!(segs[0].contains("Static"));
        assert!(segs[1].contains("Semi"));
    }

    #[test]
    fn split_caps_segments_at_breakpoint_budget() {
        let prompt = (0..5)
            .map(|i| format!("block {i}"))
            .collect::<Vec<_>>()
            .join(CACHE_SPLIT_MARKER);
        let segs = split_system_segments(&prompt);
        assert_eq!(segs.len(), MAX_SYSTEM_SEGMENTS);
        // Overflow blocks merged into the last segment.
        assert!(segs[2].contains("block 2") && segs[2].contains("block 4"));
    }

    #[test]
    fn attribution_identifies_changed_block() {
        let scope = "test-attrib-scope";
        let stable = "soul".to_string();
        note_cache_attribution(scope, &[stable.clone(), "wiki v1".to_string()]);
        note_cache_attribution(scope, &[stable.clone(), "wiki v1".to_string()]);
        note_cache_attribution(scope, &[stable.clone(), "wiki v2".to_string()]);
        note_cache_attribution(scope, &[stable, "wiki v2".to_string(), "x".to_string()]);

        let rows = cache_attribution_snapshot();
        let get = |cause: &str| {
            rows.iter()
                .find(|(s, c, _)| s == scope && c == cause)
                .map(|(_, _, n)| *n)
                .unwrap_or(0)
        };
        assert_eq!(get("cold"), 1);
        assert_eq!(get("none"), 1);
        assert_eq!(get("system_block_1"), 1);
        assert_eq!(get("layout"), 1);
    }

    #[test]
    fn moa_messages_thread_history_then_current_turn() {
        // HIGH-B regression: MoA used to send ONLY the current user turn.
        let history = vec![
            ("user".to_string(), "第一輪問題".to_string()),
            ("assistant".to_string(), "first answer".to_string()),
            ("ASSISTANT".to_string(), "case-insensitive".to_string()),
            ("weird-role".to_string(), "fail-safe as user".to_string()),
        ];
        let msgs = build_moa_messages(&history, "現在的問題");
        assert_eq!(msgs.len(), 5);
        assert_eq!(msgs[0].role, duduclaw_llm::Role::User);
        assert_eq!(msgs[1].role, duduclaw_llm::Role::Assistant);
        assert_eq!(msgs[2].role, duduclaw_llm::Role::Assistant);
        assert_eq!(msgs[3].role, duduclaw_llm::Role::User);
        // Current turn is always the last user message.
        assert_eq!(msgs[4].role, duduclaw_llm::Role::User);
        assert_eq!(
            msgs[4].parts,
            vec![duduclaw_llm::ContentPart::Text("現在的問題".to_string())]
        );
        // Empty history == pre-fix shape (single user turn).
        let solo = build_moa_messages(&[], "hi");
        assert_eq!(solo.len(), 1);
        assert_eq!(solo[0].role, duduclaw_llm::Role::User);
    }

    #[test]
    fn api_usage_to_token_usage() {
        let api = ApiUsage {
            input_tokens: 100,
            output_tokens: 50,
            cache_read_input_tokens: 15000,
            cache_creation_input_tokens: 0,
        };
        let tu: TokenUsage = api.into();
        assert_eq!(tu.input_tokens, 100);
        assert_eq!(tu.cache_read_tokens, 15000);
        assert_eq!(tu.output_tokens, 50);
        assert!(tu.cache_efficiency() > 0.99);
    }
}
