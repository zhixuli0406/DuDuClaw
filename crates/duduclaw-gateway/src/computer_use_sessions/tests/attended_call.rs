//! Container requests retain their ordinary behavior, while the shared host
//! scope tells origin/link admission which runtime actually made the call.
use super::*;

#[tokio::test]
async fn signed_computer_route_attendance_is_per_runtime_even_with_same_live_agent_turn_and_person() {
    let home=home();let h=home.path();let state=Arc::new(FakeState::default());
    let manager=Arc::new(manager(h,&state,IDLE_TIMEOUT));
    insert_session(&manager,&state,"alice",ComputerUseConfig::default());
    let request=|reference:Option<&str>| {
        let bytes=serde_json::to_vec(&json!({"op":"screenshot","turn_id":"same-live-turn","principal_context":reference,"on_behalf_of":"user:owner"})).unwrap();
        call(http::router(manager.clone()),loopback(),headers(h,"alice",&bytes),bytes)
    };
    attended::verified_channel_message("line:caller-proof","human",false,async {
        assert!(attended::bind_final_route("line:caller-proof","line:caller-proof","alice","same-live-turn"));
        duduclaw_memory::feedback::CURRENT_SESSION_ID.scope(Some("line:caller-proof".into()),
            duduclaw_memory::feedback::CURRENT_TURN_ID.scope(Some("same-live-turn".into()),
                crate::on_behalf_of::ON_BEHALF_OF.scope(Some("user:owner".into()),async {
                    let _turn=turns::register_current_turn("alice").unwrap();
                    let guard=crate::on_behalf_of::context::register(h,"alice").unwrap();
                    assert_eq!(request(Some(guard.reference())).await.0,200);
                    assert_eq!(state.observed_attendance.lock().unwrap().pop(),Some(true));
                    assert_eq!(request(None).await.0,200);
                    assert_eq!(state.observed_attendance.lock().unwrap().pop(),Some(false));
                    for (supplied,expected) in [(Some("same-live-turn"),Some("same-live-turn")),(Some("other-live-turn"),None),(None,None)] {
                        let bytes=serde_json::to_vec(&json!({"op":"action","turn_id":supplied,"principal_context":guard.reference(),"action":{"type":"scroll","x":1,"y":1,"direction":"down","amount":1}})).unwrap();
                        let result=call(http::router(manager.clone()),loopback(),headers(h,"alice",&bytes),bytes).await;
                        assert_eq!(result.0,200,"container behavior stays available: {result:?}");
                        assert_eq!(state.observed_action_turn.lock().unwrap().pop(),Some(expected.map(str::to_owned)),"supplied turn {supplied:?}");
                    }
                    for kind in [crate::cost_telemetry::RequestType::Dispatch,crate::cost_telemetry::RequestType::Cron,crate::cost_telemetry::RequestType::Evolution] {
                        attended_call::runtime_scope(kind==crate::cost_telemetry::RequestType::Chat,async {
                            let background=crate::on_behalf_of::context::register(h,"alice").unwrap();
                            assert_eq!(crate::on_behalf_of::context::lookup(h,"alice",background.reference()),Some(Some("user:owner".into())));
                            assert!(turns::attended_live_for("alice","same-live-turn"));
                            assert_eq!(request(Some(background.reference())).await.0,200);
                            assert_eq!(state.observed_attendance.lock().unwrap().pop(),Some(false),"{kind}");
                        }).await;
                    }
                    crate::memory_provenance::DISPATCH_RUN.scope(Some(("upstream".into(),"run".into())),async {
                        let background=crate::on_behalf_of::context::register(h,"alice").unwrap();
                        assert_eq!(request(Some(background.reference())).await.0,200);
                        assert_eq!(state.observed_attendance.lock().unwrap().pop(),Some(false));
                    }).await;
                    crate::runtime::GOAL_ROUND_ATTRIBUTION.scope(crate::runtime::GoalRoundAttribution{episode_id:"goal-attended".into(),round:None},async {
                        let background=crate::on_behalf_of::context::register(h,"alice").unwrap();
                        assert_eq!(request(Some(background.reference())).await.0,200);
                        assert_eq!(state.observed_attendance.lock().unwrap().pop(),Some(false));
                    }).await;
                    for sender in [crate::responsibility::HEARTBEAT_SENDER,"workflow"] {
                        crate::claude_runner::REPLY_SENDER.scope(sender.into(),async {
                            let background=crate::on_behalf_of::context::register(h,"alice").unwrap();
                            assert_eq!(request(Some(background.reference())).await.0,200);
                            assert_eq!(state.observed_attendance.lock().unwrap().pop(),Some(false));
                        }).await;
                    }
                    let captured=crate::on_behalf_of::context::attended_call(h,"alice",guard.reference()).unwrap();
                    let reference=guard.reference().to_owned();drop(guard);
                    assert!(!captured.valid_for("alice"));
                    assert_eq!(request(Some(&reference)).await.0,200);
                    assert_eq!(state.observed_attendance.lock().unwrap().pop(),Some(false));
                }))).await;
    }).await;
}
