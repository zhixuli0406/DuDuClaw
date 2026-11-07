//! Native Notion/GitHub *home* connectors for isolated MCP (v1.81 task D2).
//!
//! Notion and GitHub are home-wide integrations: the gateway's `mcp_oauth`
//! vault holds exactly one token per provider and there is no per-person
//! account. Isolated employees therefore reach them only through a catalog
//! route of kind SharedHome:
//!
//! - [`NotionPolicy`] / [`GithubPolicy`] select **only** the installed
//!   SharedHome account, and only while the integration is enabled, the
//!   employee's current `agent.toml` allows the tool and the catalog route is
//!   still an active home route for that exact provider. They never return a
//!   Personal account. A Personal employee can use the result only when the
//!   binding authority's product-home check (C2,
//!   `BindingAuthority::authorize_product_home_connector`) accepts it; that
//!   check is applied by `SelectedConnector`, not here.
//! - [`HomeOAuthCredentialProvider`] reads the home-wide token through the
//!   existing `get_valid_notion_token` / `get_valid_github_token`, only for an
//!   account that is an active SharedHome route of that exact provider, before
//!   and after the (possibly refreshing) await.
//!
//! Security: every failure is a data-free [`VerificationError`]; provider
//! error strings (which may echo server text) are dropped, never formatted or
//! logged. Token bytes live in `Zeroizing<String>` until they are moved into
//! the executor's [`AccountCredential`] (itself zeroizing, no Debug/Serialize).
use std::{future::Future, path::{Path, PathBuf}, pin::Pin, sync::Arc};
use duduclaw_isolation::ids::EmployeeId;
use duduclaw_mcp_boundary::{RequestContext, VerificationError};
use zeroize::{Zeroize, Zeroizing};
use crate::isolation_connector_clients::{AccountCredential, AccountCredentialProvider};
use crate::isolation_connectors::{LiveConnectorSelectionPolicy, ProductConnectorAccount, provider_for_tool};

pub const NOTION: &str = "notion";
pub const GITHUB: &str = "github";
/// The only refresh endpoint an isolated GitHub acquisition may cause a
/// refresh_token grant to be sent to (the product's built-in template).
const GITHUB_TOKEN_ENDPOINT: &str = "https://github.com/login/oauth/access_token";
/// Neither `get_valid_*` returns the token's expiry. `mcp_oauth::get_token`
/// only returns tokens with more than 60 s left, so a 60 s lease never outlives
/// the stored token. The executor refuses every HTTP hop after the lease.
const LEASE_SECS: u64 = 60;

/// Catalog check that C1's production `ConnectorCatalog` implements.
///
/// Contract for implementers:
/// - `Ok(true)` only when `account` has a route that is active, of kind
///   SharedHome, and whose provider is exactly `provider`. A Personal route,
///   a revoked/inactive route, an unknown account or a route of another
///   provider is `Ok(false)`.
/// - An unreadable/locked/corrupt store is `Err`, never `Ok(false)`-by-default
///   and never `Ok(true)`.
/// - It is called before and after every credential await and on every
///   selection recheck, so it must read current state (no startup snapshot)
///   and must not call back into product selection.
pub trait HomeRouteCheck: Send + Sync {
    fn is_active_home_route(&self, account: &str, provider: &str) -> Result<bool, VerificationError>;
}

/// Source of the home-wide provider token. Production is [`OAuthVaultHomeTokens`];
/// the seam exists so the account/route fence can be tested without a vault.
pub trait HomeTokenSource: Send + Sync {
    fn home_token<'a>(&'a self, provider: &'static str)
        -> Pin<Box<dyn Future<Output = Result<Zeroizing<String>, VerificationError>> + Send + 'a>>;
}

/// Reads the gateway's `mcp_oauth` vault through the existing helpers.
pub struct OAuthVaultHomeTokens { home: PathBuf }
impl OAuthVaultHomeTokens {
    pub fn new(home: PathBuf) -> Result<Self, VerificationError> {
        if !home.is_absolute() { return Err(VerificationError::Invalid); }
        Ok(Self { home })
    }
}
impl HomeTokenSource for OAuthVaultHomeTokens {
    fn home_token<'a>(&'a self, provider: &'static str)
        -> Pin<Box<dyn Future<Output = Result<Zeroizing<String>, VerificationError>> + Send + 'a>> {
        Box::pin(async move {
            let token = match provider {
                NOTION => crate::notion_workspace::get_valid_notion_token(&self.home).await
                    .map_err(|_| VerificationError::Unavailable)?,
                GITHUB => {
                    // `get_valid_github_token` may send the refresh_token to the
                    // stored client config's token_url. Refuse before any I/O
                    // unless that endpoint is the pinned GitHub host.
                    {
                        let mut config = crate::mcp_oauth::get_client_config(&self.home, crate::github_workspace::GITHUB_PROVIDER);
                        if let Some(config) = config.as_mut() { config.client_secret.zeroize(); }
                        github_refresh_config_allowed(config.as_ref())?;
                    }
                    crate::github_workspace::get_valid_github_token(&self.home).await
                        .map_err(|_| VerificationError::Unavailable)?
                }
                _ => return Err(VerificationError::Invalid),
            };
            Ok(Zeroizing::new(token))
        })
    }
}

/// No client config means a refresh cannot be attempted (the helper returns
/// `ClientConfigMissing` without network). A present config must name GitHub
/// and the exact pinned endpoint: another host, scheme, port, userinfo, query
/// or path is refused.
fn github_refresh_config_allowed(config: Option<&crate::mcp_oauth::McpOAuthClientConfig>) -> Result<(), VerificationError> {
    match config {
        None => Ok(()),
        Some(config) if config.provider_id == GITHUB && pinned_github_token_endpoint(&config.token_url) => Ok(()),
        Some(_) => Err(VerificationError::Unavailable),
    }
}
fn pinned_github_token_endpoint(url: &str) -> bool { url == GITHUB_TOKEN_ENDPOINT }

fn home_provider(provider: &str) -> Option<&'static str> {
    match provider { NOTION => Some(NOTION), GITHUB => Some(GITHUB), _ => None }
}
/// Mirrors `AccountCredential::new` so that constructor cannot fail after the
/// token has been moved out of its zeroizing wrapper.
fn valid_account(account: &str) -> bool {
    !account.is_empty() && account.len() <= 128 && !account.chars().any(char::is_control)
}
fn bearer_shape(token: &str) -> bool {
    !token.is_empty() && token.len() <= 16384
        && token.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'+' | b'/' | b'='))
}

/// `AccountCredentialProvider` for the Notion/GitHub home tokens.
pub struct HomeOAuthCredentialProvider {
    routes: Arc<dyn HomeRouteCheck>,
    tokens: Arc<dyn HomeTokenSource>,
}
impl HomeOAuthCredentialProvider {
    pub fn new(home: PathBuf, routes: Arc<dyn HomeRouteCheck>) -> Result<Self, VerificationError> {
        Ok(Self::with_source(routes, Arc::new(OAuthVaultHomeTokens::new(home)?)))
    }
    pub fn with_source(routes: Arc<dyn HomeRouteCheck>, tokens: Arc<dyn HomeTokenSource>) -> Self {
        Self { routes, tokens }
    }
    fn require_home_route(&self, account: &str, provider: &'static str) -> Result<(), VerificationError> {
        match self.routes.is_active_home_route(account, provider) {
            Ok(true) => Ok(()),
            Ok(false) => Err(VerificationError::BindingMismatch),
            Err(_) => Err(VerificationError::Unavailable),
        }
    }
}
impl AccountCredentialProvider for HomeOAuthCredentialProvider {
    fn acquire<'a>(&'a self, account: &'a str, provider: &'a str)
        -> Pin<Box<dyn Future<Output = Result<AccountCredential, VerificationError>> + Send + 'a>> {
        Box::pin(async move {
            let provider = home_provider(provider).ok_or(VerificationError::BindingMismatch)?;
            if !valid_account(account) { return Err(VerificationError::Invalid); }
            self.require_home_route(account, provider)?;
            // Refresh/store failures all become the existing data-free Unavailable.
            let mut token = self.tokens.home_token(provider).await.map_err(|_| VerificationError::Unavailable)?;
            // The route may have been revoked or re-kinded during the await.
            self.require_home_route(account, provider)?;
            if !bearer_shape(&token) { return Err(VerificationError::Unavailable); }
            let expires_at = duduclaw_mcp_boundary::authority::now_seconds()?
                .checked_add(LEASE_SECS).ok_or(VerificationError::Unavailable)?;
            // Moves the buffer (no copy) into AccountCredential's own Zeroizing.
            AccountCredential::new(account.to_owned(), provider.to_owned(), std::mem::take(&mut *token), expires_at)
        })
    }
}

/// Shared implementation of the two home selection policies.
struct HomePolicy {
    home: PathBuf,
    agent: String,
    employee: EmployeeId,
    provider: &'static str,
    account: ProductConnectorAccount,
    routes: Arc<dyn HomeRouteCheck>,
}
impl HomePolicy {
    fn new(home: PathBuf, agent: String, employee: EmployeeId, provider: &'static str,
        account: ProductConnectorAccount, routes: Arc<dyn HomeRouteCheck>) -> Result<Self, VerificationError> {
        if !home.is_absolute() || !duduclaw_core::is_valid_agent_id(&agent)
            || duduclaw_core::is_reserved_agent_id(&agent) || agent == duduclaw_core::UNTRUSTED_AGENT_ID {
            return Err(VerificationError::Invalid);
        }
        // Home-wide integrations have no personal account: refuse to install one.
        match &account {
            ProductConnectorAccount::SharedHome { account, tenant }
                if valid_account(account) && !tenant.is_empty() && !tenant.chars().any(char::is_control) => {}
            _ => return Err(VerificationError::Invalid),
        }
        Ok(Self { home, agent, employee, provider, account, routes })
    }
    fn supports(&self, tool: &str) -> bool { provider_for_tool(tool) == Some(self.provider) }
    fn candidates(&self, context: &RequestContext, tool: &str, provider: &str)
        -> Result<Vec<ProductConnectorAccount>, VerificationError> {
        context.verify_live(duduclaw_mcp_boundary::authority::now_seconds()?)?;
        if context.record().employee != self.employee || provider != self.provider || !self.supports(tool) {
            return Err(VerificationError::Unavailable);
        }
        integration_enabled(&self.home, self.provider)?;
        employee_allows(&self.home, &self.agent, tool)?;
        match self.routes.is_active_home_route(self.account.account(), self.provider) {
            Ok(true) => {}
            Ok(false) => return Err(VerificationError::BindingMismatch),
            Err(_) => return Err(VerificationError::Unavailable),
        }
        // File and catalog reads may race a lifecycle revoke.
        context.verify_live(duduclaw_mcp_boundary::authority::now_seconds()?)?;
        Ok(vec![self.account.clone()])
    }
}

/// Current `config.toml` integration gate. Unreadable or invalid config never
/// defaults to enabled.
/// - GitHub: the existing `[integrations] github = true` gate
///   (`github_workspace::integration_enabled`), required to be a real boolean.
/// - Notion: the product has no enable flag (connecting is the opt-in and the
///   SharedHome route only exists while connected). An explicit
///   `[integrations] notion = false` is honoured as a kill switch; a non-boolean
///   value is refused.
fn integration_enabled(home: &Path, provider: &'static str) -> Result<(), VerificationError> {
    let raw = std::fs::read_to_string(home.join("config.toml")).map_err(|_| VerificationError::Unavailable)?;
    let table: toml::Table = toml::from_str(&raw).map_err(|_| VerificationError::Invalid)?;
    let flag = match table.get("integrations") {
        None => None,
        Some(section) => match section.as_table().ok_or(VerificationError::Invalid)?.get(provider) {
            None => None,
            Some(value) => Some(value.as_bool().ok_or(VerificationError::Invalid)?),
        },
    };
    let enabled = match provider {
        GITHUB => flag == Some(true) && crate::github_workspace::integration_enabled(home),
        NOTION => flag != Some(false),
        _ => return Err(VerificationError::Invalid),
    };
    if enabled { Ok(()) } else { Err(VerificationError::Unavailable) }
}

/// The employee's current effective `agent.toml`: the shared
/// `denied_tools`/`allowed_tools` verdict (`tool_list_verdict`, as used by the
/// MCP front door and `tools/list`), `action_rules` Block, and agent status.
/// Unlike `agent_toml::load`, malformed sections refuse instead of defaulting.
fn employee_allows(home: &Path, agent: &str, tool: &str) -> Result<(), VerificationError> {
    let dir = crate::ephemeral::resolve_agent_dir(home, agent).or_else(||
        (!crate::ephemeral::is_ephemeral_id(agent)).then(|| home.join("agents").join(agent)))
        .ok_or(VerificationError::Unavailable)?;
    let raw = duduclaw_core::agent_toml::read_effective_text(&dir).map_err(|_| VerificationError::Unavailable)?;
    let config: toml::Table = toml::from_str(&raw).map_err(|_| VerificationError::Invalid)?;
    if let Some(section) = config.get("agent") {
        if let Some(status) = section.as_table().ok_or(VerificationError::Invalid)?.get("status") {
            if status.as_str().ok_or(VerificationError::Invalid)? != "active" { return Err(VerificationError::Unavailable); }
        }
    }
    let caps = config.get("capabilities").map(|c| c.as_table().ok_or(VerificationError::Invalid)).transpose()?;
    let list = |key: &str| -> Result<Vec<String>, VerificationError> {
        match caps.and_then(|c| c.get(key)) {
            None => Ok(Vec::new()),
            Some(value) => value.as_array().ok_or(VerificationError::Invalid)?.iter()
                .map(|entry| entry.as_str().map(str::to_owned).ok_or(VerificationError::Invalid)).collect(),
        }
    };
    let (denied, allowed) = (list("denied_tools")?, list("allowed_tools")?);
    if duduclaw_core::tool_catalog::tool_list_verdict(tool, &denied, &allowed)
        != duduclaw_core::tool_catalog::ToolListVerdict::Allowed {
        return Err(VerificationError::Unavailable);
    }
    if duduclaw_core::agent_toml::load_action_rules(&dir).resolve(tool, duduclaw_core::effect_of(tool))
        == Some(duduclaw_core::ActionVerdict::Block) {
        return Err(VerificationError::Unavailable);
    }
    Ok(())
}

/// Live selection policy for the Notion home workspace (SharedHome only).
pub struct NotionPolicy(HomePolicy);
impl NotionPolicy {
    pub fn new(home: PathBuf, agent: String, employee: EmployeeId, account: ProductConnectorAccount,
        routes: Arc<dyn HomeRouteCheck>) -> Result<Self, VerificationError> {
        HomePolicy::new(home, agent, employee, NOTION, account, routes).map(Self)
    }
}
impl LiveConnectorSelectionPolicy for NotionPolicy {
    fn supports(&self, tool: &str) -> bool { self.0.supports(tool) }
    fn candidates(&self, context: &RequestContext, tool: &str, provider: &str)
        -> Result<Vec<ProductConnectorAccount>, VerificationError> {
        self.0.candidates(context, tool, provider)
    }
}

/// Live selection policy for the GitHub home account (SharedHome only).
pub struct GithubPolicy(HomePolicy);
impl GithubPolicy {
    pub fn new(home: PathBuf, agent: String, employee: EmployeeId, account: ProductConnectorAccount,
        routes: Arc<dyn HomeRouteCheck>) -> Result<Self, VerificationError> {
        HomePolicy::new(home, agent, employee, GITHUB, account, routes).map(Self)
    }
}
impl LiveConnectorSelectionPolicy for GithubPolicy {
    fn supports(&self, tool: &str) -> bool { self.0.supports(tool) }
    fn candidates(&self, context: &RequestContext, tool: &str, provider: &str)
        -> Result<Vec<ProductConnectorAccount>, VerificationError> {
        self.0.candidates(context, tool, provider)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, atomic::{AtomicUsize, Ordering}};
    use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord,
        EmployeeBinding, Lane, Principal, ResourceGrant}, ids::{RunId, Sha256Digest}};
    use duduclaw_mcp_boundary::bindings::BindingStore;
    use crate::isolation_connectors::{ConnectorCatalog, SelectedConnector};
    use rusqlite::Connection;
    use serde_json::json;

    const SECRET: &str = "ghp_fixtureHomeSecret0123456789";
    const EMPLOYEE: EmployeeId = EmployeeId([2; 16]);

    struct Routes(Mutex<Vec<(&'static str, &'static str)>>);
    impl Routes {
        fn new(active: &[(&'static str, &'static str)]) -> Arc<Self> { Arc::new(Self(Mutex::new(active.to_vec()))) }
        fn clear(&self) { self.0.lock().unwrap().clear(); }
    }
    impl HomeRouteCheck for Routes {
        fn is_active_home_route(&self, account: &str, provider: &str) -> Result<bool, VerificationError> {
            Ok(self.0.lock().unwrap().iter().any(|(a, p)| *a == account && *p == provider))
        }
    }
    struct BrokenRoutes;
    impl HomeRouteCheck for BrokenRoutes {
        fn is_active_home_route(&self, _: &str, _: &str) -> Result<bool, VerificationError> { Err(VerificationError::Unavailable) }
    }
    struct Source { token: &'static str, fail: bool, calls: AtomicUsize, revoke: Option<Arc<Routes>> }
    impl Source {
        fn new(token: &'static str) -> Self { Self { token, fail: false, calls: AtomicUsize::new(0), revoke: None } }
    }
    impl HomeTokenSource for Source {
        fn home_token<'a>(&'a self, _: &'static str)
            -> Pin<Box<dyn Future<Output = Result<Zeroizing<String>, VerificationError>> + Send + 'a>> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                tokio::task::yield_now().await;
                if let Some(routes) = &self.revoke { routes.clear(); }
                if self.fail { return Err(VerificationError::Unavailable); }
                Ok(Zeroizing::new(self.token.to_owned()))
            })
        }
    }

    struct Live;
    impl duduclaw_isolation::capability::LiveRequestVerifier for Live {
        fn verify_live(&self, _: &CapabilityRecord, _: u64) -> Result<(), VerificationError> { Ok(()) }
    }
    fn text<const N: usize>(s: &str) -> Text<N> { Text::new(s.into()).unwrap() }
    fn context(tool: &str, account: &str) -> RequestContext {
        let binding = EmployeeBinding::Shared(text("company"));
        RequestContext::with_live_verifier(CapabilityRecord {
            id: CapabilityId(Sha256Digest([1; 32])), employee: EMPLOYEE, run: RunId([3; 16]), task: None,
            principal: Principal::Background { tenant_id: text("company"), generation: 1 },
            employee_binding: binding.clone(), lane: Lane::Standard,
            tools: List::new(vec![text(tool)]).unwrap(),
            resources: List::new(vec![ResourceGrant::Connector { account: text(account), binding }]).unwrap(),
            policy_revision: 1, gateway_epoch: [0; 16], issued_at: 1, not_before: 1, expires_at: u64::MAX,
            nonce: [0; 32], jti: [0; 16],
        }, Arc::new(Live))
    }
    fn home(config: &str, agent: &str) -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("agents/employee-a")).unwrap();
        std::fs::write(home.path().join("config.toml"), config).unwrap();
        std::fs::write(home.path().join("agents/employee-a/agent.toml"), agent).unwrap();
        home
    }
    fn shared(account: &str) -> ProductConnectorAccount {
        ProductConnectorAccount::SharedHome { account: account.into(), tenant: "company".into() }
    }
    fn notion(home: &tempfile::TempDir, routes: Arc<dyn HomeRouteCheck>) -> NotionPolicy {
        NotionPolicy::new(home.path().into(), "employee-a".into(), EMPLOYEE, shared("home-notion"), routes).unwrap()
    }
    fn github(home: &tempfile::TempDir, routes: Arc<dyn HomeRouteCheck>) -> GithubPolicy {
        GithubPolicy::new(home.path().into(), "employee-a".into(), EMPLOYEE, shared("home-github"), routes).unwrap()
    }

    #[test]
    fn notion_policy_returns_only_catalog_home_account() {
        let home = home("", "[agent]\nstatus='active'\n");
        // A Personal (or malformed) account can never be installed.
        for account in [ProductConnectorAccount::Personal { account: "alice-notion".into(), owner: "alice".into() },
            ProductConnectorAccount::SharedHome { account: "home-notion".into(), tenant: String::new() },
            ProductConnectorAccount::SharedHome { account: String::new(), tenant: "company".into() }] {
            assert!(NotionPolicy::new(home.path().into(), "employee-a".into(), EMPLOYEE, account.clone(), Routes::new(&[])).is_err());
            assert!(GithubPolicy::new(home.path().into(), "employee-a".into(), EMPLOYEE, account, Routes::new(&[])).is_err());
        }
        let ctx = context("notion_search", "home-notion");
        let routes = Routes::new(&[("home-notion", "notion")]);
        let policy = notion(&home, routes.clone());
        assert!(policy.supports("notion_search") && policy.supports("notion_page_append"));
        assert!(!policy.supports("github_status") && !policy.supports("gmail_search"));
        assert!(policy.candidates(&ctx, "notion_search", "notion").unwrap() == vec![shared("home-notion")]);
        // Wrong provider/tool/employee, or a route that is not a Notion home route.
        assert!(policy.candidates(&ctx, "notion_search", "github").is_err());
        assert!(policy.candidates(&ctx, "github_status", "notion").is_err());
        let mut other = ctx.record().clone();
        other.employee = EmployeeId([9; 16]);
        assert!(policy.candidates(&RequestContext::with_live_verifier(other, Arc::new(Live)), "notion_search", "notion").is_err());
        assert!(notion(&home, Routes::new(&[("home-notion", "github")])).candidates(&ctx, "notion_search", "notion").is_err());
        assert!(notion(&home, Arc::new(BrokenRoutes)).candidates(&ctx, "notion_search", "notion").is_err());

        // End to end: the shared employee selects exactly the catalog home
        // account, and a later route revoke fences the selection.
        let principal = Principal::Background { tenant_id: text("company"), generation: 1 };
        let binding = EmployeeBinding::Shared(text("company"));
        let mut store = BindingStore::new(Connection::open_in_memory().unwrap()).unwrap();
        store.register_principal(&principal).unwrap();
        store.create_employee(EMPLOYEE, &binding).unwrap();
        store.register_connector("home-notion", &binding).unwrap();
        let catalog = Arc::new(ConnectorCatalog::new(Connection::open_in_memory().unwrap()).unwrap());
        catalog.register("home-notion", "notion").unwrap();
        let selected = SelectedConnector::select_with_policy(&ctx, "notion_search", &json!({"query":"plan"}),
            Arc::new(Mutex::new(store)), catalog, Some(Arc::new(notion(&home, routes.clone())))).unwrap();
        assert_eq!((selected.account(), selected.provider()), ("home-notion", "notion"));
        assert!(selected.recheck().is_ok());
        routes.clear();
        assert!(selected.recheck().is_err());
    }

    #[test]
    fn home_policy_disabled_integration_is_refused() {
        let agent = "[agent]\nstatus='active'\n";
        let ctx = context("github_search_issues", "home-github");
        let routes = Routes::new(&[("home-github", "github")]);
        assert!(github(&home("[integrations]\ngithub=true\n", agent), routes.clone())
            .candidates(&ctx, "github_search_issues", "github").is_ok());
        for config in ["", "[integrations]\ngithub=false\n", "[integrations]\ngithub='true'\n", "integrations='x'\n", "invalid ["] {
            assert!(github(&home(config, agent), routes.clone()).candidates(&ctx, "github_search_issues", "github").is_err(), "{config}");
        }
        let missing = home("[integrations]\ngithub=true\n", agent);
        std::fs::remove_file(missing.path().join("config.toml")).unwrap();
        assert!(github(&missing, routes).candidates(&ctx, "github_search_issues", "github").is_err());

        let ctx = context("notion_search", "home-notion");
        let routes = Routes::new(&[("home-notion", "notion")]);
        for config in ["[integrations]\nnotion=false\n", "[integrations]\nnotion=1\n", "invalid ["] {
            assert!(notion(&home(config, agent), routes.clone()).candidates(&ctx, "notion_search", "notion").is_err(), "{config}");
        }
        let missing = home("", agent);
        std::fs::remove_file(missing.path().join("config.toml")).unwrap();
        assert!(notion(&missing, routes).candidates(&ctx, "notion_search", "notion").is_err());
    }

    #[test]
    fn home_policy_requires_current_employee_tool_allowance() {
        let ctx = context("notion_page_append", "home-notion");
        let routes = Routes::new(&[("home-notion", "notion")]);
        assert!(notion(&home("", "[capabilities]\nallowed_tools=['mcp__duduclaw__*']\n"), routes.clone())
            .candidates(&ctx, "notion_page_append", "notion").is_ok());
        for agent in ["[capabilities]\ndenied_tools=['notion_page_append']\n",
            "[capabilities]\nallowed_tools=['notion_search']\n",
            "[capabilities]\ndenied_tools='notion_page_append'\n",
            "[agent]\nstatus='paused'\n", "invalid ["] {
            assert!(notion(&home("", agent), routes.clone()).candidates(&ctx, "notion_page_append", "notion").is_err(), "{agent}");
        }
        let gone = home("", "");
        std::fs::remove_file(gone.path().join("agents/employee-a/agent.toml")).unwrap();
        assert!(notion(&gone, routes).candidates(&ctx, "notion_page_append", "notion").is_err());
    }

    #[tokio::test]
    async fn native_home_credential_refuses_mismatched_account() {
        let routes = Routes::new(&[("home-notion", "notion"), ("home-github", "github")]);
        let source = Arc::new(Source::new(SECRET));
        let provider = HomeOAuthCredentialProvider::with_source(routes.clone(), source.clone());
        for (account, kind) in [("alice-notion", "notion"), ("home-notion", "github"), ("home-github", "notion"),
            ("home-notion", "google"), ("home-notion", "odoo"), ("", "notion")] {
            assert!(provider.acquire(account, kind).await.is_err(), "{account}/{kind}");
        }
        assert_eq!(source.calls.load(Ordering::SeqCst), 0, "no token read before the route matches");
        assert!(provider.acquire("home-notion", "notion").await.is_ok());
        assert!(provider.acquire("home-github", "github").await.is_ok());
        // Unreadable catalog, refresh failure, and a revoke during the await.
        assert!(HomeOAuthCredentialProvider::with_source(Arc::new(BrokenRoutes), source.clone())
            .acquire("home-notion", "notion").await.is_err());
        let failing = Source { fail: true, ..Source::new(SECRET) };
        assert_eq!(HomeOAuthCredentialProvider::with_source(routes.clone(), Arc::new(failing))
            .acquire("home-github", "github").await.err(), Some(VerificationError::Unavailable));
        let revoking = Source { revoke: Some(routes.clone()), ..Source::new(SECRET) };
        assert!(HomeOAuthCredentialProvider::with_source(routes.clone(), Arc::new(revoking))
            .acquire("home-notion", "notion").await.is_err());
    }

    #[test]
    fn github_redirect_refused() {
        // The pin is the product's built-in GitHub refresh endpoint.
        let builtin = crate::mcp_oauth::builtin_providers("http://127.0.0.1/callback").into_iter()
            .find(|p| p.provider_id == "github").unwrap();
        assert!(pinned_github_token_endpoint(&builtin.token_url));
        for url in ["https://evil.example/login/oauth/access_token", "https://github.com.evil.example/login/oauth/access_token",
            "http://github.com/login/oauth/access_token", "https://user@github.com/login/oauth/access_token",
            "https://github.com:8443/login/oauth/access_token", "https://api.github.com/login/oauth/access_token",
            "https://github.com/login/oauth/access_token?redirect=https://evil.example",
            "https://github.com/login/oauth/access_token/../../evil", " https://github.com/login/oauth/access_token"] {
            assert!(!pinned_github_token_endpoint(url), "{url}");
        }
        let config = |provider: &str, token_url: &str| crate::mcp_oauth::McpOAuthClientConfig {
            provider_id: provider.into(), client_id: "fixture".into(), client_secret: String::new(),
            auth_url: "https://github.com/login/oauth/authorize".into(), token_url: token_url.into(),
            scopes: vec!["repo".into()], redirect_uri: "http://127.0.0.1/callback".into(),
        };
        assert!(github_refresh_config_allowed(None).is_ok());
        assert!(github_refresh_config_allowed(Some(&config("github", GITHUB_TOKEN_ENDPOINT))).is_ok());
        assert_eq!(github_refresh_config_allowed(Some(&config("github", "https://evil.example/token"))),
            Err(VerificationError::Unavailable));
        assert!(github_refresh_config_allowed(Some(&config("notion", GITHUB_TOKEN_ENDPOINT))).is_err());
    }

    #[tokio::test]
    async fn native_home_credential_error_text_contains_no_token() {
        let routes = Routes::new(&[("home-github", "github")]);
        let mut errors = Vec::new();
        let revoking = Source { revoke: Some(routes.clone()), ..Source::new(SECRET) };
        errors.push(HomeOAuthCredentialProvider::with_source(routes.clone(), Arc::new(revoking))
            .acquire("home-github", "github").await.err().unwrap());
        let routes = Routes::new(&[("home-github", "github")]);
        // A malformed vault value that embeds the secret is refused, not echoed.
        let malformed = Source::new("ghp_fixtureHomeSecret0123456789 \"injected\"");
        errors.push(HomeOAuthCredentialProvider::with_source(routes.clone(), Arc::new(malformed))
            .acquire("home-github", "github").await.err().unwrap());
        let failing = Source { fail: true, ..Source::new(SECRET) };
        errors.push(HomeOAuthCredentialProvider::with_source(routes.clone(), Arc::new(failing))
            .acquire("home-github", "github").await.err().unwrap());
        errors.push(HomeOAuthCredentialProvider::with_source(routes, Arc::new(Source::new(SECRET)))
            .acquire("other", "github").await.err().unwrap());
        for error in errors {
            let text = format!("{error} {error:?}");
            assert!(!text.contains(SECRET) && !text.contains("fixtureHomeSecret"), "{text}");
        }
    }
}
