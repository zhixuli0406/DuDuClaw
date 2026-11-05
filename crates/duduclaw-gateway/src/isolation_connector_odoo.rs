//! Odoo account selection and exact-credential preparation. The CLI executor
//! keeps the existing model/action/company and business operation handlers.
use std::{path::{Path, PathBuf}, sync::Arc};
use duduclaw_isolation::ids::EmployeeId;
use duduclaw_mcp_boundary::{tool_policy::AuthorizedToolCall, RequestContext, VerificationError};
use duduclaw_odoo::{AgentOdooConfig, OdooConfig, OdooConnector};
use duduclaw_security::secret_ref::{Secret, SecretRef};
use crate::isolation_connectors::{LiveConnectorSelectionPolicy, ProductConnectorAccount, SelectedConnector, provider_for_tool};

#[derive(Clone, PartialEq)]
struct SourceSnapshot { global: toml::Value, agent: Option<toml::Value> }
impl SourceSnapshot {
    fn load(home: &Path, agent: &str) -> Result<Self, VerificationError> {
        let raw = std::fs::read_to_string(home.join("config.toml")).map_err(|_| VerificationError::Unavailable)?;
        let global: toml::Table = toml::from_str(&raw).map_err(|_| VerificationError::Invalid)?;
        let agent_dir = crate::ephemeral::resolve_agent_dir(home, agent).or_else(||
            (!crate::ephemeral::is_ephemeral_id(agent)).then(|| home.join("agents").join(agent)))
            .ok_or(VerificationError::Unavailable)?;
        let agent = duduclaw_core::agent_toml::read_effective_text(&agent_dir)
            .map_err(|_| VerificationError::Unavailable)?;
        let agent: toml::Table = toml::from_str(&agent).map_err(|_| VerificationError::Invalid)?;
        Ok(Self { global: global.get("odoo").cloned().ok_or(VerificationError::Unavailable)?, agent: agent.get("odoo").cloned() })
    }
    fn configs(&self) -> Result<(OdooConfig, Option<AgentOdooConfig>), VerificationError> {
        let mut global = toml::Table::new(); global.insert("odoo".into(), self.global.clone());
        let global = OdooConfig::from_toml(&global);
        let agent = match &self.agent {
            Some(agent) => {
                let mut table = toml::Table::new(); table.insert("odoo".into(), agent.clone());
                AgentOdooConfig::parse_agent_toml(&toml::to_string(&table).map_err(|_| VerificationError::Invalid)?)
                    .map_err(|_| VerificationError::Invalid)?
            },
            None => None,
        };
        Ok((global, agent))
    }
}

pub struct OdooConnectorProvider {
    home: PathBuf,
    agent: String,
    employee: EmployeeId,
    account: ProductConnectorAccount,
    source: SourceSnapshot,
}
/// A per-call connector whose immutable guard follows every clone and RPC.
/// No credential bytes are exposed to the tool handler.
pub struct PreparedOdooConnector {
    pub global: OdooConfig,
    pub agent: Option<AgentOdooConfig>,
    pub connector: Arc<OdooConnector>,
}
impl OdooConnectorProvider {
    pub fn new(home: PathBuf, agent: String, employee: EmployeeId, account: ProductConnectorAccount)
        -> Result<Self, VerificationError> {
        if !home.is_absolute() || !duduclaw_core::is_valid_agent_id(&agent)
            || duduclaw_core::is_reserved_agent_id(&agent) || agent == duduclaw_core::UNTRUSTED_AGENT_ID {
            return Err(VerificationError::Invalid);
        }
        let source = SourceSnapshot::load(&home, &agent)?;
        let (mut config, override_config) = source.configs()?;
        if let Some(agent) = &override_config { agent.apply_to(&mut config).map_err(|_| VerificationError::Invalid)?; }
        if !config.is_configured() { return Err(VerificationError::Unavailable); }
        // A private registration needs explicit agent-owned credential fields;
        // omitted fields must never inherit the shared home's credential.
        if matches!(account, ProductConnectorAccount::Personal { .. }) {
            let table = source.agent.as_ref().and_then(toml::Value::as_table).ok_or(VerificationError::Invalid)?;
            let present = |name: &str| table.get(name).and_then(toml::Value::as_str).is_some_and(|s| !s.is_empty());
            if !present("username") || (!present("api_key_enc") && !present("password_enc")) {
                return Err(VerificationError::BindingMismatch);
            }
            // api_key wins in the existing merger; a private password-only
            // override may not accidentally pick up a shared api key.
            if !present("api_key_enc") && !config.api_key_enc.is_empty() { return Err(VerificationError::BindingMismatch); }
        }
        Ok(Self { home, agent, employee, account, source })
    }
    pub fn check_operation(&self, operation: Option<(&str, String)>) -> Result<(), VerificationError> {
        if !self.live() { return Err(VerificationError::Unavailable); }
        if let Some((verb, model)) = operation {
            let (global, agent) = self.source.configs()?;
            global.feature_allows_model(&model).map_err(|_| VerificationError::BindingMismatch)?;
            if agent.as_ref().is_some_and(|agent| !agent.permits_model(&model) || !agent.permits(verb, &model)) {
                return Err(VerificationError::BindingMismatch);
            }
        }
        Ok(())
    }
    pub fn agent(&self) -> &str { &self.agent }
    pub fn home(&self) -> &Path { &self.home }
    fn live(&self) -> bool { SourceSnapshot::load(&self.home, &self.agent).is_ok_and(|snapshot| snapshot == self.source) }
    pub async fn prepare(&self, selected: SelectedConnector, call: &AuthorizedToolCall)
        -> Result<PreparedOdooConnector, VerificationError> {
        selected.verify_call(call)?;
        if selected.account() != self.account.account() || !self.live() { return Err(VerificationError::BindingMismatch); }
        let (global, agent) = self.source.configs()?;
        let mut config = global.clone();
        if let Some(agent) = &agent { agent.apply_to(&mut config).map_err(|_| VerificationError::Invalid)?; }
        let selected = Arc::new(selected);
        let home = self.home.clone(); let employee = self.agent.clone(); let source = self.source.clone();
        let io: duduclaw_odoo::connector::IoAuthority = Arc::new(move ||
            SourceSnapshot::load(&home, &employee).is_ok_and(|current| current == source) && selected.recheck().is_ok());
        if !io() { return Err(VerificationError::Unavailable); }
        let reference = if !config.api_key_enc.is_empty() { &config.api_key_enc } else { &config.password_enc };
        if reference.is_empty() { return Err(VerificationError::Unavailable); }
        let manager = duduclaw_security::secret_manager::SecretManagerConfig::load_from_home(&self.home).await;
        if !io() { return Err(VerificationError::Unavailable); }
        let credential = SecretRef::from_single(reference).resolve(&manager, &self.home).await
            .map(Secret::expose_owned).map(zeroize::Zeroizing::new).ok_or(VerificationError::Unavailable)?;
        if !io() { return Err(VerificationError::Unavailable); }
        let mut connector = OdooConnector::connect_checked(&config, &credential, io).await
            .map_err(|_| VerificationError::Unavailable)?;
        if let Some(agent) = &agent { connector = connector.with_company_ids(agent.company_ids.iter().map(|&id| id as i64).collect()); }
        connector.verify_io().map_err(|_| VerificationError::Unavailable)?;
        Ok(PreparedOdooConnector { global, agent, connector: Arc::new(connector) })
    }
}
impl LiveConnectorSelectionPolicy for OdooConnectorProvider {
    fn supports(&self, tool: &str) -> bool { provider_for_tool(tool) == Some("odoo") }
    fn candidates(&self, context: &RequestContext, tool: &str, provider: &str)
        -> Result<Vec<ProductConnectorAccount>, VerificationError> {
        context.verify_live(duduclaw_mcp_boundary::authority::now_seconds()?)?;
        if context.record().employee != self.employee || provider != "odoo" || !self.supports(tool) || !self.live() {
            return Err(VerificationError::Unavailable);
        }
        Ok(vec![self.account.clone()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn home(agent: &str) -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("agents/employee-a")).unwrap();
        std::fs::write(home.path().join("agents/employee-a/agent.toml"), agent).unwrap();
        std::fs::write(home.path().join("config.toml"), "[odoo]\nurl='https://odoo.example.com'\ndb='demo'\nusername='shared'\napi_key_enc='shared-reference'\nfeatures_crm=true\n").unwrap();
        home
    }
    fn account() -> ProductConnectorAccount { ProductConnectorAccount::Personal { account:"alice-odoo".into(), owner:"alice".into() } }
    #[test]
    fn private_odoo_source_never_inherits_home_credentials() {
        for agent in ["", "[odoo]\nusername='alice'\n", "[odoo]\nusername='alice'\npassword_enc='private-reference'\n"] {
            let home = home(agent);
            assert!(OdooConnectorProvider::new(home.path().into(), "employee-a".into(), EmployeeId([2;16]), account()).is_err());
        }
    }
    #[test]
    fn odoo_live_policy_preserves_action_model_and_configuration_fences() {
        let home = home("[odoo]\nusername='alice'\napi_key_enc='private-reference'\nallowed_models=['crm.lead']\nallowed_actions=['read','search']\n");
        let provider = OdooConnectorProvider::new(home.path().into(), "employee-a".into(), EmployeeId([2;16]), account()).unwrap();
        assert!(provider.check_operation(Some(("search", "crm.lead".into()))).is_ok());
        assert!(provider.check_operation(Some(("write", "crm.lead".into()))).is_err());
        assert!(provider.check_operation(Some(("read", "res.partner".into()))).is_err());
        std::fs::write(home.path().join("agents/employee-a/agent.toml"), "[odoo]\nusername='bob'\napi_key_enc='different-reference'\n").unwrap();
        assert!(provider.check_operation(None).is_err());
    }
}
