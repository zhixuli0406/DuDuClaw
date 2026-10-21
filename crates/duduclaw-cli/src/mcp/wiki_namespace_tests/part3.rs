use super::*;

#[tokio::test(flavor = "current_thread")]
async fn skill_search_rejects_unknown_hub_exactly() {
    use serde_json::json;
    let home = std::env::temp_dir().join(format!("duduclaw-mcp-hub-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    // Adversarial near-miss ids must error, not fall through to aggregate.
    for bad in ["githu", "github2", "hub", "clawhub-evil"] {
        let res =
            super::handle_skill_search(&json!({"query": "x", "hub": bad}), &home)
                .await;
        assert_eq!(
            res["isError"].as_bool(),
            Some(true),
            "hub '{bad}' must be rejected"
        );
        let text = res["content"][0]["text"].as_str().unwrap_or("");
        assert!(text.contains("unknown hub"), "{text}");
    }
    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test(flavor = "current_thread")]
async fn skill_hub_install_validates_inputs_fail_closed() {
    use serde_json::json;
    let home = std::env::temp_dir().join(format!("duduclaw-mcp-hubi-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();

    // Missing params.
    let res = super::handle_skill_hub_install(&json!({}), &home, "dudu", true).await;
    assert_eq!(res["isError"].as_bool(), Some(true));

    // Path traversal in slug.
    let res = super::handle_skill_hub_install(
        &json!({"hub": "clawhub", "skill_name": "../evil"}),
        &home,
        "dudu",
        true,
    )
    .await;
    assert_eq!(res["isError"].as_bool(), Some(true));

    // Unknown hub is denied without any network call (before approval).
    let res = super::handle_skill_hub_install(
        &json!({"hub": "not-a-hub", "skill_name": "fine-name"}),
        &home,
        "dudu",
        true,
    )
    .await;
    assert_eq!(res["isError"].as_bool(), Some(true));
    let text = res["content"][0]["text"].as_str().unwrap_or("");
    assert!(text.contains("unknown hub"), "{text}");

    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test(flavor = "current_thread")]
async fn skill_pin_and_curator_status_roundtrip() {
    use serde_json::json;
    let home = std::env::temp_dir().join(format!("duduclaw-mcp-pin-{}", uuid::Uuid::new_v4()));
    let skills = home.join("skills");
    std::fs::create_dir_all(&skills).unwrap();
    std::fs::write(skills.join("keeper.md"), "---\nname: keeper\n---\nbody").unwrap();

    // Force a pass so the skill gets tracked.
    let res = super::handle_skill_curator_status(&json!({"run": true}), &home).await;
    assert!(res["isError"].as_bool() != Some(true), "{res}");
    let text = res["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("1 tracked skill"), "{text}");

    // Pin it.
    let res = super::handle_skill_pin(&json!({"skill_name": "keeper"}), &home).await;
    assert!(res["isError"].as_bool() != Some(true), "{res}");
    let text = res["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("pinned"), "{text}");

    // Status reflects the pin.
    let res = super::handle_skill_curator_status(&json!({}), &home).await;
    let text = res["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("keeper [global]"), "{text}");

    // Unknown skill errors honestly.
    let res = super::handle_skill_pin(&json!({"skill_name": "ghost"}), &home).await;
    assert_eq!(res["isError"].as_bool(), Some(true));

    let _ = std::fs::remove_dir_all(&home);
}

// ─────────────────────────────────────────────────────────────────
// agent_update_soul follow-up fixes (#3, #4 — 2026-05-20)
//
// Pre-fix: handle_agent_update_soul wrote SOUL.md but did NOT refresh
// soul_guard hash and did NOT append to tool_calls.jsonl. Result was
// permanent silent drift after every legitimate use of the tool. The
// tests below pin the contract that BOTH side-effects fire on success
// and on selected failure paths.
// ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn agent_update_soul_refreshes_soul_guard_hash() {
    let tmp = TempDir::new();
    let home = tmp.path();
    make_minimal_agent(home, "tester");
    let agents_dir = home.join("agents");
    // Seed an initial SOUL.md so the test exercises the "update" branch
    // (not the create-from-nothing branch).
    std::fs::write(agents_dir.join("tester").join("SOUL.md"), "initial soul\n").unwrap();

    let new_content = "## Identity\n\nI am the test agent.\n";
    let params = serde_json::json!({
        "agent_id": "tester",
        "content": new_content,
    });

    let result = handle_agent_update_soul(&params, home).await;
    assert!(
        result.get("isError").and_then(|v| v.as_bool()) != Some(true),
        "agent_update_soul should succeed; got: {result}"
    );

    // The stored hash MUST equal the SHA-256 of the new content.
    // Without the soul_guard::accept_soul_change call, the stored hash
    // would still be the hash of "initial soul\n".
    let agent_dir = agents_dir.join("tester");
    let stored = duduclaw_security::soul_guard::read_stored_hash(&agent_dir)
        .expect("stored hash must exist after agent_update_soul");
    let expected = duduclaw_security::soul_guard::fingerprint_soul(&agent_dir)
        .expect("SOUL.md must exist");
    assert_eq!(
        stored, expected,
        "stored soul hash must match SOUL.md fingerprint after update"
    );
}

#[tokio::test]
async fn agent_update_soul_appends_audit_row() {
    let tmp = TempDir::new();
    let home = tmp.path();
    make_minimal_agent(home, "tester");

    let params = serde_json::json!({
        "agent_id": "tester",
        "content": "## Identity\n\nNew soul.\n",
    });
    let _ = handle_agent_update_soul(&params, home).await;

    let rows = read_audit_rows(home, "agent_update_soul");
    assert_eq!(rows.len(), 1, "exactly one audit row expected");
    let row = &rows[0];
    assert_eq!(row.get("agent_id").and_then(|v| v.as_str()), Some("tester"));
    assert_eq!(row.get("success").and_then(|v| v.as_bool()), Some(true));
    let summary = row
        .get("params_summary")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(
        summary.contains("ok:") && summary.contains("size="),
        "audit summary should include hash + size; got: {summary}"
    );
}

#[tokio::test]
async fn agent_update_soul_audits_validation_rejections() {
    let tmp = TempDir::new();
    let home = tmp.path();

    // Empty agent_id → rejected.
    let params_no_id = serde_json::json!({ "agent_id": "", "content": "x" });
    let r1 = handle_agent_update_soul(&params_no_id, home).await;
    assert_eq!(r1.get("isError").and_then(|v| v.as_bool()), Some(true));

    // Nonexistent agent_id → rejected (after agent_id validation passes).
    let params_ghost = serde_json::json!({ "agent_id": "ghost", "content": "x" });
    let r2 = handle_agent_update_soul(&params_ghost, home).await;
    assert_eq!(r2.get("isError").and_then(|v| v.as_bool()), Some(true));

    // Empty content with valid agent → also rejected.
    make_minimal_agent(home, "real");
    let params_empty = serde_json::json!({ "agent_id": "real", "content": "" });
    let r3 = handle_agent_update_soul(&params_empty, home).await;
    assert_eq!(r3.get("isError").and_then(|v| v.as_bool()), Some(true));

    let rows = read_audit_rows(home, "agent_update_soul");
    assert_eq!(rows.len(), 3, "all three rejections should be audited");
    for row in &rows {
        assert_eq!(row.get("success").and_then(|v| v.as_bool()), Some(false));
        let summary = row
            .get("params_summary")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert!(
            summary.starts_with("REJECTED:"),
            "rejection audit must start with REJECTED:; got: {summary}"
        );
    }
}

/// A full `agent.toml` the typed loader reads (rename tests need `display_name`).
fn write_named_agent(home: &std::path::Path, name: &str, display: &str) {
    let dir = home.join("agents").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("agent.toml"),
        format!(
            "[agent]\nname = \"{name}\"\ndisplay_name = \"{display}\"\nrole = \"specialist\"\nstatus = \"active\"\ntrigger = \"@{display}\"\nreports_to = \"\"\nicon = \"\"\n\n[model]\npreferred = \"claude-sonnet-4-6\"\nfallback = \"claude-haiku-4-5\"\naccount_pool = []\n\n[container]\ntimeout_ms = 1000\nmax_concurrent = 1\nreadonly_project = true\n\n[heartbeat]\nenabled = false\ninterval_seconds = 300\nmax_concurrent_runs = 1\ncron = \"\"\n\n[budget]\nmonthly_limit_cents = 0\nwarn_threshold_percent = 80\nhard_stop = false\n\n[permissions]\ncan_create_agents = true\ncan_send_cross_agent = true\ncan_modify_own_skills = true\ncan_modify_own_soul = false\ncan_schedule_tasks = true\nallowed_channels = []\n\n[evolution]\ngvu_enabled = true\n"
        ),
    )
    .unwrap();
}

/// P1: the rename sync moves an approved SOUL baseline along, and leaves a
/// SOUL that was already changed outside the system flagged.
#[tokio::test]
async fn agent_update_rename_keeps_baseline_and_drift() {
    use duduclaw_security::soul_guard::{DocDrift, ProtectedDoc, accept_doc_change, doc_drift};
    let tmp = TempDir::new();
    let home = tmp.path();
    write_named_agent(home, "renamer", "Alice");
    // The operator here is not the employee's manager; open delegation so
    // the rename itself is what is tested.
    std::fs::write(home.join("config.toml"), "[delegation]\npolicy = \"open\"\n").unwrap();
    let dir = home.join("agents").join("renamer");
    std::fs::write(dir.join("SOUL.md"), "I am Alice.\n").unwrap();
    accept_doc_change(ProtectedDoc::Soul, "renamer", &dir).unwrap();
    let op = crate::mcp::record_authz::RecordActor::Operator("dudu");
    let r = handle_agent_update(&serde_json::json!({ "agent_id": "renamer", "display_name": "Alicia" }), home, op).await;
    assert!(r.get("isError").and_then(|v| v.as_bool()) != Some(true), "{r}");
    assert_eq!(std::fs::read_to_string(dir.join("SOUL.md")).unwrap(), "I am Alicia.\n");
    assert_eq!(doc_drift(ProtectedDoc::Soul, &dir), DocDrift::Intact, "an approved SOUL stays approved");
    assert!(dir.join("SOUL.md.lock").exists(), "written under the shared file lock");

    std::fs::write(dir.join("SOUL.md"), "I am Alicia, edited outside.\n").unwrap();
    let r = handle_agent_update(&serde_json::json!({ "agent_id": "renamer", "display_name": "Ally" }), home, op).await;
    assert!(r.get("isError").and_then(|v| v.as_bool()) != Some(true), "{r}");
    assert_eq!(std::fs::read_to_string(dir.join("SOUL.md")).unwrap(), "I am Ally, edited outside.\n");
    assert_eq!(doc_drift(ProtectedDoc::Soul, &dir), DocDrift::Drifted, "drift is not blessed by a rename");
}

#[tokio::test]
async fn agent_update_soul_writes_under_the_shared_lock() {
    let tmp = TempDir::new();
    let home = tmp.path();
    make_minimal_agent(home, "locker");
    let soul = home.join("agents").join("locker").join("SOUL.md");
    std::fs::write(&soul, "v1\n").unwrap();
    // Hold the lock from another thread; the tool must wait for it.
    let (tx, rx) = std::sync::mpsc::channel();
    let held = soul.clone();
    let t = std::thread::spawn(move || {
        duduclaw_core::with_file_lock(&held, || {
            tx.send(()).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(400));
            std::fs::write(&held, "written by the lock holder\n")
        })
        .unwrap();
    });
    rx.recv().unwrap();
    let started = std::time::Instant::now();
    let r = handle_agent_update_soul(&serde_json::json!({ "agent_id": "locker", "content": "v2\n" }), home).await;
    assert!(r.get("isError").and_then(|v| v.as_bool()) != Some(true), "{r}");
    assert!(started.elapsed() >= std::time::Duration::from_millis(250), "did not wait for the lock");
    t.join().unwrap();
    assert_eq!(std::fs::read_to_string(&soul).unwrap(), "v2\n");
    let text = r["content"][0]["text"].as_str().unwrap();
    let old = <sha2::Sha256 as sha2::Digest>::digest(b"written by the lock holder\n");
    assert!(text.contains(&format!("{old:x}")), "the old hash is read inside the lock: {text}");
}
