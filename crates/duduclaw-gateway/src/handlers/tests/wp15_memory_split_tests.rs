//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

//! WP15 — `memory.browse` / `memory.search` return two lists: `entries`
//! (memories about the user) and `signals` (the platform's own learning
//! telemetry). These tests pin the *response shape* on both endpoints,
//! including the db-not-found early returns, so a caller never has to
//! special-case a missing `signals` key.
use super::*;

fn payload(frame: WsFrame) -> Value {
    match frame {
        WsFrame::Response {
            ok: true,
            payload: Some(d),
            ..
        } => d,
        other => panic!("expected an ok response, got {other:?}"),
    }
}

/// Contents of the two lists, as string vectors, for readable assertions.
fn lists(body: &Value) -> (Vec<String>, Vec<String>) {
    let pluck = |key: &str| -> Vec<String> {
        body.get(key)
            .and_then(|v| v.as_array())
            .unwrap_or_else(|| panic!("`{key}` must be present and an array: {body}"))
            .iter()
            .map(|e| e["content"].as_str().unwrap_or_default().to_string())
            .collect()
    };
    (pluck("entries"), pluck("signals"))
}

fn entry(agent: &str, content: &str, source_event: &str) -> duduclaw_core::types::MemoryEntry {
    duduclaw_core::types::MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: agent.to_string(),
        content: content.to_string(),
        timestamp: Utc::now(),
        tags: vec![],
        embedding: None,
        layer: Default::default(),
        importance: 5.0,
        access_count: 0,
        last_accessed: None,
        source_event: source_event.to_string(),
    }
}

const TELEMETRY: &str = "Prediction deviation: expected satisfaction 0.70, inferred 0.52 \
                             (delta 0.18). Topic surprise: 1.00. Corrections: yes. Follow-ups: no.";
const MEMORY: &str = "The satisfaction survey for Acme is due on Friday";

async fn seed(home: &std::path::Path, agent: &str) {
    let db = home.join("agents").join(agent).join("memory.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let engine = SqliteMemoryEngine::new(&db).unwrap();
    engine
        .store(agent, entry(agent, MEMORY, "conversation_summary"))
        .await
        .unwrap();
    engine
        .store(agent, entry(agent, TELEMETRY, "prediction_episodic"))
        .await
        .unwrap();
}

#[tokio::test]
async fn browse_returns_both_lists_when_the_db_is_missing() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let body = payload(
        handler
            .handle_memory_browse(json!({ "agent_id": "no-such-agent" }))
            .await,
    );
    assert_eq!(lists(&body), (vec![], vec![]));
}

#[tokio::test]
async fn search_returns_both_lists_when_the_db_is_missing() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let body = payload(
        handler
            .handle_memory_search(json!({ "agent_id": "no-such-agent", "query": "anything" }))
            .await,
    );
    // The early return must be shaped exactly like `memory.browse`'s.
    assert_eq!(lists(&body), (vec![], vec![]));
}

#[tokio::test]
async fn browse_files_telemetry_under_signals() {
    let home = tempfile::tempdir().unwrap();
    let agent = "wp15-browse";
    seed(home.path(), agent).await;
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let body = payload(
        handler
            .handle_memory_browse(json!({ "agent_id": agent, "limit": 50 }))
            .await,
    );
    let (entries, signals) = lists(&body);
    assert_eq!(entries, vec![MEMORY.to_string()]);
    assert_eq!(signals, vec![TELEMETRY.to_string()]);
}

#[tokio::test]
async fn search_splits_its_results_the_same_way() {
    let home = tempfile::tempdir().unwrap();
    let agent = "wp15-search";
    seed(home.path(), agent).await;
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    // "satisfaction" appears in both rows, so the split — not the query —
    // is what keeps the telemetry out of `entries`.
    let body = payload(
        handler
            .handle_memory_search(json!({
                "agent_id": agent,
                "query": "satisfaction",
                "limit": 50,
            }))
            .await,
    );
    let (entries, signals) = lists(&body);
    assert!(
        entries
            .iter()
            .all(|c| !c.starts_with("Prediction deviation")),
        "telemetry must never reach the memory list: {entries:?}"
    );
    assert_eq!(entries, vec![MEMORY.to_string()]);
    assert_eq!(signals, vec![TELEMETRY.to_string()]);
}
