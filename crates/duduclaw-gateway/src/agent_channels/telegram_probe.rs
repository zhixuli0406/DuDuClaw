//! Check a pasted BotFather token with `getMe` before it is saved
//! ("verify before applying"): a token Telegram rejects is never stored.

use std::time::Duration;

/// Outcome of `getMe`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// Telegram confirmed the token; the bot's `@username`.
    Verified { username: String },
    /// Telegram answered and refused the token (401 / 404).
    Rejected(String),
    /// No usable answer (network, 429, 5xx) — says nothing about the token.
    Unverified(String),
}

/// Ask Telegram who this bot is.
pub async fn get_me(token: &str) -> Probe {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => return Probe::Unverified(e.to_string()),
    };
    let url = format!("{}/getMe", crate::telegram::telegram_poll_api_base(token));
    let body: serde_json::Value = match client.get(&url).send().await {
        Ok(resp) => match resp.json().await {
            Ok(v) => v,
            Err(e) => return Probe::Unverified(redact(&e.to_string())),
        },
        Err(e) => return Probe::Unverified(redact(&e.to_string())),
    };
    classify(&body)
}

fn redact(err: &str) -> String {
    crate::secret_redact::redact_secrets(err).into_owned()
}

/// Classify a `getMe` body. Pure, unit-tested.
pub fn classify(body: &serde_json::Value) -> Probe {
    if body.get("ok").and_then(|v| v.as_bool()) == Some(true) {
        let username = body
            .pointer("/result/username")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        return Probe::Verified { username };
    }
    let code = body.get("error_code").and_then(|v| v.as_i64());
    let desc = body
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if matches!(code, Some(401) | Some(404)) {
        Probe::Rejected(desc)
    } else {
        Probe::Unverified(format!("Telegram API {} {desc}", code.unwrap_or(0)))
    }
}
