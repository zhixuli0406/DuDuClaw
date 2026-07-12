//! Tokenizer access for JitRL feedback ingestion.
//!
//! JitRL biases are keyed by **token id in the active model's vocabulary**, so
//! `record_feedback` must tokenize the response with the *same* tokenizer the
//! serving backend uses. Fabricating ids (e.g. hashing words) would poison the
//! store, so when no tokenizer is reachable, feedback recording fails loudly
//! instead of degrading silently.
//!
//! v1 ships [`HttpTokenizer`], which drives the `/tokenize` endpoint exposed
//! by the common local OpenAI-compatible servers:
//! - llama.cpp server: `POST /tokenize {"content": "..."} → {"tokens":[...]}`
//! - vLLM:             `POST /tokenize {"model": "...", "prompt": "..."} → {"tokens":[...]}`
//!
//! Both live at the server root (not under `/v1`), so the base URL's trailing
//! `/v1` is stripped. The llama.cpp shape is tried first, then the vLLM shape.

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::error::{InferenceError, Result};

/// Anything that can turn text into the active model's token ids.
#[async_trait]
pub trait JitrlTokenizer: Send + Sync {
    async fn encode(&self, text: &str) -> Result<Vec<u32>>;
}

/// Derive the `/tokenize` URL from an OpenAI-compat base URL
/// (`http://host:port/v1` → `http://host:port/tokenize`).
pub fn tokenize_url_from_base(base_url: &str) -> String {
    let root = base_url
        .trim_end_matches('/')
        .trim_end_matches("/v1")
        .trim_end_matches('/');
    format!("{root}/tokenize")
}

/// Parse a `/tokenize` response body. Accepts token arrays of plain integers
/// (llama.cpp, vLLM) or objects carrying an `id` field (llama.cpp
/// `with_pieces` mode). Returns `None` when the shape is unrecognized.
pub fn parse_tokenize_response(body: &Value) -> Option<Vec<u32>> {
    let tokens = body.get("tokens")?.as_array()?;
    let mut out = Vec::with_capacity(tokens.len());
    for t in tokens {
        let id = match t {
            Value::Number(n) => n.as_u64()?,
            Value::Object(o) => o.get("id")?.as_u64()?,
            _ => return None,
        };
        out.push(u32::try_from(id).ok()?);
    }
    Some(out)
}

/// HTTP tokenizer against a local OpenAI-compatible server's `/tokenize`.
pub struct HttpTokenizer {
    client: reqwest::Client,
    tokenize_url: String,
    model: String,
    api_key: Option<String>,
}

impl HttpTokenizer {
    pub fn new(base_url: &str, model: &str, api_key: Option<String>) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            tokenize_url: tokenize_url_from_base(base_url),
            model: model.to_string(),
            api_key,
        }
    }

    async fn try_shape(&self, body: Value) -> Result<Option<Vec<u32>>> {
        let mut req = self.client.post(&self.tokenize_url).json(&body);
        if let Some(ref key) = self.api_key {
            req = req.bearer_auth(key);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| InferenceError::Http(format!("jitrl tokenize: {e}")))?;
        if !resp.status().is_success() {
            return Ok(None);
        }
        let value: Value = resp
            .json()
            .await
            .map_err(|e| InferenceError::Http(format!("jitrl tokenize parse: {e}")))?;
        Ok(parse_tokenize_response(&value))
    }
}

#[async_trait]
impl JitrlTokenizer for HttpTokenizer {
    async fn encode(&self, text: &str) -> Result<Vec<u32>> {
        // llama.cpp server shape first, then vLLM.
        if let Some(tokens) = self.try_shape(json!({ "content": text })).await? {
            return Ok(tokens);
        }
        if let Some(tokens) = self
            .try_shape(json!({ "model": self.model, "prompt": text }))
            .await?
        {
            return Ok(tokens);
        }
        Err(InferenceError::Http(format!(
            "jitrl: no usable /tokenize endpoint at {} — feedback not recorded (token ids \
             must come from the serving model's tokenizer)",
            self.tokenize_url
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_url_strips_v1_suffix() {
        assert_eq!(
            tokenize_url_from_base("http://localhost:8080/v1"),
            "http://localhost:8080/tokenize"
        );
        assert_eq!(
            tokenize_url_from_base("http://localhost:8080/v1/"),
            "http://localhost:8080/tokenize"
        );
        assert_eq!(
            tokenize_url_from_base("http://localhost:8080"),
            "http://localhost:8080/tokenize"
        );
    }

    #[test]
    fn parses_plain_integer_tokens() {
        let body = serde_json::json!({ "tokens": [1, 2, 42] });
        assert_eq!(parse_tokenize_response(&body), Some(vec![1, 2, 42]));
    }

    #[test]
    fn parses_object_tokens_with_id() {
        // llama.cpp `with_pieces` shape.
        let body = serde_json::json!({
            "tokens": [{"id": 5, "piece": "he"}, {"id": 9, "piece": "llo"}]
        });
        assert_eq!(parse_tokenize_response(&body), Some(vec![5, 9]));
    }

    #[test]
    fn rejects_unknown_shapes() {
        assert!(parse_tokenize_response(&serde_json::json!({"count": 3})).is_none());
        assert!(parse_tokenize_response(&serde_json::json!({"tokens": ["a"]})).is_none());
        assert!(parse_tokenize_response(&serde_json::json!({"tokens": [-1]})).is_none());
    }
}
