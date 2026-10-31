//! Host-owned correlation for withdrawing only one cancelled turn's approvals.
//!
//! The persistent tombstone also fences MCP processes racing with cancellation.
//! No request payload or client message id supplies this identity.
//! Environment fallback is NOT signed: the existing agent HMAC authenticates
//! only agent_id, not session/turn. Signing requires a new domain-separated
//! token and transport through every runtime/MCP spawn. A Bash-capable employee
//! can forge these variables; real containment requires withholding Bash or a
//! task sandbox. The Bash command lane alone is only a speed bump.

use super::*;

/// Both origin columns must agree. Scoped cards need a surviving detail row;
/// missing detail rows never grant decision authority.
pub(super) const LIVE_ORIGIN_SQL: &str = "(
    (origin_session_id IS NULL AND origin_turn_id IS NULL)
    OR (origin_session_id IS NOT NULL AND origin_turn_id IS NOT NULL
        AND EXISTS (SELECT 1 FROM approval_turn_origins o WHERE o.approval_id=approvals.id
            AND NOT EXISTS (SELECT 1 FROM cancelled_approval_turns c
                WHERE c.session_id=o.session_id AND c.turn_id=o.turn_id))))";

tokio::task_local! {
    static TURN_ORIGIN: (String, String);
}

/// Scope the complete gateway turn, including pre-runtime approval requests.
pub(crate) async fn scope_turn<F: std::future::Future>(
    session_id: &str,
    turn_id: &str,
    future: F,
) -> F::Output {
    TURN_ORIGIN
        .scope((session_id.into(), turn_id.into()), future)
        .await
}

/// Read without creating or migrating an unused approval store. Unreadable
/// fences refuse dispatch; a later stale-queue retry can check again.
pub(crate) async fn is_turn_cancelled(home: &Path, session: &str, turn: &str) -> Result<bool, String> {
    let path = home.join("approvals.db");
    let (session, turn) = (session.to_owned(), turn.to_owned());
    tokio::task::spawn_blocking(move || {
        if !path.try_exists().map_err(|e| format!("check approval store: {e}"))? { return Ok(false); }
        let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| format!("open cancellation fence: {e}"))?;
        conn.busy_timeout(Duration::from_secs(5)).map_err(|e| e.to_string())?;
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM cancelled_approval_turns WHERE session_id=?1 AND turn_id=?2)",
            params![session, turn], |row| row.get(0),
        ).map_err(|e| format!("read cancellation fence: {e}"))
    }).await.map_err(|e| format!("cancellation fence task: {e}"))?
}

fn checked_origin(session: String, turn: String) -> Result<(String, String), String> {
    if session.is_empty()
        || session.len() > 1024
        || session.chars().any(char::is_control)
        || turn.is_empty()
        || turn.len() > 2048
        || turn.chars().any(char::is_control)
    {
        return Err("invalid approval turn origin".into());
    }
    Ok((session, turn))
}

fn refused_origin(source: &str, reason: &str) -> String {
    warn!(source, reason, "refusing approval with invalid host turn origin");
    format!("{reason} approval turn origin ({source})")
}

fn origin_from_pair(
    source: &str,
    session: Option<String>,
    turn: Option<String>,
) -> Result<Option<(String, String)>, String> {
    match (session, turn) {
        (Some(session), Some(turn)) => checked_origin(session, turn)
            .map(Some)
            .map_err(|_| refused_origin(source, "invalid")),
        (None, None) => Ok(None),
        _ => Err(refused_origin(source, "incomplete")),
    }
}

pub(super) fn current_origin() -> Result<Option<(String, String)>, String> {
    if let Ok((session, turn)) = TURN_ORIGIN.try_with(Clone::clone) {
        return origin_from_pair("gateway", Some(session), Some(turn));
    }
    let session = duduclaw_memory::feedback::CURRENT_SESSION_ID
        .try_with(Clone::clone)
        .ok()
        .flatten();
    let turn = duduclaw_memory::feedback::CURRENT_TURN_ID
        .try_with(Clone::clone)
        .ok()
        .flatten();
    if session.is_some() || turn.is_some() {
        return origin_from_pair("feedback", session, turn);
    }
    // The one-shot MCP server inherits these from the runtime's spawn. Only
    // fully absent host identity retains legacy unscoped behavior.
    origin_from_env(
        std::env::var_os(duduclaw_core::ENV_TRUST_SESSION_ID),
        std::env::var_os(duduclaw_core::ENV_TRUST_TURN_ID),
    )
}

fn origin_from_env(
    session: Option<std::ffi::OsString>,
    turn: Option<std::ffi::OsString>,
) -> Result<Option<(String, String)>, String> {
    let utf8 = |value: std::ffi::OsString| value.into_string()
        .map_err(|_| refused_origin("environment", "non-UTF-8"));
    origin_from_pair("environment", session.map(utf8).transpose()?, turn.map(utf8).transpose()?)
}

/// Pending cards are withdrawn atomically. This fixed 24-hour window only
/// fences late inserts racing process-tree termination; it is not an approval
/// TTL. A process escaping through setsid remains a non-sandbox boundary.
const TOMBSTONE_RETENTION_HOURS: i64 = 24;
const TOMBSTONE_ROW_CAP: i64 = 100_000;
const TOMBSTONE_SWEEP_BATCH: i64 = 1_000;

pub(super) fn sweep_tombstones(conn: &Connection) -> Result<(), String> {
    let expired = conn.execute(
        "DELETE FROM cancelled_approval_turns WHERE rowid IN (
           SELECT rowid FROM cancelled_approval_turns
           WHERE julianday(cancelled_at) < julianday('now') - ?1 / 24.0
           ORDER BY julianday(cancelled_at) LIMIT ?2)",
        params![TOMBSTONE_RETENTION_HOURS, TOMBSTONE_SWEEP_BATCH],
    ).map_err(|e| format!("sweep approval turn tombstones: {e}"))?;
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM cancelled_approval_turns", [], |r| r.get(0))
        .map_err(|e| format!("count approval turn tombstones: {e}"))?;
    if count > TOMBSTONE_ROW_CAP {
        // Never evict the rolling quota window, even when it exceeds the cap.
        let removed = conn.execute(
            "DELETE FROM cancelled_approval_turns WHERE rowid IN (
               SELECT rowid FROM cancelled_approval_turns
               WHERE julianday(cancelled_at) < julianday('now','-1 hour')
               ORDER BY julianday(cancelled_at) LIMIT ?1)",
            [(count - TOMBSTONE_ROW_CAP).min(TOMBSTONE_SWEEP_BATCH - expired as i64)],
        ).map_err(|e| format!("cap approval turn tombstones: {e}"))?;
        warn!(count, removed, "approval turn tombstones exceed row cap; preserving recent rows");
    }
    Ok(())
}

#[derive(Debug)]
pub struct TurnWithdrawal {
    pub withdrawn: usize,
    pub approved_before_cancel: Vec<String>,
}

// Already accepted turns must always be able to cancel.
pub(crate) const WEBCHAT_CANCELS_PER_HOUR: i64 = 100;

impl ApprovalBroker {
    /// Gateway maintenance owns the schedule; dashboard polling does not sweep.
    pub(crate) async fn sweep_cancelled_turns(&self) -> Result<(), String> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            let conn = store.conn.blocking_lock();
            sweep_tombstones(&conn)
        }).await.map_err(|e| format!("cancellation sweep task: {e}"))?
    }

    /// Persistent per-principal rolling quota. NULL legacy principals do not count.
    pub(crate) async fn check_webchat_turn_admission(&self, principal: &str) -> Result<(), String> {
        let store = self.store.clone();
        let principal = principal.to_owned();
        tokio::task::spawn_blocking(move || {
            let conn = store.conn.blocking_lock();
            let limited: bool = conn.query_row(
                "SELECT COUNT(*) >= ?2 FROM (SELECT 1 FROM cancelled_approval_turns
                 WHERE principal=?1 AND julianday(cancelled_at) > julianday('now','-1 hour')
                 LIMIT ?2)",
                params![principal, WEBCHAT_CANCELS_PER_HOUR], |row| row.get(0),
            ).map_err(|e| format!("check WebChat cancel quota: {e}"))?;
            if limited { return Err("turn_limit".into()); }
            Ok(())
        }).await.map_err(|e| format!("WebChat cancel quota task: {e}"))?
    }

    /// The immediate transaction serializes cancellation with every decision
    /// and MCP insert. Shared keyed cards survive while any origin is live.
    pub async fn withdraw_turn(&self, session_id: &str, turn_id: &str) -> Result<TurnWithdrawal, String> {
        self.withdraw_turn_for_principal(session_id, turn_id, None).await
    }

    pub(crate) async fn withdraw_turn_for_principal(
        &self, session_id: &str, turn_id: &str, principal: Option<&str>,
    ) -> Result<TurnWithdrawal, String> {
        checked_origin(session_id.into(), turn_id.into())?;
        let mut conn = self.store.conn.lock().await;
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| format!("begin approval turn cancellation: {e}"))?;
        tx.execute(
            "INSERT INTO cancelled_approval_turns(session_id, turn_id, cancelled_at, principal)
             VALUES (?1, ?2, ?3, ?4) ON CONFLICT(session_id, turn_id) DO NOTHING",
            params![session_id, turn_id, Utc::now().to_rfc3339(), principal],
        ).map_err(|e| format!("fence approval turn: {e}"))?;
        let approved_before_cancel = {
            let mut stmt = tx.prepare(
                "SELECT a.id FROM approvals a JOIN approval_turn_origins o ON a.id=o.approval_id
                 WHERE o.session_id=?1 AND o.turn_id=?2 AND a.status='approved' ORDER BY a.id",
            ).map_err(|e| e.to_string())?;
            stmt.query_map(params![session_id, turn_id], |r| r.get::<_, String>(0))
                .map_err(|e| e.to_string())?.collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?
        };
        let withdrawn = tx.execute(
            "UPDATE approvals SET status='denied', decided_by='system:cancel:webchat_turn', decided_at=?3
             WHERE status='pending' AND origin_session_id IS NOT NULL
             AND EXISTS (SELECT 1 FROM approval_turn_origins o WHERE o.approval_id=approvals.id
                         AND o.session_id=?1 AND o.turn_id=?2)
             AND NOT EXISTS (SELECT 1 FROM approval_turn_origins o WHERE o.approval_id=approvals.id
                 AND NOT EXISTS (SELECT 1 FROM cancelled_approval_turns c
                                 WHERE c.session_id=o.session_id AND c.turn_id=o.turn_id))",
            params![session_id, turn_id, Utc::now().to_rfc3339()],
        ).map_err(|e| format!("withdraw turn approvals: {e}"))?;
        tx.commit().map_err(|e| format!("commit approval cancellation: {e}"))?;
        // Existing withdraw() emits no lifecycle notifications: observers poll
        // the committed rows. Keep that behavior without replaying its CAS.
        Ok(TurnWithdrawal { withdrawn, approved_before_cancel })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn request(broker: &ApprovalBroker, session: &str, turn: &str) -> ApprovalId {
        scope_turn(
            session,
            turn,
            broker.request("bot", "test", "pending", Value::Null, 600),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn decide_requires_complete_origin_and_live_detail() {
        for case in ["missing", "session_null", "turn_null", "cancelled", "unscoped", "live"] {
            let broker = ApprovalBroker::new(std::sync::Arc::new(ApprovalStore::open_in_memory().unwrap()));
            let id = request(&broker, "session", "turn").await;
            {
                let conn = broker.store.conn.lock().await;
                match case {
                    "missing" => { conn.execute("DELETE FROM approval_turn_origins WHERE approval_id=?1", [id.as_str()]).unwrap(); }
                    "session_null" => { conn.execute("UPDATE approvals SET origin_session_id=NULL WHERE id=?1", [id.as_str()]).unwrap(); }
                    "turn_null" => { conn.execute("UPDATE approvals SET origin_turn_id=NULL WHERE id=?1", [id.as_str()]).unwrap(); }
                    "unscoped" => {
                        conn.execute("UPDATE approvals SET origin_session_id=NULL,origin_turn_id=NULL WHERE id=?1", [id.as_str()]).unwrap();
                        conn.execute("DELETE FROM approval_turn_origins WHERE approval_id=?1", [id.as_str()]).unwrap();
                    }
                    _ => {
                        conn.execute("INSERT INTO cancelled_approval_turns(session_id,turn_id,cancelled_at) VALUES ('session','turn',datetime('now'))", []).unwrap();
                        if case == "live" {
                            conn.execute("INSERT INTO approval_turn_origins VALUES (?1,'session','live')", [id.as_str()]).unwrap();
                        }
                    }
                }
            }
            let allowed = matches!(case, "unscoped" | "live");
            assert_eq!(broker.decide(&id, true, "human:test").await.is_ok(), allowed, "{case}");
            assert_eq!(broker.get(&id).await.unwrap().unwrap().status,
                if allowed { ApprovalStatus::Approved } else { ApprovalStatus::Pending }, "{case}");
        }
    }

    #[tokio::test]
    async fn webchat_cancel_withdraws_exact_turn_and_fences_late_requests() {
        let broker = ApprovalBroker::new(std::sync::Arc::new(
            ApprovalStore::open_in_memory().unwrap(),
        ));
        let target = request(&broker, "session-a", "turn-1").await;
        let other_turn = request(&broker, "session-a", "turn-2").await;
        let other_session = request(&broker, "session-b", "turn-1").await;
        let decided = request(&broker, "session-a", "turn-1").await;
        broker
            .decide(&decided, true, "human:approved")
            .await
            .unwrap();
        assert_eq!(
            broker.withdraw_turn("session-a", "turn-1").await.unwrap().withdrawn,
            1
        );
        let cancelled = broker.get(&target).await.unwrap().unwrap();
        assert_eq!(cancelled.status, ApprovalStatus::Denied);
        assert_eq!(
            cancelled.decided_by.as_deref(),
            Some("system:cancel:webchat_turn")
        );
        for id in [other_turn, other_session] {
            assert_eq!(
                broker.get(&id).await.unwrap().unwrap().status,
                ApprovalStatus::Pending
            );
        }
        let approved = broker.get(&decided).await.unwrap().unwrap();
        assert_eq!(approved.status, ApprovalStatus::Approved);
        assert_eq!(approved.decided_by.as_deref(), Some("human:approved"));
        assert!(
            scope_turn(
                "session-a",
                "turn-1",
                broker.request("bot", "test", "late", Value::Null, 600)
            )
            .await
            .is_err()
        );
        assert_eq!(
            broker.withdraw_turn("session-a", "turn-1").await.unwrap().withdrawn,
            0
        );
    }

    #[tokio::test]
    async fn webchat_cancel_tombstone_survives_broker_reopen() {
        let home = tempfile::tempdir().unwrap();
        let broker = ApprovalBroker::open(home.path()).unwrap();
        let mcp_broker = ApprovalBroker::open(home.path()).unwrap();
        let target = request(&broker, "session", "turn").await;
        broker.withdraw_turn("session", "turn").await.unwrap();
        assert!(
            scope_turn(
                "session",
                "turn",
                mcp_broker.request("bot", "test", "racing MCP", Value::Null, 600)
            )
            .await
            .is_err()
        );
        drop(mcp_broker);
        drop(broker);
        let reopened = ApprovalBroker::open(home.path()).unwrap();
        assert_eq!(
            reopened.get(&target).await.unwrap().unwrap().status,
            ApprovalStatus::Denied
        );
        assert!(
            scope_turn(
                "session",
                "turn",
                reopened.request("bot", "test", "late", Value::Null, 600)
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn cancel_reports_approval_that_won_the_race() {
        let broker = ApprovalBroker::new(std::sync::Arc::new(ApprovalStore::open_in_memory().unwrap()));
        let id = request(&broker, "session", "turn").await;
        broker.decide(&id, true, "human").await.unwrap();
        let result = broker.withdraw_turn("session", "turn").await.unwrap();
        assert_eq!(result.withdrawn, 0);
        assert_eq!(result.approved_before_cancel, vec![id.as_str().to_string()]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn decide_and_cancel_are_serialized_across_store_connections() {
        let home = tempfile::tempdir().unwrap();
        let first = ApprovalBroker::open(home.path()).unwrap();
        let second = ApprovalBroker::open(home.path()).unwrap();
        for index in 0..16 {
            let turn = format!("turn-{index}");
            let id = request(&first, "session", &turn).await;
            let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
            let decide_broker = second.clone();
            let decide_id = id.clone();
            let decide_barrier = barrier.clone();
            let decision = tokio::spawn(async move {
                decide_barrier.wait().await;
                decide_broker.decide(&decide_id, true, "human").await
            });
            barrier.wait().await;
            let cancellation = first.withdraw_turn("session", &turn).await.unwrap();
            let decision = decision.await.unwrap();
            let row = first.get(&id).await.unwrap().unwrap();
            if decision.is_ok() {
                assert_eq!(row.status, ApprovalStatus::Approved);
                assert_eq!(cancellation.approved_before_cancel, vec![id.as_str().to_string()]);
            } else {
                assert_eq!(row.status, ApprovalStatus::Denied);
                assert_eq!(cancellation.withdrawn, 1);
            }
        }
    }

    #[tokio::test]
    async fn decision_sql_refuses_a_pending_row_with_a_cancelled_origin() {
        let broker = ApprovalBroker::new(std::sync::Arc::new(ApprovalStore::open_in_memory().unwrap()));
        let id = request(&broker, "session", "turn").await;
        {
            let conn = broker.store.conn.lock().await;
            conn.execute("INSERT INTO cancelled_approval_turns(session_id, turn_id, cancelled_at) VALUES ('session','turn',?1)",
                [Utc::now().to_rfc3339()]).unwrap();
        }
        assert!(broker.decide(&id, true, "human").await.is_err());
        assert_eq!(broker.get(&id).await.unwrap().unwrap().status, ApprovalStatus::Pending);
    }

    #[tokio::test]
    async fn keyed_reuse_tracks_each_origin_and_cancelled_retry_stays_idempotent() {
        let broker = ApprovalBroker::new(std::sync::Arc::new(ApprovalStore::open_in_memory().unwrap()));
        let id = request(&broker, "session", "first").await;
        let record = broker.get(&id).await.unwrap().unwrap();
        assert!(!scope_turn("session", "second", broker.store.insert_if_absent(&record)).await.unwrap());
        assert_eq!(broker.withdraw_turn("session", "first").await.unwrap().withdrawn, 0);
        assert_eq!(broker.get(&id).await.unwrap().unwrap().status, ApprovalStatus::Pending);
        assert!(!scope_turn("session", "first", broker.store.insert_if_absent(&record)).await.unwrap());
        assert_eq!(broker.withdraw_turn("session", "second").await.unwrap().withdrawn, 1);
        assert!(broker.decide(&id, true, "human").await.is_err());
    }

    #[tokio::test]
    async fn shared_keyed_card_can_be_decided_after_first_origin_cancel() {
        let broker = ApprovalBroker::new(std::sync::Arc::new(ApprovalStore::open_in_memory().unwrap()));
        let id = request(&broker, "session", "first").await;
        let record = broker.get(&id).await.unwrap().unwrap();
        scope_turn("session", "second", broker.store.insert_if_absent(&record)).await.unwrap();
        broker.withdraw_turn("session", "first").await.unwrap();
        broker.decide(&id, true, "human").await.unwrap();
        assert_eq!(broker.withdraw_turn("session", "second").await.unwrap().approved_before_cancel,
            vec![id.as_str().to_string()]);
    }

    #[tokio::test]
    async fn malformed_or_incomplete_feedback_and_gateway_origins_refuse_requests() {
        let broker = ApprovalBroker::new(std::sync::Arc::new(ApprovalStore::open_in_memory().unwrap()));
        for (session, turn) in [
            (Some("bad\n"), Some("turn")),
            (Some("session"), Some("")),
            (Some("session"), None),
            (None, Some("turn")),
        ] {
            let result = duduclaw_memory::feedback::CURRENT_SESSION_ID.scope(session.map(str::to_string),
                duduclaw_memory::feedback::CURRENT_TURN_ID.scope(turn.map(str::to_string),
                    broker.request("bot", "ordinary", "invalid feedback", Value::Null, 60))).await;
            assert!(result.unwrap_err().contains("feedback"));
        }
        for (session, turn) in [("bad\n", "turn"), ("session", ""), ("", "turn")] {
            assert!(scope_turn(session, turn, broker.request("bot", "strict", "bad", Value::Null, 60))
                .await.unwrap_err().contains("gateway"));
        }
        let count: i64 = broker.store.conn.lock().await.query_row(
            "SELECT COUNT(*) FROM approvals", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn non_webchat_env_origin_request_is_checked_in_isolated_process() {
        const CHILD_CASE: &str = "DUDUCLAW_APPROVAL_ORIGIN_TEST_CHILD";
        if let Ok(case) = std::env::var(CHILD_CASE) {
            let broker = ApprovalBroker::new(std::sync::Arc::new(ApprovalStore::open_in_memory().unwrap()));
            let result = broker.request("bot", "ordinary", "env only", Value::Null, 60).await;
            if case != "valid" && case != "absent" {
                assert!(result.unwrap_err().contains("environment"));
                let count: i64 = broker.store.conn.lock().await.query_row(
                    "SELECT COUNT(*) FROM approvals", [], |row| row.get(0)).unwrap();
                assert_eq!(count, 0);
            } else {
                let id = result.unwrap();
                let conn = broker.store.conn.lock().await;
                let origin: Option<String> = conn.query_row(
                    "SELECT origin_session_id FROM approvals WHERE id=?1", [id.as_str()], |r| r.get(0),
                ).unwrap();
                assert_eq!(origin, (case == "valid").then(|| "session".to_string()));
            }
            return;
        }
        let cases = [
            "valid", "absent", "session_only", "turn_only", "malformed", "empty",
            #[cfg(unix)]
            "non_utf8_session",
            #[cfg(unix)]
            "non_utf8_turn",
        ];
        for case in cases {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command.args(["--exact", "approval::turn_origin::tests::non_webchat_env_origin_request_is_checked_in_isolated_process"])
                .env(CHILD_CASE, case)
                .env_remove(duduclaw_core::ENV_TRUST_SESSION_ID)
                .env_remove(duduclaw_core::ENV_TRUST_TURN_ID);
            if case != "absent" && case != "turn_only" {
                command.env(duduclaw_core::ENV_TRUST_SESSION_ID, "session");
            }
            if case != "absent" && case != "session_only" {
                command.env(duduclaw_core::ENV_TRUST_TURN_ID, match case {
                    "malformed" => "bad\n",
                    "empty" => "",
                    _ => "turn",
                });
            }
            #[cfg(unix)]
            if case.starts_with("non_utf8_") {
                use std::os::unix::ffi::OsStringExt;
                let key = if case == "non_utf8_session" {
                    duduclaw_core::ENV_TRUST_SESSION_ID
                } else {
                    duduclaw_core::ENV_TRUST_TURN_ID
                };
                command.env(key, std::ffi::OsString::from_vec(vec![0xff]));
            }
            let output = command.output().unwrap();
            assert!(output.status.success(), "case {case}: {} {}",
                String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        }
    }

    #[tokio::test]
    async fn tombstone_sweep_keeps_recent_fences_and_removes_old_ones() {
        let broker = ApprovalBroker::new(std::sync::Arc::new(ApprovalStore::open_in_memory().unwrap()));
        let long = scope_turn("session", "old", broker.request("bot", "test", "long TTL", Value::Null, 365 * 24 * 3600)).await.unwrap();
        broker.withdraw_turn("session", "old").await.unwrap();
        assert_eq!(broker.get(&long).await.unwrap().unwrap().status, ApprovalStatus::Denied);
        broker.withdraw_turn("session", "recent").await.unwrap();
        {
            let conn = broker.store.conn.lock().await;
            conn.execute("UPDATE cancelled_approval_turns SET cancelled_at=datetime('now','-25 hours') WHERE turn_id='old'", []).unwrap();
        }
        broker.sweep_cancelled_turns().await.unwrap();
        let conn = broker.store.conn.lock().await;
        let turns: Vec<String> = conn.prepare("SELECT turn_id FROM cancelled_approval_turns").unwrap()
            .query_map([], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
        assert_eq!(turns, vec!["recent"]);
    }

    #[tokio::test]
    async fn tombstone_capacity_still_fences_and_withdraws_without_evicting_recent_fences() {
        let broker = ApprovalBroker::new(std::sync::Arc::new(ApprovalStore::open_in_memory().unwrap()));
        {
            let conn = broker.store.conn.lock().await;
            conn.execute(
                "WITH RECURSIVE ids(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM ids WHERE n<?1)
                 INSERT INTO cancelled_approval_turns(session_id, turn_id, cancelled_at) SELECT 'session', CAST(n AS TEXT), strftime('%Y-%m-%dT%H:%M:%fZ','now') FROM ids",
                [100_000],
            ).unwrap();
        }
        broker.check_webchat_turn_admission("user:alice").await.unwrap();
        let target = request(&broker, "session", "overflow").await;
        assert_eq!(broker.withdraw_turn("session", "overflow").await.unwrap().withdrawn, 1);
        assert_eq!(broker.get(&target).await.unwrap().unwrap().status, ApprovalStatus::Denied);
        assert!(scope_turn("session", "overflow",
            broker.request("bot", "test", "late", Value::Null, 60)).await.is_err());
        let count: i64 = broker.store.conn.lock().await.query_row(
            "SELECT COUNT(*) FROM cancelled_approval_turns", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 100_001);
        assert!(broker.withdraw_turn("session", "1").await.is_ok());
        broker.sweep_cancelled_turns().await.unwrap();
        let conn = broker.store.conn.lock().await;
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM cancelled_approval_turns", [], |r| r.get::<_, i64>(0)).unwrap(), 100_001);
        conn.execute("UPDATE cancelled_approval_turns SET cancelled_at=datetime('now','-2 hours') WHERE turn_id='1'", []).unwrap();
        conn.execute("WITH RECURSIVE ids(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM ids WHERE n<1500)
            INSERT INTO cancelled_approval_turns(session_id,turn_id,cancelled_at)
            SELECT 'older',CAST(n AS TEXT),datetime('now','-3 hours') FROM ids", []).unwrap();
        sweep_tombstones(&conn).unwrap();
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM cancelled_approval_turns", [], |r| r.get::<_, i64>(0)).unwrap(), 100_501);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM cancelled_approval_turns WHERE session_id='older'", [], |r| r.get::<_, i64>(0)).unwrap(), 500);
        sweep_tombstones(&conn).unwrap();
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM cancelled_approval_turns", [], |r| r.get::<_, i64>(0)).unwrap(), 100_000);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM cancelled_approval_turns WHERE turn_id='1' AND session_id='session'", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        drop(conn);
        broker.check_webchat_turn_admission("user:alice").await.unwrap();
    }

    #[tokio::test]
    async fn webchat_cancel_quota_upgrades_legacy_tombstones_without_losing_fences() {
        let home = tempfile::tempdir().unwrap();
        let conn = Connection::open(home.path().join("approvals.db")).unwrap();
        conn.execute_batch("CREATE TABLE cancelled_approval_turns (
            session_id TEXT NOT NULL, turn_id TEXT NOT NULL, cancelled_at TEXT NOT NULL,
            PRIMARY KEY(session_id,turn_id));
            INSERT INTO cancelled_approval_turns VALUES ('session','legacy',datetime('now'));").unwrap();
        let broker = ApprovalBroker::open(home.path()).unwrap();
        broker.check_webchat_turn_admission("user:alice").await.unwrap();
        broker.withdraw_turn_for_principal("session", "new", Some("user:alice")).await.unwrap();
        let principal: Option<String> = conn.query_row(
            "SELECT principal FROM cancelled_approval_turns WHERE turn_id='legacy'", [], |r| r.get(0)).unwrap();
        assert_eq!(principal, None);
        let principal: String = conn.query_row(
            "SELECT principal FROM cancelled_approval_turns WHERE turn_id='new'", [], |r| r.get(0)).unwrap();
        assert_eq!(principal, "user:alice");
        drop(broker);
        let reopened = ApprovalBroker::open(home.path()).unwrap();
        for turn in ["legacy", "new"] {
            assert!(scope_turn("session", turn,
                reopened.request("bot", "test", "late", Value::Null, 600)).await.is_err());
        }
    }

    #[test]
    fn webchat_host_env_origin_requires_a_complete_valid_pair() {
        assert_eq!(
            origin_from_env(Some("session".into()), Some("turn".into())).unwrap(),
            Some(("session".into(), "turn".into()))
        );
        assert!(origin_from_env(Some("session".into()), None).is_err());
        assert!(origin_from_env(None, Some("turn".into())).is_err());
        assert!(origin_from_env(Some("session".into()), Some("".into())).is_err());
        assert!(origin_from_env(Some("session\nforeign".into()), Some("turn".into())).is_err());
        assert!(origin_from_env(Some("s".repeat(1025).into()), Some("turn".into())).is_err());
        assert!(origin_from_env(Some("session".into()), Some("t".repeat(2049).into())).is_err());
        assert_eq!(origin_from_env(None, None).unwrap(), None);
    }
}
