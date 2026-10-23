//! Per-employee Google identity, phase B2a: an AI employee acts as one
//! configured Workspace colleague through a domain-wide-delegation service
//! account.
//!
//! The selector lives in `agent.toml [google]` (parsed by
//! [`duduclaw_core::google_identity`], the single parse point shared with
//! the MCP server). The `home` selector keeps today's in-process path and
//! never comes here. A person identity (`delegate:<email>`) is only ever
//! minted inside the gateway, the one process that holds the gateway lock and
//! may unseal the delegation key:
//!
//! - [`route`]: `POST /api/internal/google-token`, the signed loopback route
//!   the MCP server asks for a short-lived access token.
//! - [`resolve`]: the decision (selector → requester gate → domain →
//!   colleague still active → protection gate → sealed key → mint), with the
//!   explicit `fallback = "home"` escape hatch for the cases the design marks
//!   fallback-eligible. A refused requester or a configuration error never
//!   falls back.
//! - [`delegation_key`]: the service-account key, sealed with the B2c
//!   protected key (`pk:v1`), never readable as a file by an employee.
//! - [`requester_gate`]: who may, through this employee, use the account.
//! - [`check`]: the dashboard's "test connection" (mints for the subject,
//!   sends nothing).
//!
//! Every refusal has a closed code ([`Refusal::code`]) and a zh-TW message
//! that names who can fix it.

pub mod check;
pub(crate) mod credential_registry;
pub mod delegation_key;
pub mod execute;
pub mod mailbox;
pub(crate) mod mcp_mount;
pub mod native_tools;
pub mod requester_gate;
pub mod resolve;
pub mod route;

#[cfg(test)]
mod tests;

use std::path::Path;

use duduclaw_security::credential_protection::ProtectedKey;

pub use duduclaw_core::google_identity::{
    Effective, EffectiveIdentity, Fallback, Requesters, Selector,
};

/// A refused person identity. `code` is a closed set (audit + tests);
/// `fallback_eligible` marks the cases `fallback = "home"` may cover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub code: &'static str,
    pub message_zh: String,
    pub fallback_eligible: bool,
}

impl Refusal {
    pub fn hard(code: &'static str, message_zh: impl Into<String>) -> Self {
        Self {
            code,
            message_zh: message_zh.into(),
            fallback_eligible: false,
        }
    }

    pub fn soft(code: &'static str, message_zh: impl Into<String>) -> Self {
        Self {
            code,
            message_zh: message_zh.into(),
            fallback_eligible: true,
        }
    }
}

/// Every refusal code this module emits.
pub const REFUSAL_CODES: &[&str] = &[
    "no_verified_caller",
    "disabled",
    "not_available",
    "config_invalid",
    "requester_refused",
    "person_inactive",
    "credential_protection",
    "delegate_unavailable",
    "delegate_rejected",
    "google_unreachable",
    "gateway_unreachable",
];

/// Message when the protected key is not ready (B2c design §11).
const NOT_PROTECTED: &str = "這台 DuDuClaw 的憑證還放在一般檔案裡，不能讓 AI 員工代表特定的人使用 Google 帳號。請管理員到「安全」頁把憑證保護改放系統金鑰圈。";

/// The B2c gate: the protected key, or a refusal. Blocking (keychain IPC):
/// call from the blocking pool.
pub fn protection_gate(home: &Path) -> Result<ProtectedKey, Refusal> {
    gate_with(&crate::handlers::credential_protection_rpc::protection_for(
        home,
    ))
}

/// [`protection_gate`] over a given protection (tests use other stores).
pub fn gate_with(
    cp: &duduclaw_security::credential_protection::CredentialProtection,
) -> Result<ProtectedKey, Refusal> {
    cp.person_identity_gate().map_err(|e| match e.code {
        "not_enabled" => Refusal::hard("credential_protection", NOT_PROTECTED),
        _ => Refusal::hard(
            "credential_protection",
            format!(
                "憑證保護目前不能使用，所以 AI 員工不能代表特定的人使用 Google 帳號：{}",
                e.message_zh
            ),
        ),
    })
}

/// [`protection_gate`] on the blocking pool.
pub async fn protection_gate_async(home: &Path) -> Result<ProtectedKey, Refusal> {
    let home = home.to_path_buf();
    tokio::task::spawn_blocking(move || protection_gate(&home))
        .await
        .unwrap_or_else(|_| {
            Err(Refusal::hard(
                "credential_protection",
                "憑證保護檢查中斷，請稍後再試。",
            ))
        })
}

/// The home service account's `subject` (for the default domain rule), if
/// the home path uses one.
pub fn home_subject(home: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(home.join("config.toml")).ok()?;
    crate::google_service_account::parse_config(&raw, home)
        .ok()
        .flatten()
        .map(|c| c.subject)
}

/// One row in `tool_calls.jsonl` for a gateway decision (`google_token`).
/// Never carries a token or key material.
pub fn audit_issue(
    home: &Path,
    agent: &str,
    tool: &str,
    requester_kind: &str,
    outcome: &Result<resolve::Resolved, Refusal>,
) {
    use serde_json::json;
    let (success, identity, account, fallback, reason) = match outcome {
        Ok(resolve::Resolved::Delegated { email, .. }) => {
            (true, "delegate", email.as_str(), false, "")
        }
        Ok(resolve::Resolved::UseHome { fallback, .. }) => (true, "home", "", *fallback, ""),
        Err(r) => (false, "delegate", "", false, r.code),
    };
    let summary = format!(
        "tool={tool} identity={identity}{}",
        if success { "" } else { " refused" }
    );
    duduclaw_security::audit::append_tool_call_with_extras(
        home,
        agent,
        "google_token",
        &summary,
        success,
        &[
            ("google_identity", json!(identity)),
            ("google_account", json!(account)),
            ("requester_kind", json!(requester_kind)),
            ("google_tool", json!(tool)),
            ("fallback", json!(fallback)),
            ("reason", json!(reason)),
        ],
    );
}

/// Why a gateway-side Google call (Agent Mail) has no backend.
#[derive(Debug)]
pub enum GatewayBackendError {
    /// The employee's identity refused (no fallback to home unless its
    /// `fallback = "home"` allowed it).
    Refused(Refusal),
    /// The home account itself failed (unchanged path).
    Home(crate::google_workspace::GoogleAuthError),
}

/// The Google backend for a path the gateway runs itself on behalf of
/// `agent` (`None`: no owning employee, the home default). The requester is
/// a system turn of `kind` on behalf of nobody.
pub async fn gateway_backend(
    home: &Path,
    agent: Option<&str>,
    kind: &str,
) -> Result<crate::google_workspace::GoogleBackend, GatewayBackendError> {
    gateway_backend_with_account(home, agent, kind)
        .await
        .map(|(backend, _)| backend)
}

/// The resolved receipt identity accompanies the backend so a fallback can
/// never stamp company mail as the delegate's personal data.
pub async fn gateway_backend_with_account(
    home: &Path,
    agent: Option<&str>,
    kind: &str,
) -> Result<(crate::google_workspace::GoogleBackend, Option<String>), GatewayBackendError> {
    use crate::google_workspace::{GoogleBackend, resolve_backend};
    let agent = agent.filter(|a| !a.is_empty());
    let eff = duduclaw_core::google_identity::effective_for(home, agent);
    match (&eff.effective, agent) {
        (Effective::Home, _) => resolve_backend(home)
            .await
            .map(|b| (b, None))
            .map_err(GatewayBackendError::Home),
        (Effective::Delegate { .. }, Some(agent)) => {
            let requester = duduclaw_knowledge::requester::RequesterIdentity::System {
                kind: kind.to_string(),
                on_behalf_of: None,
            };
            let people = requester_gate::people_for(home, None);
            let outcome = resolve::resolve_person(home, agent, &requester, people.as_ref()).await;
            audit_issue(home, agent, kind, requester.kind(), &outcome);
            match outcome {
                Ok(resolve::Resolved::Delegated { token, email }) => {
                    Ok((GoogleBackend::Direct(token.access_token), Some(email)))
                }
                Ok(resolve::Resolved::UseHome { .. }) => resolve_backend(home)
                    .await
                    .map(|b| (b, None))
                    .map_err(GatewayBackendError::Home),
                Err(r) => Err(GatewayBackendError::Refused(r)),
            }
        }
        (other, _) => Err(GatewayBackendError::Refused(
            resolve::refusal_for(other)
                .unwrap_or_else(|| Refusal::hard("config_invalid", "設定錯誤")),
        )),
    }
}

/// When `agent`'s identity is a delegate, the colleague's email: the
/// mailbox the gateway reads for it is that person's, so the mail pages
/// narrow to the admin and that person.
pub fn delegate_email_of(home: &Path, agent: Option<&str>) -> Option<String> {
    let agent = agent.filter(|a| !a.is_empty())?;
    match duduclaw_core::google_identity::effective_for(home, Some(agent)).effective {
        Effective::Delegate { email } => Some(email),
        _ => None,
    }
}
