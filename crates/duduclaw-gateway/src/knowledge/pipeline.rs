//! One ingest job end to end: fetch (URL sources) → pre-parse gate →
//! child-process extraction → chunking → one indexing transaction.
//!
//! The same [`extract_file`] is used by `duduclaw knowledge eval`, so an
//! evaluation measures exactly what production indexes.

use std::path::{Path, PathBuf};
use std::time::Duration;

use duduclaw_knowledge::blob::BlobStore;
use duduclaw_knowledge::extract::{ExtractError, ExtractOutput, Format};
use duduclaw_knowledge::jobs::{self, Job};
use duduclaw_knowledge::limits::KnowledgeConfig;
use duduclaw_knowledge::sources;

use super::extract_spawn::{ExtractorCmd, SpawnLimits};

/// What one ingest needs.
#[derive(Debug, Clone)]
pub struct PipelineCtx {
    pub home: PathBuf,
    pub cfg: KnowledgeConfig,
    pub extractor: ExtractorCmd,
}

/// Is `text` instruction-like (prompt injection)? Index-time flag only.
pub fn is_suspect(text: &str) -> bool {
    duduclaw_security::input_guard::scan_input(text, duduclaw_security::input_guard::DEFAULT_BLOCK_THRESHOLD).blocked
}

/// Gate and extract a file on disk. `display_name` only appears in messages.
pub async fn extract_file(
    home: &Path,
    cfg: &KnowledgeConfig,
    extractor: &ExtractorCmd,
    format: Format,
    path: &Path,
    display_name: &str,
) -> Result<(ExtractOutput, bool), ExtractError> {
    let size = tokio::fs::metadata(path)
        .await
        .map(|m| m.len())
        .map_err(|_| ExtractError::new("extract_crashed", "找不到這份文件的原始檔，請刪除後重新上傳。"))?;
    if size > cfg.max_source_bytes {
        return Err(ExtractError::new(
            "too_large",
            &format!("檔案超過 {} MB 上限。", cfg.max_source_bytes / (1024 * 1024)),
        ));
    }
    if format.is_ooxml() {
        let limits = crate::document_limits::DocumentLimits::from_home(home);
        let p = path.to_path_buf();
        let guard = tokio::task::spawn_blocking(move || crate::document_limits::guard_document_path(&p, &limits))
            .await
            .map_err(|_| ExtractError::new("extract_crashed", "檢查文件時發生錯誤。"))?;
        if let Err(v) = guard {
            return Err(ExtractError {
                code: "document_limits".into(),
                message: v.user_message(display_name),
            });
        }
    }
    let limits = SpawnLimits {
        timeout: Duration::from_secs(cfg.extract_timeout_secs),
        memory_mb: cfg.extract_memory_mb,
        max_chars: cfg.max_source_chars,
    };
    let spawned = super::extract_spawn::run(extractor, format, path, limits).await?;
    Ok((spawned.output, spawned.sandboxed))
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> duduclaw_knowledge::KbResult<T> + Send + 'static,
) -> duduclaw_knowledge::KbResult<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| duduclaw_knowledge::KbError::Io(std::io::Error::other(e.to_string())))?
}

fn fail(home: &Path, job: &Job, code: &str, message: &str) {
    let result = duduclaw_knowledge::schema::open_home(home).and_then(|conn| {
        sources::record_failure(&conn, &job.source_id, code, message)?;
        jobs::fail(&conn, job.id, code, message)
    });
    if let Err(e) = result {
        tracing::error!(error = %e, source = %job.source_id, "knowledge: could not record ingest failure");
    }
}

/// Run one claimed job. Never panics; every outcome is written back.
pub async fn process_job(ctx: &PipelineCtx, job: Job) {
    let home = ctx.home.clone();
    let source_id = job.source_id.clone();
    let loaded = blocking(move || {
        let conn = duduclaw_knowledge::schema::open_home(&home)?;
        let src = sources::get_source(&conn, &source_id)?;
        if let Some(s) = &src {
            if s.status != "deleting" {
                sources::set_status(&conn, &s.id, "extracting")?;
            }
        }
        Ok(src)
    })
    .await;
    let src = match loaded {
        Ok(Some(s)) if s.status != "deleting" => s,
        Ok(_) => return,
        Err(e) => {
            tracing::error!(error = %e, "knowledge: could not load source for ingest");
            return;
        }
    };
    let Some(format) = Format::parse(&src.format) else {
        fail(&ctx.home, &job, "unsupported_format", "不支援這種檔案格式。");
        return;
    };
    let store = BlobStore::for_home(&ctx.home);
    if src.kind == "url" {
        let url = src.url.clone().unwrap_or_default();
        match super::url_ingest::fetch(&url).await {
            Ok(body) => {
                if let Err(e) = store.put_named(&src.blob_sha256, &body) {
                    tracing::error!(error = %e, "knowledge: could not store fetched page");
                    fail(
                        &ctx.home,
                        &job,
                        "url_fetch_failed",
                        "網頁已取得但無法存檔，請稍後再試。",
                    );
                    return;
                }
            }
            Err(e) => {
                fail(&ctx.home, &job, &e.code, &e.message);
                return;
            }
        }
    }
    let path = match store.path_for(&src.blob_sha256) {
        Ok(p) => p,
        Err(_) => {
            fail(&ctx.home, &job, "extract_crashed", "原始檔識別碼不正確。");
            return;
        }
    };
    let (output, sandboxed) = match extract_file(&ctx.home, &ctx.cfg, &ctx.extractor, format, &path, &src.title).await {
        Ok(v) => v,
        Err(e) => {
            fail(&ctx.home, &job, &e.code, &e.message);
            return;
        }
    };
    let home = ctx.home.clone();
    let cfg = ctx.cfg.clone();
    let job_id = job.id;
    let sid = src.id.clone();
    let indexed = blocking(move || {
        let mut conn = duduclaw_knowledge::schema::open_home(&home)?;
        // A delete while extracting cancels the job: nothing is written.
        if !jobs::is_running(&conn, job_id)? {
            return Ok(None);
        }
        sources::set_status(&conn, &sid, "indexing")?;
        let mut warnings = output.warnings.clone();
        if !sandboxed {
            warnings.push(duduclaw_knowledge::extract::Warning {
                code: "extract_unsandboxed".into(),
                pages: Vec::new(),
                count: None,
            });
        }
        let prepared = duduclaw_knowledge::chunk::prepare(format, output.segments);
        let outcome = duduclaw_knowledge::index::write_index(
            &mut conn,
            &sid,
            &prepared,
            &warnings,
            &output.extractor_version,
            &is_suspect,
            &cfg,
        )?;
        jobs::complete(&conn, job_id)?;
        Ok(Some(outcome))
    })
    .await;
    match indexed {
        Ok(Some(o)) => {
            tracing::info!(source = %src.id, chunks = o.chunks, suspect = o.suspect, "knowledge: source indexed")
        }
        Ok(None) => tracing::info!(source = %src.id, "knowledge: ingest cancelled (source deleted)"),
        Err(e) => {
            let (code, message) = match &e {
                duduclaw_knowledge::KbError::Rejected { code, message } => (code.to_string(), message.clone()),
                other => {
                    tracing::error!(error = %other, source = %src.id, "knowledge: indexing failed");
                    ("extract_crashed".to_string(), other.public_message())
                }
            };
            fail(&ctx.home, &job, &code, &message);
        }
    }
}
