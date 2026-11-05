//! Production resource dispatch. Never forwards a connector to home-account
//! selection, or a file to a handler that reopens an employee pathname.
use std::{future::Future, io::Read, path::PathBuf, pin::Pin, sync::Arc};
use duduclaw_gateway::{isolation_resources::GatewayResourceResolver,
    isolation_connectors::BoundConnectorExecutor};
use duduclaw_mcp_boundary::{resources::ResourceRoots, tool_policy::{AuthorizedToolCall, ResourceResolver},
    VerificationError};
use serde_json::Value;

pub struct GatewayResourceToolService {
    home: PathBuf,
    agent: String,
    roots: Arc<ResourceRoots>,
    resolver: Arc<GatewayResourceResolver>,
    connectors: Option<Arc<dyn BoundConnectorExecutor>>,
}
impl GatewayResourceToolService {
    pub fn new(home: PathBuf, agent: String, roots: Arc<ResourceRoots>,
        resolver: Arc<GatewayResourceResolver>, connectors: Option<Arc<dyn BoundConnectorExecutor>>) -> Self {
        Self { home, agent, roots, resolver, connectors }
    }

    fn read_wiki(&self, call: &AuthorizedToolCall) -> Result<Value, VerificationError> {
        let args = call.arguments();
        // Shared publication has independent ACL/trust policy; the v1 fd route
        // serves only the caller's own published namespace.
        if args.get("scope").is_some_and(|scope| scope != "agent")
            || args.get("target_employee").is_some() {
            return Err(VerificationError::BindingMismatch);
        }
        let page = args.get("page_path").and_then(Value::as_str).ok_or(VerificationError::Invalid)?;
        crate::mcp::validate_wiki_read_path(page).map_err(|_| VerificationError::Invalid)?;
        let namespace = format!("agent:{}", self.agent);
        let wiki_dir = self.home.join("agents").join(&self.agent).join("wiki");
        let _lease = duduclaw_memory::WikiDeliveryFence::for_wiki_dir(&wiki_dir).try_shared()
            .map_err(|_| VerificationError::Unavailable)?;
        let metadata = std::fs::symlink_metadata(&wiki_dir).map_err(|_| VerificationError::Unavailable)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() { return Err(VerificationError::Invalid); }
        let live = std::fs::File::open(&wiki_dir).map_err(|_| VerificationError::Unavailable)?;
        self.roots.verify_wiki_root(&namespace, &live)?;
        let file = self.roots.open_wiki_read(call.context(), &namespace, page)?;
        let cap = crate::mcp::WIKI_MAX_PAGE_SIZE;
        if file.metadata().map_err(|_| VerificationError::Unavailable)?.len() > cap as u64 {
            return Err(VerificationError::Invalid);
        }
        let mut raw = String::new();
        file.take((cap + 1) as u64).read_to_string(&mut raw).map_err(|_| VerificationError::Invalid)?;
        if raw.len() > cap { return Err(VerificationError::Invalid); }
        // Parse the exact fd bytes with the existing bounded plain-text metadata
        // parser. No WikiStore path read and no archive/document extraction.
        if !crate::mcp::WIKI_RESERVED.contains(&std::path::Path::new(page)
            .file_name().and_then(|s| s.to_str()).unwrap_or("")) {
            let parsed = duduclaw_memory::wiki::parse_wiki_page(page, &raw)
                .map_err(|_| VerificationError::Invalid)?;
            if parsed.do_not_inject || !parsed.trust.is_finite() || parsed.trust < 0.1 {
                return Err(VerificationError::Invalid);
            }
            check_live_wiki_trust(&self.home, &self.agent, page)?;
        }
        Ok(crate::mcp::tool_text(&raw))
    }
}

fn check_live_wiki_trust(home: &std::path::Path, agent: &str, page: &str) -> Result<(), VerificationError> {
    use rusqlite::OptionalExtension;
    let path = home.join("wiki_trust.db");
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => {},
        _ => return Err(VerificationError::Unavailable),
    }
    let db = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
        | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX).map_err(|_| VerificationError::Unavailable)?;
    let state: Option<(f64, i64)> = db.query_row(
        "SELECT trust,do_not_inject FROM wiki_trust_state WHERE page_path=?1 AND agent_id=?2",
        rusqlite::params![page, agent], |row| Ok((row.get(0)?, row.get(1)?))).optional()
        .map_err(|_| VerificationError::Unavailable)?;
    if state.is_some_and(|(trust, blocked)| !trust.is_finite() || trust < 0.1 || blocked != 0) {
        return Err(VerificationError::Invalid);
    }
    Ok(())
}

impl crate::mcp_isolation::ResourceToolService for GatewayResourceToolService {
    fn supports(&self, tool: &str) -> bool {
        if crate::mcp_isolation_disposition::intentional_refusal(tool).is_some() { return false; }
        crate::mcp_isolation::legacy_resource_handler(tool) || self.connectors.as_ref().is_some_and(|executor| executor.supports(tool)
            && self.resolver.connector_selection_ready(tool)
            && duduclaw_gateway::isolation_connectors::provider_for_tool(tool).is_some())
    }
    fn resolver(&self) -> &dyn ResourceResolver { self.resolver.as_ref() }
    fn check_handler_policy<'a>(&'a self, call: &'a AuthorizedToolCall)
        -> Pin<Box<dyn Future<Output = Result<(), VerificationError>> + Send + 'a>> {
        Box::pin(async move {
            if !self.supports(call.name()) { return Err(VerificationError::Unavailable); }
            if duduclaw_gateway::isolation_connectors::provider_for_tool(call.name()).is_some() {
                self.resolver.connector(call.context(), call.name(), call.arguments())?.recheck()?;
            }
            Ok(())
        })
    }
    fn execute<'a>(&'a self, call: AuthorizedToolCall)
        -> Pin<Box<dyn Future<Output = Result<Value, VerificationError>> + Send + 'a>> {
        Box::pin(async move {
            if call.name() == "wiki_read" { return self.read_wiki(&call); }
            let executor = self.connectors.as_ref().filter(|e| e.supports(call.name()))
                .ok_or(VerificationError::Unavailable)?;
            let selected = self.resolver.connector(call.context(), call.name(), call.arguments())?;
            selected.verify_call(&call)?;
            executor.execute(selected, call).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn isolated_wiki_live_trust_fails_closed() {
        let home = tempfile::tempdir().unwrap();
        assert!(check_live_wiki_trust(home.path(), "main", "page.md").is_ok());
        let db = rusqlite::Connection::open(home.path().join("wiki_trust.db")).unwrap();
        assert!(check_live_wiki_trust(home.path(), "main", "page.md").is_err());
        db.execute_batch("CREATE TABLE wiki_trust_state(page_path TEXT, agent_id TEXT, trust REAL, do_not_inject INTEGER);
            INSERT INTO wiki_trust_state VALUES('page.md','main',0.9,1);").unwrap();
        assert!(check_live_wiki_trust(home.path(), "main", "page.md").is_err());
        db.execute("UPDATE wiki_trust_state SET do_not_inject=0", []).unwrap();
        assert!(check_live_wiki_trust(home.path(), "main", "page.md").is_ok());
    }
}
