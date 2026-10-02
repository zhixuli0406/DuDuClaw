//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    pub(crate) async fn handle_approvals_decide(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let id = match params.get("id").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return WsFrame::error_response("", "id is required"),
        };
        let approve = params
            .get("approve")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let reason = params
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let broker = match crate::approval::ApprovalBroker::open(&self.home_dir) {
            Ok(b) => b,
            Err(e) => return WsFrame::error_response("", &format!("open approvals: {e}")),
        };
        let approval_id = crate::approval::ApprovalId::from(id.clone());
        // Snapshot the record BEFORE deciding — we need its action_kind +
        // payload to run any post-decision side-effect (e.g. skill_create).
        let record = match Self::required_approval_for_decision(broker.get(&approval_id).await) {
            Ok(rec) => Some(rec),
            Err(response) => return response,
        };
        // L1: a knowledge review past its deadline is a denial (TTL = DENY),
        // even before the expiry sweep has flipped its status — approving it
        // would write knowledge on a card the reviewer could no longer rely on.
        if let Some(rec) = &record {
            if crate::approval_notify::is_dashboard_only_kind(&rec.action_kind)
                && rec.is_stale(chrono::Utc::now())
            {
                let _ = broker.poll(&approval_id).await;
                return WsFrame::error_response(
                    "",
                    "這則知識審核已逾期，已自動捨棄，無法再核准。",
                );
            }
        }
        // R-M5: a knowledge review applies its side effect FIRST and records
        // the decision only after it succeeded — a failed side effect leaves
        // the approval pending (retryable) instead of terminal with the claim
        // stranded.
        if let Some(rec) = &record {
            if rec.action_kind == crate::wiki_ingest::ACTION_KIND_KNOWLEDGE_QUARANTINE {
                return self
                    .decide_knowledge_quarantine(&broker, &approval_id, rec, approve, ctx)
                    .await;
            }
        }
        // Board-kind gate (WP17 invariant).
        if let Some(rec) = &record {
            let kind = crate::governance::ApprovalKind::parse(&rec.action_kind);
            // Decision Lab is admin-only. A manager who cannot inspect the
            // exact synthetic run must not mint its human-inspection receipt.
            if rec.action_kind == "support_pilot_review" && !ctx.is_admin() {
                return WsFrame::error_response(
                    "",
                    "synthetic pilot review requires an admin who can inspect Decision Lab",
                );
            }
            if kind.requires_board() && !ctx.is_admin() {
                return WsFrame::error_response(
                    "",
                    "this approval kind requires board (admin) rights to decide",
                );
            }
        }
        if record.as_ref().is_some_and(|record| record.action_kind == "discovery") {
            let caller = match crate::discovery::service::TrustedCaller::from_user(ctx) {
                Ok(caller) => caller, Err(error) => return WsFrame::error_response("", &error),
            };
            let store = match self.task_store().await { Ok(store) => store, Err(frame) => return frame };
            if let Err(error) = crate::discovery::service::prepare_approval_decision(&store, &broker, &caller, &approval_id, approve).await {
                return WsFrame::error_response("", &error);
            }
        }
        let decided_by = format!("dashboard:{}", ctx.user_id);
        if let Err(e) = broker.decide(&approval_id, approve, &decided_by).await {
            return WsFrame::error_response("", &format!("decide: {e}"));
        }

        if record.as_ref().is_some_and(|record| record.action_kind == "discovery") {
            let caller = match crate::discovery::service::TrustedCaller::from_user(ctx) {
                Ok(caller) => caller, Err(error) => return WsFrame::error_response("", &error),
            };
            let store = match self.task_store().await { Ok(store) => store, Err(frame) => return frame };
            if let Err(error) = crate::discovery::service::authorize_approved_request(&store, &broker, &caller, &approval_id).await {
                return WsFrame::error_response("", &error);
            }
        }

        // H1 (unified decision hand-off, 07-unified-decision-design.md §6):
        // a dashboard decision must retire the channel card(s) this approval
        // fanned out to, the same way a channel button press does — otherwise
        // "decided on the dashboard" leaves a stale, still-clickable card
        // behind. Fire-and-forget: an edit is cosmetic and must never delay
        // or fail a decision already durable in `approvals.db`.
        if let Some(rec) = &record {
            let decider_name = self.user_display_name(&ctx.user_id).await;
            crate::approval_notify::spawn_dashboard_collapse(
                self.home_dir.clone(),
                rec.clone(),
                approve,
                decider_name,
            );
        }

        // ── S1 (PORTICO): mint an epoch-bound capability on approve ──
        // Approving is no longer a permanent grant. If the approval payload
        // carries a `scope_epoch` (a task_id / session_id subgoal key), mint
        // a revocable capability handle bound to it; closing that subgoal
        // (task done / session end) auto-revokes the handle. Absent a
        // scope_epoch we skip minting — the legacy one-shot behaviour — so
        // this is additive and never blocks existing approve flows.
        let mut capability_handle: Value = Value::Null;
        if approve {
            if let Some(rec) = &record {
                if let Some(epoch) = rec.payload.get("scope_epoch").and_then(|v| v.as_str()) {
                    if !epoch.is_empty() {
                        match crate::capability::CapabilityBroker::open(&self.home_dir) {
                            Ok(caps) => {
                                let ttl = rec
                                    .payload
                                    .get("capability_ttl_seconds")
                                    .and_then(|v| v.as_i64())
                                    .unwrap_or(rec.ttl_seconds);
                                match caps
                                    .grant_from_approval(&broker, &approval_id, epoch, ttl, None)
                                    .await
                                {
                                    Ok(g) => capability_handle = json!(g.handle_id),
                                    Err(e) => {
                                        // Fail-closed: minting failed ⇒ report,
                                        // do NOT silently proceed as if granted.
                                        return WsFrame::error_response(
                                            "",
                                            &format!("已核准但核發授權憑證失敗（保守拒絕）：{e}"),
                                        );
                                    }
                                }
                            }
                            Err(e) => {
                                return WsFrame::error_response(
                                    "",
                                    &format!("開啟授權憑證儲存失敗：{e}"),
                                );
                            }
                        }
                    }
                }
            }
        }

        // ── Side-effect: custom skill creation (V13-T13.0) ──
        // Approver identity is already gated by require_manager!; routing to a
        // human's manager is a fallback here (no manager_id column yet ⇒ any
        // admin/manager may approve; single-admin self-approval is audited).
        let mut side_effect: Value = Value::Null;
        if let Some(rec) = &record {
            if rec.action_kind == crate::custom_skills::ACTION_KIND_SKILL_CREATE {
                if approve {
                    match self
                        .install_approved_custom_skill(&id, rec, &ctx.user_id)
                        .await
                    {
                        Ok(name) => {
                            let created_by = rec
                                .payload
                                .get("created_by_user")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            side_effect = json!({
                                "installed_skill": name,
                                "self_approved": crate::custom_skills::is_self_approval(created_by, &ctx.user_id),
                            });
                        }
                        Err(e) => {
                            return WsFrame::error_response(
                                "",
                                &format!("approved, but install side-effect failed: {e}"),
                            );
                        }
                    }
                } else {
                    // Deny → mark the registry row rejected with the reason.
                    if let Some(cs_id) = rec.payload.get("custom_skill_id").and_then(|v| v.as_str())
                    {
                        if let Ok(store) = self.custom_skill_store() {
                            let _ = store
                                .transition(
                                    cs_id,
                                    crate::custom_skills::CustomSkillStatus::Rejected,
                                    None,
                                    Some(if reason.is_empty() {
                                        "rejected by approver"
                                    } else {
                                        &reason
                                    }),
                                    false,
                                )
                                .await;
                        }
                        side_effect = json!({ "custom_skill_rejected": cs_id });
                    }
                }
            }
        }

        WsFrame::ok_response(
            "",
            json!({
                "id": id,
                "decided": if approve { "approved" } else { "denied" },
                "side_effect": side_effect,
                "capability_handle": capability_handle,
            }),
        )
    }

    /// `approvals.decide` for `knowledge_quarantine` (dashboard-only kind).
    ///
    /// Order: the card must be pending and not past its deadline; the side
    /// effect runs; only then is the decision recorded. If the side effect
    /// fails, the approval stays pending and the caller gets the error — a
    /// retry is idempotent (applied rows are skipped; rows a failed release
    /// already converted to held claims are re-reported and get their card).
    /// Concurrent decisions on one card are serialized by a per-id lock and
    /// the card is re-read inside it, so the second one finds the card no
    /// longer pending and gets an error frame without applying anything. If
    /// the side effect applied but `decide` then fails (expiry, store error),
    /// the response is an error frame naming what was applied, plus the audit
    /// row `knowledge_review_decision_unrecorded` — never an OK frame.
    async fn decide_knowledge_quarantine(
        &self,
        broker: &crate::approval::ApprovalBroker,
        approval_id: &crate::approval::ApprovalId,
        rec: &crate::approval::ApprovalRecord,
        approve: bool,
        ctx: &UserContext,
    ) -> WsFrame {
        // Serialize side effect + decision per approval id (the gateway is
        // the only writer of decisions for this dashboard-only kind), then
        // re-read the card inside the lock: a concurrent approve/deny or an
        // expiry that happened while waiting is seen here, before anything
        // is applied.
        let lock = knowledge_review_lock(approval_id.as_str());
        let _guard = lock.lock().await;
        let fresh = match broker.get(approval_id).await {
            Ok(Some(r)) => r,
            Ok(None) => return WsFrame::error_response("", "找不到這筆審核"),
            Err(e) => return WsFrame::error_response("", &format!("讀取審核失敗：{e}")),
        };
        let rec = &fresh;
        if rec.status != crate::approval::ApprovalStatus::Pending {
            return WsFrame::error_response(
                "",
                &format!("approval {} already {}", rec.id, rec.status.as_str()),
            );
        }
        if rec.is_stale(chrono::Utc::now()) {
            let _ = broker.poll(approval_id).await;
            return WsFrame::error_response("", "這則知識審核已逾期，已自動捨棄，無法再核准。");
        }
        let memory_db = rec
            .payload
            .get("memory_db")
            .and_then(|v| v.as_str())
            .map(std::path::PathBuf::from);
        let q_agent = rec
            .payload
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or(rec.agent_id.as_str())
            .to_string();
        let ids: Vec<String> = rec
            .payload
            .get("quarantined_ids")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        // Trust-held claims are promoted with the approver's authority,
        // bound to the digest of the row the card showed (R-H1).
        let promote = rec
            .payload
            .get("promote_on_approve")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let claim_digest = rec
            .payload
            .get("claim_digest")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let origin = match (
            rec.payload.get("origin_channel").and_then(|v| v.as_str()),
            rec.payload.get("origin_chat_id").and_then(|v| v.as_str()),
        ) {
            (Some(ch), Some(chat)) => Some((ch.to_string(), chat.to_string())),
            _ => None,
        };

        let mut side_effect = Value::Null;
        match memory_db {
            Some(db) if !ids.is_empty() => {
                match crate::wiki_ingest::apply_quarantine_decision(
                    self.home_dir.clone(),
                    db.clone(),
                    q_agent.clone(),
                    ids,
                    approve,
                    promote,
                    claim_digest,
                )
                .await
                {
                    Ok(report) => {
                        side_effect = report.side_effect(approve, promote);
                        if !report.held.is_empty() {
                            crate::wiki_ingest::dispatch_quarantine_side_effects(
                                &q_agent,
                                &self.home_dir,
                                &db,
                                &report.held,
                                origin.as_ref(),
                            )
                            .await;
                        }
                    }
                    Err(e) => {
                        return WsFrame::error_response(
                            "",
                            &format!("沒有套用，這筆審核仍待決定，可以再試一次：{e}"),
                        );
                    }
                }
            }
            _ => warn!("knowledge_quarantine approval missing memory_db/ids in payload"),
        }

        let decided_by = format!("dashboard:{}", ctx.user_id);
        let decided = {
            #[cfg(test)]
            let forced = take_forced_decide_failure(approval_id.as_str());
            #[cfg(not(test))]
            let forced: Option<String> = None;
            match forced {
                Some(e) => Err(e),
                None => broker.decide(approval_id, approve, &decided_by).await,
            }
        };
        if let Err(e) = decided {
            // The side effect is applied but the decision is not recorded
            // (the card expired in between, or a real store error): never
            // report success. Say exactly what was applied, and leave an
            // audit row so the mismatch can be traced.
            let action = if approve { "approve" } else { "deny" };
            warn!(approval_id = %approval_id, "knowledge review decision unrecorded: {e}");
            duduclaw_security::audit::append_audit_event(
                &self.home_dir,
                &duduclaw_security::audit::AuditEvent::new(
                    AUDIT_KNOWLEDGE_REVIEW_DECISION_UNRECORDED,
                    &q_agent,
                    duduclaw_security::audit::Severity::Warning,
                    json!({
                        "approval_id": rec.id.as_str(),
                        "action": action,
                        "side_effect": side_effect,
                        "error": duduclaw_core::truncate_chars(&e, 300),
                    }),
                ),
            );
            return WsFrame::error_response(
                "",
                &format!(
                    "已套用（{action}：{side_effect}），但這筆審核的決定沒有記錄成功（{err}）。\
                     資料以已套用的結果為準，已寫入稽核紀錄 {AUDIT_KNOWLEDGE_REVIEW_DECISION_UNRECORDED}。",
                    err = duduclaw_core::truncate_chars(&e, 300),
                ),
            );
        }
        let decider_name = self.user_display_name(&ctx.user_id).await;
        crate::approval_notify::spawn_dashboard_collapse(
            self.home_dir.clone(),
            rec.clone(),
            approve,
            decider_name,
        );
        WsFrame::ok_response(
            "",
            json!({
                "id": rec.id.as_str(),
                "decided": if approve { "approved" } else { "denied" },
                "side_effect": side_effect,
                "capability_handle": Value::Null,
            }),
        )
    }

    /// WP14-T14.6: recent budget-breaker incidents from `budget_events.jsonl`,
    /// newest first, plus a per-agent open-count. `limit` default 50.
    pub(crate) async fn handle_budget_incidents(&self, params: Value) -> WsFrame {
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
        let path = self.home_dir.join("budget_events.jsonl");
        let raw = std::fs::read_to_string(&path).unwrap_or_default();
        let mut events: Vec<Value> = raw
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .collect();
        events.reverse(); // newest first
        let mut per_agent: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();
        for e in &events {
            if let Some(a) = e.get("agent_id").and_then(|v| v.as_str()) {
                *per_agent.entry(a.to_string()).or_insert(0) += 1;
            }
        }
        let recent: Vec<Value> = events.into_iter().take(limit).collect();
        let agents: Vec<Value> = per_agent
            .into_iter()
            .map(|(agent_id, count)| json!({ "agent_id": agent_id, "open_events": count }))
            .collect();
        WsFrame::ok_response("", json!({ "incidents": recent, "by_agent": agents }))
    }

    /// WP10-T10.1: skill leaderboard ranked by estimated minutes saved. Scans
    /// global + per-agent skills, reads `estimated_minutes_saved` from metadata.
    /// Only skills carrying an estimate (i.e. approved via WP8's flow) appear —
    /// drafts have no estimate and are naturally excluded (no fabricated data).
    pub(crate) async fn handle_skills_leaderboard(&self, params: Value) -> WsFrame {
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
        let loc = duduclaw_agent::skill_loader::DEFAULT_SKILL_LOCALE;

        // Build the (dir, scope, owner) scan list, then await each.
        let mut targets: Vec<(std::path::PathBuf, &'static str, String)> =
            vec![(self.home_dir.join("skills"), "global", String::new())];
        if let Ok(rd) = std::fs::read_dir(self.home_dir.join("agents")) {
            for entry in rd.flatten() {
                if entry.path().is_dir() {
                    let agent = entry.file_name().to_string_lossy().to_string();
                    targets.push((entry.path().join("SKILLS"), "agent", agent));
                }
            }
        }

        let mut rows: Vec<Value> = Vec::new();
        for (dir, scope, owner) in targets {
            for sk in duduclaw_agent::registry::AgentRegistry::load_skills(&dir).await {
                let meta = duduclaw_agent::skill_loader::parse_skill_meta_from_content(
                    &sk.content,
                    &sk.name,
                );
                if let Some(mins) = meta.estimated_minutes_saved {
                    rows.push(json!({
                        "skill": sk.name,
                        "display_name": meta.display_name(loc),
                        "estimated_minutes_saved": mins,
                        "scope": scope,
                        "owner": owner,
                    }));
                }
            }
        }

        rows.sort_by(|a, b| {
            b["estimated_minutes_saved"]
                .as_u64()
                .unwrap_or(0)
                .cmp(&a["estimated_minutes_saved"].as_u64().unwrap_or(0))
        });
        rows.truncate(limit);
        WsFrame::ok_response(
            "",
            json!({
                "leaderboard": rows,
                "metric": "estimated_minutes_saved",
                "note": "Ranked by per-use minutes saved; usage-count multiplication pending a persisted counter.",
            }),
        )
    }
}

/// Audit event: a knowledge-review side effect was applied but its decision
/// could not be recorded.
pub(crate) const AUDIT_KNOWLEDGE_REVIEW_DECISION_UNRECORDED: &str =
    "knowledge_review_decision_unrecorded";

/// Per-approval-id lock serializing side effect + decision for knowledge
/// reviews. Entries are tiny and keyed by approval id; they are dropped when
/// no request holds them.
fn knowledge_review_lock(id: &str) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    static LOCKS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>>,
    > = std::sync::OnceLock::new();
    let map = LOCKS.get_or_init(Default::default);
    let mut m = map.lock().unwrap_or_else(|p| p.into_inner());
    m.retain(|_, w| w.strong_count() > 0);
    if let Some(l) = m.get(id).and_then(std::sync::Weak::upgrade) {
        return l;
    }
    let l = std::sync::Arc::new(tokio::sync::Mutex::new(()));
    m.insert(id.to_string(), std::sync::Arc::downgrade(&l));
    l
}

#[cfg(test)]
static FORCED_DECIDE_FAILURE: std::sync::Mutex<Option<(String, String)>> = std::sync::Mutex::new(None);

/// Test seam: make the next `decide` for `id` fail with `err`.
#[cfg(test)]
pub(crate) fn force_decide_failure(id: &str, err: &str) {
    *FORCED_DECIDE_FAILURE.lock().unwrap() = Some((id.to_string(), err.to_string()));
}

#[cfg(test)]
fn take_forced_decide_failure(id: &str) -> Option<String> {
    let mut g = FORCED_DECIDE_FAILURE.lock().unwrap();
    match g.as_ref() {
        Some((i, _)) if i == id => g.take().map(|(_, e)| e),
        _ => None,
    }
}
