//! Unit tests for [`super`], moved verbatim out of the former `ephemeral.rs` — gc cases.

use super::*;

#[test]
fn an_ordinary_ephemeral_gets_no_team_intrinsic() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_parent(home, "boss", "[capabilities]\nallowed_tools = [\"Read\"]\n");
    // The MCP `spawn_ephemeral` path: no intrinsics, so the exemption
    // cannot be reached by an agent synthesizing its own sub-agent.
    let err = scaffold(
        home,
        &EphemeralSpawnSpec {
            parent: "boss".into(),
            instruction: "hand something off".into(),
            tools: strs(&["team_handoff"]),
            tier: "standard".into(),
        },
    )
    .unwrap_err();
    assert!(err.contains("privilege escalation"), "got: {err}");
    assert_no_scaffold(home);
}

#[test]
fn role_member_id_carries_role_and_round_and_stays_a_valid_agent_id() {
    let id = new_role_member_id("boss", 3, Role::Planner).unwrap();
    assert!(
        id.starts_with(EPHEMERAL_ID_PREFIX),
        "cost attribution JOINs on `eph-`: {id}"
    );
    assert!(is_ephemeral_id(&id), "{id}");
    assert!(id.contains("-r3-"), "{id}");
    assert!(id.contains("-planner-"), "{id}");

    // A very long parent id still fits inside the 64-char agent-id limit.
    let long = "a".repeat(64);
    assert!(duduclaw_core::is_valid_agent_id(&long));
    let id = new_role_member_id(&long, 9999, Role::Executor).unwrap();
    assert!(is_ephemeral_id(&id), "{id}");
    assert!(id.len() <= 64, "len {} for {id}", id.len());

    // Two members of the same (parent, round, role) never collide.
    let a = new_role_member_id("boss", 1, Role::Executor).unwrap();
    let b = new_role_member_id("boss", 1, Role::Executor).unwrap();
    assert_ne!(a, b);

    // Refusals, not truncations.
    assert!(new_role_member_id("boss", ROLE_MEMBER_ROUND_MAX + 1, Role::Executor).is_err());
    assert!(new_role_member_id("../evil", 1, Role::Executor).is_err());
    assert!(new_role_member_id("", 1, Role::Executor).is_err());
}

#[test]
fn role_member_raw_model_wins_over_tier() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_parent(home, "boss", "");

    // A CLAUDE-family role member is the case the old code got wrong: the
    // non-Claude early return did not cover it, so `tier_model` replaced
    // the role's explicit model with the employee's tier lineup.
    let spec = role_spec("boss", Role::Executor, "claude", "claude-haiku-4.5");
    let member = scaffold_role_member(home, &spec).unwrap();
    for tier in [ModelTier::Cheap, ModelTier::Standard, ModelTier::Preferred] {
        assert_eq!(
            resolve_tier_model_for_dir(&member.dir, tier, "claude-haiku-4.5"),
            "claude-haiku-4.5",
            "the role's own model must survive tier {tier:?}"
        );
    }

    // Ordinary ephemeral agents keep the unchanged tier behavior.
    let plain = scaffold(
        home,
        &EphemeralSpawnSpec {
            parent: "boss".into(),
            instruction: "summarize".into(),
            tools: strs(&["Read"]),
            tier: "cheap".into(),
        },
    )
    .unwrap();
    assert_eq!(
        resolve_tier_model_for_dir(&plain.dir, ModelTier::Cheap, "parent-preferred-model"),
        "parent-utility-model",
        "tier resolution for a plain ephemeral must be untouched"
    );
}

#[test]
fn finish_role_member_removes_the_scaffold_immediately() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_parent(home, "boss", "");
    let spec = role_spec("boss", Role::Executor, "claude", "claude-opus-5");
    let member = scaffold_role_member(home, &spec).unwrap();
    assert!(member.dir.exists());

    finish_role_member(home, &member.agent_id, RoleMemberOutcome::Accepted).unwrap();
    assert!(
        !member.dir.exists(),
        "a role member must not wait out the 1h grace / 24h TTL"
    );
    // Idempotent: a second terminal event is a no-op, not an error.
    finish_role_member(home, &member.agent_id, RoleMemberOutcome::Cancelled).unwrap();
}

#[test]
fn finish_role_member_refuses_an_ordinary_ephemeral() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_parent(home, "boss", "");
    let plain = scaffold(
        home,
        &EphemeralSpawnSpec {
            parent: "boss".into(),
            instruction: "summarize".into(),
            tools: strs(&["Read"]),
            tier: "standard".into(),
        },
    )
    .unwrap();

    let err =
        finish_role_member(home, &plain.agent_id, RoleMemberOutcome::Accepted).unwrap_err();
    assert!(err.contains("not a team role member"), "got: {err}");
    assert!(
        plain.dir.exists(),
        "an ordinary ephemeral keeps its grace window"
    );
}

#[test]
fn finish_role_member_refuses_anything_outside_the_ephemeral_namespace() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_parent(home, "boss", "");
    // Materialise the namespace, then plant a symlink pointing out of it.
    let member = scaffold_role_member(
        home,
        &role_spec("boss", Role::Executor, "claude", "claude-opus-5"),
    )
    .unwrap();

    // A victim directory outside `.ephemeral/`, shaped like a role member.
    let victim = tmp.path().join("precious");
    std::fs::create_dir_all(&victim).unwrap();
    std::fs::copy(member.dir.join("agent.toml"), victim.join("agent.toml")).unwrap();
    std::fs::write(victim.join("do-not-delete.txt"), b"important").unwrap();

    // Ids that cannot even name a scaffold.
    assert!(finish_role_member(home, "../../etc", RoleMemberOutcome::Failed).is_err());
    assert!(finish_role_member(home, "boss", RoleMemberOutcome::Failed).is_err());

    #[cfg(unix)]
    {
        let link_id = "eph-boss-r1-executor-aaaaaa";
        let link = ephemeral_root(home).join(link_id);
        std::os::unix::fs::symlink(&victim, &link).unwrap();
        // Resolves outside the root ⇒ treated as "not ours" and left alone.
        assert!(finish_role_member(home, link_id, RoleMemberOutcome::Failed).is_ok());
        assert!(
            victim.join("do-not-delete.txt").exists(),
            "a symlinked scaffold must never get its TARGET deleted"
        );
    }
    assert!(victim.exists());
}

#[tokio::test]
async fn sweep_collects_a_completed_role_member_without_the_grace_window() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_parent(home, "boss", "");

    // A role member whose round died before `finish_role_member` ran: the
    // `.completed` marker exists, but it is only seconds old.
    let member = scaffold_role_member(
        home,
        &role_spec("boss", Role::Planner, "claude", "claude-opus-5"),
    )
    .unwrap();
    std::fs::write(
        member.dir.join(".completed"),
        chrono::Utc::now().to_rfc3339(),
    )
    .unwrap();

    // An ordinary ephemeral in exactly the same state must survive.
    let plain = scaffold(
        home,
        &EphemeralSpawnSpec {
            parent: "boss".into(),
            instruction: "summarize".into(),
            tools: strs(&["Read"]),
            tier: "standard".into(),
        },
    )
    .unwrap();
    std::fs::write(
        plain.dir.join(".completed"),
        chrono::Utc::now().to_rfc3339(),
    )
    .unwrap();

    assert_eq!(sweep(home).await, 1);
    assert!(
        !member.dir.exists(),
        "role member must be collected at once"
    );
    assert!(
        plain.dir.exists(),
        "ordinary ephemeral GC policy must be untouched"
    );
}

#[test]
fn admit_role_member_queues_instead_of_failing_at_capacity() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_parent(home, "boss", "");
    std::fs::write(
        home.join("config.toml"),
        "[dispatch]\nephemeral_max_active = 1\n",
    )
    .unwrap();

    let spec = role_spec("boss", Role::Executor, "claude", "claude-opus-5");
    let first = admit_role_member(home, &spec, Some("task-abc#2")).unwrap();
    assert!(
        matches!(first, RoleMemberAdmitted::Scaffolded(_)),
        "{first:?}"
    );

    let second = admit_role_member(home, &spec, Some("task-abc#2")).unwrap();
    match second {
        RoleMemberAdmitted::Queued { position, .. } => assert_eq!(position, 1),
        other => panic!("expected a queued member at capacity, got {other:?}"),
    }
    assert_eq!(
        duduclaw_core::spawn_admission::role_member_queue_depth(home),
        1
    );
    // The ephemeral rail's own queue is untouched.
    assert_eq!(
        duduclaw_core::spawn_admission::queue_depth(home, EPHEMERAL_ADMISSION_CLASS),
        0
    );

    // The round ending purges its queued members.
    let removed = duduclaw_core::spawn_admission::invalidate_role_members(home, "task-abc#2");
    assert_eq!(removed.len(), 1);
}

#[test]
fn admit_role_member_never_queues_a_validation_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_parent(home, "boss", "");
    std::fs::write(
        home.join("config.toml"),
        "[dispatch]\nephemeral_max_active = 1\n",
    )
    .unwrap();
    // Fill the one slot, then submit a member that can never be valid.
    admit_role_member(
        home,
        &role_spec("boss", Role::Executor, "claude", "claude-opus-5"),
        None,
    )
    .unwrap();

    let bad = role_spec("boss", Role::Executor, "claude", "gpt-5.4");
    assert!(admit_role_member(home, &bad, None).is_err());
    assert_eq!(
        duduclaw_core::spawn_admission::role_member_queue_depth(home),
        0,
        "retrying an invalid assignment can never succeed — it must not be queued"
    );
}

#[test]
fn role_member_payload_round_trips() {
    let mut spec = role_spec("boss", Role::Utility, "codex", "gpt-5.4");
    spec.effort = Some(Effort::XHigh);
    let restored = role_member_from_payload(&role_member_payload(&spec)).unwrap();
    assert_eq!(restored.parent_agent, spec.parent_agent);
    assert_eq!(restored.task_id, spec.task_id);
    assert_eq!(restored.round, spec.round);
    assert_eq!(restored.role, spec.role);
    assert_eq!(restored.runtime, spec.runtime);
    assert_eq!(restored.model, spec.model);
    assert_eq!(restored.effort, Some(Effort::XHigh));
    assert_eq!(restored.instruction, spec.instruction);
    assert_eq!(restored.tools, spec.tools);

    // An unset effort round-trips as unset, not as a default.
    spec.effort = None;
    assert_eq!(
        role_member_from_payload(&role_member_payload(&spec))
            .unwrap()
            .effort,
        None
    );
    // A malformed ticket is dropped, never half-reconstructed.
    assert!(role_member_from_payload(&serde_json::json!({"role": "executor"})).is_none());
    assert!(role_member_from_payload(&serde_json::json!(null)).is_none());
}

#[test]
fn role_member_scaffold_requires_a_task_id() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_parent(home, "boss", "");
    let mut spec = role_spec("boss", Role::Executor, "claude", "claude-opus-5");
    spec.task_id = "   ".into();
    assert!(scaffold_role_member(home, &spec).is_err());
    assert_no_scaffold(home);
}
