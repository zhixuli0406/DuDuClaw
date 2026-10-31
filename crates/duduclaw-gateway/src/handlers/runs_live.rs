//! Opt-in live steps use exactly the run inspector's content authority.
use super::*;
use crate::live_events::{RunSelector, RunStep};

impl MethodHandler {
    pub(crate) async fn handle_runs_subscription(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let selector = match RunSelector::parse(&params) {
            Ok(s) => s, Err(e) => return WsFrame::error_response("", e),
        };
        // Subscribe and unsubscribe share this reply. Every authorization
        // refusal is the same sentence, so a caller cannot tell a missing
        // run from one they are not allowed to see.
        let selected = match selector {
            RunSelector::Run(id) => {
                let (id, _) = match self.authorize_live_run(&id, ctx).await {
                    Ok(pair) => pair,
                    Err(error) => {
                        tracing::debug!(error = %error, "runs subscription refused");
                        return WsFrame::error_response("", "Run not found");
                    }
                };
                json!({"run_id": id})
            }
        };
        WsFrame::ok_response("", json!({"subscription": selected}))
    }

    /// Metadata-only equivalent of runs.get's authority checks. Do not call
    /// the transcript RPC for each step: it can project a whole conversation.
    /// Identity and SQLite reads use the process-wide push pool. The async
    /// task gate stays between those slices and is not nested inside one.
    async fn authorize_live_run(&self, id: &str, ctx: &UserContext) -> Result<(String, String), String> {
        let home = self.home_dir.clone();
        let reader = ctx.clone();
        if let Some(rest) = id.strip_prefix("dispatch:") {
            let numeric = rest.parse::<i64>().map_err(|_| "Run not found")?;
            let (agent_id, task_id, live) = Self::block_auth({
                let home = home.clone();
                let reader = reader.clone();
                move || load_dispatch_live(&home, &reader, numeric)
            })
            .await?;
            if let Some(task_id) = task_id.as_deref().filter(|task| !task.is_empty()) {
                let tasks = self.task_store().await.map_err(|_| "permission denied")?;
                self.authorize_task_content_read(&tasks, &live, task_id, AccessLevel::Viewer)
                    .await
                    .map_err(|_| "permission denied")?;
            }
            let agent_check = agent_id.clone();
            Self::block_auth({
                let home = home.clone();
                let reader = reader.clone();
                move || {
                    let fresh = super::bound_history_rpc::live_bound_reader(&home, &reader)?;
                    if !fresh.has_agent_access(&agent_check, AccessLevel::Viewer) {
                        return Err("permission denied".into());
                    }
                    if !fresh.is_admin() && crate::machine_employee::requires_admin_for_id(&home, &agent_check) {
                        return Err("permission denied".into());
                    }
                    Ok(())
                }
            })
            .await?;
            return Ok((format!("dispatch:{numeric}"), agent_id));
        }
        let live = Self::block_auth({
            let home = home.clone();
            let reader = reader.clone();
            move || super::bound_history_rpc::live_bound_reader(&home, &reader)
        })
        .await?;
        let id = crate::history_privacy::handles::resolve_run(&self.home_dir, &live.user_id, id).await?;
        let (session, row_text) = id.rsplit_once('#').ok_or("Run not found")?;
        let row = row_text.parse::<i64>().map_err(|_| "Run not found")?;
        let session = session.to_owned();
        let resolved = id.clone();
        let agent = Self::block_auth({
            let home = home.clone();
            let reader = reader.clone();
            move || confirm_session_run(&home, &reader, &session, row)
        })
        .await?;
        Ok((resolved, agent))
    }

    /// `None` from the pool (closed semaphore or a join error) denies.
    async fn block_auth<T: Send + 'static>(
        work: impl FnOnce() -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        match super::push_filter::push_blocking(work).await {
            Some(result) => result,
            None => Err("permission denied".into()),
        }
    }

    pub(crate) async fn may_push_run_step(&self, step: &RunStep, ctx: &UserContext) -> bool {
        self.authorize_live_run(&step.run_id, ctx)
            .await
            .is_ok_and(|(id, agent)| id == step.run_id && agent == step.agent_id)
    }
}

fn load_dispatch_live(
    home: &Path,
    ctx: &UserContext,
    numeric: i64,
) -> Result<(String, Option<String>, UserContext), String> {
    let live = super::bound_history_rpc::live_bound_reader(home, ctx)?;
    let store = crate::run_steps::shared_store(home).ok_or("Run not found")?;
    let run = store.get_dispatch_run(numeric)?.ok_or("Run not found")?;
    if !live.has_agent_access(&run.agent_id, AccessLevel::Viewer) {
        return Err("permission denied".into());
    }
    if !live.is_admin() && crate::machine_employee::requires_admin_for_id(home, &run.agent_id) {
        return Err("permission denied".into());
    }
    Ok((run.agent_id, run.task_id, live))
}

/// Session tool steps require Full history. The connection is dropped before
/// the function returns, and identity is read again inside this call.
fn confirm_session_run(home: &Path, ctx: &UserContext, session: &str, row: i64) -> Result<String, String> {
    let agent = {
        let conn = rusqlite::Connection::open_with_flags(
            home.join("sessions.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(|_| "Run not found")?;
        conn.query_row(
            "SELECT s.agent_id FROM sessions s JOIN session_messages m ON s.id=m.session_id
             WHERE s.id=?1 AND m.id=?2 AND m.role='user' AND COALESCE(m.hidden,0)=0 AND m.undone_at IS NULL",
            params![session, row],
            |r| r.get::<_, String>(0),
        )
        .map_err(|_| "Run not found")?
    };
    let live = super::bound_history_rpc::live_bound_reader(home, ctx)?;
    let mode = crate::history_privacy::mode(home, &live, &agent)?;
    // Filtered history explicitly omits tool_steps in runs.get.
    if mode != crate::history_privacy::Mode::Full {
        return Err("run steps are not visible to this reader".into());
    }
    if !live.has_agent_access(&agent, AccessLevel::Viewer) {
        return Err("permission denied".into());
    }
    if !live.is_admin() && crate::machine_employee::requires_admin_for_id(home, &agent) {
        return Err("permission denied".into());
    }
    // Same outcome as bound_history_recheck when the first mode was Full.
    let fresh = super::bound_history_rpc::live_bound_reader(home, &live)?;
    if !fresh.has_agent_access(&agent, AccessLevel::Viewer)
        || (!fresh.is_admin() && crate::machine_employee::requires_admin_for_id(home, &agent))
    {
        return Err("permission denied".into());
    }
    let current = crate::history_privacy::mode(home, &fresh, &agent).map_err(|_| "session not found")?;
    if current != crate::history_privacy::Mode::Full {
        return Err("history authority changed; refresh".into());
    }
    Ok(agent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::task_privacy_tests::{fixture, ok, PRIVATE, OPEN};

    fn dispatch_step(home: &Path, task_id: &str) -> RunStep {
        let store = crate::run_steps::shared_store(home).unwrap();
        let prompt = format!("[goal-loop task_id={task_id} iter=1] review the task");
        let id = store.record_dispatch_run(
            "sales", "dispatch", "2026-10-29T00:00:00Z", "2026-10-29T00:01:00Z",
            "completed", &prompt, "", &[],
        ).unwrap();
        assert_eq!(store.get_dispatch_run(id).unwrap().unwrap().task_id.as_deref(), Some(task_id));
        RunStep { agent_id: "sales".into(), run_id: format!("dispatch:{id}"), tool: "Read".into(), phase: "finished" }
    }

    #[tokio::test]
    async fn machine_runs_recheck_old_viewer_binding_at_subscription_and_delivery() {
        let f = fixture().await;
        let root = crate::handlers::task_privacy_tests::admin(&f);
        f.db.bind_agent(&f.bob.user_id, "sales", AccessLevel::Viewer).unwrap();
        let step = dispatch_step(&f.home, OPEN);
        assert!(ok(&f.handler.handle_runs_subscription(json!({"run_id":step.run_id}), &f.bob).await));
        assert!(f.handler.may_push_run_step(&step, &f.bob).await);
        duduclaw_core::preset::install_system_operator_preset(&f.home, false).unwrap();
        duduclaw_core::preset::bind(&f.home, "sales", &f.home.join("agents/sales"),
            "system-operator", "test", "test").unwrap();
        for _ in 0..2 {
            assert!(!ok(&f.handler.handle_runs_subscription(json!({"run_id":step.run_id}), &f.bob).await));
            assert!(!f.handler.may_push_run_step(&step, &f.bob).await);
            assert!(ok(&f.handler.handle_runs_subscription(json!({"run_id":step.run_id}), &root).await));
            assert!(f.handler.may_push_run_step(&step, &root).await);
            // A binding whose preset no longer resolves stays restricted.
            let _ = std::fs::remove_file(f.home.join("presets/system-operator/preset.toml"));
        }
        let path = duduclaw_core::preset::bindings_path(&f.home);
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(!ok(&f.handler.handle_runs_subscription(json!({"run_id":step.run_id}), &f.bob).await));
        assert!(!f.handler.may_push_run_step(&step, &f.bob).await);
        assert!(f.handler.may_push_run_step(&step, &root).await);
    }

    #[tokio::test]
    async fn machine_session_run_requires_admin_even_with_full_history_binding() {
        let f = fixture().await;
        let root = crate::handlers::task_privacy_tests::admin(&f);
        f.db.bind_agent(&f.bob.user_id, "sales", AccessLevel::Owner).unwrap();
        let sessions = crate::session::SessionManager::new(&f.home.join("sessions.db")).unwrap();
        let row = {
            let conn = sessions.acquire().await;
            conn.execute("INSERT INTO sessions(id,agent_id,last_active,created_at)
                VALUES ('machine-live','sales','2026-10-29T00:00:00Z','2026-10-29T00:00:00Z')", []).unwrap();
            conn.execute("INSERT INTO session_messages(session_id,role,content,timestamp)
                VALUES ('machine-live','user','prompt','2026-10-29T00:00:00Z')", []).unwrap();
            conn.last_insert_rowid()
        };
        let step = RunStep { agent_id: "sales".into(), run_id: format!("machine-live#{row}"), tool: "Read".into(), phase: "started".into() };
        assert!(ok(&f.handler.handle_runs_subscription(json!({"run_id":step.run_id}), &f.bob).await));
        assert!(f.handler.may_push_run_step(&step, &f.bob).await);
        duduclaw_core::preset::install_system_operator_preset(&f.home, false).unwrap();
        duduclaw_core::preset::bind(&f.home, "sales", &f.home.join("agents/sales"),
            "system-operator", "test", "test").unwrap();
        assert!(!ok(&f.handler.handle_runs_subscription(json!({"run_id":step.run_id}), &f.bob).await));
        assert!(!f.handler.may_push_run_step(&step, &f.bob).await);
        assert!(ok(&f.handler.handle_runs_subscription(json!({"run_id":step.run_id}), &root).await));
        assert!(f.handler.may_push_run_step(&step, &root).await);
    }

    #[tokio::test]
    async fn dispatch_subscription_and_delivery_follow_task_audience() {
        let f = fixture().await;
        let private = dispatch_step(&f.home, PRIVATE);
        let open = dispatch_step(&f.home, OPEN);
        assert!(ok(&f.handler.handle_runs_subscription(json!({"run_id": private.run_id}), &f.alice).await));
        assert!(!ok(&f.handler.handle_runs_subscription(json!({"run_id": private.run_id}), &f.bob).await));
        assert!(f.handler.may_push_run_step(&private, &f.alice).await);
        assert!(!f.handler.may_push_run_step(&private, &f.bob).await);
        assert!(ok(&f.handler.handle_runs_subscription(json!({"run_id": open.run_id}), &f.bob).await));
        assert!(f.handler.may_push_run_step(&open, &f.bob).await);
        // Agent-wide selectors are refused before they can receive candidates.
        assert!(!ok(&f.handler.handle_runs_subscription(json!({"agent_id": "sales"}), &f.bob).await));
        assert!(!f.handler.may_push_run_step(&private, &f.bob).await);
    }

    #[tokio::test]
    async fn revoked_binding_denies_existing_subscription_delivery() {
        let f = fixture().await;
        let step = dispatch_step(&f.home, OPEN);
        assert!(ok(&f.handler.handle_runs_subscription(json!({"run_id": step.run_id}), &f.alice).await));
        assert!(f.handler.may_push_run_step(&step, &f.alice).await);
        f.db.unbind_agent(&f.alice.user_id, "sales").unwrap();
        // Reuse the cached connection context: authority must be re-read.
        assert!(!f.handler.may_push_run_step(&step, &f.alice).await);
        assert!(!ok(&f.handler.handle_runs_subscription(json!({"run_id": step.run_id}), &f.alice).await));
        assert!(!ok(&f.handler.handle_runs_subscription(json!({"agent_id": "sales"}), &f.alice).await));
    }

    #[tokio::test]
    async fn unknown_runs_and_unbound_agents_are_refused() {
        let f = fixture().await;
        for run_id in ["dispatch:999999", "dispatch:not-a-number", "missing-session#1"] {
            assert!(!ok(&f.handler.handle_runs_subscription(json!({"run_id": run_id}), &f.alice).await));
            let step = RunStep { agent_id: "sales".into(), run_id: run_id.into(), tool: "Read".into(), phase: "started" };
            assert!(!f.handler.may_push_run_step(&step, &f.alice).await);
        }
        assert!(!ok(&f.handler.handle_runs_subscription(json!({"agent_id": "unbound-agent"}), &f.alice).await));
        let mut step = dispatch_step(&f.home, OPEN);
        step.agent_id = "unbound-agent".into();
        assert!(!f.handler.may_push_run_step(&step, &f.alice).await);
    }

    #[tokio::test]
    async fn filtered_history_and_hidden_turns_never_deliver_steps() {
        let f = fixture().await;
        let sessions = crate::session::SessionManager::new(&f.home.join("sessions.db")).unwrap();
        let row = {
            let conn = sessions.acquire().await;
            conn.execute("INSERT INTO sessions(id,agent_id,last_active,created_at)
                VALUES ('live-session','sales','2026-10-29T00:00:00Z','2026-10-29T00:00:00Z')", []).unwrap();
            conn.execute("INSERT INTO session_messages(session_id,role,content,timestamp)
                VALUES ('live-session','user','private prompt','2026-10-29T00:00:00Z')", []).unwrap();
            conn.last_insert_rowid()
        };
        let step = RunStep { agent_id: "sales".into(), run_id: format!("live-session#{row}"), tool: "Read".into(), phase: "started" };
        assert!(!ok(&f.handler.handle_runs_subscription(json!({"run_id": step.run_id}), &f.alice).await));
        assert!(!f.handler.may_push_run_step(&step, &f.alice).await);
        f.db.bind_agent(&f.alice.user_id, "sales", AccessLevel::Owner).unwrap();
        assert!(ok(&f.handler.handle_runs_subscription(json!({"run_id": step.run_id}), &f.alice).await));
        assert!(f.handler.may_push_run_step(&step, &f.alice).await);
        {
            let conn = sessions.acquire().await;
            conn.execute("UPDATE session_messages SET hidden=1 WHERE id=?1", params![row]).unwrap();
        }
        assert!(!f.handler.may_push_run_step(&step, &f.alice).await);
        assert!(!ok(&f.handler.handle_runs_subscription(json!({"run_id": step.run_id}), &f.alice).await));
    }
    #[tokio::test]
    async fn private_and_revoked_steps_never_enter_queue_or_signal_loss() {
        let f = fixture().await;
        let private = dispatch_step(&f.home, PRIVATE);
        let open = dispatch_step(&f.home, OPEN);
        let handler = Arc::new(f.handler);
        let gate = handler.clone();
        let ctx = f.bob.clone();
        let (subscriptions, mut rx) = crate::live_events::RunSubscriptions::new_authorized(
            &f.home, &f.bob.user_id, Arc::new(move |step| {
                let handler = gate.clone();
                let ctx = ctx.clone();
                Box::pin(async move { handler.may_push_run_step(&step, &ctx).await })
            }),
        );
        // Simulate a stale selector after audience restriction. Authorization
        // must hold even if subscribe-time checks happened before the change.
        subscriptions.update(RunSelector::Run(private.run_id.clone()), true).unwrap();
        subscriptions.update(RunSelector::Run(open.run_id.clone()), true).unwrap();
        for _ in 0..100 {
            crate::live_events::publish_step(&f.home, "sales", &private.run_id, "Read", "finished");
            subscriptions.wait_for_authorization().await;
        }
        assert!(rx.try_recv().is_err());
        assert!(!subscriptions.take_lost());
        crate::live_events::publish_step(&f.home, "sales", &open.run_id, "Read", "finished");
        subscriptions.wait_for_authorization().await;
        assert_eq!(rx.try_recv().unwrap().run_id, open.run_id);
        f.db.unbind_agent(&f.bob.user_id, "sales").unwrap();
        for _ in 0..100 {
            crate::live_events::publish_step(&f.home, "sales", &open.run_id, "Read", "finished");
            subscriptions.wait_for_authorization().await;
        }
        assert!(rx.try_recv().is_err());
        assert!(!subscriptions.take_lost());
    }

    #[test]
    fn selector_refuses_ambiguous_empty_and_invalid_values() {
        for v in [json!({}), json!({"run_id":""}), json!({"agent_id":"../a"}), json!({"agent_id":"sales"}),
            json!({"agent_id":"a","run_id":"dispatch:1"}), json!({"run_id":1})] {
            assert!(RunSelector::parse(&v).is_err());
        }
    }

    fn client_error(frame: &WsFrame) -> String {
        match frame {
            WsFrame::Response { ok: false, error: Some(Value::String(text)), .. } => text.clone(),
            other => panic!("expected an error response, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn subscription_refusals_are_all_run_not_found() {
        let f = fixture().await;
        let missing = client_error(
            &f.handler.handle_runs_subscription(json!({"run_id": "dispatch:999999"}), &f.alice).await,
        );
        let open = dispatch_step(&f.home, OPEN);
        f.db.unbind_agent(&f.bob.user_id, "sales").unwrap();
        let unseen = client_error(
            &f.handler.handle_runs_subscription(json!({"run_id": open.run_id}), &f.bob).await,
        );
        let sessions = crate::session::SessionManager::new(&f.home.join("sessions.db")).unwrap();
        let row = {
            let conn = sessions.acquire().await;
            conn.execute(
                "INSERT INTO sessions(id,agent_id,last_active,created_at)
                 VALUES ('live-session','sales','2026-10-29T00:00:00Z','2026-10-29T00:00:00Z')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO session_messages(session_id,role,content,timestamp)
                 VALUES ('live-session','user','private prompt','2026-10-29T00:00:00Z')",
                [],
            )
            .unwrap();
            conn.last_insert_rowid()
        };
        let session_run = format!("live-session#{row}");
        // Alice is Operator, so this existing session run is Filtered.
        let filtered = client_error(
            &f.handler.handle_runs_subscription(json!({"run_id": session_run}), &f.alice).await,
        );
        assert_eq!(missing, "Run not found");
        assert_eq!(unseen, missing);
        assert_eq!(filtered, missing);
        assert!(ok(
            &f.handler.handle_runs_subscription(json!({"run_id": open.run_id}), &f.alice).await
        ));
        f.db.bind_agent(&f.alice.user_id, "sales", AccessLevel::Owner).unwrap();
        assert!(ok(
            &f.handler.handle_runs_subscription(json!({"run_id": session_run}), &f.alice).await
        ));
    }

    #[tokio::test]
    async fn queued_step_is_dropped_when_reader_revoked_before_send() {
        let f = fixture().await;
        let step = dispatch_step(&f.home, OPEN);
        let handler = Arc::new(f.handler);
        let gate = handler.clone();
        let ctx = f.alice.clone();
        let (subscriptions, mut inbound) = crate::live_events::RunSubscriptions::new_authorized(
            &f.home,
            &f.alice.user_id,
            Arc::new(move |step| {
                let handler = gate.clone();
                let ctx = ctx.clone();
                Box::pin(async move { handler.may_push_run_step(&step, &ctx).await })
            }),
        );
        subscriptions.update(RunSelector::Run(step.run_id.clone()), true).unwrap();
        crate::live_events::publish_step(&f.home, "sales", &step.run_id, "Read", "finished");
        subscriptions.wait_for_authorization().await;
        let queued = inbound.try_recv().unwrap();
        assert_eq!(queued.run_id, step.run_id);

        let delivered = forward_one(&subscriptions, queued.clone(), &handler, &f.alice).await;
        assert_eq!(delivered.unwrap().run_id, step.run_id);
        f.db.unbind_agent(&f.alice.user_id, "sales").unwrap();
        let dropped = forward_one(&subscriptions, queued, &handler, &f.alice).await;
        assert!(dropped.is_none());
        assert!(!subscriptions.take_lost());
    }

    async fn forward_one(
        subscriptions: &crate::live_events::RunSubscriptions,
        step: RunStep,
        handler: &Arc<MethodHandler>,
        ctx: &UserContext,
    ) -> Option<RunStep> {
        let (inbound_tx, inbound_rx) = tokio::sync::mpsc::channel(1);
        inbound_tx.try_send(step).unwrap();
        drop(inbound_tx);
        let (outbound_tx, mut outbound_rx) = tokio::sync::mpsc::channel(1);
        let handler = handler.clone();
        let ctx = ctx.clone();
        crate::live_events::forward_authorized(
            subscriptions,
            inbound_rx,
            outbound_tx,
            Arc::new(move |step| {
                let handler = handler.clone();
                let ctx = ctx.clone();
                Box::pin(async move { handler.may_push_run_step(&step, &ctx).await })
            }),
        )
        .await;
        outbound_rx.try_recv().ok()
    }
}
