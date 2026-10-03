//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    pub async fn new(home_dir: PathBuf) -> Self {
        Self::with_extension(home_dir, Arc::new(crate::extension::NullExtension)).await
    }

    /// Create a new handler with a custom extension (used by Pro binary).
    pub async fn with_extension(home_dir: PathBuf, extension: Arc<dyn GatewayExtension>) -> Self {
        let agents_dir = home_dir.join("agents");
        // v1.68: one-time reset of scaffold-noise `false` permission flags
        // (they are enforced from now on); runs before the registry scan so
        // the scan sees the migrated files.
        {
            let home = home_dir.clone();
            let _ = tokio::task::spawn_blocking(move || {
                super::agents_update_v168::migrate_all_agent_permissions(&home)
            })
            .await;
        }
        let mut registry = AgentRegistry::new(agents_dir.clone());
        if let Err(e) = registry.scan().await {
            tracing::warn!("Failed to scan agents directory: {e}");
        }

        // Install the agent-file-guard PreToolUse hook into every existing
        // agent's .claude/settings.json on startup. Idempotent — merges into
        // existing settings without clobbering user-added hooks.
        let bin = crate::agent_hook_installer::resolve_duduclaw_bin();
        if let Ok(mut entries) = tokio::fs::read_dir(&agents_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                // Skip _trash and other non-agent directories.
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name.starts_with('_') || name.is_empty() {
                    continue;
                }
                if let Err(e) =
                    crate::agent_hook_installer::ensure_agent_hook_settings(&path, &bin).await
                {
                    tracing::warn!(
                        agent = %name,
                        error = %e,
                        "Failed to install agent-file-guard hook on startup"
                    );
                }
            }
        }
        let home_dir_for_registry = home_dir.clone();
        Self {
            registry: Arc::new(RwLock::new(registry)),
            home_dir,
            start_time: Instant::now(),
            channel_status: Arc::new(RwLock::new(std::collections::HashMap::new())),
            heartbeat: RwLock::new(None),
            reply_ctx: RwLock::new(None),
            channel_handles: tokio::sync::Mutex::new(std::collections::HashMap::new()),
            pending_update: RwLock::new(None),
            user_db: RwLock::new(None),
            jwt_config: RwLock::new(None),
            extension,
            edition_override: RwLock::new(None),
            cli_auth_sessions: RwLock::new(std::collections::HashMap::new()),
            setup_token_session: RwLock::new(None),
            cron_store: RwLock::new(None),
            cron_scheduler: RwLock::new(None),
            mcp_oauth_pending: RwLock::new(std::collections::HashMap::new()),
            task_store: RwLock::new(None),
            autopilot_store: RwLock::new(None),
            event_tx: RwLock::new(None),
            autopilot_event_tx: RwLock::new(None),
            redaction_manager: RwLock::new(None),
            redaction_poison: RwLock::new(None),
            redaction_gc: tokio::sync::Mutex::new(None),
            audit_index: tokio::sync::OnceCell::new(),
            message_queue: RwLock::new(None),
            driver_handles: tokio::sync::Mutex::new(std::collections::HashMap::new()),
            os_watchers: crate::os_events::OsWatcherRegistry::new(home_dir_for_registry.clone()),
            os_frontmost: crate::os_frontmost::OsFrontmostRegistry::new(),
            footprint: crate::footprint_distill::FootprintTracker::new(
                home_dir_for_registry,
                std::collections::HashSet::new(),
            ),
            forward_model: RwLock::new(None),
            tick_hub: RwLock::new(None),
            tick_runtime: tokio::sync::Mutex::new(None),
        }
    }

    /// Inject the explicit edition form-factor override (called once after
    /// gateway start). `None` keeps per-request resolution from env + tier.
    pub async fn set_edition_override(&self, edition: Option<duduclaw_core::EditionProfile>) {
        *self.edition_override.write().await = edition;
    }

    /// Resolve the active product form-factor ([`EditionProfile`]) at request
    /// time using the documented precedence: `DUDUCLAW_EDITION` env >
    /// explicit override > license tier > `Personal`. This is the value the
    /// dashboard reads to decide whether to show enterprise management
    /// surfaces. It never gates a core feature.
    ///
    /// [`EditionProfile`]: duduclaw_core::EditionProfile
    pub(crate) async fn resolve_edition_profile(&self) -> duduclaw_core::EditionProfile {
        let tier_key = match crate::license_runtime::global() {
            Some(runtime) => Some(runtime.snapshot().await.tier.as_toml_key().to_string()),
            None => None,
        };
        let env = std::env::var("DUDUCLAW_EDITION").ok();
        let override_ed = *self.edition_override.read().await;
        duduclaw_core::EditionProfile::resolve(
            env.as_deref(),
            override_ed.map(|e| e.as_str()),
            tier_key.as_deref(),
        )
    }

    /// Lazily open (once) the shared [`AuditEventIndex`] and return it.
    ///
    /// M1/M60: the index is opened a single time (a fresh DB connection per
    /// request was O(total-audit-history) on a hot path). The first call also
    /// runs an initial `sync_from_files`; thereafter a background task that
    /// calls [`refresh_audit_index`](Self::refresh_audit_index) keeps it fresh,
    /// so request handlers do NOT sync inline.
    pub(crate) async fn audit_index(
        &self,
    ) -> Result<Arc<crate::evolution_events::query::AuditEventIndex>, String> {
        use crate::evolution_events::query::AuditEventIndex;
        let idx = self
            .audit_index
            .get_or_try_init(|| async {
                let idx = AuditEventIndex::open(&self.home_dir)?;
                // Initial sync so the very first query isn't empty/stale.
                if let Err(e) = idx.sync_from_files().await {
                    warn!("audit_index: initial sync warning (stale index): {e}");
                }
                Ok::<_, String>(Arc::new(idx))
            })
            .await?;
        Ok(idx.clone())
    }

    /// Refresh the shared audit index once (called on a background interval by
    /// the gateway — M1/M60 — to replace per-request `sync_from_files`).
    /// Best-effort: errors are logged and swallowed.
    pub async fn refresh_audit_index(&self) {
        match self.audit_index().await {
            Ok(idx) => {
                if let Err(e) = idx.sync_from_files().await {
                    warn!("audit_index background sync warning: {e}");
                }
            }
            Err(e) => warn!("audit_index background sync: open failed: {e}"),
        }
    }

    /// Inject the redaction manager (called once after gateway start when
    /// `[redaction] enabled` is true). `None` ⇒ redaction disabled.
    /// Install or clear the redaction manager, restarting the paired vault GC
    /// task to match. Called at boot and by `redaction.update` hot reload —
    /// in-flight pipelines keep their old `Arc` and drain naturally; every
    /// subsequent message sees the new rules.
    pub async fn swap_redaction_manager(
        &self,
        manager: Option<Arc<duduclaw_redaction::RedactionManager>>,
    ) {
        // Stop the old sweeper first so two GC tasks never run concurrently.
        if let Some(old) = self.redaction_gc.lock().await.take() {
            old.stop().await;
        }
        let gc = manager.as_ref().map(|m| {
            duduclaw_redaction::spawn_gc(
                m.vault().clone(),
                m.audit_sink().clone(),
                // `[redaction] purge_after_expire_days` (was always 30).
                crate::redaction_sources::gc_config_for(m),
            )
        });
        crate::redaction_sources::set_current(manager.clone());
        *self.redaction_manager.write().await = manager;
        *self.redaction_gc.lock().await = gc;
    }

    /// Read the redaction manager handle.
    pub async fn get_redaction_manager(&self) -> Option<Arc<duduclaw_redaction::RedactionManager>> {
        self.redaction_manager.read().await.clone()
    }

    /// Enter (or clear) the redaction poison state. `None` clears it.
    pub async fn set_redaction_poison(&self, poison: Option<RedactionPoison>) {
        *self.redaction_poison.write().await = poison;
    }

    /// Read the current redaction poison state, if any.
    pub async fn get_redaction_poison(&self) -> Option<RedactionPoison> {
        self.redaction_poison.read().await.clone()
    }

    /// Inject the SQLite-backed cron task store (called once after gateway start).
    pub async fn set_cron_store(&self, store: Arc<CronStore>) {
        *self.cron_store.write().await = Some(store);
    }

    /// Inject the SQLite-backed task board store (called once after gateway start).
    pub async fn set_task_store(&self, store: Arc<TaskStore>) {
        *self.task_store.write().await = Some(store);
    }

    /// Inject the SQLite-backed autopilot rule store (called once after gateway start).
    pub async fn set_autopilot_store(&self, store: Arc<AutopilotStore>) {
        *self.autopilot_store.write().await = Some(store);
    }

    /// WP-A9: inject the A3 task-forward-model (called once at gateway
    /// start, only when `[task_forward_model] enabled = true` — see
    /// `server.rs`). Shared by both the `DispatchEngine` settle hook and
    /// the `GoalLoopDriver` predict hook — see the `forward_model` field's
    /// doc comment on why this must be the SAME `Arc`, not two separately
    /// constructed models.
    pub async fn set_forward_model(
        &self,
        model: Arc<crate::prediction::task_forward_store::TaskForwardModel>,
    ) {
        *self.forward_model.write().await = Some(model);
    }

    /// WP-A9: read back the shared A3 task-forward-model, if one was
    /// injected. `None` when an operator has explicitly set
    /// `[task_forward_model] enabled = false` (the key defaults to `true`
    /// since v1.54) — callers treat that identically to "hook disabled".
    pub async fn forward_model(
        &self,
    ) -> Option<Arc<crate::prediction::task_forward_store::TaskForwardModel>> {
        self.forward_model.read().await.clone()
    }

    /// Resident sensing (WP4): inject the shared tick-observation hub
    /// (called once after gateway start, only when the autopilot engine —
    /// and therefore the tick-source runtime — was started; see
    /// `server.rs`).
    pub async fn set_tick_hub(&self, hub: Arc<crate::tick_source::TickHub>) {
        *self.tick_hub.write().await = Some(hub);
    }

    /// Inject the event broadcast sender for task/activity real-time events.
    pub async fn set_event_tx(&self, tx: tokio::sync::broadcast::Sender<String>) {
        *self.event_tx.write().await = Some(tx);
    }

    /// Inject the typed event broadcast sender consumed by `AutopilotEngine`.
    pub async fn set_autopilot_event_tx(
        &self,
        tx: tokio::sync::broadcast::Sender<crate::autopilot_engine::AutopilotEvent>,
    ) {
        *self.autopilot_event_tx.write().await = Some(tx);
    }

    /// Publish an autopilot event to the engine (best-effort).
    pub(crate) async fn emit_autopilot_event(&self, event: crate::autopilot_engine::AutopilotEvent) {
        if let Some(tx) = self.autopilot_event_tx.read().await.as_ref() {
            let _ = tx.send(event);
        }
    }

    /// Read access to the autopilot event broadcast sender, for the dashboard
    /// WebSocket's live `os.events.subscribe` tail (P4-3+). The caller clones
    /// the returned `Sender` and calls `.subscribe()` to get a fresh
    /// `Receiver` scoped to one connection — dropping it (connection close)
    /// unsubscribes automatically, no manual bookkeeping required. `None`
    /// only in the narrow startup window before `set_autopilot_event_tx` has
    /// run (the WS loop treats that as "never resolves" — see `server.rs`).
    pub async fn autopilot_event_tx(
        &self,
    ) -> Option<tokio::sync::broadcast::Sender<crate::autopilot_engine::AutopilotEvent>> {
        self.autopilot_event_tx.read().await.clone()
    }

    /// Inject the running cron scheduler handle (called once after gateway start).
    pub async fn set_cron_scheduler(&self, scheduler: Arc<CronScheduler>) {
        *self.cron_scheduler.write().await = Some(scheduler);
    }

    /// Inject the SQLite message queue (called once after gateway start). Needed
    /// so the goal-loop driver can be rebuilt on a hot config reload.
    pub async fn set_message_queue(&self, mq: Arc<crate::message_queue::MessageQueue>) {
        *self.message_queue.write().await = Some(mq);
    }

    /// Shared OS-watcher registry (for server.rs startup wiring + status).
    pub fn os_watchers(&self) -> Arc<crate::os_events::OsWatcherRegistry> {
        self.os_watchers.clone()
    }

    /// Shared frontmost-poll registry (for server.rs startup wiring + P4-3
    /// hot reload + `os.status`).
    pub fn os_frontmost(&self) -> Arc<crate::os_frontmost::OsFrontmostRegistry> {
        self.os_frontmost.clone()
    }

    /// Handler-held footprint tracker (for server.rs startup wiring + P4-3
    /// hot reload + `os.status`).
    pub fn footprint_tracker(&self) -> Arc<crate::footprint_distill::FootprintTracker> {
        self.footprint.clone()
    }

    /// Register a background driver handle keyed by a stable name, aborting any
    /// prior handle for that key first (abort+respawn hot-reload pattern).
    pub(crate) async fn register_driver_handle(&self, key: &'static str, handle: tokio::task::JoinHandle<()>) {
        let mut map = self.driver_handles.lock().await;
        if let Some(old) = map.insert(key, handle) {
            old.abort();
        }
    }

    /// Abort and deregister a background driver handle. Returns whether one was
    /// running.
    pub(crate) async fn abort_driver_handle(&self, key: &'static str) -> bool {
        if let Some(old) = self.driver_handles.lock().await.remove(key) {
            old.abort();
            true
        } else {
            false
        }
    }

    /// (Re)build and spawn the autonomous goal-loop driver from current config.
    ///
    /// Shared by gateway startup and the `system.update_config` hot reload of
    /// `[goal_loop] iteration_cap_simple` / `[dispatch] policy`. The driver is a
    /// stateless periodic poller (durable state lives in SQLite / the task rows),
    /// so aborting the old task between ticks and respawning with fresh config is
    /// safe. Gated by `[dispatch] enabled` (the same gate as startup): when
    /// dispatch is disabled, any existing driver is aborted and none is spawned.
    ///
    /// Returns `true` iff a driver is now running.
    pub async fn respawn_goal_loop_driver(&self) -> bool {
        if !crate::dispatch_engine::dispatch_engine_enabled(&self.home_dir) {
            self.abort_driver_handle("goal_loop").await;
            return false;
        }
        let (Some(ts), Some(mq)) = (
            self.task_store.read().await.clone(),
            self.message_queue.read().await.clone(),
        ) else {
            warn!("goal loop driver not (re)started: task store or message queue unavailable");
            self.abort_driver_handle("goal_loop").await;
            return false;
        };

        let cfg = crate::goal_loop::GoalLoopConfig::from_home(&self.home_dir);
        // RFC-27: resolve the edition concurrency limit from the active edition
        // via the SAME `resolve_edition_profile()` chain every other edition
        // gate uses (no second source of truth — 鐵律 4). Personal → a small
        // default cap; Enterprise → `None` (the gate is a no-op). Resolved at
        // (re)spawn, so an edition/license change takes effect on the next
        // config reload — the same cadence at which `[goal_loop]` is re-read.
        let concurrency_cfg = duduclaw_core::ConcurrencyGateConfig::from_home(&self.home_dir);
        let concurrency_limit = duduclaw_core::concurrency_effective_limit(
            self.resolve_edition_profile().await,
            &concurrency_cfg,
        );
        let mut driver = crate::goal_loop::GoalLoopDriver::new(ts, mq, cfg)
            .with_home_dir(self.home_dir.clone())
            .with_concurrency_limit(
                concurrency_limit,
                concurrency_cfg.concurrency_lease_ttl_secs,
            );
        // WP-A9: wire the SAME forward-model `Arc` the `DispatchEngine`
        // settle hook uses (constructed once in `server.rs`, gated on
        // `[task_forward_model] enabled`). `None` ⇒ predict hook stays a
        // no-op, matching design §7.3's default-off contract.
        if let Some(fm) = self.forward_model().await {
            driver = driver.with_forward_model(fm);
        }
        if let Some(policy) = crate::dispatch_policy::build_policy(&self.home_dir) {
            info!(policy = %policy.kind().as_str(), "Goal loop: dispatch policy active");
            driver = driver.with_policy(policy);
        }
        match crate::approval::ApprovalBroker::open(&self.home_dir) {
            Ok(broker) => driver = driver.with_broker(Arc::new(broker)),
            Err(e) => warn!(
                error = %e,
                "Goal loop: ApprovalBroker unavailable — kickoff gate disabled"
            ),
        }
        let driver = Arc::new(driver);
        let handle = tokio::spawn(async move { driver.run().await });
        self.register_driver_handle("goal_loop", handle).await;
        info!("Goal loop driver (re)started");
        true
    }

    /// H6 (WP-B, `resume_on_restart`): boot-time reconciliation — see
    /// `goal_loop::pause_inflight_on_restart` for the actual scan/escalate
    /// logic (kept in `goal_loop.rs`; this method only supplies the store
    /// handles it needs, mirroring how `respawn_goal_loop_driver` above
    /// resolves them).
    ///
    /// **Deliberately called ONLY from the gateway boot path in
    /// `server.rs`** — NEVER from `respawn_goal_loop_driver`'s hot-reload
    /// call sites (`system.update_config`'s `[goal_loop]`/`[dispatch]`
    /// reload branches). Those fire on every unrelated config edit while
    /// the SAME process — and its already-tracked in-flight goals — keeps
    /// running; conflating that with an actual process restart would
    /// escalate live goals just because an operator tweaked an unrelated
    /// setting, which is not what `resume_on_restart` means.
    ///
    /// No-op when the task store is unavailable yet, or when
    /// `resume_on_restart` resolves to `Auto` (no longer the
    /// `GoalLoopConfig` default since WP-E — see `goal_loop::ResumeOnRestart`)
    /// — see `goal_loop::pause_inflight_on_restart`'s own no-op contract.
    pub async fn pause_inflight_goal_tasks_on_restart(&self) -> usize {
        let (Some(ts), Some(mq)) = (
            self.task_store.read().await.clone(),
            self.message_queue.read().await.clone(),
        ) else {
            return 0;
        };
        crate::goal_loop::pause_inflight_on_restart(ts, mq, &self.home_dir).await
    }

    /// (Re)build and spawn the dispatch engine (zombie reclaim + goal-mode
    /// acceptance review) from current config. Shared by gateway startup and
    /// the `system.update_config` hot reload of `[dispatch] enabled` — gated
    /// on that same flag, so `false` tears down any running engine and `true`
    /// (re)spawns one without a restart. Like the goal loop it is a stateless
    /// periodic poller over SQLite, so abort-between-ticks + respawn is safe.
    ///
    /// When `[task_forward_model] enabled = true` this also (re)uses — or, on
    /// a runtime false→true dispatch enable, constructs and registers — the
    /// shared forward-model `Arc`, so callers must respawn the goal-loop
    /// driver *after* this method to hand its predict hook the same `Arc`.
    /// Returns `true` iff the engine is now running.
    pub async fn respawn_dispatch_engine(&self) -> bool {
        if !crate::dispatch_engine::dispatch_engine_enabled(&self.home_dir) {
            self.abort_driver_handle("dispatch").await;
            return false;
        }
        let Some(ts) = self.task_store.read().await.clone() else {
            warn!("dispatch engine not (re)started: task store unavailable");
            self.abort_driver_handle("dispatch").await;
            return false;
        };
        let caller = crate::dispatch_engine::GoalAcceptanceCaller {
            home_dir: self.home_dir.clone(),
        };
        let judge: Arc<dyn crate::dispatch_engine::AcceptanceJudge> =
            Arc::new(crate::dispatch_engine::LlmAcceptanceJudge::new(caller));
        // H1 two-stage adjudication: the cheap first-stage evaluator runs on
        // the SAME utility choke-point as the panel (own caller instance — the
        // judge consumed the first). Always wired; `[dispatch] two_stage_judge`
        // (default true) is read at review time so the switch hot-reloads.
        let evaluator: Arc<dyn crate::dispatch_engine::PreAcceptanceEvaluator> =
            Arc::new(crate::dispatch_engine::LlmPreEvaluator::new(
                crate::dispatch_engine::GoalAcceptanceCaller {
                    home_dir: self.home_dir.clone(),
                },
            ));
        let mut builder = crate::dispatch_engine::DispatchEngine::new(ts, Some(judge))
            .with_evaluator(evaluator)
            // WP4 GroundEval: fold `tool_calls.jsonl` evidence into the
            // goal-mode acceptance judge prompt.
            .with_home_dir(self.home_dir.clone())
            // Iterative Kanban: share the goal loop's soft cap so a rejection
            // past it flags `diminishing` on the board.
            .with_soft_cap(crate::goal_loop::GoalLoopConfig::from_home(&self.home_dir).soft_cap);
        let tfm_cfg = crate::prediction::task_forward_store::TaskForwardModelConfig::from_home(
            &self.home_dir,
        );
        if tfm_cfg.enabled {
            // One coherent in-memory bucket cache: reuse the registered Arc
            // when present (boot or a prior respawn built it), construct and
            // register it otherwise (see `MethodHandler::forward_model` docs).
            let fm = match self.forward_model().await {
                Some(fm) => fm,
                None => {
                    let fm = Arc::new(
                        crate::prediction::task_forward_store::TaskForwardModel::new(
                            self.home_dir.join("prediction.db"),
                        ),
                    );
                    self.set_forward_model(fm.clone()).await;
                    info!("A3 task-forward-model enabled ([task_forward_model] enabled = true)");
                    fm
                }
            };
            builder = builder.with_forward_model(fm);
        }
        let engine = Arc::new(builder);
        let handle = tokio::spawn(async move { engine.run().await });
        self.register_driver_handle("dispatch", handle).await;
        info!("Dispatch engine (re)started (durable SQLite派工：殭屍回收 + goal-mode 驗收)");
        true
    }

    /// (Re)build and spawn the semi-automatic topology-evolution driver from
    /// current config. Shared by startup and the `system.update_config` hot
    /// reload of `[topology_evolution] enabled`. Like the goal loop it is a
    /// stateless periodic poller (durable state in SQLite / the ApprovalBroker),
    /// so abort-between-ticks + respawn is safe. When `enabled = false`, any
    /// existing driver is aborted and none is spawned (true→false teardown;
    /// false→true first spawn). Returns `true` iff a driver is now running.
    pub async fn respawn_topology_driver(&self) -> bool {
        if !crate::topology_evolution::enabled(&self.home_dir) {
            self.abort_driver_handle("topology").await;
            return false;
        }
        let Some(ts) = self.task_store.read().await.clone() else {
            warn!("topology evolution driver not (re)started: task store unavailable");
            self.abort_driver_handle("topology").await;
            return false;
        };
        let broker = match crate::approval::ApprovalBroker::open(&self.home_dir) {
            Ok(b) => Arc::new(b),
            Err(e) => {
                warn!(error = %e, "Topology evolution: ApprovalBroker unavailable — D5 disabled");
                self.abort_driver_handle("topology").await;
                return false;
            }
        };
        let cfg = crate::topology_evolution::TopologyEvolutionConfig::from_home(&self.home_dir);
        let driver = Arc::new(crate::topology_evolution::TopologyEvolutionDriver::new(
            ts,
            self.home_dir.clone(),
            broker,
            cfg,
        ));
        let handle = tokio::spawn(async move { driver.run().await });
        self.register_driver_handle("topology", handle).await;
        info!("Topology evolution driver (re)started");
        true
    }

    /// Hot stop/start one agent's OS-native background work after an
    /// `os_native` / `[os_watch]` edit committed to `agent.toml`. Reads the
    /// freshly-scanned registry and reconciles all three OS-native subsystems
    /// (P4-3) — filesystem watcher, frontmost poll task, and footprint
    /// aggregation membership — so a dashboard edit takes effect without a
    /// gateway restart. When `os_native = false`, everything is stopped;
    /// otherwise each subsystem (re)starts from its current `[os_watch]`
    /// config (a missing `paths` / `frontmost_poll_secs` / `footprint` field
    /// simply leaves that one subsystem idle). Returns whether a filesystem
    /// watcher is running after the reload.
    ///
    /// The `[proactive]` table needs no reload path here — `ProactiveGate`
    /// re-reads `read_proactive_config(agent_dir)` per evaluation, so a written
    /// toggle is effective on the next event with no cached state to bust.
    pub(crate) async fn hot_reload_os_watcher(&self, agent_id: &str) -> bool {
        let (os_native, footprint_on, agent_dir) = {
            let reg = self.registry.read().await;
            match reg.get(agent_id) {
                Some(a) => (
                    a.config.capabilities.os_native,
                    crate::footprint_distill::read_footprint_enabled(&a.dir),
                    a.dir.clone(),
                ),
                None => return false,
            }
        };
        // Footprint membership is reconciled regardless of the event bus
        // (aggregation is in-process, not bus-dependent).
        self.footprint
            .set_enabled(agent_id, os_native && footprint_on);

        if !os_native {
            self.os_watchers.stop_agent(agent_id).await;
            self.os_frontmost.stop_agent(agent_id).await;
            return false;
        }
        let Some(tx) = self.autopilot_event_tx.read().await.clone() else {
            // No autopilot event bus (task/autopilot store missing) — watchers
            // have nowhere to forward. Ensure none is left running.
            self.os_watchers.stop_agent(agent_id).await;
            self.os_frontmost.stop_agent(agent_id).await;
            warn!(agent = %agent_id, "os_watch hot reload skipped: autopilot event bus unavailable");
            return false;
        };
        // Frontmost polling reconciles from frontmost_poll_secs (0/absent ⇒ off).
        self.os_frontmost
            .start_agent(agent_id, &agent_dir, tx.clone())
            .await;
        self.os_watchers.start_agent(agent_id, &agent_dir, tx).await
    }

    /// Count agents (other than `except_agent_id`, by registry name) that
    /// already have `[capabilities] os_native = true` on disk. The "used"
    /// figure for the OS-native quota; `os.status` reports the total (no
    /// exclusion).
    pub(crate) async fn count_os_native_agents(&self, except_agent_id: Option<&str>) -> usize {
        self.registry
            .read()
            .await
            .list()
            .iter()
            .filter(|a| a.config.capabilities.os_native)
            .filter(|a| except_agent_id != Some(a.config.agent.name.as_str()))
            .count()
    }

    /// Fail-closed OS-native quota gate for the write path (`agents.update` /
    /// `os.settings.update`). Returns `Some(error_frame)` when setting
    /// `agent_id`'s `os_native` to `true` would exceed the edition quota
    /// (`license_runtime::os_native_agent_quota`), else `None` (allowed).
    /// Unlimited editions (Enterprise) always allow. Re-saving an agent that is
    /// already OS-native is never blocked (it is excluded from the count).
    pub(crate) async fn os_native_quota_reject(&self, agent_id: &str) -> Option<WsFrame> {
        let edition = self.resolve_edition_profile().await;
        let limit = crate::license_runtime::os_native_agent_quota(edition)?;
        let used_others = self.count_os_native_agents(Some(agent_id)).await;
        if used_others as u32 >= limit {
            Some(os_native_quota_reject_frame(limit))
        } else {
            None
        }
    }

    /// Notify the cron scheduler to reload immediately. Call this after any
    /// mutation (add / update / delete / enable-toggle). No-op if the
    /// scheduler has not been injected yet.
    pub(crate) async fn notify_cron_reload(&self) {
        if let Some(scheduler) = self.cron_scheduler.read().await.as_ref() {
            scheduler.reload_now();
        }
    }

    /// Get the extension reference.
    pub fn extension(&self) -> &Arc<dyn GatewayExtension> {
        &self.extension
    }

    /// Inject user database and JWT config (called once after gateway start).
    pub async fn set_user_db(&self, db: Arc<UserDb>, jwt: Arc<JwtConfig>) {
        *self.user_db.write().await = Some(db);
        *self.jwt_config.write().await = Some(jwt);
    }

    /// Inject the reply context for hot-starting channels. Called once after
    /// ReplyContext is constructed in server.rs.
    pub async fn set_reply_ctx(&self, ctx: Arc<crate::channel_reply::ReplyContext>) {
        *self.reply_ctx.write().await = Some(ctx);
    }

    /// Register a running channel handle (for hot-stop on remove).
    /// If a handle with the same name already exists, it is aborted first.
    pub async fn register_channel_handle(&self, name: &str, handle: tokio::task::JoinHandle<()>) {
        let mut handles = self.channel_handles.lock().await;
        if let Some(old) = handles.insert(name.to_string(), handle) {
            old.abort();
        }
    }

    // WP12 (M6): `set_channel_state` was removed here. It was a zero-caller
    // duplicate of `channel_reply::set_channel_connected` that bypassed both the
    // credential redaction and the snapshot/broadcast path — a trap for the next
    // channel author. Use `set_channel_connected` instead.

    /// Get the shared channel status map for use by channel bots.
    pub fn channel_status(&self) -> &Arc<RwLock<std::collections::HashMap<String, ChannelState>>> {
        &self.channel_status
    }

    /// Get a reference to the shared agent registry.
    pub fn registry(&self) -> &Arc<RwLock<AgentRegistry>> {
        &self.registry
    }

    /// Get the home directory path.
    pub fn home_dir(&self) -> &Path {
        &self.home_dir
    }

    /// Get the pending OAuth flows map (used by HTTP callback handler).
    pub fn mcp_oauth_pending(
        &self,
    ) -> &RwLock<std::collections::HashMap<String, crate::mcp_oauth::PendingOAuth>> {
        &self.mcp_oauth_pending
    }

    /// Set the heartbeat scheduler reference (called after gateway start).
    pub async fn set_heartbeat(&self, scheduler: Arc<duduclaw_agent::HeartbeatScheduler>) {
        *self.heartbeat.write().await = Some(scheduler);
    }
}
