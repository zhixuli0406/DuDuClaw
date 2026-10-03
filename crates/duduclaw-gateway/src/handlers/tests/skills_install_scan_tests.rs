//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

//! skills.install must re-run the security scanner server-side: the vet
//! RPC is a separate client call that a malicious client can simply skip.
use super::*;

fn frame_ok(frame: &WsFrame) -> bool {
    matches!(frame, WsFrame::Response { ok: true, .. })
}

fn frame_error_text(frame: &WsFrame) -> String {
    match frame {
        WsFrame::Response { error: Some(e), .. } => e.to_string(),
        _ => String::new(),
    }
}

/// `system.doctor` must include the MCP cold-start card (the "agent has
/// no tools" class) with a valid status. The grok card is only present
/// when a grok CLI exists on the machine, so it is not asserted here.
#[tokio::test]
async fn system_doctor_includes_mcp_server_check() {
    let home = tempfile::tempdir().expect("tempdir");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let checks = handler.run_doctor_checks().await;
    let mcp = checks
        .iter()
        .find(|c| c["name"] == "mcp_server")
        .expect("mcp_server check missing from system.doctor");
    let status = mcp["status"].as_str().unwrap_or("");
    assert!(
        ["pass", "warn", "fail"].contains(&status),
        "unexpected status: {status}"
    );
    assert!(
        !mcp["message"].as_str().unwrap_or("").is_empty(),
        "mcp_server check must carry a message"
    );
}

/// WP6/B2 — re-reading skills from disk must not quietly drop the WP7
/// layering. A globally-installed skill has to keep showing up in an
/// agent's `skills.list` **and** keep its `global` scope badge; listing
/// only the agent's own `SKILLS/` would have "fixed" staleness by hiding
/// every company-wide skill.
#[tokio::test]
async fn skills_list_keeps_global_layer_and_scope_badge() {
    let home = tempfile::tempdir().expect("tempdir");
    let root = home.path();

    // A global (company-wide) skill, written straight to disk the way the
    // synthesis pipeline's graduation step does.
    let global_dir = root.join("skills");
    std::fs::create_dir_all(&global_dir).unwrap();
    std::fs::write(
        global_dir.join("company-tone.md"),
        "---\nname: company-tone\n---\n\n用公司語氣回覆。\n",
    )
    .unwrap();

    // One agent with a skill of its own.
    let agent_dir = root.join("agents").join("agnes");
    std::fs::create_dir_all(agent_dir.join("SKILLS")).unwrap();
    // Reuse a real shipped template rather than a hand-rolled stub:
    // `AgentConfig` requires [model]/[container]/[heartbeat]/[budget]/
    // [permissions]/[evolution], and a stub that silently fails to parse
    // makes this test pass for the wrong reason (empty skill list).
    std::fs::write(
        agent_dir.join("agent.toml"),
        include_str!("../../../../../templates/evaluator/agent.toml")
            .replace("name = \"evaluator\"", "name = \"agnes\""),
    )
    .unwrap();
    std::fs::write(
        agent_dir.join("SKILLS").join("invoice-ocr.md"),
        "---\nname: invoice-ocr\n---\n\n讀發票。\n",
    )
    .unwrap();

    let handler = MethodHandler::new(root.to_path_buf()).await;
    {
        let mut reg = handler.registry.write().await;
        reg.scan().await.expect("scan");
    }

    let frame = handler
        .handle_skills_list(json!({ "agent_id": "agnes" }))
        .await;
    let payload = match &frame {
        WsFrame::Response {
            payload: Some(p), ..
        } => p.clone(),
        _ => panic!("skills.list failed: {}", frame_error_text(&frame)),
    };
    let skills = payload["skills"].as_array().expect("skills array");

    let global = skills
        .iter()
        .find(|s| s["name"] == "company-tone")
        .expect("a global skill must still appear in the agent's list");
    assert_eq!(
        global["scope"], "global",
        "the global layer must keep its scope badge"
    );

    let local = skills
        .iter()
        .find(|s| s["name"] == "invoice-ocr")
        .expect("the agent's own skill must appear");
    assert_eq!(local["scope"], "agent");
}

/// Build the same one-agent-plus-one-global-skill fixture the test above
/// uses, so the aggregate-view assertions read against a known layout.
fn write_skills_fixture(root: &std::path::Path) {
    let global_dir = root.join("skills");
    std::fs::create_dir_all(&global_dir).unwrap();
    std::fs::write(
        global_dir.join("company-tone.md"),
        "---\nname: company-tone\n---\n\n用公司語氣回覆。\n",
    )
    .unwrap();

    let agent_dir = root.join("agents").join("agnes");
    std::fs::create_dir_all(agent_dir.join("SKILLS")).unwrap();
    std::fs::write(
        agent_dir.join("agent.toml"),
        include_str!("../../../../../templates/evaluator/agent.toml")
            .replace("name = \"evaluator\"", "name = \"agnes\""),
    )
    .unwrap();
    std::fs::write(
        agent_dir.join("SKILLS").join("invoice-ocr.md"),
        "---\nname: invoice-ocr\n---\n\n讀發票。\n",
    )
    .unwrap();
}

/// The aggregate branch (`skills.list` with no `agent_id`) backs the "全部
/// AI 員工" default view. It used to serve `agent.skills` — the scan-time
/// snapshot — so a skill written out-of-band after the last scan was
/// visible in the per-agent view and missing from the aggregate one. Both
/// branches must now read the same disk truth.
#[tokio::test]
async fn skills_list_aggregate_reads_disk_not_the_scan_snapshot() {
    let home = tempfile::tempdir().expect("tempdir");
    let root = home.path();
    write_skills_fixture(root);

    let handler = MethodHandler::new(root.to_path_buf()).await;
    {
        let mut reg = handler.registry.write().await;
        reg.scan().await.expect("scan");
    }

    // Written AFTER the scan — exactly what `skill_graduate` / the
    // synthesis pipeline do.
    std::fs::write(
        root.join("agents")
            .join("agnes")
            .join("SKILLS")
            .join("late-arrival.md"),
        "---\nname: late-arrival\n---\n\n掃描後才寫入。\n",
    )
    .unwrap();

    let frame = handler.handle_skills_list(json!({})).await;
    let payload = match &frame {
        WsFrame::Response {
            payload: Some(p), ..
        } => p.clone(),
        _ => panic!("skills.list failed: {}", frame_error_text(&frame)),
    };

    let agents = payload["agents"].as_array().expect("agents array");
    let agnes = agents
        .iter()
        .find(|a| a["agent_id"] == "agnes")
        .expect("agnes must be listed");
    let names: Vec<&str> = agnes["skills"]
        .as_array()
        .expect("skills array")
        .iter()
        .filter_map(|s| s["name"].as_str())
        .collect();
    assert!(
        names.contains(&"late-arrival"),
        "a skill written after the last scan must still be listed, got {names:?}"
    );
    assert!(
        !names.contains(&"company-tone"),
        "the global layer is listed once under `global_skills`; repeating it \
             per agent would show the same skill N times, got {names:?}"
    );

    let global: Vec<&str> = payload["global_skills"]
        .as_array()
        .expect("global_skills array")
        .iter()
        .filter_map(|s| s["name"].as_str())
        .collect();
    assert_eq!(global, vec!["company-tone"]);
}

/// An empty skill list is unfalsifiable from the browser without the
/// directories that were actually walked. "技能庫什麼都看不到" stayed an
/// unresolvable support ticket precisely because the UI could not tell
/// "no skills exist" from "the folder being read is not the folder skills
/// were written to".
#[tokio::test]
async fn skills_list_reports_the_directories_it_scanned() {
    let home = tempfile::tempdir().expect("tempdir");
    let root = home.path();
    write_skills_fixture(root);

    let handler = MethodHandler::new(root.to_path_buf()).await;
    {
        let mut reg = handler.registry.write().await;
        reg.scan().await.expect("scan");
    }

    let frame = handler
        .handle_skills_list(json!({ "agent_id": "agnes" }))
        .await;
    let payload = match &frame {
        WsFrame::Response {
            payload: Some(p), ..
        } => p.clone(),
        _ => panic!("skills.list failed: {}", frame_error_text(&frame)),
    };
    let scanned = payload["scanned"].as_array().expect("scanned array");

    let global = scanned
        .iter()
        .find(|s| s["layer"] == "global")
        .expect("the global layer must be reported");
    assert_eq!(global["exists"], true);
    assert_eq!(global["count"], 1);
    assert_eq!(
        global["path"].as_str().unwrap(),
        root.join("skills").display().to_string(),
        "the reported path must be the one actually read"
    );

    let agent = scanned
        .iter()
        .find(|s| s["layer"] == "agent")
        .expect("the per-agent layer must be reported");
    assert_eq!(agent["count"], 1);
}

/// Every agent-creation path must seed the bundled skills. Wiring it to
/// the MCP `create_agent` tool alone meant a dashboard-onboarded customer
/// got an empty `SKILLS/` — and, since nothing writes `<home>/skills/`
/// either, a permanently blank Skills page.
#[tokio::test]
async fn agent_creation_seeds_the_builtin_skills() {
    let home = tempfile::tempdir().expect("tempdir");
    let root = home.path();
    let skills_dir = root.join("agents").join("newbie").join("SKILLS");
    std::fs::create_dir_all(&skills_dir).unwrap();

    MethodHandler::seed_builtin_skills(&skills_dir);

    let loaded = duduclaw_agent::registry::AgentRegistry::load_skills(&skills_dir).await;
    assert!(
        !loaded.is_empty(),
        "a freshly created staffer must start with skills the loader can see"
    );
    // Seeding writes `<name>/SKILL.md`; the loader keys those on the
    // parent directory name. If the two layouts ever drift apart the
    // skills would be on disk yet invisible — the exact failure mode the
    // customer reported.
    for (name, _) in duduclaw_agent::builtin_skills::BUILTIN_SKILLS {
        assert!(
            loaded.iter().any(|s| s.name == *name),
            "seeded skill `{name}` must be visible to the loader"
        );
    }

    // Idempotent: re-seeding must not duplicate or clobber.
    std::fs::write(
        skills_dir
            .join(duduclaw_agent::builtin_skills::BUILTIN_SKILLS[0].0)
            .join("SKILL.md"),
        "# edited by the operator\n",
    )
    .unwrap();
    MethodHandler::seed_builtin_skills(&skills_dir);
    let after = std::fs::read_to_string(
        skills_dir
            .join(duduclaw_agent::builtin_skills::BUILTIN_SKILLS[0].0)
            .join("SKILL.md"),
    )
    .unwrap();
    assert_eq!(
        after, "# edited by the operator\n",
        "operator edits must win"
    );
}

/// WP6 — a routine created from the dashboard must announce itself on the
/// same `events.db` feedback path the MCP `tasks_create` (with `schedule`) tool uses, so a
/// second browser tab (or the operator's phone) refreshes instead of
/// silently disagreeing with the tab that made the change.
#[tokio::test]
async fn dashboard_cron_add_emits_dashboard_feedback_event() {
    let home = tempfile::tempdir().expect("tempdir");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    // `MethodHandler::new` leaves the cron store unset (the gateway wires it
    // during startup) — seed it so this exercises the real insert path.
    handler
        .set_cron_store(Arc::new(
            CronStore::open(home.path()).expect("open cron store"),
        ))
        .await;

    let frame = handler
        .handle_cron_add(json!({
            "name": "每日晨報",
            "cron": "0 9 * * *",
            "task": "整理今天的行程",
            "agent_id": "agnes",
        }))
        .await;
    assert!(
        matches!(frame, WsFrame::Response { ok: true, .. }),
        "cron.add should succeed: {}",
        frame_error_text(&frame)
    );

    let bus = crate::events_store::EventBusStore::open(home.path()).expect("events.db");
    let rows = bus.fetch_since(0, 50).await.expect("fetch_since");
    let row = rows
        .iter()
        .find(|r| r.event == crate::dashboard_feedback::EV_CRON_CHANGED)
        .expect("dashboard cron.add must raise cron.changed");
    let payload: Value = serde_json::from_str(&row.payload).expect("payload json");
    assert_eq!(payload["action"], "created");
    assert_eq!(payload["name"], "每日晨報");
    assert_eq!(payload["cron"], "0 9 * * *");
    assert_eq!(payload["agent_id"], "agnes");

    // ...and it is a row the bridge will actually push to the socket.
    assert!(
        crate::dashboard_feedback::dashboard_push_frame(&row.event, &row.payload).is_some(),
        "the emitted row must be pushable to the dashboard"
    );
}

/// A rejected `cron.add` persists nothing, so it must announce nothing —
/// otherwise every connected tab refetches to discover the absence of what
/// the event implied.
#[tokio::test]
async fn rejected_dashboard_cron_add_emits_no_dashboard_feedback_event() {
    let home = tempfile::tempdir().expect("tempdir");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    // Store present on purpose: the silence must come from the rejection,
    // not from an uninitialized store short-circuiting the handler.
    handler
        .set_cron_store(Arc::new(
            CronStore::open(home.path()).expect("open cron store"),
        ))
        .await;

    let frame = handler
        .handle_cron_add(json!({
            "name": "壞排程",
            "cron": "not a cron",
            "task": "x",
        }))
        .await;
    assert!(
        matches!(frame, WsFrame::Response { ok: false, .. }),
        "an invalid cron expression must be rejected"
    );

    let bus = crate::events_store::EventBusStore::open(home.path()).expect("events.db");
    let rows = bus.fetch_since(0, 50).await.expect("fetch_since");
    assert!(
        rows.iter()
            .all(|r| r.event != crate::dashboard_feedback::EV_CRON_CHANGED),
        "a failed cron.add must stay silent"
    );
}

#[tokio::test]
async fn malicious_content_install_is_rejected_by_server_side_scan() {
    let home = tempfile::tempdir().expect("tempdir");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    // Code-execution pattern the scanner classifies ≥ High (same fixture
    // family as security_scanner::test_code_execution_error_blocks).
    let malicious = "name: evil-skill\n\nimport subprocess\nsubprocess.run(['ls'])\n";
    let frame = handler
        .handle_skills_install(json!({
            "url": "https://example.com/SKILL.md",
            "scope": "global",
            "content": malicious,
        }))
        .await;

    assert!(!frame_ok(&frame), "high-risk content must be rejected");
    let err = frame_error_text(&frame);
    assert!(
        err.contains("Security scan rejected"),
        "error should carry the scan verdict, got: {err}"
    );
    // Fail-closed: nothing may reach the global skills directory.
    assert!(
        !home.path().join("skills").join("evil-skill.md").exists(),
        "rejected skill must not be written to the skills dir"
    );
}

#[tokio::test]
async fn clean_content_installs() {
    let home = tempfile::tempdir().expect("tempdir");
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    let clean = "name: tidy-skill\ndescription: A helpful guide\n\n# Tidy Skill\n\nHelpful, harmless steps.\n";
    let frame = handler
        .handle_skills_install(json!({
            "url": "https://example.com/SKILL.md",
            "scope": "global",
            "content": clean,
        }))
        .await;

    assert!(frame_ok(&frame), "clean content must install: {frame:?}");
    assert!(
        home.path().join("skills").join("tidy-skill.md").exists(),
        "installed skill file should exist in the global skills dir"
    );
}
