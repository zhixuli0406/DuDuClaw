//! Concrete account-bound native connectors. Credential lookup is an exact
//! gateway-owned account operation; no legacy OAuth/home selection is called.
use std::{collections::BTreeMap, future::Future, path::{Path, PathBuf}, pin::Pin, sync::{Arc, RwLock}};
use duduclaw_mcp_boundary::{tool_policy::AuthorizedToolCall, VerificationError};
use serde::Serialize;
use serde_json::{Value, json};
use zeroize::Zeroizing;
use crate::isolation_connectors::{BoundConnectorExecutor, SelectedConnector, provider_for_tool};

const REFUSED: &str = "無法確認這個連線帳號的授權，已停止操作。";
const REMOTE_FAILED: &str = "無法確認連線操作的結果；若有寫入，請先到原服務檢查，勿直接重試。";

/// No Debug/Serialize implementation: credentials never enter output or logs.
pub struct AccountCredential {
    account: String,
    provider: String,
    token: Zeroizing<String>,
    expires_at: u64,
}
impl AccountCredential {
    pub fn new(account: String, provider: String, token: String, expires_at: u64) -> Result<Self, VerificationError> {
        if account.is_empty() || account.len() > 128 || token.is_empty() || token.len() > 16384
            || !token.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'+' | b'/' | b'='))
            || account.chars().any(char::is_control)
            || !matches!(provider.as_str(), "google" | "notion" | "github") {
            return Err(VerificationError::Invalid);
        }
        Ok(Self { account, provider, token: Zeroizing::new(token), expires_at })
    }
}

/// Production OAuth refresh adapters may implement this interface, but must
/// retain the exact account identity through refresh. The executor independently
/// verifies returned account/provider/expiry after this await.
pub trait AccountCredentialProvider: Send + Sync {
    fn acquire<'a>(&'a self, account: &'a str, provider: &'a str)
        -> Pin<Box<dyn Future<Output = Result<AccountCredential, VerificationError>> + Send + 'a>>;
}

/// Concrete boot-populated credential vault. Missing/expired accounts refuse;
/// operator refresh replaces only the same account/provider entry.
#[derive(Default)]
pub struct AccountCredentialVault { entries: RwLock<BTreeMap<String, AccountCredential>> }
impl AccountCredentialVault {
    pub fn install(&self, credential: AccountCredential) -> Result<(), VerificationError> {
        let mut entries = self.entries.write().map_err(|_| VerificationError::Unavailable)?;
        if entries.get(&credential.account).is_some_and(|old| old.provider != credential.provider) {
            return Err(VerificationError::BindingMismatch);
        }
        entries.insert(credential.account.clone(), credential);
        Ok(())
    }
    /// Revoke the BindingStore account first to fence in-flight requests, then
    /// remove its material here. Removal alone only prevents future acquisition.
    pub fn remove(&self, account: &str) -> Result<(), VerificationError> {
        self.entries.write().map_err(|_| VerificationError::Unavailable)?.remove(account);
        Ok(())
    }
}
impl AccountCredentialProvider for AccountCredentialVault {
    fn acquire<'a>(&'a self, account: &'a str, provider: &'a str)
        -> Pin<Box<dyn Future<Output = Result<AccountCredential, VerificationError>> + Send + 'a>> {
        Box::pin(async move {
            let entries = self.entries.read().map_err(|_| VerificationError::Unavailable)?;
            let entry = entries.get(account).filter(|e| e.provider == provider).ok_or(VerificationError::Unavailable)?;
            AccountCredential::new(entry.account.clone(), entry.provider.clone(), entry.token.to_string(), entry.expires_at)
        })
    }
}

async fn credential(selected: &SelectedConnector, provider: &dyn AccountCredentialProvider)
    -> Result<AccountCredential, VerificationError> {
    selected.recheck()?;
    let credential = provider.acquire(selected.account(), selected.provider()).await?;
    selected.recheck()?;
    if credential.account != selected.account() || credential.provider != selected.provider()
        || duduclaw_mcp_boundary::authority::now_seconds()? >= credential.expires_at {
        return Err(VerificationError::BindingMismatch);
    }
    Ok(credential)
}

tokio::task_local! {
    static BOUND_CONNECTOR: Arc<SelectedConnector>;
    static CREDENTIAL_EXPIRY: u64;
    static CLIENT_POLICY: Arc<NativeClientPolicy>;
}
pub(crate) fn active() -> bool { BOUND_CONNECTOR.try_with(|_| ()).is_ok() }
pub(crate) fn client_builder() -> reqwest::ClientBuilder {
    let builder = reqwest::Client::builder();
    if active() { builder.no_proxy().retry(reqwest::retry::never()) } else { builder }
}
pub(crate) fn redirects() -> reqwest::redirect::Policy {
    if active() { reqwest::redirect::Policy::none() } else { reqwest::redirect::Policy::default() }
}
/// Called by each native transport immediately before every network effect,
/// including pagination/secondary reads. Isolated scopes never auto-retry.
pub(crate) fn before_request(provider: &str, url: &str) -> Result<(), &'static str> {
    BOUND_CONNECTOR.try_with(|selected| {
        selected.recheck().map_err(|_| REFUSED)?;
        CLIENT_POLICY.try_with(|policy| policy.check(provider)).map_err(|_| REFUSED)?.map_err(|_| REFUSED)?;
        let now = duduclaw_mcp_boundary::authority::now_seconds().map_err(|_| REFUSED)?;
        if !CREDENTIAL_EXPIRY.try_with(|expires| now < *expires).unwrap_or(false) { return Err(REFUSED); }
        if selected.provider() != provider { return Err(REFUSED); }
        let url = url::Url::parse(url).map_err(|_| REFUSED)?;
        if url.scheme() != "https" || url.port_or_known_default() != Some(443)
            || !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
            return Err(REFUSED);
        }
        let allowed = match provider {
            "google" => matches!(url.host_str(), Some("gmail.googleapis.com" | "www.googleapis.com" |
                "sheets.googleapis.com" | "forms.googleapis.com" | "tasks.googleapis.com" |
                "docs.googleapis.com" | "slides.googleapis.com" | "chat.googleapis.com")),
            "notion" => url.host_str() == Some("api.notion.com"),
            "github" => url.host_str() == Some("api.github.com"),
            _ => false,
        };
        if allowed { selected.recheck().map_err(|_| REFUSED) } else { Err(REFUSED) }
    }).unwrap_or(Ok(()))
}

struct FixedGoogleAccess<'a> {
    selected: &'a SelectedConnector,
    credential: &'a AccountCredential,
    tool: &'a str,
}
#[async_trait::async_trait]
impl crate::google_identity::native_tools::NativeGoogleAccess for FixedGoogleAccess<'_> {
    async fn backend(&self, home: &Path, tool: &str) -> Result<crate::google_workspace::GoogleBackend, String> {
        self.token(home, tool).await.map(crate::google_workspace::GoogleBackend::Direct)
    }
    async fn token(&self, _home: &Path, tool: &str) -> Result<String, String> {
        self.selected.recheck().map_err(|_| REFUSED.to_owned())?;
        if tool != self.tool || self.selected.provider() != "google"
            || duduclaw_mcp_boundary::authority::now_seconds().map_err(|_| REFUSED.to_owned())? >= self.credential.expires_at {
            return Err(REFUSED.into());
        }
        Ok(self.credential.token.to_string())
    }
}

struct NativeClientPolicy { home: PathBuf, agent: String }
impl NativeClientPolicy {
    fn check(&self, provider: &str) -> Result<(), VerificationError> {
        if provider == "notion" { return Ok(()); }
        // Read current effective settings every time, including between native
        // pagination hops. An unreadable/invalid config is never a default.
        let raw = std::fs::read_to_string(self.home.join("config.toml")).map_err(|_| VerificationError::Unavailable)?;
        let table: toml::Table = toml::from_str(&raw).map_err(|_| VerificationError::Invalid)?;
        let key = match provider { "google" => "google_workspace", "github" => "github", _ => return Err(VerificationError::Invalid) };
        if table.get("integrations").and_then(toml::Value::as_table).and_then(|t| t.get(key))
            .and_then(toml::Value::as_bool) != Some(true) { return Err(VerificationError::Unavailable); }
        if provider == "github" {
            return if crate::github_workspace::integration_enabled(&self.home) { Ok(()) }
                else { Err(VerificationError::Unavailable) };
        }
        if !crate::google_workspace::integration_enabled(&self.home) { return Err(VerificationError::Unavailable); }
        let agent_dir = crate::ephemeral::resolve_agent_dir(&self.home, &self.agent).or_else(||
            (!crate::ephemeral::is_ephemeral_id(&self.agent)).then(|| self.home.join("agents").join(&self.agent)))
            .ok_or(VerificationError::Unavailable)?;
        let agent = duduclaw_core::agent_toml::read_effective_text(&agent_dir)
            .map_err(|_| VerificationError::Unavailable)?;
        let effective = duduclaw_core::google_identity::combine(
            &duduclaw_core::google_identity::parse_agent_section(&agent),
            &duduclaw_core::google_identity::parse_home_config(&raw));
        // SelectedConnector rechecks the installed product policy, including
        // personal requester/consent and shared-home selection, before every I/O.
        // Do not erase Personal parity by imposing the old home-only selector.
        if matches!(effective.effective, duduclaw_core::google_identity::Effective::Disabled
            | duduclaw_core::google_identity::Effective::Invalid(_)) {
            return Err(VerificationError::Unavailable);
        }
        Ok(())
    }
}

pub struct NativeConnectorExecutor {
    policy: Arc<NativeClientPolicy>,
    credentials: Arc<dyn AccountCredentialProvider>,
}
impl NativeConnectorExecutor {
    pub fn new(home: PathBuf, agent: String, credentials: Arc<dyn AccountCredentialProvider>) -> Result<Self, VerificationError> {
        if !home.is_absolute() || !duduclaw_core::is_valid_agent_id(&agent)
            || duduclaw_core::is_reserved_agent_id(&agent) || agent == duduclaw_core::UNTRUSTED_AGENT_ID {
            return Err(VerificationError::Invalid);
        }
        Ok(Self { policy: Arc::new(NativeClientPolicy { home, agent }), credentials })
    }
    async fn run(&self, selected: &SelectedConnector, call: &AuthorizedToolCall, token: AccountCredential) -> Result<Value, VerificationError> {
        let tool = call.name();
        let args = call.arguments();
        let result = match tool {
            "google_status" | "notion_status" | "github_status" => tool_text(json!({"account":selected.account(),
                "provider":selected.provider(),"configured":true,"provider_verified":false})),
            "gmail_search" | "gmail_read" | "gmail_create_draft" | "calendar_list_events" | "calendar_create_event" |
            // Google Chat reads keep native_legacy's admin opt-in gate, the
            // Direct-OAuth-only backend rule and its content-free audit.
            "google_chat_spaces" | "google_chat_messages" |
            "sheets_read" | "sheets_append" | "forms_get" | "forms_list_responses" | "gtasks_lists" | "gtasks_list" |
            "gtasks_create" | "gtasks_complete" | "drive_search" | "drive_read" | "docs_read" | "docs_append" | "slides_read" => {
                let access = FixedGoogleAccess { selected, credential: &token, tool };
                let result = crate::google_workspace::native_legacy::handle(tool, args, &self.policy.home, &access).await;
                if result.get("isError").and_then(Value::as_bool) == Some(true) { remote_error() } else { result }
            }
            "notion_search" => render(crate::notion_workspace::notion_search(&token.token,
                required(args, "query")?, limit(args)).await),
            "notion_page_read" => render(crate::notion_workspace::notion_page_read(&token.token,
                identifier(args, "page_id")?).await),
            "notion_page_append" => render(crate::notion_workspace::notion_page_append(&token.token,
                identifier(args, "page_id")?, required(args, "text")?).await),
            "github_search_issues" => render(crate::github_workspace::github_search_issues(&token.token,
                required(args, "query")?, limit(args)).await),
            "github_issue_read" | "github_pr_read" | "github_issue_comment" => {
                let owner = identifier(args, "owner")?;
                let repo = identifier(args, "repo")?;
                let number = args.get("number").and_then(Value::as_u64)
                    .or_else(|| args.get("number").and_then(Value::as_str).and_then(|s| s.parse().ok()))
                    .filter(|n| *n > 0).ok_or(VerificationError::Invalid)?;
                match tool {
                    "github_issue_read" => render(crate::github_workspace::github_issue_read(&token.token, owner, repo, number).await),
                    "github_pr_read" => render(crate::github_workspace::github_pr_read(&token.token, owner, repo, number).await),
                    _ => render(crate::github_workspace::github_issue_comment(&token.token, owner, repo, number,
                        required(args, "body")?).await),
                }
            }
            _ => return Err(VerificationError::Unavailable),
        };
        if selected.recheck().is_err() || self.policy.check(selected.provider()).is_err()
            || duduclaw_mcp_boundary::authority::now_seconds().map_or(true, |now| now >= token.expires_at) {
            return Ok(remote_error());
        }
        // A remote body is not allowed to echo credential bytes into a tool
        // result. The existing native renderer never intentionally returns them.
        let encoded = serde_json::to_string(&result).map_err(|_| VerificationError::Unavailable)?;
        if encoded.len() > 512 * 1024 || encoded.contains(token.token.as_str()) { return Ok(remote_error()); }
        Ok(result)
    }
}
impl BoundConnectorExecutor for NativeConnectorExecutor {
    fn supports(&self, tool: &str) -> bool {
        matches!(provider_for_tool(tool), Some("google" | "notion" | "github"))
    }
    fn execute<'a>(&'a self, selected: SelectedConnector, call: AuthorizedToolCall)
        -> Pin<Box<dyn Future<Output = Result<Value, VerificationError>> + Send + 'a>> {
        Box::pin(async move {
            selected.verify_call(&call)?;
            if !self.supports(call.name()) || selected.context().record() != call.context().record()
                || provider_for_tool(call.name()) != Some(selected.provider())
                || !call.resources().iter().any(|r| matches!(r,
                    duduclaw_isolation::capability::ResourceGrant::Connector { account, binding }
                    if account.as_str() == selected.account() && binding == selected.binding())) {
                return Err(VerificationError::BindingMismatch);
            }
            selected.recheck()?;
            self.policy.check(selected.provider())?;
            if selected.provider() == "google" {
                validate_google_recipients(call.arguments())?;
            }
            let selected = Arc::new(selected);
            let token = credential(&selected, self.credentials.as_ref()).await?;
            self.policy.check(selected.provider())?;
            let expires_at = token.expires_at;
            BOUND_CONNECTOR.scope(selected.clone(), CREDENTIAL_EXPIRY.scope(expires_at,
                CLIENT_POLICY.scope(self.policy.clone(), self.run(&selected, &call, token)))).await
        })
    }
}
fn validate_google_recipients(args: &Value) -> Result<(), VerificationError> {
    for key in ["to", "cc", "attendees"] {
        if let Some(value) = args.get(key) {
            let value = value.as_str().ok_or(VerificationError::Invalid)?;
            if value.contains(',') || value.contains(';') || value.chars().any(char::is_control) {
                return Err(VerificationError::Invalid);
            }
        }
    }
    Ok(())
}
fn required<'a>(args: &'a Value, key: &str) -> Result<&'a str, VerificationError> {
    args.get(key).and_then(Value::as_str).filter(|s| !s.trim().is_empty() && s.len() <= 65536)
        .ok_or(VerificationError::Invalid)
}
fn identifier<'a>(args: &'a Value, key: &str) -> Result<&'a str, VerificationError> {
    let value = required(args, key)?;
    if value.len() > 256 || !value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        || matches!(value, "." | "..") { return Err(VerificationError::Invalid); }
    Ok(value)
}
fn limit(args: &Value) -> u32 {
    args.get("max_results").and_then(Value::as_u64).unwrap_or(10).clamp(1, 25) as u32
}
fn tool_text(value: Value) -> Value { json!({"content":[{"type":"text","text":value.to_string()}]}) }
fn remote_error() -> Value { json!({"content":[{"type":"text","text":REMOTE_FAILED}],"isError":true}) }
fn render<T: Serialize, E>(result: Result<T, E>) -> Value {
    match result {
        Ok(value) => match serde_json::to_value(value) { Ok(value) => tool_text(value), Err(_) => remote_error() },
        Err(_) => remote_error(),
    }
}

#[cfg(test)]
#[path = "isolation_connector_clients_tests.rs"]
mod tests;
