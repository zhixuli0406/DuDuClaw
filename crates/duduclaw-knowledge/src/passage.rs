//! One passage with its surrounding text, for the source viewer and the
//! `knowledge_get_passage` tool. The server cuts `before` / `match` /
//! `after` itself (in Unicode scalar values) so no client does offset maths.
//!
//! The allowed set is re-checked on every call. A chunk that does not exist
//! and one the caller may not read give the same answer.

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::acl::AllowedSet;
use crate::locator::Locator;
use crate::search::load_chunk;
use crate::{KbError, KbResult};

/// Largest context a caller may ask for on each side.
pub const MAX_CONTEXT_CHARS: usize = 2_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Passage {
    pub chunk_id: i64,
    pub source_id: String,
    pub collection_id: String,
    pub title: String,
    pub locator: Locator,
    pub locator_text: String,
    pub heading_path: String,
    pub segment_label: Option<String>,
    pub before: String,
    #[serde(rename = "match")]
    pub matched: String,
    pub after: String,
    pub prev_chunk_id: Option<i64>,
    pub next_chunk_id: Option<i64>,
    pub suspect: bool,
}

fn neighbour(conn: &Connection, source_id: &str, ord: i64, delta: i64) -> KbResult<Option<i64>> {
    let mut stmt =
        conn.prepare_cached("SELECT id FROM chunks WHERE source_id = ?1 AND ord = ?2")?;
    let mut rows = stmt.query(params![source_id, ord + delta])?;
    Ok(match rows.next()? {
        Some(r) => Some(r.get(0)?),
        None => None,
    })
}

/// The passage `chunk_id` with up to `context_chars` on each side.
pub fn get_passage(
    conn: &Connection,
    chunk_id: i64,
    allowed: &AllowedSet,
    context_chars: usize,
) -> KbResult<Passage> {
    let row = load_chunk(conn, chunk_id)?.ok_or(KbError::PermissionDenied)?;
    if !allowed.contains(&row.collection_id) {
        return Err(KbError::PermissionDenied);
    }
    let ctx = context_chars.min(MAX_CONTEXT_CHARS);
    let mut stmt = conn
        .prepare_cached("SELECT ord, label, char_start, text FROM source_segments WHERE source_id = ?1 ORDER BY ord")?;
    let segments: Vec<(i64, Option<String>, i64, String)> = stmt
        .query_map(params![row.source_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<Result<_, _>>()?;
    let full: Vec<char> = segments
        .iter()
        .map(|s| s.3.as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
        .chars()
        .collect();
    let start = (row.char_start.max(0) as usize).min(full.len());
    let end = (row.char_end.max(0) as usize).clamp(start, full.len());
    let before: String = full[start.saturating_sub(ctx)..start].iter().collect();
    let after: String = full[end..(end + ctx).min(full.len())].iter().collect();
    let segment_label = segments
        .iter()
        .find(|s| s.0 == row.segment_ord)
        .and_then(|s| s.1.clone());
    Ok(Passage {
        chunk_id: row.id,
        prev_chunk_id: neighbour(conn, &row.source_id, row.ord, -1)?,
        next_chunk_id: neighbour(conn, &row.source_id, row.ord, 1)?,
        source_id: row.source_id,
        collection_id: row.collection_id,
        title: row.title,
        locator_text: row.locator.display(),
        locator: row.locator,
        heading_path: row.heading_path,
        segment_label,
        before,
        matched: row.text,
        after,
        suspect: row.flags & crate::index::FLAG_SUSPECT != 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acl::{HumanCtx, Narrowing, effective_collections};
    use crate::extract::{Format, RawSegment, SegmentKind};
    use crate::limits::KnowledgeConfig;
    use crate::store::{NewCollection, create_collection};

    #[test]
    fn passage_has_context_and_neighbours_and_checks_acl() {
        let mut conn = crate::schema::open_in_memory().unwrap();
        let cfg = KnowledgeConfig::default();
        let c = create_collection(
            &mut conn,
            &NewCollection {
                name: "HR".into(),
                ..Default::default()
            },
            "u",
            &cfg,
        )
        .unwrap();
        let other = create_collection(
            &mut conn,
            &NewCollection {
                name: "X".into(),
                ..Default::default()
            },
            "v",
            &cfg,
        )
        .unwrap();
        let mut ns = crate::sources::test_support::new_source(&c.id, &"5".repeat(64));
        ns.format = Format::Txt;
        let crate::sources::Added::New(s) =
            crate::sources::add_source(&mut conn, &ns, &cfg).unwrap()
        else {
            panic!()
        };
        let text = (0..80)
            .map(|i| format!("第{i}條規定內容在這裡說明。"))
            .collect::<String>();
        let prepared = crate::chunk::prepare(
            Format::Txt,
            vec![RawSegment::new(SegmentKind::Text, text.clone())],
        );
        crate::index::write_index(&mut conn, &s.id, &prepared, &[], "t", &|_| false, &cfg).unwrap();
        let ids: Vec<i64> = {
            let mut st = conn.prepare("SELECT id FROM chunks ORDER BY ord").unwrap();
            st.query_map([], |r| r.get(0))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };
        assert!(ids.len() >= 2);
        let set = effective_collections(
            &conn,
            &HumanCtx::dashboard("u", None, false),
            None,
            &Narrowing::default(),
        )
        .unwrap();
        let p = get_passage(&conn, ids[1], &set, 50).unwrap();
        assert_eq!(p.before.chars().count(), 50);
        assert_eq!(p.prev_chunk_id, Some(ids[0]));
        let joined = format!("{}{}{}", p.before, p.matched, p.after);
        assert!(text.contains(&joined));
        // Another user's set: same refusal as a missing chunk.
        let other_set = effective_collections(
            &conn,
            &HumanCtx::dashboard("v", None, false),
            None,
            &Narrowing::default(),
        )
        .unwrap();
        assert!(other_set.contains(&other.id));
        assert!(matches!(
            get_passage(&conn, ids[1], &other_set, 0),
            Err(KbError::PermissionDenied)
        ));
        assert!(matches!(
            get_passage(&conn, 999_999, &set, 0),
            Err(KbError::PermissionDenied)
        ));
    }
}
