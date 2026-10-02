//! Operator-owned settings for discovery. Missing permissions deny execution.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DiscoveryConfig {
    pub attempt: AttemptSettings,
    pub approved_workspace_roots: Vec<PathBuf>,
    pub attempt_extra_read_paths: Vec<PathBuf>,
    pub account_pool: Vec<String>,
    pub allow_unconfined: bool,
    pub max_starting_workspace_bytes: u64,
    pub max_run_bytes: u64,
    pub max_total_bytes: u64,
    pub retained_hours: u64,
    pub evaluators: BTreeMap<String, EvaluatorConfig>,
}
impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            attempt: AttemptSettings::default(),
            approved_workspace_roots: vec![],
            attempt_extra_read_paths: vec![],
            account_pool: vec![],
            allow_unconfined: false,
            max_starting_workspace_bytes: 64 * 1024 * 1024,
            max_run_bytes: 512 * 1024 * 1024,
            max_total_bytes: 2 * 1024 * 1024 * 1024,
            retained_hours: 24,
            evaluators: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AttemptSandbox {
    #[default]
    Container,
    Native,
    None,
}

/// Operator-owned image entries. Executables are paths INSIDE a Linux image,
/// never host binaries or user-submitted shell fragments.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptRuntimeConfig {
    pub image: String,
    pub executable: PathBuf,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AttemptSettings {
    pub sandbox: AttemptSandbox,
    pub strict_usd: bool,
    pub allow_shared_account_pool: bool,
    pub memory_bytes: u64,
    pub pids: u32,
    pub cpu_millis: u32,
    pub tmp_bytes: u64,
    pub max_snapshot_bytes: u64,
    pub runtimes: BTreeMap<String, AttemptRuntimeConfig>,
}
impl Default for AttemptSettings {
    fn default() -> Self {
        Self {
            sandbox: AttemptSandbox::Container, strict_usd: false,
            allow_shared_account_pool: false, memory_bytes: 4 * 1024 * 1024 * 1024,
            pids: 128, cpu_millis: 1000, tmp_bytes: 128 * 1024 * 1024,
            max_snapshot_bytes: 512 * 1024 * 1024, runtimes: BTreeMap::new(),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluatorConfig {
    pub command: Vec<String>,
    pub sha256: String,
    pub sandbox: EvaluatorSandbox,
    #[serde(default)]
    pub image: Option<String>,
    pub good_solution: PathBuf,
    pub cheating_solution: PathBuf,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    #[serde(default = "default_memory")]
    pub memory_bytes: u64,
    #[serde(default = "default_pids")]
    pub pids: u32,
    #[serde(default = "default_scratch")]
    pub scratch_bytes: u64,
    #[serde(default)]
    pub test_data: Option<PathBuf>,
    #[serde(default)]
    pub timing_sensitive: bool,
}
fn default_timeout() -> u64 {
    30
}
fn default_memory() -> u64 {
    512 * 1024 * 1024
}
fn default_pids() -> u32 {
    64
}
fn default_scratch() -> u64 {
    64 * 1024 * 1024
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvaluatorSandbox {
    Container,
    Native,
    None,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_deny_paths_and_unconfined() {
        let config: DiscoveryConfig = toml::from_str("").unwrap();
        assert!(!config.allow_unconfined);
        assert!(config.approved_workspace_roots.is_empty());
        assert!(config.evaluators.is_empty());
    }
    #[test]
    fn rejects_unknown_or_missing_evaluator_settings() {
        assert!(
            toml::from_str::<DiscoveryConfig>("allow_unconfined = true\n typo = true").is_err()
        );
        assert!(toml::from_str::<EvaluatorConfig>("command = ['/tmp/x']").is_err());
    }
}
