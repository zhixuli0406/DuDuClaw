use super::*;
use crate::channel_reply::entry_gate_test_helpers::owned_channel;

#[tokio::test(flavor = "current_thread")]
async fn heartbeat_assignee_gate_checks_effective_artifact_before_host_spawn() {
    let (home, _ctx, _runtime, calls) = owned_channel(RuntimeType::Codex).await;
    let resolved = duduclaw_core::preset::agent_resolved_path(home.path(), "worker");
    std::fs::create_dir_all(resolved.parent().unwrap()).unwrap();
    let before = crate::model_call_probe::calls();
    // Keep the raw profile healthy: refusal must come from the effective file.
    for (text, expected) in [
        ("[runtime]\nprovider='codex'\n[model]\npreferred='gpt-5.4'", None),
        ("[runtime]\nprovider='claude'\n[model]\npreferred='claude-sonnet-4-6'", None),
        ("[runtime]\nprovider='typo'\n[model]\npreferred='gpt-5.4'", Some("agent_config_unreadable")),
        ("[runtime]\nprovider='claude'\n[model]\npreferred='gpt-5.4'", Some("model_provider_mismatch")),
        ("[broken", Some("agent_config_unreadable")),
    ] {
        std::fs::write(&resolved, text).unwrap();
        let result = check_task_assignee(home.path(), "worker")
            .map_err(|refusal| background_refusal(&refusal));
        match expected {
            Some(code) => assert_eq!(background_refusal_code(&result.unwrap_err()), Some(code)),
            None => assert_eq!(result, Ok(())),
        }
    }
    std::fs::write(&resolved, [0xff, 0xfe]).unwrap();
    assert_eq!(check_task_assignee(home.path(), "worker").unwrap_err().code(), "agent_config_unreadable");
    assert_eq!(crate::model_call_probe::calls(), before);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn background_fallback_note_keeps_anchored_code_and_only_describes_mismatches() {
    for refusal in refusals() {
        for fallback in [None, Some(RuntimeType::Codex)] {
            let error = background_refusal_with_fallback(&refusal, fallback);
            assert_eq!(background_refusal_code(&error), Some(refusal.code()));
            let expected = if matches!(refusal, EntryRefusal::ModelProviderMismatch { .. }) {
                runtime_config::alignment_refusal_with_fallback(background_refusal(&refusal), fallback)
            } else { background_refusal(&refusal) };
            assert_eq!(error, expected);
            assert_eq!(error.contains("A fallback runtime is configured, but it was not used because the model setting itself is incorrect."),
                fallback.is_some() && refusal.code() == "model_provider_mismatch");
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn task_assignee_checks_directory_resolved_from_registry_name() {
    let (home, _ctx, _runtime, _calls) = owned_channel(RuntimeType::Codex).await;
    let directory = home.path().join("agents/w-dir");
    std::fs::rename(home.path().join("agents/worker"), &directory).unwrap();
    assert_eq!(check_task_assignee(home.path(), "worker"), Ok(()));
    let path = directory.join("agent.toml");
    let healthy = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, healthy.replace("provider='codex'", "provider='grok'")).unwrap();
    let refusal = check_task_assignee(home.path(), "worker").unwrap_err();
    assert_eq!(refusal.code(), "model_provider_mismatch");
    assert_eq!(refusal, EntryRefusal::ModelProviderMismatch {
        model: "gpt-5.4".into(), provider: RuntimeType::Grok,
    });
}

#[tokio::test(flavor = "current_thread")]
async fn strict_refusals_cover_every_config_failure_without_registry_rescan() {
    let (home, ctx, _runtime, _calls) = owned_channel(RuntimeType::Codex).await;
    let registry = ctx.registry.read().await;
    let path = home.path().join("agents/worker/agent.toml");
    for (text, expected) in [
        ("[runtime", RuntimeSettingsError::Malformed),
        ("[runtime]\nprovider=42\n[model]\npreferred='gpt-5.4'", RuntimeSettingsError::Malformed),
        ("[runtime]\nprovider='typo'\n[model]\npreferred='gpt-5.4'", RuntimeSettingsError::UnknownProvider),
        ("[runtime]\nprovider='codex'\nfallback='typo'\n[model]\npreferred='gpt-5.4'", RuntimeSettingsError::UnknownProvider),
        ("[runtime]\nprovider='codex'", RuntimeSettingsError::MissingModel),
        ("[runtime]\nprovider='claude'\n[model]\npreferred='  '", RuntimeSettingsError::MissingModel),
        ("[runtime]\nprovider='openai_compat'\n[model]", RuntimeSettingsError::MissingModel),
        ("[runtime]\nprovider='codex'\n[model]\npreferred=42", RuntimeSettingsError::Malformed),
        ("[runtime]\nprovider='codex'\n[model]\npreferred='gpt-5.4'\n[team.roles.planner]\nruntime='typo'", RuntimeSettingsError::UnknownProvider),
        ("[runtime]\nprovider='codex'\n[model]\npreferred='gpt-5.4'\n[team.roles.planner]\nruntime=42", RuntimeSettingsError::Malformed),
    ] {
        std::fs::write(&path, text).unwrap();
        assert_eq!(resolve_and_check(home.path(), &registry, "worker").unwrap_err(), EntryRefusal::Config(expected), "{text}");
    }
    std::fs::remove_file(&path).unwrap();
    assert_eq!(resolve_and_check(home.path(), &registry, "worker").unwrap_err(), EntryRefusal::Config(RuntimeSettingsError::Missing));
    std::fs::write(&path, [0xff, 0xfe]).unwrap();
    assert_eq!(resolve_and_check(home.path(), &registry, "worker").unwrap_err(), EntryRefusal::Config(RuntimeSettingsError::Unreadable));
    let denied = resolve_with_reader(home.path(), &registry, "worker", |_| {
        Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
    });
    assert_eq!(denied.unwrap_err(), EntryRefusal::Config(RuntimeSettingsError::Unreadable));
    assert_eq!(resolve_and_check(home.path(), &registry, "absent").unwrap_err(), EntryRefusal::AgentNotFound);
}

#[tokio::test(flavor = "current_thread")]
async fn active_model_and_product_checks_share_the_effective_document() {
    let (home, ctx, _runtime, _calls) = owned_channel(RuntimeType::Codex).await;
    let registry = ctx.registry.read().await;
    let path = home.path().join("agents/worker/agent.toml");
    for text in [
        "[runtime]\nprovider='gemini'\n[model]\npreferred='gemini-2.5-pro'",
        "[runtime]\nprovider='codex'\nfallback='antigravity'\n[model]\npreferred='gpt-5.4'",
        "[runtime]\nprovider='codex'\n[model]\npreferred='gpt-5.4'\n[team.roles.planner]\nruntime='gemini'",
    ] {
        std::fs::write(&path, text).unwrap();
        assert!(matches!(resolve_and_check(home.path(), &registry, "worker"), Err(EntryRefusal::InactiveRuntime { .. })), "{text}");
    }
    std::fs::write(&path, "[runtime]\nprovider='grok'\n[model]\npreferred='gpt-5.4'").unwrap();
    assert_eq!(resolve_and_check(home.path(), &registry, "worker").unwrap_err(), EntryRefusal::ModelProviderMismatch {
        model: "gpt-5.4".into(), provider: RuntimeType::Grok,
    });
    // Legacy bare aliases still resolve to the answering engine's model.
    std::fs::write(&path, "[runtime]\nprovider='codex'\n[model]\npreferred='opus'").unwrap();
    let turn = resolve_and_check(home.path(), &registry, "worker").unwrap();
    assert_eq!(turn.model, runtime_config::runnable_model("worker", "opus", RuntimeType::Codex).as_ref());
    assert_eq!(turn.runtime_settings.provider, RuntimeType::Codex);
    std::fs::write(home.path().join("config.toml"), "[team.roles.planner]\nruntime='gemini'").unwrap();
    assert!(matches!(resolve_and_check(home.path(), &registry, "worker"), Err(EntryRefusal::InactiveRuntime { .. })));
    // A readable global file with an invalid policy is a policy refusal.
    std::fs::write(home.path().join("config.toml"), "[product]\nprofile='not-a-profile'").unwrap();
    assert!(matches!(resolve_and_check(home.path(), &registry, "worker"), Err(EntryRefusal::ProductPolicy { .. })));
}

#[tokio::test(flavor = "current_thread")]
async fn one_read_one_parse_snapshot_uses_resolved_artifact_and_survives_disk_change() {
    let (home, ctx, _runtime, _calls) = owned_channel(RuntimeType::Codex).await;
    let registry = ctx.registry.read().await;
    let raw = home.path().join("agents/worker/agent.toml");
    let resolved = duduclaw_core::preset::agent_resolved_path(home.path(), "worker");
    std::fs::create_dir_all(resolved.parent().unwrap()).unwrap();
    std::fs::write(&resolved, "[runtime]\nprovider='codex'\nminimal_context=false\n[model]\npreferred='gpt-5.4'\neffort='high'\n[model.local]\nmodel='local-snapshot'").unwrap();
    // The source must not be parsed once an effective artifact was selected.
    std::fs::write(&raw, "[broken").unwrap();
    let reads = std::cell::Cell::new(0);
    let turn = resolve_with_reader(home.path(), &registry, "worker", |path| {
        reads.set(reads.get() + 1);
        assert_eq!(path, resolved);
        let text = std::fs::read_to_string(path)?;
        std::fs::write(path, "[broken-after-read")?;
        Ok(text)
    }).unwrap();
    assert_eq!(reads.get(), 1);
    // There is one text.parse in resolve_with_reader by construction. Every
    // projection and gate consumes its document, not independently read text.
    assert_eq!(turn.config_path, resolved);
    assert!(turn.preset_resolved);
    assert_eq!(turn.model, "gpt-5.4");
    assert_eq!(turn.runtime_settings.preferred_model.as_deref(), Some("gpt-5.4"));
    assert_eq!(turn.sections.runtime.minimal_context, Some(false));
    assert_eq!(turn.local_model_id.as_deref(), Some("local-snapshot"));
    assert_eq!(turn.document["model"]["effort"].as_str(), Some("high"));
    // An invalid resolved artifact cannot fall back to a valid source.
    std::fs::write(&raw, "[runtime]\nprovider='codex'\n[model]\npreferred='gpt-5.4'").unwrap();
    assert_eq!(resolve_and_check(home.path(), &registry, "worker").unwrap_err(), EntryRefusal::Config(RuntimeSettingsError::Malformed));
    std::fs::remove_file(&resolved).unwrap();
    std::fs::create_dir(&resolved).unwrap();
    assert_eq!(resolve_and_check(home.path(), &registry, "worker").unwrap_err(), EntryRefusal::Config(RuntimeSettingsError::Unreadable));
}

fn refusals() -> Vec<EntryRefusal> {
    vec![
        EntryRefusal::AgentNotFound,
        EntryRefusal::Config(RuntimeSettingsError::Missing),
        EntryRefusal::Config(RuntimeSettingsError::Unreadable),
        EntryRefusal::Config(RuntimeSettingsError::Malformed),
        EntryRefusal::Config(RuntimeSettingsError::UnknownProvider),
        EntryRefusal::Config(RuntimeSettingsError::MissingModel),
        EntryRefusal::InactiveRuntime { diagnostic: "/private/secret/agent.toml".into() },
        EntryRefusal::ModelProviderMismatch { model: "secret-model".into(), provider: RuntimeType::Codex },
        EntryRefusal::ProductPolicy { diagnostic: "/private/policy".into() },
        EntryRefusal::SystemPromptUnreadable,
    ]
}

#[tokio::test]
async fn every_refusal_has_localized_operator_text_and_neutral_public_text() {
    for refusal in refusals() {
        for language in ["zh-TW", "en", "ja-JP", "zh-CN"] {
            let operator = refusal.user_text("webchat:operator", Some(language), "12345678-turn");
            assert!(operator.contains(refusal.code()), "{operator}");
            assert!(operator.contains("12345678"));
            assert!(!operator.contains("secret") && !operator.contains("/private"));
            for session in ["odoolivechat:shop:43", "widget-visitor:abc"] {
                assert_eq!(refusal.user_text(session, Some(language), "turn"), crate::odoo_livechat::VISITOR_FALLBACK_MESSAGE);
            }
            let widget = crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope(
                "widget-visitor:account".into(), async {
                    refusal.user_text("webchat:ordinary-session", Some(language), "turn")
                },
            ).await;
            assert_eq!(widget, crate::odoo_livechat::VISITOR_FALLBACK_MESSAGE);
        }
    }
}

#[test]
fn exemptions_follow_parsed_commands_and_keep_model_authorization() {
    use crate::chat_commands::{parse_command, parse_takeover};
    assert_eq!(EXEMPT_COMMANDS.len(), 10);
    for text in ["!STOP", "!STOP ALL", "!RESUME", "!STATUS", "/status", "/help", "/new", "/pair 123456", "/model", "/model gpt-5.4", "/reset", "/h", "/s", "/m"] {
        assert!(command_is_exempt(&parse_command(text, None).unwrap()), "{text}");
    }
    for text in ["/goal do work", "/assign work", "/usage", "/compact", "/resume", "/stop", "/pause", "/voice"] {
        assert!(!command_is_exempt(&parse_command(text, None).unwrap()), "{text}");
    }
    for text in ["!STOPPED", "/status-extra", "/modelled", "/takeovers", "hello !STOP"] {
        assert!(!parse_command(text, None).as_ref().is_some_and(command_is_exempt), "{text}");
        assert!(parse_takeover(text).is_none(), "{text}");
    }
    for text in ["/takeover", "/takeover end", "/takeover +30m"] {
        assert!(takeover_is_exempt(&parse_takeover(text).unwrap()));
    }
    assert!(parse_command("/model gpt-5.4", None).unwrap().requires_admin());
    assert!(!parse_command("/model", None).unwrap().requires_admin());
}
