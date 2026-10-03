//! R1 (2026-10) — Gemini CLI runtime deprecation on the dashboard write
//! paths: `agents.create` / `agents.update` still write a deprecated runtime,
//! but every such write leaves a `runtime_provider_deprecated` audit row that
//! names the caller; a non-deprecated runtime leaves none. The save-time
//! auto-align never writes a deprecated runtime.
use super::*;

fn ok(frame: &WsFrame) -> bool {
    matches!(frame, WsFrame::Response { ok: true, .. })
}

fn payload(frame: &WsFrame) -> Value {
    match frame {
        WsFrame::Response { payload: Some(p), .. } => p.clone(),
        other => panic!("no payload: {other:?}"),
    }
}

/// Every `runtime_provider_deprecated` row in the audit log.
fn deprecated_rows(home: &std::path::Path) -> Vec<Value> {
    let Ok(raw) = std::fs::read_to_string(home.join("security_audit.jsonl")) else {
        return Vec::new();
    };
    raw.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["event_type"] == "runtime_provider_deprecated")
        .collect()
}

fn caller() -> UserContext {
    let mut c = UserContext::admin_fallback();
    c.user_id = "user-42".into();
    c
}

fn provider_on_disk(home: &std::path::Path, agent: &str) -> Option<String> {
    let raw = std::fs::read_to_string(home.join("agents").join(agent).join("agent.toml")).ok()?;
    let t: toml::Table = raw.parse().ok()?;
    t.get("runtime")?
        .get("provider")?
        .as_str()
        .map(str::to_string)
}

#[tokio::test]
async fn create_with_gemini_succeeds_and_is_audited_with_the_caller() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = caller();
    let frame = handler
        .handle_agents_create_as(
            json!({
                "name": "gem-agent",
                "display_name": "Gem",
                "runtime": { "provider": "gemini", "fallback": "gemini" },
            }),
            Some(&ctx),
        )
        .await;
    assert!(ok(&frame), "{frame:?}");
    assert_eq!(provider_on_disk(home.path(), "gem-agent").as_deref(), Some("gemini"));

    let rows = deprecated_rows(home.path());
    assert_eq!(rows.len(), 2, "one row per deprecated field: {rows:?}");
    let fields: Vec<&str> = rows.iter().map(|r| r["details"]["field"].as_str().unwrap()).collect();
    assert!(fields.contains(&"runtime.provider") && fields.contains(&"runtime.fallback"));
    for r in &rows {
        assert_eq!(r["agent_id"], "gem-agent");
        let d = &r["details"];
        assert_eq!(d["agent_id"], "gem-agent");
        assert_eq!(d["value"], "gemini");
        assert_eq!(d["replacement"], "antigravity");
        assert_eq!(d["remove_in"], "v1.70.0");
        assert_eq!(d["source"], "agents.create");
        assert_eq!(d["user_id"], "user-42");
    }
}

#[tokio::test]
async fn create_with_a_live_runtime_is_not_audited() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = caller();
    let frame = handler
        .handle_agents_create_as(
            json!({
                "name": "agy-agent",
                "display_name": "Agy",
                "runtime": { "provider": "antigravity", "fallback": "claude" },
            }),
            Some(&ctx),
        )
        .await;
    assert!(ok(&frame), "{frame:?}");
    assert!(deprecated_rows(home.path()).is_empty());
}

#[tokio::test]
async fn update_to_gemini_succeeds_and_is_audited_other_values_are_not() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = caller();
    assert!(ok(&handler
        .handle_agents_create_as(json!({ "name": "upd", "display_name": "Upd" }), Some(&ctx))
        .await));
    assert!(deprecated_rows(home.path()).is_empty());

    // A gemini model keeps the provider/model pair consistent, so the
    // auto-align has nothing to do and the written value is what lands.
    let frame = handler
        .handle_agents_update_as(
            json!({
                "agent_id": "upd",
                "preferred": "gemini-2.5-pro",
                "runtime": { "provider": "gemini" },
            }),
            Some(&ctx),
        )
        .await;
    assert!(ok(&frame), "{frame:?}");
    assert_eq!(provider_on_disk(home.path(), "upd").as_deref(), Some("gemini"));
    let rows = deprecated_rows(home.path());
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["details"]["source"], "agents.update");
    assert_eq!(rows[0]["details"]["field"], "runtime.provider");
    assert_eq!(rows[0]["details"]["user_id"], "user-42");
    let p = payload(&frame);
    assert!(p.get("runtime_provider_align_skipped").is_some_and(|v| v.is_null()), "{p}");

    // A non-deprecated value adds no row.
    let frame = handler
        .handle_agents_update_as(
            json!({
                "agent_id": "upd",
                "preferred": "claude-sonnet-4-6",
                "runtime": { "provider": "claude" },
            }),
            Some(&ctx),
        )
        .await;
    assert!(ok(&frame), "{frame:?}");
    assert_eq!(deprecated_rows(home.path()).len(), 1);
}

#[test]
fn auto_align_never_targets_a_deprecated_runtime() {
    use duduclaw_core::types::RuntimeType;
    use crate::handlers::agents_update::auto_align_target;
    assert_eq!(
        auto_align_target(Some(RuntimeType::Gemini)),
        (None, Some("deprecated_runtime"))
    );
    for rt in [RuntimeType::Claude, RuntimeType::OpenAiCompat, RuntimeType::Antigravity] {
        assert_eq!(auto_align_target(Some(rt)), (Some(rt), None));
    }
    assert_eq!(auto_align_target(None), (None, None));
}

#[test]
fn audit_payload_shape_is_stable() {
    use super::super::runtime_apply::{DeprecatedRuntimeWrite, deprecated_runtime_audit_details};
    let w = DeprecatedRuntimeWrite {
        field: "fallback",
        value: "gemini",
        replacement: "antigravity",
        remove_in: "v1.70.0",
    };
    assert_eq!(
        deprecated_runtime_audit_details("a1", "agents.update", "u1", &w),
        json!({
            "agent_id": "a1",
            "field": "runtime.fallback",
            "value": "gemini",
            "replacement": "antigravity",
            "remove_in": "v1.70.0",
            "source": "agents.update",
            "user_id": "u1",
        })
    );
}

#[test]
fn runtime_models_detect_row_carries_deprecation_fields() {
    use super::super::runtime_models_rpc::runtime_detect_row;
    for spec in duduclaw_core::runtime_catalog::cli_specs() {
        let row = runtime_detect_row(spec, None, None);
        if spec.id == "gemini" {
            assert_eq!(row["deprecated"], true);
            assert_eq!(row["replacement"], "antigravity");
            assert_eq!(row["remove_in"], "v1.70.0");
        } else {
            assert_eq!(row["deprecated"], false, "{}", spec.id);
            assert!(row["replacement"].is_null(), "{}", spec.id);
            assert!(row["remove_in"].is_null(), "{}", spec.id);
        }
        assert_eq!(row["id"], spec.id);
        assert_eq!(row["installed"], false);
    }
}

/// R1 follow-up: choosing a Gemini model on a Claude-runtime agent saves the
/// model, reports the skipped auto-align in the response and leaves the
/// provider on disk untouched; the skip alone is not a change, so an update
/// that carries no recognised field still fails (C1).
#[tokio::test]
async fn gemini_model_on_a_claude_agent_skips_align_and_no_field_update_still_errors() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = caller();
    assert!(ok(&handler
        .handle_agents_create_as(
            json!({ "name": "claude-agent", "display_name": "C", "runtime": { "provider": "claude" } }),
            Some(&ctx),
        )
        .await));
    let before = provider_on_disk(home.path(), "claude-agent");
    assert_eq!(before.as_deref(), Some("claude"));
    let toml_path = home.path().join("agents/claude-agent/agent.toml");

    // As if the Gemini CLI were installed (the auto-align would otherwise
    // pick openai_compat, which is not deprecated). Unique model id: no
    // other test is affected.
    let model = "gemini-2.5-pro-align-skip-probe";
    crate::runtime_config::FAMILY_CLI_INSTALLED_FOR_MODEL.lock().unwrap().push(model.to_string());
    assert_eq!(
        crate::runtime_config::infer_provider_for_model(model),
        Some(duduclaw_core::types::RuntimeType::Gemini)
    );
    let frame = handler
        .handle_agents_update_as(json!({ "agent_id": "claude-agent", "preferred": model }), Some(&ctx))
        .await;
    assert!(ok(&frame), "{frame:?}");
    assert_eq!(payload(&frame)["runtime_provider_align_skipped"], "deprecated_runtime");
    assert_eq!(provider_on_disk(home.path(), "claude-agent"), before, "provider untouched");
    assert!(deprecated_rows(home.path()).is_empty(), "nothing deprecated was written");

    let on_disk = std::fs::read(&toml_path).unwrap();
    let frame = handler
        .handle_agents_update_as(json!({ "agent_id": "claude-agent", "no_such_field": 1 }), Some(&ctx))
        .await;
    assert!(!ok(&frame), "an update with no recognised field must fail: {frame:?}");
    assert!(format!("{frame:?}").contains("No valid fields to update"), "{frame:?}");
    assert_eq!(std::fs::read(&toml_path).unwrap(), on_disk, "agent.toml not rewritten");
}

/// C2: re-saving an unchanged `gemini` provider while changing only the
/// fallback writes no new `runtime_provider_deprecated` row; changing the
/// provider back to gemini later does.
#[tokio::test]
async fn unchanged_deprecated_provider_is_not_audited_again() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = caller();
    assert!(ok(&handler
        .handle_agents_create_as(
            json!({ "name": "gem", "display_name": "G", "runtime": { "provider": "gemini" } }),
            Some(&ctx),
        )
        .await));
    assert_eq!(deprecated_rows(home.path()).len(), 1);

    let frame = handler
        .handle_agents_update_as(
            json!({ "agent_id": "gem", "preferred": "gemini-2.5-pro", "runtime": { "provider": "gemini", "fallback": "claude" } }),
            Some(&ctx),
        )
        .await;
    assert!(ok(&frame), "{frame:?}");
    assert_eq!(provider_on_disk(home.path(), "gem").as_deref(), Some("gemini"));
    assert_eq!(deprecated_rows(home.path()).len(), 1, "unchanged provider: no new row");

    // A changed deprecated value is still audited.
    let frame = handler
        .handle_agents_update_as(
            json!({ "agent_id": "gem", "preferred": "gemini-2.5-pro", "runtime": { "provider": "gemini", "fallback": "gemini" } }),
            Some(&ctx),
        )
        .await;
    assert!(ok(&frame), "{frame:?}");
    let rows = deprecated_rows(home.path());
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[1]["details"]["field"], "runtime.fallback");
}
