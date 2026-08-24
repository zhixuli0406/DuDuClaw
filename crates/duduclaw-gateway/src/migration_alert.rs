//! H3g-b: surface a failed `/data` migration to the dashboard.
//!
//! `duduclaw-data-migrate.service` (H3g, see
//! `crates/duduclaw-core/src/data_migrations.rs`) runs BEFORE the gateway on
//! every boot (`After=`, never `Requires=`, so a failed migration never
//! blocks the box from coming up unattended — see that unit file's own
//! comment) and, on failure, records the details to
//! `<home>/system/migrations.failed.json` via
//! `data_migrations::write_failure_marker`. That marker's own doc comment
//! already says it is "consumed by the gateway at startup (surfaced to the
//! dashboard Activity Feed / audit log)" — until this module, nothing
//! actually did that: the file could sit there indefinitely with no signal
//! reaching an operator short of SSHing in and reading it by hand.
//!
//! This module is that missing consumer. [`check_and_notify`] is called
//! once per gateway boot (`server.rs`, spawned so a slow/failing write can
//! never delay the rest of boot): it reads back any recorded failure via
//! `data_migrations::read_failure` and, if one exists and has not already
//! been surfaced on a PRIOR boot, posts a security audit log entry
//! (`duduclaw_security::audit`, `security_audit.jsonl`) plus an Activity
//! Feed row (`TaskStore::append_activity`, the same dashboard-visible feed
//! `artifact_gate.rs`'s `record_gate_event` and every other `*_notify.rs`
//! module in this crate write to — no new UI surface needed).
//!
//! ## One-time, not repeated on every restart
//!
//! A migration failure marker is durable — it is only ever cleared by a
//! subsequent CLEAN `data_migrate` run (`data_migrations::run_pending`
//! removes it), never by this module. Without dedup, a box stuck in the
//! failed state would re-post the same event on every single gateway
//! restart (crash loop, manual `systemctl restart`, …), flooding the
//! Activity Feed with duplicates of a single unresolved problem. A small
//! marker file (`<home>/system/migrations.failed.notified`, holding the
//! `failed_at_unix` of the failure last surfaced) makes this idempotent:
//! [`check_and_notify`] compares the CURRENT failure's timestamp against
//! that marker and only posts when they differ — a genuinely NEW failure
//! (a later boot's migration run failing again, e.g. on a different script)
//! always gets its own notification, but the same unresolved failure never
//! floods the feed across restarts.
//!
//! Everything here is best-effort telemetry, never control flow: every
//! failure (missing home dir, task-store open error, marker write error)
//! is logged and swallowed. This is a secondary notification path, not the
//! source of truth — an operator can always run `duduclaw data-migrate
//! --check` directly against the same marker file.

use std::path::{Path, PathBuf};

use duduclaw_core::data_migrations::{self, MigrationFailure};
use duduclaw_security::audit::{AuditEvent, Severity, append_audit_event};
use tracing::warn;

use crate::task_store::{ActivityRow, TaskStore};

/// `agent_id` stamped on both the audit event and the Activity Feed row —
/// this is a system-originated event, not attributable to any one AI agent
/// (mirrors `artifact_gate.rs`/`channel_alerts.rs`'s own system-sender
/// sentinels for this exact situation).
const SOURCE_ID: &str = "system";

const NOTIFIED_MARKER_FILENAME: &str = "migrations.failed.notified";

fn notified_marker_path(home: &Path) -> PathBuf {
    home.join("system").join(NOTIFIED_MARKER_FILENAME)
}

/// Which `failed_at_unix` was last surfaced, if any. A missing or corrupt
/// marker reads as "never notified" — fail-open toward RE-notifying a real
/// failure rather than silently going quiet forever because of an
/// unrelated marker-file problem.
fn last_notified_failed_at(home: &Path) -> Option<u64> {
    let raw = std::fs::read_to_string(notified_marker_path(home)).ok()?;
    raw.trim().parse::<u64>().ok()
}

fn write_notified_failed_at(home: &Path, failed_at_unix: u64) {
    let path = notified_marker_path(home);
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            warn!(error = %e, "migration_alert: could not create {} (non-fatal)", parent.display());
            return;
        }
    }
    if let Err(e) = std::fs::write(&path, failed_at_unix.to_string()) {
        warn!(error = %e, "migration_alert: could not write notified marker (non-fatal)");
    }
}

/// Called once per gateway boot. See the module doc comment for the full
/// contract (dedup, best-effort, what gets written where).
pub async fn check_and_notify(home: &Path) {
    let Some(failure) = data_migrations::read_failure(home) else {
        return;
    };
    if last_notified_failed_at(home) == Some(failure.failed_at_unix) {
        return;
    }

    append_audit_event(
        home,
        &AuditEvent::new(
            "data_migration_failed",
            SOURCE_ID,
            Severity::Critical,
            serde_json::json!({
                "script": failure.script,
                "exit_code": failure.exit_code,
                "output_tail": duduclaw_core::truncate_chars(&failure.output_tail, 2000),
                "failed_at_unix": failure.failed_at_unix,
            }),
        ),
    );

    if let Err(e) = post_activity_event(home, &failure).await {
        warn!(error = %e, "migration_alert: activity feed append failed (non-fatal)");
    }

    // Marked regardless of whether the Activity Feed append above
    // succeeded: the audit log entry (the durable record) always lands
    // first and is what matters most, and the Activity Feed is a
    // best-effort secondary surface — matching every other `*_notify.rs`
    // module in this crate, none of which retry a failed append on the
    // next tick either.
    write_notified_failed_at(home, failure.failed_at_unix);
}

async fn post_activity_event(home: &Path, failure: &MigrationFailure) -> Result<(), String> {
    let store = TaskStore::open(home)?;
    let exit_code_text = failure
        .exit_code
        .map(|c| c.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let row = ActivityRow {
        id: uuid::Uuid::new_v4().to_string(),
        event_type: "data_migration_failed".to_string(),
        agent_id: SOURCE_ID.to_string(),
        task_id: None,
        summary: format!(
            "資料遷移腳本「{}」執行失敗（exit code {exit_code_text}），開機流程未中斷但需要人工檢查（{}）。",
            failure.script,
            "duduclaw data-migrate --check"
        ),
        timestamp: chrono::Utc::now().to_rfc3339(),
        metadata: Some(
            serde_json::json!({
                "script": failure.script,
                "exit_code": failure.exit_code,
                "failed_at_unix": failure.failed_at_unix,
            })
            .to_string(),
        ),
    };
    store.append_activity(&row).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_failure(failed_at_unix: u64) -> MigrationFailure {
        MigrationFailure {
            script: "0001-example.sh".to_string(),
            exit_code: Some(1),
            output_tail: "boom".to_string(),
            failed_at_unix,
        }
    }

    #[test]
    fn last_notified_failed_at_is_none_when_no_marker_exists() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(last_notified_failed_at(home.path()), None);
    }

    #[test]
    fn write_then_read_notified_marker_round_trips() {
        let home = tempfile::tempdir().unwrap();
        write_notified_failed_at(home.path(), 12345);
        assert_eq!(last_notified_failed_at(home.path()), Some(12345));
    }

    #[test]
    fn last_notified_failed_at_is_none_on_corrupt_marker() {
        let home = tempfile::tempdir().unwrap();
        let path = notified_marker_path(home.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not-a-number").unwrap();
        assert_eq!(last_notified_failed_at(home.path()), None);
    }

    #[tokio::test]
    async fn check_and_notify_is_a_silent_noop_when_there_is_no_recorded_failure() {
        let home = tempfile::tempdir().unwrap();
        check_and_notify(home.path()).await;
        // No failure recorded ⇒ no marker should ever be written, no audit
        // log file should be created.
        assert_eq!(last_notified_failed_at(home.path()), None);
        assert!(!home.path().join("security_audit.jsonl").exists());
    }

    #[tokio::test]
    async fn check_and_notify_posts_audit_and_activity_then_marks_notified() {
        let home = tempfile::tempdir().unwrap();
        let marker_dir = data_migrations::marker_dir(home.path());
        std::fs::create_dir_all(&marker_dir).unwrap();
        // Write a failure record directly via the same helper `run_pending`
        // uses, so this test exercises the REAL on-disk shape rather than a
        // hand-rolled approximation.
        let failure = sample_failure(1_000_000);
        let path = data_migrations::failure_marker_path(home.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_string(&failure).unwrap()).unwrap();

        check_and_notify(home.path()).await;

        assert_eq!(last_notified_failed_at(home.path()), Some(1_000_000));
        let audit_content = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
        assert!(audit_content.contains("data_migration_failed"));
        assert!(audit_content.contains("0001-example.sh"));

        let store = TaskStore::open(home.path()).unwrap();
        let (rows, _total) = store.list_activity(None, None, 10, 0).await.unwrap();
        assert!(
            rows.iter().any(|r| r.event_type == "data_migration_failed"),
            "expected a data_migration_failed row in the Activity Feed, got: {rows:?}"
        );
    }

    #[tokio::test]
    async fn check_and_notify_does_not_repost_the_same_failure_on_a_second_boot() {
        let home = tempfile::tempdir().unwrap();
        let failure = sample_failure(2_000_000);
        let path = data_migrations::failure_marker_path(home.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_string(&failure).unwrap()).unwrap();

        check_and_notify(home.path()).await; // boot 1: posts
        check_and_notify(home.path()).await; // boot 2 (same unresolved failure): must NOT repost

        let store = TaskStore::open(home.path()).unwrap();
        let (rows, _total) = store.list_activity(None, None, 10, 0).await.unwrap();
        let count = rows
            .iter()
            .filter(|r| r.event_type == "data_migration_failed")
            .count();
        assert_eq!(count, 1, "must post exactly once across repeated boots: {rows:?}");
    }

    #[tokio::test]
    async fn check_and_notify_reposts_a_genuinely_new_failure() {
        let home = tempfile::tempdir().unwrap();
        let path = data_migrations::failure_marker_path(home.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        std::fs::write(&path, serde_json::to_string(&sample_failure(3_000_000)).unwrap()).unwrap();
        check_and_notify(home.path()).await;

        // A later boot's migration run failed again — a DIFFERENT
        // failed_at_unix (even for the same script name) must be treated
        // as new and surfaced again, not swallowed by the old marker.
        std::fs::write(&path, serde_json::to_string(&sample_failure(3_000_500)).unwrap()).unwrap();
        check_and_notify(home.path()).await;

        assert_eq!(last_notified_failed_at(home.path()), Some(3_000_500));
        let store = TaskStore::open(home.path()).unwrap();
        let (rows, _total) = store.list_activity(None, None, 10, 0).await.unwrap();
        let count = rows
            .iter()
            .filter(|r| r.event_type == "data_migration_failed")
            .count();
        assert_eq!(count, 2, "a genuinely new failure must be surfaced again: {rows:?}");
    }
}
