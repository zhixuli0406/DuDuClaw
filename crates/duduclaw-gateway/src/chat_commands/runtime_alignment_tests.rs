//! #210 source-first real /model writer seam; UNRUN.
use super::*;
#[tokio::test(flavor = "current_thread")]
async fn same_model_command_repairs_runtime_and_rescans_without_rewriting_other_keys() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("agents/worker");
    std::fs::create_dir_all(&dir).unwrap();
    let original = "# retained\n[agent]\nname='worker'\ndisplay_name='Worker'\nrole='main'\n[model]\npreferred='gpt-5.4'\n[runtime]\nprovider='claude'\nfallback='openai_compat'\n";
    std::fs::write(dir.join("agent.toml"), original).unwrap();
    let registry = std::sync::Arc::new(tokio::sync::RwLock::new(
        duduclaw_agent::AgentRegistry::new(home.path().join("agents")),
    ));
    registry.write().await.scan().await.unwrap();
    let sessions = std::sync::Arc::new(
        crate::session::SessionManager::new(&home.path().join("sessions.db")).unwrap(),
    );
    let status = std::sync::Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()));
    let (tx, _rx) = tokio::sync::broadcast::channel(16);
    let ctx = ReplyContext::new(registry, home.path().to_owned(), sessions, status, tx);
    let _availability = crate::runtime_config::testing::AvailabilityGuard::at("gpt-5.4", true);
    let reply = handle_model(&ctx, "worker", Some("gpt-5.4")).await;
    assert!(reply.contains("已修正執行環境"), "{reply}");
    let raw = std::fs::read_to_string(dir.join("agent.toml")).unwrap();
    let t: toml::Table = raw.parse().unwrap();
    assert_eq!(t["model"]["preferred"].as_str(), Some("gpt-5.4"));
    assert_eq!(t["runtime"]["provider"].as_str(), Some("codex"));
    assert_eq!(t["runtime"]["fallback"].as_str(), Some("openai_compat"));
    assert!(raw.contains("# retained"));
    assert!(
        handle_model(&ctx, "worker", Some("gpt-5.4"))
            .await
            .contains("沒有變更")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn model_and_inferred_runtime_cannot_partially_commit_when_locked_mutation_refuses() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("agents/worker");
    std::fs::create_dir_all(&dir).unwrap();
    let original = "[agent]\nname='worker'\nrole='main'\n[model]\npreferred='claude-sonnet-4-6'\n";
    std::fs::write(dir.join("agent.toml"), original).unwrap();
    let registry = std::sync::Arc::new(tokio::sync::RwLock::new(
        duduclaw_agent::AgentRegistry::new(home.path().join("agents")),
    ));
    registry.write().await.scan().await.unwrap();
    let _availability = crate::runtime_config::testing::AvailabilityGuard::at("gpt-5.4", true);
    let result = crate::channel_reply::update_agent_toml_edit(
        &registry,
        "worker",
        crate::channel_reply::AgentTomlEdit {
            source: "chat.model",
            user_id: "owned-test",
            guard: || Ok(()),
        },
        |table| {
            table
                .get_mut("model")
                .unwrap()
                .as_table_mut()
                .unwrap()
                .insert("preferred".into(), toml::Value::String("gpt-5.4".into()));
            crate::runtime_config::align_model_runtime(table)?;
            Err("owned atomic refusal".into())
        },
    )
    .await;
    assert!(result.is_err());
    assert_eq!(
        std::fs::read_to_string(dir.join("agent.toml")).unwrap(),
        original
    );
}
