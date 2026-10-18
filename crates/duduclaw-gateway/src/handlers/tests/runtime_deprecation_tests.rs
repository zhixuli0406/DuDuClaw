//! Removed Gemini CLI runtime: dashboard writes reject it, while Gemini
//! models remain usable through Antigravity.
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
async fn create_rejects_removed_gemini_runtime() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle_agents_create_as(
            json!({
                "name": "gem-agent",
                "display_name": "Gem",
                "runtime": { "provider": "gemini", "fallback": "gemini" },
            }),
            Some(&caller()),
        )
        .await;
    assert!(!ok(&frame), "{frame:?}");
    assert!(!home.path().join("agents/gem-agent/agent.toml").exists());
}

#[tokio::test]
async fn update_rejects_removed_gemini_runtime_without_rewriting_config() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = caller();
    assert!(ok(&handler
        .handle_agents_create_as(
            json!({ "name": "upd", "display_name": "Upd", "runtime": { "provider": "claude" } }),
            Some(&ctx),
        )
        .await));
    let toml_path = home.path().join("agents/upd/agent.toml");
    let before = std::fs::read(&toml_path).unwrap();
    let frame = handler
        .handle_agents_update_as(
            json!({ "agent_id": "upd", "runtime": { "provider": "gemini" } }),
            Some(&ctx),
        )
        .await;
    assert!(!ok(&frame), "{frame:?}");
    assert_eq!(std::fs::read(&toml_path).unwrap(), before);
    assert_eq!(
        provider_on_disk(home.path(), "upd").as_deref(),
        Some("claude")
    );
}

#[test]
fn runtime_catalog_and_detect_rows_exclude_gemini_cli() {
    use super::super::runtime_models_rpc::runtime_detect_row;
    assert!(duduclaw_core::runtime_catalog::spec_for("gemini").is_none());
    assert!(duduclaw_core::types::RuntimeType::parse("gemini").is_none());
    for spec in duduclaw_core::runtime_catalog::cli_specs() {
        assert_ne!(spec.id, "gemini");
        let row = runtime_detect_row(spec, None, None);
        assert_eq!(row["id"], spec.id);
        assert_eq!(row["installed"], false);
        assert_eq!(row["deprecated"], false);
    }
}

#[tokio::test]
async fn gemini_model_id_does_not_auto_align_to_antigravity() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = caller();
    assert!(ok(&handler
        .handle_agents_create_as(
            json!({ "name": "claude-agent", "display_name": "C", "runtime": { "provider": "claude" } }),
            Some(&ctx),
        )
        .await));

    // Force a deterministic binary probe for this unique model id.
    let model = "gemini-2.5-pro-antigravity-align-probe";
    crate::runtime_config::FAMILY_CLI_INSTALLED_FOR_MODEL
        .lock()
        .unwrap()
        .push(model.to_string());
    assert_eq!(
        crate::runtime_config::infer_provider_for_model(model),
        Some(duduclaw_core::types::RuntimeType::Antigravity)
    );
    let frame = handler
        .handle_agents_update_as(json!({ "agent_id": "claude-agent", "preferred": model }), Some(&ctx))
        .await;
    assert!(ok(&frame), "{frame:?}");
    assert_eq!(
        provider_on_disk(home.path(), "claude-agent").as_deref(),
        Some("claude")
    );
    assert_eq!(
        payload(&frame)["runtime_provider_align_skipped"],
        "model_id_requires_display_name"
    );
}
