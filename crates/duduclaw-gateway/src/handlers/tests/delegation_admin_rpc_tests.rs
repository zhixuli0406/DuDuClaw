//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

//! WP21 §2.8 — `delegation.get` / `delegation.set` dashboard RPCs.
use super::*;

/// Seed a scannable agent dir (the registry loads through the full
/// `AgentConfig` deserializer, so every required section must be present).
fn seed_agent(home: &std::path::Path, name: &str, department: &str) {
    seed_agent_in_dir(home, name, name, department);
}

/// Same as [`seed_agent`] but the directory name and `[agent] name` can
/// differ — WP21 欠帳③ fixture: the registry indexes by `name`, while bus
/// tasks / the C1 predicate key agents by directory name.
fn seed_agent_in_dir(home: &std::path::Path, dir_name: &str, name: &str, department: &str) {
    let dir = home.join("agents").join(dir_name);
    std::fs::create_dir_all(&dir).unwrap();
    let toml = format!(
        r#"[agent]
name = "{name}"
display_name = "{name}"
role = "specialist"
status = "active"
trigger = ""
reports_to = ""
department = "{department}"
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
"#
    );
    std::fs::write(dir.join("agent.toml"), toml).unwrap();
}

fn frame_ok(f: &WsFrame) -> bool {
    matches!(f, WsFrame::Response { ok: true, .. })
}

fn frame_data(f: &WsFrame) -> Value {
    match f {
        WsFrame::Response { payload, .. } => payload.clone().unwrap_or(Value::Null),
        other => panic!("expected response, got {other:?}"),
    }
}

fn frame_error(f: &WsFrame) -> String {
    match f {
        WsFrame::Response { error: Some(e), .. } => e.to_string(),
        other => panic!("expected error frame, got {other:?}"),
    }
}

fn admin_ctx() -> UserContext {
    UserContext::admin_fallback()
}

fn manager_ctx() -> UserContext {
    UserContext {
        user_id: "m1".to_string(),
        email: "m1@test.local".to_string(),
        role: UserRole::Manager,
        agent_access: std::collections::HashMap::new(),
        must_change_password: false,
    }
}

/// Fresh home, no `[delegation]` section ⇒ the safe defaults, never an error.
#[tokio::test]
async fn get_returns_defaults_without_config() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle("delegation.get", json!({}), &admin_ctx())
        .await;
    assert!(frame_ok(&frame), "get must not error: {frame:?}");
    let data = frame_data(&frame);
    assert_eq!(data["policy"], json!("department"));
    assert_eq!(data["allow"], json!([]));
    assert_eq!(data["warnings"], json!([]));
}

/// A garbage policy value and malformed whitelist rows are reported as
/// warnings and cleaned, not fatal — the operator must still see the page.
#[tokio::test]
async fn get_reports_warnings_for_bad_values() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        "[delegation]\npolicy = \"anarchy\"\nallow = [[\"a\", \"b\"], [\"solo\"], [\"c\", \"c\"], [\"b\", \"a\"]]\n",
    )
    .unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let data = frame_data(
        &handler
            .handle("delegation.get", json!({}), &admin_ctx())
            .await,
    );

    // Unknown policy falls back to the stricter default.
    assert_eq!(data["policy"], json!("department"));
    // ["b","a"] is the same unordered pair as ["a","b"] ⇒ deduped.
    assert_eq!(data["allow"], json!([["a", "b"]]));
    let warnings = data["warnings"].as_array().unwrap();
    assert_eq!(
        warnings.len(),
        3,
        "policy + short row + self-pair: {warnings:?}"
    );
}

/// Both RPCs are owner/admin-only, fail-closed before any file I/O.
#[tokio::test]
async fn non_admin_is_denied() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = manager_ctx();
    for (method, params) in [
        ("delegation.get", json!({})),
        ("delegation.set", json!({ "policy": "open" })),
    ] {
        let frame = handler.handle(method, params, &ctx).await;
        assert!(!frame_ok(&frame), "{method} must deny a manager: {frame:?}");
    }
    assert!(
        !home.path().join("config.toml").exists(),
        "a denied call must not write config"
    );
}

/// Happy path: policy + whitelist land in config.toml and read back.
#[tokio::test]
async fn set_persists_policy_and_pairs() {
    let home = tempfile::tempdir().unwrap();
    seed_agent(home.path(), "sales-lead", "sales");
    seed_agent(home.path(), "warehouse-lead", "warehouse");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let frame = handler
        .handle(
            "delegation.set",
            json!({ "policy": "hierarchy", "allow": [["sales-lead", "warehouse-lead"]] }),
            &admin_ctx(),
        )
        .await;
    assert!(frame_ok(&frame), "set must succeed: {frame:?}");

    let raw = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert!(raw.contains("[delegation]"), "{raw}");
    assert!(raw.contains("hierarchy"), "{raw}");

    let data = frame_data(
        &handler
            .handle("delegation.get", json!({}), &admin_ctx())
            .await,
    );
    assert_eq!(data["policy"], json!("hierarchy"));
    assert_eq!(data["allow"], json!([["sales-lead", "warehouse-lead"]]));

    // The change is auditable with actor + before/after.
    let audit = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
    assert!(audit.contains("delegation_config_changed"), "{audit}");
    assert!(audit.contains("\"before\""), "{audit}");
    assert!(audit.contains("\"after\""), "{audit}");
}

/// WP21 欠帳③ — namespace unification, directory-name = agent-name case
/// (the common case, and what every other test in this module already
/// exercises implicitly via `seed_agent`). Spelled out explicitly so a
/// future change to the resolution order has one direct regression test.
#[tokio::test]
async fn set_keeps_ids_unchanged_when_dir_name_equals_agent_name() {
    let home = tempfile::tempdir().unwrap();
    seed_agent(home.path(), "sales-lead", "sales");
    seed_agent(home.path(), "warehouse-lead", "warehouse");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let frame = handler
        .handle(
            "delegation.set",
            json!({ "allow": [["sales-lead", "warehouse-lead"]] }),
            &admin_ctx(),
        )
        .await;
    assert!(frame_ok(&frame), "{frame:?}");
    assert_eq!(
        frame_data(&frame)["allow"],
        json!([["sales-lead", "warehouse-lead"]])
    );
}

/// WP21 欠帳③ — the core regression: when an agent's directory name and
/// `[agent] name` differ, `delegation.set` must persist the **directory
/// name** (the namespace `gate_bus_dispatch` / `DispatchOrgView` actually
/// judge against — bus tasks carry `sender_agent`/`target` as directory
/// names), not the `name` the caller typed. Otherwise the pair passes
/// validation but the whitelist entry never matches at enforcement time.
/// This test proves both ends: the persisted value is normalized, and the
/// C1 predicate (`gate_bus_dispatch`) actually honours it afterward.
#[tokio::test]
async fn set_normalizes_name_to_directory_name_and_predicate_honours_it() {
    let home = tempfile::tempdir().unwrap();
    // Directory name != `[agent] name` for both agents in the pair, and
    // the two are in different departments so nothing but the explicit
    // whitelist entry could let a cross-department dispatch through.
    seed_agent_in_dir(home.path(), "biz-dev-dir", "sales-lead", "sales");
    seed_agent_in_dir(home.path(), "wh-dir", "warehouse-lead", "warehouse");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    // Caller enters the display `name`, not the directory name — exactly
    // what the dashboard's agent picker would submit today.
    let frame = handler
        .handle(
            "delegation.set",
            json!({ "allow": [["sales-lead", "warehouse-lead"]] }),
            &admin_ctx(),
        )
        .await;
    assert!(frame_ok(&frame), "set must succeed: {frame:?}");

    // Persisted value is normalized to directory names, not the typed name.
    let raw = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert!(raw.contains("biz-dev-dir"), "{raw}");
    assert!(raw.contains("wh-dir"), "{raw}");
    assert!(!raw.contains("sales-lead"), "{raw}");
    assert!(!raw.contains("warehouse-lead"), "{raw}");

    let data = frame_data(
        &handler
            .handle("delegation.get", json!({}), &admin_ctx())
            .await,
    );
    assert_eq!(data["allow"], json!([["biz-dev-dir", "wh-dir"]]));

    // Predicate side: the C1 gate judges dispatch using the same
    // directory-name namespace the bus actually carries. Cross-department
    // strangers only get through via the whitelist — and only because the
    // saved entry now matches that namespace instead of the stale `name`.
    let registry = Arc::new(RwLock::new(AgentRegistry::new(home.path().join("agents"))));
    let outcome = crate::delegation_gate::gate_bus_dispatch(
        home.path(),
        &registry,
        Some("biz-dev-dir"),
        None,
        "wh-dir",
        "task-1",
        "bus_dispatch",
    )
    .await;
    assert!(
        !matches!(outcome, crate::delegation_gate::GateOutcome::Deny(_)),
        "whitelist must be enforceable after normalization: {outcome:?}"
    );
}

/// A pair where one side is typed by directory name and the other by
/// `[agent] name` still resolves — either spelling is accepted as input,
/// both normalize to the same directory-name namespace.
#[tokio::test]
async fn set_accepts_either_dir_name_or_agent_name_as_input() {
    let home = tempfile::tempdir().unwrap();
    seed_agent_in_dir(home.path(), "biz-dev-dir", "sales-lead", "sales");
    seed_agent_in_dir(home.path(), "wh-dir", "warehouse-lead", "warehouse");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let frame = handler
        .handle(
            "delegation.set",
            // "biz-dev-dir" (directory name) paired with "warehouse-lead"
            // (agent name) — both resolve to the same directory-name pair.
            json!({ "allow": [["biz-dev-dir", "warehouse-lead"]] }),
            &admin_ctx(),
        )
        .await;
    assert!(frame_ok(&frame), "{frame:?}");
    assert_eq!(
        frame_data(&frame)["allow"],
        json!([["biz-dev-dir", "wh-dir"]])
    );
}

/// An unknown policy value rejects the whole payload (nothing written).
#[tokio::test]
async fn set_rejects_unknown_policy() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle(
            "delegation.set",
            json!({ "policy": "anarchy" }),
            &admin_ctx(),
        )
        .await;
    assert!(!frame_ok(&frame));
    assert!(frame_error(&frame).contains("department"), "{frame:?}");
    assert!(!home.path().join("config.toml").exists());
}

/// A pair naming a non-existent agent rejects the whole save and says
/// which id is unknown — a typo'd whitelist grants nothing silently.
#[tokio::test]
async fn set_rejects_unknown_agent_and_names_it() {
    let home = tempfile::tempdir().unwrap();
    seed_agent(home.path(), "sales-lead", "sales");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let frame = handler
        .handle(
            "delegation.set",
            json!({ "allow": [["sales-lead", "ghost-lead"]] }),
            &admin_ctx(),
        )
        .await;
    assert!(!frame_ok(&frame));
    let err = frame_error(&frame);
    assert!(
        err.contains("ghost-lead"),
        "must name the missing agent: {err}"
    );
    assert!(
        !home.path().join("config.toml").exists(),
        "rejected payload must not be partially written"
    );
}

/// Malformed rows are structural errors (reject); self-pairs and duplicate
/// pairs are meaningless rather than wrong, so they are cleaned silently.
#[tokio::test]
async fn set_rejects_bad_shapes_but_cleans_noise() {
    let home = tempfile::tempdir().unwrap();
    seed_agent(home.path(), "a", "x");
    seed_agent(home.path(), "b", "y");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = admin_ctx();

    for bad in [
        json!({ "allow": [["a"]] }),
        json!({ "allow": [["a", "b", "c"]] }),
        json!({ "allow": [["a", ""]] }),
        json!({ "allow": "a,b" }),
    ] {
        let frame = handler.handle("delegation.set", bad.clone(), &ctx).await;
        assert!(!frame_ok(&frame), "{bad} must be rejected: {frame:?}");
    }

    // Self-pair dropped, ["b","a"] deduped against ["a","b"].
    let frame = handler
        .handle(
            "delegation.set",
            json!({ "allow": [["a", "a"], ["a", "b"], ["b", "a"]] }),
            &ctx,
        )
        .await;
    assert!(frame_ok(&frame), "{frame:?}");
    assert_eq!(frame_data(&frame)["allow"], json!([["a", "b"]]));
}

/// A payload with neither field is a no-op error, not an accidental reset.
#[tokio::test]
async fn set_requires_at_least_one_field() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle("delegation.set", json!({}), &admin_ctx())
        .await;
    assert!(!frame_ok(&frame));
    assert!(!home.path().join("config.toml").exists());
}

/// Writing `[delegation]` leaves the rest of config.toml intact.
#[tokio::test]
async fn set_preserves_unrelated_config() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        "[general]\ndefault_agent = \"kiki\"\n",
    )
    .unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle("delegation.set", json!({ "policy": "open" }), &admin_ctx())
        .await;
    assert!(frame_ok(&frame), "{frame:?}");
    let raw = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert!(raw.contains("kiki"), "unrelated config lost: {raw}");
    assert!(raw.contains("open"), "{raw}");
}

/// WP22 T4 — two directories sharing the same `[agent] name` is a
/// pre-existing-install case this WP cannot retroactively rename its way
/// out of (registry scan only warns, never blocks load). `delegation.set`
/// must treat that shared name as ambiguous and reject a pair naming it,
/// rather than silently resolving to whichever directory `map.insert`
/// happened to see last.
#[tokio::test]
async fn set_rejects_ambiguous_agent_name_and_names_it() {
    let home = tempfile::tempdir().unwrap();
    // Two directories, same `[agent] name` — the last-wins collision.
    seed_agent_in_dir(home.path(), "sales-old", "sales-lead", "sales");
    seed_agent_in_dir(home.path(), "sales-new", "sales-lead", "sales");
    seed_agent(home.path(), "warehouse-lead", "warehouse");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let frame = handler
        .handle(
            "delegation.set",
            json!({ "allow": [["sales-lead", "warehouse-lead"]] }),
            &admin_ctx(),
        )
        .await;
    assert!(
        !frame_ok(&frame),
        "ambiguous name must be rejected: {frame:?}"
    );
    let err = frame_error(&frame);
    assert!(
        err.contains("sales-lead"),
        "must name the ambiguous id: {err}"
    );
    assert!(
        err.contains("多個"),
        "must explain the ambiguity, not just 'not found': {err}"
    );
    assert!(
        !home.path().join("config.toml").exists(),
        "rejected payload must not be partially written"
    );
}

/// The escape hatch: typing the directory name directly — never
/// ambiguous, since directory names are unique by construction (the
/// filesystem enforces it) — still resolves and saves normally even
/// though the *other* agent-name namespace is ambiguous elsewhere.
#[tokio::test]
async fn set_accepts_directory_name_despite_unrelated_ambiguous_name() {
    let home = tempfile::tempdir().unwrap();
    seed_agent_in_dir(home.path(), "sales-old", "sales-lead", "sales");
    seed_agent_in_dir(home.path(), "sales-new", "sales-lead", "sales");
    seed_agent(home.path(), "warehouse-lead", "warehouse");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let frame = handler
        .handle(
            "delegation.set",
            json!({ "allow": [["sales-new", "warehouse-lead"]] }),
            &admin_ctx(),
        )
        .await;
    assert!(
        frame_ok(&frame),
        "directory-name input must resolve: {frame:?}"
    );
    assert_eq!(
        frame_data(&frame)["allow"],
        json!([["sales-new", "warehouse-lead"]])
    );
}
