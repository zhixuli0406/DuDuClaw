//! The LINE ingress workers.
//!
//! - `line_workers` ordinary workers (default 8) and one decision worker
//!   claim events; one conversation is still processed in order.
//! - One maintenance task and one snapshot lane per home (see
//!   [`super::upkeep`]); an event becomes claimable only once its
//!   route/authority snapshot is stored (review N1), so the acknowledgement
//!   never depends on employee configuration being readable.

use super::super::*;
use super::delivery::{
    ProgressLedger, RunReceipt, final_status, is_redelivery, reply_token_deadline,
};
use super::revision::{RevisionError, line_revision};
use super::upkeep::{maintenance_loop, snapshot_loop};
use super::{Binding, INGRESS_BINDING, PROGRESS, RUN};
use crate::channel_ingress::alerts::{self, AlertKind};
use crate::channel_ingress::config::{IngressConfig, LateReply};
use crate::channel_ingress::{AttemptReceipt, Deferred, IngressRow, IngressStore, LEASE_SECONDS};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IngressLane {
    Normal,
    Decision,
    Maintenance,
    Snapshot,
}

pub(crate) async fn drain_line_ingress(state: LineState) {
    if state.ingress.is_none() {
        return;
    }
    let workers = IngressConfig::load(&state.home_dir).await.line_workers;
    let mut set = tokio::task::JoinSet::new();
    let mut lanes = std::collections::HashMap::new();
    let spawn = |set: &mut tokio::task::JoinSet<()>, lane: IngressLane| {
        let st = state.clone();
        match lane {
            IngressLane::Maintenance => set.spawn(maintenance_loop(st)),
            IngressLane::Snapshot => set.spawn(snapshot_loop(st)),
            _ => set.spawn(drain_line_worker(st, lane)),
        }
    };
    let mut plan = vec![
        IngressLane::Maintenance,
        IngressLane::Snapshot,
        IngressLane::Decision,
    ];
    plan.extend(std::iter::repeat_n(IngressLane::Normal, workers));
    for lane in plan {
        let handle = spawn(&mut set, lane);
        lanes.insert(handle.id(), lane);
    }
    while let Some(result) = set.join_next_with_id().await {
        let id = match result {
            Ok((id, ())) => id,
            Err(error) => error.id(),
        };
        let lane = lanes.remove(&id).expect("bounded worker lane registered");
        error!(
            ?lane,
            "LINE ingress worker stopped; restarting bounded worker"
        );
        let handle = spawn(&mut set, lane);
        lanes.insert(handle.id(), lane);
    }
}

pub(crate) async fn drain_line_worker(state: LineState, lane: IngressLane) {
    #[cfg(test)]
    if let Some(probe) = super::worker_test_probe(&state.home_dir) {
        probe
            .starts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
    let Some(store) = state.ingress.clone() else {
        return;
    };
    let min_idle = std::time::Duration::from_millis(250);
    let max_idle = std::time::Duration::from_secs(1);
    let mut idle = min_idle;
    loop {
        // Registered before the claim, so an append in between still wakes us.
        let woken = store.work_signal().notified();
        tokio::pin!(woken);
        woken.as_mut().enable();
        let cfg = IngressConfig::load(&state.home_dir).await;
        let now = chrono::Utc::now().timestamp();
        let claimed = if !cfg.line_enabled {
            // The stop switch pauses dispatch; the ledger stays.
            Ok(None)
        } else {
            match lane {
                IngressLane::Decision => store.claim_decision(now).await,
                _ => store.claim(now).await,
            }
        };
        match claimed {
            Ok(Some(row)) => {
                idle = min_idle;
                process_claimed(&state, &store, row, &cfg).await;
                continue;
            }
            Ok(None) => {}
            Err(_) => error!("LINE ingress claim unavailable"),
        }
        tokio::select! {
            _ = &mut woken => {}
            _ = tokio::time::sleep(idle) => {}
        }
        idle = (idle * 2).min(max_idle);
    }
}

async fn alert(store: &IngressStore, kind: AlertKind, row: &IngressRow, reason: &str) {
    alerts::record(store, kind, &row.id, reason).await;
}

async fn quarantine(
    _state: &LineState,
    store: &IngressStore,
    row: &IngressRow,
    reason: &str,
    cause: &str,
) {
    if store
        .transition(row, "claimed", "quarantined", Some(reason))
        .await
        == Ok(true)
    {
        alert(store, AlertKind::Quarantined, row, cause).await;
    }
}

/// The snapshot this run is admitted under (route, authority, resolved
/// employee), or `None` when the row was deferred / quarantined / lost.
/// Only rows with a stored snapshot are claimable (review N1).
async fn admit(
    state: &LineState,
    store: &IngressStore,
    row: &IngressRow,
    event: &LineEvent,
) -> Option<(String, String, String)> {
    match line_revision(state, event, None).await {
        Err(RevisionError::Unavailable(reason)) => {
            let now = chrono::Utc::now().timestamp();
            if store.defer_unavailable(row, reason, now).await == Ok(Deferred::Quarantined) {
                alert(
                    store,
                    AlertKind::Quarantined,
                    row,
                    "revalidation_unavailable",
                )
                .await;
            }
            None
        }
        Err(RevisionError::Changed(cause)) => {
            quarantine(
                state,
                store,
                row,
                "account_route_authorization_changed",
                cause,
            )
            .await;
            None
        }
        Ok(rev) if rev.route == row.revision && rev.authority == row.authorization_revision => {
            Some((rev.route, rev.authority, rev.agent))
        }
        Ok(_) => {
            quarantine(
                state,
                store,
                row,
                "account_route_authorization_changed",
                "snapshot_changed",
            )
            .await;
            None
        }
    }
}

async fn process_claimed(
    state: &LineState,
    store: &Arc<IngressStore>,
    row: IngressRow,
    cfg: &IngressConfig,
) {
    let parsed = row
        .payload
        .as_deref()
        .and_then(|p| serde_json::from_str::<serde_json::Value>(p).ok());
    let Some(payload) = parsed else {
        quarantine(
            state,
            store,
            &row,
            "payload_unavailable",
            "payload_unavailable",
        )
        .await;
        return;
    };
    let Ok(event) = serde_json::from_value::<LineEvent>(payload["event"].clone()) else {
        quarantine(state, store, &row, "payload_invalid", "payload_invalid").await;
        return;
    };
    let Some((revision, authorization, resolved)) = admit(state, store, &row, &event).await else {
        return;
    };
    let Some(token) = read_line_config(&state.home_dir)
        .await
        .map(|(t, _)| t)
        .filter(|t| !t.is_empty())
    else {
        // Review L4: reaching the limit here is announced like any other.
        let now = chrono::Utc::now().timestamp();
        if store
            .defer_unavailable(&row, "credentials_unreadable", now)
            .await
            == Ok(Deferred::Quarantined)
        {
            alert(
                store,
                AlertKind::Quarantined,
                &row,
                "revalidation_unavailable",
            )
            .await;
        }
        return;
    };
    // Decide the reply strategy before running anything (review I-HIGH-1 (3)).
    // The reply token is LINE's, valid from receipt: a retry or rerun does
    // not renew it. With "fail" a run past it is not executed (the resolve
    // step already refuses those; this also covers a setting changed after
    // the operator's decision).
    // A redelivered webhook's token is never tried (review N2).
    let rerun = row.run_authorization_id.is_some();
    let now = chrono::Utc::now().timestamp();
    let token_deadline = reply_token_deadline(row.received_at, &event);
    if cfg.late_reply == LateReply::Fail && now > token_deadline && event.reply_token.is_some() {
        let reason = if is_redelivery(&event) {
            "redelivered_reply_token_not_used"
        } else {
            "late_reply_expired"
        };
        if store
            .transition(&row, "claimed", "failed_before_dispatch", Some(reason))
            .await
            == Ok(true)
        {
            alert(store, AlertKind::LateReplyFailed, &row, reason).await;
        }
        return;
    }
    if !durable_line_enabled(&state.home_dir).await
        || store.renew(&row, chrono::Utc::now().timestamp()).await != Ok(true)
        || store.transition(&row, "claimed", "dispatching", None).await != Ok(true)
    {
        return;
    }
    let outcome = dispatch(
        state,
        store,
        &row,
        event,
        payload,
        token,
        resolved,
        Binding {
            state: state.clone(),
            revision,
            authorization,
            payload: serde_json::Value::Null,
            row: row.clone(),
        },
        RunReceipt::new(token_deadline, cfg.late_reply, rerun),
    )
    .await;
    let (status, reason, receipt) = outcome;
    if store
        .transition_with(&row, "dispatching", status, reason, Some(&receipt))
        .await
        != Ok(true)
    {
        error!(ingress_id = %row.id, "LINE dispatch receipt not persisted; recovery will require review");
        return;
    }
    let kind = match status {
        "uncertain" => AlertKind::Uncertain,
        "undelivered" => AlertKind::Undelivered,
        _ => return,
    };
    alert(store, kind, &row, reason.unwrap_or("unknown")).await;
}

/// Keep the dispatch lease alive. Ends only when the lease is definitely
/// lost, or when renewals kept failing until the lease would run out
/// (review I-MEDIUM-5).
async fn heartbeat(store: Arc<IngressStore>, row: IngressRow, _home: PathBuf) {
    #[cfg(not(test))]
    let (every, after_error) = (
        std::time::Duration::from_secs(20),
        std::time::Duration::from_secs(2),
    );
    #[cfg(test)]
    let (every, after_error) = (
        std::time::Duration::from_millis(25),
        std::time::Duration::from_millis(25),
    );
    #[cfg(test)]
    let probe = super::worker_test_probe(&_home);
    let mut last_ok = std::time::Instant::now();
    let mut wait = every;
    loop {
        tokio::time::sleep(wait).await;
        #[cfg(test)]
        if let Some(probe) = &probe {
            *probe
                .renewals
                .lock()
                .unwrap()
                .entry(row.id.clone())
                .or_default() += 1;
        }
        match store.renew(&row, chrono::Utc::now().timestamp()).await {
            Ok(true) => {
                last_ok = std::time::Instant::now();
                wait = every;
            }
            Ok(false) => break,
            Err(_) => {
                // Stop retrying with a margin before the lease would lapse.
                if last_ok.elapsed().as_secs() as i64 >= LEASE_SECONDS - 15 {
                    break;
                }
                wait = after_error;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn dispatch(
    state: &LineState,
    store: &Arc<IngressStore>,
    row: &IngressRow,
    event: LineEvent,
    payload: serde_json::Value,
    token: String,
    resolved: String,
    mut binding: Binding,
    receipt: RunReceipt,
) -> (&'static str, Option<&'static str>, AttemptReceipt) {
    binding.payload = payload;
    let heartbeat = heartbeat(store.clone(), row.clone(), state.home_dir.clone());
    tokio::pin!(heartbeat);
    let account_id = binding.payload["destination"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let conversation = line_conversation(&event);
    let target = crate::approval::TrustedReplyTarget::new(
        crate::approval::DecisionContext {
            channel: "line".into(),
            account_id: account_id.clone(),
            conversation_id: conversation.clone(),
            principal_id: event
                .source
                .as_ref()
                .and_then(|s| s.user_id.clone())
                .unwrap_or_default(),
        },
        token.clone(),
        conversation,
        None,
    )
    .map(|target| {
        target.with_decision_access_scope(crate::decision_notify::DecisionAccessScope {
            channel_id: event
                .source
                .as_ref()
                .and_then(|source| source.group_id.as_deref().or(source.room_id.as_deref())),
            guild_id: None,
            session_id: None,
        })
    })
    .and_then(|target| target.with_ingress_run(&row.run_id));
    let ledger = Arc::new(ProgressLedger::default());
    let run = RUN.scope(std::cell::RefCell::new(receipt), async {
        #[cfg(test)]
        super::dispatch_test_pause(state, row).await;
        PROGRESS
            .scope(
                ledger.clone(),
                INGRESS_BINDING.scope(
                    binding,
                    crate::approval::scope_trusted_reply(target, async {
                        if row.decision_fastlane {
                            process_line_decision(event, state, &token).await;
                        } else {
                            process_line_events(vec![event], state, &token, &resolved, &account_id)
                                .await;
                        }
                    }),
                ),
            )
            .await;
        RUN.with(|r| r.borrow().clone())
    });
    tokio::pin!(run);
    let finished = tokio::select! {
        receipt = &mut run => Some(receipt),
        _ = &mut heartbeat => None,
    };
    let progress_note = ledger.settle(std::time::Duration::from_secs(10)).await;
    match finished {
        Some(r) => (
            final_status(r.outcome),
            r.outcome,
            AttemptReceipt {
                provider_receipt: r.provider_receipt,
                delivered_via: (!r.delivered_via.is_empty()).then(|| r.delivered_via.join(",")),
                progress_note,
            },
        ),
        None => (
            "uncertain",
            Some("dispatch_lease_lost"),
            AttemptReceipt {
                progress_note,
                ..Default::default()
            },
        ),
    }
}
