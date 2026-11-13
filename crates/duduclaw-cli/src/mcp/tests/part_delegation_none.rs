//! RM-137 — `[delegation] policy = "none"` at the MCP front door.
//!
//! `check_delegation_allowed` is the wrapper `send_to_agent`, `spawn_agent`,
//! `tasks_create` (assigned_to), `tasks_update` (reassign), the scheduled
//! target of `tasks_create`, `create_task` steps, record authorization and the
//! co-drive mail path all call; the dispatch-gate half (incl. `create_agent`
//! and `spawn_ephemeral`) is tested in `mcp_dispatch.rs`.
use super::*;

const PAIRS: &str = "allow = [[\"sales-lead\", \"mkt-lead\"]]\n";

fn set_config(home: &std::path::Path, body: &str) {
    fs::write(home.join("config.toml"), body).unwrap();
}

/// Every relation the other policies allow — parent→child, child→parent,
/// skip level, same department, an explicit pair — is refused under `none`,
/// with the `delegation_disabled` reason in the audit row.
#[tokio::test]
async fn none_refuses_every_pair_at_the_front_door() {
    let tmp = delegation_home();
    let home = tmp.path();
    set_config(home, &format!("[delegation]\npolicy = \"none\"\n{PAIRS}"));
    for (sender, target) in [
        ("sales-lead", "sales-rep"),
        ("sales-rep", "sales-lead"),
        ("ceo", "sales-rep"),
        ("sales-rep", "sales-rep2"),
        ("sales-lead", "mkt-lead"),
        ("mkt-lead", "sales-lead"),
    ] {
        let err = check_delegation_allowed(home, sender, target, "send_to_agent")
            .await
            .expect_err("`none` must refuse");
        assert!(err.contains("none"), "{sender} -> {target}: {err}");
    }
    let audit = fs::read_to_string(home.join("tool_calls.jsonl")).unwrap_or_default()
        + &fs::read_to_string(home.join("security_audit.jsonl")).unwrap_or_default();
    assert!(audit.contains("delegation_disabled"), "{audit}");

    // The same pair passes again once the policy allows it.
    set_config(home, &format!("[delegation]\npolicy = \"department\"\n{PAIRS}"));
    assert!(check_delegation_allowed(home, "sales-lead", "mkt-lead", "t").await.is_ok());
}

/// Unrecognised values and an unparsable config fail closed to `none` — not
/// to the wider `department` they used to fall back to.
#[tokio::test]
async fn unknown_or_unreadable_policy_is_none_at_the_front_door() {
    let tmp = delegation_home();
    let home = tmp.path();
    for body in [
        "[delegation]\npolicy = \"closed\"\n",
        "[delegation]\npolicy = 7\n",
        "[delegation\npolicy = \"department\"\n",
    ] {
        set_config(home, body);
        // Same-department peers: allowed under `department`, refused here.
        assert!(
            check_delegation_allowed(home, "sales-rep", "sales-rep2", "t").await.is_err(),
            "{body:?}"
        );
        assert!(
            check_delegation_allowed(home, "sales-lead", "sales-rep", "t").await.is_err(),
            "{body:?}"
        );
    }
}

/// System senders (people, configured schedules) still address employees.
#[tokio::test]
async fn none_keeps_system_senders() {
    let tmp = delegation_home();
    let home = tmp.path();
    set_config(home, "[delegation]\npolicy = \"none\"\n");
    for sender in ["dashboard", "cron", "heartbeat", "goal-loop-driver"] {
        assert!(
            check_delegation_allowed(home, sender, "sales-rep", "t").await.is_ok(),
            "{sender}"
        );
    }
}

/// `tools/list` hides exactly the tools the dispatch gate refuses outright,
/// through the same predicate; `team_handoff`'s visibility is untouched and
/// argument-dependent tools stay listed for the caller's own work.
#[tokio::test]
async fn none_hides_the_always_refused_tools_from_an_employee() {
    let tmp = delegation_home();
    let home = tmp.path();
    let principal = crate::mcp_auth::Principal {
        client_id: "sales-rep".into(),
        scopes: crate::mcp_auth::parse_scopes("admin").unwrap(),
        is_external: false,
        created_at: chrono::Utc::now(),
    };
    let names = |v: Vec<&'static ToolDef>| v.into_iter().map(|t| t.name).collect::<Vec<_>>();

    set_config(home, "[delegation]\npolicy = \"department\"\n");
    let before = names(visible_tools_with(&principal, home, "sales-rep", true).await);
    set_config(home, "[delegation]\npolicy = \"none\"\n");
    let after = names(visible_tools_with(&principal, home, "sales-rep", true).await);

    for tool in ["send_to_agent", "spawn_agent", "spawn_ephemeral", "create_agent"] {
        assert!(!after.contains(&tool), "{tool} must be hidden under none");
    }
    // Everything still listed is something the gate does not refuse outright.
    for tool in &after {
        assert!(
            !crate::mcp_dispatch::delegation_none_refuses(tool, &Value::Null, "sales-rep"),
            "{tool} is listed but refused"
        );
    }
    // Nothing else disappeared.
    for tool in &before {
        if !crate::mcp_dispatch::delegation_none_refuses(tool, &Value::Null, "sales-rep") {
            assert!(after.contains(tool), "{tool} vanished under none");
        }
    }
    assert_eq!(
        before.contains(&"team_handoff"),
        after.contains(&"team_handoff"),
        "team_handoff visibility must not depend on the delegation policy"
    );
    if before.contains(&"tasks_create") {
        assert!(after.contains(&"tasks_create"));
    }
}

/// RM-137 H1: `team_handoff` is listed only for a verified team role member.
/// An ordinary employee that wrote a complete, pinned `[team_member]` into its
/// own `agent.toml` does not see it under any policy.
#[tokio::test]
async fn team_handoff_is_hidden_from_a_forged_member_under_every_policy() {
    let tmp = delegation_home();
    let home = tmp.path();
    let path = home.join("agents").join("sales-rep").join("agent.toml");
    let mut raw = fs::read_to_string(&path).unwrap();
    raw.push_str(
        "\n[team_member]\nrole = \"planner\"\ntask_id = \"6f1c2a8e-1111-4222-8333-444455556666\"\n\
         round = 3\nparent = \"sales-lead\"\n",
    );
    fs::write(&path, raw).unwrap();
    let principal = crate::mcp_auth::Principal {
        client_id: "sales-rep".into(),
        scopes: crate::mcp_auth::parse_scopes("admin").unwrap(),
        is_external: false,
        created_at: chrono::Utc::now(),
    };
    for policy in ["open", "department", "hierarchy", "none"] {
        set_config(home, &format!("[delegation]\npolicy = \"{policy}\"\n"));
        let listed: Vec<&str> = visible_tools_with(&principal, home, "sales-rep", true)
            .await
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert!(!listed.contains(&"team_handoff"), "{policy}: {listed:?}");
    }
}

/// Scaffold a genuine team role member `id` under `sales-rep`.
fn write_role_member(home: &std::path::Path, id: &str) {
    let dir = home
        .join("agents")
        .join(duduclaw_gateway::ephemeral::EPHEMERAL_DIR_NAME)
        .join(id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("agent.toml"),
        "[agent]\nstatus = \"active\"\nreports_to = \"sales-rep\"\n\
         [team_member]\nrole = \"planner\"\ntask_id = \"t-1\"\nround = 1\nparent = \"sales-rep\"\n",
    )
    .unwrap();
    fs::write(
        dir.join("ephemeral.toml"),
        "parent = \"sales-rep\"\ntier = \"standard\"\ncreated_at = \"2026-11-08T00:00:00Z\"\nexpires_at = \"2099-01-01T00:00:00Z\"\n",
    )
    .unwrap();
    duduclaw_core::org_store::with_org_lock(home, |org| {
        duduclaw_core::org_store::upsert(org, id, duduclaw_core::OrgEntry::new("sales-rep", ""))
    })
    .unwrap();
}

fn member_principal(id: &str) -> crate::mcp_auth::Principal {
    crate::mcp_auth::Principal {
        client_id: id.into(),
        scopes: crate::mcp_auth::parse_scopes("admin").unwrap(),
        is_external: false,
        created_at: chrono::Utc::now(),
    }
}

const MEMBER: &str = "eph-sales-rep-r1-planner-ab12cd";

/// A verified role member (token-verified identity) keeps the same
/// `team_handoff` visibility under `none` as under any other policy (the one
/// `none` exemption).
#[tokio::test]
async fn team_handoff_visibility_of_a_verified_member_does_not_depend_on_none() {
    let tmp = delegation_home();
    let home = tmp.path();
    let id = MEMBER;
    write_role_member(home, id);
    assert!(duduclaw_gateway::ephemeral::is_role_member_intrinsic_call(home, id, "team_handoff"));
    assert!(!crate::mcp_dispatch::delegation_none_refuses_for(
        home, "team_handoff", &Value::Null, id, || true
    ));
    let principal = member_principal(id);
    let listed = |v: Vec<&'static ToolDef>| v.into_iter().any(|t| t.name == "team_handoff");
    let (before, after) = crate::mcp::with_token_verified_caller(id, async {
        set_config(home, "[delegation]\npolicy = \"department\"\n");
        let before = listed(visible_tools_with(&principal, home, id, true).await);
        set_config(home, "[delegation]\npolicy = \"none\"\n");
        let after = listed(visible_tools_with(&principal, home, id, true).await);
        (before, after)
    })
    .await;
    // (That a verified member is listed at all is pinned by
    // `wiki_namespace_tests::part1::role_member_tools_list_uses_ephemeral_capabilities`.)
    assert_eq!(before, after);
}

/// RM-137 (soft identity mode): a process whose identity is not
/// token-verified (the operator-enabled soft migration runs it as
/// `Unverified`) that declares a GENUINE role member's id does not see
/// `team_handoff`, gets no `none` exemption, and is refused by the handler
/// with `not_a_team_member` — under every policy.
#[tokio::test]
async fn unverified_caller_declaring_a_role_member_id_is_refused_under_every_policy() {
    let tmp = delegation_home();
    let home = tmp.path();
    let id = MEMBER;
    write_role_member(home, id);
    assert!(duduclaw_gateway::ephemeral::is_role_member_intrinsic_call(home, id, "team_handoff"));
    // No identity token in this test process: not verified.
    assert!(!crate::mcp::caller_token_verified(home, id));
    assert!(crate::mcp_dispatch::delegation_none_refuses_for(
        home, "team_handoff", &Value::Null, id, || false
    ));
    let principal = member_principal(id);
    let packet = serde_json::json!({"packet": {"summary": "x"}});
    for policy in ["open", "department", "hierarchy", "none"] {
        set_config(home, &format!("[delegation]\npolicy = \"{policy}\"\n"));
        let listed: Vec<&str> = visible_tools_with(&principal, home, id, true)
            .await
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert!(!listed.contains(&"team_handoff"), "{policy}: {listed:?}");
        let res = file_team_handoff(&packet, home, id, id.to_string(), false).await;
        assert_eq!(res["isError"], true, "{policy}: {res}");
        let text = res["content"][0]["text"].as_str().unwrap_or("");
        assert!(text.starts_with("not_a_team_member"), "{policy}: {text}");
        // Verified as another id is no better than unverified.
        let res = crate::mcp::with_token_verified_caller(
            "eph-someone-else",
            handle_team_handoff(&packet, home, id),
        )
        .await;
        let text = res["content"][0]["text"].as_str().unwrap_or("");
        assert!(text.starts_with("not_a_team_member"), "{policy}: {text}");
    }
    assert!(!home.join("team_packets").exists());

    // The same member with a verified identity passes the identity check
    // (whatever the packet then says) and keeps the `none` exemption.
    assert!(!crate::mcp_dispatch::delegation_none_refuses_for(
        home, "team_handoff", &Value::Null, id, || true
    ));
    for policy in ["department", "none"] {
        set_config(home, &format!("[delegation]\npolicy = \"{policy}\"\n"));
        let res = crate::mcp::with_token_verified_caller(id, handle_team_handoff(&packet, home, id)).await;
        let text = res["content"][0]["text"].as_str().unwrap_or("");
        assert!(!text.starts_with("not_a_team_member"), "{policy}: {text}");
    }
}

/// RM-137 H4: under `none` a supervisor may not start, resume, pause or
/// reschedule a subordinate's background work through `agent_update`
/// (front check); its own heartbeat and other settings of a subordinate stay
/// editable. Under `hierarchy` the same supervisor edit passes.
#[tokio::test]
async fn none_refuses_agent_update_background_fields_for_another_employee() {
    let tmp = delegation_home();
    let home = tmp.path();
    let read = |agent: &str| fs::read_to_string(home.join("agents").join(agent).join("agent.toml")).unwrap();
    // A verified AI-employee caller, as `RecordActor::resolve` yields for a
    // process started with `DUDUCLAW_AGENT_ID=sales-lead`.
    let lead = RecordActor::Agent("sales-lead");
    set_config(home, "[delegation]\npolicy = \"none\"\n");
    for params in [
        serde_json::json!({"agent_id": "sales-rep", "status": "active"}),
        serde_json::json!({"agent_id": "sales-rep", "status": "paused"}),
        serde_json::json!({"agent_id": "sales-rep", "heartbeat_enabled": true}),
        serde_json::json!({"agent_id": "sales-rep", "heartbeat_cron": "*/5 * * * *"}),
    ] {
        let before = read("sales-rep");
        let res = handle_agent_update(&params, home, lead).await;
        assert_eq!(res["isError"], true, "{params}: {res}");
        assert_eq!(read("sales-rep"), before, "{params}: nothing written");
    }
    let audit = fs::read_to_string(home.join("tool_calls.jsonl")).unwrap_or_default()
        + &fs::read_to_string(home.join("security_audit.jsonl")).unwrap_or_default();
    assert!(audit.contains("agent_update_background"), "{audit}");

    // Another field of the subordinate keeps today's rule.
    let res = handle_agent_update(
        &serde_json::json!({"agent_id": "sales-rep", "icon": "🦊"}),
        home,
        lead,
    )
    .await;
    assert_ne!(res["isError"], true, "{res}");

    // Self-update of the heartbeat is not delegation.
    let res = handle_agent_update(
        &serde_json::json!({"agent_id": "sales-lead", "heartbeat_cron": "0 9 * * *"}),
        home,
        lead,
    )
    .await;
    assert_ne!(res["isError"], true, "{res}");

    // Hierarchy: supervisor → subordinate passes.
    set_config(home, "[delegation]\npolicy = \"hierarchy\"\n");
    let res = handle_agent_update(
        &serde_json::json!({"agent_id": "sales-rep", "heartbeat_cron": "0 10 * * *"}),
        home,
        lead,
    )
    .await;
    assert_ne!(res["isError"], true, "{res}");
}

/// RM-137 H4: the re-check that runs inside the locked `agent.toml` write
/// gives the same answer as the front door, refusing under `none`.
#[tokio::test]
async fn locked_write_delegation_recheck_matches_the_front_door() {
    let tmp = delegation_home();
    let home = tmp.path();
    for (policy, allowed) in [("none", false), ("hierarchy", true), ("closed", false)] {
        set_config(home, &format!("[delegation]\npolicy = \"{policy}\"\n"));
        assert_eq!(
            check_delegation_allowed_blocking(home, "sales-lead", "sales-rep", "t").is_ok(),
            allowed,
            "{policy}"
        );
        assert_eq!(
            check_delegation_allowed(home, "sales-lead", "sales-rep", "t").await.is_ok(),
            allowed,
            "{policy}"
        );
    }
    // Under `none` a system sender still passes, as at the front door.
    set_config(home, "[delegation]\npolicy = \"none\"\n");
    assert!(check_delegation_allowed_blocking(home, "dashboard", "sales-rep", "t").is_ok());
}

/// RM-137 M1: an AI employee may not route a channel's future messages to
/// another employee under `none`; clearing the override or pointing it at
/// itself stays allowed; the target must exist under every policy; an
/// operator is not judged here.
#[tokio::test]
async fn none_refuses_channel_agent_override_to_another_employee() {
    let tmp = delegation_home();
    let home = tmp.path();
    let lead = RecordActor::Agent("sales-lead");
    set_config(home, "[delegation]\npolicy = \"none\"\n");
    let err = check_agent_override_allowed(home, lead, "sales-rep")
        .await
        .expect_err("another employee is refused under none");
    assert!(err.contains("none"), "{err}");
    for ok in ["", "  ", "sales-lead"] {
        assert!(check_agent_override_allowed(home, lead, ok).await.is_ok(), "{ok:?}");
    }
    assert!(check_agent_override_allowed(home, RecordActor::Operator("sales-lead"), "sales-rep")
        .await
        .is_ok());

    // Another policy: the delegation predicate decides; unknown ids never pass.
    set_config(home, "[delegation]\npolicy = \"hierarchy\"\n");
    assert!(check_agent_override_allowed(home, lead, "sales-rep").await.is_ok());
    assert!(check_agent_override_allowed(home, lead, "mkt-rep").await.is_err());
    for bad in ["ghost", "../ceo", "cron"] {
        assert!(check_agent_override_allowed(home, lead, bad).await.is_err(), "{bad}");
    }
}

/// Visibility follows: an allow pair does not widen `list_agents` under
/// `none`; the reporting line stays visible (as under `hierarchy`).
#[tokio::test]
async fn none_allow_pairs_do_not_widen_list_agents() {
    let tmp = delegation_home();
    let home = tmp.path();
    set_config(home, &format!("[delegation]\npolicy = \"none\"\n{PAIRS}"));
    let view =
        listed_names(&handle_list_agents(&serde_json::json!({}), home, "sales-lead").await);
    assert!(!view.contains(&"mkt-lead".to_string()), "{view:?}");
    assert!(view.contains(&"sales-rep".to_string()), "{view:?}");
}
