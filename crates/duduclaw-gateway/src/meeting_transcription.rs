//! Shared gateway-owned transcription orchestration and per-attempt accounting.
use std::{path::Path, sync::Arc};
use duduclaw_inference::transcription::{
    config::{resolve_gemini_model, EngineKind, TranscriptionConfig},
    pipeline, GeminiEngine, TranscriptionEngine, TranscriptionUsage, WhisperEngine,
};
use serde_json::{json, Value};

/// The worker persists progress and may fence a cancelled job before each call.
/// `(0, 0)` is a fence only (before preparation and before saving outputs).
#[async_trait::async_trait]
pub trait JobObserver: Send + Sync {
    fn source_binding(&self) -> Option<(&str,u64)> { None }
    async fn before_chunk(&self, index: usize, total: usize) -> std::result::Result<(), String>;
    async fn cost(&self, total_millicents: u64, estimated: bool) -> std::result::Result<(), String>;
}

pub struct RedactionContext {
    pub manager: Arc<duduclaw_redaction::RedactionManager>,
    pub agent_id: String,
    pub session_id: String,
}

async fn provider_key(home: &Path, provider: &str) -> std::result::Result<String, String> {
    // The account rotator owns provider selection and the credential-inheritance gate.
    let rotator = crate::claude_runner::get_rotator_cached(home).await
        .map_err(|_| "無法讀取轉錄服務的憑證設定。")?;
    rotator.select_for_provider(provider).await.and_then(|env| env.raw_key)
        .filter(|key| !key.trim().is_empty())
        .ok_or_else(|| format!("尚未設定 {provider} 的 API 憑證。"))
}

pub async fn execute(
    home: &Path, agent: &str, source: &Path, output: &Path,
    cfg: &TranscriptionConfig, kind: EngineKind, language: &str,
    control: Option<&dyn JobObserver>, output_id: Option<&str>, session: Option<&str>,
    staging_root: Option<&Path>,
) -> std::result::Result<Value, String> {
    if let Some(control) = control { control.before_chunk(0, 0).await?; }
    pipeline::preflight_output(output)?;
    let mut registry = duduclaw_llm::ModelRegistry::vendored();
    registry.load_override(&home.join("models.toml"))?;
    let model_name=match kind { EngineKind::Gemini=>resolve_gemini_model(cfg,&registry)?, EngineKind::Whisper=>"openai/whisper-1".into(), EngineKind::GoogleStt=>"google-stt".into() };
    let engine: Box<dyn TranscriptionEngine> = match kind {
        EngineKind::Gemini => Box::new(GeminiEngine::new(provider_key(home, "gemini").await?, model_name.clone()).map_err(|e| e.to_string())?),
        EngineKind::Whisper => Box::new(WhisperEngine::new(provider_key(home, "openai").await?).map_err(|e| e.to_string())?),
        EngineKind::GoogleStt => return Err("Google Cloud 語音轉錄尚未支援長會議；請由管理員選擇 Gemini 或 Whisper。".into()),
    };
    let redaction = init_redaction(home, agent, session)?;
    let telemetry = crate::cost_telemetry::CostTelemetry::new(&home.join("cost_telemetry.db"))?;
    let prepared = pipeline::prepare_audio_bound_with_memory_limit(source, cfg.max_bytes, staging_root,control.and_then(|c|c.source_binding()),cfg.media_memory_limit_bytes).await?;
    let observer = MeetingUsageObserver {
        telemetry: &telemetry, registry: &registry, home, agent, control,
        provider: kind.as_str(), model: &model_name, record_id:output_id.unwrap_or("synchronous"),
        totals: tokio::sync::Mutex::new((0, 0)),
    };
    let transcription = pipeline::transcribe_chunks(engine.as_ref(), &prepared, language, &observer).await;
    if transcription.is_err() { crate::meeting_jobs::audit(home,"meeting_provider_result",agent,agent,output_id.unwrap_or("synchronous"),"failed",Some(kind.as_str()),Some(&model_name)); }
    let mut transcript = transcription?;
    pipeline::convert_taiwan(&mut transcript)?;
    let source_name = source.file_name().and_then(|s| s.to_str()).unwrap_or("會議錄音");
    let (source_name, transcript) = redact_for_storage(redaction.as_ref(), agent, source_name, transcript)?;
    if let Some(control) = control { control.before_chunk(0, 0).await?; }
    let paths = pipeline::save_outputs_with_id(output, &source_name, kind.as_str(), prepared.duration_ms, &transcript, output_id)?;
    let speaker_count = transcript.segments.iter().filter_map(|s| s.speaker.as_deref()).collect::<std::collections::BTreeSet<_>>().len();
    let cost_total_millicents = observer.totals.lock().await.0;
    let preview = transcript.segments.iter().take(3).map(|segment| segment.text.as_str()).collect::<Vec<_>>().join(" ");
    Ok(json!({
        "markdown_path": paths.markdown, "json_path": paths.json, "output_bytes":paths.bytes,
        "segment_count":transcript.segments.len(),
        "preview": duduclaw_core::truncate_chars(&preview, 240),
        "engine": kind.as_str(), "language": transcript.language,
        "duration_ms": prepared.duration_ms, "speaker_count": speaker_count,
        "cost_total_millicents": cost_total_millicents,
        "notice": "音訊已傳送至選定的轉錄服務；跨段的講者標籤可能不一致。"
    }))
}

pub fn init_redaction(home: &Path, agent: &str, session: Option<&str>) -> std::result::Result<Option<RedactionContext>, String> {
    #[derive(serde::Deserialize, Default)]
    struct Global { #[serde(default)] redaction: duduclaw_redaction::RedactionConfig }
    let raw = match std::fs::read_to_string(home.join("config.toml")) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(_) => return Err("無法讀取隱私遮罩設定，已停止轉錄。".into()),
    };
    let mut config = toml::from_str::<Global>(&raw).map_err(|_| "隱私遮罩設定格式錯誤，已停止轉錄。")?.redaction;
    let agent_dir = crate::ephemeral::resolve_agent_dir(home, agent).unwrap_or_else(|| home.join("agents").join(agent));
    let agent_enabled = match std::fs::read_to_string(agent_dir.join("agent.toml")) {
        Ok(raw) => {
            #[derive(serde::Deserialize, Default)]
            struct Toggle { #[serde(default)] enabled: bool }
            #[derive(serde::Deserialize)]
            struct Employee { #[serde(default)] redaction: Toggle }
            toml::from_str::<Employee>(&raw).map_err(|_| "員工隱私遮罩設定格式錯誤，已停止轉錄。")?.redaction.enabled
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && agent == "meeting-operator" => false,
        Err(_) => return Err("無法讀取員工隱私遮罩設定，已停止轉錄。".into()),
    };
    // The employee toggle may require masking, but cannot turn off the global policy.
    config.enabled |= agent_enabled;
    if !config.enabled { return Ok(None); }
    if agent_enabled && config.profiles.is_empty() && config.rules.is_empty() {
        return Err("這位員工已啟用隱私遮罩，但尚未設定遮罩規則；請管理員完成設定後重試。".into());
    }
    let manager = duduclaw_redaction::RedactionManager::open(config, duduclaw_redaction::ManagerPaths::under_home(home))
        .map_err(|_| "隱私遮罩無法初始化，已停止轉錄。")?;
    Ok(Some(RedactionContext {
        manager: Arc::new(manager), agent_id: agent.into(),
        session_id: session.unwrap_or("meeting-transcription").into(),
    }))
}

fn redact_for_storage(
    layer: Option<&RedactionContext>, agent: &str,
    source_name: &str, transcript: duduclaw_inference::transcription::Transcript,
) -> std::result::Result<(String, duduclaw_inference::transcription::Transcript), String> {
    let mut saved = json!({"source": source_name, "transcript": transcript});
    if let Some(layer) = layer {
        let pipeline = layer.manager.pipeline(agent, Some(layer.session_id.clone()))
            .map_err(|_| "隱私遮罩無法初始化，未儲存逐字稿。")?;
        pipeline.redact_value(&mut saved, &duduclaw_redaction::ToolContext { tool_name: "meeting_transcribe", args: None })
            .map_err(|_| "隱私遮罩處理失敗，未儲存逐字稿。")?;
    }
    let source_name = saved["source"].as_str().ok_or("無法處理錄音名稱。")?.to_owned();
    let transcript = serde_json::from_value(saved["transcript"].clone()).map_err(|_| "無法處理遮罩後的逐字稿。")?;
    Ok((source_name, transcript))
}

fn usage_millicents(registry: &duduclaw_llm::ModelRegistry, item: &TranscriptionUsage) -> std::result::Result<u64, String> {
    if item.model == "openai/whisper-1" {
        // transcribe_chunks replaces provider duration with the locally probed
        // chunk length before delivering usage to this accounting observer.
        let seconds = item.audio_seconds.filter(|s| s.is_finite() && *s >= 0.0)
            .ok_or("轉錄服務未回報有效錄音長度，無法計算費用。")?;
        // Whisper's published $0.006/minute = 10 true millicents/second.
        // Round to the ledger's cents only after accumulating this job's calls.
        return Ok((seconds * 10.0).round() as u64);
    }
    let model = registry.get(&item.model).ok_or("轉錄模型缺少費率資料，無法記錄費用。")?;
    Ok(registry.cost_audio_millicents(&duduclaw_llm::NormalizedUsage {
        input_tokens: item.input_tokens,
        output_tokens: item.output_tokens,
        reasoning_tokens: item.reasoning_tokens,
        ..Default::default()
    }, item.input_audio_tokens, model))
}

struct MeetingUsageObserver<'a> {
    telemetry: &'a crate::cost_telemetry::CostTelemetry,
    registry: &'a duduclaw_llm::ModelRegistry,
    home: &'a Path,
    agent: &'a str,
    provider: &'a str,
    model: &'a str,
    record_id: &'a str,
    control: Option<&'a dyn JobObserver>,
    // True millicents accumulated, and cents already persisted for this job.
    totals: tokio::sync::Mutex<(u64, u64)>,
}

#[async_trait::async_trait]
impl pipeline::UsageObserver for MeetingUsageObserver<'_> {
    async fn before_chunk(&self, index: usize, total: usize) -> std::result::Result<(), String> {
        if let Some(control) = self.control {
            if let Err(error)=control.before_chunk(index, total).await {
                crate::meeting_jobs::audit(self.home,"meeting_provider_dispatch",self.agent,self.agent,self.record_id,"gate_refused",Some(self.provider),Some(self.model));
                return Err(error);
            }
        }
        crate::meeting_jobs::audit(self.home,"meeting_provider_dispatch",self.agent,self.agent,self.record_id,"allowed",Some(self.provider),Some(self.model));
        Ok(())
    }
    async fn record(&self, item: &TranscriptionUsage) -> std::result::Result<(), String> {
        let unmeasured = item.input_tokens == 0 && item.output_tokens == 0
            && item.reasoning_tokens == 0 && item.audio_seconds.is_none();
        // HTTP failures and missing provider usage are recorded explicitly as
        // unmeasured, never priced as free or mapped onto a text-model fallback.
        let price = usage_millicents(self.registry, item);
        let estimated = item.estimated || unmeasured || price.is_err();
        let mc = price.unwrap_or(0);
        crate::meeting_jobs::audit(self.home,"meeting_provider_result",self.agent,self.agent,self.record_id,if estimated {"estimated"}else{"measured"},Some(self.provider),Some(&item.model));
        let mut totals = self.totals.lock().await;
        let cumulative = totals.0.saturating_add(mc);
        let cents = cumulative.saturating_add(500) / 1000;
        record_usage(self.telemetry, self.agent, item, cents.saturating_sub(totals.1), estimated).await?;
        *totals = (cumulative, cents);
        if let Some(control) = self.control { control.cost(cumulative, estimated).await?; }
        Ok(())
    }
}

async fn record_usage(
    telemetry: &crate::cost_telemetry::CostTelemetry,
    agent: &str, item: &TranscriptionUsage, cost_cents: u64, usage_estimated: bool,
) -> std::result::Result<(), String> {
    use crate::cost_telemetry::{RequestType, TokenUsage};
    let task = std::env::var(duduclaw_core::ENV_TASK_ID).ok();
    telemetry.record_priced(agent, if task.is_some() { RequestType::Dispatch } else { RequestType::Chat }, &item.model, &TokenUsage {
        input_tokens: item.input_tokens,
        output_tokens: item.output_tokens.saturating_add(item.reasoning_tokens),
        ..Default::default()
    }, cost_cents, usage_estimated, task.as_deref()).await
        .map_err(|_| "轉錄費用紀錄失敗，已停止後續處理。".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcription_whisper_cost_uses_duration_and_rejects_unknown_duration() {
        let registry = duduclaw_llm::ModelRegistry::vendored();
        let mut item = TranscriptionUsage { model: "openai/whisper-1".into(), audio_seconds: Some(1800.0), ..Default::default() };
        assert_eq!(usage_millicents(&registry, &item).unwrap(), 18000);
        item.audio_seconds = None;
        assert!(usage_millicents(&registry, &item).is_err());
        item.audio_seconds = Some(f64::NAN);
        assert!(usage_millicents(&registry, &item).is_err());
        item.model = "unknown/model".into();
        assert!(usage_millicents(&registry, &item).is_err());
    }

    #[test]
    fn transcription_redacts_both_saved_artifacts_and_source_name() {
        use duduclaw_inference::transcription::{Segment, Transcript};
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("agents/alice")).unwrap();
        std::fs::write(home.path().join("agents/alice/agent.toml"), "[redaction]\nenabled = true").unwrap();
        std::fs::write(home.path().join("config.toml"), "[redaction]\nenabled = false\nprofiles = ['general']").unwrap();
        let layer = init_redaction(home.path(), "alice", None).unwrap().unwrap();
        let transcript = Transcript { language: "zh-TW".into(), segments: vec![Segment {
            start_ms: 0, end_ms: 1000, speaker: Some("講者 1".into()), text: "請聯絡 alice@example.com".into(),
        }] };
        let (source, transcript) = redact_for_storage(Some(&layer), "alice", "alice@example.com.mp3", transcript).unwrap();
        let paths = pipeline::save_outputs(&home.path().canonicalize().unwrap(), &source, "gemini", 1000, &transcript).unwrap();
        for path in [paths.markdown, paths.json] {
            let saved = std::fs::read_to_string(path).unwrap();
            assert!(!saved.contains("alice@example.com"));
            assert!(saved.contains("REDACT:"));
        }
    }

    #[test]
    fn transcription_agent_redaction_missing_rules_and_malformed_settings_fail_closed() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("agents/alice")).unwrap();
        std::fs::write(home.path().join("agents/alice/agent.toml"), "[redaction]\nenabled = true").unwrap();
        assert!(init_redaction(home.path(), "alice", None).is_err());
        std::fs::write(home.path().join("config.toml"), "[redaction]\nenabled = 'yes'").unwrap();
        assert!(init_redaction(home.path(), "alice", None).is_err());
        std::fs::write(home.path().join("config.toml"), "[redaction]\nenabled = false").unwrap();
        std::fs::write(home.path().join("agents/alice/agent.toml"), "[redaction]\nenabled = 'yes'").unwrap();
        assert!(init_redaction(home.path(), "alice", None).is_err());
    }

    #[tokio::test]
    async fn transcription_usage_persists_per_chunk_and_marks_unmeasured_attempts() {
        use pipeline::UsageObserver;
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("cost_telemetry.db");
        let telemetry = crate::cost_telemetry::CostTelemetry::new(&path).unwrap();
        let registry = duduclaw_llm::ModelRegistry::vendored();
        let observer = MeetingUsageObserver { telemetry: &telemetry, registry: &registry, home: home.path(), provider: "whisper", model: "openai/whisper-1", record_id:"test-job", agent: "alice", control: None, totals: tokio::sync::Mutex::new((0, 0)) };
        let item = TranscriptionUsage { model: "openai/whisper-1".into(), audio_seconds: Some(30.0), ..Default::default() };
        observer.record(&item).await.unwrap();
        let db = rusqlite::Connection::open(&path).unwrap();
        assert_eq!(db.query_row("SELECT COUNT(*) FROM token_usage", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        observer.record(&item).await.unwrap();
        observer.record(&TranscriptionUsage { model: "openai/whisper-1".into(), ..Default::default() }).await.unwrap();
        assert_eq!(db.query_row("SELECT SUM(cost_millicents) FROM token_usage", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        assert_eq!(db.query_row("SELECT COUNT(*) FROM token_usage WHERE usage_estimated = 1 AND input_tokens = 0 AND output_tokens = 0", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    }

}
