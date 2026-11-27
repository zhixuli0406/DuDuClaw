//! Signed employee execution with a bounded, one-shot, pre-approval snapshot.
use super::{execute::ExecuteState, owner_snapshot::{OwnerSnapshot, changed}, Refusal};
use duduclaw_core::google_identity as gi;
use duduclaw_core::authority_coordinator::AuthorityCoordinator;
use duduclaw_knowledge::requester::RequesterIdentity;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Mutex, atomic::{AtomicUsize, Ordering}};
use std::time::{Duration, Instant};

#[derive(Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum Request {
    #[serde(rename = "employee_prepare")]
    Prepare { tool: String, arguments: Value, turn_id: Option<String>, run_id: Option<String>, call_id: String },
    #[serde(rename = "employee")]
    Execute { tool: String, arguments: Value, turn_id: Option<String>, run_id: Option<String>, call_id: String,
        approval_binding_digest: String },
}
#[derive(Clone, PartialEq, Eq, Serialize)]
struct Binding {
    agent: String, tool: String, args_digest: String,
    turn_id: Option<String>, run_id: Option<String>, call_id: String,
    snapshot: OwnerSnapshot,
    approval_nonce: String,
}
struct Pending { binding: Binding, created: Instant }
#[derive(Default)]
pub(super) struct Store(Mutex<HashMap<(String, String), Pending>>);
pub(super) fn valid_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}
pub(super) fn requester(agent: &str, turn: Option<&str>, run: Option<&str>) -> Result<RequesterIdentity, Refusal> {
    let mut selected = None;
    // Every supplied ID must be registered to this signed agent. A forged
    // run cannot be ignored just because a valid turn was supplied too.
    for id in [turn, run].into_iter().flatten() {
        if !valid_id(id) { return Err(changed()); }
        let who = crate::knowledge::turn_requesters::lookup(agent, id).ok_or_else(changed)?;
        if who == RequesterIdentity::Unknown || selected.as_ref().is_some_and(|old| old != &who) {
            return Err(changed());
        }
        selected = Some(who);
    }
    selected.ok_or_else(changed)
}
impl Binding {
    fn digest(&self) -> String { gi::google_arguments_digest(&json!(self)) }
}

pub(super) async fn serve(state: &ExecuteState, agent: &str, request: Request) -> Result<Value, Refusal> {
    let execute = matches!(&request, Request::Execute { .. });
    let (chat, drive) = match &request {
        Request::Prepare { tool, .. } | Request::Execute { tool, .. } =>
            (tool == super::chat_send::TOOL, gi::is_drive_write_tool(tool)),
    };
    let progress = AtomicUsize::new(0);
    let mut binding = None;
    let result = match tokio::time::timeout(Duration::from_secs(if drive { 660 } else if chat { 420 } else { 120 }),
        serve_inner(state, agent, request, &progress, &mut binding)).await {
        Ok(result) => result,
        Err(_) => Err(Refusal::hard(if !chat && progress.load(Ordering::SeqCst) == 0 { "not_sent" } else { "result_unconfirmed" },
            "Google 操作逾時；若已送出，結果無法確認，請先檢查 Google。")),
    };
    if execute {
        if let Some(binding) = &binding {
            audit(&state.home, binding, progress.load(Ordering::SeqCst), result.is_ok(), result.as_ref().err().map(|e| e.code));
        }
    }
    result
}

async fn serve_inner(state: &ExecuteState, agent: &str, request: Request,
    progress: &AtomicUsize, audit_binding: &mut Option<Binding>) -> Result<Value, Refusal> {
    let (tool, args, turn, run, call, approved) = match request {
        Request::Prepare { tool, arguments, turn_id, run_id, call_id } => (tool, arguments, turn_id, run_id, call_id, None),
        Request::Execute { tool, arguments, turn_id, run_id, call_id, approval_binding_digest } =>
            (tool, arguments, turn_id, run_id, call_id, Some(approval_binding_digest)),
    };
    if gi::is_drive_write_tool(&tool) {
        return super::drive_write::serve::serve(state, agent, &tool, &args, turn, run, call, approved, progress).await;
    }
    if gi::native_google_scope(&tool).is_none() || !args.is_object() || !valid_id(&call) {
        return Err(changed());
    }
    crate::google_workspace::validate_native_tool_inputs(&tool, &args)
        .map_err(|m| Refusal::hard("native_arguments_invalid", m))?;
    if !crate::google_workspace::integration_enabled(&state.home) {
        return Err(Refusal::hard("disabled", "管理員尚未開啟 Google 整合。"));
    }
    let who = requester(agent, turn.as_deref(), run.as_deref())?;
    if tool == super::chat_send::TOOL {
        return super::chat_send::serve(state, agent, &args, who, turn, call, approved).await;
    }
    let context = super::effect_authority::GoogleCallContext::employee(&state.home, agent, who,
        super::requester_gate::people_for(&state.home, state.users.as_ref()), &tool, &args, turn.clone(), run.clone())?;
    let coordinator = AuthorityCoordinator::new(&state.home);
    if approved.is_none() {
        let c = coordinator.clone();
        tokio::task::spawn_blocking(move || c.initialize()).await.map_err(|_| changed())?
            .map_err(|_| changed())?;
    }
    let lease = coordinator.shared().await.map_err(|_| changed())?;
    let snapshot = OwnerSnapshot::capture(&state.home, agent)?;
    let mut binding = Binding { approval_nonce: uuid::Uuid::new_v4().to_string(), agent: agent.to_owned(), tool: tool.clone(), args_digest: gi::google_arguments_digest(&args),
        turn_id: turn, run_id: run, call_id: call.clone(), snapshot };
    *audit_binding = Some(binding.clone());
    let key = (agent.to_owned(), call);
    {
        let mut entries = state.employees.0.lock().map_err(|_| changed())?;
        entries.retain(|_, p| p.created.elapsed() < Duration::from_secs(600));
        if let Some(approved) = approved {
            let pending = entries.remove(&key).ok_or_else(changed)?;
            binding.approval_nonce = pending.binding.approval_nonce.clone();
            if pending.binding != binding || approved != binding.digest() { return Err(changed()); }
        } else {
            if entries.len() >= 1024 || entries.contains_key(&key) { return Err(changed()); }
            let digest = binding.digest();
            entries.insert(key, Pending { binding: binding.clone(), created: Instant::now() });
            return Ok(json!({"approval_binding_digest":digest, "owner_id":binding.snapshot.owner_id,
                "owner_email":binding.snapshot.email, "owner_epoch":binding.snapshot.epoch}));
        }
    }
    drop(lease);
    // The private host dispatcher supplies this binding only after ALL of its
    // policy/approval/egress gates. A signature authenticates the host; it is
    // not interpreted as a human approval. Required human grants are checked
    // against the broker independently, before touching credential material.
    verify_approval(&state.home, &binding, &args).await?;
    super::effect_authority::employee::execute(context, &binding.snapshot, &args, progress).await
}

fn audit(home: &std::path::Path, binding: &Binding, steps: usize, success: bool, reason: Option<&str>) {
    let outcome = if steps == 0 { "not_sent" } else if success { "confirmed" } else { "may_have_sent" };
    duduclaw_security::audit::append_tool_call_with_extras(home, &binding.agent, "google_execute", &binding.tool,
        success, &[("call_id", json!(binding.call_id)), ("run_id", json!(binding.run_id)),
        ("google_turn_id", json!(binding.turn_id)), ("owner_id", json!(binding.snapshot.owner_id)),
        ("authority_epoch", json!(binding.snapshot.epoch)), ("args_digest", json!(binding.args_digest)),
        ("step_count", json!(steps)), ("outcome", json!(outcome)), ("reason", json!(reason))]);
}

async fn verify_approval(home: &std::path::Path, binding: &Binding, args: &Value) -> Result<(), Refusal> {
    verify_policy_approval(home, &binding.agent, &binding.tool, &binding.digest(), args).await
}

pub(super) async fn verify_policy_approval(home: &std::path::Path, agent: &str, tool: &str, digest: &str, args: &Value) -> Result<(), Refusal> {
    use crate::approval::{ApprovalBroker, ApprovalStatus};
    let dir = home.join("agents").join(agent);
    #[derive(Deserialize)]
    struct Config { #[serde(default)] capabilities: duduclaw_core::types::CapabilitiesConfig }
    let raw = duduclaw_core::agent_toml::read_effective_text(&dir).map_err(|_| changed())?;
    let config: Config = toml::from_str(&raw).map_err(|_| changed())?;
    if duduclaw_core::tool_catalog::tool_list_verdict(tool,
        &config.capabilities.denied_tools, &config.capabilities.allowed_tools)
        != duduclaw_core::tool_catalog::ToolListVerdict::Allowed { return Err(changed()); }
    let event = duduclaw_security::policy_kernel::ToolCallEvent {
        tool_name: tool, arguments: args, agent_id: agent,
    };
    let policy_ask = match duduclaw_security::policy_kernel::evaluate(&event, &config.capabilities.policy) {
        duduclaw_security::policy_kernel::Decision::Allow => false,
        duduclaw_security::policy_kernel::Decision::AllowRewritten(rewritten) if &rewritten == args => false,
        duduclaw_security::policy_kernel::Decision::Ask { .. } => true,
        _ => return Err(changed()),
    };
    let rules = duduclaw_core::agent_toml::load_action_rules(&dir);
    let verdict = rules.resolve_for_call(tool, duduclaw_core::effect_of(tool), args);
    if verdict == Some(duduclaw_core::ActionVerdict::Block) { return Err(changed()); }
    let required = policy_ask || crate::approval::tool_requires_approval(&dir, tool)
        || crate::approval::tool_is_irreversible(&dir, tool)
        || verdict == Some(duduclaw_core::ActionVerdict::Ask);
    if !required { return Ok(()); }
    let broker = ApprovalBroker::open(home).map_err(|_| changed())?;
    let records = broker.list_by_kind("mcp_call").await.map_err(|_| changed())?;

    let found = records.iter().any(|r| chrono::DateTime::parse_from_rfc3339(&r.created_at).ok()
        .and_then(|at| at.checked_add_signed(chrono::Duration::seconds(r.ttl_seconds)))
        .is_some_and(|until| until > chrono::Utc::now()) && r.agent_id == agent
        && r.payload.get("google_owner_binding").and_then(Value::as_str) == Some(digest)
        && r.payload.get("arguments") == Some(args)
        && (r.status == ApprovalStatus::Approved
            || (r.status == ApprovalStatus::Invalidated && r.invalidated_reason.as_deref()
                .is_some_and(|s| s.starts_with(crate::approval::action_grant::CONSUMED_PREFIX)))));
    if !found { return Err(Refusal::hard("approval_denied", "無法確認這次 Google 負責人操作的核准。")); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn assert_drive_early_refusal(case: &str) {
        for tool in gi::DRIVE_WRITE_TOOLS {
            for execute in [false, true] {
                let home = tempfile::tempdir().unwrap();
                std::fs::write(home.path().join("config.toml"),
                    format!("[integrations]\ngoogle_workspace={}\n", case != "disabled")).unwrap();
                let state = ExecuteState::new(home.path().to_path_buf(), None);
                let turn = uuid::Uuid::new_v4().to_string();
                let who = if case == "unknown" { RequesterIdentity::Unknown }
                    else { RequesterIdentity::DashboardUser { user_id: "member".into() } };
                let _guard = if case == "unregistered" { None } else {
                    Some(crate::knowledge::turn_requesters::register("bot", &turn, who).unwrap())
                };
                let arguments = if case == "malformed" { json!([]) } else { json!({}) };
                let turn_id = (case != "missing_ids").then_some(turn.clone());
                let call_id = if case == "invalid_call" { String::new() } else { "call".into() };
                let request = if execute {
                    Request::Execute { tool: (*tool).into(), arguments, turn_id, run_id: None,
                        call_id, approval_binding_digest: "untrusted-digest".into() }
                } else {
                    Request::Prepare { tool: (*tool).into(), arguments, turn_id, run_id: None, call_id }
                };
                let result = serve(&state, "bot", request).await;
                let code = if case == "disabled" { "disabled" } else { "changed" };
                assert_eq!(result.unwrap_err().code, code, "{tool}: {case}");
                let raw = std::fs::read_to_string(home.path().join("tool_calls.jsonl")).unwrap();
                let rows: Vec<Value> = raw.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
                assert_eq!(rows.len(), 1, "{tool}: {case}");
                let row = &rows[0];
                assert_eq!(row["tool_name"], "google_drive_write");
                assert_eq!(row["tool"], *tool);
                assert_eq!(row["outcome"], "refused");
                assert_eq!(row["reason"], code);
                assert_eq!(row["success"], false);
                if case == "missing_ids" {
                    assert_eq!(row["google_turn_id"], "");
                } else {
                    assert_eq!(row["google_turn_id"], turn);
                    assert_eq!(row["id_source"], "turn");
                }
            }
        }
    }

    #[tokio::test]
    async fn drive_integration_disabled_is_audited_once_as_refused() {
        assert_drive_early_refusal("disabled").await;
    }

    #[tokio::test]
    async fn drive_malformed_arguments_are_audited_once_as_refused() {
        assert_drive_early_refusal("malformed").await;
    }

    #[tokio::test]
    async fn drive_unknown_requester_is_audited_once_as_refused() {
        assert_drive_early_refusal("unknown").await;
        assert_drive_early_refusal("unregistered").await;
    }

    #[tokio::test]
    async fn drive_invalid_call_and_missing_ids_are_audited_once_as_refused() {
        assert_drive_early_refusal("invalid_call").await;
        assert_drive_early_refusal("missing_ids").await;
    }

    #[test]
    fn d12_audit_preserves_google_turn_id() {
        let home = tempfile::tempdir().unwrap();
        let binding = Binding {
            agent: "bot".into(), tool: "gmail_read".into(), args_digest: "digest".into(),
            turn_id: Some("registered-d12-turn".into()), run_id: None, call_id: "call".into(),
            approval_nonce: "nonce".into(),
            snapshot: OwnerSnapshot {
                owner_id: "holder".into(), email: "holder@c.test".into(), employee_nonce: "nonce".into(),
                employee_id: "employee".into(), incarnation: 1, owner_generation: 1,
                attestation_revision: "revision".into(), epoch: "epoch".into(), policy_digest: "policy".into(),
                user_epoch: "user-epoch".into(), credential_epoch: 1, policy_epoch: "policy-epoch".into(),
            },
        };
        audit(home.path(), &binding, 1, true, None);
        let raw = std::fs::read_to_string(home.path().join("tool_calls.jsonl")).unwrap();
        let row: Value = serde_json::from_str(raw.lines().next().unwrap()).unwrap();
        assert_eq!(row["tool_name"], "google_execute");
        assert_eq!(row["google_turn_id"], "registered-d12-turn");
    }

    #[test]
    fn no_wire_subject_or_forged_run() {
        for field in ["subject", "email", "owner_id", "credential_ref"] {
            let mut request = json!({"kind":"employee", "tool":"gmail_read", "arguments":{},
                "call_id":"call", "turn_id":"turn", "approval_binding_digest":"digest"});
            request[field] = json!("forged");
            assert!(serde_json::from_value::<Request>(request).is_err());
        }
        let _guard = crate::knowledge::turn_requesters::register("bot", "real-turn",
            RequesterIdentity::System { kind:"test".into(), on_behalf_of:None }).unwrap();
        assert!(requester("bot", Some("real-turn"), Some("forged-run")).is_err());
        assert!(requester("other", Some("real-turn"), None).is_err());
    }
}
