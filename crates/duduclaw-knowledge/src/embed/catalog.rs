//! The pinned local embedding models (`embed-catalog.json`).
//!
//! Every file a model needs is named with its size and SHA-256 at one
//! Hugging Face commit; nothing resolves `main` or "latest" at runtime. The
//! JSON (not a Rust table) is the single source because the offline-bundle
//! script reads it too.
//!
//! Models with a non-commercial licence are not listed (jina-embeddings v3 /
//! v5 are CC BY-NC 4.0).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// How token states become one vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pooling {
    /// Mask-weighted mean over tokens (BERT-style encoders such as e5).
    Mean,
    /// The hidden state of the last real token (decoder embedders such as
    /// Qwen3-Embedding, whose tokenizer appends `<|endoftext|>`).
    LastToken,
}

/// One pinned file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelFile {
    /// Path inside the repo and under the install directory.
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

/// A catalog entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelSpec {
    pub id: String,
    pub display_name: String,
    pub repo: String,
    pub revision: String,
    pub license: String,
    pub license_source: String,
    /// Stored vector size (after Matryoshka truncation when smaller than
    /// `native_dim`).
    pub dim: usize,
    pub native_dim: usize,
    pub max_tokens: usize,
    pub pooling: Pooling,
    pub query_prefix: String,
    pub passage_prefix: String,
    pub model_file: String,
    pub tokenizer_file: String,
    /// Decoder exports with a `past_key_values.*` input take an empty cache
    /// of shape `[batch, kv_heads, 0, head_dim]`. Zero for encoders.
    pub kv_heads: usize,
    pub head_dim: usize,
    /// `fast` / `slow`, for the dashboard.
    pub speed: String,
    /// Most texts per forward pass. The int8 exports quantise activations
    /// with one dynamic scale per tensor, so other rows (and their padding)
    /// shift a text's vector; measured on 2026-10-22: e5 alone vs batched
    /// cosine 0.9994, Qwen3 0.965. Qwen3 therefore runs one text at a time.
    pub batch_limit: usize,
    pub files: Vec<ModelFile>,
}

#[derive(Debug, Deserialize)]
struct CatalogFile {
    models: Vec<ModelSpec>,
}

const RAW: &str = include_str!("../../embed-catalog.json");

/// Every model, in catalog order (the first is the default).
pub fn models() -> &'static [ModelSpec] {
    static CATALOG: OnceLock<Vec<ModelSpec>> = OnceLock::new();
    CATALOG.get_or_init(|| match serde_json::from_str::<CatalogFile>(RAW) {
        Ok(c) => c.models,
        Err(e) => {
            // The file is compiled in and covered by a test; this only
            // fires for a broken build. No model is better than a guess.
            tracing::error!(error = %e, "knowledge: embedding catalog is unreadable");
            Vec::new()
        }
    })
}

/// The default local model id.
pub const DEFAULT_MODEL: &str = "multilingual-e5-small";

/// Look a model up by id (exact match).
pub fn find(id: &str) -> Option<&'static ModelSpec> {
    models().iter().find(|m| m.id == id)
}

/// Marker written after every file of a model verified.
pub const INSTALL_MARKER: &str = "installed.json";

impl ModelSpec {
    /// `<home>/models/embedding/<id>/`.
    pub fn install_dir(&self, home: &Path) -> PathBuf {
        home.join("models").join("embedding").join(&self.id)
    }

    /// Download URL of one file at the pinned commit.
    pub fn url(&self, file: &ModelFile) -> String {
        format!(
            "https://huggingface.co/{}/resolve/{}/{}",
            self.repo, self.revision, file.path
        )
    }

    /// Where `file` lands under `dir`.
    pub fn dest(dir: &Path, file: &ModelFile) -> PathBuf {
        let mut p = dir.to_path_buf();
        for seg in file.path.split('/') {
            p.push(seg);
        }
        p
    }

    /// Total download bytes (model files only).
    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    /// Stored with every vector: `local/<id>@<rev12>/<dim>`. Vectors of
    /// different ids are never compared.
    pub fn model_id(&self) -> String {
        let rev = duduclaw_core::truncate_bytes(&self.revision, 12);
        format!("local/{}@{rev}/{}", self.id, self.dim)
    }

    /// The text a passage is embedded as.
    pub fn passage_input(&self, text: &str) -> String {
        format!("{}{text}", self.passage_prefix)
    }

    /// The text a query is embedded as.
    pub fn query_input(&self, text: &str) -> String {
        format!("{}{text}", self.query_prefix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_parses_and_every_file_is_pinned() {
        assert!(models().len() >= 2, "catalog must parse");
        assert_eq!(models()[0].id, DEFAULT_MODEL);
        for m in models() {
            assert_eq!(m.revision.len(), 40, "{} must pin a full commit", m.id);
            assert!(
                m.id.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.')
                    && !m.id.contains(".."),
                "{} is used as a directory name",
                m.id
            );
            assert!(m.revision.chars().all(|c| c.is_ascii_hexdigit()));
            assert!(m.dim > 0 && m.dim <= m.native_dim);
            assert!(m.files.iter().any(|f| f.path == m.model_file));
            assert!(m.files.iter().any(|f| f.path == m.tokenizer_file));
            for f in &m.files {
                assert!(
                    !f.path.starts_with('/')
                        && !f.path.contains('\\')
                        && f.path
                            .split('/')
                            .all(|seg| !seg.is_empty() && seg != "." && seg != ".."),
                    "unsafe catalog path {}",
                    f.path
                );
                assert_eq!(f.sha256.len(), 64, "{}", f.path);
                assert!(
                    f.sha256
                        .chars()
                        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
                );
                assert!(f.size > 0);
                let url = m.url(f);
                assert!(url.starts_with("https://huggingface.co/"));
                assert!(url.contains(&m.revision) && !url.contains("/resolve/main/"));
            }
            if m.pooling == Pooling::LastToken {
                assert!(m.kv_heads > 0 && m.head_dim > 0);
            }
        }
    }

    #[test]
    fn no_non_commercial_model_is_listed() {
        for m in models() {
            let l = m.license.to_ascii_lowercase();
            assert!(!l.contains("nc"), "{} has a non-commercial licence", m.id);
            assert!(!m.repo.to_ascii_lowercase().contains("jina"));
        }
    }

    #[test]
    fn e5_prefixes_follow_the_model_card() {
        let e5 = find("multilingual-e5-small").unwrap();
        assert_eq!(e5.query_input("特休幾天"), "query: 特休幾天");
        assert_eq!(e5.passage_input("特休七日"), "passage: 特休七日");
        assert_eq!(e5.pooling, Pooling::Mean);
        assert_eq!(e5.dim, 384);
    }

    #[test]
    fn qwen3_query_uses_the_instruct_format_and_passages_are_bare() {
        let q = find("qwen3-embedding-0.6b").unwrap();
        let input = q.query_input("特休幾天");
        assert!(input.starts_with("Instruct: "));
        assert!(input.ends_with("\nQuery:特休幾天"), "{input:?}");
        assert_eq!(q.passage_input("特休七日"), "特休七日");
        assert_eq!(q.pooling, Pooling::LastToken);
    }

    #[test]
    fn model_id_binds_revision_and_dim() {
        let e5 = find(DEFAULT_MODEL).unwrap();
        assert_eq!(
            e5.model_id(),
            "local/multilingual-e5-small@614241f622f5/384"
        );
        let dir = e5.install_dir(Path::new("/h"));
        assert_eq!(
            dir,
            PathBuf::from("/h/models/embedding/multilingual-e5-small")
        );
    }
}
