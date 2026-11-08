//! Durable approval and idempotent application (design §10.3, R7-M3).
//! Product adapters remain mandatory: this module never installs authority itself.
use duduclaw_isolation::{ids::{RequestId, Sha256Digest}, migration::*};
use hmac::{Hmac, Mac};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum ConfirmationError {
    #[error("confirmation denied")] Denied,
    #[error("proposal changed or unavailable")] Stale,
    #[error("invalid proposal")] Invalid,
    #[error("proposal limit exceeded")] Limit,
    #[error("confirmation storage unavailable")] Storage,
    #[error("proposal application failed")] Apply,
    #[error("proposal compensation incomplete")] Compensation,
    /// F6 §3.12 (R14): an `EmployeeRetire` item whose root operation already
    /// started cannot be rejected; it stays where it was and `retry`
    /// resumes the same retire operation (UI: `RETIRE_FORWARD_ONLY`).
    #[error("retire is forward only")] ForwardOnly,
}
type Result<T> = std::result::Result<T, ConfirmationError>;
const MAX_CONFIRMATIONS: u64 = 2048;
const MAX_CONFIRMATION_STORE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_CONFIRMATION_BYTES: usize = 512 * 1024;
impl From<rusqlite::Error> for ConfirmationError {
    fn from(_: rusqlite::Error) -> Self { Self::Storage }
}
impl From<serde_json::Error> for ConfirmationError {
    fn from(_: serde_json::Error) -> Self { Self::Invalid }
}

/// Construct only after authenticating the live dashboard session. Never deserialize
/// this from an RPC. CSRF belongs to that session; actor is an opaque audit identity.
pub struct ApprovalSession {
    admin: bool,
    mfa_enrolled: bool,
    step_up_at: Option<u64>,
    csrf: [u8; 32],
    actor: RequestId,
}
impl ApprovalSession {
    pub fn authorize(&self, csrf: &[u8; 32], now: u64) -> Result<()> { self.check(csrf, now) }
    pub fn from_verified_session(admin: bool, mfa_enrolled: bool,
        step_up_at: Option<u64>, csrf: [u8; 32], actor: RequestId) -> Self {
        Self { admin, mfa_enrolled, step_up_at, csrf, actor }
    }
    fn check(&self, csrf: &[u8; 32], now: u64) -> Result<()> {
        let mismatch = self.csrf.iter().zip(csrf).fold(0u8, |a, (b, c)| a | (b ^ c));
        let fresh = self.step_up_at.is_some_and(|at| at <= now
            && now - at <= CONFIRMATION_STEP_UP_SECONDS);
        if !self.admin || !self.mfa_enrolled || !fresh || mismatch != 0 {
            return Err(ConfirmationError::Denied);
        }
        Ok(())
    }
}

/// Uses the same validators as a fresh product write. It must check the current
/// Personal owner login/binding, capacity, default permission ceilings and secrets.
/// No implementation is supplied: missing product integration must refuse.
pub trait ProposalValidator {
    fn validate_fresh(&self, item: &ConfirmationItem) -> Result<()>;
}

/// Every method MUST bind operation_id AND digest and be idempotent across crashes.
/// stage validates via normal product APIs without enabling anything; publish uses
/// atomic replacement; read_back verifies effective state. disable must revoke all
/// partial effects (including UID tombstones) and verify their disabled state.
/// Calls run after a durable intent commits, without a SQLite writer transaction.
/// Adapters must enforce bounded RPC timeouts. A per-proposal process lock keeps
/// apply and compensation mutually exclusive; receipt commits use an intent CAS.
pub trait ProposalApplier {
    fn stage(&mut self, item: &ConfirmationItem) -> Result<()>;
    fn publish(&mut self, item: &ConfirmationItem) -> Result<()>;
    fn read_back(&mut self, item: &ConfirmationItem) -> Result<()>;
    fn disable_and_read_back(&mut self, item: &ConfirmationItem) -> Result<()>;
}

/// HMAC prevents low-entropy credentials being recovered from proposal digests.
/// Secret bytes never enter this module's database, Debug, audit or response schema.
pub fn secret_fingerprint(key: &[u8; 32], field: &str, secret: &[u8],
    needs_rotation: bool) -> Result<ProposalValue> {
    if field.is_empty() || field.len() > 128 || secret.len() > MAX_PROPOSAL_VALUE_BYTES {
        return Err(ConfirmationError::Invalid);
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|_| ConfirmationError::Invalid)?;
    mac.update(b"duduclaw-migration-secret-v1\0");
    mac.update(&(field.len() as u32).to_be_bytes());
    mac.update(field.as_bytes());
    mac.update(secret);
    Ok(ProposalValue::SecretFingerprint {
        keyed_fingerprint: Sha256Digest(mac.finalize().into_bytes().into()), needs_rotation,
    })
}

fn fields_for(category: ConfirmationCategory) -> &'static [&'static str] {
    use ConfirmationCategory::*;
    match category {
        User => &["name", "display_name", "role"],
        Role => &["name", "permissions"],
        Department => &["name", "display_name"],
        // binding_kind/owner_id/tenant_id: immutable principal binding, parsed
        // and strictly validated by allocation::specification (v1.81).
        // adopt_quarantine: F6 §3.10 (F6g) digest-covered adoption reference
        // `<hex>:<incarnation>:<digest>`, strictly parsed and bound to its
        // quarantine row by the gateway's EmployeeAuthority.
        Employee => &["employee_id", "runtime", "model", "capabilities", "permissions", "account_pool", "disk_bytes", "settings",
            "binding_kind", "owner_id", "tenant_id", "adopt_quarantine"],
        EmployeeInstructions => &["employee_id", "instruction_kind", "text"],
        UserAgentBinding => &["employee_id", "owner_id", "binding_kind"],
        ChannelBinding => &["employee_id", "channel", "binding"],
        LegacyApproval => &["reference"],
        McpServer => &["employee_id", "env", "name", "transport", "command", "args", "url", "credential"],
        Skill => &["employee_id", "name", "artifact_digest", "text"],
        Hook => &["employee_id", "name", "event", "settings", "command", "args"],
        Policy => &["employee_id", "name", "rules"],
        Connector => &["settings", "name", "binding_kind", "owner_id", "credential"],
        DataSourceAuthorization => &["settings", "name", "employee_id", "grants"],
        Budget => &["name", "amount_cents", "period"],
        ConfigurationSection => &["section", "settings", "credential"],
        Schedule => &["settings", "name", "schedule", "objective", "employee_id"],
        // F6 §3.12 (R12): every field digest-covered; strictly parsed by
        // allocation::retire_specification (all required, exact values).
        EmployeeRetire => crate::allocation::RETIRE_FIELDS,
    }
}

/// Exact category field sets prevent unknown legacy keys silently taking effect.
/// Nested JSON is deliberately not interpreted here; validate_fresh must parse it
/// with the corresponding product schema before approval and staging.
fn normalize(category: ConfirmationCategory, fields: &mut [ProposalField]) -> Result<()> {
    if fields.is_empty() || fields.len() > MAX_PROPOSAL_FIELDS { return Err(ConfirmationError::Invalid); }
    fields.sort_by(|a, b| a.name.cmp(&b.name));
    for (index, field) in fields.iter().enumerate() {
        if (field.name != "requires_reentry" && !fields_for(category).iter().any(|name| *name == field.name))
            || (index > 0 && fields[index - 1].name == field.name) {
            return Err(ConfirmationError::Invalid);
        }
        match (&*field.name, &field.value) {
            ("credential", ProposalValue::SecretFingerprint { .. }) => {}
            ("credential", _) | (_, ProposalValue::SecretFingerprint { .. }) => return Err(ConfirmationError::Invalid),
            (_, ProposalValue::Text { value }) if value.len() <= MAX_PROPOSAL_VALUE_BYTES => {}
            _ => return Err(ConfirmationError::Invalid),
        }
    }
    Ok(())
}

fn digest(category: ConfirmationCategory, fields: &[ProposalField]) -> Result<Sha256Digest> {
    let mut hash = Sha256::new();
    hash.update(b"duduclaw-migration-proposal-v1\0");
    hash.update(serde_json::to_vec(&(category, fields))?);
    Ok(Sha256Digest(hash.finalize().into()))
}

/// Revalidate the entire normalized payload at the privileged allocation boundary.
pub fn verify_item_binding(item: &ConfirmationItem) -> Result<()> {
    item.validate().map_err(|_| ConfirmationError::Invalid)?;
    let mut fields = item.fields.clone();
    normalize(item.category, &mut fields)?;
    if fields != item.fields || digest(item.category, &fields)? != item.binding.proposal_digest {
        return Err(ConfirmationError::Stale);
    }
    Ok(())
}
#[path = "confirmation_effects.rs"]
mod effects;

pub struct ConfirmationStore { conn: Connection }
impl ConfirmationStore {
    /// Opens only a store already initialized in a fresh gateway home.
    pub fn open_existing(path: &std::path::Path) -> Result<Self> {
        let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL;")?;
        conn.query_row("SELECT COUNT(*) FROM migration_proposals", [], |r| r.get::<_, u64>(0))?;
        effects::schema(&conn)?;
        Ok(Self { conn })
    }
    /// Does not create a database or schema. The gateway only uses this after the
    /// installer has created a fresh migration store at its fixed trusted path.
    pub fn open_read_only(path: &std::path::Path) -> Result<Self> {
        let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        conn.query_row("SELECT COUNT(*) FROM migration_proposals", [], |r| r.get::<_, u64>(0))?;
        Ok(Self { conn })
    }
    /// [`Self::open_read_only`] for a connection the caller already opened
    /// read-only through its own hardened (no-follow, owner/mode/inode
    /// checked) opener. Never creates or writes; a writable connection is
    /// refused so this cannot become a schema-creating side door.
    pub fn from_read_only_connection(conn: Connection) -> Result<Self> {
        if !conn.is_readonly(rusqlite::DatabaseName::Main)? { return Err(ConfirmationError::Storage); }
        conn.query_row("SELECT COUNT(*) FROM migration_proposals", [], |r| r.get::<_, u64>(0))?;
        Ok(Self { conn })
    }
    /// The caller supplies a freshly created gateway-owned database, never legacy
    /// input. File ownership and exclusive migration mode are installer obligations.
    pub fn from_connection(conn: Connection) -> Result<Self> {
        conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS migration_proposals (
                id BLOB NOT NULL, revision INTEGER NOT NULL, item TEXT NOT NULL,
                PRIMARY KEY(id, revision), CHECK(revision > 0));
            CREATE TABLE IF NOT EXISTS migration_apply (
                id BLOB NOT NULL, revision INTEGER NOT NULL, digest BLOB NOT NULL,
                step INTEGER NOT NULL, PRIMARY KEY(id, revision));
            CREATE TABLE IF NOT EXISTS migration_confirmation_audit (
                sequence INTEGER PRIMARY KEY, proposal_id BLOB NOT NULL,
                revision INTEGER NOT NULL, digest BLOB NOT NULL,
                event TEXT NOT NULL, actor BLOB);
            CREATE TABLE IF NOT EXISTS migration_rotations (
                id BLOB NOT NULL, revision INTEGER NOT NULL, field TEXT NOT NULL,
                fingerprint BLOB NOT NULL, PRIMARY KEY(id, revision, field));")?;
        effects::schema(&conn)?;
        Ok(Self { conn })
    }

    pub fn list(&self) -> Result<Vec<ConfirmationItem>> {
        let (count, bytes) = usage(&self.conn)?;
        if count > MAX_CONFIRMATIONS || bytes > MAX_CONFIRMATION_STORE_BYTES {
            return Err(ConfirmationError::Limit);
        }
        let mut query = self.conn.prepare("SELECT item FROM migration_proposals ORDER BY id, revision")?;
        let rows = query.query_map([], |row| row.get::<_, String>(0))?;
        rows.map(|row| {
            let item: ConfirmationItem = serde_json::from_str(&row?)?;
            item.validate().map_err(|_| ConfirmationError::Invalid)?;
            if digest(item.category, &item.fields)? != item.binding.proposal_digest {
                return Err(ConfirmationError::Stale);
            }
            Ok(item)
        }).collect()
    }

    pub fn stage_proposal(&mut self, operation_id: ProposalOperationId,
        category: ConfirmationCategory, mut fields: Vec<ProposalField>,
        validator: &dyn ProposalValidator) -> Result<ConfirmationItem> {
        normalize(category, &mut fields)?;
        let item = ConfirmationItem {
            binding: ProposalBinding { operation_id, proposal_digest: digest(category, &fields)? },
            category, status: if category == ConfirmationCategory::LegacyApproval {
                ConfirmationStatus::ExpiredRejected
            } else { ConfirmationStatus::Pending },
            // Conservative: unknown product defaults never turn a risky item into
            // bulk approval. Only inert department labels qualify for bulk in v1.
            high_risk: category != ConfirmationCategory::Department,
            fields,
        };
        item.validate().map_err(|_| ConfirmationError::Invalid)?;
        let encoded = serde_json::to_string(&item)?;
        if encoded.len() > MAX_CONFIRMATION_BYTES { return Err(ConfirmationError::Limit); }
        if operation_id.revision > i64::MAX as u64 { return Err(ConfirmationError::Invalid); }
        validator.validate_fresh(&item)?;
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = load(&tx, &item.binding)? {
            return if existing.fields == item.fields && existing.category == category {
                Ok(existing)
            } else { Err(ConfirmationError::Stale) };
        }
        let previous: Option<String> = tx.query_row(
            "SELECT item FROM migration_proposals WHERE id=?1 ORDER BY revision DESC LIMIT 1",
            [operation_id.proposal_id.0.as_slice()], |r| r.get(0)).optional()?;
        let is_revision = previous.is_some();
        if let Some(previous) = previous {
            let mut previous: ConfirmationItem = serde_json::from_str(&previous)?;
            if previous.status != ConfirmationStatus::Pending || previous.category != category
                || previous.binding.operation_id.revision.checked_add(1) != Some(operation_id.revision) {
                return Err(ConfirmationError::Stale);
            }
            previous.transition(ConfirmationStatus::ExpiredRejected).map_err(|_| ConfirmationError::Invalid)?;
            save(&tx, &previous)?;
            audit(&tx, &previous, "superseded", None)?;
        } else if operation_id.revision != 1 {
            return Err(ConfirmationError::Stale);
        }
        let (count, bytes) = usage(&tx)?;
        if count >= MAX_CONFIRMATIONS || bytes + encoded.len() as u64 + 64 * (count + 1) > MAX_CONFIRMATION_STORE_BYTES {
            return Err(ConfirmationError::Limit);
        }
        if category == ConfirmationCategory::Employee && !is_revision {
            // SR2: only open Employee proposals (latest revision still awaiting
            // a decision or being applied/compensated) hold a slot. Closed ones
            // (applied, failed, rejected, expired) no longer exhaust the quota;
            // the store-wide row/byte bound above still applies to them.
            let count: u32 = tx.query_row(
                "SELECT COUNT(*) FROM migration_proposals p
                 WHERE json_extract(p.item,'$.category')='employee'
                   AND p.revision=(SELECT MAX(q.revision) FROM migration_proposals q WHERE q.id=p.id)
                   AND json_extract(p.item,'$.status') IN ('pending','approved','applying','rejecting')",
                [], |r| r.get(0))?;
            if count >= DEFAULT_EMPLOYEE_PROPOSAL_LIMIT { return Err(ConfirmationError::Limit); }
        }
        tx.execute("INSERT INTO migration_proposals(id,revision,item) VALUES(?1,?2,?3)",
            params![operation_id.proposal_id.0.as_slice(), operation_id.revision, encoded])?;
        audit(&tx, &item, "staged", None)?;
        tx.commit()?;
        Ok(item)
    }

    pub fn approve(&mut self, bindings: &[ProposalBinding], session: &ApprovalSession,
        csrf: [u8; 32], now: u64, validator: &dyn ProposalValidator) -> Result<()> {
        session.check(&csrf, now)?;
        if bindings.is_empty() || bindings.len() > 200 { return Err(ConfirmationError::Limit); }
        // Validators can wait for live registry or remote readback. Never keep a
        // SQLite writer while they run. The transaction below rechecks every
        // immutable binding and Pending status before committing the batch.
        for binding in bindings {
            let item = required(&self.conn, binding)?;
            if item.status != ConfirmationStatus::Pending || (bindings.len() > 1 && item.high_risk) {
                return Err(ConfirmationError::Denied);
            }
            validator.validate_fresh(&item)?;
        }
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for binding in bindings {
            let mut item = required(&tx, binding)?;
            if item.status != ConfirmationStatus::Pending || (bindings.len() > 1 && item.high_risk) {
                return Err(ConfirmationError::Denied);
            }
            item.transition(ConfirmationStatus::Approved).map_err(|_| ConfirmationError::Denied)?;
            save(&tx, &item)?;
            audit(&tx, &item, "approved", Some(session.actor))?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Advances one durable effect: intent -> commit -> adapter -> receipt CAS.
    /// The proposal lock is released by process death, unlike a stale lease that
    /// could permit a still-running publisher to race compensation. Other
    /// proposals remain writable throughout an external call.
    pub fn apply_next(&mut self, binding: &ProposalBinding,
        adapter: &mut dyn ProposalApplier) -> Result<ConfirmationItem> {
        let _guard = effects::lock(&self.conn, binding)?;
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut item = required(&tx, binding)?;
        if item.status == ConfirmationStatus::Applied { return Ok(item); }
        if item.status == ConfirmationStatus::Approved {
            item.transition(ConfirmationStatus::Applying).map_err(|_| ConfirmationError::Denied)?;
        } else if item.status != ConfirmationStatus::Applying { return Err(ConfirmationError::Denied); }
        let next = match step(&tx, binding)? {
            None | Some(3) => 0, Some(0) => 1, Some(1) => 2,
            _ => return Err(ConfirmationError::Stale),
        };
        save(&tx, &item)?;
        let intent = effects::intent(&tx, &item, next)?;
        tx.commit()?;
        let result = match next {
            0 => adapter.stage(&item), 1 => adapter.publish(&item), _ => adapter.read_back(&item),
        };
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        effects::consume(&tx, &item, next, &intent)?;
        if result.is_err() {
            item.transition(ConfirmationStatus::Failed).map_err(|_| ConfirmationError::Invalid)?;
            save(&tx, &item)?;
            audit(&tx, &item, "failed_compensation_pending", None)?;
            let intent = effects::intent(&tx, &item, 3)?;
            tx.commit()?;
            let disabled = adapter.disable_and_read_back(&item).is_ok();
            let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            effects::consume(&tx, &item, 3, &intent)?;
            if disabled {
                receipt(&tx, &item, 3)?;
                audit(&tx, &item, "failed_disabled", None)?;
            } else {
                audit(&tx, &item, "compensation_failed", None)?;
            }
            tx.commit()?;
            return Err(if disabled { ConfirmationError::Apply } else { ConfirmationError::Compensation });
        }
        receipt(&tx, &item, next)?;
        if next == 2 { item.transition(ConfirmationStatus::Applied).map_err(|_| ConfirmationError::Invalid)?; }
        save(&tx, &item)?;
        audit(&tx, &item, match next { 0 => "apply_staged", 1 => "published", _ => "read_back" }, None)?;
        tx.commit()?;
        Ok(item)
    }

    pub fn retry(&mut self, binding: &ProposalBinding, session: &ApprovalSession,
        csrf: [u8; 32], now: u64, adapter: &mut dyn ProposalApplier,
        validator: &dyn ProposalValidator) -> Result<()> {
        session.check(&csrf, now)?;
        let _guard = effects::lock(&self.conn, binding)?;
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut item = required(&tx, binding)?;
        if item.status != ConfirmationStatus::Failed { return Err(ConfirmationError::Denied); }
        let intent = effects::intent(&tx, &item, 3)?;
        tx.commit()?;
        // F6 §3.12 (R14): a retire is forward only. Its compensation is a
        // read-back that succeeds only while the root never started, so a
        // retry never depends on it: the same retire operation resumes
        // (spawnd replays it idempotently from the recorded phase).
        if item.category != ConfirmationCategory::EmployeeRetire {
            adapter.disable_and_read_back(&item).map_err(|_| ConfirmationError::Compensation)?;
        }
        // Disable before absence validation. Both calls are outside the writer.
        // A validation failure leaves Failed and can safely repeat compensation.
        validator.validate_fresh(&item)?;
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        effects::consume(&tx, &item, 3, &intent)?;
        receipt(&tx, &item, 3)?;
        item.transition(ConfirmationStatus::Applying).map_err(|_| ConfirmationError::Invalid)?;
        save(&tx, &item)?;
        audit(&tx, &item, "retry", Some(session.actor))?;
        tx.commit()?;
        Ok(())
    }

    pub fn reject(&mut self, binding: &ProposalBinding, session: &ApprovalSession,
        csrf: [u8; 32], now: u64, adapter: &mut dyn ProposalApplier) -> Result<()> {
        session.check(&csrf, now)?;
        let _guard = effects::lock(&self.conn, binding)?;
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut item = required(&tx, binding)?;
        if item.status == ConfirmationStatus::Rejected { return Ok(()); }
        // Forward only (F6 §3.12, R14): past Pending, an EmployeeRetire item
        // is rejected only when its compensation read-back says the root
        // entry is still Ready (no root effect yet); otherwise nothing moves.
        let forward_only = item.category == ConfirmationCategory::EmployeeRetire
            && item.status != ConfirmationStatus::Pending;
        let before = item.clone();
        if item.status == ConfirmationStatus::Pending {
            item.transition(ConfirmationStatus::Rejected).map_err(|_| ConfirmationError::Denied)?;
        } else if item.status != ConfirmationStatus::Rejecting {
            if item.status == ConfirmationStatus::Applying {
                item.transition(ConfirmationStatus::Failed).map_err(|_| ConfirmationError::Denied)?;
            }
            item.transition(ConfirmationStatus::Rejecting).map_err(|_| ConfirmationError::Denied)?;
        }
        save(&tx, &item)?;
        audit(&tx, &item, "reject_requested", Some(session.actor))?;
        if item.status == ConfirmationStatus::Rejected { tx.commit()?; return Ok(()); }
        let intent = effects::intent(&tx, &item, 3)?;
        tx.commit()?;
        if let Err(error) = adapter.disable_and_read_back(&item) {
            if !forward_only { return Err(ConfirmationError::Compensation); }
            let _ = error;
            // Nothing sticks in Rejecting: restore the prior state. A crash
            // left a retire in Rejecting earlier; its resumable state is Failed
            // (`retry` continues the same operation).
            let mut restored = before;
            if restored.status == ConfirmationStatus::Rejecting { restored.status = ConfirmationStatus::Failed; }
            let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            effects::consume(&tx, &item, 3, &intent)?;
            save(&tx, &restored)?;
            audit(&tx, &restored, "reject_refused_forward_only", Some(session.actor))?;
            tx.commit()?;
            return Err(ConfirmationError::ForwardOnly);
        }
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        effects::consume(&tx, &item, 3, &intent)?;
        receipt(&tx, &item, 3)?;
        item.transition(ConfirmationStatus::Rejected).map_err(|_| ConfirmationError::Denied)?;
        save(&tx, &item)?;
        audit(&tx, &item, "rejected_disabled", Some(session.actor))?;
        tx.commit()?;
        Ok(())
    }

    /// Bulk rejection is atomic for untouched proposals. Partial applications
    /// require per-item compensation through reject(), never a bulk shortcut.
    pub fn reject_pending(&mut self, bindings: &[ProposalBinding], session: &ApprovalSession,
        csrf: [u8; 32], now: u64) -> Result<()> {
        session.check(&csrf, now)?;
        if bindings.is_empty() || bindings.len() > 200 { return Err(ConfirmationError::Limit); }
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for binding in bindings {
            let mut item = required(&tx, binding)?;
            if item.status != ConfirmationStatus::Pending { return Err(ConfirmationError::Denied); }
            item.transition(ConfirmationStatus::Rejected).map_err(|_| ConfirmationError::Denied)?;
            save(&tx, &item)?;
            audit(&tx, &item, "rejected", Some(session.actor))?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Admin purge (Admin + MFA + fresh step-up + CSRF) of closed Employee
    /// proposals that never reached an apply step: every revision is an
    /// Employee proposal, the latest is Rejected or ExpiredRejected, and no
    /// apply journal, effect intent or rotation row exists for the id. Such a
    /// proposal had no external effect, so deleting its rows frees store
    /// capacity without losing compensation state; the audit trail keeps one
    /// `purged` event. Anything else (open, applied, failed, compensated
    /// after an apply step, or another category) is left untouched.
    /// Returns the number of proposals purged.
    pub fn purge_closed_employee(&mut self, ids: &[RequestId], session: &ApprovalSession,
        csrf: [u8; 32], now: u64) -> Result<usize> {
        session.check(&csrf, now)?;
        if ids.len() > 200 { return Err(ConfirmationError::Limit); }
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut purged = 0;
        for id in ids {
            let mut query = tx.prepare("SELECT item FROM migration_proposals WHERE id=?1 ORDER BY revision")?;
            let revisions = query.query_map([id.0.as_slice()], |r| r.get::<_, String>(0))?
                .map(|row| -> Result<ConfirmationItem> { Ok(serde_json::from_str(&row?)?) })
                .collect::<Result<Vec<_>>>()?;
            drop(query);
            let Some(latest) = revisions.last() else { continue };
            if revisions.iter().any(|item| item.category != ConfirmationCategory::Employee
                || item.binding.operation_id.proposal_id != *id)
                || !matches!(latest.status, ConfirmationStatus::Rejected | ConfirmationStatus::ExpiredRejected) {
                continue;
            }
            let touched: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM migration_apply WHERE id=?1)
                    OR EXISTS(SELECT 1 FROM migration_effect_intents WHERE id=?1)
                    OR EXISTS(SELECT 1 FROM migration_rotations WHERE id=?1)",
                [id.0.as_slice()], |r| r.get(0))?;
            if touched { continue; }
            audit(&tx, latest, "purged", Some(session.actor))?;
            tx.execute("DELETE FROM migration_proposals WHERE id=?1", [id.0.as_slice()])?;
            purged += 1;
        }
        tx.commit()?;
        Ok(purged)
    }

    /// Rotation evidence comes from a fresh protected secret write and readback,
    /// never an imported assertion. Adapter verification is mandatory.
    pub fn record_rotation(&mut self, binding: &ProposalBinding, field: &str,
        new_fingerprint: Sha256Digest, validator: &dyn RotationVerifier) -> Result<()> {
        let _guard = effects::lock(&self.conn, binding)?;
        let item = required(&self.conn, binding)?;
        if item.status != ConfirmationStatus::Applied { return Err(ConfirmationError::Denied); }
        let old = item.fields.iter().find(|f| f.name == field).ok_or(ConfirmationError::Invalid)?;
        let ProposalValue::SecretFingerprint { keyed_fingerprint, .. } = &old.value else {
            return Err(ConfirmationError::Invalid);
        };
        if *keyed_fingerprint == new_fingerprint { return Err(ConfirmationError::Denied); }
        validator.verify_rotated(binding, field, new_fingerprint)?;
        let tx = self.conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if required(&tx, binding)?.status != ConfirmationStatus::Applied { return Err(ConfirmationError::Stale); }
        tx.execute("INSERT INTO migration_rotations VALUES(?1,?2,?3,?4)
            ON CONFLICT(id,revision,field) DO UPDATE SET fingerprint=excluded.fingerprint",
            params![binding.operation_id.proposal_id.0.as_slice(), binding.operation_id.revision,
                field, new_fingerprint.0.as_slice()])?;
        audit(&tx, &item, "secret_rotated", None)?;
        tx.commit()?;
        Ok(())
    }

    pub fn incomplete(&self) -> Result<bool> {
        for item in self.list()? {
            if !matches!(item.status, ConfirmationStatus::Applied | ConfirmationStatus::Rejected | ConfirmationStatus::ExpiredRejected) {
                return Ok(true);
            }
            if item.status != ConfirmationStatus::Applied { continue; }
            for field in &item.fields {
                if matches!(field.value, ProposalValue::SecretFingerprint { needs_rotation: true, .. }) {
                    let count: u32 = self.conn.query_row("SELECT COUNT(*) FROM migration_rotations WHERE id=?1 AND revision=?2 AND field=?3",
                        params![item.binding.operation_id.proposal_id.0.as_slice(), item.binding.operation_id.revision, field.name], |r| r.get(0))?;
                    if count == 0 { return Ok(true); }
                }
            }
        }
        Ok(false)
    }

    pub fn rotation_pending(&self, binding: &ProposalBinding) -> Result<bool> {
        let item = required(&self.conn, binding)?;
        if matches!(item.status, ConfirmationStatus::Rejected | ConfirmationStatus::ExpiredRejected) { return Ok(false); }
        for field in &item.fields {
            if matches!(field.value, ProposalValue::SecretFingerprint { needs_rotation: true, .. }) {
                let count: u32 = self.conn.query_row("SELECT COUNT(*) FROM migration_rotations WHERE id=?1 AND revision=?2 AND field=?3",
                    params![binding.operation_id.proposal_id.0.as_slice(), binding.operation_id.revision, field.name], |r| r.get(0))?;
                if count == 0 { return Ok(true); }
            }
        }
        Ok(false)
    }

    /// The approval record of this exact binding (F6 §3.12: the retire
    /// approver sent with `Action::Retire`): the actor of its latest
    /// `approved` audit event. `None` before approval; a stored actor that is
    /// not 16 bytes is `Invalid` (never treated as absent).
    pub fn approver(&self, binding: &ProposalBinding) -> Result<Option<RequestId>> {
        required(&self.conn, binding)?;
        let actor: Option<Option<Vec<u8>>> = self.conn.query_row(
            "SELECT actor FROM migration_confirmation_audit WHERE proposal_id=?1 AND revision=?2 AND digest=?3
             AND event='approved' ORDER BY sequence DESC LIMIT 1",
            params![binding.operation_id.proposal_id.0.as_slice(), binding.operation_id.revision,
                binding.proposal_digest.0.as_slice()], |r| r.get(0)).optional()?;
        match actor {
            None => Ok(None),
            Some(actor) => {
                let bytes: [u8; 16] = actor.ok_or(ConfirmationError::Invalid)?.try_into()
                    .map_err(|_| ConfirmationError::Invalid)?;
                Ok(Some(RequestId(bytes)))
            }
        }
    }

    pub fn apply_receipt(&self, binding: &ProposalBinding) -> Result<Option<ProposalApplyReceipt>> {
        let item = required(&self.conn, binding)?;
        let Some(value) = step(&self.conn, binding)? else { return Ok(None); };
        let step = match value {
            0 => ProposalApplyStep::Staged, 1 => ProposalApplyStep::Published,
            2 => ProposalApplyStep::ReadBack, 3 => ProposalApplyStep::Disabled,
            _ => return Err(ConfirmationError::Invalid),
        };
        Ok(Some(ProposalApplyReceipt { binding: *binding, category: item.category, step }))
    }
}

pub trait RotationVerifier {
    fn verify_rotated(&self, binding: &ProposalBinding, field: &str, new_fingerprint: Sha256Digest) -> Result<()>;
}

fn load(conn: &Connection, binding: &ProposalBinding) -> Result<Option<ConfirmationItem>> {
    let json: Option<String> = conn.query_row("SELECT item FROM migration_proposals WHERE id=?1 AND revision=?2",
        params![binding.operation_id.proposal_id.0.as_slice(), binding.operation_id.revision], |r| r.get(0)).optional()?;
    let Some(json) = json else { return Ok(None); };
    let item: ConfirmationItem = serde_json::from_str(&json)?;
    item.validate().map_err(|_| ConfirmationError::Invalid)?;
    if item.binding != *binding || digest(item.category, &item.fields)? != binding.proposal_digest {
        return Err(ConfirmationError::Stale);
    }
    Ok(Some(item))
}
fn usage(conn: &Connection) -> Result<(u64, u64)> {
    Ok(conn.query_row("SELECT COUNT(*), COALESCE(SUM(length(CAST(item AS BLOB))),0) FROM migration_proposals",
        [], |r| Ok((r.get(0)?, r.get(1)?)))?)
}
fn required(conn: &Connection, binding: &ProposalBinding) -> Result<ConfirmationItem> {
    load(conn, binding)?.ok_or(ConfirmationError::Stale)
}
fn save(conn: &Connection, item: &ConfirmationItem) -> Result<()> {
    let count = conn.execute("UPDATE migration_proposals SET item=?3 WHERE id=?1 AND revision=?2",
        params![item.binding.operation_id.proposal_id.0.as_slice(), item.binding.operation_id.revision, serde_json::to_string(item)?])?;
    if count != 1 { return Err(ConfirmationError::Stale); }
    Ok(())
}
fn audit(conn: &Connection, item: &ConfirmationItem, event: &str, actor: Option<RequestId>) -> Result<()> {
    conn.execute("INSERT INTO migration_confirmation_audit(proposal_id,revision,digest,event,actor) VALUES(?1,?2,?3,?4,?5)",
        params![item.binding.operation_id.proposal_id.0.as_slice(), item.binding.operation_id.revision,
            item.binding.proposal_digest.0.as_slice(), event, actor.map(|id| id.0.to_vec())])?;
    Ok(())
}
fn step(conn: &Connection, binding: &ProposalBinding) -> Result<Option<i64>> {
    let entry: Option<(Vec<u8>, i64)> = conn.query_row("SELECT digest,step FROM migration_apply WHERE id=?1 AND revision=?2",
        params![binding.operation_id.proposal_id.0.as_slice(), binding.operation_id.revision],
        |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
    match entry {
        Some((digest, step)) if digest == binding.proposal_digest.0 => Ok(Some(step)),
        Some(_) => Err(ConfirmationError::Stale), None => Ok(None),
    }
}
fn receipt(conn: &Connection, item: &ConfirmationItem, step: i64) -> Result<()> {
    conn.execute("INSERT INTO migration_apply VALUES(?1,?2,?3,?4)
        ON CONFLICT(id,revision) DO UPDATE SET step=excluded.step WHERE digest=excluded.digest",
        params![item.binding.operation_id.proposal_id.0.as_slice(), item.binding.operation_id.revision,
            item.binding.proposal_digest.0.as_slice(), step])?;
    Ok(())
}

#[cfg(test)]
#[path = "confirmation_tests.rs"]
mod tests;
#[cfg(test)]
#[path = "confirmation_retire_tests.rs"]
mod retire_tests;
