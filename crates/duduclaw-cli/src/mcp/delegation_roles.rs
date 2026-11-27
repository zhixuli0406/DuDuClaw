//! No local role/principal inference: an opaque runtime reference is resolved
//! by the gateway over the existing signed, non-redirecting loopback protocol.
use std::path::Path;
use duduclaw_core::delegation_policy::{OwnerRoleView, OwnerRoleLookupError, DelegationRequesterRole,
    DelegationConfigRead, DelegationContext, DelegationBoundaries, DelegationAllowSource};
use duduclaw_gateway::delegation_roles::RoleFacts;

#[derive(Clone)]
pub(crate) struct LoopbackOwnerRoles(Result<RoleFacts, OwnerRoleLookupError>);
impl OwnerRoleView for LoopbackOwnerRoles {
    fn avatar_owner_role(&self, agent: &str) -> Result<Option<String>, OwnerRoleLookupError> {
        self.0.as_ref().map_err(|_| OwnerRoleLookupError)?.avatar_owner_role(agent)
    }
    fn requester_role(&self) -> Result<Option<DelegationRequesterRole>, OwnerRoleLookupError> {
        self.0.as_ref().map_err(|_| OwnerRoleLookupError)?.requester_role()
    }
}
impl LoopbackOwnerRoles {
    pub(crate) fn facts(&self) -> Option<RoleFacts> { self.0.as_ref().ok().cloned() }
    pub(crate) fn requester_id(&self) -> Option<&str> {
        self.0.as_ref().ok()?.requester_account_id.as_deref()
    }
    pub(crate) async fn load(home: &Path, agent: &str, read: &DelegationConfigRead) -> Self {
        if read.overrides.unrestricted_roles.is_empty() { return Self(Err(OwnerRoleLookupError)); }
        #[cfg(test)]
        if let Ok((h, a, roles)) = TEST_ROLES.try_with(Clone::clone) {
            if h == home && a == agent { return roles; }
        }
        let result = match credentials(agent) {
            Some(credentials) => fetch(home, credentials, None).await,
            None => Err(OwnerRoleLookupError),
        };
        Self(result)
    }
    /// Locked rechecks must refresh gateway facts too. Capture the connection
    /// identity before entering the thread (isolation identity is task-local).
    pub(crate) fn load_blocking(home: &Path, agent: &str, read: &DelegationConfigRead) -> Self {
        if read.overrides.unrestricted_roles.is_empty() { return Self(Err(OwnerRoleLookupError)); }
        #[cfg(test)]
        if let Ok((h, a, roles)) = TEST_ROLES.try_with(Clone::clone) {
            if h == home && a == agent { return roles; }
        }
        let Some(credentials) = credentials(agent) else { return Self(Err(OwnerRoleLookupError)); };
        let home = home.to_owned();
        Self(std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread().enable_all().build()
                .map_err(|_| OwnerRoleLookupError)?.block_on(fetch(&home, credentials, None))
        }).join().unwrap_or(Err(OwnerRoleLookupError)))
    }
}
struct Credentials { agent: String, context: String, key: String, token: String }
fn credentials(agent: &str) -> Option<Credentials> {
    if !duduclaw_core::is_valid_agent_id(agent) || duduclaw_core::is_reserved_agent_id(agent) { return None; }
    let env = |key| crate::mcp_isolation_identity::var(key).ok().filter(|v| !v.is_empty());
    let context = env(duduclaw_gateway::on_behalf_of::context::ENV_CONTEXT)?;
    if context.len() != 64 || !context.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) { return None; }
    Some(Credentials { agent: agent.into(), context, key: env(duduclaw_core::ENV_MCP_API_KEY)?,
        token: env(duduclaw_core::ENV_AGENT_TOKEN)? })
}
async fn fetch(home: &Path, who: Credentials, record: Option<(&str, &str, &str)>) -> Result<RoleFacts, OwnerRoleLookupError> {
    let mut body = serde_json::json!({"context":who.context});
    if let Some((kind, id, target)) = record { body["record"] = serde_json::json!({"kind":kind,"id":id,"target":target}); }
    let bytes = serde_json::to_vec(&body).map_err(|_| OwnerRoleLookupError)?;
    let (port, _) = duduclaw_core::gateway_port_for_home(home);
    let client = reqwest::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(10)).build().map_err(|_| OwnerRoleLookupError)?;
    let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs()).unwrap_or(0);
    let nonce: [u8; 16] = rand::random();
    let mut request = client.post(format!("http://127.0.0.1:{port}{}",
        duduclaw_gateway::on_behalf_of::route::DELEGATION_ROLES_ROUTE)).header("Content-Type", "application/json");
    for (key, value) in super::computer_use_client::signed_headers(&who.key, &who.agent, &who.token, timestamp, &nonce, &bytes) {
        request = request.header(key, value);
    }
    let mut response = request.body(bytes).send().await.map_err(|_| OwnerRoleLookupError)?;
    if response.status() != reqwest::StatusCode::OK { return Err(OwnerRoleLookupError); }
    let mut output = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) if output.len() + chunk.len() <= 4096 => output.extend_from_slice(&chunk),
            Ok(None) => break,
            _ => return Err(OwnerRoleLookupError),
        }
    }
    decode(&who.agent, &output)
}
fn decode(agent: &str, bytes: &[u8]) -> Result<RoleFacts, OwnerRoleLookupError> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Answer { ok: bool, facts: RoleFacts }
    let answer: Answer = serde_json::from_slice(bytes).map_err(|_| OwnerRoleLookupError)?;
    if !answer.ok || answer.facts.agent != agent
        || (answer.facts.requester_role.is_some() && answer.facts.requester_account_id.as_deref()
            .and_then(duduclaw_core::HumanPrincipal::user).is_none()) {
        return Err(OwnerRoleLookupError);
    }
    Ok(answer.facts)
}

/// Shared by tools/list and dispatch: only the verified sender/turn can exempt
/// `none`. Target-specific boundaries still run in the handler before evaluation.
pub(crate) struct NonePolicy { disabled: bool, unrestricted: bool }
impl NonePolicy {
    pub(crate) fn blocks(&self, tool: &str) -> bool {
        // RM-183 grants delegation to existing employees. Employee creation,
        // ephemeral creation and team membership keep their original gates.
        let role_exemption = matches!(tool, "send_to_agent" | "spawn_agent" | "tasks_create"
            | "tasks_update" | "create_task" | "create_reminder" | "update_cron_task"
            | "run_cron_task");
        self.disabled && !(self.unrestricted && role_exemption)
    }
}
pub(crate) async fn none_policy(home: &Path, agent: &str) -> NonePolicy {
    let read = duduclaw_core::delegation_policy::read_delegation_config(home);
    let disabled = read.rules.policy == duduclaw_core::DelegationPolicy::Disabled;
    let unrestricted = if disabled {
        let roles = LoopbackOwnerRoles::load(home, agent, &read).await;
        unrestricted(home, agent, &read, &roles)
    } else { false };
    NonePolicy { disabled, unrestricted }
}
#[cfg(test)]
async fn none_blocks(home: &Path, agent: &str) -> bool {
    none_policy(home, agent).await.blocks("send_to_agent")
}
fn unrestricted(home: &Path, agent: &str, read: &DelegationConfigRead, roles: &LoopbackOwnerRoles) -> bool {
    // This is a sender-only eligibility probe, never a target authorization.
    // Use a distinct sentinel and require exactly UnrestrictedRole, not a pair/mode.
    duduclaw_core::delegation_policy::evaluate_delegation(&read.rules, &duduclaw_core::MapOrgView::new(),
        agent, "__delegation_eligibility__", &DelegationContext {
            overrides: &read.overrides, owner_roles: Some(roles), extra_system_senders: &[],
            boundaries: DelegationBoundaries { personal_target_allowed: true, main_target_allowed: true,
                can_send_cross_agent: duduclaw_gateway::delegation_roles::can_send(home, agent) },
        }) == Ok(DelegationAllowSource::UnrestrictedRole)
}

pub(crate) fn audit_allow(home: &Path, sender: &str, target: &str, source: DelegationAllowSource,
    roles: &LoopbackOwnerRoles, path: &str) {
    if !matches!(source, DelegationAllowSource::UnrestrictedRole | DelegationAllowSource::OneWayPair) { return; }
    duduclaw_security::audit::append_tool_call_with_extras(home, sender, "delegation_allowed",
        &format!("{path}: '{sender}' -> '{target}' allowed ({})", source.as_str()), true, &[
            ("sender", serde_json::json!(sender)), ("target", serde_json::json!(target)),
            ("source", serde_json::json!(source.as_str())), ("path_kind", serde_json::json!(path)),
            ("requester_account_id", serde_json::json!(if source == DelegationAllowSource::UnrestrictedRole {
                roles.requester_id() } else { None })),
        ]);
}
#[cfg(test)]
mod tests;

#[cfg(test)]
tokio::task_local! {
    static TEST_ROLES: (std::path::PathBuf, String, LoopbackOwnerRoles);
}

/// Register a one-shot message receipt before enqueue. Failure leaves the
/// queue gate to evaluate ordinary delegation policy.
pub(crate) async fn record_initiator(home: &Path, kind: &str, agent: &str, target: &str, id: &str) {
    let read = duduclaw_core::delegation_policy::read_delegation_config(home);
    if read.overrides.unrestricted_roles.is_empty() { return; }
    let result = match credentials(agent) {
        Some(who) => fetch(home, who, Some((kind, id, target))).await.map(|_| ()),
        None => Err(OwnerRoleLookupError),
    };
    if result.is_err() {
        tracing::warn!(agent, target, "Message initiator could not be verified; queue role pass unavailable");
    }
}
