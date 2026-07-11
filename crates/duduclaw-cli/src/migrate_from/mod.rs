//! `duduclaw migrate-from <openclaw|hermes|paperclip>` — painless migration
//! from the three big competitor platforms into DuDuClaw.
//!
//! Default is a **dry-run** that prints the migration plan; `--apply` performs
//! the writes. Every item is reported honestly (IMPORTED / PARTIAL / SKIPPED /
//! CONFLICT) and nothing is ever silently dropped: a parse failure, an
//! unsupported credential, or a fail-closed security block is recorded with a
//! zh-TW reason. Original session/conversation files are archived verbatim to
//! `~/.duduclaw/imported/<platform>/raw/` so no data is lost even when v1 does
//! not parse it into `sessions.db`.
//!
//! Design spec: `commercial/docs/TODO-migrate-from-2026-07-11.md` (L3).

mod apply;
mod hermes;
mod openclaw;
mod paperclip;
mod report;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;

use duduclaw_core::error::{DuDuClawError, Result};

use report::Report;

// ─────────────────────────── Platform ───────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Platform {
    OpenClaw,
    Hermes,
    Paperclip,
}

impl Platform {
    fn as_str(self) -> &'static str {
        match self {
            Platform::OpenClaw => "openclaw",
            Platform::Hermes => "hermes",
            Platform::Paperclip => "paperclip",
        }
    }
}

/// Shared run context threaded through every platform importer.
pub(crate) struct Ctx {
    pub home: PathBuf,
    pub platform: Platform,
    pub apply: bool,
    pub rename: bool,
}

impl Ctx {
    fn imported_dir(&self) -> PathBuf {
        self.home.join("imported").join(self.platform.as_str())
    }
    fn raw_dir(&self) -> PathBuf {
        self.imported_dir().join("raw")
    }
}

// ─────────────────────────── Entry point ───────────────────────────

/// CLI entry for `duduclaw migrate-from`.
///
/// `json` switches the human console output for a single machine-readable JSON
/// object on stdout (consumed by the dashboard `migrate.scan`/`migrate.apply`
/// RPCs, which spawn this binary). All log noise already routes to stderr, so
/// stdout stays a clean protocol channel in `--json` mode.
pub async fn run(
    platform: &str,
    source: Option<PathBuf>,
    apply: bool,
    rename: bool,
    json: bool,
) -> Result<()> {
    let plat = match platform.trim().to_ascii_lowercase().as_str() {
        "openclaw" | "moltbot" | "clawdbot" => Platform::OpenClaw,
        "hermes" => Platform::Hermes,
        "paperclip" => Platform::Paperclip,
        other => {
            return Err(DuDuClawError::Config(format!(
                "未知平台 '{other}'。支援: openclaw / hermes / paperclip"
            )));
        }
    };

    // paperclip cannot be discovered — it needs an explicit official export dir.
    if plat == Platform::Paperclip && source.is_none() {
        if json {
            // No console help in machine mode — surface a fatal error so the
            // caller renders an error frame instead of parsing help text.
            return Err(DuDuClawError::Config(
                "paperclip 轉移需要 --source 指向官方匯出目錄".to_string(),
            ));
        }
        paperclip::print_export_help();
        return Ok(());
    }

    let home = crate::duduclaw_home();
    let ctx = Ctx {
        home,
        platform: plat,
        apply,
        rename,
    };

    let mut report = match plat {
        Platform::OpenClaw => openclaw::migrate(&ctx, source).await,
        Platform::Hermes => hermes::migrate(&ctx, source).await,
        Platform::Paperclip => paperclip::migrate(&ctx, source).await,
    }?;

    // Always note the v1 honest boundaries so the operator knows the edges.
    report.note(
        "v1 不解析對話歷史入 sessions.db；原始 session 檔已原樣歸檔到 imported/<platform>/raw/。",
    );

    // Apply writes the on-disk markdown archive; capture its path for the report.
    let report_path = if apply {
        Some(write_report_file(&ctx, &report)?)
    } else {
        None
    };

    if json {
        let path_str = report_path.as_ref().map(|p| p.display().to_string());
        let value = report.to_json(path_str);
        // Exactly one JSON object on stdout — nothing else.
        println!("{}", serde_json::to_string(&value)?);
        return Ok(());
    }

    report.render_console();

    if let Some(path) = report_path {
        use console::style;
        println!("  {} 報告已寫入: {}", style("✓").green(), path.display());
        println!();
    }

    Ok(())
}

/// Write the markdown report to `~/.duduclaw/imported/<platform>/migration-report.md`.
/// Returns the path written so callers can surface it (console line / JSON field).
fn write_report_file(ctx: &Ctx, report: &Report) -> Result<PathBuf> {
    let dir = ctx.imported_dir();
    std::fs::create_dir_all(&dir).map_err(|e| {
        DuDuClawError::Io(std::io::Error::other(format!(
            "建立 {} 失敗: {e}",
            dir.display()
        )))
    })?;
    let path = dir.join("migration-report.md");
    std::fs::write(&path, report.render_markdown()).map_err(|e| {
        DuDuClawError::Io(std::io::Error::other(format!(
            "寫入報告 {} 失敗: {e}",
            path.display()
        )))
    })?;
    Ok(path)
}

// ─────────────────────── Pure helpers (unit-tested) ───────────────────────

/// Mask a secret for display: keep first 4 + last 4 chars, hide the middle.
/// Char-based (never byte slicing) so CJK/emoji tokens don't panic.
pub(super) fn mask_token(tok: &str) -> String {
    let chars: Vec<char> = tok.chars().collect();
    let n = chars.len();
    if n == 0 {
        return String::new();
    }
    if n <= 8 {
        return "*".repeat(n);
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[n - 4..].iter().collect();
    format!("{head}…{tail}")
}

/// Map a source model string to DuDuClaw `[model] preferred`.
///
/// Strips a leading `anthropic/` provider prefix. Any other value is kept
/// verbatim; `needs_review` is set when the resulting id is not a Claude model
/// (the caller marks the item PARTIAL so a human confirms the runtime mapping).
pub(super) fn map_model(raw: &str) -> (String, bool) {
    let trimmed = raw.trim();
    let stripped = trimmed
        .strip_prefix("anthropic/")
        .unwrap_or(trimmed)
        .to_string();
    let needs_review = !stripped.starts_with("claude");
    (stripped, needs_review)
}

/// Parse a `.env`-style file into key→value. Handles `export KEY=val`,
/// `# comments`, blank lines, and one layer of surrounding single/double
/// quotes. Values are otherwise kept verbatim.
pub(super) fn parse_env_file(content: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let key = k.trim().to_string();
        if key.is_empty() {
            continue;
        }
        let mut val = v.trim().to_string();
        let bytes = val.as_bytes();
        // Strip one matching pair of surrounding quotes (quotes are ASCII, so
        // slicing at [1..len-1] is always on a char boundary).
        if bytes.len() >= 2
            && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
                || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
        {
            val = val[1..val.len() - 1].to_string();
        }
        map.insert(key, val);
    }
    map
}

/// Split a Markdown document into (YAML frontmatter, body).
///
/// Frontmatter is recognised only when the document opens with a `---` fence
/// line and has a matching closing `---`. Returns `(None, whole_document)`
/// otherwise. Handcrafted (no external frontmatter crate) per the spec.
pub(super) fn parse_frontmatter(content: &str) -> (Option<serde_yaml::Value>, String) {
    let normalized = content.strip_prefix('\u{feff}').unwrap_or(content);
    let mut lines = normalized.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return (None, content.to_string());
    }
    let mut yaml_lines: Vec<&str> = Vec::new();
    let mut body_lines: Vec<&str> = Vec::new();
    let mut in_body = false;
    for line in lines {
        if !in_body && line.trim_end() == "---" {
            in_body = true;
            continue;
        }
        if in_body {
            body_lines.push(line);
        } else {
            yaml_lines.push(line);
        }
    }
    if !in_body {
        // No closing fence → not valid frontmatter, treat all as body.
        return (None, content.to_string());
    }
    let yaml_src = yaml_lines.join("\n");
    let parsed = serde_yaml::from_str::<serde_yaml::Value>(&yaml_src).ok();
    (parsed, body_lines.join("\n"))
}

/// Extract Markdown bullet lines (`-`, `*`, `+`) as individual facts.
pub(super) fn extract_bullets(content: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in content.lines() {
        let t = line.trim();
        let stripped = t
            .strip_prefix("- ")
            .or_else(|| t.strip_prefix("* "))
            .or_else(|| t.strip_prefix("+ "));
        if let Some(s) = stripped {
            let s = s.trim();
            if !s.is_empty() {
                out.push(s.to_string());
            }
        }
    }
    out
}

/// A defensively-parsed cron job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CronJob {
    pub name: String,
    pub cron: String,
    pub task: String,
}

/// Defensively parse one cron entry from an arbitrary JSON object. Tries the
/// common key spellings for schedule / task / name; returns `None` (→ the
/// caller records SKIPPED) when the schedule or task cannot be found — never
/// fabricates a value.
pub(super) fn parse_cron_job(v: &serde_json::Value) -> Option<CronJob> {
    let obj = v.as_object()?;
    let get = |keys: &[&str]| -> Option<String> {
        for k in keys {
            if let Some(s) = obj.get(*k).and_then(|x| x.as_str())
                && !s.trim().is_empty()
            {
                return Some(s.trim().to_string());
            }
        }
        None
    };
    let cron = get(&[
        "cron",
        "schedule",
        "cron_expr",
        "cronExpression",
        "expression",
    ])?;
    let task = get(&[
        "task",
        "prompt",
        "message",
        "command",
        "text",
        "instruction",
    ])?;
    let name =
        get(&["name", "id", "title", "label"]).unwrap_or_else(|| "imported-cron".to_string());
    Some(CronJob { name, cron, task })
}

/// Result of a `reports_to` topological sort.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TopoOutcome {
    /// Agents ordered parents-before-children (safe creation order).
    Sorted(Vec<String>),
    /// A cycle exists; carries the ids involved. Caller creates all agents
    /// with an empty `reports_to` and marks the run PARTIAL.
    Cycle(Vec<String>),
}

/// Topologically sort agents by `reports_to` (parent must precede child).
///
/// Input is `(id, reports_to)` pairs; a `reports_to` pointing outside the set
/// (or `None`) is treated as a root. Deterministic: ties break in input order.
pub(super) fn topo_sort_agents(nodes: &[(String, Option<String>)]) -> TopoOutcome {
    use std::collections::BTreeSet;
    let ids: BTreeSet<&str> = nodes.iter().map(|(id, _)| id.as_str()).collect();

    // indegree by index; children adjacency by index.
    let mut indeg = vec![0usize; nodes.len()];
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    let index_of: BTreeMap<&str, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, (id, _))| (id.as_str(), i))
        .collect();

    for (i, (_id, rt)) in nodes.iter().enumerate() {
        if let Some(parent) = rt
            && parent != &nodes[i].0
            && ids.contains(parent.as_str())
        {
            let p = index_of[parent.as_str()];
            indeg[i] += 1;
            children[p].push(i);
        }
    }

    // Seed the queue in input order for a stable result.
    let mut queue: VecDeque<usize> = (0..nodes.len()).filter(|&i| indeg[i] == 0).collect();
    let mut order: Vec<String> = Vec::with_capacity(nodes.len());
    while let Some(i) = queue.pop_front() {
        order.push(nodes[i].0.clone());
        for &c in &children[i] {
            indeg[c] -= 1;
            if indeg[c] == 0 {
                queue.push_back(c);
            }
        }
    }

    if order.len() == nodes.len() {
        TopoOutcome::Sorted(order)
    } else {
        let stuck: Vec<String> = nodes
            .iter()
            .enumerate()
            .filter(|(i, _)| indeg[*i] > 0)
            .map(|(_, (id, _))| id.clone())
            .collect();
        TopoOutcome::Cycle(stuck)
    }
}

/// Normalise an arbitrary source name into a filesystem-safe agent id.
pub(super) fn sanitize_agent_id(raw: &str) -> String {
    let lowered = raw.trim().to_lowercase();
    let mut s: String = lowered
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    let s = s.trim_matches('-').to_string();
    duduclaw_core::truncate_chars(&s, 64)
}
