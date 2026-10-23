//! Read-only SQL scope. No summary, title, tool steps or prior context enters
//! this module. Exact microsecond checks follow SQLite's coarser time filter.

use super::classify::{HistoryClass, classify};
use crate::{
    session::{SessionManager, SessionMessage},
    session_origin::{OriginKind, SessionOrigin},
    team_oversight::TeamOversight,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::BTreeSet;

#[derive(Default)]
pub struct ListFilter {
    pub report: Option<String>,
    pub class: Option<String>,
}
pub struct ReadSession {
    pub id: String,
    pub class: HistoryClass,
    pub last_active: String,
    pub rows: Vec<SessionMessage>,
    pub turns: usize,
}
pub struct Page {
    pub sessions: Vec<ReadSession>,
    pub next_before: Option<String>,
}

fn origin(kind: Option<String>, owner: Option<String>) -> Option<SessionOrigin> {
    kind.map(|k| match (OriginKind::parse(&k), owner) {
        (OriginKind::WebchatUser, Some(o)) => SessionOrigin::webchat_user(&o),
        (k, _) => SessionOrigin::of_kind(k),
    })
}

pub fn class_name(class: &HistoryClass) -> &'static str {
    match class {
        HistoryClass::ViewerOwn => "own",
        HistoryClass::ReportOwnWebchat { .. } => "report_webchat",
        HistoryClass::External { .. } => "external",
        HistoryClass::Hidden => "hidden",
    }
}

fn owners(scope: &TeamOversight) -> BTreeSet<String> {
    scope.reports.keys().cloned().collect()
}

fn messages(
    conn: &Connection,
    session: &str,
    since: DateTime<Utc>,
) -> Result<Vec<SessionMessage>, String> {
    let broad = (since - chrono::Duration::seconds(1)).to_rfc3339();
    let mut stmt = conn
        .prepare(
            "SELECT role,content,tokens,timestamp FROM session_messages
        WHERE session_id=?1 AND hidden=0 AND undone_at IS NULL AND role IN ('user','assistant')
          AND julianday(timestamp)>=julianday(?2) ORDER BY id LIMIT 20001",
        )
        .map_err(|e| e.to_string())?;
    let mapped = stmt
        .query_map(params![session, broad], |r| {
            Ok(SessionMessage {
                role: r.get(0)?,
                content: r.get(1)?,
                tokens: r.get(2)?,
                timestamp: r.get(3)?,
            })
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    let mut bytes = 0usize;
    let mut scanned = 0;
    for row in mapped {
        scanned += 1;
        if scanned > 20_000 {
            return Err("history window too large".into());
        }
        let row = row.map_err(|e| e.to_string())?;
        let Ok(time) = DateTime::parse_from_rfc3339(&row.timestamp) else {
            continue;
        };
        if time < since {
            continue;
        }
        bytes = bytes.saturating_add(row.content.len());
        if bytes > 8 * 1024 * 1024 {
            return Err("history window too large".into());
        }
        out.push(row);
    }
    Ok(out)
}

pub async fn get(
    sm: &SessionManager,
    scope: &TeamOversight,
    id: &str,
) -> Result<Option<ReadSession>, String> {
    let conn = sm.acquire().await;
    let row:Option<(String,Option<String>,Option<String>,String)>=conn.query_row("SELECT agent_id,origin_kind,owner_user_id,last_active FROM sessions WHERE id=?1 AND archived_at IS NULL",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(|e|e.to_string())?;
    let Some((agent, kind, owner, last_active)) = row else {
        return Ok(None);
    };
    if agent != scope.agent {
        return Ok(None);
    }
    let class = classify(
        id,
        origin(kind, owner).as_ref(),
        &scope.viewer_user_id,
        &owners(scope),
    );
    if class == HistoryClass::Hidden {
        return Ok(None);
    }
    let rows = messages(&conn, id, scope.since)?;
    let turns = rows.len();
    Ok((!rows.is_empty()).then(|| ReadSession {
        id: id.into(),
        class,
        last_active,
        rows,
        turns,
    }))
}

pub async fn list(
    sm: &SessionManager,
    scope: &TeamOversight,
    filter: &ListFilter,
    limit: usize,
    before: Option<&str>,
) -> Result<Page, String> {
    if !(1..=200).contains(&limit) {
        return Err("invalid limit".into());
    }
    if filter
        .report
        .as_ref()
        .is_some_and(|r| !scope.reports.contains_key(r))
    {
        return Err("permission denied".into());
    }
    if filter
        .class
        .as_ref()
        .is_some_and(|c| !matches!(c.as_str(), "own" | "report_webchat" | "external"))
    {
        return Err("invalid class".into());
    }
    let (before_time, before_id) = match before {
        None => (None, None),
        Some(raw) => {
            if raw.len() > 4096 {
                return Err("invalid before".into());
            }
            if let Ok(time) = DateTime::parse_from_rfc3339(raw) {
                (Some(time.to_rfc3339()), None)
            } else {
                let bytes = URL_SAFE_NO_PAD.decode(raw).map_err(|_| "invalid before")?;
                let (time, id): (String, String) =
                    serde_json::from_slice(&bytes).map_err(|_| "invalid before")?;
                DateTime::parse_from_rfc3339(&time).map_err(|_| "invalid before")?;
                if id.len() > 1024 {
                    return Err("invalid before".into());
                }
                (Some(time), Some(id))
            }
        }
    };
    let conn = sm.acquire().await;
    let owners_json = serde_json::to_string(&owners(scope)).map_err(|e| e.to_string())?;
    let broad = (scope.since - chrono::Duration::seconds(1)).to_rfc3339();
    let exact_before = if before_id.is_none() {
        before_time
            .as_deref()
            .map(DateTime::parse_from_rfc3339)
            .transpose()
            .map_err(|_| "invalid before")?
    } else {
        None
    };
    let broad_before = exact_before.map(|t| (t + chrono::Duration::seconds(1)).to_rfc3339());
    // Scope precedes pagination. The cursor is (last_active,id), so equal
    // activity times neither duplicate nor hide a permitted conversation.
    let mut stmt=conn.prepare("SELECT id,origin_kind,owner_user_id,last_active FROM sessions
      WHERE agent_id=?1 AND archived_at IS NULL AND julianday(last_active)>=julianday(?2)
        AND ((origin_kind='webchat_user' AND (owner_user_id=?3 OR owner_user_id IN (SELECT value FROM json_each(?4))))
             OR origin_kind IN ('channel','webchat_visitor','public_visitor'))
        AND (?5 IS NULL OR (origin_kind='webchat_user' AND owner_user_id=?5))
        AND (?6 IS NULL OR (?6='own' AND origin_kind='webchat_user' AND owner_user_id=?3)
             OR (?6='report_webchat' AND origin_kind='webchat_user' AND owner_user_id IN (SELECT value FROM json_each(?4)))
             OR (?6='external' AND origin_kind IN ('channel','webchat_visitor','public_visitor')))
        AND (?7 IS NULL OR (?8 IS NULL AND julianday(last_active)<julianday(?9))
             OR (?8 IS NOT NULL AND (last_active<?7 OR (last_active=?7 AND id<?8))))
      ORDER BY last_active DESC,id DESC LIMIT 10001").map_err(|e|e.to_string())?;
    let rows = stmt
        .query_map(
            params![
                scope.agent,
                broad,
                scope.viewer_user_id,
                owners_json,
                filter.report,
                filter.class,
                before_time,
                before_id,
                broad_before
            ],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, String>(3)?,
                ))
            },
        )
        .map_err(|e| e.to_string())?;
    let mut sessions = Vec::new();
    let mut title_bytes = 0usize;
    let owner_set = owners(scope);
    for (scanned, row) in rows.enumerate() {
        if scanned >= 10_000 {
            return Err("history page scan limit".into());
        }
        let (id, kind, owner, last_active) = row.map_err(|e| e.to_string())?;
        if let Some(before) = exact_before {
            let Ok(active) = DateTime::parse_from_rfc3339(&last_active) else {
                continue;
            };
            if active >= before {
                continue;
            }
        }
        let class = classify(
            &id,
            origin(kind, owner).as_ref(),
            &scope.viewer_user_id,
            &owner_set,
        );
        if class == HistoryClass::Hidden {
            continue;
        }
        let visible = messages(&conn, &id, scope.since)?;
        if visible.is_empty() {
            continue;
        }
        let turns = visible.len();
        let title: Vec<SessionMessage> = visible
            .into_iter()
            .find(|m| m.role == "user")
            .into_iter()
            .collect();
        title_bytes = title_bytes.saturating_add(title.iter().map(|m| m.content.len()).sum());
        if title_bytes > 8 * 1024 * 1024 {
            return Err("history page too large".into());
        }
        sessions.push(ReadSession {
            id,
            class,
            last_active,
            rows: title,
            turns,
        });
        if sessions.len() == limit {
            break;
        }
    }
    let next_before = if sessions.len() == limit {
        sessions.last().map(|s| {
            URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(&(&s.last_active, &s.id)).expect("string cursor"))
        })
    } else {
        None
    };
    Ok(Page {
        sessions,
        next_before,
    })
}

#[cfg(test)]
#[path = "reader_tests.rs"]
mod tests;
