//! SQLite-backed reservation store for restaurant (and similar) agents.
//!
//! Provides CRUD operations and conflict detection for time-slot reservations.

use std::path::Path;

use chrono::{Duration, NaiveDate, NaiveTime, Timelike, Utc};
use duduclaw_core::error::{DuDuClawError, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

// ── Data types ───────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Reservation {
    pub id: String,
    pub agent_id: String,
    pub customer_name: String,
    pub phone: String,
    pub date: NaiveDate,
    pub time: NaiveTime,
    pub party_size: u32,
    pub status: ReservationStatus,
    pub notes: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ReservationStatus {
    Pending,
    Confirmed,
    Cancelled,
    Completed,
}

impl ReservationStatus {
    /// Serialize to a lowercase string for SQLite storage.
    fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Confirmed => "confirmed",
            Self::Cancelled => "cancelled",
            Self::Completed => "completed",
        }
    }

    /// Deserialize from a string stored in SQLite.
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "pending" => Ok(Self::Pending),
            "confirmed" => Ok(Self::Confirmed),
            "cancelled" => Ok(Self::Cancelled),
            "completed" => Ok(Self::Completed),
            other => Err(DuDuClawError::Agent(format!(
                "Unknown reservation status: {other}"
            ))),
        }
    }
}

// ── Store ────────────────────────────────────────────────────

/// SQLite-backed reservation store.
///
/// Each agent can have its own database file, or share one with
/// `agent_id` partitioning.
///
/// The `conn` is wrapped in a `tokio::sync::Mutex` so that it is safe to
/// share across async tasks without blocking the executor thread.
pub struct ReservationStore {
    conn: Mutex<Connection>,
}

impl ReservationStore {
    /// Open (or create) a reservation database at `db_path`.
    pub fn new(db_path: &Path) -> Result<Self> {
        // Ensure parent directory exists
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                DuDuClawError::Agent(format!(
                    "Failed to create database directory {}: {e}",
                    parent.display()
                ))
            })?;
        }

        let conn = Connection::open(db_path).map_err(|e| {
            DuDuClawError::Agent(format!("Failed to open reservation DB: {e}"))
        })?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS reservations (
                id           TEXT PRIMARY KEY,
                agent_id     TEXT NOT NULL,
                customer_name TEXT NOT NULL,
                phone        TEXT NOT NULL,
                date         TEXT NOT NULL,
                time         TEXT NOT NULL,
                party_size   INTEGER NOT NULL,
                status       TEXT NOT NULL DEFAULT 'pending',
                notes        TEXT NOT NULL DEFAULT '',
                created_at   TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_reservations_agent_date
                ON reservations (agent_id, date);",
        )
        .map_err(|e| {
            DuDuClawError::Agent(format!("Failed to initialize reservations table: {e}"))
        })?;

        Ok(Self { conn: Mutex::new(conn) })
    }

    /// Insert a new reservation.
    pub async fn create(&self, res: &Reservation) -> Result<()> {
        let conn = self.conn.lock().await;
        conn
            .execute(
                "INSERT INTO reservations
                    (id, agent_id, customer_name, phone, date, time, party_size, status, notes, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    res.id,
                    res.agent_id,
                    res.customer_name,
                    res.phone,
                    res.date.format("%Y-%m-%d").to_string(),
                    res.time.format("%H:%M").to_string(),
                    res.party_size,
                    res.status.as_str(),
                    res.notes,
                    res.created_at,
                ],
            )
            .map_err(|e| {
                DuDuClawError::Agent(format!("Failed to create reservation: {e}"))
            })?;

        Ok(())
    }

    /// List all reservations for a given agent on a specific date.
    ///
    /// Returns reservations sorted by time ascending.
    pub async fn list_by_date(
        &self,
        agent_id: &str,
        date: NaiveDate,
    ) -> Result<Vec<Reservation>> {
        let conn = self.conn.lock().await;
        let date_str = date.format("%Y-%m-%d").to_string();

        let mut stmt = conn
            .prepare(
                "SELECT id, agent_id, customer_name, phone, date, time,
                        party_size, status, notes, created_at
                 FROM reservations
                 WHERE agent_id = ?1 AND date = ?2
                 ORDER BY time ASC",
            )
            .map_err(|e| {
                DuDuClawError::Agent(format!("Failed to prepare query: {e}"))
            })?;

        let rows = stmt
            .query_map(params![agent_id, date_str], |row| {
                Ok(ReservationRow {
                    id: row.get(0)?,
                    agent_id: row.get(1)?,
                    customer_name: row.get(2)?,
                    phone: row.get(3)?,
                    date: row.get(4)?,
                    time: row.get(5)?,
                    party_size: row.get(6)?,
                    status: row.get(7)?,
                    notes: row.get(8)?,
                    created_at: row.get(9)?,
                })
            })
            .map_err(|e| {
                DuDuClawError::Agent(format!("Failed to query reservations: {e}"))
            })?;

        let mut reservations = Vec::new();
        for row_result in rows {
            let row = row_result.map_err(|e| {
                DuDuClawError::Agent(format!("Failed to read reservation row: {e}"))
            })?;
            reservations.push(row_to_reservation(row)?);
        }

        Ok(reservations)
    }

    /// Update the status of a reservation by ID.
    pub async fn update_status(&self, id: &str, status: ReservationStatus) -> Result<()> {
        let conn = self.conn.lock().await;
        let affected = conn
            .execute(
                "UPDATE reservations SET status = ?1 WHERE id = ?2",
                params![status.as_str(), id],
            )
            .map_err(|e| {
                DuDuClawError::Agent(format!("Failed to update reservation status: {e}"))
            })?;

        if affected == 0 {
            return Err(DuDuClawError::Agent(format!(
                "Reservation not found: {id}"
            )));
        }

        Ok(())
    }

    /// Delete old non-confirmed reservations older than `days` days.
    ///
    /// This prevents unbounded PII accumulation for pending/cancelled entries.
    /// Confirmed and completed reservations are retained for audit purposes.
    pub async fn cleanup_old(&self, days: i64) -> Result<usize> {
        let conn = self.conn.lock().await;
        let cutoff = (chrono::Utc::now() - chrono::Duration::days(days))
            .format("%Y-%m-%d")
            .to_string();
        let deleted = conn
            .execute(
                "DELETE FROM reservations WHERE date < ?1 AND status NOT IN ('confirmed', 'completed')",
                rusqlite::params![cutoff],
            )
            .map_err(|e| DuDuClawError::Agent(format!("Failed to cleanup old reservations: {e}")))?;
        Ok(deleted)
    }

    /// Check for conflicting reservations within a time window.
    ///
    /// A conflict exists when an existing reservation's time falls within
    /// `[time, time + duration_min)` or the existing reservation's window
    /// overlaps the requested slot. Only non-cancelled reservations are
    /// considered.
    ///
    /// For simplicity, we assume each reservation occupies `duration_min`
    /// minutes starting from its `time` field.
    pub async fn check_conflicts(
        &self,
        agent_id: &str,
        date: NaiveDate,
        time: NaiveTime,
        duration_min: u32,
    ) -> Result<Vec<Reservation>> {
        let conn = self.conn.lock().await;
        let date_str = date.format("%Y-%m-%d").to_string();
        let req_start = time.format("%H:%M").to_string();

        // Calculate the end time of the requested slot in Rust (no SQL string concat)
        let total_minutes =
            time.hour() * 60 + time.minute() + duration_min;
        if total_minutes >= 24 * 60 {
            return Err(DuDuClawError::Agent(
                "Reservation cannot span past midnight".into(),
            ));
        }
        let req_end = NaiveTime::from_hms_opt(total_minutes / 60, total_minutes % 60, 0)
            .ok_or_else(|| DuDuClawError::Agent("Invalid end time".into()))?;
        let req_end_str = req_end.format("%H:%M").to_string();

        // Pre-compute the SQLite time modifier string in Rust to avoid
        // SQL string concatenation (security: prevents injection via duration_min).
        let time_modifier = format!("+{duration_min} minutes");

        // An existing reservation at `existing_time` with the same duration
        // conflicts if:
        //   existing_start < req_end AND req_start < existing_end
        // where existing_end = existing_time + duration_min
        //
        // Since SQLite stores time as text in HH:MM format, lexicographic
        // comparison works correctly for 24h time strings.
        let mut stmt = conn
            .prepare(
                "SELECT id, agent_id, customer_name, phone, date, time,
                        party_size, status, notes, created_at
                 FROM reservations
                 WHERE agent_id = ?1
                   AND date = ?2
                   AND status != 'cancelled'
                   AND time < ?3
                   AND time(time, ?4) > ?5",
            )
            .map_err(|e| {
                DuDuClawError::Agent(format!("Failed to prepare conflict query: {e}"))
            })?;

        let rows = stmt
            .query_map(
                params![
                    agent_id,
                    date_str,
                    req_end_str,
                    time_modifier,
                    req_start,
                ],
                |row| {
                    Ok(ReservationRow {
                        id: row.get(0)?,
                        agent_id: row.get(1)?,
                        customer_name: row.get(2)?,
                        phone: row.get(3)?,
                        date: row.get(4)?,
                        time: row.get(5)?,
                        party_size: row.get(6)?,
                        status: row.get(7)?,
                        notes: row.get(8)?,
                        created_at: row.get(9)?,
                    })
                },
            )
            .map_err(|e| {
                DuDuClawError::Agent(format!("Failed to query conflicts: {e}"))
            })?;

        let mut conflicts = Vec::new();
        for row_result in rows {
            let row = row_result.map_err(|e| {
                DuDuClawError::Agent(format!("Failed to read conflict row: {e}"))
            })?;
            conflicts.push(row_to_reservation(row)?);
        }

        Ok(conflicts)
    }
}

// ── Internal helpers ─────────────────────────────────────────

/// Raw row from SQLite (all strings) before parsing into typed `Reservation`.
struct ReservationRow {
    id: String,
    agent_id: String,
    customer_name: String,
    phone: String,
    date: String,
    time: String,
    party_size: u32,
    status: String,
    notes: String,
    created_at: String,
}

/// Convert a raw SQLite row into a typed `Reservation`.
fn row_to_reservation(row: ReservationRow) -> Result<Reservation> {
    let date = NaiveDate::parse_from_str(&row.date, "%Y-%m-%d").map_err(|e| {
        DuDuClawError::Agent(format!("Invalid date '{}': {e}", row.date))
    })?;
    let time = NaiveTime::parse_from_str(&row.time, "%H:%M").map_err(|e| {
        DuDuClawError::Agent(format!("Invalid time '{}': {e}", row.time))
    })?;
    let status = ReservationStatus::from_str(&row.status)?;

    Ok(Reservation {
        id: row.id,
        agent_id: row.agent_id,
        customer_name: row.customer_name,
        phone: row.phone,
        date,
        time,
        party_size: row.party_size,
        status,
        notes: row.notes,
        created_at: row.created_at,
    })
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_store() -> (TempDir, ReservationStore) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("reservations.db");
        let store = ReservationStore::new(&db_path).unwrap();
        (tmp, store)
    }

    fn sample_reservation(id: &str, time_str: &str) -> Reservation {
        Reservation {
            id: id.to_string(),
            agent_id: "test-agent".to_string(),
            customer_name: "Alice".to_string(),
            phone: "0912345678".to_string(),
            date: NaiveDate::from_ymd_opt(2026, 4, 1).unwrap(),
            time: NaiveTime::parse_from_str(time_str, "%H:%M").unwrap(),
            party_size: 4,
            status: ReservationStatus::Pending,
            notes: String::new(),
            created_at: "2026-03-31T10:00:00Z".to_string(),
        }
    }

    #[tokio::test]
    async fn test_create_and_list() {
        let (_tmp, store) = make_store();
        let res = sample_reservation("r1", "18:00");

        store.create(&res).await.unwrap();

        let date = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();
        let list = store.list_by_date("test-agent", date).await.unwrap();

        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "r1");
        assert_eq!(list[0].customer_name, "Alice");
        assert_eq!(list[0].party_size, 4);
    }

    #[tokio::test]
    async fn test_list_empty_date() {
        let (_tmp, store) = make_store();
        let res = sample_reservation("r1", "18:00");
        store.create(&res).await.unwrap();

        // Different date should return empty
        let other_date = NaiveDate::from_ymd_opt(2026, 4, 2).unwrap();
        let list = store.list_by_date("test-agent", other_date).await.unwrap();
        assert!(list.is_empty());
    }

    #[tokio::test]
    async fn test_list_different_agent() {
        let (_tmp, store) = make_store();
        let res = sample_reservation("r1", "18:00");
        store.create(&res).await.unwrap();

        let date = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();
        let list = store.list_by_date("other-agent", date).await.unwrap();
        assert!(list.is_empty());
    }

    #[tokio::test]
    async fn test_update_status() {
        let (_tmp, store) = make_store();
        let res = sample_reservation("r1", "18:00");
        store.create(&res).await.unwrap();

        store
            .update_status("r1", ReservationStatus::Confirmed)
            .await
            .unwrap();

        let date = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();
        let list = store.list_by_date("test-agent", date).await.unwrap();
        assert_eq!(list[0].status, ReservationStatus::Confirmed);
    }

    #[tokio::test]
    async fn test_update_status_not_found() {
        let (_tmp, store) = make_store();
        let result = store.update_status("nonexistent", ReservationStatus::Cancelled).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_conflict_overlapping() {
        let (_tmp, store) = make_store();

        // Existing reservation at 18:00 (duration 90 min => ends 19:30)
        let existing = sample_reservation("r1", "18:00");
        store.create(&existing).await.unwrap();

        // Check for conflict at 19:00 with 90 min duration
        // 19:00 is within [18:00, 19:30) of existing
        let date = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();
        let time = NaiveTime::from_hms_opt(19, 0, 0).unwrap();
        let conflicts = store
            .check_conflicts("test-agent", date, time, 90)
            .await
            .unwrap();

        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].id, "r1");
    }

    #[tokio::test]
    async fn test_no_conflict_non_overlapping() {
        let (_tmp, store) = make_store();

        // Existing reservation at 18:00, duration 60 min => ends 19:00
        let existing = sample_reservation("r1", "18:00");
        store.create(&existing).await.unwrap();

        // Check for conflict at 20:00 with 60 min duration — no overlap
        let date = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();
        let time = NaiveTime::from_hms_opt(20, 0, 0).unwrap();
        let conflicts = store
            .check_conflicts("test-agent", date, time, 60)
            .await
            .unwrap();

        assert!(conflicts.is_empty());
    }

    #[tokio::test]
    async fn test_cancelled_not_conflicting() {
        let (_tmp, store) = make_store();

        let existing = sample_reservation("r1", "18:00");
        store.create(&existing).await.unwrap();
        store
            .update_status("r1", ReservationStatus::Cancelled)
            .await
            .unwrap();

        // Same time should not conflict because existing is cancelled
        let date = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();
        let time = NaiveTime::from_hms_opt(18, 0, 0).unwrap();
        let conflicts = store
            .check_conflicts("test-agent", date, time, 90)
            .await
            .unwrap();

        assert!(conflicts.is_empty());
    }

    #[tokio::test]
    async fn test_multiple_reservations_sorted() {
        let (_tmp, store) = make_store();

        // Insert in reverse order
        store.create(&sample_reservation("r2", "20:00")).await.unwrap();
        store.create(&sample_reservation("r1", "18:00")).await.unwrap();
        store.create(&sample_reservation("r3", "19:00")).await.unwrap();

        let date = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();
        let list = store.list_by_date("test-agent", date).await.unwrap();

        assert_eq!(list.len(), 3);
        // Should be sorted by time ascending
        assert_eq!(list[0].time, NaiveTime::from_hms_opt(18, 0, 0).unwrap());
        assert_eq!(list[1].time, NaiveTime::from_hms_opt(19, 0, 0).unwrap());
        assert_eq!(list[2].time, NaiveTime::from_hms_opt(20, 0, 0).unwrap());
    }

    #[test]
    fn test_status_roundtrip() {
        for status in &[
            ReservationStatus::Pending,
            ReservationStatus::Confirmed,
            ReservationStatus::Cancelled,
            ReservationStatus::Completed,
        ] {
            let s = status.as_str();
            let parsed = ReservationStatus::from_str(s).unwrap();
            assert_eq!(&parsed, status);
        }
    }

    #[test]
    fn test_status_invalid() {
        let result = ReservationStatus::from_str("unknown");
        assert!(result.is_err());
    }
}
