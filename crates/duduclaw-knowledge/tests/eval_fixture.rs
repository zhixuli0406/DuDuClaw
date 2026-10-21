//! Keyword-retrieval regression on the synthetic zh-TW set in
//! `tests/fixtures/eval/` (fictional documents). Extraction runs in-process
//! here; `duduclaw knowledge eval` runs the same code through the
//! extraction child process.

use std::path::Path;
use std::time::Instant;

use duduclaw_knowledge::acl::{HumanCtx, Narrowing, effective_collections};
use duduclaw_knowledge::embed::NoEmbedder;
use duduclaw_knowledge::eval::{evaluate, parse_questions};
use duduclaw_knowledge::extract::{Format, extract};
use duduclaw_knowledge::limits::KnowledgeConfig;
use duduclaw_knowledge::sources::{Added, NewSource, SourceKind, add_source};
use duduclaw_knowledge::store::{NewCollection, create_collection};

/// Keyword-only Recall@5 floor for this set. Raise it when retrieval
/// improves; never lower it to make a change pass.
const MIN_RECALL_AT_5: f64 = 0.9;

#[test]
fn synthetic_zh_tw_keyword_baseline() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/eval");
    let mut conn = duduclaw_knowledge::schema::open_in_memory().unwrap();
    let cfg = KnowledgeConfig::default();
    let c = create_collection(
        &mut conn,
        &NewCollection {
            name: "eval".into(),
            ..Default::default()
        },
        "eval",
        &cfg,
    )
    .unwrap();
    let mut docs: Vec<_> = std::fs::read_dir(root.join("docs"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    docs.sort();
    for path in docs {
        let name = path.file_name().unwrap().to_str().unwrap().to_string();
        let format = Format::from_file_name(&name).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let new = NewSource {
            collection_id: c.id.clone(),
            kind: SourceKind::File,
            title: name.clone(),
            original_name: Some(name.clone()),
            url: None,
            format,
            blob_sha256: duduclaw_knowledge::sha256_hex(&bytes),
            bytes: bytes.len() as u64,
            origin: serde_json::json!({}),
            created_by: "eval".into(),
        };
        let Added::New(src) = add_source(&mut conn, &new, &cfg).unwrap() else {
            panic!("duplicate {name}")
        };
        let out = extract(format, &bytes, cfg.max_source_chars).unwrap();
        let prepared = duduclaw_knowledge::chunk::prepare(format, out.segments);
        duduclaw_knowledge::index::write_index(
            &mut conn,
            &src.id,
            &prepared,
            &out.warnings,
            &out.extractor_version,
            &|_| false,
            &cfg,
        )
        .unwrap();
    }
    let questions = parse_questions(&std::fs::read_to_string(root.join("questions.jsonl")).unwrap()).unwrap();
    let allowed = effective_collections(
        &conn,
        &HumanCtx::dashboard("eval", None, false),
        None,
        &Narrowing::default(),
    )
    .unwrap();
    let report = evaluate("keyword", &questions, |q| {
        let t = Instant::now();
        let hits = duduclaw_knowledge::search::search(&conn, &allowed, q, 10, &NoEmbedder)
            .unwrap()
            .hits;
        (hits, t.elapsed())
    });
    eprintln!("{}", serde_json::to_string_pretty(&report).unwrap());
    assert!(
        report.answerable.recall_at_5 >= MIN_RECALL_AT_5,
        "keyword Recall@5 {} < {MIN_RECALL_AT_5}; misses {:?}",
        report.answerable.recall_at_5,
        report.misses
    );
}
