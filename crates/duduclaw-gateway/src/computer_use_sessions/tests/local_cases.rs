//! Real signed route, synthetic backend. An upstream turn/principal must not
//! lend attendance to another runtime, including the same employee's dispatch.
use super::*;

#[tokio::test]
#[cfg(target_os = "macos")]
async fn local_normal_close_retains_login_only_after_a_host_classified_normal_exit() {
    let root = home();
    let state = Arc::new(FakeState::default());
    state.local_target.store(true, Ordering::Release);
    state.normal_local_exit.store(true, Ordering::Release);
    let manager = manager(root.path(), &state, IDLE_TIMEOUT);
    insert_session(&manager, &state, "alice", ComputerUseConfig::default());
    let entry = manager.lookup("alice").unwrap();
    entry
        .lock()
        .await
        .backend
        .control()
        .stopped
        .store(true, Ordering::Release);
    assert_eq!(manager.reap_once().await, 1);
    assert!(
        !state.login_discarded.load(Ordering::Acquire),
        "normal dedicated-window close is not owner revocation or an emergency logout"
    );
}

#[tokio::test]
#[cfg(target_os = "macos")]
async fn local_normal_close_never_preserves_a_revoked_held_or_explicitly_discarded_login() {
    for why in ["owner", "held", "explicit", "container"] {
        let root = home();
        let state = Arc::new(FakeState::default());
        state
            .local_target
            .store(why != "container", Ordering::Release);
        state.normal_local_exit.store(true, Ordering::Release);
        let manager = manager(root.path(), &state, IDLE_TIMEOUT);
        insert_session(&manager, &state, "alice", ComputerUseConfig::default());
        let entry = manager.lookup("alice").unwrap();
        {
            let session = entry.lock().await;
            session
                .backend
                .control()
                .stopped
                .store(true, Ordering::Release);
            match why {
                "owner" => state.owner_changed.store(true, Ordering::Release),
                "held" => {
                    *session.shared.live.hold.lock().unwrap() = Some(live_view::Hold {
                        categories: vec!["synthetic".into()],
                        since_unix: 0,
                    })
                }
                _ => session
                    .shared
                    .discard_profile
                    .store(true, Ordering::Release),
            }
        }
        assert_eq!(manager.reap_once().await, 1);
        assert!(state.login_discarded.load(Ordering::Acquire), "{why}");
    }
}

#[cfg(target_os = "macos")]
async fn controller_reaper_case(mode: &str) -> usize {
    let home = home();
    let h = home.path();
    std::fs::write(
        h.join("config.toml"),
        "[computer_use.local_browser]\nenabled=true\n",
    )
    .unwrap();
    let state = Arc::new(FakeState::default());
    state
        .local_target
        .store(mode != "container", Ordering::Release);
    let manager = Arc::new(manager(h, &state, IDLE_TIMEOUT));
    manager.companion.attach_manager(&manager);
    manager.companion.fixture_key([0x28; 32]);
    insert_session(&manager, &state, "alice", ComputerUseConfig::default());
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let report = json!({"session_available":false,"on_console":false,"login_done":false,"locked":true,"idle_seconds":9999.0,"browser_frontmost":false,"input_sequence":1});
    assert_eq!(
        native_start_message(
            &manager,
            &[0x28; 32],
            &nonce,
            json!({"op":"poll","presence":report})
        )
        .await
        .1["ok"],
        true
    );
    assert!(
        !manager.companion.ready(),
        "controller liveness never proves physical presence"
    );
    assert!(
        !attended_call::current("alice"),
        "controller liveness never proves an attended runtime"
    );
    let entry = manager.lookup("alice").unwrap();
    {
        let mut session = entry.lock().await;
        session.last_activity = Instant::now() - Duration::from_secs(121);
        session.deadline = Instant::now() + Duration::from_secs(600);
        if mode == "stale" {
            session.last_activity = Instant::now(); // an active AI cannot keep an offline controller alive
            *session.shared.native_controller_heartbeat.lock().unwrap() =
                Some(Instant::now() - Duration::from_secs(31));
        } else if mode == "deadline" {
            session.deadline = Instant::now() - Duration::from_millis(1);
        }
    }
    manager.reap_once().await
}
#[tokio::test]
#[cfg(target_os = "macos")]
async fn local_controller_heartbeat_keeps_a_manual_login_past_the_tool_idle_limit() {
    assert_eq!(controller_reaper_case("fresh").await, 0);
}
#[tokio::test]
#[cfg(target_os = "macos")]
async fn local_controller_stale_heartbeat_ends_even_with_recent_ai_activity() {
    assert_eq!(controller_reaper_case("stale").await, 1);
}
#[tokio::test]
#[cfg(target_os = "macos")]
async fn local_controller_heartbeat_never_extends_hard_deadlines_or_container_idle() {
    assert_eq!(controller_reaper_case("deadline").await, 1);
    assert_eq!(controller_reaper_case("container").await, 1);
}
#[tokio::test]
#[cfg(target_os = "macos")]
async fn local_controller_without_presence_still_cannot_read_or_write_the_real_backend() {
    let root = home();
    let h = root.path();
    std::fs::write(
        h.join("config.toml"),
        "[computer_use.local_browser]\nenabled=true\n",
    )
    .unwrap();
    write_agent(
        h,
        "alice",
        "[capabilities]\ncomputer_use=true\n[capabilities.computer_use_config]\nlocal_browser=true\n",
    );
    let manager = Arc::new(manager(h, &Arc::new(FakeState::default()), IDLE_TIMEOUT));
    manager.companion.attach_manager(&manager);
    manager.companion.fixture_key([0x28; 32]);
    let report = json!({"session_available":false,"on_console":false,"login_done":false,"locked":true,"idle_seconds":9999.0,"browser_frontmost":false,"input_sequence":1});
    assert_eq!(
        native_start_message(
            &manager,
            &[0x28; 32],
            &uuid::Uuid::new_v4().simple().to_string(),
            json!({"op":"poll","presence":report})
        )
        .await
        .1["ok"],
        true
    );
    let mut backend = local_browser::backend::LocalBrowserBackend::new(
        h.to_owned(),
        "alice".into(),
        ComputerUseConfig::default(),
        manager.companion.clone(),
    );
    backend
        .register("cu-native-controller-no-presence")
        .await
        .unwrap();
    attended::verified_channel_message(
        "line:controller-no-presence",
        "synthetic-human",
        false,
        async {
            assert!(attended::bind_final_route(
                "line:controller-no-presence",
                "line:controller-no-presence",
                "alice",
                "controller-turn"
            ));
            duduclaw_memory::feedback::CURRENT_SESSION_ID
                .scope(
                    Some("line:controller-no-presence".into()),
                    duduclaw_memory::feedback::CURRENT_TURN_ID.scope(
                        Some("controller-turn".into()),
                        async {
                            let proof = attended_call::capture("alice").unwrap();
                            attended_call::request_scope(Some(proof), async {
                                for reason in [
                                    backend.screenshot().await.unwrap_err().to_string(),
                                    backend.page_text().await.unwrap_err().to_string(),
                                    backend.window_title().await.unwrap_err(),
                                    backend
                                        .execute(&ComputerAction::LeftClick { coordinate: [1, 1] })
                                        .await
                                        .unwrap_err()
                                        .to_string(),
                                ] {
                                    assert!(reason.contains("local_presence_Locked"), "{reason}");
                                }
                            })
                            .await;
                        },
                    ),
                )
                .await;
        },
    )
    .await;
    backend.stop().await;
}

#[cfg(target_os = "macos")]
async fn native_start_message(
    manager: &Arc<ComputerUseSessions>,
    key: &[u8; 32],
    nonce: &str,
    body: Value,
) -> (u16, Value) {
    use duduclaw_core::local_browser_channel as wire;
    let timestamp = chrono::Utc::now().timestamp() as u64;
    let bytes = serde_json::to_vec(&body).unwrap();
    let mut signed = HeaderMap::new();
    signed.insert("x-local-time", timestamp.to_string().parse().unwrap());
    signed.insert("x-local-nonce", nonce.parse().unwrap());
    signed.insert(
        "x-local-signature",
        wire::sign(key, false, timestamp, nonce, &bytes)
            .unwrap()
            .parse()
            .unwrap(),
    );
    use tower::ServiceExt;
    let mut request = axum::http::Request::builder()
        .uri(wire::ROUTE)
        .method("POST")
        .extension(axum::extract::ConnectInfo(loopback()))
        .body(axum::body::Body::from(bytes))
        .unwrap();
    request.headers_mut().extend(signed);
    let response = local_browser::companion::router(manager.companion.clone())
        .oneshot(request)
        .await
        .unwrap();
    let status = response.status();
    let signature = response
        .headers()
        .get("x-local-signature")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    if status.is_success() {
        assert!(wire::verify(
            key,
            true,
            timestamp,
            nonce,
            &bytes,
            signature.as_deref().unwrap(),
            timestamp
        ));
    }
    (
        status.as_u16(),
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
#[cfg(target_os = "macos")]
async fn standalone_native_start_is_signed_single_use_owner_bound_and_never_a_chat_identity() {
    for mode in ["normal", "stop", "owner_aba"] {
        let stop_during_start = mode != "normal";
        let home = home();
        let h = home.path();
        std::fs::write(
            h.join("config.toml"),
            "[computer_use.local_browser]\nenabled=true\n",
        )
        .unwrap();
        write_agent(
            h,
            "alice",
            "[capabilities]\ncomputer_use=true\n[capabilities.computer_use_config]\nlocal_browser=true\n",
        );
        let db = rusqlite::Connection::open(h.join("users.db")).unwrap();
        db.execute_batch("CREATE TABLE users(id TEXT,status TEXT,updated_at TEXT); CREATE TABLE user_agent_bindings(user_id TEXT,agent_name TEXT,access_level TEXT,bound_at TEXT); INSERT INTO users VALUES('synthetic-owner','active','initial'); INSERT INTO user_agent_bindings VALUES('synthetic-owner','alice','owner','initial');").unwrap();
        let state = Arc::new(FakeState::default());
        state.local_target.store(true, Ordering::Release);
        if stop_during_start {
            state.start_delay_ms.store(100, Ordering::Release);
        }
        let mut raw = manager(h, &state, IDLE_TIMEOUT);
        raw.local_factory = Some(raw.factory.clone());
        let manager = Arc::new(raw);
        manager.companion.attach_manager(&manager);
        let key = [0x32u8; 32];
        manager.companion.fixture_key(key);
        let fresh = || uuid::Uuid::new_v4().simple().to_string();
        let presence = json!({"session_available":true,"on_console":true,"login_done":true,"locked":false,"idle_seconds":0.0,"browser_frontmost":false,"input_sequence":0});
        assert_eq!(
            native_start_message(
                &manager,
                &key,
                &fresh(),
                json!({"op":"poll","presence":presence})
            )
            .await
            .1["ok"],
            true
        );
        assert_eq!(
            native_start_message(&manager, &key, &fresh(), json!({"op":"continue"}))
                .await
                .1["ok"],
            true
        );
        // A principal/turn field is not accepted even with a genuine fixture
        // native signature; JWT/employee headers alone are tested separately.
        let forged=native_start_message(&manager,&key,&fresh(),json!({"op":"start","employee":"alice","on_behalf_of":"synthetic-owner","turn_id":"invented"})).await;
        assert_eq!(forged.1["code"], "invalid_local_request");
        assert!(
            manager
                .start(
                    "alice",
                    StartRequest {
                        target: SessionTarget::LocalBrowser,
                        ..Default::default()
                    }
                )
                .await
                .is_err(),
            "native consent cannot be borrowed by the regular MCP start"
        );
        let nonce = fresh();
        let request = json!({"op":"start","employee":"alice"});
        assert_eq!(
            native_start_message(&manager, &key, &nonce, request.clone())
                .await
                .1["accepted"],
            true
        );
        assert_eq!(
            native_start_message(&manager, &key, &nonce, request)
                .await
                .0,
            403,
            "signed native intent cannot replay"
        );
        if stop_during_start {
            tokio::time::timeout(Duration::from_secs(2), async {
                while state.start_human.lock().unwrap().is_empty() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            if mode == "stop" {
                assert_eq!(
                    native_start_message(&manager, &key, &fresh(), json!({"op":"stop"}))
                        .await
                        .1["ok"],
                    true
                );
            } else {
                assert!(
                    !h.join("local_browser/alice").exists(),
                    "first permit precedes profile preparation"
                );
                assert!(h.join("agents/alice/state/local_browser_epoch").exists());
                // The same public barrier used before primary-owner writes.
                // No DB fingerprint changes: A→B→A can return to identical rows.
                manager
                    .forget_login("alice", "primary_owner_changed")
                    .await
                    .unwrap();
                manager
                    .forget_login("alice", "primary_owner_changed")
                    .await
                    .unwrap();
            }
        }
        let result = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let answer = native_start_message(
                    &manager,
                    &key,
                    &fresh(),
                    json!({"op":"poll","presence":presence}),
                )
                .await
                .1;
                if answer["start_result"].is_object() {
                    break answer["start_result"].clone();
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            result["ok"], !stop_during_start,
            "{mode}: an old permit must not publish after a barrier"
        );
        assert_eq!(
            *state.start_human.lock().unwrap(),
            vec![false],
            "native OS actor never becomes attended chat"
        );
        if stop_during_start {
            assert!(
                manager.entry("alice").is_none(),
                "Stop invalidates the native permit before session publication"
            );
            assert!(state.stops.load(Ordering::Acquire) > 0);
        } else {
            assert_eq!(
                manager.entry("alice").unwrap().shared.target,
                SessionTarget::LocalBrowser
            );
            let body = serde_json::to_vec(&json!({"op":"screenshot"})).unwrap();
            assert_eq!(
                call(
                    http::router(manager.clone()),
                    loopback(),
                    headers(h, "alice", &body),
                    body
                )
                .await
                .0,
                403,
                "standalone start seeds no AI page-read authority"
            );
            manager.stop("alice", None).await.unwrap();
            db.execute("UPDATE users SET status='suspended'", [])
                .unwrap();
            assert_eq!(
                native_start_message(
                    &manager,
                    &key,
                    &fresh(),
                    json!({"op":"start","employee":"alice"})
                )
                .await
                .1["code"],
                "native_start_active_owner_required"
            );
        }
    }
}

#[tokio::test]
#[cfg(target_os = "macos")]
async fn an_existing_local_frame_needs_new_native_consent_after_transport_requester_or_owner_change()
 {
    let mut observed = Vec::new();
    for mode in ["transport", "requester", "owner_epoch"] {
        let home = home();
        let h = home.path();
        std::fs::create_dir_all(h.join("local_browser/alice/profile")).unwrap();
        let state = Arc::new(FakeState::default());
        state.local_target.store(true, Ordering::Release);
        let manager = Arc::new(manager(h, &state, IDLE_TIMEOUT));
        insert_session(&manager, &state, "alice", ComputerUseConfig::default());
        let screenshot = |reference: String| {
            let bytes =
                serde_json::to_vec(&json!({"op":"screenshot","principal_context":reference}))
                    .unwrap();
            call(
                http::router(manager.clone()),
                loopback(),
                headers(h, "alice", &bytes),
                bytes,
            )
        };
        attended::verified_channel_message(
            "line:local-read-consent",
            "synthetic-human",
            false,
            async {
                assert!(attended::bind_final_route(
                    "line:local-read-consent",
                    "line:local-read-consent",
                    "alice",
                    "local-consent-turn"
                ));
                duduclaw_memory::feedback::CURRENT_SESSION_ID
                    .scope(
                        Some("line:local-read-consent".into()),
                        duduclaw_memory::feedback::CURRENT_TURN_ID.scope(
                            Some("local-consent-turn".into()),
                            async {
                                let _turn = turns::register_current_turn("alice").unwrap();
                                let first = crate::on_behalf_of::ON_BEHALF_OF
                                    .scope(Some("user:synthetic-a".into()), async {
                                        crate::on_behalf_of::context::register(h, "alice").unwrap()
                                    })
                                    .await;
                                state.native_review_approved.store(true, Ordering::Release);
                                assert_eq!(screenshot(first.reference().into()).await.0, 200);
                                state.native_review_approved.store(false, Ordering::Release);
                                assert_eq!(
                                    screenshot(first.reference().into()).await.0,
                                    200,
                                    "same live lease may reuse its unexpired native consent"
                                );
                                let second = match mode {
                                    "requester" => Some(
                                        crate::on_behalf_of::ON_BEHALF_OF
                                            .scope(Some("user:synthetic-b".into()), async {
                                                crate::on_behalf_of::context::register(h, "alice")
                                                    .unwrap()
                                            })
                                            .await,
                                    ),
                                    "transport" => Some(
                                        crate::on_behalf_of::ON_BEHALF_OF
                                            .scope(Some("user:synthetic-a".into()), async {
                                                crate::on_behalf_of::context::register(h, "alice")
                                                    .unwrap()
                                            })
                                            .await,
                                    ),
                                    _ => {
                                        super::super::invalidate_local_browser_owner(h, "alice")
                                            .unwrap();
                                        super::super::invalidate_local_browser_owner(h, "alice")
                                            .unwrap();
                                        None
                                    }
                                };
                                let reference =
                                    second.as_ref().unwrap_or(&first).reference().to_owned();
                                observed.push((mode, screenshot(reference).await.0));
                                assert_eq!(
                                    state.observed_attendance.lock().unwrap().len(),
                                    2,
                                    "a denied new caller must not read page pixels"
                                );
                            },
                        ),
                    )
                    .await;
            },
        )
        .await;
    }
    assert_eq!(
        observed,
        vec![("transport", 409), ("requester", 409), ("owner_epoch", 409)],
        "live chat/proof alone cannot reuse old native page consent"
    );
}

#[tokio::test]
#[cfg(target_os = "macos")]
async fn local_signed_route_requires_own_live_runtime_context_not_upstream_turn_or_principal() {
    let home = home();
    let h = home.path();
    let state = Arc::new(FakeState::default());
    state.local_target.store(true, Ordering::Release);
    let manager = Arc::new(manager(h, &state, IDLE_TIMEOUT));
    insert_session(&manager, &state, "alice", ComputerUseConfig::default());
    let screenshot = |agent: &str, reference: Option<&str>| {
        let bytes=serde_json::to_vec(&json!({"op":"screenshot","turn_id":"human-live", "principal_context":reference,"on_behalf_of":"user:owner"})).unwrap();
        call(
            http::router(manager.clone()),
            loopback(),
            headers(h, agent, &bytes),
            bytes,
        )
    };
    attended::verified_channel_message("line:local-real-route","human",false,async {
        assert!(attended::bind_final_route("line:local-real-route","line:local-real-route","alice","human-live"));
        duduclaw_memory::feedback::CURRENT_SESSION_ID.scope(Some("line:local-real-route".into()),
            duduclaw_memory::feedback::CURRENT_TURN_ID.scope(Some("human-live".into()),
                crate::on_behalf_of::ON_BEHALF_OF.scope(Some("user:owner".into()),async {
                    let turn=turns::register_current_turn("alice").unwrap();
                    assert!(turns::attended_live_for("alice","human-live"));
                    let direct=crate::on_behalf_of::context::register(h,"alice").unwrap();
                    let reference=direct.reference().to_owned();
                    state.native_review_approved.store(true,Ordering::Release);
                    assert_eq!(screenshot("alice",Some(&reference)).await.0,200,"positive actual HTTP route");
                    state.native_review_approved.store(false,Ordering::Release);
                    assert!(!h.join("audit/browser/screenshots").exists(), "local viewport must remain memory-only");
                    let audit=std::fs::read_to_string(h.join("audit/browser/audit.jsonl")).unwrap();
                    let entry:Value=serde_json::from_str(audit.lines().last().unwrap()).unwrap();
                    assert_eq!(entry["details"]["target"],"local_browser");
                    assert!(entry["screenshot_path"].is_null());
                    assert_eq!(screenshot("alice",None).await.0,403,"turn + principal alone have no authority");
                    for source in [crate::cost_telemetry::RequestType::Dispatch,crate::cost_telemetry::RequestType::Cron,crate::cost_telemetry::RequestType::Evolution] {
                        attended_call::runtime_scope(source==crate::cost_telemetry::RequestType::Chat,async {
                            let background=crate::on_behalf_of::context::register(h,"alice").unwrap();
                            assert_eq!(crate::on_behalf_of::context::lookup(h,"alice",background.reference()),Some(Some("user:owner".into())));
                            assert!(turns::attended_live_for("alice","human-live"),"upstream stays alive");
                            let (status,answer)=screenshot("alice",Some(background.reference())).await;
                            assert_eq!(status,403,"{source}: {answer}");
                        }).await;
                    }
                    crate::memory_provenance::DISPATCH_RUN.scope(Some(("upstream".into(),"dispatch".into())),async {
                        let background=crate::on_behalf_of::context::register(h,"alice").unwrap();
                        assert_eq!(screenshot("alice",Some(background.reference())).await.0,403);
                    }).await;
                    crate::runtime::GOAL_ROUND_ATTRIBUTION.scope(crate::runtime::GoalRoundAttribution{episode_id:"goal-local".into(),round:Some(1)},async {
                        let background=crate::on_behalf_of::context::register(h,"alice").unwrap();
                        assert_eq!(screenshot("alice",Some(background.reference())).await.0,403);
                    }).await;
                    crate::claude_runner::REPLY_SENDER.scope(crate::responsibility::HEARTBEAT_SENDER.into(),async {
                        let background=crate::on_behalf_of::context::register(h,"alice").unwrap();
                        assert_eq!(screenshot("alice",Some(background.reference())).await.0,403);
                    }).await;
                    let in_flight=crate::on_behalf_of::context::attended_call(h,"alice",&reference).unwrap();
                    assert!(in_flight.valid_for("alice"));
                    assert!(crate::on_behalf_of::context::attended_call(h,"bob",&reference).is_none());
                    write_agent(h,"alice","[capabilities]\ncomputer_use=true\napproval_required_tools=['computer_screenshot']\n");
                    let (status,denial)=screenshot("alice",Some(&reference)).await;
                    assert_eq!(status,409,"only native review may satisfy local ask; fake backend cannot approve: {denial}");
                    assert_eq!(denial["code"],"approval_denied");
                    assert!(!h.join("approvals.db").exists(),"local ask never creates a dashboard/chat approval");
                    drop(direct);
                    assert!(!in_flight.valid_for("alice"),"runtime exit invalidates even a captured request");
                    assert_eq!(screenshot("alice",Some(&reference)).await.0,403);
                    drop(turn);
                }))).await;
    }).await;
    assert!(state.executed.lock().unwrap().is_empty());
}

#[tokio::test]
#[cfg(unix)]
async fn local_offboarding_cleans_profile_even_with_container_persistence_disabled() {
    let home = home();
    let h = home.path();
    let state = Arc::new(FakeState::default());
    let manager = manager(h, &state, IDLE_TIMEOUT);
    let profile = h.join("local_browser/alice/profile/Default");
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join("Cookies"), "synthetic login").unwrap();
    let before = h.join("agents/alice/state/local_browser_epoch");
    manager
        .forget_login("alice", "owner_changed")
        .await
        .unwrap();
    let first = std::fs::read_to_string(&before).unwrap();
    assert!(!h.join("local_browser/alice/profile").exists());
    // A→B→A must change the epoch even if the owner set returns to A.
    std::fs::create_dir_all(&profile).unwrap();
    manager
        .forget_login("alice", "owner_changed")
        .await
        .unwrap();
    assert_ne!(std::fs::read_to_string(before).unwrap(), first);
}

#[tokio::test]
async fn local_navigation_keeps_modify_block_while_adding_send_block() {
    let home = home();
    let h = home.path();
    for effect in ["modify", "send"] {
        write_agent(
            h,
            "alice",
            &format!(
                "[capabilities]\ncomputer_use=true\naction_rules=[{{effect='{effect}',verdict='block'}}]\n"
            ),
        );
        if effect == "modify" {
            assert!(
                gates::tool_gates(h, "alice", gates::TOOL_NAVIGATE)
                    .await
                    .is_err()
            );
        } else {
            assert!(gates::local_navigation_rule(h, "alice", gates::TOOL_NAVIGATE).is_err());
        }
    }
}
