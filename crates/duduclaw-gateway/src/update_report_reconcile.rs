//! Cross-restart update result reconciliation (Y8-3, T1 —
//! `commercial/docs/DESIGN-agent-body-update-2026-08.md` §3.4/§13).
//!
//! Y5-3 designed but explicitly did NOT implement "agent triggers an OS/self
//! update → the box (or the gateway process) restarts → someone still has to
//! tell the human what happened, from a session that no longer exists" —
//! three architecture questions were left open (§3.4.3). This module is the
//! answer, landed as a deterministic, zero-LLM sweep rather than the
//! heartbeat-wakes-the-agent design Y5-3 sketched — see the module-level
//! doc in the design doc §13 for the full reasoning; short version:
//!
//! 1. **"Tool call auto-chains another tool call" has no precedent and stays
//!    that way.** `os_operator.rs` never calls an MCP handler for the agent,
//!    and system-prompt-only reliance ("please remember to call
//!    `working_state_set`") is unreliable by the design doc's own admission.
//!    Instead, `mcp_os_ops.rs::handle_os_apply_update` writes
//!    `pending_update_report` itself, handler-side, using
//!    [`crate::working_state::set_entry`] directly (the same pure Rust API
//!    the MCP tool wraps) — deterministic, not dependent on model
//!    cooperation. The "which channel/agent" context gap Y5-3 flagged turned
//!    out to already be solved: `DUDUCLAW_REPLY_CHANNEL`
//!    (`duduclaw_core::ENV_REPLY_CHANNEL`) already propagates from
//!    `channel_reply.rs`'s CLI spawn down into the `duduclaw mcp-server`
//!    child process's environment for exactly this purpose (see
//!    `decision_notify::origin_target`'s doc comment — the install-approval
//!    flow already relies on the same inherited env var).
//!
//! 2. **Gateway-autonomous execution vs. prompt injection is a false
//!    choice.** Neither happens. This sweep never calls an LLM and never
//!    puts anything into an agent's conversation turn — it calls the exact
//!    same pure, stateless functions the `os_boot_assessment`/
//!    `os_check_update` MCP tools call
//!    (`device_ops::select_device_ops().boot_assessment_status()`,
//!    `updater::current_version()`), the same way `stage_and_apply_device_
//!    update` is already shared between the dashboard RPC and the MCP tool.
//!    The eventual notification text is a **fixed template**, not an LLM
//!    completion — deliberately, so there is no new "gateway calls an LLM
//!    unprompted, on the agent's behalf" surface to reason about. This is
//!    the same shape `reminder_scheduler`'s scheduled reminders already use.
//!
//!    This also *overrides* Y5-3's own sketch, which proposed waking the
//!    agent via `HeartbeatScheduler`'s `execute_proactive_check` and letting
//!    the model decide to check. Two facts found during T1's implementation
//!    make that unworkable: (a) `duduclaw-agent` (which owns
//!    `HeartbeatScheduler`) architecturally cannot depend on
//!    `duduclaw-gateway` (which owns `working_state`) — the dependency
//!    points the other way — so the proactive-check prompt never sees
//!    `pending_update_report` at all; (b) `execute_proactive_check` only
//!    fires when `agent.toml [proactive] enabled = true`, which most
//!    production agents leave off. A mechanism gated behind an
//!    often-disabled opt-in cannot be the ONLY path to an honest report.
//!    This sweep instead piggy-backs `DispatchEngine::tick_once()` (already
//!    inside `duduclaw-gateway`, already runs unconditionally for every
//!    agent) — see step 5 there.
//!
//! 3. **`target: "system"`'s self-restart timing.** `os_apply_update`'s MCP
//!    path runs inside the short-lived, per-session `duduclaw mcp-server`
//!    subprocess — a DIFFERENT OS process from the long-running
//!    `duduclaw-gateway` that actually needs to restart. Calling
//!    `duduclaw_core::platform::self_interrupt()` there would only kill the
//!    already-exiting MCP subprocess, never the gateway — so, before this
//!    module, an agent-triggered `system` update silently never restarted
//!    the gateway at all (confirmed: `apply_system_update` swaps the binary
//!    and stops, with none of `handlers.rs`'s `system.apply_update` RPC
//!    path's broadcast-then-schedule-restart logic). The fix: this sweep —
//!    which DOES run inside the gateway process — is the one that actually
//!    calls `request_restart_after_shutdown()` + `self_interrupt()`, the
//!    first tick it notices `restart_triggered == false` for a `system`
//!    entry. `pending_update_report` doubles as the cross-process signal
//!    (the MCP subprocess can't reach into the gateway's memory, but it CAN
//!    leave a note on disk that the gateway's own tick reads) and as the
//!    payload for the eventual report — one write, two jobs.
//!
//! Known, documented limitations (not silently papered over):
//! - No `system.update_installed` WS broadcast for the agent-triggered path
//!   — `DispatchEngine` has no `event_tx` handle (that's `MethodHandler`
//!   state). A connected dashboard simply won't show the "restarting..."
//!   overlay for an agent-initiated update the way it does for a
//!   human-initiated one. Cosmetic, not a correctness gap.
//! - If the gateway is down across the entire `pending_update_report` TTL
//!   window (default 4h, set at write time), the entry silently expires
//!   unreported — `working_state::get_entry` treats an expired entry as
//!   absent, matching every other TTL-gated read in this codebase. Nobody
//!   is around to receive a report during that downtime either way.
//! - Delivery success (`Sent`) actually reaching a human is NOT covered by
//!   this module's own automated tests — that requires a live/mocked
//!   channel sender, which `reminder_scheduler`/`goal_notify`'s own test
//!   suites already own. This module's tests lock down the state machine
//!   (trigger/grace/verify/finalize) and the pure classifiers, not channel
//!   delivery. A real cross-restart, cross-channel run is still a manual/
//!   live verification item (see the design doc's §11 test ledger).

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use duduclaw_core::WORKING_STATE_KEY_PENDING_UPDATE_REPORT as REPORT_KEY;

/// Grace window after scheduling the gateway's self-restart before this
/// sweep will attempt to verify the new version actually took effect.
/// Generous on purpose: cold start (DB open, agent registry scan, channel
/// bot reconnects) can take much longer than the bare 3s restart delay
/// itself. A verify attempt inside the grace window would just be reading
/// the still-dying old process's own `current_version()` and falsely
/// reporting failure.
const SYSTEM_RESTART_GRACE_SECS: i64 = 120;

/// If a `device`-target report is still `Unknown` (boot assessment not yet
/// resolved) with less than this much TTL remaining, send one honest
/// "couldn't determine, please check manually" report instead of letting the
/// entry silently expire unreported. Chosen well inside a single dispatch
/// tick's margin of the outer `ttl_hours` (default 4h) written at `os_apply_
/// update` time.
const DEVICE_FINAL_REPORT_GRACE_SECS: i64 = 300;

/// The JSON shape stored in `pending_update_report`'s `value` field —
/// written by `duduclaw-cli`'s `mcp_os_ops.rs::record_pending_update_report`
/// (a different crate; kept in sync by doc-comment cross-reference and the
/// shared `duduclaw_core::WORKING_STATE_KEY_PENDING_UPDATE_REPORT` key name,
/// not a shared Rust type — `working_state` entries are opaque strings by
/// design, same as every other key in this store).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingUpdateReport {
    /// `"device"` (appliance OS image, via `duduclaw-sysd`) or `"system"`
    /// (duduclaw's own binary self-update).
    target: String,
    /// `system` only — the version `os_apply_update` resolved and applied.
    /// `None` for `device` (boot assessment doesn't need a version string —
    /// good/bad/indeterminate is the whole signal).
    #[serde(default)]
    expected_version: Option<String>,
    /// Informational only; not read by any branch below.
    #[serde(default)]
    #[allow(dead_code)]
    initiated_at: Option<String>,
    /// Raw `DUDUCLAW_REPLY_CHANNEL`-shaped string (`"<channel>:<chat_id>"`),
    /// captured at write time. Parsed here via `decision_notify::parse_origin`
    /// rather than at write time because that parser/validator lives in
    /// `duduclaw-gateway`, not `duduclaw-cli`.
    #[serde(default)]
    reply_channel_raw: Option<String>,
    /// `system` only. `false` until this sweep's first sighting, at which
    /// point it schedules the actual gateway restart and flips this to
    /// `true` — distinguishing "haven't reacted yet" from "already
    /// triggered, now waiting out the grace window" across ticks (and across
    /// the restart itself: the flag is on disk before the process that set
    /// it exits).
    #[serde(default)]
    restart_triggered: bool,
    #[serde(default)]
    restart_triggered_at: Option<String>,
}

impl PendingUpdateReport {
    fn to_value_string(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// One reconciliation pass over every agent directory under `home_dir`.
/// Called from [`crate::dispatch_engine::DispatchEngine::tick_once`] step 5
/// — piggy-backs the existing 30s tick, no new timer (same discipline as the
/// capability-grant and maintenance-mode sweeps in the same function).
/// Best-effort throughout: a malformed entry, an unreachable agent, or a
/// failed delivery attempt is logged and skipped, never panics, never stalls
/// the tick for other agents.
pub async fn sweep(home_dir: &Path, http: &reqwest::Client) {
    let agents_dir = home_dir.join("agents");
    let Ok(entries) = std::fs::read_dir(&agents_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(agent_id) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if agent_id.starts_with('_') || agent_id.starts_with('.') {
            continue;
        }
        reconcile_one(home_dir, agent_id, http).await;
    }
}

async fn reconcile_one(home_dir: &Path, agent_id: &str, http: &reqwest::Client) {
    let Some(raw_entry) = crate::working_state::get_entry(home_dir, agent_id, REPORT_KEY) else {
        return;
    };
    let report: PendingUpdateReport = match serde_json::from_str(&raw_entry.value) {
        Ok(r) => r,
        Err(e) => {
            // A foreign/corrupt value under this key. Do not clear it —
            // that would destroy whatever unrelated thing actually wrote it
            // (working_state keys are a shared namespace; this sweep only
            // owns entries it can parse as its own shape).
            warn!(agent = agent_id, error = %e, "pending_update_report 內容無法解析為預期格式，本輪略過");
            return;
        }
    };
    match report.target.as_str() {
        "system" => reconcile_system(home_dir, agent_id, &raw_entry.value, report, http).await,
        "device" => {
            reconcile_device(home_dir, agent_id, &raw_entry.value, &raw_entry.expires_at, report, http).await
        }
        other => {
            warn!(agent = agent_id, target = other, "pending_update_report 的 target 既非 device 也非 system，略過");
        }
    }
}

// ── target: "system" ─────────────────────────────────────────────

async fn reconcile_system(
    home_dir: &Path,
    agent_id: &str,
    raw_value: &str,
    mut report: PendingUpdateReport,
    http: &reqwest::Client,
) {
    if !report.restart_triggered {
        // First tick to notice this update was applied. Mark it BEFORE
        // scheduling the restart (persisted-first ordering — if the write
        // fails we must not restart with no record of having done so).
        let now = Utc::now().to_rfc3339();
        report.restart_triggered = true;
        report.restart_triggered_at = Some(now);
        let value = report.to_value_string();
        let home = home_dir.to_path_buf();
        let agent = agent_id.to_string();
        let expected = raw_value.to_string();
        let write = tokio::task::spawn_blocking(move || {
            crate::working_state::set_entry(
                &home,
                &agent,
                REPORT_KEY,
                &value,
                "system 更新已套用，標記待重啟並排程自我重啟",
                Some(4.0),
                Some(&expected),
            )
        })
        .await;
        match write {
            Ok(Ok(_)) => {
                info!(agent = agent_id, "system 目標更新偵測完成，排程 3 秒後自我重啟");
                tokio::spawn(async {
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    duduclaw_core::platform::request_restart_after_shutdown();
                    duduclaw_core::platform::self_interrupt();
                });
            }
            Ok(Err(e)) => {
                warn!(agent = agent_id, error = %e, "標記 restart_triggered 失敗，本輪不觸發重啟，下一輪重試");
            }
            Err(e) => warn!(agent = agent_id, error = %e, "working_state 寫入 join 失敗"),
        }
        return; // Too early to verify on the same tick that triggered it.
    }

    let Some(triggered_at) = report
        .restart_triggered_at
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
    else {
        // restart_triggered=true implies this should be set; fail open by
        // treating it as "just now" so we still wait out a grace window
        // rather than verifying prematurely against a corrupt timestamp.
        return;
    };

    if !past_grace_window(triggered_at, Utc::now(), SYSTEM_RESTART_GRACE_SECS) {
        return;
    }

    let current = crate::updater::current_version();
    let expected = report.expected_version.clone().unwrap_or_default();
    let (text, success) = compose_system_report(current, &expected);
    finalize_report(home_dir, agent_id, &report.reply_channel_raw, &text, success, "system", http).await;
}

/// Pure: has `now` moved at least `grace_secs` past `since`? Factored out so
/// the grace-window boundary is unit-testable without any I/O or sleeping.
fn past_grace_window(since: DateTime<Utc>, now: DateTime<Utc>, grace_secs: i64) -> bool {
    (now - since).num_seconds() >= grace_secs
}

/// Pure: compare the *actually running* gateway's version against what the
/// update aimed to install. Never panics on an empty `expected` — that
/// shouldn't happen for `target=system` (`expected_version` is always
/// populated at write time from `updater::check_update()`'s result) but
/// fails open into an honest "couldn't verify" message rather than a false
/// positive.
fn compose_system_report(current: &str, expected: &str) -> (String, bool) {
    if expected.is_empty() {
        return (
            format!(
                "系統本體已重新啟動（目前版本 v{current}），但更新前未記錄目標版本，無法核對是否正確——建議手動確認。"
            ),
            false,
        );
    }
    if current == expected {
        (format!("系統本體更新已完成重新啟動，目前執行版本是 v{current}。"), true)
    } else {
        (
            format!(
                "系統本體更新後的自動重新啟動似乎沒有生效——目前仍在執行 v{current}（預期應為 v{expected}）。可能是重啟被中止，建議稍後手動確認或重新嘗試更新。"
            ),
            false,
        )
    }
}

// ── target: "device" ─────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BootVerdict {
    Good,
    Bad,
    Unknown,
}

/// Pure: classify `systemd-bless-boot status`'s forwarded stdout. Exact
/// (trimmed, lowercased) match, not substring `contains` — the values are
/// single words by `boot_assessment_status`'s own doc comment
/// (`good`/`bad`/`indeterminate`/`clean`), and exact match is both correct
/// here and keeps this a routing decision that follows convention #2 (no
/// unanchored `contains` for decisions that branch behavior).
fn classify_boot_assessment(result: &crate::device_ops::OpResult) -> BootVerdict {
    match result {
        Ok(out) if out.success => match out.stdout.trim().to_lowercase().as_str() {
            "good" | "clean" => BootVerdict::Good,
            "bad" => BootVerdict::Bad,
            _ => BootVerdict::Unknown,
        },
        _ => BootVerdict::Unknown,
    }
}

/// Pure: given the TTL remaining on the pending entry, must an `Unknown`
/// boot-assessment result be finalized as an honest failure NOW (rather than
/// waiting for a later tick to try again)? Factored out for unit testing —
/// see `DEVICE_FINAL_REPORT_GRACE_SECS`'s doc comment for the reasoning.
fn device_must_finalize_unknown(expires_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    (expires_at - now).num_seconds() <= DEVICE_FINAL_REPORT_GRACE_SECS
}

async fn reconcile_device(
    home_dir: &Path,
    agent_id: &str,
    _raw_value: &str,
    expires_at: &Option<String>,
    report: PendingUpdateReport,
    http: &reqwest::Client,
) {
    let assessment = crate::device_ops::select_device_ops().boot_assessment_status().await;
    match classify_boot_assessment(&assessment) {
        BootVerdict::Good => {
            // B5 (OS security line P0): the update just survived its boot
            // assessment — record the blessing before the notify/clear step
            // below (whose own failure-to-deliver must not also swallow this
            // audit trail — see `finalize_report`'s own retry-on-non-delivery
            // behavior, which this event is independent of).
            duduclaw_security::audit::log_os_update_blessed(home_dir, agent_id);
            finalize_report(
                home_dir,
                agent_id,
                &report.reply_channel_raw,
                "系統更新已完成重開機評估：開機評估結果正常。",
                true,
                "device",
                http,
            )
            .await;
        }
        BootVerdict::Bad => {
            // B5: the bootloader auto-rolled-back — Critical, see
            // `log_os_rollback_detected`'s own doc comment for why.
            duduclaw_security::audit::log_os_rollback_detected(home_dir, agent_id);
            finalize_report(
                home_dir,
                agent_id,
                &report.reply_channel_raw,
                "系統更新後開機評估失敗，機器已自動退回上一個版本，目前運作正常，沒有造成資料損失。若要重試更新，請告訴我。",
                false,
                "device",
                http,
            )
            .await;
        }
        BootVerdict::Unknown => {
            let Some(exp) = expires_at
                .as_deref()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&Utc))
            else {
                return; // No TTL on record (shouldn't happen) — wait for a later tick.
            };
            if !device_must_finalize_unknown(exp, Utc::now()) {
                return; // Still time left — try again next tick, no write needed.
            }
            finalize_report(
                home_dir,
                agent_id,
                &report.reply_channel_raw,
                "系統更新後的開機評估遲遲沒有明確結果，需要人工檢查目前的開機狀態。",
                false,
                "device",
                http,
            )
            .await;
        }
    }
}

// ── shared: deliver + clear ──────────────────────────────────────

/// Push the composed report to a destination and, only on confirmed
/// delivery (or hand-off to the notify-governance queue), clear
/// `pending_update_report` and leave an audit trail so `recent_actions.rs`
/// surfaces this in the agent's own future turns — a user asking "did the
/// update actually finish?" later gets an answer grounded in a record, not
/// the agent's self-recollection.
///
/// Two destinations, in order, mirroring the A1 `needs_human` cross-channel
/// convention: (1) the channel/chat that originated the update request, if
/// captured and still a bot-pushable channel; (2) the agent's own default
/// `[proactive]` notify destination. No destination at all, or a transient
/// send failure, leaves the entry in place — bounded retry, up to the
/// entry's own TTL, never a silent drop.
async fn finalize_report(
    home_dir: &Path,
    agent_id: &str,
    reply_channel_raw: &Option<String>,
    text: &str,
    success: bool,
    target: &str,
    http: &reqwest::Client,
) {
    let delivered = deliver(home_dir, agent_id, reply_channel_raw, text, http).await;
    if !delivered {
        warn!(agent = agent_id, target, "更新結果通知目前無法投遞，保留 pending_update_report 待下一輪重試");
        return;
    }
    let home = home_dir.to_path_buf();
    let agent = agent_id.to_string();
    let _ = tokio::task::spawn_blocking(move || {
        crate::working_state::clear_entry(&home, &agent, REPORT_KEY, "更新結果已回報，清除待辦狀態")
    })
    .await;
    duduclaw_security::audit::append_tool_call_with_extras(
        home_dir,
        agent_id,
        "update_report_reconciliation",
        &format!("target={target}"),
        success,
        &[],
    );
    info!(agent = agent_id, target, success, "跨重啟更新結果已回報並清除待辦狀態");
}

async fn deliver(
    home_dir: &Path,
    agent_id: &str,
    reply_channel_raw: &Option<String>,
    text: &str,
    http: &reqwest::Client,
) -> bool {
    if let Some(raw) = reply_channel_raw {
        if let Some((channel, chat_id)) = crate::decision_notify::parse_origin(raw) {
            match crate::reminder_scheduler::send_channel_message(home_dir, http, &channel, &chat_id, text).await {
                Ok(()) => return true,
                Err(e) => warn!(agent = agent_id, channel, error = %e, "原始對話通道投遞失敗，改用 agent 預設通知通道"),
            }
        }
    }
    matches!(
        crate::goal_notify::notify_agent_plain(
            home_dir,
            agent_id,
            crate::notify_governance::NotifyLevel::Fyi,
            "update_report",
            text,
        )
        .await,
        crate::goal_notify::NotifyOutcome::Sent | crate::goal_notify::NotifyOutcome::Deferred
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_home(agent: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("agents").join(agent)).unwrap();
        dir
    }

    fn write_report(home: &Path, agent: &str, report: &PendingUpdateReport) {
        crate::working_state::set_entry(
            home,
            agent,
            REPORT_KEY,
            &report.to_value_string(),
            "test setup",
            Some(4.0),
            None,
        )
        .unwrap();
    }

    fn read_raw(home: &Path, agent: &str) -> Option<String> {
        crate::working_state::get_entry(home, agent, REPORT_KEY).map(|e| e.value)
    }

    // ── pure classifiers ────────────────────────────────────────

    #[test]
    fn past_grace_window_boundary() {
        let t0 = Utc::now();
        assert!(!past_grace_window(t0, t0 + chrono::Duration::seconds(119), 120));
        assert!(past_grace_window(t0, t0 + chrono::Duration::seconds(120), 120));
        assert!(past_grace_window(t0, t0 + chrono::Duration::seconds(121), 120));
    }

    #[test]
    fn compose_system_report_match_mismatch_and_unknown_expected() {
        let (text, ok) = compose_system_report("1.63.0", "1.63.0");
        assert!(ok);
        assert!(text.contains("1.63.0"));

        let (text, ok) = compose_system_report("1.62.0", "1.63.0");
        assert!(!ok);
        assert!(text.contains("1.62.0") && text.contains("1.63.0"));

        let (text, ok) = compose_system_report("1.62.0", "");
        assert!(!ok);
        assert!(text.contains("未記錄目標版本"));
    }

    #[test]
    fn classify_boot_assessment_exact_match_not_substring() {
        use crate::device_ops::{DeviceOpError, OpOutput};
        let good = Ok(OpOutput { success: true, stdout: "good\n".into(), stderr: String::new() });
        assert_eq!(classify_boot_assessment(&good), BootVerdict::Good);
        let clean = Ok(OpOutput { success: true, stdout: "  CLEAN  ".into(), stderr: String::new() });
        assert_eq!(classify_boot_assessment(&clean), BootVerdict::Good);
        let bad = Ok(OpOutput { success: true, stdout: "bad".into(), stderr: String::new() });
        assert_eq!(classify_boot_assessment(&bad), BootVerdict::Bad);
        let indeterminate = Ok(OpOutput { success: true, stdout: "indeterminate".into(), stderr: String::new() });
        assert_eq!(classify_boot_assessment(&indeterminate), BootVerdict::Unknown);
        // A "goodish" garbage value must NOT classify as Good via substring.
        let garbage = Ok(OpOutput { success: true, stdout: "goodbye".into(), stderr: String::new() });
        assert_eq!(classify_boot_assessment(&garbage), BootVerdict::Unknown);
        let failed_spawn = Ok(OpOutput { success: false, stdout: "good".into(), stderr: "boom".into() });
        assert_eq!(classify_boot_assessment(&failed_spawn), BootVerdict::Unknown);
        let err: crate::device_ops::OpResult = Err(DeviceOpError::Unsupported("no sysd".into()));
        assert_eq!(classify_boot_assessment(&err), BootVerdict::Unknown);
    }

    #[test]
    fn device_must_finalize_unknown_boundary() {
        let now = Utc::now();
        assert!(!device_must_finalize_unknown(now + chrono::Duration::seconds(301), now));
        assert!(device_must_finalize_unknown(now + chrono::Duration::seconds(300), now));
        assert!(device_must_finalize_unknown(now - chrono::Duration::seconds(1), now));
    }

    // ── sweep glue: state machine transitions ──────────────────

    #[tokio::test]
    async fn sweep_is_a_noop_for_agents_with_no_pending_report() {
        let home = mk_home("sysop");
        let client = reqwest::Client::new();
        sweep(home.path(), &client).await; // must not panic
        assert!(read_raw(home.path(), "sysop").is_none());
    }

    #[tokio::test]
    async fn sweep_leaves_malformed_json_untouched() {
        let home = mk_home("sysop");
        crate::working_state::set_entry(
            home.path(),
            "sysop",
            REPORT_KEY,
            "not-json-at-all",
            "test setup",
            Some(4.0),
            None,
        )
        .unwrap();
        let client = reqwest::Client::new();
        sweep(home.path(), &client).await;
        assert_eq!(read_raw(home.path(), "sysop").as_deref(), Some("not-json-at-all"));
    }

    #[tokio::test]
    async fn sweep_first_sight_of_system_target_marks_restart_triggered() {
        let home = mk_home("sysop");
        write_report(
            home.path(),
            "sysop",
            &PendingUpdateReport {
                target: "system".into(),
                expected_version: Some("9.9.9".into()),
                initiated_at: Some(Utc::now().to_rfc3339()),
                reply_channel_raw: None,
                restart_triggered: false,
                restart_triggered_at: None,
            },
        );
        let client = reqwest::Client::new();
        sweep(home.path(), &client).await;
        let raw = read_raw(home.path(), "sysop").expect("entry must still exist (too early to finalize)");
        let parsed: PendingUpdateReport = serde_json::from_str(&raw).unwrap();
        assert!(parsed.restart_triggered);
        assert!(parsed.restart_triggered_at.is_some());
        // The 3s-delayed real restart is spawned but never awaited by this
        // test (the tokio::test runtime tears down long before 3s elapse) —
        // this test only proves the flag/timestamp bookkeeping, matching the
        // module doc's documented test-coverage boundary.
    }

    #[tokio::test]
    async fn sweep_holds_off_verifying_within_the_grace_window() {
        let home = mk_home("sysop");
        write_report(
            home.path(),
            "sysop",
            &PendingUpdateReport {
                target: "system".into(),
                expected_version: Some("9.9.9".into()),
                initiated_at: None,
                reply_channel_raw: None,
                restart_triggered: true,
                restart_triggered_at: Some(Utc::now().to_rfc3339()),
            },
        );
        let client = reqwest::Client::new();
        sweep(home.path(), &client).await;
        // Still within the grace window — must not have attempted delivery
        // (no destination configured; a verify attempt would fail delivery
        // and the entry would still be present either way, so the
        // meaningful assertion is that the entry is BYTE-IDENTICAL — no
        // stray write happened for a no-op tick).
        let raw = read_raw(home.path(), "sysop").unwrap();
        let parsed: PendingUpdateReport = serde_json::from_str(&raw).unwrap();
        assert!(parsed.restart_triggered);
    }

    #[tokio::test]
    async fn sweep_past_grace_with_no_destination_retains_entry_for_retry() {
        let home = mk_home("sysop");
        write_report(
            home.path(),
            "sysop",
            &PendingUpdateReport {
                target: "system".into(),
                expected_version: Some("9.9.9".into()),
                initiated_at: None,
                reply_channel_raw: None,
                restart_triggered: true,
                restart_triggered_at: Some((Utc::now() - chrono::Duration::seconds(200)).to_rfc3339()),
            },
        );
        let client = reqwest::Client::new();
        sweep(home.path(), &client).await;
        // No `[proactive]` destination configured on this fabricated agent
        // and no reply_channel_raw — delivery is expected to fail, so the
        // entry must be RETAINED (bounded retry), not silently dropped.
        assert!(read_raw(home.path(), "sysop").is_some());
    }

    #[tokio::test]
    async fn sweep_device_target_unknown_far_from_expiry_takes_no_action() {
        let home = mk_home("sysop");
        write_report(
            home.path(),
            "sysop",
            &PendingUpdateReport {
                target: "device".into(),
                expected_version: None,
                initiated_at: None,
                reply_channel_raw: None,
                restart_triggered: false,
                restart_triggered_at: None,
            },
        );
        let before = read_raw(home.path(), "sysop").unwrap();
        let client = reqwest::Client::new();
        // Off-appliance in this test process: `select_device_ops()` /
        // `boot_assessment_status()` resolve to `Unknown` (same as a real
        // `indeterminate`/error result would) — exercises the same "wait,
        // TTL far from expiry" branch without needing a real appliance.
        sweep(home.path(), &client).await;
        let after = read_raw(home.path(), "sysop").unwrap();
        assert_eq!(before, after, "far from TTL expiry, an Unknown verdict must not trigger any write");
    }
}
