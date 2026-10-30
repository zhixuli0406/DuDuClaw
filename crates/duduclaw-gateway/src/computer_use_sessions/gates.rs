//! The per-employee tool gates for the computer-use route (security review
//! F1c/F1d). The MCP dispatcher enforces these for every tool, but the route
//! can be called without passing through it (the internal key and the
//! identity token sit in the employee's own MCP registration), so the
//! gateway enforces them again for each op, mapping the op to its tool name:
//!
//! - `[capabilities] denied_tools` / `allowed_tools` — the shared
//!   [`duduclaw_core::tool_catalog::tool_list_verdict`] decision;
//! - `[capabilities] scoped_tools` — an active PORTICO grant
//!   ([`crate::capability_grants`]) is required for a listed tool;
//! - `approval_required_tools`, `irreversible_tools` and
//!   `maybe_irreversible_tools` — a human decision through the
//!   [`ApprovalBroker`] (no LLM judge here: a maybe-irreversible computer
//!   tool is always asked about). The MCP-side approval gate skips the eight
//!   `computer_*` tools so nobody is asked twice;
//! - `[capabilities] action_rules` (2026-10) — the same resolution the MCP
//!   gate uses ([`duduclaw_core::ActionRules::resolve`] over
//!   [`duduclaw_core::agent_toml::load_action_rules`]): `block` refuses
//!   (`action_rule`), `ask` joins the approval lists above.
//!
//! Every gate fails closed. Refusals carry operator-safe messages and the
//! `forbidden` / `approval_denied` codes; tool-gate refusals write the same
//! `tool_calls.jsonl` denial row the MCP gate writes.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;
use tracing::warn;

use crate::approval::{ApprovalBroker, ApprovalStatus};
use crate::capability_grants;

use super::{ErrorCode, OpError};

/// `computer_session_start`.
pub const TOOL_START: &str = "computer_session_start";
/// `computer_screenshot`.
pub const TOOL_SCREENSHOT: &str = "computer_screenshot";
/// `computer_click`.
pub const TOOL_CLICK: &str = "computer_click";
/// `computer_type`.
pub const TOOL_TYPE: &str = "computer_type";
/// `computer_key`.
pub const TOOL_KEY: &str = "computer_key";
/// `computer_scroll`.
pub const TOOL_SCROLL: &str = "computer_scroll";
/// `computer_session_stop`.
pub const TOOL_STOP: &str = "computer_session_stop";
/// `computer_navigate`.
pub const TOOL_NAVIGATE: &str = "computer_navigate";
/// `computer_workspace_list`.
pub const TOOL_WS_LIST: &str = "computer_workspace_list";
/// `computer_workspace_read`.
pub const TOOL_WS_READ: &str = "computer_workspace_read";
/// `computer_workspace_write`.
pub const TOOL_WS_WRITE: &str = "computer_workspace_write";

/// How long a computer-tool approval waits for a human.
pub const APPROVAL_TTL_SECS: i64 = 300;
/// Poll interval while waiting for that decision.
pub const APPROVAL_POLL: Duration = Duration::from_secs(2);

/// The employee's directory (`.ephemeral/` role members included).
pub(super) fn agent_dir(home: &Path, agent_id: &str) -> PathBuf {
    crate::ephemeral::resolve_agent_dir(home, agent_id)
        .unwrap_or_else(|| home.join("agents").join(agent_id))
}

fn forbidden(message: String) -> OpError {
    OpError::new(ErrorCode::Forbidden, message)
}

/// Write the MCP gate's denial row (`append_tool_call_denied`, no input) off
/// the async runtime.
async fn audit_denial(home: &Path, agent_id: &str, tool: &'static str, class: &'static str, detail: &str) {
    let (home, agent, detail) = (home.to_path_buf(), agent_id.to_string(), detail.to_string());
    let _ = tokio::task::spawn_blocking(move || {
        duduclaw_security::audit::append_tool_call_denied(&home, &agent, tool, class, &detail, None);
    })
    .await;
}

/// `denied_tools` / `allowed_tools` and `scoped_tools` for `tool`.
pub(super) async fn tool_gates(home: &Path, agent_id: &str, tool: &'static str) -> Result<(), OpError> {
    use duduclaw_core::tool_catalog::{ToolListVerdict, tool_list_verdict};

    let dir = agent_dir(home, agent_id);
    let caps = duduclaw_core::agent_toml::load(&dir).capabilities;
    let refusal = match tool_list_verdict(tool, &caps.denied_tools, &caps.allowed_tools) {
        ToolListVerdict::Allowed => None,
        ToolListVerdict::Denied => Some((
            "denied_tools",
            format!("工具「{tool}」已被此員工的 [capabilities] denied_tools 設定阻擋，已拒絕執行。"),
        )),
        ToolListVerdict::NotAllowlisted => Some((
            "allowed_tools",
            format!("工具「{tool}」不在此員工的 [capabilities] allowed_tools 允許清單中，已拒絕執行。"),
        )),
    };
    if let Some((class, message)) = refusal {
        audit_denial(home, agent_id, tool, class, &message).await;
        return Err(forbidden(message));
    }

    if action_rule_verdict(&dir, tool) == Some(duduclaw_core::ActionVerdict::Block) {
        let message = format!(
            "工具「{tool}」（{} 類動作）已被此員工的 [capabilities] action_rules 設定阻擋，已拒絕執行。",
            duduclaw_core::effect_of(tool)
        );
        audit_denial(home, agent_id, tool, "action_rule", &message).await;
        return Err(forbidden(message));
    }

    let scoped = capability_grants::scoped_tools(&dir);
    if capability_grants::set_contains_tool(&scoped, tool) {
        let granted = match capability_grants::CapabilityGrantStore::open(home) {
            Ok(store) => store.has_active_grant(agent_id, tool).await,
            Err(e) => {
                warn!(agent = %agent_id, tool, error = %e, "capability grant store unavailable — denying scoped computer tool (fail-closed)");
                false
            }
        };
        if !granted {
            let message = format!(
                "工具「{tool}」為階段性授權工具，目前無有效授權。請先呼叫 capability_request（附 tool 與 reason）取得人工核准後再執行。"
            );
            audit_denial(home, agent_id, tool, "capability_grant_missing", &message).await;
            return Err(forbidden(message));
        }
    }
    Ok(())
}

/// The employee's `[capabilities] action_rules` verdict for `tool`.
fn action_rule_verdict(dir: &Path, tool: &str) -> Option<duduclaw_core::ActionVerdict> {
    duduclaw_core::agent_toml::load_action_rules(dir).resolve(tool, duduclaw_core::effect_of(tool))
}

/// Whether `tool` is in any of the employee's approval lists, or an action
/// rule asks (or blocks) it.
pub(super) fn approval_required(home: &Path, agent_id: &str, tool: &str) -> bool {
    let dir = agent_dir(home, agent_id);
    crate::approval::tool_requires_approval(&dir, tool)
        || crate::approval::tool_is_irreversible(&dir, tool)
        || crate::approval::tool_is_maybe_irreversible(&dir, tool)
        || action_rule_verdict(&dir, tool).is_some_and(|v| v >= duduclaw_core::ActionVerdict::Ask)
}

/// Route callers share the gateway ledger, including calls that bypass MCP.
#[cfg(test)]
pub(super) fn egress_check(home: &Path, agent: &str, tool: &str, turn: Option<&str>) -> Result<Option<serde_json::Value>, OpError> {
    egress_check_with_call(home, agent, tool, turn, &uuid::Uuid::new_v4().to_string())
}

pub(super) fn egress_check_with_call(home: &Path, agent: &str, tool: &str, turn: Option<&str>, call_id: &str) -> Result<Option<serde_json::Value>, OpError> {
    use duduclaw_core::egress_guard::{effective_mode_checked, Mode};
    if !matches!(tool, TOOL_NAVIGATE | TOOL_TYPE) { return Ok(None); }
    let mode = effective_mode_checked(home, Some(&agent_dir(home, agent))).map_err(forbidden)?;
    if mode == Mode::Off { return Ok(None); }
    let taint = crate::egress_guard::query_turn(agent, turn);
    if !taint.tainted { return Ok(None); }
    match mode {
        Mode::Off => Ok(None),
        Mode::Shadow => {
            let kind = if taint.sources.iter().any(|s| s == "turn_state_unavailable") { "would_refuse_ledger_unavailable" } else { "would_ask" };
            if crate::egress_shadow::record(home, agent, tool, kind, &taint.sources, call_id, crate::egress_shadow::Transport::Mcp) {
                duduclaw_security::audit::append_tool_call_with_extras(home, agent, tool,
                    "egress_guard: would ask before sending private data", true,
                    &[("egress_guard", json!(kind)), ("call_id", json!(call_id)), ("sources", json!(taint.sources))]);
            }
            Ok(None)
        }
        Mode::Ask => Ok(Some(json!({"reason": "egress_guard", "sources": taint.sources}))),
        Mode::Block => Err(forbidden(format!("外送防護已阻擋工具「{tool}」：本回合已讀取私人資料（來源：{}）。", taint.sources.join("、")))),
    }
}

fn approval_denied(message: String) -> OpError {
    OpError::new(ErrorCode::ApprovalDenied, message)
}

/// P3 action review (`config.toml [action_review] mode`, default off) for a
/// computer tool every list gate let run without a person: the same review
/// the MCP gate runs for other tools, which skips the `computer_*` tools
/// because the gateway owns them. `Ok(false)` runs, `Ok(true)` needs a
/// person's approval (`ask`, or no verdict under `enforce` — fail closed),
/// `Err` refuses (`block` under `enforce`). Only argument keys reach the
/// reviewer. One `tool_calls.jsonl` row per review.
pub(super) async fn action_review(
    home: &Path,
    agent_id: &str,
    tool: &'static str,
    args: &serde_json::Value,
) -> Result<bool, OpError> {
    use crate::action_review::{self as ar, ActionReviewMode, ReviewOutcome};
    if !duduclaw_core::effect_of(tool).is_side_effecting() {
        return Ok(false);
    }
    let mode = ActionReviewMode::from_home(home);
    if mode == ActionReviewMode::Off {
        return Ok(false);
    }
    let dir = agent_dir(home, agent_id);
    let input = ar::ReviewInput::build(tool, &json!({ "arguments": args }), &dir);
    let verdict = ar::review(home, &dir, &input).await;
    let result = ar::outcome(mode, verdict);
    {
        let (home, agent) = (home.to_path_buf(), agent_id.to_string());
        let audit = ar::audit_value(verdict);
        let mode_s = mode.as_str();
        let ok = result == ReviewOutcome::Run;
        let _ = tokio::task::spawn_blocking(move || {
            duduclaw_security::audit::append_tool_call_with_extras(
                &home,
                &agent,
                tool,
                &format!("action review ({mode_s}) → {audit}"),
                ok,
                &[
                    ("action_review", serde_json::Value::String(audit.to_string())),
                    ("action_review_mode", serde_json::Value::String(mode_s.to_string())),
                ],
            );
        })
        .await;
    }
    match result {
        ReviewOutcome::Run => Ok(false),
        ReviewOutcome::AskPerson => Ok(true),
        ReviewOutcome::Refuse => Err(forbidden(format!(
            "工具「{tool}」的呼叫經動作審查（[action_review] enforce）判定會越過此員工的界線，已拒絕執行。"
        ))),
    }
}

/// Ask a human through the [`ApprovalBroker`] (`action_kind = "mcp_call"`)
/// and wait for the decision. `detail` is gateway-built text (never
/// agent-controlled free text). Anything but an approval refuses.
///
/// P7 (2026-10-08 close-out): the request is a single-use
/// [`ActionGrant`](crate::approval::action_grant::ActionGrant) bound to this
/// employee, tool and `args` (a gateway-built snapshot of the operation:
/// hashes for typed text and URLs, never the raw text); on approval the
/// binding is checked against the call and consumed once before it runs.
pub(super) async fn obtain_approval(
    home: &Path,
    agent_id: &str,
    tool: &'static str,
    detail: &str,
    args: &serde_json::Value,
    egress: Option<&serde_json::Value>,
    ttl_secs: i64,
    poll: Duration,
) -> Result<(), OpError> {
    use crate::approval::action_grant::{ACTION_GRANT_KEY, ActionGrant, GrantRefusal, verify_and_consume};
    let broker = match ApprovalBroker::open(home) {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "ApprovalBroker unavailable — denying computer tool (fail-closed)");
            return Err(approval_denied(format!(
                "審批系統暫時無法使用，工具「{tool}」的呼叫已拒絕。請稍後再試或由管理員手動處理。"
            )));
        }
    };
    let call = json!({ "arguments": args });
    let grant = ActionGrant::build(agent_id, tool, &call);
    let summary = format!(
        "AI 員工 {agent_id} 要使用電腦操作工具「{tool}」{detail}；此工具列在審批清單中，需經管理員核可後才能執行。\n\n{}",
        grant.card_text()
    );
    let mut payload = json!({
        "tool": tool,
        "via": "computer_use_route",
        ACTION_GRANT_KEY: grant.to_json(),
    });
    let matches = duduclaw_core::agent_toml::load_action_rules(&agent_dir(home, agent_id))
        .approval_matches_for_call(tool, duduclaw_core::effect_of(tool), args);
    if !matches.is_empty() {
        payload["action_rule_matches"] = json!(matches);
        payload["reason"] = json!("action_rule");
    }
    if let Some(egress) = egress {
        payload["egress_guard"] = egress.clone();
        payload["arguments"] = args.clone();
    }
    let id = match broker.request(agent_id, "mcp_call", &summary, payload, ttl_secs).await {
        Ok(id) => id,
        Err(e) => {
            warn!(error = %e, "computer tool approval request failed — denying (fail-closed)");
            return Err(approval_denied(format!(
                "審批系統無法建立審核請求，工具「{tool}」的呼叫已拒絕。"
            )));
        }
    };
    match broker.await_decision(&id, poll).await {
        Ok(ApprovalStatus::Approved) => match verify_and_consume(&broker, &id, &grant).await {
            Ok(()) => Ok(()),
            Err(refusal) => {
                warn!(approval = %id, ?refusal, "computer tool approval not usable — denying (fail-closed)");
                let what = match refusal {
                    GrantRefusal::Mismatch => "核准內容與這次呼叫不符",
                    GrantRefusal::AlreadyUsed | GrantRefusal::NotApproved => "這份核准已被使用或已失效",
                    GrantRefusal::Store(_) => "無法確認核准內容",
                };
                Err(approval_denied(format!("{what}，工具「{tool}」的呼叫已拒絕。")))
            }
        },
        Ok(ApprovalStatus::Denied | ApprovalStatus::Answered | ApprovalStatus::Invalidated) =>
            Err(approval_denied(format!(
            "工具「{tool}」的呼叫已被管理員拒絕（審核編號 {id}）。"
        ))),
        Ok(ApprovalStatus::Expired) => Err(approval_denied(format!(
            "工具「{tool}」的呼叫逾時未核可，已自動拒絕（審核編號 {id}）。"
        ))),
        Ok(ApprovalStatus::Pending) => Err(approval_denied(format!(
            "審核狀態異常（仍為待審），工具「{tool}」的呼叫已拒絕。"
        ))),
        Err(e) => {
            warn!(error = %e, "computer tool await_decision failed — denying (fail-closed)");
            Err(approval_denied(format!(
                "等待審核決定時發生錯誤，工具「{tool}」的呼叫已拒絕。"
            )))
        }
    }
}


#[cfg(test)]
mod egress_tests {
    use super::*;

    #[test]
    fn route_egress_reads_the_gateway_ledger_and_strict_modes() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let agent = "computer-egress-test";
        let dir = home.join("agents").join(agent);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), "").unwrap();
        std::fs::write(home.join("config.toml"), "[security]\negress_guard='ask'").unwrap();
        let metadata = egress_check(home, agent, TOOL_NAVIGATE, None).unwrap().unwrap();
        assert_eq!(metadata["reason"], "egress_guard");
        assert_eq!(metadata["sources"], json!(["turn_state_unavailable"]));
        assert!(egress_check(home, agent, TOOL_CLICK, None).unwrap().is_none());
        let turn = uuid::Uuid::new_v4().to_string();
        crate::egress_guard::register_turn(agent, &turn);
        assert!(egress_check(home, agent, TOOL_TYPE, Some(&turn)).unwrap().is_none());
        std::fs::write(home.join("config.toml"), "[security]\negress_guard='block'").unwrap();
        assert!(egress_check(home, agent, TOOL_TYPE, None).unwrap_err().message.contains("turn_state_unavailable"));
        std::fs::write(home.join("config.toml"), "[security]\negress_guard='off'").unwrap();
        assert!(egress_check(home, agent, TOOL_TYPE, None).unwrap().is_none());
    }
    #[tokio::test]
    async fn execution_turn_keeps_computer_egress_separate_from_provenance() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let agent = "computer-egress-execution";
        let dir = home.join("agents").join(agent);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), "").unwrap();
        std::fs::write(home.join("config.toml"), "[security]\negress_guard='block'").unwrap();
        duduclaw_memory::feedback::CURRENT_TURN_ID.scope(Some("persisted-upstream".into()),
            crate::runtime_turn::scope(agent, async {
                let turn = crate::runtime_turn::current().unwrap();
                assert!(egress_check(home, agent, TOOL_TYPE, Some(&turn)).unwrap().is_none());
                assert!(egress_check(home, agent, TOOL_TYPE, Some("persisted-upstream")).is_err());
                crate::egress_guard::mark_turn(agent, Some(&turn), "memory_search");
                assert!(egress_check(home, agent, TOOL_TYPE, Some(&turn)).is_err());
            }),
        ).await;
    }

}

#[cfg(test)]
mod shadow_tests {
    use super::*;

    #[test]
    fn computer_precheck_and_recheck_count_one_call() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"), "[security]\negress_guard='shadow'\n").unwrap();
        let agent = format!("shadow-{}",uuid::Uuid::new_v4());
        crate::egress_guard::register_turn(&agent,"turn");
        crate::egress_guard::mark_turn(&agent,Some("turn"),"memory_search");
        let call = uuid::Uuid::new_v4().to_string();
        for _ in 0..2 { egress_check_with_call(home.path(),&agent,TOOL_NAVIGATE,Some("turn"),&call).unwrap(); }
        let report = crate::egress_shadow::report(home.path(),7).unwrap();
        assert_eq!((report.total,report.ticket_total),(1,1));
        assert_eq!(std::fs::read_to_string(home.path().join("tool_calls.jsonl")).unwrap().lines().count(),1);
    }
}
