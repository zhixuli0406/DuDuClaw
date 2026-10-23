//! Signed HTTP requests recover the human only from host turn registrations.
use super::*;

#[tokio::test]
async fn signed_start_and_action_approvals_keep_the_registered_person_and_refuse_self_approval() {
    for start in [true, false] {
        let tmp = home();
        let h = tmp.path();
        write_config(h, "[approval]\nforbid_self_approval = true\n");
        let db = duduclaw_auth::UserDb::new(&h.join("users.db")).unwrap();
        let user = db
            .create_user(
                "approver@test.local",
                "Approver",
                "TeamPass-12345",
                duduclaw_auth::UserRole::Admin,
            )
            .unwrap();
        db.bind_channel_identity(&user.id, "line", "U-obo", true)
            .unwrap();
        let expected = format!("user:{}", user.id);
        let _turn = crate::knowledge::turn_requesters::register(
            "alice",
            "obo-route-turn",
            duduclaw_knowledge::requester::RequesterIdentity::DashboardUser { user_id: user.id },
        )
        .unwrap();
        let state = Arc::new(FakeState::default());
        let mgr = Arc::new(manager(h, &state, IDLE_TIMEOUT));
        if !start {
            insert_session(&mgr, &state, "alice", ComputerUseConfig::default());
        }
        write_agent(
            h,
            "alice",
            "[capabilities]\ncomputer_use = true\napproval_required_tools = [\"computer_session_start\", \"computer_click\"]\n",
        );
        let body = if start {
            json!({"op":"start", "turn_id":"obo-route-turn", "on_behalf_of":"user:forged"})
        } else {
            json!({"op":"action", "turn_id":"obo-route-turn", "on_behalf_of":"user:forged",
                   "action":{"type":"click", "x":1, "y":1}})
        };
        let bytes = serde_json::to_vec(&body).unwrap();
        let decide = async {
            let broker = crate::approval::ApprovalBroker::open(h).unwrap();
            let rec = tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    if let Some(rec) = broker
                        .list_pending(Some("alice"))
                        .await
                        .unwrap()
                        .into_iter()
                        .next()
                    {
                        break rec;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("signed route should raise an approval");
            assert_eq!(rec.on_behalf_of.as_deref(), Some(expected.as_str()));
            let refusal =
                crate::approval_notify::apply_decision(h, "line", "U-obo", rec.id.as_str(), true)
                    .await
                    .unwrap_err();
            assert_eq!(refusal, crate::on_behalf_of::self_approval::REFUSAL);
            assert_eq!(
                broker.get(&rec.id).await.unwrap().unwrap().status,
                crate::approval::ApprovalStatus::Pending
            );
            crate::approval_notify::apply_decision(h, "line", "U-obo", rec.id.as_str(), false)
                .await
                .unwrap();
        };
        let ((status, result), ()) = tokio::join!(
            call(
                http::router(mgr),
                loopback(),
                headers(h, "alice", &bytes),
                bytes
            ),
            decide
        );
        assert_ne!(status, 200, "{result}");
        assert_eq!(state.starts.load(Ordering::SeqCst), 0);
        assert!(state.executed.lock().unwrap().is_empty());
    }
}

#[test]
fn only_the_same_employees_registered_turn_can_name_a_request_principal() {
    let home = tempfile::tempdir().unwrap();
    let _guard = crate::knowledge::turn_requesters::register(
        "alice",
        "obo-owned-turn",
        duduclaw_knowledge::requester::RequesterIdentity::DashboardUser {
            user_id: "real-person".into(),
        },
    )
    .unwrap();
    for turn in [None, Some("unknown-turn"), Some("obo-owned-turn")] {
        let value = json!({"op":"start", "turn_id":turn, "on_behalf_of":"user:forged"});
        let request = serde_json::from_value(value).unwrap();
        let alice = http::request_principal(home.path(), "alice", &request);
        assert_eq!(
            alice.as_deref(),
            (turn == Some("obo-owned-turn")).then_some("user:real-person")
        );
        assert!(http::request_principal(home.path(), "bob", &request).is_none());
    }
}
