//! [`ApprovalBroker`] — the single HITL request/decide/await primitive.
//! Moved verbatim out of `approval.rs`.

use super::*;

impl ApprovalBroker {
    pub fn new(store: std::sync::Arc<ApprovalStore>) -> Self {
        Self { store }
    }

    /// Open the on-disk store and wrap it in a broker.
    pub fn open(home_dir: &Path) -> Result<Self, String> {
        Ok(Self::new(std::sync::Arc::new(ApprovalStore::open(
            home_dir,
        )?)))
    }

    /// Record a new pending approval. `payload` is the exact thing to
    /// re-dispatch once approved. A non-positive `ttl` falls back to
    /// [`DEFAULT_TTL_SECONDS`] (a zero/negative TTL would mean "expire
    /// immediately", a fail-closed footgun for callers who forget it).
    pub async fn request(
        &self,
        agent_id: &str,
        action_kind: &str,
        summary: &str,
        payload: Value,
        ttl_seconds: i64,
    ) -> Result<ApprovalId, String> {
        let ttl = if ttl_seconds > 0 {
            ttl_seconds
        } else {
            DEFAULT_TTL_SECONDS
        };
        let rec = ApprovalRecord {
            id: ApprovalId::new(),
            agent_id: agent_id.to_string(),
            action_kind: action_kind.to_string(),
            summary: summary.to_string(),
            payload,
            status: ApprovalStatus::Pending,
            created_at: Utc::now().to_rfc3339(),
            decided_at: None,
            decided_by: None,
            ttl_seconds: ttl,
            notify_channel: None,
            notify_chat_id: None,
            reminded_at: None,
            simulation: None,
            request_kind: RequestKind::Approval,
            binding: None,
            answer: None,
            invalidated_reason: None,
        };
        let id = rec.id.clone();
        self.store.insert(&rec).await?;
        info!(
            approval_id = %id,
            agent_id,
            action_kind,
            ttl_seconds = ttl,
            "approval requested"
        );
        // WP20: a pending approval nobody can see is a guaranteed TTL denial.
        // Push it to the humans who can decide it, on the channel they are
        // actually on. Best-effort and time-boxed — a channel outage must never
        // stop the approval from being filed.
        self.push_new_request(&rec).await;
        Ok(id)
    }

    /// Same as [`Self::request`], but additionally stamps a D1 **simulation
    /// narrative** (WebDreamer arXiv:2411.06559) on the row — the ActionGuard
    /// judge's structured "what will the world look like after this call
    /// runs" output. Deliberately a separate method rather than a new
    /// parameter on [`Self::request`]: `request` has ~20 existing call sites
    /// across the codebase and none of them need to change to pick this up.
    /// `simulation` should be built via [`SimulationNarrative::to_json`]; a
    /// `Value::Null` (or any value [`SimulationNarrative::from_json`] reads as
    /// empty) is stored as `None` so a caller that has nothing to say behaves
    /// exactly like [`Self::request`].
    pub async fn request_with_simulation(
        &self,
        agent_id: &str,
        action_kind: &str,
        summary: &str,
        payload: Value,
        ttl_seconds: i64,
        simulation: Value,
    ) -> Result<ApprovalId, String> {
        let ttl = if ttl_seconds > 0 {
            ttl_seconds
        } else {
            DEFAULT_TTL_SECONDS
        };
        let simulation = SimulationNarrative::from_json(&simulation);
        let rec = ApprovalRecord {
            id: ApprovalId::new(),
            agent_id: agent_id.to_string(),
            action_kind: action_kind.to_string(),
            summary: summary.to_string(),
            payload,
            status: ApprovalStatus::Pending,
            created_at: Utc::now().to_rfc3339(),
            decided_at: None,
            decided_by: None,
            ttl_seconds: ttl,
            notify_channel: None,
            notify_chat_id: None,
            reminded_at: None,
            request_kind: RequestKind::Approval,
            binding: None,
            answer: None,
            invalidated_reason: None,
            simulation: if simulation.is_empty() {
                None
            } else {
                Some(simulation.to_json())
            },
        };
        let id = rec.id.clone();
        self.store.insert(&rec).await?;
        info!(
            approval_id = %id,
            agent_id,
            action_kind,
            ttl_seconds = ttl,
            "approval requested (with simulation narrative)"
        );
        self.push_new_request(&rec).await;
        Ok(id)
    }

    /// The DuDuClaw home directory backing this broker, or `None` for an
    /// in-memory (test) store. Channel notification needs it to read the
    /// encrypted channel config, the agent registry, and `users.db`; deriving
    /// it from `approvals.db`'s parent keeps [`ApprovalBroker::request`]'s
    /// signature untouched (every existing caller keeps working) while making
    /// the notification automatically OFF under `open_in_memory` — so no unit
    /// test ever attempts a network send.
    pub(super) fn home_dir(&self) -> Option<PathBuf> {
        self.store
            .db_path
            .as_ref()
            .and_then(|p| p.parent().map(Path::to_path_buf))
    }

    /// WP20: push the freshly-filed approval to a channel and record where it
    /// landed. Silent no-op for in-memory stores and for kinds that own their
    /// own notification ([`SELF_NOTIFYING_KINDS`]).
    pub(super) async fn push_new_request(&self, rec: &ApprovalRecord) {
        if SELF_NOTIFYING_KINDS.contains(&rec.action_kind.as_str()) {
            return;
        }
        let Some(home) = self.home_dir() else { return };
        let fut = crate::approval_notify::notify_new_approval(&home, rec);
        match tokio::time::timeout(NOTIFY_TIMEOUT, fut).await {
            Ok(Some((channel, chat_id))) => {
                if let Err(e) = self
                    .store
                    .set_notify_target(&rec.id, &channel, &chat_id)
                    .await
                {
                    warn!(approval_id = %rec.id, error = %e, "approval push: target write failed");
                }
            }
            Ok(None) => {
                warn!(
                    approval_id = %rec.id,
                    action_kind = %rec.action_kind,
                    "approval push: no reachable channel destination — this approval \
                     is only visible in the dashboard and WILL auto-deny at TTL"
                );
            }
            Err(_) => warn!(approval_id = %rec.id, "approval push timed out"),
        }
    }

    /// WP20: send the "about to auto-deny" nudge exactly once, if due. Called
    /// from the paths that already read a pending row, so no new loop exists.
    pub(super) async fn maybe_remind(&self, rec: &ApprovalRecord, now: DateTime<Utc>) {
        if !rec.reminder_due(now) {
            return;
        }
        // F5-C (review F4-L7): a bound request is answered only in the exact
        // account, conversation and person it is bound to, by full id. The
        // legacy reminder would push a button card with an employee's bot
        // token to `notify_chat_id`, so bound rows are never reminded here.
        if rec.binding.is_some() {
            return;
        }
        // P2-A M-3: kinds whose pushes are capped elsewhere get no reminder —
        // a reminder would bypass that cap (and, for a responsibility
        // question, carry the employee's text with buttons).
        if super::no_reminder(&rec.action_kind) {
            return;
        }
        let Some(home) = self.home_dir() else { return };
        // Claim first, send second: losing the race means someone else is
        // sending, and a claim that is never followed by a successful send is
        // strictly better than a reminder storm.
        match self.store.claim_reminder(&rec.id, &now.to_rfc3339()).await {
            Ok(true) => {
                // B5: an admin who already has the dashboard open in a
                // browser tab gets routed straight to the inbox row at the
                // exact instant the channel nudge fires below — no reason
                // to make them separately notice a channel ping when the tab
                // is already open. Independent of the channel push outcome:
                // same-origin dashboard navigation is free and does not need
                // a reachable channel destination (unlike `notify_reminder`,
                // which may find none — see `push_new_request`'s
                // "dashboard-only" warning case).
                crate::dashboard_navigate::push_dashboard_navigate(&reminder_navigate_path(
                    &rec.id,
                ));

                let fut = crate::approval_notify::notify_reminder(&home, rec);
                match tokio::time::timeout(NOTIFY_TIMEOUT, fut).await {
                    // The reminder is also the retry: when the FIRST push found
                    // no destination (or failed), `notify_reminder` re-resolves
                    // the chain and may land somewhere new. That destination
                    // must be written back — `notify_chat_id` is what the
                    // inbound button handler matches the presser against, so
                    // without this the reminder would carry buttons that can
                    // never authorize anyone (dead buttons, the exact
                    // silent-failure class WP20 exists to remove).
                    Ok(Some((channel, chat_id))) => {
                        if notify_target_changed(rec, &channel, &chat_id) {
                            if let Err(e) = self
                                .store
                                .set_notify_target(&rec.id, &channel, &chat_id)
                                .await
                            {
                                warn!(approval_id = %rec.id, error = %e,
                                      "approval reminder: target write-back failed");
                            }
                        }
                    }
                    Ok(None) => warn!(
                        approval_id = %rec.id,
                        "approval reminder: still no reachable destination"
                    ),
                    Err(_) => warn!(approval_id = %rec.id, "approval reminder timed out"),
                }
            }
            Ok(false) => {}
            Err(e) => warn!(approval_id = %rec.id, error = %e, "approval reminder claim failed"),
        }
    }

    /// Fetch the full record (payload included) for re-dispatch.
    pub async fn get(&self, id: &ApprovalId) -> Result<Option<ApprovalRecord>, String> {
        self.store.get(id).await
    }

    /// Current status. Opportunistically expires the record first so a
    /// caller polling past the TTL observes `Expired` without needing a
    /// separate sweep.
    pub async fn poll(&self, id: &ApprovalId) -> Result<ApprovalStatus, String> {
        let rec = self
            .store
            .get(id)
            .await?
            .ok_or_else(|| format!("approval {id} not found"))?;
        let now = Utc::now();
        if rec.is_stale(now) {
            // Best-effort expire; ignore race (someone may have just decided).
            let _ = self
                .store
                .decide_if_pending(
                    id,
                    ApprovalStatus::Expired,
                    DECIDED_BY_TTL,
                    &now.to_rfc3339(),
                )
                .await?;
            // Re-read to report the authoritative post-expiry status.
            let fresh = self.store.get(id).await?;
            return Ok(fresh.map(|r| r.status).unwrap_or(ApprovalStatus::Expired));
        }
        // WP20: `await_decision` drives this every couple of seconds while a
        // caller blocks, so the ⅔-TTL nudge rides along for free.
        self.maybe_remind(&rec, now).await;
        Ok(rec.status)
    }

    /// Approve or deny a pending approval. Idempotent-safe: refuses to
    /// change a terminal state (a second decide — including double-approve
    /// — is rejected). The store's `WHERE status = 'pending'` guard closes
    /// the concurrent-decider race.
    pub async fn decide(
        &self,
        id: &ApprovalId,
        approve: bool,
        decided_by: &str,
    ) -> Result<(), String> {
        let rec = self
            .store
            .get(id)
            .await?
            .ok_or_else(|| format!("approval {id} not found"))?;
        if rec.binding.is_some() || rec.request_kind != RequestKind::Approval {
            return Err("bound requests require authenticated decision context".into());
        }
        if rec.is_stale(Utc::now()) {
            self.store
                .decide_if_pending(
                    id,
                    ApprovalStatus::Expired,
                    DECIDED_BY_TTL,
                    &Utc::now().to_rfc3339(),
                )
                .await?;
            return Err("approval expired".into());
        }
        if rec.status.is_terminal() {
            return Err(format!(
                "approval {id} already {} — refusing to change terminal state",
                rec.status.as_str()
            ));
        }
        let new_status = if approve {
            ApprovalStatus::Approved
        } else {
            ApprovalStatus::Denied
        };
        let n = self
            .store
            .decide_if_pending(id, new_status, decided_by, &Utc::now().to_rfc3339())
            .await?;
        if n == 0 {
            // Lost the race to another decider between get() and update.
            return Err(format!("approval {id} was decided concurrently"));
        }
        info!(approval_id = %id, decision = new_status.as_str(), decided_by, "approval decided");
        Ok(())
    }

    /// Test-only seam: stamp the delivered notification destination without
    /// going through a real channel send, so `approval_notify`'s inbound tests
    /// can exercise the destination-match authorization path.
    #[cfg(test)]
    pub(crate) async fn set_notify_target_for_test(
        &self,
        id: &ApprovalId,
        channel: &str,
        chat_id: &str,
    ) -> Result<(), String> {
        self.store.set_notify_target(id, channel, chat_id).await
    }

    /// All pending approvals, optionally filtered to one agent. Sweeps
    /// stale rows first so the returned set never contains an expired
    /// pending row.
    pub async fn list_pending(
        &self,
        agent_id: Option<&str>,
    ) -> Result<Vec<ApprovalRecord>, String> {
        self.expire_stale().await?;
        self.store.list_pending(agent_id).await
    }

    /// Every approval of one `action_kind`, any status.
    pub async fn list_by_kind(&self, kind: &str) -> Result<Vec<ApprovalRecord>, String> {
        self.store.list_by_kind(kind).await
    }

    /// Withdraw a pending approval as a system DENY (`decided_by` names why).
    /// Returns `false` when it was no longer pending.
    pub async fn withdraw(&self, id: &ApprovalId, decided_by: &str) -> Result<bool, String> {
        let n = self
            .store
            .decide_if_pending(
                id,
                ApprovalStatus::Denied,
                decided_by,
                &Utc::now().to_rfc3339(),
            )
            .await?;
        Ok(n > 0)
    }

    /// Overwrite an approval's summary and payload (data-subject erase).
    pub async fn replace_text(
        &self,
        id: &ApprovalId,
        summary: &str,
        payload: &Value,
    ) -> Result<(), String> {
        self.store
            .replace_text(id, summary, payload)
            .await
            .map(|_| ())
    }

    /// Sweep: mark every pending approval past its TTL as `expired`.
    /// Returns the number expired. TTL expiry counts as DENY.
    pub async fn expire_stale(&self) -> Result<u64, String> {
        let now = Utc::now();
        let pending = self.store.list_pending(None).await?;
        let mut expired = 0u64;
        for rec in pending {
            if rec.is_stale(now) {
                let n = self
                    .store
                    .decide_if_pending(
                        &rec.id,
                        ApprovalStatus::Expired,
                        DECIDED_BY_TTL,
                        &now.to_rfc3339(),
                    )
                    .await?;
                expired += n as u64;
            } else if rec.reminder_due(now) && self.home_dir().is_some() {
                // WP20: piggyback the ⅔-TTL reminder on the sweep that already
                // walks every pending row — covers approvals nobody is polling.
                //
                // Detached, unlike the `poll` path: `expire_stale` runs inside
                // the dashboard's `approvals.list` RPC, and awaiting N channel
                // sends there would make a UI call as slow as the slowest bot
                // API. The once-only DB claim inside `maybe_remind` still
                // guarantees a single send per approval.
                let broker = self.clone();
                tokio::spawn(async move { broker.maybe_remind(&rec, now).await });
            }
        }
        if expired > 0 {
            info!(
                count = expired,
                "approvals expired by TTL (treated as deny)"
            );
        }
        Ok(expired)
    }

    /// Block until the approval reaches a terminal state or its TTL
    /// elapses, polling every `poll_interval`. Returns `Expired` on TTL —
    /// which callers MUST treat as a denial (fail-closed). Max wait is
    /// bounded by the record's own TTL.
    pub async fn await_decision(
        &self,
        id: &ApprovalId,
        poll_interval: Duration,
    ) -> Result<ApprovalStatus, String> {
        // Bound the loop by the record's TTL so we can never wait forever.
        let deadline = {
            let rec = self
                .store
                .get(id)
                .await?
                .ok_or_else(|| format!("approval {id} not found"))?;
            rec.expires_at()
        };
        loop {
            let status = self.poll(id).await?;
            if status.is_terminal() {
                return Ok(status);
            }
            // Past deadline but poll() hasn't expired it yet (clock skew /
            // unparseable ts already handled inside poll) — force expire.
            if let Some(exp) = deadline {
                if Utc::now() >= exp {
                    let _ = self
                        .store
                        .decide_if_pending(
                            id,
                            ApprovalStatus::Expired,
                            DECIDED_BY_TTL,
                            &Utc::now().to_rfc3339(),
                        )
                        .await?;
                    return Ok(ApprovalStatus::Expired);
                }
            }
            tokio::time::sleep(poll_interval).await;
        }
    }
}

/// B5: the dashboard route a "⅔-TTL about to auto-deny" push routes an open
/// tab to — the exact `/inbox?item=<id>` H5 deep-link contract `InboxPage`
/// already reads (see `deep_link.rs`'s `DeepLinkKind::Approval` for the
/// external-URL twin of this same route, used by the channel-side push).
pub(super) fn reminder_navigate_path(id: &ApprovalId) -> String {
    format!("/inbox?item={}", id.as_str())
}

// ── Decision source: agent.toml [capabilities] ──────────────
