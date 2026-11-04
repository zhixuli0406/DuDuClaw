//! Timed adapter over the existing OpenAI Whisper API transport.

use std::path::Path;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use tokio::io::AsyncReadExt;
use zeroize::Zeroizing;
use super::{audio_mime, bounded_json, http_client, Segment, Transcript, TranscriptionEngine, TranscriptionError, TranscriptionResult, TranscriptionUsage};

const LIMIT: u64 = 25 * 1024 * 1024;

pub struct WhisperEngine {
    key: Zeroizing<String>,
    client: reqwest::Client,
    endpoint: String,
}

impl WhisperEngine {
    pub fn new(api_key: String) -> Result<Self, TranscriptionError> {
        if api_key.trim().is_empty() { return Err(TranscriptionError::new("請先設定 OpenAI API 憑證。")); }
        Ok(Self { key: Zeroizing::new(api_key), client: http_client()?, endpoint: "https://api.openai.com/v1/audio/transcriptions".into() })
    }

    #[cfg(test)]
    pub(super) fn for_test(endpoint: String) -> Self {
        let mut engine = Self::new("test-key".into()).unwrap();
        engine.endpoint = endpoint;
        engine
    }
}

#[async_trait]
impl TranscriptionEngine for WhisperEngine {
    async fn transcribe(&self, audio_path: &Path, language_hint: &str, _speaker_context: &[String]) -> Result<TranscriptionResult, TranscriptionError> {
        let mime = audio_mime(audio_path)?;
        let file = tokio::fs::File::open(audio_path).await.map_err(|_| TranscriptionError::new("無法讀取會議音訊。"))?;
        let metadata = file.metadata().await.map_err(|_| TranscriptionError::new("無法讀取會議音訊資訊。"))?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > LIMIT {
            return Err(TranscriptionError::new("Whisper 每段音訊不得超過 25 MB；請安裝 ffmpeg 以壓縮及分段。"));
        }
        let mut bytes = Vec::new();
        file.take(LIMIT + 1).read_to_end(&mut bytes).await.map_err(|_| TranscriptionError::new("無法讀取會議音訊。"))?;
        if bytes.len() as u64 != metadata.len() { return Err(TranscriptionError::new("會議音訊已變更，請重試。")); }
        // Whisper expects ISO 639-1 rather than a regional BCP-47 language hint.
        let language = language_hint.split('-').next().unwrap_or("");
        if !language.is_empty() && (language.len() != 2 || !language.bytes().all(|v| v.is_ascii_lowercase())) {
            return Err(TranscriptionError::new("Whisper 語言提示必須使用語言代碼，例如 zh-TW 或 en。"));
        }
        let extension = audio_path.extension().and_then(|v| v.to_str()).unwrap_or("m4a");
        let value = async {
            let response = crate::whisper::transcribe_api_verbose(
                &self.client, &self.endpoint, self.key.as_str(), bytes,
                &format!("audio.{extension}"), mime, language,
            ).await.map_err(TranscriptionError::new)?;
            bounded_json(response).await
        }.await.map_err(|mut error| {
            // The provider may have processed audio before the response was lost.
            error.usage.push(TranscriptionUsage { model: "openai/whisper-1".into(), ..Default::default() });
            error
        })?;
        let usage = vec![TranscriptionUsage {
            model: "openai/whisper-1".into(),
            audio_seconds: value["duration"].as_f64().filter(|v| v.is_finite() && *v >= 0.0),
            ..Default::default()
        }];
        match parse_verbose(value) {
            Ok(transcript) => Ok(TranscriptionResult { transcript, usage }),
            Err(mut error) => { error.usage = usage; Err(error) }
        }
    }
}

#[derive(Deserialize)]
struct VerboseTranscript { language: String, duration: f64, segments: Vec<VerboseSegment> }
#[derive(Deserialize)]
struct VerboseSegment { start: f64, end: f64, text: String }

pub(super) fn parse_verbose(value: Value) -> Result<Transcript, TranscriptionError> {
    let invalid = || TranscriptionError::new("Whisper 逐字稿缺少有效的時間標記或語言資訊。");
    let wire: VerboseTranscript = serde_json::from_value(value).map_err(|_| invalid())?;
    if wire.language.trim().is_empty() || wire.language.len() > 64 || !wire.duration.is_finite()
        || !(0.0..=7.0 * 24.0 * 3600.0).contains(&wire.duration) || wire.segments.len() > 30_000 {
        return Err(invalid());
    }
    let mut segments = Vec::with_capacity(wire.segments.len());
    let mut previous = 0.0;
    for segment in wire.segments {
        if !segment.start.is_finite() || !segment.end.is_finite() || segment.start < previous
            || segment.end < segment.start || segment.end > wire.duration + 1.0
            || segment.text.trim().is_empty() || segment.text.len() > 64 * 1024 {
            return Err(invalid());
        }
        previous = segment.start;
        segments.push(Segment { start_ms: (segment.start * 1000.0).round() as u64,
            end_ms: (segment.end * 1000.0).round() as u64, speaker: None, text: segment.text });
    }
    Ok(Transcript { language: wire.language, segments })
}
