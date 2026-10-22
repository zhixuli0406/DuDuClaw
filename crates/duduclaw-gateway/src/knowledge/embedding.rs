//! Which embedder is in effect, built from `[knowledge.embedding]`.
//!
//! - `local`: the catalog model installed under `<home>/models/embedding/`;
//!   one [`LocalOnnxEmbedder`] per process, reused across calls and
//!   unloaded after the idle window.
//! - `openai` / `gemini`: only when an Admin acknowledged the disclosure for
//!   exactly this provider, host and model, the redaction rules compile, and
//!   an API key resolves (account rotator, optionally narrowed to
//!   `cloud_account`, then the provider's env var).
//! - anything that is not usable resolves to an [`UnavailableEmbedder`]
//!   carrying the zh-TW reason, which search shows as "keyword only" — never
//!   another provider in its place.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use duduclaw_knowledge::embed::cloud::{CloudEmbedder, HttpTransport};
use duduclaw_knowledge::embed::config::{CloudTarget, EmbeddingConfig, ProviderKind};
use duduclaw_knowledge::embed::local_onnx::LocalOnnxEmbedder;
use duduclaw_knowledge::embed::vector_cache::VectorCache;
use duduclaw_knowledge::embed::{
    Embedder, UnavailableEmbedder, catalog, install, is_available, vectors,
};

/// zh-TW reasons.
pub const REASON_DISABLED: &str = "向量檢索已關閉（設定為只用關鍵字）";
pub const REASON_NO_ACK: &str = "雲端向量服務尚未由管理員確認資料揭露，目前只用關鍵字";
/// How long a built cloud embedder (and its compiled redaction rules) is
/// reused. Config changes rebuild at once (the fingerprint has the
/// config.toml hash); the force-disable flag is checked on every text.
const CLOUD_TTL: Duration = Duration::from_secs(60);

type LocalKey = (PathBuf, String, usize, usize);

fn local_slot() -> &'static Mutex<Option<(LocalKey, Arc<LocalOnnxEmbedder>)>> {
    static SLOT: OnceLock<Mutex<Option<(LocalKey, Arc<LocalOnnxEmbedder>)>>> = OnceLock::new();
    SLOT.get_or_init(Default::default)
}

fn cloud_slot() -> &'static Mutex<Option<(String, Instant, Arc<dyn Embedder>)>> {
    static SLOT: OnceLock<Mutex<Option<(String, Instant, Arc<dyn Embedder>)>>> = OnceLock::new();
    SLOT.get_or_init(Default::default)
}

/// The vector cache for `home` (one per home, process-wide).
pub fn cache(home: &Path) -> Arc<VectorCache> {
    static CACHES: OnceLock<Mutex<HashMap<PathBuf, Arc<VectorCache>>>> = OnceLock::new();
    CACHES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .entry(home.to_path_buf())
        .or_default()
        .clone()
}

fn unavailable(reason: impl Into<String>) -> Arc<dyn Embedder> {
    Arc::new(UnavailableEmbedder::new(reason))
}

/// The local embedder for `cfg`, or why there is none. Never loads the
/// model (that happens on first use).
pub fn local(home: &Path, cfg: &EmbeddingConfig) -> Arc<dyn Embedder> {
    let Some(spec) = catalog::find(&cfg.local_model) else {
        return unavailable(format!("設定的本機向量模型「{}」不存在", cfg.local_model));
    };
    if let Some(problem) = install::installed_problem(spec, home) {
        return unavailable(problem);
    }
    let Some(lib) = install::ort_lib(home) else {
        return unavailable("這個平台沒有可用的 ONNX Runtime");
    };
    let key: LocalKey = (
        home.to_path_buf(),
        spec.id.clone(),
        cfg.effective_threads(),
        cfg.batch_size,
    );
    let mut slot = local_slot().lock().unwrap_or_else(|p| p.into_inner());
    if let Some((k, e)) = slot.as_ref()
        && *k == key
    {
        return e.clone();
    }
    let e = Arc::new(LocalOnnxEmbedder::new(spec, home, lib, key.2, key.3));
    *slot = Some((key, e.clone()));
    e
}

/// Drop the local session (model removed or switched).
pub fn drop_local() {
    *local_slot().lock().unwrap_or_else(|p| p.into_inner()) = None;
}

/// Unload a resident local model idle for `minutes` (0 = never).
pub fn unload_idle(minutes: u64) {
    if minutes == 0 {
        return;
    }
    let slot = local_slot().lock().unwrap_or_else(|p| p.into_inner());
    if let Some((_, e)) = slot.as_ref() {
        e.unload_if_idle(Duration::from_secs(minutes * 60));
    }
}

/// Whether the local model is resident right now.
pub fn local_loaded() -> bool {
    local_slot()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .is_some_and(|(_, e)| e.is_loaded())
}

async fn resolve_key(home: &Path, target: &CloudTarget, account: &str) -> Option<String> {
    let provider = target.provider.as_str();
    let pool: Vec<String> = if account.is_empty() {
        Vec::new()
    } else {
        vec![account.to_string()]
    };
    let rotator_key = match crate::claude_runner::get_rotator_cached(home).await {
        Ok(r) => r
            .select_for_provider_with_pool(provider, &pool)
            .await
            .and_then(|a| a.raw_key),
        Err(e) => {
            tracing::warn!(error = %e, "knowledge: account rotator unavailable; trying env key");
            None
        }
    };
    rotator_key
        .filter(|k| !k.trim().is_empty())
        .or_else(|| duduclaw_core::provider_env::resolve_env_key(provider))
}

async fn cloud(home: &Path, cfg: &EmbeddingConfig) -> Arc<dyn Embedder> {
    let target = match cfg.cloud_target() {
        Some(Ok(t)) => t,
        Some(Err(e)) => return unavailable(e),
        None => return unavailable(REASON_DISABLED),
    };
    if !cfg.ack_covers(&target) {
        return unavailable(REASON_NO_ACK);
    }
    let fingerprint = format!(
        "{}|{}|{}|{}|{}",
        home.display(),
        target.provider.as_str(),
        target.base_url,
        target.model,
        duduclaw_knowledge::sha256_hex(
            std::fs::read(home.join("config.toml"))
                .unwrap_or_default()
                .as_slice()
        )
    );
    {
        let slot = cloud_slot().lock().unwrap_or_else(|p| p.into_inner());
        if let Some((f, at, e)) = slot.as_ref()
            && *f == fingerprint
            && at.elapsed() < CLOUD_TTL
        {
            return e.clone();
        }
    }
    let redactor = match super::redaction_adapter::build(home) {
        Ok(r) => super::redaction_adapter::guarded(home, r),
        Err(e) => return unavailable(format!("{e}，文件內容不會送到雲端，目前只用關鍵字")),
    };
    let Some(key) = resolve_key(home, &target, &cfg.cloud_account).await else {
        return unavailable(format!(
            "找不到 {} 的 API 金鑰（請在帳號頁新增，或設定環境變數），目前只用關鍵字",
            target.provider.vendor()
        ));
    };
    let transport = match HttpTransport::new() {
        Ok(t) => Arc::new(t),
        Err(e) => return unavailable(format!("無法建立連線：{e}")),
    };
    let e: Arc<dyn Embedder> = Arc::new(CloudEmbedder::new(
        target.provider,
        &target.model,
        &target.base_url,
        &target.host,
        key,
        redactor,
        transport,
    ));
    *cloud_slot().lock().unwrap_or_else(|p| p.into_inner()) =
        Some((fingerprint, Instant::now(), e.clone()));
    e
}

/// Forget a cached cloud embedder (provider changed).
pub fn drop_cloud() {
    *cloud_slot().lock().unwrap_or_else(|p| p.into_inner()) = None;
}

/// The embedder in effect for `home`.
pub async fn resolve(home: &Path) -> Arc<dyn Embedder> {
    let cfg = EmbeddingConfig::from_home(home);
    resolve_with(home, &cfg).await
}

pub async fn resolve_with(home: &Path, cfg: &EmbeddingConfig) -> Arc<dyn Embedder> {
    match cfg.provider {
        ProviderKind::Local => {
            let (h, c) = (home.to_path_buf(), cfg.clone());
            tokio::task::spawn_blocking(move || local(&h, &c))
                .await
                .unwrap_or_else(|_| unavailable("向量模型狀態讀取失敗"))
        }
        ProviderKind::Cloud(_) => cloud(home, cfg).await,
        ProviderKind::None => match &cfg.unknown_provider {
            Some(v) => unavailable(format!(
                "不支援的向量供應者「{}」，目前只用關鍵字",
                duduclaw_core::truncate_chars(v, 40)
            )),
            None => unavailable(REASON_DISABLED),
        },
    }
}

/// Queue an embed job for `source_id` when an embedder is usable.
pub async fn schedule_source(home: &Path, source_id: &str) {
    let e = resolve(home).await;
    if !is_available(e.as_ref()) {
        return;
    }
    let (h, sid) = (home.to_path_buf(), source_id.to_string());
    let queued = tokio::task::spawn_blocking(move || {
        let mut conn = duduclaw_knowledge::schema::open_home(&h)?;
        vectors::queue_embed(&mut conn, &sid)
    })
    .await;
    match queued {
        Ok(Ok(true)) => super::worker::notify(),
        Ok(Ok(false)) => {}
        Ok(Err(e)) => tracing::warn!(error = %e, "knowledge: could not queue embedding"),
        Err(e) => tracing::warn!(error = %e, "knowledge: embedding schedule task failed"),
    }
}

/// Queue embed jobs for every ready source still missing vectors of the
/// embedder in effect. Returns how many were queued.
pub async fn schedule_all(home: &Path) -> usize {
    let e = resolve(home).await;
    if !is_available(e.as_ref()) {
        return 0;
    }
    let h = home.to_path_buf();
    let model = e.model_id().to_string();
    let cloud = matches!(e.egress(), duduclaw_knowledge::embed::Egress::Cloud { .. });
    let queued = tokio::task::spawn_blocking(move || -> duduclaw_knowledge::KbResult<usize> {
        let mut conn = duduclaw_knowledge::schema::open_home(&h)?;
        let excluded = if cloud {
            vectors::cloud_excluded_collections(&conn)?
        } else {
            Default::default()
        };
        let mut n = 0;
        for sid in vectors::sources_needing(&conn, &model, &excluded)? {
            n += vectors::queue_embed(&mut conn, &sid)? as usize;
        }
        Ok(n)
    })
    .await;
    match queued {
        Ok(Ok(n)) => {
            if n > 0 {
                tracing::info!(sources = n, "knowledge: embedding jobs queued");
                super::worker::notify();
            }
            n
        }
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "knowledge: could not queue embedding backfill");
            0
        }
        Err(_) => 0,
    }
}

/// Log "keyword only" at most once an hour per reason (search calls this).
pub fn warn_keyword_only(reason: &str) {
    static LAST: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    let mut g = LAST
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let due = g
        .get(reason)
        .is_none_or(|t| t.elapsed() >= Duration::from_secs(3600));
    if due {
        tracing::warn!(%reason, "knowledge: vector retrieval unavailable; searching by keywords only");
        g.insert(reason.to_string(), Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn defaults_without_a_model_are_keyword_only_with_a_reason() {
        let home = tempfile::tempdir().unwrap();
        let e = resolve(home.path()).await;
        assert!(!is_available(e.as_ref()));
        let reason = match e.embed_query("x") {
            Err(duduclaw_knowledge::embed::EmbedError::Unavailable(r)) => r,
            other => panic!("{other:?}"),
        };
        assert!(
            reason.contains("尚未安裝") || reason.contains("平台"),
            "{reason}"
        );
    }

    #[tokio::test]
    async fn cloud_without_ack_never_builds_a_sender() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            "[knowledge.embedding]\nprovider = \"openai\"\n[redaction]\nenabled = true\nprofiles = [\"general\"]\n",
        )
        .unwrap();
        let e = resolve(home.path()).await;
        assert!(!is_available(e.as_ref()));
        assert!(
            matches!(e.embed_query("x"), Err(duduclaw_knowledge::embed::EmbedError::Unavailable(r)) if r == REASON_NO_ACK)
        );
    }

    #[tokio::test]
    async fn cloud_with_ack_but_no_redaction_refuses() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            "[knowledge.embedding]\nprovider = \"openai\"\ncloud_disclosure_ack = { by = \"a\", at = \"t\", provider = \"openai\", host = \"api.openai.com\", model = \"text-embedding-3-small\" }\n",
        )
        .unwrap();
        drop_cloud();
        let e = resolve(home.path()).await;
        assert!(!is_available(e.as_ref()));
        assert!(
            matches!(e.embed_query("x"), Err(duduclaw_knowledge::embed::EmbedError::Unavailable(r)) if r.contains("去識別化"))
        );
    }

    #[tokio::test]
    async fn none_and_unknown_providers_say_so() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            "[knowledge.embedding]\nprovider = \"none\"\n",
        )
        .unwrap();
        let e = resolve(home.path()).await;
        assert!(
            matches!(e.embed_query("x"), Err(duduclaw_knowledge::embed::EmbedError::Unavailable(r)) if r == REASON_DISABLED)
        );
        std::fs::write(
            home.path().join("config.toml"),
            "[knowledge.embedding]\nprovider = \"voyage\"\n",
        )
        .unwrap();
        let e = resolve(home.path()).await;
        assert!(
            matches!(e.embed_query("x"), Err(duduclaw_knowledge::embed::EmbedError::Unavailable(r)) if r.contains("voyage"))
        );
    }
}
