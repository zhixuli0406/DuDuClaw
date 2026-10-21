//! `config.toml [knowledge]` — the operator's switches and ceilings.
//!
//! Every value is clamped into a documented range: a missing file, a missing
//! section, a wrong-typed key or an out-of-range number resolves to the
//! default or the nearest bound. There is no "unlimited" setting.

use std::path::Path;

/// Default single-file ceiling (100 MiB).
pub const DEFAULT_MAX_SOURCE_BYTES: u64 = 100 * 1024 * 1024;
const MIN_MAX_SOURCE_BYTES: u64 = 1024 * 1024;
const MAX_MAX_SOURCE_BYTES: u64 = 200 * 1024 * 1024;
/// Default ceiling on extracted characters per source (whole document is
/// refused above it, never truncated).
pub const DEFAULT_MAX_SOURCE_CHARS: usize = 500_000;
/// Files accepted in one upload request.
pub const MAX_FILES_PER_UPLOAD: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnowledgeConfig {
    /// Master switch. Off ⇒ the MCP tools are neither listed nor callable and
    /// every RPC refuses.
    pub enabled: bool,
    pub max_source_bytes: u64,
    pub max_source_chars: usize,
    pub max_sources_per_collection: usize,
    pub max_collections: usize,
    pub max_chunks_total: usize,
    pub extract_timeout_secs: u64,
    pub extract_memory_mb: u64,
    pub access_log_retention_days: u32,
}

impl Default for KnowledgeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_source_bytes: DEFAULT_MAX_SOURCE_BYTES,
            max_source_chars: DEFAULT_MAX_SOURCE_CHARS,
            max_sources_per_collection: 300,
            max_collections: 200,
            max_chunks_total: 200_000,
            extract_timeout_secs: 180,
            extract_memory_mb: 2048,
            access_log_retention_days: 180,
        }
    }
}

fn int(table: &toml::Table, key: &str) -> Option<i64> {
    table.get(key).and_then(toml::Value::as_integer)
}

fn clamp_u64(v: Option<i64>, default: u64, lo: u64, hi: u64) -> u64 {
    match v {
        Some(n) if n > 0 => (n as u64).clamp(lo, hi),
        _ => default,
    }
}

impl KnowledgeConfig {
    /// Parse the `[knowledge]` table of a `config.toml` document.
    pub fn from_toml_str(text: &str) -> Self {
        let root: toml::Table = text.parse().unwrap_or_default();
        Self::from_root(&root)
    }

    /// Read `<home>/config.toml`. Unreadable ⇒ defaults.
    pub fn from_home(home: &Path) -> Self {
        let text = std::fs::read_to_string(home.join("config.toml")).unwrap_or_default();
        Self::from_toml_str(&text)
    }

    fn from_root(root: &toml::Table) -> Self {
        let d = Self::default();
        let Some(t) = root.get("knowledge").and_then(toml::Value::as_table) else {
            return d;
        };
        Self {
            enabled: t.get("enabled").and_then(toml::Value::as_bool).unwrap_or(d.enabled),
            max_source_bytes: clamp_u64(
                int(t, "max_source_bytes"),
                d.max_source_bytes,
                MIN_MAX_SOURCE_BYTES,
                MAX_MAX_SOURCE_BYTES,
            ),
            max_source_chars: clamp_u64(int(t, "max_source_chars"), d.max_source_chars as u64, 1_000, 2_000_000)
                as usize,
            max_sources_per_collection: clamp_u64(
                int(t, "max_sources_per_collection"),
                d.max_sources_per_collection as u64,
                1,
                10_000,
            ) as usize,
            max_collections: clamp_u64(int(t, "max_collections"), d.max_collections as u64, 1, 10_000) as usize,
            max_chunks_total: clamp_u64(int(t, "max_chunks_total"), d.max_chunks_total as u64, 1_000, 2_000_000)
                as usize,
            extract_timeout_secs: clamp_u64(int(t, "extract_timeout_secs"), d.extract_timeout_secs, 10, 1_800),
            extract_memory_mb: clamp_u64(int(t, "extract_memory_mb"), d.extract_memory_mb, 256, 16_384),
            access_log_retention_days: clamp_u64(
                int(t, "access_log_retention_days"),
                d.access_log_retention_days as u64,
                1,
                3_650,
            ) as u32,
        }
    }

    /// Largest upload request body: every file at its ceiling plus 1 MiB of
    /// multipart framing.
    pub fn max_upload_body_bytes(&self) -> usize {
        (self.max_source_bytes as usize)
            .saturating_mul(MAX_FILES_PER_UPLOAD)
            .saturating_add(1024 * 1024)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_section_is_default() {
        assert_eq!(KnowledgeConfig::from_toml_str(""), KnowledgeConfig::default());
        assert_eq!(KnowledgeConfig::from_toml_str("not toml ["), KnowledgeConfig::default());
    }

    #[test]
    fn values_are_clamped_and_wrong_types_fall_back() {
        let c = KnowledgeConfig::from_toml_str(
            "[knowledge]\nenabled = false\nmax_source_bytes = 1\nmax_source_chars = \"many\"\nextract_timeout_secs = 999999\n",
        );
        assert!(!c.enabled);
        assert_eq!(c.max_source_bytes, MIN_MAX_SOURCE_BYTES);
        assert_eq!(c.max_source_chars, DEFAULT_MAX_SOURCE_CHARS);
        assert_eq!(c.extract_timeout_secs, 1_800);
        let c = KnowledgeConfig::from_toml_str("[knowledge]\nmax_source_bytes = 0\n");
        assert_eq!(c.max_source_bytes, DEFAULT_MAX_SOURCE_BYTES);
    }
}
