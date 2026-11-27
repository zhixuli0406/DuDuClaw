//! Employee call context: requester comes only from the live gateway registry,
//! never a fake dedicated credential or a caller-provided subject.
use super::*;
use crate::google_identity::owner_snapshot::{OwnerSnapshot, changed};
use serde_json::Value;

impl GoogleCallContext {
    pub(crate) fn employee(
        home: &Path, agent: &str, requester: RequesterIdentity,
        people: Box<dyn People>, tool: &str, args: &Value, turn_id: Option<String>, run_id: Option<String>,
    ) -> Result<Self, Refusal> {
        if gi::native_google_scope(tool).is_none() || duduclaw_core::google_identity::is_drive_write_tool(tool) || requester == RequesterIdentity::Unknown {
            return Err(changed());
        }
        let (home, agent_dir, canonical_agent) = Self::canonical(home, &home.join("agents").join(agent))?;
        if canonical_agent != agent { return Err(changed()); }
        let context = Self { owner_snapshot: None, home, agent_dir, agent: canonical_agent, requester, people,
            purpose: GooglePurpose::Employee { native_tool: tool.to_owned(),
                final_arguments_digest: gi::google_arguments_digest(args), turn_id, run_id }, tenant: None,
        };
        context.validate()?;
        context.validate_employee()?;
        Ok(context)
    }
    pub(crate) fn validate_employee(&self) -> Result<(), Refusal> {
        self.validate()?;
        if let GooglePurpose::Employee { turn_id, run_id, .. } = &self.purpose {
            for id in [turn_id, run_id].into_iter().flatten() {
                if crate::knowledge::turn_requesters::lookup(&self.agent, id).as_ref() != Some(&self.requester) {
                    return Err(changed());
                }
            }
        }
        let policy = crate::google_identity::delegate_owner::effective_for(&self.home, Some(&self.agent));
        let gi::Effective::Delegate { email } = &policy.effective else { return Err(changed()); };
        crate::google_identity::requester_gate::check(policy.requesters, &self.requester,
            &self.agent, email, self.people.as_ref())?;
        crate::google_identity::resolve::precheck(&self.home, email, self.people.as_ref())?;
        if matches!(self.purpose.scope_key(), "google_chat_spaces" | "google_chat_messages")
            && !crate::google_workspace::chat_read_tool_available(&self.home, self.purpose.scope_key()) {
            return Err(Refusal::hard("scope_missing", crate::google_workspace::chat_read::GUIDANCE));
        }
        Ok(())
    }
}

// These fields and the included transport are inside this credential owner.
// A bearer is never a response, a callback argument, or a serializable value.
struct Resolved { access: zeroize::Zeroizing<String>, subject: String }
struct Bearer<'a> { material: &'a Resolved, permit: Permit<'a> }
struct Permit<'a> {
    context: &'a GoogleCallContext,
    snapshot: &'a OwnerSnapshot,
    progress: &'a std::sync::atomic::AtomicUsize,
}
impl Permit<'_> {
    async fn acquire_native_lease(&self) -> Result<Option<duduclaw_core::authority_coordinator::SharedLease>, Refusal> {
        self.snapshot.lease(&self.context.home, &self.context.agent).await.map(Some)
    }
    fn native_sent(&self) { self.progress.fetch_add(1, std::sync::atomic::Ordering::SeqCst); }
    #[cfg(test)] fn test_native_boundary(&self, stage: &str) { crate::google_workspace::native_call::testing::boundary(&self.context.home, stage); }
    #[cfg(test)] fn test_endpoint(&self, url: &str) -> Option<reqwest::Url> { crate::google_workspace::native_call::testing::endpoint(&self.context.home, url) }
    #[cfg(test)] fn test_completed(&self, _: &Value) {}
}
fn validate_native(permit: &Permit<'_>, _: &Resolved,
    template: &crate::google_workspace::native_call::NativeCallTemplate) -> Result<(), Refusal> {
    permit.context.validate_employee()?;
    permit.snapshot.validate(&permit.context.home, &permit.context.agent)?;
    let GooglePurpose::Employee { native_tool, final_arguments_digest, .. } = &permit.context.purpose
        else { return Err(changed()); };
    if native_tool != template.tool() || final_arguments_digest != template.digest() {
        return Err(changed());
    }
    Ok(())
}
fn source_notice(material: &Resolved) -> String {
    format!("本次使用目前負責人已認證的 Google 帳號：{}。", material.subject)
}
mod native_io {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/google_identity/assertion_native_io.inc.rs"));
}

pub(crate) async fn execute(
    context: GoogleCallContext, snapshot: &OwnerSnapshot, args: &Value,
    progress: &std::sync::atomic::AtomicUsize,
) -> Result<Value, Refusal> {
    context.validate_employee()?;
    snapshot.validate(&context.home, &context.agent)?;
    if context.purpose.scope_key() == "google_status" {
        return Ok(serde_json::json!({"content":[{"type":"text", "text":
            format!("目前負責人 Google 帳號：{}；僅檢查本機授權，未取得權杖。", snapshot.email)}]}));
    }
    let mut template = crate::google_workspace::native_call::NativeCallTemplate::prepare(context.purpose.scope_key(), args)
        .map_err(|m| Refusal::hard("native_arguments_invalid", m))?;
    template.validate_closed().map_err(|m| Refusal::hard("native_arguments_invalid", m))?;
    if matches!(template.tool(), "gmail_send_draft" | "calendar_delete_event") {
        template.bind_confirmation(crate::google_workspace::native_confirmation::HostContext::new(
            &context.home, &context.agent, snapshot.email.clone(),
        ).map_err(|m| Refusal::hard("native_confirmation_unavailable", m))?);
        snapshot.validate(&context.home, &context.agent)?;
    }
    // The owner selector forces CacheMode::Bypass in the existing resolver.
    #[cfg(test)]
    if crate::google_identity::route_test_probe::refuse_work(&context.home, crate::google_identity::route_test_probe::Work::Token) {
        return Err(changed());
    }
    let scope = context.assertion_scope()?;
    let scopes: Vec<&str> = scope.split_whitespace().collect();
    let resolved = crate::google_identity::resolve::resolve_employee_scoped(&context.home,
        &context.agent, &context.requester, context.people.as_ref(), context.purpose.scope_key(), &scopes).await?;
    snapshot.validate(&context.home, &context.agent)?;
    let crate::google_identity::resolve::Resolved::Delegated { token, email } = resolved
        else { return Err(changed()); };
    if email != snapshot.email { return Err(changed()); }
    let material = Resolved { access: zeroize::Zeroizing::new(token.access_token), subject: email };
    let permit = Permit { context: &context, snapshot, progress };
    let output = Bearer { material: &material, permit }.execute_native(&template).await
        .map_err(|e| e.into_refusal())?;
    snapshot.validate(&context.home, &context.agent).map_err(|_| Refusal::hard(
        "result_unconfirmed", "Google 結果無法確認；請先檢查 Google，勿直接重試。"))?;
    Ok(output.into_value())
}

#[cfg(test)]
#[path = "employee_tests.rs"]
mod tests;

/// The isolation bridge has already verified and pinned the live capability,
/// requester and source before approval. This path never invents a key reference.
pub(crate) async fn execute_isolated(home: &Path, agent: &str, requester: RequesterIdentity,
    people: Box<dyn People>, tool: &str, args: &Value, snapshot: &OwnerSnapshot,
    tenant: Option<TenantPin>, progress: &std::sync::atomic::AtomicUsize) -> Result<Value, Refusal> {
    let context = GoogleCallContext::employee(home, agent, requester, people, tool, args, None, None)?
        .pin_tenant(tenant)?;
    match tokio::time::timeout(std::time::Duration::from_secs(120), execute(context, snapshot, args, progress)).await {
        Ok(result) => result,
        Err(_) => Err(Refusal::hard(if progress.load(std::sync::atomic::Ordering::SeqCst) == 0 { "not_sent" } else { "result_unconfirmed" },
            "Google 操作逾時；若已送出，結果無法確認，請先檢查 Google。")),
    }
}
