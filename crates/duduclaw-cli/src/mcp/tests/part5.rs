use super::*;

#[tokio::test(flavor = "current_thread")]
async fn f2_list_agents_hides_deleted_and_archived() {
    let tmp = TempDir::new();
    let home = tmp.path();
    let agents_dir = home.join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    create_test_agent(&agents_dir, "active-one", "");
    create_test_agent(&agents_dir, "archived-one", "");
    create_test_agent(&agents_dir, "deleted-one", "");
    set_agent_status(&agents_dir, "archived-one", "archived");
    set_agent_status(&agents_dir, "deleted-one", "deleted");

    // WP21 T6: these three agents are unrelated siblings (no reports_to,
    // no department), so under the default `department` policy an
    // ordinary caller would see none of them — orthogonal to what this
    // test verifies (F2 status filtering). Call as a system sender
    // ("dashboard") to keep the pre-WP21 unrestricted listing.
    // Default: only active shown.
    let res = handle_list_agents(&serde_json::json!({}), home, "dashboard").await;
    let text = res["content"][0]["text"].as_str().unwrap_or("");
    assert!(text.contains("active-one"), "active must be listed: {text}");
    assert!(
        !text.contains("archived-one"),
        "archived hidden by default: {text}"
    );
    assert!(
        !text.contains("deleted-one"),
        "deleted always hidden: {text}"
    );

    // include_archived=true: archived surfaces, deleted still hidden.
    let res = handle_list_agents(
        &serde_json::json!({ "include_archived": true }),
        home,
        "dashboard",
    )
    .await;
    let text = res["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        text.contains("archived-one"),
        "archived shown on request: {text}"
    );
    assert!(
        !text.contains("deleted-one"),
        "deleted still hidden even with flag: {text}"
    );
}

// ── O2: spawn_ephemeral (dynamic sub-agent synthesis) ────────────

#[tokio::test]
async fn e2e_spawn_ephemeral_rejects_privilege_escalation() {
    let tmp = TempDir::new();
    let home = tmp.path();
    let agents_dir = home.join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    create_test_agent_with_caps(&agents_dir, "boss", &["Read", "Grep"]);

    let ctx = DelegationContext {
        depth: 0,
        origin: None,
    };
    let params = serde_json::json!({
        "instruction": "You are a log summarizer.",
        "context": "Summarize the attached logs.",
        "tools": ["Read", "Bash"], // Bash is NOT in boss's allowlist
    });
    let result = spawn_ephemeral_with_ctx(&params, home, "boss", ctx).await;

    assert_eq!(result["isError"], true);
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("privilege escalation"), "got: {text}");
    // Fail-closed: nothing scaffolded, nothing queued.
    let eph_root = agents_dir.join(".ephemeral");
    assert!(
        !eph_root.exists() || fs::read_dir(&eph_root).unwrap().next().is_none(),
        "escalation attempt must not leave a scaffold"
    );
    assert!(!home.join("bus_queue.jsonl").exists());
}

#[tokio::test]
async fn e2e_spawn_ephemeral_scaffolds_and_queues() {
    let tmp = TempDir::new();
    let home = tmp.path();
    let agents_dir = home.join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    create_test_agent_with_caps(&agents_dir, "boss", &["Read", "Grep", "WebFetch"]);

    let ctx = DelegationContext {
        depth: 1,
        origin: Some("root".into()),
    };
    let params = serde_json::json!({
        "instruction": "You extract dates from text. 只回傳日期。",
        "context": "Extract every date from: meeting on 2026-07-11.",
        "tools": ["Read"],
        "tier": "cheap",
    });
    let result = spawn_ephemeral_with_ctx(&params, home, "boss", ctx).await;

    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("synthesized and task queued"),
        "expected success, got: {text}"
    );

    // Bus entry targets the ephemeral id with incremented depth.
    let content = fs::read_to_string(home.join("bus_queue.jsonl")).unwrap();
    let msg: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
    let eph_id = msg["agent_id"].as_str().unwrap().to_string();
    assert!(
        duduclaw_gateway::ephemeral::is_ephemeral_id(&eph_id),
        "bus target must be an ephemeral id, got {eph_id}"
    );
    assert_eq!(msg["delegation_depth"], 2);
    assert_eq!(msg["origin_agent"], "root");
    assert_eq!(msg["sender_agent"], "boss");

    // Scaffold exists under the dedicated namespace with the restricted
    // subset, the instruction as SOUL.md, and the requested tier.
    let dir = duduclaw_gateway::ephemeral::resolve_agent_dir(home, &eph_id)
        .expect("scaffold must resolve inside the ephemeral namespace");
    let cfg: duduclaw_core::types::AgentConfig =
        toml::from_str(&fs::read_to_string(dir.join("agent.toml")).unwrap()).unwrap();
    assert_eq!(cfg.capabilities.allowed_tools, vec!["Read".to_string()]);
    assert_eq!(cfg.agent.reports_to, "boss");
    let soul = fs::read_to_string(dir.join("SOUL.md")).unwrap();
    assert!(soul.contains("只回傳日期"));
    let meta = duduclaw_gateway::ephemeral::read_meta(&dir).unwrap();
    assert_eq!(meta.tier, "cheap");
    assert_eq!(meta.parent, "boss");
}

// ── H19: admission queue (queue-vs-fail on over-capacity ephemeral spawn) ──

/// Default `[dispatch] admission = "queue"`: a legitimate spawn request
/// that merely arrives while capacity is exhausted must be durably
/// queued, NOT hard-rejected.
#[tokio::test]
async fn e2e_spawn_ephemeral_queues_by_default_when_over_capacity() {
    let tmp = TempDir::new();
    let home = tmp.path();
    let agents_dir = home.join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    create_test_agent_with_caps(&agents_dir, "boss", &["Read"]);
    fs::write(
        home.join("config.toml"),
        "[dispatch]\nephemeral_max_active = 1\n",
    )
    .unwrap();

    let ctx = DelegationContext {
        depth: 0,
        origin: None,
    };
    let params = |ctxtext: &str| {
        serde_json::json!({
            "instruction": "You are a worker.",
            "context": ctxtext,
            "tools": ["Read"],
        })
    };

    // First fills the (cap=1) capacity — succeeds normally.
    let first =
        spawn_ephemeral_with_ctx(&params("first task"), home, "boss", ctx.clone()).await;
    assert_ne!(
        first["isError"], true,
        "first spawn under cap must succeed: {first}"
    );

    // Second arrives while at capacity — must be queued, not rejected.
    let second = spawn_ephemeral_with_ctx(&params("second task"), home, "boss", ctx).await;
    assert_ne!(
        second["isError"], true,
        "over-capacity spawn must be queued (isError=false) by default, got: {second}"
    );
    let text = second["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("queued"), "got: {text}");
    assert!(text.contains("Ticket:"), "got: {text}");

    assert_eq!(
        duduclaw_core::spawn_admission::queue_depth(
            home,
            duduclaw_gateway::ephemeral::EPHEMERAL_ADMISSION_CLASS,
        ),
        1,
        "the over-capacity request must be durably queued"
    );
    // No second scaffold or second bus entry was created for it yet.
    let bus_lines = fs::read_to_string(home.join("bus_queue.jsonl"))
        .unwrap()
        .lines()
        .count();
    assert_eq!(
        bus_lines, 1,
        "only the first (admitted) spawn reached the bus"
    );
}

/// `[dispatch] admission = "fail"` is the explicit opt-out: over-capacity
/// requests fall back to the pre-H19 hard-reject behavior, byte-identical
/// error text, and nothing is queued.
#[tokio::test]
async fn e2e_spawn_ephemeral_admission_fail_falls_back_to_hard_reject() {
    let tmp = TempDir::new();
    let home = tmp.path();
    let agents_dir = home.join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    create_test_agent_with_caps(&agents_dir, "boss", &["Read"]);
    fs::write(
        home.join("config.toml"),
        "[dispatch]\nephemeral_max_active = 1\nadmission = \"fail\"\n",
    )
    .unwrap();

    let ctx = DelegationContext {
        depth: 0,
        origin: None,
    };
    let params = |ctxtext: &str| {
        serde_json::json!({
            "instruction": "You are a worker.",
            "context": ctxtext,
            "tools": ["Read"],
        })
    };
    let first =
        spawn_ephemeral_with_ctx(&params("first task"), home, "boss", ctx.clone()).await;
    assert_ne!(first["isError"], true);

    let second = spawn_ephemeral_with_ctx(&params("second task"), home, "boss", ctx).await;
    assert_eq!(
        second["isError"], true,
        "admission=fail must hard-reject, got: {second}"
    );
    let text = second["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains(duduclaw_gateway::ephemeral::EPHEMERAL_CAPACITY_ERROR_PREFIX),
        "got: {text}"
    );
    assert_eq!(
        duduclaw_core::spawn_admission::queue_depth(
            home,
            duduclaw_gateway::ephemeral::EPHEMERAL_ADMISSION_CLASS,
        ),
        0,
        "admission=fail must never persist a queued ticket"
    );
}

/// WP21 C5 regression: the ephemeral agent's parent is the caller, always.
/// `spawn_ephemeral` needs no separate C4 placement check *because* of this
/// — if a future refactor lets a tool parameter choose the parent, the C4
/// gate must be added at the same time or self-service escalation reopens.
#[tokio::test]
async fn c5_spawn_ephemeral_parent_is_always_the_caller() {
    let tmp = TempDir::new();
    let home = tmp.path();
    let agents_dir = home.join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    create_test_agent(&agents_dir, "ceo", "");
    create_test_agent_with_caps(&agents_dir, "boss", &["Read"]);

    // Hostile params: try to name a different parent three plausible ways.
    let params = serde_json::json!({
        "instruction": "x",
        "context": "y",
        "tools": ["Read"],
        "parent": "ceo",
        "reports_to": "ceo",
        "caller": "ceo",
    });
    let ctx = DelegationContext {
        depth: 0,
        origin: None,
    };
    let result = spawn_ephemeral_with_ctx(&params, home, "boss", ctx).await;
    assert_ne!(result["isError"], true, "expected success, got: {result}");

    let content = fs::read_to_string(home.join("bus_queue.jsonl")).unwrap();
    let msg: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
    let eph_id = msg["agent_id"].as_str().unwrap().to_string();
    let dir = duduclaw_gateway::ephemeral::resolve_agent_dir(home, &eph_id).unwrap();
    let cfg: duduclaw_core::types::AgentConfig =
        toml::from_str(&fs::read_to_string(dir.join("agent.toml")).unwrap()).unwrap();
    assert_eq!(
        cfg.agent.reports_to, "boss",
        "ephemeral parent must be the caller, never a tool parameter"
    );
    assert_eq!(
        duduclaw_gateway::ephemeral::read_meta(&dir).unwrap().parent,
        "boss"
    );
}

#[tokio::test]
async fn e2e_spawn_ephemeral_rejects_raw_model_id_tier_and_depth_limit() {
    let tmp = TempDir::new();
    let home = tmp.path();
    let agents_dir = home.join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    create_test_agent_with_caps(&agents_dir, "boss", &["Read"]);

    // A raw model id in `tier` must be rejected (multi-model doctrine).
    let params = serde_json::json!({
        "instruction": "x", "context": "y", "tools": ["Read"],
        "tier": "claude-opus-4-5",
    });
    let ctx = DelegationContext {
        depth: 0,
        origin: None,
    };
    let result = spawn_ephemeral_with_ctx(&params, home, "boss", ctx).await;
    assert_eq!(result["isError"], true);
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("invalid tier"), "got: {text}");

    // Depth limit applies exactly like spawn_agent.
    let params = serde_json::json!({
        "instruction": "x", "context": "y", "tools": ["Read"],
    });
    let ctx = DelegationContext {
        depth: 4,
        origin: None,
    };
    let result = spawn_ephemeral_with_ctx(&params, home, "boss", ctx).await;
    assert_eq!(result["isError"], true);
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("delegation depth limit"), "got: {text}");
}

#[tokio::test]
async fn e2e_depth_zero_defaults_origin_to_caller() {
    let tmp = TempDir::new();
    let home = tmp.path();
    let agents_dir = home.join("agents");
    fs::create_dir_all(&agents_dir).unwrap();
    init_message_queue_schema(home);

    create_test_agent(&agents_dir, "main", "");
    create_test_agent(&agents_dir, "worker", "main");

    // No origin/sender set — simulates first delegation (no dispatcher context)
    let ctx = DelegationContext {
        depth: 0,
        origin: None,
    };
    let params = serde_json::json!({ "agent_id": "worker", "prompt": "first delegation" });
    let result = send_to_agent_with_ctx(&params, home, "main", ctx).await;

    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("depth=1"),
        "Expected depth 1 (0+1), got: {text}"
    );

    // v1.8.18: verify via SQLite — bus_queue.jsonl is no longer
    // written by `send_to_agent` (see `send_to_agent_never_writes_bus_queue_jsonl`).
    let db_path = home.join("message_queue.db");
    let (depth, origin_agent): (i32, String) = rusqlite::Connection::open(&db_path)
        .expect("open message_queue.db")
        .query_row(
            "SELECT delegation_depth, origin_agent \
                 FROM message_queue ORDER BY rowid DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("row in message_queue.db");
    assert_eq!(depth, 1);
    assert_eq!(origin_agent, "main", "Should fall back to caller");
}

// ── v1.8.25: local timezone auto-detect ─────────────────────────

#[test]
fn detect_local_timezone_returns_valid_iana_name() {
    // This is a host-system-dependent test. We can't assert the
    // specific zone (CI may run on UTC / America/Los_Angeles /
    // Asia/Taipei), but we CAN assert that whatever we get back
    // parses as a valid chrono-tz IANA name. That's the contract
    // the cron rail relies on.
    //
    // On hosts with no discoverable TZ (extremely minimal Docker
    // images), the function legitimately returns None — we accept
    // both outcomes but check parseability of any name returned.
    if let Some(tz_name) = detect_local_timezone() {
        assert!(
            duduclaw_core::parse_timezone(&tz_name).is_some(),
            "detected TZ '{tz_name}' must round-trip through parse_timezone"
        );
        assert!(!tz_name.is_empty(), "detected TZ must not be empty string");
    }
}

// ── RFC-22 Phase 3 W2: claimed-author detection tests ───────────────

#[test]
fn detect_authors_extracts_zh_observation_heading() {
    // Replicates the 5/5 wiki shape that triggered hallucinated PM section.
    let content = r#"---
title: "test"
---

# Discussion

## duduclaw-tl 的觀點

Some content...

## duduclaw-pm 的觀點

PM-style content (potentially hallucinated).
"#;
    let authors = detect_claimed_authors_in_wiki(content);
    assert_eq!(
        authors,
        vec!["duduclaw-pm".to_string(), "duduclaw-tl".to_string()],
        "should extract both ## <agent> 的觀點 sections"
    );
}

#[test]
fn detect_authors_handles_bold_attribution() {
    let content = "**回覆人**：duduclaw-tl\n\n some content";
    let authors = detect_claimed_authors_in_wiki(content);
    assert_eq!(authors, vec!["duduclaw-tl".to_string()]);
}

#[test]
fn detect_authors_filters_non_agent_shapes() {
    // Heading that looks like the pattern but agent name is not valid
    // (uppercase, special chars) — must not be reported.
    let content = "## DuDuClaw 的觀點\n\n## hello.world 的觀點\n";
    let authors = detect_claimed_authors_in_wiki(content);
    assert!(
        authors.is_empty(),
        "uppercase / dotted names must not be matched: got {authors:?}"
    );
}

#[test]
fn detect_authors_picks_up_frontmatter_claimed_authors() {
    let content = r#"---
title: "x"
claimed_authors: [agnes, duduclaw-tl, duduclaw-pm]
---

# Body
"#;
    let mut authors = detect_claimed_authors_in_wiki(content);
    authors.sort();
    assert_eq!(
        authors,
        vec![
            "agnes".to_string(),
            "duduclaw-pm".to_string(),
            "duduclaw-tl".to_string(),
        ]
    );
}

#[test]
fn detect_authors_empty_for_solo_author_doc() {
    // Single-author doc with only a frontmatter `author: agnes` — no
    // ## <agent> 觀點 sections, no `**回覆人**` attribution. Must NOT
    // pick up `agnes` from the regular `author:` field, because that's
    // the canonical authorship (different from "claimed_authors").
    let content = r#"---
title: "x"
author: agnes
---

# Body
just a normal note
"#;
    let authors = detect_claimed_authors_in_wiki(content);
    assert!(
        authors.is_empty(),
        "regular author frontmatter is NOT a claimed-authorship signal; got {authors:?}"
    );
}

#[test]
fn is_agent_id_shape_rejects_obvious_bad() {
    assert!(is_agent_id_shape("agnes"));
    assert!(is_agent_id_shape("duduclaw-tl"));
    assert!(is_agent_id_shape("xianwen-eng-ai"));
    assert!(!is_agent_id_shape("Agnes")); // uppercase
    assert!(!is_agent_id_shape("a")); // too short
    assert!(!is_agent_id_shape("")); // empty
    assert!(!is_agent_id_shape("with space"));
    assert!(!is_agent_id_shape("../etc/passwd"));
    assert!(!is_agent_id_shape("123")); // no alphabetic
}

/// Exercise both real enqueue paths with a host pair instead of global env.
#[tokio::test]
async fn spawn_downstream_lineage_is_complete_or_dropped_and_audited() {
    for ephemeral in [false, true] {
        for (turn, session, missing) in [
            (Some("origin-turn"), Some("webchat:person#agent:boss"), None),
            (Some("origin-turn"), None, Some("session_id")),
            (None, Some("webchat:person#agent:boss"), Some("turn_id")),
            (None, None, None),
        ] {
            let tmp = TempDir::new();
            let home = tmp.path();
            let agents = home.join("agents");
            fs::create_dir_all(&agents).unwrap();
            create_test_agent_with_caps(&agents, "boss", &["Read"]);
            create_test_agent(&agents, "worker", "boss");
            let upstream = (turn.map(str::to_owned), session.map(str::to_owned));
            let ctx = DelegationContext { depth: 0, origin: None };
            let args = if ephemeral {
                serde_json::json!({
                    "instruction": "Read the report.", "context": "Review the report.",
                    "tools": ["Read"], "turn_id": "forged", "session_id": "forged",
                })
            } else {
                serde_json::json!({
                    "agent_id": "worker", "task": "Read the report.",
                    "turn_id": "forged", "session_id": "forged",
                })
            };
            let result = if ephemeral {
                spawn_ephemeral_with_ctx_and_upstream(&args, home, "boss", ctx, upstream).await
            } else {
                spawn_agent_with_ctx_and_upstream(&args, home, "boss", ctx, upstream).await
            };
            assert_ne!(result["isError"], true, "ephemeral={ephemeral}: {result}");
            let lines = fs::read_to_string(home.join("bus_queue.jsonl")).unwrap();
            let row: serde_json::Value = serde_json::from_str(lines.trim()).unwrap();
            assert_eq!(row["spawned"], true);
            if turn.is_some() && session.is_some() {
                assert_eq!(row["turn_id"], turn.unwrap());
                assert_eq!(row["session_id"], session.unwrap());
            } else {
                assert!(row.get("turn_id").is_none(), "{row}");
                assert!(row.get("session_id").is_none(), "{row}");
            }
            assert_eq!(row["upstream_unknown"].as_bool().unwrap_or(false), missing.is_some());
            let audits: Vec<serde_json::Value> = fs::read_to_string(home.join("security_audit.jsonl"))
                .unwrap_or_default().lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .filter(|row| row["event_type"] == "bus_upstream_identity_dropped")
                .collect();
            assert_eq!(audits.len(), usize::from(missing.is_some()));
            if let Some(missing) = missing {
                assert_eq!(audits[0]["details"]["missing"], missing);
                assert_eq!(audits[0]["details"]["message_id"], row["message_id"]);
            }
        }
    }
}

#[tokio::test]
async fn spawn_downstream_lineage_survives_ephemeral_admission_replay() {
    for (turn, session, unknown) in [
        (Some("origin-turn"), Some("webchat:person#agent:boss"), false),
        (Some("origin-turn"), None, true),
    ] {
        let tmp = TempDir::new();
        let home = tmp.path();
        create_test_agent_with_caps(&home.join("agents"), "boss", &["Read"]);
        fs::write(home.join("config.toml"), "[dispatch]\nephemeral_max_active = 1\n").unwrap();
        let args = serde_json::json!({
            "instruction": "Read the report.", "context": "Review the report.", "tools": ["Read"],
        });
        let ctx = || DelegationContext { depth: 0, origin: None };
        let first = spawn_ephemeral_with_ctx_and_upstream(
            &args, home, "boss", ctx(), (None, None),
        ).await;
        assert_ne!(first["isError"], true, "{first}");
        let result = spawn_ephemeral_with_ctx_and_upstream(
            &args, home, "boss", ctx(), (turn.map(str::to_owned), session.map(str::to_owned)),
        ).await;
        assert_ne!(result["isError"], true, "{result}");
        let class = duduclaw_gateway::ephemeral::EPHEMERAL_ADMISSION_CLASS;
        let ticket = duduclaw_core::spawn_admission::dequeue_next_matching(home, class, |_| true)
            .ticket.unwrap();
        assert_eq!(ticket.owner_key.as_deref(), turn, "H19 owner scope is unchanged");
        assert_eq!(ticket.payload["upstream_unknown"].as_bool().unwrap_or(false), unknown);
        assert!(duduclaw_core::spawn_admission::restore_dequeued(home, &ticket).unwrap());
        let first_bus = fs::read_to_string(home.join("bus_queue.jsonl")).unwrap();
        let first_row: serde_json::Value = serde_json::from_str(first_bus.trim()).unwrap();
        let first_dir = duduclaw_gateway::ephemeral::resolve_agent_dir(
            home, first_row["agent_id"].as_str().unwrap(),
        ).unwrap();
        fs::remove_dir_all(first_dir).unwrap();
        assert_eq!(duduclaw_gateway::ephemeral::drain_admission_queue(home).await.admitted, 1);
        let bus = fs::read_to_string(home.join("bus_queue.jsonl")).unwrap();
        let replayed: serde_json::Value = serde_json::from_str(bus.lines().last().unwrap()).unwrap();
        assert_eq!(replayed["spawned"], true);
        if unknown {
            assert!(replayed.get("turn_id").is_none());
            assert!(replayed.get("session_id").is_none());
        } else {
            assert_eq!(replayed["turn_id"], turn.unwrap());
            assert_eq!(replayed["session_id"], session.unwrap());
        }
        assert_eq!(replayed["upstream_unknown"].as_bool().unwrap_or(false), unknown);
        let events = fs::read_to_string(home.join("security_audit.jsonl")).unwrap_or_default();
        assert_eq!(events.lines().filter(|line| line.contains("bus_upstream_identity_dropped")).count(), usize::from(unknown));
    }
}
