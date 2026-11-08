//! Durable lifetime employee bindings (§6.1.1). This store accepts only trusted
//! gateway administration, never MCP arguments. Callers must revoke running
//! sessions/leases when changing principal generations or tombstoning employees;
//! every new authorization also re-reads these tables without a cache.
use duduclaw_isolation::capability::{EmployeeBinding, Principal};
use duduclaw_isolation::ids::EmployeeId;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BindingError {
    #[error("binding rejected")] Denied,
    #[error("invalid binding value")] Invalid,
    #[error("binding store unavailable")] Unavailable,
}

type Result<T> = std::result::Result<T, BindingError>;

/// Fresh binding checks supplied by the host's authoritative registries. A
/// product adapter can reuse its existing stores without copying authority into
/// the standalone BindingStore. Unreadable or unknown state must reject.
///
/// `generation` is the account route generation the caller's selection pinned
/// (v1.81 C2). A host whose account store has generations must refuse when the
/// current route generation differs; the standalone [`BindingStore`] test
/// double has none and leaves that comparison to its caller.
pub trait BindingAuthority: Send + Sync {
    fn validate(&self, employee: EmployeeId, binding: &EmployeeBinding,
        principal: &Principal) -> Result<()>;
    fn authorize_connector(&self, employee: EmployeeId, binding: &EmployeeBinding,
        principal: &Principal, account: &str, generation: u64) -> Result<()>;
    fn authorize_product_home_connector(&self, _employee: EmployeeId, _binding: &EmployeeBinding,
        _principal: &Principal, _account: &str, _generation: u64, _home_binding: &EmployeeBinding) -> Result<()> {
        Err(BindingError::Denied)
    }
}

/// Own this connection in the gateway's protected data directory. SQLite
/// constraints defend normal write paths, not a compromised gateway/DB owner.
pub struct BindingStore { connection: Connection }

/// Compose A's live run/policy registry with D's durable binding constraints.
/// The inner authority still owns run/control/policy consistency; this wrapper
/// adds a fresh principal/member/tombstone check on every boundary verification.
pub struct BindingCheckedAuthority {
    pub run_policy: std::sync::Arc<dyn crate::LiveAuthority>,
    pub bindings: std::sync::Arc<dyn BindingAuthority>,
}

impl crate::LiveAuthority for BindingCheckedAuthority {
    fn snapshot(&self, employee: EmployeeId, run: duduclaw_isolation::execution::RunHandle)
        -> std::result::Result<crate::LiveSnapshot, crate::VerificationError> {
        let snapshot = self.run_policy.snapshot(employee, run)?;
        if snapshot.employee != employee || snapshot.run != run {
            return Err(crate::VerificationError::BindingMismatch);
        }
        self.bindings.validate(employee, &snapshot.binding, &snapshot.principal)
            .map_err(|_| crate::VerificationError::Revoked)?;
        Ok(snapshot)
    }
}

impl BindingAuthority for std::sync::Mutex<BindingStore> {
    fn validate(&self, employee: EmployeeId, binding: &EmployeeBinding,
        principal: &Principal) -> Result<()> {
        self.lock().map_err(|_| BindingError::Unavailable)?.validate(employee, binding, principal)
    }
    // Test double: no route generations here; the catalog caller compares them.
    fn authorize_connector(&self, employee: EmployeeId, binding: &EmployeeBinding,
        principal: &Principal, account: &str, _generation: u64) -> Result<()> {
        self.lock().map_err(|_| BindingError::Unavailable)?
            .authorize_connector(employee, binding, principal, account)
    }
    fn authorize_product_home_connector(&self, employee: EmployeeId, binding: &EmployeeBinding,
        principal: &Principal, account: &str, _generation: u64, home_binding: &EmployeeBinding) -> Result<()> {
        self.lock().map_err(|_| BindingError::Unavailable)?
            .authorize_product_home_connector(employee, binding, principal, account, home_binding)
    }
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS iso_principals (
    kind TEXT NOT NULL CHECK(kind IN ('human','background')),
    id TEXT NOT NULL CHECK(length(CAST(id AS BLOB)) BETWEEN 1 AND 128),
    generation INTEGER NOT NULL CHECK(generation >= 0),
    active INTEGER NOT NULL CHECK(active IN (0,1)),
    PRIMARY KEY(kind,id)
) STRICT;
CREATE TRIGGER IF NOT EXISTS iso_principals_no_replace BEFORE INSERT ON iso_principals
WHEN EXISTS(SELECT 1 FROM iso_principals WHERE kind=NEW.kind AND id=NEW.id)
BEGIN SELECT RAISE(ABORT,'immutable principal'); END;
CREATE TRIGGER IF NOT EXISTS iso_principals_no_delete BEFORE DELETE ON iso_principals
BEGIN SELECT RAISE(ABORT,'immutable principal'); END;
CREATE TRIGGER IF NOT EXISTS iso_principals_monotonic BEFORE UPDATE ON iso_principals
WHEN NEW.kind != OLD.kind OR NEW.id != OLD.id OR NEW.generation < OLD.generation
 OR (OLD.active=0 AND NEW.active=1 AND NEW.generation <= OLD.generation)
BEGIN SELECT RAISE(ABORT,'immutable principal'); END;
CREATE TABLE IF NOT EXISTS iso_employee_bindings (
    employee BLOB PRIMARY KEY NOT NULL CHECK(length(employee)=16),
    kind TEXT NOT NULL CHECK(kind IN ('personal','shared')),
    binding_id TEXT NOT NULL CHECK(length(CAST(binding_id AS BLOB)) BETWEEN 1 AND 128),
    tombstone INTEGER NOT NULL DEFAULT 0 CHECK(tombstone IN (0,1))
) STRICT;
CREATE TRIGGER IF NOT EXISTS iso_employee_no_replace BEFORE INSERT ON iso_employee_bindings
WHEN EXISTS(SELECT 1 FROM iso_employee_bindings WHERE employee=NEW.employee)
BEGIN SELECT RAISE(ABORT,'immutable employee'); END;
CREATE TRIGGER IF NOT EXISTS iso_employee_no_delete BEFORE DELETE ON iso_employee_bindings
BEGIN SELECT RAISE(ABORT,'immutable employee'); END;
CREATE TRIGGER IF NOT EXISTS iso_employee_immutable BEFORE UPDATE ON iso_employee_bindings
WHEN NEW.employee != OLD.employee OR NEW.kind != OLD.kind OR NEW.binding_id != OLD.binding_id
 OR NEW.tombstone < OLD.tombstone
BEGIN SELECT RAISE(ABORT,'immutable employee'); END;
CREATE TABLE IF NOT EXISTS iso_employee_members (
    employee BLOB NOT NULL REFERENCES iso_employee_bindings(employee),
    user_id TEXT NOT NULL CHECK(length(CAST(user_id AS BLOB)) BETWEEN 1 AND 128),
    role TEXT NOT NULL CHECK(role IN ('owner','operator','viewer')),
    PRIMARY KEY(employee,user_id)
) STRICT;
CREATE TRIGGER IF NOT EXISTS iso_member_insert BEFORE INSERT ON iso_employee_members
WHEN NOT EXISTS(SELECT 1 FROM iso_employee_bindings e WHERE e.employee=NEW.employee
 AND e.tombstone=0 AND (e.kind='shared' OR (e.binding_id=NEW.user_id AND NEW.role='owner')))
 OR NOT EXISTS(SELECT 1 FROM iso_principals p WHERE p.kind='human' AND p.id=NEW.user_id AND p.active=1)
 OR EXISTS(SELECT 1 FROM iso_employee_members m WHERE m.employee=NEW.employee AND m.user_id=NEW.user_id)
BEGIN SELECT RAISE(ABORT,'member rejected'); END;
CREATE TRIGGER IF NOT EXISTS iso_member_update BEFORE UPDATE ON iso_employee_members
BEGIN SELECT RAISE(ABORT,'immutable member'); END;
CREATE TRIGGER IF NOT EXISTS iso_member_personal_delete BEFORE DELETE ON iso_employee_members
WHEN EXISTS(SELECT 1 FROM iso_employee_bindings e WHERE e.employee=OLD.employee AND e.kind='personal')
BEGIN SELECT RAISE(ABORT,'immutable personal member'); END;
CREATE TRIGGER IF NOT EXISTS iso_member_revoke_generation AFTER DELETE ON iso_employee_members
BEGIN UPDATE iso_principals SET generation=generation+1 WHERE kind='human' AND id=OLD.user_id; END;
CREATE TABLE IF NOT EXISTS iso_connectors (
    account TEXT PRIMARY KEY NOT NULL CHECK(length(CAST(account AS BLOB)) BETWEEN 1 AND 128),
    kind TEXT NOT NULL CHECK(kind IN ('personal','shared')),
    binding_id TEXT NOT NULL CHECK(length(CAST(binding_id AS BLOB)) BETWEEN 1 AND 128),
    revoked INTEGER NOT NULL DEFAULT 0 CHECK(revoked IN (0,1))
) STRICT;
CREATE TRIGGER IF NOT EXISTS iso_connector_no_replace BEFORE INSERT ON iso_connectors
WHEN EXISTS(SELECT 1 FROM iso_connectors WHERE account=NEW.account)
BEGIN SELECT RAISE(ABORT,'immutable connector'); END;
CREATE TRIGGER IF NOT EXISTS iso_connector_no_delete BEFORE DELETE ON iso_connectors
BEGIN SELECT RAISE(ABORT,'immutable connector'); END;
CREATE TRIGGER IF NOT EXISTS iso_connector_immutable BEFORE UPDATE ON iso_connectors
WHEN NEW.account != OLD.account OR NEW.kind != OLD.kind OR NEW.binding_id != OLD.binding_id
 OR NEW.revoked < OLD.revoked
BEGIN SELECT RAISE(ABORT,'immutable connector'); END;
"#;

fn identifier(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        return Err(BindingError::Invalid);
    }
    Ok(())
}

fn binding_parts(binding: &EmployeeBinding) -> Result<(&'static str, &str)> {
    let (kind, id) = match binding {
        EmployeeBinding::Personal(owner) => ("personal", owner.as_str()),
        EmployeeBinding::Shared(tenant) => ("shared", tenant.as_str()),
    };
    identifier(id)?;
    Ok((kind, id))
}

fn principal_parts(principal: &Principal) -> Result<(&'static str, &str, i64)> {
    let (kind, id, generation) = match principal {
        Principal::Human { user_id, generation } => ("human", user_id.as_str(), *generation),
        Principal::Background { tenant_id, generation } => ("background", tenant_id.as_str(), *generation),
    };
    identifier(id)?;
    Ok((kind, id, generation.try_into().map_err(|_| BindingError::Invalid)?))
}

impl BindingStore {
    /// Initialize on a gateway-owned connection. No legacy bindings are imported.
    pub fn new(mut connection: Connection) -> Result<Self> {
        connection.pragma_update(None, "foreign_keys", true).map_err(|_| BindingError::Unavailable)?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| BindingError::Unavailable)?;
        tx.execute_batch(SCHEMA).map_err(|_| BindingError::Unavailable)?;
        tx.commit().map_err(|_| BindingError::Unavailable)?;
        Ok(Self { connection })
    }

    pub fn register_principal(&self, principal: &Principal) -> Result<()> {
        let (kind, id, generation) = principal_parts(principal)?;
        self.connection.execute("INSERT INTO iso_principals VALUES (?1,?2,?3,1)",
            params![kind, id, generation]).map_err(|_| BindingError::Denied)?;
        Ok(())
    }

    /// Increasing the generation invalidates every earlier capability. The caller
    /// must also cancel live runs and credential leases after this durable write.
    pub fn advance_principal(&self, principal: &Principal) -> Result<()> {
        let (kind, id, generation) = principal_parts(principal)?;
        let count = self.connection.execute(
            "UPDATE iso_principals SET generation=?3,active=1 WHERE kind=?1 AND id=?2 AND generation < ?3",
            params![kind, id, generation]).map_err(|_| BindingError::Unavailable)?;
        if count != 1 { return Err(BindingError::Denied); }
        Ok(())
    }

    pub fn revoke_principal(&self, principal: &Principal) -> Result<()> {
        let (kind, id, generation) = principal_parts(principal)?;
        let count = self.connection.execute(
            "UPDATE iso_principals SET active=0 WHERE kind=?1 AND id=?2 AND generation=?3",
            params![kind, id, generation]).map_err(|_| BindingError::Unavailable)?;
        if count != 1 { return Err(BindingError::Denied); }
        Ok(())
    }

    pub fn create_employee(&mut self, employee: EmployeeId, binding: &EmployeeBinding) -> Result<()> {
        let (kind, id) = binding_parts(binding)?;
        let tx = self.connection.transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| BindingError::Unavailable)?;
        let principal_kind = if kind == "personal" { "human" } else { "background" };
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM iso_principals WHERE kind=?1 AND id=?2 AND active=1)",
            params![principal_kind, id], |row| row.get(0)).map_err(|_| BindingError::Unavailable)?;
        if !exists { return Err(BindingError::Denied); }
        tx.execute("INSERT INTO iso_employee_bindings VALUES (?1,?2,?3,0)",
            params![employee.0.as_slice(), kind, id]).map_err(|_| BindingError::Denied)?;
        if kind == "personal" {
            tx.execute("INSERT INTO iso_employee_members VALUES (?1,?2,'owner')",
                params![employee.0.as_slice(), id]).map_err(|_| BindingError::Denied)?;
        }
        tx.commit().map_err(|_| BindingError::Unavailable)
    }

    pub fn tombstone_employee(&self, employee: EmployeeId) -> Result<()> {
        let count = self.connection.execute(
            "UPDATE iso_employee_bindings SET tombstone=1 WHERE employee=?1",
            params![employee.0.as_slice()]).map_err(|_| BindingError::Unavailable)?;
        if count != 1 { return Err(BindingError::Denied); }
        Ok(())
    }

    pub fn add_shared_member(&self, employee: EmployeeId, user: &str, role: &str) -> Result<()> {
        identifier(user)?;
        if !matches!(role, "owner" | "operator" | "viewer") { return Err(BindingError::Invalid); }
        let count = self.connection.execute(
            "INSERT INTO iso_employee_members SELECT employee,?2,?3 FROM iso_employee_bindings
             WHERE employee=?1 AND kind='shared' AND tombstone=0",
            params![employee.0.as_slice(), user, role]).map_err(|_| BindingError::Denied)?;
        if count != 1 { return Err(BindingError::Denied); }
        Ok(())
    }

    /// Removal atomically advances the user's generation through a DB trigger,
    /// so re-adding a shared member never revives an earlier capability.
    pub fn remove_shared_member(&self, employee: EmployeeId, user: &str) -> Result<()> {
        identifier(user)?;
        let count = self.connection.execute(
            "DELETE FROM iso_employee_members WHERE employee=?1 AND user_id=?2
             AND EXISTS(SELECT 1 FROM iso_employee_bindings e WHERE e.employee=?1 AND e.kind='shared')",
            params![employee.0.as_slice(), user]).map_err(|_| BindingError::Unavailable)?;
        if count != 1 { return Err(BindingError::Denied); }
        Ok(())
    }

    /// Checks a single SQLite statement snapshot, including current membership,
    /// generation, tombstone, and lifetime binding. Unknown/unreadable is denied.
    pub fn validate(&self, employee: EmployeeId, binding: &EmployeeBinding, principal: &Principal) -> Result<()> {
        let (kind, binding_id) = binding_parts(binding)?;
        let (principal_kind, principal_id, generation) = principal_parts(principal)?;
        let valid: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM iso_employee_bindings e, iso_principals p
             WHERE e.employee=?1 AND e.kind=?2 AND e.binding_id=?3 AND e.tombstone=0
             AND p.kind=?4 AND p.id=?5 AND p.generation=?6 AND p.active=1
             AND ((e.kind='personal' AND p.kind='human' AND e.binding_id=p.id
                  AND EXISTS(SELECT 1 FROM iso_employee_members m WHERE m.employee=e.employee AND m.user_id=p.id AND m.role='owner')
                  AND (SELECT count(*) FROM iso_employee_members m WHERE m.employee=e.employee)=1)
               OR (e.kind='shared' AND ((p.kind='background' AND p.id=e.binding_id)
                  OR (p.kind='human' AND EXISTS(SELECT 1 FROM iso_employee_members m WHERE m.employee=e.employee AND m.user_id=p.id))))))",
            params![employee.0.as_slice(), kind, binding_id, principal_kind, principal_id, generation],
            |row| row.get(0)).map_err(|_| BindingError::Unavailable)?;
        if !valid { return Err(BindingError::Denied); }
        Ok(())
    }

    pub fn register_connector(&self, account: &str, binding: &EmployeeBinding) -> Result<()> {
        identifier(account)?;
        let (kind, id) = binding_parts(binding)?;
        self.connection.execute("INSERT INTO iso_connectors VALUES (?1,?2,?3,0)",
            params![account, kind, id]).map_err(|_| BindingError::Denied)?;
        Ok(())
    }

    pub fn revoke_connector(&self, account: &str) -> Result<()> {
        identifier(account)?;
        let count = self.connection.execute("UPDATE iso_connectors SET revoked=1 WHERE account=?1",
            params![account]).map_err(|_| BindingError::Unavailable)?;
        if count != 1 { return Err(BindingError::Denied); }
        Ok(())
    }

    /// A trusted live product-selection adapter has explicitly selected the
    /// installed shared home account. This does not select or discover accounts.
    /// Personal credentials remain exclusive to their owner's Personal employee;
    /// Shared employees cannot cross tenants. Callers must repeat product policy
    /// checks immediately before invoking this method and every credential use.
    pub fn authorize_product_home_connector(&self, employee: EmployeeId, binding: &EmployeeBinding,
        principal: &Principal, account: &str, home_binding: &EmployeeBinding) -> Result<()> {
        identifier(account)?;
        let EmployeeBinding::Shared(home_tenant) = home_binding else { return Err(BindingError::Denied); };
        if matches!(binding, EmployeeBinding::Shared(tenant) if tenant != home_tenant) {
            return Err(BindingError::Denied);
        }
        let tx = self.connection.unchecked_transaction().map_err(|_| BindingError::Unavailable)?;
        self.validate(employee, binding, principal)?;
        let found = tx.query_row(
            "SELECT 1 FROM iso_connectors WHERE account=?1 AND kind='shared' AND binding_id=?2 AND revoked=0",
            params![account, home_tenant.as_str()], |_| Ok(())).optional().map_err(|_| BindingError::Unavailable)?;
        if found.is_none() { return Err(BindingError::Denied); }
        tx.commit().map_err(|_| BindingError::Unavailable)
    }

    /// Caller still needs a matching ResourceGrant::Connector and existing tool
    /// policy gates. This check never hands out credential material by itself.
    pub fn authorize_connector(&self, employee: EmployeeId, binding: &EmployeeBinding,
        principal: &Principal, account: &str) -> Result<()> {
        identifier(account)?;
        // A read transaction keeps the identity and credential checks on the same
        // snapshot even if another gateway connection revokes between statements.
        let tx = self.connection.unchecked_transaction().map_err(|_| BindingError::Unavailable)?;
        self.validate(employee, binding, principal)?;
        let (kind, id) = binding_parts(binding)?;
        let found = tx.query_row(
            "SELECT 1 FROM iso_connectors WHERE account=?1 AND kind=?2 AND binding_id=?3 AND revoked=0",
            params![account, kind, id], |_| Ok(())).optional().map_err(|_| BindingError::Unavailable)?;
        if found.is_none() { return Err(BindingError::Denied); }
        tx.commit().map_err(|_| BindingError::Unavailable)
    }
}

#[cfg(test)]
#[path = "bindings_tests.rs"]
mod tests;
