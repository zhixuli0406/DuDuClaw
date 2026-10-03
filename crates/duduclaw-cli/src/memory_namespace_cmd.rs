//! `duduclaw memory migrate-namespace` — operator-only migration of the
//! pre-v1.68.0 shared MCP memory pool (`internal/gateway-internal`).
//!
//! Until v1.68.0 every gateway-spawned employee wrote its memory-tool rows into
//! that one pool. From v1.68.0 an employee with a verified identity reads and
//! writes under its own id (see `mcp_namespace::resolve_for_caller`), so the
//! old rows are visible to nobody through the tools. Nothing moves them
//! automatically; this command lets an operator look at them, export them,
//! move them into an employee's namespace, or archive them.
//!
//! Every subcommand refuses to run inside an AI employee's session (same rule
//! as `duduclaw org sync`): the pool may hold several employees' rows, and
//! moving rows into an employee's memory changes what it is told.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use clap::{ArgGroup, Subcommand, ValueEnum};
use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_memory::{MigrationDisposition, NamespaceRow, OnRefused, SqliteMemoryEngine};

/// Tag added to rows `archive` expires.
pub const ARCHIVE_TAG: &str = "namespace-archived";
/// Audit event written by a real `assign` / `archive` run.
pub const AUDIT_NAMESPACE_MIGRATED: &str = "memory_namespace_migrated";
/// Audit rows within this many seconds of a memory row count for the
/// time-proximity attribution hint.
const TIME_WINDOW_SECS: i64 = 600;
/// Memory tools whose `tool_calls.jsonl` rows can name a memory id.
const ATTRIBUTING_TOOLS: &[&str] = &["memory_store", "memory_alias_add", "user_profile_record"];

#[derive(Subcommand, Debug)]
pub enum MemoryCommands {
    /// Inspect or move the pre-v1.68.0 shared memory pool (internal/gateway-internal)
    MigrateNamespace {
        #[command(subcommand)]
        command: MigrateNamespaceCommands,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum RefusedMode {
    /// Hold the row for review in the target (an inbox card per row)
    Hold,
    /// Leave the row in the shared pool
    Skip,
}

#[derive(Subcommand, Debug)]
pub enum MigrateNamespaceCommands {
    /// Count the shared pool's rows and show who wrote them (from tool_calls.jsonl)
    List,
    /// Write the shared pool's rows (all fields) to a JSON file for review
    Export {
        #[arg(long)]
        out: PathBuf,
    },
    /// Move shared-pool rows into an employee's own memory
    #[command(group(ArgGroup::new("selector").required(true).args(["all", "ids", "attributed"])))]
    Assign {
        /// Target employee (directory name under agents/)
        #[arg(long)]
        to: String,
        /// Every row in the pool (also moves the pool's entity aliases)
        #[arg(long)]
        all: bool,
        /// Only these row ids (comma-separated)
        #[arg(long, value_delimiter = ',')]
        ids: Option<Vec<String>>,
        /// Rows tool_calls.jsonl attributes to the target employee
        #[arg(long)]
        attributed: bool,
        /// With --attributed, also take rows attributed only by time proximity
        #[arg(long)]
        include_inferred: bool,
        /// A row a more trusted fact in the target refuses: hold for review or skip
        #[arg(long, value_enum, default_value_t = RefusedMode::Hold)]
        refused: RefusedMode,
        /// Also move rows the prompt-injection scan flags (default: skip them)
        #[arg(long)]
        include_flagged: bool,
        /// Print the plan only
        #[arg(long)]
        dry_run: bool,
        /// Required for a real run
        #[arg(long)]
        confirm: bool,
    },
    /// Expire the rows left in the shared pool (kept for history, tagged)
    Archive {
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        confirm: bool,
    },
}

/// Refuse inside an AI employee session (`DUDUCLAW_AGENT_ID` or
/// `DUDUCLAW_AGENT_TOKEN` present), like `org sync`.
fn refuse_in_agent_session() -> Result<()> {
    let id = std::env::var(duduclaw_core::ENV_AGENT_ID).unwrap_or_default();
    let token = std::env::var(duduclaw_core::ENV_AGENT_TOKEN).unwrap_or_default();
    match agent_session_refusal(&id, &token) {
        Some(msg) => Err(DuDuClawError::Agent(msg)),
        None => Ok(()),
    }
}

/// The refusal for an agent-session environment, `None` for an operator
/// terminal. Pure so it can be tested without touching the process env.
fn agent_session_refusal(id: &str, token: &str) -> Option<String> {
    if id.trim().is_empty() && token.trim().is_empty() {
        return None;
    }
    let shown = if id.trim().is_empty() {
        "（未具名）".to_string()
    } else {
        duduclaw_core::truncate_chars(id.trim(), 64)
    };
    Some(format!(
        "這個指令不能在 AI 員工的工作階段中執行（偵測到身分：{shown}）。\n\
         共用記憶池可能有好幾位員工的資料，搬移會改變員工被告知的內容，\
         只能由管理者在自己的終端機執行 `duduclaw memory migrate-namespace`。"
    ))
}

pub async fn run(home: &Path, cmd: MemoryCommands) -> Result<()> {
    refuse_in_agent_session()?;
    let MemoryCommands::MigrateNamespace { command } = cmd;
    let out = match command {
        MigrateNamespaceCommands::List => list(home).await?,
        MigrateNamespaceCommands::Export { out } => export(home, &out).await?,
        MigrateNamespaceCommands::Assign {
            to,
            all,
            ids,
            attributed,
            include_inferred,
            refused,
            include_flagged,
            dry_run,
            confirm,
        } => {
            let selector = selector_from(all, ids, attributed, include_inferred)?;
            assign_with(home, &to, selector, refused, dry_run || !confirm, include_flagged).await?
        }
        MigrateNamespaceCommands::Archive { dry_run, confirm } => {
            archive(home, dry_run || !confirm).await?
        }
    };
    print!("{out}");
    Ok(())
}

/// The row selection of `assign`. clap's group enforces exactly one of
/// `--all` / `--ids` / `--attributed`; `--include-inferred` is checked here
/// because clap counts a defaulted boolean flag as present for `requires`.
fn selector_from(
    all: bool,
    ids: Option<Vec<String>>,
    attributed: bool,
    include_inferred: bool,
) -> Result<Selector> {
    if include_inferred && !attributed {
        return Err(DuDuClawError::Agent(
            "--include-inferred 只能搭配 --attributed 使用。".to_string(),
        ));
    }
    Ok(if all {
        Selector::All
    } else if attributed {
        Selector::Attributed { include_inferred }
    } else {
        Selector::Ids(ids.unwrap_or_default())
    })
}

fn pool() -> String {
    crate::mcp_namespace::shared_internal_pool()
}

fn open_engine(home: &Path) -> Result<Option<SqliteMemoryEngine>> {
    let db = home.join("memory.db");
    if !db.is_file() {
        return Ok(None);
    }
    let engine = SqliteMemoryEngine::new(&db)
        .map_err(|e| DuDuClawError::Memory(format!("無法開啟 memory.db：{e}")))?
        .with_supersession_trust_guard(
            crate::mcp::supersession_trust_guard_enabled_from_config(home),
        );
    Ok(Some(engine))
}

// ── Attribution ──────────────────────────────────────────────────────────────

/// How `tool_calls.jsonl` ties a pool row to an employee.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attribution {
    /// A memory-tool call's result names this memory id.
    MemoryId(String),
    /// A `memory_store` call's input content equals the row's content.
    Content(String),
    /// Only one employee has audit rows within ±10 minutes of the row.
    Inferred(String),
    /// No audit evidence, or several employees in the window.
    Unattributed { candidates: Vec<String> },
}

impl Attribution {
    fn label(&self) -> String {
        match self {
            Self::MemoryId(a) => format!("{a}（記憶 id 相符）"),
            Self::Content(a) => format!("{a}（內容相符）"),
            Self::Inferred(a) => format!("{a}（時間推定）"),
            Self::Unattributed { candidates } if candidates.is_empty() => "無法歸屬（沒有紀錄）".into(),
            Self::Unattributed { candidates } => {
                format!("無法歸屬（同時段有 {}）", candidates.join("、"))
            }
        }
    }
}

struct AuditRow {
    at: Option<chrono::DateTime<chrono::Utc>>,
    agent: String,
    tool: String,
    result_tokens: HashSet<String>,
    input_content: Option<String>,
}

/// Read `tool_calls.jsonl` and its rotated `.jsonl.old`. Unparseable lines
/// are skipped.
fn read_audit_rows(home: &Path) -> Vec<AuditRow> {
    let mut out = Vec::new();
    for name in ["tool_calls.jsonl.old", "tool_calls.jsonl"] {
        let Ok(text) = std::fs::read_to_string(home.join(name)) else { continue };
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
            let agent = v["agent_id"].as_str().unwrap_or("").trim().to_string();
            if agent.is_empty() {
                continue;
            }
            let tool = v["tool_name"].as_str().unwrap_or("").to_string();
            let at = v["timestamp"]
                .as_str()
                .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                .map(|t| t.with_timezone(&chrono::Utc));
            let result_tokens = v["result_text"]
                .as_str()
                .map(id_tokens)
                .unwrap_or_default();
            let input_content = if v["input_truncated"].as_bool() == Some(true) {
                None
            } else {
                v["input"]
                    .as_str()
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                    .and_then(|i| i["content"].as_str().map(str::to_string))
            };
            out.push(AuditRow { at, agent, tool, result_tokens, input_content });
        }
    }
    out
}

/// Maximal runs of UUID characters — compared with exact equality, so an id
/// only matches a whole token, never a substring.
fn id_tokens(text: &str) -> HashSet<String> {
    text.split(|c: char| !(c.is_ascii_hexdigit() || c == '-'))
        .filter(|t| t.len() >= 32)
        .map(str::to_ascii_lowercase)
        .collect()
}

fn attribute(row: &NamespaceRow, audit: &[AuditRow]) -> Attribution {
    let id = row.id.to_ascii_lowercase();
    let memory_tool = |r: &&AuditRow| ATTRIBUTING_TOOLS.contains(&r.tool.as_str());
    let by_id: HashSet<&str> = audit
        .iter()
        .filter(memory_tool)
        .filter(|r| r.result_tokens.contains(&id))
        .map(|r| r.agent.as_str())
        .collect();
    if by_id.len() == 1 {
        return Attribution::MemoryId(by_id.into_iter().next().unwrap_or_default().to_string());
    }
    let by_content: HashSet<&str> = audit
        .iter()
        .filter(|r| r.tool == "memory_store")
        .filter(|r| r.input_content.as_deref() == Some(row.content.as_str()))
        .map(|r| r.agent.as_str())
        .collect();
    if by_id.is_empty() && by_content.len() == 1 {
        return Attribution::Content(by_content.into_iter().next().unwrap_or_default().to_string());
    }
    if by_id.len() > 1 || by_content.len() > 1 {
        let mut c: Vec<String> = by_id.iter().chain(by_content.iter()).map(|s| s.to_string()).collect();
        c.sort();
        c.dedup();
        return Attribution::Unattributed { candidates: c };
    }
    let Some(t) = chrono::DateTime::parse_from_rfc3339(&row.timestamp)
        .ok()
        .map(|t| t.with_timezone(&chrono::Utc))
    else {
        return Attribution::Unattributed { candidates: vec![] };
    };
    let mut near: Vec<String> = audit
        .iter()
        .filter(|r| r.at.is_some_and(|a| (a - t).num_seconds().abs() <= TIME_WINDOW_SECS))
        .map(|r| r.agent.clone())
        .collect();
    near.sort();
    near.dedup();
    if near.len() == 1 {
        Attribution::Inferred(near.remove(0))
    } else {
        Attribution::Unattributed { candidates: near }
    }
}

// ── list ─────────────────────────────────────────────────────────────────────

fn snippet(s: &str) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let cut = duduclaw_core::truncate_chars(&flat, 60);
    if cut.len() < flat.len() { format!("{cut}…") } else { cut.to_string() }
}

pub async fn list(home: &Path) -> Result<String> {
    let ns = pool();
    let Some(engine) = open_engine(home)? else {
        return Ok("找不到 memory.db，沒有共用記憶池資料。\n".into());
    };
    let rows = engine.list_namespace_rows(&ns).await?;
    let aliases = engine.namespace_alias_count(&ns).await?;
    let mut o = String::new();
    if rows.is_empty() {
        o.push_str(&format!("共用記憶池 {ns} 沒有資料（實體別名 {aliases} 組）。\n"));
        return Ok(o);
    }
    let valid = rows.iter().filter(|r| r.valid_until.is_none()).count();
    let mut by_layer: BTreeMap<&str, usize> = BTreeMap::new();
    let mut by_origin: BTreeMap<String, usize> = BTreeMap::new();
    for r in &rows {
        *by_layer.entry(r.layer.as_str()).or_default() += 1;
        *by_origin.entry(r.origin.clone().unwrap_or_else(|| "（未記錄）".into())).or_default() += 1;
    }
    let fmt_counts = |m: Vec<(String, usize)>| {
        m.into_iter().map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join("、")
    };
    o.push_str(&format!(
        "共用記憶池 {ns}：{} 筆（目前有效 {valid}、已失效 {}）\n",
        rows.len(),
        rows.len() - valid
    ));
    o.push_str(&format!(
        "  依層級：{}\n",
        fmt_counts(by_layer.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    ));
    o.push_str(&format!("  依來源：{}\n", fmt_counts(by_origin.into_iter().collect())));
    let mut stamps: Vec<&str> = rows.iter().map(|r| r.timestamp.as_str()).collect();
    stamps.sort();
    o.push_str(&format!(
        "  最舊 {}，最新 {}\n",
        stamps.first().copied().unwrap_or(""),
        stamps.last().copied().unwrap_or("")
    ));
    o.push_str(&format!("  實體別名：{aliases} 組（只有 assign --all 會一起搬）\n"));

    let audit = read_audit_rows(home);
    let attributions: Vec<Attribution> = rows.iter().map(|r| attribute(r, &audit)).collect();
    let mut per_agent: BTreeMap<String, [usize; 3]> = BTreeMap::new();
    let mut unattributed = 0usize;
    for a in &attributions {
        match a {
            Attribution::MemoryId(x) => per_agent.entry(x.clone()).or_default()[0] += 1,
            Attribution::Content(x) => per_agent.entry(x.clone()).or_default()[1] += 1,
            Attribution::Inferred(x) => per_agent.entry(x.clone()).or_default()[2] += 1,
            Attribution::Unattributed { .. } => unattributed += 1,
        }
    }
    o.push_str(&format!("歸屬（依 tool_calls.jsonl，{} 列稽核紀錄）：\n", audit.len()));
    for (agent, [by_id, by_content, inferred]) in &per_agent {
        o.push_str(&format!(
            "  {agent}：記憶 id 相符 {by_id}、內容相符 {by_content}、時間推定 {inferred}\n"
        ));
    }
    o.push_str(&format!("  無法歸屬：{unattributed}\n"));
    o.push_str("逐筆：\n");
    for (r, a) in rows.iter().zip(&attributions) {
        o.push_str(&format!(
            "  {}  {} {} {}{}  {}  {}\n",
            r.id,
            r.layer,
            r.origin.as_deref().unwrap_or("-"),
            r.timestamp,
            if r.valid_until.is_some() { " [已失效]" } else { "" },
            a.label(),
            snippet(&r.content),
        ));
    }
    o.push_str(
        "說明：tool_calls.jsonl 記錄 memory_store 與 memory_alias_add 的輸入與結果（結果含記憶 id）；\
         user_profile_record 不寫入 tool_calls.jsonl，這類資料只能依同時段的其他工具呼叫推定。\n",
    );
    Ok(o)
}

// ── export ───────────────────────────────────────────────────────────────────

pub async fn export(home: &Path, out: &Path) -> Result<String> {
    if out.exists() {
        return Err(DuDuClawError::Agent(format!(
            "{} 已存在，不覆寫；請換一個檔名。",
            out.display()
        )));
    }
    let ns = pool();
    let (rows, aliases) = match open_engine(home)? {
        Some(e) => (e.list_namespace_rows(&ns).await?, e.list_entity_aliases(&ns).await?),
        None => (vec![], vec![]),
    };
    let doc = serde_json::json!({
        "namespace": ns,
        "exported_at": chrono::Utc::now().to_rfc3339(),
        "rows": rows,
        "aliases": aliases
            .iter()
            .map(|(c, a)| serde_json::json!({ "canonical": c, "alias": a }))
            .collect::<Vec<_>>(),
    });
    let body = serde_json::to_string_pretty(&doc).map_err(|e| DuDuClawError::Agent(e.to_string()))?;
    write_private(out, &body)?;
    Ok(format!("已匯出 {} 筆、別名 {} 組到 {}\n", rows.len(), aliases.len(), out.display()))
}

fn write_private(path: &Path, body: &str) -> Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(path)
        .map_err(|e| DuDuClawError::Agent(format!("無法建立 {}：{e}", path.display())))?;
    f.write_all(body.as_bytes())
        .map_err(|e| DuDuClawError::Agent(format!("寫入 {} 失敗：{e}", path.display())))
}

// ── assign ───────────────────────────────────────────────────────────────────

pub enum Selector {
    All,
    Ids(Vec<String>),
    Attributed { include_inferred: bool },
}

fn disposition_label(d: &MigrationDisposition) -> String {
    match d {
        MigrationDisposition::Moved => "搬移".into(),
        MigrationDisposition::MovedCurrent => "搬移（成為目前事實）".into(),
        MigrationDisposition::MovedSuperseding { superseded } => {
            format!("搬移並取代目標的 {}", superseded.join("、"))
        }
        MigrationDisposition::MovedAsHistory => "搬移為歷史（早於目標的目前事實）".into(),
        MigrationDisposition::MovedDuplicate { of } => format!("搬移為歷史（與目標的 {of} 相同）"),
        MigrationDisposition::Held { refusal } => format!(
            "暫存待審（目標的 {} 可信度 {:.2} 高於 {:.2}）",
            refusal.existing_id, refusal.existing_trust, refusal.write_trust
        ),
        MigrationDisposition::DuplicateOfHeld { held_id } => {
            format!("搬移為歷史（相同說法已在審核中：{held_id}）")
        }
        MigrationDisposition::Refused { refusal, not_held_reason } => format!(
            "留在共用池（目標的 {} 可信度 {:.2} 較高{}）",
            refusal.existing_id,
            refusal.existing_trust,
            match not_held_reason.as_deref() {
                Some("too_long") => "；內容過長，無法送審",
                _ => "",
            }
        ),
        MigrationDisposition::SkippedQuarantined => "留在共用池（隔離中的資料）".into(),
        MigrationDisposition::AlreadyInTarget => "已在目標（先前已搬移）".into(),
        MigrationDisposition::NotFound => "找不到（不在共用池）".into(),
    }
}

pub async fn assign(
    home: &Path,
    to: &str,
    selector: Selector,
    refused: RefusedMode,
    dry_run: bool,
) -> Result<String> {
    assign_with(home, to, selector, refused, dry_run, false).await
}

/// [`assign`] with the choice of moving rows the prompt-injection scan flags.
/// Rows stored before the scanner existed may carry injection text; by
/// default they stay in the pool and are listed as 「疑似注入，略過」.
pub async fn assign_with(
    home: &Path,
    to: &str,
    selector: Selector,
    refused: RefusedMode,
    dry_run: bool,
    include_flagged: bool,
) -> Result<String> {
    if !crate::mcp_namespace::client_is_agent(home, to) {
        return Err(DuDuClawError::Agent(format!(
            "找不到 AI 員工「{}」（需要 agents/<名稱>/agent.toml，請用目錄名稱）。",
            duduclaw_core::truncate_chars(to, 64)
        )));
    }
    let ns = pool();
    let Some(engine) = open_engine(home)? else {
        return Ok("找不到 memory.db，沒有需要搬移的資料。\n".into());
    };
    let rows = engine.list_namespace_rows(&ns).await?;
    let (ids, move_aliases, selector_name): (Vec<String>, bool, &str) = match &selector {
        Selector::All => (rows.iter().map(|r| r.id.clone()).collect(), true, "all"),
        Selector::Ids(v) => (
            v.iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
            false,
            "ids",
        ),
        Selector::Attributed { include_inferred } => {
            let audit = read_audit_rows(home);
            let picked = rows
                .iter()
                .filter(|r| match attribute(r, &audit) {
                    Attribution::MemoryId(a) | Attribution::Content(a) => a == to,
                    Attribution::Inferred(a) => *include_inferred && a == to,
                    Attribution::Unattributed { .. } => false,
                })
                .map(|r| r.id.clone())
                .collect();
            (picked, false, if *include_inferred { "attributed+inferred" } else { "attributed" })
        }
    };
    // Prompt-injection scan of every selected row (same scanner and block
    // threshold as the MCP / channel front doors).
    let flagged: Vec<String> = ids
        .iter()
        .filter(|id| {
            rows.iter().find(|r| &r.id == *id).is_some_and(|r| {
                duduclaw_security::input_guard::scan_input(
                    &r.content,
                    duduclaw_security::input_guard::DEFAULT_BLOCK_THRESHOLD,
                )
                .blocked
            })
        })
        .cloned()
        .collect();
    let ids: Vec<String> = if include_flagged {
        ids
    } else {
        ids.into_iter().filter(|id| !flagged.contains(id)).collect()
    };
    let on_refused = match refused {
        RefusedMode::Hold => OnRefused::Hold {
            max_chars: duduclaw_gateway::wiki_ingest::max_review_card_chars(),
        },
        RefusedMode::Skip => OnRefused::Skip,
    };
    let (plan, aliases) = engine
        .migrate_namespace_rows(&ns, to, &ids, on_refused, move_aliases, dry_run)
        .await?;

    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut held: Vec<(String, duduclaw_memory::SupersessionRefusal)> = Vec::new();
    let mut o = String::new();
    o.push_str(&format!(
        "{}：從 {ns} 搬到 {to}，選取 {} 筆（{selector_name}）\n",
        if dry_run { "預演（未寫入）" } else { "已執行" },
        ids.len()
    ));
    for id in &flagged {
        o.push_str(&format!(
            "  {id}  {}\n",
            if include_flagged { "疑似注入，仍搬移（--include-flagged）" } else { "疑似注入，略過" }
        ));
    }
    for (id, d) in &plan {
        let key = match d {
            MigrationDisposition::Held { refusal } => {
                held.push((id.clone(), refusal.clone()));
                "held"
            }
            MigrationDisposition::Refused { .. } | MigrationDisposition::SkippedQuarantined => "left",
            MigrationDisposition::AlreadyInTarget => "already",
            MigrationDisposition::NotFound => "not_found",
            _ => "moved",
        };
        *counts.entry(key).or_default() += 1;
        o.push_str(&format!("  {id}  {}\n", disposition_label(d)));
    }
    if move_aliases {
        o.push_str(&format!("  實體別名：{aliases} 組{}\n", if dry_run { "將搬移" } else { "已搬移" }));
    }
    let count = |k: &str| counts.get(k).copied().unwrap_or(0);
    if !flagged.is_empty() && !include_flagged {
        o.push_str(&format!(
            "疑似注入 {} 筆未搬移（要一併搬移請加 --include-flagged）。\n",
            flagged.len()
        ));
    }
    o.push_str(&format!(
        "合計：搬移 {}、暫存待審 {}、留在共用池 {}、先前已搬 {}、找不到 {}\n",
        count("moved"),
        count("held"),
        count("left"),
        count("already"),
        count("not_found")
    ));
    if dry_run {
        o.push_str("這是預演。確認無誤後加上 --confirm 執行。\n");
        return Ok(o);
    }
    if !held.is_empty() {
        let filed = duduclaw_gateway::wiki_ingest::file_migration_held_claims(
            home,
            &home.join("memory.db"),
            to,
            &held,
        )
        .await;
        o.push_str(&format!("已為 {filed} 筆暫存的說法建立審核項目（儀表板收件匣）。\n"));
    }
    duduclaw_security::audit::append_audit_event(
        home,
        &duduclaw_security::audit::AuditEvent::new(
            AUDIT_NAMESPACE_MIGRATED,
            to,
            duduclaw_security::audit::Severity::Info,
            serde_json::json!({
                "action": "assign",
                "from": ns,
                "to": to,
                "selector": selector_name,
                "selected": ids.len(),
                "moved": count("moved"),
                "held": count("held"),
                "left_in_place": count("left"),
                "already_in_target": count("already"),
                "not_found": count("not_found"),
                "aliases_moved": aliases,
                "injection_flagged": flagged.len(),
                "injection_flagged_moved": include_flagged,
                "refused_mode": match refused { RefusedMode::Hold => "hold", RefusedMode::Skip => "skip" },
            }),
        ),
    );
    Ok(o)
}

// ── archive ──────────────────────────────────────────────────────────────────

pub async fn archive(home: &Path, dry_run: bool) -> Result<String> {
    let ns = pool();
    let Some(engine) = open_engine(home)? else {
        return Ok("找不到 memory.db，沒有需要封存的資料。\n".into());
    };
    let n = engine.archive_namespace(&ns, ARCHIVE_TAG, dry_run).await?;
    if dry_run {
        return Ok(format!(
            "預演（未寫入）：{ns} 有 {n} 筆目前有效的資料會設為失效並加上標籤 {ARCHIVE_TAG}。\n\
             確認無誤後加上 --confirm 執行。\n"
        ));
    }
    duduclaw_security::audit::append_audit_event(
        home,
        &duduclaw_security::audit::AuditEvent::new(
            AUDIT_NAMESPACE_MIGRATED,
            duduclaw_gateway::mcp_internal_key::INTERNAL_CLIENT_ID,
            duduclaw_security::audit::Severity::Info,
            serde_json::json!({ "action": "archive", "from": ns, "archived": n, "tag": ARCHIVE_TAG }),
        ),
    );
    Ok(format!("已封存 {ns} 的 {n} 筆資料（設為失效、標籤 {ARCHIVE_TAG}，保留供查閱）。\n"))
}

#[cfg(test)]
mod tests;
