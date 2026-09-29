//! Unit tests for [`super`], moved verbatim out of the former `ephemeral.rs` — role member cases.

use super::*;

/// A smaller-than-default configured cap is honored end-to-end (not just
/// the pure clamp function).
#[test]
fn scaffold_cap_honors_a_small_configured_value() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_parent(home, "boss", "");
    std::fs::write(
        home.join("config.toml"),
        "[dispatch]\nephemeral_max_active = 2\n",
    )
    .unwrap();
    let spec = |n: usize| EphemeralSpawnSpec {
        parent: "boss".into(),
        instruction: format!("worker {n}"),
        tools: strs(&["Read"]),
        tier: "standard".into(),
    };
    assert!(scaffold(home, &spec(1)).is_ok());
    assert!(scaffold(home, &spec(2)).is_ok());
    assert!(
        scaffold(home, &spec(3))
            .unwrap_err()
            .starts_with(EPHEMERAL_CAPACITY_ERROR_PREFIX)
    );
}

/// FIFO release: once a slot frees (simulated GC removal, since the real
/// resource is disk-scaffold-count-until-GC), the drain loop admits the
/// OLDEST queued ticket first and stops as soon as the (now again full)
/// cap is hit — the second ticket stays queued for the next tick.
#[tokio::test]
async fn drain_admission_queue_admits_oldest_ticket_first_then_stops_at_cap() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_parent(home, "boss", "");
    std::fs::write(
        home.join("config.toml"),
        "[dispatch]\nephemeral_max_active = 1\n",
    )
    .unwrap();

    // Fill the (cap=1) capacity directly, as a live in-flight scaffold would.
    let filled = scaffold(
        home,
        &EphemeralSpawnSpec {
            parent: "boss".into(),
            instruction: "occupies the only slot".into(),
            tools: strs(&["Read"]),
            tier: "standard".into(),
        },
    )
    .unwrap();

    let admission_cfg = duduclaw_core::spawn_admission::AdmissionConfig::from_home(home);
    let a = duduclaw_core::spawn_admission::enqueue(
        home,
        EPHEMERAL_ADMISSION_CLASS,
        &admission_cfg,
        None,
        queued_ephemeral_payload("boss", "first queued", "ctx-1"),
    )
    .unwrap();
    let b = duduclaw_core::spawn_admission::enqueue(
        home,
        EPHEMERAL_ADMISSION_CLASS,
        &admission_cfg,
        None,
        queued_ephemeral_payload("boss", "second queued", "ctx-2"),
    )
    .unwrap();
    assert!(matches!(
        a,
        duduclaw_core::spawn_admission::EnqueueOutcome::Queued { .. }
    ));
    assert!(matches!(
        b,
        duduclaw_core::spawn_admission::EnqueueOutcome::Queued { .. }
    ));

    // Simulate GC freeing the one slot.
    std::fs::remove_dir_all(&filled.dir).unwrap();

    let summary = drain_admission_queue(home).await;
    assert_eq!(
        summary.admitted, 1,
        "cap=1 admits exactly one per drain pass"
    );
    assert_eq!(summary.expired, 0);
    assert_eq!(summary.failed, 0);
    assert_eq!(
        duduclaw_core::spawn_admission::queue_depth(home, EPHEMERAL_ADMISSION_CLASS),
        1,
        "the second ticket stays queued — capacity is full again"
    );

    // The admitted ticket must be the FIFO-oldest ("first queued"), and
    // must have actually landed on the bus.
    let content = std::fs::read_to_string(home.join("bus_queue.jsonl")).unwrap();
    assert!(
        content.contains("ctx-1"),
        "oldest ticket's context must be dispatched: {content}"
    );
    assert!(
        !content.contains("ctx-2"),
        "second ticket must NOT be dispatched yet: {content}"
    );

    // A second drain pass (after freeing the slot again) admits the rest.
    let live_dir = std::fs::read_dir(ephemeral_root(home))
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.path().is_dir())
        .unwrap()
        .path();
    std::fs::remove_dir_all(&live_dir).unwrap();
    let summary2 = drain_admission_queue(home).await;
    assert_eq!(summary2.admitted, 1);
    assert_eq!(
        duduclaw_core::spawn_admission::queue_depth(home, EPHEMERAL_ADMISSION_CLASS),
        0
    );
}

/// TTL expiry: a ticket that outlives its TTL is dropped (never admitted)
/// and reported in the summary — even when capacity IS free.
#[tokio::test]
async fn drain_admission_queue_drops_and_reports_expired_tickets() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_parent(home, "boss", "");
    // Plenty of capacity; TTL 0 ⇒ already expired the instant it is queued.
    std::fs::write(
        home.join("config.toml"),
        "[dispatch]\nephemeral_max_active = 32\nqueue_item_ttl_secs = 0\n",
    )
    .unwrap();
    let admission_cfg = duduclaw_core::spawn_admission::AdmissionConfig::from_home(home);
    duduclaw_core::spawn_admission::enqueue(
        home,
        EPHEMERAL_ADMISSION_CLASS,
        &admission_cfg,
        None,
        queued_ephemeral_payload("boss", "will expire", "ctx-expired"),
    )
    .unwrap();

    let summary = drain_admission_queue(home).await;
    assert_eq!(summary.admitted, 0);
    assert_eq!(summary.expired, 1);
    assert_eq!(summary.failed, 0);
    assert_eq!(
        duduclaw_core::spawn_admission::queue_depth(home, EPHEMERAL_ADMISSION_CLASS),
        0
    );
    assert!(
        !home.join("bus_queue.jsonl").exists(),
        "an expired ticket must never be dispatched"
    );
}

#[test]
fn role_member_scaffold_writes_the_three_overrides() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_parent(
        home,
        "boss",
        "[capabilities]\nallowed_tools = [\"Read\", \"Grep\"]\n",
    );

    let mut spec = role_spec("boss", Role::Verifier, "gemini", "gemini-3-pro-preview");
    spec.effort = Some(Effort::High);
    let result = scaffold_role_member(home, &spec).unwrap();

    let t = member_toml(&result.dir);
    assert_eq!(t["runtime"]["provider"].as_str(), Some("gemini"));
    assert_eq!(
        t["model"]["preferred"].as_str(),
        Some("gemini-3-pro-preview")
    );
    assert_eq!(t["model"]["effort"].as_str(), Some("high"));
    // WP-3's reader sees exactly the key it looks for.
    assert_eq!(
        duduclaw_core::effort::read_agent_effort(&result.dir),
        Some(Effort::High)
    );
    // `[team_member]` carries the assignment; `[agent] role` stays `worker`.
    assert_eq!(t[ROLE_MEMBER_SECTION]["role"].as_str(), Some("verifier"));
    assert_eq!(t[ROLE_MEMBER_SECTION]["task_id"].as_str(), Some("task-abc"));
    assert_eq!(t[ROLE_MEMBER_SECTION]["round"].as_integer(), Some(2));
    assert_eq!(t[ROLE_MEMBER_SECTION]["parent"].as_str(), Some("boss"));
    assert_eq!(t["agent"]["role"].as_str(), Some("worker"));
    assert_eq!(
        read_role_member(&result.dir),
        Some(RoleMemberRecord {
            role: Role::Verifier,
            task_id: "task-abc".into(),
            round: 2,
            parent: "boss".into(),
        })
    );
    // Everything else the employee set is still inherited.
    assert_eq!(t["model"]["utility"].as_str(), Some("parent-utility-model"));
    assert_eq!(t["agent"]["reports_to"].as_str(), Some("boss"));
    // …and it still parses as a complete AgentConfig.
    let cfg: duduclaw_core::types::AgentConfig =
        toml::from_str(&std::fs::read_to_string(result.dir.join("agent.toml")).unwrap())
            .unwrap();
    assert_eq!(cfg.model.preferred, "gemini-3-pro-preview");
    assert_eq!(cfg.capabilities.allowed_tools, vec!["Read".to_string()]);
}

#[test]
fn role_member_scaffold_omits_the_effort_key_when_unset() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_parent(home, "boss", "");
    let spec = role_spec("boss", Role::Executor, "claude", "claude-opus-5");
    assert!(spec.effort.is_none());
    let result = scaffold_role_member(home, &spec).unwrap();

    let t = member_toml(&result.dir);
    assert!(
        t["model"].get("effort").is_none(),
        "an unset effort must write NO key (provider default), not a default value"
    );
    assert_eq!(duduclaw_core::effort::read_agent_effort(&result.dir), None);
}

#[test]
fn role_member_scaffold_drops_an_inherited_runtime_fallback() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_parent(
        home,
        "boss",
        "[runtime]\nprovider = \"claude\"\nfallback = \"codex\"\n",
    );
    let spec = role_spec("boss", Role::Verifier, "gemini", "gemini-3-pro-preview");
    let result = scaffold_role_member(home, &spec).unwrap();
    let t = member_toml(&result.dir);
    assert_eq!(t["runtime"]["provider"].as_str(), Some("gemini"));
    assert!(
        t["runtime"].get("fallback").is_none(),
        "a fallback would silently move the verifier back onto another family"
    );
}

#[test]
fn role_member_scaffold_rejects_model_outside_the_runtime_family() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_parent(home, "boss", "");
    // gpt-* is codex's family, not claude's — never guessed into place.
    let spec = role_spec("boss", Role::Executor, "claude", "gpt-5.4");
    let err = scaffold_role_member(home, &spec).unwrap_err();
    assert!(err.contains("does not belong to runtime"), "got: {err}");
    assert_no_scaffold(home);

    // A family the catalog knows nothing about is an error too.
    let spec = role_spec("boss", Role::Executor, "claude", "llama-4-400b");
    assert!(scaffold_role_member(home, &spec).is_err());
    assert_no_scaffold(home);
}

#[test]
fn role_member_scaffold_rejects_runtime_outside_the_allowlist() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_parent(home, "boss", "");
    // `qwen` is a real catalog runtime but NOT in TEAM_ROLE_RUNTIME_ALLOWLIST.
    let spec = role_spec("boss", Role::Executor, "qwen", "qwen3-max");
    let err = scaffold_role_member(home, &spec).unwrap_err();
    assert!(err.contains("cannot back a team role"), "got: {err}");
    assert_no_scaffold(home);

    // And a name that is no runtime at all.
    let spec = role_spec(
        "boss",
        Role::Executor,
        "definitely-not-a-runtime",
        "claude-opus-5",
    );
    assert!(scaffold_role_member(home, &spec).is_err());
    assert_no_scaffold(home);
}

#[test]
fn role_member_scaffold_canonicalises_a_runtime_alias() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_parent(home, "boss", "");
    // `agy` is antigravity's alias, and antigravity serves gemini-* models.
    let spec = role_spec("boss", Role::Planner, "agy", "gemini-3-flash");
    let result = scaffold_role_member(home, &spec).unwrap();
    assert_eq!(
        member_toml(&result.dir)["runtime"]["provider"].as_str(),
        Some("antigravity"),
        "the alias must be resolved before it is written"
    );
}

#[test]
fn role_member_scaffold_still_enforces_tool_containment() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_parent(home, "boss", "[capabilities]\nallowed_tools = [\"Read\"]\n");
    let mut spec = role_spec("boss", Role::Executor, "claude", "claude-opus-5");
    spec.tools = strs(&["Read", "Bash"]);
    let err = scaffold_role_member(home, &spec).unwrap_err();
    assert!(err.contains("privilege escalation"), "got: {err}");
    assert_no_scaffold(home);
}

#[test]
fn role_member_scaffold_admits_the_team_intrinsic_tool() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    // An employee allowlist written before teams existed cannot name
    // `team_handoff`; the member still gets it.
    write_parent(home, "boss", "[capabilities]\nallowed_tools = [\"Read\"]\n");
    let mut spec = role_spec("boss", Role::Executor, "claude", "claude-opus-5");
    spec.tools = strs(&["team_handoff", "Read"]);
    let result = scaffold_role_member(home, &spec).unwrap();
    let cfg: duduclaw_core::types::AgentConfig =
        toml::from_str(&std::fs::read_to_string(result.dir.join("agent.toml")).unwrap())
            .unwrap();
    assert_eq!(
        cfg.capabilities.allowed_tools,
        strs(&["team_handoff", "Read"])
    );

    // Anything else outside the employee's allowlist is still refused.
    let mut wide = role_spec("boss", Role::Executor, "claude", "claude-opus-5");
    wide.tools = strs(&["team_handoff", "Bash"]);
    let err = scaffold_role_member(home, &wide).unwrap_err();
    assert!(err.contains("privilege escalation"), "got: {err}");
}
