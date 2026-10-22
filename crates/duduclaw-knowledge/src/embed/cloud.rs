//! Cloud embedding providers (OpenAI, Gemini).
//!
//! Every text is de-identified by a [`TextRedactor`] before a request is
//! built; a redactor error fails the call and nothing is sent. The provider
//! and host the text goes to are reported through [`Embedder::egress`], and
//! the gateway only builds a cloud embedder after an Admin acknowledged that
//! disclosure for exactly that provider, host and model.
//!
//! HTTP goes through [`JsonTransport`] so tests can use a local mock; the
//! real one ([`HttpTransport`]) is a blocking client and is only ever called
//! off the async runtime.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use super::pooling::l2_normalize;
use super::redactor::TextRedactor;
use super::{Egress, EmbedError, Embedder};

/// Supported providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudProvider {
    OpenAi,
    Gemini,
}

impl CloudProvider {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "openai" => Some(Self::OpenAi),
            "gemini" => Some(Self::Gemini),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Gemini => "gemini",
        }
    }

    /// Display name for the disclosure text.
    pub fn vendor(self) -> &'static str {
        match self {
            Self::OpenAi => "OpenAI",
            Self::Gemini => "Google Gemini",
        }
    }

    pub fn default_base_url(self) -> &'static str {
        match self {
            Self::OpenAi => "https://api.openai.com",
            Self::Gemini => "https://generativelanguage.googleapis.com",
        }
    }

    pub fn default_model(self) -> &'static str {
        match self {
            Self::OpenAi => "text-embedding-3-small",
            Self::Gemini => "gemini-embedding-001",
        }
    }

    /// Vector size we request (`dimensions` / `outputDimensionality`).
    pub fn dim(self) -> usize {
        match self {
            Self::OpenAi => 1536,
            Self::Gemini => 768,
        }
    }

    /// Texts per request (OpenAI allows 2048 inputs, Gemini batches of 100;
    /// we stay well below both).
    pub fn max_batch(self) -> usize {
        match self {
            Self::OpenAi => 64,
            Self::Gemini => 64,
        }
    }
}

/// Accept `https://…`, or `http://` only to a loopback host (a local proxy
/// or test double). Returns the normalised base (no trailing slash) and
/// the host.
pub fn validate_base_url(raw: &str) -> Result<(String, String), String> {
    let url = reqwest::Url::parse(raw.trim()).map_err(|_| "雲端端點網址格式不正確".to_string())?;
    let host = url.host_str().unwrap_or("").to_string();
    if host.is_empty() || !url.username().is_empty() || url.password().is_some() {
        return Err("雲端端點網址格式不正確".to_string());
    }
    let loopback = matches!(host.as_str(), "127.0.0.1" | "localhost" | "[::1]" | "::1");
    match url.scheme() {
        "https" => {}
        "http" if loopback => {}
        _ => return Err("雲端端點必須使用 https（本機測試端點除外）".to_string()),
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("雲端端點網址不可帶查詢參數".to_string());
    }
    let base = url.as_str().trim_end_matches('/').to_string();
    Ok((base, host))
}

/// POST a JSON body, get a JSON body.
pub trait JsonTransport: Send + Sync {
    fn post_json(
        &self,
        url: &str,
        headers: &[(&str, String)],
        body: &Value,
    ) -> Result<Value, String>;
}

/// Largest response body accepted from a provider.
const MAX_RESPONSE_BYTES: u64 = 32 * 1024 * 1024;

/// The real transport: blocking reqwest with a 60 s timeout.
///
/// A `reqwest::blocking::Client` owns a small runtime and panics when it is
/// created or dropped on an async worker thread. Callers hold embedders
/// across async code, so the client is built once on a plain thread and
/// kept for the life of the process; every handle shares it.
pub struct HttpTransport {
    client: &'static reqwest::blocking::Client,
}

fn shared_client() -> Result<&'static reqwest::blocking::Client, String> {
    static CLIENT: std::sync::OnceLock<Result<reqwest::blocking::Client, String>> =
        std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            std::thread::spawn(|| {
                reqwest::blocking::Client::builder()
                    .timeout(Duration::from_secs(60))
                    .connect_timeout(Duration::from_secs(15))
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .map_err(|e| e.to_string())
            })
            .join()
            .unwrap_or_else(|_| Err("HTTP client thread panicked".to_string()))
        })
        .as_ref()
        .map_err(Clone::clone)
}

impl HttpTransport {
    pub fn new() -> Result<Self, String> {
        Ok(Self {
            client: shared_client()?,
        })
    }
}

impl JsonTransport for HttpTransport {
    fn post_json(
        &self,
        url: &str,
        headers: &[(&str, String)],
        body: &Value,
    ) -> Result<Value, String> {
        let mut req = self.client.post(url).json(body);
        for (k, v) in headers {
            req = req.header(*k, v);
        }
        let resp = req
            .send()
            .map_err(|e| format!("連線失敗：{}", e.without_url()))?;
        let status = resp.status();
        if !status.is_success() {
            // Provider error bodies can echo input or part of the key; only
            // the status code is kept (it lands in logs and job errors).
            return Err(format!("HTTP {}", status.as_u16()));
        }
        // A misbehaving endpoint must not exhaust memory: 32 MB is about ten
        // times the largest batch we request.
        let mut body = Vec::new();
        std::io::Read::read_to_end(
            &mut std::io::Read::take(resp, MAX_RESPONSE_BYTES + 1),
            &mut body,
        )
        .map_err(|e| format!("讀取回應失敗：{e}"))?;
        if body.len() as u64 > MAX_RESPONSE_BYTES {
            return Err("雲端回應過大".to_string());
        }
        serde_json::from_slice(&body).map_err(|_| "雲端回應不是 JSON".to_string())
    }
}

/// One configured cloud embedder.
pub struct CloudEmbedder {
    provider: CloudProvider,
    model: String,
    base_url: String,
    host: String,
    key: String,
    model_id: String,
    redactor: Arc<dyn TextRedactor>,
    transport: Arc<dyn JsonTransport>,
}

impl std::fmt::Debug for CloudEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never prints the key.
        f.debug_struct("CloudEmbedder")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("host", &self.host)
            .finish()
    }
}

impl CloudEmbedder {
    /// `base_url` must already be validated ([`validate_base_url`]).
    pub fn new(
        provider: CloudProvider,
        model: &str,
        base_url: &str,
        host: &str,
        key: String,
        redactor: Arc<dyn TextRedactor>,
        transport: Arc<dyn JsonTransport>,
    ) -> Self {
        Self {
            model_id: format!("{}/{model}@{}", provider.as_str(), provider.dim()),
            provider,
            model: model.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
            host: host.to_string(),
            key,
            redactor,
            transport,
        }
    }

    fn redact_all(&self, texts: &[&str]) -> Result<Vec<String>, EmbedError> {
        texts
            .iter()
            .map(|t| {
                self.redactor.redact(t).map_err(|e| {
                    EmbedError::Unavailable(format!("去識別化無法執行，文字未送出：{e}"))
                })
            })
            .collect()
    }

    fn call(&self, texts: &[String], query: bool) -> Result<Vec<Vec<f32>>, EmbedError> {
        let mut out = Vec::with_capacity(texts.len());
        for batch in texts.chunks(self.provider.max_batch()) {
            let (url, headers, body) = match self.provider {
                CloudProvider::OpenAi => {
                    super::cloud_openai::request(&self.base_url, &self.model, &self.key, batch)
                }
                CloudProvider::Gemini => super::cloud_gemini::request(
                    &self.base_url,
                    &self.model,
                    &self.key,
                    batch,
                    query,
                ),
            };
            let headers: Vec<(&str, String)> =
                headers.iter().map(|(k, v)| (*k, v.clone())).collect();
            let resp = self
                .transport
                .post_json(&url, &headers, &body)
                .map_err(|e| {
                    EmbedError::Failed(format!("{} 向量服務錯誤：{e}", self.provider.vendor()))
                })?;
            let vectors = match self.provider {
                CloudProvider::OpenAi => super::cloud_openai::parse(&resp, batch.len()),
                CloudProvider::Gemini => super::cloud_gemini::parse(&resp, batch.len()),
            }
            .map_err(|e| {
                EmbedError::Failed(format!("{} 回應格式不符：{e}", self.provider.vendor()))
            })?;
            for v in vectors {
                if v.len() != self.provider.dim() {
                    return Err(EmbedError::Failed(format!(
                        "{} 回傳 {} 維，預期 {}",
                        self.provider.vendor(),
                        v.len(),
                        self.provider.dim()
                    )));
                }
                out.push(l2_normalize(v));
            }
        }
        Ok(out)
    }
}

impl Embedder for CloudEmbedder {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dim(&self) -> usize {
        self.provider.dim()
    }

    fn max_input_tokens(&self) -> usize {
        match self.provider {
            CloudProvider::OpenAi => 8192,
            CloudProvider::Gemini => 2048,
        }
    }

    fn embed_passages(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, EmbedError> {
        let clean = self.redact_all(texts)?;
        self.call(&clean, false)
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        let clean = self.redact_all(&[text])?;
        self.call(&clean, true)?
            .pop()
            .ok_or_else(|| EmbedError::Failed("雲端沒有回傳向量".to_string()))
    }

    fn egress(&self) -> Egress {
        Egress::Cloud {
            provider: self.provider.as_str().to_string(),
            host: self.host.clone(),
        }
    }
}

#[cfg(test)]
#[path = "cloud_tests.rs"]
mod tests;
