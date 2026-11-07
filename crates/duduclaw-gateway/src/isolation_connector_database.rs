//! Account-bound database adapter. Product source grants are read live; the
//! source configuration is pinned for a call and checked again before I/O.
use std::{collections::BTreeMap, future::Future, path::PathBuf, pin::Pin, sync::Arc};
use duduclaw_db::{DbSource, DbSourceConfig, Filter, FilterOp, SelectRequest};
use duduclaw_isolation::capability::ResourceGrant;
use duduclaw_mcp_boundary::{tool_policy::AuthorizedToolCall, RequestContext, VerificationError};
use serde_json::{Value, json};
use crate::isolation_connectors::{BoundConnectorExecutor, LiveConnectorSelectionPolicy,
    ProductConnectorAccount, SelectedConnector};

pub const DATABASE_TOOLS: &[&str] = &["db_sources", "db_tables", "db_select", "db_query"];

/// The boot installer supplies the account-to-source bindings after registering
/// the same immutable account/binding in ConnectorCatalog and BindingStore.
pub struct DatabaseConnectorProvider {
    home: PathBuf,
    agent: String,
    employee: duduclaw_isolation::ids::EmployeeId,
    accounts: BTreeMap<String, (ProductConnectorAccount, String, toml::Value)>,
}
impl DatabaseConnectorProvider {
    pub fn new(home: PathBuf, agent: String, employee: duduclaw_isolation::ids::EmployeeId, accounts: Vec<(ProductConnectorAccount, String)>)
        -> Result<Self, VerificationError> {
        if accounts.is_empty() || accounts.len()>128 || !home.is_absolute() || !duduclaw_core::is_valid_agent_id(&agent)
            || duduclaw_core::is_reserved_agent_id(&agent) || agent == duduclaw_core::UNTRUSTED_AGENT_ID {
            return Err(VerificationError::Invalid);
        }
        let mut registry = BTreeMap::new();
        let mut sources_seen=std::collections::BTreeSet::new();
        for (account, source) in accounts {
            if !sources_seen.insert(source.clone()) {return Err(VerificationError::Invalid);}
            if !duduclaw_db::config::is_valid_source_name(&source) { return Err(VerificationError::Invalid); }
            let pinned = source_config(&home, &agent, &source)?;
            if registry.insert(account.account().to_owned(), (account, source, pinned)).is_some() {
                return Err(VerificationError::Invalid);
            }
        }
        Ok(Self { home, agent, employee, accounts: registry })
    }
    fn source_config(&self, source: &str) -> Result<toml::Value, VerificationError> {
        let live = source_config(&self.home, &self.agent, source)?;
        if !self.accounts.values().any(|(_, name, pinned)| name == source && pinned == &live) {
            return Err(VerificationError::BindingMismatch);
        }
        Ok(live)
    }
    async fn run(&self, selected: SelectedConnector, call: AuthorizedToolCall) -> Result<Value, VerificationError> {
        selected.verify_call(&call)?;
        if !DATABASE_TOOLS.contains(&call.name()) { return Err(VerificationError::Unavailable); }
        if call.name() == "db_sources" {
            // The selected account anchors the call. Every listed member must
            // independently pass the finite grant, owner, binding and live policy.
            let mut rows = Vec::new();
            let mut members = Vec::new();
            for (account, (_, source, _)) in &self.accounts {
                if call.arguments().get("connector_account").and_then(Value::as_str)
                    .is_some_and(|explicit| explicit != account) { continue; }
                if !call.employee_scope() && !call.resources().iter().any(|r|
                    matches!(r,ResourceGrant::DatabaseTable {database,..} if database.as_str()==source)) {continue;}
                let Ok(member) = selected.collection_member(account) else { continue };
                member.recheck()?;
                let pinned = self.source_config(source)?;
                let mut sources = toml::Table::new(); sources.insert(source.clone(), pinned);
                let mut config = toml::Table::new(); config.insert("db_sources".into(), toml::Value::Table(sources));
                let parsed = duduclaw_db::parse_db_sources(&config);
                let entry = parsed.get(source).ok_or(VerificationError::Unavailable)?;
                rows.push(json!({"name":entry.name,"label":entry.label,"driver":entry.driver.as_str()}));
                members.push(member);
            }
            if members.is_empty() { return Err(VerificationError::BindingMismatch); }
            for member in members { member.recheck()?; }
            selected.recheck()?;
            return Ok(tool_result(Value::Array(rows)));
        }
        let (_, source, _) = self.accounts.get(selected.account()).ok_or(VerificationError::BindingMismatch)?;
        if call.arguments().get("source").is_some_and(|s| !s.as_str().is_some_and(|s| s.trim().eq_ignore_ascii_case(source)))
            || call.arguments().get("source").is_none() {
            return Err(VerificationError::BindingMismatch);
        }
        let pinned = self.source_config(source)?;
        let mut sources = toml::Table::new();
        sources.insert(source.clone(), pinned.clone());
        let mut config = toml::Table::new();
        config.insert("db_sources".into(), toml::Value::Table(sources));
        let parsed = duduclaw_db::parse_db_sources(&config);
        let entry = parsed.get(source).ok_or(VerificationError::Unavailable)?;
        let selected = Arc::new(selected);
        let home = self.home.clone();
        let agent = self.agent.clone();
        let source = source.clone();
        let bound = selected.clone();
        let io: duduclaw_db::IoAuthority = Arc::new(move ||
            source_config(&home, &agent, &source).is_ok_and(|current| current == pinned) && bound.recheck().is_ok());
        if !io() { return Err(VerificationError::Unavailable); }
        let mut tables = entry.allowed_tables.clone();
        if !call.context().record().employee_scope() {
            tables = call.resources().iter().filter_map(|r| match r {
                ResourceGrant::DatabaseTable { database, table } if database.as_str() == entry.name
                    && table.as_str() != "*" && (entry.allows_all_tables() || entry.allowed_tables.iter()
                        .any(|allowed| allowed.trim().eq_ignore_ascii_case(table.as_str()))) => Some(table.as_str().to_owned()),
                _ => None,
            }).collect();
            if tables.is_empty() { return Err(VerificationError::BindingMismatch); }
        }
        // Parse and enforce the free-SQL/table policy before acquiring secrets.
        let request = if call.name() == "db_select" { Some(select_request(call.arguments())?) }
        else if call.name()=="db_query" && !call.employee_scope() {
            let sql=call.arguments().get("sql").and_then(Value::as_str).ok_or(VerificationError::Invalid)?;
            let mut request=duduclaw_db::parse_finite_select(sql).map_err(|_|VerificationError::Invalid)?;
            if let Some(limit)=number(call.arguments().get("limit")) {
                if limit==0 {return Err(VerificationError::Invalid);}
                request.limit=Some(request.limit.map_or(limit,|sql_limit|sql_limit.min(limit)));
            }
            Some(request)
        }else{None};
        if let Some(request) = &request {
            if !tables.iter().any(|table| if call.employee_scope() {table == "*" || table.trim().eq_ignore_ascii_case(&request.table)} else {table == &request.table}) {
                return Err(VerificationError::BindingMismatch);
            }
        }
        if call.name() == "db_query" && call.employee_scope() && !entry.allows_all_tables() { return Err(VerificationError::Unavailable); }
        let sql = if call.name() == "db_query" && call.employee_scope() {
            let sql = call.arguments().get("sql").and_then(Value::as_str).ok_or(VerificationError::Invalid)?;
            select_only_before_connect(sql)?;
            Some(sql)
        } else { None };
        if !io() { return Err(VerificationError::Unavailable); }
        let url = entry.resolve_url(&self.home).await.map_err(|_| VerificationError::Unavailable)?;
        if !io() { return Err(VerificationError::Unavailable); }
        let db = DbSource::connect_checked(DbSourceConfig {
            name: entry.name.clone(), label: entry.label.clone(), driver: entry.driver,
            url: url.expose_owned(), allowed_tables: tables, max_rows: entry.max_rows, timeout_ms: entry.timeout_ms,
        }, io.clone()).await.map_err(|_| VerificationError::Unavailable)?;
        let db=if call.employee_scope(){db}else{db.with_exact_table_names()};
        let result = match call.name() {
            "db_tables" => db.list_tables().await.map(|tables| json!({"tables":tables.iter().map(|table|
                json!({"name":table.name,"columns":table.columns.iter().map(|column|
                    json!({"name":column.name,"type":column.data_type})).collect::<Vec<_>>()})).collect::<Vec<_>>()})),
            "db_select" => db.select(request.as_ref().ok_or(VerificationError::Invalid)?).await
                .and_then(|result| serde_json::to_value(result).map_err(|_| duduclaw_db::DbError::Query("invalid result".into()))),
            "db_query" if !call.employee_scope() => db.select(request.as_ref().ok_or(VerificationError::Invalid)?).await
                .and_then(|result| serde_json::to_value(result).map_err(|_| duduclaw_db::DbError::Query("invalid result".into()))),
            "db_query" => db.query(sql.ok_or(VerificationError::Invalid)?, number(call.arguments().get("limit"))).await
                .and_then(|result| serde_json::to_value(result).map_err(|_| duduclaw_db::DbError::Query("invalid result".into()))),
            _ => { db.close().await; return Err(VerificationError::Unavailable); },
        };
        db.close().await;
        if !io() { return Err(VerificationError::Unavailable); }
        let result = result.map_err(|_| VerificationError::Unavailable)?;
        duduclaw_security::audit::append_tool_call_with_extras(&self.home, &self.agent, call.name(),
            "isolated database read", true, &[("db_source", json!(entry.name))]);
        Ok(tool_result(result))
    }
}
// The DB crate's statement guard deliberately admits modifying CTEs and
// relies on driver read-only mode. Protected calls must refuse them before
// resolving credentials or connecting. Keep this conservative lexical fence
// in addition to the existing statement and driver guards.
fn select_only_before_connect(sql: &str) -> Result<(), VerificationError> {
    duduclaw_db::ensure_read_only_statement(sql).map_err(|_| VerificationError::Invalid)?;
    let mut chars = sql.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\'' | '"' | '`' => {
                while let Some(next) = chars.next() {
                    if next == ch {
                        if chars.peek() == Some(&ch) { chars.next(); } else { break; }
                    }
                }
            },
            '-' if chars.peek() == Some(&'-') => {
                for next in chars.by_ref() { if next == '\n' { break; } }
            },
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                // MySQL/MariaDB executable comments are SQL, not comments.
                if matches!(chars.peek(), Some('!' | 'M' | 'm')) { return Err(VerificationError::Invalid); }
                while let Some(next) = chars.next() {
                    // PostgreSQL nests comments, MySQL does not. Refuse the
                    // ambiguous form instead of hiding SQL from this guard.
                    if next == '/' && chars.peek() == Some(&'*') { return Err(VerificationError::Invalid); }
                    if next == '*' && chars.peek() == Some(&'/') { chars.next(); break; }
                }
            },
            ch if ch.is_ascii_alphabetic() || ch == '_' => {
                let mut word = ch.to_string();
                while chars.peek().is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_') {
                    word.push(chars.next().ok_or(VerificationError::Invalid)?);
                }
                if matches!(word.to_ascii_uppercase().as_str(), "INSERT" | "UPDATE" | "DELETE" | "MERGE"
                    | "REPLACE" | "CREATE" | "ALTER" | "DROP" | "TRUNCATE" | "INTO" | "COPY"
                    | "CALL" | "EXECUTE" | "GRANT" | "REVOKE" | "ATTACH" | "DETACH" | "PRAGMA") {
                    return Err(VerificationError::Invalid);
                }
            },
            _ => {},
        }
    }
    Ok(())
}
fn source_config(home: &std::path::Path, agent: &str, source: &str) -> Result<toml::Value, VerificationError> {
    let agent_dir = crate::ephemeral::resolve_agent_dir(home, agent).or_else(||
        (!crate::ephemeral::is_ephemeral_id(agent)).then(|| home.join("agents").join(agent)))
        .ok_or(VerificationError::Unavailable)?;
    let effective = duduclaw_core::agent_toml::read_effective_text(&agent_dir)
        .map_err(|_| VerificationError::Unavailable)?;
    let agent: toml::Table = toml::from_str(&effective).map_err(|_| VerificationError::Invalid)?;
    let grants = agent.get("capabilities").and_then(|t| t.get("db_sources")).and_then(toml::Value::as_array)
        .ok_or(VerificationError::Unavailable)?;
    if grants.iter().any(|g| g.as_str().is_none()) || !grants.iter().any(|g|
        g.as_str().is_some_and(|g| g.trim().eq_ignore_ascii_case(source))) { return Err(VerificationError::BindingMismatch); }
    let raw = std::fs::read_to_string(home.join("config.toml")).map_err(|_| VerificationError::Unavailable)?;
    let config: toml::Table = toml::from_str(&raw).map_err(|_| VerificationError::Invalid)?;
    config.get("db_sources").and_then(|t| t.get(source)).cloned().ok_or(VerificationError::Unavailable)
}
impl LiveConnectorSelectionPolicy for DatabaseConnectorProvider {
    fn supports(&self, tool: &str) -> bool { DATABASE_TOOLS.contains(&tool) }
    fn candidates(&self, context: &RequestContext, tool: &str, provider: &str)
        -> Result<Vec<ProductConnectorAccount>, VerificationError> {
        context.verify_live(duduclaw_mcp_boundary::authority::now_seconds()?)?;
        if context.record().employee != self.employee || provider != "database" || !DATABASE_TOOLS.contains(&tool) { return Err(VerificationError::Unavailable); }
        Ok(self.accounts.values().filter(|(_, source, _)| self.source_config(source).is_ok())
            .map(|(account, _, _)| account.clone()).collect())
    }
    fn candidates_for_call(&self, context: &RequestContext, tool: &str, provider: &str, arguments: &Value)
        -> Result<Vec<ProductConnectorAccount>, VerificationError> {
        let mut candidates = self.candidates(context, tool, provider)?;
        if let Some(source) = arguments.get("source") {
            let source = source.as_str().ok_or(VerificationError::Invalid)?.trim();
            candidates.retain(|account| self.accounts.get(account.account()).is_some_and(|(_, name, _)| name.eq_ignore_ascii_case(source)));
        }
        if let Some(account) = arguments.get("connector_account").and_then(Value::as_str) {
            candidates.retain(|candidate| candidate.account() == account);
        }
        Ok(candidates)
    }
}
impl BoundConnectorExecutor for DatabaseConnectorProvider {
    fn supports(&self, tool: &str) -> bool { DATABASE_TOOLS.contains(&tool) }
    fn execute<'a>(&'a self, selected: SelectedConnector, call: AuthorizedToolCall)
        -> Pin<Box<dyn Future<Output = Result<Value, VerificationError>> + Send + 'a>> {
        Box::pin(self.run(selected, call))
    }
}
fn tool_result(value: Value) -> Value { json!({"content":[{"type":"text","text":value.to_string()}]}) }
fn number(value: Option<&Value>) -> Option<usize> {
    let value = value?;
    value.as_u64().and_then(|n| usize::try_from(n).ok()).or_else(|| value.as_str()?.trim().parse().ok())
}
fn structured(value: Option<&Value>) -> Result<Value, VerificationError> {
    match value { None | Some(Value::Null) => Ok(Value::Null), Some(Value::String(s)) =>
        serde_json::from_str(s).map_err(|_| VerificationError::Invalid), Some(v) => Ok(v.clone()) }
}
fn select_request(args: &Value) -> Result<SelectRequest, VerificationError> {
    let table = args.get("table").and_then(Value::as_str).filter(|s| !s.trim().is_empty())
        .ok_or(VerificationError::Invalid)?.trim().to_owned();
    let columns = structured(args.get("columns"))?;
    let columns = if columns.is_null() { vec![] } else { columns.as_array().ok_or(VerificationError::Invalid)?
        .iter().map(|v| v.as_str().map(|s| s.trim().to_owned()).ok_or(VerificationError::Invalid))
        .collect::<Result<Vec<_>,_>>()? };
    let filters = structured(args.get("filter"))?;
    let mut filter = Vec::new();
    if !filters.is_null() {
        for value in filters.as_array().ok_or(VerificationError::Invalid)? {
            filter.push(Filter {
                column: value.get("column").and_then(Value::as_str).ok_or(VerificationError::Invalid)?.trim().to_owned(),
                op: FilterOp::parse(value.get("op").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).unwrap_or("=")).ok_or(VerificationError::Invalid)?,
                value: value.get("value").cloned().unwrap_or(Value::Null),
            });
        }
    }
    Ok(SelectRequest { table, columns, filter, order_by: args.get("order_by").and_then(Value::as_str)
        .map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned), limit: number(args.get("limit")) })
}

#[cfg(test)]
#[path = "isolation_connector_database_tests.rs"]
mod tests;
