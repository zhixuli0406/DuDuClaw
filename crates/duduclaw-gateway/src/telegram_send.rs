//! Shared Telegram text delivery. Failure telemetry never includes message bodies.
use std::path::Path;
use std::borrow::Cow;
use std::time::Duration;
use tokio::time::Instant;
use serde_json::{Value, json};
use crate::isolation_http::LiveRequest;

const LIMIT: usize = crate::channel_format::limits::TELEGRAM_MESSAGE;
// Bound synchronous parsing/rendering before the delivery chunk cap applies.
const MAX_INPUT_BYTES: usize = 15 * LIMIT;

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
    pub delivered_chunks: usize,
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
            if chunks.len() == 17 { return chunks; }
        }
        current.push(ch);
    }
    if !current.is_empty() { chunks.push(Chunk { plain: current.clone(), text: current, mode: None }); }
    chunks
}

// Borrow tokens at character boundaries; ordinary characters never allocate.
fn html_tokens(html: &str) -> Vec<&str> {
    let mut chars = html.char_indices().peekable();
    let mut tokens = Vec::new();
    while let Some((start, ch)) = chars.next() {
        let mut end = start + ch.len_utf8();
        if ch == '<' || ch == '&' {
            let terminator = if ch == '<' { '>' } else { ';' };
            for (offset, c) in chars.by_ref() {
                end = offset + c.len_utf8();
                if c == terminator { break; }
            }
        }
        if html.get(start..end).is_some_and(|t| t.starts_with("<a href=")) {
            if let Some(close) = html.get(end..).and_then(|rest| rest.find("</a>")) {
                end += close + "</a>".len();
                while chars.peek().is_some_and(|(offset, _)| *offset < end) { chars.next(); }
            }
        }
        tokens.push(html.get(start..end).expect("character boundaries"));
    }
    tokens
}
fn decode(text: &str) -> String {
    text.replace("&quot;", "\"").replace("&#39;", "'").replace("&apos;", "'")
        .replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}
// A complete anchor is indivisible: fallback of its chunk always keeps its URL.
fn plain_tokens<'a>(tokens: &[&'a str]) -> Vec<Cow<'a, str>> {
    tokens.iter().map(|&token| {
        if let Some((href, label)) = token.strip_prefix("<a href=\"").and_then(|s| s.split_once("\">")) {
            if let Some(label) = label.strip_suffix("</a>") {
                let label: String = html_tokens(label).into_iter().map(visible).collect();
                let url = decode(href);
                return Cow::Owned(if label == url { label } else { format!("{label} ({url})") });
            }
        }
        visible(token)
    }).collect()
}
fn visible(token: &str) -> Cow<'_, str> {
    if token.starts_with('<') { return Cow::Borrowed(""); }
    match token { "&amp;" => "&", "&lt;" => "<", "&gt;" => ">", "&quot;" => "\"", "&#39;" | "&apos;" => "'", other => other }.into()
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
                    if result.len() == 17 { return result; }
                }
                if atom.len() > LIMIT { result.extend(plain_chunks(&atom)); } else { current.push_str(&atom); }
            }
            if !current.is_empty() { result.push(Chunk { text: current.clone(), plain: current, mode: Some("Markdown") }); }
            result
        }
        Format::MarkdownHtml => {
            let html = format!("{}{}", crate::markdown_render::escape_html(prefix.unwrap_or("")), crate::markdown_render::to_telegram_html(text));
            let tokens = html_tokens(&html);
            let plain_tokens = plain_tokens(&tokens);
            let plain: String = plain_tokens.iter().map(|s| s.as_ref()).collect();
            let mut result = Vec::new();
            let mut stack: Vec<String> = Vec::new();
            let mut current = String::new();
            let mut current_plain = String::new();
            for (token, token_plain) in tokens.into_iter().zip(plain_tokens) {
                let mut next_stack = stack.clone();
                if token.starts_with("</") { next_stack.pop(); }
                else if token.starts_with('<') && !token.ends_with("</a>") { next_stack.push(token.to_owned()); }
                if current.len() + token.len() + closing(&next_stack).len() > LIMIT
                    || current_plain.len() + token_plain.len() > LIMIT {
                    if current_plain.is_empty() { return plain_chunks(&plain); }
                    result.push(Chunk { text: format!("{}{}", current, closing(&stack)), plain: std::mem::take(&mut current_plain), mode: Some("HTML") });
                    if result.len() == 17 { return result; }
                    current = stack.concat();
                    if current.len() + token.len() + closing(&next_stack).len() > LIMIT
                    || current_plain.len() + token_plain.len() > LIMIT { return plain_chunks(&plain); }
                }
                current.push_str(&token);
                current_plain.push_str(&token_plain);
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

fn failure(description: &str) -> Failure {
    Failure { http_status: None, description: description.into(), content: false, delivered_chunks: 0 }
}
fn retry_seconds(data: &Value, attempt: usize) -> u64 {
    data["parameters"]["retry_after"].as_u64().unwrap_or(if attempt == 1 { 1 } else { 3 }).clamp(1, 30)
}

// Bridge partial progress across the existing String-returning caller APIs.
// Each outer retry owns a fresh task-local scope; unrelated sends cannot set it.
tokio::task_local! { static PARTIAL: std::cell::Cell<bool>; }
pub(crate) async fn track_partial<T>(future: impl std::future::Future<Output = T>) -> (T, bool) {
    PARTIAL.scope(std::cell::Cell::new(false), async {
        let result = future.await;
        (result, PARTIAL.with(|partial| partial.get()))
    }).await
}

pub(crate) fn delivery_notice(language: Option<&str>, incomplete: bool) -> &'static str {
    let primary = language.unwrap_or("zh-TW").split(['-', '_']).next().unwrap_or("zh").to_ascii_lowercase();
    match (primary.as_str(), incomplete) {
        ("en", false) => "The rest of this message is under Automations in the dashboard.",
        ("en", true) => "Message incomplete. See Automations in the dashboard.",
        ("ja", false) => "続きはダッシュボードの「自動化」で確認してください。",
        ("ja", true) => "メッセージの一部を配信できませんでした。ダッシュボードの「自動化」で確認してください。",
        (_, false) => "其餘內容請到儀表板「自動化」查看。",
        (_, true) => "訊息未完整送達，請到儀表板「自動化」查看。",
    }
}

#[allow(clippy::too_many_arguments)]
async fn post(client: &reqwest::Client, api_base: &str, chat_id: &str, text: &str,
    mode: Option<&str>, options: &Options<'_>, reply: Option<Value>, markup: Option<Value>,
    deadline: Instant, accepted_chunks: Option<&mut usize>) -> Result<(), Failure> {
    let mut body = json!({"chat_id":chat_id, "text":text});
    if let Some(mode) = mode { body["parse_mode"] = json!(mode); }
    if let Some(thread) = options.thread { body["message_thread_id"] = json!(thread); }
    if let Some(reply) = reply { body["reply_parameters"] = reply; }
    if let Some(markup) = markup { body["reply_markup"] = markup; }
    for attempt in 1..=3 {
        if Instant::now() >= deadline { return Err(failure("Telegram delivery budget exhausted")); }
        if crate::channel_reply::guard_lost(options.guarded).await { return Err(failure("Reply guard lost")); }
        let attempt_deadline = deadline.min(Instant::now() + Duration::from_secs(45));
        let (status, data) = tokio::time::timeout_at(attempt_deadline, async {
            let resp = client.post(format!("{api_base}/sendMessage")).json(&body).send_authorized().await
                .map_err(|_| failure("Telegram transport error"))?;
            let status = resp.status().as_u16();
            Ok::<_, Failure>((status, resp.json::<Value>().await.unwrap_or(Value::Null)))
        }).await.map_err(|_| failure(if Instant::now() >= deadline {
            "Telegram delivery budget exhausted"
        } else { "Telegram request timeout" }))??;
        if (200..300).contains(&status) && data["ok"] == true {
            if let Some(count) = accepted_chunks { *count += 1; }
            let id = data["result"]["message_id"].as_i64().map(|v| v.to_string());
            let text = data["result"]["text"].as_str().unwrap_or(if mode.is_none() { text } else { "[文字回覆]" });
            crate::channel_reply::group_buffer::delivered(text, id.as_deref()).await;
            return Ok(());
        }
        let code = data["error_code"].as_u64().unwrap_or(0);
        let description = if data.is_null() { "Invalid Telegram response".into() }
            else { safe_description(data["description"].as_str().unwrap_or("")) };
        let content = status == 400 && matches!(description.as_str(), "can't parse entities" | "message is too long" | "message text is empty");
        let retryable = status == 429 || (500..600).contains(&status) || code == 429 || (500..600).contains(&code);
        if attempt == 3 || !retryable {
            return Err(Failure { http_status: Some(status), description, content, delivered_chunks: 0 });
        }
        tokio::time::timeout_at(deadline, tokio::time::sleep(Duration::from_secs(retry_seconds(&data, attempt))))
            .await.map_err(|_| failure("Telegram delivery budget exhausted"))?;
    }
    unreachable!()
}

/// Bound, render and deliver with independent entity/reply fallbacks.
pub(crate) async fn send(client: &reqwest::Client, api_base: &str, chat_id: &str,
    text: &str, format: Format, options: Options<'_>) -> Result<(), Failure> {
    let deadline = Instant::now() + Duration::from_secs(120);
    send_until(client, api_base, chat_id, text, format, options, deadline).await
}

async fn send_until(client: &reqwest::Client, api_base: &str, chat_id: &str,
    text: &str, format: Format, options: Options<'_>, deadline: Instant) -> Result<(), Failure> {
    let bounded = duduclaw_core::truncate_bytes(text, MAX_INPUT_BYTES);
    let truncated = bounded.len() < text.len();
    let mut delivered = 0;
    let mut total = 0;
    let mut language = None;
    let delivery = async {
        language = crate::prompt_identity::read_default_language(options.home).await;
        let mut chunks = chunks(bounded, format, options.literal_prefix);
        if truncated || chunks.len() > crate::channel_format::limits::DELIVERY_CHUNKS {
            chunks.truncate(crate::channel_format::limits::DELIVERY_CHUNKS - 1);
            let notice = delivery_notice(language.as_deref(), false).to_owned();
            chunks.push(Chunk { text: notice.clone(), plain: notice, mode: None });
        }
        if chunks.is_empty() { chunks.push(Chunk { text: String::new(), plain: String::new(), mode: None }); }
        total = chunks.len();
        for (i, chunk) in chunks.iter().enumerate() {
            let mut reply = if i == 0 { options.reply_to.map(|id| json!({"message_id":id})) } else { None };
            let markup = if i + 1 == chunks.len() { options.markup.clone() } else { None };
            let mut plain = false;
            loop {
                let result = post(client, api_base, chat_id, if plain { &chunk.plain } else { &chunk.text },
                    if plain { None } else { chunk.mode }, &options, reply.clone(), markup.clone(), deadline, Some(&mut delivered)).await;
                match &result {
                    Err(error) if reply.is_some() && error.http_status == Some(400)
                        && matches!(error.description.as_str(), "message to be replied not found" | "replied message not found" | "message not found") => { reply = None; }
                    Err(error) if !plain && chunk.mode.is_some() && error.http_status == Some(400)
                        && error.description == "can't parse entities" => { plain = true; }
                    _ => { result?; break; }
                }
            }
        }
        Ok::<_, Failure>(())
    };
    let result = tokio::time::timeout_at(deadline, delivery).await
        .unwrap_or_else(|_| Err(failure("Telegram delivery budget exhausted")));
    if let Err(mut error) = result {
        error.delivered_chunks = delivered;
        tracing::warn!(kind = options.kind, ref_id = options.ref_id, delivered_chunks = error.delivered_chunks,
            http_status = ?error.http_status, description = %error.description, "Telegram delivery failed");
        let record = json!({"event":"telegram_send_failed", "channel":"telegram", "chat_id":chat_id,
            "kind":options.kind, "ref_id":options.ref_id, "chunk":delivered+1, "total_chunks":total,
            "delivered_chunks":error.delivered_chunks, "http_status":error.http_status, "description":error.description,
            "failure_kind":if error.content {"content"} else {"transport_or_access"}, "timestamp":chrono::Utc::now().to_rfc3339()});
        if let Err(e) = crate::trajectory_guard::append_anomaly(options.home, &record) {
            tracing::warn!(error = %e, "telegram: failure record could not be written");
        }
        if error.delivered_chunks > 0 && PARTIAL.try_with(|partial| !partial.replace(true)).unwrap_or(false)
            && !is_public_visitor(chat_id) {
            // This notice shares the delivery budget and never retries the body.
            let _ = post(client, api_base, chat_id, delivery_notice(language.as_deref(), true), None,
                &options, None, None, deadline, None).await;
        }
        return Err(error);
    }
    Ok(())
}

fn is_public_visitor(chat_id: &str) -> bool {
    chat_id.starts_with("odoolivechat:")
}

fn cron_notice(name: &str, language: Option<&str>) -> String {
    let primary = language.unwrap_or("zh-TW").split(['-', '_']).next().unwrap_or("zh").to_ascii_lowercase();
    match primary.as_str() {
        "en" => format!("⏰ [{name}] The result could not be delivered. View it under Automations in the dashboard."),
        "ja" => format!("⏰ [{name}] 結果を配信できませんでした。ダッシュボードの「自動化」で確認してください。"),
        _ => format!("⏰ [{name}] 的結果無法送達，請到儀表板「自動化」查看"),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn send_cron(client: &reqwest::Client, api_base: &str, chat_id: &str,
    text: &str, name: &str, id: &str, home: &Path) -> Result<(), Failure> {
    let deadline = Instant::now() + Duration::from_secs(120);
    let prefix = format!("⏰ [{name}] ");
    let mut options = Options::new(home, "cron", id);
    options.literal_prefix = Some(&prefix);
    let result = send_until(client, api_base, chat_id, text, Format::MarkdownHtml, options, deadline).await;
    if result.as_ref().err().is_some_and(|e| e.content) && !is_public_visitor(chat_id) {
        let name = duduclaw_core::plain_line(name, 120);
        let language = tokio::time::timeout_at(deadline, crate::prompt_identity::read_default_language(home))
            .await.unwrap_or(None);
        let notice = cron_notice(&name, language.as_deref());
        let _ = send_until(client, api_base, chat_id, &notice, Format::Plain,
            Options::new(home, "cron_notice", id), deadline).await;
    }
    result
}

#[cfg(test)]
#[path = "telegram_send_tests.rs"]
mod tests;
