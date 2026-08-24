//! Marketplace-grade Hugging Face search — intent + hardware-fit + MoE-aware.
//!
//! The old flow (`curated.rs` list + single-quant `hf_api::search_models`)
//! demanded users understand GGUF/quant codes and hand-edit inference.toml.
//! This module powers the "本地模型" marketplace page instead: pick an
//! INTENT (chat / code / long-context / chinese), the gateway matches it
//! against a publisher-whitelisted HF sweep, computes a tri-state hardware
//! fit per quant (green/comfortable · yellow/tight · red/too-big), and
//! flags MoE models whose experts can be offloaded
//! (`llama-cpp-2 add_cpu_moe_override` — the turbo-fieldfare lesson: MoE
//! routed experts don't need to live in fast memory).
//!
//! Design doc: commercial/docs/DESIGN-local-model-marketplace-2026-08-13.md
//! (HF API surface live-verified 2026-08-13). Fail-open everywhere: HF
//! unreachable ⇒ cached results or empty list, never an error page.

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::types::HardwareInfo;

const HF_API_BASE: &str = "https://huggingface.co/api";
/// Whole-sweep cache TTL (same discipline as `hf_api.rs` / skill hub).
const CACHE_TTL_HOURS: u64 = 24;
/// Curated GGUF publishers, priority order (localbench KL-divergence Pareto
/// frontier 2026-08: unsloth strongest on MoE, bartowski/mradermacher broad;
/// lmstudio-community weaker quality but beginner-friendly file counts;
/// ggml-org official). TheBloke is years-stale — deliberately absent.
const PUBLISHERS: &[&str] = &[
    "unsloth",
    "bartowski",
    "mradermacher",
    "lmstudio-community",
    "ggml-org",
];
/// Per-publisher list-call page size.
const LIST_LIMIT: usize = 30;
/// After dedup + intent filter, at most this many repos get a detail fetch
/// (`?blobs=true`) per sweep — bounds the call count far under the
/// anonymous API budget (500/5min).
const DETAIL_FETCH_CAP: usize = 12;
/// llama.cpp runtime overhead reserve (graph + buffers), bytes.
const RUNTIME_OVERHEAD_BYTES: u64 = 768 * 1024 * 1024;
/// Only this fraction of available memory is considered usable.
const USABLE_FRACTION: f64 = 0.9;
/// Below this fraction of usable memory ⇒ Comfortable; above ⇒ Tight.
const COMFORT_FRACTION: f64 = 0.6;

// ── Public types ─────────────────────────────────────────────────────────

/// User-facing intent — the marketplace's first question.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    Chat,
    Code,
    LongContext,
    Chinese,
}

impl Intent {
    pub fn from_str_loose(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "chat" => Some(Self::Chat),
            "code" => Some(Self::Code),
            "long_context" | "longcontext" | "long" => Some(Self::LongContext),
            "chinese" | "zh" => Some(Self::Chinese),
            _ => None,
        }
    }
}

/// Tri-state hardware fit for one quant file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FitLevel {
    Comfortable,
    Tight,
    TooBig,
}

/// One downloadable quant variant (single file or shard group).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuantOption {
    /// First (or only) shard filename.
    pub filename: String,
    /// Normalized quant code ("Q4_K_M", "IQ4_XS", …).
    pub quant: String,
    /// Total bytes across all shards.
    pub size_bytes: u64,
    /// All shard paths when split (empty = single file).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shards: Vec<String>,
    /// Importance-matrix calibrated quant (filename carries "imatrix").
    pub imatrix: bool,
    /// Full-load fit against the machine.
    pub fit: FitLevel,
    /// MoE-only: fit when experts are offloaded to system RAM
    /// (`cpu_moe`) — attention/shared weights against GPU memory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fit_offload: Option<FitLevel>,
}

/// One marketplace card.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketModel {
    /// HF repo id ("unsloth/Qwen3-8B-GGUF").
    pub repo: String,
    /// Cleaned display name ("Qwen3-8B").
    pub name: String,
    pub publisher: String,
    pub downloads: u64,
    pub likes: u64,
    /// Gated repo — install requires the user's own HF token + web consent.
    pub gated: bool,
    /// Total parameter count in billions, when derivable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params_b: Option<f64>,
    /// GGUF architecture from HF's server-side header parse ("qwen3moe").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub architecture: Option<String>,
    /// Mixture-of-Experts — expert offload applies.
    pub moe: bool,
    /// MoE active parameters in billions (the "-A3B" in "30B-A3B").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_params_b: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_length: Option<u64>,
    pub has_chat_template: bool,
    pub languages: Vec<String>,
    /// Auto-selected quant for this machine (the one-click target).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recommended: Option<QuantOption>,
    /// Every enumerable quant, for the advanced drawer.
    pub quants: Vec<QuantOption>,
}

// ── HF wire types ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct HfListModel {
    #[serde(rename = "modelId", alias = "id")]
    model_id: String,
    #[serde(default)]
    downloads: Option<u64>,
    #[serde(default)]
    tags: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct HfDetailModel {
    #[serde(default)]
    downloads: Option<u64>,
    #[serde(default)]
    likes: Option<u64>,
    #[serde(default)]
    tags: Option<Vec<String>>,
    /// `false`, `"auto"` or `"manual"` — hence a raw Value.
    #[serde(default)]
    gated: Option<serde_json::Value>,
    #[serde(default)]
    siblings: Option<Vec<HfSibling>>,
    /// HF's server-side GGUF header parse — architecture / total params /
    /// context length / chat template without downloading a byte.
    #[serde(default)]
    gguf: Option<HfGgufMeta>,
}

#[derive(Debug, Deserialize)]
struct HfSibling {
    #[serde(rename = "rfilename")]
    filename: String,
    #[serde(default)]
    size: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct HfGgufMeta {
    #[serde(default)]
    total: Option<u64>,
    #[serde(default)]
    architecture: Option<String>,
    #[serde(default)]
    context_length: Option<u64>,
    #[serde(default)]
    chat_template: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SweepCache {
    timestamp: u64,
    intent: String,
    results: Vec<MarketModel>,
}

// ── Search ───────────────────────────────────────────────────────────────

/// One marketplace sweep for `intent`, fitted against `hw`.
///
/// Publisher-whitelisted list calls (5) → dedup by base-model identity →
/// intent filter → detail fetch for the top [`DETAIL_FETCH_CAP`] repos
/// (gguf meta + quant enumeration) → per-quant fit. 24h cache per intent.
pub async fn market_search(intent: Intent, hw: &HardwareInfo, home_dir: &Path) -> Vec<MarketModel> {
    let intent_key = format!("{intent:?}").to_ascii_lowercase();
    if let Some(cached) = load_cache(&intent_key, home_dir).await {
        info!(intent = %intent_key, count = cached.len(), "market: cached sweep");
        // Fit depends on live hardware — recompute over cached metadata.
        return cached.into_iter().map(|m| refit(m, hw)).collect();
    }

    let client = match reqwest::Client::builder().timeout(Duration::from_secs(15)).build() {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "market: http client build failed");
            return Vec::new();
        }
    };
    let hf_token = std::env::var("HF_TOKEN").unwrap_or_default();

    // 1. Publisher sweep (one list call each, merged).
    let mut listed: Vec<HfListModel> = Vec::new();
    for author in PUBLISHERS {
        let url = format!(
            "{HF_API_BASE}/models?author={author}&filter=gguf&pipeline_tag=text-generation&sort=downloads&direction=-1&limit={LIST_LIMIT}"
        );
        let mut req = client.get(&url);
        if !hf_token.is_empty() {
            req = req.bearer_auth(&hf_token);
        }
        match req.send().await {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(mut models) = resp.json::<Vec<HfListModel>>().await {
                    listed.append(&mut models);
                }
            }
            Ok(resp) => warn!(author, status = %resp.status(), "market: list call failed"),
            Err(e) => warn!(author, error = %e, "market: HF unreachable"),
        }
    }

    // 2. Dedup by base identity, keeping the highest-priority publisher.
    let mut best: std::collections::BTreeMap<String, HfListModel> = Default::default();
    for m in listed {
        if !intent_matches_listing(intent, &m) {
            continue;
        }
        let key = base_identity(&m.model_id);
        match best.get(&key) {
            Some(existing)
                if publisher_rank(&existing.model_id) <= publisher_rank(&m.model_id) => {}
            _ => {
                best.insert(key, m);
            }
        }
    }
    let mut candidates: Vec<HfListModel> = best.into_values().collect();
    candidates.sort_by(|a, b| b.downloads.unwrap_or(0).cmp(&a.downloads.unwrap_or(0)));
    candidates.truncate(DETAIL_FETCH_CAP);

    // 3. Detail fetch → cards.
    let mut out: Vec<MarketModel> = Vec::new();
    for c in candidates {
        if let Some(model) = fetch_market_model(&client, &hf_token, &c.model_id, intent, hw).await {
            out.push(model);
        }
    }

    if !out.is_empty() {
        save_cache(&intent_key, &out, home_dir).await;
    }
    info!(intent = %intent_key, count = out.len(), "market: sweep completed");
    out
}

/// Full quant listing for one repo (the advanced drawer / install picker).
pub async fn market_quants(repo: &str, hw: &HardwareInfo, home_dir: &Path) -> Option<MarketModel> {
    let _ = home_dir; // reserved for a per-repo cache if sweeps prove hot
    if !valid_repo_id(repo) {
        return None;
    }
    let client = reqwest::Client::builder().timeout(Duration::from_secs(15)).build().ok()?;
    let hf_token = std::env::var("HF_TOKEN").unwrap_or_default();
    fetch_market_model(&client, &hf_token, repo, Intent::Chat, hw).await
}

async fn fetch_market_model(
    client: &reqwest::Client,
    hf_token: &str,
    repo: &str,
    _intent: Intent,
    hw: &HardwareInfo,
) -> Option<MarketModel> {
    let url = format!("{HF_API_BASE}/models/{repo}?blobs=true");
    let mut req = client.get(&url);
    if !hf_token.is_empty() {
        req = req.bearer_auth(hf_token);
    }
    let resp = req.send().await.ok()?;
    if !resp.status().is_success() {
        warn!(repo, status = %resp.status(), "market: detail fetch failed");
        return None;
    }
    let detail: HfDetailModel = resp.json().await.ok()?;

    let name = display_name(repo);
    let tags = detail.tags.unwrap_or_default();
    let gguf = detail.gguf;
    let architecture = gguf.as_ref().and_then(|g| g.architecture.clone());
    let (moe, active_params_b) = detect_moe(architecture.as_deref(), &name);
    let params_b = gguf
        .as_ref()
        .and_then(|g| g.total)
        .map(|t| t as f64 / 1e9)
        .or_else(|| parse_params_b(&name));

    let mut quants = enumerate_quants(detail.siblings.unwrap_or_default());
    for q in &mut quants {
        apply_fit(q, hw, moe, active_params_b, params_b);
    }
    let recommended = pick_auto_quant(&quants).cloned();

    Some(MarketModel {
        repo: repo.to_string(),
        name,
        publisher: repo.split('/').next().unwrap_or("").to_string(),
        downloads: detail.downloads.unwrap_or(0),
        likes: detail.likes.unwrap_or(0),
        gated: matches!(&detail.gated, Some(v) if v.as_bool() != Some(false)),
        params_b,
        architecture,
        moe,
        active_params_b,
        context_length: gguf.as_ref().and_then(|g| g.context_length),
        has_chat_template: gguf
            .as_ref()
            .and_then(|g| g.chat_template.as_deref())
            .is_some_and(|t| !t.is_empty()),
        languages: languages_from(&tags, repo),
        recommended,
        quants,
    })
}

// ── Classification helpers ───────────────────────────────────────────────

/// Cheap listing-stage intent filter (tags + name heuristics — quant repos
/// notoriously under-tag, so name hints back the tags up).
fn intent_matches_listing(intent: Intent, m: &HfListModel) -> bool {
    let id = m.model_id.to_ascii_lowercase();
    let tags = m.tags.clone().unwrap_or_default();
    let has_tag = |t: &str| tags.iter().any(|x| x == t);
    match intent {
        // Chat is the broad default — instruct-tuned families all qualify;
        // exclude obvious base/completion-only artifacts.
        Intent::Chat => !id.contains("-base") && !id.contains("embed"),
        Intent::Code => id.contains("coder") || id.contains("code") || has_tag("code"),
        // Listing has no context_length; defer to detail stage — keep
        // families known for 128K+ and let `context_length` decide later.
        Intent::LongContext => !id.contains("embed"),
        Intent::Chinese => {
            has_tag("zh")
                || ["qwen", "glm", "yi-", "minicpm", "breeze", "taide", "internlm"]
                    .iter()
                    .any(|h| id.contains(h))
        }
    }
}

/// Strip publisher + GGUF suffixes so "unsloth/Qwen3-8B-GGUF" and
/// "bartowski/Qwen3-8B-GGUF" dedup to one identity.
fn base_identity(repo: &str) -> String {
    repo.split('/')
        .next_back()
        .unwrap_or(repo)
        .to_ascii_lowercase()
        .replace("-gguf", "")
        .replace("_gguf", "")
}

fn publisher_rank(repo: &str) -> usize {
    let owner = repo.split('/').next().unwrap_or("");
    PUBLISHERS.iter().position(|p| *p == owner).unwrap_or(usize::MAX)
}

fn display_name(repo: &str) -> String {
    repo.split('/')
        .next_back()
        .unwrap_or(repo)
        .replace("-GGUF", "")
        .replace("-gguf", "")
}

/// MoE detection: GGUF architecture carries "moe", or the name carries the
/// "-A<n>B" active-params convention ("Qwen3-30B-A3B").
fn detect_moe(architecture: Option<&str>, name: &str) -> (bool, Option<f64>) {
    let arch_moe = architecture.is_some_and(|a| a.to_ascii_lowercase().contains("moe"));
    let active = parse_active_params_b(name);
    (arch_moe || active.is_some(), active)
}

fn parse_active_params_b(name: &str) -> Option<f64> {
    let lower = name.to_ascii_lowercase();
    for part in lower.split(&['-', '_'][..]) {
        if let Some(rest) = part.strip_prefix('a') {
            if let Some(num) = rest.strip_suffix('b') {
                if let Ok(v) = num.parse::<f64>() {
                    if v > 0.0 && v < 1000.0 {
                        return Some(v);
                    }
                }
            }
        }
    }
    None
}

fn parse_params_b(name: &str) -> Option<f64> {
    let lower = name.to_ascii_lowercase();
    for part in lower.split(&['-', '_', '.'][..]) {
        // Skip the active-params token ("a3b") — that is not the total.
        if part.starts_with('a') {
            continue;
        }
        if let Some(num) = part.strip_suffix('b') {
            if let Ok(v) = num.parse::<f64>() {
                if v > 0.0 && v < 3000.0 {
                    return Some(v);
                }
            }
        }
    }
    None
}

fn languages_from(tags: &[String], repo: &str) -> Vec<String> {
    let mut langs: Vec<String> = tags
        .iter()
        .filter(|t| matches!(t.as_str(), "en" | "zh" | "ja" | "ko" | "de" | "fr" | "es" | "multilingual"))
        .cloned()
        .collect();
    // Name-hint fallback — quant repos habitually drop language tags.
    let id = repo.to_ascii_lowercase();
    if !langs.iter().any(|l| l == "zh")
        && ["qwen", "glm", "yi-", "minicpm", "breeze", "taide", "internlm"]
            .iter()
            .any(|h| id.contains(h))
    {
        langs.push("zh".to_string());
    }
    if langs.is_empty() {
        langs.push("en".to_string());
    }
    langs
}

// ── Quant enumeration ────────────────────────────────────────────────────

const QUANT_PATTERNS: &[&str] = &[
    "Q4_K_M", "Q4_K_S", "Q4_K_L", "Q4_0", "Q4_1", "Q5_K_M", "Q5_K_S", "Q5_0", "Q5_1", "Q3_K_M",
    "Q3_K_S", "Q3_K_L", "Q6_K", "Q8_0", "Q2_K", "F16", "BF16", "F32", "IQ4_XS", "IQ4_NL", "IQ3_M",
    "IQ3_XS", "IQ3_XXS", "IQ2_M",
];

fn quant_of(filename: &str) -> Option<String> {
    let upper = filename.to_ascii_uppercase();
    QUANT_PATTERNS.iter().find(|p| upper.contains(*p)).map(|p| p.to_string())
}

/// Path-safe check mirroring `downloader::validate_filename`'s allowlist,
/// extended with `/` for quant-subdirectory repos ("Q4_K_M/model-….gguf").
fn safe_repo_path(p: &str) -> bool {
    !p.contains("..")
        && !p.starts_with('/')
        && !p.starts_with('.')
        && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
}

/// Group `siblings` into quant variants; shard groups are summed and sorted.
fn enumerate_quants(siblings: Vec<HfSibling>) -> Vec<QuantOption> {
    use std::collections::BTreeMap;
    // key = (quant, shard-group stem or filename)
    let mut groups: BTreeMap<String, QuantOption> = BTreeMap::new();
    for s in siblings {
        if !s.filename.ends_with(".gguf") || !safe_repo_path(&s.filename) {
            continue;
        }
        let Some(quant) = quant_of(&s.filename) else { continue };
        // Shard files: "…-00001-of-00003.gguf" → group by the stem.
        let (group_key, is_shard) = match shard_stem(&s.filename) {
            Some(stem) => (format!("{quant}:{stem}"), true),
            None => (format!("{quant}:{}", s.filename), false),
        };
        let entry = groups.entry(group_key).or_insert_with(|| QuantOption {
            filename: s.filename.clone(),
            quant: quant.clone(),
            size_bytes: 0,
            shards: Vec::new(),
            imatrix: s.filename.to_ascii_lowercase().contains("imatrix"),
            fit: FitLevel::TooBig,
            fit_offload: None,
        });
        entry.size_bytes += s.size.unwrap_or(0);
        if is_shard {
            entry.shards.push(s.filename.clone());
            entry.shards.sort();
            // First shard is the file llama.cpp opens.
            if let Some(first) = entry.shards.first() {
                entry.filename = first.clone();
            }
        }
    }
    let mut out: Vec<QuantOption> = groups.into_values().collect();
    out.sort_by(|a, b| a.size_bytes.cmp(&b.size_bytes));
    out
}

fn shard_stem(filename: &str) -> Option<String> {
    let idx = filename.find("-of-")?;
    let head = &filename[..idx];
    let stem = head.rsplit_once('-').map(|(s, _)| s.to_string())?;
    Some(stem)
}

// ── Hardware fit ─────────────────────────────────────────────────────────

/// KV-cache reserve at the default 8K context, banded by model size
/// (design §2.3 — file-size-band approximation; exact per-layer math needs
/// header fields the sweep doesn't fetch).
fn kv_reserve_bytes(params_b: Option<f64>) -> u64 {
    match params_b {
        Some(p) if p <= 4.0 => 512 * 1024 * 1024,
        Some(p) if p <= 15.0 => 1024 * 1024 * 1024,
        _ => 2 * 1024 * 1024 * 1024,
    }
}

fn fit_of(need: u64, avail_mb: u64) -> FitLevel {
    let usable = (avail_mb as f64 * 1024.0 * 1024.0 * USABLE_FRACTION) as u64;
    if usable == 0 || need > usable {
        FitLevel::TooBig
    } else if (need as f64) <= usable as f64 * COMFORT_FRACTION {
        FitLevel::Comfortable
    } else {
        FitLevel::Tight
    }
}

/// Fill `fit` / `fit_offload` for one quant against this machine.
///
/// Full load competes for the larger memory pool (unified/VRAM machines:
/// `vram_available_mb` already reflects it; pure-CPU: system RAM). MoE
/// offload mode sizes only the shared/attention slice
/// (`size × active/total`) against that pool — experts stream from system
/// RAM (`add_cpu_moe_override`), whose capacity gates via the FULL size.
fn apply_fit(
    q: &mut QuantOption,
    hw: &HardwareInfo,
    moe: bool,
    active_params_b: Option<f64>,
    params_b: Option<f64>,
) {
    let pool_mb = hw.vram_available_mb.max(hw.ram_available_mb);
    let need = q.size_bytes + kv_reserve_bytes(params_b) + RUNTIME_OVERHEAD_BYTES;
    q.fit = fit_of(need, pool_mb);

    if moe {
        if let (Some(active), Some(total)) = (active_params_b, params_b) {
            if total > 0.0 && active > 0.0 && active < total {
                let shared = (q.size_bytes as f64 * (active / total)) as u64;
                let gpu_need = shared + kv_reserve_bytes(params_b) + RUNTIME_OVERHEAD_BYTES;
                // Experts must still FIT in system RAM to be offloaded.
                let ram_ok = fit_of(q.size_bytes, hw.ram_available_mb) != FitLevel::TooBig;
                let gpu_fit = fit_of(gpu_need, pool_mb);
                q.fit_offload = Some(if ram_ok { gpu_fit } else { FitLevel::TooBig });
            }
        }
    }
}

/// Auto-quant policy (design §3.3): the largest quant that still fits
/// comfortably, preferring the standard ladder and imatrix builds. Falls
/// back to the smallest Tight fit; never auto-picks TooBig.
pub fn pick_auto_quant(quants: &[QuantOption]) -> Option<&QuantOption> {
    const LADDER: &[&str] = &[
        "Q8_0", "Q6_K", "Q5_K_M", "Q4_K_M", "IQ4_XS", "Q4_K_S", "Q3_K_M", "IQ3_M",
    ];
    // Best comfortable, walked from the top of the ladder down.
    for pref in LADDER {
        let mut hits: Vec<&QuantOption> = quants
            .iter()
            .filter(|q| q.quant == *pref && q.fit == FitLevel::Comfortable)
            .collect();
        hits.sort_by_key(|q| !q.imatrix); // imatrix builds first
        if let Some(q) = hits.first() {
            return Some(q);
        }
    }
    quants
        .iter()
        .filter(|q| q.fit != FitLevel::TooBig)
        .min_by_key(|q| q.size_bytes)
}

fn refit(mut m: MarketModel, hw: &HardwareInfo) -> MarketModel {
    let (moe, active, total) = (m.moe, m.active_params_b, m.params_b);
    for q in &mut m.quants {
        apply_fit(q, hw, moe, active, total);
    }
    m.recommended = pick_auto_quant(&m.quants).cloned();
    m
}

fn valid_repo_id(repo: &str) -> bool {
    let parts: Vec<&str> = repo.split('/').collect();
    parts.len() == 2
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        })
}

// ── Cache ────────────────────────────────────────────────────────────────

fn cache_path(intent_key: &str, home_dir: &Path) -> std::path::PathBuf {
    home_dir.join("cache").join(format!("hf-market-{intent_key}.json"))
}

async fn load_cache(intent_key: &str, home_dir: &Path) -> Option<Vec<MarketModel>> {
    let content = tokio::fs::read_to_string(cache_path(intent_key, home_dir)).await.ok()?;
    let cache: SweepCache = serde_json::from_str(&content).ok()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if now.saturating_sub(cache.timestamp) > CACHE_TTL_HOURS * 3600 {
        return None;
    }
    Some(cache.results)
}

async fn save_cache(intent_key: &str, results: &[MarketModel], home_dir: &Path) {
    let path = cache_path(intent_key, home_dir);
    if let Some(dir) = path.parent() {
        let _ = tokio::fs::create_dir_all(dir).await;
    }
    let cache = SweepCache {
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        intent: intent_key.to_string(),
        results: results.to_vec(),
    };
    if let Ok(json) = serde_json::to_string(&cache) {
        let _ = tokio::fs::write(&path, json).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hw(vram_mb: u64, ram_mb: u64) -> HardwareInfo {
        HardwareInfo {
            gpu_type: crate::types::GpuType::AppleSilicon,
            gpu_name: "test".into(),
            vram_total_mb: vram_mb,
            vram_available_mb: vram_mb,
            ram_total_mb: ram_mb,
            ram_available_mb: ram_mb,
            cpu_cores: 8,
            recommended_backend: crate::types::BackendType::LlamaCpp,
            recommended_max_model_gb: 8.0,
        }
    }

    fn sib(name: &str, size: u64) -> HfSibling {
        HfSibling { filename: name.into(), size: Some(size) }
    }

    const GB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn moe_detection_from_arch_and_name() {
        assert_eq!(detect_moe(Some("qwen3moe"), "Qwen3-30B-A3B"), (true, Some(3.0)));
        assert_eq!(detect_moe(None, "Qwen3-30B-A3B"), (true, Some(3.0)));
        assert_eq!(detect_moe(Some("llama"), "Llama-3.1-8B-Instruct"), (false, None));
        // Total params must skip the active token.
        assert_eq!(parse_params_b("Qwen3-30B-A3B"), Some(30.0));
    }

    #[test]
    fn quant_enumeration_groups_shards_and_sorts() {
        let quants = enumerate_quants(vec![
            sib("model-Q4_K_M-00002-of-00002.gguf", 2 * GB),
            sib("model-Q4_K_M-00001-of-00002.gguf", 3 * GB),
            sib("model-Q8_0.gguf", 9 * GB),
            sib("model-IQ4_XS-imatrix.gguf", 4 * GB),
            sib("README.md", 1),
            sib("../evil-Q4_K_M.gguf", GB),
        ]);
        assert_eq!(quants.len(), 3);
        // Sorted by size; the shard group summed to 5GB with first shard as entry.
        let q4 = quants.iter().find(|q| q.quant == "Q4_K_M").unwrap();
        assert_eq!(q4.size_bytes, 5 * GB);
        assert_eq!(q4.shards.len(), 2);
        assert!(q4.filename.ends_with("00001-of-00002.gguf"));
        assert!(quants.iter().any(|q| q.quant == "IQ4_XS" && q.imatrix));
    }

    #[test]
    fn fit_tristate_and_auto_quant() {
        let hw16 = hw(16 * 1024, 16 * 1024);
        let mut quants = enumerate_quants(vec![
            sib("m-Q8_0.gguf", 30 * GB),
            sib("m-Q5_K_M.gguf", 11 * GB),
            sib("m-Q4_K_M.gguf", 5 * GB),
        ]);
        for q in &mut quants {
            apply_fit(q, &hw16, false, None, Some(8.0));
        }
        let by = |name: &str| quants.iter().find(|q| q.quant == name).unwrap();
        assert_eq!(by("Q8_0").fit, FitLevel::TooBig);
        assert_eq!(by("Q5_K_M").fit, FitLevel::Tight);
        assert_eq!(by("Q4_K_M").fit, FitLevel::Comfortable);
        // Auto pick: best comfortable on the ladder, never TooBig.
        assert_eq!(pick_auto_quant(&quants).unwrap().quant, "Q4_K_M");
    }

    #[test]
    fn moe_offload_unlocks_bigger_models() {
        // 18GB Q4 of a 30B-A3B on a 16GB machine: full load TooBig, but the
        // active slice (~1.8GB) + KV + overhead fits comfortably.
        let hw16 = hw(16 * 1024, 16 * 1024);
        let mut q = QuantOption {
            filename: "m-Q4_K_M-00001-of-00002.gguf".into(),
            quant: "Q4_K_M".into(),
            size_bytes: 12 * GB,
            shards: vec![],
            imatrix: false,
            fit: FitLevel::TooBig,
            fit_offload: None,
        };
        apply_fit(&mut q, &hw16, true, Some(3.0), Some(30.0));
        assert_eq!(q.fit, FitLevel::TooBig);
        assert_eq!(q.fit_offload, Some(FitLevel::Comfortable));

        // Experts that don't even fit in system RAM stay TooBig.
        let hw8 = hw(8 * 1024, 8 * 1024);
        apply_fit(&mut q, &hw8, true, Some(3.0), Some(30.0));
        assert_eq!(q.fit_offload, Some(FitLevel::TooBig));
    }

    #[test]
    fn dedup_and_intent_filters() {
        assert_eq!(base_identity("unsloth/Qwen3-8B-GGUF"), "qwen3-8b");
        assert_eq!(base_identity("bartowski/Qwen3-8B-GGUF"), "qwen3-8b");
        assert!(publisher_rank("unsloth/x") < publisher_rank("bartowski/x"));
        let m = |id: &str, tags: &[&str]| HfListModel {
            model_id: id.into(),
            downloads: Some(1),
            tags: Some(tags.iter().map(|s| s.to_string()).collect()),
        };
        assert!(intent_matches_listing(Intent::Code, &m("unsloth/Qwen3-Coder-GGUF", &[])));
        assert!(!intent_matches_listing(Intent::Code, &m("unsloth/Llama-8B-GGUF", &[])));
        assert!(intent_matches_listing(Intent::Chinese, &m("bartowski/GLM-4-GGUF", &[])));
        assert!(intent_matches_listing(Intent::Chinese, &m("x/some-model", &["zh"])));
        assert!(!intent_matches_listing(Intent::Chat, &m("x/model-base-GGUF", &[])));
        assert!(Intent::from_str_loose("long_context").is_some());
        assert!(Intent::from_str_loose("nope").is_none());
    }

    #[test]
    fn repo_id_validation() {
        assert!(valid_repo_id("unsloth/Qwen3-8B-GGUF"));
        assert!(!valid_repo_id("unsloth"));
        assert!(!valid_repo_id("a/b/c"));
        assert!(!valid_repo_id("un$loth/x"));
    }
}
