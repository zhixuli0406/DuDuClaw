//! Resolve the primary owner's attested login address, never tool parameters.
use std::path::Path;
use duduclaw_auth::{UserDb, UserStatus};
use duduclaw_core::google_identity::{self as gi, Effective};
use super::{Refusal, tenant_source};

fn refused() -> Refusal {
    Refusal::hard("attestation_required", "無法確認目前負責人的 Google 信箱。請管理員到「整合 → Google → 公司授權代表」確認目前負責人與信箱後重新認證。")
}

/// Protected employees also retain the native lifecycle check. Ordinary
/// employees use the random nonce stored outside their directory in users.db.
pub(crate) fn incarnation(home: &Path, agent: &str) -> Option<tenant_source::Incarnation> {
    if crate::runtime_readiness::is_protected_linux() {
        return tenant_source::current_incarnation(home, agent);
    }
    let db = crate::decision_notify::open_user_db_read_only(home).ok()??;
    let nonce = db.google_employee_nonce(agent).ok()??;
    let id = uuid::Uuid::parse_str(&nonce).ok()?;
    if id.get_version_num() != 4 { return None; }
    Some((duduclaw_isolation::ids::EmployeeId(*id.as_bytes()), 0))
}

pub(crate) fn owner(db: &UserDb, agent: &str) -> Result<duduclaw_auth::User, Refusal> {
    let user = db.google_owner(agent).map_err(|_| refused())?;
    if user.status != UserStatus::Active { return Err(refused()); }
    gi::normalize_email(&user.email).map_err(|_| refused())?;
    Ok(user)
}

pub(crate) fn subject(home: &Path, agent: &str, db: &UserDb) -> Result<String, Refusal> {
    let path = home.join("agents").join(agent);
    if !std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink()) {
        return Err(refused());
    }
    let nonce = db.google_employee_nonce(agent).map_err(|_| refused())?.ok_or_else(refused)?;
    let generation = db.google_primary_owner_generation(agent).map_err(|_| refused())?;
    let owner = owner(db, agent)?;
    let email = gi::normalize_email(&owner.email).map_err(|_| refused())?;
    let attestation = db.google_delegate_attestation(agent).map_err(|_| refused())?.ok_or_else(refused)?;
    if attestation.attestation.employee_nonce.as_deref() != Some(nonce.as_str()) { return Err(refused()); }
    if gi::normalize_email(&attestation.attestation.email_at_attestation).ok().as_deref() != Some(email.as_str()) {
        return Err(refused());
    }
    let accounts = tenant_source::subject_accounts(db, agent, incarnation(home, agent), &email)
        .map_err(|_| refused())?;
    if !matches!(accounts.as_slice(), [account] if account.active && account.attested && account.user_id == owner.id) {
        return Err(refused());
    }
    let current = self::owner(db, agent)?;
    if current.id != owner.id || current.email != owner.email
        || db.google_primary_owner_generation(agent).map_err(|_| refused())? != generation {
        return Err(refused());
    }
    Ok(email)
}

/// Gateway-only expansion. All preflight and final-effect rechecks use this
/// function so a stale attestation can never select a fallback identity.
pub(crate) fn effective_for(home: &Path, agent: Option<&str>) -> gi::EffectiveIdentity {
    let mut identity = gi::effective_for(home, agent);
    if identity.effective == Effective::DelegateOwner {
        let selected = (|| {
            let agent = agent.ok_or_else(refused)?;
            let db = crate::decision_notify::open_user_db_read_only(home).map_err(|_| refused())?
                .ok_or_else(refused)?;
            subject(home, agent, &db)
        })();
        identity.effective = match selected {
            Ok(email) => Effective::Delegate { email },
            Err(error) => Effective::Invalid(error.message_zh),
        };
        identity.fallback = gi::Fallback::None;
    }
    identity
}

pub(crate) fn notice(home: &Path, agent: &str, db: Option<&UserDb>, include_holder: bool) -> serde_json::Value {
    let Some(db) = db else { return serde_json::json!({"state":"unavailable", "message":refused().message_zh}); };
    let Ok(owner) = owner(db, agent) else { return serde_json::json!({"state":"person_inactive", "message":refused().message_zh}); };
    let email = gi::normalize_email(&owner.email).unwrap_or_default();
    let mut value = serde_json::json!({"state": if subject(home, agent, db).is_ok() {"attested_person"} else {"attestation_required"},
        "subject": email, "kind":"delegate", "message":refused().message_zh});
    if include_holder {
        if let Ok(Some(holder)) = db.google_delegate_holder(&owner.id) {
            value["holder"] = serde_json::json!({"user_id":owner.id,"name":owner.display_name,
                "created_at":holder.created_at,"email_self_set":holder.email_self_set});
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_auth::{AccessLevel, UserRole};

    #[test]
    fn token_requests_cannot_select_a_delegation_subject() {
        for key in ["subject", "email", "user_id", "owner"] {
            let mut request = serde_json::json!({"tool":"gmail_read"});
            request[key] = serde_json::json!("attacker@c.test");
            assert!(serde_json::from_value::<super::super::route::TokenRequest>(request).is_err());
        }
    }

    #[test]
    fn current_owner_requires_attestation_and_changes_retire_it() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("agents/bot")).unwrap();
        std::fs::write(home.path().join("agents/bot/agent.toml"), "[google]\nidentity='delegate:owner'").unwrap();
        let db = UserDb::new(&home.path().join("users.db")).unwrap();
        let admin = db.create_user("admin@c.test", "Admin", "test-password", UserRole::Admin).unwrap();
        let alice = db.create_user("alice@c.test", "Alice", "test-password", UserRole::Employee).unwrap();
        let bob = db.create_user("bob@c.test", "Bob", "test-password", UserRole::Employee).unwrap();
        db.bind_agent(&alice.id, "bot", AccessLevel::Owner).unwrap();
        db.bind_agent(&bob.id, "bot", AccessLevel::Owner).unwrap();
        db.set_google_primary_owner("bot", &alice.id, &admin.id).unwrap();
        db.ensure_google_employee_nonce("bot").unwrap();
        let attest = || {
            let employee = incarnation(home.path(), "bot").unwrap();
            db.attest_google_delegate_owner(&duduclaw_auth::google_delegate_attestation::AttestationRequest {
                agent_id: "bot", employee_id: &hex::encode(employee.0.0), employee_incarnation: employee.1,
                subject: "alice@c.test", user_id: &alice.id, acknowledge_self_set: true,
            }, &|email| email == "alice@c.test", &admin.id).unwrap();
        };
        assert_eq!(subject(home.path(), "bot", &db).unwrap_err().code, "attestation_required");
        attest();
        let audit = db.query_audit_log(None, Some(duduclaw_auth::google_delegate_attestation::ATTESTED_ACTION), 10).unwrap();
        assert_eq!(audit.len(), 1);
        assert_eq!(audit[0].user_id.as_deref(), Some(admin.id.as_str()));
        assert!(audit[0].detail.as_deref().unwrap_or("").contains(&alice.id));
        assert!(audit[0].detail.as_deref().unwrap_or("").contains("alice@c.test"));
        assert_eq!(subject(home.path(), "bot", &db).unwrap(), "alice@c.test");
        db.set_google_primary_owner("bot", &bob.id, &admin.id).unwrap();
        assert!(subject(home.path(), "bot", &db).is_err());
        db.set_google_primary_owner("bot", &alice.id, &admin.id).unwrap();
        assert!(subject(home.path(), "bot", &db).is_err(), "A -> B -> A needs a new attestation");
        attest();
        db.change_own_email(&alice.id, "changed@c.test").unwrap();
        assert!(subject(home.path(), "bot", &db).is_err());
        db.change_own_email(&alice.id, "alice@c.test").unwrap();
        assert!(subject(home.path(), "bot", &db).is_err());
        attest();
        db.set_user_status(&alice.id, UserStatus::Suspended).unwrap();
        assert!(subject(home.path(), "bot", &db).is_err());
        // No operation accepts a subject parameter; only users.db supplies it.
        assert!(matches!(effective_for(home.path(), Some("bot")).effective, Effective::Invalid(_)));
    }
    #[test]
    fn nonce_refuses_missing_recreated_and_restored_directory() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("agents/bot");
        std::fs::create_dir_all(&path).unwrap();
        let db = UserDb::new(&home.path().join("users.db")).unwrap();
        let owner = db.create_user("nonce@c.test", "Owner", "test-password", UserRole::Employee).unwrap();
        db.bind_agent(&owner.id, "bot", AccessLevel::Owner).unwrap();
        assert!(incarnation(home.path(), "bot").is_none());
        assert!(subject(home.path(), "bot", &db).is_err(), "missing nonce refuses");
        db.ensure_google_employee_nonce("bot").unwrap();
        let attest = || {
            let employee = incarnation(home.path(), "bot").unwrap();
            db.attest_google_delegate_owner(&duduclaw_auth::google_delegate_attestation::AttestationRequest {
                agent_id:"bot", employee_id:&hex::encode(employee.0.0), employee_incarnation:employee.1,
                subject:"nonce@c.test", user_id:&owner.id, acknowledge_self_set:true,
            }, &|email| email == "nonce@c.test", "admin").unwrap();
        };
        attest();
        let nonce = db.google_employee_nonce("bot").unwrap().unwrap();
        assert_eq!(uuid::Uuid::parse_str(&nonce).unwrap().get_version_num(), 4);
        assert!(subject(home.path(), "bot", &db).is_ok());
        // Moving the same directory away and back preserves its birth time.
        let saved = home.path().join("saved-bot");
        let birth = std::fs::metadata(&path).unwrap().created().ok();
        std::fs::rename(&path, &saved).unwrap();
        duduclaw_core::employee_incarnation::reconcile(home.path()).unwrap();
        std::fs::rename(&saved, &path).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().created().ok(), birth);
        assert!(db.google_employee_nonce("bot").unwrap().is_none());
        assert!(subject(home.path(), "bot", &db).is_err(), "old birth time never restores authority");
        std::fs::remove_dir(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        duduclaw_core::employee_incarnation::created(home.path(), "bot").unwrap();
        assert_ne!(db.google_employee_nonce("bot").unwrap().as_deref(), Some(nonce.as_str()));
        assert!(subject(home.path(), "bot", &db).is_err(), "recreation rejects the retained attestation");
        attest();
        assert!(subject(home.path(), "bot", &db).is_ok());
        duduclaw_core::employee_incarnation::deleted(home.path(), "bot").unwrap();
        assert!(subject(home.path(), "bot", &db).is_err(), "soft deletion invalidates with directory intact");
    }

}
