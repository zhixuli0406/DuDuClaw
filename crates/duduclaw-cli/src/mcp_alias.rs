//! T5 / O3 · O4 · O13 — parameter parsing for the **merged** MCP entry points.
//!
//! The 2026-09-29 feature audit (`wiki/reports/feature-audit-2026-09-29.md`
//! §1 T5) found three families where the same capability was reachable
//! through several sibling tool names:
//!
//! | Audit row | Before | After |
//! |---|---|---|
//! | O3 | `wiki_*` (14) **and** a near-mirror `shared_wiki_*` (6) | one `wiki_*` set with a `scope` parameter |
//! | O4 | `create_task` / `tasks_create` / `goals_create` / `schedule_task` | `tasks_create` with `kind` + `schedule` |
//! | O13 | `skill_search` (hubs) / `skill_bank_search` / hub-only search | `skill_search` with `source` |
//!
//! ## Deprecation contract (not a removal)
//!
//! Every old name stays **listed in `tools/list` and callable, byte-identical**
//! for two minor versions (removal target **v1.69.0**). Only its
//! `description` gains a `[deprecated → <new tool> <param>]` prefix, and
//! `duduclaw_core::tool_catalog` marks it `deprecated: true`. Hiding a tool
//! from `tools/list` would make it *uncallable* — MCP's list is the
//! declaration surface, not a hint — so hiding is exactly the wrong move for
//! a deprecation window. See `docs/guides/deprecations.md`.
//!
//! ## Why the parsers live here rather than in `mcp.rs`
//!
//! `mcp.rs` is 32k lines and edited concurrently. These are small, total,
//! side-effect-free functions over `serde_json::Value`, so they are unit
//! testable without a home dir, an MCP server, or a tokio runtime — and the
//! dispatch arms in `mcp.rs` stay one-liners.
//!
//! Every parser is **fail-closed**: an unrecognised token is an error, never
//! a silent fallback to the permissive branch (coding convention 4).

use serde_json::Value;

// ── O3: wiki scope ──────────────────────────────────────────────────────

/// Which wiki a `wiki_*` call addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WikiScope {
    /// The calling agent's own wiki (`<home>/agents/<id>/wiki/`). The default,
    /// so an existing `wiki_read` call is byte-identical after the merge.
    #[default]
    Agent,
    /// The cross-agent shared wiki (`<home>/shared/wiki/`) — what the legacy
    /// `shared_wiki_*` aliases address.
    Shared,
}

impl WikiScope {
    /// Stable token. Never localise.
    pub fn as_str(self) -> &'static str {
        match self {
            WikiScope::Agent => "agent",
            WikiScope::Shared => "shared",
        }
    }
}

/// Resolve the effective wiki scope for one tool call.
///
/// - A legacy `shared_wiki_*` tool name always resolves to
///   [`WikiScope::Shared`], regardless of the arguments — the alias *is* the
///   scope, and letting a `scope` argument override it would turn a
///   deprecated shared-wiki alias into a back door onto an agent wiki.
/// - Otherwise the `scope` argument decides, defaulting to
///   [`WikiScope::Agent`].
/// - An unknown or non-string `scope` is an `Err` (fail-closed) — never a
///   silent fallback, because the two scopes have different trust boundaries
///   (`.scope.toml` SoT policy, author-or-main-agent delete rule).
pub fn resolve_wiki_scope(tool_name: &str, args: &Value) -> Result<WikiScope, String> {
    if tool_name.starts_with("shared_wiki_") {
        return Ok(WikiScope::Shared);
    }
    match args.get("scope") {
        None | Some(Value::Null) => Ok(WikiScope::Agent),
        Some(Value::String(s)) => match s.trim().to_ascii_lowercase().as_str() {
            "" | "agent" | "local" => Ok(WikiScope::Agent),
            "shared" => Ok(WikiScope::Shared),
            other => Err(format!(
                "unknown scope '{other}' — use \"agent\" (your own wiki, default) or \"shared\" (the cross-agent wiki)"
            )),
        },
        Some(_) => Err("scope must be a string: \"agent\" or \"shared\"".to_string()),
    }
}

// ── O4: task kind ───────────────────────────────────────────────────────

/// What `tasks_create` is being asked to create.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TaskKind {
    /// A Kanban board task. The default — an existing `tasks_create` call is
    /// byte-identical after the merge.
    #[default]
    Task,
    /// An autonomous goal: `goal_mode` task with a frozen acceptance contract,
    /// driven to completion by the goal loop. Same path as the dashboard
    /// `tasks.goal_create` RPC and the chat `/goal` command.
    Goal,
    Discovery,
}

impl TaskKind {
    /// Stable token. Never localise.
    pub fn as_str(self) -> &'static str {
        match self {
            TaskKind::Task => "task",
            TaskKind::Goal => "goal",
            TaskKind::Discovery => "discovery",
        }
    }
}

/// Resolve `tasks_create`'s `kind` argument. Fail-closed on an unknown value:
/// a typo'd `kind` must never quietly create a plain board task when the
/// caller asked for an autonomous goal (they have different acceptance and
/// escalation semantics).
pub fn resolve_task_kind(args: &Value) -> Result<TaskKind, String> {
    match args.get("kind") {
        None | Some(Value::Null) => Ok(TaskKind::Task),
        Some(Value::String(s)) => match s.trim().to_ascii_lowercase().as_str() {
            "" | "task" => Ok(TaskKind::Task),
            "goal" => Ok(TaskKind::Goal),
            "discovery" => Ok(TaskKind::Discovery),
            other => Err(format!(
                "unknown kind '{other}' — use \"task\" (Kanban board task, default) or \"goal\" (autonomous goal with judge acceptance)"
            )),
        },
        Some(_) => Err("kind must be a string: \"task\" or \"goal\"".to_string()),
    }
}

// ── O4: schedule ────────────────────────────────────────────────────────

/// How a `tasks_create` `schedule` value was understood.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleSpec {
    /// A cron expression (5-field minute precision or 6-field with seconds),
    /// handed to the persistent `CronScheduler` — the recurring rail that
    /// `schedule_task` has always used.
    Cron(String),
    /// A single RFC3339 instant, handed to the one-shot reminder scheduler
    /// (`agent_callback` delivery). Cron rows cannot express "once", so a
    /// timestamp deliberately takes the other rail rather than being coerced
    /// into an expression that would silently fire again next year.
    Once(String),
}

/// Classify a `schedule` argument.
///
/// Decision order is deliberate: an RFC3339 timestamp is tried **first**,
/// because `2026-10-01T09:00:00Z` contains no spaces and could never be a
/// valid cron expression, whereas a cron expression can never parse as
/// RFC3339. A value that is neither is an error naming both accepted shapes.
pub fn classify_schedule(raw: &str) -> Result<ScheduleSpec, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("schedule must not be empty".to_string());
    }
    if chrono::DateTime::parse_from_rfc3339(trimmed).is_ok() {
        return Ok(ScheduleSpec::Once(trimmed.to_string()));
    }
    let field_count = trimmed.split_whitespace().count();
    if (5..=6).contains(&field_count) {
        return Ok(ScheduleSpec::Cron(trimmed.to_string()));
    }
    Err(format!(
        "schedule '{trimmed}' is neither a cron expression (5 or 6 space-separated fields, e.g. \"0 9 * * *\") nor an RFC3339 instant (e.g. \"2026-10-01T09:00:00+08:00\")"
    ))
}

// ── O13: skill search source ────────────────────────────────────────────

/// Which skill index `skill_search` queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SkillSource {
    /// Hubs **and** the learned skill bank, de-duplicated by skill name.
    #[default]
    All,
    /// The `github` hub only (the GitHub Search API index).
    Github,
    /// The curated skill hubs (whatever `HubRegistry` has configured).
    Hub,
    /// This deployment's own learned skill bank.
    Bank,
}

impl SkillSource {
    /// Stable token. Never localise.
    pub fn as_str(self) -> &'static str {
        match self {
            SkillSource::All => "all",
            SkillSource::Github => "github",
            SkillSource::Hub => "hub",
            SkillSource::Bank => "bank",
        }
    }

    /// Does this source query the hub registry at all?
    pub fn queries_hubs(self) -> bool {
        matches!(self, SkillSource::All | SkillSource::Github | SkillSource::Hub)
    }

    /// Does this source query the learned skill bank at all?
    pub fn queries_bank(self) -> bool {
        matches!(self, SkillSource::All | SkillSource::Bank)
    }
}

/// Resolve `skill_search`'s `source` argument.
///
/// A legacy `skill_bank_search` call always resolves to [`SkillSource::Bank`]
/// — same reasoning as the wiki aliases: the alias *is* the source.
pub fn resolve_skill_source(tool_name: &str, args: &Value) -> Result<SkillSource, String> {
    if tool_name == "skill_bank_search" {
        return Ok(SkillSource::Bank);
    }
    match args.get("source") {
        None | Some(Value::Null) => Ok(SkillSource::All),
        Some(Value::String(s)) => match s.trim().to_ascii_lowercase().as_str() {
            "" | "all" => Ok(SkillSource::All),
            "github" => Ok(SkillSource::Github),
            "hub" | "hubs" => Ok(SkillSource::Hub),
            "bank" | "skill_bank" => Ok(SkillSource::Bank),
            other => Err(format!(
                "unknown source '{other}' — use \"all\" (default), \"github\", \"hub\", or \"bank\""
            )),
        },
        Some(_) => Err("source must be a string: all / github / hub / bank".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── O3 ──────────────────────────────────────────────────────────────

    #[test]
    fn wiki_scope_defaults_to_agent_so_existing_calls_are_byte_identical() {
        assert_eq!(
            resolve_wiki_scope("wiki_read", &json!({"page_path": "a.md"})).unwrap(),
            WikiScope::Agent
        );
        assert_eq!(
            resolve_wiki_scope("wiki_read", &json!({"scope": null})).unwrap(),
            WikiScope::Agent
        );
    }

    #[test]
    fn wiki_scope_shared_alias_ignores_a_conflicting_scope_argument() {
        // The deprecated alias must never become a back door onto an agent
        // wiki by passing scope="agent".
        assert_eq!(
            resolve_wiki_scope("shared_wiki_write", &json!({"scope": "agent"})).unwrap(),
            WikiScope::Shared
        );
        assert_eq!(
            resolve_wiki_scope("shared_wiki_ls", &json!({})).unwrap(),
            WikiScope::Shared
        );
    }

    #[test]
    fn wiki_scope_rejects_unknown_and_non_string_values() {
        assert!(resolve_wiki_scope("wiki_ls", &json!({"scope": "global"})).is_err());
        assert!(resolve_wiki_scope("wiki_ls", &json!({"scope": 1})).is_err());
        // Not a prefix/substring match: "sharedx" must not read as "shared".
        assert!(resolve_wiki_scope("wiki_ls", &json!({"scope": "sharedx"})).is_err());
    }

    #[test]
    fn wiki_scope_accepts_explicit_shared_on_the_merged_name() {
        assert_eq!(
            resolve_wiki_scope("wiki_search", &json!({"scope": " Shared "})).unwrap(),
            WikiScope::Shared
        );
    }

    // ── O4 ──────────────────────────────────────────────────────────────

    #[test]
    fn task_kind_defaults_to_task() {
        assert_eq!(resolve_task_kind(&json!({"title": "x"})).unwrap(), TaskKind::Task);
        assert_eq!(resolve_task_kind(&json!({"kind": ""})).unwrap(), TaskKind::Task);
    }

    #[test]
    fn task_kind_goal_is_recognised_and_typos_fail_closed() {
        assert_eq!(resolve_task_kind(&json!({"kind": "GOAL"})).unwrap(), TaskKind::Goal);
        assert!(resolve_task_kind(&json!({"kind": "goals"})).is_err());
        assert!(resolve_task_kind(&json!({"kind": true})).is_err());
    }

    #[test]
    fn schedule_classifies_cron_and_rfc3339_without_confusing_them() {
        assert_eq!(
            classify_schedule("0 9 * * *").unwrap(),
            ScheduleSpec::Cron("0 9 * * *".into())
        );
        assert_eq!(
            classify_schedule("0 0 9 * * *").unwrap(),
            ScheduleSpec::Cron("0 0 9 * * *".into())
        );
        assert_eq!(
            classify_schedule(" 2026-10-01T09:00:00+08:00 ").unwrap(),
            ScheduleSpec::Once("2026-10-01T09:00:00+08:00".into())
        );
    }

    #[test]
    fn schedule_rejects_shapes_that_are_neither() {
        assert!(classify_schedule("").is_err());
        assert!(classify_schedule("tomorrow").is_err());
        assert!(classify_schedule("0 9 * *").is_err()); // 4 fields
        assert!(classify_schedule("0 0 0 9 * * *").is_err()); // 7 fields
    }

    // ── O13 ─────────────────────────────────────────────────────────────

    #[test]
    fn skill_source_defaults_to_all_and_bank_alias_pins_bank() {
        assert_eq!(
            resolve_skill_source("skill_search", &json!({"query": "x"})).unwrap(),
            SkillSource::All
        );
        assert_eq!(
            resolve_skill_source("skill_bank_search", &json!({"source": "github"})).unwrap(),
            SkillSource::Bank
        );
    }

    #[test]
    fn skill_source_routing_flags_match_the_documented_rule() {
        assert!(SkillSource::All.queries_hubs() && SkillSource::All.queries_bank());
        assert!(SkillSource::Github.queries_hubs() && !SkillSource::Github.queries_bank());
        assert!(SkillSource::Hub.queries_hubs() && !SkillSource::Hub.queries_bank());
        assert!(!SkillSource::Bank.queries_hubs() && SkillSource::Bank.queries_bank());
    }

    #[test]
    fn skill_source_rejects_unknown_tokens() {
        assert!(resolve_skill_source("skill_search", &json!({"source": "npm"})).is_err());
        assert!(resolve_skill_source("skill_search", &json!({"source": 3})).is_err());
    }
}
