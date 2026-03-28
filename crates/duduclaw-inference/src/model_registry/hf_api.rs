//! HuggingFace API client for model search and file listing.

use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use tracing::{info, warn};

use super::curated::is_trusted;
use super::{ModelTier, RegistryEntry};

const HF_API_BASE: &str = "https://huggingface.co/api";
const CACHE_FILE: &str = "cache/hf-search-cache.json";
const CACHE_TTL_HOURS: u64 = 24;

/// HuggingFace model search response (partial).
#[derive(Debug, Deserialize)]
struct HfModel {
    #[serde(rename = "modelId")]
    model_id: String,
    downloads: Option<u64>,
    tags: Option<Vec<String>>,
    siblings: Option<Vec<HfSibling>>,
}

/// A file within a HF repo.
#[derive(Debug, Deserialize)]
struct HfSibling {
    #[serde(rename = "rfilename")]
    filename: String,
    size: Option<u64>,
}

/// Cached search results.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct SearchCache {
    timestamp: u64,
    query: String,
    results: Vec<RegistryEntry>,
}

/// Search HuggingFace for GGUF models.
///
/// Returns curated entries first (marked [推薦]), then community entries ([社群]).
pub async fn search_models(
    query: &str,
    available_ram_mb: u64,
    home_dir: &Path,
) -> Vec<RegistryEntry> {
    // Check cache first
    if let Some(cached) = load_cache(query, home_dir).await {
        info!(query, count = cached.len(), "Using cached HF search results");
        return filter_and_sort(cached, available_ram_mb);
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_default();

    let hf_token = std::env::var("HF_TOKEN").unwrap_or_default();

    // Search for GGUF models
    let search_query = if query.to_lowercase().contains("gguf") {
        query.to_string()
    } else {
        format!("{query} gguf")
    };

    let url = format!(
        "{HF_API_BASE}/models?search={}&filter=gguf&sort=downloads&direction=-1&limit=20",
        urlencoding::encode(&search_query)
    );

    let mut req = client.get(&url);
    if !hf_token.is_empty() {
        req = req.bearer_auth(&hf_token);
    }

    let results = match req.send().await {
        Ok(resp) if resp.status().is_success() => {
            match resp.json::<Vec<HfModel>>().await {
                Ok(models) => convert_hf_models(models),
                Err(e) => {
                    warn!(error = %e, "Failed to parse HF search response");
                    Vec::new()
                }
            }
        }
        Ok(resp) => {
            warn!(status = %resp.status(), "HF API error");
            Vec::new()
        }
        Err(e) => {
            warn!(error = %e, "HF API unreachable");
            Vec::new()
        }
    };

    // Cache results
    if !results.is_empty() {
        save_cache(query, &results, home_dir).await;
    }

    info!(query, count = results.len(), "HF search completed");
    filter_and_sort(results, available_ram_mb)
}

/// Convert HF API response models to RegistryEntry list.
fn convert_hf_models(models: Vec<HfModel>) -> Vec<RegistryEntry> {
    let mut entries = Vec::new();

    for model in models {
        let siblings = model.siblings.unwrap_or_default();
        let gguf_files: Vec<&HfSibling> = siblings
            .iter()
            .filter(|s| s.filename.ends_with(".gguf"))
            .collect();

        if gguf_files.is_empty() {
            continue;
        }

        // Pick the best GGUF file (prefer Q4_K_M)
        let best = pick_best_quantization(&gguf_files);
        let Some(file) = best else { continue };

        let repo = &model.model_id;
        let tier = if is_trusted(repo) {
            ModelTier::Recommended
        } else {
            ModelTier::Community
        };

        let tags = model.tags.unwrap_or_default();
        let name = repo.split('/').last().unwrap_or(repo)
            .replace("-GGUF", "")
            .replace("-gguf", "");

        let size = file.size.unwrap_or(0);
        let quant = extract_quantization(&file.filename);
        let params = extract_params(&name);

        entries.push(RegistryEntry {
            name: name.clone(),
            repo: repo.clone(),
            filename: file.filename.clone(),
            size_bytes: size,
            quantization: quant,
            params: params.clone(),
            languages: guess_languages(&tags),
            tags: tags.iter().filter(|t| !t.starts_with("license:")).cloned().collect(),
            min_ram_mb: estimate_min_ram(size),
            description: format!("{name} ({params})"),
            tier,
            downloads: model.downloads.unwrap_or(0),
        });
    }

    entries
}

/// Pick the best quantization from available GGUF files.
///
/// Preference order: Q4_K_M > Q4_K_S > Q5_K_M > Q4_0 > Q8_0 > others
fn pick_best_quantization<'a>(files: &[&'a HfSibling]) -> Option<&'a HfSibling> {
    let preference = ["Q4_K_M", "q4_k_m", "Q4_K_S", "q4_k_s", "Q5_K_M", "q5_k_m", "Q4_0", "q4_0", "Q8_0", "q8_0"];

    for pref in preference {
        if let Some(f) = files.iter().find(|f| f.filename.contains(pref)) {
            return Some(f);
        }
    }
    // Fallback to first GGUF
    files.first().copied()
}

/// Extract quantization type from filename.
fn extract_quantization(filename: &str) -> String {
    let upper = filename.to_uppercase();
    let patterns = [
        "Q4_K_M", "Q4_K_S", "Q4_K_L", "Q4_0", "Q4_1",
        "Q5_K_M", "Q5_K_S", "Q5_0", "Q5_1",
        "Q3_K_M", "Q3_K_S", "Q3_K_L",
        "Q6_K", "Q8_0", "Q2_K", "F16", "F32",
        "IQ4_XS", "IQ4_NL", "IQ3_XS", "IQ3_XXS",
    ];
    for p in patterns {
        if upper.contains(p) {
            return p.to_string();
        }
    }
    "unknown".to_string()
}

/// Extract parameter count from model name.
fn extract_params(name: &str) -> String {
    let lower = name.to_lowercase();
    for part in lower.split(&['-', '_', '.'][..]) {
        if part.ends_with('b') {
            let num = &part[..part.len() - 1];
            if num.parse::<f64>().is_ok() {
                return part.to_uppercase();
            }
        }
    }
    "?B".to_string()
}

/// Guess supported languages from HF tags.
fn guess_languages(tags: &[String]) -> Vec<String> {
    let mut langs = Vec::new();
    for tag in tags {
        match tag.as_str() {
            "en" | "zh" | "ja" | "ko" | "de" | "fr" | "es" | "multilingual" => {
                langs.push(tag.clone());
            }
            _ => {}
        }
    }
    if langs.is_empty() {
        langs.push("en".to_string());
    }
    langs
}

/// Estimate minimum RAM from file size (file size + ~20% overhead).
fn estimate_min_ram(size_bytes: u64) -> u64 {
    (size_bytes / (1024 * 1024)) * 12 / 10
}

/// Filter by RAM and sort: recommended first, then by downloads.
fn filter_and_sort(mut entries: Vec<RegistryEntry>, available_ram_mb: u64) -> Vec<RegistryEntry> {
    entries.retain(|e| e.min_ram_mb <= available_ram_mb || available_ram_mb == 0);
    entries.sort_by(|a, b| {
        // Recommended before Community
        let tier_cmp = (a.tier as u8).cmp(&(b.tier as u8));
        if tier_cmp != std::cmp::Ordering::Equal {
            return tier_cmp;
        }
        // Then by downloads descending
        b.downloads.cmp(&a.downloads)
    });
    entries
}

/// Load cached search results if still valid.
async fn load_cache(query: &str, home_dir: &Path) -> Option<Vec<RegistryEntry>> {
    let cache_path = home_dir.join(CACHE_FILE);
    let content = tokio::fs::read_to_string(&cache_path).await.ok()?;
    let cache: SearchCache = serde_json::from_str(&content).ok()?;

    if cache.query != query {
        return None;
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    if now - cache.timestamp > CACHE_TTL_HOURS * 3600 {
        return None;
    }

    Some(cache.results)
}

/// Save search results to cache.
async fn save_cache(query: &str, results: &[RegistryEntry], home_dir: &Path) {
    let cache_dir = home_dir.join("cache");
    let _ = tokio::fs::create_dir_all(&cache_dir).await;

    let cache = SearchCache {
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        query: query.to_string(),
        results: results.to_vec(),
    };

    if let Ok(json) = serde_json::to_string_pretty(&cache) {
        let _ = tokio::fs::write(cache_dir.join("hf-search-cache.json"), json).await;
    }
}
