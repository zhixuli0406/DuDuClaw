//! WP-F (P2-c): file-change evidence for a goal task — "what did this task
//! actually touch", shown to the human BEFORE they approve a `needs_human`
//! escalation.
//!
//! Borrowed from WorkBuddy's 「變更」tab (`research/harness-2026-08/
//! workbuddy-codebuddy-work.md` §2.4/§2.7 — "用於查看當前任務帶來的檔案修改,
//! 方便您在接受結果前先做一輪檢查"). The HITL card already carries the
//! ActionGuard *simulation* (what the agent SAYS it will do); this module
//! supplies the complementary half — what the audit trail RECORDS it did.
//!
//! ## Two evidence sources, one shape
//!
//! 1. **Native tool events** (`runtime::NativeToolEvent`) — `Write` / `Edit` /
//!    `NotebookEdit` / `Bash` and their codex/gemini equivalents. These never
//!    reach `tool_calls.jsonl` (that trail is written by the MCP dispatch
//!    layer), and the in-memory `(task_id, round)` bridge in
//!    `prediction::task_observe` is remove-once + process-lifetime, so it is
//!    gone long before a human opens the card. [`record_round_changes`] is
//!    therefore called from `dispatcher.rs` — the ONE place every goal-loop
//!    dispatch round's native events are already collected — and persists
//!    just the file-effect subset into `task_changes.jsonl`, keyed by task id.
//! 2. **MCP audit rows** (`tool_calls.jsonl`) — `shared_wiki_write` and
//!    friends. Already durable; attributed to the task by the same
//!    (agent, claim→review window) convention `dispatch_engine` uses for the
//!    `<tool_activity>` judge block.
//!
//! ## Honesty rules (this is HITL evidence, not a narrative)
//!
//! - Text shown to the human is the ALREADY-MASKED text the audit layer
//!   produced (`native_event_input_text` / `mask_sensitive_json`). This module
//!   never re-reads a file from disk to "enrich" a row — that would walk
//!   around the mask.
//! - No evidence ⇒ an empty list, never a fabricated summary. The dashboard
//!   says 「此任務沒有留下檔案變更紀錄」and stops there.
//! - Every failure (missing file, unreadable line, bad JSON, lock error)
//!   degrades to "fewer/no rows", never to an error the caller must handle.

use std::path::Path;

use duduclaw_core::{truncate_bytes, truncate_chars};

use crate::runtime::NativeToolEvent;

/// Durable per-task change ledger, next to `tool_calls.jsonl`.
pub const TASK_CHANGES_FILE: &str = "task_changes.jsonl";

/// Rotation threshold for [`TASK_CHANGES_FILE`]. Rows are small (path + a
/// capped snippet), so 8 MB is a lot of history; the ledger is evidence for
/// open decisions, not an archive.
const ROTATION_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Tail window read back for a query — bounded so a card render never pays
/// for a whole-file scan.
const TAIL_READ_BYTES: u64 = 1024 * 1024;

/// Per-round cap on persisted rows (a runaway loop must not be able to grow
/// the ledger without bound).
const MAX_ROWS_PER_ROUND: usize = 50;

/// Byte cap for the human-visible snippet (CJK-safe truncation).
const SNIPPET_MAX_BYTES: usize = 240;

/// Byte cap for a rendered path / shell command.
const PATH_MAX_BYTES: usize = 400;

/// Default / hard ceiling for a query's row count.
pub const DEFAULT_QUERY_LIMIT: usize = 100;
pub const MAX_QUERY_LIMIT: usize = 500;

// ── Shape ────────────────────────────────────────────────────────────────

/// The file effect a tool call had. Deliberately coarse — this is a review
/// aid, not a VCS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeOp {
    /// Whole-file create/overwrite.
    Write,
    /// In-place modification of an existing file.
    Edit,
    /// Removal.
    Delete,
    /// A shell command whose leading verb is a well-known file mutator. The
    /// `path` field carries the COMMAND, not a resolved path — we do not
    /// pretend to know which files a shell line touched.
    Shell,
}

impl ChangeOp {
    pub fn as_str(self) -> &'static str {
        match self {
            ChangeOp::Write => "write",
            ChangeOp::Edit => "edit",
            ChangeOp::Delete => "delete",
            ChangeOp::Shell => "shell",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s {
            "write" => Some(ChangeOp::Write),
            "edit" => Some(ChangeOp::Edit),
            "delete" => Some(ChangeOp::Delete),
            "shell" => Some(ChangeOp::Shell),
            _ => None,
        }
    }
}

/// Which trail this row came from — surfaced so the operator can tell
/// "the runtime saw this tool run" from "the MCP audit log recorded it".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeSource {
    /// Runtime-native tool event (Write/Edit/Bash …), persisted at dispatch.
    Native,
    /// `tool_calls.jsonl` MCP audit row inside the task's window.
    McpAudit,
}

impl ChangeSource {
    pub fn as_str(self) -> &'static str {
        match self {
            ChangeSource::Native => "native",
            ChangeSource::McpAudit => "mcp_audit",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s {
            "native" => Some(ChangeSource::Native),
            "mcp_audit" => Some(ChangeSource::McpAudit),
            _ => None,
        }
    }
}

/// One file-effect tool call, projected to what a reviewer needs.
#[derive(Debug, Clone, PartialEq)]
pub struct FileChange {
    /// The touched path, or — for [`ChangeOp::Shell`] — the command itself.
    pub path: String,
    pub op: ChangeOp,
    /// The tool that produced the effect, verbatim (`Write`, `apply_patch`,
    /// `shared_wiki_write`, …).
    pub tool_name: String,
    /// RFC3339 UTC.
    pub timestamp: String,
    pub success: bool,
    /// Masked excerpt of what was written / the command's own text. `None`
    /// when the producer captured no input text.
    pub snippet: Option<String>,
    pub source: ChangeSource,
    /// Goal-loop round, when known (native rows only).
    pub round: Option<u32>,
}

impl FileChange {
    fn to_json(&self, task_id: &str, agent_id: &str) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        map.insert("timestamp".into(), self.timestamp.clone().into());
        map.insert("task_id".into(), task_id.into());
        map.insert("agent_id".into(), agent_id.into());
        map.insert("path".into(), self.path.clone().into());
        map.insert("op".into(), self.op.as_str().into());
        map.insert("tool_name".into(), self.tool_name.clone().into());
        map.insert("success".into(), self.success.into());
        map.insert("source".into(), self.source.as_str().into());
        if let Some(round) = self.round {
            map.insert("round".into(), round.into());
        }
        if let Some(snippet) = &self.snippet {
            map.insert("snippet".into(), snippet.clone().into());
        }
        serde_json::Value::Object(map)
    }

    /// Dashboard wire shape (`tasks.changes`).
    pub fn to_wire_json(&self) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        map.insert("path".into(), self.path.clone().into());
        map.insert("op".into(), self.op.as_str().into());
        map.insert("tool_name".into(), self.tool_name.clone().into());
        map.insert("timestamp".into(), self.timestamp.clone().into());
        map.insert("success".into(), self.success.into());
        map.insert("source".into(), self.source.as_str().into());
        map.insert(
            "snippet".into(),
            match &self.snippet {
                Some(s) => serde_json::Value::String(s.clone()),
                None => serde_json::Value::Null,
            },
        );
        map.insert(
            "round".into(),
            match self.round {
                Some(r) => serde_json::Value::from(r),
                None => serde_json::Value::Null,
            },
        );
        serde_json::Value::Object(map)
    }

    fn from_ledger_line(line: &str, task_id: &str) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_str(line).ok()?;
        if v.get("task_id").and_then(|x| x.as_str()) != Some(task_id) {
            return None;
        }
        Some(FileChange {
            path: v.get("path").and_then(|x| x.as_str())?.to_string(),
            op: ChangeOp::from_str(v.get("op").and_then(|x| x.as_str())?)?,
            tool_name: v
                .get("tool_name")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            timestamp: v.get("timestamp").and_then(|x| x.as_str())?.to_string(),
            success: v.get("success").and_then(|x| x.as_bool()).unwrap_or(true),
            snippet: v
                .get("snippet")
                .and_then(|x| x.as_str())
                .map(str::to_string),
            source: v
                .get("source")
                .and_then(|x| x.as_str())
                .and_then(ChangeSource::from_str)
                .unwrap_or(ChangeSource::Native),
            round: v
                .get("round")
                .and_then(|x| x.as_u64())
                .and_then(|n| u32::try_from(n).ok()),
        })
    }
}

// ── Classification ───────────────────────────────────────────────────────

/// Tool names whose effect is "a whole file now exists / was replaced".
/// Exact match only (coding convention 2 — never `contains`), covering the
/// Claude / codex / gemini native vocabularies.
const WRITE_TOOLS: &[&str] = &["Write", "write_file", "create_file", "WriteFile"];

/// Tool names that modify an existing file in place.
const EDIT_TOOLS: &[&str] = &[
    "Edit",
    "MultiEdit",
    "NotebookEdit",
    "edit_file",
    "apply_patch",
    "str_replace_editor",
    "replace",
];

/// Tool names that remove a file.
const DELETE_TOOLS: &[&str] = &["delete_file", "DeleteFile"];

/// Shell-ish tools — a row is produced only when the command's own leading
/// verb is a known mutator (see [`shell_command_mutates`]).
const SHELL_TOOLS: &[&str] = &["Bash", "shell", "run_shell_command", "run_terminal_cmd"];

/// MCP tools (`tool_calls.jsonl` names) whose effect a reviewer would call a
/// file change. Deliberately short — a long speculative list would turn the
/// tab into noise, and a tool with no path-bearing argument (e.g. `canvas_push`)
/// would only ever be silently dropped, so it does not belong here.
const MCP_WRITE_TOOLS: &[&str] = &["shared_wiki_write", "agent_update_soul"];
const MCP_DELETE_TOOLS: &[&str] = &["shared_wiki_delete"];

/// Leading command verbs whose file effect is unambiguous. Matched against
/// the FIRST token of each `;`/`&&`/`||`/`|` segment — never a substring scan
/// (coding convention 2: `rm` must not match inside `alarm`).
const MUTATING_SHELL_VERBS: &[&str] = &[
    "rm", "mv", "cp", "mkdir", "rmdir", "touch", "tee", "truncate", "dd", "ln", "chmod", "chown",
    "patch", "install", "unzip", "tar",
];

/// Keys that carry a path, in the order we trust them.
const PATH_KEYS: &[&str] = &[
    "file_path",
    "notebook_path",
    "page_path",
    "filePath",
    "path",
    "file",
    "target_file",
];

/// Keys that carry the human-meaningful body of the change.
const SNIPPET_KEYS: &[&str] = &[
    "new_string",
    "new_source",
    "content",
    "replacement",
    "patch",
    "command",
];

fn classify_tool(tool_name: &str) -> Option<ChangeOp> {
    if WRITE_TOOLS.contains(&tool_name) || MCP_WRITE_TOOLS.contains(&tool_name) {
        return Some(ChangeOp::Write);
    }
    if EDIT_TOOLS.contains(&tool_name) {
        return Some(ChangeOp::Edit);
    }
    if DELETE_TOOLS.contains(&tool_name) || MCP_DELETE_TOOLS.contains(&tool_name) {
        return Some(ChangeOp::Delete);
    }
    if SHELL_TOOLS.contains(&tool_name) {
        return Some(ChangeOp::Shell);
    }
    None
}

/// `true` when a shell command's own leading verb (of any `;`/`&&`/`||`/`|`
/// segment) is a known file mutator, or the segment redirects into a file
/// (`>` / `>>`). Conservative on purpose: an unrecognised command produces NO
/// row rather than a speculative one — an over-eager "this changed files"
/// claim is worse for a reviewer than a missing line they can still find in
/// the activity log.
pub fn shell_command_mutates(command: &str) -> bool {
    for segment in command.split(|c| matches!(c, ';' | '\n' | '&' | '|')) {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }
        // Output redirection into a file is a file effect regardless of verb.
        if segment.contains('>') {
            return true;
        }
        let mut tokens = segment.split_whitespace();
        // Skip a leading `sudo` / env assignment so `sudo rm -rf x` still counts.
        let mut first = tokens.next().unwrap_or("");
        if first == "sudo" || first.contains('=') {
            first = tokens.next().unwrap_or("");
        }
        // `sed -i` / `git apply` / `git rm` need the second token too.
        let verb = first.rsplit('/').next().unwrap_or(first);
        if MUTATING_SHELL_VERBS.contains(&verb) {
            return true;
        }
        let second = tokens.next().unwrap_or("");
        if verb == "sed" && second.starts_with("-i") {
            return true;
        }
        if verb == "git" && matches!(second, "apply" | "rm" | "mv" | "checkout" | "restore") {
            return true;
        }
    }
    false
}

/// Pull a string field out of the masked input text. Prefers a real JSON
/// parse; falls back to a bounded raw scan because the audit layer truncates
/// at a char budget and may hand us a JSON fragment that no longer parses.
fn field_str(parsed: Option<&serde_json::Value>, raw: &str, key: &str) -> Option<String> {
    if let Some(v) = parsed.and_then(|p| p.get(key)).and_then(|v| v.as_str()) {
        if !v.trim().is_empty() {
            return Some(v.to_string());
        }
    }
    // Fallback scan: `"key":"…"` with escaped-quote awareness.
    let needle = format!("\"{key}\"");
    let start = raw.find(&needle)? + needle.len();
    let rest = raw[start..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(esc) => out.push(esc),
                None => break,
            },
            '"' => break,
            other => out.push(other),
        }
    }
    if out.trim().is_empty() { None } else { Some(out) }
}

/// A few MCP tools name their target indirectly. The mapping is exact and
/// deterministic (SOUL.md always sits at the agent directory root), never a
/// guess — a tool whose target cannot be derived exactly is simply not listed
/// in [`MCP_WRITE_TOOLS`].
fn mcp_path_override(
    tool_name: &str,
    parsed: Option<&serde_json::Value>,
    raw: &str,
) -> Option<String> {
    match tool_name {
        "agent_update_soul" => {
            let agent = field_str(parsed, raw, "agent_id")?;
            Some(format!("{}/SOUL.md", agent.trim()))
        }
        _ => None,
    }
}

/// Build a [`FileChange`] from one observed tool call, or `None` when the
/// tool has no file effect (or a shell command we cannot confidently call a
/// mutation).
///
/// `input_text` MUST already be masked by its producer
/// (`runtime::native_event_input_text*` / `audit::mask_sensitive_json`) —
/// this function only truncates and reshapes.
pub fn extract_change(
    tool_name: &str,
    success: bool,
    timestamp: &str,
    input_text: Option<&str>,
    source: ChangeSource,
    round: Option<u32>,
) -> Option<FileChange> {
    let op = classify_tool(tool_name)?;
    let raw = input_text.unwrap_or("");
    let parsed: Option<serde_json::Value> = serde_json::from_str(raw).ok();
    let parsed_ref = parsed.as_ref();

    let (path, snippet) = if op == ChangeOp::Shell {
        let command = field_str(parsed_ref, raw, "command")
            .or_else(|| field_str(parsed_ref, raw, "cmd"))
            // A producer that gave us plain text rather than a JSON envelope.
            .or_else(|| {
                if parsed_ref.is_none() && !raw.trim().is_empty() {
                    Some(raw.to_string())
                } else {
                    None
                }
            })?;
        if !shell_command_mutates(&command) {
            return None;
        }
        let description = field_str(parsed_ref, raw, "description");
        (command, description)
    } else {
        let path = mcp_path_override(tool_name, parsed_ref, raw)
            .or_else(|| PATH_KEYS.iter().find_map(|k| field_str(parsed_ref, raw, k)))?;
        let snippet = SNIPPET_KEYS.iter().find_map(|k| field_str(parsed_ref, raw, k));
        (path, snippet)
    };

    let path = path.trim();
    if path.is_empty() {
        return None;
    }

    Some(FileChange {
        path: truncate_bytes(path, PATH_MAX_BYTES).to_string(),
        op,
        tool_name: truncate_chars(tool_name, 80),
        timestamp: timestamp.to_string(),
        success,
        snippet: snippet
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| truncate_bytes(s, SNIPPET_MAX_BYTES).to_string()),
        source,
        round,
    })
}

// ── Write side (dispatcher.rs, once per goal-loop round) ─────────────────

/// Persist the file-effect subset of one goal-loop round's native tool events.
///
/// Called from `dispatcher.rs` right where the WP-A4/A5/T10 collector is
/// already drained — the only point at which native (non-MCP) tool evidence
/// exists for a goal-loop dispatch. Everything about it is best-effort: a
/// missing directory, a lock failure or a serialization error drops rows
/// silently rather than disturbing a dispatch that already succeeded.
pub fn record_round_changes(
    home_dir: &Path,
    task_id: &str,
    agent_id: &str,
    round: u32,
    events: &[NativeToolEvent],
) {
    if task_id.is_empty() || events.is_empty() {
        return;
    }
    let now = chrono::Utc::now().to_rfc3339();
    let rows: Vec<serde_json::Value> = events
        .iter()
        .filter_map(|e| {
            extract_change(
                &e.tool_name,
                e.success,
                &now,
                e.input_text.as_deref(),
                ChangeSource::Native,
                Some(round),
            )
        })
        .take(MAX_ROWS_PER_ROUND)
        .map(|c| c.to_json(task_id, agent_id))
        .collect();
    if rows.is_empty() {
        return;
    }
    append_rows(home_dir, &rows);
}

fn append_rows(home_dir: &Path, rows: &[serde_json::Value]) {
    let path = home_dir.join(TASK_CHANGES_FILE);
    maybe_rotate(&path);
    let mut body = String::new();
    for row in rows {
        match serde_json::to_string(row) {
            Ok(line) => {
                body.push_str(&line);
                body.push('\n');
            }
            Err(_) => continue,
        }
    }
    if body.is_empty() {
        return;
    }
    // Cross-process advisory lock (coding convention 3): the gateway is the
    // only writer today, but the ledger sits next to `tool_calls.jsonl` in a
    // directory several processes append to.
    let _ = duduclaw_core::with_file_lock(&path, || {
        use std::io::Write as _;
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).append(true);
        // Rows carry masked excerpts of file content — never world-readable.
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = f.metadata() {
                if meta.permissions().mode() & 0o077 != 0 {
                    let mut perms = meta.permissions();
                    perms.set_mode(0o600);
                    let _ = f.set_permissions(perms);
                }
            }
        }
        f.write_all(body.as_bytes())
    });
}

fn maybe_rotate(path: &Path) {
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    if meta.len() <= ROTATION_MAX_BYTES {
        return;
    }
    let rotated = path.with_extension("jsonl.1");
    let _ = std::fs::rename(path, rotated);
}

// ── Read side (`tasks.changes` RPC) ──────────────────────────────────────

/// Everything the dashboard needs to render the 「變更」tab.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TaskChangeEvidence {
    /// Newest first, capped at the caller's limit.
    pub changes: Vec<FileChange>,
    /// Distinct paths touched (shell rows excluded — a command is not a path).
    pub distinct_paths: usize,
    /// `true` when rows were dropped by the limit.
    pub truncated: bool,
}

/// Read the ledger rows belonging to `task_id`.
fn read_ledger(home_dir: &Path, task_id: &str) -> Vec<FileChange> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let path = home_dir.join(TASK_CHANGES_FILE);
    let Ok(mut file) = std::fs::File::open(&path) else {
        return Vec::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(TAIL_READ_BYTES);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut raw = Vec::new();
    if file.read_to_end(&mut raw).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&raw);
    // A mid-file seek lands inside a line — drop the first partial line.
    let text = if start > 0 {
        match text.find('\n') {
            Some(pos) => &text[pos + 1..],
            None => return Vec::new(),
        }
    } else {
        &text[..]
    };
    text.lines()
        .filter_map(|line| FileChange::from_ledger_line(line.trim(), task_id))
        .collect()
}

/// Read the MCP audit rows attributable to this task: same `agent_id` +
/// `[since, until]` window convention `dispatch_engine` already uses for the
/// judge's `<tool_activity>` block. An empty/unparseable window yields no
/// rows (never "everything the agent ever did").
fn read_mcp_rows(home_dir: &Path, agent_id: &str, since: &str, until: &str) -> Vec<FileChange> {
    if agent_id.is_empty() {
        return Vec::new();
    }
    let (Ok(since_dt), Ok(until_dt)) = (
        chrono::DateTime::parse_from_rfc3339(since),
        chrono::DateTime::parse_from_rfc3339(until),
    ) else {
        return Vec::new();
    };
    let path = home_dir.join("tool_calls.jsonl");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in raw.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        if v.get("agent_id").and_then(|x| x.as_str()) != Some(agent_id) {
            continue;
        }
        let Some(ts) = v.get("timestamp").and_then(|x| x.as_str()) else {
            continue;
        };
        let Ok(ts_dt) = chrono::DateTime::parse_from_rfc3339(ts) else {
            continue;
        };
        if ts_dt < since_dt || ts_dt > until_dt {
            continue;
        }
        let Some(tool_name) = v.get("tool_name").and_then(|x| x.as_str()) else {
            continue;
        };
        let success = v.get("success").and_then(|x| x.as_bool()).unwrap_or(false);
        let input = v.get("input").and_then(|x| x.as_str());
        if let Some(change) = extract_change(
            tool_name,
            success,
            ts,
            input,
            ChangeSource::McpAudit,
            None,
        ) {
            out.push(change);
        }
    }
    out
}

/// Collect a task's change evidence from both trails.
///
/// `agent_id` / `since` / `until` describe the task's claim→review window and
/// are used ONLY for the MCP half; the native half is keyed by task id and
/// needs no window. Passing an empty `agent_id` (unknown assignee) simply
/// drops the MCP half instead of widening attribution.
pub fn collect_task_changes(
    home_dir: &Path,
    task_id: &str,
    agent_id: &str,
    since: &str,
    until: &str,
    limit: usize,
) -> TaskChangeEvidence {
    if task_id.is_empty() {
        return TaskChangeEvidence::default();
    }
    let limit = limit.clamp(1, MAX_QUERY_LIMIT);
    let mut all = read_ledger(home_dir, task_id);
    all.extend(read_mcp_rows(home_dir, agent_id, since, until));

    // Newest first — a reviewer asks "what did it just do" far more often
    // than "where did it start".
    all.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

    let truncated = all.len() > limit;
    all.truncate(limit);

    let mut paths: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for c in &all {
        if c.op != ChangeOp::Shell {
            paths.insert(c.path.as_str());
        }
    }
    TaskChangeEvidence {
        distinct_paths: paths.len(),
        truncated,
        changes: all,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn native(tool: &str, success: bool, input: Option<&str>) -> NativeToolEvent {
        NativeToolEvent {
            tool_name: tool.to_string(),
            success,
            result_text: None,
            input_text: input.map(str::to_string),
        }
    }

    // ── extraction ──────────────────────────────────────────────────────

    #[test]
    fn write_tool_yields_path_and_snippet() {
        let input = r#"{"file_path":"/repo/src/main.rs","content":"fn main() {}"}"#;
        let c = extract_change("Write", true, "2026-08-15T10:00:00Z", Some(input), ChangeSource::Native, Some(2))
            .expect("Write is a file effect");
        assert_eq!(c.path, "/repo/src/main.rs");
        assert_eq!(c.op, ChangeOp::Write);
        assert_eq!(c.snippet.as_deref(), Some("fn main() {}"));
        assert_eq!(c.round, Some(2));
        assert_eq!(c.source, ChangeSource::Native);
    }

    #[test]
    fn edit_tool_prefers_new_string_as_snippet() {
        let input = r#"{"file_path":"/repo/a.ts","old_string":"a","new_string":"b"}"#;
        let c = extract_change("Edit", true, "t", Some(input), ChangeSource::Native, None).unwrap();
        assert_eq!(c.op, ChangeOp::Edit);
        assert_eq!(c.snippet.as_deref(), Some("b"));
    }

    #[test]
    fn notebook_edit_uses_notebook_path() {
        let input = r#"{"notebook_path":"/repo/nb.ipynb","new_source":"print(1)"}"#;
        let c = extract_change("NotebookEdit", true, "t", Some(input), ChangeSource::Native, None)
            .unwrap();
        assert_eq!(c.path, "/repo/nb.ipynb");
        assert_eq!(c.op, ChangeOp::Edit);
    }

    #[test]
    fn read_only_and_unknown_tools_produce_nothing() {
        for tool in ["Read", "Grep", "memory_search", "tasks_list", "WebFetch"] {
            assert!(
                extract_change(tool, true, "t", Some(r#"{"file_path":"/x"}"#), ChangeSource::Native, None)
                    .is_none(),
                "{tool} must not be reported as a file change"
            );
        }
    }

    #[test]
    fn failed_calls_are_kept_and_marked() {
        let c = extract_change(
            "Write",
            false,
            "t",
            Some(r#"{"file_path":"/repo/x.rs","content":"x"}"#),
            ChangeSource::Native,
            None,
        )
        .unwrap();
        assert!(!c.success, "a blocked/failed write is exactly what live state hides");
    }

    #[test]
    fn missing_path_field_yields_nothing_never_a_placeholder() {
        assert!(extract_change("Write", true, "t", Some(r#"{"content":"x"}"#), ChangeSource::Native, None).is_none());
        assert!(extract_change("Write", true, "t", None, ChangeSource::Native, None).is_none());
    }

    #[test]
    fn truncated_json_still_yields_a_path_via_raw_scan() {
        // The audit layer caps input at a char budget, so a big Write lands
        // here as an unparseable fragment — the path must survive anyway.
        let input = r#"{"file_path":"/repo/big.rs","content":"aaaaaaaaaa"#;
        assert!(serde_json::from_str::<serde_json::Value>(input).is_err());
        let c = extract_change("Write", true, "t", Some(input), ChangeSource::Native, None).unwrap();
        assert_eq!(c.path, "/repo/big.rs");
    }

    #[test]
    fn masked_text_is_passed_through_verbatim_never_re_read() {
        let input = r#"{"file_path":"/repo/.env","content":"API_KEY=***MASKED***"}"#;
        let c = extract_change("Write", true, "t", Some(input), ChangeSource::Native, None).unwrap();
        assert_eq!(c.snippet.as_deref(), Some("API_KEY=***MASKED***"));
    }

    #[test]
    fn cjk_snippet_and_path_truncate_on_char_boundaries() {
        let long_body = "鴻海台積電聯發科".repeat(200);
        let input = serde_json::json!({
            "file_path": format!("/repo/{}", "報告".repeat(400)),
            "content": long_body,
        })
        .to_string();
        // Must not panic on a mid-char byte budget.
        let c = extract_change("Write", true, "t", Some(&input), ChangeSource::Native, None).unwrap();
        assert!(c.path.len() <= PATH_MAX_BYTES);
        assert!(c.snippet.as_ref().unwrap().len() <= SNIPPET_MAX_BYTES);
        assert!(c.path.starts_with("/repo/報告"));
    }

    // ── shell heuristics ────────────────────────────────────────────────

    #[test]
    fn mutating_shell_commands_are_detected() {
        for cmd in [
            "rm -rf build",
            "mv a.txt b.txt",
            "mkdir -p out",
            "echo hi > out.txt",
            "cat a >> b",
            "sed -i '' 's/a/b/' f.rs",
            "git apply patch.diff",
            "sudo rm /etc/hosts",
            "npm run build && cp dist/x /srv/x",
            "/bin/rm x",
        ] {
            assert!(shell_command_mutates(cmd), "{cmd} should count as a file effect");
        }
    }

    #[test]
    fn non_mutating_shell_commands_are_not_reported() {
        for cmd in [
            "ls -la",
            "cargo test",
            "git status",
            "grep -rn rm src/",
            "alarm --check",
            "python -c 'print(1)'",
        ] {
            assert!(!shell_command_mutates(cmd), "{cmd} must not be called a file change");
        }
    }

    #[test]
    fn shell_row_carries_the_command_not_a_guessed_path() {
        let input = r#"{"command":"rm -rf /repo/build","description":"清掉舊產物"}"#;
        let c = extract_change("Bash", true, "t", Some(input), ChangeSource::Native, None).unwrap();
        assert_eq!(c.op, ChangeOp::Shell);
        assert_eq!(c.path, "rm -rf /repo/build");
        assert_eq!(c.snippet.as_deref(), Some("清掉舊產物"));
    }

    #[test]
    fn read_only_shell_yields_no_row() {
        let input = r#"{"command":"cargo test --workspace"}"#;
        assert!(extract_change("Bash", true, "t", Some(input), ChangeSource::Native, None).is_none());
    }

    // ── ledger round trip ───────────────────────────────────────────────

    #[test]
    fn record_and_read_round_trips_only_file_effects() {
        let dir = tempfile::tempdir().unwrap();
        record_round_changes(
            dir.path(),
            "task-1",
            "worker",
            3,
            &[
                native("Read", true, Some(r#"{"file_path":"/repo/a.rs"}"#)),
                native("Write", true, Some(r#"{"file_path":"/repo/a.rs","content":"x"}"#)),
                native("Bash", true, Some(r#"{"command":"ls"}"#)),
                native("Bash", false, Some(r#"{"command":"rm -rf /repo/tmp"}"#)),
            ],
        );
        let ev = collect_task_changes(dir.path(), "task-1", "worker", "2026-01-01T00:00:00Z", "2030-01-01T00:00:00Z", 50);
        assert_eq!(ev.changes.len(), 2, "Read + read-only Bash are excluded");
        assert_eq!(ev.distinct_paths, 1, "shell rows are not paths");
        assert!(ev.changes.iter().any(|c| c.path == "/repo/a.rs" && c.op == ChangeOp::Write));
        assert!(ev.changes.iter().any(|c| c.op == ChangeOp::Shell && !c.success));
        assert!(ev.changes.iter().all(|c| c.round == Some(3)));
    }

    #[test]
    fn ledger_rows_of_another_task_are_never_returned() {
        let dir = tempfile::tempdir().unwrap();
        record_round_changes(
            dir.path(),
            "task-A",
            "worker",
            1,
            &[native("Write", true, Some(r#"{"file_path":"/a","content":"1"}"#))],
        );
        record_round_changes(
            dir.path(),
            "task-B",
            "worker",
            1,
            &[native("Write", true, Some(r#"{"file_path":"/b","content":"2"}"#))],
        );
        let ev = collect_task_changes(dir.path(), "task-A", "", "", "", 50);
        assert_eq!(ev.changes.len(), 1);
        assert_eq!(ev.changes[0].path, "/a");
    }

    #[test]
    fn no_evidence_is_an_honest_empty_result() {
        let dir = tempfile::tempdir().unwrap();
        let ev = collect_task_changes(dir.path(), "task-1", "worker", "2026-01-01T00:00:00Z", "2030-01-01T00:00:00Z", 50);
        assert_eq!(ev, TaskChangeEvidence::default());
        assert!(ev.changes.is_empty());
        assert_eq!(ev.distinct_paths, 0);
        assert!(!ev.truncated);
    }

    #[test]
    fn empty_task_id_or_no_events_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        record_round_changes(dir.path(), "", "worker", 1, &[native("Write", true, Some(r#"{"file_path":"/a"}"#))]);
        record_round_changes(dir.path(), "task-1", "worker", 1, &[]);
        // A round whose events had no file effect must not create the file either.
        record_round_changes(dir.path(), "task-1", "worker", 1, &[native("Read", true, Some("{}"))]);
        assert!(!dir.path().join(TASK_CHANGES_FILE).exists());
    }

    #[test]
    fn malformed_ledger_lines_are_skipped_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        record_round_changes(
            dir.path(),
            "task-1",
            "worker",
            1,
            &[native("Write", true, Some(r#"{"file_path":"/a","content":"1"}"#))],
        );
        let path = dir.path().join(TASK_CHANGES_FILE);
        let mut body = std::fs::read_to_string(&path).unwrap();
        body.push_str("not-json\n{}\n");
        std::fs::write(&path, body).unwrap();
        let ev = collect_task_changes(dir.path(), "task-1", "", "", "", 50);
        assert_eq!(ev.changes.len(), 1);
    }

    #[test]
    fn per_round_row_cap_is_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let events: Vec<NativeToolEvent> = (0..MAX_ROWS_PER_ROUND + 20)
            .map(|i| native("Write", true, Some(&format!(r#"{{"file_path":"/f{i}","content":"x"}}"#))))
            .collect();
        record_round_changes(dir.path(), "task-1", "worker", 1, &events);
        let ev = collect_task_changes(dir.path(), "task-1", "", "", "", MAX_QUERY_LIMIT);
        assert_eq!(ev.changes.len(), MAX_ROWS_PER_ROUND);
    }

    #[test]
    fn query_limit_truncates_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(TASK_CHANGES_FILE);
        let mut body = String::new();
        for i in 0..5 {
            body.push_str(&serde_json::json!({
                "timestamp": format!("2026-08-15T10:0{i}:00+00:00"),
                "task_id": "task-1",
                "agent_id": "worker",
                "path": format!("/f{i}"),
                "op": "write",
                "tool_name": "Write",
                "success": true,
                "source": "native",
            }).to_string());
            body.push('\n');
        }
        std::fs::write(&path, body).unwrap();
        let ev = collect_task_changes(dir.path(), "task-1", "", "", "", 2);
        assert!(ev.truncated);
        assert_eq!(ev.changes.len(), 2);
        assert_eq!(ev.changes[0].path, "/f4", "newest first");
        assert_eq!(ev.changes[1].path, "/f3");
    }

    // ── MCP audit half ──────────────────────────────────────────────────

    #[test]
    fn mcp_rows_are_scoped_to_agent_and_window() {
        let dir = tempfile::tempdir().unwrap();
        let rows = [
            // in window, right agent, file-effect tool
            r#"{"timestamp":"2026-08-15T10:02:00+00:00","agent_id":"w","tool_name":"shared_wiki_write","success":true,"input":"{\"page_path\":\"sop/a.md\",\"content\":\"hi\"}"}"#,
            // wrong agent
            r#"{"timestamp":"2026-08-15T10:02:00+00:00","agent_id":"other","tool_name":"shared_wiki_write","success":true,"input":"{\"page_path\":\"sop/b.md\"}"}"#,
            // outside the window
            r#"{"timestamp":"2026-08-15T09:00:00+00:00","agent_id":"w","tool_name":"shared_wiki_write","success":true,"input":"{\"page_path\":\"sop/c.md\"}"}"#,
            // no file effect
            r#"{"timestamp":"2026-08-15T10:03:00+00:00","agent_id":"w","tool_name":"memory_search","success":true,"input":"{\"query\":\"x\"}"}"#,
        ];
        std::fs::write(dir.path().join("tool_calls.jsonl"), rows.join("\n")).unwrap();
        let ev = collect_task_changes(
            dir.path(),
            "task-1",
            "w",
            "2026-08-15T10:00:00+00:00",
            "2026-08-15T10:05:00+00:00",
            50,
        );
        assert_eq!(ev.changes.len(), 1);
        assert_eq!(ev.changes[0].path, "sop/a.md");
        assert_eq!(ev.changes[0].source, ChangeSource::McpAudit);
        assert_eq!(ev.changes[0].round, None);
    }

    #[test]
    fn unknown_assignee_drops_the_mcp_half_instead_of_widening_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tool_calls.jsonl"),
            r#"{"timestamp":"2026-08-15T10:02:00+00:00","agent_id":"w","tool_name":"shared_wiki_write","success":true,"input":"{\"page_path\":\"sop/a.md\"}"}"#,
        )
        .unwrap();
        let ev = collect_task_changes(dir.path(), "task-1", "", "2026-08-15T10:00:00+00:00", "2026-08-15T10:05:00+00:00", 50);
        assert!(ev.changes.is_empty());
    }

    #[test]
    fn unparseable_window_yields_no_mcp_rows() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("tool_calls.jsonl"),
            r#"{"timestamp":"2026-08-15T10:02:00+00:00","agent_id":"w","tool_name":"shared_wiki_write","success":true,"input":"{\"page_path\":\"sop/a.md\"}"}"#,
        )
        .unwrap();
        let ev = collect_task_changes(dir.path(), "task-1", "w", "nope", "also-nope", 50);
        assert!(ev.changes.is_empty(), "a bad window must not degrade to 'everything'");
    }

    #[test]
    fn soul_update_resolves_its_target_path_deterministically() {
        let c = extract_change(
            "agent_update_soul",
            true,
            "t",
            Some(r#"{"agent_id":"trader","content":"人格：穩健"}"#),
            ChangeSource::McpAudit,
            None,
        )
        .unwrap();
        assert_eq!(c.path, "trader/SOUL.md");
        assert_eq!(c.op, ChangeOp::Write);
    }

    #[test]
    fn wire_json_is_stable_and_nulls_absent_fields() {
        let c = extract_change("Write", true, "2026-08-15T10:00:00Z", Some(r#"{"file_path":"/a"}"#), ChangeSource::McpAudit, None).unwrap();
        let j = c.to_wire_json();
        assert_eq!(j["path"], "/a");
        assert_eq!(j["op"], "write");
        assert_eq!(j["source"], "mcp_audit");
        assert!(j["snippet"].is_null());
        assert!(j["round"].is_null());
    }
}
