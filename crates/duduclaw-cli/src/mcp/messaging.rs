use super::*;

pub(crate) async fn handle_send_message(
    params: &Value,
    home_dir: &Path,
    http: &reqwest::Client,
    _agent_id: &str,
) -> Value {
    let channel = params.get("channel").and_then(|v| v.as_str()).unwrap_or("");
    let chat_id = params.get("chat_id").and_then(|v| v.as_str()).unwrap_or("");
    let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");

    if channel.is_empty() || chat_id.is_empty() || text.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: channel, chat_id, and text are required"}],
            "isError": true
        });
    }

    // Delegate to the unified `channel_sender::resolve_channel_target` +
    // `create_sender` path via `reminder_scheduler::send_channel_message`
    // (same shared implementation `autopilot_engine`'s `notify` action now
    // uses) — supports every bot-pushable channel
    // (telegram/line/discord/slack/whatsapp/feishu/googlechat/teams/wecom/
    // dingtalk), not just the original hardcoded telegram/line/discord
    // three. WebChat is refused inside `resolve_channel_target`: it's a
    // session-scoped WebSocket connection with no persistent bot identity a
    // stateless MCP call can deliver into.
    let result = match duduclaw_gateway::reminder_scheduler::send_channel_message(
        home_dir, http, channel, chat_id, text,
    )
    .await
    {
        Ok(()) => "Message sent successfully.".to_string(),
        Err(e) => format!("Error: {e}"),
    };

    serde_json::json!({
        "content": [{"type": "text", "text": result}]
    })
}

pub(crate) async fn handle_web_search(params: &Value, http: &reqwest::Client) -> Value {
    let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
    if query.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: query is required"}],
            "isError": true
        });
    }

    let url = format!(
        "https://html.duckduckgo.com/html/?q={}",
        urlencoding::encode(query)
    );

    // Enforce 10s timeout so web_search doesn't block the MCP server (CLI-H5)
    let search_future = async {
        let resp = http
            .get(&url)
            .header("User-Agent", "DuDuClaw/0.6")
            .send()
            .await?;
        resp.text().await
    };

    let result = match tokio::time::timeout(std::time::Duration::from_secs(10), search_future).await
    {
        Ok(Ok(body)) => extract_search_results(&body),
        Ok(Err(e)) => format!("Error performing search: {e}"),
        Err(_) => "Error: web search timed out after 10 seconds".to_string(),
    };

    serde_json::json!({
        "content": [{"type": "text", "text": result}]
    })
}

/// Extract text results from DuckDuckGo HTML response using `scraper` (CLI-M5).
pub(crate) fn extract_search_results(html: &str) -> String {
    use scraper::{Html, Selector};

    let document = Html::parse_document(html);
    let mut results = Vec::new();

    // Try selectors in priority order
    let selectors = [".result__snippet", ".result__a", ".links_main a"];

    for sel_str in selectors {
        if let Ok(selector) = Selector::parse(sel_str) {
            for element in document.select(&selector) {
                let text: String = element.text().collect::<Vec<_>>().join(" ");
                let clean = text.trim().to_string();
                if !clean.is_empty() && clean.len() > 10 {
                    results.push(clean);
                }
                if results.len() >= 5 {
                    break;
                }
            }
        }
        if !results.is_empty() {
            break;
        }
    }

    if results.is_empty() {
        "No results found.".to_string()
    } else {
        results
            .iter()
            .enumerate()
            .map(|(i, r)| format!("{}. {}", i + 1, r))
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

/// Send a photo or sticker via a channel.
pub(crate) async fn handle_send_media(
    params: &Value,
    home_dir: &Path,
    http: &reqwest::Client,
    media_type: &str,
) -> Value {
    let channel = params.get("channel").and_then(|v| v.as_str()).unwrap_or("");
    let chat_id = params.get("chat_id").and_then(|v| v.as_str()).unwrap_or("");
    let url_or_id = params
        .get("url_or_path")
        .or_else(|| params.get("url"))
        .or_else(|| params.get("sticker_id"))
        .or_else(|| params.get("file_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if chat_id.is_empty() || url_or_id.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: chat_id and url/sticker_id are required for {media_type}")}],
            "isError": true
        });
    }

    let config = read_config(home_dir).await;
    let config_ref = config.as_ref();
    let result = match channel {
        "telegram" => {
            let token = match config_ref {
                Some(c) => decrypt_channel_token(c, "telegram_bot_token", home_dir).await,
                None => String::new(),
            };
            if token.is_empty() {
                "Error: telegram_bot_token not configured".to_string()
            } else {
                let (method, key) = match media_type {
                    "photo" => ("sendPhoto", "photo"),
                    _ => ("sendSticker", "sticker"),
                };
                let api_url = format!("https://api.telegram.org/bot{token}/{method}");
                match http
                    .post(&api_url)
                    .json(&serde_json::json!({ "chat_id": chat_id, key: url_or_id }))
                    .send()
                    .await
                {
                    Ok(r) => format!("{media_type} sent. Status: {}", r.status()),
                    Err(e) => format!("Error: {e}"),
                }
            }
        }
        "discord" => {
            let token = match config_ref {
                Some(c) => decrypt_channel_token(c, "discord_bot_token", home_dir).await,
                None => String::new(),
            };
            if token.is_empty() {
                "Error: discord_bot_token not configured".to_string()
            } else {
                let api_url = format!("https://discord.com/api/v10/channels/{chat_id}/messages");
                match http
                    .post(&api_url)
                    .header("Authorization", format!("Bot {token}"))
                    .json(&serde_json::json!({ "content": url_or_id }))
                    .send()
                    .await
                {
                    Ok(r) => format!("{media_type} sent. Status: {}", r.status()),
                    Err(e) => format!("Error: {e}"),
                }
            }
        }
        _ => format!("Channel '{channel}' does not support {media_type} yet"),
    };

    serde_json::json!({ "content": [{"type": "text", "text": result}] })
}
