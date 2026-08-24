//! CD-1 co-drive script orchestration entry point: [`run_script`].
//!
//! Handles the whole-script refuse-list (§8-3), connects to comp, then
//! walks the steps via [`super::step::run_one_step`] — per-step
//! ApprovalBroker gating, wire dispatch, and freeze/resume retry live in
//! `step.rs` (split out to keep both files under this project's per-file
//! size convention). This file owns the report shape, the activity-feed
//! ticker, and the top-level control flow.
//!
//! Design authority: `commercial/docs/DESIGN-codrive-desktop-2026-08.md`
//! §3.4 (approval reuse), §6 (safety red lines), §8-3 (refuse-list).
//!
//! **CD-2 file-size note**: this file is already near this project's
//! per-file convention (200-400 lines typical, 800 max). New orchestration
//! logic that doesn't fit `step.rs`'s existing per-step split should get
//! its own module (see `identity.rs` for the pattern: a focused concern,
//! `pub use`-re-exported from `mod.rs`) rather than being appended here.

use std::path::Path;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::json;

use crate::approval::ApprovalBroker;
use crate::events_store::EventBusStore;
use crate::task_store::{ActivityRow, TaskStore};

use super::client::{CodriveClient, CodriveCmd};
use super::config::CodriveConfig;
use super::mode::{CodriveDrivingMode, CodriveHandoverReason};
use super::plan_approval::{PLAN_DENIED_STATE, gate_plan_approval};
use super::script::CodriveScript;
use super::step::{TOOL_NAME, run_one_step};

/// Refuse-list keywords (design §8-3 initial list): hitting one of these
/// in `target_app` / `task_summary` / any step narration refuses the WHOLE
/// script before any connection is attempted — not even an approval row is
/// created.
const DENY_KEYWORDS: &[&str] = &["網銀", "netbank", "online banking", "captcha"];

/// Per-step outcome of one `codrive_run` call.
#[derive(Debug, Clone, Serialize)]
pub struct CodriveStepReport {
    pub index: usize,
    pub narration: String,
    /// One of: `applied` / `refused` / `denied` / `dropped_frozen_reapplied`
    /// / `taken_over` (CD-3) / `api_action` (WP-CD4a, C-L2 registry hit —
    /// served by a native CLI/D-Bus action, never touched the comp socket)
    /// / `aborted`. WP-CD4b's C-L3 AT-SPI2 locate does NOT add a new tag
    /// here — a locate hit still dispatches over the comp socket exactly
    /// like a literal-coordinate step (just with resolved x/y), so it
    /// reports `applied`/`reapplied` same as always; see `tool_calls.jsonl`'s
    /// `locate_outcome` audit field (`step::try_atspi_locate`) for whether a
    /// given step was actually served by a locate.
    pub outcome: &'static str,
    pub detail: Option<String>,
    pub approval_id: Option<String>,
    pub at: String,
}

/// Full report of one `codrive_run` call — this IS the tool's JSON
/// response shape.
#[derive(Debug, Clone, Serialize)]
pub struct CodriveRunReport {
    pub session_id: String,
    /// e.g. `completed` / `refused_invalid` / `refused_denylist` /
    /// `aborted_connect` / `aborted_plan_denied` (A2: the DESIGN §3.5
    /// session-level plan card was not approved — see the `plan_approval`
    /// module) / `aborted_approval_denied` /
    /// `aborted_approval_expired` / `aborted_frozen_timeout` /
    /// `aborted_emergency_stop` / `aborted_connection_lost`. (CD-3: a
    /// `Credential`-classed step no longer produces `refused_credential` —
    /// see `step::take_over_reason`; the step's own outcome becomes
    /// `taken_over` instead and the script continues.)
    pub final_state: &'static str,
    pub detail: Option<String>,
    pub steps: Vec<CodriveStepReport>,
    /// A2 §1: which seat comp reported was driving when this session
    /// started, and when it ended. `None` means comp never reported a mode
    /// on this connection — either a compositor that predates A2, or a run
    /// that ended before any mode-bearing line came back. It deliberately
    /// does NOT default to `human`: "nobody told us" and "the agent had no
    /// driving authority" are different facts and a report that conflates
    /// them is a report that lies.
    ///
    /// **Cost: zero extra wire ops.** Both values are read out of
    /// [`super::client::CodriveClient::last_driving_mode`], which is
    /// populated as a side effect of lines the session was already going to
    /// exchange — the `driving_mode` pushes comp sends on every real mode
    /// change (A2 §3.2) and the `status` acks `step::wait_for_resume`
    /// already polls while frozen. No `status` query was added to any
    /// script path for this field.
    pub driving_mode_at_start: Option<CodriveDrivingMode>,
    pub driving_mode_at_end: Option<CodriveDrivingMode>,
    /// The handover reason accompanying [`Self::driving_mode_at_end`], when
    /// the session ended in `handover`.
    pub handover_reason_at_end: Option<CodriveHandoverReason>,
}

/// Run one co-drive script end to end. Never panics; every failure path
/// (refuse-list hit, connect failure, approval denial, freeze timeout,
/// emergency stop) returns an honest report instead — "空結果優於假結果".
///
/// `agent_id` is TRUSTED here — this function does not re-check
/// `[capabilities] codrive` for it, mirroring the "trust boundary at the
/// edge, trusted executor at the core" split `duduclaw-comp`'s
/// `listener.rs`/`codrive::exec` already use for the wire protocol (see
/// that crate's module docs). The caller (today: `duduclaw-cli/src/
/// mcp.rs::handle_codrive_run`) MUST have already resolved and authorized
/// `agent_id` via [`super::identity::resolve_run_identity`] before calling
/// this — see that function's doc comment for the full "`agent` parameter
/// overrides the WHOLE call identity, including the capability re-check"
/// semantics (CD-2 task brief item 2), why it's the conservative/correct
/// reading, and exactly what would need to change to narrow it to
/// approval-attribution-only. Every consequential step's approval row
/// (`step::gate_consequential` → `ApprovalBroker::request_with_simulation
/// (agent_id, ...)`) and every audit-denial line in this file is filed
/// under this same `agent_id`.
pub async fn run_script(
    home_dir: &Path,
    agent_id: &str,
    script: CodriveScript,
) -> CodriveRunReport {
    let session_id = uuid::Uuid::new_v4().to_string();
    let mut steps: Vec<CodriveStepReport> = Vec::new();

    let script = match script.sanitize() {
        Ok(s) => s,
        Err(e) => {
            return early_report(session_id, "refused_invalid", Some(e));
        }
    };

    // ── §8-3 refuse-list: hit ⇒ refuse the whole script, no approval row. ─
    if let Some(reason) = denylist_hit(&script) {
        duduclaw_security::audit::append_tool_call_denied(
            home_dir,
            agent_id,
            TOOL_NAME,
            "codrive_refused_denylist",
            &reason,
            None,
        );
        ticker(
            home_dir,
            agent_id,
            "codrive_session",
            &session_id,
            &format!("共駕請求被拒（拒做清單）：{reason}"),
        )
        .await;
        return early_report(session_id, "refused_denylist", Some(reason));
    }
    // CD-3 (task brief item 1): a credential-classed step is no longer a
    // whole-script refusal — see `step::take_over_reason` for the
    // per-step auto-conversion to a `take_over` hand-off. The refuse-list
    // above (banking/CAPTCHA keywords) is a SEPARATE, unchanged gate.

    let cfg = CodriveConfig::from_home(home_dir);

    // ── DESIGN §3.5 plan-approval card (A2 item 3), opt-in ──────────────
    // Deliberately BEFORE `resolve_endpoint`/`connect`: a plan nobody
    // approved must never open a co-drive session at all, so there is no
    // window in which comp has an authenticated agent connection for a run
    // that is about to be refused. Fail-closed on every non-approval —
    // see `plan_approval`'s module doc.
    if cfg.plan_approval {
        if let Err(detail) = gate_plan_approval(home_dir, agent_id, &script, &cfg).await {
            ticker(
                home_dir,
                agent_id,
                "codrive_session",
                &session_id,
                &format!("共駕計畫未獲核准，未連線即中止：{detail}"),
            )
            .await;
            return early_report(session_id, PLAN_DENIED_STATE, Some(detail));
        }
    }

    let (socket_path, token) = match resolve_endpoint(&cfg).await {
        Ok(v) => v,
        Err(e) => {
            ticker(
                home_dir,
                agent_id,
                "codrive_session",
                &session_id,
                &format!("共駕連線設定錯誤：{e}"),
            )
            .await;
            return early_report(session_id, "aborted_connect", Some(e));
        }
    };
    let connect_timeout = Duration::from_secs(cfg.connect_timeout_secs.max(1));
    let mut client = match CodriveClient::connect(&socket_path, &token, connect_timeout).await {
        Ok(c) => c,
        Err(e) => {
            let detail = e.to_string();
            ticker(
                home_dir,
                agent_id,
                "codrive_session",
                &session_id,
                &format!("共駕連線失敗：{detail}"),
            )
            .await;
            return early_report(session_id, "aborted_connect", Some(detail));
        }
    };

    // A2 §1: the mode comp reported by the time the session was live.
    // Read from the client's passive observation cache — no `status` query
    // is sent for it (see `CodriveRunReport::driving_mode_at_start`).
    let driving_mode_at_start = client.last_driving_mode();

    ticker(
        home_dir,
        agent_id,
        "codrive_session",
        &session_id,
        &format!("開始共駕：{}", script.task_summary),
    )
    .await;

    // CD-3 (task brief item 3, DESIGN §3.4 watch mode): opt-in, whole-
    // script idle-supervision request. Best-effort — a failed `watch`
    // toggle degrades to "no idle supervision this run", never aborts the
    // script (supervision is a safety ENHANCEMENT here, not itself a
    // safety GATE like the approval/refuse-list checks above).
    if script.watch_mode {
        match client.send(&CodriveCmd::Watch { enable: true }).await {
            Ok(_) => {
                ticker(home_dir, agent_id, "codrive_session", &session_id, "已啟用旁觀模式（人離開自動暫停）").await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "codrive: failed to enable watch mode — continuing without idle supervision");
            }
        }
    }

    // Fail-closed: if the broker can't even open, every consequential step
    // is denied (see `step::run_one_step`'s `broker.is_none()` branch) — a
    // script with no consequential steps still runs fine.
    let broker = ApprovalBroker::open(home_dir).ok();
    let started = Instant::now();
    let deadline = Duration::from_secs(cfg.max_session_secs.max(1));

    let mut final_state: &'static str = "completed";
    let mut final_detail: Option<String> = None;

    for (index, step) in script.steps.iter().enumerate() {
        ticker(
            home_dir,
            agent_id,
            "codrive_step",
            &session_id,
            &step.narration,
        )
        .await;
        match run_one_step(
            &mut client,
            broker.as_ref(),
            home_dir,
            agent_id,
            &session_id,
            &script.target_app,
            index,
            step,
            &cfg,
            started,
            deadline,
        )
        .await
        {
            Ok(s) => {
                let outcome = if s.via_api_action {
                    "api_action"
                } else if s.taken_over {
                    "taken_over"
                } else if s.reapplied {
                    "dropped_frozen_reapplied"
                } else {
                    "applied"
                };
                steps.push(step_report(
                    index,
                    &step.narration,
                    outcome,
                    None,
                    s.approval_id,
                ));
            }
            Err(a) => {
                steps.push(step_report(
                    index,
                    &step.narration,
                    a.step_outcome,
                    Some(a.detail.clone()),
                    a.approval_id,
                ));
                final_state = a.final_state;
                final_detail = Some(a.detail);
                break;
            }
        }
    }

    ticker(
        home_dir,
        agent_id,
        "codrive_session",
        &session_id,
        &format!("共駕結束：{final_state}"),
    )
    .await;
    CodriveRunReport {
        session_id,
        final_state,
        detail: final_detail,
        steps,
        driving_mode_at_start,
        driving_mode_at_end: client.last_driving_mode(),
        handover_reason_at_end: client.last_handover_reason(),
    }
}

/// Every pre-session failure path (invalid script, refuse-list hit,
/// unapproved plan, no endpoint, failed connect) shares one shape: no steps
/// ran and no driving mode is known, because comp was never in a position
/// to report one. Kept as a constructor so a future field cannot be added
/// to [`CodriveRunReport`] and silently forgotten on four of five paths.
fn early_report(
    session_id: String,
    final_state: &'static str,
    detail: Option<String>,
) -> CodriveRunReport {
    CodriveRunReport {
        session_id,
        final_state,
        detail,
        steps: Vec::new(),
        driving_mode_at_start: None,
        driving_mode_at_end: None,
        handover_reason_at_end: None,
    }
}

fn denylist_hit(script: &CodriveScript) -> Option<String> {
    let mut haystacks: Vec<&str> = vec![script.target_app.as_str(), script.task_summary.as_str()];
    haystacks.extend(script.steps.iter().map(|s| s.narration.as_str()));
    for kw in DENY_KEYWORDS {
        if haystacks
            .iter()
            .any(|h| duduclaw_core::word_contains_ci(h, kw))
        {
            return Some(format!("命中拒做關鍵詞「{kw}」"));
        }
    }
    None
}

/// `pub(super)` so [`super::status`]'s read-only query reuses this exact
/// endpoint/token resolution instead of keeping a second copy that could
/// drift from the config surface.
pub(super) async fn resolve_endpoint(
    cfg: &CodriveConfig,
) -> Result<(std::path::PathBuf, String), String> {
    let socket_path = cfg.resolved_socket_path()?;
    let token_path = cfg.resolved_token_path()?;
    let token = tokio::fs::read_to_string(&token_path)
        .await
        .map_err(|e| format!("讀取共駕鑑別 token 失敗（{}）：{e}", token_path.display()))?;
    Ok((socket_path, token.trim().to_string()))
}

fn step_report(
    index: usize,
    narration: &str,
    outcome: &'static str,
    detail: Option<String>,
    approval_id: Option<String>,
) -> CodriveStepReport {
    CodriveStepReport {
        index,
        narration: narration.to_string(),
        outcome,
        detail,
        approval_id,
        at: now_rfc3339(),
    }
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Post one activity-feed entry (menu-bar / notification ticker). MCP
/// subprocesses have no `event_tx` back into the gateway, so this goes
/// through the same durable path `handle_activity_post` uses
/// (`duduclaw-cli/src/mcp.rs`): `TaskStore::append_activity` +
/// `EventBusStore` `activity.new` broadcast. `session_id` is stamped as
/// `task_id` purely to group a session's rows in the feed — this is a
/// co-drive session, not a task-board task, and `activity.task_id` carries
/// no foreign-key constraint. `pub(super)` so `step.rs` can post its own
/// freeze/resume ticker entries through the same path.
pub(super) async fn ticker(
    home_dir: &Path,
    agent_id: &str,
    event_type: &str,
    session_id: &str,
    summary: &str,
) {
    let Ok(store) = TaskStore::open(home_dir) else {
        tracing::warn!("codrive: failed to open task store for ticker");
        return;
    };
    let row = ActivityRow {
        id: uuid::Uuid::new_v4().to_string(),
        event_type: event_type.to_string(),
        agent_id: agent_id.to_string(),
        task_id: Some(session_id.to_string()),
        summary: duduclaw_core::truncate_chars(summary, 500),
        timestamp: now_rfc3339(),
        metadata: None,
    };
    if store.append_activity(&row).await.is_err() {
        tracing::warn!("codrive: append_activity failed");
        return;
    }
    if let Ok(bus) = EventBusStore::open(home_dir) {
        let payload = json!({
            "id": row.id, "type": row.event_type, "agent_id": row.agent_id,
            "task_id": row.task_id, "summary": row.summary,
            "timestamp": row.timestamp, "metadata": row.metadata,
        });
        let _ = bus.append("activity.new", &payload.to_string()).await;
    }
}
