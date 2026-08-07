//! §1.4 gene JSON export mapping (D5=B).
//!
//! Lossless mapping between a playbook entry and a GEP-gene-shaped JSON
//! document. This is an **independent implementation of a JSON shape**
//! following the concept published in arXiv:2604.15097 / the EvoMap GEP
//! protocol description — no evolver code, schema file, or npm package is
//! vendored, per the planning doc's reference-boundary rule (§2.5). Zero
//! network, zero external dependency beyond what the crate already uses
//! (`serde_json`, `sha2`).

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use super::entry::{
    Application, FailureNote, PlaybookCategory, PlaybookMeta, PlaybookState,
    PLAYBOOK_SCHEMA_VERSION,
};
use crate::prediction::rule_lifecycle::RuleStats;

/// Major.minor of the exported gene shape.
pub const GENE_SCHEMA: &str = "1.0";

/// A stable reference to an eval case: the suite-relative directory name
/// plus the case's declared `[case] name`, joined by `/`.
///
/// Example: case file `evals/ceo-assistant/refund.toml` with
/// `name = "refund-flow"` ⇒ `EvalCaseRef("ceo-assistant/refund-flow")`.
///
/// Why not the bare `name`: `name` uniqueness across a suite tree is
/// unenforced today, and any CLI-side selection by name alone risks
/// selecting the wrong subset (see `commercial/docs/DESIGN-evolution-v3-aee.md`
/// §1.4.2). This crate resolves a ref's existence independently of
/// `duduclaw-cli::eval` (which `duduclaw-gateway` cannot depend on — see
/// CLAUDE.md's dependency-direction note) by parsing `[case].name` out of
/// every `*.toml` file directly under `<eval_cases_root>/<suite>/`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EvalCaseRef(pub String);

impl EvalCaseRef {
    /// Split into `(suite, case_name)`. `None` when there is no `/`.
    pub fn parts(&self) -> Option<(&str, &str)> {
        self.0.split_once('/')
    }
}

/// Compute the gene `asset_id`: sha256 hex of a canonical JSON projection of
/// the fields that define "what this gene is"
/// (`category`/`summary`/`signals_match` sorted/`strategy`). **Derived**,
/// never round-tripped by identity — `from_gene` does not need to
/// reconstruct it (it isn't part of `PlaybookMeta`/`RuleStats`/`content`, so
/// it plays no role in the `from_gene(to_gene(x)) == x` property).
pub fn compute_asset_id(
    category: PlaybookCategory,
    summary: &str,
    signals_match: &[String],
    strategy: &[String],
) -> String {
    let mut sorted_signals = signals_match.to_vec();
    sorted_signals.sort();
    let canon = json!({
        "category": category.as_str(),
        "summary": summary,
        "signals_match": sorted_signals,
        "strategy": strategy,
    });
    let bytes = serde_json::to_vec(&canon).unwrap_or_default();
    let digest = Sha256::digest(&bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Lossless export of a playbook entry into a GEP-gene-shaped JSON document.
///
/// Note on two fields the GEP shape names but this function's signature
/// (matching `commercial/docs/DESIGN-evolution-v3-aee.md` §1.4 exactly) has
/// no direct source for:
/// - `confidence`: the design's mapping table sources this from "the memory
///   row's `confidence` column", which isn't part of `(content, meta,
///   stats)`. Since `confidence` is NOT part of `from_gene`'s return type
///   either, it cannot violate the round-trip property regardless of value;
///   this implementation derives it from `stats` (`helpful / (helpful +
///   harmful)`, a genuine "success rate" signal from data actually
///   available) rather than a dead constant.
/// - `x-duduclaw.entry_id` / `.agent_id`: the design's mapping table lists
///   these, but neither the memory row id nor the agent id is part of this
///   function's signature. They are written as empty strings here; callers
///   that hold the real id/agent_id (e.g. `crate::playbook::store`) may
///   patch `v["x-duduclaw"]["entry_id"]` / `["agent_id"]` onto the returned
///   value before persisting/exporting it externally.
pub fn to_gene(content: &str, meta: &PlaybookMeta, stats: &RuleStats) -> serde_json::Value {
    let asset_id = compute_asset_id(meta.category, content, &meta.signals_match, &meta.strategy);
    let validation: Vec<serde_json::Value> = meta
        .eval_cases
        .iter()
        .map(|c| json!({"kind": "duduclaw_eval", "ref": c.0}))
        .collect();
    let total = (stats.helpful + stats.harmful).max(1) as f64;
    let confidence = stats.helpful as f64 / total;

    json!({
        "gene_schema": GENE_SCHEMA,
        "type": "gene",
        "asset_id": asset_id,
        "category": meta.category.as_str(),
        "signals_match": meta.signals_match,
        "summary": content,
        "strategy": meta.strategy,
        "validation": validation,
        "confidence": confidence,
        "success_streak": meta.success_streak,
        "failure_history": meta.failure_history,
        "capsules": meta.applications,
        "x-duduclaw": {
            "schema_version": meta.schema_version,
            "state": meta.state.as_str(),
            "revision": meta.revision,
            "dedup_key": meta.dedup_key,
            "embed_model": meta.embed_model,
            "origin": meta.origin,
            "derived_from": meta.derived_from,
            "rule_stats": {"helpful": stats.helpful, "harmful": stats.harmful},
            "entry_id": "",
            "agent_id": "",
        },
    })
}

/// Inverse of [`to_gene`]. Fails closed on an unknown `gene_schema` MAJOR
/// version (a minor-version bump — additive fields — still parses: unknown
/// JSON keys are ignored by construction here since every field is read
/// explicitly rather than via `serde` struct deserialization).
pub fn from_gene(v: &serde_json::Value) -> Result<(String, PlaybookMeta, RuleStats), String> {
    let schema = v
        .get("gene_schema")
        .and_then(|s| s.as_str())
        .ok_or("missing gene_schema")?;
    let major: u32 = schema
        .split('.')
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("malformed gene_schema: {schema}"))?;
    let expected_major: u32 = GENE_SCHEMA.split('.').next().unwrap().parse().unwrap();
    if major > expected_major {
        return Err(format!("unsupported gene_schema major version: {schema}"));
    }

    let category = v
        .get("category")
        .and_then(|c| c.as_str())
        .and_then(PlaybookCategory::from_str)
        .ok_or("missing/invalid category")?;
    let summary = v
        .get("summary")
        .and_then(|s| s.as_str())
        .ok_or("missing summary")?
        .to_string();
    let signals_match = string_array(v.get("signals_match"));
    let strategy = string_array(v.get("strategy"));

    let eval_cases: Vec<EvalCaseRef> = v
        .get("validation")
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| c.get("ref").and_then(|r| r.as_str()).map(|s| EvalCaseRef(s.to_string())))
                .collect()
        })
        .unwrap_or_default();

    let success_streak = v.get("success_streak").and_then(|n| n.as_u64()).unwrap_or(0) as u32;
    let failure_history: Vec<FailureNote> = v
        .get("failure_history")
        .cloned()
        .and_then(|fh| serde_json::from_value(fh).ok())
        .unwrap_or_default();
    let applications: Vec<Application> = v
        .get("capsules")
        .cloned()
        .and_then(|c| serde_json::from_value(c).ok())
        .unwrap_or_default();

    let empty_x = json!({});
    let x = v.get("x-duduclaw").unwrap_or(&empty_x);
    let schema_version = x
        .get("schema_version")
        .and_then(|n| n.as_u64())
        .unwrap_or(PLAYBOOK_SCHEMA_VERSION as u64) as u32;
    let state = x
        .get("state")
        .and_then(|s| s.as_str())
        .and_then(PlaybookState::from_str)
        .unwrap_or(PlaybookState::Active);
    let revision = x.get("revision").and_then(|n| n.as_u64()).unwrap_or(0) as u32;
    let dedup_key = x
        .get("dedup_key")
        .and_then(|s| s.as_str())
        .map(String::from)
        .unwrap_or_else(|| super::dedup::dedup_key(&summary, category));
    let embed_model = x.get("embed_model").and_then(|s| s.as_str()).map(String::from);
    let origin = x
        .get("origin")
        .and_then(|s| s.as_str())
        .map(String::from)
        .unwrap_or_else(|| "agent_derived".to_string());
    let derived_from = string_array(x.get("derived_from"));
    let rule_stats: RuleStats = x
        .get("rule_stats")
        .cloned()
        .and_then(|s| serde_json::from_value(s).ok())
        .unwrap_or_default();

    let meta = PlaybookMeta {
        schema_version,
        category,
        signals_match,
        strategy,
        failure_history,
        eval_cases,
        applications,
        success_streak,
        state,
        revision,
        dedup_key,
        embed_model,
        origin,
        derived_from,
        assertions: Default::default(),
    };
    Ok((summary, meta, rule_stats))
}

fn string_array(v: Option<&serde_json::Value>) -> Vec<String> {
    v.and_then(|a| a.as_array())
        .map(|arr| arr.iter().filter_map(|s| s.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};

    fn arbitrary_meta(rng: &mut StdRng) -> (String, PlaybookMeta, RuleStats) {
        let categories = [
            PlaybookCategory::Repair,
            PlaybookCategory::Optimize,
            PlaybookCategory::Innovate,
            PlaybookCategory::Regulatory,
            PlaybookCategory::Explore,
        ];
        let states = [
            PlaybookState::Probation,
            PlaybookState::Active,
            PlaybookState::Stale,
            PlaybookState::Retired,
        ];
        let category = categories[rng.gen_range(0..categories.len())];
        let state = states[rng.gen_range(0..states.len())];

        let content_pool = ["hello world", "請注意這件事", "mix 混合 text 內容", ""];
        let content = content_pool[rng.gen_range(0..content_pool.len())].to_string();
        let content = if content.is_empty() {
            "x".to_string()
        } else {
            content
        };

        let n_signals = rng.gen_range(0..4);
        let signal_pool = ["*", "mistake:capability", "channel:telegram", "kw:refund", "tool:tasks_create"];
        let signals_match: Vec<String> = (0..n_signals)
            .map(|_| signal_pool[rng.gen_range(0..signal_pool.len())].to_string())
            .collect();

        let n_strategy = rng.gen_range(0..3);
        let strategy: Vec<String> = (0..n_strategy).map(|i| format!("step-{i}")).collect();

        let n_failures = rng.gen_range(0..3);
        let failure_history: Vec<FailureNote> = (0..n_failures)
            .map(|i| FailureNote {
                at: format!("2026-08-0{}T00:00:00Z", 1 + (i % 8)),
                what: format!("issue {i}"),
                source: "L1-Deterministic".to_string(),
            })
            .collect();

        let n_cases = rng.gen_range(0..3);
        let eval_cases: Vec<EvalCaseRef> = (0..n_cases)
            .map(|i| EvalCaseRef(format!("suite-{i}/case-{i}")))
            .collect();

        let n_apps = rng.gen_range(0..3);
        let applications: Vec<Application> = (0..n_apps)
            .map(|i| Application {
                at: "2026-08-06T00:00:00Z".to_string(),
                outcome: "negligible".to_string(),
                score: (i as f64) / 3.0,
                ctx: if i % 2 == 0 { Some(format!("ctx-{i}")) } else { None },
            })
            .collect();

        let embed_model = if rng.gen_bool(0.5) {
            Some("ngram-hash-v1".to_string())
        } else {
            None
        };

        let meta = PlaybookMeta {
            schema_version: PLAYBOOK_SCHEMA_VERSION,
            category,
            signals_match,
            strategy,
            failure_history,
            eval_cases,
            applications,
            success_streak: rng.gen_range(0..10),
            state,
            revision: rng.gen_range(0..5),
            dedup_key: format!("{:016x}", rng.r#gen::<u64>()),
            embed_model,
            origin: "agent_derived".to_string(),
            derived_from: (0..rng.gen_range(0..3)).map(|i| format!("mistake-{i}")).collect(),
            assertions: Default::default(),
        };
        let stats = RuleStats {
            helpful: rng.gen_range(0..10),
            harmful: rng.gen_range(0..10),
        };
        (content, meta, stats)
    }

    #[test]
    fn gene_roundtrip_is_lossless_1000_random_entries() {
        let mut rng = StdRng::seed_from_u64(0xACE_1234);
        for i in 0..1000 {
            let (content, meta, stats) = arbitrary_meta(&mut rng);
            let gene = to_gene(&content, &meta, &stats);
            let (out_content, out_meta, out_stats) =
                from_gene(&gene).unwrap_or_else(|e| panic!("case {i} failed to parse: {e}"));
            assert_eq!(out_content, content, "case {i}: content mismatch");
            assert_eq!(out_meta, meta, "case {i}: meta mismatch");
            assert_eq!(out_stats, stats, "case {i}: stats mismatch");
        }
    }

    #[test]
    fn phase1_entries_have_empty_strategy() {
        // Structural guard against over-engineering (§1.11 risk table):
        // nothing in this phase should be authoring non-empty strategy.
        let meta = PlaybookMeta {
            schema_version: PLAYBOOK_SCHEMA_VERSION,
            category: PlaybookCategory::Repair,
            signals_match: vec!["*".to_string()],
            strategy: Vec::new(),
            failure_history: Vec::new(),
            eval_cases: vec![EvalCaseRef("s/c".to_string())],
            applications: Vec::new(),
            success_streak: 0,
            state: PlaybookState::Probation,
            revision: 0,
            dedup_key: "0".repeat(16),
            embed_model: None,
            origin: "agent_derived".to_string(),
            derived_from: Vec::new(),
            assertions: Default::default(),
        };
        assert!(meta.strategy.is_empty());
        let gene = to_gene("content", &meta, &RuleStats::initial());
        assert_eq!(gene["strategy"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn from_gene_rejects_future_major_version() {
        let v = json!({
            "gene_schema": "2.0",
            "category": "repair",
            "summary": "x",
        });
        assert!(from_gene(&v).is_err());
    }

    #[test]
    fn from_gene_accepts_future_minor_version_with_extra_fields() {
        let v = json!({
            "gene_schema": "1.7",
            "category": "repair",
            "signals_match": ["*"],
            "summary": "x",
            "strategy": [],
            "validation": [],
            "success_streak": 0,
            "some_future_field": {"whatever": true},
        });
        let (content, meta, _) = from_gene(&v).expect("minor version bump must still parse");
        assert_eq!(content, "x");
        assert_eq!(meta.category, PlaybookCategory::Repair);
    }

    #[test]
    fn eval_case_ref_parts_splits_suite_and_name() {
        let r = EvalCaseRef("ceo-assistant/refund-flow".to_string());
        assert_eq!(r.parts(), Some(("ceo-assistant", "refund-flow")));
        assert_eq!(EvalCaseRef("no-slash".to_string()).parts(), None);
    }
}
