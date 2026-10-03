//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

#[tokio::test]
pub(super) async fn system_update_config_rejects_bad_belief_values() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    // flat_band_pct out of the 0.01-10.0 range.
    let bad_band = handler
        .handle_system_update_config(
            json!({ "belief": { "flat_band_pct": 15.0 } }),
            &admin_ctx(),
        )
        .await;
    assert!(
        !frame_ok(&bad_band),
        "out-of-range flat_band_pct must be rejected"
    );

    // tick_subject_map key too long.
    let long_key = "x".repeat(65);
    let bad_key = handler
        .handle_system_update_config(
            json!({
                "belief": { "tick_subject_map": { long_key: "subject" } },
            }),
            &admin_ctx(),
        )
        .await;
    assert!(
        !frame_ok(&bad_key),
        "over-long tick_subject_map key must be rejected"
    );

    // tick_subject_map with more than 32 entries.
    let mut too_many = serde_json::Map::new();
    for i in 0..33 {
        too_many.insert(format!("field_{i}"), json!(format!("subject_{i}")));
    }
    let bad_size = handler
        .handle_system_update_config(
            json!({
                "belief": { "tick_subject_map": too_many },
            }),
            &admin_ctx(),
        )
        .await;
    assert!(
        !frame_ok(&bad_size),
        "more than 32 tick_subject_map entries must be rejected"
    );

    // Nothing was written by any rejected payload.
    assert!(
        !home.path().join("config.toml").exists(),
        "no partial write on validation failure"
    );
}

// ── Expert packs dashboard RPCs ──────────────────────────────────────────

/// All four experts.* RPCs are admin-only fail-closed: a manager-role
/// caller is denied before any handler logic runs.
#[tokio::test]
pub(super) async fn experts_rpcs_deny_non_admin() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = UserContext {
        user_id: "m1".to_string(),
        email: "m1@test.local".to_string(),
        role: UserRole::Manager,
        agent_access: std::collections::HashMap::new(),
        must_change_password: false,
    };
    for method in [
        "experts.list",
        "experts.install",
        "experts.remove",
        "experts.hooks_apply",
        "experts.catalog",
        "experts.install_builtin",
        "experts.generate",
        "experts.generate_revise",
        "experts.install_draft",
    ] {
        let frame = handler
            .handle(
                method,
                json!({
                    "slug": "x", "path": "/tmp/x", "industry": "x",
                    "description": "d", "draft_id": "x", "feedback": "f"
                }),
                &ctx,
            )
            .await;
        assert!(!frame_ok(&frame), "{method} must deny non-admin: {frame:?}");
    }
}

/// `experts.catalog` is fail-safe: a fresh home with no premium tree
/// returns an ok frame with an empty pack list, never an error.
#[tokio::test]
pub(super) async fn experts_catalog_fail_safe_without_premium() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = UserContext::admin_fallback();
    let frame = handler.handle("experts.catalog", json!({}), &ctx).await;
    assert!(frame_ok(&frame), "catalog must not error: {frame:?}");
    let data = frame_data(&frame);
    assert!(data["packs"].is_array());
    assert!(data["deployed"].is_boolean());
    assert!(data["unlocked"].is_boolean());
}

/// `gallery.list` is admin-only fail-closed, same as the `experts.*`
/// family it reads from (P2-b mirrors P2-a's license/deployment gate).
#[tokio::test]
pub(super) async fn gallery_list_denies_non_admin() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = UserContext {
        user_id: "m1".to_string(),
        email: "m1@test.local".to_string(),
        role: UserRole::Manager,
        agent_access: std::collections::HashMap::new(),
        must_change_password: false,
    };
    let frame = handler.handle("gallery.list", json!({}), &ctx).await;
    assert!(
        !frame_ok(&frame),
        "gallery.list must deny non-admin: {frame:?}"
    );
}

/// `gallery.list` is fail-safe: a fresh home with no premium tree returns
/// an ok frame with an empty card list, never an error.
#[tokio::test]
pub(super) async fn gallery_list_fail_safe_without_premium() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = UserContext::admin_fallback();
    let frame = handler.handle("gallery.list", json!({}), &ctx).await;
    assert!(frame_ok(&frame), "gallery.list must not error: {frame:?}");
    let data = frame_data(&frame);
    assert!(data["cards"].is_array());
    assert!(data["deployed"].is_boolean());
    assert!(data["unlocked"].is_boolean());
}

// ── Agent Mail (P2-d) ────────────────────────────────────────────────

/// Every `mail.*` RPC is manager-gated (approval-centre tier).
#[tokio::test]
pub(super) async fn mail_rpcs_deny_a_plain_employee() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = UserContext {
        user_id: "u1".to_string(),
        email: "u1@test.local".to_string(),
        role: UserRole::Employee,
        agent_access: std::collections::HashMap::new(),
        must_change_password: false,
    };
    for method in [
        "mail.status",
        "mail.list",
        "mail.read",
        "mail.archive",
        "mail.outbox",
        "mail.decide",
    ] {
        let frame = handler.handle(method, json!({}), &ctx).await;
        assert!(
            !frame_ok(&frame),
            "{method} must deny a plain employee: {frame:?}"
        );
    }
}

/// A fresh home has no `[mail]` section: status reports the feature off
/// and the lists come back empty — never an error, never a fabricated row.
#[tokio::test]
pub(super) async fn mail_status_and_lists_are_fail_safe_on_a_fresh_home() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = UserContext::admin_fallback();

    let frame = handler.handle("mail.status", json!({}), &ctx).await;
    assert!(frame_ok(&frame), "{frame:?}");
    let data = frame_data(&frame);
    assert_eq!(data["enabled"], json!(false));
    assert_eq!(
        data["auto_trigger"],
        json!(false),
        "到達即觸發 defaults off"
    );
    assert_eq!(data["smtp_configured"], json!(false));

    for (method, key) in [("mail.list", "messages"), ("mail.outbox", "drafts")] {
        let frame = handler.handle(method, json!({}), &ctx).await;
        assert!(frame_ok(&frame), "{method}: {frame:?}");
        assert_eq!(frame_data(&frame)[key].as_array().map(Vec::len), Some(0));
    }
}

/// `approve` is a send decision: a missing flag must read as neither
/// answer, and an unknown draft must not be invented.
#[tokio::test]
pub(super) async fn mail_decide_requires_an_explicit_flag_and_a_real_draft() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = UserContext::admin_fallback();

    let no_flag = handler
        .handle("mail.decide", json!({ "mail_id": "out-1" }), &ctx)
        .await;
    assert!(!frame_ok(&no_flag), "missing approve must be refused");

    let no_draft = handler
        .handle(
            "mail.decide",
            json!({ "mail_id": "out-1", "approve": true }),
            &ctx,
        )
        .await;
    assert!(!frame_ok(&no_draft), "unknown draft must be refused");
}

/// `mail.decide` is a narrower gate onto the shared approval store. It must
/// only ever decide a row whose `action_kind` really is the mail kind —
/// otherwise a crafted draft could be used to approve something else.
#[tokio::test]
pub(super) async fn mail_decide_refuses_an_approval_of_another_kind() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = UserContext::admin_fallback();

    let broker = crate::approval::ApprovalBroker::open(home.path()).unwrap();
    let foreign = broker
        .request("sales", "mcp_install", "install something", json!({}), 3600)
        .await
        .unwrap();
    let cfg = crate::mail::MailConfig::default();
    crate::mail::record_outbox_draft(
        home.path(),
        &cfg,
        "sales",
        "client@example.com",
        "s",
        "b",
        foreign.as_str(),
        None,
    );

    let frame = handler
        .handle(
            "mail.decide",
            json!({ "mail_id": crate::mail::list_outbox(home.path(), None, None, 10)[0].mail_id, "approve": true }),
            &ctx,
        )
        .await;
    assert!(
        !frame_ok(&frame),
        "cross-kind decision must be refused: {frame:?}"
    );
    // And the foreign approval is still pending — untouched.
    let rec = broker.get(&foreign).await.unwrap().unwrap();
    assert_eq!(rec.status, crate::approval::ApprovalStatus::Pending);
}

#[test]
pub(super) fn approval_decision_requires_a_readable_existing_record() {
    for lookup in [Err("transient read failure".to_owned()), Ok(None)] {
        let response = MethodHandler::required_approval_for_decision(lookup).unwrap_err();
        assert!(!frame_ok(&response));
    }
}

#[tokio::test]
pub(super) async fn synthetic_pilot_review_decision_requires_an_admin() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let broker = crate::approval::ApprovalBroker::open(home.path()).unwrap();
    let missing = handler
        .handle(
            "approvals.decide",
            json!({ "id": "123e4567-e89b-42d3-a456-426614174000", "approve": true }),
            &UserContext::admin_fallback(),
        )
        .await;
    assert!(!frame_ok(&missing));
    let approval = broker
        .request(
            "requesting-admin",
            "support_pilot_review",
            "Inspect one synthetic run",
            json!({ "replay_hash": "exact-run" }),
            3600,
        )
        .await
        .unwrap();
    let mut manager = UserContext::admin_fallback();
    manager.role = UserRole::Manager;
    manager.user_id = "manager-1".into();
    let refused = handler
        .handle(
            "approvals.decide",
            json!({ "id": approval.as_str(), "approve": true }),
            &manager,
        )
        .await;
    assert!(!frame_ok(&refused));
    assert_eq!(
        broker.get(&approval).await.unwrap().unwrap().status,
        crate::approval::ApprovalStatus::Pending,
    );
    let admin = UserContext::admin_fallback();
    let decided = handler
        .handle(
            "approvals.decide",
            json!({ "id": approval.as_str(), "approve": true }),
            &admin,
        )
        .await;
    assert!(frame_ok(&decided), "{decided:?}");
    assert_eq!(
        broker.get(&approval).await.unwrap().unwrap().status,
        crate::approval::ApprovalStatus::Approved,
    );
}

/// A confirmed draft is reported as queued, never as sent: the actual
/// transmission happens later in `mail_worker::settle_outbox`.
#[tokio::test]
pub(super) async fn mail_decide_reports_queued_not_sent_and_refuses_a_second_decision() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = UserContext::admin_fallback();

    let broker = crate::approval::ApprovalBroker::open(home.path()).unwrap();
    let approval = broker
        .request(
            "sales",
            crate::mail::OUTBOUND_ACTION_KIND,
            "寄信",
            json!({}),
            3600,
        )
        .await
        .unwrap();
    let cfg = crate::mail::MailConfig::default();
    let mail_id = crate::mail::record_outbox_draft(
        home.path(),
        &cfg,
        "sales",
        "client@example.com",
        "報價回覆",
        "附上報價單。",
        approval.as_str(),
        None,
    );

    let frame = handler
        .handle(
            "mail.decide",
            json!({ "mail_id": mail_id, "approve": true }),
            &ctx,
        )
        .await;
    assert!(frame_ok(&frame), "{frame:?}");
    assert_eq!(frame_data(&frame)["state"], "approved_queued");

    // The draft is still `pending` in the ledger — only the worker moves it.
    assert_eq!(
        crate::mail::list_outbox(home.path(), None, None, 10)[0].status,
        crate::mail::OutboxStatus::Pending
    );
    // The decision itself is terminal in the broker, so a flip is refused.
    assert_eq!(
        broker.get(&approval).await.unwrap().unwrap().status,
        crate::approval::ApprovalStatus::Approved
    );
    let again = handler
        .handle(
            "mail.decide",
            json!({ "mail_id": mail_id, "approve": false }),
            &ctx,
        )
        .await;
    assert!(
        !frame_ok(&again),
        "a decided approval must not be flippable"
    );
}

/// `experts.install_builtin` rejects unsafe industry slugs BEFORE license
/// checks or any subprocess spawn (path-traversal fence).
#[tokio::test]
pub(super) async fn experts_install_builtin_rejects_bad_slugs() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = UserContext::admin_fallback();
    for bad in ["../etc", "a/b", "..", "", "UPPER"] {
        let frame = handler
            .handle("experts.install_builtin", json!({ "industry": bad }), &ctx)
            .await;
        assert!(!frame_ok(&frame), "{bad:?} must be rejected: {frame:?}");
    }
    // Nothing was written into the cache by the rejected calls.
    assert!(!crate::expert_generate::builtin_cache_dir(home.path()).exists());
}

/// `experts.generate` validates inputs before any LLM call.
#[tokio::test]
pub(super) async fn experts_generate_rejects_invalid_inputs() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = UserContext::admin_fallback();

    // Missing / empty description.
    for params in [json!({}), json!({ "description": "   " })] {
        let frame = handler.handle("experts.generate", params, &ctx).await;
        assert!(!frame_ok(&frame), "empty description must be rejected");
    }
    // Out-of-range team size and unknown channel.
    let frame = handler
        .handle(
            "experts.generate",
            json!({ "description": "d", "team_size": 99 }),
            &ctx,
        )
        .await;
    assert!(!frame_ok(&frame));
    let frame = handler
        .handle(
            "experts.generate",
            json!({ "description": "d", "channels": ["myspace"] }),
            &ctx,
        )
        .await;
    assert!(!frame_ok(&frame));
}

/// `experts.generate_revise` fences draft ids, requires feedback, and
/// enforces the round cap — all before any LLM call.
#[tokio::test]
pub(super) async fn experts_generate_revise_guards() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = UserContext::admin_fallback();

    // Traversal / unknown draft ids.
    for id in ["../evil", "ghost"] {
        let frame = handler
            .handle(
                "experts.generate_revise",
                json!({ "draft_id": id, "feedback": "改" }),
                &ctx,
            )
            .await;
        assert!(!frame_ok(&frame), "{id:?} must be rejected");
    }
    // Empty feedback.
    let frame = handler
        .handle(
            "experts.generate_revise",
            json!({ "draft_id": "abc", "feedback": "  " }),
            &ctx,
        )
        .await;
    assert!(!frame_ok(&frame));

    // Round cap: a draft at MAX_GENERATE_ROUNDS refuses further revision.
    let state = crate::expert_generate::DraftState {
        draft_id: "abc-cap".into(),
        request: crate::expert_generate::GenerateRequest {
            industry_hint: String::new(),
            description: "d".into(),
            team_size: 2,
            channels: vec![],
        },
        rounds: crate::expert_generate::MAX_GENERATE_ROUNDS,
        created_at: crate::expert_admin::now_iso(),
        updated_at: crate::expert_admin::now_iso(),
        last_generation: "{}".into(),
    };
    crate::expert_generate::write_draft_state(home.path(), &state).unwrap();
    let frame = handler
        .handle(
            "experts.generate_revise",
            json!({ "draft_id": "abc-cap", "feedback": "改" }),
            &ctx,
        )
        .await;
    assert!(!frame_ok(&frame), "round cap must reject: {frame:?}");
    let err = format!("{frame:?}");
    assert!(err.contains("上限"), "cap message surfaced: {err}");
}

/// `experts.install_draft` fences draft ids and refuses missing drafts.
#[tokio::test]
pub(super) async fn experts_install_draft_guards() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = UserContext::admin_fallback();
    for id in ["../evil", "ghost", ""] {
        let frame = handler
            .handle("experts.install_draft", json!({ "draft_id": id }), &ctx)
            .await;
        assert!(!frame_ok(&frame), "{id:?} must be rejected");
    }
    // A draft that smuggled hooks in is refused at the install boundary.
    let pack = crate::expert_generate::draft_pack_dir(home.path(), "abc-hooked").unwrap();
    std::fs::create_dir_all(pack.join("hooks")).unwrap();
    std::fs::write(pack.join("expert.toml"), "[expert]\nname = \"x\"\n").unwrap();
    std::fs::write(pack.join("hooks/pre.sh"), "echo hi").unwrap();
    let frame = handler
        .handle(
            "experts.install_draft",
            json!({ "draft_id": "abc-hooked" }),
            &ctx,
        )
        .await;
    assert!(!frame_ok(&frame), "hooks-carrying draft must be refused");
    assert!(format!("{frame:?}").contains("hooks"));
}

#[tokio::test]
pub(super) async fn experts_list_and_remove_round_trip() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = UserContext::admin_fallback();

    // Empty on a fresh home.
    let frame = handler.handle("experts.list", json!({}), &ctx).await;
    assert!(frame_ok(&frame));
    assert_eq!(
        frame_data(&frame)["packs"].as_array().map(|a| a.len()),
        Some(0)
    );

    // Seed one installed pack + pending hooks state via the shared impl.
    let rec = crate::expert_admin::InstallRecord {
        slug: "sales".into(),
        kind: crate::expert_admin::PackKind::Native,
        display_name: "銷售團隊".into(),
        version: "1.2.0".into(),
        description: "demo".into(),
        agents: vec!["sales-lead".into()],
        global_skills: vec![],
        wiki_files: vec![],
        installed_at: crate::expert_admin::now_iso(),
    };
    crate::expert_admin::write_record(home.path(), &rec).unwrap();
    crate::expert_admin::write_hooks_state(
        home.path(),
        "sales",
        &crate::expert_admin::HooksState {
            status: crate::expert_admin::HooksStatus::PendingApproval,
            approval_id: Some("ap-1".into()),
            files: vec!["pre.sh".into()],
            updated_at: crate::expert_admin::now_iso(),
        },
    )
    .unwrap();
    std::fs::create_dir_all(home.path().join("agents/sales-lead")).unwrap();

    let frame = handler.handle("experts.list", json!({}), &ctx).await;
    assert!(frame_ok(&frame));
    let packs = frame_data(&frame)["packs"].as_array().unwrap().clone();
    assert_eq!(packs.len(), 1);
    assert_eq!(packs[0]["slug"].as_str(), Some("sales"));
    assert_eq!(packs[0]["display_name"].as_str(), Some("銷售團隊"));
    assert_eq!(packs[0]["hooks_status"].as_str(), Some("pending_approval"));
    assert_eq!(packs[0]["agents"].as_array().map(|a| a.len()), Some(1));

    // Remove deletes the recorded agent dir and the pack record.
    let frame = handler
        .handle("experts.remove", json!({ "slug": "sales" }), &ctx)
        .await;
    assert!(frame_ok(&frame), "remove: {frame:?}");
    assert!(!home.path().join("agents/sales-lead").exists());
    let frame = handler.handle("experts.list", json!({}), &ctx).await;
    assert_eq!(
        frame_data(&frame)["packs"].as_array().map(|a| a.len()),
        Some(0)
    );

    // Unknown slug errors (not silent success).
    let frame = handler
        .handle("experts.remove", json!({ "slug": "ghost" }), &ctx)
        .await;
    assert!(!frame_ok(&frame));
}

#[tokio::test]
pub(super) async fn experts_install_rejects_missing_and_non_zip_sources() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = UserContext::admin_fallback();

    // Missing path param.
    let frame = handler.handle("experts.install", json!({}), &ctx).await;
    assert!(!frame_ok(&frame));
    // Nonexistent source.
    let frame = handler
        .handle(
            "experts.install",
            json!({ "path": home.path().join("nope.zip").to_string_lossy() }),
            &ctx,
        )
        .await;
    assert!(!frame_ok(&frame));
    // Existing file that is not a .zip (and not a dir) is rejected before
    // any subprocess spawns.
    let txt = home.path().join("notes.txt");
    std::fs::write(&txt, "hi").unwrap();
    let frame = handler
        .handle(
            "experts.install",
            json!({ "path": txt.to_string_lossy() }),
            &ctx,
        )
        .await;
    assert!(!frame_ok(&frame));
}

#[tokio::test]
pub(super) async fn experts_hooks_apply_reports_pending_and_errors_on_unknown() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let ctx = UserContext::admin_fallback();

    // No managed hooks ⇒ error.
    let frame = handler
        .handle("experts.hooks_apply", json!({ "slug": "ghost" }), &ctx)
        .await;
    assert!(!frame_ok(&frame));

    // Undecided approval ⇒ status stays pending_approval.
    let broker = crate::approval::ApprovalBroker::open(home.path()).unwrap();
    let id = broker
        .request(
            "p",
            crate::expert_admin::HOOKS_ACTION_KIND,
            "enable",
            json!({}),
            3600,
        )
        .await
        .unwrap();
    crate::expert_admin::write_hooks_state(
        home.path(),
        "p",
        &crate::expert_admin::HooksState {
            status: crate::expert_admin::HooksStatus::PendingApproval,
            approval_id: Some(id.to_string()),
            files: vec!["pre.sh".into()],
            updated_at: crate::expert_admin::now_iso(),
        },
    )
    .unwrap();
    let frame = handler
        .handle("experts.hooks_apply", json!({ "slug": "p" }), &ctx)
        .await;
    assert!(frame_ok(&frame), "{frame:?}");
    assert_eq!(
        frame_data(&frame)["status"].as_str(),
        Some("pending_approval")
    );
}

#[tokio::test]
pub(super) async fn memory_graph_absent_db_is_empty_not_error() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let frame = handler
        .handle_memory_graph(json!({ "agent_id": "never-seeded" }))
        .await;
    assert!(frame_ok(&frame), "absent db ⇒ empty graph, not error");
    let data = frame_data(&frame);
    assert_eq!(
        data.get("edges")
            .and_then(|v| v.as_array())
            .map(|a| a.len()),
        Some(0)
    );
}

/// The install RPCs reach the CLI through `duduclaw pack …` (v1.69.0 removed
/// `expert install`); `convert-teams` stays under `expert`.
#[cfg(unix)]
#[tokio::test]
pub(super) async fn experts_install_spawns_the_pack_group() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let log = home.path().join("argv.txt");
    let fake = home.path().join("fake-duduclaw");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\necho \"home=$DUDUCLAW_HOME\"\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

    let args: Vec<std::ffi::OsString> =
        vec!["install".into(), "/tmp/pack".into(), "--attach-under".into(), "ceo".into()];
    let out = handler
        .spawn_cli_group_with_bin(&fake, "pack", &args, 30)
        .await
        .expect("fake binary succeeds");
    assert!(out.contains(&format!("home={}", home.path().display())), "{out}");
    let seen = std::fs::read_to_string(&log).unwrap();
    assert_eq!(
        seen.lines().collect::<Vec<_>>(),
        ["pack", "install", "/tmp/pack", "--attach-under", "ceo"]
    );
}
