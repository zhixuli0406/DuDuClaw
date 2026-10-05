//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

//! D6 HITL knowledge-graph curation RPCs: memory.graph export,
//! memory.invalidate_origin rollback, and the D3 retrieval-weights wiring.
use super::*;

/// B5 (OS security line P0): `handle_system_update_config` now takes a
/// `&UserContext` so `config_changed` audit events can attribute who
/// made the change. This module's direct-call tests bypass the
/// `require_admin!()` dispatch gate entirely (they call the handler, not
/// `.handle(method, params, ctx)`), so the role here is not enforced —
/// it only needs to be a plausible fixture value.
pub(super) fn admin_ctx() -> UserContext {
    UserContext {
        user_id: "admin-test".to_string(),
        email: "admin@test.local".to_string(),
        role: UserRole::Admin,
        agent_access: std::collections::HashMap::new(),
        must_change_password: false,
    }
}

pub(super) fn frame_ok(frame: &WsFrame) -> bool {
    matches!(frame, WsFrame::Response { ok: true, .. })
}

pub(super) fn frame_data(frame: &WsFrame) -> Value {
    match frame {
        WsFrame::Response {
            payload: Some(d), ..
        } => d.clone(),
        _ => Value::Null,
    }
}

pub(super) fn triple_entry(agent: &str, content: &str) -> duduclaw_core::types::MemoryEntry {
    duduclaw_core::types::MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: agent.to_string(),
        content: content.to_string(),
        timestamp: Utc::now(),
        tags: vec![],
        embedding: None,
        layer: Default::default(),
        importance: 5.0,
        access_count: 0,
        last_accessed: None,
        source_event: String::new(),
    }
}

/// Seed `agents/<id>/memory.db` with a couple of SPO triples so the graph
/// export has nodes + edges. Returns the memory-db path.
pub(super) async fn seed_agent_memory(home: &std::path::Path, agent: &str) -> std::path::PathBuf {
    let db = home.join("agents").join(agent).join("memory.db");
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let engine = SqliteMemoryEngine::new(&db).unwrap();
    engine
        .store_temporal(
            agent,
            triple_entry(agent, "Alice works at Acme"),
            duduclaw_memory::TemporalMeta {
                subject: Some("alice".into()),
                predicate: Some("works_at".into()),
                object: Some("acme".into()),
                origin: Some("chan-good".into()),
                origin_trust: Some(0.9),
                ..Default::default()
            }, duduclaw_memory::lineage::Provenance::test_only(),
        )
        .await
        .unwrap();
    engine
        .store_temporal(
            agent,
            triple_entry(agent, "Bob knows Alice"),
            duduclaw_memory::TemporalMeta {
                subject: Some("bob".into()),
                predicate: Some("knows".into()),
                object: Some("alice".into()),
                origin: Some("chan-bad".into()),
                origin_trust: Some(0.3),
                ..Default::default()
            }, duduclaw_memory::lineage::Provenance::test_only(),
        )
        .await
        .unwrap();
    db
}

#[tokio::test]
pub(super) async fn memory_graph_happy_path() {
    let home = tempfile::tempdir().unwrap();
    let agent = "agent-graph";
    seed_agent_memory(home.path(), agent).await;
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let frame = handler
        .handle_memory_graph(json!({ "agent_id": agent }))
        .await;
    assert!(frame_ok(&frame), "graph export must succeed: {frame:?}");
    let data = frame_data(&frame);
    let nodes = data.get("nodes").and_then(|v| v.as_array()).unwrap();
    let edges = data.get("edges").and_then(|v| v.as_array()).unwrap();
    assert!(!nodes.is_empty(), "expected entity nodes");
    assert_eq!(edges.len(), 2, "two triples ⇒ two edges");
    assert_eq!(data.get("truncated").and_then(|v| v.as_bool()), Some(false));
    // Provenance surfaces on the edge (origin_trust drives the colour tier).
    let has_trust = edges
        .iter()
        .all(|e| e.get("origin_trust").and_then(|v| v.as_f64()).is_some());
    assert!(has_trust, "every edge carries origin_trust");
}

#[tokio::test]
pub(super) async fn memory_graph_missing_agent_id_fails() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler.handle_memory_graph(json!({})).await;
    assert!(!frame_ok(&frame), "missing agent_id must be rejected");
}

// ── v1.39: system.update_config new knobs ────────────────────────────────

#[tokio::test]
pub(super) async fn system_update_config_v139_knobs_persist_and_validate() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    // Valid nested payload across all five sections.
    let frame = handler
        .handle_system_update_config(json!({
            "knowledge_guard": { "enabled": false, "window_secs": 7200, "max_per_subject": 3 },
            "goal_loop": { "planner_enabled": true, "iteration_cap_simple": 5 },
            "dispatch": { "policy": "round_robin" },
            "memory": { "graph_embed_seed": true },
            "topology_evolution": { "enabled": false },
        }), &admin_ctx())
        .await;
    assert!(frame_ok(&frame), "valid knobs must persist: {frame:?}");
    let data = frame_data(&frame);
    // iteration_cap_simple / dispatch.policy → goal_loop driver hot reload.
    let hot: Vec<String> = data
        .get("hot_reloaded")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        hot.contains(&"goal_loop".to_string()),
        "goal_loop reload flagged: {hot:?}"
    );
    // "easy" knobs (knowledge_guard/memory/planner) → applied:true.
    assert_eq!(data.get("applied").and_then(|v| v.as_bool()), Some(true));

    // The values landed in config.toml.
    let cfg: toml::Table = std::fs::read_to_string(home.path().join("config.toml"))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        cfg["knowledge_guard"]["window_secs"].as_integer(),
        Some(7200)
    );
    assert_eq!(
        cfg["goal_loop"]["iteration_cap_simple"].as_integer(),
        Some(5)
    );
    assert_eq!(cfg["dispatch"]["policy"].as_str(), Some("round_robin"));
    assert_eq!(cfg["memory"]["graph_embed_seed"].as_bool(), Some(true));

    // Re-read through GoalLoopConfig / DispatchPolicyKind to prove the shape
    // is what the consumers parse.
    assert_eq!(
        crate::goal_loop::GoalLoopConfig::from_home(home.path()).iteration_cap_simple,
        5
    );
    assert_eq!(
        crate::dispatch_policy::DispatchPolicyKind::from_home(home.path()).as_str(),
        "round_robin"
    );
}

/// [skills] gap_digest_enabled round-trip: system.update_config persists
/// the flag, skill_gap_digest's parser reads it back, and system.config
/// exposes the structured value for the dashboard toggle.
#[tokio::test]
pub(super) async fn system_update_config_gap_digest_enabled_round_trip() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    // Default (no config) ⇒ false in system.config.
    let frame = handler.handle_system_config().await;
    // No config.toml yet — handler errors on read; write first, then read.
    let _ = frame;

    let frame = handler
        .handle_system_update_config(json!({ "gap_digest_enabled": true }), &admin_ctx())
        .await;
    assert!(
        frame_ok(&frame),
        "gap_digest_enabled=true must persist: {frame:?}"
    );

    let raw = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    let cfg: toml::Table = raw.parse().unwrap();
    assert_eq!(cfg["skills"]["gap_digest_enabled"].as_bool(), Some(true));
    // The digest consumer parses the same shape.
    assert!(crate::skill_gap_digest::gap_digest_enabled_from_str(&raw));

    // system.config surfaces the structured flag for the dashboard.
    let frame = handler.handle_system_config().await;
    assert!(frame_ok(&frame));
    let data = frame_data(&frame);
    assert_eq!(
        data.get("gap_digest_enabled").and_then(|v| v.as_bool()),
        Some(true)
    );

    // Turning it back off round-trips too.
    let frame = handler
        .handle_system_update_config(json!({ "gap_digest_enabled": false }), &admin_ctx())
        .await;
    assert!(frame_ok(&frame));
    let raw = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert!(!crate::skill_gap_digest::gap_digest_enabled_from_str(&raw));
}

/// [memory] novelty_gate round-trip: system.update_config persists the
/// flag and system.config exposes the structured value (default `true`
/// when absent, matching `mcp.rs::novelty_gate_enabled_from_config`'s
/// fail-closed default) for the dashboard toggle.
#[tokio::test]
pub(super) async fn system_update_config_novelty_gate_enabled_round_trip() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    // Turn it off.
    let frame = handler
        .handle_system_update_config(json!({ "novelty_gate_enabled": false }), &admin_ctx())
        .await;
    assert!(
        frame_ok(&frame),
        "novelty_gate_enabled=false must persist: {frame:?}"
    );

    let raw = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    let cfg: toml::Table = raw.parse().unwrap();
    assert_eq!(cfg["memory"]["novelty_gate"].as_bool(), Some(false));

    // system.config surfaces the structured flag for the dashboard.
    let frame = handler.handle_system_config().await;
    assert!(frame_ok(&frame));
    let data = frame_data(&frame);
    assert_eq!(
        data.get("novelty_gate_enabled").and_then(|v| v.as_bool()),
        Some(false)
    );

    // Turning it back on round-trips too.
    let frame = handler
        .handle_system_update_config(json!({ "novelty_gate_enabled": true }), &admin_ctx())
        .await;
    assert!(frame_ok(&frame));
    let raw = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    let cfg: toml::Table = raw.parse().unwrap();
    assert_eq!(cfg["memory"]["novelty_gate"].as_bool(), Some(true));
}

/// Absent config ⇒ `system.config` reports the fail-closed default
/// (`true`), matching `mcp.rs::novelty_gate_enabled_from_config`'s
/// default so the dashboard toggle never shows a stale/wrong initial
/// state on a fresh install.
#[tokio::test]
pub(super) async fn system_config_novelty_gate_enabled_defaults_true_when_absent() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    // No config.toml at all yet.
    std::fs::write(home.path().join("config.toml"), "").unwrap();

    let frame = handler.handle_system_config().await;
    assert!(frame_ok(&frame));
    let data = frame_data(&frame);
    assert_eq!(
        data.get("novelty_gate_enabled").and_then(|v| v.as_bool()),
        Some(true)
    );
}

/// [notify] daily_digest / daily_digest_at round-trip (W2-8): persists,
/// `DigestConfig::from_home` reads the same shape back, and
/// `system.config` exposes both structured fields for the dashboard.
#[tokio::test]
pub(super) async fn system_update_config_daily_digest_round_trip() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    // Absent config ⇒ fail-closed default (off, 09:00).
    std::fs::write(home.path().join("config.toml"), "").unwrap();
    let frame = handler.handle_system_config().await;
    assert!(frame_ok(&frame));
    let data = frame_data(&frame);
    assert_eq!(
        data.get("daily_digest_enabled").and_then(|v| v.as_bool()),
        Some(false)
    );
    assert_eq!(
        data.get("daily_digest_at").and_then(|v| v.as_str()),
        Some("09:00")
    );

    let frame = handler
        .handle_system_update_config(
            json!({ "daily_digest": true, "daily_digest_at": "07:30" }),
            &admin_ctx(),
        )
        .await;
    assert!(frame_ok(&frame), "{frame:?}");

    let raw = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    let cfg: toml::Table = raw.parse().unwrap();
    assert_eq!(cfg["notify"]["daily_digest"].as_bool(), Some(true));
    assert_eq!(cfg["notify"]["daily_digest_at"].as_str(), Some("07:30"));
    // The scheduler's own reader parses the exact same shape.
    let digest_cfg = crate::notify_digest::DigestConfig::from_toml_str(&raw);
    assert!(digest_cfg.enabled);
    assert_eq!(
        digest_cfg.at,
        chrono::NaiveTime::from_hms_opt(7, 30, 0).unwrap()
    );

    // system.config surfaces both structured fields for the dashboard.
    let frame = handler.handle_system_config().await;
    assert!(frame_ok(&frame));
    let data = frame_data(&frame);
    assert_eq!(
        data.get("daily_digest_enabled").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        data.get("daily_digest_at").and_then(|v| v.as_str()),
        Some("07:30")
    );

    // Turning it back off round-trips too.
    let frame = handler
        .handle_system_update_config(json!({ "daily_digest": false }), &admin_ctx())
        .await;
    assert!(frame_ok(&frame));
    let raw = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    let cfg: toml::Table = raw.parse().unwrap();
    assert_eq!(cfg["notify"]["daily_digest"].as_bool(), Some(false));
    // The time set earlier is untouched by an update that only sent the flag.
    assert_eq!(cfg["notify"]["daily_digest_at"].as_str(), Some("07:30"));
}

/// A malformed `daily_digest_at` is rejected at write time — fail-closed,
/// not silently accepted then blanked to 09:00 by the scheduler's own
/// fail-open read path hours later.
#[tokio::test]
pub(super) async fn system_update_config_rejects_malformed_daily_digest_at() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let frame = handler
        .handle_system_update_config(json!({ "daily_digest_at": "早上九點" }), &admin_ctx())
        .await;
    assert!(
        !frame_ok(&frame),
        "malformed daily_digest_at must be rejected"
    );

    // Nothing was written.
    let raw = std::fs::read_to_string(home.path().join("config.toml")).unwrap_or_default();
    assert!(!raw.contains("daily_digest_at"));
}

/// S20: `[miniapp] enabled` round-trips through `system.update_config` and
/// is reported back by `system.config`. Before 2026-09 the key had no
/// dashboard surface at all (`grep miniapp web/src` was empty), so the
/// Telegram Mini App could only be turned on by hand-editing config.toml.
/// Default stays `false` — absent config must report off.
#[tokio::test]
pub(super) async fn system_update_config_miniapp_enabled_round_trip() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    // Absent config ⇒ reported off (matches `miniapp::enabled`).
    let frame = handler.handle_system_config().await;
    if frame_ok(&frame) {
        assert_eq!(
            frame_data(&frame)
                .get("miniapp_enabled")
                .and_then(|v| v.as_bool()),
            Some(false),
            "absent [miniapp] must report disabled"
        );
    }

    let frame = handler
        .handle_system_update_config(json!({ "miniapp_enabled": true }), &admin_ctx())
        .await;
    assert!(frame_ok(&frame), "miniapp_enabled=true must persist: {frame:?}");

    let cfg: toml::Table = std::fs::read_to_string(home.path().join("config.toml"))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(cfg["miniapp"]["enabled"].as_bool(), Some(true));
    // The routes' own reader agrees — no second parsing dialect.
    assert!(crate::miniapp::enabled(home.path()));

    let frame = handler.handle_system_config().await;
    assert!(frame_ok(&frame));
    assert_eq!(
        frame_data(&frame)
            .get("miniapp_enabled")
            .and_then(|v| v.as_bool()),
        Some(true)
    );

    // And back off.
    let frame = handler
        .handle_system_update_config(json!({ "miniapp_enabled": false }), &admin_ctx())
        .await;
    assert!(frame_ok(&frame));
    assert!(!crate::miniapp::enabled(home.path()));
}

#[tokio::test]
pub(super) async fn system_update_config_rejects_bad_dispatch_policy_and_cap() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    // Unknown dispatch.policy enum value → rejected.
    let bad_policy = handler
        .handle_system_update_config(
            json!({ "dispatch": { "policy": "chaos_monkey" } }),
            &admin_ctx(),
        )
        .await;
    assert!(
        !frame_ok(&bad_policy),
        "invalid dispatch.policy must be rejected"
    );

    // iteration_cap_simple out of the 1..=20 range → rejected.
    let bad_cap = handler
        .handle_system_update_config(
            json!({ "goal_loop": { "iteration_cap_simple": 99 } }),
            &admin_ctx(),
        )
        .await;
    assert!(
        !frame_ok(&bad_cap),
        "out-of-range iteration_cap_simple must be rejected"
    );

    // knowledge_guard.window_secs = 0 → rejected.
    let bad_win = handler
        .handle_system_update_config(
            json!({ "knowledge_guard": { "window_secs": 0 } }),
            &admin_ctx(),
        )
        .await;
    assert!(!frame_ok(&bad_win), "window_secs=0 must be rejected");

    // A rejected payload must not create config.toml.
    assert!(
        !home.path().join("config.toml").exists(),
        "no partial write on validation failure"
    );
}

/// WP-5D judge seam: `[dispatch] judge` accepts exactly `mav` and
/// `external` (v1.69.0 removed `evaluator_only` / `human_only`) and persists
/// the canonical token `JudgeMode::from_home` reads back. Hard boundaries
/// asserted here because they are the seam's security posture:
/// - an unknown value is REJECTED at write time (the read path
///   additionally falls back to `mav`, so both layers fail safe);
/// - a removed value (and its old alias) is REJECTED at write time with a
///   message an end user can act on, and the stored value is untouched;
/// - `judge_command` / `judge_timeout_secs` are NOT settable through this
///   RPC at all — they name an executable, and this method is reachable
///   from the dashboard.
#[tokio::test]
pub(super) async fn system_update_config_judge_seam_whitelist() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    for mode in ["mav", "external"] {
        let f = handler
            .handle_system_update_config(json!({ "dispatch": { "judge": mode } }), &admin_ctx())
            .await;
        assert!(frame_ok(&f), "{mode} must be accepted");
        assert_eq!(
            crate::judge_mode::JudgeMode::from_home(Some(home.path())),
            crate::judge_mode::JudgeMode::from_config_str(mode).unwrap(),
            "{mode} must round-trip through config.toml"
        );
    }

    // Removed values → rejected with an actionable zh-TW message that names
    // the replacement and no internal file name; the last good value
    // survives.
    for removed in ["evaluator_only", "human_only", "evaluator", "Human"] {
        let f = handler
            .handle_system_update_config(
                json!({ "dispatch": { "judge": removed } }),
                &admin_ctx(),
            )
            .await;
        assert!(!frame_ok(&f), "removed value {removed} must be rejected");
        let msg = match &f {
            WsFrame::Response { error: Some(e), .. } => e.to_string(),
            _ => String::new(),
        };
        assert!(msg.contains("v1.69.0"), "{removed}: {msg}");
        assert!(msg.contains("標準驗收"), "{removed}: {msg}");
        assert!(!msg.contains("config.toml"), "no internal file names: {msg}");
        assert!(!msg.contains(".rs"), "no internal file names: {msg}");
        assert_eq!(
            crate::judge_mode::JudgeMode::from_home(Some(home.path())),
            crate::judge_mode::JudgeMode::External,
            "a refused write must leave the stored value alone ({removed})"
        );
    }
    let raw = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert!(!raw.contains("human_only") && !raw.contains("evaluator_only"), "{raw}");

    // Unknown value → rejected, and the last good value survives.
    let bad = handler
        .handle_system_update_config(
            json!({ "dispatch": { "judge": "eval_backed" } }),
            &admin_ctx(),
        )
        .await;
    assert!(!frame_ok(&bad), "unknown judge mode must be rejected");
    assert_eq!(
        crate::judge_mode::JudgeMode::from_home(Some(home.path())),
        crate::judge_mode::JudgeMode::External
    );

    // The command is operator-only: this RPC must never write it (the key
    // is simply not in the `[dispatch]` whitelist, so the payload carries
    // no recognized change at all).
    let _ = handler
        .handle_system_update_config(
            json!({
                "dispatch": { "judge_command": ["/bin/sh", "-c", "echo pwned"] }
            }),
            &admin_ctx(),
        )
        .await;
    let raw = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert!(
        !raw.contains("judge_command"),
        "judge_command must never be settable over RPC: {raw}"
    );
    assert_eq!(
        crate::judge_mode::ExternalJudgeConfig::from_home(Some(home.path())),
        None
    );
}

/// WP-E: `[goal_loop] resume_on_restart` whitelist accepts exactly
/// "auto"/"pause" and persists the exact value `GoalLoopConfig::from_home`
/// reads back — this is a boot-only-read field (see the doc comment on
/// the write-site above), so it must persist WITHOUT being flagged as
/// `applied` or `hot_reloaded` (unlike the "easy"/"hard" knobs in the
/// same section).
#[tokio::test]
pub(super) async fn system_update_config_resume_on_restart_accepts_auto_and_pause() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let frame = handler
        .handle_system_update_config(
            json!({ "goal_loop": { "resume_on_restart": "pause" } }),
            &admin_ctx(),
        )
        .await;
    assert!(frame_ok(&frame), "\"pause\" must be accepted: {frame:?}");
    let data = frame_data(&frame);
    assert_eq!(
        data.get("applied").and_then(|v| v.as_bool()),
        Some(false),
        "resume_on_restart alone must not be reported as live-applied — it only takes effect on next gateway restart"
    );
    let hot: Vec<String> = data
        .get("hot_reloaded")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        hot.is_empty(),
        "resume_on_restart must never trigger a driver hot reload: {hot:?}"
    );

    let cfg: toml::Table = std::fs::read_to_string(home.path().join("config.toml"))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        cfg["goal_loop"]["resume_on_restart"].as_str(),
        Some("pause")
    );
    assert_eq!(
        crate::goal_loop::GoalLoopConfig::from_home(home.path()).resume_on_restart(),
        crate::goal_loop::ResumeOnRestart::Pause
    );

    // Round-trips back to "auto" too.
    let frame = handler
        .handle_system_update_config(
            json!({ "goal_loop": { "resume_on_restart": "auto" } }),
            &admin_ctx(),
        )
        .await;
    assert!(frame_ok(&frame), "\"auto\" must be accepted: {frame:?}");
    assert_eq!(
        crate::goal_loop::GoalLoopConfig::from_home(home.path()).resume_on_restart(),
        crate::goal_loop::ResumeOnRestart::Auto
    );
}

/// WP-E: any value other than exactly "auto"/"pause" is rejected
/// fail-closed at write time, and — matching the sibling dispatch.policy
/// / iteration_cap_simple / window_secs rejection test above — a
/// rejected payload must not create config.toml at all.
#[tokio::test]
pub(super) async fn system_update_config_rejects_bad_resume_on_restart() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    for bad in [
        "Auto", "PAUSE", "pausing", "", "auto ", " pause", "yes", "true",
    ] {
        let frame = handler
            .handle_system_update_config(
                json!({ "goal_loop": { "resume_on_restart": bad } }),
                &admin_ctx(),
            )
            .await;
        assert!(
            !frame_ok(&frame),
            "resume_on_restart={bad:?} must be rejected (whitelist is exactly auto/pause, no case-folding)"
        );
    }

    assert!(
        !home.path().join("config.toml").exists(),
        "no partial write on validation failure"
    );
}

/// `[belief] flat_band_pct` / `tick_subject_map` round-trip: persists to
/// config.toml, `BeliefConfig::from_home` reads the exact same shape back
/// (proving the write is what the settlement path actually consumes).
#[tokio::test]
pub(super) async fn system_update_config_belief_round_trips() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let frame = handler
        .handle_system_update_config(
            json!({
                "belief": {
                    "flat_band_pct": 0.75,
                    "tick_subject_map": {
                        "conversion_rate": "trial_conversion_rate",
                        "complaint_count": "daily_complaints",
                    },
                },
            }),
            &admin_ctx(),
        )
        .await;
    assert!(
        frame_ok(&frame),
        "valid belief knobs must persist: {frame:?}"
    );
    let data = frame_data(&frame);
    assert_eq!(data.get("applied").and_then(|v| v.as_bool()), Some(true));

    let raw = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    let cfg: toml::Table = raw.parse().unwrap();
    assert!((cfg["belief"]["flat_band_pct"].as_float().unwrap() - 0.75).abs() < 1e-9);
    assert_eq!(
        cfg["belief"]["tick_subject_map"]["conversion_rate"].as_str(),
        Some("trial_conversion_rate")
    );

    // The exact same shape re-parses through the real consumer.
    let parsed = crate::prediction::belief::BeliefConfig::from_home(home.path());
    assert!((parsed.flat_band_pct - 0.75).abs() < 1e-9);
    assert_eq!(
        parsed
            .tick_subject_map
            .get("complaint_count")
            .map(String::as_str),
        Some("daily_complaints")
    );
}
