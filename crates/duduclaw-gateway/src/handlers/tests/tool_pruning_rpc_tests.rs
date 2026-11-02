//! Administrative discovery observations never widen an employee's authority.
use super::agent_files_rpc_tests::{audit_rows, err, fixture, ok};
use super::*;

#[tokio::test]
async fn pruning_rpcs_require_admin_and_restore_is_audited() {
    let f = fixture().await;
    for method in ["agents.tool_pruning_status", "agents.tool_pruning_restore"] {
        let refused = f.call(method, json!({"agent_id":"a1"}), &f.owner_a).await;
        assert!(!err(&refused).is_empty());
    }
    let status = ok(&f
        .call(
            "agents.tool_pruning_status",
            json!({"agent_id":"a1"}),
            &f.admin,
        )
        .await);
    assert_eq!(status["mode"], "shadow");
    assert_eq!(status["estimate"], true);
    assert_eq!(status["prunable_names"], json!([]));
    assert_eq!(status["estimated_tokens"], 0);
    assert!(status["listed_count"].as_u64().unwrap() > 0);
    let restored = ok(&f
        .call(
            "agents.tool_pruning_restore",
            json!({"agent_id":"a1"}),
            &f.admin,
        )
        .await);
    assert_eq!(restored["prunable_names"], json!([]));
    let marker = std::fs::read_to_string(f.home.join("agents/a1/tool_pruning_reset_at")).unwrap();
    assert!(chrono::DateTime::parse_from_rfc3339(&marker).is_ok());
    assert_eq!(audit_rows(&f.home, "tool_pruning_restored").len(), 1);
}

#[test]
fn pruning_capability_update_is_validated() {
    let mut table = toml::Table::new();
    for mode in ["off", "shadow", "on"] {
        super::super::capabilities_apply::apply_capabilities_to_table(
            &mut table,
            &json!({"capabilities":{"tool_pruning":mode}}),
        )
        .unwrap();
        assert_eq!(table["capabilities"]["tool_pruning"].as_str(), Some(mode));
    }
    for invalid in [json!(true), json!("invalid"), Value::Null] {
        assert!(
            super::super::capabilities_apply::apply_capabilities_to_table(
                &mut table,
                &json!({"capabilities":{"tool_pruning":invalid}})
            )
            .is_err()
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn restore_refuses_symlinked_employee_directory() {
    let f = fixture().await;
    let original = f.home.join("agents/a1");
    let moved = f.home.join("kept-agent");
    std::fs::rename(&original, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &original).unwrap();
    let response = f
        .call(
            "agents.tool_pruning_restore",
            json!({"agent_id":"a1"}),
            &f.admin,
        )
        .await;
    assert!(err(&response).contains("Failed to restore"));
    assert!(!moved.join("tool_pruning_reset_at").exists());
}
