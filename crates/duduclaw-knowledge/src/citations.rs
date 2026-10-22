//! `turn_citations`: which passages a reply offered and cited, per turn.
//!
//! Written by the gateway after a channel reply is stored; read to restore
//! WebChat citations when a conversation is reopened and to find the turns
//! that cited a source (forget by source). Rows keep the source id and title
//! after the source itself is deleted, so a reopened conversation can say
//! "source deleted" instead of silently losing the reference. No document
//! text beyond the short excerpt is stored.

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::KbResult;
use crate::acl::AllowedSet;

pub(crate) const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS turn_citations (
  turn_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  session_id TEXT NOT NULL,
  user_message_key TEXT,
  assistant_message_id INTEGER,
  n INTEGER NOT NULL,
  chunk_id INTEGER,
  source_id TEXT NOT NULL,
  title TEXT NOT NULL,
  locator TEXT NOT NULL,
  excerpt TEXT NOT NULL,
  cited INTEGER NOT NULL DEFAULT 0,
  at TEXT NOT NULL,
  PRIMARY KEY (turn_id, agent_id, n)
);
CREATE INDEX IF NOT EXISTS idx_tc_source ON turn_citations(source_id);
CREATE INDEX IF NOT EXISTS idx_tc_session ON turn_citations(session_id, at);
"#;

/// Longest excerpt kept per row.
pub const MAX_EXCERPT_CHARS: usize = 80;

/// One offered passage of one turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnCitation {
    pub turn_id: String,
    pub agent_id: String,
    pub session_id: String,
    pub user_message_key: Option<String>,
    pub assistant_message_id: Option<i64>,
    pub n: u32,
    pub chunk_id: Option<i64>,
    pub source_id: String,
    pub title: String,
    pub locator: String,
    pub excerpt: String,
    pub cited: bool,
    pub at: String,
}

/// Store the rows of one turn (replacing any earlier rows of that turn).
pub fn record_turn(conn: &mut Connection, rows: &[TurnCitation]) -> KbResult<()> {
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT OR REPLACE INTO turn_citations(turn_id, agent_id, session_id, user_message_key,
                assistant_message_id, n, chunk_id, source_id, title, locator, excerpt, cited, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        )?;
        for r in rows {
            stmt.execute(params![
                duduclaw_core::truncate_chars(&r.turn_id, 200),
                r.agent_id,
                duduclaw_core::truncate_chars(&r.session_id, 300),
                r.user_message_key,
                r.assistant_message_id,
                r.n,
                r.chunk_id,
                r.source_id,
                duduclaw_core::truncate_chars(&r.title, 120),
                duduclaw_core::truncate_chars(&r.locator, 80),
                duduclaw_core::truncate_chars(&r.excerpt, MAX_EXCERPT_CHARS),
                r.cited as i64,
                r.at,
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<TurnCitation> {
    Ok(TurnCitation {
        turn_id: r.get(0)?,
        agent_id: r.get(1)?,
        session_id: r.get(2)?,
        user_message_key: r.get(3)?,
        assistant_message_id: r.get(4)?,
        n: r.get(5)?,
        chunk_id: r.get(6)?,
        source_id: r.get(7)?,
        title: r.get(8)?,
        locator: r.get(9)?,
        excerpt: r.get(10)?,
        cited: r.get::<_, i64>(11)? != 0,
        at: r.get(12)?,
    })
}

const COLS: &str = "turn_id, agent_id, session_id, user_message_key, assistant_message_id, n, chunk_id, source_id, title, locator, excerpt, cited, at";

/// The cited rows of a session, oldest first, at most `limit`.
pub fn cited_in_session(
    conn: &Connection,
    session_id: &str,
    limit: usize,
) -> KbResult<Vec<TurnCitation>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLS} FROM turn_citations WHERE session_id = ?1 AND cited = 1
          ORDER BY at, turn_id, n LIMIT ?2"
    ))?;
    let rows = stmt.query_map(params![session_id, limit as i64], row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Every turn that cited `source_id` (forget by source).
pub fn turns_citing_source(conn: &Connection, source_id: &str) -> KbResult<Vec<TurnCitation>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLS} FROM turn_citations WHERE source_id = ?1 AND cited = 1 ORDER BY at"
    ))?;
    let rows = stmt.query_map(params![source_id], row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// How a stored citation may be shown now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CitationState {
    /// The source exists and the viewer may read it.
    Available,
    /// The source was deleted (title kept).
    Deleted,
    /// The viewer may no longer read it: title and excerpt withheld.
    Restricted,
}

/// A stored citation as returned to a viewer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShownCitation {
    pub turn_id: String,
    pub assistant_message_id: Option<i64>,
    pub n: u32,
    pub chunk_id: Option<i64>,
    pub source_id: String,
    pub title: String,
    pub locator: String,
    pub excerpt: String,
    pub state: CitationState,
}

/// Re-check every stored citation against what the viewer may read now.
pub fn shown_for_viewer(
    conn: &Connection,
    rows: Vec<TurnCitation>,
    allowed: &AllowedSet,
) -> KbResult<Vec<ShownCitation>> {
    let mut stmt = conn.prepare_cached("SELECT collection_id FROM sources WHERE id = ?1")?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let coll: Option<String> = {
            let mut q = stmt.query(params![r.source_id])?;
            match q.next()? {
                Some(row) => Some(row.get(0)?),
                None => None,
            }
        };
        let state = match coll {
            None => CitationState::Deleted,
            Some(c) if allowed.contains(&c) => CitationState::Available,
            Some(_) => CitationState::Restricted,
        };
        let hide = state == CitationState::Restricted;
        out.push(ShownCitation {
            turn_id: r.turn_id,
            assistant_message_id: r.assistant_message_id,
            n: r.n,
            chunk_id: if state == CitationState::Available {
                r.chunk_id
            } else {
                None
            },
            source_id: if hide { String::new() } else { r.source_id },
            title: if hide { String::new() } else { r.title },
            locator: if hide { String::new() } else { r.locator },
            excerpt: if state == CitationState::Available {
                r.excerpt
            } else {
                String::new()
            },
            state,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acl::{HumanCtx, Narrowing, effective_collections};
    use crate::index::test_support::indexed_source;
    use crate::limits::KnowledgeConfig;
    use crate::store::{NewCollection, create_collection};

    fn tc(turn: &str, n: u32, source: &str, cited: bool) -> TurnCitation {
        TurnCitation {
            turn_id: turn.into(),
            agent_id: "bot".into(),
            session_id: "line:U1".into(),
            user_message_key: Some("m:10".into()),
            assistant_message_id: Some(11),
            n,
            chunk_id: Some(1),
            source_id: source.into(),
            title: "員工手冊.pdf".into(),
            locator: "第 3 頁".into(),
            excerpt: "特休假依年資計算".into(),
            cited,
            at: crate::now_rfc3339(),
        }
    }

    #[test]
    fn records_and_reads_back() {
        let mut conn = crate::schema::open_in_memory().unwrap();
        record_turn(
            &mut conn,
            &[tc("t1", 1, "src_a", true), tc("t1", 2, "src_b", false)],
        )
        .unwrap();
        // Same key replaces.
        record_turn(&mut conn, &[tc("t1", 1, "src_a", true)]).unwrap();
        let rows = cited_in_session(&conn, "line:U1", 100).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source_id, "src_a");
        assert_eq!(turns_citing_source(&conn, "src_a").unwrap().len(), 1);
        assert!(turns_citing_source(&conn, "src_b").unwrap().is_empty());
    }

    #[test]
    fn shown_state_follows_current_access() {
        let mut conn = crate::schema::open_in_memory().unwrap();
        let cfg = KnowledgeConfig::default();
        let a = create_collection(
            &mut conn,
            &NewCollection {
                name: "A".into(),
                slug: Some("a".into()),
                ..Default::default()
            },
            "owner-a",
            &cfg,
        )
        .unwrap();
        let src = indexed_source(&mut conn, &a.id, "doc.txt", &["內容"]);
        record_turn(
            &mut conn,
            &[tc("t1", 1, &src, true), tc("t1", 2, "src_gone", true)],
        )
        .unwrap();
        let rows = cited_in_session(&conn, "line:U1", 10).unwrap();
        let owner = HumanCtx::dashboard("owner-a", None, false);
        let allowed = effective_collections(&conn, &owner, None, &Narrowing::default()).unwrap();
        let shown = shown_for_viewer(&conn, rows.clone(), &allowed).unwrap();
        assert_eq!(shown[0].state, CitationState::Available);
        assert_eq!(shown[1].state, CitationState::Deleted);
        assert_eq!(shown[1].title, "員工手冊.pdf");
        let stranger = HumanCtx::dashboard("someone", None, false);
        let none = effective_collections(&conn, &stranger, None, &Narrowing::default()).unwrap();
        let shown = shown_for_viewer(&conn, rows, &none).unwrap();
        assert_eq!(shown[0].state, CitationState::Restricted);
        assert!(shown[0].title.is_empty() && shown[0].excerpt.is_empty());
        assert!(shown[0].chunk_id.is_none());
    }
}
