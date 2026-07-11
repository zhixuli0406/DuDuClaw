//! Unified message formatting for cross-channel rich replies.
//!
//! Converts AI reply text into platform-native rich message formats:
//! Discord Embeds, Telegram MarkdownV2, LINE Flex Messages, Slack Block Kit.

use serde_json::{json, Value};

/// Platform-aware message limits.
pub mod limits {
    pub const DISCORD_MESSAGE: usize = 2000;
    pub const DISCORD_EMBED_DESC: usize = 4096;
    pub const SLACK_MESSAGE: usize = 4000;
    pub const TELEGRAM_MESSAGE: usize = 4096;
    pub const LINE_MESSAGE: usize = 5000;
}

/// A rich message component that can be rendered to any platform.
#[derive(Debug, Clone)]
pub enum RichComponent {
    /// Plain text content.
    Text(String),
    /// Embed / card with optional title, description, color, footer.
    Embed {
        title: Option<String>,
        description: String,
        color: Option<u32>,
        footer: Option<String>,
    },
    /// WP16: a row of action buttons (approval decisions, etc.). Channels with
    /// native buttons (Telegram inline keyboard / Slack Block Kit actions /
    /// Discord components / LINE quick reply) render them natively; channels
    /// without button support fall back to a numbered-command text hint.
    Buttons(Vec<ActionButton>),
}

/// Visual weight of an action button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonStyle {
    /// The affirmative / primary action (approve).
    Primary,
    /// A destructive / caution action (deny, reject).
    Danger,
    /// Everything else.
    Neutral,
}

/// A single actionable button. `action_id` is the opaque token the channel
/// returns on click; keep it short (Telegram caps callback_data at 64 bytes).
#[derive(Debug, Clone)]
pub struct ActionButton {
    pub label: String,
    pub action_id: String,
    pub style: ButtonStyle,
}

impl ActionButton {
    pub fn new(label: impl Into<String>, action_id: impl Into<String>, style: ButtonStyle) -> Self {
        Self { label: label.into(), action_id: action_id.into(), style }
    }
}

/// A complete rich message ready for platform rendering.
#[derive(Debug, Clone)]
pub struct RichMessage {
    pub components: Vec<RichComponent>,
}

/// Per-scope reply rendering mode (the `response_mode` channel setting).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResponseMode {
    /// Short replies plain, long replies embed (current heuristic).
    #[default]
    Auto,
    /// Always plain-text messages, never embeds.
    Plain,
    /// Always embed(s), even for short replies.
    Embed,
}

impl ResponseMode {
    /// Parse the stored setting value; unknown values fall back to Auto.
    pub fn parse(value: &str) -> Self {
        match value {
            "plain" => Self::Plain,
            "embed" => Self::Embed,
            _ => Self::Auto,
        }
    }
}

impl RichMessage {
    pub fn text(content: impl Into<String>) -> Self {
        Self { components: vec![RichComponent::Text(content.into())] }
    }

    pub fn embed(description: impl Into<String>) -> Self {
        Self {
            components: vec![RichComponent::Embed {
                title: None,
                description: description.into(),
                color: None,
                footer: None,
            }],
        }
    }

}

// ── Discord formatting ─────────────────────────────────────────

/// Brand color for DuDuClaw embeds (warm amber).
const DUDUCLAW_COLOR: u32 = 0xF59E0B;
/// Error embed color.
const ERROR_COLOR: u32 = 0xFF4444;

/// Max embeds Discord accepts in a single message.
const DISCORD_MAX_EMBEDS_PER_MSG: usize = 10;
/// Max aggregate characters across all embeds in a single message.
const DISCORD_EMBED_AGGREGATE: usize = 6000;

/// Format a reply as a single Discord message payload (JSON).
///
/// Backwards-compatible single-payload entry point. For long replies this
/// returns the FIRST message produced by [`to_discord_messages`]; callers that
/// must not drop overflow should prefer [`to_discord_messages`].
///
/// - Short replies (<200 chars, no code blocks) → plain text
/// - Long replies → embed(s) with amber accent
pub fn to_discord_message(text: &str, agent_name: Option<&str>, error: bool) -> Value {
    to_discord_messages(text, agent_name, error)
        .into_iter()
        .next()
        .unwrap_or_else(|| json!({ "content": "" }))
}

/// Format a reply as one OR MORE Discord message payloads (JSON).
///
/// HC2: a single Discord message is capped at 10 embeds AND 6000 aggregate
/// embed characters. The previous single-message formatter silently dropped
/// the 10th+ embed (`&embeds[..min(10)]`) and ignored the 6000-char cap.
/// This splits the embeds across as many messages as needed so nothing is lost.
/// Each returned `Value` is a complete message body ready to POST.
pub fn to_discord_messages(text: &str, agent_name: Option<&str>, error: bool) -> Vec<Value> {
    to_discord_messages_mode(text, agent_name, error, ResponseMode::Auto)
}

/// Like [`to_discord_messages`] but honouring an explicit [`ResponseMode`]
/// (the per-scope `response_mode` channel setting).
pub fn to_discord_messages_mode(
    text: &str,
    agent_name: Option<&str>,
    error: bool,
    mode: ResponseMode,
) -> Vec<Value> {
    // Discord renders standard markdown natively EXCEPT pipe tables —
    // downgrade those to monospace code fences (no-op when no table).
    let text = &crate::markdown_render::preprocess_discord_markdown(text);
    let color = if error { ERROR_COLOR } else { DUDUCLAW_COLOR };
    let footer_text = agent_name
        .map(|n| format!("DuDuClaw \u{00b7} {n}"))
        .unwrap_or_else(|| "DuDuClaw".to_string());

    // Plain mode → plain-text messages only, split at the message limit.
    if mode == ResponseMode::Plain && !error {
        return split_text(text, limits::DISCORD_MESSAGE - 100)
            .into_iter()
            .map(|chunk| json!({ "content": chunk }))
            .collect();
    }

    // Short, simple replies → single plain-text message (Auto heuristic;
    // Embed mode forces the embed path even for short replies).
    if mode != ResponseMode::Embed && text.len() < 200 && !text.contains("```") && !error {
        return vec![json!({ "content": text })];
    }

    // Long replies → embed(s). Build all chunks first (never dropped).
    let chunks = split_text(text, limits::DISCORD_EMBED_DESC);
    let last_idx = chunks.len().saturating_sub(1);
    let embeds: Vec<Value> = chunks.iter().enumerate().map(|(i, chunk)| {
        let mut embed = json!({
            "description": chunk,
            "color": color,
        });
        // Footer only on the very last embed of the whole reply.
        if i == last_idx {
            embed["footer"] = json!({ "text": footer_text });
        }
        embed
    }).collect();

    // Pack embeds into messages, respecting both the 10-embeds and the
    // 6000-aggregate-char limits. Each chunk is already ≤ DISCORD_EMBED_DESC
    // (4096) so a single embed always fits in one message on its own.
    let mut messages: Vec<Value> = Vec::new();
    let mut current: Vec<Value> = Vec::new();
    let mut current_chars = 0usize;

    for embed in embeds {
        let embed_chars = embed["description"].as_str().map(|s| s.chars().count()).unwrap_or(0);
        let would_overflow_count = current.len() >= DISCORD_MAX_EMBEDS_PER_MSG;
        let would_overflow_chars =
            !current.is_empty() && current_chars + embed_chars > DISCORD_EMBED_AGGREGATE;

        if would_overflow_count || would_overflow_chars {
            messages.push(json!({ "embeds": std::mem::take(&mut current) }));
            current_chars = 0;
        }

        current_chars += embed_chars;
        current.push(embed);
    }

    if !current.is_empty() {
        messages.push(json!({ "embeds": current }));
    }

    if messages.is_empty() {
        messages.push(json!({ "content": "" }));
    }

    messages
}

// ── Telegram formatting ────────────────────────────────────────

// ── LINE formatting ────────────────────────────────────────────

/// Format a reply as a LINE Flex Message (card-style).
///
/// LINE renders no markup at all, so markdown is first converted to
/// readable plain text (headings → 【】, tables → key-value records,
/// emphasis stripped).
pub fn to_line_flex_message(text: &str, agent_name: Option<&str>) -> Value {
    to_line_flex_message_styled(text, agent_name, false)
}

/// Dark code-panel colors for LINE Flex code blocks (LINE has no monospace
/// font control, so a dark panel visually separates code from prose).
const LINE_CODE_BG: &str = "#1E293B";
const LINE_CODE_FG: &str = "#E2E8F0";
/// Error accent for LINE Flex error replies.
const LINE_ERROR_ACCENT: &str = "#DC2626";

/// Split markdown into alternating prose / fenced-code segments.
/// Returns `(is_code, content)` pairs; fence lines themselves are dropped.
fn split_code_segments(text: &str) -> Vec<(bool, String)> {
    let mut segments: Vec<(bool, String)> = Vec::new();
    let mut current = String::new();
    let mut in_code = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            if !current.trim().is_empty() {
                segments.push((in_code, current.trim_end().to_string()));
            }
            current = String::new();
            in_code = !in_code;
            continue;
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.trim().is_empty() {
        segments.push((in_code, current.trim_end().to_string()));
    }
    segments
}

/// Like [`to_line_flex_message`] with error styling: error replies render as
/// a red-accented bubble, and fenced code blocks render as dark code panels.
pub fn to_line_flex_message_styled(text: &str, agent_name: Option<&str>, error: bool) -> Value {
    let footer_text = agent_name
        .map(|n| format!("DuDuClaw \u{00b7} {n}"))
        .unwrap_or_else(|| "DuDuClaw".to_string());

    // Short prose replies → plain text (errors always get the styled bubble).
    if !error && text.len() < 200 && !text.contains("```") {
        return json!({
            "type": "text",
            "text": crate::markdown_render::to_line_plain(text)
        });
    }

    // Build body components: prose as normal text, code blocks as dark panels.
    let mut body_contents: Vec<Value> = Vec::new();
    if error {
        body_contents.push(json!({
            "type": "text",
            "text": "⚠️ 錯誤",
            "weight": "bold",
            "size": "sm",
            "color": LINE_ERROR_ACCENT
        }));
        body_contents.push(json!({ "type": "separator", "margin": "sm", "color": LINE_ERROR_ACCENT }));
    }
    for (is_code, segment) in split_code_segments(text) {
        if is_code {
            body_contents.push(json!({
                "type": "box",
                "layout": "vertical",
                "backgroundColor": LINE_CODE_BG,
                "cornerRadius": "6px",
                "paddingAll": "8px",
                "margin": "sm",
                "contents": [{
                    "type": "text",
                    "text": segment,
                    "wrap": true,
                    "size": "xs",
                    "color": LINE_CODE_FG
                }]
            }));
        } else {
            let plain = crate::markdown_render::to_line_plain(&segment);
            if plain.trim().is_empty() {
                continue;
            }
            body_contents.push(json!({
                "type": "text",
                "text": plain,
                "wrap": true,
                "size": "sm",
                "margin": "sm"
            }));
        }
    }
    // LINE rejects an empty body box — fall back to a single text component.
    if body_contents.is_empty() {
        body_contents.push(json!({
            "type": "text",
            "text": crate::markdown_render::to_line_plain(text),
            "wrap": true,
            "size": "sm"
        }));
    }

    let alt_plain = crate::markdown_render::to_line_plain(text);
    let mut bubble = json!({
        "type": "bubble",
        "body": {
            "type": "box",
            "layout": "vertical",
            "contents": body_contents
        },
        "footer": {
            "type": "box",
            "layout": "vertical",
            "contents": [{
                "type": "text",
                "text": footer_text,
                "size": "xxs",
                "color": if error { LINE_ERROR_ACCENT } else { "#999999" },
                "align": "end"
            }]
        }
    });
    if error {
        bubble["styles"] = json!({ "body": { "backgroundColor": "#FFF5F5" } });
    }

    json!({
        "type": "flex",
        "altText": truncate_chars(&alt_plain, 200),
        "contents": bubble
    })
}

// ── Slack formatting ───────────────────────────────────────────

/// Format a reply as Slack Block Kit message.
pub fn to_slack_blocks(text: &str) -> Value {
    let blocks = vec![
        json!({
            "type": "section",
            "text": {
                "type": "mrkdwn",
                "text": to_slack_mrkdwn(text)
            }
        })
    ];

    json!({ "blocks": blocks })
}

/// Convert standard markdown to Slack mrkdwn format.
///
/// Handles: bold (**→*), headings (#→*bold*), links ([text](url)→<url|text>).
/// Code blocks and inline code are compatible between formats.
fn to_slack_mrkdwn(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut in_code_block = false;

    for line in text.lines() {
        // Don't transform inside code blocks
        if line.starts_with("```") {
            in_code_block = !in_code_block;
            result.push_str(line);
            result.push('\n');
            continue;
        }
        if in_code_block {
            result.push_str(line);
            result.push('\n');
            continue;
        }

        let mut transformed = line.to_string();

        // Headings: # Title → *Title*
        if let Some(heading) = transformed.strip_prefix("### ") {
            transformed = format!("*{heading}*");
        } else if let Some(heading) = transformed.strip_prefix("## ") {
            transformed = format!("*{heading}*");
        } else if let Some(heading) = transformed.strip_prefix("# ") {
            transformed = format!("*{heading}*");
        }

        // Bold: **text** → *text*
        transformed = transformed.replace("**", "*");

        // Links: [text](url) → <url|text>
        // Simple regex-free approach: find [...](...)
        let mut link_result = String::with_capacity(transformed.len());
        let mut chars = transformed.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '[' {
                // Try to parse [text](url)
                let mut link_text = String::new();
                let mut found_close = false;
                for inner in chars.by_ref() {
                    if inner == ']' { found_close = true; break; }
                    link_text.push(inner);
                }
                if found_close && chars.peek() == Some(&'(') {
                    chars.next(); // consume '('
                    let mut url = String::new();
                    for inner in chars.by_ref() {
                        if inner == ')' { break; }
                        url.push(inner);
                    }
                    link_result.push_str(&format!("<{url}|{link_text}>"));
                } else {
                    link_result.push('[');
                    link_result.push_str(&link_text);
                    if found_close { link_result.push(']'); }
                }
            } else {
                link_result.push(c);
            }
        }

        result.push_str(&link_result);
        result.push('\n');
    }

    // Remove trailing newline
    if result.ends_with('\n') {
        result.pop();
    }
    result
}

// ── Common utilities ───────────────────────────────────────────

/// Find the largest byte index ≤ `max_byte` that sits on a valid UTF-8
/// character boundary. Safe for CJK and emoji text.
fn safe_byte_boundary(text: &str, max_byte: usize) -> usize {
    let mut end = max_byte.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// Truncate a string to at most `max_chars` characters (not bytes).
/// Safe for CJK / multi-byte text.
pub fn truncate_chars(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

/// Split text into chunks respecting code block and newline boundaries.
/// All byte offsets are snapped to valid UTF-8 character boundaries.
pub fn split_text(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut start = 0;
    let mut pending_code_prefix = String::new();

    while start < text.len() {
        let remaining = &text[start..];
        if remaining.len() <= max_len {
            let mut chunk = remaining.to_string();
            if !pending_code_prefix.is_empty() {
                chunk = format!("{pending_code_prefix}{chunk}");
                pending_code_prefix.clear();
            }
            chunks.push(chunk);
            break;
        }

        let search_end = safe_byte_boundary(text, start + max_len);

        // Guard against zero-progress (should not happen with safe_byte_boundary,
        // but protect against pathological inputs).
        if search_end <= start {
            // Force at least one character forward
            let next = text[start..].char_indices().nth(1).map(|(i, _)| start + i).unwrap_or(text.len());
            chunks.push(text[start..next].to_string());
            start = next;
            continue;
        }

        let search_range = &text[start..search_end];

        // Find best split point (prefer paragraph/line boundaries)
        let split_at = if let Some(pos) = search_range.rfind("\n\n") {
            start + pos + 2
        } else if let Some(pos) = search_range.rfind('\n') {
            start + pos + 1
        } else {
            search_end
        };

        // Ensure forward progress
        let split_at = if split_at <= start { search_end } else { split_at };

        let raw_chunk = &text[start..split_at];

        // Count ``` in the raw chunk only (exclude pending prefix) to avoid count pollution
        let in_code_block = raw_chunk.matches("```").count() % 2 == 1;

        let mut chunk = raw_chunk.to_string();
        if !pending_code_prefix.is_empty() {
            chunk = format!("{pending_code_prefix}{chunk}");
            pending_code_prefix.clear();
        }

        if in_code_block {
            chunks.push(format!("{chunk}\n```"));
            pending_code_prefix = "```\n".to_string();
        } else {
            chunks.push(chunk);
        }

        start = split_at;
    }

    chunks
}

// ── Conversation buttons ──────────────────────────────────────

/// Discord action row with conversation control buttons.
///
/// `custom_id` format: `duduclaw:{action}[:{session_id}]` — parsed by
/// `discord::handle_component_interaction`. Discord caps custom_id at 100
/// chars; session ids (`discord:thread:{snowflake}`) fit comfortably.
pub fn discord_conversation_buttons(session_id: &str) -> Value {
    json!({
        "type": 1,
        "components": [
            {
                "type": 2,
                "style": 2,
                "label": "🔄 New Session",
                "custom_id": format!("duduclaw:new_session:{session_id}")
            },
            {
                "type": 2,
                "style": 2,
                "label": "🤖 Switch Agent",
                "custom_id": "duduclaw:agent_menu"
            }
        ]
    })
}

/// Discord action row with a select menu for agent switching.
/// Sent as an ephemeral response to the "Switch Agent" button.
pub fn discord_agent_select_menu(agent_names: &[String]) -> Value {
    let options: Vec<Value> = agent_names
        .iter()
        .take(25) // Discord select menu hard cap
        .map(|name| json!({ "label": truncate_chars(name, 100), "value": truncate_chars(name, 100) }))
        .collect();
    json!({
        "type": 1,
        "components": [{
            "type": 3,
            "custom_id": "duduclaw:agent_select",
            "placeholder": "選擇要切換的 Agent",
            "options": options
        }]
    })
}

/// Telegram inline keyboard with conversation control buttons.
pub fn telegram_conversation_buttons() -> Value {
    json!({
        "inline_keyboard": [[
            { "text": "🔄 New Session", "callback_data": "duduclaw:new_session" },
            { "text": "🎤 Voice Toggle", "callback_data": "duduclaw:voice_toggle" }
        ]]
    })
}

/// Slack Block Kit `actions` block with conversation control buttons.
/// `action_id` mirrors the Discord custom_id convention; the session id is
/// carried in `value` and handled by the `interactive` Socket-Mode envelope.
pub fn slack_action_buttons(session_id: &str) -> Value {
    json!({
        "type": "actions",
        "elements": [{
            "type": "button",
            "text": { "type": "plain_text", "text": "🔄 New Session" },
            "action_id": "duduclaw:new_session",
            "value": session_id
        }]
    })
}

/// LINE quickReply payload with conversation control buttons.
/// Attached to the LAST message of a reply (LINE shows quickReply only on
/// the most recent message); handled by the `postback` webhook event.
pub fn line_quick_reply() -> Value {
    json!({
        "items": [{
            "type": "action",
            "action": {
                "type": "postback",
                "label": "🔄 新對話",
                "data": "duduclaw:new_session",
                "displayText": "開啟新對話"
            }
        }]
    })
}

// ── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_discord_short_reply_plain_text() {
        let msg = to_discord_message("Hello!", None, false);
        assert_eq!(msg["content"], "Hello!");
        assert!(msg.get("embeds").is_none());
    }

    #[test]
    fn test_discord_long_reply_embed() {
        let long_text = "a".repeat(300);
        let msg = to_discord_message(&long_text, Some("test-agent"), false);
        assert!(msg.get("embeds").is_some());
        let embed = &msg["embeds"][0];
        assert_eq!(embed["color"], DUDUCLAW_COLOR);
        assert!(embed["footer"]["text"].as_str().unwrap().contains("test-agent"));
    }

    #[test]
    fn test_discord_error_embed() {
        let msg = to_discord_message("Error occurred", None, true);
        let embed = &msg["embeds"][0];
        assert_eq!(embed["color"], ERROR_COLOR);
    }

    #[test]
    fn test_split_text_short() {
        let chunks = split_text("hello", 100);
        assert_eq!(chunks, vec!["hello"]);
    }

    #[test]
    fn test_split_text_respects_newlines() {
        let text = "line1\nline2\nline3\nline4";
        let chunks = split_text(text, 12);
        assert!(chunks.len() >= 2);
        // Each chunk should end at a newline boundary
        for chunk in &chunks[..chunks.len()-1] {
            assert!(chunk.ends_with('\n'));
        }
    }

    #[test]
    fn test_line_short_reply() {
        let msg = to_line_flex_message("Hi!", None);
        assert_eq!(msg["type"], "text");
    }

    #[test]
    fn test_line_long_reply_flex() {
        let long_text = "a".repeat(300);
        let msg = to_line_flex_message(&long_text, None);
        assert_eq!(msg["type"], "flex");
    }

    #[test]
    fn test_slack_blocks() {
        let msg = to_slack_blocks("**hello**");
        let block = &msg["blocks"][0];
        assert_eq!(block["text"]["text"], "*hello*");
    }

    // ── HC2: Discord embed splitting ───────────────────────────────

    #[test]
    fn test_discord_messages_short_plain() {
        let msgs = to_discord_messages("Hi!", None, false);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["content"], "Hi!");
    }

    #[test]
    fn test_discord_messages_no_drop_over_ten_embeds() {
        // Each chunk is ~4096 chars → one embed each. 12 chunks * 4096 ≈ 49k
        // chars: well over both the 10-embed and 6000-char caps, forcing
        // several messages. Crucially, NO embed may be dropped.
        let big = "a".repeat(limits::DISCORD_EMBED_DESC * 12);
        let msgs = to_discord_messages(&big, Some("agent"), false);
        assert!(msgs.len() > 1, "expected multiple messages, got {}", msgs.len());

        let mut total_embeds = 0usize;
        for m in &msgs {
            let embeds = m["embeds"].as_array().expect("each message has embeds");
            // Never exceed Discord's 10-embed-per-message hard limit.
            assert!(embeds.len() <= 10, "message exceeds 10 embeds: {}", embeds.len());
            // Never exceed the 6000 aggregate-char cap (unless a single embed
            // alone is larger, which split_text prevents at 4096).
            let agg: usize = embeds.iter()
                .map(|e| e["description"].as_str().map(|s| s.chars().count()).unwrap_or(0))
                .sum();
            assert!(agg <= 6000, "message exceeds 6000 aggregate chars: {agg}");
            total_embeds += embeds.len();
        }
        // 12 * 4096 chars split at 4096 → at least 12 embeds, all preserved.
        assert!(total_embeds >= 12, "embeds were dropped: only {total_embeds}");

        // Footer must appear exactly once, on the final embed.
        let last_msg = msgs.last().unwrap();
        let last_embeds = last_msg["embeds"].as_array().unwrap();
        let last_embed = last_embeds.last().unwrap();
        assert!(last_embed["footer"]["text"].as_str().unwrap().contains("agent"));
    }

    #[test]
    fn test_discord_message_single_compat() {
        // Backwards-compat: single-payload form still returns a valid body.
        let msg = to_discord_message("Error", None, true);
        assert!(msg.get("embeds").is_some());
    }

    // ── response_mode consumer ─────────────────────────────────────

    #[test]
    fn test_response_mode_parse() {
        assert_eq!(ResponseMode::parse("plain"), ResponseMode::Plain);
        assert_eq!(ResponseMode::parse("embed"), ResponseMode::Embed);
        assert_eq!(ResponseMode::parse("auto"), ResponseMode::Auto);
        assert_eq!(ResponseMode::parse("garbage"), ResponseMode::Auto);
    }

    #[test]
    fn test_discord_plain_mode_never_embeds() {
        let long_text = "a".repeat(300);
        let msgs = to_discord_messages_mode(&long_text, Some("agent"), false, ResponseMode::Plain);
        for m in &msgs {
            assert!(m.get("embeds").is_none(), "plain mode must not produce embeds");
            assert!(m.get("content").is_some());
        }
    }

    #[test]
    fn test_discord_embed_mode_forces_embed_for_short() {
        let msgs = to_discord_messages_mode("Hi!", None, false, ResponseMode::Embed);
        assert!(msgs[0].get("embeds").is_some(), "embed mode must embed even short replies");
    }

    #[test]
    fn test_discord_plain_mode_error_still_embeds() {
        // Errors keep the red embed even in plain mode so they stay visible.
        let msgs = to_discord_messages_mode("boom", None, true, ResponseMode::Plain);
        assert!(msgs[0].get("embeds").is_some());
    }

    // ── LINE Flex styling ──────────────────────────────────────────

    #[test]
    fn test_split_code_segments() {
        let md = "before\n```rust\nlet x = 1;\n```\nafter";
        let segs = split_code_segments(md);
        assert_eq!(segs.len(), 3);
        assert!(!segs[0].0 && segs[0].1.contains("before"));
        assert!(segs[1].0 && segs[1].1.contains("let x = 1;"));
        assert!(!segs[2].0 && segs[2].1.contains("after"));
    }

    #[test]
    fn test_line_flex_code_block_gets_dark_panel() {
        let md = format!("說明文字\n```\ncode line\n```\n{}", "尾".repeat(300));
        let msg = to_line_flex_message(&md, Some("agent"));
        assert_eq!(msg["type"], "flex");
        let contents = msg["contents"]["body"]["contents"].as_array().unwrap();
        let has_code_panel = contents.iter().any(|c| c["backgroundColor"] == LINE_CODE_BG);
        assert!(has_code_panel, "code block should render as a dark panel box");
    }

    #[test]
    fn test_line_flex_error_variant_red_accent() {
        let msg = to_line_flex_message_styled("something failed", None, true);
        assert_eq!(msg["type"], "flex", "errors always render as flex");
        let contents = msg["contents"]["body"]["contents"].as_array().unwrap();
        assert_eq!(contents[0]["text"], "⚠️ 錯誤");
        assert_eq!(contents[0]["color"], LINE_ERROR_ACCENT);
        assert_eq!(msg["contents"]["styles"]["body"]["backgroundColor"], "#FFF5F5");
    }

    // ── Interactive component builders ─────────────────────────────

    #[test]
    fn test_discord_buttons_custom_ids() {
        let row = discord_conversation_buttons("discord:thread:123");
        let comps = row["components"].as_array().unwrap();
        assert_eq!(comps[0]["custom_id"], "duduclaw:new_session:discord:thread:123");
        assert_eq!(comps[1]["custom_id"], "duduclaw:agent_menu");
    }

    #[test]
    fn test_discord_agent_select_menu_caps_at_25() {
        let agents: Vec<String> = (0..30).map(|i| format!("agent-{i}")).collect();
        let row = discord_agent_select_menu(&agents);
        let options = row["components"][0]["options"].as_array().unwrap();
        assert_eq!(options.len(), 25);
        assert_eq!(row["components"][0]["custom_id"], "duduclaw:agent_select");
    }

    #[test]
    fn test_slack_action_buttons_shape() {
        let block = slack_action_buttons("slack:group:C123");
        assert_eq!(block["type"], "actions");
        assert_eq!(block["elements"][0]["action_id"], "duduclaw:new_session");
        assert_eq!(block["elements"][0]["value"], "slack:group:C123");
    }

    #[test]
    fn test_line_quick_reply_shape() {
        let qr = line_quick_reply();
        assert_eq!(qr["items"][0]["action"]["type"], "postback");
        assert_eq!(qr["items"][0]["action"]["data"], "duduclaw:new_session");
    }
}
