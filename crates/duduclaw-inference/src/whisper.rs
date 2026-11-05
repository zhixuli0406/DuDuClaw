//! Local Whisper transcription via whisper-rs (whisper.cpp binding).

/// Whisper transcription backend supporting API and Local modes.
pub enum WhisperMode {
    /// OpenAI Whisper API (requires OPENAI_API_KEY).
    Api,
    /// Local whisper.cpp model (requires model file).
    #[cfg(feature = "whisper")]
    Local { model_path: std::path::PathBuf },
}

/// Transcribe audio data to text.
pub async fn transcribe(
    audio_data: &[u8],
    language: Option<&str>,
    mode: &WhisperMode,
) -> Result<String, String> {
    match mode {
        WhisperMode::Api => transcribe_api(audio_data, language).await,
        #[cfg(feature = "whisper")]
        WhisperMode::Local { model_path } => transcribe_local(audio_data, language, model_path),
    }
}

/// Transcribe via OpenAI Whisper API.
async fn transcribe_api(audio_data: &[u8], language: Option<&str>) -> Result<String, String> {
    let api_key = std::env::var("OPENAI_API_KEY")
        .map_err(|_| "OPENAI_API_KEY not set".to_string())?;
    let lang = language.unwrap_or("zh");

    // Detect audio format from magic bytes
    let (file_name, mime_type) = if audio_data.starts_with(b"OggS") {
        ("audio.ogg", "audio/ogg")
    } else if audio_data.starts_with(b"\xff\xfb")
        || audio_data.starts_with(b"\xff\xf3")
        || audio_data.starts_with(b"ID3")
    {
        ("audio.mp3", "audio/mpeg")
    } else if audio_data.starts_with(b"RIFF") {
        ("audio.wav", "audio/wav")
    } else if audio_data.starts_with(b"fLaC") {
        ("audio.flac", "audio/flac")
    } else {
        ("audio.ogg", "audio/ogg") // default fallback
    };

    let form = api_form(audio_data.to_vec(), file_name, mime_type, lang)?;
    let client = reqwest::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(60)).build()
        .map_err(|_| "Whisper HTTP client unavailable".to_string())?;
    let resp = client.post("https://api.openai.com/v1/audio/transcriptions")
        .header("Authorization", format!("Bearer {api_key}"))
        .multipart(form)
        .send().await
        .map_err(|e| format!("Whisper API: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Whisper API {}", resp.status()));
    }
    read_transcription_response(resp).await
}

async fn read_transcription_response(resp: reqwest::Response) -> Result<String, String> {
    read_transcription_chunks(resp.content_length(), resp.bytes_stream()).await
}

async fn read_transcription_chunks<T: AsRef<[u8]>, E>(length: Option<u64>,
    mut chunks: impl futures_util::Stream<Item = Result<T, E>> + Unpin) -> Result<String, String> {
    use futures_util::StreamExt;
    const RESPONSE_LIMIT: usize = 256 * 1024;
    if length.is_some_and(|length| length > RESPONSE_LIMIT as u64) {
        return Err("Whisper response too large".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.map_err(|_| "Whisper response unavailable".to_string())?;
        let chunk = chunk.as_ref();
        if chunk.len() > RESPONSE_LIMIT.saturating_sub(bytes.len()) { return Err("Whisper response too large".into()); }
        bytes.extend_from_slice(chunk);
    }
    let result: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| "Invalid Whisper response".to_string())?;
    result.get("text").and_then(|t| t.as_str()).map(|s| s.to_string())
        .ok_or("No text".to_string())
}

fn api_form(audio_data: Vec<u8>, file_name: &str, mime_type: &str, language: &str)
    -> Result<reqwest::multipart::Form, String> {
    let part = reqwest::multipart::Part::bytes(audio_data).file_name(file_name.to_owned())
        .mime_str(mime_type).map_err(|_| "Invalid audio MIME type".to_string())?;
    let mut form = reqwest::multipart::Form::new().text("model", "whisper-1").part("file", part);
    if !language.is_empty() { form = form.text("language", language.to_owned()); }
    Ok(form)
}

/// The meeting adapter uses the same Whisper API with segment-level timestamps.
/// The caller supplies credentials so no process-wide environment mutation is needed.
pub(crate) async fn transcribe_api_verbose(
    client: &reqwest::Client,
    endpoint: &str,
    api_key: &str,
    audio_data: Vec<u8>,
    file_name: &str,
    mime_type: &str,
    language: &str,
) -> Result<reqwest::Response, String> {
    let form = api_form(audio_data, file_name, mime_type, language)?
        .text("response_format", "verbose_json")
        .text("timestamp_granularities[]", "segment");
    client.post(endpoint).bearer_auth(api_key).multipart(form).send().await
        .map_err(|_| "無法連線至 Whisper 轉錄服務。".to_string())
}

/// Transcribe locally via whisper.cpp.
#[cfg(feature = "whisper")]
fn transcribe_local(
    audio_data: &[u8],
    language: Option<&str>,
    model_path: &std::path::Path,
) -> Result<String, String> {
    use whisper_rs::{WhisperContext, WhisperContextParameters, FullParams, SamplingStrategy};

    // Validate that input appears to be raw PCM (not encoded audio like OGG/MP3)
    // OGG files start with "OggS" magic bytes
    if audio_data.len() >= 4 && &audio_data[0..4] == b"OggS" {
        return Err("transcribe_local expects raw 16-bit PCM data, not OGG. Convert audio first.".to_string());
    }
    // ID3 tag or MP3/MPEG sync word: first 11 bits are all 1s (0xFF + upper 3 bits of next byte)
    if (audio_data.len() >= 3 && &audio_data[0..3] == b"ID3")
        || (audio_data.len() >= 2 && audio_data[0] == 0xFF && (audio_data[1] & 0xE0) == 0xE0)
    {
        return Err(anyhow::anyhow!("transcribe_local expects raw 16-bit PCM data, not MP3/MPEG. Convert audio first.").to_string());
    }

    let ctx = WhisperContext::new_with_params(
        model_path.to_str().ok_or("Invalid model path")?,
        WhisperContextParameters::default(),
    ).map_err(|e| format!("Load model: {e}"))?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    if let Some(lang) = language {
        params.set_language(Some(lang));
    } else {
        params.set_language(Some("zh"));
    }
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);

    // Decode audio (assume 16kHz mono PCM f32)
    // In practice, you'd convert OGG/MP3 to PCM first
    let samples: Vec<f32> = audio_data.chunks(2)
        .map(|c| {
            let val = i16::from_le_bytes([c[0], *c.get(1).unwrap_or(&0)]);
            val as f32 / 32768.0
        })
        .collect();

    let mut state = ctx.create_state().map_err(|e| format!("State: {e}"))?;
    state.full(params, &samples).map_err(|e| format!("Transcribe: {e}"))?;

    let n = state.full_n_segments().map_err(|e| format!("Segments: {e}"))?;
    let mut text = String::new();
    for i in 0..n {
        if let Ok(seg) = state.full_get_segment_text(i) {
            text.push_str(&seg);
        }
    }
    Ok(text.trim().to_string())
}

#[cfg(test)]
mod i1c_tests {
    use super::*;
    #[tokio::test]
    async fn i1c_whisper_rejects_declared_and_streamed_oversize_without_credentials() {
        let empty = futures_util::stream::iter(Vec::<Result<Vec<u8>, ()>>::new());
        assert_eq!(read_transcription_chunks(Some(9999999), empty).await.unwrap_err(), "Whisper response too large");
        let chunks = futures_util::stream::iter(vec![Ok::<_, ()>(vec![b'x'; 200 * 1024]), Ok(vec![b'x'; 100 * 1024])]);
        assert_eq!(read_transcription_chunks(None, chunks).await.unwrap_err(), "Whisper response too large");
    }
    #[tokio::test]
    async fn i1c_whisper_bounded_response_preserves_transcript_and_rejects_invalid_json() {
        let chunks = futures_util::stream::iter(vec![Ok::<_, ()>(br#"{"text":"hello"}"#.to_vec())]);
        assert_eq!(read_transcription_chunks(None, chunks).await.unwrap(), "hello");
        let invalid = futures_util::stream::iter(vec![Ok::<_, ()>(vec![b'{'])]);
        assert!(read_transcription_chunks(None, invalid).await.is_err());
        let truncated = futures_util::stream::iter(vec![Ok(br#"{"text":"hello"}"#.to_vec()), Err(())]);
        assert!(read_transcription_chunks(None, truncated).await.is_err());
    }
}
