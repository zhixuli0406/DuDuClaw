//! `forget-source list`: what an operator picks a source from (P2-B).
//!
//! Without `--session` each line is one conversation (or one scheduled-run
//! series, one imported file); with `--session` each line is one message or
//! one run of it. A memory row is counted once per line, however many of its
//! sources fall on that line. What an employee stored itself during a turn
//! (`mcp_turn`) is shown under the user message that started the turn when
//! the lineage recorded the two together, because forgetting that message
//! forgets the turn too.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_gateway::memory_forget_steps as steps;

use super::{open_engine, valid_namespace};

/// Lineage kinds that are markers, never a source an operator can forget.
const NOT_SOURCES: &str = "('system', 'untracked_parent', 'upstream_unknown')";

/// The order kinds are named in on a line.
const KIND_ORDER: &[&str] = &[
    "channel_message",
    "mcp_turn",
    "dispatch_run",
    "mcp_external",
    "import_item",
    "footprint_day",
];

/// Human name of a `source_kind`.
pub(crate) fn kind_label(kind: &str) -> &'static str {
    match kind {
        "channel_message" => "聊天訊息",
        "mcp_turn" => "員工自行存入",
        "dispatch_run" => "排程／派工執行",
        "mcp_external" => "外部 MCP 用戶端",
        "import_item" => "匯入",
        "footprint_day" => "足跡",
        _ => "其他",
    }
}

fn db_err(e: rusqlite::Error) -> DuDuClawError {
    DuDuClawError::Memory(e.to_string())
}

pub(super) async fn list(home: &Path, agent: &str, session: Option<&str>) -> Result<String> {
    if !valid_namespace(home, agent) {
        return Err(DuDuClawError::Agent(format!("不認得的命名空間：{agent}")));
    }
    let engine = open_engine(home)?;
    let conn = engine.conn_for_maintenance().await;
    match session {
        None => list_sessions(&conn, agent),
        Some(s) => list_messages(home, &conn, agent, s),
    }
}

fn list_sessions(conn: &rusqlite::Connection, agent: &str) -> Result<String> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT source_session, GROUP_CONCAT(DISTINCT source_kind),
                    COUNT(DISTINCT memory_store || ':' || memory_id), MAX(source_observed_at)
             FROM memory_origins
             WHERE agent_id = ?1 AND source_kind NOT IN {NOT_SOURCES}
             GROUP BY source_session
             ORDER BY MAX(source_observed_at) DESC LIMIT 200"
        ))
        .map_err(db_err)?;
    let rows: Vec<(String, String, i64, String)> = stmt
        .query_map([agent], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .and_then(|it| it.collect())
        .map_err(db_err)?;
    let mut o = format!(
        "{agent} 的記憶來源：每一行是一段對話（或一系列排程執行、一個匯入檔），\
         依最近時間排列，最多 200 行。\n\
         筆數是目前帶著這個來源的記憶，同一筆記憶在一行裡只算一次；方括號是這些記憶的來源種類。\n"
    );
    if rows.is_empty() {
        o.push_str("  （沒有任何有來源紀錄的記憶）\n");
        return Ok(o);
    }
    for (session, kinds, n, at) in rows {
        let present: BTreeSet<&str> = kinds.split(',').collect();
        let mut labels: Vec<&str> = KIND_ORDER
            .iter()
            .filter(|k| present.contains(**k))
            .map(|k| kind_label(k))
            .collect();
        if present.iter().any(|k| !KIND_ORDER.contains(k)) {
            labels.push(kind_label(""));
        }
        o.push_str(&format!(
            "  {session}  [{}]  {n} 筆  最近 {}\n",
            labels.join("、"),
            when(&at)
        ));
    }
    o.push_str("要看某一段對話裡的個別訊息或執行：\n  duduclaw memory forget-source list --agent ");
    o.push_str(&format!("{agent} --session '<對話代號>'\n"));
    Ok(o)
}

/// Sort key of a message key: `m:<seq>` by its number (`m:9` before
/// `m:10`), anything else after every numbered message, by text.
pub(super) fn message_order(key: &str) -> (i64, &str) {
    match key.strip_prefix("m:").and_then(|n| n.parse::<i64>().ok()) {
        Some(n) => (n, ""),
        None => (i64::MAX, key),
    }
}

/// One line of `list --session`.
#[derive(Default)]
struct Group {
    memories: BTreeSet<String>,
    kinds: BTreeSet<String>,
    latest: String,
    has_turn: bool,
}

fn list_messages(
    home: &Path,
    conn: &rusqlite::Connection,
    agent: &str,
    session: &str,
) -> Result<String> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT memory_store || ':' || memory_id, source_kind, source_message,
                    source_observed_at, role
             FROM memory_origins
             WHERE agent_id = ?1 AND source_session = ?2 AND source_kind NOT IN {NOT_SOURCES}"
        ))
        .map_err(db_err)?;
    let rows: Vec<(String, String, String, String, String)> = stmt
        .query_map([agent, session], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .and_then(|it| it.collect())
        .map_err(db_err)?;

    // Turn → user message, from rows where one write recorded both directly.
    let mut direct: HashMap<&str, (Vec<&str>, Vec<&str>)> = HashMap::new();
    for (mem, kind, key, _, role) in &rows {
        if role != "direct" {
            continue;
        }
        let e = direct.entry(mem.as_str()).or_default();
        match kind.as_str() {
            "channel_message" => e.0.push(key.as_str()),
            "mcp_turn" => e.1.push(key.as_str()),
            _ => {}
        }
    }
    let mut turn_of: BTreeMap<&str, &str> = BTreeMap::new();
    for (msgs, turns) in direct.values() {
        if let Some(m) = msgs.iter().min_by_key(|k| message_order(k)) {
            for t in turns {
                let slot = turn_of.entry(*t).or_insert(*m);
                if message_order(m) < message_order(slot) {
                    *slot = *m;
                }
            }
        }
    }

    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    for (mem, kind, key, at, _) in &rows {
        let line = turn_of.get(key.as_str()).copied().unwrap_or(key.as_str());
        let g = groups.entry(line.to_string()).or_default();
        g.memories.insert(mem.clone());
        g.kinds.insert(kind.clone());
        g.has_turn |= kind == "mcp_turn";
        if *at > g.latest {
            g.latest = at.clone();
        }
    }
    let mut ordered: Vec<(String, Group)> = groups.into_iter().collect();
    ordered.sort_by(|a, b| b.1.latest.cmp(&a.1.latest).then(a.0.cmp(&b.0)));
    ordered.truncate(500);

    let mut o = format!(
        "{agent} 在對話 {session} 的記憶來源：每一行是一則訊息或一次執行，依最近時間排列，最多 500 行。\n\
         員工在某一輪對話中自行存入的記憶，若記錄得到觸發那一輪的使用者訊息，就併在那則訊息底下\
         （忘記那則訊息時一起忘記）。筆數是目前帶著這個來源的記憶，同一筆記憶在一行裡只算一次。\n"
    );
    if ordered.is_empty() {
        o.push_str("  （沒有任何有來源紀錄的記憶）\n");
        return Ok(o);
    }
    let db = steps::sessions_db_path(home);
    for (key, g) in &ordered {
        let (label, arg) = line_label(&db, session, key, g);
        o.push_str(&format!(
            "  {label}  {} 筆  最近 {}  → --message {arg}\n",
            g.memories.len(),
            when(&g.latest)
        ));
    }
    o.push_str(&format!(
        "建立計畫時把箭頭後的值交給 --message，例如：\n  duduclaw memory forget-source plan --agent {agent} \
         --session '{}' --message 5\n訊息序號寫 5 或 m:5 都可以；不加 --message 則忘記整段對話。\n",
        session.replace('\'', "'\\''")
    ));
    Ok(o)
}

fn line_label(db: &Path, session: &str, key: &str, g: &Group) -> (String, String) {
    if let Some(n) = key.strip_prefix("m:").and_then(|n| n.parse::<i64>().ok()) {
        let role = steps::message_label(db, session, n)
            .map(|(r, _)| match r.as_str() {
                "user" => "使用者訊息",
                "assistant" => "員工回覆",
                _ => "訊息",
            })
            .unwrap_or("訊息");
        let extra = if g.has_turn {
            "（含員工在這一輪自行存入的記憶）"
        } else {
            ""
        };
        return (format!("#{n} {role}{extra}"), n.to_string());
    }
    let kinds: Vec<&str> = g.kinds.iter().map(|k| kind_label(k)).collect();
    let what = if key.starts_with("turn:") {
        "員工在一輪對話中自行存入（記錄裡對不到觸發的訊息）".to_string()
    } else {
        kinds.join("、")
    };
    (format!("{key}  {what}"), key.to_string())
}

/// `2026-10-05T01:02:03.000000Z` → `2026-10-05 01:02:03`.
fn when(at: &str) -> String {
    duduclaw_core::truncate_chars(at, 19).replace('T', " ")
}
