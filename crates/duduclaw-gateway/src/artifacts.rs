//! I-2b: **產物物件化** — provenance for the files an AI staff member hands over.
//!
//! `office_docs.rs` already decides *what* a deliverable is: the agent declares
//! it with a `📎DELIVER:` marker, and `sweep_undeclared_deliverables` is the
//! deterministic net for the "wrote it but forgot the marker" half. Both paths
//! then archive a copy under `<agent>/attachments/` — and that is exactly where
//! the knowledge died. The dashboard's `/files` page saw a flat directory in
//! which a customer-sent PDF and a report the agent produced for a goal task
//! are byte-identical citizens: no direction, no task, no round, no time other
//! than mtime.
//!
//! This module is the missing ledger. Every write into `attachments/` now
//! appends one row here, and the read side joins it back so a file can answer
//! "who made me, for which task, and did the agent declare me or did we sweep
//! me up?".
//!
//! ## Honesty rules (this is provenance, not a story)
//!
//! - Direction is recorded at the write site, never guessed later. An inbound
//!   channel attachment is [`ArtifactOrigin::Uploaded`] because
//!   `media::save_attachment_in_base` is only ever reached from an inbound
//!   path; the deliverable archive in `office_docs` records
//!   [`ArtifactOrigin::Declared`] / [`ArtifactOrigin::Swept`] explicitly.
//! - Files that predate the ledger are backfilled ONLY as far as evidence
//!   reaches ([`backfill`]). Anything the evidence cannot place stays
//!   [`ArtifactOrigin::Unknown`] — 「來源不明」 is the honest answer and the
//!   dashboard says so, rather than assuming "the agent probably made it".
//! - Task attribution is two-tier and labelled: [`Attribution::Exact`] when the
//!   row itself carries the task id (or the task's own change ledger names the
//!   file), [`Attribution::Inferred`] when only the (agent, claim→review
//!   window) convention places it there — the same window `task_changes` uses
//!   for its MCP half. The UI never presents an inference as a fact.
//! - Every failure (missing dir, lock error, malformed line) degrades to
//!   "fewer/no rows". Recording provenance must never break a delivery that
//!   already succeeded.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use duduclaw_core::truncate_bytes;
use tracing::warn;

/// Durable provenance ledger, next to `tool_calls.jsonl` / `task_changes.jsonl`.
pub const ARTIFACTS_FILE: &str = "artifacts.jsonl";

/// Rotation threshold — rows are tiny (names + ids), so this is a long history.
const ROTATION_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Tail window read back per query. Bounded so a page render never pays for a
/// whole-file scan.
const TAIL_READ_BYTES: u64 = 2 * 1024 * 1024;

/// Byte cap for any single stored name / path (CJK-safe truncation).
const NAME_MAX_BYTES: usize = 400;

/// Default / hard ceiling for a query's row count.
pub const DEFAULT_QUERY_LIMIT: usize = 100;
pub const MAX_QUERY_LIMIT: usize = 500;

/// Files inspected per attachments directory during [`backfill`]. A directory
/// larger than this is backfilled newest-first up to the cap rather than
/// stalling boot.
const BACKFILL_MAX_FILES_PER_DIR: usize = 2000;

/// WP-4B: total bytes archived per task in one
/// [`archive_goal_task_artifacts`] call — a multiple of the per-file cap
/// (`media::MAX_FILE_SIZE`) so an unattended goal-loop settle cannot fill
/// disk even when a task legitimately produced several near-max-size
/// deliverables across its rounds.
const GOAL_ARCHIVE_MAX_TOTAL_BYTES: u64 = 5 * crate::media::MAX_FILE_SIZE;

// ── Shape ────────────────────────────────────────────────────────────────

/// Where a file in `attachments/` came from. Recorded at the write site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactOrigin {
    /// The agent declared it with `📎DELIVER:` — an explicit hand-over.
    Declared,
    /// `sweep_undeclared_deliverables` recovered it: the agent produced the
    /// file but forgot the marker.
    Swept,
    /// A human sent it in through a channel (inbound attachment).
    Uploaded,
    /// Derived read-side from the task's own change ledger — the round wrote a
    /// deliverable-shaped file. Never persisted to the ledger.
    Produced,
    /// Backfilled from a file whose direction the evidence could not establish.
    Unknown,
}

impl ArtifactOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            ArtifactOrigin::Declared => "declared",
            ArtifactOrigin::Swept => "swept",
            ArtifactOrigin::Uploaded => "uploaded",
            ArtifactOrigin::Produced => "produced",
            ArtifactOrigin::Unknown => "unknown",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s {
            "declared" => Some(ArtifactOrigin::Declared),
            "swept" => Some(ArtifactOrigin::Swept),
            "uploaded" => Some(ArtifactOrigin::Uploaded),
            "produced" => Some(ArtifactOrigin::Produced),
            "unknown" => Some(ArtifactOrigin::Unknown),
            _ => None,
        }
    }

    /// `true` when the file left the company (something the agent produced),
    /// as opposed to something a human sent in or a file of unknown direction.
    pub fn is_outbound(self) -> bool {
        matches!(
            self,
            ArtifactOrigin::Declared | ArtifactOrigin::Swept | ArtifactOrigin::Produced
        )
    }
}

/// How confidently a row is tied to a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attribution {
    /// The row itself names the task (or the task's change ledger names the file).
    Exact,
    /// Only the (agent, claim→review window) convention places it here.
    Inferred,
}

impl Attribution {
    pub fn as_str(self) -> &'static str {
        match self {
            Attribution::Exact => "exact",
            Attribution::Inferred => "inferred",
        }
    }
}

/// One provenance row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactRecord {
    /// RFC3339 UTC — when the file landed in `attachments/`.
    pub produced_at: String,
    /// Owning agent, or empty for the shared (`<home>/attachments/`) bucket.
    pub agent_id: String,
    /// The on-disk name inside `attachments/` (`<ts>_<sanitized>`), i.e. the
    /// key `/api/files` lists and `/api/files/download` accepts.
    pub archived_name: String,
    /// The original, human-meaningful filename.
    pub display_name: String,
    pub size: u64,
    pub origin: ArtifactOrigin,
    pub task_id: Option<String>,
    pub round: Option<u32>,
    /// Channel the hand-over went through (`telegram`, `webchat`, …).
    pub channel: Option<String>,
    /// Where the agent produced it, when known (outbound rows only).
    pub source_path: Option<String>,
}

impl ArtifactRecord {
    fn to_json(&self) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        map.insert("produced_at".into(), self.produced_at.clone().into());
        map.insert("agent_id".into(), self.agent_id.clone().into());
        map.insert("archived_name".into(), self.archived_name.clone().into());
        map.insert("display_name".into(), self.display_name.clone().into());
        map.insert("size".into(), self.size.into());
        map.insert("origin".into(), self.origin.as_str().into());
        if let Some(t) = &self.task_id {
            map.insert("task_id".into(), t.clone().into());
        }
        if let Some(r) = self.round {
            map.insert("round".into(), r.into());
        }
        if let Some(c) = &self.channel {
            map.insert("channel".into(), c.clone().into());
        }
        if let Some(p) = &self.source_path {
            map.insert("source_path".into(), p.clone().into());
        }
        serde_json::Value::Object(map)
    }

    fn from_line(line: &str) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_str(line).ok()?;
        Some(ArtifactRecord {
            produced_at: v.get("produced_at").and_then(|x| x.as_str())?.to_string(),
            agent_id: v
                .get("agent_id")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            archived_name: v.get("archived_name").and_then(|x| x.as_str())?.to_string(),
            display_name: v
                .get("display_name")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            size: v.get("size").and_then(|x| x.as_u64()).unwrap_or(0),
            origin: v
                .get("origin")
                .and_then(|x| x.as_str())
                .and_then(ArtifactOrigin::from_str)
                .unwrap_or(ArtifactOrigin::Unknown),
            task_id: v
                .get("task_id")
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            round: v
                .get("round")
                .and_then(|x| x.as_u64())
                .and_then(|n| u32::try_from(n).ok()),
            channel: v
                .get("channel")
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            source_path: v
                .get("source_path")
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        })
    }
}

/// The write-site context handed to [`record_saved`].
#[derive(Debug, Clone, Copy)]
pub struct SaveContext<'a> {
    pub origin: ArtifactOrigin,
    pub task_id: Option<&'a str>,
    pub round: Option<u32>,
    pub channel: Option<&'a str>,
    pub source_path: Option<&'a Path>,
}

impl<'a> SaveContext<'a> {
    /// An inbound channel attachment — the default for every
    /// `media::save_attachment_in_base` caller.
    pub fn uploaded(channel: Option<&'a str>) -> Self {
        SaveContext {
            origin: ArtifactOrigin::Uploaded,
            task_id: None,
            round: None,
            channel,
            source_path: None,
        }
    }

    /// An outbound deliverable (`📎DELIVER:` or the sweep).
    pub fn delivered(origin: ArtifactOrigin, channel: Option<&'a str>, source: &'a Path) -> Self {
        SaveContext {
            origin,
            task_id: None,
            round: None,
            channel,
            source_path: Some(source),
        }
    }
}

// ── Path helpers ─────────────────────────────────────────────────────────

/// Mirror of `media::save_attachment_in_base`'s sanitizer — the archived name
/// is `<ts_millis>_<sanitize(original)>`, so matching a workdir file against
/// its archived copy means comparing sanitized basenames.
pub fn sanitize_name(filename: &str) -> String {
    filename
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Split an archived name back into `(millis, sanitized_original)`.
/// `None` when the name does not follow the `<ts>_<name>` convention.
pub fn split_archived_name(archived: &str) -> Option<(u64, &str)> {
    let (ts, rest) = archived.split_once('_')?;
    let millis: u64 = ts.parse().ok()?;
    if rest.is_empty() {
        return None;
    }
    Some((millis, rest))
}

/// Derive `(home_dir, agent_id)` from an attachment base directory.
///
/// `<home>/agents/<id>` ⇒ `(home, id)`; anything else is treated as the shared
/// home bucket ⇒ `(base, "")`. Mirrors `media::record_ext_gap_signal`.
pub fn derive_home_and_agent(base_dir: &Path) -> (PathBuf, String) {
    if let (Some(name), Some(parent)) = (
        base_dir.file_name().and_then(|n| n.to_str()),
        base_dir.parent(),
    ) && parent.file_name().and_then(|n| n.to_str()) == Some("agents")
        && let Some(home) = parent.parent()
    {
        return (home.to_path_buf(), name.to_string());
    }
    (base_dir.to_path_buf(), String::new())
}

/// Extensions a human would call a deliverable. Used only for the read-side
/// [`ArtifactOrigin::Produced`] derivation — the declared/swept write paths
/// record whatever the agent actually handed over.
const ARTIFACT_EXTS: &[&str] = &[
    "docx", "doc", "xlsx", "xls", "pptx", "ppt", "pdf", "csv", "odt", "ods", "odp", "md", "html",
    "htm", "png", "jpg", "jpeg", "gif", "svg", "zip",
];

/// Filenames that are agent machinery, never a hand-over, even though their
/// extension is in [`ARTIFACT_EXTS`]. Exact match (coding convention 2).
const INTERNAL_NAMES: &[&str] = &[
    "SOUL.md",
    "CLAUDE.md",
    "AGENTS.md",
    "GEMINI.md",
    "CONTRACT.toml",
    "README.md",
    "TEAM.md",
];

/// Path segments that mark internal state rather than work product.
const INTERNAL_DIRS: &[&str] = &[
    "state",
    "sessions",
    "logs",
    "memory",
    "skills",
    "evals",
    ".claude",
    "node_modules",
    "target",
];

/// `true` when a written path looks like a deliverable rather than agent
/// machinery. Conservative on purpose: a missed row is recoverable from the
/// 「變更」tab, a wrong one teaches the user the 產物 list cannot be trusted.
pub fn is_artifact_path(path: &str) -> bool {
    let p = Path::new(path);
    let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if INTERNAL_NAMES.contains(&name) {
        return false;
    }
    let ext_ok = p
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ARTIFACT_EXTS.contains(&e.to_ascii_lowercase().as_str()));
    if !ext_ok {
        return false;
    }
    // Any internal directory anywhere on the path disqualifies it.
    !p.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|s| INTERNAL_DIRS.contains(&s))
    })
}

// ── Write side ───────────────────────────────────────────────────────────

/// Append one provenance row for a file just written into
/// `<base_dir>/attachments/`.
///
/// `saved_path` is the concrete archived path; `display_name` the original
/// filename. Entirely best-effort — a lock failure or a serialization error
/// drops the row rather than disturbing the delivery.
pub fn record_saved(
    base_dir: &Path,
    saved_path: &Path,
    display_name: &str,
    size: u64,
    ctx: &SaveContext<'_>,
) {
    let Some(archived_name) = saved_path.file_name().and_then(|n| n.to_str()) else {
        return;
    };
    let (home, agent_id) = derive_home_and_agent(base_dir);
    let rec = ArtifactRecord {
        produced_at: chrono::Utc::now().to_rfc3339(),
        agent_id,
        archived_name: truncate_bytes(archived_name, NAME_MAX_BYTES).to_string(),
        display_name: truncate_bytes(display_name, NAME_MAX_BYTES).to_string(),
        size,
        origin: ctx.origin,
        task_id: ctx.task_id.filter(|s| !s.is_empty()).map(str::to_string),
        round: ctx.round,
        channel: ctx.channel.filter(|s| !s.is_empty()).map(str::to_string),
        source_path: ctx
            .source_path
            .and_then(|p| p.to_str())
            .map(|p| truncate_bytes(p, NAME_MAX_BYTES).to_string()),
    };
    append_rows(&home, std::slice::from_ref(&rec.to_json()));
}

fn append_rows(home_dir: &Path, rows: &[serde_json::Value]) {
    if rows.is_empty() {
        return;
    }
    let path = home_dir.join(ARTIFACTS_FILE);
    maybe_rotate(&path);
    let mut body = String::new();
    for row in rows {
        if let Ok(line) = serde_json::to_string(row) {
            body.push_str(&line);
            body.push('\n');
        }
    }
    if body.is_empty() {
        return;
    }
    // Cross-process advisory lock (coding convention 3): the ledger sits in a
    // directory several processes append to.
    let _ = duduclaw_core::with_file_lock(&path, || {
        use std::io::Write as _;
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).append(true);
        // Rows carry filenames a customer sent in — never world-readable.
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = f.metadata()
                && meta.permissions().mode() & 0o077 != 0
            {
                let mut perms = meta.permissions();
                perms.set_mode(0o600);
                let _ = f.set_permissions(perms);
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
    let _ = std::fs::rename(path, path.with_extension("jsonl.1"));
}

// ── Read side ────────────────────────────────────────────────────────────

/// Read the ledger tail. Newest rows last (file order preserved).
fn read_ledger(home_dir: &Path) -> Vec<ArtifactRecord> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let path = home_dir.join(ARTIFACTS_FILE);
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
        .filter_map(|line| ArtifactRecord::from_line(line.trim()))
        .collect()
}

/// Provenance for one archived file, as `/api/files` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileProvenance {
    pub origin: ArtifactOrigin,
    pub task_id: Option<String>,
    pub round: Option<u32>,
    pub display_name: String,
    pub channel: Option<String>,
}

/// Build the `archived_name → provenance` index for one bucket
/// (`Some(agent)` or the shared bucket). Later rows win, so a backfilled
/// `unknown` row is superseded by a real one written afterwards.
pub fn provenance_index(home_dir: &Path, agent: Option<&str>) -> BTreeMap<String, FileProvenance> {
    let want = agent.unwrap_or("");
    let mut out = BTreeMap::new();
    for rec in read_ledger(home_dir) {
        if rec.agent_id != want {
            continue;
        }
        out.insert(
            rec.archived_name.clone(),
            FileProvenance {
                origin: rec.origin,
                task_id: rec.task_id,
                round: rec.round,
                display_name: rec.display_name,
                channel: rec.channel,
            },
        );
    }
    out
}

/// One row of the task detail page's 「產物」tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskArtifact {
    /// Human-meaningful name.
    pub name: String,
    /// The `/api/files/download?name=` key. `None` ⇒ the file was written but
    /// never archived, so there is nothing to hand the browser.
    pub archived_name: Option<String>,
    pub agent_id: String,
    pub origin: ArtifactOrigin,
    pub attribution: Attribution,
    pub produced_at: String,
    pub size: Option<u64>,
    pub round: Option<u32>,
    pub channel: Option<String>,
    /// Where the agent wrote it, when known.
    pub source_path: Option<String>,
}

impl TaskArtifact {
    pub fn to_wire_json(&self) -> serde_json::Value {
        serde_json::json!({
            "name": self.name,
            "archived_name": self.archived_name,
            "agent_id": self.agent_id,
            "origin": self.origin.as_str(),
            "attribution": self.attribution.as_str(),
            "produced_at": self.produced_at,
            "size": self.size,
            "round": self.round,
            "channel": self.channel,
            "source_path": self.source_path,
        })
    }
}

/// Everything the 「產物」tab renders.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskArtifactEvidence {
    /// Newest first, capped at the caller's limit.
    pub artifacts: Vec<TaskArtifact>,
    /// `true` when rows were dropped by the limit.
    pub truncated: bool,
    /// How many rows are only placed by the time window, not by an id.
    pub inferred_count: usize,
}

/// Merge key: the file's own basename, so 摘要.md written in a round and
/// 摘要.md archived on delivery are one artifact.
///
/// Deliberately NOT the sanitized form as the primary key: sanitization maps
/// every non-ASCII character to `_`, so 報告.md and 摘要.md would collide and a
/// false merge silently loses a distinct deliverable. The sanitized form is
/// only used as a secondary alias in [`merge_artifacts`]'s reconciliation pass,
/// to fold in backfilled rows whose original name is genuinely unrecoverable.
fn merge_key(rec: &TaskArtifact) -> String {
    Path::new(&rec.name)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&rec.name)
        .to_string()
}

/// Fold `item` into an already-seen row for the same file. The downloadable
/// row wins the identity of the pair; everything else is filled in from
/// whichever half knows it, and a real basename always beats a sanitized
/// stand-in.
fn merge_into(existing: &mut TaskArtifact, item: TaskArtifact) {
    let existing_is_sanitized = existing.name == sanitize_name(&existing.name);
    let item_is_sanitized = item.name == sanitize_name(&item.name);
    if existing_is_sanitized && !item_is_sanitized {
        existing.name = item.name.clone();
    }
    if existing.attribution != Attribution::Exact && item.attribution == Attribution::Exact {
        existing.attribution = Attribution::Exact;
    }
    if existing.round.is_none() {
        existing.round = item.round;
    }
    if existing.source_path.is_none() {
        existing.source_path = item.source_path.clone();
    }
    if existing.channel.is_none() {
        existing.channel = item.channel.clone();
    }
    if existing.size.is_none() {
        existing.size = item.size;
    }
    if existing.archived_name.is_none() && item.archived_name.is_some() {
        // The archived copy is the row a human can actually open, so its
        // hand-over time and origin describe the artifact from here on.
        existing.archived_name = item.archived_name;
        existing.produced_at = item.produced_at;
        if item.origin.is_outbound() && item.origin != ArtifactOrigin::Produced {
            existing.origin = item.origin;
        }
        if existing.agent_id.is_empty() {
            existing.agent_id = item.agent_id;
        }
    }
}

/// Fold rows describing the same file into one.
///
/// Two passes, deliberately order-independent. Pass 1 groups on the real
/// basename — never on the sanitized form, since sanitization maps every CJK
/// character to `_` and 報告.md / 摘要.md would collide, silently losing a
/// distinct deliverable. Pass 2 reconciles the one case where the original name
/// is genuinely unrecoverable (a backfilled row, whose display name IS the
/// sanitized form): it folds into a real-named row only when EXACTLY ONE
/// candidate matches — an ambiguous match stays two rows, which is the honest
/// outcome.
fn merge_artifacts(items: Vec<TaskArtifact>) -> Vec<TaskArtifact> {
    let mut buckets: BTreeMap<String, TaskArtifact> = BTreeMap::new();
    for item in items {
        let key = merge_key(&item);
        match buckets.get_mut(&key) {
            Some(existing) => merge_into(existing, item),
            None => {
                buckets.insert(key, item);
            }
        }
    }
    let sanitized_keys: Vec<String> = buckets
        .keys()
        .filter(|k| **k == sanitize_name(k))
        .cloned()
        .collect();
    for key in sanitized_keys {
        let candidates: Vec<String> = buckets
            .keys()
            .filter(|j| **j != key && sanitize_name(j) == key)
            .cloned()
            .collect();
        let [target] = candidates.as_slice() else {
            continue;
        };
        let target = target.clone();
        if let Some(item) = buckets.remove(&key)
            && let Some(existing) = buckets.get_mut(&target)
        {
            merge_into(existing, item);
        }
    }
    buckets.into_values().collect()
}

/// Collect a task's deliverables from both trails.
///
/// 1. **Ledger rows** (`artifacts.jsonl`) — exact when the row carries
///    `task_id`; inferred when only `agent_id` + the `[since, until]`
///    claim→review window place it there (uploads and unknown-direction rows
///    are never inferred into a task: only something the agent handed over is
///    a 產物).
/// 2. **Change-ledger rows** (`task_changes.jsonl`) — writes/edits of
///    deliverable-shaped paths inside this task's rounds, already keyed by task
///    id. These carry the round number and are always exact.
///
/// Rows describing the same file merge, preferring the one that can be
/// downloaded. An empty result is an empty result — never a summary.
pub fn collect_task_artifacts(
    home_dir: &Path,
    task_id: &str,
    agent_id: &str,
    since: &str,
    until: &str,
    limit: usize,
) -> TaskArtifactEvidence {
    if task_id.is_empty() {
        return TaskArtifactEvidence::default();
    }
    let limit = limit.clamp(1, MAX_QUERY_LIMIT);
    let window = match (
        chrono::DateTime::parse_from_rfc3339(since),
        chrono::DateTime::parse_from_rfc3339(until),
    ) {
        (Ok(a), Ok(b)) if a <= b => Some((a, b)),
        // An unparseable window drops the inferred half rather than widening
        // it to "everything this agent ever delivered".
        _ => None,
    };

    let mut collected: Vec<TaskArtifact> = Vec::new();

    for rec in read_ledger(home_dir) {
        let exact = rec.task_id.as_deref() == Some(task_id);
        let inferred = !exact
            && rec.task_id.is_none()
            && !agent_id.is_empty()
            && rec.agent_id == agent_id
            && rec.origin.is_outbound()
            && window.is_some_and(|(a, b)| {
                chrono::DateTime::parse_from_rfc3339(&rec.produced_at)
                    .map(|ts| ts >= a && ts <= b)
                    .unwrap_or(false)
            });
        if !exact && !inferred {
            continue;
        }
        collected.push(TaskArtifact {
            name: if rec.display_name.is_empty() {
                rec.archived_name.clone()
            } else {
                rec.display_name.clone()
            },
            archived_name: Some(rec.archived_name.clone()),
            agent_id: rec.agent_id.clone(),
            origin: rec.origin,
            attribution: if exact {
                Attribution::Exact
            } else {
                Attribution::Inferred
            },
            produced_at: rec.produced_at.clone(),
            size: Some(rec.size),
            round: rec.round,
            channel: rec.channel.clone(),
            source_path: rec.source_path.clone(),
        });
    }

    for change in crate::task_changes::collect_task_changes(
        home_dir,
        task_id,
        // The MCP half of the change ledger is not a file hand-over surface —
        // pass an empty agent so only the task-keyed native rows come back.
        "",
        "",
        "",
        crate::task_changes::MAX_QUERY_LIMIT,
    )
    .changes
    {
        if !matches!(
            change.op,
            crate::task_changes::ChangeOp::Write | crate::task_changes::ChangeOp::Edit
        ) || !change.success
            || !is_artifact_path(&change.path)
        {
            continue;
        }
        let name = Path::new(&change.path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&change.path)
            .to_string();
        collected.push(TaskArtifact {
            name,
            archived_name: None,
            agent_id: agent_id.to_string(),
            origin: ArtifactOrigin::Produced,
            attribution: Attribution::Exact,
            produced_at: change.timestamp.clone(),
            size: None,
            round: change.round,
            channel: None,
            source_path: Some(change.path.clone()),
        });
    }

    let mut all = merge_artifacts(collected);
    all.sort_by(|a, b| b.produced_at.cmp(&a.produced_at).then(a.name.cmp(&b.name)));
    let truncated = all.len() > limit;
    all.truncate(limit);
    let inferred_count = all
        .iter()
        .filter(|a| a.attribution == Attribution::Inferred)
        .count();
    TaskArtifactEvidence {
        artifacts: all,
        truncated,
        inferred_count,
    }
}

// ── Backfill ─────────────────────────────────────────────────────────────

/// What a [`backfill`] pass did. Every number is measured, never estimated.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackfillReport {
    /// Files inspected across all buckets.
    pub scanned: usize,
    /// Files that already had a ledger row (a re-run adds nothing).
    pub already_known: usize,
    /// Rows written with a task attribution recovered from `task_changes.jsonl`.
    pub attributed: usize,
    /// Rows written as `unknown` — evidence could not place them.
    pub unknown: usize,
}

impl BackfillReport {
    pub fn added(&self) -> usize {
        self.attributed + self.unknown
    }
}

/// One-time, re-runnable provenance backfill for files that predate the ledger.
///
/// Idempotent by construction: a file already present in the ledger is skipped,
/// so running it every boot costs a directory listing and nothing else.
///
/// Attribution is evidence-only. `task_changes.jsonl` is the one durable trail
/// that names files an agent wrote *and* the task it wrote them for, so an
/// archived copy whose sanitized basename matches such a write is backfilled as
/// [`ArtifactOrigin::Swept`] (we know the agent produced it; we cannot know
/// whether it carried a `📎DELIVER:` marker at the time) with the task id and
/// round. Everything else becomes [`ArtifactOrigin::Unknown`] — an honest
/// 「來源不明」, never a guess about direction.
pub fn backfill(home_dir: &Path) -> BackfillReport {
    let mut report = BackfillReport::default();

    // (agent, sanitized basename) → (task_id, round, timestamp) from the change
    // ledger. Cheap: the same bounded tail read the 「變更」tab pays for.
    let writes = index_task_writes(home_dir);

    let mut buckets: Vec<(Option<String>, PathBuf)> = vec![(None, home_dir.join("attachments"))];
    if let Ok(entries) = std::fs::read_dir(home_dir.join("agents")) {
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if !crate::files_api::is_safe_agent_id(&name) {
                continue;
            }
            let dir = entry.path().join("attachments");
            buckets.push((Some(name), dir));
        }
    }

    for (agent, dir) in buckets {
        let agent_id = agent.clone().unwrap_or_default();
        let known: std::collections::BTreeSet<String> =
            provenance_index(home_dir, agent.as_deref())
                .into_keys()
                .collect();
        let mut rows: Vec<serde_json::Value> = Vec::new();
        for entry in list_bucket(&dir) {
            report.scanned += 1;
            let (archived_name, size, mtime_ms) = entry;
            if known.contains(&archived_name) {
                report.already_known += 1;
                continue;
            }
            let (millis, sanitized) = match split_archived_name(&archived_name) {
                Some((m, rest)) => (Some(m), rest.to_string()),
                None => (None, sanitize_name(&archived_name)),
            };
            let produced_at = millis
                .or(Some(mtime_ms))
                .and_then(|ms| chrono::DateTime::from_timestamp_millis(ms as i64))
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());

            let hit = writes.get(&(agent_id.clone(), sanitized.clone()));
            let rec = match hit {
                Some((task_id, round)) => {
                    report.attributed += 1;
                    ArtifactRecord {
                        produced_at,
                        agent_id: agent_id.clone(),
                        archived_name: archived_name.clone(),
                        display_name: sanitized.clone(),
                        size,
                        origin: ArtifactOrigin::Swept,
                        task_id: Some(task_id.clone()),
                        round: *round,
                        channel: None,
                        source_path: None,
                    }
                }
                None => {
                    report.unknown += 1;
                    ArtifactRecord {
                        produced_at,
                        agent_id: agent_id.clone(),
                        archived_name: archived_name.clone(),
                        display_name: sanitized.clone(),
                        size,
                        origin: ArtifactOrigin::Unknown,
                        task_id: None,
                        round: None,
                        channel: None,
                        source_path: None,
                    }
                }
            };
            rows.push(rec.to_json());
        }
        append_rows(home_dir, &rows);
    }
    report
}

/// `(name, size, mtime_millis)` for the regular files directly inside `dir`,
/// newest-first, capped.
fn list_bucket(dir: &Path) -> Vec<(String, u64, u64)> {
    let mut out: Vec<(String, u64, u64)> = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in rd.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        out.push((name, meta.len(), mtime));
    }
    out.sort_by(|a, b| b.2.cmp(&a.2));
    out.truncate(BACKFILL_MAX_FILES_PER_DIR);
    out
}

/// `(agent, sanitized basename) → (task_id, round)` from the change ledger —
/// the only durable trail that ties a written file to a task.
fn index_task_writes(home_dir: &Path) -> BTreeMap<(String, String), (String, Option<u32>)> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let mut out = BTreeMap::new();
    let path = home_dir.join(crate::task_changes::TASK_CHANGES_FILE);
    let Ok(mut file) = std::fs::File::open(&path) else {
        return out;
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(TAIL_READ_BYTES);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return out;
    }
    let mut raw = Vec::new();
    if file.read_to_end(&mut raw).is_err() {
        return out;
    }
    let text = String::from_utf8_lossy(&raw);
    let text = if start > 0 {
        match text.find('\n') {
            Some(pos) => &text[pos + 1..],
            None => return out,
        }
    } else {
        &text[..]
    };
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        let (Some(task_id), Some(agent_id), Some(p)) = (
            v.get("task_id").and_then(|x| x.as_str()),
            v.get("agent_id").and_then(|x| x.as_str()),
            v.get("path").and_then(|x| x.as_str()),
        ) else {
            continue;
        };
        if !matches!(v.get("op").and_then(|x| x.as_str()), Some("write" | "edit"))
            || !is_artifact_path(p)
        {
            continue;
        }
        let Some(base) = Path::new(p).file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let round = v
            .get("round")
            .and_then(|x| x.as_u64())
            .and_then(|n| u32::try_from(n).ok());
        // Later rows win: the most recent task to write this name owns it.
        out.insert(
            (agent_id.to_string(), sanitize_name(base)),
            (task_id.to_string(), round),
        );
    }
    out
}

// ── Goal-loop settle archiving (WP-4B) ──────────────────────────────────
//
// The declared/swept archive above only ever runs on the channel-reply path
// (`office_docs::deliver_one` / `sweep_undeclared_deliverables`), which a
// goal-loop dispatch round never touches — `dispatcher.rs` drives goal tasks
// straight through `claude_runner`, not `channel_reply`. So a goal task's
// deliverables previously existed only as an unarchived
// [`ArtifactOrigin::Produced`] breadcrumb in `task_changes.jsonl`
// (`archived_name: None`): the 「產物」tab could say WHERE a file was
// written, never offer a download. [`archive_goal_task_artifacts`] closes
// that gap by physically copying a task's produced files into the SAME
// `attachments/` bucket the declared/swept path uses, the moment a goal task
// is accepted — so the same `/api/files/download` endpoint and the same
// ledger merge logic ([`merge_into`]) that already handle "a Produced row
// gained a matching archived copy" pick it up with no further plumbing.

/// What one [`archive_goal_task_artifacts`] call did. Every number is
/// measured, never estimated — mirrors [`BackfillReport`]'s honesty stance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GoalArchiveReport {
    /// Files copied into `attachments/` and recorded in the ledger.
    pub archived: usize,
    /// A file with the same sanitized name + size already sat in
    /// `attachments/` (mirrors `office_docs::already_archived`) — not
    /// re-copied, since the running total must reflect real new disk usage.
    pub already_archived: usize,
    /// The source path was not absolute, could not be canonicalized, no
    /// longer exists, or is not a regular file by settle time (the agent may
    /// have deleted or moved it across rounds).
    pub skipped_missing: usize,
    /// The canonicalized source escaped the agent's own directory tree — a
    /// symlink-escape attempt, a non-absolute path, or a write outside the
    /// trusted workspace. Always logged (never silent) — this is a
    /// security-relevant skip, not a housekeeping one.
    pub skipped_outside_root: usize,
    /// The file (or the running per-task total) exceeded the size cap.
    /// Always logged (never silent).
    pub skipped_oversize: usize,
}

/// True when `dir` already holds an archived copy of `display_name` with
/// `size` bytes — the goal-loop counterpart of `office_docs::already_archived`
/// (private to that module), reusing the same `<ts>_<sanitized>` name+size
/// match so a task settled twice (or a file also delivered via the
/// declared/swept path earlier in the same round) does not duplicate the
/// copy on disk.
fn goal_archive_already_present(dir: &Path, display_name: &str, size: u64) -> bool {
    let target = sanitize_name(display_name);
    list_bucket(dir)
        .into_iter()
        .any(|(name, entry_size, _)| {
            entry_size == size
                && split_archived_name(&name).is_some_and(|(_, sanitized)| sanitized == target)
        })
}

/// WP-4B: archive a goal task's produced deliverable-shaped files into the
/// owning agent's `attachments/`, and record each copy in the provenance
/// ledger with an EXACT task/round attribution (unlike the read-side
/// inference [`collect_task_artifacts`] falls back to for undeclared uploads).
///
/// Called from `dispatch_engine.rs` at accept-settle — the moment a goal
/// task's phase closes as `done`. Sourced from `task_changes.jsonl`
/// (`task_changes::collect_task_changes`, native rows only) rather than a
/// single round's live `NativeToolEvent` slice, so a file written in an
/// EARLIER revision round (and never re-touched in the accepted round) is
/// still archived — the ledger already has it, this only makes it
/// downloadable. Rows are newest-first, so when the same path was written
/// more than once across rounds only its most recent version is archived.
///
/// ## Security (coding convention 4 — fail closed)
///
/// - Every candidate path must be absolute, canonicalize, and land inside the
///   agent's own directory tree (`<home>/agents/<agent_id>/`); anything
///   else — including a symlink planted to point outside it — is rejected
///   and counted in [`GoalArchiveReport::skipped_outside_root`], never
///   silently followed.
/// - `agent_id` is validated against the same allowlist the `/api/files`
///   endpoints use ([`crate::files_api::is_safe_agent_id`]) BEFORE it is
///   joined into a path — a malformed id must not be able to walk the join
///   itself outside `<home>/agents/`.
/// - A path already inside `attachments/` is skipped (it is already an
///   archive; re-copying it would just duplicate disk usage and could loop
///   on its own output if ever called twice).
/// - Per-file (`media::MAX_FILE_SIZE`) and per-task
///   ([`GOAL_ARCHIVE_MAX_TOTAL_BYTES`]) byte caps stop an unattended loop
///   from filling disk; both are recorded in the report, never silently
///   dropped ("skipped-oversize" per the design brief).
///
/// Best-effort end to end: any single-file failure (copy error, missing
/// file, oversize) is skipped and counted, never propagated — this function
/// must not be able to fail (or even delay past a warning) the settle that
/// already committed the task's verdict.
pub async fn archive_goal_task_artifacts(
    home_dir: &Path,
    task_id: &str,
    agent_id: &str,
) -> GoalArchiveReport {
    let mut report = GoalArchiveReport::default();
    if task_id.is_empty() || agent_id.is_empty() {
        return report;
    }
    if !crate::files_api::is_safe_agent_id(agent_id) {
        warn!(
            task = task_id, agent = agent_id,
            "WP-4B goal archive: agent id failed the safety allowlist — archiving skipped entirely"
        );
        return report;
    }

    let agent_dir = home_dir.join("agents").join(agent_id);
    let Ok(canon_root) = std::fs::canonicalize(&agent_dir) else {
        // No agent workspace to trust ⇒ nothing to archive from, fail-open
        // (an agent directory that vanished mid-task is an anomaly the
        // caller's own logs already cover; this is not that caller).
        return report;
    };
    let attach_dir = agent_dir.join("attachments");
    let canon_attach = std::fs::canonicalize(&attach_dir).unwrap_or_else(|_| attach_dir.clone());

    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut total_bytes: u64 = 0;

    for change in crate::task_changes::collect_task_changes(
        home_dir,
        task_id,
        // Same convention as `collect_task_artifacts`: an empty agent drops
        // the MCP-audit half so only task-keyed native rows come back — MCP
        // tools (`shared_wiki_write` et al.) are not goal-loop file
        // deliverables in this sense.
        "",
        "",
        "",
        crate::task_changes::MAX_QUERY_LIMIT,
    )
    .changes
    {
        if !matches!(
            change.op,
            crate::task_changes::ChangeOp::Write | crate::task_changes::ChangeOp::Edit
        ) || !change.success
            || !is_artifact_path(&change.path)
        {
            continue;
        }
        // Newest-first input ⇒ the first time we see a path is its most
        // recent version; later (older) rows for the same path are skipped.
        if !seen.insert(change.path.clone()) {
            continue;
        }

        let src = Path::new(&change.path);
        if !src.is_absolute() {
            report.skipped_outside_root += 1;
            warn!(
                task = task_id, path = %change.path,
                "WP-4B goal archive: source path is not absolute — skipped"
            );
            continue;
        }
        let canon_src = match std::fs::canonicalize(src) {
            Ok(p) => p,
            Err(_) => {
                report.skipped_missing += 1;
                continue;
            }
        };
        if !canon_src.starts_with(&canon_root) || canon_src.starts_with(&canon_attach) {
            report.skipped_outside_root += 1;
            warn!(
                task = task_id, path = %change.path,
                "WP-4B goal archive: source path escapes the agent workspace — skipped (possible symlink escape)"
            );
            continue;
        }
        let meta = match std::fs::metadata(&canon_src) {
            Ok(m) if m.is_file() => m,
            _ => {
                report.skipped_missing += 1;
                continue;
            }
        };
        let size = meta.len();
        if size > crate::media::MAX_FILE_SIZE {
            report.skipped_oversize += 1;
            warn!(
                task = task_id, path = %change.path, size,
                "WP-4B goal archive: file exceeds the per-file cap — skipped"
            );
            continue;
        }
        if total_bytes.saturating_add(size) > GOAL_ARCHIVE_MAX_TOTAL_BYTES {
            report.skipped_oversize += 1;
            warn!(
                task = task_id, path = %change.path, size,
                "WP-4B goal archive: per-task archive budget exhausted — skipped"
            );
            continue;
        }

        let display_name = canon_src
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file");

        if goal_archive_already_present(&attach_dir, display_name, size) {
            report.already_archived += 1;
            continue;
        }

        let data = match tokio::fs::read(&canon_src).await {
            Ok(d) => d,
            Err(e) => {
                report.skipped_missing += 1;
                warn!(task = task_id, path = %change.path, error = %e, "WP-4B goal archive: read failed — skipped");
                continue;
            }
        };

        match crate::media::save_attachment_in_base_untracked(&agent_dir, &data, display_name)
            .await
        {
            Ok(saved) => {
                total_bytes = total_bytes.saturating_add(data.len() as u64);
                report.archived += 1;
                record_saved(
                    &agent_dir,
                    &saved,
                    display_name,
                    data.len() as u64,
                    &SaveContext {
                        origin: ArtifactOrigin::Swept,
                        task_id: Some(task_id),
                        round: change.round,
                        channel: None,
                        source_path: Some(src),
                    },
                );
            }
            Err(e) => {
                warn!(
                    task = task_id, path = %change.path, error = %e,
                    "WP-4B goal archive: archive copy failed — settle continues"
                );
            }
        }
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_declared<'a>(src: &'a Path) -> SaveContext<'a> {
        SaveContext::delivered(ArtifactOrigin::Declared, Some("telegram"), src)
    }

    #[test]
    fn sanitize_and_split_round_trip_the_archive_convention() {
        // One replacement per char (not per byte) — CJK collapses hard, which
        // is exactly why sanitized names are only a secondary merge key.
        assert_eq!(sanitize_name("報告 v2.docx"), "___v2.docx");
        assert_eq!(
            split_archived_name("1755000000000_report.docx"),
            Some((1_755_000_000_000, "report.docx"))
        );
        // Not the convention ⇒ None, never a bogus split.
        assert_eq!(split_archived_name("report.docx"), None);
        assert_eq!(split_archived_name("abc_report.docx"), None);
        assert_eq!(split_archived_name("1755_"), None);
    }

    #[test]
    fn derive_home_and_agent_handles_both_buckets() {
        let (home, agent) = derive_home_and_agent(Path::new("/h/agents/sales"));
        assert_eq!(home, PathBuf::from("/h"));
        assert_eq!(agent, "sales");
        // Shared bucket: base IS the home, no agent.
        let (home2, agent2) = derive_home_and_agent(Path::new("/h"));
        assert_eq!(home2, PathBuf::from("/h"));
        assert_eq!(agent2, "");
    }

    #[test]
    fn artifact_path_gate_excludes_machinery() {
        assert!(is_artifact_path("/h/agents/a/報告.docx"));
        assert!(is_artifact_path("/h/agents/a/out/summary.md"));
        assert!(is_artifact_path("/h/agents/a/chart.png"));
        // Internal names and dirs never count as a hand-over.
        assert!(!is_artifact_path("/h/agents/a/SOUL.md"));
        assert!(!is_artifact_path("/h/agents/a/CLAUDE.md"));
        assert!(!is_artifact_path("/h/agents/a/state/working_state.json"));
        assert!(!is_artifact_path("/h/agents/a/memory/notes.md"));
        assert!(!is_artifact_path("/h/agents/a/.claude/settings.md"));
        // Non-deliverable extensions.
        assert!(!is_artifact_path("/h/agents/a/data.json"));
        assert!(!is_artifact_path("/h/agents/a/script.py"));
        assert!(!is_artifact_path("/h/agents/a/noext"));
    }

    #[test]
    fn record_and_index_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let base = home.join("agents").join("sales");
        std::fs::create_dir_all(base.join("attachments")).unwrap();
        let saved = base.join("attachments").join("1755000000000_報告.docx");
        std::fs::write(&saved, b"docx").unwrap();

        let src = base.join("報告.docx");
        record_saved(&base, &saved, "報告.docx", 4, &ctx_declared(&src));

        let idx = provenance_index(home, Some("sales"));
        let p = idx.get("1755000000000_報告.docx").expect("row present");
        assert_eq!(p.origin, ArtifactOrigin::Declared);
        assert_eq!(p.display_name, "報告.docx");
        assert_eq!(p.channel.as_deref(), Some("telegram"));
        assert!(p.task_id.is_none());
        // A different bucket must not see it.
        assert!(provenance_index(home, None).is_empty());
        assert!(provenance_index(home, Some("other")).is_empty());
    }

    #[test]
    fn uploaded_rows_are_labelled_inbound_and_never_inferred_into_a_task() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let base = home.join("agents").join("sales");
        std::fs::create_dir_all(base.join("attachments")).unwrap();
        let saved = base.join("attachments").join("1755000000000_in.pdf");
        record_saved(
            &base,
            &saved,
            "in.pdf",
            10,
            &SaveContext::uploaded(Some("line")),
        );

        let idx = provenance_index(home, Some("sales"));
        assert_eq!(idx["1755000000000_in.pdf"].origin, ArtifactOrigin::Uploaded);

        // Inside the task window, but an upload is the human's input — it is
        // not a deliverable and must never appear in the 產物 tab.
        let ev = collect_task_artifacts(
            home,
            "task-1",
            "sales",
            "2020-01-01T00:00:00+00:00",
            "2090-01-01T00:00:00+00:00",
            50,
        );
        assert!(ev.artifacts.is_empty());
    }

    #[test]
    fn exact_task_rows_beat_the_window_and_are_labelled_exact() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let rows = [
            serde_json::json!({
                "produced_at": "2026-08-15T10:01:00+00:00",
                "agent_id": "sales",
                "archived_name": "1_a.docx",
                "display_name": "a.docx",
                "size": 12,
                "origin": "declared",
                "task_id": "task-1",
                "round": 2,
            }),
            // Same agent, in the window, no task id ⇒ inferred.
            serde_json::json!({
                "produced_at": "2026-08-15T10:02:00+00:00",
                "agent_id": "sales",
                "archived_name": "2_b.xlsx",
                "display_name": "b.xlsx",
                "size": 20,
                "origin": "swept",
            }),
            // Outside the window ⇒ dropped.
            serde_json::json!({
                "produced_at": "2026-08-15T08:00:00+00:00",
                "agent_id": "sales",
                "archived_name": "3_c.pdf",
                "display_name": "c.pdf",
                "size": 30,
                "origin": "declared",
            }),
            // Another agent ⇒ dropped.
            serde_json::json!({
                "produced_at": "2026-08-15T10:03:00+00:00",
                "agent_id": "other",
                "archived_name": "4_d.pdf",
                "display_name": "d.pdf",
                "size": 40,
                "origin": "declared",
            }),
        ];
        let body: String = rows
            .iter()
            .map(|r| format!("{r}\n"))
            .collect::<Vec<_>>()
            .concat();
        std::fs::write(home.join(ARTIFACTS_FILE), body).unwrap();

        let ev = collect_task_artifacts(
            home,
            "task-1",
            "sales",
            "2026-08-15T10:00:00+00:00",
            "2026-08-15T10:05:00+00:00",
            50,
        );
        let names: Vec<&str> = ev.artifacts.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["b.xlsx", "a.docx"], "newest first");
        assert_eq!(ev.inferred_count, 1);
        let a = ev.artifacts.iter().find(|a| a.name == "a.docx").unwrap();
        assert_eq!(a.attribution, Attribution::Exact);
        assert_eq!(a.round, Some(2));
        let b = ev.artifacts.iter().find(|a| a.name == "b.xlsx").unwrap();
        assert_eq!(b.attribution, Attribution::Inferred);
    }

    #[test]
    fn unparseable_window_drops_the_inferred_half_instead_of_widening_it() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::write(
            home.join(ARTIFACTS_FILE),
            serde_json::json!({
                "produced_at": "2026-08-15T10:02:00+00:00",
                "agent_id": "sales",
                "archived_name": "2_b.xlsx",
                "display_name": "b.xlsx",
                "size": 20,
                "origin": "declared",
            })
            .to_string(),
        )
        .unwrap();
        let ev = collect_task_artifacts(home, "task-1", "sales", "nope", "also-nope", 50);
        assert!(ev.artifacts.is_empty());
    }

    #[test]
    fn round_written_files_surface_as_produced_and_merge_with_their_archive() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        // The task's round wrote two files: one deliverable, one machinery.
        crate::task_changes::record_round_changes(
            home,
            "task-1",
            "sales",
            3,
            &[
                crate::runtime::NativeToolEvent {
                    tool_name: "Write".into(),
                    success: true,
                    result_text: None,
                    input_text: Some(
                        r#"{"file_path":"/h/agents/sales/摘要.md","content":"x"}"#.into(),
                    ),
                },
                crate::runtime::NativeToolEvent {
                    tool_name: "Write".into(),
                    success: true,
                    result_text: None,
                    input_text: Some(
                        r#"{"file_path":"/h/agents/sales/state/notes.md","content":"x"}"#.into(),
                    ),
                },
            ],
        );

        let ev = collect_task_artifacts(home, "task-1", "sales", "", "", 50);
        assert_eq!(ev.artifacts.len(), 1, "state/ machinery is not a 產物");
        assert_eq!(ev.artifacts[0].name, "摘要.md");
        assert_eq!(ev.artifacts[0].origin, ArtifactOrigin::Produced);
        assert_eq!(ev.artifacts[0].round, Some(3));
        assert!(
            ev.artifacts[0].archived_name.is_none(),
            "written but never archived ⇒ nothing to download"
        );

        // Now the same file gets delivered + archived: one merged row that CAN
        // be downloaded, still carrying the round.
        let base = home.join("agents").join("sales");
        std::fs::create_dir_all(base.join("attachments")).unwrap();
        let archived = format!("1755000000000_{}", sanitize_name("摘要.md"));
        let saved = base.join("attachments").join(&archived);
        record_saved(
            &base,
            &saved,
            "摘要.md",
            5,
            &ctx_declared(Path::new("/h/agents/sales/摘要.md")),
        );
        let ev2 = collect_task_artifacts(
            home,
            "task-1",
            "sales",
            "2020-01-01T00:00:00+00:00",
            "2090-01-01T00:00:00+00:00",
            50,
        );
        assert_eq!(ev2.artifacts.len(), 1, "same file must not double-count");
        assert_eq!(ev2.artifacts[0].archived_name.as_deref(), Some(&*archived));
        assert_eq!(ev2.artifacts[0].round, Some(3), "round survives the merge");
    }

    /// Sanitization maps every CJK char to `_`, so two different reports would
    /// share a sanitized name. Merging on it would silently drop one of them —
    /// the primary key is the real basename precisely to prevent that.
    #[test]
    fn distinct_cjk_names_never_false_merge() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        crate::task_changes::record_round_changes(
            home,
            "task-1",
            "sales",
            1,
            &[
                crate::runtime::NativeToolEvent {
                    tool_name: "Write".into(),
                    success: true,
                    result_text: None,
                    input_text: Some(r#"{"file_path":"/w/報告.md","content":"a"}"#.into()),
                },
                crate::runtime::NativeToolEvent {
                    tool_name: "Write".into(),
                    success: true,
                    result_text: None,
                    input_text: Some(r#"{"file_path":"/w/摘要.md","content":"b"}"#.into()),
                },
            ],
        );
        let ev = collect_task_artifacts(home, "task-1", "sales", "", "", 50);
        let mut names: Vec<&str> = ev.artifacts.iter().map(|a| a.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["報告.md", "摘要.md"]);
    }

    #[test]
    fn no_evidence_is_an_honest_empty_result() {
        let dir = tempfile::tempdir().unwrap();
        let ev = collect_task_artifacts(
            dir.path(),
            "task-1",
            "sales",
            "2026-01-01T00:00:00+00:00",
            "2030-01-01T00:00:00+00:00",
            50,
        );
        assert_eq!(ev, TaskArtifactEvidence::default());
        // An empty task id yields nothing rather than "every artifact".
        assert!(
            collect_task_artifacts(dir.path(), "", "sales", "", "", 50)
                .artifacts
                .is_empty()
        );
    }

    #[test]
    fn malformed_ledger_lines_are_skipped_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let mut body = serde_json::json!({
            "produced_at": "2026-08-15T10:01:00+00:00",
            "agent_id": "sales",
            "archived_name": "1_a.docx",
            "display_name": "a.docx",
            "size": 12,
            "origin": "declared",
            "task_id": "task-1",
        })
        .to_string();
        body.push_str("\nnot-json\n{}\n");
        std::fs::write(home.join(ARTIFACTS_FILE), body).unwrap();
        let ev = collect_task_artifacts(home, "task-1", "sales", "", "", 50);
        assert_eq!(ev.artifacts.len(), 1);
    }

    // ── backfill ────────────────────────────────────────────────────────

    #[test]
    fn backfill_attributes_what_it_can_and_marks_the_rest_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let base = home.join("agents").join("sales");
        let attach = base.join("attachments");
        std::fs::create_dir_all(&attach).unwrap();
        std::fs::create_dir_all(home.join("attachments")).unwrap();

        // Two pre-existing archived files; only the first is named by a task's
        // change ledger.
        std::fs::write(attach.join("1755000000000_report.docx"), b"aaa").unwrap();
        std::fs::write(attach.join("1755000000001_mystery.pdf"), b"bb").unwrap();
        // Shared bucket file — no agent, no evidence.
        std::fs::write(home.join("attachments").join("1755000000002_x.csv"), b"c").unwrap();

        crate::task_changes::record_round_changes(
            home,
            "task-9",
            "sales",
            2,
            &[crate::runtime::NativeToolEvent {
                tool_name: "Write".into(),
                success: true,
                result_text: None,
                input_text: Some(
                    r#"{"file_path":"/h/agents/sales/report.docx","content":"x"}"#.into(),
                ),
            }],
        );

        let r1 = backfill(home);
        assert_eq!(r1.scanned, 3);
        assert_eq!(r1.attributed, 1);
        assert_eq!(r1.unknown, 2);
        assert_eq!(r1.already_known, 0);

        let idx = provenance_index(home, Some("sales"));
        let attributed = &idx["1755000000000_report.docx"];
        assert_eq!(attributed.task_id.as_deref(), Some("task-9"));
        assert_eq!(attributed.round, Some(2));
        assert_eq!(attributed.origin, ArtifactOrigin::Swept);
        // Honest: no evidence ⇒ 來源不明, not a guess.
        assert_eq!(
            idx["1755000000001_mystery.pdf"].origin,
            ArtifactOrigin::Unknown
        );
        assert!(idx["1755000000001_mystery.pdf"].task_id.is_none());

        // Re-running is a no-op (idempotent by construction).
        let r2 = backfill(home);
        assert_eq!(r2.added(), 0);
        assert_eq!(r2.already_known, 3);
        assert_eq!(provenance_index(home, Some("sales")).len(), 2);
    }

    #[test]
    fn backfill_never_overwrites_a_real_row() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let base = home.join("agents").join("sales");
        let attach = base.join("attachments");
        std::fs::create_dir_all(&attach).unwrap();
        let saved = attach.join("1755000000000_live.docx");
        std::fs::write(&saved, b"x").unwrap();
        record_saved(&base, &saved, "live.docx", 1, &ctx_declared(Path::new("/s")));

        let r = backfill(home);
        assert_eq!(r.added(), 0);
        assert_eq!(
            provenance_index(home, Some("sales"))["1755000000000_live.docx"].origin,
            ArtifactOrigin::Declared,
            "a live-recorded origin must not be downgraded to unknown"
        );
    }

    #[test]
    fn backfill_of_an_empty_home_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let r = backfill(dir.path());
        assert_eq!(r, BackfillReport::default());
        assert!(!dir.path().join(ARTIFACTS_FILE).exists());
    }

    // ── WP-4B: goal-loop settle archiving ─────────────────────────────────

    fn write_native(path: &Path, content: &[u8]) -> crate::runtime::NativeToolEvent {
        std::fs::write(path, content).unwrap();
        crate::runtime::NativeToolEvent {
            tool_name: "Write".into(),
            success: true,
            result_text: None,
            input_text: Some(format!(
                r#"{{"file_path":"{}","content":"x"}}"#,
                path.display()
            )),
        }
    }

    #[tokio::test]
    async fn goal_archive_copies_cjk_named_produced_file_with_exact_attribution() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let agent_dir = home.join("agents").join("sales");
        std::fs::create_dir_all(&agent_dir).unwrap();
        let src = agent_dir.join("報告.md");

        crate::task_changes::record_round_changes(
            home,
            "task-1",
            "sales",
            2,
            &[write_native(&src, b"hello")],
        );

        let report = archive_goal_task_artifacts(home, "task-1", "sales").await;
        assert_eq!(report.archived, 1, "{report:?}");
        assert_eq!(report.skipped_missing, 0);
        assert_eq!(report.skipped_outside_root, 0);
        assert_eq!(report.skipped_oversize, 0);

        let idx = provenance_index(home, Some("sales"));
        assert_eq!(idx.len(), 1);
        let (_, prov) = idx.iter().next().unwrap();
        assert_eq!(prov.origin, ArtifactOrigin::Swept);
        assert_eq!(prov.task_id.as_deref(), Some("task-1"));
        assert_eq!(prov.round, Some(2));
        assert_eq!(prov.display_name, "報告.md");

        // The 產物 tab now offers a real download, not just a source-path
        // breadcrumb: the Produced (task_changes-derived) row and this new
        // ledger row merge into one artifact.
        let ev = collect_task_artifacts(home, "task-1", "sales", "", "", 50);
        assert_eq!(ev.artifacts.len(), 1, "{ev:?}");
        assert!(ev.artifacts[0].archived_name.is_some());
        assert_eq!(ev.artifacts[0].round, Some(2));

        // Re-running is idempotent: the same file is not re-copied.
        let report2 = archive_goal_task_artifacts(home, "task-1", "sales").await;
        assert_eq!(report2.archived, 0);
        assert_eq!(report2.already_archived, 1, "{report2:?}");
        assert_eq!(provenance_index(home, Some("sales")).len(), 1);
    }

    #[tokio::test]
    async fn goal_archive_skips_oversize_file_and_counts_it_not_silently() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let agent_dir = home.join("agents").join("sales");
        std::fs::create_dir_all(&agent_dir).unwrap();
        let src = agent_dir.join("big.pdf");
        // Sparse file over the per-file cap — no need to actually write tens
        // of MB to exercise the size-check-before-read path.
        let f = std::fs::File::create(&src).unwrap();
        f.set_len(crate::media::MAX_FILE_SIZE + 1).unwrap();
        drop(f);

        crate::task_changes::record_round_changes(
            home,
            "task-1",
            "sales",
            1,
            &[crate::runtime::NativeToolEvent {
                tool_name: "Write".into(),
                success: true,
                result_text: None,
                input_text: Some(format!(
                    r#"{{"file_path":"{}","content":"x"}}"#,
                    src.display()
                )),
            }],
        );

        let report = archive_goal_task_artifacts(home, "task-1", "sales").await;
        assert_eq!(report.archived, 0);
        assert_eq!(report.skipped_oversize, 1, "{report:?}");
        assert!(provenance_index(home, Some("sales")).is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn goal_archive_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let agent_dir = home.join("agents").join("sales");
        std::fs::create_dir_all(&agent_dir).unwrap();

        // A secret sitting OUTSIDE the agent workspace entirely.
        let secret = dir.path().join("secret.pdf");
        std::fs::write(&secret, b"top secret").unwrap();
        // The agent's own Write call names a path INSIDE its workspace that
        // is actually a symlink pointing at the secret.
        let escape = agent_dir.join("report.pdf");
        symlink(&secret, &escape).unwrap();

        crate::task_changes::record_round_changes(
            home,
            "task-1",
            "sales",
            1,
            &[crate::runtime::NativeToolEvent {
                tool_name: "Write".into(),
                success: true,
                result_text: None,
                input_text: Some(format!(
                    r#"{{"file_path":"{}","content":"x"}}"#,
                    escape.display()
                )),
            }],
        );

        let report = archive_goal_task_artifacts(home, "task-1", "sales").await;
        assert_eq!(report.archived, 0);
        assert_eq!(report.skipped_outside_root, 1, "{report:?}");
        assert!(
            provenance_index(home, Some("sales")).is_empty(),
            "a symlink escape must never be archived"
        );
    }

    #[tokio::test]
    async fn goal_archive_rejects_path_outside_agent_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let agent_dir = home.join("agents").join("sales");
        std::fs::create_dir_all(&agent_dir).unwrap();
        // A real file that just happens to sit outside this agent's tree
        // (e.g. a stray absolute path from a misbehaving tool call) — no
        // symlink involved, the plain containment check alone must reject it.
        let outside = home.join("agents").join("other").join("leak.pdf");
        std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
        std::fs::write(&outside, b"not yours").unwrap();

        crate::task_changes::record_round_changes(
            home,
            "task-1",
            "sales",
            1,
            &[crate::runtime::NativeToolEvent {
                tool_name: "Write".into(),
                success: true,
                result_text: None,
                input_text: Some(format!(
                    r#"{{"file_path":"{}","content":"x"}}"#,
                    outside.display()
                )),
            }],
        );

        let report = archive_goal_task_artifacts(home, "task-1", "sales").await;
        assert_eq!(report.archived, 0);
        assert_eq!(report.skipped_outside_root, 1, "{report:?}");
    }

    #[tokio::test]
    async fn goal_archive_of_no_evidence_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::create_dir_all(home.join("agents").join("sales")).unwrap();
        let report = archive_goal_task_artifacts(home, "task-1", "sales").await;
        assert_eq!(report, GoalArchiveReport::default());
        // Empty task id / agent id ⇒ no-op rather than "archive everything".
        assert_eq!(
            archive_goal_task_artifacts(home, "", "sales").await,
            GoalArchiveReport::default()
        );
        assert_eq!(
            archive_goal_task_artifacts(home, "task-1", "").await,
            GoalArchiveReport::default()
        );
    }
}
