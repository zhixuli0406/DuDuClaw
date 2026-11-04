//! Operator-owned meeting transcription policy. Invalid settings fail closed.
use std::path::Path;
use serde::{Deserialize, Serialize};
use duduclaw_llm::ModelRegistry;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EngineKind {
    #[default]
    Gemini,
    GoogleStt,
    Whisper,
}

impl EngineKind {
    pub fn as_str(self) -> &'static str {
        match self { Self::Gemini => "gemini", Self::GoogleStt => "google_stt", Self::Whisper => "whisper" }
    }
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "gemini" => Ok(Self::Gemini), "google_stt" => Ok(Self::GoogleStt),
            "whisper" => Ok(Self::Whisper), _ => Err("不支援此轉錄引擎".into()),
        }
    }
}

impl std::str::FromStr for EngineKind {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> { Self::parse(value) }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TranscriptionConfig {
    pub engine: EngineKind,
    pub model: Option<String>,
    pub max_bytes: u64,
    pub staging_quota_bytes: u64,
    pub max_concurrent_jobs: u8,
    pub media_memory_limit_bytes: u64,
}

impl Default for TranscriptionConfig {
    fn default() -> Self {
        Self { engine: EngineKind::Gemini, model: None, max_bytes: 4 * 1024 * 1024 * 1024, staging_quota_bytes: 10 * 1024 * 1024 * 1024, max_concurrent_jobs: 1, media_memory_limit_bytes: super::pipeline::DEFAULT_MEDIA_MEMORY_LIMIT_BYTES }
    }
}

impl TranscriptionConfig {
    pub fn load(home: &Path) -> Result<Self, String> {
        let raw = match std::fs::read_to_string(home.join("config.toml")) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(_) => return Err("無法讀取會議轉錄設定".into()),
        };
        Self::from_toml(&raw)
    }
    pub fn from_toml(raw: &str) -> Result<Self, String> {
        #[derive(Deserialize)]
        struct Wrapper { #[serde(default)] transcription: TranscriptionConfig }
        let config = toml::from_str::<Wrapper>(raw)
            .map_err(|_| "會議轉錄設定格式錯誤".to_string())?.transcription;
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.max_bytes == 0 || self.max_bytes > 4 * 1024 * 1024 * 1024 {
            return Err("會議附件大小上限須介於 1 位元組與 4 GB".into());
        }
        if !(1..=2).contains(&self.max_concurrent_jobs) || self.staging_quota_bytes == 0 || self.staging_quota_bytes > i64::MAX as u64 {
            return Err("會議工作並行數須為 1 或 2，暫存配額須為有效正整數".into());
        }
        if self.media_memory_limit_bytes == 0 || self.media_memory_limit_bytes > i64::MAX as u64 {
            return Err("音訊處理記憶體上限須為有效正整數".into());
        }
        if self.model.as_ref().is_some_and(|m| m.is_empty() || m.len() > 200
            || !m.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))) {
            return Err("會議轉錄模型名稱格式錯誤".into());
        }
        Ok(())
    }
    /// Employees cannot switch the destination provider selected by an operator.
    pub fn select_engine(&self, requested: Option<&str>) -> Result<EngineKind, String> {
        let selected = requested.map(EngineKind::parse).transpose()?.unwrap_or(self.engine);
        if selected != self.engine {
            return Err("請由管理員變更會議轉錄引擎；本次要求與核准的引擎不同".into());
        }
        Ok(selected)
    }
}

pub fn resolve_gemini_model(config: &TranscriptionConfig, registry: &ModelRegistry) -> Result<String, String> {
    let model = if let Some(id) = &config.model {
        registry.get(id)
    } else {
        registry.models().find(|m| m.provider == "gemini" && m.caps.audio && m.caps.structured)
    }.ok_or_else(|| "請設定登錄表中可處理音訊的 Gemini 模型".to_string())?;
    if model.provider != "gemini" || !model.caps.audio || !model.caps.structured {
        return Err("設定的模型不支援 Gemini 音訊與結構化轉錄".into());
    }
    Ok(model.qualified_id())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transcription_selection_fails_closed() {
        let cfg = TranscriptionConfig::from_toml("[transcription]\nengine='whisper'\nmax_bytes=42").unwrap();
        assert_eq!(cfg.select_engine(None).unwrap(), EngineKind::Whisper);
        assert!(cfg.select_engine(Some("gemini")).is_err());
        for raw in ["[transcription]\nengine='typo'", "[transcription]\nmax_bytes=0", "[transcription]\nmax_bytes=4294967297", "[transcription]\nmodle='x'", "[transcription]\nmodel=''", "not toml"] {
            assert!(TranscriptionConfig::from_toml(raw).is_err(), "{raw}");
        }
        assert_eq!(TranscriptionConfig::from_toml("").unwrap().engine, EngineKind::Gemini);
    }
    #[test]
    fn transcription_model_comes_from_registry() {
        let registry = ModelRegistry::vendored();
        let cfg = TranscriptionConfig::default();
        let id = resolve_gemini_model(&cfg, &registry).unwrap();
        assert!(registry.get(&id).unwrap().caps.audio);
        let cfg = TranscriptionConfig { model: Some("unknown".into()), ..cfg };
        assert!(resolve_gemini_model(&cfg, &registry).is_err());
        assert!(resolve_gemini_model(&TranscriptionConfig::default(), &ModelRegistry::empty()).is_err());
    }
}


#[cfg(test)]
mod memory_limit_tests {
    use super::*;
    #[test]
    fn transcription_media_memory_limit_defaults_and_rejects_invalid_values() {
        assert_eq!(TranscriptionConfig::default().media_memory_limit_bytes, 2 * 1024 * 1024 * 1024);
        assert_eq!(TranscriptionConfig::from_toml("[transcription]\nmedia_memory_limit_bytes=1048576").unwrap().media_memory_limit_bytes, 1048576);
        for value in ["0", "-1", "'unbounded'"] {
            assert!(TranscriptionConfig::from_toml(&format!("[transcription]\nmedia_memory_limit_bytes={value}")).is_err());
        }
    }
}
