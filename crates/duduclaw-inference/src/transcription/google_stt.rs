//! Cloud Speech v2 batch input must be in GCS; results are returned inline.
use std::{path::{Path, PathBuf}, time::Duration};
mod cleanup;
mod token;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;
use zeroize::{Zeroize, Zeroizing};
use super::{bounded_json, config::GoogleSttConfig, http_client, Segment, Transcript, TranscriptionEngine, TranscriptionError, TranscriptionResult, TranscriptionUsage};

const LIMIT: u64 = 25 * 1024 * 1024;
const MODEL: &str = "google-stt/chirp_2";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

pub struct GoogleSttEngine {
    credentials: Zeroizing<String>,
    config: GoogleSttConfig,
    client: reqwest::Client,
    speech_base: String,
    storage_base: String,
    cleanup_home: PathBuf,
    #[cfg(test)]
    _test_home: Option<tempfile::TempDir>,
    #[cfg(test)]
    test_token: Option<String>,
}

#[derive(Deserialize)]
struct ServiceAccount {
    #[serde(rename = "type")]
    kind: String,
    client_email: String,
    private_key: String,
}

impl Drop for ServiceAccount {
    fn drop(&mut self) { self.private_key.zeroize(); }
}

fn service_account(raw: &str) -> Result<ServiceAccount, TranscriptionError> {
    if raw.len() > 32 * 1024 { return Err(TranscriptionError::new("Google Cloud 服務帳戶憑證過大。")); }
    let account: ServiceAccount = serde_json::from_str(raw)
        .map_err(|_| TranscriptionError::new("Google Cloud 憑證必須是有效的服務帳戶 JSON。"))?;
    if account.kind != "service_account" || account.client_email.len() > 254
        || !account.client_email.ends_with(".iam.gserviceaccount.com")
        || account.client_email.chars().any(|c| c.is_control()) {
        return Err(TranscriptionError::new("Google Cloud 服務帳戶憑證格式錯誤。"));
    }
    jsonwebtoken::EncodingKey::from_rsa_pem(account.private_key.as_bytes())
        .map_err(|_| TranscriptionError::new("Google Cloud 服務帳戶私鑰格式錯誤。"))?;
    Ok(account)
}

pub fn validate_service_account_json(raw: &str) -> Result<(), TranscriptionError> { service_account(raw).map(|_| ()) }

impl GoogleSttEngine {
    pub fn validate_service_account_json(raw: &str) -> Result<(), TranscriptionError> { validate_service_account_json(raw) }

    pub fn new(credentials: String, config: GoogleSttConfig, home: &Path) -> Result<Self, TranscriptionError> {
        let credentials = Zeroizing::new(credentials);
        config.validate().map_err(TranscriptionError::new)?;
        validate_service_account_json(&credentials)?;
        Ok(Self { credentials, speech_base: format!("https://{}-speech.googleapis.com", config.location),
            storage_base: "https://storage.googleapis.com".into(), cleanup_home: home.to_path_buf(), config, client: http_client()?,
            #[cfg(test)] _test_home: None,
            #[cfg(test)] test_token: None })
    }

    #[cfg(test)]
    pub(super) fn for_test(base: String, config: GoogleSttConfig) -> Self {
        let home = tempfile::tempdir().unwrap();
        Self { credentials: Zeroizing::new(String::new()), config, client: http_client().unwrap(),
            cleanup_home: home.path().to_path_buf(), _test_home: Some(home),
            speech_base: base.clone(), storage_base: base, test_token: Some("test-token".into()) }
    }

    async fn access_token(&self) -> Result<Zeroizing<String>, TranscriptionError> {
        #[cfg(test)] if let Some(token) = &self.test_token { return Ok(Zeroizing::new(token.clone())); }
        #[derive(Serialize)]
        struct Claims<'a> { iss: &'a str, scope: &'a str, aud: &'a str, iat: u64, exp: u64 }
        let account = service_account(&self.credentials)?;
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| TranscriptionError::new("系統時間無效，無法驗證 Google Cloud 憑證。"))?.as_secs();
        let key = jsonwebtoken::EncodingKey::from_rsa_pem(account.private_key.as_bytes())
            .map_err(|_| TranscriptionError::new("Google Cloud 私鑰無效。"))?;
        let assertion = Zeroizing::new(jsonwebtoken::encode(&jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256),
            &Claims { iss: &account.client_email, scope: "https://www.googleapis.com/auth/cloud-platform", aud: TOKEN_URL, iat: now, exp: now + 3600 }, &key)
            .map_err(|_| TranscriptionError::new("無法簽署 Google Cloud 驗證要求。"))?);
        let response = self.client.post(TOKEN_URL).form(&[("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"), ("assertion", assertion.as_str())])
            .send().await.map_err(|_| TranscriptionError::new("Google Cloud 憑證驗證連線失敗。"))?;
        token::read_response(response).await
    }

    pub(super) fn batch_request(&self, uri: &str) -> Value {
        json!({"config":{"autoDecodingConfig":{},"languageCodes":[self.config.language],"model":self.config.model,
            "features":{"enableAutomaticPunctuation":true,"enableWordTimeOffsets":true}},
            "files":[{"uri":uri}],"recognitionOutputConfig":{"inlineResponseConfig":{}}})
    }

    pub(super) fn operation_path(&self, name: &str) -> Result<String, TranscriptionError> {
        let prefix = format!("projects/{}/locations/{}/operations/", self.config.project_id, self.config.location);
        let id = name.strip_prefix(&prefix).ok_or_else(|| TranscriptionError::new("Google Cloud 工作識別格式錯誤。"))?;
        if id.is_empty() || id.len() > 200 || !id.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_')) {
            return Err(TranscriptionError::new("Google Cloud 工作識別格式錯誤。"));
        }
        Ok(format!("{}/v2/{name}", self.speech_base))
    }

    async fn recognize(&self, token: &str, uri: &str) -> Result<Transcript, TranscriptionError> {
        let endpoint = format!("{}/v2/projects/{}/locations/{}/recognizers/_:batchRecognize", self.speech_base, self.config.project_id, self.config.location);
        let response = self.client.post(endpoint).bearer_auth(token).json(&self.batch_request(uri)).send().await
            .map_err(|_| TranscriptionError::new("Google Cloud 轉錄連線失敗；要求可能已計費。"))?;
        let mut operation = checked_json(response).await?;
        let mut operation_url = None;
        for _ in 0..480 {
            if operation["done"].as_bool() == Some(true) {
                if let Some(code) = operation["error"]["code"].as_i64().filter(|c| *c != 0) { return Err(rpc_error(code)); }
                return map_response(&operation["response"], uri, &self.config.language);
            }
            if operation_url.is_none() {
                operation_url = Some(self.operation_path(operation["name"].as_str().ok_or_else(|| TranscriptionError::new("Google Cloud 未回傳工作識別。"))?)?);
            }
            #[cfg(not(test))] tokio::time::sleep(Duration::from_secs(5)).await;
            let response = self.client.get(operation_url.as_ref().unwrap()).bearer_auth(token).send().await
                .map_err(|_| TranscriptionError::new("無法讀取 Google Cloud 轉錄進度；要求可能已計費。"))?;
            operation = checked_json(response).await?;
        }
        Err(TranscriptionError::new("Google Cloud 轉錄等待逾時；雲端要求可能仍在執行及計費。"))
    }
}

fn rpc_error(code: i64) -> TranscriptionError {
    TranscriptionError::new(match code { 7 | 16 => "Google Cloud 驗證或存取權限不足，請管理員檢查憑證與 IAM 權限。",
        8 => "Google Cloud 轉錄配額不足，請稍後重試或由管理員檢查配額。", _ => "Google Cloud 轉錄失敗，請管理員檢查服務設定。" })
}

async fn checked_json(response: reqwest::Response) -> Result<Value, TranscriptionError> {
    match response.status().as_u16() {
        401 | 403 => return Err(rpc_error(7)), 429 => return Err(rpc_error(8)), _ => {}
    }
    bounded_json(response).await
}

#[async_trait]
impl TranscriptionEngine for GoogleSttEngine {
    async fn transcribe(&self, audio_path: &Path, _language_hint: &str, _speaker_context: &[String]) -> Result<TranscriptionResult, TranscriptionError> {
        let file = tokio::fs::File::open(audio_path).await.map_err(|_| TranscriptionError::new("無法讀取會議音訊。"))?;
        let metadata = file.metadata().await.map_err(|_| TranscriptionError::new("無法讀取會議音訊資訊。"))?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > LIMIT {
            return Err(TranscriptionError::new("Google Cloud 每段音訊上限為 25 MB；請安裝 ffmpeg 以分段。"));
        }
        let mut bytes = Vec::new();
        file.take(LIMIT + 1).read_to_end(&mut bytes).await.map_err(|_| TranscriptionError::new("無法讀取會議音訊。"))?;
        if bytes.len() as u64 != metadata.len() { return Err(TranscriptionError::new("會議音訊已變更，請重試。")); }
        let token = self.access_token().await?;
        #[cfg(not(test))]
        let object = format!("duduclaw-meetings/{}.m4a", uuid::Uuid::new_v4());
        #[cfg(test)] let object = "duduclaw-meetings/test.m4a".to_owned();
        let uri = format!("gs://{}/{object}", self.config.bucket);
        let upload = format!("{}/upload/storage/v1/b/{}/o", self.storage_base, self.config.bucket);
        // The fsynced journal and cross-process lock precede the upload.
        // Cancellation drops the lock but leaves the intent for reconciliation.
        let mut cleanup = cleanup::PendingObject::create(&self.cleanup_home, &self.config.bucket, &object)?;
        let mut attempted = false;
        let result = async {
            let response = self.client.post(upload).query(&[("uploadType", "media"), ("name", object.as_str()), ("ifGenerationMatch", "0")])
                .bearer_auth(token.as_str()).header("Content-Type", "application/octet-stream").body(bytes).send().await
                .map_err(|_| TranscriptionError::new("Google Cloud 錄音上傳失敗。"))?;
            let uploaded = checked_json(response).await?;
            cleanup.set_generation(uploaded.get("generation").and_then(Value::as_str))?;
            attempted = true;
            tokio::time::timeout(Duration::from_secs(3000), self.recognize(token.as_str(), &uri)).await
                .map_err(|_| TranscriptionError::new("Google Cloud 轉錄等待逾時；雲端要求可能仍在執行及計費。"))?
        }.await;
        let cleaned = self.cleanup_object(&mut cleanup, token.as_str()).await;
        let usage = if attempted { vec![TranscriptionUsage { model: MODEL.into(), estimated: true, ..Default::default() }] } else { Vec::new() };
        let cleaned = match cleaned {
            Ok(cleaned) => cleaned,
            Err(mut error) => { error.usage = usage; return Err(error); }
        };
        if !cleaned {
            return Err(TranscriptionError { detail: None, message: "無法確認雲端錄音已刪除；請管理員檢查儲存桶保留政策，轉錄要求可能已計費。".into(), usage });
        }
        match result {
            Ok(transcript) => Ok(TranscriptionResult { transcript, usage }),
            Err(mut error) => { error.usage = usage; Err(error) }
        }
    }
}

fn milliseconds(value: &Value) -> Result<u64, TranscriptionError> {
    let text = value.as_str().and_then(|s| s.strip_suffix('s')).ok_or_else(|| TranscriptionError::new("Google Cloud 時間標記無效。"))?;
    let seconds = text.parse::<f64>().ok().filter(|s| s.is_finite() && (0.0..=1801.0).contains(s))
        .ok_or_else(|| TranscriptionError::new("Google Cloud 時間標記超過分段上限。"))?;
    Ok((seconds * 1000.0).round() as u64)
}

pub(super) fn map_response(response: &Value, uri: &str, language: &str) -> Result<Transcript, TranscriptionError> {
    let file = response["results"].get(uri).ok_or_else(|| TranscriptionError::new("Google Cloud 逐字稿缺少錄音結果。"))?;
    if let Some(code) = file["error"]["code"].as_i64().filter(|c| *c != 0) { return Err(rpc_error(code)); }
    let transcript = file["inlineResult"]["transcript"].as_object()
        .ok_or_else(|| TranscriptionError::new("Google Cloud 未回傳行內逐字稿。"))?;
    // An empty protobuf repeated field is omitted for a successful silent file.
    let empty = Vec::new();
    let rows = match transcript.get("results") {
        None => &empty,
        Some(value) => value.as_array().ok_or_else(|| TranscriptionError::new("Google Cloud 逐字稿結果格式錯誤。"))?,
    };
    if rows.len() > 30_000 { return Err(TranscriptionError::new("Google Cloud 逐字稿分段過多。")); }
    let mut segments = Vec::new();
    let mut previous = 0;
    for row in rows {
        let alternative = &row["alternatives"][0];
        let text = alternative["transcript"].as_str().filter(|s| !s.trim().is_empty() && s.len() <= 64 * 1024)
            .ok_or_else(|| TranscriptionError::new("Google Cloud 逐字稿內容無效。"))?;
        let words = alternative["words"].as_array().filter(|w| !w.is_empty())
            .ok_or_else(|| TranscriptionError::new("Google Cloud 未回傳逐字時間標記。"))?;
        let start_ms = milliseconds(&words[0]["startOffset"])?;
        let end_ms = milliseconds(&words[words.len() - 1]["endOffset"])?;
        if start_ms < previous || end_ms < start_ms { return Err(TranscriptionError::new("Google Cloud 逐字稿時間順序無效。")); }
        previous = end_ms;
        segments.push(Segment { start_ms, end_ms, speaker: None, text: text.into() });
    }
    Ok(Transcript { language: language.into(), segments })
}
