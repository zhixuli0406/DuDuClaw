//! Isolation revocation at the real approval/operation connection mutex.
use super::*;
use std::{future::Future, sync::{Arc, atomic::{AtomicBool, Ordering}}, task::Poll};
use duduclaw_mcp_boundary::io_authority;

#[tokio::test]
async fn revoked_consumers_and_operation_writers_leave_real_database_unchanged() {
    for operation in 0..5 {
        let broker = broker();
        let id = ApprovalId::from("approval".to_string());
        {
            let conn = broker.store.conn.lock().await;
            conn.execute("INSERT INTO approvals(id,agent_id,action_kind,summary,payload,status,created_at,ttl_seconds)
                VALUES('approval','alice','test','test','{}','approved','2026-11-05T00:00:00Z',3600)", []).unwrap();
            conn.execute("INSERT INTO approval_operations(operation_id,run_id,step_key,approval_id,binding_json,payload_json,
                state,fence,lease_owner,lease_token,lease_until) VALUES('operation','run','step','approval',
                '{\"resume_handler\":\"live_only_v1\"}','{}',?1,1,'owner','token',?2)",
                params![if operation == 4 { "uncertain" } else { "executing" }, if operation == 1 { 1 } else { Utc::now().timestamp()+120 }]).unwrap();
        }
        let live = Arc::new(AtomicBool::new(true));
        let held = broker.store.conn.lock().await;
        let request = io_authority::scope(crate::isolation_io_test_support::context(live.clone()), async {
            match operation {
                0 => broker.consume_approved(&id, "test").await.map(|_| ()),
                1 => broker.recover_operations().await.map(|_| ()),
                2 => broker.invalidate_live_on_restart().await,
                3 => broker.settle_operation(&OperationClaim { operation_id:"operation".into(),token:"token".into(),fence:1,owner:"owner".into() },
                    OperationState::Failed, None, Some("failed")).await,
                _ => broker.resolve_uncertain("operation", 1, false, json!({"observed":"failure"}), "operator", "test").await,
            }
        });
        tokio::pin!(request);
        std::future::poll_fn(|cx| { assert!(request.as_mut().poll(cx).is_pending()); Poll::Ready(()) }).await;
        live.store(false, Ordering::SeqCst);
        drop(held);
        assert!(request.await.unwrap_err().contains("revoked"), "operation {operation}");
        let conn = broker.store.conn.lock().await;
        assert_eq!(conn.query_row("SELECT status FROM approvals WHERE id='approval'", [], |r|r.get::<_,String>(0)).unwrap(), "approved");
        assert_eq!(conn.query_row("SELECT state FROM approval_operations WHERE operation_id='operation'", [], |r|r.get::<_,String>(0)).unwrap(),
            if operation == 4 { "uncertain" } else { "executing" });
    }
}
