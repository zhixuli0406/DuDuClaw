//! `agent.toml [knowledge]` — which document-knowledge collections an AI
//! employee uses.
//!
//! ```toml
//! [knowledge]
//! collections = ["hr-handbook", "product-faq"]   # slugs; [] = none
//! ```
//!
//! The section can only **narrow**: collection permissions are granted to
//! the employee (or its department) on the collection itself, and this list
//! picks among those. No section, or no `collections` key, means "every
//! collection granted to me"; `collections = []` turns the knowledge tools
//! off for this employee (they are then neither listed nor callable).
//!
//! The section sits outside `org_field_guard`'s editable allow-list, so an
//! employee cannot change it with Write/Edit, and `agent_update` does not
//! offer it.
//!
//! [`knowledge_scope`] reads it **fail-closed**: an unreadable or malformed
//! file, or a malformed section, yields [`KnowledgeScope::Unreadable`],
//! which every caller treats as "no collections".

use std::path::Path;

use serde::{Deserialize, Serialize};

/// The typed section, kept on `AgentConfig` so an `agent_update` round trip
/// preserves it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeAgentConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collections: Option<Vec<String>>,
}

/// What an employee's `[knowledge]` section allows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KnowledgeScope {
    /// Every collection granted to the employee.
    All,
    /// Only these slugs (exact match). Empty = knowledge off.
    Only(Vec<String>),
    /// The file or the section could not be read: treated as no collections.
    Unreadable,
}

impl KnowledgeScope {
    /// Whether the knowledge tools are off for this employee.
    pub fn is_off(&self) -> bool {
        match self {
            KnowledgeScope::All => false,
            KnowledgeScope::Only(list) => list.is_empty(),
            KnowledgeScope::Unreadable => true,
        }
    }
}

/// Parse the scope out of an `agent.toml` document.
pub fn parse_scope(text: &str) -> KnowledgeScope {
    let Ok(root) = text.parse::<toml::Table>() else {
        return KnowledgeScope::Unreadable;
    };
    match root.get("knowledge") {
        None => KnowledgeScope::All,
        Some(toml::Value::Table(t)) => match t.get("collections") {
            None => KnowledgeScope::All,
            Some(toml::Value::Array(items)) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    match item.as_str() {
                        Some(s) => out.push(s.trim().to_string()),
                        None => return KnowledgeScope::Unreadable,
                    }
                }
                KnowledgeScope::Only(out)
            }
            Some(_) => KnowledgeScope::Unreadable,
        },
        Some(_) => KnowledgeScope::Unreadable,
    }
}

/// Read the scope of the employee at `agent_dir`, from the same file
/// [`crate::agent_toml::load`] reads (the preset-resolved artifact when
/// there is one). A missing `agent.toml` is [`KnowledgeScope::Unreadable`]:
/// no such employee, no collections.
pub fn knowledge_scope(agent_dir: &Path) -> KnowledgeScope {
    match crate::agent_toml::read_effective_text(agent_dir) {
        Ok(text) => parse_scope(&text),
        Err(_) => KnowledgeScope::Unreadable,
    }
}

/// `[agent] department`, when it is a valid department name.
pub fn parse_department(text: &str) -> Option<String> {
    let root = text.parse::<toml::Table>().ok()?;
    let dept = root
        .get("agent")?
        .as_table()?
        .get("department")?
        .as_str()?
        .trim();
    crate::department::is_valid_department(dept).then(|| dept.to_string())
}

/// Scope and department of the employee at `agent_dir`, from one read.
pub fn load_agent_knowledge(agent_dir: &Path) -> (KnowledgeScope, Option<String>) {
    match crate::agent_toml::read_effective_text(agent_dir) {
        Ok(text) => (parse_scope(&text), parse_department(&text)),
        Err(_) => (KnowledgeScope::Unreadable, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes() {
        assert_eq!(parse_scope("[agent]\nname='a'\n"), KnowledgeScope::All);
        assert_eq!(parse_scope("[knowledge]\n"), KnowledgeScope::All);
        assert_eq!(
            parse_scope("[knowledge]\ncollections = ['hr', 'faq']\n"),
            KnowledgeScope::Only(vec!["hr".into(), "faq".into()])
        );
        assert!(parse_scope("[knowledge]\ncollections = []\n").is_off());
        assert_eq!(
            parse_scope("[knowledge]\ncollections = 'hr'\n"),
            KnowledgeScope::Unreadable
        );
        assert_eq!(
            parse_scope("[knowledge]\ncollections = [1]\n"),
            KnowledgeScope::Unreadable
        );
        assert_eq!(parse_scope("knowledge = 1\n"), KnowledgeScope::Unreadable);
        assert_eq!(parse_scope("[knowledge\n"), KnowledgeScope::Unreadable);
    }

    #[test]
    fn department() {
        assert_eq!(
            parse_department("[agent]\ndepartment = '人資'\n").as_deref(),
            Some("人資")
        );
        assert_eq!(parse_department("[agent]\ndepartment = 'a/b'\n"), None);
        assert_eq!(parse_department("[agent]\n"), None);
    }

    #[test]
    fn missing_file_is_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(knowledge_scope(dir.path()), KnowledgeScope::Unreadable);
        std::fs::write(
            dir.path().join("agent.toml"),
            "[knowledge]\ncollections=['a']\n",
        )
        .unwrap();
        assert_eq!(
            knowledge_scope(dir.path()),
            KnowledgeScope::Only(vec!["a".into()])
        );
    }
}
