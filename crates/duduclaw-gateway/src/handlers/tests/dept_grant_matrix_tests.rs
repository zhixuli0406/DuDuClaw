use super::*;
use crate::dept_grants::methods::{coverage_for, Coverage, METHOD_COVERAGE};
use crate::dept_grants::tests::{fixture, Fixture};
use duduclaw_auth::acl::ContextOrigin;
use std::future::Future;
use std::pin::Pin;

fn assert_permission_denied(method: &str, frame: WsFrame) {
    match frame {
        WsFrame::Response { ok: false, error: Some(Value::String(error)), .. } => {
            // require_role returns exactly "permission denied". Scoped record
            // handlers may wrap that phrase, e.g. reminders' unavailable/deny.
            assert!(error.to_ascii_lowercase().contains("permission denied"),
                "{method}: expected permission refusal, got {error:?}");
        }
        other => panic!("{method}: expected permission refusal, got {other:?}"),
    }
}

fn assert_success(method: &str, frame: WsFrame) {
    assert!(matches!(frame, WsFrame::Response { ok: true, .. }),
        "{method}: expected successful RPC, got {frame:?}");
}

fn admin_only_params(method: &str) -> Value {
    match method {
        "autopilot.create" => json!({
            "name": "matrix rule", "trigger_event": "task_created", "conditions": {},
            "action": {"type": "notify", "channel": "webchat", "chat_id": "matrix", "text": "matrix"}, "agent_id": "a1",
        }),
        "autopilot.update" | "autopilot.remove" | "autopilot.history" => {
            json!({"rule_id": "matrix-rule", "enabled": false, "agent_id": "a1"})
        }
        "skills.install" => json!({"agent_id": "a1", "source": "matrix-skill", "name": "matrix-skill"}),
        _ => json!({"agent_id": "a1", "days": 7, "limit": 10}),
    }
}

#[tokio::test]
async fn admin_only_rows_refuse_manager_with_all_department_grants() {
    let f = fixture().await;
    let mut checked = 0;
    for &(method, _, coverage) in METHOD_COVERAGE {
        if coverage != Coverage::AdminOnly { continue; }
        let frame = Box::pin(f.h.handle(method, admin_only_params(method), &f.mgr_a)).await;
        assert_permission_denied(method, frame);
        checked += 1;
    }
    assert!(checked > 0, "AdminOnly matrix must not silently become empty");
}

#[tokio::test]
async fn admin_controls_succeed_through_the_same_rpc_path() {
    let f = fixture().await;
    // Both support empty telemetry; these are real allow controls,
    // rather than merely accepting an arbitrary non-permission error.
    for method in ["cost.summary", "cost.by_role"] {
        assert_eq!(coverage_for(method).map(|(_, c)| c), Some(Coverage::AdminOnly));
        assert_permission_denied(method,
            Box::pin(f.h.handle(method, admin_only_params(method), &f.mgr_a)).await);
        assert_success(method,
            Box::pin(f.h.handle(method, admin_only_params(method), &f.admin)).await);
    }
}

// Each case gets a fresh home and record setup, so remove/run/toggle calls
// cannot contaminate later cases. Setup should create records for the named
// target owned by another principal (not mgrA), avoiding creator shortcuts.
// Example setup signature: |f, agent| Box::pin(async move { /* seed record */ }).
// The outcome hook supports list RPCs: check returned rows for visibility
// instead of expecting an error when a handler intentionally filters a list.
// Mutation rows can use assert_scoped_rpc_outcome directly.
#[allow(dead_code)]
async fn assert_scoped<P, S, C>(method: &str, params_for_agent: P, setup: S, check: C)
where
    P: Fn(&str) -> Value,
    S: for<'a> Fn(&'a Fixture, &'a str) -> Pin<Box<dyn Future<Output = ()> + 'a>>,
    C: Fn(&str, WsFrame, bool),
{
    assert!(matches!(coverage_for(method), Some((_, Coverage::Scoped(_)))),
        "{method}: matrix helper requires a Scoped row");
    for case in ["allowed", "other_department", "main", "no_grants", "employee", "member_key", "other_origin", "moved_department"] {
        let f = fixture().await;
        let agent = match case {
            "other_department" => "b1",
            "main" => "main",
            _ => "a1",
        };
        setup(&f, agent).await;
        let mut ctx = match case {
            "no_grants" => f.mgr_b.clone(),
            "employee" => f.emp.clone(),
            _ => f.mgr_a.clone(),
        };
        match case {
            "member_key" => ctx.origin = ContextOrigin::MemberKey,
            "other_origin" => ctx.origin = ContextOrigin::Other,
            "moved_department" => {
                let path = f.home.join("agents/a1/agent.toml");
                let raw = std::fs::read_to_string(&path).unwrap();
                assert!(raw.contains("department = 'sales'"));
                std::fs::write(path, raw.replace("department = 'sales'", "department = 'mfg'")).unwrap();
            }
            _ => {}
        }
        let frame = Box::pin(f.h.handle(method, params_for_agent(agent), &ctx)).await;
        // Include the case in failures emitted by the supplied outcome hook.
        check(&format!("{method} [{case}]"), frame, case == "allowed");
    }
}

#[allow(dead_code)]
fn assert_scoped_rpc_outcome(label: &str, frame: WsFrame, allowed: bool) {
    if allowed { assert_success(label, frame); }
    else { assert_permission_denied(label, frame); }
}

#[tokio::test]
#[ignore = "schedule, skill and cost packages add Scoped rows after wiring their grants"]
async fn scoped_method_matrix_rows_pending_wiring() {
    // Add schedule, skill and cost rows using assert_scoped with record setup
    // hooks and, for filtered list methods, a payload visibility assertion.
    panic!("schedule, skill and cost matrix rows must be supplied by their wiring packages");
}
