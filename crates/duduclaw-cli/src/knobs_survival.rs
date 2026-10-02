//! Dedicated goal survival report entry point.

use chrono::{Duration, Utc};
use duduclaw_core::error::{DuDuClawError, Result};
use serde::Serialize;

#[path = "knobs_survival_output.rs"]
mod artifact_output;
use std::path::Path;

#[derive(Serialize)]
struct SurvivalReport {
    generated_at: chrono::DateTime<Utc>,
    window_start: chrono::DateTime<Utc>,
    window_end: chrono::DateTime<Utc>,
    days: u32,
    read_only: bool,
    consistency: &'static str,
    goal_survival: crate::weekly_report::goal_survival::GoalSurvival,
}

pub async fn run(
    home: &Path,
    days: u32,
    agent: Option<&str>,
    output: Option<&Path>,
    format: &str,
) -> Result<()> {
    if !(1..=365).contains(&days) {
        return Err(DuDuClawError::Config(
            "knobs survival --days must be in [1, 365]".into(),
        ));
    }
    if !["markdown", "json"].contains(&format) {
        return Err(DuDuClawError::Config(
            "knobs survival --format must be markdown or json".into(),
        ));
    }
    let output = output
        .map(|path| artifact_output::OutputTarget::prepare(home, path))
        .transpose()?;
    let now = Utc::now();
    let start = now - Duration::days(days as i64);
    let goal_survival =
        crate::weekly_report::goal_survival_readonly::collect(home, &start, &now, agent)
            .map_err(DuDuClawError::Gateway)?;
    let report = SurvivalReport {
        generated_at: now,
        window_start: start,
        window_end: now,
        days,
        read_only: true,
        consistency: "sha_attested_private_copy_single_read_transaction; nonempty_wal_or_journal_is_unavailable",
        goal_survival,
    };
    let rendered = if format == "json" {
        serde_json::to_string_pretty(&report)?
    } else {
        format!("# 旋鈕存活表\n\n唯讀的私人 SQLite 快照，以來源雜湊與檔案識別前後核對；來源有非空 WAL／journal 時回報無法讀取，需先由正常服務 checkpoint。來源不會寫入，僅明確指定的輸出檔會持久寫入。\n\n{}",crate::weekly_report::goal_survival::render_markdown(&report.goal_survival))
    };
    match output {
        Some(target) => target.publish(&rendered)?,
        None => print!("{rendered}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use duduclaw_gateway::task_store::{TaskRow, TaskStore};

    #[cfg(unix)]
    async fn fixture(home: &Path) {
        let store = TaskStore::open(home).expect("store");
        let mut task = TaskRow::new(
            "goal-a".into(),
            "goal".into(),
            String::new(),
            "medium".into(),
            "agnes".into(),
            "test".into(),
        );
        task.goal_mode = true;
        task.status = "done".into();
        store.insert_task(&task).await.expect("task");
        let conn = rusqlite::Connection::open(home.join("tasks.db")).unwrap();
        conn.execute("INSERT INTO task_iterations (task_id,round,dispatched_at,verdict) VALUES ('goal-a',1,?1,'accepted')", [Utc::now().to_rfc3339()]).unwrap();
    }

    fn files(home: &Path) -> Vec<(String, Vec<u8>)> {
        fn visit(root: &Path, path: &Path, out: &mut Vec<(String, Vec<u8>)>) {
            for entry in std::fs::read_dir(path).unwrap() {
                let entry = entry.unwrap().path();
                let relative = entry
                    .strip_prefix(root)
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned();
                if entry.is_dir() {
                    out.push((format!("{relative}/"), Vec::new()));
                    visit(root, &entry, out);
                } else {
                    out.push((relative, std::fs::read(&entry).unwrap()));
                }
            }
        }
        let mut out = Vec::new();
        visit(home, home, &mut out);
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn survival_read_only_command_preserves_database_and_home_bytes() {
        let home = tempfile::tempdir().unwrap();
        let output = tempfile::NamedTempFile::new().unwrap();
        fixture(home.path()).await;
        let before = files(home.path());
        run(home.path(), 7, Some("agnes"), Some(output.path()), "json")
            .await
            .unwrap();
        assert_eq!(
            files(home.path()),
            before,
            "survival may only write its explicit output artifact"
        );
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(output.path()).unwrap()).unwrap();
        assert_eq!(value["goal_survival"]["tasks"], 1);
        assert!(
            value.get("agents").is_none(),
            "dedicated command must not scan unrelated stores"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn survival_reports_unknown_historical_strata_and_strong_labels() {
        let home = tempfile::tempdir().unwrap();
        let output = tempfile::NamedTempFile::new().unwrap();
        fixture(home.path()).await;
        // This represents a pre-receipt database, rather than a newly
        // created task whose trusted ledger correctly records false flags.
        let conn = rusqlite::Connection::open(home.path().join("tasks.db")).unwrap();
        conn.execute_batch("DROP TRIGGER IF EXISTS task_survival_evidence_new_goal; DROP TABLE IF EXISTS task_survival_evidence;").unwrap();
        drop(conn);
        run(home.path(), 7, None, Some(output.path()), "json")
            .await
            .unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(output.path()).unwrap()).unwrap();
        let report = &value["goal_survival"];
        assert_eq!(report["strata"][0]["difficulty"], "unknown");
        assert_eq!(report["strata"][0]["manual_retry"], "unknown");
        assert_eq!(report["human_approval_unknown"], 1);
        assert_eq!(report["human_approved_strata"], serde_json::json!([]));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn survival_missing_or_corrupt_database_is_not_fabricated_empty_history() {
        let home = tempfile::tempdir().unwrap();
        let output = tempfile::NamedTempFile::new().unwrap();
        assert!(run(home.path(), 7, None, Some(output.path()), "json")
            .await
            .is_err());
        assert!(files(home.path()).is_empty());
        std::fs::write(home.path().join("tasks.db"), b"not sqlite").unwrap();
        assert!(run(home.path(), 7, None, Some(output.path()), "json")
            .await
            .is_err());
        assert_eq!(
            files(home.path()),
            vec![("tasks.db".into(), b"not sqlite".to_vec())]
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn survival_legacy_schema_reads_unknown_without_migration() {
        let home = tempfile::tempdir().unwrap();
        let output = tempfile::NamedTempFile::new().unwrap();
        let conn = rusqlite::Connection::open(home.path().join("tasks.db")).unwrap();
        conn.execute_batch("CREATE TABLE tasks(id TEXT,status TEXT,created_at TEXT,assigned_to TEXT,goal_mode INTEGER); CREATE TABLE task_iterations(id INTEGER,task_id TEXT,round INTEGER,verdict TEXT);").unwrap();
        conn.execute(
            "INSERT INTO tasks VALUES('old','done',?1,'agnes',1)",
            [Utc::now().to_rfc3339()],
        )
        .unwrap();
        conn.execute_batch("INSERT INTO task_iterations VALUES(1,'old',1,'accepted')")
            .unwrap();
        drop(conn);
        let before = files(home.path());
        run(home.path(), 7, None, Some(output.path()), "json")
            .await
            .unwrap();
        assert_eq!(files(home.path()), before);
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(output.path()).unwrap()).unwrap();
        assert_eq!(value["goal_survival"]["unknown_knobs"], 1);
        assert_eq!(value["goal_survival"]["strata"][0]["max_retries"], -1);
        assert_eq!(
            value["goal_survival"]["strata"][0]["manual_retry"],
            "unknown"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn survival_receipts_partition_frozen_difficulty_and_strong_approval_subset() {
        let home = tempfile::tempdir().unwrap();
        let output = tempfile::NamedTempFile::new().unwrap();
        fixture(home.path()).await;
        let conn = rusqlite::Connection::open(home.path().join("tasks.db")).unwrap();
        conn.execute_batch("CREATE TABLE IF NOT EXISTS task_survival_evidence(task_id TEXT PRIMARY KEY,difficulty TEXT,manual_retry INTEGER,human_approved INTEGER,evidence_version INTEGER NOT NULL DEFAULT 1,human_approved_iteration_id INTEGER,difficulty_dispatches INTEGER NOT NULL DEFAULT 0);").unwrap();
        // Current task knobs deliberately differ from the sealed history.
        conn.execute_batch("UPDATE tasks SET max_retries=99 WHERE id='goal-a'; UPDATE task_iterations SET knobs_json='{\"max_retries\":3}',gate_inputs_json='{\"goal_difficulty\":\"complex\"}' WHERE task_id='goal-a'; INSERT OR REPLACE INTO task_survival_evidence(task_id,difficulty,manual_retry,human_approved,evidence_version,difficulty_dispatches,human_approved_iteration_id) VALUES('goal-a','complex',1,1,1,(SELECT SUM(dispatch_count) FROM task_iterations WHERE task_id='goal-a'),(SELECT id FROM task_iterations WHERE task_id='goal-a' ORDER BY id DESC LIMIT 1));").unwrap();
        drop(conn);
        let before = files(home.path());
        run(home.path(), 7, None, Some(output.path()), "json")
            .await
            .unwrap();
        assert_eq!(files(home.path()), before);
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(output.path()).unwrap()).unwrap();
        let g = &value["goal_survival"];
        assert_eq!(g["strata"][0]["max_retries"], 3);
        assert_eq!(g["strata"][0]["difficulty"], "complex");
        assert_eq!(g["strata"][0]["manual_retry"], "yes");
        assert_eq!(g["human_approval_unknown"], 0);
        assert_eq!(g["human_approved_strata"][0]["accepted_total"], 1);
        assert!(
            g["human_approved_strata"][0]["rows"][0]["wilson_lo"]
                .as_f64()
                .unwrap()
                > 0.0
        );
        run(home.path(), 7, None, Some(output.path()), "markdown")
            .await
            .unwrap();
        let markdown = std::fs::read_to_string(output.path()).unwrap();
        assert!(markdown.contains("較強成果標籤") && markdown.contains("Wilson"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn survival_busy_wal_writer_is_explicitly_unavailable_without_sidecar_writes() {
        let home = tempfile::tempdir().unwrap();
        let output = tempfile::NamedTempFile::new().unwrap();
        fixture(home.path()).await;
        let conn = rusqlite::Connection::open(home.path().join("tasks.db")).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; UPDATE tasks SET max_retries=4;")
            .unwrap();
        let before = files(home.path());
        let error = run(home.path(), 7, None, Some(output.path()), "json")
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("unfinished WAL/journal"),
            "{error}"
        );
        assert_eq!(files(home.path()), before);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn survival_output_cannot_overwrite_its_database() {
        let home = tempfile::tempdir().unwrap();
        fixture(home.path()).await;
        let before = files(home.path());
        assert!(run(
            home.path(),
            7,
            None,
            Some(&home.path().join("tasks.db")),
            "json"
        )
        .await
        .is_err());
        assert_eq!(files(home.path()), before);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn survival_output_aliases_and_hardlinks_cannot_overwrite_sqlite_sidecars() {
        use std::os::unix::fs::symlink;
        let home = tempfile::tempdir().unwrap();
        let aliases = tempfile::tempdir().unwrap();
        fixture(home.path()).await;
        for name in [
            "tasks.db",
            "tasks.db-wal",
            "tasks.db-shm",
            "tasks.db-journal",
        ] {
            let source = home.path().join(name);
            if name != "tasks.db" {
                std::fs::write(&source, b"sidecar sentinel").unwrap();
            }
            let symbolic = aliases.path().join(format!("symlink-{name}"));
            let hard = aliases.path().join(format!("hardlink-{name}"));
            symlink(&source, &symbolic).unwrap();
            std::fs::hard_link(&source, &hard).unwrap();
            let before = std::fs::read(&source).unwrap();
            for alias in [symbolic, hard] {
                assert!(run(home.path(), 7, None, Some(&alias), "json")
                    .await
                    .is_err());
                assert_eq!(std::fs::read(&source).unwrap(), before);
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn survival_missing_sidecar_output_through_home_alias_is_refused() {
        let home = tempfile::tempdir().unwrap();
        let aliases = tempfile::tempdir().unwrap();
        fixture(home.path()).await;
        std::os::unix::fs::symlink(home.path(), aliases.path().join("home")).unwrap();
        let output = aliases.path().join("home").join("tasks.db-wal");
        let before = files(home.path());
        let error = run(home.path(), 7, None, Some(&output), "json")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("survival output"), "{error}");
        assert_eq!(files(home.path()), before);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn survival_excessive_round_or_iteration_corpus_is_unavailable() {
        let home = tempfile::tempdir().unwrap();
        let output = tempfile::NamedTempFile::new().unwrap();
        fixture(home.path()).await;
        let conn = rusqlite::Connection::open(home.path().join("tasks.db")).unwrap();
        conn.execute_batch("UPDATE task_iterations SET round=10001")
            .unwrap();
        drop(conn);
        let error = run(home.path(), 7, None, Some(output.path()), "json")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("logical round limit"), "{error}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn survival_excessive_in_window_tasks_is_unavailable_instead_of_partial_report() {
        let home = tempfile::tempdir().unwrap();
        let output = tempfile::NamedTempFile::new().unwrap();
        let mut conn = rusqlite::Connection::open(home.path().join("tasks.db")).unwrap();
        conn.execute_batch("CREATE TABLE tasks(id TEXT,status TEXT,created_at TEXT,assigned_to TEXT,goal_mode INTEGER); CREATE TABLE task_iterations(id INTEGER,task_id TEXT,round INTEGER,verdict TEXT);").unwrap();
        let tx = conn.transaction().unwrap();
        for i in 0..5001 {
            tx.execute(
                "INSERT INTO tasks VALUES(?1,'needs_human',?2,'agnes',1)",
                rusqlite::params![format!("goal-{i}"), Utc::now().to_rfc3339()],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        drop(conn);
        let error = run(home.path(), 7, None, Some(output.path()), "json")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("task limit"), "{error}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn survival_legacy_difficulty_without_dispatch_completeness_remains_unknown() {
        let home = tempfile::tempdir().unwrap();
        let output = tempfile::NamedTempFile::new().unwrap();
        fixture(home.path()).await;
        let connection = rusqlite::Connection::open(home.path().join("tasks.db")).unwrap();
        connection.execute_batch("DROP TRIGGER IF EXISTS task_survival_evidence_new_goal; DROP TABLE task_survival_evidence; CREATE TABLE task_survival_evidence(task_id TEXT PRIMARY KEY,difficulty TEXT,manual_retry INTEGER,human_approved INTEGER,evidence_version INTEGER); INSERT INTO task_survival_evidence VALUES('goal-a','simple',NULL,NULL,1); UPDATE task_iterations SET gate_inputs_json='{\"goal_difficulty\":\"simple\"}',knobs_json='{\"max_retries\":3}' WHERE task_id='goal-a';").unwrap();
        drop(connection);
        let before = files(home.path());
        run(home.path(), 7, None, Some(output.path()), "json")
            .await
            .unwrap();
        assert_eq!(files(home.path()), before);
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(output.path()).unwrap()).unwrap();
        assert_eq!(value["goal_survival"]["strata"][0]["difficulty"], "unknown");
    }

    #[cfg(unix)]
    #[test]
    fn survival_output_home_symlink_switch_cannot_turn_an_allowed_artifact_into_the_source_database(
    ) {
        let root = tempfile::tempdir().unwrap();
        let a = root.path().join("a");
        let b = root.path().join("b");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        std::fs::write(a.join("tasks.db"), b"original-home-a").unwrap();
        std::fs::write(b.join("tasks.db"), b"new-source-home-b").unwrap();
        let home = root.path().join("home");
        std::os::unix::fs::symlink(&a, &home).unwrap();
        let target = match artifact_output::OutputTarget::prepare(&home, &b.join("tasks.db")) {
            Ok(target) => target,
            Err(error) => {
                assert!(error.to_string().contains("survival output"), "{error}");
                assert_eq!(
                    std::fs::read(b.join("tasks.db")).unwrap(),
                    b"new-source-home-b"
                );
                return;
            }
        };
        std::fs::remove_file(&home).unwrap();
        std::os::unix::fs::symlink(&b, &home).unwrap();
        let before = std::fs::read(home.join("tasks.db")).unwrap();
        assert!(target
            .publish("must not overwrite the captured history")
            .is_err());
        assert_eq!(std::fs::read(home.join("tasks.db")).unwrap(), before);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn survival_large_out_of_window_history_does_not_mask_one_current_task() {
        let home = tempfile::tempdir().unwrap();
        let output = tempfile::NamedTempFile::new().unwrap();
        let mut conn = rusqlite::Connection::open(home.path().join("tasks.db")).unwrap();
        conn.execute_batch("CREATE TABLE tasks(id TEXT,status TEXT,created_at TEXT,assigned_to TEXT,goal_mode INTEGER); CREATE TABLE task_iterations(id INTEGER,task_id TEXT,round INTEGER,verdict TEXT);").unwrap();
        let tx = conn.transaction().unwrap();
        for i in 0..5001 {
            tx.execute(
                "INSERT INTO tasks VALUES(?1,'done','2000-01-01T00:00:00Z','agnes',1)",
                [format!("old-{i}")],
            )
            .unwrap();
        }
        tx.execute(
            "INSERT INTO tasks VALUES('current','done',?1,'agnes',1)",
            [Utc::now().to_rfc3339()],
        )
        .unwrap();
        tx.execute_batch("INSERT INTO task_iterations VALUES(1,'current',1,'accepted')")
            .unwrap();
        tx.commit().unwrap();
        drop(conn);
        let before = files(home.path());
        run(home.path(), 7, None, Some(output.path()), "json")
            .await
            .unwrap();
        assert_eq!(files(home.path()), before);
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(output.path()).unwrap()).unwrap();
        assert_eq!(value["goal_survival"]["tasks"], 1);
        assert_eq!(value["goal_survival"]["strata"][0]["accepted_total"], 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn survival_real_authorized_channel_receipt_reaches_strong_subset_without_a_judge_acceptance(
    ) {
        use duduclaw_gateway::task_store::{ActivityRow, IterationDispatchLedger};
        let home = tempfile::tempdir().unwrap();
        let output = tempfile::NamedTempFile::new().unwrap();
        let agent_dir = home.path().join("agents/agnes");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(
            agent_dir.join("agent.toml"),
            "[proactive]\nnotify_channel = \"telegram\"\nnotify_chat_id = \"123\"\n",
        )
        .unwrap();
        let store = TaskStore::open(home.path()).unwrap();
        let mut task = TaskRow::new(
            "trusted-goal".into(),
            "goal".into(),
            String::new(),
            "medium".into(),
            "agnes".into(),
            "test".into(),
        );
        task.goal_mode = true;
        task.status = "needs_human".into();
        task.revision_round = 99;
        store.insert_task(&task).await.unwrap();
        let ledger = IterationDispatchLedger {
            gate_inputs_json: Some(serde_json::json!({"goal_difficulty":"complex"}).to_string()),
            ..Default::default()
        };
        store
            .record_iteration_dispatch_with_ledger(
                &task.id,
                3,
                &Utc::now().to_rfc3339(),
                None,
                None,
                &ledger,
            )
            .await
            .unwrap();
        store
            .append_activity(&ActivityRow {
                id: "forged-approval".into(),
                event_type: "goal_loop.human_decision.done".into(),
                agent_id: "agnes".into(),
                task_id: Some(task.id.clone()),
                summary: "Forged human approval".into(),
                timestamp: Utc::now().to_rfc3339(),
                metadata: None,
            })
            .await
            .unwrap();
        assert!(duduclaw_gateway::goal_notify::decide_from_channel(
            home.path(),
            "telegram",
            "attacker",
            "duduclaw:goal_done:trusted-goal"
        )
        .await
        .unwrap()
        .is_err());
        drop(store);
        run(home.path(), 7, None, Some(output.path()), "json")
            .await
            .unwrap();
        let before: serde_json::Value =
            serde_json::from_slice(&std::fs::read(output.path()).unwrap()).unwrap();
        assert_eq!(before["goal_survival"]["human_approved_total"], 0);
        duduclaw_gateway::goal_notify::decide_from_channel(
            home.path(),
            "telegram",
            "123",
            "duduclaw:goal_done:trusted-goal",
        )
        .await
        .unwrap()
        .unwrap();
        let preserved = files(home.path());
        run(home.path(), 7, None, Some(output.path()), "json")
            .await
            .unwrap();
        assert_eq!(files(home.path()), preserved);
        let after: serde_json::Value =
            serde_json::from_slice(&std::fs::read(output.path()).unwrap()).unwrap();
        let g = &after["goal_survival"];
        assert_eq!(g["done_without_round"], 1);
        assert_eq!(g["strata"], serde_json::json!([]));
        assert_eq!(g["human_approved_total"], 1);
        assert_eq!(g["human_approved_strata"][0]["accepted_total"], 1);
        assert_eq!(g["human_approved_strata"][0]["difficulty"], "complex");
        let rows = g["human_approved_strata"][0]["rows"].as_array().unwrap();
        assert_eq!(
            rows.len(),
            3,
            "actual iteration is round 3, editable revision 99 is not approval evidence"
        );
        assert_eq!(rows[0]["accepted_at_or_before_k"], 0);
        assert_eq!(rows[2]["accepted_at_or_before_k"], 1);
    }
    #[cfg(not(unix))]
    #[tokio::test]
    async fn survival_explicit_file_export_reports_unsupported_and_stdout_hint_without_writes() {
        let home = tempfile::tempdir().unwrap();
        let output = tempfile::NamedTempFile::new().unwrap();
        let before = files(home.path());
        let error = run(home.path(), 7, None, Some(output.path()), "json")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("unsupported") && error.to_string().contains("stdout"));
        assert_eq!(files(home.path()), before);
        assert_eq!(std::fs::read(output.path()).unwrap(), Vec::<u8>::new());
    }
}
