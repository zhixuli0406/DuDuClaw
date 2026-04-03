//! Model manager — scan, load, unload GGUF models.

use std::collections::HashMap;
use std::path::PathBuf;

use tokio::sync::RwLock;
use tracing::info;

use crate::error::{InferenceError, Result};
use crate::types::ModelInfo;

/// Manages available and loaded models.
pub struct ModelManager {
    models_dir: PathBuf,
    /// Cached model metadata (id → info)
    available: RwLock<HashMap<String, ModelInfo>>,
    /// Currently loaded model id
    loaded_model_id: RwLock<Option<String>>,
}

impl ModelManager {
    pub fn new(models_dir: PathBuf) -> Self {
        Self {
            models_dir,
            available: RwLock::new(HashMap::new()),
            loaded_model_id: RwLock::new(None),
        }
    }

    /// Scan the models directory for GGUF files and populate metadata.
    pub async fn scan(&self) -> Result<Vec<ModelInfo>> {
        let dir = &self.models_dir;
        if !tokio::fs::try_exists(dir).await.unwrap_or(false) {
            tokio::fs::create_dir_all(dir).await?;
            return Ok(Vec::new());
        }

        let mut models = Vec::new();
        let mut entries = tokio::fs::read_dir(dir).await?;

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("gguf") {
                continue;
            }

            let metadata = entry.metadata().await?;
            let file_size = metadata.len();
            let filename = path.file_stem().unwrap_or_default().to_string_lossy().to_string();

            let param_count = extract_param_count(&filename);
            let context_length: u32 = 4096; // default, refreshed on load
            let kv_cache_mb = ModelInfo::estimate_kv_cache_mb(&param_count, context_length);
            let info = ModelInfo {
                id: filename.clone(),
                path: path.to_string_lossy().to_string(),
                architecture: extract_architecture(&filename),
                parameter_count: param_count,
                quantization: extract_quantization(&filename),
                file_size_bytes: file_size,
                estimated_memory_mb: file_size / (1024 * 1024) * 11 / 10,
                kv_cache_mb,
                is_loaded: false,
                context_length,
            };

            models.push(info);
        }

        // Update cache
        let mut cache = self.available.write().await;
        cache.clear();
        for model in &models {
            cache.insert(model.id.clone(), model.clone());
        }

        info!(count = models.len(), dir = %dir.display(), "Scanned models");
        Ok(models)
    }

    /// List all available models.
    pub async fn list(&self) -> Vec<ModelInfo> {
        let cache = self.available.read().await;
        if cache.is_empty() {
            drop(cache);
            self.scan().await.unwrap_or_default()
        } else {
            let loaded_id = self.loaded_model_id.read().await.clone();
            cache.values().map(|m| {
                let mut info = m.clone();
                info.is_loaded = Some(&info.id) == loaded_id.as_ref();
                info
            }).collect()
        }
    }

    /// Get info for a specific model by id.
    pub async fn get(&self, model_id: &str) -> Option<ModelInfo> {
        let cache = self.available.read().await;
        cache.get(model_id).cloned()
    }

    /// Resolve a model id to its full path (confined to models_dir).
    pub async fn resolve_path(&self, model_id: &str) -> Result<PathBuf> {
        // Reject path traversal attempts
        if model_id.contains('/') || model_id.contains('\\') || model_id.contains("..") {
            return Err(InferenceError::ModelNotFound {
                path: model_id.to_string(),
            });
        }

        // Only resolve within models_dir
        let with_ext = self.models_dir.join(format!("{model_id}.gguf"));
        if tokio::fs::try_exists(&with_ext).await.unwrap_or(false) {
            return Ok(with_ext);
        }

        let exact = self.models_dir.join(model_id);
        if tokio::fs::try_exists(&exact).await.unwrap_or(false) {
            return Ok(exact);
        }

        Err(InferenceError::ModelNotFound {
            path: model_id.to_string(),
        })
    }

    /// Mark a model as loaded and refresh its context_length / KV cache estimate.
    pub async fn set_loaded(&self, model_id: &str, context_size: u32) {
        *self.loaded_model_id.write().await = Some(model_id.to_string());
        let mut cache = self.available.write().await;
        if let Some(info) = cache.get_mut(model_id) {
            info.context_length = context_size;
            info.kv_cache_mb = ModelInfo::estimate_kv_cache_mb(&info.parameter_count, context_size);
        }
    }

    /// Mark no model as loaded.
    pub async fn set_unloaded(&self) {
        *self.loaded_model_id.write().await = None;
    }

    /// Get the currently loaded model id.
    pub async fn loaded_model_id(&self) -> Option<String> {
        self.loaded_model_id.read().await.clone()
    }
}

/// Extract architecture from filename heuristics.
fn extract_architecture(filename: &str) -> String {
    let lower = filename.to_lowercase();
    if lower.contains("llama") {
        "llama".to_string()
    } else if lower.contains("qwen") {
        "qwen2".to_string()
    } else if lower.contains("gemma") {
        "gemma".to_string()
    } else if lower.contains("phi") {
        "phi".to_string()
    } else if lower.contains("mistral") {
        "mistral".to_string()
    } else if lower.contains("deepseek") {
        "deepseek".to_string()
    } else {
        "unknown".to_string()
    }
}

/// Extract parameter count from a model id (filename without extension).
/// Public so backends can reuse filename-based heuristics.
pub fn extract_param_count_from_id(model_id: &str) -> String {
    extract_param_count(model_id)
}

/// Extract parameter count from filename.
/// Handles patterns like "8b", "70b", "0.6b", "1.7b" in filenames
/// separated by hyphens or underscores (but not dots, to preserve decimals).
fn extract_param_count(filename: &str) -> String {
    let lower = filename.to_lowercase();
    // Split on `-` and `_` only (not `.`) to preserve decimals like "0.6b"
    for part in lower.split(&['-', '_'][..]) {
        if part.ends_with('b') {
            let num = &part[..part.len() - 1];
            if num.parse::<f64>().is_ok() {
                return part.to_uppercase();
            }
        }
    }
    "unknown".to_string()
}

/// Extract quantization type from filename.
fn extract_quantization(filename: &str) -> String {
    let upper = filename.to_uppercase();
    let quant_patterns = [
        "Q2_K", "Q3_K_S", "Q3_K_M", "Q3_K_L",
        "Q4_0", "Q4_1", "Q4_K_S", "Q4_K_M", "Q4_K_L",
        "Q5_0", "Q5_1", "Q5_K_S", "Q5_K_M",
        "Q6_K", "Q8_0", "F16", "F32",
        "IQ1_S", "IQ2_XXS", "IQ2_XS", "IQ2_S", "IQ2_M",
        "IQ3_XXS", "IQ3_XS", "IQ3_S", "IQ4_XS", "IQ4_NL",
    ];

    for pattern in quant_patterns {
        if upper.contains(pattern) {
            return pattern.to_string();
        }
    }
    "unknown".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ModelInfo;

    #[test]
    fn extract_architecture_known() {
        assert_eq!(extract_architecture("Llama-3-8B-Instruct-Q4_K_M"), "llama");
        assert_eq!(extract_architecture("qwen3-8b-q4_k_m"), "qwen2");
        assert_eq!(extract_architecture("gemma-2-2b-q8_0"), "gemma");
        assert_eq!(extract_architecture("deepseek-v3-lite"), "deepseek");
    }

    #[test]
    fn extract_architecture_unknown() {
        assert_eq!(extract_architecture("some-random-model"), "unknown");
    }

    #[test]
    fn extract_param_count_various() {
        assert_eq!(extract_param_count("Llama-3-8B-Instruct-Q4_K_M"), "8B");
        assert_eq!(extract_param_count("qwen3-0.6b-q8_0"), "0.6B");
        assert_eq!(extract_param_count("llama-3-70b-Q4_K_M"), "70B");
        assert_eq!(extract_param_count("phi-3-mini-4k-instruct"), "unknown");
    }

    #[test]
    fn extract_quantization_various() {
        assert_eq!(extract_quantization("llama-3-70b-Q4_K_M"), "Q4_K_M");
        assert_eq!(extract_quantization("qwen3-8b-Q8_0"), "Q8_0");
        assert_eq!(extract_quantization("model-F16"), "F16");
        assert_eq!(extract_quantization("model-IQ4_NL"), "IQ4_NL");
        assert_eq!(extract_quantization("no-quant-info"), "unknown");
    }

    #[test]
    fn kv_cache_known_params() {
        // 8B model @ 4096 ctx → ~128 KB/token → 512 MB
        let kv = ModelInfo::estimate_kv_cache_mb("8B", 4096);
        assert_eq!(kv, 512);

        // 0.6B model @ 4096 ctx → ~24 KB/token → 96 MB
        let kv = ModelInfo::estimate_kv_cache_mb("0.6B", 4096);
        assert_eq!(kv, 96);

        // 70B model @ 4096 ctx → ~320 KB/token → 1280 MB
        let kv = ModelInfo::estimate_kv_cache_mb("70B", 4096);
        assert_eq!(kv, 1280);
    }

    #[test]
    fn kv_cache_unknown_params_returns_zero() {
        assert_eq!(ModelInfo::estimate_kv_cache_mb("unknown", 4096), 0);
        assert_eq!(ModelInfo::estimate_kv_cache_mb("auto", 4096), 0);
        assert_eq!(ModelInfo::estimate_kv_cache_mb("", 4096), 0);
    }

    #[test]
    fn kv_cache_zero_context_returns_zero() {
        assert_eq!(ModelInfo::estimate_kv_cache_mb("8B", 0), 0);
    }

    #[test]
    fn kv_cache_smaller_context_reduces_proportionally() {
        let full = ModelInfo::estimate_kv_cache_mb("8B", 4096);
        let half = ModelInfo::estimate_kv_cache_mb("8B", 2048);
        assert_eq!(full, half * 2);
    }
}
