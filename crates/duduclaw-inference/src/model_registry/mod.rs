//! Model registry — curated recommendations + HuggingFace search + auto-download.
//!
//! Two tiers of model discovery:
//! - **Curated** (`[推薦]`): verified repos from trusted uploaders, tested quantizations
//! - **Community** (`[社群]`): live HF search results, unverified
//!
//! Hardware-aware filtering ensures only models that fit in available RAM are shown.

pub mod curated;
pub mod downloader;
pub mod hf_api;

use serde::{Deserialize, Serialize};

/// A model entry displayed to the user during selection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryEntry {
    /// Display name (e.g., "Qwen3-8B")
    pub name: String,
    /// HuggingFace repo id (e.g., "Qwen/Qwen3-8B-GGUF")
    pub repo: String,
    /// GGUF filename within the repo (first shard if split model)
    pub filename: String,
    /// File size in bytes (total across all shards)
    pub size_bytes: u64,
    /// Quantization type (e.g., "Q4_K_M")
    pub quantization: String,
    /// Parameter count (e.g., "8B")
    pub params: String,
    /// Supported languages
    pub languages: Vec<String>,
    /// Use-case tags (e.g., "chat", "code", "reasoning")
    pub tags: Vec<String>,
    /// Minimum RAM in MB to run this model
    pub min_ram_mb: u64,
    /// Short description
    pub description: String,
    /// Trust tier
    pub tier: ModelTier,
    /// HF download count (for sorting)
    pub downloads: u64,
    /// Split GGUF shard paths relative to repo root.
    /// Empty = single file model. Non-empty = multi-shard model.
    /// Example: `["Q4_K_M/Model-Q4_K_M-00001-of-00003.gguf", ...]`
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shards: Vec<String>,
}

/// Trust tier for a model entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    /// Verified by DuDuClaw team — safe and tested
    Recommended,
    /// From HF search — unverified, use at own risk
    Community,
}

impl std::fmt::Display for ModelTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Recommended => write!(f, "推薦"),
            Self::Community => write!(f, "社群"),
        }
    }
}

impl RegistryEntry {
    /// Format file size for display.
    pub fn size_display(&self) -> String {
        let gb = self.size_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
        if gb >= 1.0 {
            format!("{:.1} GB", gb)
        } else {
            let mb = self.size_bytes as f64 / (1024.0 * 1024.0);
            format!("{:.0} MB", mb)
        }
    }

    /// Model id for inference.toml (filename without .gguf and shard suffix).
    pub fn model_id(&self) -> String {
        let base = self.filename.trim_end_matches(".gguf");
        // Strip split-model suffix like "-00001-of-00004"
        if let Some(idx) = base.rfind("-00001-of-") {
            base[..idx].to_string()
        } else {
            base.to_string()
        }
    }

    /// Whether this is a multi-shard (split) GGUF model.
    pub fn is_split(&self) -> bool {
        !self.shards.is_empty()
    }

    /// Full HF download URL (for single-file models).
    pub fn download_url(&self) -> String {
        format!(
            "https://huggingface.co/{}/resolve/main/{}",
            self.repo, self.filename
        )
    }

    /// Mirror download URL (hf-mirror.com for China).
    pub fn mirror_url(&self) -> String {
        format!(
            "https://hf-mirror.com/{}/resolve/main/{}",
            self.repo, self.filename
        )
    }

    /// All shard download URLs with (hf_url, mirror_url, local_filename).
    /// For single-file models, returns one entry.
    pub fn shard_urls(&self) -> Vec<(String, String, String)> {
        if self.shards.is_empty() {
            return vec![(self.download_url(), self.mirror_url(), self.filename.clone())];
        }
        self.shards.iter().map(|shard_path| {
            let local_name = shard_path.rsplit('/').next().unwrap_or(shard_path).to_string();
            let url = format!("https://huggingface.co/{}/resolve/main/{}", self.repo, shard_path);
            let mirror = format!("https://hf-mirror.com/{}/resolve/main/{}", self.repo, shard_path);
            (url, mirror, local_name)
        }).collect()
    }
}
