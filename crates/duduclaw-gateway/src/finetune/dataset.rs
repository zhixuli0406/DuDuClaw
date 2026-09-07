//! Dataset curation — the `finetune.datasets.*` RPCs.
//!
//! Turns what this box already stores into training JSONL. There are three
//! real sources, all of them existing gateway stores; nothing here invents
//! rows:
//!
//! | Source | Store | Becomes |
//! |---|---|---|
//! | Agent conversations | `sessions.db` → `sessions` / `session_messages` | SFT rows (multi-turn) |
//! | Task results | `tasks.db` → `tasks.result_summary` | SFT rows (single-turn: title+description → result) |
//! | Approval decisions | `approvals.db` → `approvals` | DPO pairs (approved vs denied, same agent + action kind) |
//! | Review verdicts | `tasks.db` → `task_iterations` | DPO pairs (the strongest signal: same task, rejected round vs accepted round) |
//!
//! Two output shapes, both LLaMA-Factory-native so the remote trainer needs
//! no conversion step:
//!
//! - **ShareGPT** — `{"conversations":[{"from":"human",…},{"from":"gpt",…}],"system":"…"}`
//! - **Alpaca** — `{"instruction":…,"input":…,"output":…,"history":[[q,a],…]}`
//!
//! DPO rows follow the matching ranking shape (`chosen` / `rejected`), and a
//! `dataset_info.json` registering both files is written alongside so the
//! remote `llamafactory-cli` can reference them by name.
//!
//! ## Excluded rows are excluded, not faked
//!
//! A conversation with no complete human→assistant pair, a task with no
//! `result_summary`, an approval still `pending` — none of them produce a
//! row. The counts in [`DatasetMeta`] are what actually landed in the file,
//! so a fresh install honestly builds a dataset of zero rows rather than
//! padding it.

use std::collections::HashMap;
use std::path::Path;

use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{
    dataset_dir, datasets_root, io_err, new_id, require_data_leaves_device_ack, validate_id,
    FinetuneError, Result,
};

/// Hard cap on rows pulled from any one source, so a box with years of
/// history cannot build a dataset that exhausts memory.
const SOURCE_ROW_CAP: usize = 20_000;

/// Longest single field we will emit; longer values are dropped, not
/// truncated (a half sentence is a worse training row than no row).
const MAX_FIELD_CHARS: usize = 32_000;

// ─────────────────────────── request types ───────────────────────────

/// Which stored data feeds the build.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DatasetSources {
    /// Restrict to these agent ids. Empty = every agent.
    #[serde(default)]
    pub agent_ids: Vec<String>,
    /// ISO-8601 lower bound on the row's own timestamp. `None` = all history.
    #[serde(default)]
    pub since: Option<String>,
    /// Mine approval decisions for preference pairs.
    #[serde(default)]
    pub include_approvals: bool,
    /// Include completed task results as SFT rows.
    #[serde(default)]
    pub include_task_results: bool,
}

impl DatasetSources {
    fn matches_agent(&self, agent_id: &str) -> bool {
        self.agent_ids.is_empty() || self.agent_ids.iter().any(|a| a == agent_id)
    }

    /// Lexicographic comparison works because every timestamp in these
    /// stores is ISO-8601/RFC3339 in UTC.
    fn after_since(&self, ts: &str) -> bool {
        match &self.since {
            None => true,
            Some(s) => s.is_empty() || ts >= s.as_str(),
        }
    }
}

/// Output shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatasetFormat {
    ShareGpt,
    Alpaca,
}

impl DatasetFormat {
    pub fn from_str_loose(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().replace(['-', '_'], "").as_str() {
            "sharegpt" => Ok(Self::ShareGpt),
            "alpaca" => Ok(Self::Alpaca),
            other => Err(FinetuneError::BadRequest(format!(
                "format 需為 sharegpt 或 alpaca：{other}"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ShareGpt => "sharegpt",
            Self::Alpaca => "alpaca",
        }
    }
}

// ─────────────────────────── source records ───────────────────────────
//
// Plain structs so the row builders below are pure functions the tests can
// drive from fixtures without standing up SQLite.

/// One stored conversation, already filtered to user/assistant turns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationRecord {
    pub session_id: String,
    pub agent_id: String,
    pub last_active: String,
    /// `(role, content)` in wall-clock order.
    pub turns: Vec<(String, String)>,
    /// The session's system prompt, when one was stored.
    #[serde(default)]
    pub system: Option<String>,
}

/// One completed task with a worker result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    pub id: String,
    pub title: String,
    pub description: String,
    pub result_summary: String,
    pub assigned_to: String,
    pub completed_at: String,
}

/// One decided approval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRecord {
    pub id: String,
    pub agent_id: String,
    pub action_kind: String,
    pub summary: String,
    pub payload: String,
    /// `approved` | `denied` (the store's own vocabulary; `rejected` is
    /// accepted as a synonym for robustness against older rows).
    pub status: String,
    pub decided_at: String,
}

impl ApprovalRecord {
    fn is_approved(&self) -> bool {
        self.status.eq_ignore_ascii_case("approved")
    }
    fn is_denied(&self) -> bool {
        self.status.eq_ignore_ascii_case("denied") || self.status.eq_ignore_ascii_case("rejected")
    }
}

/// One preference pair — the same prompt with a better and a worse answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreferencePair {
    /// `task_review` | `approval` — kept in the row so a reviewer can tell
    /// where a pair came from.
    pub source: String,
    pub prompt: String,
    pub chosen: String,
    pub rejected: String,
}

// ─────────────────────────── metadata ───────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DatasetCounts {
    pub conversations: usize,
    pub task_results: usize,
    pub approvals: usize,
    pub sft_rows: usize,
    pub preference_rows: usize,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetFile {
    pub name: String,
    pub rows: usize,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetMeta {
    pub id: String,
    pub name: String,
    pub format: String,
    pub created_at: String,
    #[serde(default)]
    pub built_at: Option<String>,
    #[serde(default)]
    pub sources: DatasetSources,
    #[serde(default)]
    pub preference_pairs: bool,
    #[serde(default)]
    pub counts: DatasetCounts,
    #[serde(default)]
    pub files: Vec<DatasetFile>,
}

// ─────────────────────────── pure row builders ───────────────────────────

fn field_ok(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty() && t.chars().count() <= MAX_FIELD_CHARS
}

/// Reduce a stored turn list to a strictly alternating human→gpt sequence
/// starting with a human turn. Consecutive same-role turns are joined (a
/// worker that emitted two assistant messages in a row is one answer);
/// anything before the first user turn is dropped.
fn normalise_turns(turns: &[(String, String)]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (role, content) in turns {
        let role = match role.as_str() {
            "user" | "human" => "human",
            "assistant" | "gpt" | "model" => "gpt",
            // Tool/system turns are not supervision targets here; the
            // system prompt is carried separately.
            _ => continue,
        };
        if !field_ok(content) {
            continue;
        }
        if out.is_empty() && role != "human" {
            continue;
        }
        match out.last_mut() {
            Some((last_role, last_content)) if last_role == role => {
                last_content.push_str("\n\n");
                last_content.push_str(content.trim());
            }
            _ => out.push((role.to_string(), content.trim().to_string())),
        }
    }
    // Must end on a gpt turn — a trailing unanswered question is not a
    // training row.
    if out.last().is_some_and(|(r, _)| r == "human") {
        out.pop();
    }
    out
}

/// ShareGPT SFT row from a conversation, or `None` when it yields no
/// complete human→gpt pair.
pub fn sharegpt_row_from_conversation(c: &ConversationRecord) -> Option<Value> {
    let turns = normalise_turns(&c.turns);
    if turns.len() < 2 {
        return None;
    }
    let conversations: Vec<Value> = turns
        .iter()
        .map(|(role, content)| json!({ "from": role, "value": content }))
        .collect();
    Some(json!({
        "conversations": conversations,
        "system": c.system.as_deref().filter(|s| field_ok(s)).unwrap_or_default(),
    }))
}

/// Alpaca SFT row from a conversation: the last exchange is the supervised
/// pair, everything before it is `history`.
pub fn alpaca_row_from_conversation(c: &ConversationRecord) -> Option<Value> {
    let turns = normalise_turns(&c.turns);
    if turns.len() < 2 {
        return None;
    }
    let (last_pair, prior) = turns.split_last().map(|(_, p)| (turns.len() - 1, p))?;
    let output = &turns[last_pair].1;
    let instruction = &turns[last_pair - 1].1;
    let history: Vec<Value> = prior[..last_pair - 1]
        .chunks(2)
        .filter(|c| c.len() == 2)
        .map(|c| json!([c[0].1, c[1].1]))
        .collect();
    Some(json!({
        "instruction": instruction,
        "input": "",
        "output": output,
        "history": history,
        "system": c.system.as_deref().filter(|s| field_ok(s)).unwrap_or_default(),
    }))
}

/// ShareGPT SFT row from a completed task.
pub fn sharegpt_row_from_task(t: &TaskRecord) -> Option<Value> {
    if !field_ok(&t.title) || !field_ok(&t.result_summary) {
        return None;
    }
    let human = if field_ok(&t.description) {
        format!("{}\n\n{}", t.title.trim(), t.description.trim())
    } else {
        t.title.trim().to_string()
    };
    Some(json!({
        "conversations": [
            { "from": "human", "value": human },
            { "from": "gpt", "value": t.result_summary.trim() },
        ],
        "system": "",
    }))
}

/// Alpaca SFT row from a completed task.
pub fn alpaca_row_from_task(t: &TaskRecord) -> Option<Value> {
    if !field_ok(&t.title) || !field_ok(&t.result_summary) {
        return None;
    }
    Some(json!({
        "instruction": t.title.trim(),
        "input": if field_ok(&t.description) { t.description.trim() } else { "" },
        "output": t.result_summary.trim(),
        "history": [],
        "system": "",
    }))
}

/// DPO row in the shape matching `format`.
pub fn preference_row(p: &PreferencePair, format: DatasetFormat) -> Option<Value> {
    if !field_ok(&p.prompt) || !field_ok(&p.chosen) || !field_ok(&p.rejected) {
        return None;
    }
    // An identical pair carries no preference signal.
    if p.chosen.trim() == p.rejected.trim() {
        return None;
    }
    Some(match format {
        DatasetFormat::ShareGpt => json!({
            "conversations": [{ "from": "human", "value": p.prompt.trim() }],
            "chosen": { "from": "gpt", "value": p.chosen.trim() },
            "rejected": { "from": "gpt", "value": p.rejected.trim() },
            "duduclaw_source": p.source,
        }),
        DatasetFormat::Alpaca => json!({
            "instruction": p.prompt.trim(),
            "input": "",
            "chosen": p.chosen.trim(),
            "rejected": p.rejected.trim(),
            "duduclaw_source": p.source,
        }),
    })
}

/// Every SFT row for one build, as JSONL lines.
pub fn build_sft_lines(
    conversations: &[ConversationRecord],
    tasks: &[TaskRecord],
    format: DatasetFormat,
) -> Vec<String> {
    let mut lines = Vec::new();
    for c in conversations {
        let row = match format {
            DatasetFormat::ShareGpt => sharegpt_row_from_conversation(c),
            DatasetFormat::Alpaca => alpaca_row_from_conversation(c),
        };
        if let Some(v) = row {
            lines.push(v.to_string());
        }
    }
    for t in tasks {
        let row = match format {
            DatasetFormat::ShareGpt => sharegpt_row_from_task(t),
            DatasetFormat::Alpaca => alpaca_row_from_task(t),
        };
        if let Some(v) = row {
            lines.push(v.to_string());
        }
    }
    lines
}

/// Every preference row for one build, as JSONL lines.
pub fn build_preference_lines(pairs: &[PreferencePair], format: DatasetFormat) -> Vec<String> {
    pairs
        .iter()
        .filter_map(|p| preference_row(p, format))
        .map(|v| v.to_string())
        .collect()
}

/// Pair approved against denied decisions for the same agent + action kind.
///
/// The honest reading: these are not two answers to one question, they are
/// two actions of the same kind that a human judged differently. That is a
/// real preference signal about *what this operator lets an agent do*, which
/// is exactly what a policy model should learn — but it is weaker than a
/// review verdict pair, so the row records `source: "approval"` and the
/// docs say what it means.
pub fn approvals_to_pairs(approvals: &[ApprovalRecord]) -> Vec<PreferencePair> {
    let mut by_kind: HashMap<(&str, &str), (Vec<&ApprovalRecord>, Vec<&ApprovalRecord>)> =
        HashMap::new();
    for a in approvals {
        let slot = by_kind
            .entry((a.agent_id.as_str(), a.action_kind.as_str()))
            .or_default();
        if a.is_approved() {
            slot.0.push(a);
        } else if a.is_denied() {
            slot.1.push(a);
        }
    }
    let mut keys: Vec<_> = by_kind.keys().copied().collect();
    keys.sort_unstable();
    let mut pairs = Vec::new();
    for key in keys {
        let (approved, denied) = &by_kind[&key];
        for (ok, no) in approved.iter().zip(denied.iter()) {
            pairs.push(PreferencePair {
                source: "approval".to_string(),
                prompt: format!(
                    "身為「{}」，是否應該執行下列「{}」動作？請說明你要做什麼。",
                    key.0, key.1
                ),
                chosen: ok.summary.trim().to_string(),
                rejected: no.summary.trim().to_string(),
            });
        }
    }
    pairs
}

// ─────────────────────────── SQLite collectors ───────────────────────────
//
// A missing store is not an error: a fresh install has no `tasks.db` yet and
// must still be able to build an (empty) dataset.

fn open_ro(path: &Path) -> Result<Option<Connection>> {
    if !path.exists() {
        return Ok(None);
    }
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map(Some)
        .map_err(|e| io_err(path, e))
}

/// Read conversations out of `sessions.db`.
pub fn collect_conversations(
    sessions_db: &Path,
    sources: &DatasetSources,
) -> Result<Vec<ConversationRecord>> {
    let Some(conn) = open_ro(sessions_db)? else {
        return Ok(Vec::new());
    };
    let mut stmt = conn
        .prepare(
            "SELECT id, agent_id, last_active, COALESCE(pinned_instructions, '')
             FROM sessions
             WHERE archived_at IS NULL
             ORDER BY last_active DESC
             LIMIT ?1",
        )
        .map_err(|e| io_err(sessions_db, e))?;
    let heads: Vec<(String, String, String, String)> = stmt
        .query_map([SOURCE_ROW_CAP as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .map_err(|e| io_err(sessions_db, e))?
        .filter_map(std::result::Result::ok)
        .filter(|(_, agent, last, _)| sources.matches_agent(agent) && sources.after_since(last))
        .collect();

    let mut msgs = conn
        .prepare(
            "SELECT role, content FROM session_messages
             WHERE session_id = ?1 AND COALESCE(hidden, 0) = 0 AND undone_at IS NULL
             ORDER BY id ASC",
        )
        .map_err(|e| io_err(sessions_db, e))?;

    let mut out = Vec::with_capacity(heads.len());
    for (id, agent_id, last_active, system) in heads {
        let turns: Vec<(String, String)> = msgs
            .query_map([&id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| io_err(sessions_db, e))?
            .filter_map(std::result::Result::ok)
            .collect();
        out.push(ConversationRecord {
            session_id: id,
            agent_id,
            last_active,
            turns,
            system: if system.trim().is_empty() { None } else { Some(system) },
        });
    }
    Ok(out)
}

/// Read completed tasks that actually carry a worker result.
pub fn collect_tasks(tasks_db: &Path, sources: &DatasetSources) -> Result<Vec<TaskRecord>> {
    let Some(conn) = open_ro(tasks_db)? else {
        return Ok(Vec::new());
    };
    let mut stmt = conn
        .prepare(
            "SELECT id, title, description, COALESCE(result_summary, ''), assigned_to,
                    COALESCE(completed_at, updated_at)
             FROM tasks
             WHERE status = 'done' AND COALESCE(result_summary, '') <> ''
             ORDER BY COALESCE(completed_at, updated_at) DESC
             LIMIT ?1",
        )
        .map_err(|e| io_err(tasks_db, e))?;
    let rows = stmt
        .query_map([SOURCE_ROW_CAP as i64], |r| {
            Ok(TaskRecord {
                id: r.get(0)?,
                title: r.get(1)?,
                description: r.get(2)?,
                result_summary: r.get(3)?,
                assigned_to: r.get(4)?,
                completed_at: r.get(5)?,
            })
        })
        .map_err(|e| io_err(tasks_db, e))?
        .filter_map(std::result::Result::ok)
        .filter(|t| sources.matches_agent(&t.assigned_to) && sources.after_since(&t.completed_at))
        .collect();
    Ok(rows)
}

/// Read decided approvals.
pub fn collect_approvals(
    approvals_db: &Path,
    sources: &DatasetSources,
) -> Result<Vec<ApprovalRecord>> {
    let Some(conn) = open_ro(approvals_db)? else {
        return Ok(Vec::new());
    };
    let mut stmt = conn
        .prepare(
            "SELECT id, agent_id, action_kind, summary, payload, status,
                    COALESCE(decided_at, created_at)
             FROM approvals
             WHERE status IN ('approved', 'denied', 'rejected')
             ORDER BY COALESCE(decided_at, created_at) DESC
             LIMIT ?1",
        )
        .map_err(|e| io_err(approvals_db, e))?;
    let rows = stmt
        .query_map([SOURCE_ROW_CAP as i64], |r| {
            Ok(ApprovalRecord {
                id: r.get(0)?,
                agent_id: r.get(1)?,
                action_kind: r.get(2)?,
                summary: r.get(3)?,
                payload: r.get(4)?,
                status: r.get(5)?,
                decided_at: r.get(6)?,
            })
        })
        .map_err(|e| io_err(approvals_db, e))?
        .filter_map(std::result::Result::ok)
        .filter(|a| sources.matches_agent(&a.agent_id) && sources.after_since(&a.decided_at))
        .collect();
    Ok(rows)
}

/// Mine `task_iterations` for the strongest preference signal available:
/// one task where an early round was rejected and a later round accepted.
/// Same prompt, two real answers, a human/judge verdict between them.
pub fn collect_review_pairs(tasks_db: &Path, sources: &DatasetSources) -> Result<Vec<PreferencePair>> {
    let Some(conn) = open_ro(tasks_db)? else {
        return Ok(Vec::new());
    };
    // `worker_excerpt` only exists after the 2026-08-14 additive migration;
    // an older store simply yields no pairs rather than failing the build.
    let has_excerpt = conn
        .prepare("SELECT worker_excerpt FROM task_iterations LIMIT 1")
        .is_ok();
    if !has_excerpt {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare(
            "SELECT t.id, t.title, t.description, t.assigned_to,
                    COALESCE(t.completed_at, t.updated_at),
                    acc.worker_excerpt, rej.worker_excerpt
             FROM tasks t
             JOIN task_iterations acc
               ON acc.task_id = t.id AND acc.verdict = 'accepted'
              AND COALESCE(acc.worker_excerpt, '') <> ''
             JOIN task_iterations rej
               ON rej.task_id = t.id AND rej.verdict = 'rejected'
              AND COALESCE(rej.worker_excerpt, '') <> ''
              AND rej.round < acc.round
             ORDER BY COALESCE(t.completed_at, t.updated_at) DESC
             LIMIT ?1",
        )
        .map_err(|e| io_err(tasks_db, e))?;
    let rows = stmt
        .query_map([SOURCE_ROW_CAP as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
            ))
        })
        .map_err(|e| io_err(tasks_db, e))?
        .filter_map(std::result::Result::ok)
        .filter(|(_, _, _, agent, ts, _, _)| {
            sources.matches_agent(agent) && sources.after_since(ts)
        })
        .map(|(_, title, desc, _, _, chosen, rejected)| PreferencePair {
            source: "task_review".to_string(),
            prompt: if field_ok(&desc) {
                format!("{}\n\n{}", title.trim(), desc.trim())
            } else {
                title.trim().to_string()
            },
            chosen,
            rejected,
        })
        .collect();
    Ok(rows)
}

// ─────────────────────────── dataset CRUD ───────────────────────────

fn meta_path(home: &Path, id: &str) -> Result<std::path::PathBuf> {
    Ok(dataset_dir(home, id)?.join("meta.json"))
}

fn read_meta(home: &Path, id: &str) -> Result<DatasetMeta> {
    let path = meta_path(home, id)?;
    let raw = std::fs::read_to_string(&path)
        .map_err(|_| FinetuneError::NotFound(format!("資料集 {id}")))?;
    serde_json::from_str(&raw).map_err(|e| io_err(&path, e))
}

fn write_meta(home: &Path, meta: &DatasetMeta) -> Result<()> {
    let path = meta_path(home, &meta.id)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
    }
    let body = serde_json::to_string_pretty(meta).map_err(|e| io_err(&path, e))?;
    std::fs::write(&path, body).map_err(|e| io_err(&path, e))
}

/// `finetune.datasets.list`.
pub fn list(home: &Path) -> Result<Vec<DatasetMeta>> {
    let root = datasets_root(home);
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Ok(out);
    };
    for e in entries.flatten() {
        let Some(id) = e.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if validate_id(&id).is_err() {
            continue;
        }
        if let Ok(m) = read_meta(home, &id) {
            out.push(m);
        }
    }
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(out)
}

/// `finetune.datasets.create` — registers an empty dataset. Building it is a
/// separate, explicit step.
pub fn create(home: &Path, name: &str, format: &str) -> Result<DatasetMeta> {
    let name = name.trim();
    if name.is_empty() {
        return Err(FinetuneError::BadRequest("資料集名稱不可空白".to_string()));
    }
    let format = DatasetFormat::from_str_loose(format)?;
    let meta = DatasetMeta {
        id: new_id("ds"),
        name: name.to_string(),
        format: format.as_str().to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        built_at: None,
        sources: DatasetSources::default(),
        preference_pairs: false,
        counts: DatasetCounts::default(),
        files: Vec::new(),
    };
    write_meta(home, &meta)?;
    Ok(meta)
}

/// `finetune.datasets.delete` — removes the whole dataset directory.
pub fn delete(home: &Path, id: &str) -> Result<()> {
    let dir = dataset_dir(home, id)?;
    if !dir.exists() {
        return Err(FinetuneError::NotFound(format!("資料集 {id}")));
    }
    std::fs::remove_dir_all(&dir).map_err(|e| io_err(&dir, e))
}

/// LLaMA-Factory `dataset_info.json` registering the files we just wrote, so
/// the remote trainer can say `dataset: duduclaw_sft` with no conversion.
pub fn dataset_info_json(format: DatasetFormat, has_preference: bool) -> Value {
    let mut info = serde_json::Map::new();
    match format {
        DatasetFormat::ShareGpt => {
            let tags = json!({
                "role_tag": "from",
                "content_tag": "value",
                "user_tag": "human",
                "assistant_tag": "gpt",
                "system_tag": "system",
            });
            info.insert(
                "duduclaw_sft".into(),
                json!({
                    "file_name": "train.jsonl",
                    "formatting": "sharegpt",
                    "columns": { "messages": "conversations", "system": "system" },
                    "tags": tags,
                }),
            );
            if has_preference {
                info.insert(
                    "duduclaw_dpo".into(),
                    json!({
                        "file_name": "preference.jsonl",
                        "formatting": "sharegpt",
                        "ranking": true,
                        "columns": {
                            "messages": "conversations",
                            "chosen": "chosen",
                            "rejected": "rejected",
                        },
                        "tags": tags,
                    }),
                );
            }
        }
        DatasetFormat::Alpaca => {
            info.insert(
                "duduclaw_sft".into(),
                json!({
                    "file_name": "train.jsonl",
                    "columns": {
                        "prompt": "instruction",
                        "query": "input",
                        "response": "output",
                        "history": "history",
                        "system": "system",
                    },
                }),
            );
            if has_preference {
                info.insert(
                    "duduclaw_dpo".into(),
                    json!({
                        "file_name": "preference.jsonl",
                        "ranking": true,
                        "columns": {
                            "prompt": "instruction",
                            "query": "input",
                            "chosen": "chosen",
                            "rejected": "rejected",
                        },
                    }),
                );
            }
        }
    }
    Value::Object(info)
}

/// `finetune.datasets.build` — the whole curation pass, writing
/// `train.jsonl` (+ `preference.jsonl`, `dataset_info.json`, `meta.json`)
/// under `<home>/finetune/datasets/<id>/`.
///
/// Nothing leaves the machine here, so this is NOT behind the privacy gate:
/// building is local curation. Export and remote training are the gated
/// steps.
pub fn build(
    home: &Path,
    id: &str,
    sources: DatasetSources,
    format_raw: &str,
    preference_pairs: bool,
) -> Result<DatasetMeta> {
    let mut meta = read_meta(home, id)?;
    let format = DatasetFormat::from_str_loose(format_raw)?;
    let dir = dataset_dir(home, id)?;
    std::fs::create_dir_all(&dir).map_err(|e| io_err(&dir, e))?;

    let conversations = collect_conversations(&home.join("sessions.db"), &sources)?;
    let tasks = if sources.include_task_results {
        collect_tasks(&home.join("tasks.db"), &sources)?
    } else {
        Vec::new()
    };
    let approvals = if sources.include_approvals {
        collect_approvals(&home.join("approvals.db"), &sources)?
    } else {
        Vec::new()
    };

    let sft = build_sft_lines(&conversations, &tasks, format);
    let mut pref_lines = Vec::new();
    if preference_pairs {
        let mut pairs = collect_review_pairs(&home.join("tasks.db"), &sources)?;
        pairs.extend(approvals_to_pairs(&approvals));
        pref_lines = build_preference_lines(&pairs, format);
    }

    let mut files = Vec::new();
    let train_path = dir.join("train.jsonl");
    let train_body = join_jsonl(&sft);
    std::fs::write(&train_path, &train_body).map_err(|e| io_err(&train_path, e))?;
    files.push(DatasetFile {
        name: "train.jsonl".to_string(),
        rows: sft.len(),
        size_bytes: train_body.len() as u64,
    });

    let pref_path = dir.join("preference.jsonl");
    if preference_pairs {
        let body = join_jsonl(&pref_lines);
        std::fs::write(&pref_path, &body).map_err(|e| io_err(&pref_path, e))?;
        files.push(DatasetFile {
            name: "preference.jsonl".to_string(),
            rows: pref_lines.len(),
            size_bytes: body.len() as u64,
        });
    } else if pref_path.exists() {
        let _ = std::fs::remove_file(&pref_path);
    }

    let info_path = dir.join("dataset_info.json");
    let info_body =
        serde_json::to_string_pretty(&dataset_info_json(format, preference_pairs && !pref_lines.is_empty()))
            .map_err(|e| io_err(&info_path, e))?;
    std::fs::write(&info_path, &info_body).map_err(|e| io_err(&info_path, e))?;
    files.push(DatasetFile {
        name: "dataset_info.json".to_string(),
        rows: 0,
        size_bytes: info_body.len() as u64,
    });

    meta.format = format.as_str().to_string();
    meta.built_at = Some(chrono::Utc::now().to_rfc3339());
    meta.preference_pairs = preference_pairs;
    meta.counts = DatasetCounts {
        conversations: conversations.len(),
        task_results: tasks.len(),
        approvals: approvals.len(),
        sft_rows: sft.len(),
        preference_rows: pref_lines.len(),
        bytes: files.iter().map(|f| f.size_bytes).sum(),
    };
    meta.sources = sources;
    meta.files = files;
    write_meta(home, &meta)?;
    Ok(meta)
}

fn join_jsonl(lines: &[String]) -> String {
    if lines.is_empty() {
        String::new()
    } else {
        let mut s = lines.join("\n");
        s.push('\n');
        s
    }
}

/// `finetune.datasets.preview` — the first `n` rows of each built file,
/// parsed back to JSON so the dashboard can render them structurally.
pub fn preview(home: &Path, id: &str, n: usize) -> Result<Value> {
    let meta = read_meta(home, id)?;
    let dir = dataset_dir(home, id)?;
    let n = n.clamp(1, 100);
    let take = |name: &str| -> Vec<Value> {
        std::fs::read_to_string(dir.join(name))
            .map(|s| {
                s.lines()
                    .filter(|l| !l.trim().is_empty())
                    .take(n)
                    .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                    .collect()
            })
            .unwrap_or_default()
    };
    Ok(json!({
        "dataset": meta,
        "train": take("train.jsonl"),
        "preference": take("preference.jsonl"),
    }))
}

/// `finetune.datasets.export` — hands back the on-disk path and size of the
/// built files. **Privacy-gated**: naming the path is the first step of
/// moving customer data off this machine.
pub fn export(home: &Path, id: &str, acknowledged: bool) -> Result<Value> {
    require_data_leaves_device_ack(acknowledged, "匯出資料集")?;
    let meta = read_meta(home, id)?;
    let dir = dataset_dir(home, id)?;
    let train = dir.join("train.jsonl");
    if !train.exists() {
        return Err(FinetuneError::NotFound(format!(
            "資料集 {id} 還沒建構過，請先執行「建構」"
        )));
    }
    let files: Vec<Value> = meta
        .files
        .iter()
        .map(|f| {
            json!({
                "name": f.name,
                "path": dir.join(&f.name).display().to_string(),
                "rows": f.rows,
                "size_bytes": f.size_bytes,
            })
        })
        .collect();
    Ok(json!({
        "dataset_id": meta.id,
        "dir": dir.display().to_string(),
        "path": train.display().to_string(),
        "size_bytes": meta.counts.bytes,
        "files": files,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conv(turns: &[(&str, &str)]) -> ConversationRecord {
        ConversationRecord {
            session_id: "s1".into(),
            agent_id: "finance".into(),
            last_active: "2026-09-01T00:00:00Z".into(),
            turns: turns.iter().map(|(r, c)| (r.to_string(), c.to_string())).collect(),
            system: Some("你是財務助理".into()),
        }
    }

    fn task(id: &str, result: &str) -> TaskRecord {
        TaskRecord {
            id: id.into(),
            title: "對帳".into(),
            description: "把八月發票對到銀行入帳".into(),
            result_summary: result.into(),
            assigned_to: "finance".into(),
            completed_at: "2026-09-02T00:00:00Z".into(),
        }
    }

    #[test]
    fn sharegpt_row_alternates_and_carries_system() {
        let row = sharegpt_row_from_conversation(&conv(&[
            ("user", "八月的帳對完了嗎"),
            ("assistant", "對完了，差 3 筆"),
        ]))
        .unwrap();
        assert_eq!(row["system"], "你是財務助理");
        let c = row["conversations"].as_array().unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!(c[0]["from"], "human");
        assert_eq!(c[1]["from"], "gpt");
        assert_eq!(c[1]["value"], "對完了，差 3 筆");
    }

    #[test]
    fn normalisation_drops_leading_assistant_merges_runs_and_trailing_question() {
        // Leading assistant turn dropped; two assistant turns merged; the
        // trailing unanswered human turn dropped.
        let row = sharegpt_row_from_conversation(&conv(&[
            ("assistant", "早安"),
            ("user", "問題一"),
            ("assistant", "答案 A"),
            ("assistant", "補充 B"),
            ("user", "問題二"),
        ]))
        .unwrap();
        let c = row["conversations"].as_array().unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!(c[0]["value"], "問題一");
        assert_eq!(c[1]["value"], "答案 A\n\n補充 B");
    }

    #[test]
    fn conversation_without_a_complete_pair_yields_no_row() {
        assert!(sharegpt_row_from_conversation(&conv(&[("user", "在嗎")])).is_none());
        assert!(sharegpt_row_from_conversation(&conv(&[("assistant", "在")])).is_none());
        assert!(sharegpt_row_from_conversation(&conv(&[])).is_none());
        // Tool-only turns are not supervision targets.
        assert!(sharegpt_row_from_conversation(&conv(&[("tool", "{}"), ("system", "x")])).is_none());
    }

    #[test]
    fn alpaca_row_puts_prior_turns_in_history() {
        let row = alpaca_row_from_conversation(&conv(&[
            ("user", "問題一"),
            ("assistant", "答案一"),
            ("user", "問題二"),
            ("assistant", "答案二"),
        ]))
        .unwrap();
        assert_eq!(row["instruction"], "問題二");
        assert_eq!(row["output"], "答案二");
        let h = row["history"].as_array().unwrap();
        assert_eq!(h.len(), 1);
        assert_eq!(h[0][0], "問題一");
        assert_eq!(h[0][1], "答案一");
    }

    #[test]
    fn task_rows_use_title_plus_description_as_the_prompt() {
        let sg = sharegpt_row_from_task(&task("t1", "已對完，3 筆待查")).unwrap();
        assert_eq!(sg["conversations"][0]["value"], "對帳\n\n把八月發票對到銀行入帳");
        assert_eq!(sg["conversations"][1]["value"], "已對完，3 筆待查");
        let al = alpaca_row_from_task(&task("t1", "已對完")).unwrap();
        assert_eq!(al["instruction"], "對帳");
        assert_eq!(al["input"], "把八月發票對到銀行入帳");
        // A task with no result is not a training row.
        assert!(sharegpt_row_from_task(&task("t2", "   ")).is_none());
        assert!(alpaca_row_from_task(&task("t2", "")).is_none());
    }

    #[test]
    fn build_sft_lines_emits_one_json_object_per_line_and_skips_empties() {
        let lines = build_sft_lines(
            &[
                conv(&[("user", "問"), ("assistant", "答")]),
                conv(&[("user", "沒人回")]), // skipped
            ],
            &[task("t1", "結果"), task("t2", "")], // second skipped
            DatasetFormat::ShareGpt,
        );
        assert_eq!(lines.len(), 2);
        for l in &lines {
            assert!(!l.contains('\n'));
            let v: Value = serde_json::from_str(l).unwrap();
            assert!(v["conversations"].is_array());
        }
    }

    #[test]
    fn preference_rows_match_the_dataset_format_and_drop_degenerate_pairs() {
        let p = PreferencePair {
            source: "task_review".into(),
            prompt: "對帳".into(),
            chosen: "好答案".into(),
            rejected: "壞答案".into(),
        };
        let sg = preference_row(&p, DatasetFormat::ShareGpt).unwrap();
        assert_eq!(sg["conversations"][0]["from"], "human");
        assert_eq!(sg["chosen"]["value"], "好答案");
        assert_eq!(sg["rejected"]["value"], "壞答案");
        let al = preference_row(&p, DatasetFormat::Alpaca).unwrap();
        assert_eq!(al["instruction"], "對帳");
        assert_eq!(al["chosen"], "好答案");
        // Identical chosen/rejected carries no signal.
        let same = PreferencePair { chosen: "x".into(), rejected: "x".into(), ..p.clone() };
        assert!(preference_row(&same, DatasetFormat::ShareGpt).is_none());
        // Empty side is not a pair.
        let empty = PreferencePair { rejected: "  ".into(), ..p };
        assert!(preference_row(&empty, DatasetFormat::Alpaca).is_none());
    }

    #[test]
    fn approvals_pair_approved_against_denied_within_agent_and_kind() {
        let mk = |id: &str, agent: &str, kind: &str, status: &str, summary: &str| ApprovalRecord {
            id: id.into(),
            agent_id: agent.into(),
            action_kind: kind.into(),
            summary: summary.into(),
            payload: "{}".into(),
            status: status.into(),
            decided_at: "2026-09-01T00:00:00Z".into(),
        };
        let pairs = approvals_to_pairs(&[
            mk("1", "finance", "payment", "approved", "付給既有供應商 NT$3,000"),
            mk("2", "finance", "payment", "denied", "付給未知帳號 NT$300,000"),
            // Different kind — no cross-kind pairing.
            mk("3", "finance", "email", "approved", "回信給客戶"),
            // Different agent — no cross-agent pairing.
            mk("4", "sales", "payment", "denied", "付款"),
            // Still pending rows never reach this function, but a stray one
            // must not become a pair either.
            mk("5", "finance", "payment", "pending", "?"),
        ]);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].source, "approval");
        assert!(pairs[0].chosen.contains("既有供應商"));
        assert!(pairs[0].rejected.contains("未知帳號"));
        assert!(pairs[0].prompt.contains("finance") && pairs[0].prompt.contains("payment"));
    }

    #[test]
    fn legacy_rejected_status_is_treated_as_denied() {
        let mk = |status: &str, summary: &str| ApprovalRecord {
            id: "x".into(),
            agent_id: "a".into(),
            action_kind: "k".into(),
            summary: summary.into(),
            payload: "{}".into(),
            status: status.into(),
            decided_at: "2026-01-01T00:00:00Z".into(),
        };
        let pairs = approvals_to_pairs(&[mk("approved", "好"), mk("rejected", "壞")]);
        assert_eq!(pairs.len(), 1);
    }

    #[test]
    fn dataset_info_registers_dpo_only_when_preference_rows_exist() {
        let sg = dataset_info_json(DatasetFormat::ShareGpt, false);
        assert!(sg.get("duduclaw_sft").is_some());
        assert!(sg.get("duduclaw_dpo").is_none());
        let sg2 = dataset_info_json(DatasetFormat::ShareGpt, true);
        assert_eq!(sg2["duduclaw_dpo"]["ranking"], true);
        assert_eq!(sg2["duduclaw_dpo"]["formatting"], "sharegpt");
        assert_eq!(sg2["duduclaw_sft"]["tags"]["user_tag"], "human");
        let al = dataset_info_json(DatasetFormat::Alpaca, true);
        assert_eq!(al["duduclaw_sft"]["columns"]["prompt"], "instruction");
        assert_eq!(al["duduclaw_dpo"]["ranking"], true);
    }

    #[test]
    fn format_parsing_is_loose_but_closed() {
        assert_eq!(DatasetFormat::from_str_loose(" ShareGPT ").unwrap(), DatasetFormat::ShareGpt);
        assert_eq!(DatasetFormat::from_str_loose("share-gpt").unwrap(), DatasetFormat::ShareGpt);
        assert_eq!(DatasetFormat::from_str_loose("ALPACA").unwrap(), DatasetFormat::Alpaca);
        assert!(DatasetFormat::from_str_loose("openai").is_err());
    }

    #[test]
    fn sources_filter_by_agent_and_timestamp() {
        let s = DatasetSources {
            agent_ids: vec!["finance".into()],
            since: Some("2026-09-01T00:00:00Z".into()),
            ..Default::default()
        };
        assert!(s.matches_agent("finance"));
        assert!(!s.matches_agent("sales"));
        assert!(s.after_since("2026-09-02T00:00:00Z"));
        assert!(!s.after_since("2026-08-31T23:59:59Z"));
        let all = DatasetSources::default();
        assert!(all.matches_agent("anything") && all.after_since("1999-01-01T00:00:00Z"));
    }

    // ── SQLite collectors, against the real gateway schemas ──

    fn seed_sessions(path: &Path) {
        let c = Connection::open(path).unwrap();
        c.execute_batch(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, agent_id TEXT, summary TEXT,
                 total_tokens INTEGER, last_active TEXT, model TEXT, created_at TEXT,
                 pinned_instructions TEXT, archived_at TEXT);
             CREATE TABLE session_messages (id INTEGER PRIMARY KEY AUTOINCREMENT,
                 session_id TEXT, role TEXT, content TEXT, tokens INTEGER, timestamp TEXT,
                 hidden INTEGER DEFAULT 0, undone_at TEXT);
             INSERT INTO sessions VALUES ('s1','finance','', 0,'2026-09-02T00:00:00Z','auto',
                 '2026-09-01T00:00:00Z','你是財務助理',NULL);
             INSERT INTO sessions VALUES ('s2','sales','', 0,'2026-09-02T00:00:00Z','auto',
                 '2026-09-01T00:00:00Z','',NULL);
             INSERT INTO sessions VALUES ('s3','finance','', 0,'2026-09-02T00:00:00Z','auto',
                 '2026-09-01T00:00:00Z','','2026-09-03T00:00:00Z');
             INSERT INTO session_messages (session_id,role,content,hidden,undone_at)
                 VALUES ('s1','user','問題',0,NULL),('s1','assistant','答案',0,NULL),
                        ('s1','assistant','被隱藏',1,NULL),
                        ('s1','user','被還原掉',0,'2026-09-03T00:00:00Z'),
                        ('s2','user','別家',0,NULL),('s2','assistant','別家答',0,NULL);",
        )
        .unwrap();
    }

    #[test]
    fn collect_conversations_honours_agent_filter_archive_hidden_and_undone() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("sessions.db");
        seed_sessions(&db);
        let out = collect_conversations(
            &db,
            &DatasetSources { agent_ids: vec!["finance".into()], ..Default::default() },
        )
        .unwrap();
        // s2 is another agent, s3 is archived.
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].session_id, "s1");
        assert_eq!(out[0].system.as_deref(), Some("你是財務助理"));
        // Hidden + undone messages are excluded.
        assert_eq!(out[0].turns.len(), 2);
        let lines = build_sft_lines(&out, &[], DatasetFormat::ShareGpt);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn missing_store_builds_an_empty_dataset_instead_of_failing() {
        let dir = tempfile::tempdir().unwrap();
        let s = &DatasetSources::default();
        assert!(collect_conversations(&dir.path().join("nope.db"), s).unwrap().is_empty());
        assert!(collect_tasks(&dir.path().join("nope.db"), s).unwrap().is_empty());
        assert!(collect_approvals(&dir.path().join("nope.db"), s).unwrap().is_empty());
        assert!(collect_review_pairs(&dir.path().join("nope.db"), s).unwrap().is_empty());
    }

    fn seed_tasks(path: &Path) {
        let c = Connection::open(path).unwrap();
        c.execute_batch(
            "CREATE TABLE tasks (id TEXT PRIMARY KEY, title TEXT, description TEXT, status TEXT,
                 assigned_to TEXT, created_at TEXT, updated_at TEXT, completed_at TEXT,
                 result_summary TEXT);
             CREATE TABLE task_iterations (id INTEGER PRIMARY KEY AUTOINCREMENT, task_id TEXT,
                 round INTEGER, dispatched_at TEXT, verdict TEXT, worker_excerpt TEXT);
             INSERT INTO tasks VALUES ('t1','對帳','八月發票','done','finance',
                 '2026-09-01T00:00:00Z','2026-09-02T00:00:00Z','2026-09-02T00:00:00Z','已對完');
             INSERT INTO tasks VALUES ('t2','沒結果','x','done','finance',
                 '2026-09-01T00:00:00Z','2026-09-02T00:00:00Z','2026-09-02T00:00:00Z','');
             INSERT INTO tasks VALUES ('t3','還沒完','x','in_progress','finance',
                 '2026-09-01T00:00:00Z','2026-09-02T00:00:00Z',NULL,'半成品');
             INSERT INTO task_iterations (task_id,round,dispatched_at,verdict,worker_excerpt)
                 VALUES ('t1',1,'2026-09-01T00:00:00Z','rejected','漏了三筆'),
                        ('t1',2,'2026-09-01T01:00:00Z','accepted','全部對上');",
        )
        .unwrap();
    }

    #[test]
    fn collect_tasks_takes_only_done_rows_with_a_result() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("tasks.db");
        seed_tasks(&db);
        let out = collect_tasks(&db, &DatasetSources::default()).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "t1");
        assert_eq!(out[0].result_summary, "已對完");
    }

    #[test]
    fn review_pairs_use_the_rejected_then_accepted_rounds_of_one_task() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("tasks.db");
        seed_tasks(&db);
        let pairs = collect_review_pairs(&db, &DatasetSources::default()).unwrap();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].source, "task_review");
        assert_eq!(pairs[0].chosen, "全部對上");
        assert_eq!(pairs[0].rejected, "漏了三筆");
        assert!(pairs[0].prompt.starts_with("對帳"));
    }

    fn seed_approvals(path: &Path) {
        let c = Connection::open(path).unwrap();
        c.execute_batch(
            "CREATE TABLE approvals (id TEXT PRIMARY KEY, agent_id TEXT, action_kind TEXT,
                 summary TEXT, payload TEXT, status TEXT, created_at TEXT, decided_at TEXT);
             INSERT INTO approvals VALUES ('a1','finance','payment','付給既有供應商','{}',
                 'approved','2026-09-01T00:00:00Z','2026-09-01T01:00:00Z');
             INSERT INTO approvals VALUES ('a2','finance','payment','付給未知帳號','{}',
                 'denied','2026-09-01T00:00:00Z','2026-09-01T02:00:00Z');
             INSERT INTO approvals VALUES ('a3','finance','payment','還沒決定','{}',
                 'pending','2026-09-01T00:00:00Z',NULL);",
        )
        .unwrap();
    }

    #[test]
    fn collect_approvals_skips_pending_rows() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("approvals.db");
        seed_approvals(&db);
        let out = collect_approvals(&db, &DatasetSources::default()).unwrap();
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|a| a.status != "pending"));
    }

    // ── end-to-end build against all three stores ──

    #[test]
    fn build_writes_jsonl_plus_registry_and_counts_only_real_rows() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        seed_sessions(&home.join("sessions.db"));
        seed_tasks(&home.join("tasks.db"));
        seed_approvals(&home.join("approvals.db"));

        let meta = create(home, "值班紀錄", "sharegpt").unwrap();
        let built = build(
            home,
            &meta.id,
            DatasetSources {
                agent_ids: vec!["finance".into()],
                since: None,
                include_approvals: true,
                include_task_results: true,
            },
            "sharegpt",
            true,
        )
        .unwrap();

        assert_eq!(built.counts.conversations, 1);
        assert_eq!(built.counts.task_results, 1);
        assert_eq!(built.counts.approvals, 2);
        // 1 conversation + 1 task result.
        assert_eq!(built.counts.sft_rows, 2);
        // 1 review pair + 1 approval pair.
        assert_eq!(built.counts.preference_rows, 2);
        assert!(built.counts.bytes > 0);

        let dir = dataset_dir(home, &meta.id).unwrap();
        let train = std::fs::read_to_string(dir.join("train.jsonl")).unwrap();
        assert_eq!(train.lines().count(), 2);
        let pref = std::fs::read_to_string(dir.join("preference.jsonl")).unwrap();
        assert_eq!(pref.lines().count(), 2);
        let info: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("dataset_info.json")).unwrap())
                .unwrap();
        assert_eq!(info["duduclaw_dpo"]["file_name"], "preference.jsonl");

        // Listing sees it; preview reads it back as JSON.
        let listed = list(home).unwrap();
        assert_eq!(listed.len(), 1);
        let pv = preview(home, &meta.id, 10).unwrap();
        assert_eq!(pv["train"].as_array().unwrap().len(), 2);
        assert_eq!(pv["preference"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn build_without_preference_pairs_writes_no_preference_file() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        seed_sessions(&home.join("sessions.db"));
        let meta = create(home, "只做 SFT", "alpaca").unwrap();
        let built = build(home, &meta.id, DatasetSources::default(), "alpaca", false).unwrap();
        assert_eq!(built.counts.preference_rows, 0);
        assert!(!dataset_dir(home, &meta.id).unwrap().join("preference.jsonl").exists());
        assert!(built.files.iter().all(|f| f.name != "preference.jsonl"));
    }

    #[test]
    fn export_is_privacy_gated_and_needs_a_built_dataset() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let meta = create(home, "x", "sharegpt").unwrap();

        // Gate first: no acknowledgement, no path — even before we check
        // whether the dataset was built.
        let err = export(home, &meta.id, false).unwrap_err();
        assert_eq!(err.code(), "data_leaves_device_not_acknowledged");

        // Acknowledged but never built → an honest not-found, not a path to
        // a file that does not exist.
        let err = export(home, &meta.id, true).unwrap_err();
        assert_eq!(err.code(), "not_found");

        build(home, &meta.id, DatasetSources::default(), "sharegpt", false).unwrap();
        let out = export(home, &meta.id, true).unwrap();
        assert!(out["path"].as_str().unwrap().ends_with("train.jsonl"));
        assert!(out["files"].as_array().unwrap().iter().any(|f| f["name"] == "train.jsonl"));
    }

    #[test]
    fn create_delete_and_unknown_ids() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        assert!(create(home, "  ", "sharegpt").is_err());
        assert!(create(home, "ok", "not-a-format").is_err());
        let m = create(home, "ok", "sharegpt").unwrap();
        assert_eq!(list(home).unwrap().len(), 1);
        assert_eq!(delete(home, "no-such-id").unwrap_err().code(), "not_found");
        assert_eq!(preview(home, "no-such-id", 5).unwrap_err().code(), "not_found");
        delete(home, &m.id).unwrap();
        assert!(list(home).unwrap().is_empty());
    }
}
