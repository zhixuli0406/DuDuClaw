//! Read-only operator verification against an explicitly supplied tasks DB copy.
use duduclaw_gateway::finetune::dataset::{collect_review_pairs, DatasetSources};
fn main() {
    let path = std::env::args_os().nth(1).expect("usage: dream_rsi_review_pairs <tasks.db copy>");
    let pairs = collect_review_pairs(std::path::Path::new(&path), &DatasetSources::default())
        .expect("review-pair collection failed");
    println!("{}", serde_json::json!({"review_pairs": pairs.len(), "read_only": true}));
}
