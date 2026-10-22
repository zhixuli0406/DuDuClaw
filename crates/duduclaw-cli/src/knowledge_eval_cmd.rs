//! `duduclaw knowledge eval` — retrieval quality of the document knowledge
//! base on your own documents and questions, without a gateway.
//!
//! Builds a throw-away `knowledge.db` in a work directory, ingests every
//! supported file of `--docs` with the **same** child-process extraction,
//! chunking and indexing the gateway uses, then runs each question of the
//! JSONL file and reports Recall@1/5/10, MRR@10 per question type, the
//! no-result rate of unanswerable questions and query latency. Never touches
//! the real knowledge base.
//!
//! `--model local:<catalog id>` uses a model installed under the DuDuClaw
//! home (or `--model-home`): every chunk is embedded once (timed, reported
//! as chunks per minute) and the `vector` and `hybrid` modes run with it.
//! Cloud models are not evaluated by this command.

use std::path::{Path, PathBuf};
use std::time::Instant;

use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_gateway::knowledge::extract_spawn::ExtractorCmd;
use duduclaw_gateway::knowledge::pipeline::{extract_file, is_suspect};
use duduclaw_knowledge::acl::{HumanCtx, Narrowing, effective_collections};
use duduclaw_knowledge::blob::BlobStore;
use duduclaw_knowledge::eval::{ModeReport, evaluate, parse_questions};
use duduclaw_knowledge::extract::Format;
use duduclaw_knowledge::limits::KnowledgeConfig;
use duduclaw_knowledge::sources::{Added, NewSource, SourceKind, add_source};
use duduclaw_knowledge::store::{NewCollection, create_collection};
use duduclaw_knowledge::embed::{Embedder, NoEmbedder};
use serde_json::{Value, json};

/// Arguments of `duduclaw knowledge eval`.
#[derive(Debug, Clone)]
pub struct EvalArgs {
    pub docs: PathBuf,
    pub questions: PathBuf,
    pub model: String,
    pub modes: String,
    pub k: usize,
    pub min_recall: Option<f64>,
    pub report: Option<PathBuf>,
    pub work_dir: Option<PathBuf>,
    /// Where `local:` models are installed (default: the DuDuClaw home).
    pub model_home: PathBuf,
}

fn err(msg: impl Into<String>) -> DuDuClawError {
    DuDuClawError::Gateway(msg.into())
}

const EVAL_USER: &str = "knowledge-eval";

struct Ingest {
    docs: usize,
    chunks: usize,
    skipped: Vec<Value>,
    seconds: f64,
}

async fn ingest(
    home: &Path,
    docs: &Path,
    collection_id: &str,
    cfg: &KnowledgeConfig,
) -> Result<Ingest> {
    let extractor =
        ExtractorCmd::current_exe().map_err(|e| err(format!("找不到 duduclaw 執行檔：{e}")))?;
    let store = BlobStore::for_home(home);
    let mut entries: Vec<PathBuf> = std::fs::read_dir(docs)
        .map_err(|e| err(format!("無法讀取文件資料夾 {}：{e}", docs.display())))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    entries.sort();
    let started = Instant::now();
    let (mut n, mut chunks, mut skipped) = (0usize, 0usize, Vec::new());
    for path in entries {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        let format = match Format::from_file_name(&name) {
            Ok(f) => f,
            Err(e) => {
                skipped.push(json!({"file": name, "code": e.code}));
                continue;
            }
        };
        let bytes = std::fs::read(&path).map_err(|e| err(format!("無法讀取 {name}：{e}")))?;
        let sha = store.put(&bytes).map_err(|e| err(e.to_string()))?;
        let mut conn =
            duduclaw_knowledge::schema::open_home(home).map_err(|e| err(e.to_string()))?;
        let new = NewSource {
            collection_id: collection_id.to_string(),
            kind: SourceKind::File,
            title: name.clone(),
            original_name: Some(name.clone()),
            url: None,
            format,
            blob_sha256: sha.clone(),
            bytes: bytes.len() as u64,
            origin: json!({"via": "knowledge_eval"}),
            created_by: EVAL_USER.into(),
        };
        let source = match add_source(&mut conn, &new, cfg).map_err(|e| err(e.public_message()))? {
            Added::New(s) => s,
            Added::Duplicate(_) => {
                skipped.push(json!({"file": name, "code": "duplicate"}));
                continue;
            }
        };
        let blob = store.path_for(&sha).map_err(|e| err(e.to_string()))?;
        match extract_file(home, cfg, &extractor, format, &blob, &name).await {
            Ok((output, _sandboxed)) => {
                let prepared = duduclaw_knowledge::chunk::prepare(format, output.segments);
                let outcome = duduclaw_knowledge::index::write_index(
                    &mut conn,
                    &source.id,
                    &prepared,
                    &output.warnings,
                    &output.extractor_version,
                    &is_suspect,
                    cfg,
                )
                .map_err(|e| err(format!("{name}：{}", e.public_message())))?;
                n += 1;
                chunks += outcome.chunks;
            }
            Err(e) => skipped.push(json!({"file": name, "code": e.code, "message": e.message})),
        }
    }
    Ok(Ingest {
        docs: n,
        chunks,
        skipped,
        seconds: started.elapsed().as_secs_f64(),
    })
}

fn recall_at(report: &ModeReport, k: usize) -> f64 {
    match k {
        0 | 1 => report.answerable.recall_at_1,
        2..=5 => report.answerable.recall_at_5,
        _ => report.answerable.recall_at_10,
    }
}

/// Run the evaluation. Returns the process exit code (1 when below
/// `--min-recall`).
pub async fn run(args: EvalArgs) -> Result<i32> {
    let embedder = load_embedder(&args.model, &args.model_home)?;
    let modes: Vec<String> = args
        .modes
        .split(',')
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .collect();
    let questions = parse_questions(
        &std::fs::read_to_string(&args.questions)
            .map_err(|e| err(format!("無法讀取題目檔：{e}")))?,
    )
    .map_err(err)?;
    let (work, temp) = match &args.work_dir {
        Some(dir) => (dir.clone(), None),
        None => {
            let t = tempfile::tempdir().map_err(|e| err(e.to_string()))?;
            (t.path().to_path_buf(), Some(t))
        }
    };
    let home = work.join("eval-home");
    std::fs::create_dir_all(&home).map_err(|e| err(e.to_string()))?;
    if duduclaw_knowledge::schema::db_path(&home).exists() {
        return Err(err(format!(
            "工作目錄 {} 已有評估資料庫，請換一個空的資料夾。",
            work.display()
        )));
    }
    let cfg = KnowledgeConfig::default();
    let collection = {
        let mut conn =
            duduclaw_knowledge::schema::open_home(&home).map_err(|e| err(e.to_string()))?;
        create_collection(
            &mut conn,
            &NewCollection {
                name: "eval".into(),
                slug: Some("eval".into()),
                ..Default::default()
            },
            EVAL_USER,
            &cfg,
        )
        .map_err(|e| err(e.public_message()))?
    };
    let ing = ingest(&home, &args.docs, &collection.id, &cfg).await?;
    let conn = duduclaw_knowledge::schema::open_home(&home).map_err(|e| err(e.to_string()))?;
    let allowed = effective_collections(
        &conn,
        &HumanCtx::dashboard(EVAL_USER, None, false),
        None,
        &Narrowing::default(),
    )
    .map_err(|e| err(e.to_string()))?;
    let embedding = match &embedder {
        Some(e) => Some(embed_all(&home, e.as_ref())?),
        None => None,
    };
    let cache = duduclaw_knowledge::embed::vector_cache::VectorCache::default();
    let mut reports = Vec::new();
    let mut unavailable = Vec::new();
    for mode in &modes {
        let vector_mode = matches!(mode.as_str(), "vector" | "hybrid");
        let e: &dyn Embedder = match (&embedder, vector_mode) {
            (_, false) => &NoEmbedder,
            (Some(e), true) => e.as_ref(),
            (None, true) => {
                unavailable.push(json!({"mode": mode, "reason": "未指定向量模型（--model local:<模型>）"}));
                continue;
            }
        };
        if !matches!(mode.as_str(), "keyword" | "vector" | "hybrid") {
            unavailable.push(json!({"mode": mode, "reason": "未知的模式（keyword、vector、hybrid）"}));
            continue;
        }
        let report = evaluate(mode, &questions, |q| {
            let started = Instant::now();
            let result = if mode == "vector" {
                duduclaw_knowledge::search::vector_only(&conn, &allowed, q, 10, e, &cache)
            } else {
                duduclaw_knowledge::search::search(&conn, &allowed, q, 10, e, &cache)
            };
            let hits = result.map(|r| r.hits).unwrap_or_default();
            (hits, started.elapsed())
        });
        reports.push(report);
    }
    let passed = match (args.min_recall, reports.first()) {
        (Some(min), Some(r)) => recall_at(r, args.k) >= min,
        _ => true,
    };
    let report = json!({
        "k": args.k,
        "min_recall": args.min_recall,
        "passed": passed,
        "questions": questions.len(),
        "ingest": {
            "documents": ing.docs,
            "chunks": ing.chunks,
            "seconds": ing.seconds,
            "chunks_per_minute": if ing.seconds > 0.0 { ing.chunks as f64 * 60.0 / ing.seconds } else { 0.0 },
            "skipped": ing.skipped,
        },
        "model": args.model,
        "embedding": embedding,
        "modes": reports,
        "unavailable_modes": unavailable,
        "environment": environment(),
    });
    println!(
        "文件 {} 份、段落 {} 段（略過 {} 份），題目 {} 題",
        ing.docs,
        ing.chunks,
        ing.skipped.len(),
        questions.len()
    );
    if let Some(e) = &embedding {
        println!(
            "向量：{} 段，{:.1} 秒，每分鐘 {:.0} 段（{} 執行緒）",
            e["chunks"], e["seconds"].as_f64().unwrap_or(0.0), e["chunks_per_minute"].as_f64().unwrap_or(0.0), e["threads"]
        );
    }
    for r in &reports {
        println!(
            "[{}] Recall@1 {:.2}  Recall@5 {:.2}  Recall@10 {:.2}  MRR@10 {:.2}  查詢 p50 {:.1} ms／p95 {:.1} ms",
            r.mode,
            r.answerable.recall_at_1,
            r.answerable.recall_at_5,
            r.answerable.recall_at_10,
            r.answerable.mrr_at_10,
            r.query_p50_ms,
            r.query_p95_ms
        );
        if !r.misses.is_empty() {
            println!("  未命中：{}", r.misses.join(", "));
        }
    }
    for u in &unavailable {
        println!(
            "[{}] 略過：{}",
            u["mode"].as_str().unwrap_or(""),
            u["reason"].as_str().unwrap_or("")
        );
    }
    if let Some(path) = &args.report {
        std::fs::write(
            path,
            serde_json::to_string_pretty(&report).unwrap_or_default(),
        )
        .map_err(|e| err(format!("無法寫入報表：{e}")))?;
        println!("報表已寫入 {}", path.display());
    }
    drop(temp);
    if !passed {
        println!(
            "Recall@{} 低於 --min-recall {:.2}",
            args.k,
            args.min_recall.unwrap_or(0.0)
        );
        return Ok(1);
    }
    Ok(0)
}

/// `none` ⇒ no embedder; `local:<id>` ⇒ the installed catalog model.
fn load_embedder(model: &str, model_home: &Path) -> Result<Option<Box<dyn Embedder>>> {
    use duduclaw_knowledge::embed::{catalog, install, local_onnx::LocalOnnxEmbedder};
    let model = model.trim();
    if model == "none" {
        return Ok(None);
    }
    let Some(id) = model.strip_prefix("local:") else {
        return Err(err("--model 只能是 none 或 local:<模型>（雲端模型請在 gateway 設定後用儀表板測試）。"));
    };
    let spec = catalog::find(id).ok_or_else(|| {
        let ids: Vec<&str> = catalog::models().iter().map(|m| m.id.as_str()).collect();
        err(format!("沒有這個本機模型：{id}（可用：{}）", ids.join("、")))
    })?;
    if let Some(problem) = install::installed_problem(spec, model_home) {
        return Err(err(format!("{id}：{problem}（模型位置 {}）", model_home.display())));
    }
    let lib = install::ort_lib(model_home).ok_or_else(|| err("這個平台沒有可用的 ONNX Runtime"))?;
    let cfg = duduclaw_knowledge::embed::config::EmbeddingConfig::from_home(model_home);
    Ok(Some(Box::new(LocalOnnxEmbedder::new(spec, model_home, lib, cfg.effective_threads(), cfg.batch_size))))
}

/// Embed every chunk of the evaluation database once; timing for the
/// report. Fails the run on any model error (a partial vector set would
/// make the vector numbers meaningless).
fn embed_all(home: &Path, e: &dyn Embedder) -> Result<Value> {
    let mut conn = duduclaw_knowledge::schema::open_home(home).map_err(|x| err(x.to_string()))?;
    let sources: Vec<String> = {
        let mut stmt = conn.prepare("SELECT id FROM sources ORDER BY id").map_err(|x| err(x.to_string()))?;
        stmt.query_map([], |r| r.get(0))
            .map_err(|x| err(x.to_string()))?
            .collect::<std::result::Result<_, _>>()
            .map_err(|x| err(x.to_string()))?
    };
    let cfg = duduclaw_knowledge::embed::config::EmbeddingConfig::from_home(home);
    let started = Instant::now();
    let (mut embedded, mut reused) = (0usize, 0usize);
    for s in &sources {
        let o = duduclaw_knowledge::embed::job::embed_source(&mut conn, s, e, cfg.batch_size, usize::MAX, |_, _, _| Ok(true))
            .map_err(|x| err(format!("向量計算失敗：{x}")))?;
        embedded += o.embedded;
        reused += o.reused;
    }
    let seconds = started.elapsed().as_secs_f64();
    Ok(json!({
        "model_id": e.model_id(),
        "dim": e.dim(),
        "chunks": embedded,
        "reused": reused,
        "seconds": seconds,
        "chunks_per_minute": if seconds > 0.0 { embedded as f64 * 60.0 / seconds } else { 0.0 },
        "threads": duduclaw_knowledge::embed::config::EmbeddingConfig::from_home(home).effective_threads(),
        "rss_peak_bytes": rss_peak_bytes(),
    }))
}

/// Peak resident set of this process (getrusage); `None` where unknown.
fn rss_peak_bytes() -> Option<u64> {
    #[cfg(unix)]
    {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        // SAFETY: getrusage writes into the provided struct and nothing else.
        #[allow(unsafe_code)]
        let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
        if rc != 0 {
            return None;
        }
        #[allow(unsafe_code)]
        let max = unsafe { usage.assume_init() }.ru_maxrss as u64;
        // macOS reports bytes, Linux kilobytes.
        Some(if cfg!(target_os = "macos") { max } else { max * 1024 })
    }
    #[cfg(not(unix))]
    {
        None
    }
}

fn cpu_model() -> String {
    if cfg!(target_os = "macos") {
        std::process::Command::new("sysctl")
            .args(["-n", "machdep.cpu.brand_string"])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    } else {
        std::fs::read_to_string("/proc/cpuinfo")
            .ok()
            .and_then(|t| {
                t.lines()
                    .find(|l| l.starts_with("model name"))
                    .and_then(|l| l.split(':').nth(1))
                    .map(|v| v.trim().to_string())
            })
            .unwrap_or_default()
    }
}

fn environment() -> Value {
    json!({
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "cpu": cpu_model(),
        "cpus": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
        "rss_peak_bytes": rss_peak_bytes(),
    })
}
