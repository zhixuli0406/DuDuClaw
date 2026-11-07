//! Per-account submitted skill history. No message text or skill content is stored.
use std::path::Path;

use rusqlite::{params, Connection};

fn open(home: &Path) -> rusqlite::Result<Connection> {
    let db = Connection::open(home.join("recent_skills.db"))?;
    db.busy_timeout(std::time::Duration::from_secs(3))?;
    db.execute_batch("CREATE TABLE IF NOT EXISTS recent_skills (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        user_id TEXT NOT NULL, agent_id TEXT NOT NULL, name TEXT NOT NULL,
        UNIQUE(user_id, agent_id, name)
    ); CREATE INDEX IF NOT EXISTS recent_skills_user ON recent_skills(user_id, id DESC);")?;
    Ok(db)
}

pub(crate) fn slash_names(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    for token in text.split_whitespace() {
        let Some(name) = token.strip_prefix('/') else { continue; };
        if duduclaw_agent::skill_state::validate_skill_name(name).is_ok()
            && !names.iter().any(|n| n == name)
        {
            names.push(name.to_string());
        }
        if names.len() == 20 { break; }
    }
    names
}

pub(crate) fn record(home: &Path, user_id: &str, agent_id: &str, names: &[String]) -> rusqlite::Result<()> {
    if names.is_empty() { return Ok(()); }
    let mut db = open(home)?;
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    for name in names.iter().rev().take(20) {
        tx.execute("INSERT OR REPLACE INTO recent_skills(user_id, agent_id, name) VALUES (?1, ?2, ?3)",
            params![user_id, agent_id, name])?;
    }
    tx.execute("DELETE FROM recent_skills WHERE user_id=?1 AND id NOT IN
        (SELECT id FROM recent_skills WHERE user_id=?1 ORDER BY id DESC LIMIT 100)", [user_id])?;
    tx.commit()
}

pub(crate) fn list(home: &Path, user_id: &str) -> rusqlite::Result<Vec<(String, String)>> {
    if !home.join("recent_skills.db").exists() { return Ok(Vec::new()); }
    let db = open(home)?;
    let mut stmt = db.prepare("SELECT agent_id, name FROM recent_skills WHERE user_id=?1 ORDER BY id DESC LIMIT 100")?;
    let rows = stmt.query_map([user_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect()
}

/// Only loaded, enabled skills on the authorized target count as a chat use.
pub(crate) async fn record_chat(
    home: &Path, registry: &tokio::sync::RwLock<duduclaw_agent::registry::AgentRegistry>,
    user_id: &str, agent_id: &str, text: &str,
) {
    let names = slash_names(text);
    if names.is_empty() { return; }
    let state = duduclaw_agent::skill_state::read_disabled(home, agent_id);
    if state.error().is_some() { return; }
    let reg = registry.read().await;
    let Some(agent) = reg.get(agent_id) else { return; };
    let names: Vec<_> = names.into_iter().filter(|name|
        !state.is_disabled(name) && agent.skills.iter().any(|skill| skill.name == *name)
    ).collect();
    if let Err(error) = record(home, user_id, agent_id, &names) {
        tracing::warn!(%error, "recent skill history could not be saved");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_skills_are_private_deduplicated_and_ordered() {
        let home = tempfile::tempdir().unwrap();
        record(home.path(), "alice", "a", &["one".into(), "two".into()]).unwrap();
        record(home.path(), "bob", "a", &["private".into()]).unwrap();
        record(home.path(), "alice", "a", &["two".into()]).unwrap();
        assert_eq!(list(home.path(), "alice").unwrap(), vec![("a".into(), "two".into()), ("a".into(), "one".into())]);
        assert!(list(home.path(), "nobody").unwrap().is_empty());
    }

    #[test]
    fn recent_skills_are_bounded_per_user() {
        let home = tempfile::tempdir().unwrap();
        for n in 0..110 { record(home.path(), "alice", "a", &[format!("skill-{n}")]).unwrap(); }
        assert_eq!(list(home.path(), "alice").unwrap().len(), 100);
    }

    #[test]
    fn recent_skills_parse_whole_slash_tokens_only() {
        assert_eq!(slash_names("/quote /pdf detail /quote https://host/pdf a/pdf /../x"), vec!["quote", "pdf"]);
    }
}
