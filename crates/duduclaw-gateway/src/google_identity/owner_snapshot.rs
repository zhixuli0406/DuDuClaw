//! Immutable employee authority. The single durable epoch conservatively
//! versions user, credential, attestation and policy mutations together.
use std::path::Path;
use duduclaw_core::google_identity::{self as gi, Effective};
use duduclaw_core::authority_coordinator::{AuthorityCoordinator, SharedLease};
use super::Refusal;

#[derive(Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct OwnerSnapshot {
    pub owner_id: String,
    pub email: String,
    pub employee_nonce: String,
    pub employee_id: String,
    pub incarnation: u32,
    pub owner_generation: u64,
    pub attestation_revision: String,
    pub epoch: String,
    pub policy_digest: String,
    pub user_epoch: String,
    pub credential_epoch: String,
    pub policy_epoch: String,
}
pub(crate) fn changed() -> Refusal {
    Refusal::hard("identity_changed", "Google 負責人或授權已變更，請重新核准。")
}
impl OwnerSnapshot {
    /// Caller must hold the shared barrier while capturing this snapshot.
    pub fn capture(home: &Path, agent: &str) -> Result<Self, Refusal> {
        if gi::effective_for(home, Some(agent)).effective != Effective::DelegateOwner {
            return Err(changed());
        }
        let db = crate::decision_notify::open_user_db_read_only(home)
            .map_err(|_| changed())?.ok_or_else(changed)?;
        let email = super::delegate_owner::subject(home, agent, &db)?;
        let owner = super::delegate_owner::owner(&db, agent)?;
        let attestation = db.google_delegate_attestation(agent).map_err(|_| changed())?
            .ok_or_else(changed)?.attestation;
        let (id, incarnation) = super::delegate_owner::incarnation(home, agent).ok_or_else(changed)?;
        let epoch = AuthorityCoordinator::new(home).epoch().map_err(|_| changed())?;
        let config = std::fs::read(home.join("config.toml")).map_err(|_| changed())?;
        let employee = duduclaw_core::agent_toml::read_effective_text(&home.join("agents").join(agent))
            .map_err(|_| changed())?;
        Ok(Self {
            owner_id: owner.id, email,
            employee_nonce: db.google_employee_nonce(agent).map_err(|_| changed())?.ok_or_else(changed)?,
            employee_id: hex::encode(id.0), incarnation,
            owner_generation: db.google_primary_owner_generation(agent).map_err(|_| changed())?,
            // The epoch changes for every re-attestation, even in the same clock tick.
            attestation_revision: format!("{}:{}:{}", epoch, attestation.credential_seq, attestation.attested_at),
            user_epoch: epoch.clone(), credential_epoch: epoch.clone(), policy_epoch: epoch.clone(),
            epoch,
            policy_digest: gi::google_arguments_digest(&serde_json::json!([config, employee])),
        })
    }
    pub fn optional(home: &Path, agent: &str) -> Result<Option<Self>, Refusal> {
        if gi::effective_for(home, Some(agent)).effective != Effective::DelegateOwner { return Ok(None); }
        let coordinator = AuthorityCoordinator::new(home);
        coordinator.initialize().map_err(|_| changed())?;
        let _lease = coordinator.read().map_err(|_| changed())?;
        Self::capture(home, agent).map(Some)
    }
    pub fn digest(&self) -> String {
        gi::google_arguments_digest(&serde_json::json!(self))
    }
    pub fn validate(&self, home: &Path, agent: &str) -> Result<(), Refusal> {
        if &Self::capture(home, agent)? != self { return Err(changed()); }
        Ok(())
    }
    pub async fn lease(&self, home: &Path, agent: &str) -> Result<SharedLease, Refusal> {
        let lease = AuthorityCoordinator::new(home).shared().await.map_err(|_| changed())?;
        self.validate(home, agent)?;
        Ok(lease)
    }
}
