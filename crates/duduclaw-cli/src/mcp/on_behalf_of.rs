//! The person this MCP server's employee works for (`on_behalf_of`).
//!
//! The only source is the `DUDUCLAW_ON_BEHALF_OF` variable the gateway set
//! when it spawned the employee (see `duduclaw_gateway::on_behalf_of`); no
//! tool argument is ever read. The value is written onto what this process
//! hands on (`message_queue.on_behalf_of`, the `bus_queue.jsonl` field, the
//! task row) and, with `[delegation] human_principal = "enforce"`, narrows
//! delegations the employee tree already allowed.

use super::*;

/// The principal from the host env, validated. A malformed value is audited
/// once per process as `on_behalf_of_rejected` and treated as absent.
pub(crate) fn host_on_behalf_of(home_dir: &Path, caller: &str) -> Option<String> {
    match duduclaw_core::host_principal_checked() {
        Ok(p) => p.map(|p| p.to_string()),
        Err(rejection) => {
            audit_rejection_once(home_dir, caller, &rejection);
            None
        }
    }
}

fn audit_rejection_once(
    home_dir: &Path,
    caller: &str,
    rejection: &duduclaw_core::PrincipalRejection,
) {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    if ONCE.set(()).is_ok() {
        duduclaw_gateway::on_behalf_of::audit_rejected(home_dir, caller, "mcp_env", rejection);
    }
}

/// Enforce-mode check for `sender → target` on `path_kind` (call it after
/// the employee tree allowed the delegation). Record mode always passes.
/// A refusal writes a `delegation_denied` row (which carries the principal)
/// and returns the zh-TW reason.
pub(crate) async fn check_human_principal(
    home_dir: &Path,
    sender: &str,
    target: &str,
    path_kind: &str,
    policy: duduclaw_core::DelegationPolicy,
) -> std::result::Result<(), String> {
    let mode = duduclaw_core::HumanPrincipalMode::from_home(home_dir);
    let principal = duduclaw_core::host_principal_checked();
    if let Err(rejection) = &principal {
        audit_rejection_once(home_dir, sender, rejection);
    }
    if mode == duduclaw_core::HumanPrincipalMode::Record {
        return Ok(());
    }
    let allowed = match &principal {
        Ok(p) => authority_check(home_dir, sender, target, path_kind, p.as_ref()).await,
        Err(_) => false,
    };
    let result = if allowed {
        Ok(())
    } else {
        Err(duduclaw_core::DelegationDenied {
            sender: sender.into(),
            target: target.into(),
            policy,
            reason: duduclaw_core::DelegationDenyReason::HumanPrincipalNotPermitted,
        })
    };
    match result {
        Ok(()) => Ok(()),
        Err(denied) => {
            duduclaw_security::audit::append_tool_call_with_extras(
                home_dir,
                sender,
                "delegation_denied",
                &format!(
                    "{path_kind}: '{sender}' -> '{target}' denied ({})",
                    denied.reason.as_str()
                ),
                false,
                &[
                    ("target", serde_json::json!(target)),
                    ("reason", serde_json::json!(denied.reason.as_str())),
                    ("policy", serde_json::json!(denied.policy.as_str())),
                    ("path_kind", serde_json::json!(path_kind)),
                    ("human_principal", serde_json::json!(mode.as_str())),
                ],
            );
            Err(denied.message_zh())
        }
    }
}

/// Async loopback bridge. Failure, absent context and identity disagreement
/// all deny; there is no local database or edition-environment fallback.
async fn authority_check(
    home: &Path,
    agent: &str,
    target: &str,
    operation: &str,
    principal: Option<&duduclaw_core::HumanPrincipal>,
) -> bool {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let Some(context) = env(duduclaw_gateway::on_behalf_of::context::ENV_CONTEXT) else {
        return false;
    };
    if context.len() != 64
        || !context
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return false;
    }
    let Some(key) = env(duduclaw_core::ENV_MCP_API_KEY) else {
        return false;
    };
    let Some(token) = env(duduclaw_core::ENV_AGENT_TOKEN) else {
        return false;
    };
    let body = serde_json::json!({"context":context,"target":target,"operation":authority_operation(operation)});
    let Ok(bytes) = serde_json::to_vec(&body) else {
        return false;
    };
    let (port, _) = duduclaw_core::gateway_port_for_home(home);
    let Ok(client) = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(10))
        .build()
    else {
        return false;
    };
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let nonce: [u8; 16] = rand::random();
    let mut req = client
        .post(format!(
            "http://127.0.0.1:{port}{}",
            duduclaw_gateway::on_behalf_of::route::ROUTE
        ))
        .header("Content-Type", "application/json");
    for (k, v) in
        super::computer_use_client::signed_headers(&key, agent, &token, timestamp, &nonce, &bytes)
    {
        req = req.header(k, v);
    }
    let Ok(mut resp) = req.body(bytes).send().await else {
        return false;
    };
    if resp.status() != reqwest::StatusCode::OK {
        return false;
    }
    let mut out = Vec::new();
    loop {
        match resp.chunk().await {
            Ok(Some(chunk)) if out.len() + chunk.len() <= 4096 => out.extend_from_slice(&chunk),
            Ok(None) => break,
            _ => return false,
        }
    }
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Answer {
        ok: bool,
        principal: Option<String>,
    }
    let Ok(answer) = serde_json::from_slice::<Answer>(&out) else {
        return false;
    };
    answer.ok && answer.principal == principal.map(ToString::to_string)
}

/// Preserve every existing path's semantics, including mail, record changes
/// and schedule hand-overs. The wire vocabulary is closed to two rights.
fn authority_operation(path: &str) -> &'static str {
    match duduclaw_core::human_right_for_path(path) {
        duduclaw_core::HumanRight::Call => "call",
        duduclaw_core::HumanRight::Assign => "assign",
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn permission_wire_kind_covers_all_shared_delegation_paths() {
        for path in ["send_to_agent", "spawn_agent"] {
            assert_eq!(super::authority_operation(path), "call");
        }
        for path in [
            "tasks_create",
            "tasks_update",
            "tasks_create_schedule",
            "create_task",
            "mail_read",
            "decisions_update",
            "unknown-future-path",
        ] {
            assert_eq!(super::authority_operation(path), "assign", "{path}");
        }
    }
}
