use super::*;

#[tokio::test]
async fn org_subject_gate_protects_other_teams() {
    let tmp = delegation_home();
    let home = tmp.path();
    // You may reorganise yourself and your own people...
    assert!(
        check_org_subject_allowed(home, "sales-lead", "sales-lead", "調整組織從屬")
            .await
            .is_ok()
    );
    assert!(
        check_org_subject_allowed(home, "ceo", "mkt-rep", "調整組織從屬")
            .await
            .is_ok()
    );
    // ...but not somebody else's.
    let err = check_org_subject_allowed(home, "sales-lead", "mkt-rep", "調整組織從屬")
        .await
        .expect_err("another team's agent must be off limits");
    assert!(err.contains("不在你的團隊之內"), "got: {err}");
}

/// End-to-end through the real `agent_update` handler: both halves of the
/// C4 rule (who is moved, and where to) must hold, and a rejected update
/// must leave `agent.toml` untouched.
#[tokio::test]
async fn agent_update_reports_to_enforces_subtree_rule() {
    let tmp = delegation_home();
    let home = tmp.path();

    let read_parent = |agent: &str| {
        let path = home.join("agents").join(agent).join("agent.toml");
        let cfg: duduclaw_core::types::AgentConfig =
            toml::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        cfg.agent.reports_to
    };

    // Self-promotion: sales-rep re-parents itself under the CEO.
    let params = serde_json::json!({ "agent_id": "sales-rep", "reports_to": "ceo" });
    let res = handle_agent_update(&params, home, "sales-rep").await;
    assert_eq!(res["isError"], true, "{res}");
    assert_eq!(
        read_parent("sales-rep"),
        "sales-lead",
        "rejected update must not write"
    );

    // Reorganising another team.
    let params = serde_json::json!({ "agent_id": "mkt-rep", "reports_to": "sales-lead" });
    let res = handle_agent_update(&params, home, "sales-lead").await;
    assert_eq!(res["isError"], true, "{res}");
    assert!(
        res["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("不在你的團隊之內"),
        "{res}"
    );
    assert_eq!(read_parent("mkt-rep"), "mkt-lead");

    // Legitimate: sales-lead moves its own rep under its other rep.
    let params = serde_json::json!({ "agent_id": "sales-rep", "reports_to": "sales-rep2" });
    let res = handle_agent_update(&params, home, "sales-lead").await;
    assert_ne!(res["isError"], true, "{res}");
    assert_eq!(read_parent("sales-rep"), "sales-rep2");
}

/// WP21 debt ⑥ — the subtree gate covers **every** `agent_update` field, not
/// just `reports_to`. Pre-fix, an agent could leave another department's
/// node in place and simply terminate it / repoint its model instead.
#[tokio::test]
async fn agent_update_gates_non_org_fields_too() {
    let tmp = delegation_home();
    let home = tmp.path();

    let read_cfg = |agent: &str| -> duduclaw_core::types::AgentConfig {
        let path = home.join("agents").join(agent).join("agent.toml");
        toml::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    };
    let before_model = read_cfg("mkt-rep").model.preferred.clone();

    // Another department's model: denied, and nothing is written.
    let params = serde_json::json!({ "agent_id": "mkt-rep", "model": "claude-opus-4-6" });
    let res = handle_agent_update(&params, home, "sales-lead").await;
    assert_eq!(res["isError"], true, "{res}");
    assert!(
        res["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("不在你的團隊之內"),
        "{res}"
    );
    assert_eq!(read_cfg("mkt-rep").model.preferred, before_model);

    // Another department's status ("quiet kill"): denied.
    let params = serde_json::json!({ "agent_id": "mkt-lead", "status": "terminated" });
    let res = handle_agent_update(&params, home, "sales-rep").await;
    assert_eq!(res["isError"], true, "{res}");
    assert_eq!(
        read_cfg("mkt-lead").agent.status,
        duduclaw_core::types::AgentStatus::Active
    );

    // Own subordinate's status: allowed.
    let params = serde_json::json!({ "agent_id": "sales-rep", "status": "paused" });
    let res = handle_agent_update(&params, home, "sales-lead").await;
    assert_ne!(res["isError"], true, "{res}");
    assert_eq!(
        read_cfg("sales-rep").agent.status,
        duduclaw_core::types::AgentStatus::Paused
    );

    // Skip-level (grandparent) still counts as "your team".
    let params = serde_json::json!({ "agent_id": "sales-rep", "icon": "🧪" });
    let res = handle_agent_update(&params, home, "ceo").await;
    assert_ne!(res["isError"], true, "{res}");
    assert_eq!(read_cfg("sales-rep").agent.icon, "🧪");

    // Yourself: always allowed, no org relation needed.
    let params = serde_json::json!({ "agent_id": "mkt-rep", "display_name": "行銷小幫手" });
    let res = handle_agent_update(&params, home, "mkt-rep").await;
    assert_ne!(res["isError"], true, "{res}");
    assert_eq!(read_cfg("mkt-rep").agent.display_name, "行銷小幫手");

    // Same-department peers are NOT covered: the subtree rule is stricter
    // than `can_delegate` on purpose — being allowed to *ask* a peer for
    // help is not being allowed to *rewrite* their config.
    let params = serde_json::json!({ "agent_id": "sales-rep2", "icon": "💥" });
    let res = handle_agent_update(&params, home, "sales-rep").await;
    assert_eq!(res["isError"], true, "{res}");

    // System senders bypass the gate entirely.
    let params = serde_json::json!({ "agent_id": "mkt-rep", "icon": "🖥" });
    let res = handle_agent_update(&params, home, "dashboard").await;
    assert_ne!(res["isError"], true, "{res}");
    assert_eq!(read_cfg("mkt-rep").agent.icon, "🖥");

    // `open` policy escape hatch.
    fs::write(
        home.join("config.toml"),
        "[delegation]\npolicy = \"open\"\n",
    )
    .unwrap();
    let params = serde_json::json!({ "agent_id": "mkt-rep", "icon": "🔓" });
    let res = handle_agent_update(&params, home, "sales-rep").await;
    assert_ne!(res["isError"], true, "{res}");
    assert_eq!(read_cfg("mkt-rep").agent.icon, "🔓");
}

// ── WP-B: conversational database-source grants via agent_update ──────
//
// Chat route for 「把客戶 CRM 資料庫開給小美」. The grant list is
// deny-by-default, so these tests pin both directions: what a legitimate
// grant writes, and that an unknown id writes nothing at all.

/// `db_sources` REPLACES the list, and the success text names the result.
#[tokio::test]
async fn agent_update_db_sources_replaces_the_grant_list() {
    let tmp = db_grant_home();
    let home = tmp.path();

    let res = handle_agent_update(
        &serde_json::json!({ "agent_id": "sales-rep", "db_sources": "crm, hr" }),
        home,
        "sales-lead",
    )
    .await;
    assert_ne!(res["isError"], true, "{res}");
    assert_eq!(read_db_grants(home, "sales-rep"), vec!["crm", "hr"]);

    let text = update_text(&res);
    assert!(
        text.contains("capabilities.db_sources = [\"crm\", \"hr\"]"),
        "{text}"
    );
    assert!(text.contains("目前資料庫來源授權：crm、hr"), "{text}");

    // A second replace with one id drops the other — replace, not merge.
    let res = handle_agent_update(
        &serde_json::json!({ "agent_id": "sales-rep", "db_sources": "hr" }),
        home,
        "sales-lead",
    )
    .await;
    assert_ne!(res["isError"], true, "{res}");
    assert_eq!(read_db_grants(home, "sales-rep"), vec!["hr"]);

    // The grant change gets its own audit row on top of the generic
    // tool-call record written by the dispatch layer.
    let audit = fs::read_to_string(home.join("tool_calls.jsonl")).expect("audit row written");
    assert!(audit.contains("db_sources_grant_changed"), "{audit}");
    assert!(audit.contains("調整資料庫來源授權"), "{audit}");
}

/// `db_sources_add` appends and is idempotent; ids are canonicalized to the
/// configured `[db_sources.<id>]` key.
#[tokio::test]
async fn agent_update_db_sources_add_is_idempotent() {
    let tmp = db_grant_home();
    let home = tmp.path();

    let res = handle_agent_update(
        &serde_json::json!({ "agent_id": "sales-rep", "db_sources_add": "CRM" }),
        home,
        "sales-lead",
    )
    .await;
    assert_ne!(res["isError"], true, "{res}");
    assert_eq!(
        read_db_grants(home, "sales-rep"),
        vec!["crm"],
        "id canonicalized"
    );

    // Same id again: not an error, not duplicated, reported as unchanged.
    let res = handle_agent_update(
        &serde_json::json!({ "agent_id": "sales-rep", "db_sources_add": "crm,crm" }),
        home,
        "sales-lead",
    )
    .await;
    assert_ne!(res["isError"], true, "{res}");
    assert_eq!(read_db_grants(home, "sales-rep"), vec!["crm"]);
    let text = update_text(&res);
    assert!(text.contains("未變更（已持有：crm）"), "{text}");

    // Adding a second source keeps the first.
    let res = handle_agent_update(
        &serde_json::json!({ "agent_id": "sales-rep", "db_sources_add": "hr" }),
        home,
        "sales-lead",
    )
    .await;
    assert_ne!(res["isError"], true, "{res}");
    assert_eq!(read_db_grants(home, "sales-rep"), vec!["crm", "hr"]);
    assert!(update_text(&res).contains("capabilities.db_sources += [\"hr\"]"));
}

/// Removing an id the agent does not hold is reported, not refused.
#[tokio::test]
async fn agent_update_db_sources_remove_ignores_unheld_ids() {
    let tmp = db_grant_home();
    let home = tmp.path();

    handle_agent_update(
        &serde_json::json!({ "agent_id": "sales-rep", "db_sources": "crm" }),
        home,
        "sales-lead",
    )
    .await;

    let res = handle_agent_update(
        &serde_json::json!({ "agent_id": "sales-rep", "db_sources_remove": "hr, crm" }),
        home,
        "sales-lead",
    )
    .await;
    assert_ne!(res["isError"], true, "{res}");
    assert!(read_db_grants(home, "sales-rep").is_empty());

    let text = update_text(&res);
    assert!(text.contains("未變更（未持有：hr）"), "{text}");
    assert!(
        text.contains("capabilities.db_sources -= [\"crm\"]"),
        "{text}"
    );
    assert!(text.contains("目前資料庫來源授權：（無）"), "{text}");
}

/// Revocation is config-free on purpose: a grant the operator has since
/// deleted from `config.toml` is exactly the one that must stay revocable,
/// and the id never leaves the agent's own held list.
#[tokio::test]
async fn agent_update_db_sources_remove_clears_a_stale_unconfigured_grant() {
    let tmp = db_grant_home();
    let home = tmp.path();
    let path = home.join("agents").join("sales-rep").join("agent.toml");

    // Hand-seeded state: "old" was granted back when config declared it.
    let original = fs::read_to_string(&path).unwrap();
    fs::write(
        &path,
        original.replace(
            "[capabilities]\n",
            "[capabilities]\ndb_sources = [\"old\", \"crm\"]\n",
        ),
    )
    .unwrap();
    assert_eq!(read_db_grants(home, "sales-rep"), vec!["old", "crm"]);

    // Granting "old" is still refused (config has no such source)...
    let res = handle_agent_update(
        &serde_json::json!({ "agent_id": "sales-rep", "db_sources_add": "old" }),
        home,
        "sales-lead",
    )
    .await;
    assert_eq!(res["isError"], true, "{res}");

    // ...but revoking it succeeds.
    let res = handle_agent_update(
        &serde_json::json!({ "agent_id": "sales-rep", "db_sources_remove": "old" }),
        home,
        "sales-lead",
    )
    .await;
    assert_ne!(res["isError"], true, "{res}");
    assert_eq!(read_db_grants(home, "sales-rep"), vec!["crm"]);
    assert!(update_text(&res).contains("capabilities.db_sources -= [\"old\"]"));

    // Shape is still enforced, so caller text cannot reach the change log.
    let res = handle_agent_update(
        &serde_json::json!({ "agent_id": "sales-rep", "db_sources_remove": "../../etc/passwd" }),
        home,
        "sales-lead",
    )
    .await;
    assert_eq!(res["isError"], true, "{res}");
    assert_eq!(read_db_grants(home, "sales-rep"), vec!["crm"]);
}

/// All three params in one call apply replace → add → remove, in that order.
#[tokio::test]
async fn agent_update_db_sources_precedence_is_replace_add_remove() {
    let tmp = db_grant_home();
    let home = tmp.path();

    handle_agent_update(
        &serde_json::json!({ "agent_id": "sales-rep", "db_sources": "hr" }),
        home,
        "sales-lead",
    )
    .await;

    // replace → ["crm"], add → ["crm","hr"], remove → ["hr"].
    let res = handle_agent_update(
        &serde_json::json!({
            "agent_id": "sales-rep",
            "db_sources": "crm",
            "db_sources_add": "hr",
            "db_sources_remove": "crm",
        }),
        home,
        "sales-lead",
    )
    .await;
    assert_ne!(res["isError"], true, "{res}");
    assert_eq!(read_db_grants(home, "sales-rep"), vec!["hr"]);
}

/// An unknown id is refused with the configured ids listed, and nothing is
/// written — not even the good half of the same call.
#[tokio::test]
async fn agent_update_db_sources_rejects_unknown_id_without_writing() {
    let tmp = db_grant_home();
    let home = tmp.path();
    let before =
        fs::read_to_string(home.join("agents").join("sales-rep").join("agent.toml")).unwrap();

    let res = handle_agent_update(
        &serde_json::json!({ "agent_id": "sales-rep", "db_sources_add": "crm,payroll" }),
        home,
        "sales-lead",
    )
    .await;
    assert_eq!(res["isError"], true, "{res}");

    let text = update_text(&res);
    assert!(text.contains("payroll"), "{text}");
    assert!(
        text.contains("crm"),
        "configured ids must be listed: {text}"
    );
    assert!(text.contains("hr"), "configured ids must be listed: {text}");
    // Ids only — never the source's connection string.
    assert!(!text.contains("duduclaw-test-crm.sqlite"), "{text}");

    let after =
        fs::read_to_string(home.join("agents").join("sales-rep").join("agent.toml")).unwrap();
    assert_eq!(
        before, after,
        "a rejected grant must not rewrite agent.toml"
    );

    // Same rule on the replace param.
    let res = handle_agent_update(
        &serde_json::json!({ "agent_id": "sales-rep", "db_sources": "nope" }),
        home,
        "sales-lead",
    )
    .await;
    assert_eq!(res["isError"], true, "{res}");
    assert!(read_db_grants(home, "sales-rep").is_empty());
}

/// An empty string revokes everything.
#[tokio::test]
async fn agent_update_db_sources_empty_string_revokes_all() {
    let tmp = db_grant_home();
    let home = tmp.path();

    handle_agent_update(
        &serde_json::json!({ "agent_id": "sales-rep", "db_sources": "crm,hr" }),
        home,
        "sales-lead",
    )
    .await;
    assert_eq!(read_db_grants(home, "sales-rep").len(), 2);

    let res = handle_agent_update(
        &serde_json::json!({ "agent_id": "sales-rep", "db_sources": "" }),
        home,
        "sales-lead",
    )
    .await;
    assert_ne!(res["isError"], true, "{res}");
    assert!(read_db_grants(home, "sales-rep").is_empty());

    // An empty list is skipped on serialize, so the key disappears again.
    let raw =
        fs::read_to_string(home.join("agents").join("sales-rep").join("agent.toml")).unwrap();
    assert!(!raw.contains("db_sources"), "{raw}");
    assert!(update_text(&res).contains("目前資料庫來源授權：（無）"));
}

/// A grant edit must not cost the agent its other settings: the sections
/// `agent_update` does not touch survive the rewrite with their values
/// intact (`[runtime]` is the R2-unified section that used to be dropped).
#[tokio::test]
async fn agent_update_db_sources_preserves_unrelated_sections() {
    let tmp = db_grant_home();
    let home = tmp.path();
    let path = home.join("agents").join("sales-rep").join("agent.toml");
    let original = fs::read_to_string(&path).unwrap();
    fs::write(
        &path,
        format!("{original}\n[runtime]\nprovider = \"codex\"\nfallback = \"claude\"\n"),
    )
    .unwrap();

    let res = handle_agent_update(
        &serde_json::json!({ "agent_id": "sales-rep", "db_sources_add": "crm" }),
        home,
        "sales-lead",
    )
    .await;
    assert_ne!(res["isError"], true, "{res}");

    let cfg: duduclaw_core::types::AgentConfig =
        toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(cfg.capabilities.db_sources, vec!["crm"]);
    assert_eq!(cfg.runtime.provider.as_deref(), Some("codex"));
    assert_eq!(cfg.runtime.fallback.as_deref(), Some("claude"));
    // ...and the ordinary typed sections are untouched too.
    assert_eq!(cfg.agent.reports_to, "sales-lead");
    assert_eq!(cfg.cultural_context.locale, "zh-TW");
    assert_eq!(cfg.budget.monthly_limit_cents, 1000);
}

/// WP21 collateral fix: `agent_remove` used to take no `caller` at all, so
/// any agent could delete any other agent's node by id. It now goes
/// through the exact same `check_org_subject_allowed` gate as the
/// `reports_to` branch of `agent_update` — same helper, same messages,
/// same exemptions.
#[tokio::test]
async fn agent_remove_enforces_subtree_rule() {
    let tmp = delegation_home();
    let home = tmp.path();

    // Another team's agent is off limits.
    let params = serde_json::json!({ "agent_id": "mkt-rep" });
    let res = handle_agent_remove(&params, home, "sales-lead").await;
    assert_eq!(res["isError"], true, "{res}");
    assert!(
        res["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("不在你的團隊之內"),
        "{res}"
    );
    assert!(
        home.join("agents")
            .join("mkt-rep")
            .join("agent.toml")
            .exists(),
        "a rejected remove must not touch the agent directory"
    );

    // Own subtree: allowed.
    let params = serde_json::json!({ "agent_id": "sales-rep2" });
    let res = handle_agent_remove(&params, home, "sales-lead").await;
    assert_ne!(res["isError"], true, "{res}");
    assert!(
        !home
            .join("agents")
            .join("sales-rep2")
            .join("agent.toml")
            .exists()
    );

    // System senders are exempt from the subtree rule.
    let params = serde_json::json!({ "agent_id": "mkt-rep" });
    let res = handle_agent_remove(&params, home, "dashboard").await;
    assert_ne!(res["isError"], true, "{res}");
    assert!(
        !home
            .join("agents")
            .join("mkt-rep")
            .join("agent.toml")
            .exists()
    );

    // The `open` policy escape hatch turns the whole gate off.
    fs::write(
        home.join("config.toml"),
        "[delegation]\npolicy = \"open\"\n",
    )
    .unwrap();
    let params = serde_json::json!({ "agent_id": "mkt-lead" });
    let res = handle_agent_remove(&params, home, "sales-rep").await;
    assert_ne!(res["isError"], true, "{res}");
    assert!(
        !home
            .join("agents")
            .join("mkt-lead")
            .join("agent.toml")
            .exists()
    );
}

/// End-to-end through the real `create_agent` handler: the placement gate
/// sits after `validate_reports_to`, so a well-formed but unauthorized
/// placement must still be refused *and* leave no agent directory behind.
#[tokio::test]
async fn create_agent_rejects_placement_outside_caller_subtree() {
    let tmp = delegation_home();
    let home = tmp.path();

    let params = serde_json::json!({
        "name": "mole",
        "display_name": "臥底",
        "reports_to": "ceo",
    });
    let res = handle_create_agent(&params, home, "sales-rep").await;
    assert_eq!(res["isError"], true, "{res}");
    assert!(
        res["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("只能將 AI 員工掛在自己或自己團隊之下"),
        "{res}"
    );
    assert!(
        !home.join("agents").join("mole").exists(),
        "a rejected create_agent must not scaffold a directory"
    );

    // Under itself: allowed.
    let params = serde_json::json!({
        "name": "helper",
        "display_name": "助手",
        "reports_to": "sales-rep",
    });
    let res = handle_create_agent(&params, home, "sales-rep").await;
    assert_ne!(res["isError"], true, "{res}");
    assert!(
        home.join("agents")
            .join("helper")
            .join("agent.toml")
            .exists()
    );
}

/// A non-main caller that omits `reports_to` used to default to the main
/// agent — a placement the caller itself is not authorized to make under
/// the C4 subtree rule, so the create silently failed. It must now default
/// to the caller and succeed.
#[tokio::test]
async fn create_agent_omitted_reports_to_defaults_to_non_main_caller() {
    let tmp = delegation_home();
    let home = tmp.path();

    let params = serde_json::json!({
        "name": "intern",
        "display_name": "實習生",
    });
    let res = handle_create_agent(&params, home, "sales-rep").await;
    assert_ne!(res["isError"], true, "{res}");

    let path = home.join("agents").join("intern").join("agent.toml");
    assert!(path.exists());
    let cfg: duduclaw_core::types::AgentConfig =
        toml::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(cfg.agent.reports_to, "sales-rep");
}
