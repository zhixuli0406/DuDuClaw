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
    /// A second zeroizing copy (the token moves into the new wrapper).
    fn duplicate(&self) -> Result<Self, VerificationError> {
        Self::new(self.account.clone(), self.provider.clone(), self.token.to_string(), self.expires_at)
    }
}

/// Production OAuth refresh adapters may implement this interface, but must
/// retain the exact account identity through refresh. The executor independently
/// verifies returned account/provider/expiry after this await.
pub trait AccountCredentialProvider: Send + Sync {
    fn acquire<'a>(&'a self, account: &'a str, provider: &'a str)
        -> Pin<Box<dyn Future<Output = Result<AccountCredential, VerificationError>> + Send + 'a>>;
    /// v1.81 F1: acquisition for the catalog route generation the selection
    /// pinned. Generation-keyed material ([`AccountCredentialVault`],
    /// [`VaultedCredentials`]) serves only that generation; live sources that
    /// keep no in-process material ignore it.
    fn acquire_at<'a>(&'a self, account: &'a str, provider: &'a str, _generation: u64)
        -> Pin<Box<dyn Future<Output = Result<AccountCredential, VerificationError>> + Send + 'a>> {
        self.acquire(account, provider)
    }
}

/// Process credential vault, keyed by (catalog account, route generation)
/// (v1.81 F1 / SR3 Info 5). Material of a generation that ended (revoke,
/// advance, relink) is never served for the new one: a lookup must name the
/// exact generation the selection pinned. One generation per account is kept;
/// installing an older generation than the stored one refuses, a newer one
/// replaces it. Missing/expired material refuses.
#[derive(Default)]
pub struct AccountCredentialVault { entries: RwLock<BTreeMap<String, (u64, AccountCredential)>> }
impl AccountCredentialVault {
    pub fn install(&self, credential: AccountCredential, generation: u64) -> Result<(), VerificationError> {
        if generation == 0 { return Err(VerificationError::Invalid); }
        let mut entries = self.entries.write().map_err(|_| VerificationError::Unavailable)?;
        if entries.get(&credential.account).is_some_and(|(stored, old)|
            old.provider != credential.provider || *stored > generation) {
            return Err(VerificationError::BindingMismatch);
        }
        entries.insert(credential.account.clone(), (generation, credential));
        Ok(())
    }
    /// Deactivate the catalog route first to fence in-flight requests, then
    /// remove its material here (`isolation_connector_lifecycle::revoke` does
    /// both, in that order). Removal alone only prevents future acquisition.
    /// Every generation of the account goes.
    pub fn remove(&self, account: &str) -> Result<(), VerificationError> {
        self.entries.write().map_err(|_| VerificationError::Unavailable)?.remove(account);
        Ok(())
    }
    /// Drop the account's material unless it is for exactly `generation`.
    fn retain_generation(&self, account: &str, generation: u64) -> Result<(), VerificationError> {
        let mut entries = self.entries.write().map_err(|_| VerificationError::Unavailable)?;
        if entries.get(account).is_some_and(|(stored, _)| *stored != generation) { entries.remove(account); }
        Ok(())
    }
    /// The stored material for exactly (account, provider, generation).
    fn lookup(&self, account: &str, provider: &str, generation: u64) -> Result<AccountCredential, VerificationError> {
        let entries = self.entries.read().map_err(|_| VerificationError::Unavailable)?;
        let (_, entry) = entries.get(account)
            .filter(|(stored, entry)| *stored == generation && entry.provider == provider)
            .ok_or(VerificationError::Unavailable)?;
        entry.duplicate()
    }
}
impl AccountCredentialProvider for AccountCredentialVault {
    /// Without a pinned generation nothing is served.
    fn acquire<'a>(&'a self, _account: &'a str, _provider: &'a str)
        -> Pin<Box<dyn Future<Output = Result<AccountCredential, VerificationError>> + Send + 'a>> {
        Box::pin(async { Err(VerificationError::Unavailable) })
    }
    fn acquire_at<'a>(&'a self, account: &'a str, provider: &'a str, generation: u64)
        -> Pin<Box<dyn Future<Output = Result<AccountCredential, VerificationError>> + Send + 'a>> {
        Box::pin(async move { self.lookup(account, provider, generation) })
    }
}

/// Current route state for one account (production: the shared boot
/// [`crate::isolation_connectors::ConnectorCatalog`]). `Ok(None)`: unknown id.
pub trait RouteGeneration: Send + Sync {
    fn active_generation(&self, account: &str) -> Result<Option<u64>, VerificationError>;
}
impl RouteGeneration for crate::isolation_connectors::ConnectorCatalog {
    fn active_generation(&self, account: &str) -> Result<Option<u64>, VerificationError> {
        let Ok(account) = crate::isolation_connectors::AccountId::parse(account) else { return Ok(None) };
        Ok(self.route(&account)?.filter(|route| route.active).map(|route| route.generation))
    }
}

/// The executors' credential path in protected mode (F1): material lives in
/// the process [`AccountCredentialVault`] that boot also installs into the
/// connector lifecycle (C3 revoke / C4 events drop it after their fence).
/// A miss (or expired lease) acquires from the live `source` and stores the
/// result under the pinned generation. Before serving and after storing, the
/// catalog route must be active at exactly that generation; otherwise the
/// account's material is dropped and the acquisition refuses, so a revoke
/// racing the acquisition never leaves material behind.
pub struct VaultedCredentials {
    vault: Arc<AccountCredentialVault>,
    source: Arc<dyn AccountCredentialProvider>,
    routes: Arc<dyn RouteGeneration>,
}
impl VaultedCredentials {
    pub fn new(vault: Arc<AccountCredentialVault>, source: Arc<dyn AccountCredentialProvider>,
        routes: Arc<dyn RouteGeneration>) -> Self {
        Self { vault, source, routes }
    }
    fn route_at(&self, account: &str, generation: u64) -> Result<(), VerificationError> {
        let current = self.routes.active_generation(account);
        if matches!(current, Ok(Some(current)) if current == generation) { return Ok(()); }
        // Revoked/advanced/unknown/unreadable: never keep material for it.
        let _ = self.vault.remove(account);
        Err(match current { Err(_) => VerificationError::Unavailable, Ok(_) => VerificationError::BindingMismatch })
    }
}
impl AccountCredentialProvider for VaultedCredentials {
    fn acquire<'a>(&'a self, _account: &'a str, _provider: &'a str)
        -> Pin<Box<dyn Future<Output = Result<AccountCredential, VerificationError>> + Send + 'a>> {
        Box::pin(async { Err(VerificationError::Unavailable) })
    }
    fn acquire_at<'a>(&'a self, account: &'a str, provider: &'a str, generation: u64)
        -> Pin<Box<dyn Future<Output = Result<AccountCredential, VerificationError>> + Send + 'a>> {
        Box::pin(async move {
            self.route_at(account, generation)?;
            let now = duduclaw_mcp_boundary::authority::now_seconds()?;
            if let Ok(stored) = self.vault.lookup(account, provider, generation) {
                if now < stored.expires_at { return Ok(stored); }
            }
            self.vault.retain_generation(account, generation)?;
            let fresh = self.source.acquire_at(account, provider, generation).await?;
            if fresh.account != account || fresh.provider != provider {
                return Err(VerificationError::BindingMismatch);
            }
            self.vault.install(fresh.duplicate()?, generation)?;
            // A revoke between the source read and the install: drop it again.
            self.route_at(account, generation)?;
            Ok(fresh)
        })
    }
}

async fn credential(selected: &SelectedConnector, provider: &dyn AccountCredentialProvider)
    -> Result<AccountCredential, VerificationError> {
    selected.recheck()?;
    let credential = provider.acquire_at(selected.account(), selected.provider(), selected.generation()).await?;
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
            // v1.81 E3: a provider that refreshes (GitHub home token) checks
            // `verify_before_io` after the exchange and before storing the new
            // token; scoping the acquisition makes that the full selection recheck.
            let token = duduclaw_mcp_boundary::io_authority::scope(selected.io_context(),
                credential(&selected, self.credentials.as_ref())).await?;
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
