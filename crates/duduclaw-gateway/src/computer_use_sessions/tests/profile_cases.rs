use super::*;

#[test]
fn only_safe_normal_endings_can_preserve_logins() {
    for reason in [
        EndReason::Stopped,
        EndReason::ThreatRed,
        EndReason::CapabilityRevoked,
        EndReason::LeaseLost,
        EndReason::ResumeFailed,
    ] {
        assert!(!preserve_login_on_end(
            reason,
            ThreatLevel::Green,
            false,
            true
        ));
    }
    for reason in [
        EndReason::Requested,
        EndReason::Deadline,
        EndReason::Idle,
        EndReason::OperatorStopped,
    ] {
        assert!(preserve_login_on_end(
            reason,
            ThreatLevel::Green,
            false,
            false
        ));
        assert!(!preserve_login_on_end(
            reason,
            ThreatLevel::Red,
            false,
            true
        ));
        assert!(!preserve_login_on_end(
            reason,
            ThreatLevel::Green,
            true,
            false
        ));
        assert!(preserve_login_on_end(
            reason,
            ThreatLevel::Green,
            true,
            true
        ));
    }
}

#[tokio::test]
async fn emergency_stop_and_red_end_without_exporting_current_login() {
    for red in [false, true] {
        let tmp = home();
        let state = Arc::new(FakeState::default());
        let mgr = manager(tmp.path(), &state, IDLE_TIMEOUT);
        insert_session(&mgr, &state, "alice", ComputerUseConfig::default());
        if red {
            std::fs::write(tmp.path().join("threat_level"), "RED").unwrap();
        } else {
            mgr.lookup("alice")
                .unwrap()
                .lock()
                .await
                .backend
                .control()
                .stopped
                .store(true, Ordering::SeqCst);
        }
        assert_eq!(mgr.reap_once().await, 1);
        assert!(state.login_discarded.load(Ordering::SeqCst));
    }
}

#[tokio::test]
async fn changed_login_owner_stops_tools_and_idle_sessions() {
    let tmp = home();
    let state = Arc::new(FakeState::default());
    let mgr = manager(tmp.path(), &state, IDLE_TIMEOUT);
    insert_session(&mgr, &state, "alice", ComputerUseConfig::default());
    state.owner_changed.store(true, Ordering::SeqCst);
    assert_eq!(
        mgr.screenshot("alice", None).await.unwrap_err().code,
        ErrorCode::SessionEnded
    );
    assert!(mgr.is_empty());
    assert!(state.executed.lock().unwrap().is_empty());
    insert_session(&mgr, &state, "alice", ComputerUseConfig::default());
    assert_eq!(mgr.reap_once().await, 1);
    assert!(mgr.is_empty());
}

#[tokio::test]
async fn reset_discards_the_running_profile_before_stopping() {
    let tmp = home();
    let state = Arc::new(FakeState::default());
    let mgr = manager(tmp.path(), &state, IDLE_TIMEOUT);
    insert_session(&mgr, &state, "alice", ComputerUseConfig::default());
    mgr.reset_login("alice", &duduclaw_auth::UserContext::admin_fallback())
        .await
        .unwrap();
    assert!(mgr.is_empty());
    assert_eq!(state.stops.load(Ordering::SeqCst), 1);
    assert!(state.login_discarded.load(Ordering::SeqCst));
}
