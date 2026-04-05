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
}

/// A complete rich message ready for platform rendering.
#[derive(Debug, Clone)]
pub struct RichMessage {
    pub components: Vec<RichComponent>,
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

/// Format a reply as a Discord message payload (JSON).
///
/// - Short replies (<200 chars, no code blocks) → plain text
/// - Long replies → embed with amber accent
/// - Splits at 4096 chars for embed description
pub fn to_discord_message(text: &str, agent_name: Option<&str>, error: bool) -> Value {
    let color = if error { ERROR_COLOR } else { DUDUCLAW_COLOR };
    let footer_text = agent_name
        .map(|n| format!("DuDuClaw \u{00b7} {n}"))
        .unwrap_or_else(|| "DuDuClaw".to_string());

    // Short, simple replies → plain text
    if text.len() < 200 && !text.contains("```") && !error {
        return json!({ "content": text });
    }

    // Long replies → embed(s)
    let chunks = split_text(text, limits::DISCORD_EMBED_DESC);
    let embeds: Vec<Value> = chunks.iter().enumerate().map(|(i, chunk)| {
        let mut embed = json!({
            "description": chunk,
            "color": color,
        });
        // Footer only on last embed
        if i == chunks.len() - 1 {
            embed["footer"] = json!({ "text": footer_text });
        }
        embed
    }).collect();

    // Discord allows max 10 embeds per message
    json!({ "embeds": &embeds[..embeds.len().min(10)] })
}

// ── Telegram formatting ────────────────────────────────────────

// ── LINE formatting ────────────────────────────────────────────

/// Format a reply as a LINE Flex Message (card-style).
pub fn to_line_flex_message(text: &str, agent_name: Option<&str>) -> Value {
    let footer_text = agent_name
        .map(|n| format!("DuDuClaw \u{00b7} {n}"))
        .unwrap_or_else(|| "DuDuClaw".to_string());

    // Short replies → plain text
    if text.len() < 200 && !text.contains("```") {
        return json!({
            "type": "text",
            "text": text
        });
    }

    // Longer replies → Flex Message bubble
    json!({
        "type": "flex",
        "altText": truncate_chars(text, 200),
        "contents": {
            "type": "bubble",
            "body": {
                "type": "box",
                "layout": "vertical",
                "contents": [{
                    "type": "text",
                    "text": text,
                    "wrap": true,
                    "size": "sm"
                }]
            },
            "footer": {
                "type": "box",
                "layout": "vertical",
                "contents": [{
                    "type": "text",
                    "text": footer_text,
                    "size": "xxs",
                    "color": "#999999",
                    "align": "end"
                }]
            }
        }
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
}
