//! v1.81 P1e: one-time link of legacy `goal_created` activity rows to their
//! goal (native Linux isolation, finding N3).
//!
//! Before P1d `goals_create` wrote its `goal_created` row without metadata,
//! and the creator lookup matched such a row to a goal by title — which any
//! employee could forge with `activity_post`. P1e reserves the event type
//! (`personal_visibility::is_reserved_activity_event`) and reads ownership
//! from metadata only (`personal_visibility::linked_goals`). This migration
//! links every row that has no goal link yet, once, inside the schema
//! transaction:
//!
//! - its summary `Created goal: <title>` names exactly one goal ⇒
//!   `metadata.goal_id` (the exact linkage);
//! - several goals share the title ⇒ `metadata.goal_ids` lists all of them,
//!   so each is hidden whenever this creator is hidden (fail closed);
//! - no goal has the title ⇒ nothing to link.
//!
//! Original metadata that is a JSON object is kept (the link is added to it);
//! anything else is kept under `legacy_metadata`. Every touched row carries
//! `"goal_backfill": "p1e"`. The marker row in `task_store_markers` makes the
//! migration run once.
use super::*;

/// `task_store_markers.name` of this migration.
pub(crate) const GOAL_BACKFILL_MARKER: &str = "p1e_goal_creator_backfill";
const SUMMARY_PREFIX: &str = "Created goal: ";

/// The new metadata for one legacy row, or `None` when it needs no change
/// (already linked, or no goal has its title).
pub(crate) fn backfilled_metadata(metadata: Option<&str>, goal_ids: &[String]) -> Option<String> {
    if !crate::personal_visibility::linked_goals(metadata).is_empty() || goal_ids.is_empty() {
        return None;
    }
    let mut object = match metadata.map(|raw| serde_json::from_str::<serde_json::Value>(raw)) {
        Some(Ok(serde_json::Value::Object(map))) => map,
        Some(_) => {
            let mut map = serde_json::Map::new();
            map.insert("legacy_metadata".into(), serde_json::Value::String(metadata.unwrap_or_default().into()));
            map
        }
        None => serde_json::Map::new(),
    };
    match goal_ids {
        [only] => object.insert("goal_id".into(), serde_json::Value::String(only.clone())),
        many => object.insert("goal_ids".into(), serde_json::json!(many)),
    };
    object.insert("goal_backfill".into(), serde_json::Value::String("p1e".into()));
    Some(serde_json::Value::Object(object).to_string())
}

impl TaskStore {
    /// Run [`backfilled_metadata`] over every `goal_created` row once
    /// (marker [`GOAL_BACKFILL_MARKER`]). `conn` is the schema transaction.
    pub(super) fn backfill_goal_creators(conn: &Connection) -> Result<(), String> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS task_store_markers (
                 name       TEXT PRIMARY KEY,
                 applied_at TEXT NOT NULL
             );",
        )
        .map_err(|e| format!("init task store markers: {e}"))?;
        let done = conn
            .query_row("SELECT 1 FROM task_store_markers WHERE name = ?1", [GOAL_BACKFILL_MARKER], |_| Ok(()))
            .optional()
            .map_err(|e| format!("read goal backfill marker: {e}"))?
            .is_some();
        if done {
            return Ok(());
        }
        let rows: Vec<(String, String, Option<String>)> = {
            let mut statement = conn
                .prepare("SELECT id, summary, metadata FROM activity WHERE event_type = ?1")
                .map_err(|e| format!("read legacy goal rows: {e}"))?;
            let rows = statement
                .query_map([crate::personal_visibility::GOAL_CREATED_EVENT], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })
                .map_err(|e| format!("read legacy goal rows: {e}"))?
                .collect::<Result<_, _>>()
                .map_err(|e| format!("read legacy goal rows: {e}"))?;
            rows
        };
        let mut by_title = conn
            .prepare("SELECT id FROM goals WHERE title = ?1 ORDER BY id")
            .map_err(|e| format!("read goal titles: {e}"))?;
        for (id, summary, metadata) in rows {
            let Some(title) = summary.strip_prefix(SUMMARY_PREFIX) else {
                continue;
            };
            let goals: Vec<String> = by_title
                .query_map([title], |row| row.get(0))
                .map_err(|e| format!("read goal titles: {e}"))?
                .collect::<Result<_, _>>()
                .map_err(|e| format!("read goal titles: {e}"))?;
            if let Some(updated) = backfilled_metadata(metadata.as_deref(), &goals) {
                conn.execute("UPDATE activity SET metadata = ?1 WHERE id = ?2", params![updated, id])
                    .map_err(|e| format!("backfill goal creator: {e}"))?;
            }
        }
        conn.execute(
            "INSERT INTO task_store_markers (name, applied_at) VALUES (?1, ?2)",
            params![GOAL_BACKFILL_MARKER, Utc::now().to_rfc3339()],
        )
        .map_err(|e| format!("write goal backfill marker: {e}"))?;
        Ok(())
    }
}
