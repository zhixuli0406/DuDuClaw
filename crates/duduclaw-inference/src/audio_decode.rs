//! Audio decoding pipeline — any format → PCM f32 mono 16kHz.
//!
//! Supports: OGG Opus (Telegram voice), M4A/AAC (LINE audio), MP3, WAV, FLAC.
//! Uses the `symphonia` crate for pure-Rust decoding (no ffmpeg dependency).

use std::io::Cursor;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use tracing::debug;

use crate::error::InferenceError;

/// Target sample rate for ASR models (Whisper, SenseVoice).
const TARGET_SAMPLE_RATE: u32 = 16_000;
/// Maximum input audio size (25 MB — matches Whisper API limit).
const MAX_AUDIO_BYTES: usize = 25 * 1024 * 1024;
/// Maximum PCM output samples (~10 minutes at 16kHz mono).
const MAX_PCM_SAMPLES: usize = 16_000 * 60 * 10;

/// Decode audio bytes (any supported format) to PCM f32, mono, 16kHz.
///
/// This is the universal entry point for the ASR pipeline. Input can be
/// OGG Opus, MP3, M4A/AAC, WAV, FLAC — format is auto-detected from magic bytes.
///
/// Enforces a 25MB input limit and 10-minute output limit to prevent OOM.
pub fn decode_to_pcm(data: &[u8]) -> Result<Vec<f32>, InferenceError> {
    if data.len() < 4 {
        return Err(InferenceError::Other("Audio data too short".into()));
    }
    if data.len() > MAX_AUDIO_BYTES {
        return Err(InferenceError::Other(format!(
            "Audio too large: {} bytes (max {})",
            data.len(),
            MAX_AUDIO_BYTES
        )));
    }

    let cursor = Cursor::new(data.to_vec());
    let mss = MediaSourceStream::new(Box::new(cursor), Default::default());

    let mut hint = Hint::new();
    // Help symphonia with format hints from magic bytes
    match &data[..4] {
        [0x4F, 0x67, 0x67, 0x53] => hint.with_extension("ogg"),   // OGG
        [0x49, 0x44, 0x33, ..] => hint.with_extension("mp3"),      // MP3 ID3
        [0xFF, 0xFB, ..] | [0xFF, 0xF3, ..] => hint.with_extension("mp3"), // MP3 sync
        [0x52, 0x49, 0x46, 0x46] => hint.with_extension("wav"),   // WAV
        [0x66, 0x4C, 0x61, 0x43] => hint.with_extension("flac"),  // FLAC
        _ => &mut hint, // Let symphonia probe
    };

    let format_opts = FormatOptions { enable_gapless: true, ..Default::default() };
    let metadata_opts = MetadataOptions::default();
    let decoder_opts = DecoderOptions::default();

    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &format_opts, &metadata_opts)
        .map_err(|e| InferenceError::Other(format!("Failed to probe audio format: {e}")))?;

    let mut format_reader = probed.format;

    // Find the first audio track
    let track = format_reader
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
        .ok_or_else(|| InferenceError::Other("No audio track found".into()))?;

    let track_id = track.id;
    let codec_params = track.codec_params.clone();
    let source_rate = codec_params.sample_rate.unwrap_or(44100);
    let channels = codec_params.channels.map(|c| c.count()).unwrap_or(1);

    debug!(
        source_rate,
        channels,
        codec = ?codec_params.codec,
        "Decoding audio track"
    );

    let mut decoder = symphonia::default::get_codecs()
        .make(&codec_params, &decoder_opts)
        .map_err(|e| InferenceError::Other(format!("Failed to create decoder: {e}")))?;

    let mut all_samples: Vec<f32> = Vec::new();

    // Decode all packets
    loop {
        let packet = match format_reader.next_packet() {
            Ok(p) => p,
            Err(symphonia::core::errors::Error::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break; // End of stream
            }
            Err(e) => {
                debug!("Packet read error (stopping): {e}");
                break;
            }
        };

        if packet.track_id() != track_id {
            continue;
        }

        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(e) => {
                debug!("Decode error (skipping packet): {e}");
                continue;
            }
        };

        let spec = *decoded.spec();
        let num_frames = decoded.capacity();
        let mut sample_buf = SampleBuffer::<f32>::new(num_frames as u64, spec);
        sample_buf.copy_interleaved_ref(decoded);

        let samples = sample_buf.samples();

        // Mix to mono if stereo/multichannel
        if channels > 1 {
            for chunk in samples.chunks(channels) {
                let mono: f32 = chunk.iter().sum::<f32>() / channels as f32;
                all_samples.push(mono);
            }
        } else {
            all_samples.extend_from_slice(samples);
        }
    }

    if all_samples.is_empty() {
        return Err(InferenceError::Other("No audio samples decoded".into()));
    }

    // Resample to 16kHz if needed (before truncation, so MAX_PCM_SAMPLES is in 16kHz units)
    if source_rate != TARGET_SAMPLE_RATE {
        all_samples = resample(&all_samples, source_rate, TARGET_SAMPLE_RATE);
    }

    // Enforce max duration to prevent OOM (now in 16kHz samples = correct units)
    if all_samples.len() > MAX_PCM_SAMPLES {
        tracing::warn!(
            samples = all_samples.len(),
            max = MAX_PCM_SAMPLES,
            "Audio truncated to 10 minutes"
        );
        all_samples.truncate(MAX_PCM_SAMPLES);
    }

    debug!(
        samples = all_samples.len(),
        duration_secs = all_samples.len() as f32 / TARGET_SAMPLE_RATE as f32,
        "Audio decoded to PCM f32 mono 16kHz"
    );

    Ok(all_samples)
}

/// Simple linear interpolation resampler (no anti-aliasing filter).
/// TODO: Replace with `rubato` crate for production-grade resampling.
/// Linear interpolation causes aliasing on downsample (e.g., 48kHz→16kHz),
/// which may degrade ASR accuracy on high-frequency content.
fn resample(input: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    if from_rate == to_rate || input.is_empty() {
        return input.to_vec();
    }

    let ratio = from_rate as f64 / to_rate as f64;
    let output_len = (input.len() as f64 / ratio).ceil() as usize;
    let mut output = Vec::with_capacity(output_len);

    for i in 0..output_len {
        let src_pos = i as f64 * ratio;
        let idx = src_pos.floor() as usize;
        let frac = (src_pos - idx as f64) as f32;

        let sample = if idx + 1 < input.len() {
            input[idx] * (1.0 - frac) + input[idx + 1] * frac
        } else {
            input[idx.min(input.len() - 1)]
        };
        output.push(sample);
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_identity() {
        let input = vec![0.0, 0.5, 1.0, 0.5, 0.0];
        let output = resample(&input, 16000, 16000);
        assert_eq!(input, output);
    }

    #[test]
    fn resample_downsample() {
        let input: Vec<f32> = (0..48000).map(|i| (i as f32 / 48000.0).sin()).collect();
        let output = resample(&input, 48000, 16000);
        // 48000 samples at 48kHz = 1 second → should produce ~16000 samples at 16kHz
        assert!((output.len() as i64 - 16000).abs() < 2);
    }

    #[test]
    fn decode_empty_fails() {
        assert!(decode_to_pcm(&[]).is_err());
        assert!(decode_to_pcm(&[0, 1, 2]).is_err());
    }
}
