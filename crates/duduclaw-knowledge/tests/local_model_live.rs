//! Live checks against the real pinned models. Skipped unless
//! `DUDUCLAW_KB_LIVE_HOME` names a DuDuClaw home; with
//! `DUDUCLAW_KB_LIVE_INSTALL=1` the default model (and ONNX Runtime) is
//! downloaded into it first (public Hugging Face / GitHub files).
//!
//! ```sh
//! DUDUCLAW_KB_LIVE_HOME=/tmp/kbhome DUDUCLAW_KB_LIVE_INSTALL=1 \
//!   cargo test -p duduclaw-knowledge --test local_model_live -- --nocapture --test-threads=1
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use duduclaw_knowledge::embed::catalog::{self, ModelSpec};
use duduclaw_knowledge::embed::local_onnx::LocalOnnxEmbedder;
use duduclaw_knowledge::embed::pooling::dot;
use duduclaw_knowledge::embed::{Embedder, install};

fn live_home() -> Option<PathBuf> {
    let h = std::env::var_os("DUDUCLAW_KB_LIVE_HOME")?;
    Some(PathBuf::from(h))
}

fn embedder(spec: &ModelSpec, home: &std::path::Path) -> Option<LocalOnnxEmbedder> {
    if let Some(problem) = install::installed_problem(spec, home) {
        eprintln!("skip {}: {problem}", spec.id);
        return None;
    }
    Some(LocalOnnxEmbedder::new(
        spec,
        home,
        install::ort_lib(home)?,
        4,
        8,
    ))
}

#[tokio::test(flavor = "multi_thread")]
async fn install_default_model_when_asked() {
    let Some(home) = live_home() else { return };
    if std::env::var("DUDUCLAW_KB_LIVE_INSTALL").as_deref() != Ok("1") {
        return;
    }
    let spec = catalog::find(catalog::DEFAULT_MODEL).unwrap();
    let progress: install::ProgressFn = Arc::new(|_| {});
    install::install(spec, &home, progress, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    assert!(install::installed_problem(spec, &home).is_none());
}

/// `DUDUCLAW_KB_LIVE_VERIFY=<catalog id>`: files copied in by hand (the
/// offline path) are hashed and the marker written.
#[test]
fn verify_offline_when_asked() {
    let Some(home) = live_home() else { return };
    let Ok(id) = std::env::var("DUDUCLAW_KB_LIVE_VERIFY") else {
        return;
    };
    let spec = catalog::find(&id).unwrap();
    install::verify_offline(spec, &home).unwrap();
    assert!(install::installed_problem(spec, &home).is_none());
}

#[test]
fn e5_ranks_the_answer_above_unrelated_text() {
    let Some(home) = live_home() else { return };
    let Some(e) = embedder(catalog::find("multilingual-e5-small").unwrap(), &home) else {
        return;
    };
    let q = e.embed_query("新進員工第一年有幾天特休？").unwrap();
    let docs = e
        .embed_passages(&[
            "員工到職滿六個月以上未滿一年者，給予特別休假三日；滿一年以上未滿二年者，給予七日。",
            "防滑安全鞋 QS-310 單價新台幣一千二百元，最小訂購量十雙。",
            "資訊安全政策：密碼至少十二碼，並每九十天更換一次。",
        ])
        .unwrap();
    assert_eq!(q.len(), 384);
    assert!((dot(&q, &q) - 1.0).abs() < 1e-3, "unit vector");
    let s: Vec<f32> = docs.iter().map(|d| dot(&q, d)).collect();
    eprintln!("e5 scores {s:?}");
    assert!(s[0] > s[1] && s[0] > s[2], "{s:?}");
}

/// The Qwen3-Embedding model card prints these similarities for its
/// example (fp32, 1024 dims): [[0.7646, 0.1414], [0.1355, 0.6000]]. The int8
/// export must land close to them at full width, which checks last-token
/// pooling, the appended `<|endoftext|>` and the instruction format.
/// Measured 2026-10-22 (int8, one text per pass): [[0.759, 0.189],
/// [0.198, 0.616]] — int8 quantisation moves each value by ≤ 0.07.
#[test]
fn qwen3_matches_the_model_card_example() {
    let Some(home) = live_home() else { return };
    let mut spec = catalog::find("qwen3-embedding-0.6b").unwrap().clone();
    spec.dim = spec.native_dim; // compare at full width like the card
    spec.query_prefix =
        "Instruct: Given a web search query, retrieve relevant passages that answer the query\nQuery:".into();
    let Some(e) = embedder(&spec, &home) else {
        return;
    };
    let q = [
        e.embed_query("What is the capital of China?").unwrap(),
        e.embed_query("Explain gravity").unwrap(),
    ];
    let d = e
        .embed_passages(&[
            "The capital of China is Beijing.",
            "Gravity is a force that attracts two bodies towards each other. It gives weight to physical objects and is responsible for the movement of planets around the sun.",
        ])
        .unwrap();
    let got = [
        [dot(&q[0], &d[0]), dot(&q[0], &d[1])],
        [dot(&q[1], &d[0]), dot(&q[1], &d[1])],
    ];
    eprintln!("qwen3 scores {got:?}");
    let card = [[0.7646f32, 0.1414], [0.1355, 0.6000]];
    for i in 0..2 {
        for j in 0..2 {
            assert!(
                (got[i][j] - card[i][j]).abs() < 0.08,
                "{got:?} vs card {card:?}"
            );
        }
    }
    assert!(
        got[0][0] > got[0][1] + 0.3 && got[1][1] > got[1][0] + 0.3,
        "{got:?}"
    );
}

/// Padding must not change a vector: a short text embedded alone and next
/// to a much longer one (same batch, right-padded) gives the same vector.
#[test]
fn padding_does_not_change_vectors() {
    let Some(home) = live_home() else { return };
    for id in ["multilingual-e5-small", "qwen3-embedding-0.6b"] {
        let Some(e) = embedder(catalog::find(id).unwrap(), &home) else {
            continue;
        };
        let short = "請假需於三日前提出。";
        let long = "員工到職滿六個月以上未滿一年者，給予特別休假三日；滿一年以上未滿二年者，給予七日；滿二年以上未滿三年者，給予十日。".repeat(3);
        let alone = e.embed_passages(&[short]).unwrap().remove(0);
        let batched = e.embed_passages(&[short, long.as_str()]).unwrap().remove(0);
        let c = dot(&alone, &batched);
        eprintln!("{id}: alone vs batched cosine {c}");
        assert!(c > 0.999, "{id}: {c}");
    }
}
