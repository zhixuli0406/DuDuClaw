//! `duduclaw ops channel-ingress batch` — one approval for many events
//! (review N5).
//!
//! An incident can leave hundreds of events waiting, and there is no inbox
//! page in the dashboard yet. The batch command selects the events in one
//! state (optionally with one reason) that the action applies to, at most
//! [`MAX_BATCH`], and files one dashboard-only approval of the same kind as a
//! single request. The approval binds the batch: the action, the selection,
//! the note, and every selected event's id with its state version. After an
//! Admin approves it, running the same command again applies it once: each
//! event whose state is unchanged is resolved, each one that changed in
//! between is skipped, and the result lists both. If the selection changed
//! before the decision, the waiting card is withdrawn and a new one filed.

use std::path::Path;

use rusqlite::params;
use serde_json::{Value, json};

use super::cli_approval::{
    self, CliAction, CliOutcome, CliRequest, Gate, GateSpec, UNVERIFIED_ACTOR, applicable, audit,
    short, state_version,
};
use super::{IngressRow, IngressStore, ROW_SELECT, ResolveRequest, digest, row_from_sql};
use crate::approval::{ApprovalBroker, ApprovalStatus};

/// Default and maximum events in one batch.
pub const DEFAULT_BATCH: usize = 200;
pub const MAX_BATCH: usize = 500;

/// One terminal batch request.
#[derive(Debug, Clone)]
pub struct BatchRequest {
    pub action: CliAction,
    pub status: String,
    pub reason: Option<String>,
    pub note: String,
    pub confirm_duplicate_risk: bool,
    pub limit: usize,
}

const STATUSES: [&str; 4] = [
    "uncertain",
    "quarantined",
    "undelivered",
    "failed_before_dispatch",
];

impl IngressStore {
    async fn rows_in_state(
        &self,
        status: &str,
        reason: Option<&str>,
        limit: usize,
    ) -> Result<Vec<IngressRow>, String> {
        let conn = self.connection().lock().await;
        let mut stmt = conn
            .prepare(&format!(
                "{ROW_SELECT} WHERE i.status=?1 AND (?2 IS NULL OR i.reason=?2) ORDER BY i.seq LIMIT ?3"
            ))
            .map_err(|e| e.to_string())?;
        stmt.query_map(params![status, reason, limit as i64], row_from_sql)
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }
}

fn as_single(req: &BatchRequest, id: &str) -> CliRequest {
    CliRequest {
        action: req.action,
        ingress_id: id.to_string(),
        note: req.note.clone(),
        provider_receipt: None,
        confirm_duplicate_risk: req.confirm_duplicate_risk,
    }
}

fn subject(req: &BatchRequest) -> String {
    format!(
        "batch:{}:{}",
        req.status,
        req.reason.as_deref().unwrap_or("*")
    )
}

fn batch_digest(req: &BatchRequest) -> String {
    digest(&[
        "batch",
        req.action.as_str(),
        &req.status,
        req.reason.as_deref().unwrap_or(""),
        &req.note,
        if req.confirm_duplicate_risk { "1" } else { "0" },
    ])
}

fn items_of(rows: &[IngressRow]) -> Value {
    Value::Array(
        rows.iter()
            .map(|r| json!({"ingress_id": r.id, "state_version": state_version(r)}))
            .collect(),
    )
}

fn summary(req: &BatchRequest, rows: &[IngressRow]) -> String {
    let ids = rows
        .iter()
        .take(5)
        .map(|r| short(&r.id))
        .collect::<Vec<_>>()
        .join("、");
    format!(
        "LINE 收件匣批次：{}，共 {} 則（狀態 {}，原因 {}）。前幾則：{ids}。操作理由：{}。核准後只處理狀態沒有變動的事件。{}。",
        req.action.as_str(),
        rows.len(),
        req.status,
        req.reason.as_deref().unwrap_or("不限"),
        duduclaw_core::truncate_chars(&req.note, 200),
        cli_approval::CARD_NOTE,
    )
}

fn validate(req: &BatchRequest) -> Result<(), String> {
    if !STATUSES.contains(&req.status.as_str()) {
        return Err(
            "--status 只能是 uncertain、quarantined、undelivered 或 failed_before_dispatch".into(),
        );
    }
    if req.reason.as_deref().is_some_and(|r| {
        r.is_empty() || r.len() > 64 || !r.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    }) {
        return Err("--reason 是原因代碼（英數與底線，64 字元內）".into());
    }
    if req.note.trim().is_empty() || req.note.len() > super::resolve::NOTE_MAX_BYTES {
        return Err("請用 --note 寫明理由（2000 位元組以內）".into());
    }
    if req.action == CliAction::Rerun && !req.confirm_duplicate_risk {
        return Err("批次重新執行需要 --confirm-duplicate-risk".into());
    }
    if req.limit == 0 || req.limit > MAX_BATCH {
        return Err(format!("--limit 介於 1 到 {MAX_BATCH}"));
    }
    Ok(())
}

/// File the batch approval, or apply an approved one.
pub async fn batch_request_or_apply(home: &Path, req: &BatchRequest) -> Result<CliOutcome, String> {
    let act = req.action.as_str();
    let refuse = |reason: &str, msg: String| {
        audit(home, "refused", act, "batch", None, reason);
        msg
    };
    validate(req).map_err(|m| refuse("invalid_batch", m))?;
    let store = IngressStore::open_current(home).map_err(|e| refuse("store_unavailable", e))?;
    let late = super::config::IngressConfig::load(home).await.late_reply;
    let selected: Vec<IngressRow> = store
        .rows_in_state(&req.status, req.reason.as_deref(), MAX_BATCH)
        .await
        .map_err(|e| refuse("store_unavailable", e))?
        .into_iter()
        .filter(|r| applicable(&as_single(req, &r.id), r, late).is_ok())
        .take(req.limit)
        .collect();
    let broker = ApprovalBroker::open(home).map_err(|e| refuse("approvals_unavailable", e))?;
    let (subject, wanted, items) = (subject(req), batch_digest(req), items_of(&selected));
    let same_request = |rec: &crate::approval::ApprovalRecord| {
        crate::approval::operator_cli_gate::binding_of(rec).is_some_and(|b| {
            b.action == act && b.target == subject && b.request_digest == wanted
        })
    };
    let spec = GateSpec {
        action: act.to_string(),
        target: subject.clone(),
        request_digest: wanted.clone(),
        state: digest(&["items", &items.to_string()]),
        approval_needs_same_state: false,
        summary: summary(req, &selected),
        payload: json!({
            "batch": true,
            "action": act,
            "subject": subject,
            "status": req.status,
            "reason": req.reason,
            "request_digest": wanted,
            "note": req.note,
            "confirm_duplicate_risk": req.confirm_duplicate_risk,
            "count": selected.len(),
            "items": items,
            "requested_by": UNVERIFIED_ACTOR,
        }),
    };
    // Nothing to ask for, unless an approval for this batch is waiting.
    if selected.is_empty() {
        let approved = broker
            .list_by_kind(cli_approval::ACTION_KIND)
            .await
            .map_err(|e| refuse("approvals_unavailable", e))?
            .iter()
            .any(|r| r.status == ApprovalStatus::Approved && same_request(r));
        if !approved {
            return Err(refuse(
                "nothing_selected",
                "沒有符合條件、可以這樣處理的事件。".into(),
            ));
        }
    }
    match cli_approval::gate(&broker, spec)
        .await
        .map_err(|e| refuse("approvals_unavailable", e))?
    {
        Gate::Requested(id) => {
            audit(
                home,
                "requested",
                act,
                "batch",
                Some(id.as_str()),
                "awaiting_dashboard_approval",
            );
            Ok(CliOutcome::Requested(id.as_str().to_string()))
        }
        Gate::Pending(id) => Ok(CliOutcome::Pending(id.as_str().to_string())),
        Gate::Proceed(id, decided_by, approved) => {
            let report = apply(&store, req, &approved, &decided_by, late).await;
            audit(home, "applied", act, "batch", Some(id.as_str()), "applied");
            Ok(CliOutcome::Applied(json!({
                "action": act,
                "approval_id": id.as_str(),
                "applied": report.0,
                "skipped_changed": report.1,
                "failed": report.2,
            })))
        }
    }
}

/// Apply an approved batch to the events whose state is unchanged.
async fn apply(
    store: &IngressStore,
    req: &BatchRequest,
    approved: &Value,
    decided_by: &str,
    late: super::config::LateReply,
) -> (Vec<String>, Vec<String>, Vec<Value>) {
    let actor = format!("{UNVERIFIED_ACTOR}；核准者 {decided_by}");
    let (mut applied, mut skipped, mut failed) = (Vec::new(), Vec::new(), Vec::new());
    let items = approved["items"].as_array().cloned().unwrap_or_default();
    for item in items {
        let (Some(id), Some(version)) =
            (item["ingress_id"].as_str(), item["state_version"].as_str())
        else {
            continue;
        };
        let row = match store.get(id).await {
            Ok(Some(row)) if state_version(&row) == version => row,
            Ok(_) => {
                skipped.push(short(id));
                continue;
            }
            Err(e) => {
                failed.push(json!({"ingress_id": short(id), "error": duduclaw_core::truncate_chars(&e, 120)}));
                continue;
            }
        };
        if applicable(&as_single(req, id), &row, late).is_err() {
            skipped.push(short(id));
            continue;
        }
        let result = store
            .resolve_request(&ResolveRequest {
                id,
                expected_revision: &row.revision,
                expected_attempt: row.attempt,
                action: req.action.as_str(),
                confirm_duplicate_risk: req.confirm_duplicate_risk,
                actor: &actor,
                note: &req.note,
                provider_receipt: None,
                now: chrono::Utc::now().timestamp(),
                late_reply: late,
            })
            .await;
        match result {
            Ok(()) => applied.push(short(id)),
            Err(e) => failed.push(
                json!({"ingress_id": short(id), "error": duduclaw_core::truncate_chars(&e, 120)}),
            ),
        }
    }
    (applied, skipped, failed)
}

#[cfg(test)]
#[path = "cli_batch_tests.rs"]
mod tests;
