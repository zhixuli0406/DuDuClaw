//! Root-only orchestration journal (§10.1). SQLite never opens legacy databases.
use duduclaw_isolation::{ids::{RequestId, Sha256Digest}, migration::MigrationState};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use crate::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StageOperation {
    pub migration: RequestId,
    pub target: MigrationState,
}

/// Evidence is supplied by trusted host readback, never a dashboard request.
/// A receipt omits paths, content, keys, token values and arbitrary diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "stage", rename_all = "snake_case", deny_unknown_fields)]
pub enum Evidence {
    Stopped { activation_sources_blocked: bool, uid_empty: bool },
    Quarantined { snapshot_verified: bool, fresh_home_empty: bool, gateway_denied_legacy: bool },
    Rotated { root_key: bool, identity_key: bool, mcp_key: bool, jwt_key: bool, old_auth_absent: bool },
    Revocations { obligations_recorded: bool, reconnect_required: bool, cli_tokens_absent: bool },
    Employees { count: u32, no_resources_allocated: bool },
    Content { root_only_endpoint: bool, source_labels_stamped: bool, import_mode_stopped: bool },
    Materialized { all_records_terminal: bool },
    Confirmation { authority_inactive: bool, legacy_approvals_expired: bool },
    Isolation { ownership: bool, quota: bool, protocols: bool, no_legacy_inodes: bool },
    Administrator { new_service_running: bool, setup_token_ready: bool },
    Complete { outstanding_proposals: u32, rotation_obligations: u32, failed_applies: u32,
        unclaimed_personal_owners: u32, isolation_readback: bool },
}

impl Evidence {
    pub(crate) fn validate(&self, target: MigrationState) -> Result<(), Error> {
        use MigrationState as S;
        let valid = match (target, self) {
            (S::OldEnvironmentStopped, Self::Stopped { activation_sources_blocked, uid_empty }) =>
                *activation_sources_blocked && *uid_empty,
            (S::LegacyQuarantined, Self::Quarantined { snapshot_verified, fresh_home_empty, gateway_denied_legacy }) =>
                *snapshot_verified && *fresh_home_empty && *gateway_denied_legacy,
            (S::KeysRotated, Self::Rotated { root_key, identity_key, mcp_key, jwt_key, old_auth_absent }) =>
                *root_key && *identity_key && *mcp_key && *jwt_key && *old_auth_absent,
            (S::TokenRevocationRecorded, Self::Revocations { obligations_recorded, reconnect_required, cli_tokens_absent }) =>
                *obligations_recorded && *reconnect_required && *cli_tokens_absent,
            (S::EmployeeProposalsStaged, Self::Employees { count, no_resources_allocated }) =>
                *count <= duduclaw_isolation::migration::DEFAULT_EMPLOYEE_PROPOSAL_LIMIT && *no_resources_allocated,
            (S::ContentArchived, Self::Content { root_only_endpoint, source_labels_stamped, import_mode_stopped }) =>
                *root_only_endpoint && *source_labels_stamped && *import_mode_stopped,
            (S::ContentImported, Self::Materialized { all_records_terminal }) => *all_records_terminal,
            (S::ConfirmationReady, Self::Confirmation { authority_inactive, legacy_approvals_expired }) =>
                *authority_inactive && *legacy_approvals_expired,
            (S::IsolationVerified, Self::Isolation { ownership, quota, protocols, no_legacy_inodes }) =>
                *ownership && *quota && *protocols && *no_legacy_inodes,
            (S::AwaitingAdministrator, Self::Administrator { new_service_running, setup_token_ready }) =>
                *new_service_running && *setup_token_ready,
            (S::Complete, Self::Complete { outstanding_proposals, rotation_obligations, failed_applies,
                unclaimed_personal_owners, isolation_readback }) =>
                *outstanding_proposals == 0 && *rotation_obligations == 0 && *failed_applies == 0
                    && *unclaimed_personal_owners == 0 && *isolation_readback,
            _ => false,
        };
        if valid { Ok(()) } else { Err(Error::Readback) }
    }
}

/// Installer-owned seam. Every operation must be idempotent on StageOperation.
/// None means no completed effect; an error means unknown and must stop progress.
/// The adapter holds the root migration flock for its entire lifetime, including
/// run_one. It must re-read actual state, not replay old evidence from this journal.
pub trait HostAdapter {
    fn readback(&mut self, operation: StageOperation) -> Result<Option<Evidence>, Error>;
    fn perform(&mut self, operation: StageOperation) -> Result<(), Error>;
}

/// No operational adapter is selected until installer integrations are complete.
pub struct UnavailableHost;
impl HostAdapter for UnavailableHost {
    fn readback(&mut self, _: StageOperation) -> Result<Option<Evidence>, Error> { Err(Error::Unavailable) }
    fn perform(&mut self, _: StageOperation) -> Result<(), Error> { Err(Error::Unavailable) }
}

pub struct Journal { db: Connection, migration: RequestId }

fn encode<T: Serialize>(value: &T) -> Result<String, Error> { Ok(serde_json::to_string(value)?) }

impl Journal {
    /// The installer supplies a NEW root-owned DB inside its locked 0700 journal
    /// directory. Never pass a connection to any old database or a gateway DB.
    pub fn from_connection(mut db: Connection, migration: RequestId) -> Result<Self, Error> {
        db.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS migration (
                singleton INTEGER PRIMARY KEY CHECK(singleton=1), id TEXT NOT NULL,
                state TEXT NOT NULL, intent TEXT);
            CREATE TABLE IF NOT EXISTS migration_receipts (
                target TEXT PRIMARY KEY, evidence TEXT NOT NULL, digest BLOB NOT NULL);")?;
        db.execute("INSERT OR IGNORE INTO migration VALUES(1, ?1, ?2, NULL)",
            params![encode(&migration)?, encode(&MigrationState::Planned)?])?;
        let id: String = db.query_row("SELECT id FROM migration WHERE singleton=1", [], |r| r.get(0))?;
        if id != encode(&migration)? { return Err(Error::Conflict); }
        upgrade_archive_receipts(&mut db, migration)?;
        Ok(Self { db, migration })
    }

    pub fn state(&self) -> Result<MigrationState, Error> {
        let state: String = self.db.query_row("SELECT state FROM migration WHERE singleton=1", [], |r| r.get(0))?;
        Ok(serde_json::from_str(&state)?)
    }

    /// Persist intent, execute/reconcile, then atomically persist receipt + state.
    /// An interrupted call always retains the previous completed state.
    pub fn run_one(&mut self, target: MigrationState, host: &mut impl HostAdapter) -> Result<Sha256Digest, Error> {
        let operation = StageOperation { migration: self.migration, target };
        let operation_json = encode(&operation)?;
        let target_json = encode(&target)?;
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (current, intent): (String, Option<String>) = tx.query_row(
            "SELECT state, intent FROM migration WHERE singleton=1", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
        let current_state: MigrationState = serde_json::from_str(&current)?;
        current_state.transition(target).map_err(|_| Error::Conflict)?;
        if intent.as_ref().is_some_and(|value| value != &operation_json) { return Err(Error::Conflict); }
        tx.execute("UPDATE migration SET intent=?1 WHERE singleton=1", [&operation_json])?;
        tx.commit()?;

        let evidence = match host.readback(operation)? {
            Some(value) => value,
            None => {
                host.perform(operation)?;
                host.readback(operation)?.ok_or(Error::Readback)?
            }
        };
        evidence.validate(target)?;
        let evidence_json = encode(&evidence)?;
        let digest = Sha256Digest(Sha256::digest(evidence_json.as_bytes()).into());
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let updated = tx.execute("UPDATE migration SET state=?1,intent=NULL
            WHERE singleton=1 AND state=?2 AND intent=?3",
            params![target_json, current, operation_json])?;
        if updated != 1 { return Err(Error::Conflict); }
        tx.execute("INSERT INTO migration_receipts VALUES(?1,?2,?3)",
            params![encode(&target)?, evidence_json, &digest.0[..]])?;
        tx.commit()?;
        Ok(digest)
    }

    pub fn has_pending_intent(&self) -> Result<bool, Error> {
        Ok(self.db.query_row("SELECT intent FROM migration WHERE singleton=1", [],
            |r| r.get::<_, Option<String>>(0)).optional()?.flatten().is_some())
    }
}

/// Previous journals used ContentImported for an archive-only import. Rewrite
/// that meaning atomically before any caller can return early from Complete.
/// New Materialized evidence is never downgraded. Receipt bytes/digests stay
/// unchanged because only the stage name, not its evidence, is being corrected.
fn upgrade_archive_receipts(db: &mut Connection, migration: RequestId) -> Result<(), Error> {
    use MigrationState as S;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (state, intent): (String, Option<String>) = tx.query_row(
        "SELECT state,intent FROM migration WHERE singleton=1", [], |r| Ok((r.get(0)?,r.get(1)?)))?;
    let mut current: MigrationState = serde_json::from_str(&state)?;
    let mut pending: Option<StageOperation> = intent.as_deref().map(serde_json::from_str).transpose()?;
    if pending.as_ref().is_some_and(|op| op.migration != migration) { return Err(Error::Conflict); }
    let imported: Option<(String, Vec<u8>)> = tx.query_row(
        "SELECT evidence,digest FROM migration_receipts WHERE target=?1", [encode(&S::ContentImported)?],
        |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
    let legacy = match &imported {
        Some((json, _)) => matches!(serde_json::from_str::<Evidence>(json)?, Evidence::Content { .. }),
        None => false,
    };
    if legacy {
        let (evidence, digest) = imported.as_ref().ok_or(Error::Readback)?;
        serde_json::from_str::<Evidence>(evidence)?.validate(S::ContentArchived)?;
        if digest.as_slice() != Sha256::digest(evidence.as_bytes()).as_slice() { return Err(Error::Readback); }
        let archived: Option<(String, Vec<u8>)> = tx.query_row(
            "SELECT evidence,digest FROM migration_receipts WHERE target=?1", [encode(&S::ContentArchived)?],
            |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
        if archived.as_ref().is_some_and(|existing| existing != &(evidence.clone(),digest.clone())) {
            return Err(Error::Conflict);
        }
        tx.execute("INSERT OR IGNORE INTO migration_receipts(target,evidence,digest) VALUES(?1,?2,?3)",
            params![encode(&S::ContentArchived)?, evidence, digest])?;
        tx.execute("DELETE FROM migration_receipts WHERE target IN (?1,?2)",
            params![encode(&S::ContentImported)?, encode(&S::Complete)?])?;
        if current == S::ContentImported { current = S::ContentArchived; }
        if current == S::Complete { current = S::AwaitingAdministrator; }
        // Old completion intent has no materialization stage before it. A new
        // intent must be prepared for ContentImported after admin resolution.
        if pending.as_ref().is_some_and(|op| op.target == S::Complete) { pending = None; }
    }
    if current == S::EmployeeProposalsStaged {
        if let Some(operation) = &mut pending {
            if operation.target == S::ContentImported { operation.target = S::ContentArchived; }
        }
    }
    let pending = pending.as_ref().map(encode).transpose()?;
    if encode(&current)? != state || pending != intent {
        tx.execute("UPDATE migration SET state=?1,intent=?2 WHERE singleton=1",
            params![encode(&current)?,pending])?;
    }
    tx.commit()?;
    Ok(())
}

/// Plaintext cannot be removed until a verified encrypted snapshot exists;
/// retention expiry also requires that snapshot's readback before deletion.
pub fn may_remove_quarantine(all_employees_resolved: bool, snapshot_verified: bool,
    now: u64, expires_at: u64) -> bool {
    let _ = (now, expires_at);
    snapshot_verified && all_employees_resolved
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Host { applied: bool, fail_after_effect: bool, calls: usize, evidence: Evidence }
    impl HostAdapter for Host {
        fn readback(&mut self, _: StageOperation) -> Result<Option<Evidence>, Error> {
            Ok(self.applied.then(|| self.evidence.clone()))
        }
        fn perform(&mut self, _: StageOperation) -> Result<(), Error> {
            self.calls += 1; self.applied = true;
            if self.fail_after_effect { Err(Error::Unavailable) } else { Ok(()) }
        }
    }
    fn stages() -> Vec<(MigrationState, Evidence)> {
        use MigrationState as S;
        vec![
            (S::OldEnvironmentStopped, Evidence::Stopped { activation_sources_blocked: true, uid_empty: true }),
            (S::LegacyQuarantined, Evidence::Quarantined { snapshot_verified: true, fresh_home_empty: true, gateway_denied_legacy: true }),
            (S::KeysRotated, Evidence::Rotated { root_key: true, identity_key: true, mcp_key: true, jwt_key: true, old_auth_absent: true }),
            (S::TokenRevocationRecorded, Evidence::Revocations { obligations_recorded: true, reconnect_required: true, cli_tokens_absent: true }),
            (S::EmployeeProposalsStaged, Evidence::Employees { count: 200, no_resources_allocated: true }),
            (S::ContentArchived, Evidence::Content { root_only_endpoint: true, source_labels_stamped: true, import_mode_stopped: true }),
            (S::ConfirmationReady, Evidence::Confirmation { authority_inactive: true, legacy_approvals_expired: true }),
            (S::IsolationVerified, Evidence::Isolation { ownership: true, quota: true, protocols: true, no_legacy_inodes: true }),
            (S::AwaitingAdministrator, Evidence::Administrator { new_service_running: true, setup_token_ready: true }),
            (S::ContentImported, Evidence::Materialized { all_records_terminal: true }),
            (S::Complete, Evidence::Complete { outstanding_proposals: 0, rotation_obligations: 0, failed_applies: 0, unclaimed_personal_owners: 0, isolation_readback: true }),
        ]
    }
    #[test]
    fn crash_after_every_effect_resumes_without_repeating_it() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("journal.db");
        let id = RequestId([3; 16]);
        let mut previous = MigrationState::Planned;
        for (target, evidence) in stages() {
            let mut journal = Journal::from_connection(Connection::open(&path).unwrap(), id).unwrap();
            let mut host = Host { applied: false, fail_after_effect: true, calls: 0, evidence };
            assert_eq!(journal.run_one(target, &mut host), Err(Error::Unavailable));
            assert_eq!(journal.state().unwrap(), previous);
            assert!(journal.has_pending_intent().unwrap());
            drop(journal);
            let mut reopened = Journal::from_connection(Connection::open(&path).unwrap(), id).unwrap();
            reopened.run_one(target, &mut host).unwrap();
            assert_eq!(host.calls, 1);
            assert_eq!(reopened.state().unwrap(), target);
            assert!(!reopened.has_pending_intent().unwrap());
            previous = target;
        }
    }
    #[test]
    fn missing_readback_or_adapter_never_advances() {
        let mut journal = Journal::from_connection(Connection::open_in_memory().unwrap(), RequestId([0;16])).unwrap();
        assert_eq!(journal.run_one(MigrationState::OldEnvironmentStopped, &mut UnavailableHost), Err(Error::Unavailable));
        assert_eq!(journal.state().unwrap(), MigrationState::Planned);
        let mut host = Host { applied: true, fail_after_effect: false, calls: 0,
            evidence: Evidence::Stopped { activation_sources_blocked: true, uid_empty: false } };
        assert_eq!(journal.run_one(MigrationState::OldEnvironmentStopped, &mut host), Err(Error::Readback));
        assert_eq!(journal.run_one(MigrationState::KeysRotated, &mut host), Err(Error::Conflict));
    }
    #[test]
    fn outstanding_rotation_and_owner_login_block_completion() {
        let good = Evidence::Complete { outstanding_proposals: 0, rotation_obligations: 0,
            failed_applies: 0, unclaimed_personal_owners: 0, isolation_readback: true };
        assert!(good.validate(MigrationState::Complete).is_ok());
        for bad in [Evidence::Complete { outstanding_proposals: 0, rotation_obligations: 1,
            failed_applies: 0, unclaimed_personal_owners: 0, isolation_readback: true },
            Evidence::Complete { outstanding_proposals: 0, rotation_obligations: 0,
                failed_applies: 0, unclaimed_personal_owners: 1, isolation_readback: true }] {
            assert!(bad.validate(MigrationState::Complete).is_err());
        }
        assert!(Evidence::Employees { count: 201, no_resources_allocated: true }
            .validate(MigrationState::EmployeeProposalsStaged).is_err());
    }
    #[test]
    fn snapshot_retention_requires_verified_copy() {
        assert!(!may_remove_quarantine(true, false, 100, 10));
        assert!(!may_remove_quarantine(false, true, 9, 10));
        assert!(!may_remove_quarantine(false, true, 10, 10));
        assert!(may_remove_quarantine(true, true, 9, 10));
    }
    fn put_receipt(journal: &Journal, target: MigrationState, evidence: &Evidence) {
        let json = encode(evidence).unwrap();
        journal.db.execute("INSERT INTO migration_receipts VALUES(?1,?2,?3)",
            params![encode(&target).unwrap(), &json, Sha256::digest(json.as_bytes()).as_slice()]).unwrap();
    }
    fn archive_evidence() -> Evidence {
        Evidence::Content { root_only_endpoint: true, source_labels_stamped: true, import_mode_stopped: true }
    }
    fn receipt_count(journal: &Journal, target: MigrationState) -> u64 {
        journal.db.query_row("SELECT COUNT(*) FROM migration_receipts WHERE target=?1",
            [encode(&target).unwrap()], |r|r.get(0)).unwrap()
    }
    #[test]
    fn legacy_archive_only_receipts_are_upgraded_once_and_cannot_preserve_complete() {
        use MigrationState as S;
        for (old, expected) in [(S::ContentImported,S::ContentArchived),
            (S::ConfirmationReady,S::ConfirmationReady), (S::IsolationVerified,S::IsolationVerified),
            (S::AwaitingAdministrator,S::AwaitingAdministrator), (S::Complete,S::AwaitingAdministrator)] {
            let id = RequestId([17;16]);
            let journal = Journal::from_connection(Connection::open_in_memory().unwrap(), id).unwrap();
            journal.db.execute("UPDATE migration SET state=?1", [encode(&old).unwrap()]).unwrap();
            put_receipt(&journal, S::ContentImported, &archive_evidence());
            if old == S::Complete { put_receipt(&journal, S::Complete, &stages().last().unwrap().1); }
            let reopened = Journal::from_connection(journal.db, id).unwrap();
            assert_eq!(reopened.state().unwrap(), expected);
            assert_eq!(receipt_count(&reopened,S::ContentArchived),1);
            assert_eq!(receipt_count(&reopened,S::ContentImported),0);
            assert_eq!(receipt_count(&reopened,S::Complete),0);
            let again = Journal::from_connection(reopened.db, id).unwrap();
            assert_eq!(again.state().unwrap(), expected);
            assert_eq!(receipt_count(&again,S::ContentArchived),1);
        }
    }
    #[test]
    fn legacy_archive_pending_intent_resumes_under_new_stage_name() {
        use MigrationState as S;
        let id = RequestId([18;16]);
        let journal = Journal::from_connection(Connection::open_in_memory().unwrap(), id).unwrap();
        journal.db.execute("UPDATE migration SET state=?1,intent=?2", params![encode(&S::EmployeeProposalsStaged).unwrap(),
            encode(&StageOperation { migration:id,target:S::ContentImported }).unwrap()]).unwrap();
        let mut reopened = Journal::from_connection(journal.db,id).unwrap();
        let mut host = Host {applied:true, fail_after_effect:false,calls:0,evidence:archive_evidence()};
        reopened.run_one(S::ContentArchived,&mut host).unwrap();
        assert_eq!(host.calls,0);
        assert_eq!(reopened.state().unwrap(),S::ContentArchived);
    }
    #[test]
    fn legacy_pending_completion_must_materialize_before_completing() {
        use MigrationState as S;
        let id = RequestId([19;16]);
        let journal = Journal::from_connection(Connection::open_in_memory().unwrap(), id).unwrap();
        journal.db.execute("UPDATE migration SET state=?1,intent=?2", params![encode(&S::AwaitingAdministrator).unwrap(),
            encode(&StageOperation { migration:id,target:S::Complete }).unwrap()]).unwrap();
        put_receipt(&journal,S::ContentImported,&archive_evidence());
        let mut reopened = Journal::from_connection(journal.db,id).unwrap();
        assert!(!reopened.has_pending_intent().unwrap());
        assert_eq!(reopened.run_one(S::Complete,&mut UnavailableHost),Err(Error::Conflict));
        assert_eq!(reopened.state().unwrap(),S::AwaitingAdministrator);
    }
    #[test]
    fn new_materialized_complete_journals_are_never_downgraded() {
        use MigrationState as S;
        let id = RequestId([20;16]);
        let journal = Journal::from_connection(Connection::open_in_memory().unwrap(), id).unwrap();
        journal.db.execute("UPDATE migration SET state=?1", [encode(&S::Complete).unwrap()]).unwrap();
        put_receipt(&journal,S::ContentArchived,&archive_evidence());
        put_receipt(&journal,S::ContentImported,&Evidence::Materialized {all_records_terminal:true});
        put_receipt(&journal,S::Complete,&stages().last().unwrap().1);
        let reopened = Journal::from_connection(journal.db,id).unwrap();
        assert_eq!(reopened.state().unwrap(),S::Complete);
        assert_eq!(receipt_count(&reopened,S::ContentImported),1);
        assert_eq!(receipt_count(&reopened,S::Complete),1);
    }
    #[test]
    fn conflicting_archive_receipts_roll_back_the_entire_upgrade() {
        use MigrationState as S;
        let id = RequestId([21;16]);
        let mut journal = Journal::from_connection(Connection::open_in_memory().unwrap(), id).unwrap();
        journal.db.execute("UPDATE migration SET state=?1", [encode(&S::Complete).unwrap()]).unwrap();
        put_receipt(&journal,S::ContentImported,&archive_evidence());
        put_receipt(&journal,S::Complete,&stages().last().unwrap().1);
        put_receipt(&journal,S::ContentArchived,&Evidence::Content {
            root_only_endpoint:false, source_labels_stamped:true,import_mode_stopped:true });
        assert_eq!(upgrade_archive_receipts(&mut journal.db,id),Err(Error::Conflict));
        assert_eq!(journal.state().unwrap(),S::Complete);
        assert_eq!(receipt_count(&journal,S::ContentImported),1);
        assert_eq!(receipt_count(&journal,S::Complete),1);
    }

}
