//! JitRL experience store — append-only JSONL under the DuDuClaw home dir.
//!
//! Each line is one [`ExperienceRecord`]: the prompt's shingle sketch plus the
//! per-token outcome weights distilled from an explicit feedback call. The
//! store is a sibling of `~/.duduclaw/models/` (default path
//! `~/.duduclaw/jitrl_experience.jsonl`).
//!
//! Design choices:
//! - **JSONL + advisory lock** instead of SQLite: this crate has no rusqlite
//!   dependency, and cross-process appends must hold a lock per project
//!   convention — [`duduclaw_core::with_file_lock`] wraps every read/write.
//! - **Decay at read time** (Ebbinghaus-style exponential half-life, matching
//!   the memory engine's retrievability philosophy): weights are stored raw
//!   with a timestamp and decayed by age when retrieved, so the file never
//!   needs rewriting just because time passed.
//! - **Compaction on append**: when the file exceeds `max_records` lines the
//!   oldest records are dropped (newest kept), inside the same lock.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::error::{InferenceError, Result};

/// One distilled experience: prompt fingerprint → signed token weights.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperienceRecord {
    /// Unique record id.
    pub id: String,
    /// Model whose tokenizer produced `token_weights` keys. Biases are only
    /// ever applied to requests for the same model — token ids from one
    /// vocabulary are meaningless in another.
    pub model_id: String,
    /// Bottom-k shingle sketch of the normalized prompt
    /// (see [`crate::jitrl::fingerprint`]).
    pub sketch: Vec<u64>,
    /// token id → signed outcome weight (positive = reinforce, negative = suppress).
    pub token_weights: HashMap<u32, f32>,
    /// The raw reward supplied at feedback time (clamped to [-1, 1]).
    pub reward: f32,
    /// Unix seconds at record time — decay anchor.
    pub created_at: i64,
}

/// Exponential half-life decay: `w * 0.5^(age / half_life)`.
///
/// Consistent in spirit with the memory engine's Ebbinghaus retrievability
/// (`R = exp(-t/S)`) but with a single cheap parameter. Negative ages (clock
/// skew) are treated as zero age.
pub fn decayed_weight(weight: f32, age_secs: i64, half_life_days: f32) -> f32 {
    if half_life_days <= 0.0 {
        return weight;
    }
    let age_days = (age_secs.max(0) as f64) / 86_400.0;
    let factor = 0.5_f64.powf(age_days / half_life_days as f64);
    (weight as f64 * factor) as f32
}

/// Cached parse of the whole store file, keyed by `(mtime, len)`.
struct ParsedCache {
    mtime: std::time::SystemTime,
    len: u64,
    records: std::sync::Arc<Vec<ExperienceRecord>>,
}

/// Append-only JSONL experience store.
pub struct ExperienceStore {
    path: PathBuf,
    max_records: usize,
    /// mtime-keyed parse cache (2026-07 MED): the enabled hot path called
    /// `load_for_model` — a full read+parse of the JSONL — on EVERY request.
    /// The cache is only ever populated by reads (the disabled path builds no
    /// `JitrlEngine`, hence no store, hence untouched) and is invalidated by
    /// a changed `(mtime, len)` stat or an in-process append.
    cache: std::sync::Mutex<Option<ParsedCache>>,
}

impl ExperienceStore {
    pub fn new(path: PathBuf, max_records: usize) -> Self {
        Self {
            path,
            max_records: max_records.max(1),
            cache: std::sync::Mutex::new(None),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one record, compacting (drop-oldest) if the file exceeds
    /// `max_records`. The whole read-append-compact runs under the advisory
    /// file lock so concurrent gateway/CLI processes cannot interleave lines.
    pub fn append(&self, record: &ExperienceRecord) -> Result<()> {
        let line = serde_json::to_string(record)
            .map_err(|e| InferenceError::Other(format!("jitrl: serialize record: {e}")))?;
        let path = self.path.clone();
        let max = self.max_records;
        duduclaw_core::with_file_lock(&path, || {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let existing = match std::fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(e) => return Err(e),
            };
            let mut lines: Vec<&str> = existing.lines().filter(|l| !l.trim().is_empty()).collect();
            lines.push(&line);
            if lines.len() > max {
                // Keep the newest `max` records (file order == append order).
                let drop = lines.len() - max;
                lines.drain(..drop);
            }
            let mut out = lines.join("\n");
            out.push('\n');
            // Atomic-ish rewrite: temp + rename, consistent with the project's
            // SOUL.md write pattern; the lock already excludes writers.
            let tmp = path.with_extension("jsonl.tmp");
            std::fs::write(&tmp, out)?;
            std::fs::rename(&tmp, &path)?;
            Ok(())
        })
        .map_err(InferenceError::Io)?;
        // In-process cache invalidation — mtime granularity alone could miss
        // a same-second rewrite; dropping the entry is always correct.
        if let Ok(mut cache) = self.cache.lock() {
            *cache = None;
        }
        Ok(())
    }

    /// Load all records for a model. Malformed lines are skipped (counted in
    /// a warn log), never fatal — a corrupt line must not disable the feature.
    ///
    /// Backed by an `(mtime, len)`-keyed parse cache so per-request retrieval
    /// doesn't re-read + re-parse the whole file when nothing changed.
    pub fn load_for_model(&self, model_id: &str) -> Result<Vec<ExperienceRecord>> {
        let all = self.load_all_cached()?;
        Ok(all
            .iter()
            .filter(|r| r.model_id == model_id)
            .cloned()
            .collect())
    }

    /// Whole-file parse with the `(mtime, len)` cache. A cache hit skips both
    /// the advisory lock and the parse; any stat mismatch (or a missing file)
    /// falls through to a locked re-read.
    fn load_all_cached(&self) -> Result<std::sync::Arc<Vec<ExperienceRecord>>> {
        let stat = match std::fs::metadata(&self.path) {
            Ok(m) => m.modified().ok().map(|mtime| (mtime, m.len())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(std::sync::Arc::new(Vec::new()));
            }
            Err(e) => return Err(InferenceError::Io(e)),
        };
        if let (Some((mtime, len)), Ok(cache)) = (stat, self.cache.lock()) {
            if let Some(c) = cache.as_ref() {
                if c.mtime == mtime && c.len == len {
                    return Ok(std::sync::Arc::clone(&c.records));
                }
            }
        }

        let path = self.path.clone();
        // Read content AND re-stat inside the lock so the cached `(mtime,
        // len)` key is consistent with the bytes actually parsed (writers
        // hold the same lock).
        let (content, stat) = duduclaw_core::with_file_lock(&path, || {
            let content = match std::fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(e) => return Err(e),
            };
            let stat = std::fs::metadata(&path)
                .ok()
                .and_then(|m| m.modified().ok().map(|mtime| (mtime, m.len())));
            Ok((content, stat))
        })
        .map_err(InferenceError::Io)?;

        let mut records = Vec::new();
        let mut malformed = 0usize;
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<ExperienceRecord>(line) {
                Ok(rec) => records.push(rec),
                Err(_) => malformed += 1,
            }
        }
        if malformed > 0 {
            warn!(
                path = %self.path.display(),
                malformed,
                "jitrl: skipped malformed experience lines"
            );
        }

        let records = std::sync::Arc::new(records);
        if let Some((mtime, len)) = stat {
            if let Ok(mut cache) = self.cache.lock() {
                *cache = Some(ParsedCache {
                    mtime,
                    len,
                    records: std::sync::Arc::clone(&records),
                });
            }
        }
        Ok(records)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(model: &str, tokens: &[(u32, f32)], created_at: i64) -> ExperienceRecord {
        ExperienceRecord {
            id: uuid::Uuid::new_v4().to_string(),
            model_id: model.to_string(),
            sketch: crate::jitrl::fingerprint::shingle_sketch("some prompt text"),
            token_weights: tokens.iter().copied().collect(),
            reward: 1.0,
            created_at,
        }
    }

    #[test]
    fn roundtrip_append_and_load() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = ExperienceStore::new(tmp.path().join("exp.jsonl"), 100);

        let rec = record("model-a", &[(42, 1.0), (7, -0.5)], 1_700_000_000);
        store.append(&rec).unwrap();
        store
            .append(&record("model-b", &[(9, 1.0)], 1_700_000_001))
            .unwrap();

        let loaded = store.load_for_model("model-a").unwrap();
        assert_eq!(loaded.len(), 1, "model filter must apply");
        assert_eq!(loaded[0].id, rec.id);
        assert_eq!(loaded[0].token_weights.get(&42), Some(&1.0));
        assert_eq!(loaded[0].token_weights.get(&7), Some(&-0.5));
        assert_eq!(loaded[0].sketch, rec.sketch);
    }

    #[test]
    fn decay_halves_at_half_life() {
        let half_life_days = 14.0_f32;
        let age = 14 * 86_400; // exactly one half-life
        let w = decayed_weight(1.0, age, half_life_days);
        assert!((w - 0.5).abs() < 1e-4, "got {w}");
        // Fresh weight is untouched.
        assert!((decayed_weight(1.0, 0, half_life_days) - 1.0).abs() < 1e-6);
        // Negative age (clock skew) treated as fresh.
        assert!((decayed_weight(1.0, -100, half_life_days) - 1.0).abs() < 1e-6);
        // half_life <= 0 disables decay.
        assert!((decayed_weight(0.8, 999_999, 0.0) - 0.8).abs() < 1e-6);
    }

    #[test]
    fn compaction_keeps_newest_records() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = ExperienceStore::new(tmp.path().join("exp.jsonl"), 3);
        for i in 0..5 {
            store
                .append(&record("m", &[(i as u32, 1.0)], 1_700_000_000 + i))
                .unwrap();
        }
        let loaded = store.load_for_model("m").unwrap();
        assert_eq!(loaded.len(), 3);
        let times: Vec<i64> = loaded.iter().map(|r| r.created_at).collect();
        assert_eq!(times, vec![1_700_000_002, 1_700_000_003, 1_700_000_004]);
    }

    #[test]
    fn malformed_lines_are_skipped_not_fatal() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("exp.jsonl");
        let store = ExperienceStore::new(path.clone(), 100);
        store
            .append(&record("m", &[(1, 1.0)], 1_700_000_000))
            .unwrap();
        // Corrupt the file with a partial line.
        let mut content = std::fs::read_to_string(&path).unwrap();
        content.push_str("{\"broken\": tru\n");
        std::fs::write(&path, content).unwrap();
        store
            .append(&record("m", &[(2, 1.0)], 1_700_000_001))
            .unwrap();

        let loaded = store.load_for_model("m").unwrap();
        assert_eq!(loaded.len(), 2, "valid records survive a corrupt line");
    }

    #[test]
    fn missing_file_loads_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = ExperienceStore::new(tmp.path().join("nope.jsonl"), 10);
        assert!(store.load_for_model("m").unwrap().is_empty());
    }

    #[test]
    fn parse_cache_stays_correct_across_appends_and_external_writes() {
        // 2026-07 MED: reads are served from an (mtime, len)-keyed cache.
        // Correctness contract: repeated loads agree, an in-process append
        // invalidates, and an external rewrite (different stat) is picked up.
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("exp.jsonl");
        let store = ExperienceStore::new(path.clone(), 100);

        store.append(&record("m", &[(1, 1.0)], 1_700_000_000)).unwrap();
        assert_eq!(store.load_for_model("m").unwrap().len(), 1);
        // Cache-hit path returns the same view.
        assert_eq!(store.load_for_model("m").unwrap().len(), 1);

        // In-process append invalidates the cache.
        store.append(&record("m", &[(2, 1.0)], 1_700_000_001)).unwrap();
        assert_eq!(store.load_for_model("m").unwrap().len(), 2);

        // External writer (another process): file length changes ⇒ stat
        // mismatch ⇒ re-read even if mtime granularity is coarse.
        let mut content = std::fs::read_to_string(&path).unwrap();
        let extra = serde_json::to_string(&record("m", &[(3, 1.0)], 1_700_000_002)).unwrap();
        content.push_str(&extra);
        content.push('\n');
        std::fs::write(&path, content).unwrap();
        assert_eq!(store.load_for_model("m").unwrap().len(), 3);

        // Model filter still applies on the cached view.
        assert!(store.load_for_model("other").unwrap().is_empty());
    }
}
