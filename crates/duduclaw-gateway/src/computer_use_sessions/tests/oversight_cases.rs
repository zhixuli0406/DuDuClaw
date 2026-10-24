use super::*;

#[tokio::test]
async fn a_stop_confirmation_keeps_its_session_fence_after_direct_authority_is_granted() {
    let tmp = home();
    let state = Arc::new(FakeState::default());
    let mgr = manager(tmp.path(), &state, IDLE_TIMEOUT);
    let old = insert_session(&mgr, &state, "alice", ComputerUseConfig::default());
    mgr.stop("alice", Some(&old)).await.unwrap();
    let new = insert_session(&mgr, &state, "alice", ComputerUseConfig::default());
    let upgraded = duduclaw_auth::UserContext::admin_fallback();
    assert!(live_view::authorize(
        &upgraded,
        "alice",
        live_view::LiveAction::Watch
    ));
    // This is the same post-authority helper called by the actual direct
    // dashboard RPC, with the confirmation captured before the upgrade.
    assert!(
        mgr.live_stop_with_login_bound("alice", &upgraded, false, Some(&old))
            .await
            .is_err()
    );
    assert!(mgr.live_entry("alice", Some(&new)).is_some());
}

#[tokio::test]
async fn a_team_stop_never_terminates_a_replacement_session() {
    let tmp = home();
    let state = Arc::new(FakeState::default());
    let mgr = manager(tmp.path(), &state, IDLE_TIMEOUT);
    let old = insert_session(&mgr, &state, "alice", ComputerUseConfig::default());
    mgr.stop("alice", Some(&old)).await.unwrap();
    let new = insert_session(&mgr, &state, "alice", ComputerUseConfig::default());
    let actor = duduclaw_auth::UserContext::admin_fallback();
    assert!(mgr.live_stop_expected("alice", &old, &actor).await.is_err());
    assert!(
        mgr.live_status("alice", "manager")["active"]
            .as_bool()
            .unwrap()
    );
    let result = mgr.live_stop_expected("alice", &new, &actor).await.unwrap();
    assert_eq!(result["session_id"], new);
    assert_eq!(result["ended"], true);
}

#[tokio::test]
async fn a_team_disclosure_attributes_only_the_actual_starter_among_shared_reports() {
    let tmp = home();
    let state = Arc::new(FakeState::default());
    let mgr = manager(tmp.path(), &state, IDLE_TIMEOUT);
    let id = insert_session(&mgr, &state, "alice", ComputerUseConfig::default());
    let now = chrono::Utc::now();
    mgr.entry("alice")
        .unwrap()
        .shared
        .origin
        .set(super::super::oversight::SessionOrigin::from_requester(
            &duduclaw_knowledge::requester::RequesterIdentity::DashboardUser {
                user_id: "report-a".into(),
            },
            now,
        ))
        .unwrap();
    let reports = ["report-a", "report-b"]
        .into_iter()
        .map(|user| {
            (
                user.to_owned(),
                duduclaw_auth::OverseenReport {
                    user_id: user.into(),
                    display_name: user.into(),
                    depth: 1,
                    level: duduclaw_auth::AccessLevel::Operator,
                },
            )
        })
        .collect();
    let scope = crate::team_oversight::TeamOversight {
        viewer_user_id: "manager".into(),
        agent: "alice".into(),
        reports,
        since: now - chrono::Duration::seconds(1),
        generation: 1,
    };
    assert_eq!(
        mgr.team_report_for("alice", &id, &scope),
        Some(("report-a".into(), 1))
    );
    assert_eq!(mgr.team_report_for("alice", "replaced", &scope), None);
}

#[tokio::test]
async fn masked_view_uses_shared_cache_without_queueing_behind_an_employee() {
    let tmp = home();
    let state = Arc::new(FakeState::default());
    let mgr = manager(tmp.path(), &state, IDLE_TIMEOUT);
    let id = insert_session(&mgr, &state, "alice", ComputerUseConfig::default());
    let first = mgr.screenshot("alice", Some(&id)).await.unwrap();
    let entry = mgr.entry("alice").unwrap();
    let lock = entry.session.lock().await;
    let cached = tokio::time::timeout(
        Duration::from_millis(250),
        mgr.masked_snapshot("alice", &id),
    )
    .await
    .expect("viewer must not queue behind the employee")
    .unwrap();
    assert_eq!(cached["png_base64"], first["png_base64"]);
    assert_eq!(cached["stale"], false);
    assert_eq!(
        cached.as_object().unwrap().len(),
        9,
        "closed response shape"
    );
    entry
        .shared
        .snapshot
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .captured = Instant::now() - Duration::from_secs(6);
    let stale = tokio::time::timeout(
        Duration::from_millis(250),
        mgr.masked_snapshot("alice", &id),
    )
    .await
    .expect("stale image also must not queue")
    .unwrap();
    assert_eq!(stale["stale"], true);
    drop(lock);
}

#[tokio::test]
async fn injection_hold_masks_even_an_earlier_clean_cached_image_without_resuming() {
    let tmp = home();
    let state = Arc::new(FakeState::default());
    let mgr = manager(tmp.path(), &state, IDLE_TIMEOUT);
    let id = insert_session(&mgr, &state, "alice", ComputerUseConfig::default());
    mgr.screenshot("alice", Some(&id)).await.unwrap();
    let entry = mgr.entry("alice").unwrap();
    *entry.shared.live.hold.lock().unwrap() = Some(live_view::Hold {
        categories: vec!["synthetic-fixture".into()],
        since_unix: chrono::Utc::now().timestamp(),
    });
    entry.shared.live.set_frozen(Some(Instant::now()));
    entry
        .shared
        .snapshot
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .captured = Instant::now() - Duration::from_secs(6);
    let body = mgr.masked_snapshot("alice", &id).await.unwrap();
    assert_eq!(body["fully_masked"], true);
    assert_eq!(body["mask_reason"], "injection_suspected");
    assert_eq!(
        state.thaws.load(Ordering::SeqCst),
        0,
        "team viewing never thaws a paused browser"
    );
    assert!(
        entry.shared.live.hold().is_some(),
        "viewing cannot release an injection hold"
    );
}

#[tokio::test]
async fn cached_images_are_refused_after_red_or_capability_revocation() {
    for red in [false, true] {
        let tmp = home();
        let state = Arc::new(FakeState::default());
        let mgr = manager(tmp.path(), &state, IDLE_TIMEOUT);
        let id = insert_session(&mgr, &state, "alice", ComputerUseConfig::default());
        mgr.screenshot("alice", Some(&id)).await.unwrap();
        if red {
            std::fs::write(tmp.path().join("threat_level"), "RED").unwrap();
        } else {
            write_agent(tmp.path(), "alice", "[capabilities]\ncomputer_use=false\n");
        }
        assert!(
            mgr.masked_snapshot("alice", &id).await.is_err(),
            "fresh cache cannot bypass a new safety fence"
        );
    }
}

#[tokio::test]
async fn a_snapshot_request_never_switches_to_a_replacement_session() {
    let tmp = home();
    let state = Arc::new(FakeState::default());
    let mgr = manager(tmp.path(), &state, IDLE_TIMEOUT);
    let old = insert_session(&mgr, &state, "alice", ComputerUseConfig::default());
    mgr.screenshot("alice", Some(&old)).await.unwrap();
    mgr.stop("alice", Some(&old)).await.unwrap();
    let new = insert_session(&mgr, &state, "alice", ComputerUseConfig::default());
    assert_ne!(old, new);
    assert!(mgr.masked_snapshot("alice", &old).await.is_err());
    assert!(mgr.masked_snapshot("bob", &new).await.is_err());
}

#[tokio::test]
async fn cached_views_refuse_changed_login_owners_without_waiting_for_the_session_lock() {
    let tmp = home();
    let state = Arc::new(FakeState::default());
    let mgr = manager(tmp.path(), &state, IDLE_TIMEOUT);
    let id = insert_session(&mgr, &state, "alice", ComputerUseConfig::default());
    mgr.screenshot("alice", Some(&id)).await.unwrap();
    state.owner_changed.store(true, Ordering::SeqCst);
    let session = mgr.lookup("alice").unwrap();
    let _busy = session.lock().await;
    assert!(
        tokio::time::timeout(
            Duration::from_millis(250),
            mgr.masked_snapshot("alice", &id)
        )
        .await
        .unwrap()
        .is_err()
    );
}

#[tokio::test]
async fn frozen_cached_views_refuse_the_original_session_deadline() {
    let tmp = home();
    let state = Arc::new(FakeState::default());
    let mgr = manager(tmp.path(), &state, IDLE_TIMEOUT);
    let seed = insert_session(&mgr, &state, "bob", ComputerUseConfig::default());
    let image = mgr.screenshot("bob", Some(&seed)).await.unwrap();
    let id = insert_session_until(
        &mgr,
        &state,
        "alice",
        ComputerUseConfig::default(),
        Instant::now() + Duration::from_millis(100),
    );
    let entry = mgr.entry("alice").unwrap();
    entry.shared.live.set_frozen(Some(Instant::now()));
    *entry.shared.snapshot.lock().unwrap() = Some(super::super::oversight::CachedSnapshot {
        captured: Instant::now(),
        captured_at: chrono::Utc::now().timestamp_millis(),
        body: image,
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(mgr.masked_snapshot("alice", &id).await.is_err());
}

#[tokio::test]
async fn a_hold_arriving_during_the_final_safety_wait_masks_the_cached_image() {
    let tmp = home();
    let state = Arc::new(FakeState::default());
    let mgr = manager(tmp.path(), &state, IDLE_TIMEOUT);
    let id = insert_session(&mgr, &state, "alice", ComputerUseConfig::default());
    mgr.screenshot("alice", Some(&id)).await.unwrap();
    let entry = mgr.entry("alice").unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    *mgr.action_boundary_pause.lock().unwrap() =
        Some(("snapshot_final_safety", entered.clone(), release.clone()));
    let inject = async {
        entered.notified().await;
        *entry.shared.live.hold.lock().unwrap() = Some(live_view::Hold {
            categories: vec!["fixture".into()],
            since_unix: chrono::Utc::now().timestamp(),
        });
        release.notify_one();
    };
    let (view, ()) = tokio::join!(mgr.masked_snapshot("alice", &id), inject);
    let view = view.unwrap();
    assert_eq!(view["fully_masked"], true);
    assert_eq!(view["mask_reason"], "injection_suspected");
}
