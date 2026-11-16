//! Shared Telegram text delivery. Failure telemetry never includes message bodies.
use std::path::Path;
use serde_json::{Value, json};
use crate::isolation_http::LiveRequest;

const LIMIT: usize = crate::channel_format::limits::TELEGRAM_MESSAGE;

#[derive(Clone, Copy)]
pub(crate) enum Format { MarkdownHtml, LegacyMarkdown, Plain }

pub(crate) struct Options<'a> {
    pub home: &'a Path,
    pub kind: &'a str,
    pub ref_id: &'a str,
    pub thread: Option<i64>,
    pub reply_to: Option<i64>,
    pub markup: Option<Value>,
    pub guarded: Option<&'a crate::channel_reply::GuardedReply>,
    pub literal_prefix: Option<&'a str>,
}
impl<'a> Options<'a> {
    pub fn new(home: &'a Path, kind: &'a str, ref_id: &'a str) -> Self {
        Self { home, kind, ref_id, thread: None, reply_to: None, markup: None, guarded: None, literal_prefix: None }
    }
}

#[derive(Debug)]
pub(crate) struct Failure {
    pub http_status: Option<u16>,
    pub description: String,
    pub content: bool,
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Telegram send failed (status {:?}, kind {}): {}", self.http_status,
            if self.content { "content" } else { "transport_or_access" }, self.description)
    }
}

struct Chunk { text: String, plain: String, mode: Option<&'static str> }

fn plain_chunks(text: &str) -> Vec<Chunk> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        if current.len() + ch.len_utf8() > LIMIT {
            chunks.push(Chunk { plain: current.clone(), text: std::mem::take(&mut current), mode: None });
        }
        current.push(ch);
    }
    if !current.is_empty() { chunks.push(Chunk { plain: current.clone(), text: current, mode: None }); }
    chunks
}

// Tokenize rendered HTML, keeping tags and escaped characters indivisible.
fn html_tokens(html: &str) -> Vec<String> {
    let mut chars = html.chars();
    let mut tokens = Vec::new();
    while let Some(ch) = chars.next() {
        let mut token = ch.to_string();
        if ch == '<' || ch == '&' {
            let end = if ch == '<' { '>' } else { ';' };
            for c in chars.by_ref() { token.push(c); if c == end { break; } }
        }
        tokens.push(token);
    }
    tokens
}
fn visible(token: &str) -> String {
    if token.starts_with('<') { return String::new(); }
    match token { "&amp;" => "&", "&lt;" => "<", "&gt;" => ">", "&quot;" => "\"", "&#39;" | "&apos;" => "'", other => other }.to_owned()
}
fn closing(stack: &[String]) -> String {
    stack.iter().rev().map(|tag| {
        let name = tag.trim_start_matches('<').split([' ', '>']).next().unwrap_or("");
        format!("</{name}>")
    }).collect()
}
fn chunks(text: &str, format: Format, prefix: Option<&str>) -> Vec<Chunk> {
    match format {
        Format::Plain => plain_chunks(&format!("{}{text}", prefix.unwrap_or(""))),
        Format::LegacyMarkdown => {
            // Preserve complete legacy entities. An individual entity larger than
            // Telegram's limit is delivered literally rather than split as markup.
            let mut atoms = Vec::new();
            let mut chars = text.chars().peekable();
            while let Some(ch) = chars.next() {
                let mut atom = ch.to_string();
                if matches!(ch, '*' | '_' | '`' | '[') {
                    let mut fence = 1;
                    if ch == '`' {
                        while chars.peek() == Some(&'`') {
                            atom.push(chars.next().unwrap());
                            fence += 1;
                        }
                    }
                    let end = if ch == '[' { ')' } else { ch };
                    let mut closing_run = 0;
                    let mut escaped = false;
                    for c in chars.by_ref() {
                        atom.push(c);
                        if escaped { escaped = false; closing_run = 0; continue; }
                        if c == '\\' && ch != '`' { escaped = true; continue; }
                        if c == end { closing_run += 1; } else { closing_run = 0; }
                        if closing_run == fence { break; }
                    }
                } else if ch == '\\' {
                    if let Some(c) = chars.next() { atom.push(c); }
                }
                atoms.push(atom);
            }
            let mut result = Vec::new();
            let mut current = String::new();
            for atom in atoms {
                if current.len() + atom.len() > LIMIT && !current.is_empty() {
                    result.push(Chunk { text: current.clone(), plain: std::mem::take(&mut current), mode: Some("Markdown") });
                }
                if atom.len() > LIMIT { result.extend(plain_chunks(&atom)); } else { current.push_str(&atom); }
            }
            if !current.is_empty() { result.push(Chunk { text: current.clone(), plain: current, mode: Some("Markdown") }); }
            result
        }
        Format::MarkdownHtml => {
            let html = format!("{}{}", crate::markdown_render::escape_html(prefix.unwrap_or("")), crate::markdown_render::to_telegram_html(text));
            let tokens = html_tokens(&html);
            let plain: String = tokens.iter().map(|t| visible(t)).collect();
            let mut result = Vec::new();
            let mut stack: Vec<String> = Vec::new();
            let mut current = String::new();
            let mut current_plain = String::new();
            for token in tokens {
                let mut next_stack = stack.clone();
                if token.starts_with("</") { next_stack.pop(); }
                else if token.starts_with('<') { next_stack.push(token.clone()); }
                if current.len() + token.len() + closing(&next_stack).len() > LIMIT {
                    if current_plain.is_empty() { return plain_chunks(&plain); }
                    result.push(Chunk { text: format!("{}{}", current, closing(&stack)), plain: std::mem::take(&mut current_plain), mode: Some("HTML") });
                    current = stack.concat();
                    if current.len() + token.len() + closing(&next_stack).len() > LIMIT { return plain_chunks(&plain); }
                }
                current.push_str(&token);
                current_plain.push_str(&visible(&token));
                stack = next_stack;
            }
            if !current_plain.is_empty() { result.push(Chunk { text: format!("{}{}", current, closing(&stack)), plain: current_plain, mode: Some("HTML") }); }
            result
        }
    }
}

// Keep Telegram's own description, but remove any reflected payload. Unknown
// descriptions are intentionally reduced to a fixed label: they may quote text.
fn safe_description(description: &str) -> String {
    let lower = description.to_ascii_lowercase();
    for label in ["can't parse entities", "message is too long", "message text is empty", "message to be replied not found", "replied message not found", "message not found", "chat not found", "bot was blocked", "forbidden", "unauthorized", "too many requests"] {
        if lower.contains(label) { return label.to_owned(); }
    }
    "Telegram rejected request".to_owned()
}

#[allow(clippy::too_many_arguments)]
async fn post(client: &reqwest::Client, api_base: &str, chat_id: &str, text: &str,
    mode: Option<&str>, options: &Options<'_>, reply: Option<Value>, markup: Option<Value>) -> Result<(), Failure> {
    let mut body = json!({"chat_id":chat_id, "text":text});
    if let Some(mode) = mode { body["parse_mode"] = json!(mode); }
    if let Some(thread) = options.thread { body["message_thread_id"] = json!(thread); }
    if let Some(reply) = reply { body["reply_parameters"] = reply; }
    if let Some(markup) = markup { body["reply_markup"] = markup; }
    for attempt in 1..=3 {
        let resp = client.post(format!("{api_base}/sendMessage")).json(&body).send_authorized().await
            .map_err(|_| Failure { http_status: None, description: "Telegram transport error".into(), content: false })?;
        let status = resp.status().as_u16();
        let data: Value = resp.json().await.map_err(|_| Failure { http_status: Some(status), description: "Invalid Telegram response".into(), content: false })?;
        if (200..300).contains(&status) && data["ok"] == true {
            let id = data["result"]["message_id"].as_i64().map(|v| v.to_string());
            let text = data["result"]["text"].as_str().unwrap_or(if mode.is_none() { text } else { "[文字回覆]" });
            crate::channel_reply::group_buffer::delivered(text, id.as_deref()).await;
            return Ok(());
        }
        let code = data["error_code"].as_u64().unwrap_or(u64::from(status));
        let description = safe_description(data["description"].as_str().unwrap_or(""));
        let content = status == 400 && matches!(description.as_str(), "can't parse entities" | "message is too long" | "message text is empty");
        if attempt == 3 || !(code == 429 || (500..600).contains(&code)) {
            return Err(Failure { http_status: Some(status), description, content });
        }
        let seconds = data["parameters"]["retry_after"].as_u64().filter(|s| *s > 0).unwrap_or(if attempt == 1 {1} else {3});
        tokio::time::sleep(std::time::Duration::from_secs(seconds)).await;
    }
    unreachable!()
}

/// Render and chunk text, preserving HTML entities, then retry only entity
/// rejection as plain text. Reply/thread/buttons and the generation guard survive.
pub(crate) async fn send(client: &reqwest::Client, api_base: &str, chat_id: &str,
    text: &str, format: Format, options: Options<'_>) -> Result<(), Failure> {
    let mut chunks = chunks(text, format, options.literal_prefix);
    if chunks.is_empty() {
        chunks.push(Chunk { text: String::new(), plain: String::new(), mode: None });
    }
    for (i, chunk) in chunks.iter().enumerate() {
        if crate::channel_reply::guard_lost(options.guarded).await {
            return Err(Failure { http_status: None, description: "Reply guard lost".into(), content: false });
        }
        let mut reply = if i == 0 { options.reply_to.map(|id| json!({"message_id":id})) } else { None };
        let markup = if i + 1 == chunks.len() { options.markup.clone() } else { None };
        let mut result = post(client, api_base, chat_id, &chunk.text, chunk.mode, &options, reply.clone(), markup.clone()).await;
        if result.as_ref().err().is_some_and(|e| e.http_status == Some(400) && e.description.contains("message") && e.description.contains("not found")) && reply.is_some() {
            reply = None;
            if !crate::channel_reply::guard_lost(options.guarded).await {
                result = post(client, api_base, chat_id, &chunk.text, chunk.mode, &options, None, markup.clone()).await;
            }
        }
        if chunk.mode.is_some() && result.as_ref().err().is_some_and(|e| e.http_status == Some(400) && e.description == "can't parse entities")
            && !crate::channel_reply::guard_lost(options.guarded).await {
            result = post(client, api_base, chat_id, &chunk.plain, None, &options, reply, markup).await;
        }
        if let Err(error) = result {
            tracing::warn!(kind = options.kind, ref_id = options.ref_id,
                failure_kind = if error.content { "content" } else { "transport_or_access" },
                http_status = ?error.http_status, description = %error.description, "Telegram delivery failed");
            let record = json!({"event":"telegram_send_failed", "channel":"telegram", "chat_id":chat_id,
                "kind":options.kind, "ref_id":options.ref_id, "chunk":i+1, "total_chunks":chunks.len(),
                "http_status":error.http_status, "description":error.description,
                "failure_kind":if error.content {"content"} else {"transport_or_access"}, "timestamp":chrono::Utc::now().to_rfc3339()});
            if let Err(e) = crate::trajectory_guard::append_anomaly(options.home, &record) {
                tracing::warn!(error = %e, "telegram: failure record could not be written");
            }
            return Err(error);
        }
    }
    Ok(())
}

fn cron_notice(name: &str, language: Option<&str>) -> String {
    let primary = language.unwrap_or("zh-TW").split(['-', '_']).next().unwrap_or("zh").to_ascii_lowercase();
    match primary.as_str() {
        "en" => format!("⏰ [{name}] The result could not be delivered. View it under Automations in the dashboard."),
        "ja" => format!("⏰ [{name}] 結果を配信できませんでした。ダッシュボードの「定期タスク」で確認してください。"),
        _ => format!("⏰ [{name}] 的結果無法送達，請到儀表板『例行工作』查看"),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn send_cron(client: &reqwest::Client, api_base: &str, chat_id: &str,
    text: &str, name: &str, id: &str, home: &Path) -> Result<(), Failure> {
    let prefix = format!("⏰ [{name}] ");
    let mut options = Options::new(home, "cron", id);
    options.literal_prefix = Some(&prefix);
    let result = send(client, api_base, chat_id, text, Format::MarkdownHtml, options).await;
    if result.as_ref().err().is_some_and(|e| e.content) && !chat_id.starts_with("odoolivechat:") {
        let name = duduclaw_core::plain_line(name, 120);
        let language = crate::prompt_identity::read_default_language(home).await;
        let notice = cron_notice(&name, language.as_deref());
        let _ = send(client, api_base, chat_id, &notice, Format::Plain, Options::new(home, "cron_notice", id)).await;
    }
    result
}

#[cfg(test)]
#[path = "telegram_send_tests.rs"]
mod tests;
