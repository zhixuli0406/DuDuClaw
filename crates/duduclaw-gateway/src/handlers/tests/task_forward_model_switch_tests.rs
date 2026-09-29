//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

//! v1.54 `task_forward_model.get/set` global switches. Verifies the RPC
//! reads the three flags, partial-updates without clobbering other config
//! sections, and reflects saved values on the next read.
use super::*;

fn payload(frame: &WsFrame) -> Value {
    match frame {
        WsFrame::Response {
            ok: true,
            payload: Some(p),
            ..
        } => p.clone(),
        WsFrame::Response {
            ok: false, error, ..
        } => panic!("error frame: {error:?}"),
        other => panic!("unexpected frame: {other:?}"),
    }
}

#[tokio::test]
async fn get_reflects_real_defaults_when_section_absent() {
    // Mirrors `TaskForwardModelConfig::from_home` exactly, so whatever the
    // engine treats as the default is what the dashboard shows. v1.54:
    // master `enabled` stays off; calibration + held-out gate default on
    // (they no-op while `enabled` is off).
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let expected = crate::prediction::task_forward_store::TaskForwardModelConfig::default();
    let p = payload(&handler.handle_task_forward_model_get().await);
    assert_eq!(p["enabled"], expected.enabled);
    assert_eq!(p["calibration_enabled"], expected.calibration_enabled);
    assert_eq!(p["held_out_gate_enabled"], expected.held_out_gate_enabled);
}

#[tokio::test]
async fn set_partial_update_preserves_other_sections_and_keys() {
    let home = tempfile::tempdir().unwrap();
    // Seed config.toml with unrelated sections AND an unrelated key inside
    // [task_forward_model] itself — both must survive a partial write.
    let cfg = "\
[delegation]
policy = \"department\"

[task_forward_model]
enabled = false
held_out_gate_enabled = false
min_samples = 7

[some_other]
keep = \"me\"
";
    std::fs::write(home.path().join("config.toml"), cfg).unwrap();

    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = UserContext::admin_fallback();

    // Turn on enabled + calibration_enabled only.
    let frame = handler
        .handle_task_forward_model_set(
            json!({ "enabled": true, "calibration_enabled": true }),
            &ctx,
        )
        .await;
    let p = payload(&frame);
    assert_eq!(p["enabled"], true);
    assert_eq!(p["calibration_enabled"], true);
    assert_eq!(p["held_out_gate_enabled"], false);
    assert_eq!(p["enabled_requires_restart"], true);

    // Re-read: values persisted.
    let p2 = payload(&handler.handle_task_forward_model_get().await);
    assert_eq!(p2["enabled"], true);
    assert_eq!(p2["calibration_enabled"], true);

    // Other sections + the unrelated sub-key survived byte-wise.
    let written = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    let table: toml::Table = written.parse().unwrap();
    assert_eq!(table["delegation"]["policy"].as_str(), Some("department"));
    assert_eq!(table["some_other"]["keep"].as_str(), Some("me"));
    assert_eq!(
        table["task_forward_model"]["min_samples"].as_integer(),
        Some(7)
    );
}

#[tokio::test]
async fn set_rejects_non_boolean() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = UserContext::admin_fallback();
    let frame = handler
        .handle_task_forward_model_set(json!({ "enabled": "yes" }), &ctx)
        .await;
    assert!(
        matches!(frame, WsFrame::Response { ok: false, .. }),
        "{frame:?}"
    );
}

#[tokio::test]
async fn set_rejects_non_table_section() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        "task_forward_model = \"oops\"\n",
    )
    .unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = UserContext::admin_fallback();
    let frame = handler
        .handle_task_forward_model_set(json!({ "enabled": true }), &ctx)
        .await;
    assert!(
        matches!(frame, WsFrame::Response { ok: false, .. }),
        "{frame:?}"
    );
}
