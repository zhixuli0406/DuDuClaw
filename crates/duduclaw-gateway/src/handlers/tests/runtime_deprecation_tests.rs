use super::*;

fn ok(frame: &WsFrame) -> bool {
    matches!(frame, WsFrame::Response { ok: true, .. })
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
async fn create_rejects_removed_runtimes() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    for removed in duduclaw_core::runtime_catalog::REMOVED_RUNTIMES {
        for field in ["provider", "fallback"] {
            let mut runtime = json!({"provider": "claude"});
            runtime[field] = json!(removed.id);
            let frame = handler.handle_agents_create_as(
                json!({"name": "removed-agent", "display_name": "Removed", "runtime": runtime}),
                Some(&caller()),
            ).await;
            assert!(!ok(&frame), "{frame:?}");
            assert!(format!("{frame:?}").contains(removed.removed_in), "{frame:?}");
            assert!(!home.path().join("agents/removed-agent/agent.toml").exists());
        }
    }
}

#[tokio::test]
async fn update_rejects_removed_runtimes_without_rewriting_config() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = caller();
    assert!(ok(&handler.handle_agents_create_as(
        json!({"name": "upd", "display_name": "Upd", "runtime": {"provider": "claude"}}),
        Some(&ctx),
    ).await));
    let path = home.path().join("agents/upd/agent.toml");
    let before = std::fs::read(&path).unwrap();
    for removed in duduclaw_core::runtime_catalog::REMOVED_RUNTIMES {
        for field in ["provider", "fallback"] {
            let mut runtime = json!({});
            runtime[field] = json!(removed.id);
            let frame = handler.handle_agents_update_as(
                json!({"agent_id": "upd", "runtime": runtime}), Some(&ctx),
            ).await;
            assert!(!ok(&frame), "{frame:?}");
            assert!(format!("{frame:?}").contains(removed.removed_in), "{frame:?}");
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
    }
    assert_eq!(provider_on_disk(home.path(), "upd").as_deref(), Some("claude"));
}

#[test]
fn runtime_catalog_and_detect_rows_exclude_removed_clis() {
    use super::super::runtime_models_rpc::runtime_detect_row;
    for removed in duduclaw_core::runtime_catalog::REMOVED_RUNTIMES {
        assert!(duduclaw_core::runtime_catalog::spec_for(removed.id).is_none());
        assert!(duduclaw_core::types::RuntimeType::parse(removed.id).is_none());
    }
    for spec in duduclaw_core::runtime_catalog::cli_specs() {
        assert!(duduclaw_core::runtime_catalog::removed_runtime(spec.id).is_none());
        let row = runtime_detect_row(spec, None, None, None);
        assert_eq!(row["id"], spec.id);
        assert_eq!(row["installed"], false);
        assert_eq!(row["deprecated"], false);
    }
}

#[tokio::test]
async fn gemini_model_id_does_not_auto_align_to_a_cli() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = caller();
    assert!(ok(&handler.handle_agents_create_as(
        json!({"name": "claude-agent", "display_name": "C", "runtime": {"provider": "claude"}}),
        Some(&ctx),
    ).await));
    let model = "gemini-2.5-pro";
    assert_eq!(crate::runtime_config::infer_provider_for_model(model), None);
    let frame = handler.handle_agents_update_as(
        json!({"agent_id": "claude-agent", "preferred": model}), Some(&ctx),
    ).await;
    assert!(ok(&frame), "{frame:?}");
    assert_eq!(provider_on_disk(home.path(), "claude-agent").as_deref(), Some("claude"));
}


#[test]
fn runtime_update_row_shape_and_boundary() {
    use super::super::runtime_models_rpc::runtime_detect_row;
    for spec in duduclaw_core::runtime_catalog::cli_specs() {
        let row = runtime_detect_row(spec, Some("/test/bin"), None, Some("codex-cli 0.163.0"));
        assert_eq!(row["update_required"], cfg!(unix) && spec.id == "codex");
        if spec.id == "codex" {
            assert_eq!(row["version"], "0.163.0");
            assert_eq!(row["minimum_version"], "0.164.0");
            let boundary = runtime_detect_row(spec, Some("/test/bin"), None, Some("codex-cli 0.164.0"));
            assert_eq!(boundary["update_required"], false);
            assert_eq!(runtime_detect_row(spec, Some("/test/bin"), None, None)["update_required"], cfg!(unix));
            assert_eq!(runtime_detect_row(spec, None, None, None)["update_required"], false);
        } else {
            assert!(row["version"].is_null());
            assert!(row["minimum_version"].is_null());
        }
    }
}
