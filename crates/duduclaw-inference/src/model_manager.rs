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

            let info = ModelInfo {
                id: filename.clone(),
                path: path.to_string_lossy().to_string(),
                architecture: extract_architecture(&filename),
                parameter_count: extract_param_count(&filename),
                quantization: extract_quantization(&filename),
                file_size_bytes: file_size,
                estimated_memory_mb: file_size / (1024 * 1024) * 11 / 10,
                is_loaded: false,
                context_length: 4096, // default, updated on load
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

    /// Mark a model as loaded.
    pub async fn set_loaded(&self, model_id: &str) {
        *self.loaded_model_id.write().await = Some(model_id.to_string());
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

/// Extract parameter count from filename.
fn extract_param_count(filename: &str) -> String {
    let lower = filename.to_lowercase();
    // Look for patterns like "8b", "70b", "3b", "0.6b"
    for part in lower.split(&['-', '_', '.'][..]) {
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
