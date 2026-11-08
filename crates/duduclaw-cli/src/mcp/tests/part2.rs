use super::*;
use crate::mcp::caller_shims::handle_agent_update;

/// `hierarchy` drops the department shortcut but keeps the ancestor chain.
#[tokio::test]
async fn delegation_hierarchy_policy_drops_department_shortcut() {
    let tmp = delegation_home();
    let home = tmp.path();
    fs::write(
        home.join("config.toml"),
        "[delegation]\npolicy = \"hierarchy\"\n",
    )
    .unwrap();
    assert!(
        check_delegation_allowed(home, "ceo", "sales-rep", "t")
            .await
            .is_ok()
    );
    assert!(
        check_delegation_allowed(home, "sales-rep", "sales-rep2", "t")
            .await
            .is_err()
    );
}

/// WP21 T11: `check_delegation_allowed` must honor `[delegation] allow`
/// (§2.2), not just `policy` — the front door and the C1 gate must agree.
#[tokio::test]
async fn delegation_whitelist_pair_allows_cross_department_leads() {
    let tmp = delegation_home();
    let home = tmp.path();
    fs::write(
        home.join("config.toml"),
        "[delegation]\npolicy = \"department\"\nallow = [[\"sales-lead\", \"mkt-lead\"]]\n",
    )
    .unwrap();

    assert!(
        check_delegation_allowed(home, "sales-lead", "mkt-lead", "t")
            .await
            .is_ok()
    );
    assert!(
        check_delegation_allowed(home, "mkt-lead", "sales-lead", "t")
            .await
            .is_ok()
    );
    // Their reps are not part of the whitelisted pair.
    assert!(
        check_delegation_allowed(home, "sales-rep", "mkt-lead", "t")
            .await
            .is_err()
    );

    // Also respected under `hierarchy`, per design doc §2.1 rule 5b.
    fs::write(
        home.join("config.toml"),
        "[delegation]\npolicy = \"hierarchy\"\nallow = [[\"sales-lead\", \"mkt-lead\"]]\n",
    )
    .unwrap();
    assert!(
        check_delegation_allowed(home, "sales-lead", "mkt-lead", "t")
            .await
            .is_ok()
    );
    assert!(
        check_delegation_allowed(home, "sales-rep", "sales-rep2", "t")
            .await
            .is_err()
    );
}

/// The snapshot must carry the whole ancestor chain, not just direct
/// parents — otherwise skip-level command silently reverts to the old
/// direct-only behaviour.
#[tokio::test]
async fn org_snapshot_covers_ancestor_chains_and_departments() {
    let tmp = delegation_home();
    let view = org_snapshot(tmp.path(), &["sales-rep", "mkt-rep"]).await;
    assert!(duduclaw_core::is_org_ancestor(&view, "ceo", "sales-rep"));
    assert!(duduclaw_core::is_org_ancestor(&view, "ceo", "mkt-rep"));
    assert_eq!(
        duduclaw_core::OrgView::department(&view, "sales-rep").as_deref(),
        Some("業務")
    );
    // Untouched branches are not read into the snapshot.
    assert!(duduclaw_core::OrgView::reports_to(&view, "writer").is_none());
}

// ── WP22 T1 — organisational authority lives in `<home>/org.toml` ───────

/// The point of the whole task: once an agent has a record in the store,
/// rewriting its own `agent.toml` must not move it in the org tree.
///
/// `sales-rep` and `mkt-rep` are in different departments with no ancestor
/// relation, so the department policy denies the pair. The tamper below
/// claims membership of 行銷 *and* a parent inside the marketing branch —
/// either would have been enough to flip the decision pre-WP22.
#[tokio::test]
async fn org_store_beats_a_tampered_agent_toml() {
    let tmp = delegation_home();
    let home = tmp.path();
    assert!(
        check_delegation_allowed(home, "sales-rep", "mkt-rep", "t")
            .await
            .is_err()
    );

    duduclaw_core::org_store::seed_if_absent(home).unwrap();
    tamper_agent_toml(home, "sales-rep", "mkt-lead", "行銷");

    assert!(
        check_delegation_allowed(home, "sales-rep", "mkt-rep", "t")
            .await
            .is_err(),
        "tampering with agent.toml must not grant cross-department reach"
    );
    // The placement gate reads the same authority: sales-rep still may not
    // hang a new agent under the CEO just because its file says so.
    tamper_agent_toml(home, "sales-rep", "ceo", "業務");
    assert!(
        check_org_placement_allowed(home, "sales-rep", "ceo", "建立 AI 員工")
            .await
            .is_err(),
        "self-promotion via agent.toml must not pass the C4 gate"
    );
    // …and `list_agents` visibility agrees with the delegation decision.
    let listed = handle_list_agents(&serde_json::json!({}), home, "sales-rep").await;
    let text = listed["content"][0]["text"].as_str().unwrap();
    assert!(
        !text.contains("mkt-rep"),
        "tamper must not widen visibility: {text}"
    );
}

/// The compatibility half of the authority rule: an agent the store has
/// **no** record for keeps resolving from its `agent.toml`, byte-for-byte
/// as before WP22. This is what keeps every pre-existing fixture (and
/// every un-migrated install) working.
#[tokio::test]
async fn agents_without_a_store_entry_still_resolve_from_agent_toml() {
    let tmp = delegation_home();
    let home = tmp.path();

    // Model the realistic shape: the store was bootstrapped, and the
    // marketing branch was hand-created afterwards (so it never got a
    // record) — exactly the un-migrated / hand-built case the fallback
    // exists for.
    duduclaw_core::org_store::seed_if_absent(home).unwrap();
    for stray in ["mkt-lead", "mkt-rep"] {
        duduclaw_core::org_store::with_org_lock(home, |org| duduclaw_core::org_store::remove(org, stray)).unwrap();
    }

    // mkt-lead / mkt-rep have no records — the marketing branch must still
    // resolve, from the files.
    assert!(
        check_delegation_allowed(home, "mkt-lead", "mkt-rep", "t")
            .await
            .is_ok()
    );
    // And a file edit on an unrecorded agent still takes effect (the
    // documented fallback, not a bug).
    tamper_agent_toml(home, "mkt-rep", "mkt-lead", "業務");
    assert!(
        check_delegation_allowed(home, "mkt-rep", "sales-rep2", "t")
            .await
            .is_ok()
    );
}

/// Gated writers keep both copies in step, so `duduclaw doctor` reports no
/// drift right after a create / re-parent / remove.
#[tokio::test]
async fn gated_writers_keep_store_and_mirror_in_sync() {
    let tmp = delegation_home();
    let home = tmp.path();
    duduclaw_core::org_store::seed_if_absent(home).unwrap();

    // create_agent
    let out = handle_create_agent(
        &serde_json::json!({
            "name": "sales-intern",
            "display_name": "業務實習生",
            "reports_to": "sales-rep",
        }),
        home,
        "sales-lead",
    )
    .await;
    assert!(out.get("isError").is_none(), "{out}");
    let store = duduclaw_core::org_store::load(home);
    assert_eq!(store.get("sales-intern").unwrap().reports_to, "sales-rep");
    assert!(duduclaw_core::org_store::detect_drift(home).is_empty());

    // agent_update re-parent
    let out = handle_agent_update(
        &serde_json::json!({ "agent_id": "sales-intern", "reports_to": "sales-rep2" }),
        home,
        "sales-lead",
    )
    .await;
    assert_ne!(out["isError"], true, "{out}");
    assert_eq!(
        duduclaw_core::org_store::load(home)
            .get("sales-intern")
            .unwrap()
            .reports_to,
        "sales-rep2"
    );
    assert!(duduclaw_core::org_store::detect_drift(home).is_empty());

    // agent_remove drops the record so a later agent of the same name
    // cannot inherit this one's authority.
    let out = handle_agent_remove(
        &serde_json::json!({ "agent_id": "sales-intern" }),
        home,
        "sales-lead",
    )
    .await;
    assert_ne!(out["isError"], true, "{out}");
    assert!(
        duduclaw_core::org_store::load(home)
            .get("sales-intern")
            .is_none()
    );
}

/// RM-027 round 6: an MCP `agent_update` re-parent holds the global org lock
/// while it writes `agent.toml`. A stuck writer on that file used to pin the
/// org lock forever; the wait is now bounded — the update fails with a clear
/// `lock_wait_timeout`, writes nothing (neither the mirror nor the
/// authority), releases the org lock, and succeeds once the writer is gone.
#[tokio::test]
async fn cli_agent_update_bounded_lock_wait() {
    use crate::mcp::agent_update::org_locked_wait_override;
    let tmp = delegation_home();
    let home = tmp.path();
    duduclaw_core::org_store::seed_if_absent(home).unwrap();
    org_locked_wait_override::set(home, std::time::Duration::from_millis(300));
    let toml_path = home.join("agents/sales-rep/agent.toml");
    let before = fs::read_to_string(&toml_path).unwrap();
    // A stuck writer holds sales-rep's agent.toml advisory lock.
    let stuck = duduclaw_core::fs_lock::lock_exclusive(&toml_path).unwrap();

    let started = std::time::Instant::now();
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        handle_agent_update(
            &serde_json::json!({ "agent_id": "sales-rep", "reports_to": "sales-rep2" }),
            home,
            "sales-lead",
        ),
    )
    .await
    .expect("agent_update must not wait unboundedly for the stuck agent.toml writer");
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    assert_eq!(out["isError"], true, "{out}");
    let text = update_text(&out);
    assert!(text.contains("lock_wait_timeout"), "{text}");
    // Nothing written: the mirror and the authority are unchanged.
    assert_eq!(fs::read_to_string(&toml_path).unwrap(), before);
    assert_eq!(
        duduclaw_core::org_store::load(home).get("sales-rep").unwrap().reports_to,
        "sales-lead"
    );
    // The org lock was released: another thread takes it at once.
    let h = home.to_path_buf();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(duduclaw_core::org_store::lock_org_mutations(&h).map(drop).is_ok());
    });
    assert!(rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap(), "the org lock must be free");

    // The writer is gone: the same update goes through.
    drop(stuck);
    let out = handle_agent_update(
        &serde_json::json!({ "agent_id": "sales-rep", "reports_to": "sales-rep2" }),
        home,
        "sales-lead",
    )
    .await;
    assert_ne!(out["isError"], true, "{out}");
    assert_eq!(
        duduclaw_core::org_store::load(home).get("sales-rep").unwrap().reports_to,
        "sales-rep2"
    );
    assert!(duduclaw_core::org_store::detect_drift(home).is_empty());
    org_locked_wait_override::clear(home);
}

/// RM-027 round 4, finding 1: `create_agent` takes the org lock at its
/// start — while another writer holds it the new employee is not visible
/// (no directory, no staging dir, no row) — and publishes directory and
/// authority row together once it gets the lock.
#[tokio::test]
async fn create_agent_waits_for_the_org_lock_before_anything_is_visible() {
    let tmp = delegation_home();
    let home = tmp.path().to_path_buf();
    duduclaw_core::org_store::seed_if_absent(&home).unwrap();
    let held = duduclaw_core::org_store::lock_org_mutations(&home).unwrap();
    let path = home.clone();
    let checker = async move {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        assert!(!path.join("agents/late-intern").exists());
        assert!(fs::read_dir(path.join("agents")).unwrap().filter_map(|e| e.ok()).all(|e| {
            !e.file_name().to_string_lossy().starts_with(duduclaw_core::org_store::AGENT_STAGING_PREFIX)
        }));
        assert!(duduclaw_core::org_store::load(&path).get("late-intern").is_none());
        drop(held);
    };
    let params = serde_json::json!({
        "name": "late-intern",
        "display_name": "晚到的實習生",
        "reports_to": "sales-rep",
    });
    let (out, ()) = tokio::join!(handle_create_agent(&params, &home, "sales-lead"), checker);
    assert!(out.get("isError").is_none(), "{out}");
    assert!(home.join("agents/late-intern/agent.toml").is_file());
    assert_eq!(duduclaw_core::org_store::load(&home).get("late-intern").unwrap().reports_to, "sales-rep");
}

/// An authority refusal (strict parser, cycle) fails `create_agent` and no
/// directory remains — the old "warn and fall back to the mirror" made the
/// employee govern itself.
#[tokio::test]
async fn create_agent_fails_closed_when_the_authority_refuses() {
    // Corrupt store.
    let tmp = delegation_home();
    let home = tmp.path();
    duduclaw_core::org_store::seed_if_absent(home).unwrap();
    let store = duduclaw_core::org_store::org_store_path(home);
    fs::write(&store, "schema = \"x\"\n").unwrap();
    let out = handle_create_agent(
        &serde_json::json!({ "name": "lost-intern", "display_name": "x", "reports_to": "sales-rep" }),
        home,
        "sales-lead",
    )
    .await;
    assert_eq!(out["isError"], true, "{out}");
    assert!(!home.join("agents/lost-intern").exists());
    assert!(fs::read_dir(home.join("agents")).unwrap().filter_map(|e| e.ok()).all(|e| {
        !e.file_name().to_string_lossy().starts_with(duduclaw_core::org_store::AGENT_STAGING_PREFIX)
    }));
    assert_eq!(fs::read_to_string(&store).unwrap(), "schema = \"x\"\n");

    // A placement that closes a loop through stale authority rows
    // (sales-rep → loop-intern → sales-lead) is refused.
    let tmp = delegation_home();
    let home = tmp.path();
    duduclaw_core::org_store::seed_if_absent(home).unwrap();
    duduclaw_core::org_store::with_org_lock(home, |org| {
        duduclaw_core::org_store::upsert(org, "loop-intern", duduclaw_core::OrgEntry::new("sales-lead", ""))?;
        duduclaw_core::org_store::upsert(org, "sales-rep", duduclaw_core::OrgEntry::new("loop-intern", "業務"))
    })
    .unwrap();
    let out = handle_create_agent(
        &serde_json::json!({ "name": "loop-intern", "display_name": "x", "reports_to": "sales-rep" }),
        home,
        "sales-lead",
    )
    .await;
    assert_eq!(out["isError"], true, "{out}");
    assert!(!home.join("agents/loop-intern").exists());
    assert_eq!(
        duduclaw_core::org_store::load(home).get("loop-intern").unwrap().reports_to,
        "sales-lead",
        "the refused create must not rewrite the authority"
    );
}

/// A rejected `agent_update` must leave the authority untouched — the
/// store write sits behind the same C4 gate as the file write.
#[tokio::test]
async fn denied_reparent_does_not_touch_the_store() {
    let tmp = delegation_home();
    let home = tmp.path();
    duduclaw_core::org_store::seed_if_absent(home).unwrap();

    let res = handle_agent_update(
        &serde_json::json!({ "agent_id": "sales-rep", "reports_to": "ceo" }),
        home,
        "sales-rep",
    )
    .await;
    assert_eq!(res["isError"], true, "{res}");
    assert_eq!(
        duduclaw_core::org_store::load(home)
            .get("sales-rep")
            .unwrap()
            .reports_to,
        "sales-lead"
    );
}

/// Cycle detection must read the authority, not the mirror: a chain that
/// is acyclic in the (tampered) files but cyclic in the store has to be
/// caught, or the ancestor walk can loop against real data.
#[tokio::test]
async fn cycle_detection_follows_the_authoritative_chain() {
    let tmp = delegation_home();
    let home = tmp.path();
    duduclaw_core::org_store::seed_if_absent(home).unwrap();
    // Files claim sales-lead is a root; the store still says ceo.
    tamper_agent_toml(home, "sales-lead", "", "業務");
    let err = validate_reports_to(home, "ceo", "sales-rep").await;
    assert!(err.is_err(), "ceo → sales-rep closes a cycle via the store");
}

/// Unknown agents can prove no relation, so they fail closed.
#[tokio::test]
async fn delegation_unknown_agent_fails_closed() {
    let tmp = delegation_home();
    let home = tmp.path();
    assert!(
        check_delegation_allowed(home, "sales-lead", "ghost", "t")
            .await
            .is_err()
    );
    assert!(
        check_delegation_allowed(home, "ghost", "sales-rep", "t")
            .await
            .is_err()
    );
}

// ── WP21 T6 (§2.5): read-side visibility on list_agents / agent_status ──

#[tokio::test]
async fn list_agents_department_policy_filters_to_visible_set() {
    let tmp = delegation_home();
    let names = listed_names(
        &handle_list_agents(&serde_json::json!({}), tmp.path(), "sales-rep").await,
    );

    for expected in ["sales-rep", "sales-lead", "ceo", "sales-rep2"] {
        assert!(
            names.contains(&expected.to_string()),
            "{expected} must be visible: {names:?}"
        );
    }
    for hidden in ["mkt-lead", "mkt-rep", "researcher", "writer"] {
        assert!(
            !names.contains(&hidden.to_string()),
            "{hidden} must stay hidden: {names:?}"
        );
    }
}

#[tokio::test]
async fn list_agents_hierarchy_policy_drops_department_shortcut() {
    let tmp = delegation_home();
    let home = tmp.path();
    fs::write(
        home.join("config.toml"),
        "[delegation]\npolicy = \"hierarchy\"\n",
    )
    .unwrap();
    let names =
        listed_names(&handle_list_agents(&serde_json::json!({}), home, "sales-rep").await);

    for expected in ["sales-rep", "sales-lead", "ceo"] {
        assert!(
            names.contains(&expected.to_string()),
            "{expected} must stay visible: {names:?}"
        );
    }
    // The department shortcut is gone under `hierarchy` — same-department
    // peer sales-rep2 is no longer an ancestor/descendant of sales-rep.
    assert!(
        !names.contains(&"sales-rep2".to_string()),
        "peer must be hidden: {names:?}"
    );
}

#[tokio::test]
async fn list_agents_open_policy_sees_everyone() {
    let tmp = delegation_home();
    let home = tmp.path();
    fs::write(
        home.join("config.toml"),
        "[delegation]\npolicy = \"open\"\n",
    )
    .unwrap();
    let names =
        listed_names(&handle_list_agents(&serde_json::json!({}), home, "sales-rep").await);
    for id in [
        "ceo",
        "sales-lead",
        "sales-rep",
        "sales-rep2",
        "mkt-lead",
        "mkt-rep",
        "researcher",
        "writer",
    ] {
        assert!(
            names.contains(&id.to_string()),
            "{id} must be visible under open: {names:?}"
        );
    }
}

#[tokio::test]
async fn list_agents_system_sender_sees_everyone() {
    let tmp = delegation_home();
    // Default `department` policy, but a system/human-interface caller
    // (dashboard/heartbeat/...) is not an agent in the org tree.
    let names = listed_names(
        &handle_list_agents(&serde_json::json!({}), tmp.path(), "dashboard").await,
    );
    for id in [
        "ceo",
        "sales-lead",
        "sales-rep",
        "sales-rep2",
        "mkt-lead",
        "mkt-rep",
        "researcher",
        "writer",
    ] {
        assert!(
            names.contains(&id.to_string()),
            "{id} must be visible to a system sender: {names:?}"
        );
    }
}

#[tokio::test]
async fn list_agents_whitelist_pair_extends_visibility() {
    let tmp = delegation_home();
    let home = tmp.path();
    fs::write(
        home.join("config.toml"),
        "[delegation]\npolicy = \"department\"\nallow = [[\"sales-lead\", \"mkt-lead\"]]\n",
    )
    .unwrap();

    // The whitelisted pair can see each other...
    let sales_lead_view =
        listed_names(&handle_list_agents(&serde_json::json!({}), home, "sales-lead").await);
    assert!(
        sales_lead_view.contains(&"mkt-lead".to_string()),
        "{sales_lead_view:?}"
    );

    // ...but the whitelist does not transitively open the whole other team.
    let sales_rep_view =
        listed_names(&handle_list_agents(&serde_json::json!({}), home, "sales-rep").await);
    assert!(
        !sales_rep_view.contains(&"mkt-lead".to_string()),
        "{sales_rep_view:?}"
    );
    assert!(
        !sales_rep_view.contains(&"mkt-rep".to_string()),
        "{sales_rep_view:?}"
    );
}

#[tokio::test]
async fn agent_status_denies_invisible_agent_same_as_unknown_agent() {
    let tmp = delegation_home();
    let home = tmp.path();

    // mkt-rep exists but is invisible to sales-rep under the default
    // department policy (different department, no ancestor relation).
    let invisible = handle_agent_status(
        &serde_json::json!({ "agent_id": "mkt-rep" }),
        home,
        "sales-rep",
    )
    .await;
    assert!(invisible["isError"].as_bool().unwrap_or(false));

    // A genuinely nonexistent id.
    let missing = handle_agent_status(
        &serde_json::json!({ "agent_id": "does-not-exist" }),
        home,
        "sales-rep",
    )
    .await;
    assert!(missing["isError"].as_bool().unwrap_or(false));

    // Same wording either way — cannot be used to probe which ids exist
    // (the agent id itself necessarily differs since the caller supplied
    // it, but the reason given never says "not found" vs "not visible").
    let suffix = "not found or not visible";
    assert!(
        invisible["content"][0]["text"]
            .as_str()
            .unwrap()
            .ends_with(suffix)
    );
    assert!(
        missing["content"][0]["text"]
            .as_str()
            .unwrap()
            .ends_with(suffix)
    );
}

#[tokio::test]
async fn agent_status_allows_self_ancestor_descendant_and_department_peer() {
    let tmp = delegation_home();
    let home = tmp.path();

    for target in ["sales-rep", "ceo", "sales-lead", "sales-rep2"] {
        let res = handle_agent_status(
            &serde_json::json!({ "agent_id": target }),
            home,
            "sales-rep",
        )
        .await;
        assert!(
            !res["isError"].as_bool().unwrap_or(false),
            "{target} must be visible: {res}"
        );
    }
}

#[tokio::test]
async fn agent_status_open_policy_and_system_sender_bypass_the_gate() {
    let tmp = delegation_home();
    let home = tmp.path();

    // System sender sees a department stranger under the default policy.
    let res = handle_agent_status(
        &serde_json::json!({ "agent_id": "mkt-rep" }),
        home,
        "dashboard",
    )
    .await;
    assert!(!res["isError"].as_bool().unwrap_or(false), "{res}");

    // Escape hatch.
    fs::write(
        home.join("config.toml"),
        "[delegation]\npolicy = \"open\"\n",
    )
    .unwrap();
    let res = handle_agent_status(
        &serde_json::json!({ "agent_id": "mkt-rep" }),
        home,
        "sales-rep",
    )
    .await;
    assert!(!res["isError"].as_bool().unwrap_or(false), "{res}");
}

// ── WP21 C4: org-placement gate (self-service escalation) ────────

#[tokio::test]
async fn org_placement_rejects_attaching_outside_caller_subtree() {
    let tmp = delegation_home();
    let home = tmp.path();

    // A rep hanging a new agent under the CEO would mint itself a peer of
    // its own manager — the escalation C4 exists to close.
    let err = check_org_placement_allowed(home, "sales-rep", "ceo", "建立 AI 員工")
        .await
        .expect_err("attaching under the CEO must be denied");
    assert!(
        err.contains("只能將 AI 員工掛在自己或自己團隊之下"),
        "got: {err}"
    );
    assert!(err.contains("ceo"), "got: {err}");

    // Another team's node is equally off limits.
    assert!(
        check_org_placement_allowed(home, "sales-lead", "mkt-lead", "建立 AI 員工")
            .await
            .is_err()
    );
    // ...and so is detaching to the root (no manager at all).
    assert!(
        check_org_placement_allowed(home, "sales-rep", "", "建立 AI 員工")
            .await
            .is_err()
    );
    assert!(
        check_org_placement_allowed(home, "sales-rep", "none", "建立 AI 員工")
            .await
            .is_err()
    );

    let audit = fs::read_to_string(home.join("tool_calls.jsonl")).expect("audit row written");
    assert!(audit.contains("org_placement_denied"), "got: {audit}");
}

#[tokio::test]
async fn org_placement_allows_self_and_own_subtree() {
    let tmp = delegation_home();
    let home = tmp.path();
    // Under yourself.
    assert!(
        check_org_placement_allowed(home, "sales-rep", "sales-rep", "建立 AI 員工")
            .await
            .is_ok()
    );
    // Under a node you already command, at any depth.
    assert!(
        check_org_placement_allowed(home, "ceo", "sales-rep", "建立 AI 員工")
            .await
            .is_ok()
    );
    assert!(
        check_org_placement_allowed(home, "sales-lead", "sales-rep", "建立 AI 員工")
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn org_placement_skipped_for_open_policy_and_system_senders() {
    let tmp = delegation_home();
    let home = tmp.path();

    // Human / system interfaces are not agents in the org tree.
    for sender in ["dashboard", "webhook", "cron"] {
        assert!(
            check_org_placement_allowed(home, sender, "ceo", "建立 AI 員工")
                .await
                .is_ok(),
            "{sender} must not be restricted"
        );
    }
    // ...but an agent-looking lookalike is still an agent (no substring pass).
    assert!(
        check_org_placement_allowed(home, "dashboard-x", "ceo", "建立 AI 員工")
            .await
            .is_err()
    );

    // The escape hatch turns the whole gate off.
    fs::write(
        home.join("config.toml"),
        "[delegation]\npolicy = \"open\"\n",
    )
    .unwrap();
    assert!(
        check_org_placement_allowed(home, "sales-rep", "ceo", "建立 AI 員工")
            .await
            .is_ok()
    );
}
