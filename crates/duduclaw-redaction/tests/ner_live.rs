//! Live NER tests — they drive the real 945 MB OpenAI Privacy Filter model
//! through the real ONNX Runtime.
//!
//! These SKIP (loudly, with the reason) when the model or the runtime library
//! is not installed, so a clean checkout still runs `cargo test`. They are the
//! only tests that prove the pinned manifest, the dynamic ORT load, the
//! tokenizer offsets and the Viterbi decode actually agree with each other —
//! everything else in the crate tests those pieces in isolation.
//!
//! Install with the dashboard's 「下載模型」 (`redaction.model.install`), or by
//! placing the manifest's files under `~/.duduclaw/models/privacy-filter` and
//! the ONNX Runtime library under `~/.duduclaw/lib/onnxruntime/<ver>/`.

#![cfg(feature = "ner")]

use std::collections::BTreeSet;

use duduclaw_redaction::ner::install::{InstallDirs, installed_problem};
use duduclaw_redaction::ner::runtime::NerEngine;
use duduclaw_redaction::ner::{DEFAULT_MAX_CHARS, NerConfig};
use duduclaw_redaction::{RestoreScope, RuleKind, RuleSpec};

fn home() -> Option<std::path::PathBuf> {
    std::env::var_os("DUDUCLAW_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".duduclaw")))
}

/// `Some(dirs)` when a real install is present; otherwise print why and skip.
fn installed_dirs() -> Option<InstallDirs> {
    let home = home()?;
    let dirs = InstallDirs::under_home(&home);
    match installed_problem(&dirs) {
        None => Some(dirs),
        Some(reason) => {
            eprintln!(
                "SKIP (live NER): {reason} — looked under {}",
                dirs.model_dir.display()
            );
            None
        }
    }
}

fn engine() -> Option<NerEngine> {
    let dirs = installed_dirs()?;
    Some(NerEngine::open(&dirs, &NerConfig::default()).expect("installed model must open"))
}

/// `(label, text)` pairs for every span the model found.
fn spans_of(engine: &NerEngine, text: &str) -> Vec<(String, String)> {
    engine
        .spans(text, DEFAULT_MAX_CHARS)
        .expect("inference must succeed")
        .iter()
        .map(|s| (s.label.clone(), text[s.start..s.end].to_string()))
        .collect()
}

fn labels_found(hits: &[(String, String)]) -> BTreeSet<&str> {
    hits.iter().map(|(l, _)| l.as_str()).collect()
}

/// The span texts for one label, in order.
fn texts_for<'a>(hits: &'a [(String, String)], label: &str) -> Vec<&'a str> {
    hits.iter()
        .filter(|(l, _)| l == label)
        .map(|(_, t)| t.as_str())
        .collect()
}

#[test]
fn zh_tw_sentence_with_a_name_a_date_a_phone_and_an_email() {
    let Some(engine) = engine() else { return };
    let text = "客戶王小明於1990年3月5日來電，電話 0912-345-678，信箱 xiaoming.wang@example.com.tw。";
    let hits = spans_of(&engine, text);
    eprintln!("spans: {hits:?}");

    let labels = labels_found(&hits);
    for want in ["private_person", "private_phone", "private_email"] {
        assert!(labels.contains(want), "missing {want} in {hits:?}");
    }

    assert!(
        texts_for(&hits, "private_person").iter().any(|t| t.contains("王小明")),
        "person span must cover the name: {hits:?}"
    );
    assert_eq!(texts_for(&hits, "private_phone"), vec!["0912-345-678"]);
    assert!(
        texts_for(&hits, "private_email")
            .iter()
            .any(|t| t.trim() == "xiaoming.wang@example.com.tw"),
        "email span must cover the address (the tokenizer includes the leading \
         space, which is harmless): {hits:?}"
    );

    // The birthday must be COVERED, but not necessarily as its own
    // `private_date` span. The G0 evaluation recorded the model merging an
    // adjacent name and date into one span on zh-TW text, and it does exactly
    // that here ("王小明於1990年3月5日"). Asserting a separate date label would
    // be asserting a behaviour the model does not have; asserting coverage is
    // what actually protects the data.
    assert!(
        hits.iter().any(|(_, t)| t.contains("1990年3月5日")),
        "the birthday must be inside some span: {hits:?}"
    );
}

#[test]
fn zh_tw_address_and_account_number() {
    let Some(engine) = engine() else { return };
    let text = "收件地址是台北市信義區松仁路100號8樓，匯款帳號 012-34567890123，請於今日完成。";
    let hits = spans_of(&engine, text);
    eprintln!("spans: {hits:?}");

    let labels = labels_found(&hits);
    assert!(
        labels.contains("private_address"),
        "address must be detected: {hits:?}"
    );
    assert!(
        texts_for(&hits, "private_address")
            .iter()
            .any(|t| t.contains("信義區松仁路")),
        "address span must cover the street: {hits:?}"
    );
    // The account number is caught, but the G0 spike measured Taiwanese bank
    // accounts being mislabelled as phone numbers ~40% of the time. Both
    // categories are redacted, so assert only that SOMETHING covers it —
    // asserting the label would encode a known model weakness as a contract.
    assert!(
        hits.iter().any(|(_, t)| t.contains("012-34567890123")),
        "the account number must be covered by some span: {hits:?}"
    );
}

#[test]
fn every_span_lands_on_a_char_boundary_of_the_original_text() {
    let Some(engine) = engine() else { return };
    let text = "聯絡人：陳雅婷（0987654321），地址新北市板橋區文化路一段188號3樓，Email ya.ting.chen@gmail.com";
    let spans = engine.spans(text, DEFAULT_MAX_CHARS).unwrap();
    assert!(!spans.is_empty(), "this sentence must produce spans");
    for s in spans.iter() {
        assert!(
            text.is_char_boundary(s.start) && text.is_char_boundary(s.end),
            "span {s:?} is not char-aligned — slicing it would panic"
        );
        assert!(s.end > s.start && s.end <= text.len());
    }
    eprintln!(
        "spans: {:?}",
        spans
            .iter()
            .map(|s| (s.label.as_str(), &text[s.start..s.end]))
            .collect::<Vec<_>>()
    );
}

#[test]
fn the_cache_makes_a_repeat_scan_free() {
    let Some(engine) = engine() else { return };
    let text = "客戶林志豪的生日是1985/07/21，電話 (02)2345-6789。";

    let t0 = std::time::Instant::now();
    let first = engine.spans(text, DEFAULT_MAX_CHARS).unwrap();
    let cold = t0.elapsed();

    let t1 = std::time::Instant::now();
    let second = engine.spans(text, DEFAULT_MAX_CHARS).unwrap();
    let warm = t1.elapsed();

    assert_eq!(*first, *second, "cached result must be identical");
    assert!(
        warm.as_micros() * 10 < cold.as_micros().max(1),
        "cache hit ({warm:?}) should be orders of magnitude under the cold run ({cold:?})"
    );
    eprintln!("cold {cold:?} / warm {warm:?} / spans {}", first.len());
}

#[test]
fn a_ner_rule_produces_matches_for_its_own_label_only() {
    let Some(dirs) = installed_dirs() else { return };
    let spec = RuleSpec {
        id: "ai_pii".into(),
        category: "PII".into(),
        restore_scope: RestoreScope::Owner,
        priority: 30,
        cross_session_stable: false,
        apply_to_system_prompt: false,
        enabled: true,
        kind: RuleKind::Ner {
            labels: vec!["private_person".into(), "private_phone".into()],
            min_chars: Some(8),
            max_chars: None,
        },
    };
    let rules = duduclaw_redaction::rules::ner::NerRule::compile(spec, &dirs, &NerConfig::default())
        .expect("compile with an installed model");
    assert_eq!(rules.len(), 2, "one rule per requested label");

    let text = "客戶王小明於1990年3月5日來電，電話 0912-345-678。";
    let by_cat: std::collections::BTreeMap<&str, Vec<String>> = rules
        .iter()
        .map(|r| {
            (
                r.category(),
                r.match_text(text).into_iter().map(|m| m.original).collect(),
            )
        })
        .collect();
    eprintln!("{by_cat:?}");

    assert!(
        by_cat["PERSON"].iter().any(|t| t.contains("王小明")),
        "{by_cat:?}"
    );
    assert_eq!(by_cat["PHONE"], vec!["0912-345-678".to_string()]);
    assert!(
        !by_cat.contains_key("DATE"),
        "an unrequested label must produce no rule at all: {by_cat:?}"
    );
}

#[test]
fn min_chars_keeps_short_strings_away_from_the_model() {
    let Some(dirs) = installed_dirs() else { return };
    let spec = RuleSpec {
        id: "ai_pii".into(),
        category: "PII".into(),
        restore_scope: RestoreScope::Owner,
        priority: 30,
        cross_session_stable: false,
        apply_to_system_prompt: false,
        enabled: true,
        kind: RuleKind::Ner {
            labels: vec!["private_person".into()],
            min_chars: Some(40),
            max_chars: None,
        },
    };
    let rules =
        duduclaw_redaction::rules::ner::NerRule::compile(spec, &dirs, &NerConfig::default()).unwrap();
    assert!(
        rules[0].match_text("王小明").is_empty(),
        "text under min_chars must not reach the model"
    );
}

#[test]
fn latency_is_in_the_measured_envelope() {
    let Some(engine) = engine() else { return };
    // Warm the session so we time inference, not model load.
    let _ = engine.spans("暖機用的句子，內容無關緊要，只是要讓 session 載入完成。", DEFAULT_MAX_CHARS);

    let sentences = [
        "客戶王小明於1990年3月5日來電，電話 0912-345-678，信箱 test@example.com.tw。",
        "收件地址是台北市信義區松仁路100號8樓，匯款帳號 012-34567890123。",
        "聯絡人陳雅婷，手機 0987654321，公司網址 https://portal.example.com.tw/user/93321。",
    ];
    let mut times = Vec::new();
    for (i, s) in sentences.iter().enumerate() {
        // Distinct text per iteration so the cache never answers.
        let text = format!("{s}（第 {i} 則）");
        let t0 = std::time::Instant::now();
        let hits = engine.spans(&text, DEFAULT_MAX_CHARS).unwrap();
        times.push(t0.elapsed().as_millis());
        assert!(!hits.is_empty(), "sentence {i} produced no spans");
    }
    eprintln!("per-sentence latency ms: {times:?}");
    let worst = times.iter().copied().max().unwrap_or(0);
    // The G0 spike measured p95 161 ms for zh-TW short sentences on an
    // M-series CPU. A very loose ceiling here: this asserts "not pathological"
    // (e.g. weights swapped out), not a performance target.
    assert!(worst < 5_000, "inference took {worst} ms — something is wrong");

    let stats = duduclaw_redaction::ner::runtime::global_stats().snapshot();
    eprintln!("stats: {stats:?}");
    assert!(stats.calls >= sentences.len() as u64);
    assert!(stats.avg_latency_ms.is_some());
    assert!(stats.loaded);
}
