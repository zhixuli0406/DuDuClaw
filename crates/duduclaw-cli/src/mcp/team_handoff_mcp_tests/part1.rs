use super::*;

#[tokio::test(flavor = "current_thread")]
async fn files_the_packet_at_the_documented_path() {
    let tmp = TempDir::new();
    mk_member(tmp.path(), "planner-1", "planner", Some(TASK), Some(2));
    let p = packet(Role::Planner, Role::Executor, 2);
    let out = call(tmp.path(), "planner-1", &p).await;
    assert!(!is_error(&out), "{out}");
    let body: Value = serde_json::from_str(&text_of(&out)).unwrap();
    assert_eq!(body["ok"], true);
    assert_eq!(
        body["path"].as_str().unwrap(),
        std::path::Path::new("team_packets")
            .join(TASK)
            .join("r2")
            .join("planner-to-executor.json")
            .to_string_lossy()
    );
    assert!(body["bytes"].as_u64().unwrap() > 0);

    let on_disk = duduclaw_core::task_packet::packet_path(
        tmp.path(),
        TASK,
        2,
        Role::Planner,
        Role::Executor,
    )
    .unwrap();
    let reread: TaskPacket = serde_json::from_slice(&fs::read(&on_disk).unwrap()).unwrap();
    assert_eq!(reread, p, "what lands on disk is the packet verbatim");
    // No temp file left behind.
    assert!(!on_disk.with_extension("json.tmp").exists());
}

#[tokio::test(flavor = "current_thread")]
async fn refiling_the_same_packet_id_is_idempotent() {
    let tmp = TempDir::new();
    mk_member(tmp.path(), "planner-1", "planner", Some(TASK), Some(1));
    let p = packet(Role::Planner, Role::Executor, 1);
    let first = call(tmp.path(), "planner-1", &p).await;
    let second = call(tmp.path(), "planner-1", &p).await;
    assert!(!is_error(&second), "{second}");
    let a: Value = serde_json::from_str(&text_of(&first)).unwrap();
    let b: Value = serde_json::from_str(&text_of(&second)).unwrap();
    assert_eq!(a["path"], b["path"], "a retry must not duplicate");
    let dir = duduclaw_core::task_packet::packet_path(
        tmp.path(),
        TASK,
        1,
        Role::Planner,
        Role::Executor,
    )
    .unwrap();
    let count = fs::read_dir(dir.parent().unwrap())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
        .count();
    assert_eq!(count, 1, "exactly one packet file");
}

#[tokio::test(flavor = "current_thread")]
async fn planner_fanout_keeps_every_sub_task_packet() {
    // One round, one leg, several independent sub-tasks: the composer
    // reads the whole round directory, so a second packet must land beside
    // the first instead of overwriting it.
    let tmp = TempDir::new();
    mk_member(tmp.path(), "planner-1", "planner", Some(TASK), Some(1));
    let mut paths = Vec::new();
    for i in 0..3 {
        let mut p = packet(Role::Planner, Role::Executor, 1);
        p.packet_id = format!("pk-{i}");
        p.objective = format!("子任務 {i}");
        let out = call(tmp.path(), "planner-1", &p).await;
        assert!(!is_error(&out), "{out}");
        let body: Value = serde_json::from_str(&text_of(&out)).unwrap();
        paths.push(body["path"].as_str().unwrap().to_string());
    }
    assert_eq!(
        paths.iter().collect::<std::collections::HashSet<_>>().len(),
        3,
        "three distinct files: {paths:?}"
    );
    assert!(paths[0].ends_with("planner-to-executor.json"));
    assert!(paths[1].ends_with("planner-to-executor.01.json"));
    assert!(paths[2].ends_with("planner-to-executor.02.json"));
    for (i, rel) in paths.iter().enumerate() {
        let read: TaskPacket =
            serde_json::from_slice(&fs::read(tmp.path().join(rel)).unwrap()).unwrap();
        assert_eq!(read.packet_id, format!("pk-{i}"));
        assert_eq!(read.objective, format!("子任務 {i}"));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn writes_a_provenance_row_and_an_audit_event() {
    let tmp = TempDir::new();
    mk_member(tmp.path(), "exec-1", "executor", Some(TASK), Some(1));
    let out = call(
        tmp.path(),
        "exec-1",
        &packet(Role::Executor, Role::Verifier, 1),
    )
    .await;
    assert!(!is_error(&out), "{out}");

    let ledger = fs::read_to_string(tmp.path().join("artifacts.jsonl")).unwrap();
    let row: Value = serde_json::from_str(ledger.lines().next().unwrap()).unwrap();
    assert_eq!(row["origin"], "produced");
    assert_eq!(row["task_id"], TASK);
    assert_eq!(row["round"], 1);
    assert_eq!(row["agent_id"], "exec-1");

    let audit = fs::read_to_string(tmp.path().join("security_audit.jsonl")).unwrap();
    let ev: Value = serde_json::from_str(
        audit
            .lines()
            .find(|l| l.contains("\"team_handoff\""))
            .expect("audit event written"),
    )
    .unwrap();
    assert_eq!(ev["event_type"], "team_handoff");
    assert_eq!(ev["agent_id"], "exec-1");
    assert_eq!(ev["details"]["task_id"], TASK);
    assert_eq!(ev["details"]["round"], 1);
    assert_eq!(ev["details"]["from_role"], "executor");
    assert_eq!(ev["details"]["to_role"], "verifier");
    assert_eq!(ev["details"]["constraints"], 1);
    assert_eq!(ev["details"]["audience"][0], "verifier");
    assert!(ev["details"]["bytes"].as_u64().unwrap() > 0);
}

#[tokio::test(flavor = "current_thread")]
async fn refuses_a_non_member() {
    let tmp = TempDir::new();
    // An ordinary employee: `[agent] role` is an ORG role, not a team role.
    let dir = tmp.path().join("agents").join("agnes");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("agent.toml"), "[agent]\nrole = \"main\"\n").unwrap();
    let out = call(
        tmp.path(),
        "agnes",
        &packet(Role::Planner, Role::Executor, 1),
    )
    .await;
    assert_eq!(refusal_code(&out), "not_a_team_member");

    // No agent.toml at all is equally a refusal — membership is proven,
    // never assumed.
    fs::create_dir_all(tmp.path().join("agents").join("ghost")).unwrap();
    let out = call(
        tmp.path(),
        "ghost",
        &packet(Role::Planner, Role::Executor, 1),
    )
    .await;
    assert_eq!(refusal_code(&out), "not_a_team_member");
}

/// Review finding 2 regression — this test **replaces**
/// `accepts_the_agent_role_fallback`, which asserted the defect as correct
/// behavior. `[agent] role` is an *org* role and must never confer team
/// membership: `AgentRole::Planner`'s canonical string is `"planner"`, so
/// the old fallback admitted any long-lived employee configured that way,
/// unpinned, able to write a forged planner→executor packet into any
/// task's directory.
#[tokio::test(flavor = "current_thread")]
async fn refuses_an_ordinary_employee_whose_org_role_happens_to_parse() {
    let tmp = TempDir::new();
    // `planner` and `verifier` are both real `AgentRole` variants AND real
    // team `Role` variants — the exact collision the fallback re-opened.
    for role in ["planner", "verifier"] {
        let id = format!("employee-{role}");
        let dir = tmp.path().join("agents").join(&id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("agent.toml"),
            format!("[agent]\nname = \"{id}\"\nrole = \"{role}\"\n"),
        )
        .unwrap();
        let p = packet(Role::Planner, Role::Executor, 1);
        let out = call(tmp.path(), &id, &p).await;
        assert_eq!(
            refusal_code(&out),
            "not_a_team_member",
            "[agent] role = {role:?} must not confer team membership"
        );
        assert!(
            !tmp.path().join("team_packets").exists(),
            "nothing may be written for a non-member"
        );
    }
}

/// An identity with no `task_id` / `round` pin must not be able to name an
/// arbitrary `goal_id`: both cross-checks downstream are `if let
/// Some(pinned)` and would be skipped entirely.
#[tokio::test(flavor = "current_thread")]
async fn refuses_an_unpinned_identity_naming_someone_elses_task() {
    let tmp = TempDir::new();
    // A hand-authored `[team_member]` with only a role — what a scaffolded
    // member never looks like.
    let dir = tmp.path().join("agents").join("squatter");
    fs::create_dir_all(&dir).unwrap();
    let unpinned = "[team_member]\nrole = \"planner\"\n";
    fs::write(dir.join("agent.toml"), unpinned).unwrap();
    let mut p = packet(Role::Planner, Role::Executor, 7);
    p.goal_id = "aaaaaaaa-1111-4222-8333-444455556666".into();
    let out = call(tmp.path(), "squatter", &p).await;
    assert_eq!(refusal_code(&out), "unpinned_team_member");
    assert!(
        !tmp.path().join("team_packets").exists(),
        "an unpinned caller must write nothing"
    );

    // Pinned to that same task, the identical packet is accepted — so the
    // refusal is about the missing pin, not about the packet.
    mk_member(
        tmp.path(),
        "pinned",
        "planner",
        Some("aaaaaaaa-1111-4222-8333-444455556666"),
        Some(7),
    );
    let ok = call(tmp.path(), "pinned", &p).await;
    assert!(!is_error(&ok), "{ok}");
}

#[tokio::test(flavor = "current_thread")]
async fn refuses_a_packet_claiming_someone_elses_role() {
    let tmp = TempDir::new();
    mk_member(tmp.path(), "exec-1", "executor", Some(TASK), Some(1));
    let out = call(
        tmp.path(),
        "exec-1",
        &packet(Role::Planner, Role::Executor, 1),
    )
    .await;
    assert_eq!(refusal_code(&out), "from_role_mismatch");
}

#[tokio::test(flavor = "current_thread")]
async fn refuses_a_foreign_task_or_round() {
    let tmp = TempDir::new();
    mk_member(tmp.path(), "planner-1", "planner", Some(TASK), Some(2));

    let mut foreign = packet(Role::Planner, Role::Executor, 2);
    foreign.goal_id = "0000dead-1111-4222-8333-444455556666".to_string();
    assert_eq!(
        refusal_code(&call(tmp.path(), "planner-1", &foreign).await),
        "task_id_mismatch"
    );

    let stale = packet(Role::Planner, Role::Executor, 1);
    assert_eq!(
        refusal_code(&call(tmp.path(), "planner-1", &stale).await),
        "round_mismatch"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn refuses_every_illegal_edge() {
    let tmp = TempDir::new();
    for (from, to) in [
        (Role::Planner, Role::Verifier),
        (Role::Planner, Role::Utility),
        (Role::Executor, Role::Planner),
        (Role::Verifier, Role::Planner),
        (Role::Utility, Role::Executor),
    ] {
        let id = format!("m-{from}-{to}");
        mk_member(tmp.path(), &id, from.as_str(), Some(TASK), Some(1));
        let out = call(tmp.path(), &id, &packet(from, to, 1)).await;
        assert_eq!(
            refusal_code(&out),
            "invalid_handoff_edge",
            "{from} → {to} must be refused"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn legal_edges_are_exactly_the_three_stage_transitions() {
    use duduclaw_core::types::Role::*;
    for from in Role::ALL {
        for to in Role::ALL {
            let legal = is_legal_handoff_edge(*from, *to);
            let expect = matches!(
                (*from, *to),
                (Planner, Executor) | (Executor, Verifier) | (Verifier, Executor)
            );
            assert_eq!(legal, expect, "{from} → {to}");
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn names_a_smuggled_provider_field() {
    let tmp = TempDir::new();
    mk_member(tmp.path(), "planner-1", "planner", Some(TASK), Some(1));
    for key in ["transcript", "tool_use", "thinking", "reasoning"] {
        let mut raw = serde_json::to_value(packet(Role::Planner, Role::Executor, 1)).unwrap();
        raw.as_object_mut()
            .unwrap()
            .insert(key.to_string(), serde_json::json!("…"));
        let out = handle_team_handoff(
            &serde_json::json!({ "packet": raw }),
            tmp.path(),
            "planner-1",
        )
        .await;
        assert_eq!(refusal_code(&out), "forbidden_provider_field");
        assert!(text_of(&out).contains(key), "refusal must name `{key}`");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn finds_a_forbidden_key_nested_inside_an_object() {
    let deep = serde_json::json!({ "a": [{ "b": { "encrypted_content": "x" } }] });
    assert_eq!(
        find_forbidden_packet_key(&deep, 0),
        Some("encrypted_content")
    );
    // Exact key equality only — never a substring test.
    let near_miss = serde_json::json!({ "reasoning_notes": 1, "my_transcript": 2 });
    assert_eq!(find_forbidden_packet_key(&near_miss, 0), None);
}

#[tokio::test(flavor = "current_thread")]
async fn rejects_an_over_cap_packet_whole_and_writes_nothing() {
    let tmp = TempDir::new();
    mk_member(tmp.path(), "planner-1", "planner", Some(TASK), Some(1));
    let mut p = packet(Role::Planner, Role::Executor, 1);
    for i in 0..13 {
        p.constraints
            .push(duduclaw_core::task_packet::Constraint::new(
                format!("x{i}"),
                "越界",
            ));
    }
    let out = call(tmp.path(), "planner-1", &p).await;
    assert_eq!(refusal_code(&out), "too_many_items");
    assert!(!tmp.path().join("team_packets").exists(), "nothing written");
}

#[tokio::test(flavor = "current_thread")]
async fn refuses_a_missing_or_malformed_packet() {
    let tmp = TempDir::new();
    mk_member(tmp.path(), "planner-1", "planner", Some(TASK), Some(1));
    assert_eq!(
        refusal_code(
            &handle_team_handoff(&serde_json::json!({}), tmp.path(), "planner-1").await
        ),
        "missing_packet"
    );
    assert_eq!(
        refusal_code(
            &handle_team_handoff(
                &serde_json::json!({ "packet": "not json" }),
                tmp.path(),
                "planner-1"
            )
            .await
        ),
        "invalid_packet"
    );
    assert_eq!(
        refusal_code(
            &handle_team_handoff(&serde_json::json!({ "packet": 7 }), tmp.path(), "planner-1")
                .await
        ),
        "invalid_packet"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn accepts_a_json_encoded_packet_string() {
    let tmp = TempDir::new();
    mk_member(tmp.path(), "planner-1", "planner", Some(TASK), Some(1));
    let encoded = serde_json::to_string(&packet(Role::Planner, Role::Executor, 1)).unwrap();
    let out = handle_team_handoff(
        &serde_json::json!({ "packet": encoded }),
        tmp.path(),
        "planner-1",
    )
    .await;
    assert!(!is_error(&out), "{out}");
}

#[tokio::test(flavor = "current_thread")]
async fn refuses_an_unreadable_team_member_section() {
    let tmp = TempDir::new();
    let dir = tmp.path().join("agents").join("broken");
    fs::create_dir_all(&dir).unwrap();
    // Present but wrong: a role that is not one of the four.
    fs::write(dir.join("agent.toml"), "[team_member]\nrole = \"boss\"\n").unwrap();
    let out = call(
        tmp.path(),
        "broken",
        &packet(Role::Planner, Role::Executor, 1),
    )
    .await;
    assert_eq!(refusal_code(&out), "team_member_unreadable");
}

// ── Round-2 live-test regressions ───────────────────────────────────

#[tokio::test(flavor = "current_thread")]
async fn a_role_member_under_the_ephemeral_layout_can_file_a_packet() {
    // The round-2 defect: role members live at
    // `agents/.ephemeral/<eph-id>/`, the reader looked at `agents/<id>/`,
    // so the one packet the planner did get past the validator was refused
    // `not_a_team_member` and the round ended `planner_no_packets`.
    let tmp = TempDir::new();
    let id = "eph-agnes-r1-planner-1ae27d";
    mk_ephemeral_member(tmp.path(), id, "planner", Some(TASK), Some(1));
    let out = call(tmp.path(), id, &packet(Role::Planner, Role::Executor, 1)).await;
    assert!(!is_error(&out), "{out}");
    assert!(
        duduclaw_core::task_packet::packet_path(
            tmp.path(),
            TASK,
            1,
            Role::Planner,
            Role::Executor
        )
        .unwrap()
        .exists()
    );
    // And the provenance row still attributes to the member, at the home's
    // ledger — not to an empty id inside the scaffold.
    let ledger = fs::read_to_string(tmp.path().join("artifacts.jsonl")).unwrap();
    let row: Value = serde_json::from_str(ledger.lines().next().unwrap()).unwrap();
    assert_eq!(row["agent_id"], id);
    assert_eq!(row["task_id"], TASK);
}

#[tokio::test(flavor = "current_thread")]
async fn a_non_member_under_the_ephemeral_layout_is_still_refused() {
    // The fix widens where identity is read from, never who counts as a
    // member: an `eph-` scaffold with no `[team_member]` section is an
    // ordinary ephemeral worker.
    let tmp = TempDir::new();
    let id = "eph-plain-worker-9f2a11";
    let dir = tmp
        .path()
        .join("agents")
        .join(duduclaw_gateway::ephemeral::EPHEMERAL_DIR_NAME)
        .join(id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("agent.toml"), "[agent]\nrole = \"worker\"\n").unwrap();
    let out = call(tmp.path(), id, &packet(Role::Planner, Role::Executor, 1)).await;
    assert_eq!(refusal_code(&out), "not_a_team_member");
}

#[tokio::test(flavor = "current_thread")]
async fn the_documented_minimal_packet_is_accepted_verbatim() {
    // Exactly the seven keys the tool description shows, with the identity
    // values this member actually has.
    let tmp = TempDir::new();
    mk_member(tmp.path(), "planner-1", "planner", Some(TASK), Some(2));
    let raw = serde_json::json!({
        "packet_id": "pk-min",
        "goal_id": TASK,
        "round": 2,
        "from_role": "planner",
        "to_role": "executor",
        "objective": "整理 Q2 到期合約清單",
        "output_format": "markdown",
    });
    let out = handle_team_handoff(
        &serde_json::json!({ "packet": raw }),
        tmp.path(),
        "planner-1",
    )
    .await;
    assert!(!is_error(&out), "{out}");
    let on_disk = duduclaw_core::task_packet::packet_path(
        tmp.path(),
        TASK,
        2,
        Role::Planner,
        Role::Executor,
    )
    .unwrap();
    let reread: TaskPacket = serde_json::from_slice(&fs::read(&on_disk).unwrap()).unwrap();
    assert_eq!(reread.packet_id, "pk-min");
    // The short `output_format` string is normalized to the tagged form on
    // disk, so the composer reads exactly what it always did.
    assert_eq!(reread.output_format, OutputFormat::Markdown);
    let bytes = fs::read_to_string(&on_disk).unwrap();
    assert!(
        bytes.contains(r#""output_format":{"kind":"markdown"}"#),
        "{bytes}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn identity_fields_may_be_omitted_and_are_filled_from_the_member() {
    let tmp = TempDir::new();
    mk_ephemeral_member(
        tmp.path(),
        "eph-agnes-r3-executor-0c0c0c",
        "executor",
        Some(TASK),
        Some(3),
    );
    let out = handle_team_handoff(
        &serde_json::json!({ "packet": {
            "packet_id": "pk-thin",
            "objective": "完成子任務並附證據",
            "output_format": "files",
        }}),
        tmp.path(),
        "eph-agnes-r3-executor-0c0c0c",
    )
    .await;
    assert!(!is_error(&out), "{out}");
    let on_disk = duduclaw_core::task_packet::packet_path(
        tmp.path(),
        TASK,
        3,
        Role::Executor,
        Role::Verifier,
    )
    .unwrap();
    let reread: TaskPacket = serde_json::from_slice(&fs::read(&on_disk).unwrap()).unwrap();
    assert_eq!(reread.goal_id, TASK);
    assert_eq!(reread.round, 3);
    assert_eq!(reread.from_role, Role::Executor);
    // Derived, because `executor` has exactly one legal outgoing edge.
    assert_eq!(reread.to_role, Role::Verifier);
}

#[tokio::test(flavor = "current_thread")]
async fn filling_is_never_a_bypass() {
    // A caller that DOES write an identity field is cross-checked exactly
    // as before — the default only applies to an absent key.
    let tmp = TempDir::new();
    mk_ephemeral_member(
        tmp.path(),
        "eph-agnes-r2-executor-abc123",
        "executor",
        Some(TASK),
        Some(2),
    );
    for (patch, code) in [
        (
            serde_json::json!({ "from_role": "planner" }),
            "from_role_mismatch",
        ),
        (serde_json::json!({ "round": 1 }), "round_mismatch"),
        (
            serde_json::json!({ "goal_id": "0000dead-1111-4222-8333-444455556666" }),
            "task_id_mismatch",
        ),
        (
            serde_json::json!({ "to_role": "planner" }),
            "invalid_handoff_edge",
        ),
    ] {
        let mut raw = serde_json::json!({
            "packet_id": "pk-thin",
            "objective": "完成子任務",
            "output_format": "files",
        });
        for (k, v) in patch.as_object().unwrap() {
            raw.as_object_mut().unwrap().insert(k.clone(), v.clone());
        }
        let out = handle_team_handoff(
            &serde_json::json!({ "packet": raw }),
            tmp.path(),
            "eph-agnes-r2-executor-abc123",
        )
        .await;
        assert_eq!(refusal_code(&out), code, "{patch}");
    }
}
