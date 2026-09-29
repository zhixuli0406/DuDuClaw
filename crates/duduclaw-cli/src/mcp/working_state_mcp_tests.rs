use super::*;
use std::fs;

struct TempDir(std::path::PathBuf);
impl TempDir {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("duduclaw-ws-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn parse_ok(value: &Value) -> Value {
    assert!(
        !value
            .get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        "tool returned error: {value}"
    );
    let text = value["content"][0]["text"].as_str().unwrap();
    serde_json::from_str(text).unwrap()
}

fn mk_agent(home: &std::path::Path, id: &str) {
    fs::create_dir_all(home.join("agents").join(id)).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn set_get_clear_roundtrip() {
    let tmp = TempDir::new();
    mk_agent(tmp.path(), "trader");

    let set = handle_working_state_set(
        &serde_json::json!({
            "key": "stop_loss.2317",
            "value": "262",
            "reason": "跌破即出場不猶豫",
            "ttl_hours": 4.5,
        }),
        tmp.path(),
        "trader",
    )
    .await;
    let out = parse_ok(&set);
    assert_eq!(out["ok"], true);
    assert_eq!(out["version"], 1);

    let get = handle_working_state_get(&serde_json::json!({}), tmp.path(), "trader").await;
    let full = parse_ok(&get);
    assert_eq!(full["states"]["stop_loss.2317"]["value"], "262");
    assert_eq!(full["states"]["stop_loss.2317"]["expired"], false);
    assert!(
        full["states"]["stop_loss.2317"]["expires_at"]
            .as_str()
            .is_some()
    );

    let clear = handle_working_state_clear(
        &serde_json::json!({ "key": "stop_loss.2317", "reason": "已出場" }),
        tmp.path(),
        "trader",
    )
    .await;
    let cleared = parse_ok(&clear);
    assert_eq!(cleared["retired_value"], "262");

    // History records both mutations.
    let get2 = handle_working_state_get(&serde_json::json!({}), tmp.path(), "trader").await;
    let full2 = parse_ok(&get2);
    assert_eq!(full2["history"].as_array().unwrap().len(), 2);
}

#[tokio::test(flavor = "current_thread")]
async fn cas_conflict_surfaces_as_tool_error() {
    let tmp = TempDir::new();
    mk_agent(tmp.path(), "trader");
    parse_ok(
        &handle_working_state_set(
            &serde_json::json!({ "key": "k", "value": "262", "reason": "r" }),
            tmp.path(),
            "trader",
        )
        .await,
    );
    let conflict = handle_working_state_set(
        &serde_json::json!({ "key": "k", "value": "254", "reason": "r", "expected_value": "260" }),
        tmp.path(),
        "trader",
    )
    .await;
    assert!(conflict["isError"].as_bool().unwrap_or(false));
    let text = conflict["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        text.contains("262"),
        "CAS error must report the current value: {text}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn handoff_overwrites_and_missing_fields_error() {
    let tmp = TempDir::new();
    mk_agent(tmp.path(), "trader");
    parse_ok(
        &handle_working_state_handoff(
            &serde_json::json!({ "note": "盤中巡檢中，帳務已核對" }),
            tmp.path(),
            "trader",
        )
        .await,
    );
    // Missing note → error, unknown agent → error, missing reason → error.
    let bad = handle_working_state_handoff(&serde_json::json!({}), tmp.path(), "trader").await;
    assert!(bad["isError"].as_bool().unwrap_or(false));
    let ghost = handle_working_state_set(
        &serde_json::json!({ "key": "k", "value": "v", "reason": "r" }),
        tmp.path(),
        "ghost",
    )
    .await;
    assert!(ghost["isError"].as_bool().unwrap_or(false));
    let no_reason = handle_working_state_set(
        &serde_json::json!({ "key": "k", "value": "v" }),
        tmp.path(),
        "trader",
    )
    .await;
    assert!(no_reason["isError"].as_bool().unwrap_or(false));
}

/// H8: the MCP front door parses `status` and routes into the
/// structured Ralph-style validation in `working_state::set_handoff`.
#[tokio::test(flavor = "current_thread")]
async fn handoff_structured_status_validated_end_to_end() {
    let tmp = TempDir::new();
    mk_agent(tmp.path(), "trader");

    // Unknown status string → clear error, no write.
    let bad_status = handle_working_state_handoff(
        &serde_json::json!({ "note": "備註", "status": "done" }),
        tmp.path(),
        "trader",
    )
    .await;
    assert!(bad_status["isError"].as_bool().unwrap_or(false));

    // status=continue without next_steps → rejected.
    let missing_next = handle_working_state_handoff(
        &serde_json::json!({ "note": "追蹤中", "status": "continue" }),
        tmp.path(),
        "trader",
    )
    .await;
    assert!(missing_next["isError"].as_bool().unwrap_or(false));
    let text = missing_next["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        text.contains("next_steps"),
        "error must name the missing field: {text}"
    );

    // Valid structured continue → succeeds and renders into the section.
    let ok = handle_working_state_handoff(
        &serde_json::json!({
            "note": "追蹤中",
            "status": "continue",
            "next_steps": "明早確認持股比例",
        }),
        tmp.path(),
        "trader",
    )
    .await;
    parse_ok(&ok);
    let full = handle_working_state_get(&serde_json::json!({}), tmp.path(), "trader").await;
    let full = parse_ok(&full);
    assert_eq!(full["handoff"]["status"], "continue");
    assert_eq!(full["handoff"]["next_steps"], "明早確認持股比例");

    // Structured fields without status → rejected with a clear reason.
    let no_status = handle_working_state_handoff(
        &serde_json::json!({ "note": "備註", "next_steps": "有下一步但沒給 status" }),
        tmp.path(),
        "trader",
    )
    .await;
    assert!(no_status["isError"].as_bool().unwrap_or(false));

    // Oversized structured payload → rejected, not truncated.
    let huge = "x".repeat(20_000);
    let oversize = handle_working_state_handoff(
        &serde_json::json!({
            "note": "備註",
            "status": "continue",
            "next_steps": huge,
        }),
        tmp.path(),
        "trader",
    )
    .await;
    assert!(oversize["isError"].as_bool().unwrap_or(false));
    let text = oversize["content"][0]["text"].as_str().unwrap_or("");
    assert!(text.contains("過長") && text.contains("bytes"), "{text}");

    // Legacy plain-note call (no status at all) still works unchanged.
    let legacy = handle_working_state_handoff(
        &serde_json::json!({ "note": "純文字交接照舊" }),
        tmp.path(),
        "trader",
    )
    .await;
    parse_ok(&legacy);
    let full2 = handle_working_state_get(&serde_json::json!({}), tmp.path(), "trader").await;
    let full2 = parse_ok(&full2);
    assert_eq!(full2["handoff"]["note"], "純文字交接照舊");
    assert!(full2["handoff"]["status"].is_null());
}

/// Scope table: writes are MemoryWrite, read is MemoryRead — enumerated,
/// never falling through to the Admin default.
#[test]
fn working_state_scopes_enumerated() {
    use crate::mcp_auth::{Scope, tool_requires_scope};
    for tool in [
        "working_state_set",
        "working_state_clear",
        "working_state_handoff",
    ] {
        assert_eq!(
            tool_requires_scope(tool),
            Some(Scope::MemoryWrite),
            "{tool}"
        );
    }
    assert_eq!(
        tool_requires_scope("working_state_get"),
        Some(Scope::MemoryRead)
    );
    // Internal-only: never on the external whitelist.
    for tool in [
        "working_state_set",
        "working_state_clear",
        "working_state_handoff",
        "working_state_get",
    ] {
        assert!(!EXTERNAL_TOOLS_WHITELIST.contains(&tool));
    }
}
