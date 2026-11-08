//! Fresh product writes for migration approvals. Must run on the blocking pool.
//! Durable plans are encrypted and never exposed by confirmation RPCs.
use std::path::{Path, PathBuf};
use std::io::{Read, Write};
use duduclaw_migration::confirmation::{ConfirmationError as Error, ProposalValidator, ProposalApplier, RotationVerifier, secret_fingerprint};
use duduclaw_migration::{contract::*, ids::Sha256Digest};
use duduclaw_security::crypto::CryptoEngine;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, Deserialize};
use super::{config_commit, config_raw_rpc};
type Result<T> = std::result::Result<T, Error>;
#[path = "migration_authority_bridge.rs"]
mod bridge;
use bridge::bounded;
#[path = "migration_authority_accounts.rs"]
mod accounts;
#[path = "migration_authority_categories.rs"]
mod categories;
#[path = "migration_authority_bindings.rs"]
mod bindings;
#[path = "migration_authority_artifacts.rs"]
mod artifacts;
#[path = "migration_authority_artifact_receipt.rs"]
mod artifact_receipt;
#[path = "migration_authority_channels.rs"]
mod channels;
#[path = "migration_authority_schedules.rs"]
mod schedules;
#[path = "migration_authority_policies.rs"]
mod policies;
#[path = "migration_authority_data_grants.rs"]
mod data_grants;
#[path = "migration_authority_departments.rs"]
mod departments;
#[path = "migration_authority_config_receipt.rs"]
mod config_receipt;
#[path = "migration_authority_employee.rs"]
pub(crate) mod employee;
// v1.81 C5: legacy connector accounts → catalog routes via C3 provisioning.
#[path = "migration_authority_connectors.rs"]
pub(crate) mod connectors;
pub(crate) use accounts::GatewayAccountReadback;
pub(crate) use categories::LiveAuthority;

/// Category integrations that require the live registry, root employee allocator,
/// storage service or an authenticated Personal owner. No permissive implementation.
/// Integrators must route all four effects to the same normal product API and
/// bind every remote call to item.binding; a stage file is never live authority.
pub(crate) trait MigrationAuthorityBackend: Send + Sync {
    /// Read the immutable principal and confirmed UID/quota allocation from the
    /// live B/D/H registry. A proposal label or Applied row is not this proof.
    fn readback_shared_employee(&self, _agent: &str) -> Result<()> { Err(Error::Denied) }
    fn supports(&self, category: ConfirmationCategory) -> bool;
    fn validate(&self, item: &ConfirmationItem) -> Result<()>;
    fn stage(&self, item: &ConfirmationItem) -> Result<()>;
    fn publish(&self, item: &ConfirmationItem) -> Result<()>;
    fn read_back(&self, item: &ConfirmationItem) -> Result<()>;
    fn disable(&self, item: &ConfirmationItem) -> Result<()>;
}

/// Runtime configuration owner must reload and read back the effective value.
/// Filesystem equality alone cannot prove that cached drivers or channels changed.
pub(crate) trait MigrationConfigReadback: Send + Sync {
    fn supports(&self, item: &ConfirmationItem) -> bool;
    fn reload_and_verify(&self, item: &ConfirmationItem, relative: &str, expected: &str) -> Result<()>;
}

pub(crate) struct GatewayAuthority {
    home: PathBuf,
    db: Connection,
    key: [u8; 32],
    backend: Option<std::sync::Arc<dyn MigrationAuthorityBackend>>,
    runtime: Option<std::sync::Arc<dyn MigrationConfigReadback>>,
    live: Option<std::sync::Arc<LiveAuthority>>,
    connector_env: connectors::ConnectorEnv,
}
#[derive(Serialize, Deserialize)]
#[serde(tag="kind", deny_unknown_fields)]
enum Plan {
    Department(departments::DepartmentPlan),
    Mcp(categories::McpPlan),
    Profile(categories::ProfilePlan),
    Binding(bindings::BindingPlan),
    Artifact(artifacts::ArtifactPlan),
    Channel(channels::ChannelPlan),
    Schedule(schedules::SchedulePlan),
    DataGrant(data_grants::GrantPlan),
    User { email: String, display: String, role: duduclaw_auth::UserRole },
    Config { relative: String, before: String, after: String },
    Connector(connectors::ConnectorPlan),
}

impl GatewayAuthority {
    pub(crate) fn new(home: PathBuf) -> Result<Self> {
        if !std::fs::symlink_metadata(&home).map_err(|_| Error::Storage)?.is_dir() { return Err(Error::Storage); }
        let root = home.join("migration-authority");
        create_private_dir(&root)?;
        let key = key_file(&root.join("key"))?;
        let path = root.join("apply.db");
        create_private_file(&path)?;
        let db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE).map_err(|_| Error::Storage)?;
        db.busy_timeout(std::time::Duration::from_secs(5)).map_err(|_| Error::Storage)?;
        db.execute_batch("PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS plans (
            operation TEXT PRIMARY KEY, digest BLOB NOT NULL, plan BLOB NOT NULL);
            CREATE TABLE IF NOT EXISTS rotated (
            operation TEXT NOT NULL, digest BLOB NOT NULL, field TEXT NOT NULL,
            fingerprint BLOB NOT NULL, path TEXT NOT NULL, PRIMARY KEY(operation,field));
            CREATE TABLE IF NOT EXISTS secrets (operation TEXT PRIMARY KEY, digest BLOB NOT NULL, value BLOB NOT NULL);
            CREATE TABLE IF NOT EXISTS config_publications (operation TEXT NOT NULL, digest BLOB NOT NULL,
                phase TEXT NOT NULL, identity BLOB NOT NULL, PRIMARY KEY(operation,phase));").map_err(|_| Error::Storage)?;
        Ok(Self { home, db, key, backend: None, runtime: None, live: None, connector_env: connectors::ConnectorEnv::default() })
    }
    pub(crate) fn with_live(mut self,live:std::sync::Arc<LiveAuthority>)->Self {self.live=Some(live);self}
    /// Root allocator / audit actor used by the Connector category (tests inject fixtures).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn with_connector_env(mut self, env: connectors::ConnectorEnv) -> Self { self.connector_env = env; self }
    /// The live dashboard Admin (or the injected actor) for C3 audit attribution.
    fn connector_actor(&self) -> Result<crate::isolation_connector_lifecycle::AdminActor> {
        self.connector_env.actor(self.live.as_ref().map(|live| live.context.user_id.as_str()))
    }
    pub(crate) fn with_backend(mut self, backend: std::sync::Arc<dyn MigrationAuthorityBackend>) -> Self {
        self.backend = Some(backend); self
    }
    pub(crate) fn with_runtime(mut self, runtime: std::sync::Arc<dyn MigrationConfigReadback>) -> Self {
        self.runtime = Some(runtime); self
    }
    fn require_shared_employee(&self, item: &ConfirmationItem) -> Result<()> {
        self.backend.as_ref().ok_or(Error::Denied)?.readback_shared_employee(text(item,"employee_id")?)
    }
    pub(crate) fn built_in_item_supported(item: &ConfirmationItem) -> bool {
        if item.fields.iter().any(|f|f.name=="requires_reentry") {return false;}
        if policies::item_supported(item) {return true;}
        match item.category {
            ConfirmationCategory::Department | ConfirmationCategory::User | ConfirmationCategory::Budget => true,
            ConfirmationCategory::McpServer => !item.fields.iter().any(|f|matches!(f.name.as_str(),"credential"|"requires_reentry")) && text(item,"transport").ok()==Some("stdio"),
            ConfirmationCategory::Policy => text(item, "name").ok() == Some("delegation"),
            ConfirmationCategory::ConfigurationSection => text(item, "section").ok() == Some("delegation"),
            ConfirmationCategory::Connector => connectors::item_supported(item),
            _ => false,
        }
    }
    fn verify_effective_config(&self, item: &ConfirmationItem, relative: &str, expected: &str) -> Result<()> {
        if !Self::built_in_item_supported(item) {
            let runtime = self.runtime.as_ref().filter(|r| r.supports(item)).ok_or(Error::Denied)?;
            runtime.reload_and_verify(item, relative, expected)?;
        }
        if policies::item_supported(item) {policies::read_back(&self.home,item,expected)?;}
        // Budget checks and delegation gates read these files on every call.
        if read_optional(&self.home.join(relative))? != expected { return Err(Error::Apply); }
        Ok(())
    }
    pub(crate) fn supports(&self, category: ConfirmationCategory) -> bool {
        Self::built_in_supports(category)
            || self.backend.as_ref().is_some_and(|b| b.supports(category))
    }
    pub(crate) fn built_in_supports(category: ConfirmationCategory) -> bool {
        matches!(category, ConfirmationCategory::DataSourceAuthorization | ConfirmationCategory::Schedule | ConfirmationCategory::ChannelBinding | ConfirmationCategory::Skill | ConfirmationCategory::Hook | ConfirmationCategory::UserAgentBinding | ConfirmationCategory::EmployeeInstructions | ConfirmationCategory::McpServer | ConfirmationCategory::Department | ConfirmationCategory::User
            | ConfirmationCategory::ConfigurationSection | ConfirmationCategory::Policy | ConfirmationCategory::Budget
            | ConfirmationCategory::Connector)
    }
    fn plan(&self, item: &ConfirmationItem) -> Result<Option<Plan>> {
        let row: Option<(Vec<u8>,Vec<u8>)> = self.db.query_row("SELECT digest,plan FROM plans WHERE operation=?1",
            [operation(&item.binding)], |r| Ok((r.get(0)?,r.get(1)?))).optional().map_err(|_| Error::Storage)?;
        let Some((digest, encrypted)) = row else { return Ok(None); };
        if digest != item.binding.proposal_digest.0 { return Err(Error::Stale); }
        let clear = CryptoEngine::new(&self.key).map_err(|_| Error::Storage)?.decrypt(&encrypted).map_err(|_| Error::Storage)?;
        let (bound, plan): (ProposalBinding, Plan) = serde_json::from_slice(&clear).map_err(|_| Error::Storage)?;
        if bound != item.binding { return Err(Error::Stale); }
        Ok(Some(plan))
    }
    fn save_plan(&self, item: &ConfirmationItem, plan: &Plan) -> Result<()> {
        let encoded = serde_json::to_vec(&(item.binding,plan)).map_err(|_| Error::Invalid)?;
        let encrypted = CryptoEngine::new(&self.key).map_err(|_| Error::Storage)?.encrypt(&encoded).map_err(|_| Error::Storage)?;
        self.db.execute("INSERT INTO plans VALUES(?1,?2,?3)", params![operation(&item.binding),item.binding.proposal_digest.0.as_slice(),encrypted])
            .map_err(|_| Error::Storage)?;
        Ok(())
    }
    fn user_db(&self) -> Result<duduclaw_auth::UserDb> {
        regular(&self.home.join("users.db"))?;
        duduclaw_auth::UserDb::new(&self.home.join("users.db")).map_err(|_| Error::Storage)
    }
    fn config_plan(&self, item: &ConfirmationItem, resolve_secret: bool) -> Result<Plan> {
        let (relative, section, mut value) = config_value(item)?;
        let path = self.home.join(&relative);
        validate_parents(&self.home,&path)?;
        let before = read_optional(&path)?;
        let mut table: toml::Table = before.parse().map_err(|_| Error::Invalid)?;
        if section=="accounts" {
            let secret=if resolve_secret {self.legacy_secret(item)?} else {config_commit::RAW_SECRET_MASK.into()};
            accounts::merge_account(&self.home,&mut table,item,&secret,resolve_secret)?;
        } else {
        if contains_secret(&value) {
            if item.category != ConfirmationCategory::ConfigurationSection { return Err(Error::Invalid); }
            let paths = secret_paths(value.as_table().ok_or(Error::Invalid)?)?;
            if paths.len() != 1 { return Err(Error::Invalid); }
            let secret = if resolve_secret { self.legacy_secret(item)? } else { config_commit::RAW_SECRET_MASK.into() };
            value.as_table_mut().ok_or(Error::Invalid)?.insert(paths[0].clone(),toml::Value::String(secret));
        }
        if item.category == ConfirmationCategory::Budget {
            let existing=table.entry("budget".to_string()).or_insert_with(||toml::Value::Table(toml::Table::new()))
                .as_table_mut().ok_or(Error::Invalid)?;
            for (key,value) in value.as_table().ok_or(Error::Invalid)? {existing.insert(key.clone(),value.clone());}
        } else { table.insert(section.clone(), value); }
        }
        if section=="delegation" {
            let (_,pairs,warnings)=super::MethodHandler::parse_delegation_section(&table);
            if !warnings.is_empty() || pairs.len()>200 {return Err(Error::Invalid);}
            for (a,b) in pairs {
                if !duduclaw_core::is_valid_agent_id(&a) || !duduclaw_core::is_valid_agent_id(&b)
                    || !self.home.join("agents").join(a).join("agent.toml").is_file()
                    || !self.home.join("agents").join(b).join("agent.toml").is_file() {return Err(Error::Invalid);}
            }
        }
        config_raw_rpc::validate_config_table(&table).map_err(|_| Error::Invalid)?;
        if policies::section_supported(&section) {policies::validate(&section,&table)?;}
        if relative != "config.toml" {
            let _: duduclaw_core::types::AgentConfig = toml::Value::Table(table.clone()).try_into().map_err(|_| Error::Invalid)?;
        }
        let after = config_commit::render_preserving(&before, &table).map_err(|_| Error::Invalid)?;
        crate::product_policy::validate_write(&self.home, &path, &before, &after).map_err(|_| Error::Denied)?;
        Ok(Plan::Config { relative, before, after })
    }
    /// Root import/controller uses this before staging the proposal. Returns only
    /// an HMAC; the protected key and plaintext never enter proposal/audit rows.
    pub(crate) fn fingerprint_secret(&self, secret: &str) -> Result<ProposalValue> {
        secret_fingerprint(&self.key,"credential",secret.as_bytes(),true)
    }
    /// Root-only import integration: persist the original separately, encrypted,
    /// only when its fingerprint matches the already-bound proposal.
    pub(crate) fn stage_legacy_secret(&self, item: &ConfirmationItem, secret: &str) -> Result<()> {
        bridge::operation(|| {
        if item.category != ConfirmationCategory::ConfigurationSection { return Err(Error::Denied); }
        let fingerprint=self.fingerprint_secret(secret)?;
        let expected=item.fields.iter().find(|f|f.name=="credential").ok_or(Error::Invalid)?;
        if expected.value != fingerprint {return Err(Error::Denied);}
        let encrypted=CryptoEngine::new(&self.key).map_err(|_|Error::Storage)?
            .encrypt(&serde_json::to_vec(&(item.binding,secret)).map_err(|_|Error::Invalid)?).map_err(|_|Error::Storage)?;
        self.db.execute("INSERT INTO secrets VALUES(?1,?2,?3) ON CONFLICT(operation) DO NOTHING",
            params![operation(&item.binding),item.binding.proposal_digest.0.as_slice(),encrypted]).map_err(|_|Error::Storage)?;
        if self.legacy_secret(item)?!=secret {return Err(Error::Stale);}
        Ok(())
        })
    }
    fn legacy_secret(&self,item:&ConfirmationItem)->Result<String> {
        let (digest,encrypted):(Vec<u8>,Vec<u8>)=self.db.query_row("SELECT digest,value FROM secrets WHERE operation=?1",
            [operation(&item.binding)],|r|Ok((r.get(0)?,r.get(1)?))).map_err(|_|Error::Denied)?;
        if digest!=item.binding.proposal_digest.0 {return Err(Error::Stale);}
        let decoded=CryptoEngine::new(&self.key).map_err(|_|Error::Storage)?.decrypt(&encrypted).map_err(|_|Error::Storage)?;
        let (bound,secret):(ProposalBinding,String)=serde_json::from_slice(&decoded).map_err(|_|Error::Storage)?;
        if bound!=item.binding {return Err(Error::Stale);}
        Ok(secret)
    }
    /// Called only after the RPC's live Admin, CSRF and step-up check. Publish via
    /// the normal config writer, then recompute evidence from the live readback.
    /// A crash after publication is safe: retrying the same value reads it back.
    pub(crate) fn rotate_secret(&self,item:&ConfirmationItem,new_value:&str)->Result<Sha256Digest> {
        bridge::operation(|| {
        if item.status!=ConfirmationStatus::Applied || item.category!=ConfirmationCategory::ConfigurationSection
            || new_value.is_empty() || new_value.len()>MAX_PROPOSAL_VALUE_BYTES
            || config_commit::is_secret_placeholder(new_value) {return Err(Error::Denied);}
        let runtime = self.runtime.as_ref().filter(|r| r.supports(item)).ok_or(Error::Denied)?;
        if text(item,"section")?=="accounts" {return self.rotate_account(item,new_value,runtime.as_ref());}
        let old=self.legacy_secret(item)?;
        if old==new_value {return Err(Error::Denied);}
        let section=text(item,"section")?;
        let settings:toml::Table=text(item,"settings")?.parse().map_err(|_|Error::Invalid)?;
        let paths=secret_paths(&settings)?;
        if paths.len()!=1 {return Err(Error::Invalid);}
        let path=format!("{section}.{}",paths[0]);
        let file=self.home.join("config.toml");
        let before=read_optional(&file)?;
        let mut table:toml::Table=before.parse().map_err(|_|Error::Storage)?;
        let current=config_commit::toml_at(&table,&path).and_then(toml::Value::as_str).ok_or(Error::Denied)?;
        if current!=old && current!=new_value {return Err(Error::Stale);}
        table.get_mut(section).and_then(toml::Value::as_table_mut).ok_or(Error::Invalid)?
            .insert(paths[0].clone(),toml::Value::String(new_value.into()));
        config_raw_rpc::validate_config_table(&table).map_err(|_|Error::Invalid)?;
        let after=config_commit::render_preserving(&before,&table).map_err(|_|Error::Invalid)?;
        config_commit::commit_text_locked(&file,&config_commit::content_hash(&before),&after).map_err(|_|Error::Apply)?;
        runtime.reload_and_verify(item, "config.toml", &after)?;
        let live:toml::Table=read_optional(&file)?.parse().map_err(|_|Error::Storage)?;
        let observed=config_commit::toml_at(&live,&path).and_then(toml::Value::as_str).ok_or(Error::Denied)?;
        if observed!=new_value {return Err(Error::Apply);}
        let ProposalValue::SecretFingerprint {keyed_fingerprint,..}=self.fingerprint_secret(observed)? else {return Err(Error::Invalid);};
        self.db.execute("INSERT INTO rotated VALUES(?1,?2,'credential',?3,?4) ON CONFLICT(operation,field) DO UPDATE SET fingerprint=excluded.fingerprint,path=excluded.path WHERE digest=excluded.digest",
            params![operation(&item.binding),item.binding.proposal_digest.0.as_slice(),keyed_fingerprint.0.as_slice(),path]).map_err(|_|Error::Storage)?;
        Ok(keyed_fingerprint)
        })
    }
    fn rotate_account(&self,item:&ConfirmationItem,new_value:&str,runtime:&dyn MigrationConfigReadback)->Result<Sha256Digest> {
        let input=accounts::account_input(item)?;
        let old=self.legacy_secret(item)?;
        if old==new_value {return Err(Error::Denied);}
        let file=self.home.join("config.toml");
        let before=read_optional(&file)?;
        let mut table:toml::Table=before.parse().map_err(|_|Error::Storage)?;
        let account=accounts::find_account(&table,&input.id)?.ok_or(Error::Denied)?;
        if account.get("provider").and_then(toml::Value::as_str)!=Some(input.provider.as_str()) {return Err(Error::Stale);}
        let current=accounts::read_account_secret(&account,&self.home)?;
        if current.expose()!=old && current.expose()!=new_value {return Err(Error::Stale);}
        accounts::rotate_account_key(&self.home,&mut table,item,new_value)?;
        config_raw_rpc::validate_config_table(&table).map_err(|_|Error::Invalid)?;
        let after=config_commit::render_preserving(&before,&table).map_err(|_|Error::Invalid)?;
        config_commit::commit_text_locked(&file,&config_commit::content_hash(&before),&after).map_err(|_|Error::Apply)?;
        runtime.reload_and_verify(item,"config.toml",&after)?;
        let live:toml::Table=read_optional(&file)?.parse().map_err(|_|Error::Storage)?;
        let path=format!("accounts:{}",input.id);
        let observed=self.read_secret_path(&live,&path)?;
        if observed!=new_value {return Err(Error::Apply);}
        let ProposalValue::SecretFingerprint {keyed_fingerprint,..}=self.fingerprint_secret(&observed)? else {return Err(Error::Invalid);};
        self.db.execute("INSERT INTO rotated VALUES(?1,?2,'credential',?3,?4) ON CONFLICT(operation,field) DO UPDATE SET fingerprint=excluded.fingerprint,path=excluded.path WHERE digest=excluded.digest",
            params![operation(&item.binding),item.binding.proposal_digest.0.as_slice(),keyed_fingerprint.0.as_slice(),path]).map_err(|_|Error::Storage)?;
        Ok(keyed_fingerprint)
    }
    fn read_secret_path(&self,table:&toml::Table,path:&str)->Result<String> {
        if let Some(id)=path.strip_prefix("accounts:") {
            let account=accounts::find_account(table,id)?.ok_or(Error::Denied)?;
            return Ok(accounts::read_account_secret(&account,&self.home)?.expose_owned());
        }
        config_commit::toml_at(table,path).and_then(toml::Value::as_str).map(str::to_owned).ok_or(Error::Denied)
    }

}
impl ProposalValidator for GatewayAuthority {
    fn validate_fresh(&self, item: &ConfirmationItem) -> Result<()> {
        bridge::operation(|| {
        item.validate().map_err(|_| Error::Invalid)?;
        if item.fields.iter().any(|f|f.name=="requires_reentry") {return Err(Error::Denied);}
        match item.category {
            ConfirmationCategory::Department => {
                let name = text(item,"name")?;
                if !duduclaw_core::is_valid_department(name) { return Err(Error::Invalid); }
                if item.fields.iter().any(|f| f.name == "display_name") && text(item,"display_name")? != name { return Err(Error::Invalid); }
                Ok(())
            }
            ConfirmationCategory::User => {
                let email = text(item,"name")?;
                if duduclaw_auth::db::validate_account_email(email).ok() != Some(email)
                    || !config_commit::is_plain_text(text(item,"display_name")?,200) { return Err(Error::Invalid); }
                let _: duduclaw_auth::UserRole = text(item,"role")?.parse().map_err(|_| Error::Invalid)?;
                Ok(())
            }
            ConfirmationCategory::ConfigurationSection | ConfirmationCategory::Policy | ConfirmationCategory::Budget => {
                if !Self::built_in_item_supported(item) && !self.runtime.as_ref().is_some_and(|r| r.supports(item)) {
                    return Err(Error::Denied);
                }
                self.config_plan(item, false).map(|_| ())
            }
            ConfirmationCategory::UserAgentBinding => {self.require_shared_employee(item)?;bindings::binding_plan(&self.home,item).map(|_|())},
            ConfirmationCategory::DataSourceAuthorization => {self.require_shared_employee(item)?;data_grants::grant_plan(&self.home,item,self.live.as_ref().ok_or(Error::Denied)?).map(|_|())},
            ConfirmationCategory::Schedule => {self.require_shared_employee(item)?;schedules::schedule_plan(&self.home,item,self.live.as_ref().ok_or(Error::Denied)?).map(|_|())},
            ConfirmationCategory::ChannelBinding => {self.require_shared_employee(item)?;self.live.as_ref().ok_or(Error::Denied)?;channels::channel_plan(&self.home,item).map(|_|())},
            ConfirmationCategory::Skill => {self.live.as_ref().ok_or(Error::Denied)?;artifacts::skill_plan(&self.home,item).map(|_|())},
            ConfirmationCategory::Hook => {self.live.as_ref().ok_or(Error::Denied)?;artifacts::hook_plan(&self.home,item).map(|_|())},
            ConfirmationCategory::EmployeeInstructions => {self.live.as_ref().ok_or(Error::Denied)?;categories::profile_plan(&self.home,item).map(|_|())},
            ConfirmationCategory::McpServer => categories::mcp_plan(&self.home,item).map(|_|()),
            ConfirmationCategory::Connector => connectors::validate(&self.home,item,&self.connector_env).map(|_|()),
            ConfirmationCategory::LegacyApproval => Err(Error::Denied),
            _ => self.backend.as_ref().ok_or(Error::Denied)?.validate(item),
        }
        })
    }
}
impl ProposalApplier for GatewayAuthority {
    fn stage(&mut self, item: &ConfirmationItem) -> Result<()> {
        bridge::operation(|| {
        self.validate_fresh(item)?;
        if self.plan(item)?.is_some() { return Ok(()); }
        if accounts::GatewayAccountReadback::item_supported(item) {
            let input=accounts::account_input(item)?;
            let table:toml::Table=read_optional(&self.home.join("config.toml"))?.parse().map_err(|_|Error::Invalid)?;
            if accounts::find_account(&table,&input.id)?.is_some() {return Err(Error::Stale);}
        }
        let plan = match item.category {
            ConfirmationCategory::Department => Plan::Department(departments::department_plan(&self.home,item)?),
            ConfirmationCategory::UserAgentBinding => {let plan=bindings::binding_plan(&self.home,item)?;plan.check_absent(&self.home)?;Plan::Binding(plan)},
            ConfirmationCategory::DataSourceAuthorization => Plan::DataGrant(data_grants::grant_plan(&self.home,item,self.live.as_ref().ok_or(Error::Denied)?)?),
            ConfirmationCategory::Schedule => Plan::Schedule(schedules::schedule_plan(&self.home,item,self.live.as_ref().ok_or(Error::Denied)?)?),
            ConfirmationCategory::ChannelBinding => Plan::Channel(channels::channel_plan(&self.home,item)?),
            ConfirmationCategory::Skill => {let mut plan=artifacts::skill_plan(&self.home,item)?;plan.prepare(&self.home,item)?;Plan::Artifact(plan)},
            ConfirmationCategory::Hook => {let mut plan=artifacts::hook_plan(&self.home,item)?;plan.prepare(&self.home,item)?;Plan::Artifact(plan)},
            ConfirmationCategory::EmployeeInstructions => Plan::Profile(categories::profile_plan(&self.home,item)?),
            ConfirmationCategory::McpServer => {let plan=categories::mcp_plan(&self.home,item)?;plan.check_absent(&self.home)?;Plan::Mcp(plan)},
            ConfirmationCategory::User => Plan::User { email:text(item,"name")?.into(),display:text(item,"display_name")?.into(),role:text(item,"role")?.parse().map_err(|_| Error::Invalid)? },
            ConfirmationCategory::ConfigurationSection | ConfirmationCategory::Policy | ConfirmationCategory::Budget => self.config_plan(item, true)?,
            // validate_fresh above already re-proved it; staging writes no route.
            ConfirmationCategory::Connector => Plan::Connector(connectors::parse(item)?),
            _ => return self.backend.as_ref().ok_or(Error::Denied)?.stage(item),
        };
        self.save_plan(item,&plan)
        })
    }
    fn publish(&mut self, item: &ConfirmationItem) -> Result<()> {
        bridge::operation(|| {
        let Some(plan) = self.plan(item)? else { return self.backend.as_ref().ok_or(Error::Denied)?.publish(item); };
        match plan {
            Plan::Binding(plan) => {self.require_shared_employee(item)?;plan.publish(&self.home)},
            Plan::DataGrant(plan) => {self.require_shared_employee(item)?;plan.publish(&self.home,self.live.as_ref().ok_or(Error::Denied)?)},
            Plan::Schedule(plan) => {self.require_shared_employee(item)?;plan.publish(&self.home,self.live.as_ref().ok_or(Error::Denied)?)},
            Plan::Channel(plan) => {self.require_shared_employee(item)?;plan.publish(&self.home)},
            Plan::Artifact(plan) => plan.publish(&self.home,self.live.as_ref().ok_or(Error::Denied)?),
            Plan::Mcp(plan) => plan.publish(&self.home),
            Plan::Profile(plan) => plan.publish(&self.home,self.live.as_ref().ok_or(Error::Denied)?),
            Plan::Department(mut plan) => {
                if plan.needs_stage(&self.home) {
                    plan=departments::department_plan(&self.home,item)?;
                    self.replace_department_plan(item,&plan)?;
                }
                plan.publish(&self.home)
            }
            Plan::User {email,display,role} => self.user_db()?.migration_invite_user(&operation(&item.binding),&item.binding.proposal_digest.0,&email,&display,role).map(|_|()).map_err(|_| Error::Apply),
            Plan::Config {relative,before,after} => self.publish_config(item,&relative,&before,&after),
            Plan::Connector(plan) => plan.publish(&self.home,item,&self.connector_env,&self.connector_actor()?),
        }
        })
    }
    fn read_back(&mut self, item: &ConfirmationItem) -> Result<()> {
        bridge::operation(|| {
        let Some(plan) = self.plan(item)? else { return self.backend.as_ref().ok_or(Error::Denied)?.read_back(item); };
        match plan {
            Plan::Binding(plan) => {self.require_shared_employee(item)?;plan.read_back(&self.home)},
            Plan::DataGrant(plan) => {self.require_shared_employee(item)?;plan.read_back(&self.home,self.live.as_ref().ok_or(Error::Denied)?,false)},
            Plan::Schedule(plan) => {self.require_shared_employee(item)?;plan.read_back(&self.home,self.live.as_ref().ok_or(Error::Denied)?)},
            Plan::Channel(plan) => {self.require_shared_employee(item)?;plan.read_back(&self.home,self.live.as_ref().ok_or(Error::Denied)?)},
            Plan::Artifact(plan) => plan.read_back(&self.home,self.live.as_ref().ok_or(Error::Denied)?),
            Plan::Mcp(plan) => plan.read_back(&self.home),
            Plan::Profile(plan) => plan.read_back(&self.home,self.live.as_ref().ok_or(Error::Denied)?,false),
            Plan::Department(plan) => plan.read_back(&self.home),
            Plan::User {email,display,role} => {
                let u = self.user_db()?.migration_user_readback(&operation(&item.binding),&item.binding.proposal_digest.0).map_err(|_| Error::Apply)?;
                if u.email != email || u.display_name != display || u.role != role || !u.must_change_password { return Err(Error::Apply); }
                Ok(())
            }
            Plan::Config {relative,before,after} => self.read_back_config(item,&relative,&before,&after),
            Plan::Connector(plan) => plan.read_back(&self.home,item,&self.connector_env),
        }
        })
    }
    fn disable_and_read_back(&mut self, item: &ConfirmationItem) -> Result<()> {
        bridge::operation(|| {
        let Some(plan) = self.plan(item)? else {
            return match &self.backend { Some(b) if b.supports(item.category) => b.disable(item), _ if Self::built_in_supports(item.category) => Ok(()), _ => Err(Error::Compensation) };
        };
        match plan {
            Plan::Binding(plan) => plan.disable(&self.home),
            Plan::DataGrant(plan) => plan.disable(&self.home,self.live.as_ref().ok_or(Error::Denied)?),
            Plan::Schedule(plan) => plan.disable(&self.home,self.live.as_ref().ok_or(Error::Denied)?),
            Plan::Channel(plan) => plan.disable(&self.home,self.live.as_ref().ok_or(Error::Denied)?),
            Plan::Artifact(plan) => plan.disable(&self.home,self.live.as_ref().ok_or(Error::Denied)?),
            Plan::Mcp(plan) => plan.disable(&self.home),
            Plan::Profile(plan) => plan.disable(&self.home,self.live.as_ref().ok_or(Error::Denied)?),
            Plan::Department(plan) => plan.disable(&self.home),
            Plan::User {..} => self.user_db()?.migration_user_disable(&operation(&item.binding),&item.binding.proposal_digest.0).map_err(|_| Error::Compensation),
            Plan::Config {relative,before,after} => self.disable_config(item,&relative,&before,&after),
            Plan::Connector(plan) => plan.disable(&self.home,item,&self.connector_actor().map_err(|_|Error::Compensation)?),
        }
        })
    }
}
impl RotationVerifier for GatewayAuthority {
    fn verify_rotated(&self, binding: &ProposalBinding, field: &str, fingerprint: Sha256Digest) -> Result<()> {
        bridge::operation(|| {
        let row: (Vec<u8>,Vec<u8>,String) = self.db.query_row("SELECT digest,fingerprint,path FROM rotated WHERE operation=?1 AND field=?2",
            params![operation(binding),field], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(|_| Error::Denied)?;
        if row.0 != binding.proposal_digest.0 || row.1 != fingerprint.0 { return Err(Error::Denied); }
        let live:toml::Table=read_optional(&self.home.join("config.toml"))?.parse().map_err(|_|Error::Storage)?;
        let observed=self.read_secret_path(&live,&row.2)?;
        let ProposalValue::SecretFingerprint {keyed_fingerprint,..}=self.fingerprint_secret(&observed)? else {return Err(Error::Invalid);};
        if keyed_fingerprint!=fingerprint {return Err(Error::Denied);}
        Ok(())
        })
    }
}
fn operation(binding: &ProposalBinding) -> String { format!("{}-{:x}",hex::encode(binding.operation_id.proposal_id.0),binding.operation_id.revision) }
fn text<'a>(item: &'a ConfirmationItem, name: &str) -> Result<&'a str> {
    match item.fields.iter().find(|f| f.name == name).map(|f| &f.value) {
        Some(ProposalValue::Text {value}) => Ok(value), _ => Err(Error::Invalid),
    }
}
fn config_value(item: &ConfirmationItem) -> Result<(String,String,toml::Value)> {
    match item.category {
        ConfirmationCategory::ConfigurationSection | ConfirmationCategory::Policy => {
            let policy = item.category == ConfirmationCategory::Policy;
            let section = text(item,if policy {"name"} else {"section"})?;
            if !section.bytes().all(|b|b.is_ascii_lowercase() || b == b'_') || section.is_empty()
                || matches!(section,"gateway"|"server"|"mfa"|"auth"|"mcp_keys"|"google")
                || (policy && section!="delegation" && !policies::section_supported(section)) { return Err(Error::Denied); }
            let table: toml::Table = text(item,if policy {"rules"} else {"settings"})?.parse().map_err(|_|Error::Invalid)?;
            Ok(("config.toml".into(),section.into(),toml::Value::Table(table)))
        }
        ConfirmationCategory::Budget => {
            let agent = text(item,"name")?;
            if !duduclaw_core::is_valid_agent_id(agent) { return Err(Error::Invalid); }
            let amount: i64 = text(item,"amount_cents")?.parse().map_err(|_| Error::Invalid)?;
            if amount <= 0 { return Err(Error::Invalid); }
            let key = match text(item,"period")? { "day"=>"daily_cap_cents", "month"=>"monthly_limit_cents", _=>return Err(Error::Invalid) };
            let mut table = toml::Table::new();
            table.insert(key.into(),toml::Value::Integer(amount));
            table.insert("hard_stop".into(),toml::Value::Boolean(true));
            table.insert("when_unavailable".into(),toml::Value::String("deny".into()));
            Ok((format!("agents/{agent}/agent.toml"),"budget".into(),toml::Value::Table(table)))
        }
        _ => Err(Error::Denied),
    }
}
fn contains_secret(value: &toml::Value) -> bool {
    match value {
        toml::Value::Table(t) => t.iter().any(|(k,v)|config_raw_rpc::is_secret_key(k) || matches!(k.as_str(),"env"|"headers"|"otlp_headers") || contains_secret(v)),
        toml::Value::Array(a) => a.iter().any(contains_secret),
        toml::Value::String(s) => config_raw_rpc::url_has_password(s) || config_raw_rpc::url_has_secret_query(s), _=>false,
    }
}
fn secret_paths(table: &toml::Table) -> Result<Vec<String>> {
    let mut paths=Vec::new();
    for (k,v) in table {
        if config_raw_rpc::is_secret_key(k) && v.as_str()==Some(config_commit::RAW_SECRET_MASK) { paths.push(k.clone()); }
        else if contains_secret(v) || config_raw_rpc::is_secret_key(k) { return Err(Error::Denied); }
    }
    Ok(paths)
}
fn validate_parents(home:&Path,path:&Path)->Result<()> {
    let relative=path.strip_prefix(home).map_err(|_|Error::Denied)?;
    let mut current=home.to_path_buf();
    let parts:Vec<_>=relative.components().collect();
    for part in parts.iter().take(parts.len().saturating_sub(1)) {
        if !matches!(part,std::path::Component::Normal(_)) {return Err(Error::Denied);}
        current.push(part.as_os_str());plain_directory(&current)?;
    }
    Ok(())
}
fn regular(path: &Path) -> Result<()> {
    let meta=std::fs::symlink_metadata(path).map_err(|_|Error::Storage)?;
    if !meta.file_type().is_file() { return Err(Error::Storage); }
    Ok(())
}
fn plain_directory(path: &Path) -> Result<()> {
    if !std::fs::symlink_metadata(path).map_err(|_|Error::Storage)?.file_type().is_dir() { return Err(Error::Storage); }
    Ok(())
}
fn read_optional(path: &Path) -> Result<String> {
    match config_commit::read_regular_config(path) {
        Ok(s) if s.len() <= 1024*1024 => Ok(s),
        Err(e) if e.kind()==std::io::ErrorKind::NotFound => Ok(String::new()),
        _ => Err(Error::Storage),
    }
}
fn create_private_dir(path: &Path) -> Result<()> {
    let mut b=std::fs::DirBuilder::new();
    #[cfg(unix)] { use std::os::unix::fs::DirBuilderExt; b.mode(0o700); }
    match b.create(path) { Ok(())=>(),Err(e) if e.kind()==std::io::ErrorKind::AlreadyExists=>(),_=>return Err(Error::Storage) }
    plain_directory(path)?;
    #[cfg(unix)] { use std::os::unix::fs::MetadataExt;
        let m=std::fs::symlink_metadata(path).map_err(|_|Error::Storage)?;
        // SAFETY: geteuid has no arguments or memory preconditions.
        if m.mode() & 0o077 != 0 || m.uid()!=unsafe {libc::geteuid()} {return Err(Error::Storage);} }
    Ok(())
}
fn create_private_file(path: &Path) -> Result<()> {
    let mut o=std::fs::OpenOptions::new(); o.write(true).create_new(true);
    #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; o.mode(0o600); }
    match o.open(path) { Ok(f)=>f.sync_all().map_err(|_|Error::Storage)?,Err(e) if e.kind()==std::io::ErrorKind::AlreadyExists=>(),_=>return Err(Error::Storage) }
    private_file(path)
}
fn private_file(path:&Path)->Result<()> {
    regular(path)?;
    #[cfg(unix)] { use std::os::unix::fs::MetadataExt;
        let m=std::fs::symlink_metadata(path).map_err(|_|Error::Storage)?;
        // SAFETY: geteuid has no arguments or memory preconditions.
        if m.mode() & 0o077 != 0 || m.nlink()!=1 || m.uid()!=unsafe {libc::geteuid()} {return Err(Error::Storage);}
    }
    Ok(())
}
fn key_file(path: &Path) -> Result<[u8;32]> {
    let mut o=std::fs::OpenOptions::new(); o.write(true).create_new(true);
    #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; o.mode(0o600); }
    match o.open(path) {
        Ok(mut f)=>{ let key=CryptoEngine::generate_key().map_err(|_|Error::Storage)?; f.write_all(&key).and_then(|_|f.sync_all()).map_err(|_|Error::Storage)?; }
        Err(e) if e.kind()==std::io::ErrorKind::AlreadyExists=>(),_=>return Err(Error::Storage),
    }
    private_file(path)?;
    let mut f=std::fs::File::open(path).map_err(|_|Error::Storage)?;
    let mut key=[0;32]; f.read_exact(&mut key).map_err(|_|Error::Storage)?;
    if f.metadata().map_err(|_|Error::Storage)?.len()!=32 {return Err(Error::Storage);}
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    // Unit fixture only; production must supply the runtime owner's readback.
    struct RuntimeFixture;
    impl MigrationConfigReadback for RuntimeFixture {
        fn supports(&self, _: &ConfirmationItem) -> bool { true }
        fn reload_and_verify(&self, _: &ConfirmationItem, _: &str, _: &str) -> Result<()> { Ok(()) }
    }
    use duduclaw_migration::ids::RequestId;
    fn item(category:ConfirmationCategory, fields:&[(&str,&str)]) -> ConfirmationItem {
        ConfirmationItem {binding:ProposalBinding {operation_id:ProposalOperationId {proposal_id:RequestId([7;16]),revision:1},proposal_digest:Sha256Digest([8;32])},
            category,status:ConfirmationStatus::Applying,high_risk:category!=ConfirmationCategory::Department,
            fields:fields.iter().map(|(k,v)|ProposalField {name:(*k).into(),value:ProposalValue::Text {value:(*v).into()}}).collect()}
    }
    #[test]
    #[cfg(target_os="linux")]
    fn migration_authority_department_stages_then_publishes_and_compensates() {
        let home=tempfile::tempdir().unwrap();
        let mut a=GatewayAuthority::new(home.path().into()).unwrap();
        let p=item(ConfirmationCategory::Department,&[("name","sales")]);
        a.stage(&p).unwrap();
        assert!(!crate::departments::wiki_departments_root(home.path()).join("sales").exists());
        a.publish(&p).unwrap();a.publish(&p).unwrap();a.read_back(&p).unwrap();
        a.disable_and_read_back(&p).unwrap();a.disable_and_read_back(&p).unwrap();
        a.publish(&p).unwrap();a.read_back(&p).unwrap();
        let mut changed=p.clone();changed.binding.proposal_digest=Sha256Digest([9;32]);
        assert!(a.publish(&changed).is_err());
    }
    #[test]
    fn migration_authority_config_failure_does_not_overwrite_concurrent_edit() {
        let home=tempfile::tempdir().unwrap();
        let mut a=GatewayAuthority::new(home.path().into()).unwrap().with_runtime(std::sync::Arc::new(RuntimeFixture));
        let p=item(ConfirmationCategory::ConfigurationSection,&[("section","memory"),("settings","novelty_gate = true")]);
        a.stage(&p).unwrap();
        std::fs::write(home.path().join("config.toml"),"[memory]\nnovelty_gate = false\n").unwrap();
        assert!(a.publish(&p).is_err());
        assert!(a.disable_and_read_back(&p).is_err());
        assert!(std::fs::read_to_string(home.path().join("config.toml")).unwrap().contains("false"));
    }
    #[test]
    #[cfg(unix)]
    fn migration_authority_secret_rotation_reads_live_value_and_never_exports_plaintext() {
        let home=tempfile::tempdir().unwrap();
        let mut a=GatewayAuthority::new(home.path().into()).unwrap().with_runtime(std::sync::Arc::new(RuntimeFixture));
        let mut p=item(ConfirmationCategory::ConfigurationSection,&[("section","model"),("settings","api_key = '«set»'")]);
        p.fields.push(ProposalField{name:"credential".into(),value:a.fingerprint_secret("OLD-CANARY").unwrap()});
        a.stage_legacy_secret(&p,"OLD-CANARY").unwrap();
        a.stage(&p).unwrap();a.publish(&p).unwrap();a.read_back(&p).unwrap();
        p.status=ConfirmationStatus::Applied;
        assert!(a.rotate_secret(&p,"OLD-CANARY").is_err());
        let fingerprint=a.rotate_secret(&p,"NEW-CANARY").unwrap();
        a.verify_rotated(&p.binding,"credential",fingerprint).unwrap();
        assert_eq!(a.rotate_secret(&p,"NEW-CANARY").unwrap(),fingerprint);
        let bytes=std::fs::read(home.path().join("migration-authority/apply.db")).unwrap();
        assert!(!bytes.windows(10).any(|x|x==b"OLD-CANARY"||x==b"NEW-CANARY"));
        let exported=serde_json::to_string(&p).unwrap();
        assert!(!exported.contains("CANARY"));
        std::fs::write(home.path().join("config.toml"),"[model]\napi_key='OLD-CANARY'\n").unwrap();
        assert!(a.verify_rotated(&p.binding,"credential",fingerprint).is_err());
    }
    #[test]
    fn migration_authority_cached_config_cannot_claim_applied_without_runtime() {
        let home=tempfile::tempdir().unwrap();
        let mut a=GatewayAuthority::new(home.path().into()).unwrap();
        let p=item(ConfirmationCategory::ConfigurationSection,&[("section","memory"),("settings","novelty_gate = true")]);
        assert!(!GatewayAuthority::built_in_item_supported(&p));
        assert!(a.stage(&p).is_err());
        assert!(!home.path().join("config.toml").exists());
    }
    #[test]
    fn migration_authority_missing_external_backend_cannot_claim_compensation() {
        let home=tempfile::tempdir().unwrap();
        let mut a=GatewayAuthority::new(home.path().into()).unwrap();
        // v1.81 C5 made Connector a built-in category; Role still has no adapter.
        let p=item(ConfirmationCategory::Role,&[("name","planted"),("permissions","[]")]);
        assert!(!a.supports(p.category));assert!(a.validate_fresh(&p).is_err());
        assert!(a.stage(&p).is_err());assert!(a.publish(&p).is_err());
        assert!(a.read_back(&p).is_err());assert!(a.disable_and_read_back(&p).is_err());
        // A planted Connector item (not derived from this home's legacy stores,
        // wrong proposal id, no settings) is refused and creates nothing.
        let p=item(ConfirmationCategory::Connector,&[("name","planted"),("binding_kind","shared"),("owner_id","tenant")]);
        assert!(a.validate_fresh(&p).is_err());assert!(a.stage(&p).is_err());
        assert!(a.publish(&p).is_err());assert!(a.read_back(&p).is_err());
        assert!(!home.path().join(crate::isolation_connectors::CATALOG_FILE).exists());
    }
}

/// Root-authenticated import-only startup calls this before opening the listener.
/// Staging accepts bounded inert schemas; normal product validation runs again at
/// approval. Missing dependencies must not make the old authority disappear.
pub(crate) fn stage_import_proposals(home:&Path,proposals:&[duduclaw_migration::import_manifest::PendingProposal])->std::result::Result<(),duduclaw_migration::Error> {
    use duduclaw_migration::confirmation::ConfirmationStore;
    if proposals.len()>2048 {return Err(duduclaw_migration::Error::Limit);}
    let path=home.join("migration-confirmations.db");create_private_file(&path).map_err(|_|duduclaw_migration::Error::Storage)?;
    let conn=Connection::open_with_flags(&path,rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    let mut store=ConfirmationStore::from_connection(conn).map_err(|_|duduclaw_migration::Error::Storage)?;
    duduclaw_migration::import_manifest::stage_pending_proposals(&mut store,proposals)
}

impl GatewayAuthority {
    // Only after successful compensation removed our empty directory. Persist
    // the next prepared inode before publication so an approved retry is durable.
    fn replace_department_plan(&self,item:&ConfirmationItem,plan:&departments::DepartmentPlan)->Result<()> {
        let encoded=serde_json::to_vec(&(item.binding,Plan::Department(plan.clone()))).map_err(|_|Error::Invalid)?;
        let encrypted=CryptoEngine::new(&self.key).map_err(|_|Error::Storage)?.encrypt(&encoded).map_err(|_|Error::Storage)?;
        if self.db.execute("UPDATE plans SET plan=?3 WHERE operation=?1 AND digest=?2",params![operation(&item.binding),item.binding.proposal_digest.0.as_slice(),encrypted]).map_err(|_|Error::Storage)?!=1 {return Err(Error::Stale);}Ok(())
    }
}
