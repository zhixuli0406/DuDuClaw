//! Tests of the shared operator-CLI approval gate.

use super::*;

fn no_notice(_: &ApprovalRecord, _: bool, _: &str) -> String {
    String::new()
}

fn no_hook(_: &Path, _: &ApprovalRecord, _: usize) {}

const TEST_SPEC: KindSpec = KindSpec {
    kind: "operator_gate_test_kind",
    validity: Validity::Fixed(30),
    consume: Consume::Once,
    reminders: true,
    max_pending_per_target: 3,
    max_pending_per_kind: 5,
    push_cap_per_hour: Some(2),
    legacy_scope_key: "target",
    admin_refusal: "admin only",
    expired_text: "expired",
    channel_refusal: None,
    notice: no_notice,
    on_push_suppressed: no_hook,
};

const NEVER_SPEC: KindSpec = KindSpec {
    kind: "operator_gate_test_never",
    validity: Validity::UntilRequestExpiry,
    consume: Consume::Never,
    ..TEST_SPEC
};

fn bind<'a>(target: &'a str, digest: &str, state: &str) -> Binding<'a> {
    Binding {
        action: "act",
        target,
        request_digest: digest.into(),
        state: state.into(),
        state_policy: StatePolicy::MustMatch,
        push_scope: None,
    }
}

fn filing() -> Filing<'static> {
    Filing {
        agent_id: "alice",
        summary: "card",
        extra: json!({"feature": "x"}),
        ttl_secs: 600,
    }
}

async fn run(b: &ApprovalBroker, spec: &KindSpec, bind: &Binding<'_>) -> Gate {
    gate(b, spec, bind, filing(), valid_minutes(spec, Path::new("/nonexistent")), Utc::now())
        .await
        .unwrap()
        .0
}

fn broker() -> (tempfile::TempDir, ApprovalBroker) {
    let home = tempfile::tempdir().unwrap();
    let b = ApprovalBroker::open(home.path()).unwrap();
    (home, b)
}

fn requested(g: Gate) -> ApprovalId {
    match g {
        Gate::Requested(id) => id,
        other => panic!("expected a new request, got {other:?}"),
    }
}

#[tokio::test]
async fn identical_requests_merge_and_carry_the_binding() {
    let (_h, b) = broker();
    let first = requested(run(&b, &TEST_SPEC, &bind("t1", "d", "s1")).await);
    assert_eq!(
        run(&b, &TEST_SPEC, &bind("t1", "d", "s1")).await,
        Gate::Pending(first.clone())
    );
    let rec = b.get(&first).await.unwrap().unwrap();
    assert_eq!(rec.payload["feature"], "x");
    assert_eq!(rec.payload["requested_by"], UNVERIFIED_ACTOR);
    let stored = binding_of(&rec).unwrap();
    assert_eq!(stored.target, "t1");
    assert_eq!(stored.push_scope, "t1");
}

#[tokio::test]
async fn a_state_change_voids_the_waiting_card_and_files_a_new_one() {
    let (_h, b) = broker();
    let first = requested(run(&b, &TEST_SPEC, &bind("t1", "d", "s1")).await);
    let second = requested(run(&b, &TEST_SPEC, &bind("t1", "d", "s2")).await);
    assert_ne!(first, second);
    let old = b.get(&first).await.unwrap().unwrap();
    assert_eq!(old.status, ApprovalStatus::Invalidated);
    assert_eq!(old.invalidated_reason.as_deref(), Some("state_changed"));
}

#[tokio::test]
async fn different_digests_are_separate_and_capped_per_target_then_per_kind() {
    let (_h, b) = broker();
    for d in ["a", "b", "c"] {
        requested(run(&b, &TEST_SPEC, &bind("t1", d, "s")).await);
    }
    assert_eq!(
        run(&b, &TEST_SPEC, &bind("t1", "d", "s")).await,
        Gate::Throttled {
            waiting: 3,
            scope: ThrottleScope::Target
        }
    );
    for t in ["t2", "t3"] {
        requested(run(&b, &TEST_SPEC, &bind(t, "a", "s")).await);
    }
    assert_eq!(
        run(&b, &TEST_SPEC, &bind("t4", "a", "s")).await,
        Gate::Throttled {
            waiting: 5,
            scope: ThrottleScope::Kind
        }
    );
}

#[tokio::test]
async fn only_a_fresh_dashboard_approval_for_the_same_state_proceeds_once() {
    let (_h, b) = broker();
    let id = requested(run(&b, &TEST_SPEC, &bind("t1", "d", "s")).await);
    b.decide(&id, true, "channel:telegram:1").await.unwrap();
    let again = requested(run(&b, &TEST_SPEC, &bind("t1", "d", "s")).await);
    let old = b.get(&id).await.unwrap().unwrap();
    assert_eq!(old.invalidated_reason.as_deref(), Some("not_dashboard_decision"));

    b.decide(&again, true, "dashboard:admin").await.unwrap();
    let Gate::Proceed(claim) = run(&b, &TEST_SPEC, &bind("t1", "d", "s")).await else {
        panic!("approved request must proceed");
    };
    assert_eq!(claim.id, again);
    assert!(claim.still_bound("s") && !claim.still_bound("s2"));
    let used = b.get(&again).await.unwrap().unwrap();
    assert!(used.invalidated_reason.unwrap().starts_with("consumed:"));
    // Used up: the same command files a new request.
    requested(run(&b, &TEST_SPEC, &bind("t1", "d", "s")).await);
}

#[tokio::test]
async fn an_approval_older_than_the_window_is_void() {
    let (_h, b) = broker();
    let id = requested(run(&b, &TEST_SPEC, &bind("t1", "d", "s")).await);
    b.decide(&id, true, "dashboard:admin").await.unwrap();
    let g = gate(&b, &TEST_SPEC, &bind("t1", "d", "s"), filing(), Some(0), Utc::now())
        .await
        .unwrap()
        .0;
    requested(g);
    let old = b.get(&id).await.unwrap().unwrap();
    assert_eq!(old.invalidated_reason.as_deref(), Some("approval_expired"));
}

#[tokio::test]
async fn two_concurrent_appliers_get_one_proceed() {
    let (_h, b) = broker();
    let id = requested(run(&b, &TEST_SPEC, &bind("t1", "d", "s")).await);
    b.decide(&id, true, "dashboard:admin").await.unwrap();
    let b1 = bind("t1", "d", "s");
    let b2 = bind("t1", "d", "s");
    let (x, y) = tokio::join!(run(&b, &TEST_SPEC, &b1), run(&b, &TEST_SPEC, &b2));
    let proceeds = [&x, &y]
        .iter()
        .filter(|g| matches!(g, Gate::Proceed(_)))
        .count();
    assert_eq!(proceeds, 1, "{x:?} {y:?}");
}

#[tokio::test]
async fn pending_only_keeps_an_approval_across_a_state_change() {
    let (_h, b) = broker();
    let mut first = bind("batch", "d", "items-1");
    first.state_policy = StatePolicy::PendingOnly;
    let id = requested(run(&b, &TEST_SPEC, &first).await);
    b.decide(&id, true, "dashboard:admin").await.unwrap();
    let mut later = bind("batch", "d", "items-2");
    later.state_policy = StatePolicy::PendingOnly;
    assert!(matches!(run(&b, &TEST_SPEC, &later).await, Gate::Proceed(_)));
}

#[tokio::test]
async fn never_consumed_kinds_proceed_without_spending_and_verdict_reads_them() {
    let (_h, b) = broker();
    let id = requested(run(&b, &NEVER_SPEC, &bind("plan", "h1", "")).await);
    let v = |d: &'static str| {
        let b = b.clone();
        async move {
            verdict(&b, &NEVER_SPEC, "act", "plan", d, None, Utc::now())
                .await
                .unwrap()
        }
    };
    assert_eq!(v("h1").await, Verdict::Pending { id: id.clone() });
    b.decide(&id, true, "dashboard:admin").await.unwrap();
    assert_eq!(v("h1").await, Verdict::Approved { id: id.clone() });
    assert_eq!(v("h2").await, Verdict::Mismatch { id: id.clone() });
    assert!(matches!(run(&b, &NEVER_SPEC, &bind("plan", "h1", "")).await, Gate::Proceed(_)));
    assert_eq!(v("h1").await, Verdict::Approved { id });
    assert_eq!(
        verdict(&b, &NEVER_SPEC, "act", "other", "h1", None, Utc::now())
            .await
            .unwrap(),
        Verdict::Missing
    );
}

#[tokio::test]
async fn rows_without_a_binding_are_neither_matched_nor_counted() {
    let (_h, b) = broker();
    for _ in 0..6 {
        b.request(
            "alice",
            TEST_SPEC.kind,
            "legacy",
            json!({"action": "act", "target": "t1"}),
            600,
        )
        .await
        .unwrap();
    }
    requested(run(&b, &TEST_SPEC, &bind("t1", "d", "s")).await);
}

#[tokio::test]
async fn pushes_are_capped_per_scope_for_registered_kinds_only() {
    let home = tempfile::tempdir().unwrap();
    let b = ApprovalBroker::open(home.path()).unwrap();
    let spec = spec_for(crate::computer_workspaces::cli_approval::ACTION_KIND).unwrap();
    let ws = "ws-0123456789abcdef0123456789abcdef";
    let mut ids = Vec::new();
    for n in 0..3 {
        let d = format!("d{n}");
        let bd = Binding {
            action: "renew",
            target: ws,
            request_digest: d,
            state: "s".into(),
            state_policy: StatePolicy::MustMatch,
            push_scope: None,
        };
        ids.push(file(&b, spec, &bd, filing()).await.unwrap());
    }
    for id in &ids[..2] {
        b.set_notify_target_for_test(id, "telegram", "1").await.unwrap();
    }
    let third = b.get(&ids[2]).await.unwrap().unwrap();
    assert!(!push_allowed(home.path(), &third).await);
    let mut other = third.clone();
    other.action_kind = "some_other_kind".into();
    assert!(push_allowed(home.path(), &other).await);
}

#[test]
fn the_registry_feeds_reminders_and_dashboard_only_kinds() {
    for spec in OPERATOR_CLI_KINDS {
        assert!(crate::approval_notify::is_dashboard_only_kind(spec.kind));
        assert_eq!(
            crate::approval_notify::dashboard_only_expired_text(spec.kind),
            spec.expired_text
        );
        assert_eq!(no_reminder(spec.kind), !spec.reminders);
    }
    assert!(no_reminder(crate::responsibility::operator_gate::ACTION_KIND));
    assert!(!no_reminder(crate::channel_ingress::cli_approval::ACTION_KIND));
    assert!(spec_for("tool_call").is_none());
}
