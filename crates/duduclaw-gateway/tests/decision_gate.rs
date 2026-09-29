//! Route-level regression tests for the `config.toml [decision]` kill switch
//! (2026-09-29 feature audit, X1 方案 6).
//!
//! Kept as an integration test rather than an inline `#[cfg(test)] mod tests`
//! so it exercises the module exactly as `server.rs` does — through the crate's
//! public surface, driving a real `axum::Router` with the real middleware.

use duduclaw_gateway::decision_gate::{
    DecisionConfig, decision_surface_gate, is_decision_path,
};

use axum::body::Body;
use axum::http::{Request, StatusCode};


#[test]
fn defaults_to_enabled_when_no_config_file() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = DecisionConfig::from_home(dir.path());
    assert!(
        cfg.enabled,
        "no config.toml ⇒ the decision surface stays mounted"
    );
    assert_eq!(cfg, DecisionConfig::default());
}

#[test]
fn defaults_to_enabled_when_section_absent() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), "[gateway]\nport = 18789\n").unwrap();
    assert!(DecisionConfig::from_home(dir.path()).enabled);
}

#[test]
fn explicit_false_is_honored() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        "[decision]\nenabled = false\n",
    )
    .unwrap();
    assert!(!DecisionConfig::from_home(dir.path()).enabled);
}

#[test]
fn malformed_section_falls_back_to_default() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        "[decision]\nenabled = \"not-a-bool\"\n",
    )
    .unwrap();
    assert!(
        DecisionConfig::from_home(dir.path()).enabled,
        "a malformed section must not brick the surface — it degrades to the default"
    );
}

#[test]
fn malformed_config_file_falls_back_to_default() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.toml"), "this is not toml {{{").unwrap();
    assert!(DecisionConfig::from_home(dir.path()).enabled);
}

#[test]
fn decision_path_matching_is_anchored() {
    // The surface itself.
    assert!(is_decision_path("/api/decision"));
    assert!(is_decision_path("/api/decision/overview"));
    assert!(is_decision_path("/api/decision/shadow-policy/create"));
    // Neighbours that merely share a prefix must NOT be gated
    // (CLAUDE.md coding convention 2).
    assert!(!is_decision_path("/api/decisions"));
    assert!(!is_decision_path("/api/decision-lab"));
    assert!(!is_decision_path("/api/decisionoverview"));
    assert!(!is_decision_path("/api/causal/claims"));
    assert!(!is_decision_path("/api/ccr/dashboard"));
    assert!(!is_decision_path("/"));
}

// ── Regression: the gate actually 404s the surface when off ──

async fn ok_handler() -> &'static str {
    "reached the handler"
}

fn app(enabled: bool) -> axum::Router {
    axum::Router::new()
        .route("/api/decision/overview", axum::routing::get(ok_handler))
        .route("/api/decisions", axum::routing::get(ok_handler))
        .route("/api/causal/claims", axum::routing::get(ok_handler))
        .layer(axum::middleware::from_fn_with_state(
            enabled,
            decision_surface_gate,
        ))
}

async fn status_of(enabled: bool, path: &str) -> StatusCode {
    use tower::ServiceExt;
    let res = app(enabled)
        .oneshot(
            Request::builder()
                .uri(path)
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router responds");
    res.status()
}

#[tokio::test]
async fn gate_off_404s_the_decision_surface_only() {
    assert_eq!(
        status_of(false, "/api/decision/overview").await,
        StatusCode::NOT_FOUND,
        "[decision] enabled = false ⇒ the surface is gone"
    );
    assert_eq!(
        status_of(false, "/api/decisions").await,
        StatusCode::OK,
        "a prefix-sharing neighbour must stay reachable"
    );
    assert_eq!(
        status_of(false, "/api/causal/claims").await,
        StatusCode::OK,
        "the causal line has its own switch — this gate must not touch it"
    );
}

#[tokio::test]
async fn gate_on_is_byte_identical_to_no_gate() {
    assert_eq!(status_of(true, "/api/decision/overview").await, StatusCode::OK);
    assert_eq!(status_of(true, "/api/decisions").await, StatusCode::OK);
    assert_eq!(status_of(true, "/api/causal/claims").await, StatusCode::OK);
}
