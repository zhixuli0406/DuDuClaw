//! Driver construction and the outer `run` cadence.
//! Moved verbatim out of `goal_loop.rs`.

use super::*;

impl GoalLoopDriver {
    pub fn new(store: Arc<TaskStore>, queue: Arc<MessageQueue>, config: GoalLoopConfig) -> Self {
        Self {
            store,
            queue,
            config,
            home_dir: PathBuf::from("."),
            broker: None,
            policy: None,
            inflight: Mutex::new(HashMap::new()),
            dispatch_failures: Mutex::new(HashMap::new()),
            kickoff: Mutex::new(HashMap::new()),
            notified_needs_human: Mutex::new(HashSet::new()),
            operator_skipped: Mutex::new(HashSet::new()),
            progress_seen: Mutex::new(HashMap::new()),
            progress_retry: Mutex::new(HashMap::new()),
            needs_human_retry: Mutex::new(HashMap::new()),
            kickoff_notified: Mutex::new(HashSet::new()),
            kickoff_retry: Mutex::new(HashMap::new()),
            visit_graph: Arc::new(GoalVisitGraph::new()),
            state_capture_seen: Mutex::new(HashSet::new()),
            running: Arc::new(AtomicBool::new(false)),
            forward_model: None,
            // RFC-27: gate disabled by default (None) — production wires the
            // resolved edition limit via `with_concurrency_limit`. Tests and the
            // 3-arg constructor stay on the untouched, unlimited path.
            concurrency_limit: None,
            concurrency_ttl_secs: duduclaw_core::ConcurrencyGateConfig::default()
                .concurrency_lease_ttl_secs,
        }
    }

    /// Set the DuDuClaw home dir (per-agent autonomy + channel push).
    pub fn with_home_dir(mut self, home_dir: PathBuf) -> Self {
        self.home_dir = home_dir;
        self
    }

    /// RFC-27: wire the resolved edition concurrency limit + lease TTL. `limit`
    /// is `None` for an unlimited edition (Enterprise, or a Personal cap of 0),
    /// in which case the gate stays a no-op. Called from
    /// `handlers.rs::respawn_goal_loop_driver` with the edition resolved via the
    /// existing `resolve_edition_profile()` chain.
    pub fn with_concurrency_limit(mut self, limit: Option<u32>, ttl_secs: u64) -> Self {
        self.concurrency_limit = limit;
        self.concurrency_ttl_secs = ttl_secs;
        self
    }

    /// WP-A9: wire the A3 task-forward-model predict hook. Omit (default
    /// `None`) to keep the hook a no-op — the `[task_forward_model] enabled`
    /// gate (design §7.3) is enforced by the caller deciding whether to
    /// construct a `TaskForwardModel` at all, not by a flag read here.
    pub fn with_forward_model(mut self, forward_model: Arc<TaskForwardModel>) -> Self {
        self.forward_model = Some(forward_model);
        self
    }

    /// Wire the HITL broker used for the Collaborator/Consultant kickoff gate.
    pub fn with_broker(mut self, broker: Arc<ApprovalBroker>) -> Self {
        self.broker = Some(broker);
        self
    }

    /// Wire a non-default [`DispatchPolicy`] (D4 item 2). Omit for the default
    /// `FixedHierarchy` behavior (dispatch to `assigned_to` unchanged).
    pub fn with_policy(mut self, policy: Arc<dyn DispatchPolicy>) -> Self {
        self.policy = Some(policy);
        self
    }

    /// The effective iteration cap for a task, chosen by its difficulty (MaAS
    /// dynamic depth, D4 item 3): Simple goals get the cheaper `iteration_cap_simple`.
    pub(super) fn iteration_cap_for(&self, task: &TaskRow) -> u32 {
        let text = format!(
            "{}\n{}\n{}",
            task.title,
            task.description,
            task.acceptance_criteria.as_deref().unwrap_or("")
        );
        match crate::dispatch_engine::classify_goal_difficulty(&text) {
            crate::dispatch_engine::Difficulty::Simple => self.config.iteration_cap_simple,
            crate::dispatch_engine::Difficulty::Complex => self.config.iteration_cap,
        }
    }

    /// Stop the loop after the current tick.
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    /// Run the driver loop. Mirrors the dispatch engine cadence: sleep, then one
    /// tick of goal-task dispatching.
    pub async fn run(self: Arc<Self>) {
        self.running.store(true, Ordering::SeqCst);
        info!(
            iteration_cap = self.config.iteration_cap,
            wall_clock_hours = self.config.wall_clock_hours,
            max_concurrent = self.config.max_concurrent,
            tick_secs = self.config.tick_secs,
            "Goal loop driver started (autonomous goal_mode dispatch)"
        );
        self.release_stale_goal_leases();
        while self.running.load(Ordering::SeqCst) {
            time::sleep(Duration::from_secs(self.config.tick_secs.max(1))).await;
            if let Err(e) = self.tick_once().await {
                warn!(error = %e, "goal loop tick failed (will retry next tick)");
            }
        }
        warn!("Goal loop driver stopped");
    }
}
