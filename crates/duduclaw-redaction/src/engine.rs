//! Rule set + overlap resolution.
//!
//! The engine owns a flat `Vec<Arc<dyn Rule>>`. When `apply()` is called
//! it asks every rule for matches, then resolves overlaps with a stable
//! deterministic algorithm:
//!
//! 1. Sort all matches by `(priority desc, start asc, rule_id asc)`.
//! 2. Walk the sorted list keeping a "covered" interval set.
//! 3. Drop any match whose span overlaps an already-kept span.
//!
//! This is intentionally simple — no nesting, no partial-overlap merging.
//! Profile authors are expected to give overlapping rules sensible
//! priorities (e.g. `tw_national_id` priority 100 wins over a more general
//! `digits_8_to_12` priority 50).

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

use crate::config::SourceSetting;
use crate::data_source::DataSource;
use crate::error::{RedactionError, Result};
use crate::pipeline::ToolContext;
use crate::rules::{Match, Rule, RuleKind, RuleSpec};
use crate::rules::db_field;
use crate::rules::identity::IdentityRule;
use crate::rules::json_path::JsonPathRule;
use crate::rules::keyword::KeywordRule;
use crate::rules::regex::RegexRule;
use crate::source::Source;

/// One winning match plus the rule that produced it.
#[derive(Debug, Clone)]
pub struct MatchedSpan {
    pub rule: Arc<dyn Rule>,
    pub span: Match,
}

/// One node selected by a structured (JsonPath) rule.
#[derive(Debug, Clone)]
pub struct StructuredHit {
    /// RFC-6901 pointer into the value that was resolved.
    pub pointer: String,
    /// The rule that selected it — carries category, restore scope and the
    /// `exclude_keys` the pipeline applies while descending.
    pub rule: Arc<JsonPathRule>,
}

/// A collection of compiled rules with `apply()` that returns resolved
/// matches in left-to-right order.
///
/// Text rules and structured (JsonPath) rules are kept apart: a structured
/// rule's `match_text` is empty by construction, so running it through the
/// text path would be pure overhead, and the two passes resolve conflicts
/// differently (spans vs. pointers).
pub struct RuleEngine {
    rules: Vec<Arc<dyn Rule>>,
    structured: Vec<Arc<JsonPathRule>>,
}

/// Ambient resources some rule kinds need at compile time.
///
/// Kept as an explicit struct rather than extra parameters so adding the next
/// context-dependent rule kind does not churn every call site.
#[derive(Debug, Clone)]
pub struct EngineOptions {
    /// `<home>/shared/wiki/identity/people` — required by
    /// [`RuleKind::Identity`]. `None` ⇒ this engine was built in a context
    /// with no identity source (e.g. a unit test), and an identity rule fails
    /// to compile rather than silently matching nothing.
    pub identity_people_dir: Option<PathBuf>,

    /// Data-source registry consulted by [`RuleKind::DbField`] — built-ins
    /// plus the operator's `[redaction.data_sources.*]` entries. Defaults to
    /// the built-ins alone, so a caller that forgets to thread the config
    /// through loses custom sources loudly (unknown source ⇒ load error)
    /// rather than losing Odoo silently.
    pub data_sources: std::collections::HashMap<String, DataSource>,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            identity_people_dir: None,
            data_sources: crate::data_source::builtin_registry(),
        }
    }
}

impl RuleEngine {
    /// Compile a vector of [`RuleSpec`] into a runtime engine with default
    /// ambient resources: no identity directory and the **built-in data
    /// sources only**. Operator-defined `[redaction.data_sources.*]` entries
    /// reach the engine through [`RuleEngine::from_specs_with`].
    pub fn from_specs(specs: Vec<RuleSpec>) -> Result<Self> {
        Self::from_specs_with(specs, &EngineOptions::default())
    }

    /// Compile a vector of [`RuleSpec`] into a runtime engine.
    ///
    /// Every [`RuleKind`] either compiles or *fails the load*: a rule that
    /// silently vanished would leave an operator believing a field is masked
    /// when it is not. There is no warn-and-skip path.
    pub fn from_specs_with(specs: Vec<RuleSpec>, options: &EngineOptions) -> Result<Self> {
        // Dedup by rule id with last-wins semantics: when two profiles (or a
        // profile + inline config) define the same id, the later spec must
        // override the earlier one. We keep the original ordering of the
        // *first* occurrence so deterministic priority/ordering is stable,
        // but swap in the latest spec body for that id.
        let mut order: Vec<String> = Vec::new();
        let mut by_id: std::collections::HashMap<String, RuleSpec> =
            std::collections::HashMap::new();
        for spec in specs {
            if !by_id.contains_key(&spec.id) {
                order.push(spec.id.clone());
            }
            by_id.insert(spec.id.clone(), spec);
        }
        let specs: Vec<RuleSpec> = order
            .into_iter()
            .filter_map(|id| by_id.remove(&id))
            .collect();

        let mut rules: Vec<Arc<dyn Rule>> = Vec::new();
        let mut structured: Vec<Arc<JsonPathRule>> = Vec::new();
        for spec in specs {
            match &spec.kind {
                RuleKind::Regex { .. } => {
                    let rule = RegexRule::compile(spec)?;
                    rules.push(Arc::new(rule));
                }
                RuleKind::Keyword { .. } => {
                    let rule = KeywordRule::compile(spec)?;
                    rules.push(Arc::new(rule));
                }
                RuleKind::Identity { .. } => {
                    // Fail-closed: without a people directory we cannot know
                    // which names to mask, and a rule that matches nothing is
                    // indistinguishable from a working one at runtime.
                    let Some(people_dir) = options.identity_people_dir.clone() else {
                        return Err(RedactionError::rule_compile(
                            spec.id.clone(),
                            "identity source unavailable in this context \
                             (no identity people directory configured)",
                        ));
                    };
                    let rule = IdentityRule::compile(spec, people_dir)?;
                    rules.push(Arc::new(rule));
                }
                RuleKind::JsonPath { .. } => {
                    let rule = JsonPathRule::compile(spec)?;
                    structured.push(Arc::new(rule));
                }
                RuleKind::DbField { .. } => {
                    // Sugar: expand to one JsonPath spec per bound tool, then
                    // compile each. Expansion happens here (after the id dedup
                    // above) precisely so the expanded rules may share the
                    // operator's single rule id without colliding.
                    for expanded in db_field::expand(&spec, &options.data_sources)? {
                        structured.push(Arc::new(JsonPathRule::compile(expanded)?));
                    }
                }
            }
        }

        // Structured rules run in (priority desc, spec order) — a stable sort
        // keeps the config's ordering within one priority band, so which rule
        // claims a node first is deterministic.
        structured.sort_by_key(|r| std::cmp::Reverse(r.priority()));

        Ok(RuleEngine { rules, structured })
    }

    /// Number of compiled rules (text + structured).
    pub fn rule_count(&self) -> usize {
        self.rules.len() + self.structured.len()
    }

    /// `(rule id, category)` pairs for every compiled rule — feeds the
    /// dashboard's field picker so operators see exactly which fields the
    /// active profiles cover. Structured field rules are included so the
    /// catalogue covers `json_path` / `db_field` categories too.
    pub fn rule_catalogue(&self) -> Vec<(String, String)> {
        self.rules
            .iter()
            .map(|r| (r.id().to_string(), r.category().to_string()))
            .chain(
                self.structured
                    .iter()
                    .map(|r| (r.id().to_string(), r.category().to_string())),
            )
            .collect()
    }

    /// Number of compiled structured (JsonPath / db_field) rules.
    pub fn structured_rule_count(&self) -> usize {
        self.structured.len()
    }

    /// Resolve every structured rule that applies to this tool call against
    /// `value`, returning the selected nodes as JSON pointers.
    ///
    /// Four gates, all of which must pass: the rule's `match_tool` glob, its
    /// `match_args` equality table, its `match_result` equality table, and the
    /// source's category filter. Results come back in rule order (priority
    /// desc), and within a rule in path order — the pipeline relies on that
    /// being deterministic, because the first rule to claim a node is the one
    /// whose category the token carries.
    ///
    /// `match_result` is evaluated against **this** `value` — the same
    /// document the paths resolve on. The pipeline calls this once per value
    /// it descends into (the outer envelope, then each embedded JSON leaf), so
    /// a rule bound to `"/table" = "customers.csv"` fires exactly on the
    /// payload that says so and nowhere else.
    pub fn apply_structured(
        &self,
        value: &Value,
        ctx: &ToolContext<'_>,
        setting: &SourceSetting,
    ) -> Vec<StructuredHit> {
        let mut hits = Vec::new();
        for rule in &self.structured {
            if !rule.matches_tool(ctx.tool_name) {
                continue;
            }
            if !rule.matches_args(ctx.args) {
                continue;
            }
            if !rule.matches_result(value) {
                continue;
            }
            if !setting.allows_category(rule.category()) {
                continue;
            }
            for pointer in rule.resolve_pointers(value) {
                hits.push(StructuredHit {
                    pointer,
                    rule: rule.clone(),
                });
            }
        }
        hits
    }

    /// Apply all rules to `text` and return the resolved matches in
    /// left-to-right order.
    ///
    /// Pass a `source` so rules can opt out of certain sources (e.g.
    /// only system-prompt-aware rules fire on a `SystemPrompt` source
    /// with `selective` policy).
    pub fn apply(&self, text: &str, source: &Source) -> Vec<MatchedSpan> {
        let only_system_prompt_rules = matches!(source, Source::SystemPrompt { .. });

        let mut all: Vec<MatchedSpan> = Vec::new();
        for rule in &self.rules {
            if only_system_prompt_rules && !rule.apply_to_system_prompt() {
                continue;
            }
            for span in rule.match_text(text) {
                all.push(MatchedSpan {
                    rule: rule.clone(),
                    span,
                });
            }
        }

        resolve_overlaps(all)
    }
}

/// Pure helper exposed for unit testing. Given an arbitrary list of
/// matched spans (possibly overlapping), return the kept subset sorted
/// by `start` ascending.
pub(crate) fn resolve_overlaps(mut spans: Vec<MatchedSpan>) -> Vec<MatchedSpan> {
    // Sort by (priority desc, span.start asc, rule_id asc).
    spans.sort_by(|a, b| {
        b.rule
            .priority()
            .cmp(&a.rule.priority())
            .then_with(|| a.span.start.cmp(&b.span.start))
            .then_with(|| a.rule.id().cmp(b.rule.id()))
    });

    let mut kept: Vec<MatchedSpan> = Vec::with_capacity(spans.len());
    'outer: for candidate in spans {
        for existing in &kept {
            if overlaps(&candidate.span, &existing.span) {
                continue 'outer;
            }
        }
        kept.push(candidate);
    }

    // Final return order: left-to-right by start.
    kept.sort_by_key(|m| m.span.start);
    kept
}

fn overlaps(a: &Match, b: &Match) -> bool {
    a.start < b.end && b.start < a.end
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::RestoreScope;

    fn rspec(id: &str, pattern: &str, priority: i32) -> RuleSpec {
        RuleSpec {
            id: id.into(),
            category: id.to_uppercase(),
            restore_scope: RestoreScope::Owner,
            priority,
            cross_session_stable: false,
            apply_to_system_prompt: false,
            kind: RuleKind::Regex { pattern: pattern.into() },
        }
    }

    #[test]
    fn engine_applies_multiple_rules() {
        let engine = RuleEngine::from_specs(vec![
            rspec("email", r"[\w.+-]+@[\w-]+\.[\w.-]+", 50),
            rspec("phone", r"09\d{8}", 50),
        ])
        .unwrap();
        let hits = engine.apply(
            "contact alice@acme.com or 0912345678",
            &Source::ToolResult { tool_name: "x".into() },
        );
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].span.original, "alice@acme.com");
        assert_eq!(hits[1].span.original, "0912345678");
    }

    #[test]
    fn overlap_higher_priority_wins() {
        // Two rules match overlapping spans. priority 100 must win.
        let engine = RuleEngine::from_specs(vec![
            rspec("digits", r"\d{8,12}", 50),
            rspec("tw_id", r"[A-Z][12]\d{8}", 100),
        ])
        .unwrap();
        let hits = engine.apply("A123456789", &Source::ToolResult { tool_name: "x".into() });
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].rule.id(), "tw_id");
    }

    #[test]
    fn non_overlapping_matches_all_kept() {
        let engine = RuleEngine::from_specs(vec![
            rspec("digits", r"\d{4}", 50),
        ])
        .unwrap();
        let hits = engine.apply("1234 abcd 5678", &Source::ToolResult { tool_name: "x".into() });
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn left_to_right_order_after_resolve() {
        let engine = RuleEngine::from_specs(vec![
            rspec("a", r"foo", 50),
            rspec("b", r"bar", 50),
        ])
        .unwrap();
        let hits = engine.apply("bar foo bar", &Source::ToolResult { tool_name: "x".into() });
        let starts: Vec<usize> = hits.iter().map(|m| m.span.start).collect();
        let mut sorted = starts.clone();
        sorted.sort();
        assert_eq!(starts, sorted);
    }

    #[test]
    fn system_prompt_source_only_runs_opted_in_rules() {
        let mut opted_in = rspec("opt_in", r"X", 50);
        opted_in.apply_to_system_prompt = true;
        let opted_out = rspec("opt_out", r"X", 50);

        let engine = RuleEngine::from_specs(vec![opted_in, opted_out]).unwrap();

        let prompt_hits = engine.apply("X", &Source::SystemPrompt { component: "soul".into() });
        assert_eq!(prompt_hits.len(), 1);
        assert_eq!(prompt_hits[0].rule.id(), "opt_in");

        let tool_hits = engine.apply("X", &Source::ToolResult { tool_name: "x".into() });
        assert_eq!(tool_hits.len(), 1);
        // tool-source: which one wins is deterministic by id ordering
    }

    #[test]
    fn duplicate_rule_id_last_wins() {
        // Two specs share the id "dup"; the later one (priority 100, matching
        // "bar") must override the earlier one (priority 50, matching "foo").
        let engine = RuleEngine::from_specs(vec![
            rspec("dup", r"foo", 50),
            rspec("dup", r"bar", 100),
        ])
        .unwrap();
        assert_eq!(engine.rule_count(), 1, "duplicate id must compile to one rule");

        // The surviving rule matches "bar", not "foo".
        let hits_bar = engine.apply("bar", &Source::ToolResult { tool_name: "x".into() });
        assert_eq!(hits_bar.len(), 1);
        assert_eq!(hits_bar[0].rule.priority(), 100);

        let hits_foo = engine.apply("foo", &Source::ToolResult { tool_name: "x".into() });
        assert!(hits_foo.is_empty(), "overridden rule must no longer fire");
    }

    fn identity_spec(id: &str, source: &str) -> RuleSpec {
        RuleSpec {
            id: id.into(),
            category: "PERSON".into(),
            restore_scope: RestoreScope::Owner,
            priority: 70,
            cross_session_stable: false,
            apply_to_system_prompt: false,
            kind: RuleKind::Identity { source: source.into() },
        }
    }

    #[test]
    fn every_rule_kind_compiles_or_errors_loudly() {
        // No rule kind is warn-skipped any more: an identity rule with no
        // identity source available must FAIL the load, not vanish into a
        // silently empty engine (an operator would believe names are masked).
        let err = RuleEngine::from_specs(vec![identity_spec("people", "wiki")])
            .err()
            .expect("identity rule with no source must fail the load");
        match err {
            crate::error::RedactionError::RuleCompile { ref reason, .. } => {
                assert!(reason.contains("identity source unavailable"), "{reason}");
            }
            other => panic!("expected RuleCompile, got {other:?}"),
        }
    }

    #[test]
    fn identity_rule_compiles_when_a_people_dir_is_supplied() {
        let tmp = tempfile::TempDir::new().unwrap();
        let people = tmp.path().join("shared/wiki/identity/people");
        std::fs::create_dir_all(&people).unwrap();
        std::fs::write(
            people.join("ruby.md"),
            "---\nperson_id: p1\ndisplay_name: Ruby Lin\n---\n",
        )
        .unwrap();

        let engine = RuleEngine::from_specs_with(
            vec![identity_spec("people", "wiki")],
            &EngineOptions {
                identity_people_dir: Some(people),
                ..EngineOptions::default()
            },
        )
        .unwrap();

        assert_eq!(engine.rule_count(), 1);
        assert_eq!(engine.structured_rule_count(), 0);
        // It is a text rule like any other — catalogue and text path included.
        assert_eq!(engine.rule_catalogue(), vec![("people".into(), "PERSON".into())]);
        let hits = engine.apply(
            "Ruby Lin called",
            &Source::ToolResult { tool_name: "x".into() },
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].span.original, "Ruby Lin");
    }

    #[test]
    fn identity_rule_with_unknown_source_fails_the_load() {
        let tmp = tempfile::TempDir::new().unwrap();
        let people = tmp.path().join("people");
        std::fs::create_dir_all(&people).unwrap();
        let res = RuleEngine::from_specs_with(
            vec![identity_spec("people", "ldap")],
            &EngineOptions {
                identity_people_dir: Some(people),
                ..EngineOptions::default()
            },
        );
        assert!(matches!(
            res.err(),
            Some(crate::error::RedactionError::RuleCompile { .. })
        ));
    }

    fn json_path_spec(id: &str, paths: &[&str], tool: Option<&str>) -> RuleSpec {
        RuleSpec {
            id: id.into(),
            category: "DB_FIELD".into(),
            restore_scope: RestoreScope::Owner,
            priority: 50,
            cross_session_stable: false,
            apply_to_system_prompt: false,
            kind: RuleKind::JsonPath {
                paths: paths.iter().map(|s| s.to_string()).collect(),
                match_tool: tool.map(|s| s.to_string()),
                match_args: std::collections::HashMap::new(),
                match_result: Default::default(),
                exclude_keys: Vec::new(),
            },
        }
    }

    #[test]
    fn json_path_rule_kind_is_compiled() {
        let engine =
            RuleEngine::from_specs(vec![json_path_spec("field", &["$[*].name"], None)]).unwrap();
        assert_eq!(engine.rule_count(), 1);
        assert_eq!(engine.structured_rule_count(), 1);
        // It must not participate in the text path.
        assert!(
            engine
                .apply("name is Alice", &Source::ToolResult { tool_name: "x".into() })
                .is_empty()
        );
        assert_eq!(engine.rule_catalogue(), vec![("field".into(), "DB_FIELD".into())]);
    }

    #[test]
    fn malformed_json_path_fails_the_load() {
        let res = RuleEngine::from_specs(vec![json_path_spec("bad", &["name"], None)]);
        assert!(matches!(
            res.err(),
            Some(crate::error::RedactionError::RuleCompile { .. })
        ));
    }

    #[test]
    fn db_field_expands_to_several_structured_rules() {
        let spec = RuleSpec {
            id: "customers".into(),
            category: "DB_FIELD".into(),
            restore_scope: RestoreScope::Owner,
            priority: 70,
            cross_session_stable: false,
            apply_to_system_prompt: false,
            kind: RuleKind::DbField {
                source: Some("odoo".into()),
                connector: None,
                fields: vec!["res.partner.name".into()],
            },
        };
        let engine = RuleEngine::from_specs(vec![spec]).unwrap();
        // odoo_search + odoo_execute + odoo_partner_search.
        assert_eq!(engine.structured_rule_count(), 3);
        // All three share the operator's single rule id.
        assert!(
            engine
                .rule_catalogue()
                .iter()
                .all(|(id, _)| id == "customers")
        );
    }

    #[test]
    fn unknown_connector_fails_the_load() {
        let spec = RuleSpec {
            id: "bad".into(),
            category: "X".into(),
            restore_scope: RestoreScope::Owner,
            priority: 50,
            cross_session_stable: false,
            apply_to_system_prompt: false,
            kind: RuleKind::DbField {
                source: Some("sqlserver".into()),
                connector: None,
                fields: vec!["dbo.customer.name".into()],
            },
        };
        assert!(RuleEngine::from_specs(vec![spec]).is_err());
    }

    #[test]
    fn apply_structured_honours_tool_arg_and_category_gates() {
        use crate::config::{SourceMode, SourceSetting};

        let mut with_args = json_path_spec("f", &["$[*].name"], Some("odoo_*"));
        with_args.kind = RuleKind::JsonPath {
            paths: vec!["$[*].name".into()],
            match_tool: Some("odoo_*".into()),
            match_args: [("model".to_string(), "res.partner".to_string())]
                .into_iter()
                .collect(),
            match_result: Default::default(),
            exclude_keys: vec![],
        };
        let engine = RuleEngine::from_specs(vec![with_args]).unwrap();

        let value = serde_json::json!([{"name": "Alice"}, {"name": "Bob"}]);
        let args = serde_json::json!({"model": "res.partner"});
        let open: SourceSetting = SourceMode::On.into();

        let ctx = ToolContext { tool_name: "odoo_search", args: Some(&args) };
        let hits = engine.apply_structured(&value, &ctx, &open);
        assert_eq!(
            hits.iter().map(|h| h.pointer.clone()).collect::<Vec<_>>(),
            vec!["/0/name", "/1/name"]
        );

        // Wrong tool.
        let ctx = ToolContext { tool_name: "memory_search", args: Some(&args) };
        assert!(engine.apply_structured(&value, &ctx, &open).is_empty());

        // Wrong model arg.
        let other = serde_json::json!({"model": "crm.lead"});
        let ctx = ToolContext { tool_name: "odoo_search", args: Some(&other) };
        assert!(engine.apply_structured(&value, &ctx, &open).is_empty());

        // Missing args entirely.
        let ctx = ToolContext { tool_name: "odoo_search", args: None };
        assert!(engine.apply_structured(&value, &ctx, &open).is_empty());

        // Category excluded for this source.
        let filtered = SourceSetting {
            mode: SourceMode::On,
            only_categories: vec![],
            exclude_categories: vec!["DB_FIELD".into()],
        };
        let ctx = ToolContext { tool_name: "odoo_search", args: Some(&args) };
        assert!(engine.apply_structured(&value, &ctx, &filtered).is_empty());
    }

    #[test]
    fn apply_structured_orders_by_priority_desc() {
        let mut low = json_path_spec("low", &["$.name"], None);
        low.priority = 10;
        let mut high = json_path_spec("high", &["$.name"], None);
        high.priority = 90;
        let engine = RuleEngine::from_specs(vec![low, high]).unwrap();

        let value = serde_json::json!({"name": "Alice"});
        let ctx = ToolContext { tool_name: "any", args: None };
        let hits = engine.apply_structured(&value, &ctx, &crate::config::SourceMode::On.into());
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].rule.id(), "high");
        assert_eq!(hits[1].rule.id(), "low");
    }

    #[test]
    fn keyword_rule_kind_is_compiled() {
        // WP2: keyword rules are now first-class, not skipped.
        let spec = RuleSpec {
            id: "customer".into(),
            category: "CUSTOMER".into(),
            restore_scope: RestoreScope::Owner,
            priority: 60,
            cross_session_stable: true,
            apply_to_system_prompt: false,
            kind: RuleKind::Keyword {
                values: vec!["Amazon".into()],
                case_sensitive: false,
            },
        };
        let engine = RuleEngine::from_specs(vec![spec]).unwrap();
        assert_eq!(engine.rule_count(), 1);
    }
}
