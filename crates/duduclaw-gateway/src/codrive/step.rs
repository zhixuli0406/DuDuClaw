//! CD-1 single-step execution: the ApprovalBroker gate for a consequential
//! step, the actual wire-op dispatch, and the freeze/resume retry loop.
//! Split out of `driver.rs` to keep both files under the project's
//! per-file size convention (200-400 lines typical).
//!
//! Design authority: `commercial/docs/DESIGN-codrive-desktop-2026-08.md`
//! §3.3.2 (highlight predisplay), §3.4 (approval reuse), §3.1 (freeze is
//! human-input-priority, "「交還」是明確動作").

use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::json;

use crate::approval::{ApprovalBroker, ApprovalStatus, SimulationNarrative};

use super::atspi_locate::{self, LocateOutcome};
use super::client::{CodriveButtonState, CodriveClient, CodriveClientError, CodriveCmd};
use super::config::CodriveConfig;
use super::driver::ticker;
use super::registry::{self, DispatchOutcome};
use super::script::{
    ApiActionRequest, CodriveAction, CodriveConsequential, CodriveHighlight, CodriveStep, ConsequentialClass,
    LocateRequest,
};

/// The tool name stamped on every audit row this module writes — matches
/// the MCP tool name (`codrive_run`) so `tool_calls.jsonl` correlates.
pub(super) const TOOL_NAME: &str = "codrive_run";

/// Local pre-click predisplay delay before sending the action itself
/// (design §3.3.2(b)).
const PRE_CLICK_HIGHLIGHT_DELAY: Duration = Duration::from_millis(200);
/// How long comp is asked to keep the highlight box on screen.
const HIGHLIGHT_DISPLAY_MS: u32 = 200;
/// Freeze-wait poll interval — design §5 CD-1 row: "每 1s 送 status 輪詢".
const FROZEN_POLL_INTERVAL: Duration = Duration::from_secs(1);
/// `ApprovalBroker::await_decision` poll interval for a consequential step.
const APPROVAL_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Successful per-step execution result.
pub(super) struct StepSuccess {
    pub(super) approval_id: Option<String>,
    pub(super) reapplied: bool,
    /// CD-3: true iff this step was executed as a `take_over` hand-off
    /// (either an explicit `CodriveAction::TakeOver` step or a `Credential`-
    /// classed step auto-converted to one) rather than a normal action send
    /// — `driver.rs` maps this to the step outcome `"taken_over"`.
    pub(super) taken_over: bool,
    /// WP-CD4a (C-L2): true iff this step was served by a registry-backed
    /// native API/CLI/D-Bus action (`registry::dispatch` hit + succeeded)
    /// instead of its ordinary coordinate `action` — `driver.rs` maps this
    /// to the step outcome `"api_action"`. Mutually exclusive with
    /// `taken_over`/`reapplied`: a C-L2 hit returns immediately from
    /// `run_one_step`, before either of those other paths can be reached.
    pub(super) via_api_action: bool,
}

/// Whole-script-aborting per-step failure.
pub(super) struct StepAbort {
    pub(super) step_outcome: &'static str,
    pub(super) final_state: &'static str,
    pub(super) detail: String,
    pub(super) approval_id: Option<String>,
}

enum WaitAbort {
    Timeout,
    EmergencyStop,
}

/// Run one step: gate consequential actions behind ApprovalBroker BEFORE
/// dispatching anything, then execute — retrying the whole step across a
/// human freeze (comp drops a frozen action rather than buffering it, so
/// "resume" means re-sending the same step, not resuming mid-step).
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_one_step(
    client: &mut CodriveClient,
    broker: Option<&ApprovalBroker>,
    home_dir: &Path,
    agent_id: &str,
    session_id: &str,
    target_app: &str,
    index: usize,
    step: &CodriveStep,
    cfg: &CodriveConfig,
    started: Instant,
    deadline: Duration,
) -> Result<StepSuccess, StepAbort> {
    // CD-3 (task brief item 1): a take_over step — explicit
    // `CodriveAction::TakeOver` or an auto-converted `Credential`-classed
    // step — skips `gate_consequential`/ApprovalBroker entirely (an
    // approval dialog for "may the human type their own password" makes no
    // sense) and never reaches `send_step_actions` (the credential text, if
    // any, is never sent to comp at all — only the hand-off is).
    if let Some(reason) = take_over_reason(step) {
        return run_take_over_step(client, home_dir, agent_id, session_id, reason, started, deadline).await;
    }

    let approval_id = match &step.consequential {
        None => None,
        Some(cons) => match gate_consequential(broker, home_dir, agent_id, target_app, index, step, cons, cfg).await {
            Ok(id) => Some(id),
            Err(abort) => return Err(abort),
        },
    };

    // WP-CD4a (C-L2 registry checkpoint, DESIGN §3.2 execution ladder rung
    // 2): if this step declares an `api_action`, try the registry BEFORE
    // the ordinary coordinate dispatch loop below — a hit that actually
    // executes skips that loop (and therefore `client`/comp) entirely; a
    // MISS or an exec FAILURE both fall straight through to the unchanged
    // loop, exactly as if `api_action` had never been set. The approval
    // gate above already ran either way — a consequential step is gated
    // the same regardless of which mechanism ends up carrying it out.
    if let Some(req) = &step.api_action {
        if try_registry_action(home_dir, agent_id, target_app, req).await {
            return Ok(StepSuccess { approval_id, reapplied: false, taken_over: false, via_api_action: true });
        }
    }

    // WP-CD4b (C-L3 AT-SPI2 checkpoint, DESIGN §3.2 execution ladder rung
    // 3): if this step declares a `locate` query, try to resolve on-screen
    // coordinates via the accessibility bus and, on a hit, override THIS
    // COPY of the step's action before it ever reaches `send_step_actions`
    // — the original `step.action` (and therefore any future retry of this
    // same script run) is never mutated. A MISS/FAILED locate leaves
    // `effective_action` identical to `step.action`, i.e. the step's own
    // literal C-L1 coordinates keep going exactly as before.
    //
    // WP-CD4b-fix (B3): the locate now ALSO needs `client` — resolving a
    // control's position takes two halves, "where is it inside its window"
    // (AT-SPI `CoordType::Window`) and "where is that window on screen"
    // (a read-only `window_geometry` query to comp over this same
    // connection). See `atspi_locate`'s module doc for why the old
    // `CoordType::Screen` half was structurally unusable on GTK4. Reusing
    // the already-authenticated session connection (rather than opening a
    // second one) matters: a reconnect looks like a brand-new co-drive
    // session to comp — the exact bug CD-0 §9 fixed.
    let mut effective_action = step.action.clone();
    if let Some(req) = &step.locate {
        try_atspi_locate(client, home_dir, agent_id, target_app, req, &mut effective_action).await;
    }

    let mut reapplied = false;
    loop {
        match send_step_actions(client, &step.highlight, &effective_action).await {
            Ok(()) => return Ok(StepSuccess { approval_id, reapplied, taken_over: false, via_api_action: false }),
            Err(CodriveClientError::Frozen) => {
                ticker(home_dir, agent_id, "codrive_step", session_id, "已被人類輸入凍結，等待交還（Super+Enter）").await;
                match wait_for_resume(client, started, deadline).await {
                    Ok(()) => {
                        reapplied = true;
                        ticker(home_dir, agent_id, "codrive_step", session_id, "已交還，繼續執行").await;
                    }
                    Err(WaitAbort::Timeout) => {
                        return Err(StepAbort {
                            step_outcome: "aborted",
                            final_state: "aborted_frozen_timeout",
                            detail: "aborted_frozen_timeout".to_string(),
                            approval_id,
                        });
                    }
                    Err(WaitAbort::EmergencyStop) => {
                        return Err(StepAbort {
                            step_outcome: "aborted",
                            final_state: "aborted_emergency_stop",
                            detail: "aborted_emergency_stop".to_string(),
                            approval_id,
                        });
                    }
                }
            }
            Err(CodriveClientError::Terminated) => {
                return Err(StepAbort {
                    step_outcome: "aborted",
                    final_state: "aborted_emergency_stop",
                    detail: "aborted_emergency_stop".to_string(),
                    approval_id,
                });
            }
            Err(e) => {
                return Err(StepAbort {
                    step_outcome: "aborted",
                    final_state: "aborted_connection_lost",
                    detail: format!("連線錯誤，已中止：{e}"),
                    approval_id,
                });
            }
        }
    }
}

/// CD-3 (task brief item 1): does this step get routed to a `take_over`
/// hand-off instead of its normal action-send/approval path? Returns the
/// reason to send comp if so — an explicit `CodriveAction::TakeOver`'s own
/// reason takes priority; a `Credential`-classed step (any action kind)
/// falls back to its `consequential.description`, or the step's narration
/// if that's somehow empty (schema allows an empty `description` — never
/// leave comp's audit trail with a blank reason).
fn take_over_reason(step: &CodriveStep) -> Option<String> {
    if let CodriveAction::TakeOver { reason } = &step.action {
        return Some(reason.clone());
    }
    step.consequential
        .as_ref()
        .filter(|c| c.class == ConsequentialClass::Credential)
        .map(|c| if c.description.trim().is_empty() { step.narration.clone() } else { c.description.clone() })
}

/// Executes a take_over step: sends `{"op":"take_over","reason":…}`, waits
/// for the human's Super+Enter hand-back (reusing `wait_for_resume` — a
/// takeover ends via the exact same `frozen:false` transition an ordinary
/// freeze does; see comp's `codrive/takeover.rs`), then returns without
/// ever sending the step's own action (module doc: the credential text, if
/// any, never reaches comp).
async fn run_take_over_step(
    client: &mut CodriveClient,
    home_dir: &Path,
    agent_id: &str,
    session_id: &str,
    reason: String,
    started: Instant,
    deadline: Duration,
) -> Result<StepSuccess, StepAbort> {
    ticker(home_dir, agent_id, "codrive_step", session_id, &format!("已交棒給人類接手：{reason}")).await;

    match client.send(&CodriveCmd::TakeOver { reason }).await {
        Ok(_) => {}
        Err(CodriveClientError::Frozen) => {
            // Defensive (comp's own doc says this ack shape never actually
            // happens for `take_over` — see `takeover.rs`'s module doc) —
            // still handled, not `unreachable!`, per this crate's own
            // "never trust an upstream invariant alone" convention.
            ticker(home_dir, agent_id, "codrive_step", session_id, "接手指令送出時已被凍結，等待交還").await;
        }
        Err(CodriveClientError::Terminated) => {
            return Err(StepAbort {
                step_outcome: "aborted",
                final_state: "aborted_emergency_stop",
                detail: "aborted_emergency_stop".to_string(),
                approval_id: None,
            });
        }
        Err(e) => {
            return Err(StepAbort {
                step_outcome: "aborted",
                final_state: "aborted_connection_lost",
                detail: format!("連線錯誤，已中止：{e}"),
                approval_id: None,
            });
        }
    }

    match wait_for_resume(client, started, deadline).await {
        Ok(()) => {
            ticker(home_dir, agent_id, "codrive_step", session_id, "已交還，繼續執行下一步").await;
            Ok(StepSuccess { approval_id: None, reapplied: false, taken_over: true, via_api_action: false })
        }
        Err(WaitAbort::Timeout) => Err(StepAbort {
            step_outcome: "aborted",
            final_state: "aborted_frozen_timeout",
            detail: "aborted_frozen_timeout".to_string(),
            approval_id: None,
        }),
        Err(WaitAbort::EmergencyStop) => Err(StepAbort {
            step_outcome: "aborted",
            final_state: "aborted_emergency_stop",
            detail: "aborted_emergency_stop".to_string(),
            approval_id: None,
        }),
    }
}

/// Request + await ApprovalBroker approval for one consequential step.
/// Only `Approved` returns `Ok`; every other outcome (denied / expired /
/// pending / broker error) is fail-closed and aborts the whole script —
/// mirrors `run_install_approval`'s four-state mapping
/// (`duduclaw_cli::mcp`).
#[allow(clippy::too_many_arguments)]
async fn gate_consequential(
    broker: Option<&ApprovalBroker>,
    home_dir: &Path,
    agent_id: &str,
    target_app: &str,
    index: usize,
    step: &CodriveStep,
    cons: &CodriveConsequential,
    cfg: &CodriveConfig,
) -> Result<String, StepAbort> {
    let Some(broker) = broker else {
        let detail = "審批系統無法建立審核請求，已拒絕（fail-closed）".to_string();
        duduclaw_security::audit::append_tool_call_denied(home_dir, agent_id, TOOL_NAME, "codrive_action_denied", &detail, None);
        return Err(StepAbort { step_outcome: "denied", final_state: "aborted_approval_denied", detail, approval_id: None });
    };

    let simulation = SimulationNarrative {
        world_state_change: cons.description.clone(),
        risk_points: vec![format!("目標應用：{target_app}"), format!("動作類別：{}", cons.class.as_str())],
    }
    .to_json();
    let summary = format!("共駕請求核准：{}（{}）—— {}", step.narration, cons.class.as_str(), cons.description);
    let payload = json!({
        "target_app": target_app,
        "step_index": index,
        "action": step.action,
        "consequential_class": cons.class.as_str(),
    });

    let id = match broker
        .request_with_simulation(agent_id, "codrive_action", &summary, payload, cfg.approval_ttl_secs, simulation)
        .await
    {
        Ok(id) => id,
        Err(e) => {
            let detail = format!("建立審批請求失敗，已拒絕（fail-closed）：{e}");
            duduclaw_security::audit::append_tool_call_denied(home_dir, agent_id, TOOL_NAME, "codrive_action_denied", &detail, None);
            return Err(StepAbort { step_outcome: "denied", final_state: "aborted_approval_denied", detail, approval_id: None });
        }
    };
    let approval_id = Some(id.to_string());

    match broker.await_decision(&id, APPROVAL_POLL_INTERVAL).await {
        Ok(ApprovalStatus::Approved) => Ok(id.to_string()),
        Ok(ApprovalStatus::Denied) => {
            let detail = format!("審批已拒絕（審核編號 {id}）");
            duduclaw_security::audit::append_tool_call_denied(home_dir, agent_id, TOOL_NAME, "codrive_action_denied", &detail, None);
            Err(StepAbort { step_outcome: "denied", final_state: "aborted_approval_denied", detail, approval_id })
        }
        Ok(ApprovalStatus::Expired) => {
            let detail = format!("審批逾時未核可，已自動拒絕（審核編號 {id}）");
            duduclaw_security::audit::append_tool_call_denied(home_dir, agent_id, TOOL_NAME, "codrive_action_denied", &detail, None);
            Err(StepAbort { step_outcome: "denied", final_state: "aborted_approval_expired", detail, approval_id })
        }
        Ok(ApprovalStatus::Pending) => {
            let detail = format!("審批狀態異常（仍為待審），已拒絕（審核編號 {id}）");
            duduclaw_security::audit::append_tool_call_denied(home_dir, agent_id, TOOL_NAME, "codrive_action_denied", &detail, None);
            Err(StepAbort { step_outcome: "denied", final_state: "aborted_approval_denied", detail, approval_id })
        }
        Err(e) => {
            let detail = format!("等待審批決定時發生錯誤，已拒絕（fail-closed）：{e}");
            duduclaw_security::audit::append_tool_call_denied(home_dir, agent_id, TOOL_NAME, "codrive_action_denied", &detail, None);
            Err(StepAbort { step_outcome: "denied", final_state: "aborted_approval_denied", detail, approval_id })
        }
    }
}

/// WP-CD4a: try this step's `api_action` against `registry::dispatch`.
/// Returns `true` only on an actual `Executed` hit (the caller then skips
/// the coordinate dispatch loop entirely); a `Miss` or a `Failed` both
/// return `false` so the caller falls through to that loop unchanged — the
/// whole "查無/呼叫失敗→原樣落回既有 C-L1 座標路徑" contract lives in that
/// single boolean, not in any special-cased error handling here. Every one
/// of the three outcomes is audited (`registry_outcome`: `executed` /
/// `registry_miss_fallback` / `exec_failed_fallback`) — this is the "三者
/// 各自入稽核" line the WP brief asks for, one row per attempt, disambiguated
/// by that field rather than three separate audit calls, so a reader
/// scanning `tool_calls.jsonl` for one `codrive_run` step sees exactly one
/// C-L2 row instead of reconstructing it from a sequence.
async fn try_registry_action(home_dir: &Path, agent_id: &str, target_app: &str, req: &ApiActionRequest) -> bool {
    let outcome = registry::dispatch(target_app, req).await;
    let (registry_outcome, success, detail) = match &outcome {
        DispatchOutcome::Executed { detail } => ("executed", true, detail.clone()),
        DispatchOutcome::Miss => (
            "registry_miss_fallback",
            false,
            format!("no C-L2 registry entry for app={target_app:?} action={:?} — falling back to C-L1", req.action),
        ),
        DispatchOutcome::Failed { detail } => ("exec_failed_fallback", false, detail.clone()),
    };
    let params_summary = format!(
        "api_action {registry_outcome}: app={target_app} action={} — {}",
        req.action,
        duduclaw_core::truncate_chars(&detail, 200)
    );
    duduclaw_security::audit::append_tool_call_with_extras(
        home_dir,
        agent_id,
        TOOL_NAME,
        &params_summary,
        success,
        &[
            ("registry_outcome", json!(registry_outcome)),
            ("app_id", json!(target_app)),
            ("action", json!(req.action)),
        ],
    );
    matches!(outcome, DispatchOutcome::Executed { .. })
}

/// WP-CD4b: try this step's `locate` query against `atspi_locate::locate`.
/// On a `Located` hit, overwrites `action`'s x/y in place — `Move`/`Click`
/// only, every other action kind is left untouched since there is no
/// coordinate to override. On `Miss`/`Failed`, `action` is left exactly as
/// the caller passed it in, so the step's own literal C-L1 coordinates keep
/// going unchanged — the whole "查無/失敗→原樣落回既有座標" contract lives in
/// that untouched `action`, not in any special-cased error handling here,
/// mirroring `try_registry_action`'s C-L2 shape one rung up. Every one of
/// the three outcomes is audited (`locate_outcome`: `located` /
/// `locate_miss_fallback` / `locate_failed_fallback`).
///
/// WP-CD4b-fix (B3): `Failed` is now a much broader (and much more useful)
/// category than "the bus was unreachable" — it also covers "more than one
/// accessible matched", "comp could not uniquely identify the window", and
/// "the converted point fell outside that window". Every one of those is a
/// deliberate refusal to click on an untrustworthy coordinate, and every
/// one lands in `tool_calls.jsonl` with its reason in `params_summary`.
async fn try_atspi_locate(
    client: &mut CodriveClient,
    home_dir: &Path,
    agent_id: &str,
    target_app: &str,
    req: &LocateRequest,
    action: &mut CodriveAction,
) {
    let outcome = atspi_locate::locate(client, target_app, req).await;
    let (locate_outcome, success, detail, resolved) = match &outcome {
        LocateOutcome::Located { x, y, detail } => ("located", true, detail.clone(), Some((*x, *y))),
        LocateOutcome::Miss => (
            "locate_miss_fallback",
            false,
            format!(
                "no AT-SPI match for app={target_app:?} role={:?} name={:?} — falling back to C-L1",
                req.role, req.name
            ),
            None,
        ),
        LocateOutcome::Failed { detail } => ("locate_failed_fallback", false, detail.clone(), None),
    };
    if let Some((x, y)) = resolved {
        match action {
            CodriveAction::Move { x: ax, y: ay } | CodriveAction::Click { x: ax, y: ay, .. } => {
                *ax = x;
                *ay = y;
            }
            CodriveAction::Text { .. } | CodriveAction::KeyName { .. } | CodriveAction::Wait { .. } | CodriveAction::TakeOver { .. } => {}
        }
    }
    let params_summary = format!(
        "locate {locate_outcome}: app={target_app} role={} name={} — {}",
        req.role,
        req.name,
        duduclaw_core::truncate_chars(&detail, 200)
    );
    duduclaw_security::audit::append_tool_call_with_extras(
        home_dir,
        agent_id,
        TOOL_NAME,
        &params_summary,
        success,
        &[
            ("locate_outcome", json!(locate_outcome)),
            ("app_id", json!(target_app)),
            ("role", json!(&req.role)),
            ("name", json!(&req.name)),
        ],
    );
}

/// Send one step's wire ops: optional highlight + 200ms predisplay, then
/// the action itself. `click` = move + button press + release; `key_name`
/// = press + release tap; `wait` never touches the socket. `action` is the
/// step's *effective* action — WP-CD4b's C-L3 locate checkpoint
/// (`run_one_step`) may have already overridden its x/y before this is
/// called, so this function itself has no locate-awareness at all.
async fn send_step_actions(
    client: &mut CodriveClient,
    highlight: &Option<CodriveHighlight>,
    action: &CodriveAction,
) -> Result<(), CodriveClientError> {
    if let Some(h) = highlight {
        client.send(&CodriveCmd::Highlight { x: h.x, y: h.y, w: h.w, h: h.h, ms: HIGHLIGHT_DISPLAY_MS }).await?;
        tokio::time::sleep(PRE_CLICK_HIGHLIGHT_DELAY).await;
    }
    match action {
        CodriveAction::Move { x, y } => {
            client.send(&CodriveCmd::Move { x: *x, y: *y }).await?;
        }
        CodriveAction::Click { x, y, btn } => {
            client.send(&CodriveCmd::Move { x: *x, y: *y }).await?;
            client.send(&CodriveCmd::Button { btn: *btn, state: CodriveButtonState::Press }).await?;
            client.send(&CodriveCmd::Button { btn: *btn, state: CodriveButtonState::Release }).await?;
        }
        CodriveAction::Text { s } => {
            client.send(&CodriveCmd::Text { s: s.clone() }).await?;
        }
        CodriveAction::KeyName { name } => {
            client.send(&CodriveCmd::KeyName { name: name.clone(), state: CodriveButtonState::Press }).await?;
            client.send(&CodriveCmd::KeyName { name: name.clone(), state: CodriveButtonState::Release }).await?;
        }
        CodriveAction::Wait { ms } => {
            tokio::time::sleep(Duration::from_millis(u64::from(*ms))).await;
        }
        CodriveAction::TakeOver { .. } => {
            // Defensive, not load-bearing: `run_one_step` already
            // intercepts every `TakeOver` step via `take_over_reason`
            // before `send_step_actions` is ever called — kept as an arm
            // (not `unreachable!`) so a future change fails safe instead of
            // panicking, matching comp's own `handle_agent_inject` arms for
            // its socket-thread-only ops.
            tracing::warn!("codrive: a TakeOver action reached send_step_actions unexpectedly — no-op (see step::take_over_reason)");
        }
    }
    Ok(())
}

/// Poll `status` every [`FROZEN_POLL_INTERVAL`] until the human has
/// resumed (design §3.1: "「交還」是明確動作", never inferred from
/// silence), an emergency stop is observed, or the whole-script `deadline`
/// elapses.
async fn wait_for_resume(client: &mut CodriveClient, started: Instant, deadline: Duration) -> Result<(), WaitAbort> {
    loop {
        if started.elapsed() >= deadline {
            return Err(WaitAbort::Timeout);
        }
        tokio::time::sleep(FROZEN_POLL_INTERVAL).await;
        match client.send(&CodriveCmd::Status).await {
            Ok(ack) => {
                if client.drain_events().iter().any(|e| e.event == "emergency_stop") {
                    return Err(WaitAbort::EmergencyStop);
                }
                if ack.terminated == Some(true) {
                    return Err(WaitAbort::EmergencyStop);
                }
                if ack.frozen == Some(false) {
                    return Ok(());
                }
                // Still frozen — keep polling.
            }
            Err(CodriveClientError::Terminated) => return Err(WaitAbort::EmergencyStop),
            Err(CodriveClientError::Frozen) => {} // status is available even while frozen; keep polling
            Err(_) => return Err(WaitAbort::EmergencyStop), // session unusable — honest abort
        }
    }
}
