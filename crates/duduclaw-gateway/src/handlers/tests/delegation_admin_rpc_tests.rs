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
        credential_epoch: 0,
        user_id: "m1".to_string(),
        email: "m1@test.local".to_string(),
        role: UserRole::Manager,
        agent_access: std::collections::HashMap::new(),
        must_change_password: false,
        team_access: Default::default(),
        acl_generation: 0,
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

    // RM-137: an unknown policy is enforced (and shown) as `none`; the raw
    // value is reported alongside.
    assert_eq!(data["policy"], json!("none"));
    assert_eq!(data["configured_policy"], json!("anarchy"));
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
        None,
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

// ── RM-137 ──────────────────────────────────────────────────────────────────

/// `none` is a valid value: persisted and read back, pairs kept.
#[tokio::test]
async fn set_accepts_none_and_keeps_pairs() {
    let home = tempfile::tempdir().unwrap();
    seed_agent(home.path(), "sales-lead", "sales");
    seed_agent(home.path(), "warehouse-lead", "warehouse");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle(
            "delegation.set",
            json!({ "policy": "none", "allow": [["sales-lead", "warehouse-lead"]] }),
            &admin_ctx(),
        )
        .await;
    assert!(frame_ok(&frame), "{frame:?}");
    let data = frame_data(&handler.handle("delegation.get", json!({}), &admin_ctx()).await);
    assert_eq!(data["policy"], json!("none"));
    assert_eq!(data["allow"], json!([["sales-lead", "warehouse-lead"]]));
    assert_eq!(data["warnings"], json!([]));
}

/// An unparsable config.toml: `delegation.set` refuses and writes nothing
/// (the file is byte-identical afterwards, no audit row); `delegation.get`
/// is an error rather than defaults the dashboard could save back.
#[tokio::test]
async fn broken_config_is_never_overwritten_and_get_reports_it() {
    let home = tempfile::tempdir().unwrap();
    seed_agent(home.path(), "sales-lead", "sales");
    let broken = "[general\ndefault_agent = \"kiki\"\n[delegation]\npolicy = \"hierarchy\"\n";
    std::fs::write(home.path().join("config.toml"), broken).unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    for params in [
        json!({ "policy": "department", "allow": [] }),
        json!({ "policy": "none" }),
        json!({ "allow": [] }),
    ] {
        let frame = handler.handle("delegation.set", params.clone(), &admin_ctx()).await;
        assert!(!frame_ok(&frame), "{params} must be refused: {frame:?}");
        assert!(frame_error(&frame).contains("TOML"), "{frame:?}");
        assert_eq!(
            std::fs::read_to_string(home.path().join("config.toml")).unwrap(),
            broken,
            "a refused save must not touch the file"
        );
    }
    let audit = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap_or_default();
    assert!(!audit.contains("delegation_config_changed"), "{audit}");

    let frame = handler.handle("delegation.get", json!({}), &admin_ctx()).await;
    assert!(!frame_ok(&frame), "get must not return defaults: {frame:?}");
}

fn employee_ctx() -> UserContext {
    UserContext {
        role: UserRole::Employee,
        user_id: "e1".to_string(),
        email: "e1@test.local".to_string(),
        ..manager_ctx()
    }
}

/// `delegation.effective` — global policy, source, pairs naming the
/// employee, and the inactive pairs under `none`.
#[tokio::test]
async fn effective_reports_global_policy_and_pairs() {
    let home = tempfile::tempdir().unwrap();
    seed_agent(home.path(), "sales-lead", "sales");
    seed_agent(home.path(), "warehouse-lead", "warehouse");
    seed_agent(home.path(), "intern", "sales");
    std::fs::write(
        home.path().join("config.toml"),
        "[delegation]\npolicy = \"hierarchy\"\nallow = [[\"sales-lead\", \"warehouse-lead\"]]\n",
    )
    .unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let data = frame_data(
        &handler
            .handle("delegation.effective", json!({ "agent_id": "sales-lead" }), &admin_ctx())
            .await,
    );
    assert_eq!(data["policy"], json!("hierarchy"));
    assert_eq!(data["source"], json!("global"));
    assert_eq!(data["global_policy"], json!("hierarchy"));
    assert_eq!(data["may_delegate"], json!(true));
    // seed_agent writes can_create_agents = false.
    assert_eq!(data["may_create_agents"], json!(false));
    assert_eq!(data["allow_pairs"], json!([["sales-lead", "warehouse-lead"]]));
    assert_eq!(data["inactive_allow_pairs"], json!([]));
    assert_eq!(data["config_error"], Value::Null);

    // A pair not naming the employee is not listed for it.
    let data = frame_data(
        &handler
            .handle("delegation.effective", json!({ "agent_id": "intern" }), &admin_ctx())
            .await,
    );
    assert_eq!(data["allow_pairs"], json!([]));

    // Under `none` the same pair is reported as inactive.
    std::fs::write(
        home.path().join("config.toml"),
        "[delegation]\npolicy = \"none\"\nallow = [[\"sales-lead\", \"warehouse-lead\"]]\n",
    )
    .unwrap();
    let data = frame_data(
        &handler
            .handle("delegation.effective", json!({ "agent_id": "sales-lead" }), &admin_ctx())
            .await,
    );
    assert_eq!(data["policy"], json!("none"));
    assert_eq!(data["source"], json!("global"));
    assert_eq!(data["may_delegate"], json!(false));
    assert_eq!(data["allow_pairs"], json!([]));
    assert_eq!(data["inactive_allow_pairs"], json!([["sales-lead", "warehouse-lead"]]));
}

/// RM-137 L1: the other end of an allow pair is named only when the viewer
/// may view it; otherwise it is masked and only counted. Applies to the
/// inactive list under `none` too.
#[test]
fn effective_masks_pair_peers_the_viewer_cannot_view() {
    use crate::handlers::delegation_rpc::{HIDDEN_PEER_PLACEHOLDER, effective_delegation_json};
    let home = tempfile::tempdir().unwrap();
    seed_agent(home.path(), "sales-lead", "sales");
    seed_agent(home.path(), "warehouse-lead", "warehouse");
    seed_agent(home.path(), "private-bot", "ops");
    std::fs::write(
        home.path().join("config.toml"),
        "[delegation]\npolicy = \"hierarchy\"\n\
         allow = [[\"sales-lead\", \"warehouse-lead\"], [\"private-bot\", \"sales-lead\"]]\n",
    )
    .unwrap();
    let dir = home.path().join("agents/sales-lead");
    let only_warehouse = |peer: &str| peer == "warehouse-lead";
    let data = effective_delegation_json(home.path(), &dir, "sales-lead", &only_warehouse, false);
    assert_eq!(
        data["allow_pairs"],
        json!([["sales-lead", "warehouse-lead"], [HIDDEN_PEER_PLACEHOLDER, "sales-lead"]])
    );
    assert_eq!(data["hidden_allow_pairs"], json!(1));
    assert_eq!(data["hidden_inactive_allow_pairs"], json!(0));
    assert!(!data.to_string().contains("private-bot"), "{data}");

    // Nothing visible: both peers masked, the employee itself still named.
    let data = effective_delegation_json(home.path(), &dir, "sales-lead", &|_| false, false);
    assert_eq!(data["hidden_allow_pairs"], json!(2));
    assert!(!data.to_string().contains("warehouse-lead"), "{data}");

    // Under `none` the masked pairs move to the inactive list with their count.
    std::fs::write(
        home.path().join("config.toml"),
        "[delegation]\npolicy = \"none\"\nallow = [[\"private-bot\", \"sales-lead\"]]\n",
    )
    .unwrap();
    let data = effective_delegation_json(home.path(), &dir, "sales-lead", &|_| false, false);
    assert_eq!(data["inactive_allow_pairs"], json!([[HIDDEN_PEER_PLACEHOLDER, "sales-lead"]]));
    assert_eq!(data["hidden_inactive_allow_pairs"], json!(1));
    assert_eq!(data["hidden_allow_pairs"], json!(0));
    assert!(!duduclaw_core::is_valid_agent_id(HIDDEN_PEER_PLACEHOLDER));
}

/// RM-137 re-review L: a malformed / self `[delegation] allow` entry names
/// employees verbatim in its warning. A Manager viewer of `sales-lead` —
/// with view access to it only — receives just `allow_warning_count`, never
/// the text (here naming `private-bot`, unrelated to `sales-lead` and not
/// viewable); an Admin viewer receives the texts. `delegation.get`
/// (Admin-only) keeps the full warnings.
#[tokio::test]
async fn effective_returns_allow_warning_text_only_to_an_admin() {
    use crate::handlers::delegation_rpc::effective_delegation_json;
    let home = tempfile::tempdir().unwrap();
    seed_agent(home.path(), "sales-lead", "sales");
    seed_agent(home.path(), "private-bot", "ops");
    std::fs::write(
        home.path().join("config.toml"),
        "[delegation]\npolicy = \"hierarchy\"\n\
         allow = [[\"private-bot\", \"private-bot\"], [\"private-bot\"]]\n",
    )
    .unwrap();
    let dir = home.path().join("agents/sales-lead");
    let mut manager = manager_ctx();
    manager
        .agent_access
        .insert("sales-lead".to_string(), AccessLevel::Viewer);
    assert!(!manager.has_role(UserRole::Admin));
    let data = effective_delegation_json(
        home.path(),
        &dir,
        "sales-lead",
        &|peer: &str| manager.has_agent_access(peer, AccessLevel::Viewer),
        manager.has_role(UserRole::Admin),
    );
    assert_eq!(data["allow_warning_count"], json!(2), "{data}");
    assert!(data.get("allow_warnings").is_none(), "{data}");
    assert!(data.get("warnings").is_none(), "{data}");
    assert!(!data.to_string().contains("private-bot"), "{data}");

    let admin = admin_ctx();
    assert!(admin.has_role(UserRole::Admin));
    let data = effective_delegation_json(
        home.path(),
        &dir,
        "sales-lead",
        &|peer: &str| admin.has_agent_access(peer, AccessLevel::Viewer),
        admin.has_role(UserRole::Admin),
    );
    assert_eq!(data["allow_warning_count"], json!(2), "{data}");
    let texts = data["allow_warnings"].as_array().unwrap();
    assert_eq!(texts.len(), 2, "{data}");
    assert!(texts.iter().any(|w| w.as_str().unwrap_or("").contains("private-bot")), "{data}");

    // Through the RPC as Admin (the live-context path), and `delegation.get`.
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let data = frame_data(
        &handler
            .handle("delegation.effective", json!({ "agent_id": "sales-lead" }), &admin_ctx())
            .await,
    );
    assert_eq!(data["allow_warning_count"], json!(2), "{data}");
    assert_eq!(data["allow_warnings"].as_array().map(Vec::len), Some(2), "{data}");
    let data = frame_data(&handler.handle("delegation.get", json!({}), &admin_ctx()).await);
    assert_eq!(data["warnings"].as_array().map(Vec::len), Some(2), "{data}");
}

/// The employee's own `can_send_cross_agent = false` makes it `none` for
/// that employee, with the source saying so.
#[tokio::test]
async fn effective_reports_the_employee_switch_as_source() {
    let home = tempfile::tempdir().unwrap();
    seed_agent(home.path(), "sales-lead", "sales");
    // After construction: the v1.68 boot migration resets unmarked `false`
    // flags, and the RPC re-reads agent.toml on every call anyway.
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let path = home.path().join("agents/sales-lead/agent.toml");
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(raw.contains("can_send_cross_agent = true"), "{raw}");
    std::fs::write(&path, raw.replace("can_send_cross_agent = true", "can_send_cross_agent = false")).unwrap();
    let data = frame_data(
        &handler
            .handle("delegation.effective", json!({ "agent_id": "sales-lead" }), &admin_ctx())
            .await,
    );
    assert_eq!(data["policy"], json!("none"));
    assert_eq!(data["source"], json!("agent_permissions"));
    assert_eq!(data["global_policy"], json!("department"));
    assert_eq!(data["permissions"]["can_send_cross_agent"], json!(false));
}

/// Unknown value and unparsable config: enforced as `none`, surfaced as
/// `policy_warning` / `config_error`.
#[tokio::test]
async fn effective_surfaces_unknown_value_and_config_error() {
    let home = tempfile::tempdir().unwrap();
    seed_agent(home.path(), "sales-lead", "sales");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    std::fs::write(home.path().join("config.toml"), "[delegation]\npolicy = \"closed\"\n").unwrap();
    let data = frame_data(
        &handler
            .handle("delegation.effective", json!({ "agent_id": "sales-lead" }), &admin_ctx())
            .await,
    );
    assert_eq!(data["policy"], json!("none"));
    assert_eq!(data["configured_policy"], json!("closed"));
    assert!(data["policy_warning"].as_str().is_some_and(|w| w.contains("closed")), "{data}");
    assert_eq!(data["config_error"], Value::Null);

    std::fs::write(home.path().join("config.toml"), "[delegation\npolicy = \"open\"\n").unwrap();
    let data = frame_data(
        &handler
            .handle("delegation.effective", json!({ "agent_id": "sales-lead" }), &admin_ctx())
            .await,
    );
    assert_eq!(data["policy"], json!("none"));
    assert_eq!(data["global_policy"], json!("none"));
    assert_eq!(data["config_error"], json!("unparsable"), "{data}");
    // Admin viewer: the zh-TW detail as well.
    assert!(data["config_error_detail"].as_str().is_some_and(|e| e.contains("TOML")), "{data}");
}

/// RM-137 re-review: a config problem reaches a non-Admin viewer only as a
/// category (`config_error`), never as text — the TOML parse excerpt and the
/// `[permissions]` reader's error can quote raw config. An Admin viewer also
/// gets `config_error_detail`.
#[test]
fn effective_returns_config_error_text_only_to_an_admin() {
    use crate::handlers::delegation_rpc::effective_delegation_json;
    const MARKER: &str = "SECRET-CONFIG-MARKER";
    let home = tempfile::tempdir().unwrap();
    seed_agent(home.path(), "sales-lead", "sales");
    let dir = home.path().join("agents/sales-lead");

    // Unparsable config.toml whose offending line carries the marker.
    std::fs::write(
        home.path().join("config.toml"),
        format!("[delegation-{MARKER}\npolicy = \"open\"\n"),
    )
    .unwrap();
    let data = effective_delegation_json(home.path(), &dir, "sales-lead", &|_| true, false);
    assert_eq!(data["policy"], json!("none"), "{data}");
    assert_eq!(data["config_error"], json!("unparsable"), "{data}");
    assert!(data.get("config_error_detail").is_none(), "{data}");
    assert!(!data.to_string().contains(MARKER), "{data}");
    assert!(!data.to_string().contains("TOML"), "{data}");
    let data = effective_delegation_json(home.path(), &dir, "sales-lead", &|_| true, true);
    assert_eq!(data["config_error"], json!("unparsable"), "{data}");
    assert!(data["config_error_detail"].as_str().is_some_and(|e| !e.is_empty()), "{data}");

    // Unreadable config.toml (a directory where the file should be).
    std::fs::remove_file(home.path().join("config.toml")).unwrap();
    std::fs::create_dir(home.path().join("config.toml")).unwrap();
    let data = effective_delegation_json(home.path(), &dir, "sales-lead", &|_| true, false);
    assert_eq!(data["config_error"], json!("unreadable"), "{data}");
    assert!(data.get("config_error_detail").is_none(), "{data}");
    std::fs::remove_dir(home.path().join("config.toml")).unwrap();

    // Readable config, unreadable `[permissions]` in the employee's agent.toml.
    std::fs::write(dir.join("agent.toml"), format!("[permissions\n{MARKER} = 1\n")).unwrap();
    let data = effective_delegation_json(home.path(), &dir, "sales-lead", &|_| true, false);
    assert_eq!(data["config_error"], json!("permissions_unreadable"), "{data}");
    assert!(data.get("config_error_detail").is_none(), "{data}");
    assert!(!data.to_string().contains(MARKER), "{data}");
    let data = effective_delegation_json(home.path(), &dir, "sales-lead", &|_| true, true);
    assert_eq!(data["config_error"], json!("permissions_unreadable"), "{data}");
    assert!(data["config_error_detail"].as_str().is_some_and(|e| !e.is_empty()), "{data}");

    // No problem: null for everyone, and the Admin detail is null too.
    seed_agent(home.path(), "sales-lead", "sales");
    let data = effective_delegation_json(home.path(), &dir, "sales-lead", &|_| true, true);
    assert_eq!(data["config_error"], Value::Null, "{data}");
    assert_eq!(data["config_error_detail"], Value::Null, "{data}");
}

/// Read-only and gated: an Employee-role account and a Manager with no
/// access to the employee are refused; nothing is written.
#[tokio::test]
async fn effective_is_gated_and_read_only() {
    let home = tempfile::tempdir().unwrap();
    seed_agent(home.path(), "sales-lead", "sales");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    for ctx in [employee_ctx(), manager_ctx()] {
        let frame = handler
            .handle("delegation.effective", json!({ "agent_id": "sales-lead" }), &ctx)
            .await;
        assert!(!frame_ok(&frame), "{:?} must be refused: {frame:?}", ctx.role);
    }
    let frame = handler
        .handle("delegation.effective", json!({ "agent_id": "ghost" }), &admin_ctx())
        .await;
    assert!(!frame_ok(&frame), "{frame:?}");
    assert!(!home.path().join("config.toml").exists(), "a read must not create config");
}

/// `agents.inspect.effective_permissions` reports what the MCP gate applies:
/// the stored flags, and every flag off when agent.toml is unreadable.
#[test]
fn effective_permissions_use_the_gate_reader() {
    let home = tempfile::tempdir().unwrap();
    seed_agent(home.path(), "sales-lead", "sales");
    let dir = home.path().join("agents/sales-lead");
    let p = duduclaw_core::agent_toml::load_permissions_checked(&dir).unwrap();
    assert!(!p.allows("can_create_agents"));
    assert!(p.allows("can_send_cross_agent"));
    assert!(!p.allows("can_schedule_tasks"));
    // Unwritten can_create_agents on a non-main employee resolves to false.
    std::fs::write(dir.join("agent.toml"), "[agent]\nname = \"sales-lead\"\nrole = \"specialist\"\n").unwrap();
    let p = duduclaw_core::agent_toml::load_permissions_checked(&dir).unwrap();
    assert!(!p.allows("can_create_agents"));
    assert!(p.allows("can_send_cross_agent"));
    std::fs::write(dir.join("agent.toml"), "[agent]\nname = \"sales-lead\"\nrole = \"main\"\n").unwrap();
    assert!(duduclaw_core::agent_toml::load_permissions_checked(&dir).unwrap().allows("can_create_agents"));
    std::fs::write(dir.join("agent.toml"), "[permissions]\ncan_send_cross_agent = \"yes\"\n").unwrap();
    assert!(duduclaw_core::agent_toml::load_permissions_checked(&dir).is_err());
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
