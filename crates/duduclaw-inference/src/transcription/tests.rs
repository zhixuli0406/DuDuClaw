use super::*;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn transcript_value() -> Value {
    json!({"language":"zh-TW", "segments":[{"start_ms":100, "end_ms":1500, "speaker":"講者 1", "text":"會議開始。"}]})
}

fn gemini_response(text: &str) -> Value {
    json!({"candidates":[{"finishReason":"STOP","content":{"parts":[{"text":text}]}}],
        "usageMetadata":{"promptTokenCount":110,"candidatesTokenCount":30,"thoughtsTokenCount":5,
            "promptTokensDetails":[{"modality":"AUDIO","tokenCount":100},{"modality":"TEXT","tokenCount":10}]}})
}

#[test]
fn transcription_gemini_json_is_strict() {
    let good = transcript_value();
    assert_eq!(gemini::parse_transcript_json(&good.to_string()).unwrap().segments[0].end_ms, 1500);
    for input in ["not json".into(), format!("```json\n{good}\n```"), "{}".into(),
        r#"{"language":"zh","segments":[{"start_ms":0,"end_ms":1,"text":"hi"}]}"#.into()] {
        assert!(gemini::parse_transcript_json(&input).is_err());
    }
    for (key, value) in [("start_ms", json!(-1)), ("start_ms", json!(1.5)), ("end_ms", json!(99)),
        ("text", json!("")), ("speaker", json!("講者\n1")), ("extra", json!(true))] {
        let mut bad = good.clone();
        bad["segments"][0][key] = value;
        assert!(gemini::parse_transcript_json(&bad.to_string()).is_err(), "{key}");
    }
    assert!(gemini::parse_transcript_json(r#"{"language":"zh","language":"en","segments":[]}"#).is_err());
}

#[test]
fn transcription_upload_location_cannot_redirect_credentials() {
    let base = "https://generativelanguage.googleapis.com";
    assert!(gemini::validate_upload_url(base, &format!("{base}/upload/v1beta/files?upload_id=abc")).is_ok());
    for url in ["http://generativelanguage.googleapis.com/upload/v1beta/files",
        "https://generativelanguage.googleapis.com.evil.test/upload/v1beta/files",
        "https://evil.test/upload/v1beta/files", "https://generativelanguage.googleapis.com:444/upload/v1beta/files",
        "https://user@generativelanguage.googleapis.com/upload/v1beta/files",
        "https://generativelanguage.googleapis.com/v1beta/files", "file:///upload/v1beta/files",
        "https://generativelanguage.googleapis.com/upload/v1beta/files#fragment"] {
        assert!(gemini::validate_upload_url(base, url).is_err(), "{url}");
    }
}

#[test]
fn transcription_model_path_cannot_inject_endpoint() {
    for model in ["", "gemini/../files", "gemini/x?key=bad", "gemini/x/y", "https://evil.test"] {
        assert!(GeminiEngine::new("test".into(), model.into()).is_err());
    }
}

#[test]
fn transcription_whisper_timestamps_and_validation() {
    let transcript = whisper::parse_verbose(json!({"language":"chinese", "duration":1.2,
        "segments":[{"start":0.125,"end":1.125,"text":"您好","avg_logprob":-0.3}]})).unwrap();
    assert_eq!(transcript.segments[0].start_ms, 125);
    assert_eq!(transcript.segments[0].speaker, None);
    assert!(whisper::parse_verbose(json!({"language":"chinese","duration":1.2,
        "segments":[{"start":-1,"end":1,"text":"hi"}]})).is_err());
    assert!(whisper::parse_verbose(json!({"language":"chinese","duration":1.2,
        "segments":[{"start":0,"end":20,"text":"hi"}]})).is_err());
}

struct Reply { status: u16, headers: Vec<(&'static str, String)>, body: String }
impl Reply {
    fn json(body: Value) -> Self { Self { status: 200, headers: vec![], body: body.to_string() } }
}

struct MockServer {
    base: String,
    requests: Arc<Mutex<Vec<Vec<u8>>>>,
    task: tokio::task::JoinHandle<()>,
}

impl MockServer {
    async fn start(replies: Vec<Reply>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let response_base = base.clone();
        let task = tokio::spawn(async move {
            for reply in replies {
                let (mut socket, _) = tokio::time::timeout(std::time::Duration::from_secs(10), listener.accept()).await.unwrap().unwrap();
                let mut request = Vec::new();
                let mut buf = [0_u8; 8192];
                let (head_end, content_len) = loop {
                    let n = socket.read(&mut buf).await.unwrap();
                    assert!(n > 0);
                    request.extend_from_slice(&buf[..n]);
                    if let Some(pos) = request.windows(4).position(|v| v == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&request[..pos]);
                        let len = head.lines().filter_map(|line| line.split_once(':'))
                            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                            .map(|(_, value)| value.trim().parse::<usize>().unwrap()).unwrap_or(0);
                        break (pos + 4, len);
                    }
                };
                while request.len() < head_end + content_len {
                    let n = socket.read(&mut buf).await.unwrap();
                    assert!(n > 0);
                    request.extend_from_slice(&buf[..n]);
                }
                captured.lock().unwrap().push(request);
                let body = reply.body.replace("MOCK_BASE", &response_base);
                let headers = reply.headers.iter().map(|(name, value)| format!("{name}: {}\r\n", value.replace("MOCK_BASE", &response_base))).collect::<String>();
                socket.write_all(format!("HTTP/1.1 {} Mock\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}", reply.status, body.len()).as_bytes()).await.unwrap();
            }
        });
        Self { base, requests, task }
    }

    async fn finish(self) -> Vec<Vec<u8>> {
        self.task.await.unwrap();
        Arc::try_unwrap(self.requests).unwrap().into_inner().unwrap()
    }
}

fn request_body(request: &[u8]) -> &[u8] {
    let offset = request.windows(4).position(|v| v == b"\r\n\r\n").unwrap() + 4;
    &request[offset..]
}

#[tokio::test]
async fn transcription_gemini_inline_retries_once_and_keeps_both_usage_records() {
    let server = MockServer::start(vec![Reply::json(gemini_response("invalid JSON")), Reply::json(gemini_response(&transcript_value().to_string()))]).await;
    let engine = GeminiEngine::for_test(server.base.clone(), 1024);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("audio.m4a");
    std::fs::write(&path, b"example audio").unwrap();
    let result = engine.transcribe(&path, "zh-TW", &["講者 1".into()]).await.unwrap();
    assert_eq!(result.usage.len(), 2);
    assert_eq!(result.usage[0].input_audio_tokens, 100);
    assert_eq!(result.usage[0].reasoning_tokens, 5);
    let requests = server.finish().await;
    let first: Value = serde_json::from_slice(request_body(&requests[0])).unwrap();
    let second: Value = serde_json::from_slice(request_body(&requests[1])).unwrap();
    assert_eq!(first.pointer("/contents/0/parts/1/inlineData/mimeType").unwrap(), "audio/mp4");
    assert_ne!(first.pointer("/contents/0/parts/0/text"), second.pointer("/contents/0/parts/0/text"));
}

#[tokio::test]
async fn transcription_gemini_rejects_twice_malformed_with_usage() {
    let server = MockServer::start(vec![Reply::json(gemini_response("oops")), Reply::json(gemini_response("oops"))]).await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("audio.wav");
    std::fs::write(&path, b"sample").unwrap();
    let error = GeminiEngine::for_test(server.base.clone(), 1024).transcribe(&path, "zh-TW", &[]).await.unwrap_err();
    assert_eq!(error.usage.len(), 2);
    assert!(!error.message.contains("oops"));
    assert_eq!(server.finish().await.len(), 2);
}

#[tokio::test]
async fn transcription_gemini_resumable_upload_and_cleanup() {
    let server = MockServer::start(vec![
        Reply { status: 200, headers: vec![("x-goog-upload-url", "MOCK_BASE/upload/v1beta/files?upload_id=example".into())], body: "{}".into() },
        Reply::json(json!({"file":{"name":"files/example", "uri":"MOCK_BASE/v1beta/files/example", "state":"ACTIVE"}})),
        Reply::json(gemini_response(&transcript_value().to_string())),
        Reply::json(json!({})),
    ]).await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("audio.m4a");
    std::fs::write(&path, b"sample upload").unwrap();
    GeminiEngine::for_test(server.base.clone(), 1).transcribe(&path, "zh-TW", &[]).await.unwrap();
    let requests = server.finish().await;
    assert_eq!(request_body(&requests[1]), b"sample upload");
    assert!(!String::from_utf8_lossy(&requests[1]).contains("x-goog-api-key"));
    assert!(String::from_utf8_lossy(&requests[3]).starts_with("DELETE /v1beta/files/example "));
}

#[tokio::test]
async fn transcription_gemini_cleanup_after_processing_failure() {
    let server = MockServer::start(vec![
        Reply { status: 200, headers: vec![("x-goog-upload-url", "MOCK_BASE/upload/v1beta/files?upload_id=example".into())], body: "{}".into() },
        Reply::json(json!({"file":{"name":"files/example", "uri":"MOCK_BASE/v1beta/files/example", "state":"FAILED"}})),
        Reply::json(json!({})),
    ]).await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("audio.m4a");
    std::fs::write(&path, b"sample").unwrap();
    assert!(GeminiEngine::for_test(server.base.clone(), 1).transcribe(&path, "zh-TW", &[]).await.is_err());
    assert_eq!(server.finish().await.len(), 3);
}

#[tokio::test]
async fn transcription_whisper_mock_uses_verbose_segments() {
    let server = MockServer::start(vec![Reply::json(json!({"language":"chinese","duration":1.5,
        "segments":[{"start":0.1,"end":1.5,"text":"你好"}]}))]).await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("audio.m4a");
    std::fs::write(&path, b"sample").unwrap();
    let result = WhisperEngine::for_test(format!("{}/audio/transcriptions", server.base))
        .transcribe(&path, "zh-TW", &[]).await.unwrap();
    assert_eq!(result.transcript.segments[0].start_ms, 100);
    assert_eq!(result.usage[0].audio_seconds, Some(1.5));
    let requests = server.finish().await;
    let body = String::from_utf8_lossy(request_body(&requests[0]));
    assert!(body.contains("verbose_json"));
    assert!(body.contains("timestamp_granularities[]"));
    assert!(body.contains("audio.m4a"));
    assert!(!body.contains("zh-TW"));
}

#[test]
fn transcription_hostile_usage_is_replaced_by_bounded_estimates() {
    let huge = TranscriptionUsage { model: "gemini/test".into(), input_tokens: u64::MAX,
        output_tokens: u64::MAX, reasoning_tokens: u64::MAX, input_audio_tokens: u64::MAX, ..Default::default() };
    let bounded = bounded_usage(&huge, Some(10_000));
    assert!(bounded.estimated);
    assert_eq!(bounded.input_audio_tokens, 320);
    assert_eq!(bounded.input_tokens, 4416);
    assert_eq!(bounded.output_tokens, 80);
    let whisper = TranscriptionUsage { model: "openai/whisper-1".into(), audio_seconds: Some(1_000_000.0), ..Default::default() };
    let bounded = bounded_usage(&whisper, Some(1500));
    assert!(bounded.estimated);
    assert_eq!(bounded.audio_seconds, Some(1.5));
    let valid = TranscriptionUsage { model: "openai/whisper-1".into(), audio_seconds: Some(1.5), ..Default::default() };
    assert!(!bounded_usage(&valid, Some(1500)).estimated);
    assert!(bounded_usage(&valid, None).estimated);
    let nan = TranscriptionUsage { model: "openai/whisper-1".into(), audio_seconds: Some(f64::NAN), ..Default::default() };
    assert_eq!(bounded_usage(&nan, Some(1000)).audio_seconds, Some(1.0));
}

#[test]
fn transcription_pricing_saturates_hostile_token_sums() {
    let registry = duduclaw_llm::ModelRegistry::vendored();
    let model = registry.get("gemini/gemini-3.5-flash").unwrap();
    let usage = duduclaw_llm::NormalizedUsage {
        input_tokens: u64::MAX, output_tokens: u64::MAX, reasoning_tokens: u64::MAX,
        cache_read_tokens: u64::MAX, cache_write_tokens: u64::MAX, ..Default::default()
    };
    assert!(registry.cost_audio_millicents(&usage, u64::MAX, model) > 0);
}

#[test]
fn transcription_whisper_tiny_provider_duration_uses_local_chunk_length() {
    let usage = TranscriptionUsage { model: "openai/whisper-1".into(),
        audio_seconds: Some(0.001), ..Default::default() };
    let bounded = bounded_usage(&usage, Some(600_000));
    assert_eq!(bounded.audio_seconds, Some(600.0));
    assert!(bounded.estimated);
    assert_eq!(bounded_usage(&usage, Some(1500)).audio_seconds, Some(1.5));
}

#[test]
fn transcription_gemini_one_token_uses_local_audio_estimate() {
    let usage = TranscriptionUsage { model: "gemini/test".into(),
        input_tokens: 1, input_audio_tokens: 1, ..Default::default() };
    let bounded = bounded_usage(&usage, Some(600_000));
    assert_eq!(bounded.input_audio_tokens, 19_200);
    assert_eq!(bounded.input_tokens, 23_296);
    assert!(bounded.estimated);
    // The 50% threshold accepts its boundary and refuses one token below it.
    let boundary = TranscriptionUsage { input_tokens: 9600, input_audio_tokens: 9600, ..usage.clone() };
    assert!(!bounded_usage(&boundary, Some(600_000)).estimated);
    let below = TranscriptionUsage { input_tokens: 9599, input_audio_tokens: 9599, ..usage };
    assert!(bounded_usage(&below, Some(600_000)).estimated);
    assert!(bounded_usage(&boundary, None).estimated);
}

fn google_config() -> config::GoogleSttConfig {
    config::GoogleSttConfig { project_id: "meeting-project".into(), bucket: "meeting-audio".into(), ..Default::default() }
}

fn google_response() -> Value {
    json!({"results":{"gs://meeting-audio/duduclaw-meetings/test.m4a":{
        "inlineResult":{"transcript":{"results":[{"alternatives":[{"transcript":"會議開始。", "words":[
            {"word":"會議", "startOffset":"0.125s", "endOffset":"0.7s"},
            {"word":"開始", "startOffset":"0.7s", "endOffset":"1.250s"}]}]}]}}
    }}})
}

#[test]
fn transcription_google_stt_request_config_and_mapping() {
    let engine = GoogleSttEngine::for_test("http://127.0.0.1".into(), google_config());
    let body = engine.batch_request("gs://meeting-audio/test.m4a");
    assert_eq!(body["config"]["model"], "chirp_2");
    assert_eq!(body["config"]["languageCodes"][0], "cmn-Hant-TW");
    assert_eq!(body["recognitionOutputConfig"], json!({"inlineResponseConfig":{}}));
    assert!(body["config"]["features"].get("diarizationConfig").is_none());
    let mapped = google_stt::map_response(&google_response(), "gs://meeting-audio/duduclaw-meetings/test.m4a", "cmn-Hant-TW").unwrap();
    assert_eq!(mapped.segments[0].start_ms, 125);
    assert_eq!(mapped.segments[0].end_ms, 1250);
    assert_eq!(mapped.segments[0].speaker, None);
    assert!(google_stt::map_response(&google_response(), "gs://different/file", "cmn-Hant-TW").is_err());
    for value in ["NaNs", "-1s", "1802s", "1", "1e99s"] {
        let mut response = google_response();
        response["results"]["gs://meeting-audio/duduclaw-meetings/test.m4a"]["inlineResult"]["transcript"]["results"][0]["alternatives"][0]["words"][0]["startOffset"] = json!(value);
        assert!(google_stt::map_response(&response, "gs://meeting-audio/duduclaw-meetings/test.m4a", "cmn-Hant-TW").is_err());
    }
}

#[test]
fn transcription_google_stt_rejects_config_and_untrusted_operations() {
    let config = google_config();
    assert!(config.validate().is_ok());
    for field in ["project", "bucket", "location", "model", "language", "diarization"] {
        let mut cfg = config.clone();
        match field { "project" => cfg.project_id = "../other".into(), "bucket" => cfg.bucket = "bucket?name".into(),
            "location" => cfg.location = "host.evil.test".into(), "model" => cfg.model = "long".into(),
            "language" => cfg.language = "en-US".into(), _ => cfg.diarization = true }
        assert!(cfg.validate().is_err(), "{field}");
    }
    let engine = GoogleSttEngine::for_test("http://127.0.0.1".into(), config);
    assert!(engine.operation_path("projects/meeting-project/locations/asia-southeast1/operations/abc-12").is_ok());
    for name in ["https://evil.test", "projects/other/locations/asia-southeast1/operations/a",
        "projects/meeting-project/locations/asia-southeast1/operations/../../evil",
        "projects/meeting-project/locations/asia-southeast1/operations/a?key=secret"] {
        assert!(engine.operation_path(name).is_err());
    }
    assert!(google_stt::validate_service_account_json("{\"api_key\":\"secret\"}").is_err());
    let usage = bounded_usage(&TranscriptionUsage { model: "google-stt/chirp_2".into(), estimated: true, ..Default::default() }, Some(31_250));
    assert_eq!(usage.audio_seconds, Some(31.25));
    assert_eq!(usage.input_tokens, 0);
    assert!(usage.estimated);
}

#[tokio::test]
async fn transcription_google_stt_batch_upload_poll_and_cleanup() {
    let server = MockServer::start(vec![Reply::json(json!({"name":"uploaded"})),
        Reply::json(json!({"name":"projects/meeting-project/locations/asia-southeast1/operations/one"})),
        Reply::json(json!({"done":true,"response":google_response()})), Reply::json(json!({}))]).await;
    let engine = GoogleSttEngine::for_test(server.base.clone(), google_config());
    let dir = tempfile::tempdir().unwrap();
    let audio = dir.path().join("sample.m4a");
    std::fs::write(&audio, b"audio bytes").unwrap();
    let result = engine.transcribe(&audio, "zh-TW", &[]).await.unwrap();
    assert_eq!(result.transcript.language, "cmn-Hant-TW");
    assert_eq!(result.usage[0].model, "google-stt/chirp_2");
    let requests = server.finish().await;
    assert_eq!(requests.len(), 4);
    let upload = String::from_utf8_lossy(&requests[0]);
    assert!(upload.contains("ifGenerationMatch=0"));
    assert!(upload.contains("test-token"));
    assert_eq!(request_body(&requests[0]), b"audio bytes");
    let body: Value = serde_json::from_slice(request_body(&requests[1])).unwrap();
    assert_eq!(body["files"][0]["uri"], "gs://meeting-audio/duduclaw-meetings/test.m4a");
    assert!(String::from_utf8_lossy(&requests[3]).starts_with("DELETE /storage/v1/b/meeting-audio/o/duduclaw-meetings%2Ftest.m4a"));
}

#[tokio::test]
async fn transcription_google_stt_auth_quota_errors_are_sanitized_and_cleaned() {
    for status in [401, 403, 429] {
        let server = MockServer::start(vec![Reply::json(json!({"name":"uploaded"})),
            Reply { status, headers: vec![], body:"secret-provider-diagnostic".into() }, Reply::json(json!({}))]).await;
        let engine = GoogleSttEngine::for_test(server.base.clone(), google_config());
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("sample.m4a");
        std::fs::write(&audio, b"audio").unwrap();
        let error = engine.transcribe(&audio, "cmn-Hant-TW", &[]).await.unwrap_err();
        assert!(!error.message.contains("secret-provider-diagnostic"));
        assert!(error.message.contains(if status == 429 { "配額" } else { "權限" }));
        assert_eq!(error.usage.len(), 1);
        assert_eq!(server.finish().await.len(), 3);
    }
}

#[test]
fn transcription_google_stt_silence_requires_transcript_object() {
    let uri = "gs://meeting-audio/duduclaw-meetings/test.m4a";
    for transcript in [json!({}), json!({"results":[]})] {
        let response = json!({"results":{(uri):{"inlineResult":{"transcript":transcript}}}});
        assert!(google_stt::map_response(&response, uri, "cmn-Hant-TW").unwrap().segments.is_empty());
    }
    for file in [json!({}), json!({"inlineResult":{}}), json!({"inlineResult":{"transcript":null}}),
        json!({"inlineResult":{"transcript":{"results":null}}})] {
        assert!(google_stt::map_response(&json!({"results":{(uri):file}}), uri, "cmn-Hant-TW").is_err());
    }
}

#[tokio::test]
async fn transcription_google_stt_upload_errors_cleanup_without_recognition_usage() {
    for status in [401, 403, 429] {
        let server = MockServer::start(vec![Reply { status, headers: vec![], body:"secret-upload-diagnostic".into() },
            Reply::json(json!({}))]).await;
        let engine = GoogleSttEngine::for_test(server.base.clone(), google_config());
        let dir = tempfile::tempdir().unwrap();
        let audio = dir.path().join("sample.m4a");
        std::fs::write(&audio, b"audio").unwrap();
        let error = engine.transcribe(&audio, "cmn-Hant-TW", &[]).await.unwrap_err();
        assert!(!error.message.contains("secret-upload-diagnostic"));
        assert!(error.message.contains(if status == 429 { "配額" } else { "權限" }));
        assert!(error.usage.is_empty());
        let requests = server.finish().await;
        assert_eq!(requests.len(), 2);
        assert!(String::from_utf8_lossy(&requests[1]).starts_with("DELETE "));
    }
}

#[tokio::test]
async fn transcription_google_stt_cleanup_failure_preserves_paid_attempt_usage() {
    let server = MockServer::start(vec![Reply::json(json!({"name":"uploaded"})),
        Reply::json(json!({"done":true,"response":google_response()})),
        Reply { status: 403, headers: vec![], body:"secret-delete-diagnostic".into() }]).await;
    let engine = GoogleSttEngine::for_test(server.base.clone(), google_config());
    let dir = tempfile::tempdir().unwrap();
    let audio = dir.path().join("sample.m4a");
    std::fs::write(&audio, b"audio").unwrap();
    let error = engine.transcribe(&audio, "cmn-Hant-TW", &[]).await.unwrap_err();
    assert!(error.message.contains("無法確認雲端錄音已刪除"));
    assert!(!error.message.contains("secret-delete-diagnostic"));
    assert_eq!(error.usage.len(), 1);
    assert_eq!(error.usage[0].model, "google-stt/chirp_2");
    assert_eq!(server.finish().await.len(), 3);
}

#[tokio::test]
async fn transcription_google_stt_empty_and_oversize_audio_refused_before_http() {
    let server = MockServer::start(vec![]).await;
    let engine = GoogleSttEngine::for_test(server.base.clone(), google_config());
    let dir = tempfile::tempdir().unwrap();
    let audio = dir.path().join("sample.m4a");
    let file = std::fs::File::create(&audio).unwrap();
    for length in [0, 25 * 1024 * 1024 + 1] {
        file.set_len(length).unwrap();
        let error = engine.transcribe(&audio, "cmn-Hant-TW", &[]).await.unwrap_err();
        assert!(error.message.contains("25 MB"));
        assert!(error.usage.is_empty());
    }
    assert!(server.finish().await.is_empty());
}
