//! v1.83 R4: the read-only journal verifier over real journals in a private
//! temporary directory, owned by the test account (production requires root).
use super::*;
use crate::journal::{HostAdapter, Journal, StageOperation};
use std::{fs, os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt}, path::PathBuf};

const ALL_STATES: [MigrationState; 12] = [MigrationState::Planned, MigrationState::OldEnvironmentStopped,
    MigrationState::LegacyQuarantined, MigrationState::KeysRotated, MigrationState::TokenRevocationRecorded,
    MigrationState::EmployeeProposalsStaged, MigrationState::ContentArchived, MigrationState::ConfirmationReady,
    MigrationState::IsolationVerified, MigrationState::AwaitingAdministrator, MigrationState::ContentImported,
    MigrationState::Complete];

fn evidence(target: MigrationState) -> Evidence {
    use MigrationState as S;
    match target {
        S::Planned => unreachable!("no receipt"),
        S::OldEnvironmentStopped => Evidence::Stopped { activation_sources_blocked: true, uid_empty: true },
        S::LegacyQuarantined => Evidence::Quarantined { snapshot_verified: true, fresh_home_empty: true, gateway_denied_legacy: true },
        S::KeysRotated => Evidence::Rotated { root_key: true, identity_key: true, mcp_key: true, jwt_key: true, old_auth_absent: true },
        S::TokenRevocationRecorded => Evidence::Revocations { obligations_recorded: true, reconnect_required: true, cli_tokens_absent: true },
        S::EmployeeProposalsStaged => Evidence::Employees { count: 3, no_resources_allocated: true },
        S::ContentArchived => Evidence::Content { root_only_endpoint: true, source_labels_stamped: true, import_mode_stopped: true },
        S::ConfirmationReady => Evidence::Confirmation { authority_inactive: true, legacy_approvals_expired: true },
        S::IsolationVerified => Evidence::Isolation { ownership: true, quota: true, protocols: true, no_legacy_inodes: true },
        S::AwaitingAdministrator => Evidence::Administrator { new_service_running: true, setup_token_ready: true },
        S::ContentImported => Evidence::Materialized { all_records_terminal: true },
        S::Complete => Evidence::Complete { outstanding_proposals: 0, rotation_obligations: 0, failed_applies: 0,
            unclaimed_personal_owners: 0, isolation_readback: true },
    }
}

/// Completes every stage it is asked for.
struct Done;
impl HostAdapter for Done {
    fn readback(&mut self, operation: StageOperation) -> Result<Option<Evidence>, crate::Error> {
        Ok(Some(evidence(operation.target)))
    }
    fn perform(&mut self, _: StageOperation) -> Result<(), crate::Error> { Ok(()) }
}

/// Records the intent, then dies inside the external effect.
struct Crashes;
impl HostAdapter for Crashes {
    fn readback(&mut self, _: StageOperation) -> Result<Option<Evidence>, crate::Error> { Ok(None) }
    fn perform(&mut self, _: StageOperation) -> Result<(), crate::Error> { Err(crate::Error::Unavailable) }
}

struct Fixture { _tmp: tempfile::TempDir, root: PathBuf, state_dir: PathBuf, legacy: PathBuf, key: PathBuf }

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        Self { state_dir: root.join("state"), legacy: root.join("legacy"), key: root.join("protected").join("root.key"),
            root, _tmp: tmp }
    }
    fn owner() -> u32 { rustix::process::geteuid().as_raw() }
    fn layout(&self) -> ExpectedLayout<'_> {
        ExpectedLayout { state_dir: &self.state_dir, legacy_home: &self.legacy, root_key_path: &self.key }
    }
    fn verify(&self) -> Result<Option<MigrationState>, ReadinessError> { verify_owned_by(&self.layout(), Self::owner()) }
    fn make_state_dir(&self) {
        fs::DirBuilder::new().mode(0o700).create(&self.state_dir).unwrap();
        fs::set_permissions(&self.state_dir, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fn plan(&self, migration: RequestId) -> RootPlan {
        RootPlan { migration, state_dir: self.state_dir.clone(), legacy_home: self.legacy.clone(),
            root_key_path: self.key.clone(), gateway_uid: 1000, gateway_gid: 1000, snapshot_content_limit: 1,
            snapshot_encoded_limit: 1, expires_at_unix: 1 }
    }
    fn write_private(&self, name: &str, bytes: &[u8]) {
        let path = self.state_dir.join(name);
        let mut file = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path).unwrap();
        std::io::Write::write_all(&mut file, bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    fn write_plan(&self, plan: &RootPlan) { self.write_private("plan.json", &serde_json::to_vec(plan).unwrap()); }
    fn journal_path(&self) -> PathBuf { self.state_dir.join(JOURNAL) }
    /// A journal of `migration` advanced through every stage up to `state`.
    fn journal_for(&self, migration: RequestId, state: MigrationState) {
        self.write_private(JOURNAL, b"");
        let mut journal = Journal::from_connection(Connection::open(self.journal_path()).unwrap(), migration).unwrap();
        let mut current = MigrationState::Planned;
        while current != state {
            current = next_stage(current).unwrap();
            journal.run_one(current, &mut Done).unwrap();
        }
    }
    /// The installed layout: state directory, plan and a journal at `state`.
    fn installed(state: MigrationState) -> Self {
        let fixture = Self::new();
        fixture.make_state_dir();
        fixture.write_plan(&fixture.plan(RequestId([7; 16])));
        fixture.journal_for(RequestId([7; 16]), state);
        fixture
    }
    fn rw(&self) -> Connection { Connection::open(self.journal_path()).unwrap() }
}

fn target(state: MigrationState) -> String { serde_json::to_string(&state).unwrap() }

#[test]
fn root_readiness_no_state_dir_or_empty_dir_is_no_migration() {
    let fixture = Fixture::new();
    assert_eq!(fixture.verify(), Ok(None));
    fixture.make_state_dir();
    assert_eq!(fixture.verify(), Ok(None));
}

#[test]
fn root_readiness_artefacts_without_plan_block() {
    for artefact in [JOURNAL, "snapshot.aead", "keys-rotated.json", "journal.db-journal"] {
        let fixture = Fixture::new();
        fixture.make_state_dir();
        fixture.write_private(artefact, b"x");
        assert_eq!(fixture.verify(), Err(ReadinessError::PlanMissing), "{artefact}");
        assert_eq!(ReadinessError::PlanMissing.code(), "migration_plan_missing");
    }
    // A plan whose journal was never created blocks too.
    let fixture = Fixture::new();
    fixture.make_state_dir();
    fixture.write_plan(&fixture.plan(RequestId([7; 16])));
    assert_eq!(fixture.verify(), Err(ReadinessError::JournalMissing));
}

/// Every state with intent NULL and exactly its receipts is reported as is;
/// the caller decides that only AwaitingAdministrator is pending and only
/// Complete passes.
#[test]
fn root_readiness_reports_each_state_with_exact_receipts() {
    for state in ALL_STATES {
        let fixture = Fixture::installed(state);
        assert_eq!(fixture.verify(), Ok(Some(state)), "{state:?}");
    }
}

#[test]
fn root_readiness_pending_intent_blocks_in_every_state() {
    for state in ALL_STATES {
        let Some(next) = next_stage(state) else { continue };
        let fixture = Fixture::installed(state);
        let mut journal = Journal::from_connection(fixture.rw(), RequestId([7; 16])).unwrap();
        assert!(journal.run_one(next, &mut Crashes).is_err());
        assert!(journal.has_pending_intent().unwrap());
        drop(journal);
        assert_eq!(fixture.verify(), Err(ReadinessError::IntentPending), "{state:?}");
    }
}

#[test]
fn root_readiness_missing_extra_forged_or_invalid_receipts_block() {
    use MigrationState as S;
    let mutations: [(&str, fn(&Connection)); 5] = [
        ("missing", |db| { db.execute("DELETE FROM migration_receipts WHERE target=?1", [target(S::KeysRotated)]).unwrap(); }),
        ("forged digest", |db| {
            db.execute("UPDATE migration_receipts SET digest=zeroblob(32) WHERE target=?1", [target(S::IsolationVerified)]).unwrap();
        }),
        ("invalid evidence", |db| {
            let json = serde_json::to_string(&Evidence::Stopped { activation_sources_blocked: true, uid_empty: false }).unwrap();
            db.execute("UPDATE migration_receipts SET evidence=?1, digest=?2 WHERE target=?3",
                rusqlite::params![json, Sha256::digest(json.as_bytes()).as_slice(), target(S::OldEnvironmentStopped)]).unwrap();
        }),
        ("receipt of another stage", |db| {
            let json = serde_json::to_string(&evidence(S::Complete)).unwrap();
            db.execute("UPDATE migration_receipts SET evidence=?1, digest=?2 WHERE target=?3",
                rusqlite::params![json, Sha256::digest(json.as_bytes()).as_slice(), target(S::AwaitingAdministrator)]).unwrap();
        }),
        ("extra future receipt", |db| {
            let json = serde_json::to_string(&evidence(S::ContentImported)).unwrap();
            db.execute("INSERT INTO migration_receipts VALUES(?1,?2,?3)",
                rusqlite::params![target(S::ContentImported), json, Sha256::digest(json.as_bytes()).as_slice()]).unwrap();
        }),
    ];
    for (name, mutate) in mutations {
        let fixture = Fixture::installed(S::AwaitingAdministrator);
        mutate(&fixture.rw());
        assert_eq!(fixture.verify(), Err(ReadinessError::ReceiptsInvalid), "{name}");
    }
}

fn put_receipt(db: &Connection, target_state: MigrationState, evidence: &Evidence) {
    let json = serde_json::to_string(evidence).unwrap();
    db.execute("INSERT INTO migration_receipts VALUES(?1,?2,?3)",
        rusqlite::params![target(target_state), json, Sha256::digest(json.as_bytes()).as_slice()]).unwrap();
}

/// A journal written before ContentArchived existed, at legacy `state`: the
/// archive-only import recorded as ContentImported with `Content` evidence.
fn legacy_journal(state: MigrationState) -> Fixture {
    let fixture = Fixture::installed(MigrationState::EmployeeProposalsStaged);
    let db = fixture.rw();
    let index = LEGACY_ORDER.iter().position(|stage| *stage == state).unwrap();
    for stage in &LEGACY_ORDER[LEGACY_ARCHIVE..=index] {
        let proof = if *stage == MigrationState::ContentImported { evidence(MigrationState::ContentArchived) } else { evidence(*stage) };
        put_receipt(&db, *stage, &proof);
    }
    db.execute("UPDATE migration SET state=?1", [target(state)]).unwrap();
    fixture
}

/// The exact pre-ContentArchived shape has its own code (also with an intent
/// recorded); the verifier never upgrades it, the mutating open does.
#[test]
fn root_readiness_legacy_journal_has_its_own_code_and_is_not_upgraded() {
    use MigrationState as S;
    assert_eq!(ReadinessError::JournalLegacy.code(), "migration_journal_legacy");
    for (state, upgraded) in [(S::ContentImported, S::ContentArchived), (S::ConfirmationReady, S::ConfirmationReady),
        (S::IsolationVerified, S::IsolationVerified), (S::AwaitingAdministrator, S::AwaitingAdministrator),
        (S::Complete, S::AwaitingAdministrator)] {
        let fixture = legacy_journal(state);
        let bytes = fs::read(fixture.journal_path()).unwrap();
        assert_eq!(fixture.verify(), Err(ReadinessError::JournalLegacy), "{state:?}");
        assert_eq!(fs::read(fixture.journal_path()).unwrap(), bytes, "{state:?}: the verifier wrote");
        // The mutating open (the R5 resume step) brings it up to date.
        drop(Journal::from_connection(fixture.rw(), RequestId([7; 16])).unwrap());
        assert_eq!(fixture.verify(), Ok(Some(upgraded)), "{state:?}");
    }
    let fixture = legacy_journal(S::ConfirmationReady);
    let operation = StageOperation { migration: RequestId([7; 16]), target: S::IsolationVerified };
    fixture.rw().execute("UPDATE migration SET intent=?1", [serde_json::to_string(&operation).unwrap()]).unwrap();
    assert_eq!(fixture.verify(), Err(ReadinessError::JournalLegacy));
}

/// Only the exact legacy shape is `JournalLegacy`; any other mismatch around
/// it stays `ReceiptsInvalid`.
#[test]
fn root_readiness_near_legacy_journals_are_receipts_invalid() {
    use MigrationState as S;
    let mutations: [(&str, fn(&Connection)); 5] = [
        ("legacy archive receipt invalid", |db| {
            let json = serde_json::to_string(&Evidence::Content { root_only_endpoint: false, source_labels_stamped: true,
                import_mode_stopped: true }).unwrap();
            db.execute("UPDATE migration_receipts SET evidence=?1, digest=?2 WHERE target=?3",
                rusqlite::params![json, Sha256::digest(json.as_bytes()).as_slice(), target(S::ContentImported)]).unwrap();
        }),
        ("legacy archive digest forged", |db| {
            db.execute("UPDATE migration_receipts SET digest=zeroblob(32) WHERE target=?1", [target(S::ContentImported)]).unwrap();
        }),
        ("earlier receipt missing", |db| {
            db.execute("DELETE FROM migration_receipts WHERE target=?1", [target(S::KeysRotated)]).unwrap();
        }),
        ("both archive receipts", |db| put_receipt(db, S::ContentArchived, &evidence(S::ContentArchived))),
        ("extra future receipt", |db| put_receipt(db, S::Complete, &evidence(S::Complete))),
    ];
    for (name, mutate) in mutations {
        let fixture = legacy_journal(S::AwaitingAdministrator);
        mutate(&fixture.rw());
        assert_eq!(fixture.verify(), Err(ReadinessError::ReceiptsInvalid), "{name}");
    }
    // A legacy-shaped receipt set under a state before the archive stage.
    let fixture = legacy_journal(S::ContentImported);
    fixture.rw().execute("UPDATE migration SET state=?1", [target(S::EmployeeProposalsStaged)]).unwrap();
    assert_eq!(fixture.verify(), Err(ReadinessError::ReceiptsInvalid));
}

/// KNOWN GAP (R5): receipts hash only their evidence, so the receipts of
/// migration A copied into the journal of migration B verify. This test
/// fails until R5 binds receipts to the migration id.
#[test]
#[ignore = "known gap: receipts are not bound to the migration id; fixed with the R5 receipt format (see PLAN §4.5)"]
fn root_readiness_receipts_replayed_from_another_migration_are_refused() {
    let source = Fixture::installed(MigrationState::Complete);
    let rows: Vec<(String, String, Vec<u8>)> = {
        let db = source.rw();
        let mut statement = db.prepare("SELECT target, evidence, digest FROM migration_receipts").unwrap();
        let rows: Vec<(String, String, Vec<u8>)> = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap()
            .collect::<Result<Vec<_>, _>>().unwrap();
        rows
    };
    let victim = Fixture::new();
    victim.make_state_dir();
    victim.write_plan(&victim.plan(RequestId([8; 16])));
    victim.journal_for(RequestId([8; 16]), MigrationState::Planned);
    let db = victim.rw();
    for (target_text, evidence_text, digest) in &rows {
        db.execute("INSERT INTO migration_receipts VALUES(?1,?2,?3)", rusqlite::params![target_text, evidence_text, digest]).unwrap();
    }
    db.execute("UPDATE migration SET state=?1", [target(MigrationState::Complete)]).unwrap();
    drop(db);
    assert!(victim.verify().is_err(), "receipts of migration A must not complete migration B");
}

#[test]
fn root_readiness_sidecars_block() {
    for sidecar in SIDECARS {
        let fixture = Fixture::installed(MigrationState::Complete);
        fixture.write_private(sidecar, b"");
        assert_eq!(fixture.verify(), Err(ReadinessError::JournalSidecar), "{sidecar}");
        // A dangling link is a sidecar too.
        fs::remove_file(fixture.state_dir.join(sidecar)).unwrap();
        std::os::unix::fs::symlink(fixture.root.join("missing"), fixture.state_dir.join(sidecar)).unwrap();
        assert_eq!(fixture.verify(), Err(ReadinessError::JournalSidecar), "{sidecar} link");
    }
}

#[test]
fn root_readiness_oversized_mode_link_and_owner_rejected() {
    let fixture = Fixture::installed(MigrationState::Complete);
    fs::OpenOptions::new().write(true).open(fixture.journal_path()).unwrap().set_len(MAX_JOURNAL_BYTES + 1).unwrap();
    assert_eq!(fixture.verify(), Err(ReadinessError::JournalOversized));

    let fixture = Fixture::installed(MigrationState::Complete);
    fs::set_permissions(fixture.journal_path(), fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(fixture.verify(), Err(ReadinessError::JournalUnsafe));

    let fixture = Fixture::installed(MigrationState::Complete);
    fs::hard_link(fixture.journal_path(), fixture.root.join("second-name")).unwrap();
    assert_eq!(fixture.verify(), Err(ReadinessError::JournalUnsafe));

    let fixture = Fixture::installed(MigrationState::Complete);
    fs::rename(fixture.journal_path(), fixture.root.join("elsewhere.db")).unwrap();
    std::os::unix::fs::symlink(fixture.root.join("elsewhere.db"), fixture.journal_path()).unwrap();
    assert_eq!(fixture.verify(), Err(ReadinessError::JournalUnsafe));

    // Wrong owner: the directory, and the journal descriptor itself.
    let fixture = Fixture::installed(MigrationState::Complete);
    let other = Fixture::owner().wrapping_add(1);
    assert_eq!(verify_owned_by(&fixture.layout(), other), Err(ReadinessError::StateDirUnsafe));
    let fd = rfs::open(fixture.journal_path().as_path(), OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::empty()).unwrap();
    assert_eq!(journal_stat(&fd, other), Err(ReadinessError::JournalUnsafe));
    assert!(journal_stat(&fd, Fixture::owner()).is_ok());

    // The state directory must be private.
    let fixture = Fixture::installed(MigrationState::Complete);
    fs::set_permissions(&fixture.state_dir, fs::Permissions::from_mode(0o750)).unwrap();
    assert_eq!(fixture.verify(), Err(ReadinessError::StateDirUnsafe));
}

#[test]
fn root_readiness_schema_plan_and_migration_mismatch_block() {
    for extra in ["CREATE TABLE extra(x)", "CREATE INDEX extra ON migration(state)",
        "CREATE TRIGGER extra AFTER UPDATE ON migration BEGIN SELECT 1; END",
        "CREATE VIEW extra AS SELECT state FROM migration"] {
        let fixture = Fixture::installed(MigrationState::Complete);
        fixture.rw().execute_batch(extra).unwrap();
        assert_eq!(fixture.verify(), Err(ReadinessError::JournalSchema), "{extra}");
    }
    // A plan naming another installation.
    let fixture = Fixture::new();
    fixture.make_state_dir();
    let mut plan = fixture.plan(RequestId([7; 16]));
    plan.legacy_home = fixture.root.join("other-home");
    fixture.write_plan(&plan);
    fixture.journal_for(RequestId([7; 16]), MigrationState::Complete);
    assert_eq!(fixture.verify(), Err(ReadinessError::PlanInvalid));
    // A plan file readable by others, or not JSON.
    let fixture = Fixture::installed(MigrationState::Complete);
    fs::set_permissions(fixture.state_dir.join("plan.json"), fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(fixture.verify(), Err(ReadinessError::PlanInvalid));
    // The journal of another migration.
    let fixture = Fixture::new();
    fixture.make_state_dir();
    fixture.write_plan(&fixture.plan(RequestId([7; 16])));
    fixture.journal_for(RequestId([8; 16]), MigrationState::Complete);
    assert_eq!(fixture.verify(), Err(ReadinessError::JournalConflict));
    // Not a database at all.
    let fixture = Fixture::new();
    fixture.make_state_dir();
    fixture.write_plan(&fixture.plan(RequestId([7; 16])));
    fixture.write_private(JOURNAL, b"not a database, just bytes long enough to be read as a header....");
    assert_eq!(fixture.verify(), Err(ReadinessError::JournalUnreadable));
}

#[test]
fn root_readiness_reason_codes_are_stable_ascii() {
    use ReadinessError as E;
    let all = [E::Unavailable, E::StateDirUnsafe, E::PlanMissing, E::PlanInvalid, E::JournalMissing, E::JournalUnsafe,
        E::JournalOversized, E::JournalSidecar, E::JournalChanged, E::JournalSchema, E::JournalUnreadable,
        E::JournalConflict, E::IntentPending, E::ReceiptsInvalid, E::JournalLegacy];
    let mut codes: Vec<_> = all.iter().map(|error| error.code()).collect();
    for code in &codes {
        assert!(code.starts_with("migration_") && code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'), "{code}");
    }
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(codes.len(), all.len());
}
