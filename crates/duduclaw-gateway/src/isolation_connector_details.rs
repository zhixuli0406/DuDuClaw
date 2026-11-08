//! Per-account connector settings (native Linux isolation v1.81, task C6).
//!
//! Database, website-login (computer) and channel routes need provider
//! configuration beyond the catalog route: the `[db_sources.<name>]` source,
//! the website credential id, or the exact channel route (channel + scope)
//! and where its bot token lives. This module owns that configuration.
//!
//! Storage: a second table, `isolation_connector_details`, in the catalog
//! database (`isolation-connectors.db`), opened only through
//! [`ConnectorCatalog::open`] (private-SQLite opener + 0600 checks). Sharing
//! the file is deliberate: the route and its details are written in ONE
//! transaction at provision, revoke/advance move the route generation and
//! delete its details in ONE transaction, and the reader takes the route and
//! the details from ONE read snapshot. A sibling store could not give these
//! guarantees without a cross-file protocol.
//!
//! Rows are immutable. A row is keyed by the opaque catalog account id and
//! pinned to the route's binding id, provider and generation at insert time
//! (enforced by a trigger against the routes table). It can never be
//! updated; it can be deleted only once its route is revoked or its
//! generation has moved. A details row whose generation is not the route's
//! current generation never resolves.
//!
//! Each row also pins the one protected employee (immutable `EmployeeId` and
//! agent id) the administrator validated the references for. Another employee
//! that happens to see the same route never receives these details.
//!
//! No secret value is ever stored here or logged: a channel row stores only a
//! token *reference* ([`ChannelTokenRef`]); the token is resolved in memory
//! when the employee's providers are built.
use std::{path::{Path, PathBuf}, sync::Arc};
use duduclaw_isolation::{capability::EmployeeBinding, ids::EmployeeId};
use duduclaw_mcp_boundary::VerificationError;
use rusqlite::{Connection, OptionalExtension, params};
use crate::isolation_connector_boot::{ConnectorAccountDetail, ConnectorAccountDetails, DetailPin, ProvisionedDetail};
use crate::isolation_connectors::{AccountId, CatalogError, ConnectorCatalog, ConnectorKind, ProductConnectorAccount, Route};

/// Providers whose routes cannot be served without provisioned details.
pub const DETAIL_PROVIDERS: [&str; 3] = ["database", "computer", "channel"];
pub fn needs_details(provider: &str) -> bool { DETAIL_PROVIDERS.iter().any(|known| *known == provider) }

/// Where a channel route's bot token is read from when providers are built.
/// Never the token itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelTokenRef {
    /// No token: a self-configuring channel (wecom/dingtalk/googlechat/teams)
    /// whose sender reads the installed home credential. SharedHome only.
    None,
    /// The installed home bot: `config.toml [channels] <channel token field>`.
    /// Shared (tenant/home) routes only, never a Personal route.
    Home,
    /// The employee's own agent bot: `agents/<agent>/agent.toml
    /// [channels.<channel>] bot_token(_enc)` of exactly the pinned agent (no
    /// `reports_to` walk).
    Agent,
}
impl ChannelTokenRef {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value { "none" => Self::None, "home" => Self::Home, "agent" => Self::Agent, _ => return None })
    }
    pub fn as_str(self) -> &'static str {
        match self { Self::None => "none", Self::Home => "home", Self::Agent => "agent" }
    }
}

/// The provider-specific references of one account. Secret-free by
/// construction, so Debug/PartialEq are safe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetailReference {
    /// A `[db_sources.<source>]` entry in `config.toml`.
    Database { source: String },
    /// A website credential id (`website_credentials`).
    Computer { credential: String },
    /// An exact channel route plus the token reference.
    Channel { channel: String, scope: String, status_label: String, token: ChannelTokenRef, extra_id: Option<String> },
}
impl DetailReference {
    pub fn provider(&self) -> &'static str {
        match self { Self::Database { .. } => "database", Self::Computer { .. } => "computer", Self::Channel { .. } => "channel" }
    }
}

/// The one protected employee the details are validated for, as read from
/// the employee lifecycle (never from request fields).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailEmployee {
    pub employee: EmployeeId,
    pub agent: String,
    pub binding: EmployeeBinding,
}

/// One stored details row (without its route pin columns).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountDetailRecord {
    pub employee: EmployeeId,
    pub agent: String,
    pub reference: DetailReference,
}

/// Why provision refused the details. Every variant refuses the whole
/// provision; no route is registered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DetailError {
    #[error("connector account details required")] Required,
    #[error("connector account details not applicable to this provider")] NotApplicable,
    #[error("connector account details invalid")] Invalid,
    /// The employee's binding cannot use a route with this binding/kind.
    #[error("employee does not match the connector account binding")] Employee,
    /// The reference does not exist in its registry.
    #[error("connector account reference unknown")] Unknown,
    /// The reference exists but this employee may not use it.
    #[error("connector account reference not usable by this employee")] Foreign,
    /// The registry could not be read.
    #[error("connector account reference registry unavailable")] Unavailable,
}
impl DetailError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Required => "details_required", Self::NotApplicable => "details_not_applicable",
            Self::Invalid => "details_invalid", Self::Employee => "employee_mismatch",
            Self::Unknown => "reference_unknown", Self::Foreign => "reference_foreign",
            Self::Unavailable => "registry_unavailable",
        }
    }
}

fn plain_text(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && value.trim() == value && !value.chars().any(char::is_control)
}
fn valid_agent(agent: &str) -> bool {
    duduclaw_core::is_valid_agent_id(agent) && !duduclaw_core::is_reserved_agent_id(agent)
        && agent != duduclaw_core::UNTRUSTED_AGENT_ID && agent.len() <= 128
}
/// `channel` itself, or `channel:<non-empty>` (exact prefix split, no substring test).
fn valid_status_label(channel: &str, label: &str) -> bool {
    label == channel || label.strip_prefix(channel).and_then(|rest| rest.strip_prefix(':'))
        .is_some_and(|rest| plain_text(rest, 256))
}

impl AccountDetailRecord {
    /// Shape only (the same on write and on read); registries are checked by
    /// [`validate_request`] at provision and by the providers at use.
    fn validate(&self) -> bool {
        valid_agent(&self.agent) && match &self.reference {
            DetailReference::Database { source } => duduclaw_db::config::is_valid_source_name(source) && source.len() <= 128,
            DetailReference::Computer { credential } => credential.len() <= 64 && uuid::Uuid::parse_str(credential).is_ok(),
            DetailReference::Channel { channel, scope, status_label, extra_id, .. } =>
                crate::channel_settings::VALID_CHANNEL_TYPES.iter().any(|known| known == channel)
                    && plain_text(scope, 512) && scope != "*" && valid_status_label(channel, status_label)
                    && extra_id.as_deref().is_none_or(|id| plain_text(id, 512) && !id.contains(['/', '?', '#'])),
        }
    }
}

// ── Schema (verified exactly by the catalog, like the routes table) ─────────

pub(crate) const DETAILS_TABLE_NAME: &str = "isolation_connector_details";
pub(crate) const DETAILS_TABLE: &str = "CREATE TABLE isolation_connector_details (
    account TEXT PRIMARY KEY NOT NULL,
    generation INTEGER NOT NULL,
    binding_id TEXT NOT NULL,
    provider TEXT NOT NULL,
    employee TEXT NOT NULL,
    agent TEXT NOT NULL,
    source TEXT,
    credential TEXT,
    channel TEXT,
    scope TEXT,
    status_label TEXT,
    token_ref TEXT,
    extra_id TEXT,
    created_at INTEGER NOT NULL,
    CHECK(length(account)=32 AND account NOT GLOB '*[^0-9a-f]*'),
    CHECK(generation > 0),
    CHECK(length(binding_id) BETWEEN 8 AND 160),
    CHECK(length(employee)=32 AND employee NOT GLOB '*[^0-9a-f]*'),
    CHECK(length(agent) BETWEEN 1 AND 128),
    CHECK((provider='database' AND source IS NOT NULL AND length(source) BETWEEN 1 AND 128
            AND credential IS NULL AND channel IS NULL AND scope IS NULL AND status_label IS NULL
            AND token_ref IS NULL AND extra_id IS NULL)
        OR (provider='computer' AND credential IS NOT NULL AND length(credential) BETWEEN 1 AND 64
            AND source IS NULL AND channel IS NULL AND scope IS NULL AND status_label IS NULL
            AND token_ref IS NULL AND extra_id IS NULL)
        OR (provider='channel' AND channel IS NOT NULL AND scope IS NOT NULL AND status_label IS NOT NULL
            AND token_ref IS NOT NULL AND token_ref IN ('none','home','agent')
            AND source IS NULL AND credential IS NULL)),
    CHECK(created_at > 0)
) STRICT";
// A row is born only for an active route, at exactly its current generation,
// binding and provider, and never twice for one account (also blocks
// INSERT OR REPLACE).
pub(crate) const DETAILS_INSERT_TRIGGER: &str = "CREATE TRIGGER isolation_connector_details_insert
    BEFORE INSERT ON isolation_connector_details
    WHEN EXISTS(SELECT 1 FROM isolation_connector_details WHERE account=NEW.account)
        OR NOT EXISTS(SELECT 1 FROM isolation_connector_routes WHERE account=NEW.account AND active=1
            AND generation=NEW.generation AND binding_id=NEW.binding_id AND provider=NEW.provider)
    BEGIN SELECT RAISE(ABORT,'immutable account details'); END";
pub(crate) const DETAILS_UPDATE_TRIGGER: &str = "CREATE TRIGGER isolation_connector_details_update
    BEFORE UPDATE ON isolation_connector_details
    BEGIN SELECT RAISE(ABORT,'immutable account details'); END";
// Only details of a revoked route, or of a generation that has moved, may go.
pub(crate) const DETAILS_DELETE_TRIGGER: &str = "CREATE TRIGGER isolation_connector_details_delete
    BEFORE DELETE ON isolation_connector_details
    WHEN EXISTS(SELECT 1 FROM isolation_connector_routes WHERE account=OLD.account AND active=1
        AND generation=OLD.generation)
    BEGIN SELECT RAISE(ABORT,'immutable account details'); END";
pub(crate) const DETAILS_SCHEMA: [(&str, &str, &str); 4] = [
    ("table", DETAILS_TABLE_NAME, DETAILS_TABLE),
    ("trigger", "isolation_connector_details_insert", DETAILS_INSERT_TRIGGER),
    ("trigger", "isolation_connector_details_update", DETAILS_UPDATE_TRIGGER),
    ("trigger", "isolation_connector_details_delete", DETAILS_DELETE_TRIGGER),
];
const DETAIL_COLUMNS: &str =
    "generation,binding_id,provider,employee,agent,source,credential,channel,scope,status_label,token_ref,extra_id";

/// Insert the details of a route registered in the same transaction.
pub(crate) fn insert_row(tx: &Connection, account: &AccountId, generation: i64, binding_id: &str, provider: &str,
    record: &AccountDetailRecord, now: i64) -> Result<(), CatalogError> {
    if record.reference.provider() != provider || !record.validate() { return Err(CatalogError::Invalid); }
    let (source, credential, channel, scope, status_label, token_ref, extra_id) = match &record.reference {
        DetailReference::Database { source } => (Some(source.as_str()), None, None, None, None, None, None),
        DetailReference::Computer { credential } => (None, Some(credential.as_str()), None, None, None, None, None),
        DetailReference::Channel { channel, scope, status_label, token, extra_id } => (None, None,
            Some(channel.as_str()), Some(scope.as_str()), Some(status_label.as_str()), Some(token.as_str()), extra_id.as_deref()),
    };
    let changed = tx.execute(&format!("INSERT INTO isolation_connector_details (account,{DETAIL_COLUMNS},created_at)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)"),
        params![account.as_str(), generation, binding_id, provider, hex::encode(record.employee.0), record.agent,
            source, credential, channel, scope, status_label, token_ref, extra_id, now])
        .map_err(|_| CatalogError::Unavailable)?;
    if changed != 1 { return Err(CatalogError::Unavailable); }
    Ok(())
}

/// A stored row with its pin columns, re-validated like every catalog read.
pub(crate) struct StoredDetail {
    pub(crate) generation: u64,
    pub(crate) binding_id: String,
    pub(crate) provider: String,
    pub(crate) record: AccountDetailRecord,
}
type RawDetail = (i64, String, String, String, String, Option<String>, Option<String>, Option<String>,
    Option<String>, Option<String>, Option<String>, Option<String>);

pub(crate) fn read_row(connection: &Connection, account: &AccountId) -> Result<Option<StoredDetail>, CatalogError> {
    let raw: Option<RawDetail> = connection.query_row(
        &format!("SELECT {DETAIL_COLUMNS} FROM isolation_connector_details WHERE account=?1"), [account.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?,
            row.get(7)?, row.get(8)?, row.get(9)?, row.get(10)?, row.get(11)?)))
        .optional().map_err(|_| CatalogError::Unavailable)?;
    raw.map(decode_row).transpose()
}

fn decode_row((generation, binding_id, provider, employee, agent, source, credential, channel, scope, status_label,
    token_ref, extra_id): RawDetail) -> Result<StoredDetail, CatalogError> {
    let damaged = CatalogError::Unavailable;
    let generation = u64::try_from(generation).ok().filter(|g| *g > 0).ok_or(damaged)?;
    let employee = hex::decode(&employee).ok().and_then(|bytes| <[u8; 16]>::try_from(bytes).ok())
        .map(EmployeeId).ok_or(damaged)?;
    let reference = match (provider.as_str(), source, credential, channel, scope, status_label, token_ref, extra_id) {
        ("database", Some(source), None, None, None, None, None, None) => DetailReference::Database { source },
        ("computer", None, Some(credential), None, None, None, None, None) => DetailReference::Computer { credential },
        ("channel", None, None, Some(channel), Some(scope), Some(status_label), Some(token), extra_id) =>
            DetailReference::Channel { channel, scope, status_label,
                token: ChannelTokenRef::parse(&token).ok_or(damaged)?, extra_id },
        _ => return Err(damaged),
    };
    let record = AccountDetailRecord { employee, agent, reference };
    if !record.validate() { return Err(damaged); }
    Ok(StoredDetail { generation, binding_id, provider, record })
}

// ── Provision-side validation (C3 writer) ───────────────────────────────────

/// Validate admin-supplied references against their own registries for the
/// named employee and route. Read-only; resolves a channel token only in
/// memory to prove the reference resolves, and drops it.
pub fn validate_request(home: &Path, provider: &str, kind: ConnectorKind, binding: &EmployeeBinding,
    employee: &DetailEmployee, reference: &DetailReference) -> Result<AccountDetailRecord, DetailError> {
    if !needs_details(provider) { return Err(DetailError::NotApplicable); }
    if reference.provider() != provider || !kind.admits(binding) { return Err(DetailError::Invalid); }
    // Same admission as the D1 plan: own Personal/SharedTenant routes, and
    // SharedHome routes for Personal employees or same-tenant Shared ones.
    let fits = match kind {
        ConnectorKind::Personal | ConnectorKind::SharedTenant => &employee.binding == binding,
        ConnectorKind::SharedHome => &employee.binding == binding || matches!(employee.binding, EmployeeBinding::Personal(_)),
    };
    if !fits { return Err(DetailError::Employee); }
    let record = AccountDetailRecord { employee: employee.employee, agent: employee.agent.clone(), reference: reference.clone() };
    if !record.validate() { return Err(DetailError::Invalid); }
    match reference {
        DetailReference::Database { source } => validate_database(home, &record.agent, source)?,
        DetailReference::Computer { credential } => validate_computer(home, &record.agent, credential)?,
        DetailReference::Channel { channel, scope, status_label, token, extra_id } =>
            validate_channel(home, kind, binding, &record.agent, channel, scope, status_label, *token, extra_id)?,
    }
    Ok(record)
}

fn read_config(home: &Path) -> Result<Option<toml::Table>, VerificationError> {
    match std::fs::read_to_string(home.join("config.toml")) {
        Ok(raw) => toml::from_str(&raw).map(Some).map_err(|_| VerificationError::Unavailable),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(VerificationError::Unavailable),
    }
}

fn validate_database(home: &Path, agent: &str, source: &str) -> Result<(), DetailError> {
    let config = read_config(home).map_err(|_| DetailError::Unavailable)?.ok_or(DetailError::Unknown)?;
    // Exact registered name; no case folding into another source.
    if !config.get("db_sources").and_then(|sources| sources.get(source)).is_some_and(toml::Value::is_table) {
        return Err(DetailError::Unknown);
    }
    // The employee's own effective grant, read exactly as the provider does.
    match crate::isolation_connector_database::source_config(home, agent, source) {
        Ok(_) => Ok(()),
        Err(VerificationError::BindingMismatch) => Err(DetailError::Foreign),
        Err(_) => Err(DetailError::Unavailable),
    }
}

fn validate_computer(home: &Path, agent: &str, credential: &str) -> Result<(), DetailError> {
    let metadata = crate::website_credentials::list(home).map_err(|_| DetailError::Unavailable)?;
    let mut matches = metadata.iter().filter(|record| record.id == credential);
    let record = matches.next().ok_or(DetailError::Unknown)?;
    if matches.next().is_some() { return Err(DetailError::Unavailable); }
    if !record.allowed_agents.iter().any(|allowed| allowed == agent) { return Err(DetailError::Foreign); }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn validate_channel(home: &Path, kind: ConnectorKind, binding: &EmployeeBinding, agent: &str, channel: &str,
    scope: &str, status_label: &str, token: ChannelTokenRef, extra_id: &Option<String>) -> Result<(), DetailError> {
    duduclaw_core::product_profile::load(home).and_then(|profile| profile.require_channel(channel))
        .map_err(|_| DetailError::Unknown)?;
    let self_configures = crate::channel_sender::sender_self_configures(channel);
    match token {
        ChannelTokenRef::None if !self_configures || kind != ConnectorKind::SharedHome => return Err(DetailError::Foreign),
        ChannelTokenRef::Home if kind == ConnectorKind::Personal || self_configures => return Err(DetailError::Foreign),
        ChannelTokenRef::Agent if self_configures => return Err(DetailError::Foreign),
        _ => {}
    }
    let token = match resolve_token(home, agent, channel, token).map_err(|_| DetailError::Unavailable)? {
        TokenOutcome::Missing => return Err(DetailError::Unknown),
        TokenOutcome::NoToken => None,
        TokenOutcome::Token(token) => Some(token),
    };
    // The same constructor the provider uses, on a placeholder account.
    let product = match binding {
        EmployeeBinding::Personal(owner) => ProductConnectorAccount::Personal { account: String::new(), owner: owner.as_str().to_owned() },
        EmployeeBinding::Shared(tenant) => ProductConnectorAccount::SharedHome { account: String::new(), tenant: tenant.as_str().to_owned() },
    };
    match crate::isolation_connector_channel::ChannelAccount::new(product, channel.to_owned(), scope.to_owned(),
        status_label.to_owned(), token, extra_id.clone()) {
        Ok(_) => Ok(()),
        Err(VerificationError::BindingMismatch) => Err(DetailError::Foreign),
        Err(_) => Err(DetailError::Invalid),
    }
}

enum TokenOutcome { NoToken, Token(String), Missing }

/// Resolve a token reference synchronously, in memory only. A reference that
/// no longer resolves (removed, or a network-backed `secret://` value that
/// needs the async secret manager) is `Missing`; an unreadable or damaged
/// configuration file is `Err`.
fn resolve_token(home: &Path, agent: &str, channel: &str, token: ChannelTokenRef) -> Result<TokenOutcome, VerificationError> {
    match token {
        ChannelTokenRef::None => {
            let Some(marker) = crate::channel_sender::self_config_marker_field(channel) else { return Ok(TokenOutcome::Missing) };
            let Some(config) = read_config(home)? else { return Ok(TokenOutcome::Missing) };
            Ok(if crate::config_crypto::describe_config_field(&config, "channels", marker).configured {
                TokenOutcome::NoToken
            } else { TokenOutcome::Missing })
        }
        ChannelTokenRef::Home => {
            let Some(field) = crate::otp_delivery::token_field(channel) else { return Ok(TokenOutcome::Missing) };
            let Some(config) = read_config(home)? else { return Ok(TokenOutcome::Missing) };
            Ok(crate::config_crypto::decrypt_config_field(&config, "channels", field, home)
                .map(|secret| secret.expose_owned()).filter(|value| !value.is_empty())
                .map_or(TokenOutcome::Missing, TokenOutcome::Token))
        }
        ChannelTokenRef::Agent => {
            if !valid_agent(agent) { return Err(VerificationError::Invalid); }
            Ok(crate::config_crypto::agent_channel_token_ref(home, agent, channel)
                .and_then(|reference| reference.resolve_sync(home))
                .map(|secret| secret.expose_owned()).filter(|value| !value.is_empty())
                .map_or(TokenOutcome::Missing, TokenOutcome::Token))
        }
    }
}

// ── Reader (D1 `ConnectorAccountDetails`) ───────────────────────────────────

/// Production [`ConnectorAccountDetails`] over the catalog's details table.
/// F1 constructs it with the same catalog instance it gives the factory.
pub struct CatalogAccountDetails { home: PathBuf, catalog: Arc<ConnectorCatalog> }
impl CatalogAccountDetails {
    pub fn new(home: PathBuf, catalog: Arc<ConnectorCatalog>) -> Result<Self, VerificationError> {
        if !home.is_absolute() { return Err(VerificationError::Invalid); }
        Ok(Self { home, catalog })
    }
    /// The route and its details for the route's current generation, from one
    /// read snapshot; `None` when the route is revoked or has no current
    /// details. Store damage or an unreadable store is `Err(Unavailable)`.
    fn current(&self, account: &AccountId) -> Result<Option<(Route, AccountDetailRecord)>, VerificationError> {
        Ok(match self.catalog.route_details(account)? {
            Some((route, Some(record))) if route.active => Some((route, record)),
            _ => None,
        })
    }
}
impl ConnectorAccountDetails for CatalogAccountDetails {
    fn detail(&self, route: &Route) -> Result<Option<ProvisionedDetail>, VerificationError> {
        if !needs_details(route.provider) { return Err(VerificationError::Invalid); }
        let Some((current, record)) = self.current(&route.account)? else { return Ok(None) };
        // The plan's route moved while building: refuse this build (F1 retries).
        if current != *route { return Err(VerificationError::Unavailable); }
        let detail = match &record.reference {
            DetailReference::Database { source } => ConnectorAccountDetail::Database { source: source.clone() },
            DetailReference::Computer { credential } => ConnectorAccountDetail::Computer { credential: credential.clone() },
            DetailReference::Channel { channel, scope, status_label, token, extra_id } => {
                let token = match resolve_token(&self.home, &record.agent, channel, *token)? {
                    TokenOutcome::Missing => return Ok(None),
                    TokenOutcome::NoToken => None,
                    TokenOutcome::Token(token) => Some(token),
                };
                ConnectorAccountDetail::Channel { channel: channel.clone(), scope: scope.clone(),
                    status_label: status_label.clone(), token, extra_id: extra_id.clone() }
            }
        };
        let pin = DetailPin::new(&current, record.employee, record.agent, record.reference);
        Ok(Some(ProvisionedDetail { detail, pin }))
    }
    fn current_pin(&self, account: &AccountId) -> Result<Option<DetailPin>, VerificationError> {
        Ok(self.current(account)?.map(|(route, record)| DetailPin::new(&route, record.employee, record.agent, record.reference)))
    }
}

#[cfg(test)]
#[path = "isolation_connector_details_tests.rs"]
mod tests;
