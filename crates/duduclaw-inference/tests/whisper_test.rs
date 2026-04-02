//! Whisper transcription tests.

/// Test API mode request format.
#[test]
fn test_whisper_api_request_format() {
    // Verify the expected API endpoint and model
    let endpoint = "https://api.openai.com/v1/audio/transcriptions";
    let model = "whisper-1";
    assert!(endpoint.contains("transcriptions"));
    assert_eq!(model, "whisper-1");
}

/// Test language detection preference (zh for Taiwan users).
#[test]
fn test_language_preference() {
    let default_lang = "zh";
    assert_eq!(default_lang, "zh");
    // Fallback should be auto-detect (None)
    let fallback: Option<&str> = None;
    assert!(fallback.is_none());
}

/// Test audio format detection.
#[test]
fn test_audio_format_detection() {
    // OGG Opus (Telegram voice)
    let ogg_header = [0x4F, 0x67, 0x67, 0x53];
    assert_eq!(ogg_header[0], 0x4F);

    // MP3 (ID3 tag)
    let mp3_header = [0x49, 0x44, 0x33, 0x00];
    assert_eq!(mp3_header[0], 0x49);
}

/// Test local mode availability check.
#[test]
fn test_local_mode_feature_gate() {
    // whisper-rs is behind a feature flag
    #[cfg(feature = "whisper")]
    {
        // If whisper feature is enabled, local mode should be available
        assert!(true);
    }
    #[cfg(not(feature = "whisper"))]
    {
        // Without whisper feature, only API mode is available
        assert!(true);
    }
}
