//! Background ingest jobs: a small lease-based queue in `ingest_jobs`.
//!
//! The gateway worker claims the oldest queued job and holds a lease while
//! it works. A job whose lease ran out (the gateway stopped mid-job) is put
//! back in the queue at the next start; after [`MAX_ATTEMPTS`] claims it
//! fails with `retry_exhausted`. An extraction failure is final on the first
//! try: the same bytes fail the same way again.

use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};

use crate::KbResult;

/// Claims before a job whose lease keeps expiring is given up.
pub const MAX_ATTEMPTS: i64 = 3;
/// Default lease.
pub const LEASE: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    Ingest,
    Reindex,
    Embed,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            JobKind::Ingest => "ingest",
            JobKind::Reindex => "reindex",
            JobKind::Embed => "embed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "ingest" => JobKind::Ingest,
            "reindex" => JobKind::Reindex,
            "embed" => JobKind::Embed,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: i64,
    pub source_id: String,
    pub kind: JobKind,
    pub attempts: i64,
}

fn at(offset: Duration) -> String {
    let t =
        chrono::Utc::now() + chrono::Duration::from_std(offset).unwrap_or(chrono::Duration::zero());
    t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Queue a job (inside the caller's transaction).
pub fn enqueue_tx(conn: &Connection, source_id: &str, kind: JobKind) -> KbResult<i64> {
    conn.execute(
        "INSERT INTO ingest_jobs(source_id, kind, state, created_at) VALUES (?1, ?2, 'queued', ?3)",
        params![source_id, kind.as_str(), crate::now_rfc3339()],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Claim the oldest queued job.
pub fn claim_next(conn: &Connection, lease: Duration) -> KbResult<Option<Job>> {
    let row = conn
        .query_row(
            "UPDATE ingest_jobs SET state = 'running', attempts = attempts + 1, lease_until = ?1,
                    started_at = COALESCE(started_at, ?2)
              WHERE id = (SELECT id FROM ingest_jobs WHERE state = 'queued' ORDER BY id LIMIT 1)
              RETURNING id, source_id, kind, attempts",
            params![at(lease), crate::now_rfc3339()],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()?;
    Ok(row.and_then(|(id, source_id, kind, attempts)| {
        JobKind::parse(&kind).map(|kind| Job {
            id,
            source_id,
            kind,
            attempts,
        })
    }))
}

/// Extend a running job's lease.
pub fn renew(conn: &Connection, id: i64, lease: Duration) -> KbResult<()> {
    conn.execute(
        "UPDATE ingest_jobs SET lease_until = ?2 WHERE id = ?1 AND state = 'running'",
        params![id, at(lease)],
    )?;
    Ok(())
}

pub fn set_progress(conn: &Connection, id: i64, done: i64, total: i64) -> KbResult<()> {
    conn.execute(
        "UPDATE ingest_jobs SET progress_done = ?2, progress_total = ?3 WHERE id = ?1",
        params![id, done, total],
    )?;
    Ok(())
}

/// Whether the job is still running (not cancelled by a delete).
pub fn is_running(conn: &Connection, id: i64) -> KbResult<bool> {
    let state: Option<String> = conn
        .query_row(
            "SELECT state FROM ingest_jobs WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(state.as_deref() == Some("running"))
}

pub fn complete(conn: &Connection, id: i64) -> KbResult<()> {
    conn.execute(
        "UPDATE ingest_jobs SET state = 'done', lease_until = NULL, finished_at = ?2 WHERE id = ?1 AND state = 'running'",
        params![id, crate::now_rfc3339()],
    )?;
    Ok(())
}

pub fn fail(conn: &Connection, id: i64, code: &str, message: &str) -> KbResult<()> {
    conn.execute(
        "UPDATE ingest_jobs SET state = 'failed', lease_until = NULL, error_code = ?2, error_message = ?3, finished_at = ?4
          WHERE id = ?1 AND state = 'running'",
        params![id, code, duduclaw_core::truncate_chars(message, 500), crate::now_rfc3339()],
    )?;
    Ok(())
}

/// Requeue running jobs whose lease has expired; jobs out of attempts fail
/// (and so do their sources). Returns `(requeued, exhausted source ids)`.
pub fn requeue_expired(conn: &mut Connection) -> KbResult<(usize, Vec<String>)> {
    let now = crate::now_rfc3339();
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let exhausted: Vec<(i64, String)> = {
        let mut stmt = tx.prepare(
            "SELECT id, source_id FROM ingest_jobs WHERE state = 'running' AND lease_until < ?1 AND attempts >= ?2",
        )?;
        stmt.query_map(params![now, MAX_ATTEMPTS], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?
    };
    let message = "處理這份文件時 DuDuClaw 多次中斷，已停止重試。請刪除後重新上傳，或聯絡管理員。";
    for (id, source) in &exhausted {
        tx.execute(
            "UPDATE ingest_jobs SET state = 'failed', lease_until = NULL, error_code = 'retry_exhausted', error_message = ?2,
                    finished_at = ?3 WHERE id = ?1",
            params![id, message, now],
        )?;
        crate::sources::record_failure(&tx, source, "retry_exhausted", message)?;
    }
    let requeued = tx.execute(
        "UPDATE ingest_jobs SET state = 'queued', lease_until = NULL WHERE state = 'running' AND lease_until < ?1",
        params![now],
    )?;
    tx.commit()?;
    Ok((requeued, exhausted.into_iter().map(|(_, s)| s).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::KnowledgeConfig;
    use crate::sources::{Added, add_source, get_source, test_support::new_source};
    use crate::store::{NewCollection, create_collection};

    fn setup() -> (Connection, String) {
        let mut conn = crate::schema::open_in_memory().unwrap();
        let cfg = KnowledgeConfig::default();
        let c = create_collection(
            &mut conn,
            &NewCollection {
                name: "A".into(),
                ..Default::default()
            },
            "u",
            &cfg,
        )
        .unwrap();
        let Added::New(s) =
            add_source(&mut conn, &new_source(&c.id, &"9".repeat(64)), &cfg).unwrap()
        else {
            panic!()
        };
        (conn, s.id)
    }

    #[test]
    fn claim_complete_cycle() {
        let (conn, sid) = setup();
        let job = claim_next(&conn, LEASE).unwrap().unwrap();
        assert_eq!(job.source_id, sid);
        assert_eq!(job.kind, JobKind::Ingest);
        assert_eq!(job.attempts, 1);
        assert!(
            claim_next(&conn, LEASE).unwrap().is_none(),
            "one job, claimed once"
        );
        assert!(is_running(&conn, job.id).unwrap());
        complete(&conn, job.id).unwrap();
        assert!(!is_running(&conn, job.id).unwrap());
    }

    #[test]
    fn expired_lease_requeues_then_exhausts() {
        let (mut conn, sid) = setup();
        for attempt in 1..=MAX_ATTEMPTS {
            let job = claim_next(&conn, Duration::ZERO).unwrap().unwrap();
            assert_eq!(job.attempts, attempt);
            std::thread::sleep(Duration::from_millis(3));
            let (requeued, exhausted) = requeue_expired(&mut conn).unwrap();
            if attempt < MAX_ATTEMPTS {
                assert_eq!((requeued, exhausted.len()), (1, 0));
            } else {
                assert_eq!((requeued, exhausted.clone()), (0, vec![sid.clone()]));
            }
        }
        assert!(claim_next(&conn, LEASE).unwrap().is_none());
        let s = get_source(&conn, &sid).unwrap().unwrap();
        assert_eq!(s.status, "failed");
        assert_eq!(s.error_code.as_deref(), Some("retry_exhausted"));
    }

    #[test]
    fn failed_job_is_final() {
        let (conn, _) = setup();
        let job = claim_next(&conn, LEASE).unwrap().unwrap();
        fail(&conn, job.id, "extract_timeout", "逾時").unwrap();
        assert!(claim_next(&conn, LEASE).unwrap().is_none());
    }
}
