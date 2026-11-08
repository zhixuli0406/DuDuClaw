use super::*;
fn setup() -> (Arc<MemoryStore>, ModuleGate) {
    let store = Arc::new(MemoryStore::default());
    let gate = ModuleGate::new(store.clone());
    (store, gate)
}
fn reason<T>(result: Result<T, ModuleUnavailable>) -> UnavailableReason {
    match result { Err(e) => e.reason, Ok(_) => panic!("unexpected admission") }
}
#[test]
fn module_lease_illegal_transitions_and_purge() {
    let (_, gate) = setup();
    assert_eq!(reason(gate.transition::<Voice>(ModuleState::Disabled, "test")), UnavailableReason::IllegalTransition);
    gate.transition::<Voice>(ModuleState::Active, "test").unwrap();
    for state in [ModuleState::Active, ModuleState::Disabled, ModuleState::Purging, ModuleState::Purged, ModuleState::Draining { deadline: 0 }] {
        assert_eq!(reason(gate.transition::<Voice>(state, "test")), UnavailableReason::IllegalTransition);
    }
    let (_, gate) = setup();
    gate.transition::<Voice>(ModuleState::Purging, "test").unwrap();
    assert!(gate.acquire::<Voice>(LeaseKind::Rpc).is_err());
    gate.transition::<Voice>(ModuleState::Purged, "test").unwrap();
    assert!(gate.transition::<Voice>(ModuleState::Active, "test").is_err());
    assert!(gate.acquire::<Voice>(LeaseKind::Cleanup).is_ok());
}
#[test]
fn module_lease_disabled_draining_and_q5_reads() {
    for kind in [LeaseKind::Rpc, LeaseKind::Http, LeaseKind::McpCall, LeaseKind::LoopTick, LeaseKind::ChannelHook, LeaseKind::Cli, LeaseKind::Work { work_id: "new".into() }] {
        let (_, gate) = setup();
        assert_eq!(reason(gate.acquire::<Voice>(kind.clone())), UnavailableReason::Disabled);
        gate.transition::<Voice>(ModuleState::Active, "test").unwrap();
        gate.transition::<Voice>(ModuleState::Draining { deadline: i64::MAX }, "test").unwrap();
        assert_eq!(reason(gate.acquire::<Voice>(kind)), UnavailableReason::Draining);
        assert!(gate.acquire::<Voice>(LeaseKind::ReadAccess).is_ok());
        assert!(gate.acquire::<Voice>(LeaseKind::Cleanup).is_ok());
    }
    let (_, gate) = setup();
    assert!(gate.acquire::<Voice>(LeaseKind::ReadAccess).is_ok());
    assert!(gate.acquire::<Voice>(LeaseKind::Cleanup).is_ok());
}
#[test]
fn module_lease_last_guard_drop_and_cross_handle_visibility() {
    let (store, gate) = setup();
    let other = ModuleGate::new(store.clone());
    gate.transition::<Voice>(ModuleState::Active, "test").unwrap();
    let first = gate.acquire::<Voice>(LeaseKind::Work { work_id: "turn".into() }).unwrap();
    let second = other.resume::<Voice>("turn").unwrap();
    gate.transition::<Voice>(ModuleState::Draining { deadline: i64::MAX }, "test").unwrap();
    // Simulate the end of the design's 420-second short-call grace without sleeping.
    store.transaction(ModuleId::Voice, &mut |row| { row.updated_at = now() - SHORT_CALL_GRACE_SECONDS; Ok(()) }).unwrap();
    drop(first);
    assert!(matches!(other.state::<Voice>().unwrap(), ModuleState::Draining { .. }));
    drop(second);
    assert_eq!(other.state::<Voice>().unwrap(), ModuleState::Disabled);
    other.transition::<Voice>(ModuleState::Active, "test").unwrap();
    assert!(other.resume::<Voice>("turn").is_err());
}
#[test]
fn module_lease_empty_drain_observes_short_call_grace() {
    let (store, gate) = setup();
    gate.transition::<Voice>(ModuleState::Active, "test").unwrap();
    let short = gate.acquire::<Voice>(LeaseKind::Rpc).unwrap();
    gate.transition::<Voice>(ModuleState::Draining { deadline: i64::MAX }, "test").unwrap();
    drop(short);
    assert!(matches!(gate.state::<Voice>().unwrap(), ModuleState::Draining { .. }));
    store.transaction(ModuleId::Voice, &mut |row| { row.updated_at = now() - SHORT_CALL_GRACE_SECONDS; Ok(()) }).unwrap();
    assert_eq!(gate.state::<Voice>().unwrap(), ModuleState::Disabled);
}
#[test]
fn module_lease_resume_deadline_and_live_crash_leftovers_fail_closed() {
    let (store, gate) = setup();
    gate.transition::<Voice>(ModuleState::Active, "test").unwrap();
    let work = gate.acquire::<Voice>(LeaseKind::Work { work_id: "turn".into() }).unwrap();
    gate.transition::<Voice>(ModuleState::Draining { deadline: i64::MAX }, "test").unwrap();
    store.transaction(ModuleId::Voice, &mut |row| { row.state = ModuleState::Draining { deadline: now() - 1 }; row.updated_at = 0; Ok(()) }).unwrap();
    assert_eq!(reason(gate.resume::<Voice>("turn")), UnavailableReason::WorkNotFound);
    assert!(matches!(gate.state::<Voice>().unwrap(), ModuleState::Draining { .. }));
    drop(work);
    assert_eq!(gate.state::<Voice>().unwrap(), ModuleState::Disabled);
}
#[cfg(unix)]
#[test]
fn module_lease_dead_pid_reclaimed() {
    let (store, gate) = setup();
    // A child known to have exited, rather than guessing an unused PID.
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let pid = child.id(); child.wait().unwrap();
    store.transaction(ModuleId::Voice, &mut |row| {
        row.state = ModuleState::Draining { deadline: i64::MAX }; row.updated_at = 0;
        row.inflight.push(Inflight { slot: "crashed".into(), work_id: "turn".into(), generation: 0, pid, heartbeat_at: 0 }); Ok(())
    }).unwrap();
    assert_eq!(gate.state::<Voice>().unwrap(), ModuleState::Disabled);
}
#[test]
fn module_lease_store_failure_denies_every_kind() {
    struct Broken;
    impl ModuleStore for Broken {
        fn transaction(&self, id: ModuleId, _: &mut dyn FnMut(&mut ModuleRecord) -> Result<(), ModuleUnavailable>) -> Result<(), ModuleUnavailable> {
            Err(unavailable(id, None, UnavailableReason::StoreUnreadable))
        }
    }
    let gate = ModuleGate::new(Arc::new(Broken));
    for kind in [LeaseKind::Rpc, LeaseKind::ReadAccess, LeaseKind::Cleanup] {
        assert_eq!(reason(gate.acquire::<Voice>(kind)), UnavailableReason::StoreUnreadable);
    }
}
