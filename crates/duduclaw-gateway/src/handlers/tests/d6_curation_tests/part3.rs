//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

#[tokio::test]
pub(super) async fn memory_invalidate_origin_happy_path() {
    let home = tempfile::tempdir().unwrap();
    let agent = "agent-purge";
    seed_agent_memory(home.path(), agent).await;
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    // Purge the bad channel — exactly one currently-valid fact came from it.
    let frame = handler
        .handle_memory_invalidate_origin(json!({ "agent_id": agent, "origin": "chan-bad" }))
        .await;
    assert!(frame_ok(&frame), "rollback must succeed: {frame:?}");
    let data = frame_data(&frame);
    assert_eq!(data.get("expired").and_then(|v| v.as_u64()), Some(1));

    // The graph now excludes the expired fact (currently-valid only).
    let g = handler
        .handle_memory_graph(json!({ "agent_id": agent }))
        .await;
    let edges = frame_data(&g)
        .get("edges")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(edges.len(), 1, "expired fact drops out of the graph");
}

#[tokio::test]
pub(super) async fn memory_invalidate_origin_missing_origin_fails() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle_memory_invalidate_origin(json!({ "agent_id": "agent-x" }))
        .await;
    assert!(!frame_ok(&frame), "missing origin must be rejected");
}

#[tokio::test]
pub(super) async fn memory_invalidate_origin_bad_since_fails() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle_memory_invalidate_origin(json!({
            "agent_id": "agent-x", "origin": "chan", "since": "not-a-date"
        }))
        .await;
    assert!(!frame_ok(&frame), "malformed since must be rejected");
}

#[test]
pub(super) fn retrieval_weights_default_when_config_absent() {
    let w = memory_retrieval_weights_from_table(None);
    let d = duduclaw_memory::engine::RetrievalWeights::default();
    assert_eq!(w.graph_embed_seed, d.graph_embed_seed);
    assert_eq!(w.graph_embed_seed_top_k, d.graph_embed_seed_top_k);
    assert!(
        !w.graph_embed_seed,
        "seed off by default (behaviour unchanged)"
    );
}

#[test]
pub(super) fn retrieval_weights_override_from_config() {
    let table: toml::Table = "[memory]\ngraph_embed_seed = true\ngraph_embed_seed_top_k = 8\n"
        .parse()
        .unwrap();
    let w = memory_retrieval_weights_from_table(Some(&table));
    assert!(w.graph_embed_seed);
    assert_eq!(w.graph_embed_seed_top_k, 8);
}

#[test]
pub(super) fn retrieval_weights_top_k_floored_at_one() {
    let table: toml::Table = "[memory]\ngraph_embed_seed_top_k = 0\n".parse().unwrap();
    let w = memory_retrieval_weights_from_table(Some(&table));
    assert_eq!(w.graph_embed_seed_top_k, 1, "top-k floored at 1");
}
