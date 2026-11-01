//! Redacted cross-process invalidations and optimistic approval-card digests.

use super::*;
use serde_json::json;

/// Canonical digest input for `approvals.list` / `approvals.get` cards.
/// A digest is only given to a viewer who sees every hashed field.
/// Objects are recursively sorted by key, arrays retain order, and compact
/// UTF-8 JSON is hashed with SHA-256 (lowercase hexadecimal). Transport-only
/// fields (channel links, permissions and the digest itself) are excluded.
/// Bound payloads remain redacted; their immutable payload hash is included.
/// Production dashboard decisions use `decision_digest_at` so the currently
/// displayed workflow activation facts (including pricing) are also bound.
pub fn decision_digest(rec: &ApprovalRecord) -> String {
    decision_digest_with_activation(rec, &Value::Null)
}

pub fn decision_digest_at(home: &Path, rec: &ApprovalRecord) -> String {
    let facts = if is_activation_record(rec) { activation_facts(home, rec) } else { Value::Null };
    decision_digest_with_activation(rec, &facts)
}

/// List/get passes the same computed facts to this hash and to the response:
/// a pricing change between two file reads cannot create an inconsistent card.
/// Only viewers who see every hashed field receive this digest. Unbound cards
/// hash the full payload, so a viewer receiving a redacted payload gets no digest.
/// Bound cards hash the redacted payload hash and disclose binding and answer
/// only to deciders.
pub(crate) fn decision_digest_with_activation(rec: &ApprovalRecord, facts: &Value) -> String {
    payload_hash(&serde_json::json!({
        "id": rec.id.as_str(),
        "agent_id": rec.agent_id,
        "kind": crate::governance::ApprovalKind::parse(&rec.action_kind).as_str(),
        "summary": rec.summary,
        "payload": if rec.binding.is_some() { serde_json::json!({
            "redacted": true,
            "payload_hash": rec.binding.as_ref().map(|binding| &binding.payload_hash)
        }) } else { rec.payload.clone() },
        "request_kind": rec.request_kind,
        "status": rec.status,
        "binding": rec.binding,
        "answer": rec.answer,
        "invalidated_reason": rec.invalidated_reason,
        "created_at": rec.created_at,
        "ttl_seconds": rec.ttl_seconds,
        "expires_at": rec.expires_at_epoch(),
        "simulation": rec.simulation,
        "workflow_activation": facts,
        "decided_in_dashboard_only": crate::approval_notify::is_dashboard_only_record(rec),
    }))
}

tokio::task_local! {
    /// Scoped to one dashboard decision, never inherited by spawned work.
    /// The id prevents unrelated broker activity from consuming the guard.
    pub(crate) static EXPECTED_DECISION_DIGEST: Option<(String, String)>;
    /// Read fresh projection inputs again at the decision's write transaction.
    pub(crate) static EXPECTED_DECISION_HOME: PathBuf;
}

pub(crate) fn check_expected_digest(rec: &ApprovalRecord) -> Result<(), String> {
    EXPECTED_DECISION_DIGEST.try_with(|expected| {
        if let Some((id, digest)) = expected {
            let current = EXPECTED_DECISION_HOME.try_with(|home| decision_digest_at(home, rec))
                .unwrap_or_else(|_| decision_digest(rec));
            if id == rec.id.as_str() && digest != &current {
                return Err("approval digest mismatch; refetch the approval before deciding".into());
            }
        }
        Ok(())
    }).unwrap_or(Ok(()))
}

impl ApprovalStore {
    /// The attached event row shares the quarantine card's insert transaction.
    /// This removes the extra asynchronous window before the existing erase
    /// post-check; it does not fence creation of a new card after erase. WAL
    /// databases do not promise cross-database atomicity across power loss.
    pub(super) fn insert_quarantine_creation_event(
        tx: &rusqlite::Transaction<'_>,
        rec: &ApprovalRecord,
    ) -> Result<(), String> {
        let payload = json!({
            "id": rec.id.as_str(), "agent_id": rec.agent_id,
            "action_kind": rec.action_kind, "status": rec.status,
        }).to_string();
        tx.execute(
            "INSERT INTO quarantine_live_events.events (event, payload, ts, source)
             SELECT 'approval.created', ?1, ?2, NULL
             WHERE NOT EXISTS (
                 SELECT 1 FROM quarantine_live_events.scrubbed_approval_events
                 WHERE approval_id = ?3
             )",
            params![payload, Utc::now().to_rfc3339(), rec.id.as_str()],
        ).map_err(|e| format!("insert quarantine creation event: {e}"))?;
        Ok(())
    }

    /// Called by the broker's persistence paths, after the approval commit.
    /// This deliberately works in employee-side mcp-server processes too.
    /// Invalidation delivery is best effort; approval persistence remains the
    /// authority, and clients retain a slow reconciliation poll.
    pub(super) async fn emit_lifecycle(&self, event: &str, rec: &ApprovalRecord, status: ApprovalStatus) {
        let Some(home) = self.db_path.as_ref().and_then(|p| p.parent()) else { return };
        let bus = match crate::events_store::EventBusStore::open(home) {
            Ok(bus) => bus,
            Err(error) => {
                warn!(%error, event, "approval live event store unavailable");
                return;
            }
        };
        // Never include summary, arguments, bindings, answers or file content.
        // The push gate re-reads the approval to authorize kind and audience.
        let payload = serde_json::json!({
            "id": rec.id.as_str(),
            "agent_id": rec.agent_id,
            "action_kind": rec.action_kind,
            "status": status,
        });
        if let Err(error) = bus.append(event, &payload.to_string()).await {
            warn!(%error, event, "approval live event append failed");
        }
    }
}

/// A card whose decision accepts a workflow version.
pub(crate) fn is_activation_record(rec: &ApprovalRecord) -> bool {
    rec.action_kind == WORKFLOW_ACTIVATION_KIND
        || rec.payload.get("kind").and_then(Value::as_str) == Some(WORKFLOW_ACTIVATION_KIND)
}


/// What the activation card shows: the workflow, its schedule and each
/// effect's pinned target. `Null` when the stored request cannot be read.
pub(crate) fn activation_facts(home: &std::path::Path, rec: &ApprovalRecord) -> Value {
    let Some(request) = rec
        .payload
        .get("request")
        .cloned()
        .and_then(|v| serde_json::from_value::<crate::workflow::ActivationRequest>(v).ok())
    else {
        return Value::Null;
    };
    json!({
        "workflow_id": request.spec.workflow_id,
        "revision": request.spec.workflow_revision,
        "actor": request.spec.actor,
        "schedule": request.cron.as_ref().map(|c| json!({"expression": c.expression, "timezone": c.timezone})),
        "budget": request.spec.budget,
        // U8: how long the activation stays valid once approved.
        "expires_at": request.spec.expires_at,
        // R-M4: whether the money budgets bind, and the counts that always do.
        "pricing": crate::workflow::cost_ledger::load_pricing(home).view(),
        "effect_targets": crate::workflow::effect_targets::effect_targets(request.spec.templates.values()),
        "submitted_by": rec.binding.as_ref().map(|b| b.decision_context.principal_id.clone()),
    })
}

