use super::*;

// ── Human-machine co-drive (CD-1) ────────────────────────────────────────────
// Thin shell over `duduclaw_gateway::codrive::run_script` — all the actual
// orchestration (refuse-list, ApprovalBroker gate, freeze/resume, emergency
// stop, activity-feed ticker) lives there. This front door does three things:
// parse `script` (accept either a native JSON object or a JSON-encoded
// string, since MCP callers serialize nested objects either way), resolve the
// acting agent identity, and re-check `[capabilities] codrive` for THAT
// identity before dispatching — the identity resolution + capability
// re-check itself is `duduclaw_gateway::codrive::resolve_run_identity`
// (CD-2), not duplicated here; see that function's doc comment for the full
// "`agent` overrides the WHOLE call identity" semantics and why.
//
// The dispatch-level `CODRIVE_TOOLS` gate in `mcp_dispatch.rs` is keyed to
// the CALLING principal (`principal.client_id`) and would not see an `agent`
// override, so `resolve_run_identity`'s re-check below is the actual
// enforcement point for the resolved identity — the two together are the
// "雙保險 fail-closed" the WP brief calls for, not a redundant duplicate of
// the same check.
pub(crate) async fn handle_codrive_run(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let script_value = match args.get("script") {
        Some(Value::String(s)) => match serde_json::from_str::<Value>(s) {
            Ok(v) => v,
            Err(e) => return tool_error(&format!("script is not valid JSON: {e}")),
        },
        Some(v) => v.clone(),
        None => return tool_error("script is required"),
    };
    let script: duduclaw_gateway::codrive::CodriveScript =
        match serde_json::from_value(script_value) {
            Ok(s) => s,
            Err(e) => return tool_error(&format!("script does not match the expected shape: {e}")),
        };

    let caller_agent_id = resolve_audit_agent(|| default_agent.to_string());
    let requested_agent = args.get("agent").and_then(|v| v.as_str());
    let agent_id = match duduclaw_gateway::codrive::resolve_run_identity(
        home_dir,
        &caller_agent_id,
        requested_agent,
    ) {
        Ok(id) => id,
        Err(duduclaw_gateway::codrive::RunIdentityError::InvalidAgentId) => {
            return tool_error("invalid agent id");
        }
        Err(duduclaw_gateway::codrive::RunIdentityError::CapabilityMissing(id)) => {
            let msg = "此代理未啟用人機共駕能力（agent.toml [capabilities] codrive = true）。"
                .to_string();
            duduclaw_security::audit::append_tool_call_denied(
                home_dir,
                &id,
                "codrive_run",
                "codrive_capability_missing",
                &msg,
                None,
            );
            return tool_error(&msg);
        }
    };

    let report = duduclaw_gateway::codrive::run_script(home_dir, &agent_id, script).await;
    tool_text(&serde_json::to_string(&report).unwrap_or_else(|_| "{}".to_string()))
}

/// A2 `codrive_status`: read-only driving-state query.
///
/// Authorization is the same three-layer stack `codrive_run` sits behind —
/// `Scope::Admin` (`mcp_auth::tool_requires_scope`), the deny-by-default
/// `[capabilities] codrive` dispatch gate (`mcp_dispatch::CODRIVE_TOOLS`,
/// keyed to the CALLING principal), and this in-handler re-check via
/// `resolve_run_identity` (the "雙保險 fail-closed" half). Unlike
/// `codrive_run` this tool takes NO `agent` parameter: a read has no
/// approval to attribute and no script to run, so there is nothing an
/// identity override would buy except a second way to be wrong. The
/// capability is therefore always checked against the caller's own
/// identity.
///
/// Success is not audited (it is a read that changes nothing, matching the
/// wider convention for read tools); a capability refusal is, exactly like
/// `codrive_run`'s.
pub(crate) async fn handle_codrive_status(home_dir: &Path, default_agent: &str) -> Value {
    let caller_agent_id = resolve_audit_agent(|| default_agent.to_string());
    match duduclaw_gateway::codrive::resolve_run_identity(home_dir, &caller_agent_id, None) {
        Ok(_) => {}
        Err(duduclaw_gateway::codrive::RunIdentityError::InvalidAgentId) => {
            return tool_error("invalid agent id");
        }
        Err(duduclaw_gateway::codrive::RunIdentityError::CapabilityMissing(id)) => {
            let msg = "此代理未啟用人機共駕能力（agent.toml [capabilities] codrive = true）。"
                .to_string();
            duduclaw_security::audit::append_tool_call_denied(
                home_dir,
                &id,
                "codrive_status",
                "codrive_capability_missing",
                &msg,
                None,
            );
            return tool_error(&msg);
        }
    }

    let report = duduclaw_gateway::codrive::query_codrive_status(home_dir).await;
    tool_text(&serde_json::to_string(&report).unwrap_or_else(|_| "{}".to_string()))
}

// ── Agent Mail (P2-d) ───────────────────────────────────────────────────────
// Three tools over `duduclaw_gateway::mail`. Two invariants live at this front
// door and are tested here rather than trusted to the caller:
//
// 1. **Mailbox ownership is organizational.** `agent_id` defaults to the
//    caller; naming somebody else runs the same v1.52 `delegation_policy`
//    predicate every other cross-agent path uses, so a mailbox cannot be read
//    across a boundary the org chart does not permit. A denial is audited by
//    `check_delegation_allowed` itself.
// 2. **`mail_send` has no send path.** It validates, files an approval, and
//    writes a `pending` row. The transmission lives in
//    `mail_worker::settle_outbox`, which only ever acts on a decided approval.

/// Resolve the mailbox this call may touch. `None`/blank ⇒ the caller's own.
pub(crate) async fn resolve_mail_target(
    args: &Value,
    home_dir: &Path,
    default_agent: &str,
    path_kind: &str,
) -> std::result::Result<String, String> {
    let requested = args
        .get("agent_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let Some(target) = requested else {
        return Ok(default_agent.to_string());
    };
    if target == default_agent {
        return Ok(target.to_string());
    }
    if !is_valid_agent_id(target) {
        return Err("invalid agent_id format".to_string());
    }
    check_delegation_allowed(home_dir, default_agent, target, path_kind).await?;
    Ok(target.to_string())
}

/// Refuse every mail tool when `[mail] enabled = false`, so an operator who
/// never turned the mailbox on cannot have an agent quietly accumulate drafts.
pub(crate) fn mail_enabled_or_error(
    home_dir: &Path,
) -> std::result::Result<duduclaw_gateway::mail::MailConfig, String> {
    let cfg = duduclaw_gateway::mail::MailConfig::from_home(home_dir);
    if !cfg.enabled {
        return Err(
            "信箱功能未啟用（config.toml [mail] enabled = true 由操作者開啟）。".to_string(),
        );
    }
    Ok(cfg)
}

pub(crate) async fn handle_mail_list(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    if let Err(e) = mail_enabled_or_error(home_dir) {
        return tool_error(&e);
    }
    let target = match resolve_mail_target(args, home_dir, default_agent, "mail_list").await {
        Ok(t) => t,
        Err(e) => return tool_error(&e),
    };
    let include_archived = args
        .get("include_archived")
        .and_then(|v| {
            v.as_bool()
                .or_else(|| v.as_str().map(|s| s.eq_ignore_ascii_case("true")))
        })
        .unwrap_or(false);
    let limit = args
        .get("limit")
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        })
        .map(|n| n as usize)
        .unwrap_or(20);

    let home = home_dir.to_path_buf();
    let result = tokio::task::spawn_blocking(move || {
        duduclaw_gateway::mail::list_inbox(&home, Some(&target), include_archived, limit)
    })
    .await;
    match result {
        Ok(items) => {
            // Bodies are deliberately NOT included in the list view: a listing
            // is navigation, and shipping every body would both blow the
            // context budget and splash untrusted text across a tool result
            // the agent did not ask to read.
            let rows: Vec<Value> = items
                .iter()
                .map(|m| {
                    serde_json::json!({
                        "mail_id": m.mail_id,
                        "from": m.from,
                        "subject": m.subject,
                        "received_at": m.received_at,
                        "source": m.source,
                        "read": m.read,
                        "archived": m.archived,
                        "handled": m.triggered,
                        "flagged": m.suspicious,
                    })
                })
                .collect();
            tool_text(&serde_json::json!({ "count": rows.len(), "messages": rows }).to_string())
        }
        Err(e) => tool_error(&format!("mail_list join error: {e}")),
    }
}

pub(crate) async fn handle_mail_read(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    if let Err(e) = mail_enabled_or_error(home_dir) {
        return tool_error(&e);
    }
    let target = match resolve_mail_target(args, home_dir, default_agent, "mail_read").await {
        Ok(t) => t,
        Err(e) => return tool_error(&e),
    };
    let mail_id = args
        .get("mail_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if mail_id.is_empty() {
        return tool_error("mail_id is required");
    }

    let home = home_dir.to_path_buf();
    let reader = default_agent.to_string();
    let result = tokio::task::spawn_blocking(move || {
        let Some(item) = duduclaw_gateway::mail::get_inbox_item(&home, &mail_id) else {
            return Err(format!("找不到這封信：{mail_id}"));
        };
        // Ownership is re-checked against the stored row, not just the
        // requested mailbox — otherwise naming your own agent_id would read
        // anyone's mail by id.
        if !item.agent_id.is_empty() && item.agent_id != target {
            return Err("這封信不屬於你可以讀取的信箱。".to_string());
        }
        duduclaw_gateway::mail::mark_read(&home, &mail_id, &reader);
        Ok(item)
    })
    .await;

    match result {
        Ok(Ok(item)) => tool_text(
            &serde_json::json!({
                "mail_id": item.mail_id,
                "from": item.from,
                "subject": item.subject,
                "received_at": item.received_at,
                "source": item.source,
                "flagged": item.suspicious,
                "risk_score": item.risk_score,
                // The body ships inside the same data-not-instructions frame
                // the auto-trigger prompt uses, so the warning travels with
                // the content on every path that can reach a model.
                "content": duduclaw_gateway::mail::render_mail_as_data(&item),
            })
            .to_string(),
        ),
        Ok(Err(e)) => tool_error(&e),
        Err(e) => tool_error(&format!("mail_read join error: {e}")),
    }
}

pub(crate) async fn handle_mail_send(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let cfg = match mail_enabled_or_error(home_dir) {
        Ok(c) => c,
        Err(e) => return tool_error(&e),
    };
    let to = args
        .get("to")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let subject = args
        .get("subject")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let body = args
        .get("body")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let in_reply_to = args
        .get("in_reply_to")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    // 空內容保護 + recipient shape/allowlist, before anything is persisted.
    if let Err(e) = duduclaw_gateway::mail::validate_outbound(&cfg, &to, &subject, &body) {
        return tool_error(&e);
    }

    let broker = match duduclaw_gateway::approval::ApprovalBroker::open(home_dir) {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "mail_send: approval broker unavailable — refusing (fail-closed)");
            return tool_error(
                "審批系統無法建立確認請求，這封信沒有排入寄件匣（fail-closed，絕不略過確認直接寄出）。",
            );
        }
    };
    let summary = duduclaw_gateway::mail::outbound_summary(&to, &subject);
    let payload = serde_json::json!({
        "kind": "agent_mail",
        "to": to,
        "subject": subject,
        "body": duduclaw_core::truncate_chars(&body, 2000),
        "agent_id": default_agent,
    });
    let approval_id = match broker
        .request(
            default_agent,
            duduclaw_gateway::mail::OUTBOUND_ACTION_KIND,
            &summary,
            payload,
            cfg.outbound_ttl_secs,
        )
        .await
    {
        Ok(id) => id,
        Err(e) => {
            warn!(error = %e, "mail_send: approval request failed — refusing (fail-closed)");
            return tool_error("無法建立寄件確認請求，這封信沒有排入寄件匣（fail-closed）。");
        }
    };

    let home = home_dir.to_path_buf();
    let agent = default_agent.to_string();
    let approval_str = approval_id.as_str().to_string();
    let (to_c, subj_c, body_c, reply_c) = (to.clone(), subject.clone(), body, in_reply_to);
    let mail_id = tokio::task::spawn_blocking(move || {
        duduclaw_gateway::mail::record_outbox_draft(
            &home,
            &cfg,
            &agent,
            &to_c,
            &subj_c,
            &body_c,
            &approval_str,
            reply_c.as_deref(),
        )
    })
    .await;
    let mail_id = match mail_id {
        Ok(id) => id,
        Err(e) => return tool_error(&format!("mail_send join error: {e}")),
    };

    duduclaw_security::audit::append_tool_call_with_extras(
        home_dir,
        default_agent,
        "mail_send",
        &summary,
        true,
        &[
            ("mail_id", serde_json::json!(mail_id)),
            ("approval_id", serde_json::json!(approval_id.as_str())),
            (
                "to",
                serde_json::json!(duduclaw_core::truncate_chars(&to, 120)),
            ),
            ("state", serde_json::json!("pending_confirmation")),
        ],
    );

    tool_text(
        &serde_json::json!({
            "sent": false,
            "state": "pending_confirmation",
            "mail_id": mail_id,
            "approval_id": approval_id.as_str(),
            "message": format!(
                "這封信【還沒有寄出】。已排入待確認寄件匣，等人在儀表板（或通道的確認按鈕）按下確認才會寄出；\
                 逾時未確認會自動取消。回報時請說「已排入待確認」，不要說已寄出。收件者：{}",
                duduclaw_core::truncate_chars(to.trim(), 120)
            ),
        })
        .to_string(),
    )
}
