//! `duduclaw memory forget-source` — operator-only "forget by source" (P2-B).
//!
//! `plan` is a dry run that records a plan (ids, digests and counts only).
//! `apply --plan <id> --confirm` re-checks it, deletes the rows, writes the
//! tombstones that keep anything from that source from being learned again,
//! and runs the follow-up steps outside `memory.db` (auto wiki pages, review
//! cards, hiding the forgotten messages, clearing the session summary).
//! `resume` re-runs follow-up steps that failed.
//!
//! Every subcommand refuses inside an AI employee's session (any gateway
//! turn or identity variable present, `crate::ai_session_guard`); there is
//! no MCP tool (design §6.2). `apply` additionally needs an Admin's approval
//! in the dashboard, bound to the plan hash (`gate`), and the Bash lane of
//! the file guard refuses the command for employees. None of these has a
//! switch; `[memory] forget_source = false` only stops new plans and applies.

use std::path::Path;

use clap::Subcommand;
use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_gateway::memory_forget_steps as steps;
use duduclaw_memory::{ForgetPlan, ForgetSelector, PlanOptions, PlanOutcome, SqliteMemoryEngine};

pub const AUDIT_PLANNED: &str = "memory_source_forget_planned";
pub const AUDIT_APPLIED: &str = "memory_source_forget_applied";
pub const AUDIT_REFUSED: &str = "memory_source_forget_refused";

/// Exit status of an apply / resume whose follow-up steps did not all finish.
pub const EXIT_DEGRADED: i32 = 3;

#[derive(Subcommand, Debug)]
pub enum ForgetSourceCommands {
    /// List the recorded sources (conversations) of a namespace
    List {
        #[arg(long)]
        agent: String,
        /// Show the messages of one session instead
        #[arg(long)]
        session: Option<String>,
    },
    /// Dry run: compute and record a plan (nothing is deleted)
    Plan {
        #[arg(long)]
        agent: String,
        #[arg(long)]
        session: String,
        /// Message sequence numbers (comma-separated, `5` or `m:5`), or a
        /// key from `list --session` (`run:…`, `turn:…`); omit to forget
        /// the whole conversation up to now
        #[arg(long, value_delimiter = ',')]
        message: Option<Vec<String>>,
        /// Print up to 60 characters of each memory (never stored)
        #[arg(long)]
        show_snippets: bool,
        #[arg(long)]
        max_rows: Option<usize>,
        #[arg(long)]
        ttl_minutes: Option<i64>,
    },
    /// Show a recorded plan
    Show {
        #[arg(long)]
        plan: String,
    },
    /// Apply a recorded plan (prints it only, unless --confirm)
    Apply {
        #[arg(long)]
        plan: String,
        #[arg(long)]
        confirm: bool,
    },
    /// Re-run the follow-up steps of an applied plan
    Resume {
        #[arg(long)]
        plan: String,
    },
}

/// What a subcommand produced: the text to print and whether the follow-up
/// steps all finished (`false` ⇒ exit [`EXIT_DEGRADED`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdOutput {
    pub text: String,
    pub complete: bool,
}

impl CmdOutput {
    fn ok(text: String) -> Self {
        Self {
            text,
            complete: true,
        }
    }
}

/// The refusal for an agent-session environment, `None` for an operator.
#[cfg(test)]
pub(crate) fn agent_session_refusal(id: &str, token: &str) -> Option<String> {
    if id.trim().is_empty() && token.trim().is_empty() {
        return None;
    }
    Some(AI_SESSION_REFUSAL.to_string())
}

const AI_SESSION_REFUSAL: &str = "這個指令不能在 AI 員工的工作階段中執行。依來源刪除記憶是無法復原的操作，\
     只能由管理者在自己的終端機執行 `duduclaw memory forget-source`。";

fn refused(
    home: &Path,
    agent: &str,
    plan_id: Option<&str>,
    reason: &str,
    msg: String,
) -> DuDuClawError {
    duduclaw_security::audit::append_audit_event(
        home,
        &duduclaw_security::audit::AuditEvent::new(
            AUDIT_REFUSED,
            agent,
            duduclaw_security::audit::Severity::Warning,
            serde_json::json!({ "plan_id": plan_id, "agent_id": agent, "reason": reason }),
        ),
    );
    DuDuClawError::Agent(msg)
}

/// `[memory] forget_source` (default `true`). An unreadable `config.toml` or a
/// non-boolean value counts as off: a destructive command fails closed.
pub(crate) fn forget_source_enabled(home: &Path) -> bool {
    let path = home.join("config.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return !path.exists();
    };
    let Ok(table) = text.parse::<toml::Table>() else {
        return false;
    };
    match table
        .get("memory")
        .and_then(|m| m.as_table())
        .and_then(|m| m.get("forget_source"))
    {
        None => true,
        Some(v) => v.as_bool().unwrap_or(false),
    }
}

/// A namespace this command may act on: an employee with a directory, or an
/// `external/<client>` / `internal/<client>` MCP namespace.
pub(crate) fn valid_namespace(home: &Path, ns: &str) -> bool {
    let client_ok = |c: &str| {
        !c.is_empty()
            && c.len() <= 128
            && c.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    };
    if let Some(c) = ns
        .strip_prefix("external/")
        .or_else(|| ns.strip_prefix("internal/"))
    {
        return client_ok(c);
    }
    duduclaw_core::is_valid_agent_id(ns) && home.join("agents").join(ns).is_dir()
}

/// A per-agent memory file that boot has not merged yet: reads would see a
/// different database than this command, so it refuses (design §6.1).
pub(crate) fn stray_db(home: &Path, ns: &str) -> bool {
    if !duduclaw_core::is_valid_agent_id(ns) {
        return false;
    }
    let dir = home.join("agents").join(ns);
    dir.join("state").join("memory.db").exists() || dir.join("memory.db").exists()
}

/// Owner of `session` in `sessions.db`, if recorded.
fn session_owner(home: &Path, session: &str) -> Result<Option<String>> {
    let db = steps::sessions_db_path(home);
    if !db.exists() {
        return Ok(None);
    }
    let conn = rusqlite::Connection::open(&db)
        .map_err(|e| DuDuClawError::Memory(format!("無法開啟 sessions.db：{e}")))?;
    use rusqlite::OptionalExtension;
    conn.query_row(
        "SELECT agent_id FROM sessions WHERE id = ?1",
        [session],
        |r| r.get(0),
    )
    .optional()
    .map_err(|e| DuDuClawError::Memory(format!("讀取 sessions.db 失敗：{e}")))
}

/// Highest `session_messages.id` of `session` (the watermark), if any.
fn session_watermark(home: &Path, session: &str) -> Result<Option<i64>> {
    let db = steps::sessions_db_path(home);
    if !db.exists() {
        return Ok(None);
    }
    let conn = rusqlite::Connection::open(&db)
        .map_err(|e| DuDuClawError::Memory(format!("無法開啟 sessions.db：{e}")))?;
    conn.query_row(
        "SELECT MAX(id) FROM session_messages WHERE session_id = ?1",
        [session],
        |r| r.get::<_, Option<i64>>(0),
    )
    .map_err(|e| DuDuClawError::Memory(format!("讀取 sessions.db 失敗：{e}")))
}

fn open_engine(home: &Path) -> Result<SqliteMemoryEngine> {
    let db = home.join("memory.db");
    if !db.is_file() {
        return Err(DuDuClawError::Memory("找不到 memory.db".to_string()));
    }
    SqliteMemoryEngine::new(&db)
        .map_err(|e| DuDuClawError::Memory(format!("無法開啟 memory.db：{e}")))
}

/// `--message` values: a number is a channel message `m:<n>`; `run:` /
/// `turn:` / `call:` / `item:` / `day:` keys are taken verbatim.
pub(crate) fn message_keys(values: &[String]) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for v in values.iter().map(|v| v.trim()).filter(|v| !v.is_empty()) {
        if let Ok(n) = v.parse::<i64>() {
            if n < 0 {
                return Err(DuDuClawError::Agent(format!("訊息序號不可為負數：{n}")));
            }
            out.push(format!("m:{n}"));
        } else if ["m:", "run:", "turn:", "call:", "item:", "day:"]
            .iter()
            .any(|p| v.starts_with(p))
        {
            out.push(v.to_string());
        } else {
            return Err(DuDuClawError::Agent(format!(
                "看不懂的訊息代號：{}（請用訊息序號，例如 812 或 m:812，或 list --session 列出的 run:… 等代號）",
                duduclaw_core::truncate_chars(v, 40)
            )));
        }
    }
    Ok(out)
}

pub async fn run(home: &Path, cmd: ForgetSourceCommands) -> Result<CmdOutput> {
    run_with_markers(home, cmd, &crate::ai_session_guard::markers()).await
}

/// [`run`] with the AI-session marker variables passed in (tests).
pub(crate) async fn run_with_markers(
    home: &Path,
    cmd: ForgetSourceCommands,
    markers: &[&str],
) -> Result<CmdOutput> {
    let agent_hint = match &cmd {
        ForgetSourceCommands::List { agent, .. } | ForgetSourceCommands::Plan { agent, .. } => {
            agent.clone()
        }
        _ => String::new(),
    };
    if !markers.is_empty() {
        return Err(refused(
            home,
            &agent_hint,
            None,
            "ai_session",
            AI_SESSION_REFUSAL.into(),
        ));
    }
    match cmd {
        ForgetSourceCommands::List { agent, session } => {
            let session = session.map(|s| import_session_arg(&s));
            list::list(home, &agent, session.as_deref())
                .await
                .map(CmdOutput::ok)
        }
        ForgetSourceCommands::Plan {
            agent,
            session,
            message,
            show_snippets,
            max_rows,
            ttl_minutes,
        } => {
            let opts = PlanOptions {
                max_rows,
                ttl_minutes,
            };
            plan(
                home,
                &agent,
                &session,
                &message.unwrap_or_default(),
                opts,
                show_snippets,
            )
            .await
            .map(CmdOutput::ok)
        }
        ForgetSourceCommands::Show { plan } => show(home, &plan).await.map(CmdOutput::ok),
        ForgetSourceCommands::Apply { plan, confirm } => {
            apply_cmd::apply(home, &plan, confirm).await
        }
        ForgetSourceCommands::Resume { plan } => apply_cmd::resume(home, &plan).await,
    }
}

/// [`run`] with the caller identity passed in (tests): an id or token marks
/// an AI session, like any other marker variable.
#[cfg(test)]
pub(crate) async fn run_with_env(
    home: &Path,
    cmd: ForgetSourceCommands,
    env_agent: &str,
    env_token: &str,
) -> Result<CmdOutput> {
    let markers: &[&str] = if agent_session_refusal(env_agent, env_token).is_some() {
        &[duduclaw_core::ENV_AGENT_ID]
    } else {
        &[]
    };
    run_with_markers(home, cmd, markers).await
}

#[allow(clippy::too_many_arguments)]
async fn plan(
    home: &Path,
    agent: &str,
    session: &str,
    messages: &[String],
    opts: PlanOptions,
    show_snippets: bool,
) -> Result<String> {
    if !forget_source_enabled(home) {
        return Err(refused(
            home,
            agent,
            None,
            "disabled",
            "config.toml 的 [memory] forget_source 已關閉（或無法讀取），不建立計畫。".into(),
        ));
    }
    if !valid_namespace(home, agent) {
        return Err(refused(
            home,
            agent,
            None,
            "unknown_namespace",
            format!("不認得的命名空間：{agent}"),
        ));
    }
    if stray_db(home, agent) {
        return Err(refused(
            home,
            agent,
            None,
            "stray_db",
            format!(
                "{agent} 還有尚未合併的個別 memory.db。請先重新啟動 gateway，讓開機合併完成後再試。"
            ),
        ));
    }
    // H-3: `--session import:<file>` names an imported file by its path.
    let session_owned = import_session_arg(session);
    let session = session_owned.as_str();
    let engine = open_engine(home)?;
    // M-1: another employee's conversation is allowed when this namespace's
    // own lineage recorded it (e.g. a sub-agent reply written into it).
    if let Some(owner) = session_owner(home, session)?
        && owner != agent
        && !lineage_has_session(&engine, agent, session).await?
    {
        return Err(refused(
            home,
            agent,
            None,
            "cross_namespace",
            format!(
                "這段對話屬於 {owner}，{agent} 的記憶也沒有記錄過它，不能在這個命名空間刪除它的來源。"
            ),
        ));
    }
    let mut keys = message_keys(messages)?;
    // H-2: a user message's same-turn reply is forgotten with it.
    let user_seqs: Vec<i64> = keys
        .iter()
        .filter_map(|k| k.strip_prefix("m:").and_then(|n| n.parse().ok()))
        .collect();
    let replies = steps::same_turn_replies(&steps::sessions_db_path(home), session, &user_seqs)
        .map_err(DuDuClawError::Memory)?;
    for (_, r) in &replies {
        let k = format!("m:{r}");
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    let selector = ForgetSelector {
        session: session.to_string(),
        upto_seq: if keys.is_empty() {
            session_watermark(home, session)?
        } else {
            None
        },
        // Explicit, so the preview and the plan freeze the same watermark.
        upto_time: Some(chrono::Utc::now()),
        messages: keys,
    };
    let external = steps::collect_external_for_plan(&engine, home, agent, &selector, opts)
        .await
        .map_err(DuDuClawError::Memory)?
        .unwrap_or_default();
    match engine
        .plan_forget_source(agent, &selector, opts, &external)
        .await?
    {
        PlanOutcome::NothingToForget {
            untracked_in_namespace,
        } => Ok(if already_forgotten(&engine, agent, &selector).await? {
            // N5: nothing new since the earlier forget; no plan, no card.
            "這個來源已經忘記過：之後沒有新的記憶、對話紀錄或頁面要處理，沒有建立計畫，也沒有送出核准請求。\n"
                .to_string()
        } else {
            format!(
                "查無這個來源：記憶、對話紀錄與自動建檔頁面裡都沒有它，沒有建立計畫。\
                 此命名空間另有 {untracked_in_namespace} 筆{}，本功能不會動它們。\n",
                render::UNTRACKED_PHRASE
            )
        }),
        PlanOutcome::TooLarge { reason } => Err(refused(
            home,
            agent,
            None,
            "too_large",
            format!(
                "計畫超過上限（{reason}），沒有建立。請縮小範圍（指定訊息序號）後再試；本功能不做部分刪除。"
            ),
        )),
        PlanOutcome::Planned(p) => {
            audit_planned(home, &p);
            let mut x = extras(&engine, home, &p).await;
            x.replies = replies;
            let approval_id = gate::file_request(home, &p, x.review_pages.len()).await?;
            let mut o = render::render_plan(&p, &x);
            if show_snippets {
                o.push_str(&snippets(&engine, &p).await);
            }
            o.push_str(&format!(
                "\n這是預演，沒有刪除任何東西。已送出核准請求（編號 {}）：\n\
                 {}，管理員核准後執行：\n  duduclaw memory forget-source apply --plan {} --confirm\n（計畫 {} 到期）\n",
                duduclaw_core::truncate_chars(&approval_id, 8),
                gate::APPROVE_IN_DASHBOARD,
                p.plan_id,
                p.document.expires_at
            ));
            Ok(o)
        }
    }
}

/// The `--session` value as used everywhere after the command line (N9):
/// trimmed once, and `import:<path>` turned into the hashed import session
/// of that file (a value that is already `import:<32 hex>` is kept).
pub(crate) fn import_session_arg(session: &str) -> String {
    let s = session.trim();
    match s.strip_prefix(duduclaw_memory::lineage::IMPORT_SESSION_PREFIX) {
        Some(rest) if !(rest.len() == 32 && rest.bytes().all(|b| b.is_ascii_hexdigit())) => {
            duduclaw_memory::lineage::import_session(
                &duduclaw_memory::lineage::canonical_import_id(rest),
            )
        }
        _ => s.to_string(),
    }
}

/// Whether every source `sel` names is already covered by a tombstone (N5):
/// each message key has one (or lies under a session watermark), or, for a
/// whole-session selector, a session tombstone reaches its watermark.
async fn already_forgotten(
    engine: &SqliteMemoryEngine,
    agent: &str,
    sel: &ForgetSelector,
) -> Result<bool> {
    let conn = engine.conn_for_maintenance().await;
    let e = |e: rusqlite::Error| DuDuClawError::Memory(format!("讀取忘記紀錄失敗：{e}"));
    let session = sel.session.trim();
    let upto_cover = |seq: Option<i64>| -> std::result::Result<bool, rusqlite::Error> {
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM forgotten_sources
              WHERE agent_id = ?1 AND source_session = ?2 AND scope = 'session_upto'
                AND (upto_seq IS NULL OR (?3 IS NOT NULL AND upto_seq >= ?3)))",
            rusqlite::params![agent, session, seq],
            |r| r.get::<_, bool>(0),
        )
    };
    if sel.messages.is_empty() {
        return upto_cover(sel.upto_seq).map_err(e);
    }
    for m in &sel.messages {
        let exact: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM forgotten_sources
                  WHERE agent_id = ?1 AND source_session = ?2 AND scope = 'message'
                    AND source_message = ?3)",
                rusqlite::params![agent, session, m],
                |r| r.get(0),
            )
            .map_err(e)?;
        let seq = m.strip_prefix("m:").and_then(|n| n.parse::<i64>().ok());
        if !exact && !(seq.is_some() && upto_cover(seq).map_err(e)?) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Whether `agent`'s lineage recorded any source in `session`.
async fn lineage_has_session(
    engine: &SqliteMemoryEngine,
    agent: &str,
    session: &str,
) -> Result<bool> {
    let conn = engine.conn_for_maintenance().await;
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM memory_origins WHERE agent_id = ?1 AND source_session = ?2)",
        [agent, session],
        |r| r.get::<_, bool>(0),
    )
    .map_err(|e| DuDuClawError::Memory(format!("讀取記憶來源失敗：{e}")))
}

/// Labels, other-namespace commands and review pages for the printout.
async fn extras(engine: &SqliteMemoryEngine, home: &Path, p: &ForgetPlan) -> render::Extras {
    render::Extras {
        labels: render::collateral_labels(engine, home, p).await,
        replies: plan_replies(home, p),
        review_pages: duduclaw_gateway::wiki_host_sources::pages_needing_review(
            home,
            &p.document.agent_id,
            &review_view(p),
        ),
        other_namespace_commands: render::other_namespace_commands(engine, p).await,
    }
}

/// The selector for the employee-written pages to review: the plan's
/// messages plus the turns they started (F5), so a page stamped only with
/// such a turn (written by a dispatched run) is listed too. A whole-session
/// plan already matches every turn key of the session.
fn review_view(p: &ForgetPlan) -> steps::SelectorView {
    let mut view = steps::SelectorView::from_plan(p);
    if !view.messages.is_empty() {
        for t in &p.document.body.linked_turns {
            if !view.messages.contains(t) {
                view.messages.push(t.clone());
            }
        }
    }
    view
}

/// The same-turn replies a stored plan includes (H-2), recomputed so `show`
/// and the `apply` preview print the same line as `plan`: pairs whose user
/// message and reply are both in the selector. Unreadable `sessions.db` ⇒
/// none (the line is informational; the selector already holds the keys).
fn plan_replies(home: &Path, p: &ForgetPlan) -> Vec<(i64, i64)> {
    let sel = &p.document.selector;
    let seqs: Vec<i64> = sel
        .messages
        .iter()
        .filter_map(|k| k.strip_prefix("m:").and_then(|n| n.parse().ok()))
        .collect();
    steps::same_turn_replies(&steps::sessions_db_path(home), &sel.session, &seqs)
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, r)| sel.messages.iter().any(|k| k == &format!("m:{r}")))
        .collect()
}

async fn snippets(engine: &SqliteMemoryEngine, p: &ForgetPlan) -> String {
    let mut o = String::from("\n內容片段（只顯示在這裡，不寫進計畫或稽核）：\n");
    for t in p
        .document
        .body
        .targets
        .iter()
        .filter(|t| t.store == "memories")
    {
        if let Ok(Some(e)) = engine.get_by_id(&p.document.agent_id, &t.id).await {
            o.push_str(&format!(
                "  {}  {}\n",
                t.id,
                duduclaw_core::truncate_chars(&e.content, 60)
            ));
        }
    }
    o
}

fn audit_planned(home: &Path, p: &ForgetPlan) {
    let b = &p.document.body;
    duduclaw_security::audit::append_audit_event(
        home,
        &duduclaw_security::audit::AuditEvent::new(
            AUDIT_PLANNED,
            &p.document.agent_id,
            duduclaw_security::audit::Severity::Info,
            serde_json::json!({
                "plan_id": p.plan_id,
                "agent_id": p.document.agent_id,
                "plan_hash": p.plan_hash,
                "tombstones": b.tombstones.iter().map(|t| &t.digest).collect::<Vec<_>>(),
                "targets": b.targets.len(),
                "collateral_sources": b.collateral.len(),
                "untracked_in_namespace": b.untracked_in_namespace,
                "wiki_pages": b.wiki_pages.len(),
                "expires_at": p.document.expires_at,
            }),
        ),
    );
}

async fn show(home: &Path, plan_id: &str) -> Result<String> {
    let engine = open_engine(home)?;
    match engine.get_forget_plan(plan_id).await? {
        Some(p) => {
            let x = extras(&engine, home, &p).await;
            Ok(render::render_plan(&p, &x))
        }
        None => Err(DuDuClawError::Agent(format!("找不到計畫 {plan_id}"))),
    }
}

mod apply_cmd;
mod gate;
mod list;
mod render;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_gate;
#[cfg(test)]
mod tests_live;
#[cfg(test)]
mod tests_review3;
