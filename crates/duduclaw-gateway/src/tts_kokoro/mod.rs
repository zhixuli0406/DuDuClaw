//! Local Kokoro-82M v1.1-zh, with a short-lived ONNX session per request.
use std::path::{Path, PathBuf};
use ort::{session::Session, value::Tensor};
use crate::tts::TtsProvider;
pub mod manifest;
pub mod install;
mod g2p;
mod tone_sandhi;
mod english;
pub use install::is_installed;

// The quantized model can produce non-finite audio for shorter inputs.
const MIN_KOKORO_TOKENS: usize = 12;

pub struct KokoroTtsProvider { home: PathBuf }
impl KokoroTtsProvider {
    pub fn new(home: PathBuf) -> Self { Self { home } }
}
#[async_trait::async_trait]
impl TtsProvider for KokoroTtsProvider {
    fn name(&self) -> &str { "kokoro" }
    async fn synthesize(&self, text: &str, voice: &str) -> Result<Vec<u8>, String> {
        let text = duduclaw_core::truncate_chars(text, 2000).to_string();
        let selected_voice = select_voice(&text, voice);
        if !voice.is_empty() && selected_voice != voice {
            tracing::warn!(configured_voice = %duduclaw_core::truncate_chars(voice, 80), fallback_voice = selected_voice, "Unknown Kokoro voice; using default");
        }
        let voice = selected_voice.to_string();
        let home = self.home.clone();
        tokio::task::spawn_blocking(move || {
            let lib = duduclaw_onnx::manifest::ort_lib_path(&home.join("lib/onnxruntime"))
                .ok_or("這個平台沒有可用的 ONNX Runtime")?;
            synthesize_local(&install::model_dir(&home), &lib, &text, &voice)
        }).await.map_err(|e| format!("Kokoro 工作失敗：{e}"))?
    }
}
fn select_voice<'a>(text: &str, voice: &'a str) -> &'a str {
    if manifest::VOICES.contains(&voice) { return voice; }
    if g2p::is_pure_english(text) { "af_maple" } else { "zf_001" }
}
fn parse_voice(bytes: &[u8]) -> Result<Vec<f32>, String> {
    if bytes.len() != 510 * 256 * 4 { return Err("Kokoro voice 檔案大小不符（預期 522240 bytes）".into()); }
    let values: Vec<f32> = bytes.chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
    if values.iter().any(|v| !v.is_finite()) { return Err("Kokoro voice 含無效數值".into()); }
    Ok(values)
}
fn style_row(values: &[f32], token_count: usize) -> &[f32] {
    let start = token_count.min(509) * 256;
    &values[start..start + 256]
}
fn pad_short_tokens(mut tokens: Vec<i64>, pause_id: i64) -> Vec<i64> {
    if tokens.len() < MIN_KOKORO_TOKENS {
        tokens.resize(MIN_KOKORO_TOKENS, pause_id);
    }
    tokens
}
fn synthesize_local(dir: &Path, lib: &Path, text: &str, voice: &str) -> Result<Vec<u8>, String> {
    let chunks = g2p::chunks(text, dir)?;
    if chunks.is_empty() { return Err("沒有可合成的文字".into()); }
    duduclaw_onnx::runtime::init_ort(lib)?;
    let style = parse_voice(&std::fs::read(dir.join(format!("voices/{voice}.bin"))).map_err(|e| e.to_string())?)?;
    let mut session = Session::builder().map_err(|e| e.to_string())?
        .with_intra_threads(4).map_err(|e| e.to_string())?
        .commit_from_file(dir.join("onnx/model_uint8.onnx")).map_err(|e| format!("Kokoro session：{e}"))?;
    let names: Vec<_> = session.inputs().iter().map(|i| i.name().to_string()).collect();
    if names.first().map(String::as_str) != Some("input_ids")
        || names.len() != 3 || !names.iter().any(|n| n == "style") || !names.iter().any(|n| n == "speed") {
        return Err(format!("Kokoro 模型輸入不符，預期 input_ids、style、speed，實際 {names:?}"));
    }
    let mut pcm = Vec::new();
    for (index, chunk) in chunks.into_iter().enumerate() {
        if chunk.len() > 500 { return Err("Kokoro 文字分段超出 token 限制".into()); }
        if index > 0 { pcm.resize(pcm.len() + 3600 * 2, 0); }
        let pause_id = *g2p::vocabulary().get(&'…')
            .ok_or("Kokoro 詞彙表缺少省略號停頓 token")?;
        let chunk = pad_short_tokens(chunk, pause_id);
        let count = chunk.len();
        let ids: Vec<i64> = std::iter::once(0).chain(chunk).chain(std::iter::once(0)).collect();
        let ids = Tensor::from_array((vec![1i64, ids.len() as i64], ids)).map_err(|e| e.to_string())?;
        let style = Tensor::from_array((vec![1i64, 256], style_row(&style, count).to_vec())).map_err(|e| e.to_string())?;
        let speed = Tensor::from_array((vec![1i64], vec![1.0f32])).map_err(|e| e.to_string())?;
        let outputs = session.run(ort::inputs!["input_ids" => ids, "style" => style, "speed" => speed])
            .map_err(|e| format!("Kokoro 推論失敗：{e}"))?;
        let (_, samples) = outputs[0].try_extract_tensor::<f32>().map_err(|e| e.to_string())?;
        for &sample in samples {
            if !sample.is_finite() { return Err("Kokoro 回傳無效音訊".into()); }
            pcm.extend_from_slice(&((sample.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
        }
    }
    Ok(crate::tts::pcm_i16_to_wav(&pcm, 24000))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn voice_selection_defaults_by_language_and_preserves_valid_voices() {
        for voice in ["", "zh-TW-HsiaoChenNeural", "unknown"] {
            assert_eq!(select_voice("Hello world", voice), "af_maple");
            assert_eq!(select_voice("你好世界", voice), "zf_001");
            assert_eq!(select_voice("Hello 你好", voice), "zf_001");
        }
        for voice in manifest::VOICES {
            assert_eq!(select_voice("Hello world", voice), *voice);
            assert_eq!(select_voice("你好世界", voice), *voice);
        }
    }
    #[test]
    fn short_tokens_are_padded_to_minimum() {
        assert_eq!(pad_short_tokens(vec![1, 2], 99), [1, 2, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99]);
    }
    #[test]
    fn long_tokens_are_unchanged() {
        let tokens: Vec<i64> = (0..MIN_KOKORO_TOKENS + 1).map(|n| n as i64).collect();
        assert_eq!(pad_short_tokens(tokens.clone(), 99), tokens);
    }
    #[test]
    fn empty_tokens_are_padded() {
        assert_eq!(pad_short_tokens(Vec::new(), 99), vec![99; MIN_KOKORO_TOKENS]);
    }
    #[test]
    fn voice_shape_and_style_clamp() {
        assert!(parse_voice(&[0; 4]).is_err());
        let mut v = vec![0.0; 510 * 256];
        v[509 * 256] = 0.75;
        assert_eq!(style_row(&v, 10000)[0], 0.75);
        assert_eq!(parse_voice(&vec![0; 522240]).unwrap().len(), 510 * 256);
    }
    #[test]
    #[ignore = "Requires downloaded pinned model and ONNX Runtime"]
    fn kokoro_real_model_generates_audio() {
        let dir = PathBuf::from(std::env::var("KOKORO_TEST_MODEL_DIR").expect("KOKORO_TEST_MODEL_DIR"));
        let lib = PathBuf::from(std::env::var("ORT_DYLIB_PATH").expect("ORT_DYLIB_PATH"));
        let wav = synthesize_local(&dir, &lib, "你好，我是你的 AI 員工。今天天氣真好。", "zf_001").unwrap();
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert!((wav.len() - 44) as f64 / 2.0 / 24000.0 > 1.0);
        let peak = wav[44..].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]).unsigned_abs() as f32 / 32768.0).fold(0.0f32, f32::max);
        assert!(peak > 0.01);
        if let Ok(out) = std::env::var("KOKORO_TEST_OUT") { std::fs::write(out, wav).unwrap(); }
    }
}
