//! Rule abstraction — what to match and how to label what was matched.
//!
//! Every concrete rule type (regex, identity, keyword, json_path, db_field)
//! produces the same shape of [`Match`] and obeys the same
//! [`RestoreScope`] contract. The [`crate::engine::RuleEngine`] applies
//! a collection of rules and resolves overlaps.

pub mod db_field;
pub mod identity;
pub mod json_path;
pub mod keyword;
#[cfg(feature = "ner")]
pub mod ner;
pub mod regex;

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::source::Caller;

pub use self::identity::IdentityRule;
pub use self::json_path::JsonPathRule;
pub use self::keyword::KeywordRule;
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

    /// Match the display name of every person the deployment knows about,
    /// read from the shared wiki's identity directory by
    /// [`identity::IdentityRule`]. `source` accepts `"wiki"` (or omitted /
    /// empty, which means the same); any other value is a load-time error.
    Identity {
        /// Defaulted so the documented bare `type = "identity"` form parses —
        /// without this a profile omitting `source` failed deserialisation,
        /// and (before the fail-closed fix) that surfaced as "redaction not
        /// enabled" rather than as an error.
        #[serde(default)]
        source: String,
    },

    /// Literal keyword list. v1.14.x and later.
    Keyword {
        values: Vec<String>,
        #[serde(default = "default_true")]
        case_sensitive: bool,
    },

    /// JSON-path applied to structured tool results — the *structured field*
    /// rule kind (2026-09). Unlike the text matchers above, a JsonPath rule
    /// never inspects content: it selects nodes by position in the tool
    /// result's JSON and tokenises the whole value.
    ///
    /// Wire-compatible with the pre-2026-09 `{ paths, match_tool }` form —
    /// `match_args`, `match_result` and `exclude_keys` all default to empty.
    JsonPath {
        /// Path expressions (see [`json_path`] for the supported grammar).
        paths: Vec<String>,
        /// Exact tool name, or a trailing-`*` prefix glob (same semantics as
        /// `[redaction.tool_egress]`). `None` ⇒ any tool.
        #[serde(default)]
        match_tool: Option<String>,
        /// Top-level tool-argument equality gate: every `key = value` pair
        /// must be present in the call's `arguments` object and compare
        /// exactly equal (scalars stringified). Empty ⇒ no gate.
        #[serde(default)]
        match_args: HashMap<String, String>,
        /// Tool-**result** equality gate: every `<json pointer> = value` pair
        /// must resolve, inside the very JSON the paths are applied to, to a
        /// scalar equal to the literal. Empty ⇒ no gate.
        ///
        /// For tools that name their table in the result rather than in the
        /// arguments (`csv_read` / `xlsx_read` return
        /// `{"table": "customers.csv", "rows": […]}`), this is what binds a
        /// column rule to one table.
        #[serde(default)]
        match_result: HashMap<String, String>,
        /// Object keys never tokenised under a matched node. Applied at
        /// every level of the recursion, not just the top one.
        #[serde(default)]
        exclude_keys: Vec<String>,
    },

    /// Database `table.column` sugar (2026-09). Expanded at load time into
    /// one [`RuleKind::JsonPath`] rule per bound tool — see
    /// [`db_field::expand`].
    ///
    /// `source` names an entry of the data-source registry
    /// ([`crate::data_source`]): a built-in (`odoo` / `duduclaw_db` /
    /// `duduclaw_files`), or an
    /// operator's `[redaction.data_sources.<name>]` block. An unknown source
    /// is a load-time error (fail-closed, never skipped).
    DbField {
        /// Registry entry this rule's tables belong to. Omitted ⇒ `connector`
        /// if given, else `"odoo"` (the only source that existed before the
        /// registry, so old configs keep their meaning).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<String>,
        /// Deprecated spelling of `source`, kept so pre-registry configs
        /// (`connector = "odoo"`) keep parsing. Giving both with *different*
        /// values is an error rather than a silent winner.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        connector: Option<String>,
        /// `"table.column"` or `"table.*"` entries, e.g. `res.partner.name`
        /// or `customers.email`.
        fields: Vec<String>,
    },

    /// Model-backed detection of PII with no fixed shape — names, street
    /// addresses, birthdays, account numbers (2026-09, "AI 智慧偵測").
    ///
    /// Runs the OpenAI Privacy Filter (Apache-2.0) locally through ONNX
    /// Runtime; nothing is sent anywhere. One spec compiles into one rule per
    /// label, each carrying the DuDuClaw category that label maps to
    /// ([`crate::ner::labels`]) — so the spec's own `category` is unused.
    ///
    /// The variant exists in EVERY build, feature `ner` or not, so profiles
    /// stay wire-compatible: a build without the feature parses the rule and
    /// then refuses to compile it (fail-closed), rather than silently
    /// dropping a rule the operator believes is protecting them.
    Ner {
        /// Model labels to surface. Empty ⇒ all eight.
        #[serde(default)]
        labels: Vec<String>,
        /// Minimum input length (characters) before the model is consulted.
        /// `None` ⇒ `[redaction.ner] min_chars`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min_chars: Option<usize>,
        /// Chunk size (characters) for longer inputs. `None` ⇒
        /// `[redaction.ner] max_chars`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_chars: Option<usize>,
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

    /// Whether this rule is live. `false` ⇒ the engine skips it at compile
    /// time (every kind), so a dashboard toggle can park a rule without
    /// deleting it.
    ///
    /// Defaults to `true` so every pre-existing profile and inline rule keeps
    /// firing, and is ALWAYS serialised (no `skip_serializing_if`) so a rule
    /// written back by the dashboard states its state explicitly rather than
    /// relying on a default that a future edit could change.
    #[serde(default = "default_true")]
    pub enabled: bool,

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

    /// Which detection engine produced this rule's matches, for the audit
    /// trail: `"rule"` for every deterministic matcher, `"ner"` for the
    /// model. Defaults to `"rule"` so adding a matcher never silently
    /// mislabels itself as model output.
    fn engine_kind(&self) -> &'static str {
        "rule"
    }

    /// Model revision behind this rule, when `engine_kind()` is `"ner"`.
    /// `None` for deterministic rules — there is no model to attribute.
    fn model_revision(&self) -> Option<&str> {
        None
    }
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
    fn identity_source_may_be_omitted_in_toml() {
        // The documented bare form. Before `#[serde(default)]` this failed to
        // deserialise, which the MCP layer then read as "redaction not
        // enabled" — a silent unredacted path.
        #[derive(serde::Deserialize)]
        struct Wrap {
            rules: std::collections::HashMap<String, RuleSpec>,
        }
        let toml_src = r#"
[rules.known_people]
type = "identity"
category = "PERSON"
"#;
        let parsed: Wrap = toml::from_str(toml_src).expect("bare identity rule must parse");
        let spec = &parsed.rules["known_people"];
        assert_eq!(spec.category, "PERSON");
        assert_eq!(spec.kind, RuleKind::Identity { source: String::new() });

        // ...and an empty source compiles against a real people directory.
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("people");
        std::fs::create_dir_all(&dir).unwrap();
        let mut spec = spec.clone();
        spec.id = "known_people".into();
        assert!(crate::rules::identity::IdentityRule::compile(spec, dir).is_ok());
    }

    #[test]
    fn identity_source_still_round_trips_when_present() {
        let spec: RuleSpec = toml::from_str(
            "type = \"identity\"\ncategory = \"PERSON\"\nsource = \"wiki\"\n",
        )
        .unwrap();
        assert_eq!(spec.kind, RuleKind::Identity { source: "wiki".into() });
    }

    #[test]
    fn restore_scope_default_is_owner() {
        assert_eq!(RestoreScope::default(), RestoreScope::Owner);
    }
}
