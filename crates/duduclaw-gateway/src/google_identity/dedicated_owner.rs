//! Real dedicated credentials retain Unknown requester semantics. Metadata is
//! pinned before approval; legacy clients without the binding fail closed.
use std::{path::Path, sync::Arc};
use duduclaw_core::google_identity::{self as gi, CredentialRef};
use duduclaw_knowledge::requester::RequesterIdentity;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use super::{Refusal, owner_snapshot::{OwnerSnapshot, changed}};

tokio::task_local! { static STEPS: Arc<AtomicUsize>; }
pub(crate) fn record_sent() { let _ = STEPS.try_with(|steps| steps.fetch_add(1, Ordering::SeqCst)); }
struct Audit<'a> {
    home: &'a Path, agent: &'a str, tool: &'a str, snapshot: &'a OwnerSnapshot,
    args_digest: String, call: String, progress: Arc<AtomicUsize>, confirmed: bool,
}
impl Drop for Audit<'_> {
    fn drop(&mut self) {
        let steps = self.progress.load(Ordering::SeqCst);
        duduclaw_security::audit::append_tool_call_with_extras(self.home, self.agent, "google_execute", self.tool,
            self.confirmed, &[("call_id", json!(self.call)), ("run_id", Value::Null),
            ("owner_id", json!(self.snapshot.owner_id)), ("authority_epoch", json!(self.snapshot.epoch)),
            ("args_digest", json!(self.args_digest)), ("step_count", json!(steps)),
            ("outcome", json!(if steps == 0 { "not_sent" } else if self.confirmed { "confirmed" } else { "may_have_sent" }))]);
    }
}

pub(crate) async fn serve(home: &Path, users: Option<&Arc<duduclaw_auth::UserDb>>, agent: &str,
    reference: &CredentialRef, tool: &str, args: &Value, prepare: bool, binding: Option<&str>) -> Result<Value, Refusal> {
    super::credential_registry::authorize(home, reference, agent, tool).map_err(|_| changed())?;
    crate::google_workspace::validate_native_tool_inputs(tool, args)
        .map_err(|e| Refusal::hard("native_arguments_invalid", e))?;
    let home_owned = home.to_owned();
    let agent_owned = agent.to_owned();
    let snapshot = tokio::task::spawn_blocking(move || OwnerSnapshot::optional(&home_owned, &agent_owned))
        .await.map_err(|_| changed())??.ok_or_else(changed)?;
    let people = super::requester_gate::people_for(home, users);
    let policy = super::delegate_owner::effective_for(home, Some(agent));
    super::requester_gate::check(policy.requesters, &RequesterIdentity::Unknown, agent,
        &snapshot.email, people.as_ref())?;
    let digest = gi::google_arguments_digest(&json!({"snapshot":snapshot, "tool":tool,
        "arguments":gi::google_arguments_digest(args), "credential":reference}));
    if prepare {
        if binding.is_some() { return Err(changed()); }
        return Ok(json!({"owner_binding_digest":digest, "owner_email":snapshot.email}));
    }
    let mut audit = Audit { home, agent, tool, snapshot: &snapshot, args_digest: gi::google_arguments_digest(args),
        call: uuid::Uuid::new_v4().to_string(), progress: Arc::new(AtomicUsize::new(0)), confirmed: false };
    if binding != Some(digest.as_str()) { return Err(changed()); }
    let result = STEPS.scope(audit.progress.clone(), super::effect_authority::execute_immediate_native_pinned(home, users, agent,
        reference, tool, args.clone(), None, Some(snapshot.clone()))).await;
    audit.confirmed = result.is_ok();
    result
}
