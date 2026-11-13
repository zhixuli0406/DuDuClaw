//! v1.82 fail-closed approval gate — the engine side. See
//! [`crate::autopilot_approval`] for the full lifecycle and the defect it
//! closes. Everything here fails closed: no path performs a gated action
//! except [`AutopilotEngine::execute_approved`], after the frozen payload was
//! verified against the ledger, the ledger row claimed, and — under the rule
//! fence that rule writes wait on — the rule found unchanged and enabled, its
//! breaker closed, the autonomy setting allowing it, the decision inside the
//! execution window, and the approval consumed.

use super::*;

use crate::approval::{ApprovalBroker, ApprovalId, ApprovalRecord, ApprovalStatus};
use crate::autopilot_approval::{
    self as gate, FrozenDispatch, NoticePush, RESULT_APPROVAL_ERROR, RESULT_AWAITING,
    RESULT_BLOCKED, RESULT_BREAKER_OPEN, RESULT_DENIED, RESULT_EXPIRED, RESULT_FAILURE,
    RESULT_SKIPPED, RESULT_STALE, RESULT_SUCCESS, RESULT_UNCERTAIN,
};
use crate::autopilot_store::{
    AutopilotDispatchRow, DISPATCH_AWAITING, DISPATCH_EXECUTING, DISPATCH_REQUESTING,
    DISPATCH_SETTLED, PUSH_GAVE_UP, PUSH_NO_TARGET, PUSH_PENDING, PUSH_QUEUED, PUSH_SENT,
    PUSH_SUPPRESSED,
};

/// Test replacement of the outcome chat push: `(rule_id, body) -> result`.
#[cfg(test)]
pub(super) type TestPusher = Arc<dyn Fn(&str, &str) -> NoticePush + Send + Sync>;

impl AutopilotEngine {
    // ── Startup ─────────────────────────────────────────────

    /// Loud startup report of the gate's wiring, plus a warning for every
    /// stored rule whose `require_approval` is not a JSON boolean (such a
    /// rule is treated as requiring approval).
    pub(super) async fn log_approval_gate_wiring(&self) {
        let rules = match self.store.list_rules().await {
            Ok(rules) => rules,
            Err(e) => {
                warn!(error = %e, "autopilot: could not list rules for the approval-gate startup check");
                Vec::new()
            }
        };
        let mut gated = 0usize;
        for rule in &rules {
            let action: Value = serde_json::from_str(&rule.action).unwrap_or(Value::Null);
            if let Some(odd) = crate::approval::require_approval_malformed(&action) {
                warn!(
                    rule = %rule.name,
                    rule_id = %rule.id,
                    value = %duduclaw_core::truncate_chars(&odd.to_string(), 40),
                    "autopilot: stored rule has a non-boolean require_approval — treating it as true \
                     (fail closed); fix the rule to true or false"
                );
            }
            if crate::approval::rule_requires_approval(&action) {
                gated += 1;
            }
        }
        if self.approval_broker.is_some() {
            info!(gated_rules = gated, "autopilot: approval gate attached — gated rules wait for an Admin's approval");
        } else {
            error!(
                gated_rules = gated,
                "autopilot: NO approval service attached — every rule with require_approval is \
                 BLOCKED (fail closed) and will not run until the gateway attaches the ApprovalBroker"
            );
        }
    }

    // ── Trigger ─────────────────────────────────────────────

    /// A matched, breaker-allowed (and screened) rule that requires approval.
    /// Files one approval freezing the rendered action, or blocks. Never
    /// performs the action.
    pub(super) async fn gate_on_approval(
        &self,
        rule: &AutopilotRuleRow,
        event_name: &str,
        action: &Value,
        fields: &serde_json::Map<String, Value>,
        screen_note: Option<String>,
    ) {
        let note = |base: String| match &screen_note {
            Some(n) => format!("{n}｜{base}"),
            None => base,
        };
        if let Some(odd) = crate::approval::require_approval_malformed(action) {
            warn!(
                rule_id = %rule.id,
                value = %duduclaw_core::truncate_chars(&odd.to_string(), 40),
                "autopilot: require_approval is not a boolean — treating it as true (fail closed)"
            );
        }
        // Same re-check `execute_action` makes after an awaited screening.
        if let Err(reason) = self.autonomy_check(action, fields) {
            self.record_fire(rule, RESULT_SKIPPED, note(reason)).await;
            return;
        }
        let Some(broker) = self.approval_broker.clone() else {
            error!(
                rule = %rule.name,
                rule_id = %rule.id,
                "autopilot: rule requires approval but no approval service is attached — action BLOCKED"
            );
            let detail = format!("需要核准，但核准服務沒有接上；動作沒有執行（{event_name}）");
            self.record_fire(rule, RESULT_BLOCKED, note(detail.clone())).await;
            self.approval_notice(&rule.id, &rule.name, RESULT_BLOCKED, &detail).await;
            return;
        };

        // Render exactly what would run, from the perception-neutralized
        // copy (P2-5), so the card never resurrects a raw injection payload.
        let perception = sanitize_perception_fields(fields, &self.home_dir);
        let eff_fields: &serde_json::Map<String, Value> = match &perception {
            Some((m, _)) => m,
            None => fields,
        };
        let banner = perception.as_ref().and_then(|(_, b)| b.as_deref());
        let frozen = match self
            .prepare_action(&rule.id, &rule.name, action, fields, eff_fields, banner)
            .await
        {
            Ok(Some(frozen)) => frozen,
            Ok(None) => {
                // ProactiveGate said "do not interrupt": nothing to approve.
                self.record_fire(
                    rule,
                    RESULT_SUCCESS,
                    note(format!("Triggered by {event_name}（AI 判斷不需要通知，未送審）")),
                )
                .await;
                return;
            }
            Err(e) => {
                self.record_fire(rule, RESULT_FAILURE, note(e)).await;
                return;
            }
        };

        let rule_hash = gate::rule_revision_hash(rule);
        let dispatch = FrozenDispatch {
            dispatch_id: uuid::Uuid::new_v4().to_string(),
            rule_id: rule.id.clone(),
            rule_name: rule.name.clone(),
            rule_hash: rule_hash.clone(),
            event: event_name.to_string(),
            event_ref: gate::event_ref(event_name, fields),
            source_agent: fields.get("agent_id").and_then(Value::as_str).map(str::to_string),
            action: frozen,
        };
        let now_utc = chrono::Utc::now();
        let ttl = crate::approval::DEFAULT_TTL_SECONDS;
        let latest = gate::latest_execution_at(now_utc, ttl);
        let payload = dispatch.approval_payload(&latest);
        if !gate::credential_free(&payload) {
            let detail = "動作內容含有金鑰或憑證，不能送審；動作沒有執行".to_string();
            self.record_fire(rule, RESULT_APPROVAL_ERROR, note(detail.clone())).await;
            self.approval_notice(&rule.id, &rule.name, RESULT_APPROVAL_ERROR, &detail).await;
            return;
        }
        // H1: the dashboard shows the complete frozen action; a payload too
        // large to review reliably is never filed.
        if !gate::payload_within_limit(&payload) {
            let detail = format!(
                "動作內容超過 {} KiB，無法完整送審；動作沒有執行",
                gate::MAX_FROZEN_PAYLOAD_BYTES / 1024
            );
            self.record_fire(rule, RESULT_APPROVAL_ERROR, note(detail.clone())).await;
            self.approval_notice(&rule.id, &rule.name, RESULT_APPROVAL_ERROR, &detail).await;
            return;
        }
        let now = now_utc.to_rfc3339();
        let dedupe = gate::dedupe_key(&rule.id, &rule_hash, event_name, fields);
        let ledger = dispatch.ledger_row(&now, &dedupe);
        match self.store.insert_dispatch(&ledger).await {
            Ok(true) => {}
            Ok(false) => {
                // M1: the same ingress event already filed a card for this
                // rule revision (e.g. read by a second gateway process).
                info!(
                    rule_id = %rule.id,
                    event = event_name,
                    "autopilot: duplicate ingress event for an approval-gated rule — no second card"
                );
                self.record_fire(
                    rule,
                    RESULT_SKIPPED,
                    note(format!("同一個事件已經為這條規則送審過，不重複送審（{event_name}）")),
                )
                .await;
                return;
            }
            Err(e) => {
                let detail = format!("無法寫入核准紀錄：{e}");
                self.record_fire(rule, RESULT_APPROVAL_ERROR, note(detail.clone())).await;
                self.approval_notice(&rule.id, &rule.name, RESULT_APPROVAL_ERROR, &detail).await;
                return;
            }
        }
        let filed = broker
            .request(gate::REQUESTER, gate::ACTION_KIND, &dispatch.summary(&latest), payload, ttl)
            .await;
        let approval_id = match filed {
            Ok(id) => id,
            Err(e) => {
                let detail = format!("無法建立核准單：{e}");
                self.settle_fire(rule, &ledger, note(detail)).await;
                return;
            }
        };
        match self
            .store
            .mark_dispatch_awaiting(&dispatch.dispatch_id, approval_id.as_str())
            .await
        {
            Ok(true) => {}
            other => {
                // The ledger cannot track this card: withdraw it so it can
                // never be approved into an untracked execution.
                let _ = broker.withdraw(&approval_id, "system:autopilot_ledger_error").await;
                let detail = match other {
                    Err(e) => format!("核准單已撤回：無法更新核准紀錄（{e}）"),
                    _ => "核准單已撤回：核准紀錄已被其他流程處理".to_string(),
                };
                self.settle_fire(rule, &ledger, note(detail)).await;
                return;
            }
        }
        info!(
            approval_id = %approval_id,
            dispatch_id = %dispatch.dispatch_id,
            rule = %rule.name,
            rule_id = %rule.id,
            "autopilot: action gated on human approval — nothing runs until an Admin approves"
        );
        self.record_fire(
            rule,
            RESULT_AWAITING,
            note(format!(
                "等待管理員核准（核准單 {}，{event_name}）",
                duduclaw_core::truncate_chars(approval_id.as_str(), 8)
            )),
        )
        .await;
        self.activity(
            "autopilot_awaiting_approval",
            format!("自動化規則「{}」在 {event_name} 觸發，等待管理員核准", rule.name),
            json_meta(&rule.id, &dispatch.dispatch_id, Some(approval_id.as_str())),
        )
        .await;
    }

    // ── Resumer ─────────────────────────────────────────────

    /// One pass over `awaiting` ledger rows: settle denied / expired cards
    /// and execute approved ones. Returns how many rows changed state.
    /// Callers run it only in the process holding the gateway lock.
    pub(crate) async fn resume_approvals_once(&self) -> usize {
        let Some(broker) = self.approval_broker.clone() else { return 0 };
        let rows = match self.store.list_dispatches(DISPATCH_AWAITING, gate::RESUME_BATCH).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!(error = %e, "autopilot: could not read the approval ledger");
                return 0;
            }
        };
        let mut changed = 0;
        for row in rows {
            if self.advance_dispatch(&broker, row).await {
                changed += 1;
            }
        }
        changed
    }

    async fn advance_dispatch(&self, broker: &ApprovalBroker, row: AutopilotDispatchRow) -> bool {
        let Some(approval_id) = row.approval_id.clone() else {
            return self
                .settle(&row, DISPATCH_AWAITING, RESULT_APPROVAL_ERROR, "核准紀錄沒有核准單編號，動作沒有執行", true)
                .await;
        };
        let id = ApprovalId::from(approval_id);
        let rec = match broker.get(&id).await {
            Ok(Some(rec)) => rec,
            Ok(None) => {
                return self
                    .settle(&row, DISPATCH_AWAITING, RESULT_APPROVAL_ERROR, "核准單不存在，動作沒有執行", true)
                    .await;
            }
            Err(e) => {
                // Transient store error: try again on the next tick.
                warn!(dispatch_id = %row.dispatch_id, error = %e, "autopilot: approval read failed");
                return false;
            }
        };
        match rec.status {
            ApprovalStatus::Pending => {
                // Expiry = deny: let the broker flip a stale card; the next
                // tick settles it as `expired`.
                if rec.is_stale(chrono::Utc::now()) {
                    let _ = broker.poll(&id).await;
                }
                false
            }
            ApprovalStatus::Denied => {
                let who = rec.decided_by.as_deref().unwrap_or("unknown");
                self.settle(&row, DISPATCH_AWAITING, RESULT_DENIED, &format!("核准被拒絕（{who}），動作沒有執行"), false)
                    .await
            }
            ApprovalStatus::Expired => {
                self.settle(&row, DISPATCH_AWAITING, RESULT_EXPIRED, "核准逾時，視同拒絕，動作沒有執行", false)
                    .await
            }
            ApprovalStatus::Invalidated => {
                let why = rec.invalidated_reason.as_deref().unwrap_or("unknown");
                self.settle(&row, DISPATCH_AWAITING, RESULT_DENIED, &format!("核准單已失效（{why}），動作沒有執行"), false)
                    .await
            }
            ApprovalStatus::Answered => {
                self.settle(&row, DISPATCH_AWAITING, RESULT_APPROVAL_ERROR, "核准單狀態不符，動作沒有執行", true)
                    .await
            }
            ApprovalStatus::Approved => self.execute_approved(broker, &row, &rec).await,
        }
    }

    /// Execute one approved card's frozen action — at most once.
    async fn execute_approved(
        &self,
        broker: &ApprovalBroker,
        row: &AutopilotDispatchRow,
        rec: &ApprovalRecord,
    ) -> bool {
        // 1. The card must carry exactly the dispatch the ledger froze.
        let dispatch = match self.card_matches(row, rec) {
            Ok(d) => d,
            Err(why) => {
                self.retire(broker, rec, row, "mismatch").await;
                return self.settle(row, DISPATCH_AWAITING, RESULT_APPROVAL_ERROR, &why, true).await;
            }
        };
        // 2. Single-use claim of the ledger row.
        match self.store.transition_dispatch(&row.dispatch_id, DISPATCH_AWAITING, DISPATCH_EXECUTING).await {
            Ok(true) => {}
            Ok(false) => return false,
            Err(e) => {
                warn!(dispatch_id = %row.dispatch_id, error = %e, "autopilot: could not claim approved dispatch");
                return false;
            }
        }
        #[cfg(test)]
        {
            let hook = self.after_claim_hook.lock().unwrap_or_else(|p| p.into_inner()).take();
            if let Some(hook) = hook {
                hook();
            }
        }
        // 3. Final checks under the rule fence (M2): rule update / delete /
        //    disable — in this process or any other — waits until the
        //    approval is consumed, so nothing can change between the check
        //    and the point of no return.
        let (fence, rule) = match self.store.fence_rule(&row.rule_id).await {
            Ok(taken) => taken,
            Err(e) => {
                // Cannot tell: release the claim (nothing consumed, nothing
                // ran) and retry on the next tick.
                warn!(dispatch_id = %row.dispatch_id, error = %e, "autopilot: rule fence failed before execution");
                let _ = self
                    .store
                    .transition_dispatch(&row.dispatch_id, DISPATCH_EXECUTING, DISPATCH_AWAITING)
                    .await;
                return false;
            }
        };
        // 3a. The rule must still exist, be enabled and be the frozen revision.
        let stale = match &rule {
            None => Some("規則在核准後被刪除"),
            Some(rule) if !rule.enabled => Some("規則在送審後被停用"),
            Some(rule) if gate::rule_revision_hash(rule) != row.rule_hash => Some("規則在送審後被修改"),
            Some(_) => None,
        };
        if let Some(why) = stale {
            drop(fence);
            self.retire(broker, rec, row, "stale").await;
            return self
                .settle(row, DISPATCH_EXECUTING, RESULT_STALE, &format!("{why}，已核准的動作沒有執行"), true)
                .await;
        }
        // 3b. The approval is acted on within the execution window after
        //     the decision (and never with a decision time in the future).
        if let Err(why) = decision_window_check(rec, chrono::Utc::now()) {
            drop(fence);
            self.retire(broker, rec, row, "too_old").await;
            return self.settle(row, DISPATCH_EXECUTING, RESULT_EXPIRED, why, true).await;
        }
        // 3c. The rule's circuit breaker, as it is now.
        if self.breaker_blocks_execution(&row.rule_id).await {
            drop(fence);
            self.retire(broker, rec, row, "breaker_open").await;
            return self
                .settle(
                    row,
                    DISPATCH_EXECUTING,
                    RESULT_BREAKER_OPEN,
                    "這條規則的斷路器在執行時是開啟的（疑似觸發迴圈），已核准的動作沒有執行",
                    true,
                )
                .await;
        }
        // 3d. Autonomy pauses still narrow at execution time.
        if let Err(pause) = self.autonomy_check_frozen(&dispatch) {
            drop(fence);
            self.retire(broker, rec, row, "autonomy_paused").await;
            return self.settle(row, DISPATCH_EXECUTING, RESULT_SKIPPED, &pause, false).await;
        }
        // 4. Consume the approval (approved → invalidated) — the point of no
        //    return — still under the fence.
        let reason = format!("{}{}", gate::CONSUMED_PREFIX, row.dispatch_id);
        let consumed = broker.consume_autopilot_approval(rec, &reason).await;
        if let Err(e) = fence.release() {
            // Nothing was written under the fence; the decision stands.
            warn!(dispatch_id = %row.dispatch_id, error = %e, "autopilot: rule fence release failed");
        }
        match consumed {
            Ok(true) => {}
            Ok(false) => {
                return self
                    .settle(row, DISPATCH_EXECUTING, RESULT_APPROVAL_ERROR, "核准單已被使用或已變更，動作沒有執行", true)
                    .await;
            }
            Err(e) => {
                return self
                    .settle(row, DISPATCH_EXECUTING, RESULT_APPROVAL_ERROR, &format!("無法使用核准單：{e}"), true)
                    .await;
            }
        }
        // 5. Perform exactly the frozen action, once.
        let who = rec.decided_by.as_deref().unwrap_or("unknown");
        let outcome = self.perform_action(&dispatch.action).await;
        let (result, detail) = match &outcome {
            Ok(()) => (RESULT_SUCCESS, format!("已由 {who} 核准並執行（{}）", row.event_name)),
            Err(e) => (RESULT_FAILURE, format!("已由 {who} 核准，但執行失敗：{e}")),
        };
        info!(
            dispatch_id = %row.dispatch_id,
            rule_id = %row.rule_id,
            approval_id = %rec.id,
            result,
            "autopilot: approved action executed"
        );
        // The `autopilot_triggered` Activity entry is written by the outbox
        // right after the settle (one per dispatch, re-driven after a crash).
        self.settle(row, DISPATCH_EXECUTING, result, &detail, false).await
    }

    fn card_matches(&self, row: &AutopilotDispatchRow, rec: &ApprovalRecord) -> Result<FrozenDispatch, String> {
        if rec.action_kind != gate::ACTION_KIND || rec.agent_id != gate::REQUESTER {
            return Err("核准單種類不符，動作沒有執行".into());
        }
        if row.approval_id.as_deref() != Some(rec.id.as_str()) {
            return Err("核准單與核准紀錄不符，動作沒有執行".into());
        }
        gate::verify_card_payload(row, &rec.payload).map_err(|e| format!("核准內容與送審內容不符（{e}），動作沒有執行"))
    }

    /// Global pause, the target employee and the employee whose event caused
    /// the fire — the same three checks the immediate path makes.
    fn autonomy_check_frozen(&self, dispatch: &FrozenDispatch) -> Result<(), String> {
        use duduclaw_core::autonomy_gate::{self, AutonomyPath};
        autonomy_gate::check_global(&self.home_dir, AutonomyPath::Autopilot)
            .map_err(|pause| format!("autonomy paused: {pause}"))?;
        for agent in [dispatch.action.target_agent(), dispatch.source_agent.as_deref()]
            .into_iter()
            .flatten()
        {
            autonomy_gate::check(&self.home_dir, agent, AutonomyPath::Autopilot)
                .map_err(|pause| format!("autonomy paused: {pause}"))?;
        }
        Ok(())
    }

    /// Retire an approved card without executing it (best effort: the ledger
    /// row is settled either way, and no other path executes the card).
    async fn retire(&self, broker: &ApprovalBroker, rec: &ApprovalRecord, row: &AutopilotDispatchRow, why: &str) {
        let reason = format!("{}{why}:{}", gate::RETIRED_PREFIX, row.dispatch_id);
        if let Err(e) = broker.consume_autopilot_approval(rec, &reason).await {
            warn!(approval_id = %rec.id, error = %e, "autopilot: could not retire approval card");
        }
    }

    // ── Boot recovery ───────────────────────────────────────

    /// Settle ledger rows a previous gateway process left half-way, then
    /// re-drive every outcome announcement still owed. Runs once at engine
    /// start, in the process holding the gateway lock, before the resumer:
    /// no execution of this process is in flight yet.
    pub(crate) async fn recover_approval_dispatches(&self) {
        if let Some(broker) = self.approval_broker.clone() {
            // Crashed between "ledger row written" and "approval id recorded".
            if let Ok(rows) = self.store.list_dispatches(DISPATCH_REQUESTING, 500).await {
                for row in rows {
                    self.recover_requesting(&broker, &row).await;
                }
            }
            // Crashed between "claimed" and "settled".
            if let Ok(rows) = self.store.list_dispatches(DISPATCH_EXECUTING, 500).await {
                for row in rows {
                    self.recover_executing(&broker, &row).await;
                }
            }
        }
        // Crashed between "settled" and "announced" (Activity entry / chat
        // push). A bounded number of batches here; whatever is left (or
        // fails now) is picked up by the periodic sweep.
        for _ in 0..20 {
            if self.announce_pending_notices().await < gate::NOTICE_SWEEP_BATCH as usize {
                break;
            }
        }
    }

    async fn recover_requesting(&self, broker: &ApprovalBroker, row: &AutopilotDispatchRow) {
        let cards = match broker.autopilot_cards_for_dispatch(&row.dispatch_id).await {
            Ok(cards) => cards,
            Err(e) => {
                warn!(dispatch_id = %row.dispatch_id, error = %e, "autopilot: recovery could not read approvals");
                return;
            }
        };
        let matching: Vec<&ApprovalRecord> = cards
            .iter()
            .filter(|rec| rec.agent_id == gate::REQUESTER && gate::verify_card_payload(row, &rec.payload).is_ok())
            .collect();
        if let [only] = matching.as_slice() {
            if let Ok(true) = self.store.mark_dispatch_awaiting(&row.dispatch_id, only.id.as_str()).await {
                info!(dispatch_id = %row.dispatch_id, "autopilot: recovered an approval filed before a restart");
                return;
            }
        }
        // No card (it was never filed) or an ambiguous set: nothing ran and
        // nothing may run from these cards.
        for rec in &cards {
            if rec.status == ApprovalStatus::Pending {
                let _ = broker.withdraw(&rec.id, "system:autopilot_recovery").await;
            } else if rec.status == ApprovalStatus::Approved {
                self.retire(broker, rec, row, "recovery").await;
            }
        }
        self.settle(row, DISPATCH_REQUESTING, RESULT_APPROVAL_ERROR, "閘道在送審途中中斷，這次觸發沒有執行", true)
            .await;
    }

    async fn recover_executing(&self, broker: &ApprovalBroker, row: &AutopilotDispatchRow) {
        let rec = match row.approval_id.as_deref() {
            Some(id) => match broker.get(&ApprovalId::from(id.to_string())).await {
                Ok(rec) => rec,
                Err(e) => {
                    warn!(dispatch_id = %row.dispatch_id, error = %e, "autopilot: recovery could not read approval");
                    return;
                }
            },
            None => None,
        };
        let consumed = format!("{}{}", gate::CONSUMED_PREFIX, row.dispatch_id);
        match rec {
            // Nothing runs before the consume: the action never started.
            // Back to awaiting; the resumer re-checks everything (including
            // the execution window).
            Some(rec) if rec.status == ApprovalStatus::Approved => {
                let _ = self
                    .store
                    .transition_dispatch(&row.dispatch_id, DISPATCH_EXECUTING, DISPATCH_AWAITING)
                    .await;
                info!(dispatch_id = %row.dispatch_id, "autopilot: approved action had not started before the restart; it is re-checked and may run once");
            }
            // Consumed by this dispatch: the action may have run. Never again.
            Some(rec)
                if rec.status == ApprovalStatus::Invalidated
                    && rec.invalidated_reason.as_deref() == Some(consumed.as_str()) =>
            {
                error!(dispatch_id = %row.dispatch_id, rule_id = %row.rule_id,
                    "autopilot: gateway stopped while an approved action was executing — outcome unknown, not re-run");
                self.settle(
                    row,
                    DISPATCH_EXECUTING,
                    RESULT_UNCERTAIN,
                    "閘道在執行已核准動作時中斷，無法確認是否已執行；不會自動重做",
                    true,
                )
                .await;
            }
            // Retired by this dispatch (stale / paused / breaker) before the crash.
            Some(rec)
                if rec.status == ApprovalStatus::Invalidated
                    && rec
                        .invalidated_reason
                        .as_deref()
                        .is_some_and(|r| r.starts_with(gate::RETIRED_PREFIX) && r.ends_with(row.dispatch_id.as_str())) =>
            {
                self.settle(row, DISPATCH_EXECUTING, RESULT_STALE, "核准後未執行（重新啟動前已判定不執行）", true)
                    .await;
            }
            _ => {
                self.settle(row, DISPATCH_EXECUTING, RESULT_APPROVAL_ERROR, "重新啟動後找不到可用的核准單，動作沒有執行", true)
                    .await;
            }
        }
    }

    // ── Bookkeeping ─────────────────────────────────────────

    /// Settle a ledger row — in ONE transaction with its history row and the
    /// announcements it owes (`AutopilotStore::settle_dispatch`) — then
    /// announce it ([`Self::announce_row`]). `notice`: an Admin must also hear
    /// about this outcome by chat (always true for [`gate::always_noticed`]
    /// outcomes). Returns whether this caller moved the row.
    async fn settle(&self, row: &AutopilotDispatchRow, from: &str, outcome: &str, detail: &str, notice: bool) -> bool {
        self.settle_with(row, from, outcome, detail, notice, false).await
    }

    async fn settle_with(
        &self,
        row: &AutopilotDispatchRow,
        from: &str,
        outcome: &str,
        detail: &str,
        notice: bool,
        count_fire: bool,
    ) -> bool {
        let push = notice || gate::always_noticed(outcome);
        match self
            .store
            .settle_dispatch(&row.dispatch_id, from, outcome, detail, push, count_fire)
            .await
        {
            Ok(true) => {}
            Ok(false) => return false,
            Err(e) => {
                warn!(dispatch_id = %row.dispatch_id, error = %e, "autopilot: could not settle approval ledger row");
                return false;
            }
        }
        #[cfg(test)]
        {
            if self.crash_after_settle.load(std::sync::atomic::Ordering::SeqCst) {
                return true;
            }
        }
        self.announce_dispatch(&row.dispatch_id).await;
        true
    }

    /// A gated fire that failed while being filed (its ledger row is still
    /// `requesting`): settle it `approval_error`, counting the fire, with an
    /// Admin notice. If the ledger row cannot be settled, fall back to the
    /// plain history row and the rate-limited trigger-time notice (boot
    /// recovery settles a row left `requesting`).
    async fn settle_fire(&self, rule: &AutopilotRuleRow, ledger: &AutopilotDispatchRow, detail: String) {
        if self
            .settle_with(ledger, DISPATCH_REQUESTING, RESULT_APPROVAL_ERROR, &detail, true, true)
            .await
        {
            return;
        }
        self.record_fire(rule, RESULT_APPROVAL_ERROR, detail.clone()).await;
        self.approval_notice(&rule.id, &rule.name, RESULT_APPROVAL_ERROR, &detail).await;
    }

    // ── Outcome announcements (durable outbox) ──────────────

    /// One batch of the sweep: announce every settled row that still owes
    /// its Activity Feed entry or a due chat push. Returns how many rows it
    /// looked at. Runs only in the process holding the gateway lock — at
    /// boot ([`Self::recover_approval_dispatches`]) and on every resumer tick.
    pub(crate) async fn announce_pending_notices(&self) -> usize {
        let now = chrono::Utc::now().to_rfc3339();
        let rows = match self.store.list_dispatch_notices_due(&now, gate::NOTICE_SWEEP_BATCH).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!(error = %e, "autopilot: could not read owed approval-gate announcements");
                return 0;
            }
        };
        let looked_at = rows.len();
        for row in rows {
            self.announce_row(&row).await;
        }
        looked_at
    }

    async fn announce_dispatch(&self, dispatch_id: &str) {
        match self.store.get_dispatch(dispatch_id).await {
            Ok(Some(row)) => self.announce_row(&row).await,
            Ok(None) => {}
            Err(e) => {
                warn!(dispatch_id, error = %e, "autopilot: could not read a settled dispatch; the sweep announces it")
            }
        }
    }

    /// Write what a settled row still owes: its one Activity Feed entry
    /// (then the best-effort live event), and its chat push. Each is marked
    /// done only after it succeeded; whatever is left stays owed and is
    /// re-driven by the next sweep.
    async fn announce_row(&self, row: &AutopilotDispatchRow) {
        if row.state != DISPATCH_SETTLED {
            return;
        }
        let outcome = row.outcome.as_deref().unwrap_or(RESULT_APPROVAL_ERROR);
        let detail = row.detail.as_deref().unwrap_or_default();
        if row.notice_activity_pending {
            self.announce_activity(row, outcome, detail).await;
        }
        if row.notice_push_state.as_deref() == Some(PUSH_PENDING) {
            self.drive_notice_push(row, outcome, detail).await;
        }
    }

    async fn announce_activity(&self, row: &AutopilotDispatchRow, outcome: &str, detail: &str) {
        let (event_type, summary) = gate::outcome_activity(&row.rule_name, outcome, detail);
        let meta = serde_json::json!({
            "rule_id": row.rule_id,
            "dispatch_id": row.dispatch_id,
            "approval_id": row.approval_id,
            "result": outcome,
        });
        let entry = ActivityRow {
            // One per dispatch: a re-drive after a crash inserts nothing.
            id: gate::dispatch_activity_id(&row.dispatch_id),
            event_type: event_type.into(),
            agent_id: "autopilot".into(),
            task_id: None,
            summary,
            // When the outcome was settled, not when it was (re)announced.
            timestamp: row.updated_at.clone(),
            metadata: Some(meta.to_string()),
        };
        if let Err(e) = self.task_store.append_activity_once(&entry).await {
            warn!(dispatch_id = %row.dispatch_id, error = %e,
                "autopilot: outcome Activity entry not written yet; the sweep retries it");
            return;
        }
        match self.store.mark_dispatch_activity_done(&row.dispatch_id).await {
            Ok(true) if event_type == gate::ACTIVITY_BLOCKED => {
                crate::dashboard_feedback::emit_live(&self.home_dir, "autopilot.approval_blocked", meta);
            }
            Ok(_) => {}
            Err(e) => {
                warn!(dispatch_id = %row.dispatch_id, error = %e,
                    "autopilot: could not mark the outcome Activity entry written; the sweep re-checks it")
            }
        }
    }

    /// One attempt of an owed chat push, claimed under a lease so it is never
    /// in flight twice. Limited outcomes are held to one push per rule per
    /// hour (decided before the first attempt).
    async fn drive_notice_push(&self, row: &AutopilotDispatchRow, outcome: &str, detail: &str) {
        let id = row.dispatch_id.as_str();
        if row.notice_push_attempts == 0
            && !gate::always_noticed(outcome)
            && !gate::notice_allowed(&self.approval_notices, &row.rule_id, Instant::now())
        {
            debug!(dispatch_id = id, rule_id = %row.rule_id, outcome,
                "autopilot: outcome chat push suppressed (one per rule per hour)");
            if let Err(e) = self.store.finish_dispatch_notice_push(id, PUSH_SUPPRESSED).await {
                warn!(dispatch_id = id, error = %e, "autopilot: could not record a suppressed outcome push");
            }
            return;
        }
        if row.notice_push_attempts >= gate::MAX_NOTICE_PUSH_ATTEMPTS {
            // The last attempt was interrupted before its result was recorded.
            error!(dispatch_id = id, rule_id = %row.rule_id, attempts = row.notice_push_attempts,
                "autopilot: outcome chat push did not succeed in any attempt — giving up (the Activity Feed entry remains)");
            if let Err(e) = self.store.finish_dispatch_notice_push(id, PUSH_GAVE_UP).await {
                warn!(dispatch_id = id, error = %e, "autopilot: could not record a given-up outcome push");
            }
            return;
        }
        let now = chrono::Utc::now();
        let lease = now + chrono::Duration::seconds(gate::NOTICE_PUSH_LEASE_SECS);
        match self
            .store
            .claim_dispatch_notice_push(id, row.notice_push_attempts, &now.to_rfc3339(), &lease.to_rfc3339())
            .await
        {
            Ok(true) => {}
            // Not due yet, or another sweep holds this attempt.
            Ok(false) => return,
            Err(e) => {
                warn!(dispatch_id = id, error = %e, "autopilot: could not claim an outcome push; the sweep retries it");
                return;
            }
        }
        let attempt = row.notice_push_attempts + 1;
        let body = gate::notice_body(&row.rule_name, outcome, detail);
        #[cfg(test)]
        {
            let pusher = self.notice_pusher.lock().unwrap_or_else(|p| p.into_inner()).clone();
            if let Some(pusher) = pusher {
                let result = pusher(&row.rule_id, &body);
                record_notice_push(&self.store, id, attempt, result).await;
                return;
            }
        }
        // Detached: a slow channel never stalls the engine loop. The lease
        // keeps the sweep from starting a second attempt meanwhile.
        let store = self.store.clone();
        let home = self.home_dir.clone();
        let rule_id = row.rule_id.clone();
        let dispatch_id = id.to_string();
        tokio::spawn(async move {
            let result = gate::push_admin_notice(&home, &rule_id, &body).await;
            record_notice_push(&store, &dispatch_id, attempt, result).await;
        });
    }

    /// History row for the fire itself (bumps the rule's trigger counter,
    /// like every pre-v1.82 fire).
    async fn record_fire(&self, rule: &AutopilotRuleRow, result: &str, details: String) {
        let _ = self
            .store
            .append_history(&AutopilotHistoryRow {
                id: uuid::Uuid::new_v4().to_string(),
                rule_id: rule.id.clone(),
                rule_name: rule.name.clone(),
                triggered_at: chrono::Utc::now().to_rfc3339(),
                result: result.to_string(),
                details: Some(details),
            })
            .await;
    }

    async fn activity(&self, event_type: &str, summary: String, metadata: Value) {
        let _ = self
            .task_store
            .append_activity(&ActivityRow {
                id: uuid::Uuid::new_v4().to_string(),
                event_type: event_type.into(),
                agent_id: "autopilot".into(),
                task_id: None,
                summary,
                timestamp: chrono::Utc::now().to_rfc3339(),
                metadata: Some(metadata.to_string()),
            })
            .await;
    }

    /// Trigger-time Admin notice for a gated fire that has no settled ledger
    /// row (no approval service, a payload that cannot be filed, a ledger
    /// write error): an Activity Feed row, a dashboard live event and a
    /// detached chat push — at most one per rule per hour, so a looping rule
    /// cannot flood the Admins. Settled dispatches announce through the
    /// durable outbox instead ([`Self::announce_row`]).
    pub(super) async fn approval_notice(&self, rule_id: &str, rule_name: &str, result: &str, detail: &str) {
        if !gate::always_noticed(result)
            && !gate::notice_allowed(&self.approval_notices, rule_id, Instant::now())
        {
            debug!(rule_id, result, "autopilot: approval notice suppressed (one per rule per hour)");
            return;
        }
        let body = gate::notice_body(rule_name, result, detail);
        let meta = serde_json::json!({ "rule_id": rule_id, "result": result });
        self.activity(gate::ACTIVITY_BLOCKED, body.clone(), meta.clone()).await;
        crate::dashboard_feedback::emit_live(&self.home_dir, "autopilot.approval_blocked", meta);
        let home = self.home_dir.clone();
        let rule_id = rule_id.to_string();
        tokio::spawn(async move {
            let _ = gate::push_admin_notice(&home, &rule_id, &body).await;
        });
    }
}

/// Record the result of one outcome-push attempt: sent / durably queued /
/// nobody reachable ends it; a failure stays pending with the next attempt
/// backed off, or gives up after [`gate::MAX_NOTICE_PUSH_ATTEMPTS`]. If this
/// write fails, the attempt's lease runs out and the sweep tries again.
async fn record_notice_push(store: &AutopilotStore, dispatch_id: &str, attempt: i64, result: NoticePush) {
    let recorded = match result {
        NoticePush::Sent => store.finish_dispatch_notice_push(dispatch_id, PUSH_SENT).await,
        NoticePush::Queued => store.finish_dispatch_notice_push(dispatch_id, PUSH_QUEUED).await,
        NoticePush::NoTarget => store.finish_dispatch_notice_push(dispatch_id, PUSH_NO_TARGET).await,
        NoticePush::Failed if attempt >= gate::MAX_NOTICE_PUSH_ATTEMPTS => {
            error!(dispatch_id, attempt,
                "autopilot: outcome chat push failed in every attempt — giving up (the Activity Feed entry remains)");
            store.finish_dispatch_notice_push(dispatch_id, PUSH_GAVE_UP).await
        }
        NoticePush::Failed => {
            let next = (chrono::Utc::now() + gate::notice_push_backoff(attempt)).to_rfc3339();
            warn!(dispatch_id, attempt, next_attempt = %next, "autopilot: outcome chat push failed; it is retried");
            store.defer_dispatch_notice_push(dispatch_id, &next).await
        }
    };
    if let Err(e) = recorded {
        warn!(dispatch_id, error = %e,
            "autopilot: could not record an outcome push result; the attempt is retried after its lease");
    }
}

fn json_meta(rule_id: &str, dispatch_id: &str, approval_id: Option<&str>) -> Value {
    serde_json::json!({ "rule_id": rule_id, "dispatch_id": dispatch_id, "approval_id": approval_id })
}

/// Whether `rec` was decided within the execution window of `now`:
/// `0 <= now - decided_at < gate::APPROVED_EXECUTION_WINDOW_SECS`. A
/// missing, unparseable or future decision time fails closed. (Test helper;
/// the executor uses [`decision_window_check`].)
#[cfg(test)]
pub(super) fn decided_within_window(rec: &ApprovalRecord, now: chrono::DateTime<chrono::Utc>) -> bool {
    decision_window_check(rec, now).is_ok()
}

/// [`decided_within_window`] with the reason for the refusal (history text).
pub(super) fn decision_window_check(
    rec: &ApprovalRecord,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), &'static str> {
    let Some(at) = rec
        .decided_at
        .as_deref()
        .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
    else {
        return Err("核准時間無法辨識，已核准的動作沒有執行");
    };
    let age = now - at.with_timezone(&chrono::Utc);
    if age < chrono::Duration::zero() {
        return Err("核准時間晚於現在（時鐘異常或紀錄遭改動），已核准的動作沒有執行");
    }
    if age.num_seconds() >= gate::APPROVED_EXECUTION_WINDOW_SECS {
        return Err("核准後超過 15 分鐘才處理，已核准的動作沒有執行");
    }
    Ok(())
}
