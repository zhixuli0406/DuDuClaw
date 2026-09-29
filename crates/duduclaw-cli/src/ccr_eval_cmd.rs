//! Offline synthetic CCR quality and byte-cost baseline.

use std::time::Instant;

use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_gateway::prompt_compression::estimate_tokens;
use duduclaw_llm::{CcrError, CcrRuntime, CcrScope, CcrStore};
use serde::Serialize;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ExpectedMode {
    Lossless,
    Lossy,
    Bypass,
}

impl ExpectedMode {
    fn label(self) -> &'static str {
        match self {
            Self::Lossless => "lossless",
            Self::Lossy => "lossy",
            Self::Bypass => "bypass",
        }
    }
}

struct EvalCase {
    name: &'static str,
    source_tool: &'static str,
    original: String,
    required: &'static str,
    search_query: Option<&'static str>,
    find_query: Option<&'static str>,
    expected: ExpectedMode,
}

#[derive(Serialize)]
struct CaseReport {
    name: &'static str,
    mode: &'static str,
    raw_bytes: usize,
    delivered_bytes: usize,
    retrieved_bytes: usize,
    raw_estimated_text_tokens: u64,
    delivered_estimated_text_tokens: u64,
    retrieved_estimated_text_tokens: u64,
    minimum_control_estimated_text_tokens: u64,
    required_in_delivery: bool,
    required_after_retrieval: bool,
    cross_turn_find_hit: Option<bool>,
    cross_turn_find_exact_phrase: Option<bool>,
    cross_scope_find_denied: Option<bool>,
    expired_find_denied: Option<bool>,
    find_p95_micros: Option<u128>,
    cross_scope_denied: Option<bool>,
    expired_denied: Option<bool>,
    preview_p95_micros: u128,
}

#[derive(Serialize)]
struct EvalReport {
    version: &'static str,
    cases: Vec<CaseReport>,
    raw_bytes: usize,
    delivered_bytes: usize,
    bytes_after_needed_targeted_retrievals: usize,
    delivery_byte_reduction_ratio: f64,
    net_byte_reduction_ratio_after_retrieval: f64,
    estimated_token_method: &'static str,
    raw_estimated_text_tokens: u64,
    delivered_estimated_text_tokens: u64,
    retrieved_estimated_text_tokens: u64,
    minimum_control_estimated_text_tokens: u64,
    minimum_net_estimated_text_tokens: u64,
    minimum_net_estimated_text_token_reduction_ratio: f64,
    targeted_retrievals: usize,
    retrieval_misses: usize,
    cross_turn_find_attempts: usize,
    cross_turn_find_hits: usize,
    cross_turn_find_misses: usize,
    cross_scope_denials: usize,
    expiry_denials: usize,
    cross_scope_find_denials: usize,
    expiry_find_denials: usize,
    quality_cases_passed: usize,
    limitations: Vec<&'static str>,
}

fn error(message: impl std::fmt::Display) -> DuDuClawError {
    DuDuClawError::Gateway(format!("CCR evaluation: {message}"))
}

fn synthetic_cases() -> Vec<EvalCase> {
    let records: Vec<_> = (0..120)
        .map(|id| serde_json::json!({ "id": format!("ticket-{id:03}"), "status": "resolved" }))
        .collect();
    let dense_records: Vec<_> = (0..300)
        .map(|id| serde_json::json!({ "id": id, "exact_value": format!("value-{id:04}") }))
        .collect();
    let table = std::iter::once("ticket_id,wait_minutes,status".to_string())
        .chain((0..400).map(|id| {
            format!(
                "ticket-{id:04},{},resolved",
                if id == 225 { 9000 } else { 5 }
            )
        }))
        .collect::<Vec<_>>()
        .join("\n");
    vec![
        EvalCase {
            name: "exact_json_value",
            source_tool: "read_json",
            original: serde_json::to_string_pretty(&records)
                .expect("synthetic JSON")
                .replace('\n', "\n          "),
            required: "ticket-077",
            search_query: None,
            find_query: None,
            expected: ExpectedMode::Lossless,
        },
        EvalCase {
            name: "duplicate_key_numeric_lexeme",
            source_tool: "read_json",
            original: format!(
                "{{\n{}\n\"priority\": 1.0e+02,\n\"priority\": 100,\n\"label\": \"exact-value\"\n}}",
                " ".repeat(9_000)
            ),
            required: "1.0e+02",
            search_query: None,
            find_query: None,
            expected: ExpectedMode::Lossless,
        },
        EvalCase {
            name: "dense_json_bypass",
            source_tool: "read_json",
            original: serde_json::to_string(&dense_records).expect("synthetic JSON"),
            required: "value-0200",
            search_query: None,
            find_query: None,
            expected: ExpectedMode::Bypass,
        },
        EvalCase {
            name: "rare_error",
            source_tool: "logs",
            original: format!(
                "{}ERROR rare failure code 791\n{}",
                "normal\n".repeat(500),
                "normal\n".repeat(500)
            ),
            required: "ERROR rare failure code 791",
            search_query: None,
            find_query: None,
            expected: ExpectedMode::Lossy,
        },
        EvalCase {
            name: "search_middle_hit",
            source_tool: "search",
            original: format!(
                "{}case-791 contains the exact needle\n{}",
                "ordinary result\n".repeat(400),
                "ordinary result\n".repeat(400)
            ),
            required: "case-791 contains the exact needle",
            search_query: Some("exact needle"),
            find_query: None,
            expected: ExpectedMode::Lossy,
        },
        EvalCase {
            name: "search_multi_term_middle",
            source_tool: "search",
            original: format!(
                "{}miss cache billing RESULT-773\n{}",
                "ordinary result\n".repeat(400),
                "ordinary result\n".repeat(400)
            ),
            required: "RESULT-773",
            search_query: Some("billing cache miss"),
            find_query: None,
            expected: ExpectedMode::Lossy,
        },
        EvalCase {
            name: "search_han_middle",
            source_tool: "search",
            original: format!(
                "{}客服目前積壓案件，部分已經逾期；案件 RESULT-HAN-773\n{}",
                "一般紀錄\n".repeat(400),
                "一般紀錄\n".repeat(400)
            ),
            required: "RESULT-HAN-773",
            search_query: Some("客服積壓已逾期"),
            find_query: None,
            expected: ExpectedMode::Lossy,
        },
        EvalCase {
            name: "numeric_outlier",
            source_tool: "table",
            original: table,
            required: "ticket-0225,9000,resolved",
            search_query: None,
            find_query: None,
            expected: ExpectedMode::Lossy,
        },
        EvalCase {
            name: "long_markdown",
            source_tool: "read_document",
            original: format!(
                "# Report\n{}## Root cause\n\nQueue service capacity fell during the shift.\n{}",
                "Background context.\n".repeat(250),
                "Additional details.\n".repeat(250)
            ),
            required: "Queue service capacity fell during the shift.",
            search_query: None,
            find_query: None,
            expected: ExpectedMode::Lossy,
        },
        EvalCase {
            name: "buried_exact_value",
            source_tool: "read_text",
            original: format!(
                "{}UNIQUE-MIDDLE-EXACT-928\n{}",
                "ordinary observation\n".repeat(400),
                "ordinary observation\n".repeat(400)
            ),
            required: "UNIQUE-MIDDLE-EXACT-928",
            search_query: None,
            find_query: None,
            expected: ExpectedMode::Lossy,
        },
        EvalCase {
            name: "cross_turn_multi_term",
            source_tool: "read_text",
            original: format!(
                "{}billing cache miss UNIQUE-LEXICAL-928\n{}",
                "ordinary observation\n".repeat(400),
                "ordinary observation\n".repeat(400)
            ),
            required: "UNIQUE-LEXICAL-928",
            search_query: None,
            find_query: Some("cache miss billing"),
            expected: ExpectedMode::Lossy,
        },
        EvalCase {
            name: "cross_turn_han",
            source_tool: "read_text",
            original: format!(
                "{}客服目前積壓案件，部分已經逾期；唯一值 UNIQUE-HAN-928\n{}",
                "一般觀察\n".repeat(400),
                "一般觀察\n".repeat(400)
            ),
            required: "UNIQUE-HAN-928",
            search_query: None,
            find_query: Some("客服積壓已逾期"),
            expected: ExpectedMode::Lossy,
        },
    ]
}

/// Reuse the same source text and exact-value tasks in the native-loop
/// comparison, so preview checks and task-level checks exercise one fixture.
pub(crate) fn synthetic_task_fixtures() -> Vec<(
    &'static str,
    String,
    &'static str,
    Option<&'static str>,
    Option<&'static str>,
)> {
    synthetic_cases()
        .into_iter()
        .map(|case| {
            (
                case.name,
                case.original,
                case.required,
                case.search_query,
                case.find_query,
            )
        })
        .collect()
}

fn run() -> Result<EvalReport> {
    let dir = tempfile::tempdir().map_err(error)?;
    let store = CcrStore::new(dir.path().join("ccr.db"));
    let scope = CcrScope {
        tenant_id: "synthetic".into(),
        agent_id: "support".into(),
        session_id: "eval".into(),
        source_acl: "eval-private".into(),
    };
    let allowed_sources: Vec<_> = synthetic_cases()
        .iter()
        .map(|case| ("synthetic-eval".to_owned(), case.source_tool.to_owned()))
        .collect();
    let runtime =
        CcrRuntime::new(store.clone(), scope.clone()).restrict_sources(allowed_sources.clone());
    let mut reports = Vec::new();
    for case in synthetic_cases() {
        let find_query = case.find_query.unwrap_or(case.required);
        if !case.original.contains(case.required) {
            return Err(error(format!(
                "fixture missing required value: {}",
                case.name
            )));
        }
        let mut samples = Vec::with_capacity(31);
        for _ in 0..31 {
            let started = Instant::now();
            let _ = runtime.preview_with_query(&case.original, "preview-id", case.search_query);
            samples.push(started.elapsed().as_micros());
        }
        samples.sort_unstable();
        let initial = runtime.preview_with_query(&case.original, "preview-id", case.search_query);
        let mode = match &initial {
            None => ExpectedMode::Bypass,
            Some(preview)
                if CcrRuntime::lossless_compact_structured(&case.original)
                    .is_some_and(|compact| preview.starts_with(compact.as_str())) =>
            {
                ExpectedMode::Lossless
            }
            Some(_) => ExpectedMode::Lossy,
        };
        if mode != case.expected {
            return Err(error(format!("unexpected preview mode for {}", case.name)));
        }
        let mut retrieved_bytes = 0;
        let mut retrieved_text = String::new();
        let mut control_text = String::new();
        let mut cross_turn_find_hit = None;
        let mut cross_turn_find_exact_phrase = None;
        let mut cross_scope_find_denied = None;
        let mut expired_find_denied = None;
        let mut find_p95_micros = None;
        let mut cross_scope_denied = None;
        let mut expired_denied = None;
        let delivered = if initial.is_some() {
            let source_key = runtime
                .source_key_for_call(Some("synthetic-eval"), case.source_tool)
                .ok_or_else(|| error("synthetic source is not allowed"))?;
            let entry = store
                .put(&scope, &source_key, case.name, &case.original)
                .map_err(error)?;
            let preview = runtime
                .preview_with_query(&case.original, &entry.id, case.search_query)
                .ok_or_else(|| error("preview changed after storing"))?;
            let recovered = if preview.contains(case.required) {
                true
            } else {
                let mut find_samples = Vec::with_capacity(31);
                for _ in 0..31 {
                    let started = Instant::now();
                    let hits = runtime.find(find_query, 5).map_err(error)?;
                    find_samples.push(started.elapsed().as_micros());
                    if !hits.iter().any(|hit| hit.id == entry.id) {
                        return Err(error(format!("cross-turn handle not found: {}", case.name)));
                    }
                }
                find_samples.sort_unstable();
                find_p95_micros = Some(find_samples[29]);
                let hits = runtime.find(find_query, 5).map_err(error)?;
                cross_turn_find_hit = Some(hits.iter().any(|hit| hit.id == entry.id));
                let hit = hits
                    .iter()
                    .find(|hit| hit.id == entry.id)
                    .ok_or_else(|| error("cross-turn handle disappeared"))?;
                cross_turn_find_exact_phrase = Some(hit.exact_phrase);
                // Lower-bound control text: the exact query and opaque ID
                // carried by a find/retrieve cycle. Tool-call JSON wrappers,
                // metadata, and provider role overhead are not represented.
                control_text.push_str(find_query);
                control_text.push_str(&hit.id);
                control_text.push_str(&hit.id);
                if hit.exact_phrase {
                    control_text.push_str(find_query);
                } else {
                    control_text.push_str(&hit.byte_offset.to_string());
                }
                let chunk = runtime
                    .retrieve(
                        &hit.id,
                        hit.exact_phrase.then_some(find_query),
                        hit.byte_offset,
                        512,
                    )
                    .map_err(error)?;
                retrieved_bytes = chunk.text.len();
                retrieved_text = chunk.text.clone();
                chunk.text.contains(case.required)
            };
            let other_scope = CcrScope {
                tenant_id: "different-tenant".into(),
                ..scope.clone()
            };
            cross_scope_denied = Some(matches!(
                store.retrieve(&other_scope, &entry.id, None, 0, 512),
                Err(CcrError::NotFound)
            ));
            cross_scope_find_denied = Some(
                CcrRuntime::new(store.clone(), other_scope)
                    .restrict_sources(allowed_sources.clone())
                    .find(find_query, 5)
                    .map_err(error)?
                    .is_empty(),
            );
            let conn = rusqlite::Connection::open(store.path()).map_err(error)?;
            conn.execute(
                "UPDATE ccr_entries SET expires_at=0 WHERE id=?1",
                [&entry.id],
            )
            .map_err(error)?;
            expired_denied = Some(matches!(
                store.retrieve(&scope, &entry.id, None, 0, 512),
                Err(CcrError::NotFound)
            ));
            expired_find_denied = Some(
                runtime
                    .find(find_query, 5)
                    .map_err(error)?
                    .iter()
                    .all(|hit| hit.id != entry.id),
            );
            if mode == ExpectedMode::Lossless {
                let compact = preview.split("\n[CCR:").next().unwrap_or_default();
                let compact_json: serde_json::Value =
                    serde_json::from_str(compact).map_err(error)?;
                let original_json: serde_json::Value =
                    serde_json::from_str(&case.original).map_err(error)?;
                if compact_json != original_json {
                    return Err(error("lossless JSON preview changed a field"));
                }
            }
            (preview, recovered)
        } else {
            (case.original.clone(), true)
        };
        reports.push(CaseReport {
            name: case.name,
            mode: mode.label(),
            raw_bytes: case.original.len(),
            delivered_bytes: delivered.0.len(),
            retrieved_bytes,
            raw_estimated_text_tokens: estimate_tokens(&case.original),
            delivered_estimated_text_tokens: estimate_tokens(&delivered.0),
            retrieved_estimated_text_tokens: estimate_tokens(&retrieved_text),
            minimum_control_estimated_text_tokens: estimate_tokens(&control_text),
            required_in_delivery: delivered.0.contains(case.required),
            required_after_retrieval: delivered.1,
            cross_turn_find_hit,
            cross_turn_find_exact_phrase,
            cross_scope_find_denied,
            expired_find_denied,
            find_p95_micros,
            cross_scope_denied,
            expired_denied,
            preview_p95_micros: samples[29],
        });
    }
    let raw_bytes = reports.iter().map(|case| case.raw_bytes).sum::<usize>();
    let delivered_bytes = reports
        .iter()
        .map(|case| case.delivered_bytes)
        .sum::<usize>();
    let retrieved_bytes = reports
        .iter()
        .map(|case| case.retrieved_bytes)
        .sum::<usize>();
    let raw_estimated_text_tokens = reports
        .iter()
        .map(|case| case.raw_estimated_text_tokens)
        .sum::<u64>();
    let delivered_estimated_text_tokens = reports
        .iter()
        .map(|case| case.delivered_estimated_text_tokens)
        .sum::<u64>();
    let retrieved_estimated_text_tokens = reports
        .iter()
        .map(|case| case.retrieved_estimated_text_tokens)
        .sum::<u64>();
    let minimum_control_estimated_text_tokens = reports
        .iter()
        .map(|case| case.minimum_control_estimated_text_tokens)
        .sum::<u64>();
    let minimum_net_estimated_text_tokens = delivered_estimated_text_tokens
        + retrieved_estimated_text_tokens
        + minimum_control_estimated_text_tokens;
    let targeted_retrievals = reports
        .iter()
        .filter(|case| case.retrieved_bytes > 0)
        .count();
    let retrieval_misses = reports
        .iter()
        .filter(|case| case.mode != "bypass" && !case.required_after_retrieval)
        .count();
    let cross_turn_find_attempts = reports
        .iter()
        .filter(|case| case.cross_turn_find_hit.is_some())
        .count();
    let cross_turn_find_hits = reports
        .iter()
        .filter(|case| case.cross_turn_find_hit == Some(true))
        .count();
    let cross_turn_find_misses = cross_turn_find_attempts - cross_turn_find_hits;
    let cross_scope_denials = reports
        .iter()
        .filter(|case| case.cross_scope_denied == Some(true))
        .count();
    let expiry_denials = reports
        .iter()
        .filter(|case| case.expired_denied == Some(true))
        .count();
    let cross_scope_find_denials = reports
        .iter()
        .filter(|case| case.cross_scope_find_denied == Some(true))
        .count();
    let expiry_find_denials = reports
        .iter()
        .filter(|case| case.expired_find_denied == Some(true))
        .count();
    let quality_cases_passed = reports
        .iter()
        .filter(|case| case.required_in_delivery || case.required_after_retrieval)
        .count();
    Ok(EvalReport {
        version: "ccr-synthetic-eval-v5",
        cases: reports,
        raw_bytes,
        delivered_bytes,
        bytes_after_needed_targeted_retrievals: delivered_bytes + retrieved_bytes,
        delivery_byte_reduction_ratio: 1.0 - delivered_bytes as f64 / raw_bytes as f64,
        net_byte_reduction_ratio_after_retrieval: 1.0
            - (delivered_bytes + retrieved_bytes) as f64 / raw_bytes as f64,
        estimated_token_method: "gateway_cjk_heuristic_text_only_v1",
        raw_estimated_text_tokens,
        delivered_estimated_text_tokens,
        retrieved_estimated_text_tokens,
        minimum_control_estimated_text_tokens,
        minimum_net_estimated_text_tokens,
        minimum_net_estimated_text_token_reduction_ratio: 1.0
            - minimum_net_estimated_text_tokens as f64 / raw_estimated_text_tokens as f64,
        targeted_retrievals,
        retrieval_misses,
        cross_turn_find_attempts,
        cross_turn_find_hits,
        cross_turn_find_misses,
        cross_scope_denials,
        expiry_denials,
        cross_scope_find_denials,
        expiry_find_denials,
        quality_cases_passed,
        limitations: vec![
            "Estimated text tokens use a local CJK heuristic, not provider tokenizer usage or billing.",
            "Minimum control text includes query and handle strings only; actual tool-call wrappers and cache effects are unmeasured.",
            "Synthetic snippets do not measure task success or cost per successful real task.",
            "Preview and find p95 are local microbenchmarks, not end-to-end task latency.",
            "Cache hits, provider pricing, and real task retrieval behavior are not measured.",
        ],
    })
}

pub fn evaluate() -> Result<()> {
    let report = run()?;
    println!("{}", serde_json::to_string_pretty(&report).map_err(error)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_suite_preserves_required_values_and_refuses_acl_expiry_reads() {
        let report = run().unwrap();
        assert_eq!(report.cases.len(), 12);
        assert_eq!(report.quality_cases_passed, 12);
        assert_eq!(report.targeted_retrievals, 3);
        assert_eq!(report.retrieval_misses, 0);
        assert_eq!(report.cross_turn_find_attempts, 3);
        assert_eq!(report.cross_turn_find_hits, 3);
        assert_eq!(report.cross_turn_find_misses, 0);
        assert_eq!(report.cross_scope_denials, 11);
        assert_eq!(report.expiry_denials, 11);
        assert_eq!(report.cross_scope_find_denials, 11);
        assert_eq!(report.expiry_find_denials, 11);
        assert!(
            report
                .cases
                .iter()
                .filter(|case| case.mode != "bypass")
                .all(|case| case.cross_scope_find_denied == Some(true)
                    && case.expired_find_denied == Some(true))
        );
        assert!(report.delivered_bytes < report.raw_bytes);
        assert!(report.net_byte_reduction_ratio_after_retrieval > 0.0);
        assert!(report.raw_estimated_text_tokens > report.delivered_estimated_text_tokens);
        assert_eq!(
            report.minimum_net_estimated_text_tokens,
            report.delivered_estimated_text_tokens
                + report.retrieved_estimated_text_tokens
                + report.minimum_control_estimated_text_tokens
        );
        assert!(report.minimum_net_estimated_text_token_reduction_ratio > 0.0);
        assert!(
            report.net_byte_reduction_ratio_after_retrieval < report.delivery_byte_reduction_ratio
        );
        assert!(
            report
                .cases
                .iter()
                .any(|case| case.name == "dense_json_bypass" && case.mode == "bypass")
        );
        assert!(report.cases.iter().any(|case| {
            case.name == "duplicate_key_numeric_lexeme"
                && case.mode == "lossless"
                && case.required_in_delivery
        }));
        assert!(report.cases.iter().any(|case| {
            case.name == "buried_exact_value"
                && !case.required_in_delivery
                && case.required_after_retrieval
                && case.retrieved_bytes > 0
                && case.retrieved_estimated_text_tokens > 0
                && case.minimum_control_estimated_text_tokens > 0
                && case.cross_turn_find_hit == Some(true)
                && case.cross_turn_find_exact_phrase == Some(true)
                && case.find_p95_micros.is_some()
        }));
        assert!(
            report.cases.iter().any(|case| {
                case.name == "search_multi_term_middle" && case.required_in_delivery
            })
        );
        assert!(report.cases.iter().any(|case| {
            case.name == "cross_turn_multi_term"
                && !case.required_in_delivery
                && case.required_after_retrieval
                && case.cross_turn_find_hit == Some(true)
                && case.cross_turn_find_exact_phrase == Some(false)
                && case.find_p95_micros.is_some()
        }));
        assert!(
            report
                .cases
                .iter()
                .any(|case| { case.name == "search_han_middle" && case.required_in_delivery })
        );
        assert!(report.cases.iter().any(|case| {
            case.name == "cross_turn_han"
                && !case.required_in_delivery
                && case.required_after_retrieval
                && case.cross_turn_find_hit == Some(true)
                && case.cross_turn_find_exact_phrase == Some(false)
        }));
    }
}
