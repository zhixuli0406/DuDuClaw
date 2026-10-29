use super::*;

const ORIGINAL: &str = "# operator notes\n[agent] # employee\nname = 'writer'\nstatus = 'paused' # owner pause\n\n[model]\npreferred = 'test-model' # keep model\n\n[evolution]\nenabled = false # keep evolution note\n";
const ACTIONS: [Action; 4] = [
    Action::Pause,
    Action::Resume,
    Action::Freeze,
    Action::Unfreeze,
];

fn fixture(text: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().canonicalize().unwrap();
    std::fs::create_dir_all(home.join("agents/writer")).unwrap();
    std::fs::write(home.join("agents/writer/agent.toml"), text).unwrap();
    (tmp, home)
}

fn read(home: &Path) -> String {
    std::fs::read_to_string(home.join("agents/writer/agent.toml")).unwrap()
}

#[test]
fn every_session_marker_refuses_all_four_actions_without_touching_agent_toml() {
    let (_tmp, home) = fixture(ORIGINAL);
    for marker in crate::ai_session_guard::AI_SESSION_ENV_VARS {
        // The shared lookup counts presence, including an empty value.
        let env = |k: &str| (k == *marker).then_some("");
        let markers = crate::ai_session_guard::markers_with(&|k| env(k).is_some());
        for action in ACTIONS {
            let err = run_with_markers(&home, "writer", action, &markers).unwrap_err();
            assert!(
                err.to_string().contains(AGENT_SESSION_REFUSAL),
                "{marker}: {action:?}: {err}"
            );
            assert_eq!(read(&home), ORIGINAL, "{marker}: {action:?}");
        }
    }
    assert!(!home.join("agent_file_history").exists());
    let audit = std::fs::read_to_string(home.join("security_audit.jsonl")).unwrap();
    for line in audit.lines() {
        let row: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(row["event_type"], "agent_cli_refused");
        assert_eq!(row["details"]["reason"], "agent_session");
    }
}

#[test]
fn session_refusal_caps_long_agent_ids_in_audit() {
    for character in ["a", "員", "🐾"] {
        let agent = character.repeat(10_000);
        for action in ACTIONS {
            let (_tmp, home) = fixture(ORIGINAL);
            let markers = crate::ai_session_guard::markers_with(&|k| k == "DUDUCLAW_AGENT_ID");
            let err = run_with_markers(&home, &agent, action, &markers).unwrap_err();
            assert!(err.to_string().contains(AGENT_SESSION_REFUSAL));
            assert_eq!(read(&home), ORIGINAL);
            let audit = std::fs::read_to_string(home.join("security_audit.jsonl")).unwrap();
            let rows: Vec<serde_json::Value> = audit
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(rows.len(), 1);
            let row = &rows[0];
            assert_eq!(row["event_type"], "agent_cli_refused");
            assert_eq!(row["details"]["reason"], "agent_session");
            let subject = row["agent_id"].as_str().unwrap();
            assert!(subject.chars().count() <= 128);
            assert_eq!(subject, character.repeat(128));
            assert_eq!(row["details"]["agent_truncated"], true);
        }
    }
}

#[test]
fn operator_actions_update_only_their_fields_keep_comments_and_audit() {
    for action in ACTIONS {
        let initial = match action {
            Action::Pause => ORIGINAL.replace("status = 'paused'", "status = 'active'"),
            Action::Freeze => ORIGINAL.replace("enabled = false", "enabled = true"),
            _ => ORIGINAL.to_string(),
        };
        let (_tmp, home) = fixture(&initial);
        run_with_markers(&home, "writer", action, &[]).unwrap();
        let text = read(&home);
        let parsed: toml::Table = text.parse().unwrap();
        assert!(
            text.starts_with("# operator notes\n[agent] # employee\n"),
            "{text}"
        );
        assert!(
            text.contains("# owner pause") && text.contains("# keep evolution note"),
            "{text}"
        );
        assert!(
            text.contains("preferred = 'test-model' # keep model"),
            "{text}"
        );
        match action {
            Action::Pause | Action::Resume => {
                let status = if matches!(action, Action::Pause) {
                    "paused"
                } else {
                    "active"
                };
                assert_eq!(parsed["agent"]["status"].as_str(), Some(status));
                assert_eq!(parsed["evolution"]["enabled"].as_bool(), Some(false));
                assert!(!parsed.contains_key("heartbeat"));
            }
            Action::Freeze | Action::Unfreeze => {
                let enabled = matches!(action, Action::Unfreeze);
                assert_eq!(parsed["evolution"]["enabled"].as_bool(), Some(enabled));
                assert_eq!(parsed["heartbeat"]["enabled"].as_bool(), Some(enabled));
                assert_eq!(parsed["agent"]["status"].as_str(), Some("paused"));
            }
        }
        let audit = std::fs::read_to_string(home.join("security_audit.jsonl")).unwrap();
        assert!(
            audit.contains(action.event()) && audit.contains("agent_file_changed"),
            "{audit}"
        );
        assert!(audit.contains(UNVERIFIED_ACTOR), "{audit}");
        let index =
            std::fs::read_to_string(home.join("agent_file_history/writer/agent_toml/index.jsonl"))
                .unwrap();
        let versions: Vec<serde_json::Value> = index
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(versions.len(), 2, "initial content and the CLI change");
        assert!(versions.iter().any(|v| v["source"] == "cli"));
        assert!(
            !home.join("approvals.db").exists(),
            "operator commands must stay direct"
        );
    }
}

#[test]
fn invalid_or_missing_agent_and_malformed_toml_are_not_written() {
    let (_tmp, home) = fixture(ORIGINAL);
    for action in ACTIONS {
        for id in ["../writer", "missing"] {
            assert!(run_with_markers(&home, id, action, &[]).is_err());
            assert_eq!(read(&home), ORIGINAL);
        }
    }
    assert!(!home.join("agents/missing").exists());
    for text in ["[agent", "evolution = 1\n[agent]\nname = 'writer'\n"] {
        let (_tmp, home) = fixture(text);
        assert!(run_with_markers(&home, "writer", Action::Freeze, &[]).is_err());
        assert_eq!(read(&home), text);
    }
    let (_tmp, home) = fixture("[model]\npreferred = 'test-model'\n");
    assert!(run_with_markers(&home, "writer", Action::Pause, &[]).is_err());
    assert_eq!(read(&home), "[model]\npreferred = 'test-model'\n");
}

#[test]
fn concurrent_status_and_freeze_edits_keep_both_changes() {
    let (_tmp, home) = fixture(ORIGINAL);
    std::thread::scope(|s| {
        let resume = s.spawn(|| run_with_markers(&home, "writer", Action::Resume, &[]));
        let unfreeze = s.spawn(|| run_with_markers(&home, "writer", Action::Unfreeze, &[]));
        resume.join().unwrap().unwrap();
        unfreeze.join().unwrap().unwrap();
    });
    let parsed: toml::Table = read(&home).parse().unwrap();
    assert_eq!(parsed["agent"]["status"].as_str(), Some("active"));
    assert_eq!(parsed["evolution"]["enabled"].as_bool(), Some(true));
    assert_eq!(parsed["heartbeat"]["enabled"].as_bool(), Some(true));
}

/// A freeze whose snapshot predates a completed lifecycle write (archive,
/// resume or pause) must keep that write. Deterministic interleaving, no sleeps.
#[test]
fn freeze_delayed_edit_preserves_completed_lifecycle_changes() {
    for action in [Action::Freeze, Action::Unfreeze] {
        let enabled = matches!(action, Action::Unfreeze);
        for (old, new) in [("active", "archived"), ("paused", "active"), ("active", "paused")] {
            let (_tmp, home) = fixture(&format!("# keep me\n[agent]\nname = 'writer'\nstatus = '{old}'\n"));
            crate::profile_edit_test_hook::after_snapshot(&home, "writer", old, new);
            run_with_markers(&home, "writer", action, &[]).unwrap();
            let text = read(&home);
            assert!(text.contains("# keep me"), "{text}");
            let table: toml::Table = text.parse().unwrap();
            assert_eq!(table["agent"]["status"].as_str(), Some(new), "{action:?} {old}->{new}");
            for section in ["evolution", "heartbeat"] {
                assert_eq!(table[section]["enabled"].as_bool(), Some(enabled), "{action:?} {section}");
            }
        }
    }
}

#[test]
fn shared_bash_matcher_protects_all_controls_for_agents_and_untrusted_callers() {
    for action in ACTIONS {
        let cmd = format!(
            "env -u DUDUCLAW_AGENT_ID /opt/bin/duduclaw-pro.exe ag''ent {} writer",
            action.name()
        );
        for caller in [
            duduclaw_core::HookCaller::Agent("writer".into()),
            duduclaw_core::HookCaller::Untrusted("writer".into()),
        ] {
            assert!(
                matches!(
                    bash_agent_control_decision(&cmd, &caller),
                    Some(duduclaw_core::GuardDecision::BlockedOperatorCommand { command, .. })
                        if command == format!("duduclaw agent {}", action.name())
                ),
                "{cmd}: {caller:?}"
            );
        }
        assert!(bash_agent_control_decision(&cmd, &duduclaw_core::HookCaller::Absent).is_none());
    }
}
