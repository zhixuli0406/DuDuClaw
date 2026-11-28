//! Authority is explicit; a person never becomes a synthetic employee.
use std::{borrow::Cow, path::Path, sync::Arc};
use serde_json::Value;
use super::{OdooState, odoo_access::ToolPolicy, odoo_identity_client::{self, OdooIdentity}};

// Do not derive Debug: the bearer must never be formatted.
#[derive(Clone, Copy)]
pub(super) enum OdooActor<'a> {
    Employee(&'a str),
    /// Both fields come from the gateway-authenticated call, never arguments.
    Person { user_id: &'a str, raw_key: &'a str },
}

impl<'a> OdooActor<'a> {
    pub fn employee(self) -> Option<&'a str> {
        match self { Self::Employee(agent) => Some(agent), Self::Person { .. } => None }
    }

    pub fn audit_name(self) -> Cow<'a, str> {
        match self {
            Self::Employee(agent) => Cow::Borrowed(agent),
            Self::Person { user_id, .. } => Cow::Owned(format!("person:{user_id}")),
        }
    }

    pub fn policy(self, home: &Path) -> Result<Box<ToolPolicy>, String> {
        match self {
            Self::Employee(agent) => ToolPolicy::load(home, agent),
            Self::Person { .. } => ToolPolicy::load_person(home),
        }.map(Box::new)
    }

    pub async fn profile(self, pool: &OdooState, decision: Option<&OdooIdentity>) -> Result<String, String> {
        match self {
            Self::Employee(agent) => match decision {
                Some(identity) => pool.key_for_identity(agent, identity).await.map(|key| key.1),
                None => Ok(pool.pool_key(agent).await.1),
            },
            Self::Person { user_id, .. } => {
                let identity = decision.ok_or("Missing personal identity")?;
                if identity.user_id.as_deref() != Some(user_id) { return Err("Personal identity changed".into()); }
                identity.personal_profile()
            }
        }
    }

    pub async fn connect(self, home: &Path, pool: &OdooState, tool: &str, params: &Value)
        -> Result<(Arc<duduclaw_odoo::OdooConnector>, OdooIdentity), String> {
        match self {
            Self::Employee(agent) => super::odoo::connect_odoo_pool_checked(home, pool, agent, tool, params).await,
            Self::Person { user_id, raw_key } => {
                // Reauthorize on every call, including cached connectors. Request
                // the current secret directly; do not use an employee cache entry.
                let _guard = pool.identity_cache.lock().await;
                let identity = match odoo_identity_client::request_identity_for_person_key(home, raw_key, tool, None).await {
                    Ok(identity) if identity.user_id.as_deref() == Some(user_id) => identity,
                    result => {
                        pool.forget_person_key(user_id).await;
                        return Err(match result {
                            Err(error) => error.message,
                            Ok(_) => "Odoo 本人身分已變更，這次操作尚未執行。".into(),
                        });
                    }
                };
                odoo_identity_client::record(&identity);
                let policy = Box::new(ToolPolicy::load_person(home)?);
                policy.check_call(tool, params, Some(&identity))?;
                let connector = pool.connect_person_key(policy.global, &identity).await?;
                Ok((connector, identity))
            }
        }
    }
}
