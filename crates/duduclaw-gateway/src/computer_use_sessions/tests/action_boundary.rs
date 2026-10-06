//! Host-controlled waits must not reuse a stale approved GUI frame.
use super::*;

fn barrier(
    mgr: &ComputerUseSessions,
    phase: &'static str,
) -> (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>) {
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    *mgr.action_boundary_pause.lock().unwrap() = Some((phase, entered.clone(), release.clone()));
    (entered, release)
}

fn another_frame() -> String {
    use base64::Engine;
    use image::ImageEncoder;
    let img = image::RgbaImage::from_pixel(4, 3, image::Rgba([10, 20, 30, 255]));
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(img.as_raw(), 4, 3, image::ExtendedColorType::Rgba8)
        .unwrap();
    base64::engine::general_purpose::STANDARD.encode(png)
}

fn high_risk_config() -> ComputerUseConfig {
    ComputerUseConfig {
        allowed_apps: vec!["trusted-app".into()],
        ..Default::default()
    }
}

#[tokio::test]
async fn frame_or_focus_drift_after_each_long_wait_never_clicks_and_requires_new_approval() {
    for phase in ["after_claim", "after_audit", "after_begin"] {
        for change_frame in [false, true] {
            let tmp = home();
            let state = Arc::new(FakeState::default());
            let asked = Arc::new(AtomicU32::new(0));
            let mgr = manager_asking(tmp.path(), &state, IDLE_TIMEOUT, asked.clone());
            let session = insert_session(&mgr, &state, "alice", high_risk_config());
            let (entered, release) = barrier(&mgr, phase);
            let req = click(10, 20);
            let mutate = async {
                entered.notified().await;
                assert!(state.executed.lock().unwrap().is_empty());
                if change_frame {
                    *state.frame.lock().unwrap() = Some(another_frame());
                } else {
                    *state.title.lock().unwrap() = Some(Ok("different-window".into()));
                }
                release.notify_one();
            };
            let (result, ()) = tokio::join!(
                mgr.action("alice", Some(&session), Some("yes"), &req),
                mutate
            );
            assert_eq!(
                result.unwrap_err().code,
                ErrorCode::ConfirmationDenied,
                "{phase}/{change_frame}"
            );
            assert!(
                state.executed.lock().unwrap().is_empty(),
                "{phase}/{change_frame}: zero clicks"
            );
            assert_eq!(mgr.lookup("alice").unwrap().lock().await.actions_used, 0);
            let broker = crate::approval::ApprovalBroker::open(tmp.path()).unwrap();
            let ops = broker.list_operations().await.unwrap();
            assert_eq!(ops.len(), 1);
            assert_eq!(ops[0].state, crate::approval::OperationState::Failed);
            assert_eq!(ops[0].receipt.as_ref().unwrap()["backend_invoked"], false);
            assert_eq!(ops[0].receipt.as_ref().unwrap()["observations_only"], true);
            let old_id = crate::approval::ApprovalId::from(ops[0].approval_id.clone());
            assert_eq!(
                broker.get(&old_id).await.unwrap().unwrap().status,
                crate::approval::ApprovalStatus::Invalidated
            );
            mgr.action("alice", Some(&session), Some("yes"), &req)
                .await
                .unwrap();
            assert_eq!(
                asked.load(Ordering::SeqCst),
                2,
                "the new frame needs a new request"
            );
            assert_eq!(state.executed.lock().unwrap().len(), 1);
            assert_eq!(broker.list_operations().await.unwrap().len(), 2);
        }
    }
}

#[tokio::test]
async fn live_gates_after_durable_begin_refuse_before_any_backend_action() {
    for interruption in [
        "stop",
        "pause",
        "red",
        "yellow",
        "revoke",
        "tool_denied",
        "policy",
        "deadline",
    ] {
        let tmp = home();
        let state = Arc::new(FakeState::default());
        let mgr = manager(tmp.path(), &state, IDLE_TIMEOUT);
        let session = insert_session(&mgr, &state, "alice", high_risk_config());
        let control = mgr.entry("alice").unwrap().control;
        let deadline = Instant::now() + Duration::from_secs(1);
        if interruption == "deadline" {
            mgr.lookup("alice").unwrap().lock().await.deadline = deadline;
        }
        let (entered, release) = barrier(&mgr, "after_begin");
        let req = click(10, 20);
        let mutate = async {
            entered.notified().await;
            match interruption {
                "stop" => control.stopped.store(true, Ordering::Release),
                "pause" => control.paused.store(true, Ordering::Release),
                "red" => std::fs::write(tmp.path().join("threat_level"), "RED").unwrap(),
                "yellow" => std::fs::write(tmp.path().join("threat_level"), "YELLOW").unwrap(),
                "revoke" => {
                    write_agent(tmp.path(), "alice", "[capabilities]\ncomputer_use=false\n")
                }
                "tool_denied" => write_agent(
                    tmp.path(),
                    "alice",
                    "[capabilities]\ncomputer_use=true\ndenied_tools=['computer_click']\n",
                ),
                "policy" => std::fs::write(
                    tmp.path().join("agents/alice/CONTRACT.toml"),
                    "must_not=['new policy']\n",
                )
                .unwrap(),
                "deadline" => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    tokio::time::sleep(remaining + Duration::from_millis(10)).await;
                }
                _ => unreachable!(),
            }
            release.notify_one();
        };
        let (result, ()) = tokio::join!(
            mgr.action("alice", Some(&session), Some("yes"), &req),
            mutate
        );
        assert!(result.is_err(), "{interruption}");
        assert!(state.executed.lock().unwrap().is_empty(), "{interruption}");
        let broker = crate::approval::ApprovalBroker::open(tmp.path()).unwrap();
        let ops = broker.list_operations().await.unwrap();
        assert_eq!(ops.len(), 1, "{interruption}");
        assert_eq!(
            ops[0].state,
            crate::approval::OperationState::Failed,
            "{interruption}"
        );
        assert_eq!(ops[0].receipt.as_ref().unwrap()["backend_invoked"], false);
    }
}

#[tokio::test]
async fn low_risk_action_also_refuses_frame_drift_after_audit() {
    let tmp = home();
    let state = Arc::new(FakeState::default());
    let mgr = manager(tmp.path(), &state, IDLE_TIMEOUT);
    insert_session(&mgr, &state, "alice", ComputerUseConfig::default());
    let (entered, release) = barrier(&mgr, "after_audit");
    let req = click(10, 20);
    let mutate = async {
        entered.notified().await;
        *state.frame.lock().unwrap() = Some(another_frame());
        release.notify_one();
    };
    let (result, ()) = tokio::join!(mgr.action("alice", None, None, &req), mutate);
    assert_eq!(result.unwrap_err().code, ErrorCode::ConfirmationDenied);
    assert!(state.executed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn concurrent_callers_cannot_interleave_actions_in_one_session() {
    let tmp = home();
    let state = Arc::new(FakeState::default());
    let mgr = manager(tmp.path(), &state, IDLE_TIMEOUT);
    let session = insert_session(&mgr, &state, "alice", high_risk_config());
    let (entered, release) = barrier(&mgr, "after_audit");
    let req = click(10, 20);
    let competing = async {
        entered.notified().await;
        let mut second = Box::pin(mgr.action("alice", Some(&session), Some("yes"), &req));
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut second)
                .await
                .is_err()
        );
        assert!(state.executed.lock().unwrap().is_empty());
        assert_eq!(
            mgr.action("bob", Some(&session), None, &req)
                .await
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
        release.notify_one();
        second.await.unwrap();
    };
    let (first, ()) = tokio::join!(
        mgr.action("alice", Some(&session), Some("yes"), &req),
        competing
    );
    first.unwrap();
    assert_eq!(state.executed.lock().unwrap().len(), 2);
}
