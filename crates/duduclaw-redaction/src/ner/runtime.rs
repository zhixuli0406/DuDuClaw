//! Lazy ONNX Runtime session, inference, idle unload and latency telemetry.
//!
//! One process runs at most one privacy-filter model, so this module owns a
//! single shared [`NerEngine`] keyed by model directory. All eight per-label
//! rules compiled from one `type = "ner"` spec share it, which is what makes
//! the eight of them cost **one** inference: the result cache is keyed on the
//! text, not on the label.
//!
//! ## Why the session is behind a `Mutex`
//!
//! `ort::Session::run` needs `&mut self`. The alternative — a session per
//! rule — would multiply a 1.7 GB resident set by eight. Redaction is on the
//! request path but not in a tight loop, and the G0 spike measured 131 ms
//! p50 for a zh-TW sentence, so serialising inference is the right trade.
//!
//! ## Blocking
//!
//! [`crate::rules::Rule::match_text`] is synchronous by design (see
//! `rules/mod.rs`), and every caller is on a Tokio worker. Running a 130 ms
//! CPU burn on a worker thread without telling Tokio starves the reactor, so
//! inference is wrapped in `block_in_place` whenever the current runtime is
//! multi-threaded. On a `current_thread` runtime `block_in_place` panics, so
//! there we run inline — the only correct option, and the reason the flavour
//! is checked rather than assumed.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;
use ort::value::Tensor;
use tokenizers::Tokenizer;

use super::decode::{self, LabelInfo, RawSpan, TransitionBias};
use super::install::{self, InstallDirs};
use super::{NerConfig, manifest};
use crate::error::{RedactionError, Result};

/// How many recent latencies feed the rolling average shown in the dashboard.
const LATENCY_WINDOW: usize = 100;

/// Rolling inference telemetry.
///
/// Process-global rather than threaded through every caller: there is exactly
/// one model, the dashboard RPC lives in another crate, and the alternative
/// is a handle on `RedactionManager` that exists only to be read once every
/// two seconds by a status poll.
#[derive(Debug, Default)]
pub struct NerStats {
    inner: Mutex<StatsInner>,
}

#[derive(Debug, Default)]
struct StatsInner {
    /// Most recent latencies in ms, newest last, capped at [`LATENCY_WINDOW`].
    samples: VecDeque<f64>,
    calls: u64,
    last_used_at: Option<DateTime<Utc>>,
    loaded: bool,
}

/// What `redaction.model.status` reports about the running model.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct StatsSnapshot {
    /// Mean of the rolling window, `None` before the first inference.
    pub avg_latency_ms: Option<f64>,
    /// Median of the rolling window.
    pub p50_latency_ms: Option<f64>,
    /// Total inferences since process start.
    pub calls: u64,
    /// RFC-3339 timestamp of the last inference.
    pub last_used_at: Option<String>,
    /// Whether a session is resident right now.
    pub loaded: bool,
}

impl NerStats {
    fn record(&self, elapsed: Duration) {
        let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        g.calls = g.calls.saturating_add(1);
        g.last_used_at = Some(Utc::now());
        g.samples.push_back(elapsed.as_secs_f64() * 1000.0);
        while g.samples.len() > LATENCY_WINDOW {
            g.samples.pop_front();
        }
    }

    fn set_loaded(&self, loaded: bool) {
        self.inner.lock().unwrap_or_else(|p| p.into_inner()).loaded = loaded;
    }

    /// Current snapshot.
    pub fn snapshot(&self) -> StatsSnapshot {
        let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let (avg, p50) = if g.samples.is_empty() {
            (None, None)
        } else {
            let sum: f64 = g.samples.iter().sum();
            let mut sorted: Vec<f64> = g.samples.iter().copied().collect();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            (
                Some(round1(sum / g.samples.len() as f64)),
                Some(round1(sorted[sorted.len() / 2])),
            )
        };
        StatsSnapshot {
            avg_latency_ms: avg,
            p50_latency_ms: p50,
            calls: g.calls,
            last_used_at: g.last_used_at.map(|t| t.to_rfc3339()),
            loaded: g.loaded,
        }
    }
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

/// Unload an engine that is ALREADY registered for `model_dir`, if any.
///
/// Distinct from `NerEngine::open(..).unload()`, which would *construct* an
/// engine (with whatever config the caller happened to pass) purely to throw
/// it away — and would leave that config registered for the next rule compile
/// to reuse. Used by `redaction.model.remove`, where the goal is to stop
/// holding the files, not to acquire them.
///
/// Returns true if an engine was found and unloaded.
pub fn unload_registered(model_dir: &Path) -> bool {
    let inner = {
        let reg = engine_registry().lock().unwrap_or_else(|p| p.into_inner());
        reg.get(model_dir).and_then(|w| w.upgrade())
    };
    match inner {
        Some(inner) => {
            NerEngine { inner }.unload();
            true
        }
        None => false,
    }
}

/// The process-wide stats object. Every engine reports into it.
pub fn global_stats() -> &'static Arc<NerStats> {
    static STATS: OnceLock<Arc<NerStats>> = OnceLock::new();
    STATS.get_or_init(|| Arc::new(NerStats::default()))
}

// ---------------------------------------------------------------------------
// ONNX Runtime bootstrap (load-dynamic)
// ---------------------------------------------------------------------------

/// Load `libonnxruntime` from the installed path, once per process.
///
/// With ort's `load-dynamic` feature the binary carries no ONNX Runtime; the
/// library is whatever the install step put under
/// `<home>/lib/onnxruntime/<ver>/`. Initialising twice is an error in ort, so
/// the outcome (including the failure text) is memoised.
fn init_ort(lib_path: &Path) -> std::result::Result<(), String> {
    static INIT: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| {
        if !lib_path.exists() {
            return Err(format!(
                "ONNX Runtime 執行庫不存在：{}",
                lib_path.display()
            ));
        }
        let builder = ort::init_from(lib_path)
            .map_err(|e| format!("載入 ONNX Runtime 失敗（{}）：{e}", lib_path.display()))?;
        // `commit` returns false when an environment is already installed —
        // harmless (another part of the process got there first), and not a
        // reason to refuse to run.
        let _ = builder.commit();
        Ok(())
    })
    .clone()
}

// ---------------------------------------------------------------------------
// Result cache
// ---------------------------------------------------------------------------

/// Cache key: SHA-256 of the scanned text.
///
/// A cryptographic digest rather than a cheap hash on purpose — a collision
/// here would return one document's spans for another document's text, which
/// is a data-leak bug, not a performance bug. (The design doc says blake3;
/// blake3 is not in this workspace and sha2 is, so sha2 it is. The digest
/// costs microseconds against a 130 ms inference.)
type CacheKey = [u8; 32];

fn cache_key(text: &str) -> CacheKey {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.as_bytes()).into()
}

/// Minimal LRU. A dependency for ~40 lines is not worth it, and the eviction
/// policy matters enough to want it visible.
struct Lru {
    cap: usize,
    map: HashMap<CacheKey, Arc<Vec<RawSpan>>>,
    order: VecDeque<CacheKey>,
}

impl Lru {
    fn new(cap: usize) -> Self {
        Self {
            cap: cap.max(1),
            map: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn get(&mut self, k: &CacheKey) -> Option<Arc<Vec<RawSpan>>> {
        let v = self.map.get(k)?.clone();
        if let Some(pos) = self.order.iter().position(|x| x == k) {
            self.order.remove(pos);
        }
        self.order.push_back(*k);
        Some(v)
    }

    fn put(&mut self, k: CacheKey, v: Arc<Vec<RawSpan>>) {
        if self.map.insert(k, v).is_some()
            && let Some(pos) = self.order.iter().position(|x| x == &k)
        {
            self.order.remove(pos);
        }
        self.order.push_back(k);
        while self.map.len() > self.cap {
            match self.order.pop_front() {
                Some(old) => {
                    self.map.remove(&old);
                }
                None => break,
            }
        }
    }

    fn len(&self) -> usize {
        self.map.len()
    }
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// A loaded model: session plus everything derived from the model directory.
struct Loaded {
    session: Session,
    tokenizer: Tokenizer,
    labels: Vec<LabelInfo>,
    transitions: Vec<Vec<bool>>,
    bias: TransitionBias,
    input_names: Vec<String>,
    last_used: Instant,
}

struct EngineInner {
    model_dir: PathBuf,
    ort_lib_path: PathBuf,
    threads: usize,
    idle_unload: Option<Duration>,
    loaded: Mutex<Option<Loaded>>,
    cache: Mutex<Lru>,
    stats: Arc<NerStats>,
    /// Set once an idle-unload watchdog is running, so repeated loads do not
    /// pile up threads.
    watchdog: AtomicBool,
}

/// Shared model runtime. Cheap to clone (an `Arc` inside).
#[derive(Clone)]
pub struct NerEngine {
    inner: Arc<EngineInner>,
}

impl std::fmt::Debug for NerEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NerEngine")
            .field("model_dir", &self.inner.model_dir)
            .field("threads", &self.inner.threads)
            .finish()
    }
}

/// Engines already built in this process, keyed by model directory.
fn engine_registry() -> &'static Mutex<HashMap<PathBuf, Weak<EngineInner>>> {
    static REG: OnceLock<Mutex<HashMap<PathBuf, Weak<EngineInner>>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

impl NerEngine {
    /// Build (or reuse) the engine for `dirs`.
    ///
    /// **Fail-closed**: if the model is not fully installed this returns an
    /// error, which the engine compiler turns into a rule-compile failure and
    /// the gateway into the poison state. It must never return an engine that
    /// silently matches nothing.
    ///
    /// Reuse is by model directory, and the *first* caller's `config` wins for
    /// as long as any rule still holds the engine. In practice a hot reload
    /// drops every old rule before the new ones compile, so an edit to
    /// `[redaction.ner]` takes effect on the next reload; an edit made while a
    /// long request still holds the old engine lands one reload later.
    pub fn open(dirs: &InstallDirs, config: &NerConfig) -> Result<Self> {
        if let Some(problem) = install::installed_problem(dirs) {
            return Err(RedactionError::config(problem));
        }
        let Some(ort_lib_path) = manifest::ort_lib_path(&dirs.ort_lib_root) else {
            return Err(RedactionError::config(format!(
                "AI 智慧偵測在這個平台（{}）沒有可用的 ONNX Runtime",
                manifest::current_target()
            )));
        };

        let key = dirs.model_dir.clone();
        let mut reg = engine_registry().lock().unwrap_or_else(|p| p.into_inner());
        if let Some(existing) = reg.get(&key).and_then(|w| w.upgrade()) {
            return Ok(Self { inner: existing });
        }
        let inner = Arc::new(EngineInner {
            model_dir: key.clone(),
            ort_lib_path,
            threads: config.threads,
            idle_unload: (config.idle_unload_minutes > 0)
                .then(|| Duration::from_secs(config.idle_unload_minutes * 60)),
            loaded: Mutex::new(None),
            cache: Mutex::new(Lru::new(config.cache_entries)),
            stats: global_stats().clone(),
            watchdog: AtomicBool::new(false),
        });
        reg.insert(key, Arc::downgrade(&inner));
        Ok(Self { inner })
    }

    /// Model revision this engine serves, for the audit trail.
    pub fn model_revision(&self) -> &'static str {
        manifest::MODEL_REVISION
    }

    /// Entity spans over the whole of `text`, in byte offsets into `text`.
    ///
    /// Chunks longer inputs on paragraph boundaries and maps each chunk's
    /// spans back. Results are cached on the full text.
    pub fn spans(&self, text: &str, max_chars: usize) -> Result<Arc<Vec<RawSpan>>> {
        let key = cache_key(text);
        if let Some(hit) = self
            .inner
            .cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&key)
        {
            return Ok(hit);
        }

        let spans = self.run_blocking(text, max_chars)?;
        let spans = Arc::new(spans);
        self.inner
            .cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .put(key, spans.clone());
        Ok(spans)
    }

    /// Run inference, yielding the Tokio worker when we are on one.
    fn run_blocking(&self, text: &str, max_chars: usize) -> Result<Vec<RawSpan>> {
        let multi_thread = matches!(
            tokio::runtime::Handle::try_current().map(|h| h.runtime_flavor()),
            Ok(tokio::runtime::RuntimeFlavor::MultiThread)
        );
        if multi_thread {
            tokio::task::block_in_place(|| self.run_all_chunks(text, max_chars))
        } else {
            self.run_all_chunks(text, max_chars)
        }
    }

    fn run_all_chunks(&self, text: &str, max_chars: usize) -> Result<Vec<RawSpan>> {
        let chunks = super::chunk_text(text, max_chars);
        let mut out: Vec<RawSpan> = Vec::new();
        let mut guard = self
            .inner
            .loaded
            .lock()
            .map_err(|e| RedactionError::config(format!("NER session lock poisoned: {e}")))?;
        if guard.is_none() {
            *guard = Some(self.load()?);
            self.inner.stats.set_loaded(true);
            self.spawn_idle_watchdog();
        }
        let loaded = guard.as_mut().expect("just loaded");

        for (offset, chunk) in chunks {
            let started = Instant::now();
            let spans = infer_chunk(loaded, chunk)?;
            let elapsed = started.elapsed();
            self.inner.stats.record(elapsed);
            tracing::debug!(
                chars = chunk.chars().count(),
                bytes = chunk.len(),
                spans = spans.len(),
                ms = elapsed.as_millis(),
                "ner chunk scored"
            );
            for mut s in spans {
                // Map chunk-local byte offsets back onto the full text, then
                // snap to char boundaries against the ORIGINAL string.
                let (a, b) = super::safe_bounds(text, offset + s.start, offset + s.end);
                if b <= a {
                    continue;
                }
                s.start = a;
                s.end = b;
                out.push(s);
            }
        }
        loaded.last_used = Instant::now();
        Ok(out)
    }

    fn load(&self) -> Result<Loaded> {
        let t0 = Instant::now();
        init_ort(&self.inner.ort_lib_path).map_err(RedactionError::config)?;

        let dir = &self.inner.model_dir;
        let cfg_raw = std::fs::read(dir.join("config.json"))?;
        let cfg: serde_json::Value = serde_json::from_slice(&cfg_raw)?;
        let id2label: HashMap<usize, String> = cfg
            .get("id2label")
            .and_then(|v| v.as_object())
            .ok_or_else(|| RedactionError::config("模型 config.json 缺少 id2label"))?
            .iter()
            .filter_map(|(k, v)| Some((k.parse::<usize>().ok()?, v.as_str()?.to_string())))
            .collect();
        let labels = decode::parse_labels(&id2label).map_err(RedactionError::config)?;
        let transitions = decode::build_transitions(&labels);

        let bias = match std::fs::read(dir.join("viterbi_calibration.json")) {
            Ok(raw) => serde_json::from_slice::<serde_json::Value>(&raw)
                .map(|v| TransitionBias::from_calibration_json(&v))
                .unwrap_or_default(),
            Err(_) => TransitionBias::default(),
        };

        let tokenizer = Tokenizer::from_file(dir.join("tokenizer.json"))
            .map_err(|e| RedactionError::config(format!("載入 tokenizer 失敗：{e}")))?;

        // ort's builder returns `Error<SessionBuilder>` (the error owns the
        // builder), so each step is mapped to a string before it can join the
        // crate's error type.
        let threads = self.inner.threads;
        let graph = dir.join("onnx").join("model_q4.onnx");
        let build = || -> std::result::Result<Session, String> {
            let b = Session::builder().map_err(|e| e.to_string())?;
            let b = b
                .with_optimization_level(GraphOptimizationLevel::Level3)
                .map_err(|e| e.to_string())?;
            let mut b = b.with_intra_threads(threads).map_err(|e| e.to_string())?;
            b.commit_from_file(&graph).map_err(|e| e.to_string())
        };
        let session = build()
            .map_err(|e| RedactionError::config(format!("建立 ONNX session 失敗：{e}")))?;

        let input_names: Vec<String> = session
            .inputs()
            .iter()
            .map(|i| i.name().to_string())
            .collect();

        tracing::info!(
            ms = t0.elapsed().as_millis(),
            threads = self.inner.threads,
            labels = labels.len(),
            "privacy-filter model loaded"
        );
        Ok(Loaded {
            session,
            tokenizer,
            labels,
            transitions,
            bias,
            input_names,
            last_used: Instant::now(),
        })
    }

    /// Drop the session if it has been idle past the configured window.
    /// Returns true when it actually unloaded.
    pub fn unload_if_idle(&self) -> bool {
        let Some(window) = self.inner.idle_unload else {
            return false;
        };
        let mut guard = match self.inner.loaded.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let idle = guard
            .as_ref()
            .map(|l| l.last_used.elapsed() >= window)
            .unwrap_or(false);
        if idle {
            *guard = None;
            self.inner.stats.set_loaded(false);
            tracing::info!("privacy-filter model unloaded after idle window");
            return true;
        }
        false
    }

    /// Drop the session unconditionally (used by `redaction.model.remove`).
    pub fn unload(&self) {
        let mut guard = match self.inner.loaded.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        *guard = None;
        self.inner.stats.set_loaded(false);
        self.inner
            .cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .map
            .clear();
    }

    /// Number of cached texts — test/telemetry hook.
    pub fn cache_len(&self) -> usize {
        self.inner.cache.lock().unwrap_or_else(|p| p.into_inner()).len()
    }

    /// One background thread per loaded session, ticking the idle check.
    ///
    /// Holds a `Weak`, so it exits when the last rule referencing this engine
    /// goes away (a config hot-reload drops the old engine). It also exits
    /// after unloading, and a later load spawns a fresh one — no accumulation.
    fn spawn_idle_watchdog(&self) {
        if self.inner.idle_unload.is_none() {
            return;
        }
        if self.inner.watchdog.swap(true, Ordering::SeqCst) {
            return;
        }
        let weak = Arc::downgrade(&self.inner);
        std::thread::Builder::new()
            .name("ner-idle-unload".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(Duration::from_secs(60));
                    let Some(inner) = weak.upgrade() else { return };
                    let engine = NerEngine { inner };
                    if engine.unload_if_idle() {
                        engine.inner.watchdog.store(false, Ordering::SeqCst);
                        return;
                    }
                }
            })
            .ok();
    }
}

/// One forward pass over one chunk.
fn infer_chunk(loaded: &mut Loaded, text: &str) -> Result<Vec<RawSpan>> {
    let enc = loaded
        .tokenizer
        // `false`: the model takes raw token ids, no special tokens (verified
        // in the G0 spike — adding them shifts every offset).
        .encode(text, false)
        .map_err(|e| RedactionError::config(format!("tokenize 失敗：{e}")))?;
    let ids: Vec<i64> = enc.get_ids().iter().map(|&v| v as i64).collect();
    let t = ids.len();
    if t == 0 {
        return Ok(Vec::new());
    }
    let mask: Vec<i64> = enc.get_attention_mask().iter().map(|&v| v as i64).collect();
    let offsets = enc.get_offsets();

    let mut feeds: Vec<(std::borrow::Cow<'_, str>, ort::session::SessionInputValue<'_>)> =
        Vec::with_capacity(loaded.input_names.len());
    for name in &loaded.input_names {
        let v: Vec<i64> = match name.as_str() {
            "input_ids" => ids.clone(),
            "attention_mask" => mask.clone(),
            "position_ids" => (0..t as i64).collect(),
            "token_type_ids" => vec![0i64; t],
            other => {
                return Err(RedactionError::config(format!(
                    "模型需要未知的輸入 `{other}`，這個版本的 DuDuClaw 不支援"
                )));
            }
        };
        let tensor = Tensor::from_array((vec![1i64, t as i64], v))
            .map_err(|e| RedactionError::config(format!("建立輸入張量失敗：{e}")))?;
        feeds.push((
            std::borrow::Cow::Owned(name.clone()),
            ort::session::SessionInputValue::from(tensor),
        ));
    }

    let outputs = loaded
        .session
        .run(feeds)
        .map_err(|e| RedactionError::config(format!("推論失敗：{e}")))?;
    let (shape, data) = outputs[0]
        .try_extract_tensor::<f32>()
        .map_err(|e| RedactionError::config(format!("讀取 logits 失敗：{e}")))?;

    let n_lab = loaded.labels.len();
    if shape.len() != 3 || shape[1] as usize != t || shape[2] as usize != n_lab {
        return Err(RedactionError::config(format!(
            "logits 形狀不符（預期 [1,{t},{n_lab}]，實際 {:?}）",
            &shape[..]
        )));
    }

    let mut emissions: Vec<Vec<f32>> = Vec::with_capacity(t);
    for i in 0..t {
        emissions.push(decode::log_softmax_row(&data[i * n_lab..(i + 1) * n_lab]));
    }
    let path = decode::viterbi(&emissions, &loaded.labels, &loaded.transitions, &loaded.bias);
    Ok(decode::path_to_spans(&path, &loaded.labels, offsets))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_start_empty_and_round_to_one_decimal() {
        let s = NerStats::default();
        let snap = s.snapshot();
        assert_eq!(snap.calls, 0);
        assert!(snap.avg_latency_ms.is_none());
        assert!(snap.p50_latency_ms.is_none());
        assert!(!snap.loaded);

        s.record(Duration::from_millis(100));
        s.record(Duration::from_millis(200));
        s.record(Duration::from_millis(300));
        let snap = s.snapshot();
        assert_eq!(snap.calls, 3);
        assert_eq!(snap.avg_latency_ms, Some(200.0));
        assert_eq!(snap.p50_latency_ms, Some(200.0));
        assert!(snap.last_used_at.is_some());
    }

    #[test]
    fn stats_window_keeps_only_the_most_recent_hundred() {
        let s = NerStats::default();
        for i in 0..150 {
            s.record(Duration::from_millis(i));
        }
        let snap = s.snapshot();
        assert_eq!(snap.calls, 150, "call count must be lifetime, not windowed");
        // Window holds 50..149, mean 99.5.
        assert_eq!(snap.avg_latency_ms, Some(99.5));
    }

    #[test]
    fn stats_loaded_flag_tracks_the_session() {
        let s = NerStats::default();
        s.set_loaded(true);
        assert!(s.snapshot().loaded);
        s.set_loaded(false);
        assert!(!s.snapshot().loaded);
    }

    #[test]
    fn cache_key_separates_different_texts() {
        assert_ne!(cache_key("王小明"), cache_key("王小華"));
        assert_eq!(cache_key("abc"), cache_key("abc"));
    }

    fn span(label: &str) -> Arc<Vec<RawSpan>> {
        Arc::new(vec![RawSpan {
            label: label.into(),
            start: 0,
            end: 1,
        }])
    }

    #[test]
    fn lru_returns_what_was_put() {
        let mut l = Lru::new(4);
        let k = cache_key("a");
        l.put(k, span("private_person"));
        assert_eq!(l.get(&k).unwrap()[0].label, "private_person");
        assert!(l.get(&cache_key("b")).is_none());
    }

    #[test]
    fn lru_evicts_the_least_recently_used() {
        let mut l = Lru::new(2);
        let (a, b, c) = (cache_key("a"), cache_key("b"), cache_key("c"));
        l.put(a, span("a"));
        l.put(b, span("b"));
        // Touch `a` so `b` becomes the eviction victim.
        assert!(l.get(&a).is_some());
        l.put(c, span("c"));
        assert_eq!(l.len(), 2);
        assert!(l.get(&a).is_some(), "recently used entry must survive");
        assert!(l.get(&c).is_some());
        assert!(l.get(&b).is_none(), "least recently used must be evicted");
    }

    #[test]
    fn lru_overwrite_does_not_grow_the_order_queue() {
        let mut l = Lru::new(2);
        let a = cache_key("a");
        for _ in 0..10 {
            l.put(a, span("a"));
        }
        assert_eq!(l.len(), 1);
        assert_eq!(l.order.len(), 1, "repeat puts must not leak order entries");
    }

    #[test]
    fn lru_capacity_zero_is_clamped_to_one() {
        let mut l = Lru::new(0);
        l.put(cache_key("a"), span("a"));
        assert_eq!(l.len(), 1);
    }

    #[test]
    fn unload_registered_is_a_no_op_for_an_unknown_directory() {
        // The point of this function: it must never construct (and register)
        // an engine as a side effect of being asked to free one.
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(!unload_registered(tmp.path()));
        let reg = engine_registry().lock().unwrap_or_else(|p| p.into_inner());
        assert!(!reg.contains_key(tmp.path()), "must not register anything");
    }

    #[test]
    fn engine_open_fails_closed_when_the_model_is_absent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dirs = InstallDirs::under_home(tmp.path());
        let err = NerEngine::open(&dirs, &NerConfig::default()).unwrap_err();
        assert!(
            matches!(err, RedactionError::Config(_)),
            "absent model must be a config error, never a silent no-op: {err:?}"
        );
    }
}
