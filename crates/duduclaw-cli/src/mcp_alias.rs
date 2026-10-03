//! T5 / O3 · O4 · O13 — parameter parsing for the **merged** MCP entry points,
//! and the scan for tool names removed in v1.69.0.
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
//! The old names were deprecated aliases for two minor versions and were
//! removed in v1.69.0. `duduclaw_core::tool_catalog::REMOVED_MCP_TOOLS` is
//! the one table of them: a call to one gets a tool error naming the
//! replacement, and [`scan_agent_dir_for_removed_tools`] /
//! [`scan_config_for_removed_tools`] find names left in settings, prompts and
//! skills. See `docs/guides/deprecations.md`.
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
    /// The cross-agent shared wiki (`<home>/shared/wiki/`).
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

/// Resolve the effective wiki scope for one `wiki_*` call.
///
/// - The `scope` argument decides, defaulting to [`WikiScope::Agent`].
/// - An unknown or non-string `scope` is an `Err` (fail-closed) — never a
///   silent fallback, because the two scopes have different trust boundaries
///   (`.scope.toml` SoT policy, author-or-main-agent delete rule).
pub fn resolve_wiki_scope(args: &Value) -> Result<WikiScope, String> {
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
    /// handed to the persistent `CronScheduler` (the recurring rail).
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
pub fn resolve_skill_source(args: &Value) -> Result<SkillSource, String> {
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

// ── Removed tool names left in settings, prompts and skills ────────────

/// One place a removed MCP tool name is still written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovedToolFinding {
    /// File the name is in, relative to the scanned directory (or
    /// `config.toml` for [`scan_config_for_removed_tools`]).
    pub file: String,
    /// Where in that file: a settings key such as
    /// `[capabilities] denied_tools`, or `text` for prompt / skill prose.
    pub location: String,
    /// The removed tool name.
    pub tool: &'static str,
    /// How many times the name appears there.
    pub occurrences: usize,
    /// What the leftover does now and what to write instead (English,
    /// for the operator).
    pub advice: String,
}

/// Result of scanning one employee directory. `unreadable` names files that
/// exist but could not be read or parsed, so a clean `findings` list is never
/// mistaken for "checked and clean".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemovedToolScan {
    pub findings: Vec<RemovedToolFinding>,
    pub unreadable: Vec<String>,
}

/// `agent.toml [capabilities]` lists that name tools, and what a removed
/// name left in each one does after the upgrade.
const CAPABILITY_TOOL_LISTS: &[(&str, &str)] = &[
    (
        "allowed_tools",
        "matches no tool any more; if it was this employee's only grant, the employee has lost it. \
         Replace it with the new name (which also allows the call's other forms).",
    ),
    (
        "denied_tools",
        "the MCP gate still refuses the equivalent call, but the Claude CLI flag no longer matches. \
         Replace it with the new name to deny it everywhere (that also denies the call's other forms).",
    ),
    (
        "approval_required_tools",
        "the MCP gate still asks for this approval on the equivalent call. \
         Replace it with the new name (that also covers the call's other forms).",
    ),
    (
        "irreversible_tools",
        "the MCP gate still asks for this approval on the equivalent call. \
         Replace it with the new name (that also covers the call's other forms).",
    ),
    (
        "maybe_irreversible_tools",
        "the MCP gate still judges the equivalent call. \
         Replace it with the new name (that also covers the call's other forms).",
    ),
    (
        "scoped_tools",
        "the MCP gate still requires a task grant for the equivalent call. \
         Replace it with the new name (that also covers the call's other forms).",
    ),
];

/// Prompt files read at the directory root.
const PROMPT_FILES: &[&str] = &[
    "SOUL.md",
    "IDENTITY.md",
    "CLAUDE.md",
    "AGENTS.md",
    "GEMINI.md",
    "CONTRACT.toml",
];

/// Directories whose Markdown is read recursively.
const PROSE_DIRS: &[&str] = &["SKILLS", "wiki"];

const SCAN_MAX_FILE_BYTES: u64 = 1024 * 1024;
const SCAN_MAX_FILES: usize = 2_000;
const SCAN_MAX_DEPTH: usize = 8;

fn advice_for(location_effect: &str, row: &duduclaw_core::tool_catalog::RemovedMcpTool) -> String {
    format!(
        "{location_effect} New name: `{}`; call it with `{}`.",
        row.replacement, row.replacement_args
    )
}

/// Removed names a list entry reaches. A match-all entry (`*`,
/// `mcp__duduclaw__*`) is not a leftover; a prefix wildcard such as
/// `shared_wiki_*` is reported once per removed name it used to cover.
fn removed_names_for_entry(entry: &str) -> Vec<&'static duduclaw_core::tool_catalog::RemovedMcpTool> {
    use duduclaw_core::tool_catalog::{REMOVED_MCP_TOOLS, tool_entry_matches};
    const PROBE: &str = "zz-removed-tool-probe";
    if tool_entry_matches(entry, PROBE) {
        return Vec::new();
    }
    REMOVED_MCP_TOOLS
        .iter()
        .filter(|row| tool_entry_matches(entry, row.name))
        .collect()
}

/// Occurrences of `name` in `text` as a whole tool name: not preceded or
/// followed by an identifier character, except that the Claude CLI prefix
/// `mcp__duduclaw__` may precede it.
fn count_tool_name(text: &str, name: &str) -> usize {
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut count = 0;
    let mut from = 0;
    while let Some(pos) = text[from..].find(name) {
        let start = from + pos;
        let end = start + name.len();
        let before = &text[..start];
        let before_ok = before.chars().next_back().is_none_or(|c| !is_ident(c))
            || before.ends_with("mcp__duduclaw__");
        let after_ok = text[end..].chars().next().is_none_or(|c| !is_ident(c));
        if before_ok && after_ok {
            count += 1;
        }
        from = end;
    }
    count
}

fn scan_text(rel: String, text: &str, out: &mut Vec<RemovedToolFinding>) {
    for row in duduclaw_core::tool_catalog::REMOVED_MCP_TOOLS {
        let occurrences = count_tool_name(text, row.name);
        if occurrences > 0 {
            out.push(RemovedToolFinding {
                file: rel.clone(),
                location: "text".to_string(),
                tool: row.name,
                occurrences,
                advice: advice_for(
                    "Prompt or skill text names a removed tool; a model following it gets an error naming the replacement.",
                    row,
                ),
            });
        }
    }
}

/// Read a regular file (never a link) of at most [`SCAN_MAX_FILE_BYTES`].
/// `Ok(None)` = absent, a link, or too large; `Err` = present but unreadable.
fn read_small_file(path: &std::path::Path) -> Result<Option<String>, ()> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(()),
    };
    if !meta.file_type().is_file() || meta.len() > SCAN_MAX_FILE_BYTES {
        return Ok(None);
    }
    std::fs::read_to_string(path).map(Some).map_err(|_| ())
}

fn rel_name(base: &std::path::Path, path: &std::path::Path) -> String {
    path.strip_prefix(base).unwrap_or(path).to_string_lossy().into_owned()
}

/// Scan one employee directory (`<home>/agents/<id>`) for MCP tool names
/// removed in v1.69.0: the `agent.toml [capabilities]` tool lists, the
/// prompt files at the directory root, and the Markdown under `SKILLS/` and
/// `wiki/`. Reads only; follows no symbolic links; skips files over 1 MiB.
pub fn scan_agent_dir_for_removed_tools(agent_dir: &std::path::Path) -> RemovedToolScan {
    let mut scan = RemovedToolScan::default();

    match read_small_file(&agent_dir.join("agent.toml")) {
        Ok(None) => {}
        Err(()) => scan.unreadable.push("agent.toml".to_string()),
        Ok(Some(raw)) => match raw.parse::<toml::Table>() {
            Err(_) => scan.unreadable.push("agent.toml".to_string()),
            Ok(table) => {
                let caps = table.get("capabilities").and_then(|v| v.as_table());
                for (key, effect) in CAPABILITY_TOOL_LISTS {
                    let Some(list) = caps.and_then(|c| c.get(*key)).and_then(|v| v.as_array()) else {
                        continue;
                    };
                    for entry in list.iter().filter_map(|v| v.as_str()) {
                        for row in removed_names_for_entry(entry) {
                            scan.findings.push(RemovedToolFinding {
                                file: "agent.toml".to_string(),
                                location: format!("[capabilities] {key}"),
                                tool: row.name,
                                occurrences: 1,
                                advice: advice_for(effect, row),
                            });
                        }
                    }
                }
            }
        },
    }

    for name in PROMPT_FILES {
        match read_small_file(&agent_dir.join(name)) {
            Ok(Some(text)) => scan_text((*name).to_string(), &text, &mut scan.findings),
            Ok(None) => {}
            Err(()) => scan.unreadable.push((*name).to_string()),
        }
    }

    let mut files_seen = 0usize;
    for dir in PROSE_DIRS {
        let mut stack = vec![(agent_dir.join(dir), 0usize)];
        while let Some((current, depth)) = stack.pop() {
            let entries = match std::fs::read_dir(&current) {
                Ok(e) => e,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => {
                    scan.unreadable.push(rel_name(agent_dir, &current));
                    continue;
                }
            };
            let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                let path = entry.path();
                let Ok(file_type) = entry.file_type() else {
                    scan.unreadable.push(rel_name(agent_dir, &path));
                    continue;
                };
                if file_type.is_dir() {
                    if depth < SCAN_MAX_DEPTH {
                        stack.push((path, depth + 1));
                    }
                } else if file_type.is_file()
                    && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("md"))
                {
                    if files_seen >= SCAN_MAX_FILES {
                        return scan;
                    }
                    files_seen += 1;
                    match read_small_file(&path) {
                        Ok(Some(text)) => {
                            scan_text(rel_name(agent_dir, &path), &text, &mut scan.findings)
                        }
                        Ok(None) => {}
                        Err(()) => scan.unreadable.push(rel_name(agent_dir, &path)),
                    }
                }
            }
        }
    }
    scan
}

/// Scan a parsed `config.toml` for removed tool names in the settings that
/// match tools by name: `[provenance] sensitive_tools` and
/// `[[ccr.allowed_sources]]` rows on the `duduclaw` server.
pub fn scan_config_for_removed_tools(config: &toml::Table) -> Vec<RemovedToolFinding> {
    use duduclaw_core::tool_catalog::removed_mcp_tool;
    let mut out = Vec::new();
    let sensitive = config
        .get("provenance")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("sensitive_tools"))
        .and_then(|v| v.as_array());
    for name in sensitive.into_iter().flatten().filter_map(|v| v.as_str()) {
        if let Some(row) = removed_mcp_tool(name.trim()) {
            out.push(RemovedToolFinding {
                file: "config.toml".to_string(),
                location: "[provenance] sensitive_tools".to_string(),
                tool: row.name,
                occurrences: 1,
                advice: advice_for(
                    "the gateway now gates the new name in its place (every form of it, since the list matches names only). \
                     Write the new name to make that explicit.",
                    row,
                ),
            });
        }
    }
    let sources = config
        .get("ccr")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("allowed_sources"))
        .and_then(|v| v.as_array());
    for source in sources.into_iter().flatten().filter_map(|v| v.as_table()) {
        let server = source.get("server").and_then(|v| v.as_str()).map(str::trim);
        if server != Some(duduclaw_core::tool_catalog::DUDUCLAW_MCP_SERVER) {
            continue;
        }
        let Some(row) = source.get("tool").and_then(|v| v.as_str()).and_then(|t| removed_mcp_tool(t.trim())) else {
            continue;
        };
        out.push(RemovedToolFinding {
            file: "config.toml".to_string(),
            location: "[[ccr.allowed_sources]]".to_string(),
            tool: row.name,
            occurrences: 1,
            advice: advice_for(
                "matches no tool any more, so that tool's results are no longer compressed for retrieval.",
                row,
            ),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── O3 ──────────────────────────────────────────────────────────────

    #[test]
    fn wiki_scope_defaults_to_agent_so_existing_calls_are_byte_identical() {
        assert_eq!(
            resolve_wiki_scope(&json!({"page_path": "a.md"})).unwrap(),
            WikiScope::Agent
        );
        assert_eq!(
            resolve_wiki_scope(&json!({"scope": null})).unwrap(),
            WikiScope::Agent
        );
    }

    #[test]
    fn wiki_scope_rejects_unknown_and_non_string_values() {
        assert!(resolve_wiki_scope(&json!({"scope": "global"})).is_err());
        assert!(resolve_wiki_scope(&json!({"scope": 1})).is_err());
        // Not a prefix/substring match: "sharedx" must not read as "shared".
        assert!(resolve_wiki_scope(&json!({"scope": "sharedx"})).is_err());
    }

    #[test]
    fn wiki_scope_accepts_explicit_shared_on_the_merged_name() {
        assert_eq!(
            resolve_wiki_scope(&json!({"scope": " Shared "})).unwrap(),
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
    fn skill_source_defaults_to_all() {
        assert_eq!(resolve_skill_source(&json!({"query": "x"})).unwrap(), SkillSource::All);
        assert_eq!(resolve_skill_source(&json!({"source": "bank"})).unwrap(), SkillSource::Bank);
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
        assert!(resolve_skill_source(&json!({"source": "npm"})).is_err());
        assert!(resolve_skill_source(&json!({"source": 3})).is_err());
    }

    // ── removed tool scan ───────────────────────────────────────────────

    fn write(dir: &std::path::Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn scan_finds_removed_names_in_lists_prompts_and_skills() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();
        write(
            dir,
            "agent.toml",
            "[capabilities]\nallowed_tools = [\"mcp__duduclaw__shared_wiki_read\", \"memory_search\", \"mcp__duduclaw__*\"]\ndenied_tools = [\"shared_wiki_write\"]\nscoped_tools = [\"schedule_task\"]\napproval_required_tools = [\"shared_wiki_*\"]\n",
        );
        write(dir, "SOUL.md", "用 shared_wiki_read 查 SOP，再用 shared_wiki_read 確認。\n");
        write(dir, "SKILLS/daily/SKILL.md", "Call `mcp__duduclaw__schedule_task` every morning.\n");
        write(dir, "wiki/sop.md", "not a match: can_schedule_tasks, shared_wiki_reader, my_skill_bank_search\n");
        write(dir, "notes.txt", "shared_wiki_read outside the scanned set\n");

        let scan = scan_agent_dir_for_removed_tools(dir);
        assert!(scan.unreadable.is_empty(), "{:?}", scan.unreadable);
        let find = |file: &str, location: &str, tool: &str| {
            scan.findings
                .iter()
                .find(|f| f.file == file && f.location == location && f.tool == tool)
                .unwrap_or_else(|| panic!("missing {file} {location} {tool}: {:#?}", scan.findings))
        };
        assert!(find("agent.toml", "[capabilities] allowed_tools", "shared_wiki_read").advice.contains("wiki_read"));
        assert!(find("agent.toml", "[capabilities] denied_tools", "shared_wiki_write").advice.contains("wiki_write"));
        find("agent.toml", "[capabilities] scoped_tools", "schedule_task");
        // A prefix wildcard still reaches removed names; flagged per name.
        find("agent.toml", "[capabilities] approval_required_tools", "shared_wiki_write");
        assert_eq!(find("SOUL.md", "text", "shared_wiki_read").occurrences, 2);
        find(&format!("SKILLS{}daily{}SKILL.md", std::path::MAIN_SEPARATOR, std::path::MAIN_SEPARATOR), "text", "schedule_task");
        // Whole-name matches only, the match-all wildcard is not a leftover,
        // and files outside the scanned set are ignored.
        assert!(!scan.findings.iter().any(|f| f.file.starts_with("wiki")), "{:#?}", scan.findings);
        assert!(!scan.findings.iter().any(|f| f.file == "notes.txt"));
        assert!(!scan.findings.iter().any(|f| f.tool == "memory_search"));
    }

    #[test]
    fn scan_reports_an_unparsable_agent_toml_instead_of_a_clean_result() {
        let tmp = tempfile::TempDir::new().unwrap();
        write(tmp.path(), "agent.toml", "[capabilities\nallowed_tools = [");
        let scan = scan_agent_dir_for_removed_tools(tmp.path());
        assert_eq!(scan.unreadable, vec!["agent.toml".to_string()]);
    }

    #[test]
    fn scan_of_a_clean_or_missing_directory_is_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        write(tmp.path(), "SOUL.md", "Use wiki_read with scope=\"shared\".\n");
        assert_eq!(scan_agent_dir_for_removed_tools(tmp.path()), RemovedToolScan::default());
        assert_eq!(
            scan_agent_dir_for_removed_tools(&tmp.path().join("missing")),
            RemovedToolScan::default()
        );
    }

    #[test]
    fn config_scan_flags_provenance_and_ccr_rows() {
        let config: toml::Table = toml::from_str(
            "[provenance]\nsensitive_tools = [\"send_to_agent\", \"shared_wiki_write\"]\n\n[[ccr.allowed_sources]]\nserver = \"duduclaw\"\ntool = \"shared_wiki_read\"\n\n[[ccr.allowed_sources]]\nserver = \"other\"\ntool = \"shared_wiki_read\"\n",
        )
        .unwrap();
        let found = scan_config_for_removed_tools(&config);
        assert_eq!(found.len(), 2, "{found:#?}");
        assert!(found.iter().any(|f| f.location == "[provenance] sensitive_tools" && f.tool == "shared_wiki_write"));
        assert!(found.iter().any(|f| f.location == "[[ccr.allowed_sources]]" && f.tool == "shared_wiki_read"));
    }

}
