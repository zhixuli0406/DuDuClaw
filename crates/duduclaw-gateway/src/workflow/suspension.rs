//! Stopping an activation without an Admin's revoke: suspension on authority
//! drift (F1b E-H5b), suspension after repeated limit blocks (F5-A R-M3) and
//! expiry at the end of its validity (F5-A U8).
//!
//! An activation moves `active → suspended` or `active → expired` exactly
//! once, by compare-and-set on the projection. In the same step the grant in
//! the approval ledger is revoked (R-H1), so neither a later
//! `commit_activation` nor a prepared operation can find live authority
//! behind it: nothing resumes it, and an activation that left `active` this
//! way can only end (`revoking`/`revoked`). The routine is switched off, an
//! Activity Feed row and an Admin notice say why, and `workflow_runs.get` /
//! `workflow_drafts.get` show the reason. Continuing needs a new request and
//! a new Admin approval (a new draft revision when the employee's authority
//! changed, since the accepted revision pins it).
use super::{ActivationRecord, ActivationState, WorkflowStore, workflow_notify};
use crate::approval::ApprovalBroker;
use crate::approval::policy_snapshot::{POLICY_CATEGORIES, changed_categories, policy_digests};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Why an activation stopped accepting runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivationSuspension {
    pub reason: String,
    /// Members of `approval::policy_snapshot::POLICY_CATEGORIES` for a policy
    /// change; empty for a limit suspension.
    pub changed_categories: Vec<String>,
    pub suspended_at: String,
}

pub const SUSPENDED_ERROR: &str = "workflow_activation_suspended";
pub const EXPIRED_ERROR: &str = "workflow_activation_expired";
/// Days before expiry at which Admins are told once.
pub const EXPIRY_NOTICE_DAYS: i64 = 3;

/// The stored state of an activation, read straight from the projection.
pub async fn activation_state(
    store: &WorkflowStore,
    activation_id: &str,
) -> Result<Option<ActivationRecord>, String> {
    let id = activation_id.to_string();
    store
        .with_connection(move |c| {
            use rusqlite::OptionalExtension;
            let raw: Option<String> = c
                .query_row(
                    "SELECT record_json FROM workflow_activations WHERE activation_id=?1",
                    [&id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            raw.map(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
                .transpose()
        })
        .await
}

/// Revoke the activation's grant in the approval ledger (idempotent).
async fn revoke_ledger(
    broker: &ApprovalBroker,
    record: &ActivationRecord,
    reason: &str,
) -> Result<(), String> {
    broker
        .revoke_workflow_activation(
            &record.request.activation_id,
            &record.request.spec.hash(),
            reason,
        )
        .await
        .map(|_| ())
}

/// Move an active activation to `target` (suspended or expired). Returns the
/// stored record when this call made the transition.
async fn stop(
    store: &WorkflowStore,
    broker: &ApprovalBroker,
    home: &Path,
    activation_id: &str,
    target: ActivationState,
    reason: String,
    categories: Vec<String>,
) -> Result<Option<ActivationRecord>, String> {
    let Some(mut record) = activation_state(store, activation_id).await? else {
        return Ok(None);
    };
    if record.state != ActivationState::Active {
        return Ok(None);
    }
    record.state = target;
    record.error_code = Some(reason.clone());
    record.suspension = Some(ActivationSuspension {
        reason: reason.clone(),
        changed_categories: categories,
        suspended_at: chrono::Utc::now().to_rfc3339(),
    });
    let state = serde_json::to_value(target).map_err(|e| e.to_string())?;
    let state = state.as_str().unwrap_or_default().to_string();
    let json = serde_json::to_string(&record).map_err(|e| e.to_string())?;
    let id = activation_id.to_string();
    let moved = store
        .with_transaction(move |tx| {
            tx.execute(
                "UPDATE workflow_activations SET state=?1,record_json=?2
                    WHERE activation_id=?3 AND state='active'",
                rusqlite::params![state, json, id],
            )
            .map_err(|e| e.to_string())
        })
        .await?;
    if moved != 1 {
        return Ok(None);
    }
    // R-H1: the ledger must not keep live authority behind a stopped
    // activation. A failure here is retried by the sweep (`settle_stopped`).
    if let Err(e) = revoke_ledger(broker, &record, &reason).await {
        tracing::warn!(error = %e, activation_id, "stopped activation's ledger grant not yet revoked");
    }
    if let Some(cron) = &record.request.cron {
        crate::cron_store::CronStore::open(home)?
            .set_enabled(&cron.cron_id, false)
            .await?;
    }
    Ok(Some(record))
}

/// Suspend an active activation because the employee's authority changed.
/// Returns the changed categories when this call made the transition.
pub async fn suspend_for_policy(
    store: &WorkflowStore,
    broker: &ApprovalBroker,
    home: &Path,
    activation_id: &str,
) -> Result<Option<Vec<String>>, String> {
    let Some(current) = activation_state(store, activation_id).await? else {
        return Ok(None);
    };
    // An unreadable snapshot or a record without stored digests cannot say
    // what changed; it names every category rather than none.
    let changed = match (
        &current.policy_digests,
        policy_digests(home, &current.request.spec.actor),
    ) {
        (Some(before), Ok(after)) => changed_categories(before, &after),
        _ => POLICY_CATEGORIES.iter().map(|c| c.to_string()).collect(),
    };
    let reason = format!("policy_changed:{}", changed.join(","));
    let Some(record) = stop(
        store,
        broker,
        home,
        activation_id,
        ActivationState::Suspended,
        reason,
        changed.clone(),
    )
    .await?
    else {
        return Ok(None);
    };
    let labels: Vec<&str> = changed.iter().map(|c| category_label(c)).collect();
    announce(
        home,
        &record,
        "workflow_activation_suspended",
        &format!(
            "工作流已暫停：AI 員工的權限設定有變動（{}），需要管理員重新核准新版本才能繼續",
            labels.join("、")
        ),
        serde_json::json!({ "changed_categories": changed }),
    )
    .await;
    Ok(Some(changed))
}

/// Suspend an active activation whose runs keep ending on a budget or count
/// limit (R-M3): repeating the routine would only repeat the same partial
/// work.
pub async fn suspend_for_limit(
    store: &WorkflowStore,
    broker: &ApprovalBroker,
    home: &Path,
    activation_id: &str,
    code: &str,
) -> Result<bool, String> {
    let Some(record) = stop(
        store,
        broker,
        home,
        activation_id,
        ActivationState::Suspended,
        format!("limit:{code}"),
        Vec::new(),
    )
    .await?
    else {
        return Ok(false);
    };
    announce(
        home,
        &record,
        "workflow_activation_suspended",
        "工作流已暫停：連續幾次執行都因次數或費用上限被擋下，需要調整上限或流程後重新送審",
        serde_json::json!({ "limit": code }),
    )
    .await;
    Ok(true)
}

/// What the sweep does with one active activation (U8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifetimeAction {
    Nothing,
    /// Tell Admins once that it ends within `EXPIRY_NOTICE_DAYS`.
    Notice,
    Expire,
}

/// Pure decision behind `settle_lifetimes`. An unparseable expiry expires
/// (fail closed).
pub fn lifetime_action(
    expires_at: &str,
    notice_sent: bool,
    now: chrono::DateTime<chrono::Utc>,
) -> LifetimeAction {
    let Ok(until) = chrono::DateTime::parse_from_rfc3339(expires_at) else {
        return LifetimeAction::Expire;
    };
    let until = until.with_timezone(&chrono::Utc);
    if until <= now {
        LifetimeAction::Expire
    } else if !notice_sent && until - now <= chrono::Duration::days(EXPIRY_NOTICE_DAYS) {
        LifetimeAction::Notice
    } else {
        LifetimeAction::Nothing
    }
}

/// Expire every active activation whose validity has ended, and tell Admins
/// once about each one that ends within `EXPIRY_NOTICE_DAYS` (U8). Run by
/// the resident sweep. Returns how many expired.
pub async fn settle_lifetimes(
    store: &WorkflowStore,
    broker: &ApprovalBroker,
    home: &Path,
) -> Result<usize, String> {
    let now = chrono::Utc::now();
    let active: Vec<(String, String)> = store
        .with_connection(|c| {
            let mut q = c
                .prepare("SELECT activation_id,record_json FROM workflow_activations WHERE state='active'")
                .map_err(|e| e.to_string())?;
            let rows = q
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(|e| e.to_string())?
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?;
            Ok(rows)
        })
        .await?;
    let mut expired = 0;
    for (id, raw) in active {
        let Ok(record) = serde_json::from_str::<ActivationRecord>(&raw) else {
            continue;
        };
        let action = lifetime_action(
            &record.request.spec.expires_at,
            record.expiry_notice_at.is_some(),
            now,
        );
        if action == LifetimeAction::Expire {
            if let Some(record) = stop(
                store,
                broker,
                home,
                &id,
                ActivationState::Expired,
                EXPIRED_ERROR.into(),
                Vec::new(),
            )
            .await?
            {
                expired += 1;
                announce(
                    home,
                    &record,
                    "workflow_activation_expired",
                    "工作流的啟用已到期，排程已關閉；要繼續請重新送審並由管理員核准",
                    serde_json::json!({ "expired_at": record.request.spec.expires_at }),
                )
                .await;
            }
        } else if action == LifetimeAction::Notice {
            let mut noted = record.clone();
            noted.expiry_notice_at = Some(now.to_rfc3339());
            let json = serde_json::to_string(&noted).map_err(|e| e.to_string())?;
            let key = id.clone();
            let marked = store
                .with_transaction(move |tx| {
                    tx.execute(
                        "UPDATE workflow_activations SET record_json=?1 WHERE activation_id=?2
                            AND state='active' AND json_extract(record_json,'$.expiry_notice_at') IS NULL",
                        rusqlite::params![json, key],
                    )
                    .map_err(|e| e.to_string())
                })
                .await?;
            if marked == 1 {
                announce(
                    home,
                    &noted,
                    "workflow_activation_expiring",
                    &format!(
                        "工作流的啟用將於 {} 到期；到期後排程會關閉，要繼續請重新送審",
                        workflow_notify::readable_time(&record.request.spec.expires_at)
                    ),
                    serde_json::json!({ "expires_at": record.request.spec.expires_at }),
                )
                .await;
            }
        }
    }
    // Stopped activations whose ledger grant could not be revoked at the
    // moment they stopped get another try here.
    let stopped: Vec<String> = store
        .with_connection(|c| {
            let mut q = c
                .prepare("SELECT record_json FROM workflow_activations WHERE state IN ('suspended','expired')")
                .map_err(|e| e.to_string())?;
            let rows = q
                .query_map([], |r| r.get(0))
                .map_err(|e| e.to_string())?
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?;
            Ok(rows)
        })
        .await?;
    for raw in stopped {
        if let Ok(record) = serde_json::from_str::<ActivationRecord>(&raw) {
            if !broker
                .activation_revoked(&record.request.activation_id)
                .await?
            {
                let reason = record
                    .error_code
                    .clone()
                    .unwrap_or_else(|| "stopped".into());
                revoke_ledger(broker, &record, &reason).await?;
            }
        }
    }
    Ok(expired)
}

fn category_label(category: &str) -> &'static str {
    match category {
        "capabilities" => "工具與能力",
        "permissions" => "權限開關",
        "agent_authority" => "上級／部門／角色",
        "contract" => "行為契約",
        "preset" => "職務範本",
        "org_chain" => "組織關係",
        "delegation" => "委派規則",
        "acp" => "外部代理連線",
        "provenance" => "敏感工具清單",
        "integrations" => "外部整合",
        "redaction" => "去識別化設定",
        "killswitch" => "緊急停止設定",
        _ => "其他權限設定",
    }
}

/// Activity Feed row plus a plain notice to Admins' verified chats. Both are
/// best-effort: the transition itself is already committed.
async fn announce(
    home: &Path,
    record: &ActivationRecord,
    event: &str,
    summary: &str,
    extra: serde_json::Value,
) {
    let agent = record.request.spec.actor.clone();
    let mut metadata = serde_json::json!({
        "activation_id": record.request.activation_id,
        "workflow_id": record.request.spec.workflow_id,
    });
    if let (Some(m), Some(e)) = (metadata.as_object_mut(), extra.as_object()) {
        m.extend(e.clone());
    }
    workflow_notify::activity(home, event, &agent, summary, metadata).await;
    let text = format!(
        "⏸ {summary}\nAI 員工：{}\n請開啟儀表板查看。",
        duduclaw_core::truncate_chars(&agent, 64)
    );
    workflow_notify::send(home, &agent, workflow_notify::admin_links(home), &text).await;
}
