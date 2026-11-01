//! Operator-only autonomy controls. This is deliberately a WS surface, not MCP.
//! Mutation replies and pushes contain only the selected scope; returning the
//! whole store here would leak other employees' pause reasons to a Manager.
use super::*;
use duduclaw_core::autonomy_gate::{self, ChangeMethod, ChangeReceipt, MutationOutcome, PauseEntry, PauseState};

fn target(params: &Value) -> Result<Option<&str>, &'static str> {
    let Some(object) = params.as_object() else {
        return Err("expected an object");
    };
    match (object.get("agent_id"), object.get("all")) {
        (Some(Value::String(id)), None) if duduclaw_core::is_valid_agent_id(id) => Ok(Some(id)),
        (None, Some(Value::Bool(true))) => Ok(None),
        _ => Err("exactly one of agent_id or all: true is required"),
    }
}

fn visible_state(mut state: PauseState, ctx: &UserContext) -> PauseState {
    state
        .agents
        .retain(|id, _| ctx.has_agent_access(id, AccessLevel::Viewer));
    state
}

fn authorize(home: &Path, ctx: &UserContext, agent: Option<&str>) -> Result<UserContext, String> {
    match agent {
        Some(id) => {
            let live = crate::agent_files::flow::require_live_agent_access(
                home,
                ctx,
                id,
                AccessLevel::Operator,
            )?;
            acl::require_role(&live, UserRole::Manager)?;
            Ok(live)
        }
        None => {
            let live = crate::review_evidence::audience::fresh_dashboard_context(home, ctx)?;
            acl::require_role(&live, UserRole::Admin)?;
            Ok(live)
        }
    }
}

impl MethodHandler {
    pub(crate) async fn handle_autonomy_rpc(
        &self,
        method: &str,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        if method == "autonomy.status" {
            let live = match crate::review_evidence::audience::fresh_dashboard_context(
                &self.home_dir,
                ctx,
            ) {
                Ok(live) => live,
                Err(_) => return WsFrame::error_response("", "permission denied"),
            };
            return WsFrame::ok_response(
                "",
                json!(visible_state(autonomy_gate::status(&self.home_dir), &live)),
            );
        }
        let agent = match target(&params) {
            Ok(agent) => agent,
            Err(e) => return WsFrame::error_response("", e),
        };
        let live = match authorize(&self.home_dir, ctx, agent) {
            Ok(live) => live,
            Err(_) => return WsFrame::error_response("", "permission denied"),
        };
        let reason = match params.get("reason") {
            None => "",
            Some(Value::String(reason)) => reason,
            _ => return WsFrame::error_response("", "reason must be a string"),
        };
        let entry = match method {
            "autonomy.pause" => Some(PauseEntry {
                reason: duduclaw_core::truncate_chars(reason, autonomy_gate::MAX_REASON_CHARS).to_string(),
                who: live.user_id.clone(),
                at: chrono::Utc::now().to_rfc3339(),
            }),
            "autonomy.resume" => None,
            _ => return WsFrame::error_response("", "unknown autonomy method"),
        };
        // Own every input before spawning. Dropping the connection's JoinHandle
        // only detaches this task; it cannot cancel an applied change's receipt.
        let completion = start_change(
            self.home_dir.clone(), self.event_tx.read().await.clone(),
            method.to_owned(), agent.map(str::to_owned), live.user_id, entry,
        );
        match completion.await {
            Ok(frame) => frame,
            Err(error) => {
                tracing::error!(%error, "autonomy completion task failed; reconcile on restart");
                WsFrame::error_response("", "autonomy outcome unknown; refresh status before retrying")
            }
        }
    }

    /// Called by the lock-holding gateway before serving dashboard requests.
    pub(crate) async fn reconcile_autonomy_changes(&self) {
        let home = self.home_dir.clone();
        let event_tx = self.event_tx.read().await.clone();
        match tokio::task::spawn_blocking(move || reconcile_changes(&home, event_tx.as_ref())).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::error!(%error, "autonomy completion reconciliation failed"),
            Err(error) => tracing::error!(%error, "autonomy completion reconciliation task failed"),
        }
    }

}

fn finish_change(
    home: &Path,
    event_tx: Option<&tokio::sync::broadcast::Sender<String>>,
    receipt: &ChangeReceipt,
    result: MutationOutcome,
) -> WsFrame {
    let agent = receipt.agent_id();
    let change_id = &receipt.change_id;
    let (state, durability_error) = match result {
        MutationOutcome::Applied(state) => (state, None),
        MutationOutcome::AppliedButDurabilityUnknown(state, error) => {
            tracing::error!(%error, %change_id, "autonomy state applied; durability unknown");
            (state, Some("applied_durability_unknown"))
        }
        MutationOutcome::NotApplied(error) => {
            return WsFrame::error_response("", &format!("autonomy state unchanged: {error}"));
        }
    };
    let entry = match agent {
        Some(id) => state.agents.get(id),
        None => state.global.as_ref(),
    };
    let mut payload = json!({
        "agent_id": agent,
        "all": agent.is_none(),
        "paused": entry.is_some(),
        "pause": entry,
        "applied": true,
    });
    if let Some(warning) = durability_error {
        payload["durability_warning"] = json!(warning);
    }
    if let Err(error) = record_completion(home, event_tx, receipt, false, durability_error) {
        tracing::error!(%error, %change_id, "autonomy state applied; completion pending");
        payload["audit_error"] = json!("completion unavailable; state applied; retry on startup");
    }
    WsFrame::ok_response("", payload)
}

/// The task owns the full write/receipt/push transaction across RPC cancellation.
fn start_change(
    home: PathBuf,
    event_tx: Option<tokio::sync::broadcast::Sender<String>>,
    method: String,
    agent: Option<String>,
    actor: String,
    entry: Option<PauseEntry>,
) -> tokio::task::JoinHandle<WsFrame> {
    tokio::spawn(async move {
        let receipt = match ChangeReceipt::new(agent.as_deref(), entry.as_ref(), &actor) {
            Ok(receipt) => receipt,
            Err(error) => return WsFrame::error_response("", &format!("autonomy state unchanged: {error}")),
        };
        let requested = duduclaw_security::audit::AuditEvent::new(
            "autonomy_change_requested", agent.as_deref().unwrap_or(""),
            duduclaw_security::audit::Severity::Info,
            json!({"change_id": receipt.change_id, "request_digest": receipt.request_digest,
                "actor": actor, "method": method, "agent_id": agent,
                "all": agent.is_none(), "entry": entry}),
        );
        let prep_home = home.clone();
        let prepared = tokio::task::spawn_blocking(move || {
            duduclaw_security::audit::try_append_audit_event(&prep_home, &requested)
        }).await;
        if !matches!(prepared, Ok(Ok(()))) {
            tracing::error!(?prepared, "autonomy audit unavailable; state unchanged");
            return WsFrame::error_response("", "autonomy audit unavailable; state unchanged");
        }
        let result = autonomy_gate::mutate_tracked_async(&home, entry, receipt.clone()).await;
        match tokio::task::spawn_blocking(move || {
            // Read the committed receipt: applied_at is assigned under the writer lock.
            let committed = autonomy_gate::change_receipts(&home)
                .ok().and_then(|rows| rows.into_iter().find(|r| r.change_id == receipt.change_id));
            match (result, committed) {
                (MutationOutcome::NotApplied(error), _) => {
                    WsFrame::error_response("", &format!("autonomy state unchanged: {error}"))
                }
                (result, Some(committed)) => finish_change(&home, event_tx.as_ref(), &committed, result),
                (result, None) => {
                    tracing::error!(change_id = %receipt.change_id, "applied autonomy receipt unreadable; retry on startup");
                    // Preserve the actual mutation outcome and selected-scope
                    // response even when the later receipt read is unavailable.
                    let (state, warning) = match result {
                        MutationOutcome::Applied(state) => (state, None),
                        MutationOutcome::AppliedButDurabilityUnknown(state, _) =>
                            (state, Some("applied_durability_unknown")),
                        MutationOutcome::NotApplied(error) => return WsFrame::error_response(
                            "", &format!("autonomy state unchanged: {error}"),
                        ),
                    };
                    let pause = match agent.as_deref() {
                        Some(id) => state.agents.get(id),
                        None => state.global.as_ref(),
                    };
                    let mut payload = json!({"agent_id": agent, "all": agent.is_none(),
                        "paused": pause.is_some(), "pause": pause, "applied": true,
                        "audit_error": "completion unavailable; state applied; refresh status"});
                    if let Some(warning) = warning { payload["durability_warning"] = json!(warning); }
                    WsFrame::ok_response("", payload)
                }
            }
        }).await {
            Ok(frame) => frame,
            Err(error) => {
                tracing::error!(%error, "autonomy receipt task failed; reconcile on restart");
                WsFrame::error_response("", "autonomy outcome unknown; refresh status before retrying")
            }
        }
    })
}

/// The receipt is the only authority for historical completion fields. Never
/// rebuild a mutation result from an audit row or from the current pause state.
fn record_completion(
    home: &Path,
    event_tx: Option<&tokio::sync::broadcast::Sender<String>>,
    receipt: &ChangeReceipt,
    reconciled: bool,
    durability_warning: Option<&str>,
) -> Result<(), String> {
    let agent = receipt.agent_id();
    duduclaw_security::audit::try_append_audit_event(
        home,
        &duduclaw_security::audit::AuditEvent::new(
            if receipt.method == ChangeMethod::Pause { "autonomy_paused" } else { "autonomy_resumed" },
            agent.unwrap_or(""),
            duduclaw_security::audit::Severity::Info,
            json!({"change_id": receipt.change_id, "actor": receipt.actor,
                "method": receipt.method, "request_digest": receipt.request_digest,
                "applied_at": receipt.applied_at, "reconciled": reconciled,
                "scope": {"agent_id": agent, "all": agent.is_none(),
                    "paused": receipt.method == ChangeMethod::Pause,
                    "durability_warning": durability_warning}}),
        ),
    )?;
    let current = autonomy_gate::status(home);
    if current.state_error.is_some() {
        return Err("autonomy current state unreadable; completion remains pending".into());
    }
    // Push only current scope/boolean. Never expose receipt actor or digest.
    // Broadcast is best-effort (no subscribers is normal), not durable delivery.
    if let Some(tx) = event_tx {
        let paused = match agent {
            Some(id) => current.agents.contains_key(id),
            None => current.global.is_some(),
        };
        let frame = WsFrame::Event {
            event: "autonomy.changed".into(), payload: change_event(agent, paused),
            seq: None, state_version: None, boot_id: None,
        };
        let line = serde_json::to_string(&frame).map_err(|error| error.to_string())?;
        let _ = tx.send(line);
    }
    autonomy_gate::mark_completion_recorded(home, &receipt.change_id)
        .map_err(|error| error.to_string())
}

fn reconcile_changes(
    home: &Path,
    event_tx: Option<&tokio::sync::broadcast::Sender<String>>,
) -> Result<(), String> {
    let receipts = autonomy_gate::change_receipts(home).map_err(|error| error.to_string())?;
    for receipt in receipts.into_iter().filter(|r| !r.completion_recorded) {
        if let Err(error) = record_completion(home, event_tx, &receipt, true, None) {
            tracing::error!(%error, change_id = %receipt.change_id,
                "autonomy reconciliation completion pending; retry on startup");
        }
    }
    Ok(())
}

fn change_event(agent: Option<&str>, paused: bool) -> Value {
    json!({
        "agent_id": agent,
        "all": agent.is_none(),
        "paused": paused,
        "at": chrono::Utc::now().to_rfc3339(),
    })
}

#[cfg(test)]
mod tests;
