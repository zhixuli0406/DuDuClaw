//! After a channel reply: resolve its `[n]` markers against the turn's
//! ledger, store `turn_citations`, and hand the cited sources to the
//! delivery step (WebChat frame field, messaging-app footer).
//!
//! The stored assistant message is the reply without any footer; the footer
//! is added only when the reply is sent to a messaging app.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use duduclaw_knowledge::citations::{TurnCitation, record_turn};
use duduclaw_knowledge::cite::{self, CitationRef};

use super::ledger::{self, LedgerEntry};

tokio::task_local! {
    /// Filled by the reply pipeline with the sources the reply cited; read
    /// by the delivery step of the same turn.
    pub static TURN_CITATIONS: Arc<Mutex<Vec<CitationRef>>>;
}

/// A reply with its citations resolved.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Finalized {
    /// The reply with invalid markers removed.
    pub text: String,
    /// Every passage offered this turn, `cited` set for the cited ones.
    pub rows: Vec<TurnCitation>,
    /// Cited sources in order of first citation.
    pub cited: Vec<CitationRef>,
    /// Markers that named nothing offered (removed).
    pub invalid: Vec<u32>,
}

fn citation_ref(e: &LedgerEntry) -> CitationRef {
    CitationRef {
        n: e.n,
        chunk_id: e.chunk_id,
        source_id: e.source_id.clone(),
        title: e.title.clone(),
        locator: e.locator.clone(),
        excerpt: cite::excerpt(&e.text, &e.query),
    }
}

/// Resolve `reply` against what `(agent_id, turn_id)` offered. A turn that
/// offered nothing returns the reply unchanged and no rows.
pub fn finalize(agent_id: &str, turn_id: &str, session_id: &str, reply: &str) -> Finalized {
    let offered = ledger::entries(agent_id, turn_id);
    let numbers: Vec<u32> = offered.iter().map(|e| e.n).collect();
    let parsed = cite::parse_citations(reply, &numbers);
    if !parsed.invalid.is_empty() {
        tracing::info!(
            agent = agent_id,
            invalid = ?parsed.invalid,
            "knowledge: citation_invalid — reply cited passages that were not offered; markers removed"
        );
    }
    let at = duduclaw_knowledge::now_rfc3339();
    let rows = offered
        .iter()
        .map(|e| TurnCitation {
            turn_id: turn_id.to_string(),
            agent_id: agent_id.to_string(),
            session_id: session_id.to_string(),
            user_message_key: None,
            assistant_message_id: None,
            n: e.n,
            chunk_id: Some(e.chunk_id),
            source_id: e.source_id.clone(),
            title: e.title.clone(),
            locator: e.locator.clone(),
            excerpt: cite::excerpt(&e.text, &e.query),
            cited: parsed.cited.contains(&e.n),
            at: at.clone(),
        })
        .collect();
    let cited = parsed
        .cited
        .iter()
        .filter_map(|n| offered.iter().find(|e| e.n == *n))
        .map(citation_ref)
        .collect();
    Finalized {
        text: parsed.text,
        rows,
        cited,
        invalid: parsed.invalid,
    }
}

/// Hand the cited sources to the delivery step of this turn (no-op outside
/// a reply scope).
pub fn publish(cited: &[CitationRef]) {
    let _ = TURN_CITATIONS.try_with(|slot| {
        if let Ok(mut s) = slot.lock() {
            *s = cited.to_vec();
        }
    });
}

/// Store the turn's rows with the stored message ids. Best effort: a
/// failure is logged (the reply itself is unaffected).
pub async fn store(
    home: &Path,
    mut rows: Vec<TurnCitation>,
    user_message_id: Option<i64>,
    assistant_message_id: Option<i64>,
) {
    if rows.is_empty() {
        return;
    }
    for r in &mut rows {
        r.user_message_key = user_message_id.map(|id| format!("m:{id}"));
        r.assistant_message_id = assistant_message_id;
    }
    let home: PathBuf = home.to_path_buf();
    let result = super::blocking(move || {
        let mut conn = duduclaw_knowledge::schema::open_home(&home)?;
        record_turn(&mut conn, &rows)
    })
    .await;
    if let Err(e) = result {
        tracing::warn!(error = %e, "knowledge: turn_citations write failed");
    }
}

/// The source footer for a reply sent to the messaging app of
/// `session_id`, within that channel's footer cap. `None` when nothing was
/// cited or the session is not a messaging app.
pub fn footer_for(session_id: &str, cited: &[CitationRef]) -> Option<String> {
    if !crate::channel_reply::is_messaging_session(session_id) {
        return None;
    }
    let channel = session_id.split(':').next().unwrap_or("");
    cite::render_footer(cited, cite::footer_cap(channel))
}

/// `reply` with the footer for `session_id`, unless the reply is empty
/// (empty means "send nothing").
pub fn with_footer(reply: String, session_id: &str, cited: &[CitationRef]) -> String {
    if reply.trim().is_empty() {
        return reply;
    }
    match footer_for(session_id, cited) {
        Some(f) => format!("{reply}\n\n{f}"),
        None => reply,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_knowledge::locator::{Locator, LocatorKind};
    use duduclaw_knowledge::search::Hit;

    fn hit(id: i64, title: &str, text: &str) -> Hit {
        Hit {
            chunk_id: id,
            source_id: format!("src_{id}"),
            collection_id: "col".into(),
            title: title.into(),
            locator: Locator::new(LocatorKind::Page),
            locator_text: format!("第 {id} 頁"),
            heading_path: String::new(),
            text: text.into(),
            score: 1.0,
            fts_rank: Some(1),
            like_rank: None,
            vec_rank: None,
            vec_score: None,
            suspect: false,
        }
    }

    #[test]
    fn finalize_marks_cited_and_strips_invalid() {
        let _g = ledger::open("cite-a", "turn-c1").unwrap();
        ledger::number(
            "cite-a",
            "turn-c1",
            &[
                hit(3, "員工手冊.pdf", "特休假依年資計算，滿一年者給予七日。"),
                hit(4, "報價單.xlsx", "A-1234 單價 NT$1,200。"),
            ],
            "特休假",
        );
        let f = finalize("cite-a", "turn-c1", "line:U1", "滿一年七日【1】，另見[7]。");
        assert_eq!(f.text, "滿一年七日[1]，另見。");
        assert_eq!(f.invalid, vec![7]);
        assert_eq!(f.cited.len(), 1);
        assert_eq!(f.cited[0].title, "員工手冊.pdf");
        assert!(f.cited[0].excerpt.starts_with("特休假依年資計算"));
        assert_eq!(f.rows.len(), 2);
        assert!(f.rows[0].cited && !f.rows[1].cited);
        // A turn that offered nothing leaves the reply alone.
        let f = finalize("cite-a", "turn-other", "line:U1", "見 [1]");
        assert_eq!(f.text, "見 [1]");
        assert!(f.rows.is_empty() && f.cited.is_empty());
    }

    #[test]
    fn footers_only_on_messaging_apps() {
        let cited = vec![CitationRef {
            n: 1,
            chunk_id: 1,
            source_id: "s".into(),
            title: "員工手冊.pdf".into(),
            locator: "第 3 頁".into(),
            excerpt: "特休假依年資計算…".into(),
        }];
        let out = with_footer("答案[1]".into(), "line:U1", &cited);
        assert_eq!(
            out,
            "答案[1]\n\n來源：\n[1] 員工手冊.pdf 第 3 頁\n「特休假依年資計算…」"
        );
        assert_eq!(
            with_footer("答案[1]".into(), "webchat:x:y", &cited),
            "答案[1]"
        );
        assert_eq!(with_footer(String::new(), "line:U1", &cited), "");
        assert_eq!(with_footer("答案".into(), "line:U1", &[]), "答案");
        let discord = footer_for("discord:123", &cited).unwrap();
        assert!(discord.chars().count() <= cite::footer_cap("discord"));
    }

    #[tokio::test]
    async fn publish_fills_the_turn_slot() {
        let slot = Arc::new(Mutex::new(Vec::new()));
        let c = CitationRef {
            n: 1,
            chunk_id: 1,
            source_id: "s".into(),
            title: "t".into(),
            locator: "l".into(),
            excerpt: String::new(),
        };
        TURN_CITATIONS
            .scope(slot.clone(), async { publish(std::slice::from_ref(&c)) })
            .await;
        assert_eq!(slot.lock().unwrap().clone(), vec![c]);
        // Outside a scope: no panic.
        publish(&[]);
    }
}
