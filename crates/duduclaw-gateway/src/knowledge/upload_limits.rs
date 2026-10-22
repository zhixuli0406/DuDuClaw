//! `knowledge.limits` — what the upload route will accept, so the dashboard
//! can refuse an oversized file or an unsupported type before sending it.
//! The route still enforces every limit itself (streaming per-file cap and
//! a request body cap); this answer only saves a pointless upload.

use std::path::Path;

use duduclaw_knowledge::extract::Format;
use duduclaw_knowledge::limits::{KnowledgeConfig, MAX_FILES_PER_UPLOAD};
use serde_json::{Value, json};

/// File extensions the upload route reads (lowercase, without the dot).
/// Must agree with `Format::from_file_name`; a test checks every entry.
pub const UPLOAD_EXTENSIONS: &[&str] = &[
    "pdf", "docx", "pptx", "xlsx", "xlsm", "xls", "csv", "txt", "text", "log", "md", "markdown",
    "html", "htm",
];

/// The answer for `knowledge.limits`.
pub fn describe(home: &Path) -> Value {
    let cfg = KnowledgeConfig::from_home(home);
    json!({
        "max_source_bytes": cfg.max_source_bytes,
        "max_files_per_upload": MAX_FILES_PER_UPLOAD,
        "extensions": UPLOAD_EXTENSIONS,
    })
}

/// Whether the route would read a file with this name (for tests and
/// callers that want the same rule).
pub fn accepted(name: &str) -> bool {
    Format::from_file_name(name).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_matches_the_extractor() {
        for ext in UPLOAD_EXTENSIONS {
            assert!(accepted(&format!("a.{ext}")), "{ext}");
        }
        for ext in ["doc", "ppt", "rtf", "png", "exe", "zip"] {
            assert!(!accepted(&format!("a.{ext}")), "{ext}");
        }
    }

    #[test]
    fn describe_reports_the_configured_size() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            "[knowledge]\nmax_source_bytes = 2097152\n",
        )
        .unwrap();
        let v = describe(home.path());
        assert_eq!(v["max_source_bytes"], 2_097_152);
        assert_eq!(v["max_files_per_upload"], 20);
    }
}
