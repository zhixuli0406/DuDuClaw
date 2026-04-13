//! Curated model registry — verified models from trusted uploaders.
//!
//! The curated list serves as:
//! 1. Primary recommendations during onboarding
//! 2. Fallback when HF API is unreachable
//! 3. Trust anchor — only these repos are marked [推薦]

use super::{ModelTier, RegistryEntry};

/// Trusted HuggingFace organizations/users whose models are marked [推薦].
pub const TRUSTED_UPLOADERS: &[&str] = &[
    "Qwen",
    "google",
    "meta-llama",
    "microsoft",
    "bartowski",
    "mlx-community",
    "TheBloke",
    "lmstudio-community",
    "unsloth",
    "deepseek-ai",
    "mistralai",
    "MiniMaxAI",
];

/// Check if a HF repo belongs to a trusted uploader.
pub fn is_trusted(repo: &str) -> bool {
    let owner = repo.split('/').next().unwrap_or("");
    // H-S2: exact case-sensitive match (HF org names are case-sensitive)
    TRUSTED_UPLOADERS.contains(&owner)
}

/// Built-in curated model list — fallback when network is unavailable.
///
/// Sorted by general usefulness. Each entry is a known-good GGUF from a trusted repo.
pub fn builtin_registry() -> Vec<RegistryEntry> {
    vec![
        RegistryEntry {
            name: "Qwen3-8B".to_string(),
            repo: "Qwen/Qwen3-8B-GGUF".to_string(),
            filename: "Qwen3-8B-Q4_K_M.gguf".to_string(),
            size_bytes: 5_027_783_488,
            quantization: "Q4_K_M".to_string(),
            params: "8B".to_string(),
            languages: vec!["en".into(), "zh".into(), "ja".into(), "ko".into()],
            tags: vec!["chat".into(), "code".into(), "reasoning".into()],
            min_ram_mb: 6_000,
            description: "通用對話與程式碼，多語言支援".to_string(),
            tier: ModelTier::Recommended,
            downloads: 0,
            shards: vec![],
        },
        RegistryEntry {
            name: "Gemma-3-4B".to_string(),
            repo: "bartowski/google_gemma-3-4b-it-GGUF".to_string(),
            filename: "google_gemma-3-4b-it-Q4_K_M.gguf".to_string(),
            size_bytes: 2_489_758_112,
            quantization: "Q4_K_M".to_string(),
            params: "4B".to_string(),
            languages: vec!["en".into(), "zh".into()],
            tags: vec!["chat".into(), "fast".into()],
            min_ram_mb: 3_500,
            description: "輕量快速，適合簡單任務與分類".to_string(),
            tier: ModelTier::Recommended,
            downloads: 0,
            shards: vec![],
        },
        // ── Gemma 4 ──────────────────────────────────────
        RegistryEntry {
            name: "Gemma-4-E2B".to_string(),
            repo: "unsloth/gemma-4-E2B-it-GGUF".to_string(),
            filename: "gemma-4-E2B-it-Q4_K_M.gguf".to_string(),
            size_bytes: 3_340_262_400,
            quantization: "Q4_K_M".to_string(),
            params: "2B".to_string(),
            languages: vec!["en".into(), "zh".into()],
            tags: vec!["chat".into(), "fast".into(), "low-ram".into()],
            min_ram_mb: 3_000,
            description: "Gemma 4 極輕量，適合低 RAM 設備".to_string(),
            tier: ModelTier::Recommended,
            downloads: 0,
            shards: vec![],
        },
        RegistryEntry {
            name: "Gemma-4-E4B".to_string(),
            repo: "unsloth/gemma-4-E4B-it-GGUF".to_string(),
            filename: "gemma-4-E4B-it-Q4_K_M.gguf".to_string(),
            size_bytes: 5_350_706_585,
            quantization: "Q4_K_M".to_string(),
            params: "4B".to_string(),
            languages: vec!["en".into(), "zh".into()],
            tags: vec!["chat".into(), "code".into(), "fast".into()],
            min_ram_mb: 5_000,
            description: "Gemma 4 輕量版，通用對話與程式碼".to_string(),
            tier: ModelTier::Recommended,
            downloads: 0,
            shards: vec![],
        },
        RegistryEntry {
            name: "Gemma-4-26B-A4B".to_string(),
            repo: "unsloth/gemma-4-26B-A4B-it-GGUF".to_string(),
            filename: "gemma-4-26B-A4B-it-UD-Q4_K_M.gguf".to_string(),
            size_bytes: 18_140_873_830,
            quantization: "Q4_K_M".to_string(),
            params: "26B".to_string(),
            languages: vec!["en".into(), "zh".into(), "ja".into(), "ko".into()],
            tags: vec!["chat".into(), "code".into(), "reasoning".into(), "moe".into()],
            min_ram_mb: 20_000,
            description: "Gemma 4 MoE（26B 總參數/4B 活躍），高品質推理".to_string(),
            tier: ModelTier::Recommended,
            downloads: 0,
            shards: vec![],
        },
        RegistryEntry {
            name: "Qwen3-1.7B".to_string(),
            repo: "Qwen/Qwen3-1.7B-GGUF".to_string(),
            filename: "Qwen3-1.7B-Q8_0.gguf".to_string(),
            size_bytes: 1_834_426_016,
            quantization: "Q8_0".to_string(),
            params: "1.7B".to_string(),
            languages: vec!["en".into(), "zh".into()],
            tags: vec!["chat".into(), "fast".into(), "low-ram".into()],
            min_ram_mb: 2_000,
            description: "極輕量，適合 RAM 不足的設備".to_string(),
            tier: ModelTier::Recommended,
            downloads: 0,
            shards: vec![],
        },
        RegistryEntry {
            name: "Qwen3-14B".to_string(),
            repo: "Qwen/Qwen3-14B-GGUF".to_string(),
            filename: "Qwen3-14B-Q4_K_M.gguf".to_string(),
            size_bytes: 9_001_752_960,
            quantization: "Q4_K_M".to_string(),
            params: "14B".to_string(),
            languages: vec!["en".into(), "zh".into(), "ja".into(), "ko".into()],
            tags: vec!["chat".into(), "code".into(), "reasoning".into()],
            min_ram_mb: 10_000,
            description: "高品質推理與程式碼，需 10GB+ RAM".to_string(),
            tier: ModelTier::Recommended,
            downloads: 0,
            shards: vec![],
        },
        RegistryEntry {
            name: "Qwen3-32B".to_string(),
            repo: "Qwen/Qwen3-32B-GGUF".to_string(),
            filename: "Qwen3-32B-Q4_K_M.gguf".to_string(),
            size_bytes: 19_762_149_024,
            quantization: "Q4_K_M".to_string(),
            params: "32B".to_string(),
            languages: vec!["en".into(), "zh".into(), "ja".into(), "ko".into()],
            tags: vec!["chat".into(), "code".into(), "reasoning".into(), "advanced".into()],
            min_ram_mb: 22_000,
            description: "接近 Claude 等級的推理能力，需 22GB+ RAM".to_string(),
            tier: ModelTier::Recommended,
            downloads: 0,
            shards: vec![],
        },
        // ── MiniMax (229B MoE, split GGUF) ───────────────
        RegistryEntry {
            name: "MiniMax-M2.1".to_string(),
            repo: "unsloth/MiniMax-M2.1-GGUF".to_string(),
            filename: "MiniMax-M2.1-Q4_K_M-00001-of-00003.gguf".to_string(),
            size_bytes: 148_520_437_760, // ~138.3 GB total
            quantization: "Q4_K_M".to_string(),
            params: "229B".to_string(),
            languages: vec!["en".into(), "zh".into()],
            tags: vec!["chat".into(), "code".into(), "reasoning".into(), "moe".into()],
            min_ram_mb: 160_000,
            description: "MiniMax M2.1 MoE 229B，需大量 RAM 或分散式推理".to_string(),
            tier: ModelTier::Recommended,
            downloads: 0,
            shards: vec![
                "Q4_K_M/MiniMax-M2.1-Q4_K_M-00001-of-00003.gguf".into(),
                "Q4_K_M/MiniMax-M2.1-Q4_K_M-00002-of-00003.gguf".into(),
                "Q4_K_M/MiniMax-M2.1-Q4_K_M-00003-of-00003.gguf".into(),
            ],
        },
        RegistryEntry {
            name: "MiniMax-M2.5".to_string(),
            repo: "unsloth/MiniMax-M2.5-GGUF".to_string(),
            filename: "MiniMax-M2.5-Q4_K_M-00001-of-00004.gguf".to_string(),
            size_bytes: 148_198_564_864, // ~138 GB total
            quantization: "Q4_K_M".to_string(),
            params: "229B".to_string(),
            languages: vec!["en".into(), "zh".into()],
            tags: vec!["chat".into(), "code".into(), "reasoning".into(), "moe".into()],
            min_ram_mb: 160_000,
            description: "MiniMax M2.5 MoE 229B，程式碼與長程規劃".to_string(),
            tier: ModelTier::Recommended,
            downloads: 0,
            shards: vec![
                "Q4_K_M/MiniMax-M2.5-Q4_K_M-00001-of-00004.gguf".into(),
                "Q4_K_M/MiniMax-M2.5-Q4_K_M-00002-of-00004.gguf".into(),
                "Q4_K_M/MiniMax-M2.5-Q4_K_M-00003-of-00004.gguf".into(),
                "Q4_K_M/MiniMax-M2.5-Q4_K_M-00004-of-00004.gguf".into(),
            ],
        },
        RegistryEntry {
            name: "MiniMax-M2.7".to_string(),
            repo: "unsloth/MiniMax-M2.7-GGUF".to_string(),
            filename: "MiniMax-M2.7-UD-Q4_K_M-00001-of-00004.gguf".to_string(),
            size_bytes: 150_323_855_360, // ~140 GB total
            quantization: "Q4_K_M".to_string(),
            params: "229B".to_string(),
            languages: vec!["en".into(), "zh".into()],
            tags: vec!["chat".into(), "code".into(), "reasoning".into(), "moe".into(), "advanced".into()],
            min_ram_mb: 160_000,
            description: "MiniMax M2.7 MoE 229B 旗艦模型，最強推理能力".to_string(),
            tier: ModelTier::Recommended,
            downloads: 0,
            shards: vec![
                "UD-Q4_K_M/MiniMax-M2.7-UD-Q4_K_M-00001-of-00004.gguf".into(),
                "UD-Q4_K_M/MiniMax-M2.7-UD-Q4_K_M-00002-of-00004.gguf".into(),
                "UD-Q4_K_M/MiniMax-M2.7-UD-Q4_K_M-00003-of-00004.gguf".into(),
                "UD-Q4_K_M/MiniMax-M2.7-UD-Q4_K_M-00004-of-00004.gguf".into(),
            ],
        },
    ]
}

/// Filter curated list by available RAM.
pub fn filter_by_hardware(entries: &[RegistryEntry], available_ram_mb: u64) -> Vec<RegistryEntry> {
    entries
        .iter()
        .filter(|e| e.min_ram_mb <= available_ram_mb)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_registry_not_empty() {
        let reg = builtin_registry();
        assert!(reg.len() >= 3);
        assert!(reg.iter().all(|e| e.tier == ModelTier::Recommended));
    }

    #[test]
    fn trusted_uploaders_check() {
        assert!(is_trusted("Qwen/Qwen3-8B-GGUF"));
        assert!(is_trusted("bartowski/some-model"));
        assert!(!is_trusted("random-user/some-model"));
    }

    #[test]
    fn filter_by_ram() {
        let reg = builtin_registry();
        let small = filter_by_hardware(&reg, 4000);
        assert!(small.iter().all(|e| e.min_ram_mb <= 4000));
        assert!(!small.is_empty());
    }
}
