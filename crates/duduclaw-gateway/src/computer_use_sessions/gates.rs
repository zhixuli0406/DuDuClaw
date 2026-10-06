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
//!   `computer_*` tools so nobody is asked twice.
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

/// Whether `tool` is in any of the employee's approval lists.
pub(super) fn approval_required(home: &Path, agent_id: &str, tool: &str) -> bool {
    let dir = agent_dir(home, agent_id);
    crate::approval::tool_requires_approval(&dir, tool)
        || crate::approval::tool_is_irreversible(&dir, tool)
        || crate::approval::tool_is_maybe_irreversible(&dir, tool)
}

fn approval_denied(message: String) -> OpError {
    OpError::new(ErrorCode::ApprovalDenied, message)
}

/// Ask a human through the [`ApprovalBroker`] (`action_kind = "mcp_call"`)
/// and wait for the decision. `detail` is gateway-built text (never
/// agent-controlled free text). Anything but an approval refuses.
pub(super) async fn obtain_approval(
    home: &Path,
    agent_id: &str,
    tool: &'static str,
    detail: &str,
    ttl_secs: i64,
    poll: Duration,
) -> Result<(), OpError> {
    let broker = match ApprovalBroker::open(home) {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "ApprovalBroker unavailable — denying computer tool (fail-closed)");
            return Err(approval_denied(format!(
                "審批系統暫時無法使用，工具「{tool}」的呼叫已拒絕。請稍後再試或由管理員手動處理。"
            )));
        }
    };
    let summary = format!(
        "AI 員工 {agent_id} 要使用電腦操作工具「{tool}」{detail}；此工具列在審批清單中，需經管理員核可後才能執行。"
    );
    let payload = json!({"tool": tool, "via": "computer_use_route"});
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
        Ok(ApprovalStatus::Approved) => Ok(()),
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
