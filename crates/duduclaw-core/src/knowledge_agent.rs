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
//!
//! The same section also says how channel replies use the knowledge base
//! ([`RetrievalSettings`]):
//!
//! ```toml
//! [knowledge]
//! auto_retrieve = true          # look up passages before answering (default on)
//! answer_mode = "assist"        # assist | kb_only
//! top_k = 5                     # passages put into the prompt, 1–8
//! inject_budget_chars = 6000    # characters of passages per turn, 500–12000
//! ```
//!
//! These keys are behaviour, not permission: a wrong type or an unknown
//! value falls back to the default (and `config.toml [knowledge]` can turn
//! automatic retrieval off for everyone).

use std::path::Path;

use serde::{Deserialize, Serialize};

/// The typed section, kept on `AgentConfig` so an `agent_update` round trip
/// preserves it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeAgentConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collections: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_retrieve: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inject_budget_chars: Option<u32>,
}

/// `[knowledge] answer_mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerMode {
    /// Passages are offered to the model with grounding rules (default).
    Assist,
    /// Answer only from passages: when nothing relevant is found the reply
    /// is a fixed "not found" text and the model is not called.
    KbOnly,
}

/// Default passages per turn.
pub const DEFAULT_TOP_K: usize = 5;
/// Most passages per turn.
pub const MAX_TOP_K: usize = 8;
/// Default characters of passages per turn.
pub const DEFAULT_INJECT_BUDGET_CHARS: usize = 6_000;
const MIN_INJECT_BUDGET_CHARS: usize = 500;
/// Hard ceiling on characters of passages per turn.
pub const MAX_INJECT_BUDGET_CHARS: usize = 12_000;

/// How an employee's channel replies use its collections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetrievalSettings {
    pub auto_retrieve: bool,
    pub answer_mode: AnswerMode,
    pub top_k: usize,
    pub inject_budget_chars: usize,
}

impl Default for RetrievalSettings {
    fn default() -> Self {
        Self {
            auto_retrieve: true,
            answer_mode: AnswerMode::Assist,
            top_k: DEFAULT_TOP_K,
            inject_budget_chars: DEFAULT_INJECT_BUDGET_CHARS,
        }
    }
}

/// Parse the retrieval keys of an `agent.toml` document. Anything missing,
/// wrong-typed or out of range resolves to the default or nearest bound.
pub fn parse_retrieval(text: &str) -> RetrievalSettings {
    let d = RetrievalSettings::default();
    let Some(t) = text
        .parse::<toml::Table>()
        .ok()
        .and_then(|root| root.get("knowledge").and_then(toml::Value::as_table).cloned())
    else {
        return d;
    };
    let int = |k: &str| t.get(k).and_then(toml::Value::as_integer).filter(|n| *n > 0);
    RetrievalSettings {
        auto_retrieve: t
            .get("auto_retrieve")
            .and_then(toml::Value::as_bool)
            .unwrap_or(d.auto_retrieve),
        answer_mode: match t.get("answer_mode").and_then(toml::Value::as_str) {
            Some("kb_only") => AnswerMode::KbOnly,
            _ => AnswerMode::Assist,
        },
        top_k: int("top_k")
            .map(|n| (n as usize).clamp(1, MAX_TOP_K))
            .unwrap_or(d.top_k),
        inject_budget_chars: int("inject_budget_chars")
            .map(|n| (n as usize).clamp(MIN_INJECT_BUDGET_CHARS, MAX_INJECT_BUDGET_CHARS))
            .unwrap_or(d.inject_budget_chars),
    }
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
    let (scope, dept, _) = load_agent_knowledge_settings(agent_dir);
    (scope, dept)
}

/// Scope, department and retrieval settings of the employee at
/// `agent_dir`, from one read.
pub fn load_agent_knowledge_settings(
    agent_dir: &Path,
) -> (KnowledgeScope, Option<String>, RetrievalSettings) {
    match crate::agent_toml::read_effective_text(agent_dir) {
        Ok(text) => (
            parse_scope(&text),
            parse_department(&text),
            parse_retrieval(&text),
        ),
        Err(_) => (KnowledgeScope::Unreadable, None, RetrievalSettings::default()),
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
    fn retrieval_settings() {
        assert_eq!(parse_retrieval(""), RetrievalSettings::default());
        let r = parse_retrieval(
            "[knowledge]\nauto_retrieve = false\nanswer_mode = 'kb_only'\ntop_k = 50\ninject_budget_chars = 10\n",
        );
        assert!(!r.auto_retrieve);
        assert_eq!(r.answer_mode, AnswerMode::KbOnly);
        assert_eq!(r.top_k, MAX_TOP_K);
        assert_eq!(r.inject_budget_chars, MIN_INJECT_BUDGET_CHARS);
        let r = parse_retrieval("[knowledge]\nanswer_mode = 'other'\ntop_k = 'x'\ninject_budget_chars = 99999\n");
        assert_eq!(r.answer_mode, AnswerMode::Assist);
        assert_eq!(r.top_k, DEFAULT_TOP_K);
        assert_eq!(r.inject_budget_chars, MAX_INJECT_BUDGET_CHARS);
        // The scope parser ignores the behaviour keys.
        assert_eq!(
            parse_scope("[knowledge]\nauto_retrieve = false\n"),
            KnowledgeScope::All
        );
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
