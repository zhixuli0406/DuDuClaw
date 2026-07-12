//! G4 session portability trio: `/handoff`, `/undo`, `/rollback`.
//!
//! Extends [`SessionManager`] (same crate, separate module to keep
//! `session.rs` focused) with:
//!
//! - **Handoff** — copy a session's conversation state (visible turns,
//!   which include any compression-summary system row) onto the same
//!   agent's live session on another channel. The source is annotated
//!   with a provenance marker, never deleted.
//!   Fail-closed target selection: the copy proceeds ONLY when the target
//!   is unambiguous — the agent has exactly one active session on the
//!   target channel, or a target session's chat identity matches the
//!   caller's (see [`resolve_handoff_target`]). Several unrelated sessions
//!   ⇒ refuse (a "most recent wins" pick would land user A's transcript
//!   in user B's conversation). When the target channel has no session at
//!   all we also FAIL CLOSED (the store keys sessions by
//!   `<channel>:<chat_id>`, so without an inbound message we cannot know
//!   the chat id) — the user is told to ping the agent on the target
//!   channel first.
//! - **Undo** — tombstone (`undone_at`, soft delete) the last N
//!   user+assistant turn pairs. Tombstoned rows stay in SQLite for audit
//!   until the next compression pass (compress() rewrites the message set
//!   wholesale) and are excluded from prompt reconstruction and token
//!   counts. Fail-closed: cannot cross a compression/handoff boundary
//!   (any `role = 'system'` marker row).
//! - **Rollback** — undo back to the checkpoint watermark that
//!   `append_message` records before every user turn (i.e. before every
//!   agent run). Conversation-state rollback only — file edits made by
//!   the CLI runtimes are NOT reverted (stated honestly in the reply).
//!
//! All state lives in the existing `sessions` / `session_messages`
//! tables; the idempotent column migrations are in `session.rs`.

use duduclaw_core::error::{DuDuClawError, Result};
use rusqlite::params;

use crate::session::SessionManager;

/// Maximum turn pairs a single `/undo` may remove.
pub const UNDO_MAX_PAIRS: u32 = 20;

/// Outcome of an `/undo` request.
#[derive(Debug, PartialEq, Eq)]
pub enum UndoDecision {
    /// Tombstoned `pairs` turn pairs (`messages` rows, `tokens` freed).
    Undone {
        pairs: u32,
        messages: u32,
        tokens: u32,
    },
    /// No visible user turn after the last compression/handoff boundary.
    NothingToUndo,
    /// Requested more pairs than exist since the last boundary — the whole
    /// operation is refused (fail-closed), nothing is tombstoned.
    BoundaryBlocked { available: u32 },
}

/// Outcome of a `/rollback` request.
#[derive(Debug, PartialEq, Eq)]
pub enum RollbackDecision {
    RolledBack {
        messages: u32,
        tokens: u32,
    },
    /// No checkpoint recorded yet, or nothing appended after it.
    NothingToRollback,
}

/// Outcome of a `/handoff` copy.
#[derive(Debug, PartialEq, Eq)]
pub enum HandoffDecision {
    Done(HandoffReport),
    /// Source session has no visible turns and no compression summary.
    NothingToHandoff,
}

#[derive(Debug, PartialEq, Eq)]
pub struct HandoffReport {
    pub target_session: String,
    pub copied_messages: u32,
    pub copied_tokens: u32,
}

/// Fail-closed handoff target selection (see [`resolve_handoff_target`]).
#[derive(Debug, PartialEq, Eq)]
pub enum HandoffTarget {
    /// Exactly one candidate, or one whose chat identity matches the caller.
    Resolved(String),
    /// No active session on the target channel.
    NoSession,
    /// Several unrelated sessions — refusing to guess (cross-user leak risk).
    Ambiguous(usize),
}

/// The chat identity a session key encodes: everything after the leading
/// `<channel>:` prefix (e.g. `telegram:123:45` → `123:45`,
/// `slack:group:C1` → `group:C1`).
fn session_chat_identity(session_id: &str) -> &str {
    session_id
        .split_once(':')
        .map(|(_, rest)| rest)
        .unwrap_or("")
}

/// Choose the handoff target among `candidates` (the agent's active sessions
/// on the target channel, most-recent first). Fail-closed:
///
/// - exactly one candidate ⇒ use it (no one else's conversation to hit);
/// - a candidate whose chat identity equals the caller's ⇒ use it;
/// - otherwise ⇒ [`HandoffTarget::Ambiguous`] — the caller must disambiguate
///   (e.g. by messaging the agent on the target channel first, which bumps
///   that session to be resolvable). NEVER "most recent wins": that lands
///   user A's transcript in user B's session.
pub fn resolve_handoff_target(candidates: &[String], source_session_id: &str) -> HandoffTarget {
    match candidates {
        [] => HandoffTarget::NoSession,
        [only] => HandoffTarget::Resolved(only.clone()),
        many => {
            let caller_identity = session_chat_identity(source_session_id);
            if !caller_identity.is_empty() {
                // Exact identity equality only (anchored — never substring).
                if let Some(hit) = many
                    .iter()
                    .find(|c| session_chat_identity(c) == caller_identity)
                {
                    return HandoffTarget::Resolved(hit.clone());
                }
            }
            HandoffTarget::Ambiguous(many.len())
        }
    }
}

impl SessionManager {
    /// Owning agent of a session, if the session row exists.
    pub async fn session_agent(&self, session_id: &str) -> Result<Option<String>> {
        let conn = self.acquire().await;
        let r: std::result::Result<String, _> = conn.query_row(
            "SELECT agent_id FROM sessions WHERE id = ?1",
            params![session_id],
            |row| row.get(0),
        );
        match r {
            Ok(a) => Ok(Some(a)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(DuDuClawError::Gateway(format!("session_agent: {e}"))),
        }
    }

    /// The agent's active (non-archived) sessions on `channel`, most-recent
    /// first, capped at `limit` (session ids follow the
    /// `<channel>:<chat_id>[:<thread>]` convention). Feed the result to
    /// [`resolve_handoff_target`] — never pick "most recent" blindly.
    ///
    /// `channel` must come from `SUPPORTED_CHANNEL_TYPES` (fixed lowercase
    /// names, no LIKE metacharacters) — callers validate before calling.
    pub async fn active_sessions_for_channel(
        &self,
        agent_id: &str,
        channel: &str,
        limit: u32,
    ) -> Result<Vec<String>> {
        let conn = self.acquire().await;
        let like = format!("{channel}:%");
        let mut stmt = conn
            .prepare(
                "SELECT id FROM sessions
                 WHERE agent_id = ?1 AND archived_at IS NULL AND id LIKE ?2
                 ORDER BY last_active DESC
                 LIMIT ?3",
            )
            .map_err(|e| DuDuClawError::Gateway(format!("active_sessions_for_channel: {e}")))?;
        let mapped = stmt
            .query_map(params![agent_id, like, limit], |row| row.get(0))
            .map_err(|e| DuDuClawError::Gateway(format!("active_sessions_for_channel: {e}")))?;
        let mut out = Vec::new();
        for r in mapped {
            out.push(r.map_err(|e| DuDuClawError::Gateway(e.to_string()))?);
        }
        Ok(out)
    }

    /// Copy the source session's visible conversation state onto the target
    /// session (same agent, different channel), in one transaction:
    ///
    /// 1. all visible source rows (incl. any compression-summary system row)
    ///    are copied with role/content/tokens/timestamp preserved;
    /// 2. a provenance marker (`role = 'system'`) is appended AFTER the
    ///    copied turns — the undo/rollback boundary is `MAX(id)` of system
    ///    rows, so the marker only protects the import when it sits behind
    ///    it (a marker inserted first would let `/undo`//`/rollback` eat
    ///    the entire import);
    /// 3. the target's token counter and `summary` (if empty) are updated;
    /// 4. the source is annotated with a marker turn — never deleted.
    pub async fn handoff_session(
        &self,
        source_id: &str,
        target_id: &str,
    ) -> Result<HandoffDecision> {
        if source_id == target_id {
            return Err(DuDuClawError::Gateway(
                "handoff_session: source and target are the same session".to_string(),
            ));
        }
        let conn = self.acquire().await;
        let now = chrono::Utc::now().to_rfc3339();

        let tx = conn
            .unchecked_transaction()
            .map_err(|e| DuDuClawError::Gateway(format!("begin transaction: {e}")))?;

        // Source conversation state (visible = not hidden, not undone).
        let rows: Vec<(String, String, i64, String)> = {
            let mut stmt = tx
                .prepare(
                    "SELECT role, content, tokens, timestamp FROM session_messages
                     WHERE session_id = ?1 AND hidden = 0 AND undone_at IS NULL
                     ORDER BY id ASC",
                )
                .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
            let mapped = stmt
                .query_map(params![source_id], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })
                .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
            let mut out = Vec::new();
            for r in mapped {
                out.push(r.map_err(|e| DuDuClawError::Gateway(e.to_string()))?);
            }
            out
        };
        let source_summary: String = tx
            .query_row(
                "SELECT COALESCE(summary, '') FROM sessions WHERE id = ?1",
                params![source_id],
                |row| row.get(0),
            )
            .map_err(|e| DuDuClawError::Gateway(format!("handoff read source: {e}")))?;

        if rows.is_empty() && source_summary.is_empty() {
            return Ok(HandoffDecision::NothingToHandoff);
        }

        let source_channel = source_id.split(':').next().unwrap_or(source_id);
        let target_channel = target_id.split(':').next().unwrap_or(target_id);

        // 1. Copy visible turns (original timestamps preserved for provenance).
        let mut copied_tokens: i64 = 0;
        for (role, content, tokens, timestamp) in &rows {
            tx.execute(
                "INSERT INTO session_messages (session_id, role, content, tokens, timestamp)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![target_id, role, content, tokens, timestamp],
            )
            .map_err(|e| DuDuClawError::Gateway(format!("handoff copy: {e}")))?;
            copied_tokens += (*tokens).max(0);
        }

        // 2. Provenance marker on the target, AFTER the copied turns: the
        // undo/rollback boundary is MAX(id) of system rows, so only a
        // trailing marker actually shields the imported history from
        // /undo//rollback (a leading marker left the whole import erasable).
        tx.execute(
            "INSERT INTO session_messages (session_id, role, content, tokens, timestamp)
             VALUES (?1, 'system', ?2, 0, ?3)",
            params![
                target_id,
                format!(
                    "[Handoff] 以上 {} 則訊息由 {} 頻道的對話轉移而來（{}）。請接續該對話脈絡回應。",
                    rows.len(),
                    source_channel,
                    now
                ),
                now
            ],
        )
        .map_err(|e| DuDuClawError::Gateway(format!("handoff marker: {e}")))?;

        // 3. Target counters + summary carry-over (only if the target has none).
        tx.execute(
            "UPDATE sessions SET total_tokens = total_tokens + ?1, last_active = ?2 WHERE id = ?3",
            params![copied_tokens, now, target_id],
        )
        .map_err(|e| DuDuClawError::Gateway(format!("handoff target update: {e}")))?;
        if !source_summary.is_empty() {
            tx.execute(
                "UPDATE sessions SET summary = ?1
                 WHERE id = ?2 AND COALESCE(summary, '') = ''",
                params![source_summary, target_id],
            )
            .map_err(|e| DuDuClawError::Gateway(format!("handoff summary: {e}")))?;
        }

        // 4. Annotate the source (kept intact and usable).
        tx.execute(
            "INSERT INTO session_messages (session_id, role, content, tokens, timestamp)
             VALUES (?1, 'system', ?2, 0, ?3)",
            params![
                source_id,
                format!(
                    "[Handoff] 此對話已於 {now} 轉移至 {target_channel} 頻道，本頻道的紀錄仍保留、可繼續使用。"
                ),
                now
            ],
        )
        .map_err(|e| DuDuClawError::Gateway(format!("handoff source marker: {e}")))?;
        tx.execute(
            "UPDATE sessions SET last_active = ?1 WHERE id = ?2",
            params![now, source_id],
        )
        .map_err(|e| DuDuClawError::Gateway(format!("handoff source update: {e}")))?;

        tx.commit()
            .map_err(|e| DuDuClawError::Gateway(format!("commit transaction: {e}")))?;

        tracing::info!(
            source_id,
            target_id,
            copied = rows.len(),
            "Session handoff completed"
        );
        Ok(HandoffDecision::Done(HandoffReport {
            target_session: target_id.to_string(),
            copied_messages: rows.len() as u32,
            copied_tokens: copied_tokens.max(0) as u32,
        }))
    }

    /// Tombstone the last `n` user+assistant turn pairs (soft delete via
    /// `undone_at` — rows stay for audit, excluded from reconstruction and
    /// token counts). Fail-closed: refuses entirely (nothing tombstoned)
    /// when `n` would cross a compression/handoff boundary.
    pub async fn undo_last_turns(&self, session_id: &str, n: u32) -> Result<UndoDecision> {
        if n == 0 || n > UNDO_MAX_PAIRS {
            return Err(DuDuClawError::Gateway(format!(
                "undo_last_turns: n must be 1..={UNDO_MAX_PAIRS}, got {n}"
            )));
        }
        let conn = self.acquire().await;
        let now = chrono::Utc::now().to_rfc3339();
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| DuDuClawError::Gateway(format!("begin transaction: {e}")))?;

        // Boundary = last system row (compression summary or handoff marker).
        // Boundaries are structural, so hidden/undone flags don't matter here.
        let boundary: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(id), 0) FROM session_messages
                 WHERE session_id = ?1 AND role = 'system'",
                params![session_id],
                |row| row.get(0),
            )
            .map_err(|e| DuDuClawError::Gateway(format!("undo boundary: {e}")))?;

        // Visible user turns after the boundary, newest first.
        let user_ids: Vec<i64> = {
            let mut stmt = tx
                .prepare(
                    "SELECT id FROM session_messages
                     WHERE session_id = ?1 AND role = 'user'
                       AND hidden = 0 AND undone_at IS NULL AND id > ?2
                     ORDER BY id DESC",
                )
                .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
            let mapped = stmt
                .query_map(params![session_id, boundary], |row| row.get(0))
                .map_err(|e| DuDuClawError::Gateway(e.to_string()))?;
            let mut out = Vec::new();
            for r in mapped {
                out.push(r.map_err(|e| DuDuClawError::Gateway(e.to_string()))?);
            }
            out
        };

        let available = user_ids.len() as u32;
        if available == 0 {
            return Ok(UndoDecision::NothingToUndo);
        }
        if n > available {
            return Ok(UndoDecision::BoundaryBlocked { available });
        }

        // Everything from the n-th-from-last user turn onward gets tombstoned
        // (its assistant reply and any extra rows the run appended included).
        let cutoff = user_ids[(n - 1) as usize];
        let tokens: i64 = tx
            .query_row(
                "SELECT COALESCE(SUM(tokens), 0) FROM session_messages
                 WHERE session_id = ?1 AND id >= ?2 AND undone_at IS NULL",
                params![session_id, cutoff],
                |row| row.get(0),
            )
            .map_err(|e| DuDuClawError::Gateway(format!("undo token sum: {e}")))?;
        let messages = tx
            .execute(
                "UPDATE session_messages SET undone_at = ?3
                 WHERE session_id = ?1 AND id >= ?2 AND undone_at IS NULL",
                params![session_id, cutoff, now],
            )
            .map_err(|e| DuDuClawError::Gateway(format!("undo tombstone: {e}")))?;
        tx.execute(
            "UPDATE sessions SET total_tokens = MAX(total_tokens - ?1, 0), last_active = ?2
             WHERE id = ?3",
            params![tokens, now, session_id],
        )
        .map_err(|e| DuDuClawError::Gateway(format!("undo token count: {e}")))?;

        tx.commit()
            .map_err(|e| DuDuClawError::Gateway(format!("commit transaction: {e}")))?;

        tracing::info!(
            session_id,
            pairs = n,
            messages,
            "Session turns undone (tombstoned)"
        );
        Ok(UndoDecision::Undone {
            pairs: n,
            messages: messages as u32,
            tokens: tokens.max(0) as u32,
        })
    }

    /// Undo back to the last checkpoint watermark (recorded by
    /// `append_message` just before every user turn). Conversation-state
    /// rollback only — file changes made by agent runs are NOT reverted.
    ///
    /// Fail-safe: a watermark of 0 is treated as "no checkpoint recorded"
    /// (pre-migration sessions and first-ever exchanges), so `/rollback`
    /// can never wipe a whole legacy session; `/undo` covers those cases.
    pub async fn rollback_to_checkpoint(&self, session_id: &str) -> Result<RollbackDecision> {
        let conn = self.acquire().await;
        let now = chrono::Utc::now().to_rfc3339();
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| DuDuClawError::Gateway(format!("begin transaction: {e}")))?;

        let checkpoint: i64 = match tx.query_row(
            "SELECT COALESCE(checkpoint_message_id, 0) FROM sessions WHERE id = ?1",
            params![session_id],
            |row| row.get(0),
        ) {
            Ok(v) => v,
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                return Ok(RollbackDecision::NothingToRollback)
            }
            Err(e) => return Err(DuDuClawError::Gateway(format!("rollback checkpoint: {e}"))),
        };
        if checkpoint <= 0 {
            return Ok(RollbackDecision::NothingToRollback);
        }

        // Never roll back across a compression/handoff boundary.
        let boundary: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(id), 0) FROM session_messages
                 WHERE session_id = ?1 AND role = 'system'",
                params![session_id],
                |row| row.get(0),
            )
            .map_err(|e| DuDuClawError::Gateway(format!("rollback boundary: {e}")))?;
        let watermark = checkpoint.max(boundary);

        let tokens: i64 = tx
            .query_row(
                "SELECT COALESCE(SUM(tokens), 0) FROM session_messages
                 WHERE session_id = ?1 AND id > ?2 AND undone_at IS NULL",
                params![session_id, watermark],
                |row| row.get(0),
            )
            .map_err(|e| DuDuClawError::Gateway(format!("rollback token sum: {e}")))?;
        let messages = tx
            .execute(
                "UPDATE session_messages SET undone_at = ?3
                 WHERE session_id = ?1 AND id > ?2 AND undone_at IS NULL",
                params![session_id, watermark, now],
            )
            .map_err(|e| DuDuClawError::Gateway(format!("rollback tombstone: {e}")))?;
        if messages == 0 {
            return Ok(RollbackDecision::NothingToRollback);
        }
        tx.execute(
            "UPDATE sessions SET total_tokens = MAX(total_tokens - ?1, 0), last_active = ?2
             WHERE id = ?3",
            params![tokens, now, session_id],
        )
        .map_err(|e| DuDuClawError::Gateway(format!("rollback token count: {e}")))?;

        tx.commit()
            .map_err(|e| DuDuClawError::Gateway(format!("commit transaction: {e}")))?;

        tracing::info!(
            session_id,
            messages,
            watermark,
            "Session rolled back to checkpoint"
        );
        Ok(RollbackDecision::RolledBack {
            messages: messages as u32,
            tokens: tokens.max(0) as u32,
        })
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    async fn mgr() -> (NamedTempFile, SessionManager) {
        let tmp = NamedTempFile::new().unwrap();
        let m = SessionManager::new(tmp.path()).unwrap();
        (tmp, m)
    }

    #[tokio::test]
    async fn handoff_state_lands_on_target_key() {
        let (_tmp, m) = mgr().await;
        m.get_or_create("slack:C1", "agent-a").await.unwrap();
        m.append_message("slack:C1", "user", "幫我查訂單", 10)
            .await
            .unwrap();
        m.append_message("slack:C1", "assistant", "訂單 #42 已出貨", 12)
            .await
            .unwrap();

        // Exactly one telegram session for the agent — unambiguous, proceeds.
        m.get_or_create("telegram:200", "agent-a").await.unwrap();
        m.append_message("telegram:200", "user", "hi", 1)
            .await
            .unwrap();

        let candidates = m
            .active_sessions_for_channel("agent-a", "telegram", 10)
            .await
            .unwrap();
        let target = match resolve_handoff_target(&candidates, "slack:C1") {
            HandoffTarget::Resolved(t) => t,
            other => panic!("expected Resolved, got {other:?}"),
        };
        assert_eq!(target, "telegram:200");

        let decision = m.handoff_session("slack:C1", &target).await.unwrap();
        let report = match decision {
            HandoffDecision::Done(r) => r,
            other => panic!("expected Done, got {other:?}"),
        };
        assert_eq!(report.target_session, "telegram:200");
        assert_eq!(report.copied_messages, 2);
        assert_eq!(report.copied_tokens, 22);

        // Target now carries: its own turn + the 2 copied turns + marker.
        // (Marker AFTER the import — it is the undo/rollback boundary and
        // only shields the imported turns when it trails them.)
        let msgs = m.get_messages("telegram:200").await.unwrap();
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0].content, "hi");
        assert_eq!(msgs[1].content, "幫我查訂單");
        assert_eq!(msgs[2].content, "訂單 #42 已出貨");
        assert!(
            msgs[3].content.contains("[Handoff]"),
            "provenance marker trails the imported turns"
        );
        let target_session = m.get_or_create("telegram:200", "agent-a").await.unwrap();
        assert_eq!(target_session.total_tokens, 1 + 22);

        // The boundary now protects the import: nothing to undo after it.
        let d = m.undo_last_turns("telegram:200", 1).await.unwrap();
        assert_eq!(d, UndoDecision::NothingToUndo, "marker shields the import");
        assert_eq!(m.get_messages("telegram:200").await.unwrap().len(), 4);

        // Source is annotated, not deleted.
        let src = m.get_messages("slack:C1").await.unwrap();
        assert_eq!(src.len(), 3);
        assert!(
            src[2].content.contains("[Handoff]"),
            "provenance marker on source"
        );
        assert!(src[2].content.contains("telegram"));
    }

    #[test]
    fn handoff_target_two_sessions_is_refused() {
        // Two unrelated sessions on the target channel — fail-closed refusal
        // (the old "most recent wins" leaked user A's transcript into user
        // B's conversation).
        let candidates = vec!["telegram:100".to_string(), "telegram:200".to_string()];
        assert_eq!(
            resolve_handoff_target(&candidates, "slack:C1"),
            HandoffTarget::Ambiguous(2)
        );
        // Single session proceeds.
        assert_eq!(
            resolve_handoff_target(&candidates[..1].to_vec(), "slack:C1"),
            HandoffTarget::Resolved("telegram:100".to_string())
        );
        // No session at all.
        assert_eq!(resolve_handoff_target(&[], "slack:C1"), HandoffTarget::NoSession);
    }

    #[test]
    fn handoff_target_identity_match_disambiguates() {
        // The caller's chat identity (session-key suffix) matching one of
        // several candidates resolves the ambiguity — exact equality only.
        let candidates = vec!["telegram:U9".to_string(), "telegram:U42".to_string()];
        assert_eq!(
            resolve_handoff_target(&candidates, "slack:U42"),
            HandoffTarget::Resolved("telegram:U42".to_string())
        );
        // Substring must NOT match (anchored comparison).
        let candidates = vec!["telegram:U421".to_string(), "telegram:U9".to_string()];
        assert_eq!(
            resolve_handoff_target(&candidates, "slack:U42"),
            HandoffTarget::Ambiguous(2)
        );
    }

    #[tokio::test]
    async fn handoff_empty_source_is_refused() {
        let (_tmp, m) = mgr().await;
        m.get_or_create("slack:C1", "a").await.unwrap();
        m.get_or_create("telegram:1", "a").await.unwrap();
        let decision = m.handoff_session("slack:C1", "telegram:1").await.unwrap();
        assert_eq!(decision, HandoffDecision::NothingToHandoff);
        assert!(
            m.get_messages("telegram:1").await.unwrap().is_empty(),
            "no marker junk"
        );
    }

    #[tokio::test]
    async fn handoff_skips_other_agents_and_archived_sessions() {
        let (_tmp, m) = mgr().await;
        m.get_or_create("telegram:1", "other-agent").await.unwrap();
        m.get_or_create("telegram:2", "agent-a").await.unwrap();
        m.delete_session("telegram:2").await.unwrap(); // archived
        assert!(
            m.active_sessions_for_channel("agent-a", "telegram", 10)
                .await
                .unwrap()
                .is_empty(),
            "other agents' and archived sessions must not be handoff targets"
        );
    }

    #[tokio::test]
    async fn undo_tombstones_excluded_from_reconstruction_and_tokens() {
        let (_tmp, m) = mgr().await;
        m.get_or_create("s1", "a").await.unwrap();
        m.append_message("s1", "user", "q1", 5).await.unwrap();
        m.append_message("s1", "assistant", "a1", 7).await.unwrap();
        m.append_message("s1", "user", "q2", 3).await.unwrap();
        m.append_message("s1", "assistant", "a2", 4).await.unwrap();

        let d = m.undo_last_turns("s1", 1).await.unwrap();
        assert_eq!(
            d,
            UndoDecision::Undone {
                pairs: 1,
                messages: 2,
                tokens: 7
            }
        );

        // Prompt reconstruction (get_messages) no longer sees the pair.
        let msgs = m.get_messages("s1").await.unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "q1");
        assert_eq!(msgs[1].content, "a1");

        // Token count reduced.
        let s = m.get_or_create("s1", "a").await.unwrap();
        assert_eq!(s.total_tokens, 12);

        // Tombstoned rows are NOT restorable via the hide/restore path.
        let found = m.search_hidden_messages("s1", "q2", 10).await.unwrap();
        assert!(
            found.is_empty(),
            "undone rows must not surface as hidden/restorable"
        );
    }

    #[tokio::test]
    async fn undo_refuses_past_compression_boundary() {
        let (_tmp, m) = mgr().await;
        m.get_or_create("s1", "a").await.unwrap();
        m.append_message("s1", "user", "old-q", 5).await.unwrap();
        m.append_message("s1", "assistant", "old-a", 5)
            .await
            .unwrap();
        m.compress("s1", "summary of old turns").await.unwrap();
        m.append_message("s1", "user", "new-q", 5).await.unwrap();
        m.append_message("s1", "assistant", "new-a", 5)
            .await
            .unwrap();

        // Only 1 pair exists after the boundary — asking for 2 is refused
        // entirely and nothing is tombstoned.
        let d = m.undo_last_turns("s1", 2).await.unwrap();
        assert_eq!(d, UndoDecision::BoundaryBlocked { available: 1 });
        assert_eq!(
            m.get_messages("s1").await.unwrap().len(),
            3,
            "refusal must not tombstone"
        );

        // Undoing the available pair works; then nothing is left to undo.
        let d = m.undo_last_turns("s1", 1).await.unwrap();
        assert!(matches!(d, UndoDecision::Undone { pairs: 1, .. }));
        let d = m.undo_last_turns("s1", 1).await.unwrap();
        assert_eq!(d, UndoDecision::NothingToUndo);

        // The compression summary survives.
        let msgs = m.get_messages("s1").await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "system");
    }

    #[tokio::test]
    async fn undo_rejects_out_of_range_n() {
        let (_tmp, m) = mgr().await;
        m.get_or_create("s1", "a").await.unwrap();
        assert!(m.undo_last_turns("s1", 0).await.is_err());
        assert!(m.undo_last_turns("s1", UNDO_MAX_PAIRS + 1).await.is_err());
    }

    #[tokio::test]
    async fn rollback_watermark_drops_only_last_exchange() {
        let (_tmp, m) = mgr().await;
        m.get_or_create("s1", "a").await.unwrap();
        // Exchange 1 — its user turn records checkpoint 0 (fresh session).
        m.append_message("s1", "user", "q1", 5).await.unwrap();
        m.append_message("s1", "assistant", "a1", 5).await.unwrap();
        // Exchange 2 — checkpoint watermark = id of "a1".
        m.append_message("s1", "user", "q2", 5).await.unwrap();
        m.append_message("s1", "assistant", "a2", 5).await.unwrap();
        m.append_message("s1", "assistant", "a2-extra", 2)
            .await
            .unwrap();

        let d = m.rollback_to_checkpoint("s1").await.unwrap();
        assert_eq!(
            d,
            RollbackDecision::RolledBack {
                messages: 3,
                tokens: 12
            }
        );

        let msgs = m.get_messages("s1").await.unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "q1");
        assert_eq!(msgs[1].content, "a1");

        // Idempotent: nothing left after the watermark.
        let d = m.rollback_to_checkpoint("s1").await.unwrap();
        assert_eq!(d, RollbackDecision::NothingToRollback);
    }

    #[tokio::test]
    async fn rollback_fails_safe_without_checkpoint() {
        let (_tmp, m) = mgr().await;
        m.get_or_create("s1", "a").await.unwrap();
        // First-ever exchange: checkpoint watermark is 0 ⇒ treated as unset,
        // so /rollback refuses instead of wiping the session (/undo covers it).
        m.append_message("s1", "user", "q1", 5).await.unwrap();
        m.append_message("s1", "assistant", "a1", 5).await.unwrap();
        let d = m.rollback_to_checkpoint("s1").await.unwrap();
        assert_eq!(d, RollbackDecision::NothingToRollback);
        assert_eq!(m.get_messages("s1").await.unwrap().len(), 2);

        // Unknown session ⇒ same safe refusal.
        let d = m.rollback_to_checkpoint("nope").await.unwrap();
        assert_eq!(d, RollbackDecision::NothingToRollback);
    }

    #[tokio::test]
    async fn rollback_never_crosses_compression_boundary() {
        let (_tmp, m) = mgr().await;
        m.get_or_create("s1", "a").await.unwrap();
        m.append_message("s1", "user", "q1", 5).await.unwrap();
        m.append_message("s1", "assistant", "a1", 5).await.unwrap();
        m.append_message("s1", "user", "q2", 5).await.unwrap();
        // Compression lands AFTER the checkpoint was recorded.
        m.compress("s1", "summary").await.unwrap();

        // Watermark clamps to the boundary — the summary row survives.
        let d = m.rollback_to_checkpoint("s1").await.unwrap();
        assert_eq!(d, RollbackDecision::NothingToRollback);
        let msgs = m.get_messages("s1").await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "system");
    }

    #[tokio::test]
    async fn lineage_bumps_on_every_compression() {
        let (_tmp, m) = mgr().await;
        let s = m.get_or_create("s1", "a").await.unwrap();
        assert_eq!(s.lineage, 1, "fresh session starts at #1");

        m.append_message("s1", "user", "q1", 5).await.unwrap();
        m.compress("s1", "sum-1").await.unwrap();
        assert_eq!(m.get_or_create("s1", "a").await.unwrap().lineage, 2);

        // /compact (force_compress) goes through compress() → also bumps.
        m.force_compress("s1").await.unwrap();
        assert_eq!(m.get_or_create("s1", "a").await.unwrap().lineage, 3);
    }
}
