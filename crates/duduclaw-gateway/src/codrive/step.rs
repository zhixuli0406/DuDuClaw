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

use super::client::{CodriveButtonState, CodriveClient, CodriveClientError, CodriveCmd};
use super::config::CodriveConfig;
use super::driver::ticker;
use super::script::{CodriveAction, CodriveConsequential, CodriveStep};

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
    let approval_id = match &step.consequential {
        None => None,
        Some(cons) => match gate_consequential(broker, home_dir, agent_id, target_app, index, step, cons, cfg).await {
            Ok(id) => Some(id),
            Err(abort) => return Err(abort),
        },
    };

    let mut reapplied = false;
    loop {
        match send_step_actions(client, step).await {
            Ok(()) => return Ok(StepSuccess { approval_id, reapplied }),
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

/// Send one step's wire ops: optional highlight + 200ms predisplay, then
/// the action itself. `click` = move + button press + release; `key_name`
/// = press + release tap; `wait` never touches the socket.
async fn send_step_actions(client: &mut CodriveClient, step: &CodriveStep) -> Result<(), CodriveClientError> {
    if let Some(h) = &step.highlight {
        client.send(&CodriveCmd::Highlight { x: h.x, y: h.y, w: h.w, h: h.h, ms: HIGHLIGHT_DISPLAY_MS }).await?;
        tokio::time::sleep(PRE_CLICK_HIGHLIGHT_DELAY).await;
    }
    match &step.action {
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
