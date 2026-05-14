//! Rule abstraction — what to match and how to label what was matched.
//!
//! Every concrete rule type (regex, identity, keyword, json_path, ner)
//! produces the same shape of [`Match`] and obeys the same
//! [`RestoreScope`] contract. The [`crate::engine::RuleEngine`] applies
//! a collection of rules and resolves overlaps.

pub mod regex;

use serde::{Deserialize, Serialize};

use crate::source::Caller;

pub use self::regex::RegexRule;

/// A single PII span detected by a rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    /// Byte offset in the input where the match begins.
    pub start: usize,
    /// Byte offset (exclusive) where the match ends.
    pub end: usize,
    /// The original substring (`text[start..end]`).
    pub original: String,
}

/// Who is allowed to see the original value when restoring this token.
///
/// `Owner` covers the channel's end-user — when a user opens a channel,
/// asks something, and we restore the reply *to their own channel*, they
/// always count as `Owner`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RestoreScope {
    /// Channel end-user + anyone with `RedactionAdmin` scope.
    #[default]
    Owner,
    /// Any caller with this scope.
    AnyScope { scope: String },
    /// Caller must have all listed scopes.
    AllScopes { scopes: Vec<String> },
}

impl RestoreScope {
    /// Decide if `caller` may receive the cleartext value.
    pub fn allows(&self, caller: &Caller) -> bool {
        if caller.has_scope("RedactionAdmin") {
            return true;
        }
        match self {
            RestoreScope::Owner => caller.is_owner,
            RestoreScope::AnyScope { scope } => caller.has_scope(scope),
            RestoreScope::AllScopes { scopes } => scopes.iter().all(|s| caller.has_scope(s)),
        }
    }

    /// Stable string for audit logs.
    pub fn wire(&self) -> String {
        match self {
            RestoreScope::Owner => "owner".to_string(),
            RestoreScope::AnyScope { scope } => format!("any:{scope}"),
            RestoreScope::AllScopes { scopes } => format!("all:{}", scopes.join(",")),
        }
    }
}


/// Concrete matcher payload. Each variant maps to a `Rule` implementation.
///
/// New variants are added as MVP+1 rule types land; existing variants
/// MUST keep the same wire form (toml-side `type = "..."`) so older
/// profiles keep parsing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuleKind {
    /// Pure regex match. `pattern` is compiled at load time.
    Regex { pattern: String },

    /// Match the canonical display name (and aliases) of a known person
    /// resolved by `duduclaw-identity`. v1.14.x and later.
    Identity { source: String },

    /// Literal keyword list. v1.14.x and later.
    Keyword {
        values: Vec<String>,
        #[serde(default = "default_true")]
        case_sensitive: bool,
    },

    /// JSON-path applied to structured tool results. v1.14.x and later.
    JsonPath {
        paths: Vec<String>,
        #[serde(default)]
        match_tool: Option<String>,
    },
}

fn default_true() -> bool {
    true
}

/// Operator-facing rule spec — what gets parsed from `agent.toml` / profile
/// files. The engine compiles a [`RuleSpec`] into a `Box<dyn Rule>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleSpec {
    /// Stable id (toml key). Used in audit logs and conflict resolution.
    /// When parsed from a profile / config the id is normally absent from
    /// the body and is filled in from the toml table key by the loader.
    #[serde(default)]
    pub id: String,

    /// Category name carried in the token (`<REDACT:CATEGORY:hash>`).
    pub category: String,

    /// Who can see the original.
    #[serde(default)]
    pub restore_scope: RestoreScope,

    /// Sort order when two rules overlap. Higher wins.
    #[serde(default = "default_priority")]
    pub priority: i32,

    /// If true, the token is salted with a stable per-agent key instead of
    /// the per-session salt — same value produces the same token across
    /// sessions. Use for organisational vocabulary (project codenames),
    /// not personal data.
    #[serde(default)]
    pub cross_session_stable: bool,

    /// Whether this rule also applies to system-prompt source. Default
    /// `false`: system-prompt redaction only happens for opted-in rules.
    #[serde(default)]
    pub apply_to_system_prompt: bool,

    /// The actual matcher.
    #[serde(flatten)]
    pub kind: RuleKind,
}

fn default_priority() -> i32 {
    50
}

/// The compiled, runtime form of a rule. Implementors are cheap to clone
/// (typically `Arc<...>` internally) and `Send + Sync`.
pub trait Rule: Send + Sync + std::fmt::Debug {
    /// Stable identifier — matches [`RuleSpec::id`].
    fn id(&self) -> &str;

    /// Category to carry in the token.
    fn category(&self) -> &str;

    /// Who can restore.
    fn restore_scope(&self) -> &RestoreScope;

    /// Conflict-resolution priority.
    fn priority(&self) -> i32;

    /// Cross-session stable flag.
    fn cross_session_stable(&self) -> bool;

    /// Does this rule fire against system-prompt sources?
    fn apply_to_system_prompt(&self) -> bool;

    /// Find all matches in `text`. Implementors MAY return overlapping
    /// spans; the engine resolves overlaps globally.
    fn match_text(&self, text: &str) -> Vec<Match>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_scope_admits_owner_and_admin() {
        let scope = RestoreScope::Owner;
        let owner = Caller::owner("a");
        let admin = Caller::agent("a", vec!["RedactionAdmin".into()]);
        let outsider = Caller::agent("b", vec!["FinanceRead".into()]);

        assert!(scope.allows(&owner));
        assert!(scope.allows(&admin));
        assert!(!scope.allows(&outsider));
    }

    #[test]
    fn any_scope_matches_when_caller_has_it() {
        let scope = RestoreScope::AnyScope { scope: "CustomerRead".into() };
        let c = Caller::agent("a", vec!["CustomerRead".into()]);
        assert!(scope.allows(&c));

        let c2 = Caller::agent("a", vec!["FinanceRead".into()]);
        assert!(!scope.allows(&c2));
    }

    #[test]
    fn all_scopes_requires_all_of_them() {
        let scope = RestoreScope::AllScopes {
            scopes: vec!["A".into(), "B".into()],
        };
        let with_both = Caller::agent("a", vec!["A".into(), "B".into()]);
        let with_one = Caller::agent("a", vec!["A".into()]);
        assert!(scope.allows(&with_both));
        assert!(!scope.allows(&with_one));
    }

    #[test]
    fn restore_scope_default_is_owner() {
        assert_eq!(RestoreScope::default(), RestoreScope::Owner);
    }
}
