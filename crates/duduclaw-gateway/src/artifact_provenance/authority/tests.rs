use super::*;
use crate::artifacts::{ArtifactOrigin, HistoryIndex, SaveContext};

fn row(name: &str, source: &str, owner: &str) -> ArtifactRecord {
    ArtifactRecord {
        content_hash: Some("a".repeat(64)), owner_user_id: Some(owner.into()),
        produced_at: "2026-11-05T00:00:00Z".into(), agent_id: "sales".into(),
        archived_name: name.into(), display_name: "report.txt".into(), size: 4,
        origin: ArtifactOrigin::Declared, task_id: None, round: None,
        channel: Some("webchat".into()), source_path: Some(source.into()), evidence: None,
    }
}

#[test]
fn durable_binding_survives_audit_tail_truncation_and_rotation() {
    let home = crate::fs_safe::private_test_home().unwrap();
    let record = row("old.txt", "/work/report.txt", "alice");
    crate::artifacts::append_rows_durable(home.path(), &[record.to_json()]).unwrap();
    let audit = home.path().join(ARTIFACTS_FILE);
    std::fs::OpenOptions::new().write(true).open(&audit).unwrap()
        .set_len(crate::artifacts::ROTATION_MAX_BYTES + 1).unwrap();
    crate::artifacts::append_rows_durable(home.path(), &[row("new.txt", "/work/new.txt", "bob").to_json()]).unwrap();
    assert!(audit.with_extension("jsonl.1").exists());
    std::fs::remove_file(audit.with_extension("jsonl.1")).unwrap();
    std::fs::write(&audit, b"invalid audit tail").unwrap();
    let index = HistoryIndex::load(home.path(), Some("sales"));
    let found = index.provenance("old.txt").unwrap();
    assert_eq!(found.owner_user_id.as_deref(), Some("alice"));
    assert_eq!(found.content_hash, record.content_hash);
    assert_eq!(found.size, 4);
    assert_eq!(index.archive_for_source(Path::new("/work/report.txt")).as_deref(), Some("old.txt"));
    assert!(HistoryIndex::load(home.path(), Some("other")).provenance("old.txt").is_none());
}

#[test]
fn migration_reads_rotated_and_current_rows_once_and_preserves_ambiguity() {
    let home = crate::fs_safe::private_test_home().unwrap();
    std::fs::write(home.path().join("artifacts.jsonl.1"), format!("{}\n", row("old.txt", "/work/report.txt", "alice").to_json())).unwrap();
    std::fs::write(home.path().join(ARTIFACTS_FILE), format!("{}\n", row("new.txt", "/work/report.txt", "alice").to_json())).unwrap();
    let index = HistoryIndex::load(home.path(), Some("sales"));
    assert!(index.provenance("old.txt").is_some());
    assert!(index.archive_for_source(Path::new("/work/report.txt")).is_none());
    std::fs::write(home.path().join(ARTIFACTS_FILE), format!("{}\n", row("old.txt", "/work/report.txt", "mallory").to_json())).unwrap();
    let index = HistoryIndex::load(home.path(), Some("sales"));
    assert_eq!(index.provenance("old.txt").unwrap().owner_user_id.as_deref(), Some("alice"));
}

#[test]
fn malformed_or_oversized_migration_rolls_back_all_authority_rows() {
    for oversized in [false, true] {
        let home = crate::fs_safe::private_test_home().unwrap();
        let mut bytes = format!("{}\n", row("old.txt", "/work/report.txt", "alice").to_json()).into_bytes();
        bytes.extend(if oversized { vec![b'x'; ROW_MAX_BYTES as usize + 1] } else { b"malformed\n".to_vec() });
        std::fs::write(home.path().join(ARTIFACTS_FILE), bytes).unwrap();
        assert!(open(home.path()).is_err());
        let connection = Connection::open(home.path().join(DB_FILE)).unwrap();
        let count: i64 = connection.query_row("SELECT count(*) FROM artifact_provenance", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 0);
        assert!(HistoryIndex::load(home.path(), Some("sales")).provenance_checked("old.txt").is_err());
    }
}

#[test]
fn lost_database_insert_is_reported_and_never_appended_to_audit() {
    let home = crate::fs_safe::private_test_home().unwrap();
    let connection = open(home.path()).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_artifact BEFORE INSERT ON artifact_provenance BEGIN SELECT RAISE(ABORT,'test failure'); END;").unwrap();
    let base = home.path().join("agents/sales");
    let result = crate::artifacts::record_saved_bytes(&base, &base.join("attachments/report.txt"), "report.txt", b"data", &SaveContext::uploaded(Some("webchat")));
    assert!(result.is_err());
    assert!(!home.path().join(ARTIFACTS_FILE).exists());
    assert!(HistoryIndex::load(home.path(), Some("sales")).provenance("report.txt").is_none());
}

#[test]
fn backfill_cannot_replace_a_content_binding_after_audit_rotation() {
    let home = crate::fs_safe::private_test_home().unwrap();
    let original = row("old.txt", "/work/report.txt", "alice");
    write(home.path(), &[original.to_json()], true).unwrap();
    let mut inferred = original.clone();
    inferred.content_hash = None;
    inferred.owner_user_id = None;
    inferred.source_path = None;
    inferred.origin = ArtifactOrigin::Unknown;
    write(home.path(), &[inferred.to_json()], false).unwrap();
    assert_eq!(get(&open(home.path()).unwrap(), "sales", "old.txt").unwrap(), Some(original));
}

#[test]
fn indexed_lookup_does_not_drop_old_bindings_after_many_new_rows() {
    let home = crate::fs_safe::private_test_home().unwrap();
    let records: Vec<_> = (0..10_000).map(|i| row(&format!("{i}.txt"), &format!("/work/{i}.txt"), "alice").to_json()).collect();
    write(home.path(), &records, true).unwrap();
    let index = HistoryIndex::load(home.path(), Some("sales"));
    assert!(index.provenance("0.txt").is_some());
    assert_eq!(index.archive_for_source(Path::new("/work/0.txt")).as_deref(), Some("0.txt"));
}

#[test]
fn migration_refuses_oversized_files_without_partial_import() {
    let home = crate::fs_safe::private_test_home().unwrap();
    std::fs::write(home.path().join("artifacts.jsonl.1"), format!("{}\n", row("old.txt", "/work/report.txt", "alice").to_json())).unwrap();
    std::fs::File::create(home.path().join(ARTIFACTS_FILE)).unwrap().set_len(MIGRATION_MAX_BYTES + 1).unwrap();
    assert!(open(home.path()).is_err());
    let connection = Connection::open(home.path().join(DB_FILE)).unwrap();
    assert_eq!(connection.query_row("SELECT count(*) FROM artifact_provenance", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
}

#[test]
fn query_failure_is_not_treated_as_an_unbound_public_file() {
    let home = crate::fs_safe::private_test_home().unwrap();
    write(home.path(), &[row("old.txt", "/work/report.txt", "alice").to_json()], true).unwrap();
    let index = HistoryIndex::load(home.path(), Some("sales"));
    Connection::open(home.path().join(DB_FILE)).unwrap().execute("DROP TABLE artifact_provenance", []).unwrap();
    assert!(index.provenance_checked("old.txt").is_err());
}

#[cfg(unix)]
#[test]
fn durable_authority_refuses_symlink_database_and_creates_private_mode() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let home = crate::fs_safe::private_test_home().unwrap();
    let home = home.path().canonicalize().unwrap();
    let outside = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(outside.path(), b"untouched").unwrap();
    symlink(outside.path(), home.join(DB_FILE)).unwrap();
    assert!(open(&home).is_err());
    assert_eq!(std::fs::read(outside.path()).unwrap(), b"untouched");
    std::fs::remove_file(home.join(DB_FILE)).unwrap();
    open(&home).unwrap();
    assert_eq!(std::fs::metadata(home.join(DB_FILE)).unwrap().permissions().mode() & 0o777, 0o600);
}
