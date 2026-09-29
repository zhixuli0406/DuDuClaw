//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

//! P4-3 OS page: quota gate (write side) + os.* RPC handlers.
use super::*;

/// Seed an agent dir with a complete, scannable agent.toml BEFORE the
/// handler scans (all required sections present — the registry loads it
/// through the full `AgentConfig` deserializer, not a raw table).
fn seed_agent(home: &std::path::Path, name: &str, os_native: bool) {
    let dir = home.join("agents").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let caps = if os_native {
        "[capabilities]\nos_native = true\n"
    } else {
        ""
    };
    let toml = format!(
        r#"[agent]
name = "{name}"
display_name = "{name}"
role = "specialist"
status = "active"
trigger = ""
reports_to = ""
icon = "🤖"

[model]
preferred = "claude-sonnet-4-6"
fallback = "claude-haiku-4-5"
account_pool = ["main"]

[container]
timeout_ms = 1800000
max_concurrent = 1
readonly_project = true
additional_mounts = []

[heartbeat]
enabled = false
interval_seconds = 3600
max_concurrent_runs = 1
cron = ""

[budget]
monthly_limit_cents = 5000
warn_threshold_percent = 80
hard_stop = true

[permissions]
can_create_agents = false
can_send_cross_agent = true
can_modify_own_skills = true
can_modify_own_soul = false
can_schedule_tasks = false
allowed_channels = ["*"]

[evolution]
micro_reflection = false
meso_reflection = false
macro_reflection = false
skill_auto_activate = false
skill_security_scan = true

{caps}"#
    );
    std::fs::write(dir.join("agent.toml"), toml).unwrap();
}

fn frame_ok(f: &WsFrame) -> bool {
    matches!(f, WsFrame::Response { ok: true, .. })
}

fn error_code(f: &WsFrame) -> Option<String> {
    match f {
        WsFrame::Response { error: Some(e), .. } => {
            e.get("code").and_then(|c| c.as_str()).map(String::from)
        }
        _ => None,
    }
}

#[tokio::test]
async fn personal_quota_rejects_second_os_native_agent() {
    // SAFETY: single-threaded test; ensure Personal edition regardless of
    // the ambient env so the quota is a deterministic 1.
    unsafe {
        std::env::set_var("DUDUCLAW_EDITION", "personal");
    }
    let home = tempfile::tempdir().unwrap();
    seed_agent(home.path(), "alpha", true); // already OS-native
    seed_agent(home.path(), "bravo", false);
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    // Turning bravo os_native ON would make 2 > quota(1) → structured reject.
    let frame = handler
        .handle_agents_update(json!({
            "agent_id": "bravo",
            "capabilities": { "os_native": true },
        }))
        .await;
    assert!(!frame_ok(&frame));
    assert_eq!(
        error_code(&frame).as_deref(),
        Some(OS_NATIVE_QUOTA_ERROR_CODE)
    );

    // The write must NOT have happened (fail-closed).
    let toml =
        std::fs::read_to_string(home.path().join("agents").join("bravo").join("agent.toml"))
            .unwrap();
    assert!(!toml.contains("os_native = true"));
    unsafe {
        std::env::remove_var("DUDUCLAW_EDITION");
    }
}

#[tokio::test]
async fn re_saving_already_os_native_agent_is_allowed_under_quota() {
    unsafe {
        std::env::set_var("DUDUCLAW_EDITION", "personal");
    }
    let home = tempfile::tempdir().unwrap();
    seed_agent(home.path(), "alpha", true);
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    // Re-saving alpha (already the one OS-native seat) must not be blocked.
    let frame = handler
        .handle_agents_update(json!({
            "agent_id": "alpha",
            "capabilities": { "os_native": true },
        }))
        .await;
    assert_ne!(
        error_code(&frame).as_deref(),
        Some(OS_NATIVE_QUOTA_ERROR_CODE),
        "idempotent re-save must not hit the quota gate: {frame:?}"
    );
    unsafe {
        std::env::remove_var("DUDUCLAW_EDITION");
    }
}

/// `agents.update` persists the `[proactive]` notify target
/// (channel / chat / thread), the goal-loop + gap-digest push destination.
#[tokio::test]
async fn agents_update_writes_proactive_notify_target() {
    let home = tempfile::tempdir().unwrap();
    seed_agent(home.path(), "alpha", false);
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let frame = handler
        .handle_agents_update(json!({
            "agent_id": "alpha",
            "proactive": {
                "notify_channel": "telegram",
                "notify_chat_id": "123456",
                "notify_thread_id": "42",
            },
        }))
        .await;
    assert!(frame_ok(&frame), "{frame:?}");

    let raw =
        std::fs::read_to_string(home.path().join("agents").join("alpha").join("agent.toml"))
            .unwrap();
    let table: toml::Table = raw.parse().unwrap();
    let p = table["proactive"].as_table().unwrap();
    assert_eq!(p["notify_channel"].as_str(), Some("telegram"));
    assert_eq!(p["notify_chat_id"].as_str(), Some("123456"));
    assert_eq!(p["notify_thread_id"].as_str(), Some("42"));

    // The goal-loop notifier resolves the same shape.
    assert_eq!(
        crate::goal_notify::agent_notify_target(home.path(), "alpha"),
        Some(("telegram".to_string(), "123456".to_string()))
    );
    // And it deserializes into the typed config (agents.inspect prefill).
    let cfg: duduclaw_core::types::ProactiveConfig = p.clone().try_into().unwrap();
    assert_eq!(cfg.notify_thread_id, "42");
}

// ── quiet_hours (W2-8 dashboard editor) ───────────────────────────────

/// `agents.update` persists a valid `[proactive] quiet_hours` window, and
/// `agents.inspect` reads it back both as the raw own-value the edit form
/// prefills (`quiet_hours_own`) and the effective note (`quiet_hours_note`)
/// the runtime gate would apply.
#[tokio::test]
async fn agents_update_writes_and_validates_quiet_hours() {
    let home = tempfile::tempdir().unwrap();
    seed_agent(home.path(), "alpha", false);
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let frame = handler
        .handle_agents_update(json!({
            "agent_id": "alpha",
            "proactive": { "quiet_hours": "22:00-08:00" },
        }))
        .await;
    assert!(frame_ok(&frame), "{frame:?}");

    let raw =
        std::fs::read_to_string(home.path().join("agents").join("alpha").join("agent.toml"))
            .unwrap();
    let table: toml::Table = raw.parse().unwrap();
    assert_eq!(
        table["proactive"]["quiet_hours"].as_str(),
        Some("22:00-08:00")
    );

    let inspect = handler
        .handle_agents_inspect(json!({ "agent_id": "alpha" }))
        .await;
    let payload = match inspect {
        WsFrame::Response {
            ok: true,
            payload: Some(p),
            ..
        } => p,
        other => panic!("expected ok, got {other:?}"),
    };
    assert_eq!(
        payload["proactive"]["quiet_hours_own"].as_str(),
        Some("22:00-08:00")
    );
    assert_eq!(
        payload["proactive"]["quiet_hours"].as_str(),
        Some("22:00-08:00")
    );
    let note = payload["proactive"]["quiet_hours_note"].as_str().unwrap();
    assert!(note.contains("22:00-08:00"), "{note}");

    // Round-trips through the exact reader the runtime gate uses.
    let policy = crate::notify_governance::load_agent_policy(home.path(), "alpha");
    assert_eq!(
        policy.window,
        crate::notify_governance::QuietWindow::parse("22:00-08:00")
    );
}

/// A malformed window is rejected at write time — fail-closed, not
/// silently accepted-then-ignored by the gate.
#[tokio::test]
async fn agents_update_rejects_malformed_quiet_hours() {
    let home = tempfile::tempdir().unwrap();
    seed_agent(home.path(), "alpha", false);
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let frame = handler
        .handle_agents_update(json!({
            "agent_id": "alpha",
            "proactive": { "quiet_hours": "not-a-window" },
        }))
        .await;
    assert!(!frame_ok(&frame), "malformed quiet_hours must be rejected");

    // The write must NOT have happened.
    let raw =
        std::fs::read_to_string(home.path().join("agents").join("alpha").join("agent.toml"))
            .unwrap();
    assert!(!raw.contains("quiet_hours"));
}

/// Empty string clears a previously-set window (readers treat blank as
/// unset — matches every other `[proactive]` string field's convention).
#[tokio::test]
async fn agents_update_clears_quiet_hours_with_empty_string() {
    let home = tempfile::tempdir().unwrap();
    seed_agent(home.path(), "alpha", false);
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    handler
        .handle_agents_update(json!({
            "agent_id": "alpha",
            "proactive": { "quiet_hours": "22:00-08:00" },
        }))
        .await;
    let frame = handler
        .handle_agents_update(json!({
            "agent_id": "alpha",
            "proactive": { "quiet_hours": "" },
        }))
        .await;
    assert!(frame_ok(&frame), "{frame:?}");

    let raw =
        std::fs::read_to_string(home.path().join("agents").join("alpha").join("agent.toml"))
            .unwrap();
    let table: toml::Table = raw.parse().unwrap();
    assert_eq!(table["proactive"]["quiet_hours"].as_str(), Some(""));
    assert_eq!(
        crate::notify_governance::agent_raw_quiet_hours(home.path(), "alpha"),
        ""
    );
}

// ── channel_recovered in the unified log (W2-8) ───────────────────────

/// `channel_alerts::record_recovery` appends a `channel_recovered` row
/// with no `error` field to `channel_failures.jsonl`. `audit.unified_log`
/// must render it as an informational "已恢復" line, not an empty
/// "warning" row indistinguishable from a blank/corrupt record.
#[tokio::test]
async fn unified_log_renders_channel_recovered_as_an_info_row_not_a_blank_warning() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("channel_failures.jsonl"),
        format!(
            "{}\n{}\n",
            json!({
                "event": "telegram_send_failed",
                "channel": "telegram",
                "agent": "alpha",
                "reason": "telegram_send_failed",
                "error": "401 Unauthorized",
                "timestamp": "2026-08-11T09:00:00Z",
            }),
            json!({
                "event": crate::channel_alerts::RECOVERED_EVENT,
                "channel": "telegram",
                "reason": "recovered",
                "resolved": true,
                "resolves": "telegram_send_failed",
                "timestamp": "2026-08-11T09:30:00Z",
            }),
        ),
    )
    .unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let frame = handler
        .handle_audit_unified_log(json!({ "sources": ["channel_failure"] }))
        .await;
    let payload = match frame {
        WsFrame::Response {
            ok: true,
            payload: Some(p),
            ..
        } => p,
        other => panic!("expected ok, got {other:?}"),
    };
    let events = payload["events"].as_array().unwrap();
    assert_eq!(events.len(), 2, "{events:?}");

    let failure = events
        .iter()
        .find(|e| e["event_type"] == "channel.telegram_send_failed")
        .expect("failure row present");
    assert_eq!(failure["severity"], "warning");
    assert_eq!(failure["summary"], "401 Unauthorized");

    let recovered = events
        .iter()
        .find(|e| e["event_type"] == "channel.recovered")
        .expect("recovery row present");
    assert_eq!(
        recovered["severity"], "info",
        "a recovery is good news, not a warning"
    );
    assert_eq!(recovered["summary"], "通道「Telegram」已恢復正常發送");
    assert_ne!(recovered["summary"], "", "must never render blank (F10)");
}

#[tokio::test]
async fn os_status_reports_quota_edition_and_agents() {
    unsafe {
        std::env::set_var("DUDUCLAW_EDITION", "personal");
    }
    let home = tempfile::tempdir().unwrap();
    seed_agent(home.path(), "alpha", true);
    seed_agent(home.path(), "bravo", false);
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let frame = handler.handle_os_status().await;
    let payload = match frame {
        WsFrame::Response {
            ok: true,
            payload: Some(p),
            ..
        } => p,
        other => panic!("expected ok, got {other:?}"),
    };
    assert_eq!(payload.get("edition").unwrap().as_str(), Some("personal"));
    assert_eq!(payload["quota"]["limit"].as_u64(), Some(1));
    assert_eq!(payload["quota"]["used"].as_u64(), Some(1)); // only alpha
    let agents = payload["agents"].as_array().unwrap();
    assert_eq!(agents.len(), 2);
    // Sorted by name → alpha first.
    assert_eq!(agents[0]["agent_id"].as_str(), Some("alpha"));
    assert_eq!(agents[0]["os_native"].as_bool(), Some(true));
    assert!(agents[0].get("proactive").is_some());
    assert!(agents[0].get("frontmost").is_some());
    assert_eq!(agents[1]["agent_id"].as_str(), Some("bravo"));
    assert_eq!(agents[1]["os_native"].as_bool(), Some(false));
    unsafe {
        std::env::remove_var("DUDUCLAW_EDITION");
    }
}

#[tokio::test]
async fn os_settings_update_remaps_and_writes_footprint() {
    let home = tempfile::tempdir().unwrap();
    seed_agent(home.path(), "alpha", true);
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let frame = handler
        .handle_os_settings_update(json!({
            "agent_id": "alpha",
            "footprint": true,
            "frontmost_poll_secs": 45,
            "proactive": { "enabled": true, "base_threshold": 4 },
        }))
        .await;
    assert!(
        frame_ok(&frame),
        "settings update should succeed: {frame:?}"
    );

    let toml =
        std::fs::read_to_string(home.path().join("agents").join("alpha").join("agent.toml"))
            .unwrap();
    assert!(toml.contains("footprint = true"));
    assert!(toml.contains("frontmost_poll_secs = 45"));
    assert!(toml.contains("base_threshold = 4"));
}

#[tokio::test]
async fn os_gate_recent_clamps_n_and_reads_tail() {
    let home = tempfile::tempdir().unwrap();
    seed_agent(home.path(), "alpha", true);
    // Write a few gate decisions.
    let lines = (0..5)
        .map(|i| {
            json!({
                "ts": Utc::now().to_rfc3339(),
                "agent": "alpha",
                "event": "os_file",
                "score": i,
                "decision": "suppress",
                "outcome": null,
            })
            .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(home.path().join("proactive_gate.jsonl"), lines).unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let frame = handler.handle_os_gate_recent(json!({ "n": 2 })).await;
    let payload = match frame {
        WsFrame::Response {
            ok: true,
            payload: Some(p),
            ..
        } => p,
        other => panic!("expected ok, got {other:?}"),
    };
    assert_eq!(payload["recent"].as_array().unwrap().len(), 2);
    assert!(payload.get("quadrants").is_some());

    // n over the max clamps to 200 (all 5 rows returned here).
    let frame2 = handler.handle_os_gate_recent(json!({ "n": 99999 })).await;
    if let WsFrame::Response {
        payload: Some(p), ..
    } = frame2
    {
        assert_eq!(p["recent"].as_array().unwrap().len(), 5);
    }
}

#[tokio::test]
async fn os_events_recent_reads_os_events_only() {
    let home = tempfile::tempdir().unwrap();
    // Pre-populate events.db with one os_ and one non-os event.
    {
        let store = crate::events_store::EventBusStore::open(home.path()).unwrap();
        store
            .append("task.created", r#"{"id":"t1"}"#)
            .await
            .unwrap();
        store
            .append(
                "os_file",
                r#"{"agent_id":"alpha","path":"/x.pdf","kind":"created"}"#,
            )
            .await
            .unwrap();
    }
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler.handle_os_events_recent(json!({ "n": 10 })).await;
    let payload = match frame {
        WsFrame::Response {
            ok: true,
            payload: Some(p),
            ..
        } => p,
        other => panic!("expected ok, got {other:?}"),
    };
    let events = payload["events"].as_array().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["event"].as_str(), Some("os_file"));
    assert_eq!(events[0]["payload"]["agent_id"].as_str(), Some("alpha"));
}

// ── P4-3+: os.events.subscribe / unsubscribe (live tail opt-in) ──────

fn manager_ctx() -> UserContext {
    UserContext {
        user_id: "m1".to_string(),
        email: "m1@test.local".to_string(),
        role: UserRole::Manager,
        agent_access: std::collections::HashMap::new(),
        must_change_password: false,
    }
}

#[tokio::test]
async fn os_events_subscribe_acks_for_admin() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = UserContext::admin_fallback();

    let frame = handler.handle("os.events.subscribe", json!({}), &ctx).await;
    assert!(frame_ok(&frame), "expected ok, got {frame:?}");
    let payload = match frame {
        WsFrame::Response {
            payload: Some(p), ..
        } => p,
        other => panic!("expected payload, got {other:?}"),
    };
    assert_eq!(payload["subscribed"].as_bool(), Some(true));

    let frame2 = handler
        .handle("os.events.unsubscribe", json!({}), &ctx)
        .await;
    assert!(frame_ok(&frame2), "expected ok, got {frame2:?}");
}

/// A non-admin (manager) caller must be denied — os_file/os_frontmost
/// events can carry filesystem paths and window titles, so the live tail
/// must stay behind the SAME `require_admin!` gate as every other `os.*`
/// RPC (`os.status`, `os.gate.recent`, etc.), not a lower bar.
#[tokio::test]
async fn os_events_subscribe_denies_non_admin() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = manager_ctx();

    let frame = handler.handle("os.events.subscribe", json!({}), &ctx).await;
    assert!(!frame_ok(&frame), "manager must be denied, got {frame:?}");

    let frame2 = handler
        .handle("os.events.unsubscribe", json!({}), &ctx)
        .await;
    assert!(!frame_ok(&frame2), "manager must be denied, got {frame2:?}");
}
