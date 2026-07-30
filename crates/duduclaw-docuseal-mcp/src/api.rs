//! Thin DocuSeal REST executor: base URL + `X-Auth-Token`, runs an
//! [`HttpPlan`] and returns the response for the RPC layer to wrap.

use serde_json::Value;

use crate::tools::{HttpPlan, Method};

/// Response body cap rendered back into a tool result (chars). DocuSeal list
/// endpoints max out at 100 rows; anything past this is truncation-marked.
const MAX_RESULT_CHARS: usize = 60_000;

/// Outcome of an executed plan, MCP-shaped: text content + isError.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiOutcome {
    pub content: String,
    pub is_error: bool,
}

/// DocuSeal API client (cloud or self-hosted).
pub struct DocusealApi {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl DocusealApi {
    /// `base_url` examples: `https://api.docuseal.com` (cloud global),
    /// `https://api.docuseal.eu` (cloud EU), `https://sign.example.com/api`
    /// (self-hosted). A trailing slash is trimmed. Plain http is allowed only
    /// for localhost (self-hosted dev).
    pub fn new(base_url: &str, api_key: &str) -> Result<Self, String> {
        let base = base_url.trim_end_matches('/').to_string();
        let localhost_ok =
            base.starts_with("http://127.0.0.1") || base.starts_with("http://localhost");
        if !base.starts_with("https://") && !localhost_ok {
            return Err(format!("DOCUSEAL_BASE_URL must be https:// (got {base})"));
        }
        if api_key.trim().is_empty() {
            return Err("DOCUSEAL_API_KEY is empty".into());
        }
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self { http, base_url: base, api_key: api_key.to_string() })
    }

    /// Execute one plan. HTTP-level failures (network, non-2xx) come back as
    /// `is_error = true` tool outcomes — the agent sees the cause and can
    /// adjust; nothing here kills the server loop.
    pub async fn execute(&self, plan: &HttpPlan) -> ApiOutcome {
        let url = format!("{}{}", self.base_url, plan.path);
        let mut req = match plan.method {
            Method::Get => self.http.get(&url),
            Method::Post => self.http.post(&url),
            Method::Put => self.http.put(&url),
            Method::Delete => self.http.delete(&url),
        };
        req = req.header("X-Auth-Token", &self.api_key);
        if !plan.query.is_empty() {
            req = req.query(&plan.query);
        }
        if let Some(body) = &plan.body {
            req = req.json(body);
        }

        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                return ApiOutcome { content: format!("request failed: {e}"), is_error: true }
            }
        };
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();

        if !status.is_success() {
            let snippet: String = text.chars().take(500).collect();
            return ApiOutcome {
                content: format!("DocuSeal API returned HTTP {status}: {snippet}"),
                is_error: true,
            };
        }
        ApiOutcome { content: render_body(&text), is_error: false }
    }
}

/// Pretty-print JSON bodies (raw passthrough otherwise), capped CJK-safely.
fn render_body(text: &str) -> String {
    let pretty = serde_json::from_str::<Value>(text)
        .and_then(|v| serde_json::to_string_pretty(&v))
        .unwrap_or_else(|_| text.to_string());
    if pretty.chars().count() <= MAX_RESULT_CHARS {
        return pretty;
    }
    let truncated: String = pretty.chars().take(MAX_RESULT_CHARS).collect();
    format!("{truncated}\n… (truncated)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_validation() {
        assert!(DocusealApi::new("https://api.docuseal.com", "k").is_ok());
        assert!(DocusealApi::new("https://sign.example.com/api/", "k").is_ok());
        assert!(DocusealApi::new("http://127.0.0.1:3000/api", "k").is_ok());
        assert!(DocusealApi::new("http://api.docuseal.com", "k").is_err());
        assert!(DocusealApi::new("https://api.docuseal.com", " ").is_err());
    }

    #[test]
    fn render_body_pretty_prints_and_caps() {
        assert_eq!(render_body("{\"a\":1}"), "{\n  \"a\": 1\n}");
        assert_eq!(render_body("not json"), "not json");
        let big = format!("\"{}\"", "字".repeat(MAX_RESULT_CHARS));
        assert!(render_body(&big).ends_with("… (truncated)"));
    }
}
