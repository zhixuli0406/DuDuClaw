//! "AI 智慧偵測" — native NER detection with the OpenAI Privacy Filter.
//!
//! The regex / keyword / identity rules catch data with a *fixed shape*
//! (a national ID, an API key). Names, street addresses, birthdays and
//! account numbers have no fixed shape, so today they are only caught when
//! an operator names the exact database column they live in. This module is
//! the second layer for the rest: a 1.5B-parameter (50M active) bidirectional
//! token classifier, Apache-2.0, run locally through ONNX Runtime — no text
//! leaves the machine.
//!
//! It is **not** an anonymisation guarantee, and nothing in the product may
//! claim otherwise. The model card is explicit that it is a data-minimisation
//! component, and our own G0 measurements on 25 zh-TW sentences put overall
//! recall at 79.8% (person 72%). Regex rules stay on; this adds to them.
//!
//! ## Layout
//!
//! | module | role |
//! |---|---|
//! | [`labels`] | model label ↔ DuDuClaw category table |
//! | [`decode`] | constrained BIOES Viterbi + span building |
//! | [`manifest`] | pinned download manifest (URL / size / sha256) |
//! | [`install`] | resumable download, verify, extract (feature `ner`) |
//! | [`runtime`] | lazy session, inference, idle unload, latency stats (feature `ner`) |
//!
//! The rule itself is [`crate::rules::ner::NerRule`].

pub mod decode;
pub mod labels;
pub mod manifest;

#[cfg(feature = "ner")]
pub mod install;
#[cfg(feature = "ner")]
pub mod runtime;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Default per-rule minimum input length (characters) before the model is
/// consulted. Short strings are where the model's precision is worst (a bare
/// value with no surrounding clue) and where a regex rule is most likely to
/// already have an answer.
pub const DEFAULT_MIN_CHARS: usize = 24;

/// Default maximum characters fed to the model in one pass. Longer text is
/// chunked on paragraph boundaries with the offsets fixed up afterwards.
pub const DEFAULT_MAX_CHARS: usize = 32_000;

/// Default priority for a NER rule — below every built-in regex rule (50–100)
/// so a precise pattern always wins an overlap and the model only fills gaps.
pub const DEFAULT_NER_PRIORITY: i32 = 30;

/// Where the model and the ONNX Runtime library live on disk.
///
/// Lives here rather than in [`install`] so it names the same type in a build
/// without the `ner` feature — [`crate::EngineOptions`] carries one, and that
/// struct must not change shape with a feature flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallDirs {
    /// `<home>/models/privacy-filter`.
    pub model_dir: PathBuf,
    /// `<home>/lib/onnxruntime` — the version directory is appended by the
    /// manifest so several ORT versions can coexist across an upgrade.
    pub ort_lib_root: PathBuf,
}

impl InstallDirs {
    /// Conventional layout under a DuDuClaw home.
    pub fn under_home(home: &Path) -> Self {
        Self {
            model_dir: home.join("models").join("privacy-filter"),
            ort_lib_root: home.join("lib").join("onnxruntime"),
        }
    }
}

/// `[redaction.ner]` — deployment-wide settings for the model runtime.
///
/// Unknown keys are ignored, matching the rest of [`crate::RedactionConfig`];
/// every field has a default so an operator who writes nothing gets the
/// documented behaviour.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct NerConfig {
    /// Where the model files live. `None` ⇒ `<duduclaw_home>/models/privacy-filter`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_dir: Option<PathBuf>,

    /// ONNX Runtime intra-op thread count. The G0 spike measured 4 threads as
    /// the knee on a 10-core M-series; more threads bought nothing.
    pub threads: usize,

    /// Unload the session after this many minutes with no inference,
    /// releasing ~1.7 GB of resident memory. `0` disables unloading.
    pub idle_unload_minutes: u64,

    /// Deployment-wide default for a rule's `min_chars`.
    pub min_chars: usize,

    /// Deployment-wide default for a rule's `max_chars`.
    pub max_chars: usize,

    /// Result cache size, in distinct texts. One tool result is commonly
    /// scanned by several rules in the same turn; the cache makes the eight
    /// per-label rules cost one inference between them.
    pub cache_entries: usize,
}

impl Default for NerConfig {
    fn default() -> Self {
        Self {
            model_dir: None,
            threads: 4,
            idle_unload_minutes: 10,
            min_chars: DEFAULT_MIN_CHARS,
            max_chars: DEFAULT_MAX_CHARS,
            cache_entries: 256,
        }
    }
}

impl NerConfig {
    /// Resolve the model directory against a DuDuClaw home.
    pub fn resolve_model_dir(&self, home: &Path) -> PathBuf {
        self.model_dir
            .clone()
            .unwrap_or_else(|| home.join("models").join("privacy-filter"))
    }

    /// Clamp every field into a range that cannot wedge the runtime.
    ///
    /// Applied at engine-compile time rather than at parse time so a config
    /// read for display round-trips exactly what the operator wrote.
    pub fn sanitized(&self) -> Self {
        Self {
            model_dir: self.model_dir.clone(),
            threads: self.threads.clamp(1, 64),
            idle_unload_minutes: self.idle_unload_minutes.min(24 * 60),
            min_chars: self.min_chars.min(100_000),
            max_chars: self.max_chars.clamp(64, 200_000),
            cache_entries: self.cache_entries.clamp(1, 4096),
        }
    }
}

/// Split `text` into chunks of at most `max_chars` **characters**, preferring
/// paragraph boundaries, and report each chunk's byte offset in the original.
///
/// Returned offsets are byte offsets so a span found inside a chunk maps back
/// with a plain addition. Every boundary is a char boundary by construction
/// (we only ever cut between `char_indices` positions), so the caller can
/// slice safely.
pub fn chunk_text(text: &str, max_chars: usize) -> Vec<(usize, &str)> {
    let max_chars = max_chars.max(1);
    if text.chars().count() <= max_chars {
        return if text.is_empty() {
            Vec::new()
        } else {
            vec![(0, text)]
        };
    }

    let mut out: Vec<(usize, &str)> = Vec::new();
    let mut start = 0usize; // byte offset of the current chunk

    while start < text.len() {
        let rest = &text[start..];
        // Byte offset (within `rest`) just past the max_chars-th character.
        let hard_end = match rest.char_indices().nth(max_chars) {
            Some((idx, _)) => idx,
            None => {
                out.push((start, rest));
                break;
            }
        };
        // Prefer the last paragraph break inside the window; fall back to the
        // last newline, then to the hard character cut.
        let window = &rest[..hard_end];
        let cut = window
            .rfind("\n\n")
            .map(|i| i + 2)
            .or_else(|| window.rfind('\n').map(|i| i + 1))
            .filter(|&i| i > 0)
            .unwrap_or(hard_end);

        out.push((start, &rest[..cut]));
        start += cut;
    }
    out.retain(|(_, s)| !s.is_empty());
    out
}

/// Byte-range slice that never panics on a multi-byte boundary.
///
/// Tokenizer offsets are already char-aligned in practice, but this is the
/// one place model-derived integers become a string slice, so it snaps
/// outward rather than trusting them (CLAUDE.md convention 1).
pub fn safe_slice(s: &str, start: usize, end: usize) -> &str {
    let mut a = start.min(s.len());
    let mut b = end.min(s.len());
    if a > b {
        std::mem::swap(&mut a, &mut b);
    }
    while a > 0 && !s.is_char_boundary(a) {
        a -= 1;
    }
    while b < s.len() && !s.is_char_boundary(b) {
        b += 1;
    }
    &s[a..b]
}

/// Snap a byte range outward to char boundaries, returning the corrected pair.
pub fn safe_bounds(s: &str, start: usize, end: usize) -> (usize, usize) {
    let mut a = start.min(s.len());
    let mut b = end.min(s.len());
    if a > b {
        std::mem::swap(&mut a, &mut b);
    }
    while a > 0 && !s.is_char_boundary(a) {
        a -= 1;
    }
    while b < s.len() && !s.is_char_boundary(b) {
        b += 1;
    }
    (a, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_design_contract() {
        let c = NerConfig::default();
        assert_eq!(c.threads, 4);
        assert_eq!(c.idle_unload_minutes, 10);
        assert_eq!(c.min_chars, 24);
        assert_eq!(c.max_chars, 32_000);
        assert_eq!(c.cache_entries, 256);
        assert!(c.model_dir.is_none());
    }

    #[test]
    fn model_dir_defaults_under_home_and_honours_an_override() {
        let c = NerConfig::default();
        assert_eq!(
            c.resolve_model_dir(Path::new("/h")),
            PathBuf::from("/h").join("models").join("privacy-filter")
        );
        let c2 = NerConfig {
            model_dir: Some(PathBuf::from("/elsewhere")),
            ..NerConfig::default()
        };
        assert_eq!(c2.resolve_model_dir(Path::new("/h")), PathBuf::from("/elsewhere"));
    }

    #[test]
    fn sanitized_clamps_hostile_values() {
        let c = NerConfig {
            threads: 0,
            idle_unload_minutes: 999_999,
            max_chars: 1,
            cache_entries: 0,
            ..NerConfig::default()
        }
        .sanitized();
        assert_eq!(c.threads, 1);
        assert_eq!(c.idle_unload_minutes, 24 * 60);
        assert_eq!(c.max_chars, 64);
        assert_eq!(c.cache_entries, 1);
    }

    #[test]
    fn ner_config_parses_from_a_partial_toml_block() {
        let c: NerConfig = toml::from_str("threads = 8\n").unwrap();
        assert_eq!(c.threads, 8);
        assert_eq!(c.min_chars, DEFAULT_MIN_CHARS, "unset keys keep defaults");
    }

    #[test]
    fn short_text_is_one_chunk_at_offset_zero() {
        assert_eq!(chunk_text("hello", 100), vec![(0, "hello")]);
        assert!(chunk_text("", 100).is_empty());
    }

    #[test]
    fn chunks_prefer_paragraph_breaks_and_offsets_are_exact() {
        let text = "aaaa\n\nbbbb\n\ncccc";
        let chunks = chunk_text(text, 6);
        assert!(chunks.len() > 1);
        for (off, c) in &chunks {
            assert_eq!(&text[*off..*off + c.len()], *c, "offset must index the original");
        }
        let joined: String = chunks.iter().map(|(_, c)| *c).collect();
        assert_eq!(joined, text, "chunking must be lossless");
    }

    #[test]
    fn cjk_chunking_never_splits_a_codepoint() {
        let text = "台北市信義區松仁路一百號八樓。".repeat(20);
        let chunks = chunk_text(&text, 7);
        assert!(chunks.len() > 1);
        for (off, c) in &chunks {
            assert!(text.is_char_boundary(*off), "offset {off} is mid-codepoint");
            assert!(text.is_char_boundary(off + c.len()));
            assert!(c.chars().count() <= 7, "chunk over budget: {}", c.chars().count());
        }
        let joined: String = chunks.iter().map(|(_, c)| *c).collect();
        assert_eq!(joined, text);
    }

    #[test]
    fn a_span_found_in_a_chunk_maps_back_to_the_original_text() {
        let text = format!("{}王小明的電話", "填充字串。".repeat(30));
        let chunks = chunk_text(&text, 40);
        // Locate the name inside whichever chunk holds it, then map back.
        let needle = "王小明";
        let mut mapped = None;
        for (off, c) in &chunks {
            if let Some(i) = c.find(needle) {
                mapped = Some(off + i);
                break;
            }
        }
        let mapped = mapped.expect("needle must survive chunking");
        assert_eq!(&text[mapped..mapped + needle.len()], needle);
        assert_eq!(mapped, text.find(needle).unwrap());
    }

    #[test]
    fn single_paragraph_longer_than_the_budget_still_gets_cut() {
        let text = "x".repeat(50);
        let chunks = chunk_text(&text, 10);
        assert_eq!(chunks.len(), 5);
        assert_eq!(chunks[0], (0, &text[..10]));
        assert_eq!(chunks[4].0, 40);
    }

    #[test]
    fn safe_slice_snaps_out_of_a_codepoint_instead_of_panicking() {
        let s = "王小明";
        assert_eq!(safe_slice(s, 0, 3), "王");
        // byte 1 is mid-codepoint
        assert_eq!(safe_slice(s, 1, 2), "王");
        assert_eq!(safe_slice(s, 0, 999), s);
        assert_eq!(safe_slice(s, 5, 1), "王小");
    }

    #[test]
    fn safe_bounds_agrees_with_safe_slice() {
        let s = "abc王小明";
        for a in 0..s.len() + 2 {
            for b in 0..s.len() + 2 {
                let (x, y) = safe_bounds(s, a, b);
                assert!(s.is_char_boundary(x) && s.is_char_boundary(y));
                assert_eq!(&s[x..y], safe_slice(s, a, b));
            }
        }
    }
}
