//! `agent.toml [knowledge] collections` from the dashboard.
//!
//! - `knowledge.agent.collections {agent_id}`: the collections granted to
//!   the employee (directly or through its department) and its current
//!   binding, for the employee edit page. Needs owner access to the
//!   employee (Admin: any).
//! - `agents.update {knowledge: {collections: [...] | null}}`: writes the
//!   binding. The list can only pick among granted collections, so it never
//!   reaches beyond what collection owners granted; `null` removes the key
//!   ("every granted collection") and `[]` turns knowledge off. A list that
//!   names a collection the employee was not granted is refused, and so is
//!   any non-empty list when the knowledge database cannot be read.

use std::path::Path;

use duduclaw_core::knowledge_agent::{KnowledgeScope, knowledge_scope};
use duduclaw_knowledge::acl::{self, AgentCtx, AgentNarrowing, HumanCtx, Narrowing};
use duduclaw_knowledge::store::{self, CollectionSummary};
use duduclaw_knowledge::{KbResult, schema};
use serde_json::{Value, json};

use super::principals::AgentRef;

/// Most slugs one binding may list.
pub const MAX_BOUND_COLLECTIONS: usize = 200;

/// A validated `[knowledge] collections` value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Binding {
    /// Remove the key: every granted collection.
    AllGranted,
    /// Exactly these slugs (empty = off).
    Only(Vec<String>),
}

/// Collections granted to the employee, ignoring its own binding.
pub fn granted_collections(home: &Path, agent: &AgentRef) -> KbResult<Vec<CollectionSummary>> {
    let conn = schema::open_home(home)?;
    // Human side "admin, 1:1": only the employee side decides.
    let human = HumanCtx::dashboard("", None, true);
    let agent_ctx = AgentCtx {
        agent_id: agent.id.clone(),
        department: agent.department.clone(),
        narrowing: AgentNarrowing::All,
    };
    let allowed = acl::effective_collections(&conn, &human, Some(&agent_ctx), &Narrowing::default())?;
    store::list_by_ids(&conn, allowed.ids())
}

/// `knowledge.agent.collections` answer.
pub fn describe(home: &Path, agent: &AgentRef, agent_dir: &Path) -> KbResult<Value> {
    let granted = granted_collections(home, agent)?;
    let (bound, unreadable) = match knowledge_scope(agent_dir) {
        KnowledgeScope::All => (Value::Null, false),
        KnowledgeScope::Only(list) => (json!(list), false),
        KnowledgeScope::Unreadable => (Value::Null, true),
    };
    let rows: Vec<Value> = granted
        .iter()
        .map(|c| {
            json!({
                "id": c.collection.id,
                "slug": c.collection.slug,
                "name": c.collection.name,
                "description": c.collection.description,
                "sensitivity": c.collection.sensitivity,
                "source_count": c.source_count,
            })
        })
        .collect();
    Ok(json!({ "granted": rows, "bound": bound, "unreadable": unreadable }))
}

fn parse_slugs(raw: &Value) -> Result<Option<Vec<String>>, String> {
    let obj = raw
        .as_object()
        .ok_or_else(|| "knowledge 必須是物件，例如 {\"collections\": [\"hr-handbook\"]}。".to_string())?;
    let Some(list) = obj.get("collections") else {
        return Err("knowledge 缺少 collections。".to_string());
    };
    if list.is_null() {
        return Ok(None);
    }
    let items = list
        .as_array()
        .ok_or_else(|| "collections 必須是集合代號的清單，或 null。".to_string())?;
    if items.len() > MAX_BOUND_COLLECTIONS {
        return Err(format!("一位 AI 員工最多綁定 {MAX_BOUND_COLLECTIONS} 個集合。"));
    }
    let mut out: Vec<String> = Vec::with_capacity(items.len());
    for item in items {
        let slug = item
            .as_str()
            .map(str::trim)
            .ok_or_else(|| "collections 只能放集合代號（文字）。".to_string())?;
        if !store::valid_slug(slug) {
            return Err(format!("集合代號「{}」格式不正確。", duduclaw_core::truncate_chars(slug, 64)));
        }
        if !out.iter().any(|s| s == slug) {
            out.push(slug.to_string());
        }
    }
    Ok(Some(out))
}

/// Validate `params.knowledge` for `agents.update`.
pub fn validate(home: &Path, agent: &AgentRef, raw: &Value) -> Result<Binding, String> {
    let Some(slugs) = parse_slugs(raw)? else {
        return Ok(Binding::AllGranted);
    };
    if slugs.is_empty() {
        return Ok(Binding::Only(slugs));
    }
    let granted = granted_collections(home, agent).map_err(|e| {
        tracing::warn!(agent = %agent.id, error = %e, "knowledge binding: could not read grants");
        "暫時無法確認這位 AI 員工可用的知識集合，請稍後再試。".to_string()
    })?;
    for slug in &slugs {
        if !granted.iter().any(|c| c.collection.slug == *slug) {
            return Err(format!(
                "集合「{}」沒有授權給這位 AI 員工或其部門，請先到文件知識庫的權限頁授權。",
                duduclaw_core::truncate_chars(slug, 64)
            ));
        }
    }
    Ok(Binding::Only(slugs))
}

/// Write the binding into an `agent.toml` table, keeping any other
/// `[knowledge]` keys. Returns the change lines.
pub fn apply_to_table(table: &mut toml::Table, binding: &Binding) -> Vec<String> {
    match binding {
        Binding::AllGranted => {
            let Some(section) = table.get_mut("knowledge").and_then(toml::Value::as_table_mut) else {
                return vec!["knowledge.collections cleared".to_string()];
            };
            section.remove("collections");
            if section.is_empty() {
                table.remove("knowledge");
            }
            vec!["knowledge.collections cleared".to_string()]
        }
        Binding::Only(slugs) => {
            let section = table
                .entry("knowledge")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
            if !section.is_table() {
                *section = toml::Value::Table(toml::map::Map::new());
            }
            if let Some(t) = section.as_table_mut() {
                t.insert(
                    "collections".into(),
                    toml::Value::Array(slugs.iter().cloned().map(toml::Value::String).collect()),
                );
            }
            vec![format!("knowledge.collections = {slugs:?}")]
        }
    }
}

#[cfg(test)]
#[path = "agent_binding_tests.rs"]
mod tests;
