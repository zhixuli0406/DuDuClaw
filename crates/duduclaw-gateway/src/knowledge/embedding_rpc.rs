//! `knowledge.embedding.*` — how meaning-based (vector) search is set up.
//!
//! | method | who | what |
//! |---|---|---|
//! | `status` | signed in | provider, availability + reason, coverage, models, install job, cloud disclosure |
//! | `install` `{model}` | Admin | download a catalog model (and ONNX Runtime) in the background |
//! | `cancel` | Admin | stop the download (partial files kept for resume) |
//! | `remove` `{model}` | Admin | delete a model's files |
//! | `verify_offline` `{model}` | Admin | hash files unpacked from an offline bundle, then mark installed |
//! | `select` `{provider, local_model?, cloud_model?, cloud_account?, cloud_base_url?, acknowledge_disclosure?}` | Admin | write `[knowledge.embedding]` |
//! | `rebuild` | Admin | queue vectors for every source still missing them |
//!
//! Choosing `openai` / `gemini` requires `acknowledge_disclosure = true`;
//! the gateway records who acknowledged sending de-identified passages to
//! which provider, host and model, and the cloud embedder is only built
//! while that record matches. Switching away removes the record.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use duduclaw_auth::UserContext;
use duduclaw_knowledge::embed::catalog;
use duduclaw_knowledge::embed::cloud::{CloudProvider, validate_base_url};
use duduclaw_knowledge::embed::config::{EmbeddingConfig, PROVIDER_VALUES, ProviderKind};
use duduclaw_knowledge::embed::install::{self, Progress};
use duduclaw_knowledge::embed::{Egress, EmbedError, Embedder, is_available, vectors};
use serde_json::{Value, json};

#[derive(Default)]
struct JobSlot {
    model: Option<String>,
    running: bool,
    progress: Option<Progress>,
    error: Option<String>,
    cancel: Option<Arc<AtomicBool>>,
    handle: Option<tokio::task::JoinHandle<()>>,
}

fn slot() -> std::sync::MutexGuard<'static, JobSlot> {
    static SLOT: OnceLock<Mutex<JobSlot>> = OnceLock::new();
    SLOT.get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// Whether `method` belongs here.
pub fn is_embedding_method(method: &str) -> bool {
    method.starts_with("knowledge.embedding.")
}

fn admin(ctx: &UserContext) -> Result<(), String> {
    if ctx.is_admin() {
        Ok(())
    } else {
        Err("只有管理員可以變更向量檢索設定。".to_string())
    }
}

fn model_param(p: &Value) -> Result<&'static catalog::ModelSpec, String> {
    let id = p
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    catalog::find(id).ok_or_else(|| "沒有這個本機向量模型。".to_string())
}

/// Answer a `knowledge.embedding.*` method.
pub async fn handle(
    home: &Path,
    method: &str,
    p: &Value,
    ctx: &UserContext,
) -> Result<Value, String> {
    match method {
        "knowledge.embedding.status" => Ok(status(home, ctx.is_admin()).await),
        "knowledge.embedding.install" => {
            admin(ctx)?;
            install_start(home, model_param(p)?, &ctx.user_id)
        }
        "knowledge.embedding.cancel" => {
            admin(ctx)?;
            Ok(cancel())
        }
        "knowledge.embedding.remove" => {
            admin(ctx)?;
            remove(home, model_param(p)?, &ctx.user_id).await
        }
        "knowledge.embedding.verify_offline" => {
            admin(ctx)?;
            verify_offline(home, model_param(p)?, &ctx.user_id).await
        }
        "knowledge.embedding.select" => {
            admin(ctx)?;
            super::embedding_select::select(home, p, &ctx.user_id).await
        }
        "knowledge.embedding.rebuild" => {
            admin(ctx)?;
            let n = super::embedding::schedule_all(home).await;
            Ok(json!({ "queued": n }))
        }
        _ => Err("未知的向量檢索操作。".to_string()),
    }
}

fn reason_of(e: &dyn Embedder) -> Option<String> {
    if is_available(e) {
        return None;
    }
    match e.embed_query("") {
        Err(EmbedError::Unavailable(r) | EmbedError::Failed(r)) => Some(r),
        Ok(_) => None,
    }
}

async fn status(home: &Path, is_admin: bool) -> Value {
    let cfg = EmbeddingConfig::from_home(home);
    let embedder = super::embedding::resolve_with(home, &cfg).await;
    let available = is_available(embedder.as_ref());
    let reason = reason_of(embedder.as_ref());
    let model_id = embedder.model_id().to_string();
    let cloud = matches!(embedder.egress(), Egress::Cloud { .. });
    let (h, m) = (home.to_path_buf(), model_id.clone());
    let counts =
        tokio::task::spawn_blocking(move || -> duduclaw_knowledge::KbResult<(i64, i64, i64)> {
            let Some(conn) = duduclaw_knowledge::schema::open_read_only(&h)? else {
                return Ok((0, 0, 0));
            };
            let excluded = if cloud {
                vectors::cloud_excluded_collections(&conn)?
            } else {
                Default::default()
            };
            let (have, total) = vectors::coverage(&conn, &m, None, &excluded)?;
            Ok((have, total, vectors::vector_rows(&conn, &m)?))
        })
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or((0, 0, 0));
    let coverage = if available && counts.1 > 0 {
        counts.0 as f64 / counts.1 as f64
    } else {
        0.0
    };
    let job = {
        let g = slot();
        json!({
            "model": g.model,
            "state": if g.running { "downloading" } else if g.error.is_some() { "error" } else { "idle" },
            "progress": g.progress,
            // Install errors can name local paths: Admins only.
            "error": if is_admin { g.error.clone() } else { None },
        })
    };
    let models: Vec<Value> = catalog::models()
        .iter()
        .map(|m| {
            let problem = install::installed_problem(m, home);
            json!({
                "id": m.id,
                "display_name": m.display_name,
                "license": m.license,
                "license_source": m.license_source,
                "repo": m.repo,
                "revision": m.revision,
                "dim": m.dim,
                "speed": m.speed,
                "size_bytes": install::total_install_bytes(m, home),
                "model_bytes": m.total_bytes(),
                "installed": problem.is_none(),
                "problem": problem,
                "selected": cfg.local_model == m.id,
            })
        })
        .collect();
    let cloud_info = match cfg.cloud_target() {
        // Others only learn which vendor computes the vectors.
        Some(Ok(t)) if !is_admin => json!({ "vendor": t.provider.vendor() }),
        Some(Err(_)) if !is_admin => Value::Null,
        Some(Ok(t)) => json!({
            "provider": t.provider.as_str(),
            "vendor": t.provider.vendor(),
            "model": t.model,
            "host": t.host,
            "ack_valid": cfg.ack_covers(&t),
        }),
        Some(Err(e)) => json!({ "error": e }),
        None => Value::Null,
    };
    // Compiling the rules is not free (it may load the AI detection model):
    // only for the Admin who can act on the answer.
    let redaction = if !is_admin {
        Value::Null
    } else {
        match super::redaction_adapter::build(home) {
            Ok(_) => json!({ "ready": true }),
            Err(e) => json!({ "ready": false, "reason": e }),
        }
    };
    json!({
        // Fields the knowledge-base page reads.
        "provider": cfg.provider.as_str(),
        "available": available,
        "mode": if available { "hybrid" } else { "keyword_only" },
        "reason": reason,
        "coverage": coverage,
        // Settings card.
        "model_id": if available { Some(model_id) } else { None },
        "egress": if cloud { "cloud" } else { "local" },
        "chunks_with_vectors": counts.0,
        "chunks_total": counts.1,
        "vector_rows": counts.2,
        "max_vector_rows": cfg.max_vector_rows,
        "local_model": cfg.local_model,
        "local_loaded": super::embedding::local_loaded(),
        "platform": duduclaw_onnx::manifest::current_target(),
        "platform_supported": duduclaw_onnx::manifest::ort_archive_for_current_platform().is_some(),
        "models": models,
        "job": job,
        "cloud": cloud_info,
        "cloud_providers": [
            {"id": "openai", "vendor": CloudProvider::OpenAi.vendor(), "default_model": CloudProvider::OpenAi.default_model(), "host": "api.openai.com"},
            {"id": "gemini", "vendor": CloudProvider::Gemini.vendor(), "default_model": CloudProvider::Gemini.default_model(), "host": "generativelanguage.googleapis.com"},
        ],
        "acknowledgement": if is_admin { cfg.ack.as_ref().map(|a| json!(a)) } else { None },
        "redaction": redaction,
        "is_admin": is_admin,
    })
}

fn install_start(
    home: &Path,
    spec: &'static catalog::ModelSpec,
    actor: &str,
) -> Result<Value, String> {
    if duduclaw_onnx::manifest::ort_archive_for_current_platform().is_none() {
        return Err(format!(
            "這個平台（{}）沒有可用的 ONNX Runtime，無法安裝本機向量模型。",
            duduclaw_onnx::manifest::current_target()
        ));
    }
    if install::installed_problem(spec, home).is_none() {
        return Ok(json!({ "started": false, "installed": true }));
    }
    let mut g = slot();
    if g.running {
        return Ok(json!({ "started": false, "installed": false }));
    }
    let cancel = Arc::new(AtomicBool::new(false));
    let progress: install::ProgressFn = Arc::new(|p: Progress| slot().progress = Some(p));
    let home_task: PathBuf = home.to_path_buf();
    let cancel_task = cancel.clone();
    let actor = actor.to_string();
    let handle = tokio::spawn(async move {
        let outcome = install::install(spec, &home_task, progress, cancel_task).await;
        {
            let mut g = slot();
            g.running = false;
            g.handle = None;
            g.cancel = None;
            g.error = outcome.as_ref().err().cloned();
        }
        match outcome {
            Ok(()) => {
                tracing::info!(model = %spec.id, "knowledge: embedding model installed");
                super::audit_admin(
                    &home_task,
                    "knowledge_embedding_model_installed",
                    &actor,
                    json!({"model": spec.id, "revision": spec.revision, "via": "download"}),
                );
                super::embedding::schedule_all(&home_task).await;
            }
            Err(e) => {
                tracing::warn!(model = %spec.id, error = %e, "knowledge: embedding model install failed")
            }
        }
    });
    g.model = Some(spec.id.clone());
    g.running = true;
    g.error = None;
    g.progress = Some(Progress {
        file: "starting".into(),
        done_bytes: 0,
        total_bytes: install::total_install_bytes(spec, home),
    });
    g.cancel = Some(cancel);
    g.handle = Some(handle);
    Ok(json!({ "started": true, "installed": false }))
}

fn cancel() -> Value {
    let mut g = slot();
    if !g.running {
        return json!({ "cancelled": false });
    }
    if let Some(c) = g.cancel.take() {
        c.store(true, Ordering::Relaxed);
    }
    if let Some(h) = g.handle.take() {
        h.abort();
    }
    g.running = false;
    g.error = Some("下載已取消".to_string());
    json!({ "cancelled": true })
}

async fn remove(
    home: &Path,
    spec: &'static catalog::ModelSpec,
    actor: &str,
) -> Result<Value, String> {
    if slot().running {
        return Err("正在下載中，請先取消再移除。".to_string());
    }
    super::embedding::drop_local();
    super::embedding::cache(home).clear();
    let h = home.to_path_buf();
    tokio::task::spawn_blocking(move || install::remove(spec, &h))
        .await
        .map_err(|e| e.to_string())??;
    super::audit_admin(
        home,
        "knowledge_embedding_model_removed",
        actor,
        json!({"model": spec.id}),
    );
    Ok(json!({ "removed": true }))
}

async fn verify_offline(
    home: &Path,
    spec: &'static catalog::ModelSpec,
    actor: &str,
) -> Result<Value, String> {
    if slot().running {
        return Err("正在下載中，請等下載結束再檢查。".to_string());
    }
    let h = home.to_path_buf();
    tokio::task::spawn_blocking(move || install::verify_offline(spec, &h))
        .await
        .map_err(|e| e.to_string())??;
    super::audit_admin(
        home,
        "knowledge_embedding_model_installed",
        actor,
        json!({"model": spec.id, "revision": spec.revision, "via": "offline"}),
    );
    let queued = super::embedding::schedule_all(home).await;
    Ok(json!({ "installed": true, "queued": queued }))
}

/// Accepted `provider` values (re-exported for the select handler).
pub(crate) fn provider_ok(p: &str) -> Option<ProviderKind> {
    PROVIDER_VALUES
        .contains(&p)
        .then(|| ProviderKind::parse(p))
        .flatten()
}

/// Validate an optional cloud base URL.
pub(crate) fn base_url_ok(raw: &str) -> Result<(), String> {
    if raw.is_empty() {
        Ok(())
    } else {
        validate_base_url(raw).map(|_| ())
    }
}
