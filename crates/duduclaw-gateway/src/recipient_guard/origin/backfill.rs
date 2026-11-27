//! One-time startup backfill of gateway origin proofs for the WebChat
//! schedules and reminders released in v1.81 (RM-124).
//!
//! v1.81 let an AI employee create a schedule or reminder whose results go
//! back into the signed-in WebChat conversation that created it. Those rows
//! predate [`super::TABLE`]: they carry the stamped target
//! (`notify_chat_id` / reminder `chat_id`, channel `webchat`) but no proof
//! id, so after the upgrade every delivery would be refused
//! `origin_unproven`. This pass gives each such row the proof a live turn
//! would have minted, if — and only if — the same checks pass today.
//!
//! * **Who** — only the process holding the gateway lock
//!   (`gateway_instance::held`), from `start_gateway` before the cron and
//!   reminder schedulers start. The entry point is crate-private, so the
//!   employee's MCP process (which links this crate but never holds the lock)
//!   cannot call it.
//! * **Which rows** — cron rows with `notify_channel = 'webchat'` and pending
//!   reminders with `channel = 'webchat'`, without a proof id. Channel
//!   origins are new in v1.82 (no released row exists) and are never read.
//!   A row whose `notify_origin` names a different destination, an
//!   isolation-bound reminder (its process-local authority did not survive
//!   the restart), a duplicate reminder id or the unresolved `default`
//!   employee gets nothing.
//! * **Creator** — taken only from what the row itself recorded: the host
//!   carried principal (`on_behalf_of`, which must be a `user:` principal)
//!   and, for cron rows, the dashboard creator (`created_by_user_id`). At
//!   least one must be present, and if both are they must name the same
//!   account; otherwise nothing is minted. That account must then be exactly
//!   the session row's owner ([`super::try_mint_webchat`]).
//! * **Checks** — [`super::try_mint_webchat`], the same function a live
//!   WebChat turn mints through: session row exists as the stamped
//!   generation, is `webchat_user`, owned by the creator (owner tag in the
//!   id), answered by this employee, creator Active and still allowed to
//!   call the employee, store caps. A row that fails keeps no proof and is
//!   refused at fire time — correct for a revoked creator.
//! * **Write** — only the proof id column; nothing else on the row changes
//!   (in particular `notify_origin`, part of the isolation execution
//!   material, is left alone: fire time treats a missing origin of a WebChat
//!   target as the target itself, `scheduled_webchat::stored_origin`). The
//!   write is conditional on the row still being the legacy row the proof
//!   was minted for; otherwise the fresh record is deleted again.
//! * **Once** — a pass that completes writes the completion marker
//!   [`MARKER`] into `sessions.db`'s `session_migrations` table (the
//!   database of [`super::TABLE`]) as its last write, after every proof and
//!   row write of the pass committed. Once the marker exists no later start
//!   scans or mints: a row without a proof that appears afterwards is not
//!   legacy and never gets one here.
//! * **Fail closed** — an unreadable marker or any unreadable store before
//!   minting aborts the pass with nothing minted; an unreadable or
//!   unwritable store mid-pass stops it. A stopped pass writes no marker, and
//!   the next start runs the pass again. Idempotent: a row that has a proof
//!   id is skipped.
//! * **Logging** — one line per start: minted / skipped / refused counts, the
//!   closed reason the pass stopped, or (debug) that the migration already
//!   completed. No ids or user data.

use std::collections::HashMap;
use std::path::Path;

use rusqlite::{Connection, OpenFlags, TransactionBehavior, params};

use super::{MintRefusal, OriginDenial};
use crate::reminder_scheduler::{LegacyWebchatProof, Reminder, ReminderStatus};

/// Completion marker of this migration (`session_migrations.name` in
/// `sessions.db`, next to [`super::TABLE`]).
const MARKER: &str = "schedule_origin_backfill_v1";

/// Counts of one pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Summary {
    /// Legacy WebChat rows that got a proof now.
    pub minted: usize,
    /// WebChat rows that already had a proof id (or changed under the pass).
    pub skipped: usize,
    /// Legacy WebChat rows left without a proof (checks failed, creator not
    /// recorded unambiguously, store full).
    pub refused: usize,
}

/// Why a pass stopped. Closed; only [`Self::code`] is logged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Abort {
    /// `cron_tasks.db` could not be read.
    Schedules,
    /// `reminders.jsonl` could not be read (or is malformed / oversized).
    Reminders,
    /// `sessions.db` or live account / conversation state could not be read.
    State,
    /// `users.db` could not be opened.
    Accounts,
    /// A proof record, a row or the completion marker could not be written.
    Write,
}

impl Abort {
    fn code(self) -> &'static str {
        match self {
            Self::Schedules => "schedules_unreadable",
            Self::Reminders => "reminders_unreadable",
            Self::State => "state_unreadable",
            Self::Accounts => "accounts_unreadable",
            Self::Write => "write_failed",
        }
    }
}

/// Run the backfill once at gateway startup. A no-op unless this process
/// holds the gateway lock. Must be awaited before the schedulers start.
pub(crate) async fn startup_backfill(home: &Path) {
    if !duduclaw_core::gateway_instance::held(home) {
        return;
    }
    let pass_home = home.to_path_buf();
    let authority = duduclaw_mcp_boundary::io_authority::capture();
    match tokio::task::spawn_blocking(move || authority.run(|| run_once(&pass_home))).await {
        Ok(Ok(None)) => tracing::debug!("legacy WebChat schedule origin backfill already completed"),
        Ok(Ok(Some(summary))) => tracing::info!(
            minted = summary.minted,
            skipped = summary.skipped,
            refused = summary.refused,
            "legacy WebChat schedule origin backfill finished"
        ),
        Ok(Err(abort)) => tracing::warn!(
            reason = abort.code(),
            "legacy WebChat schedule origin backfill stopped; rows without a proof are retried at the next start"
        ),
        Err(_) => tracing::warn!(
            reason = "task_failed",
            "legacy WebChat schedule origin backfill stopped; rows without a proof are retried at the next start"
        ),
    }
}

/// The employee a legacy row may be backfilled for: a concrete id (the
/// `default` alias is resolved only at fire time and is never minted for).
fn concrete_agent(agent: &str) -> bool {
    agent != "default" && duduclaw_core::is_valid_agent_id(agent)
}

/// The one creator a legacy row records, or `None` when it records none or
/// more than one. `on_behalf_of`, when present, must be a `user:` principal
/// (an unmapped sender or a rejected value is never a WebChat account).
fn recorded_creator(on_behalf_of: Option<&str>, created_by: Option<&str>) -> Option<String> {
    let principal = match on_behalf_of {
        None => None,
        Some(raw) => Some(duduclaw_core::HumanPrincipal::parse(raw.trim()).ok()?.user_id()?.to_owned()),
    };
    let created_by = created_by.filter(|user| !user.is_empty()).map(str::to_owned);
    match (principal, created_by) {
        (Some(principal), Some(created_by)) => (principal == created_by).then_some(principal),
        (Some(creator), None) | (None, Some(creator)) => Some(creator),
        (None, None) => None,
    }
}

/// `true` when a stored `notify_origin` is absent or is the target itself.
fn origin_is_target(origin: Option<&str>, target: &str) -> bool {
    origin.is_none_or(|origin| origin.is_empty() || origin == target)
}

fn has_proof(proof: Option<&str>) -> bool {
    proof.is_some_and(|proof| !proof.is_empty())
}

/// A WebChat cron row as stored (only the columns the pass needs).
struct CronRow {
    id: String,
    agent: String,
    target: Option<String>,
    origin: Option<String>,
    proof: Option<String>,
    on_behalf_of: Option<String>,
    created_by: Option<String>,
}

/// A legacy row the pass will try to mint for.
struct Job {
    id: String,
    agent: String,
    target: String,
    creator: String,
    on_behalf_of: Option<String>,
    created_by: Option<String>,
}

enum Class {
    Skip,
    Refuse,
    Mint(Job),
}

fn classify_cron(row: CronRow) -> Class {
    if has_proof(row.proof.as_deref()) {
        return Class::Skip;
    }
    let Some(target) = row.target.filter(|target| !target.is_empty()) else { return Class::Refuse };
    if !origin_is_target(row.origin.as_deref(), &target) || !concrete_agent(&row.agent) {
        return Class::Refuse;
    }
    let Some(creator) = recorded_creator(row.on_behalf_of.as_deref(), row.created_by.as_deref()) else {
        return Class::Refuse;
    };
    Class::Mint(Job {
        id: row.id, agent: row.agent, target, creator,
        on_behalf_of: row.on_behalf_of, created_by: row.created_by,
    })
}

/// `None` for a reminder the pass never looks at (not pending, not WebChat).
fn classify_reminder(reminder: &Reminder, duplicate: bool) -> Option<Class> {
    if reminder.status != ReminderStatus::Pending || reminder.channel != crate::scheduled_webchat::CHANNEL {
        return None;
    }
    if has_proof(reminder.notify_origin_proof.as_deref()) {
        return Some(Class::Skip);
    }
    if duplicate
        || reminder.isolation_bound
        || reminder.chat_id.is_empty()
        || !origin_is_target(reminder.notify_origin.as_deref(), &reminder.chat_id)
        || !concrete_agent(&reminder.agent_id)
    {
        return Some(Class::Refuse);
    }
    let Some(creator) = recorded_creator(reminder.on_behalf_of.as_deref(), None) else {
        return Some(Class::Refuse);
    };
    Some(Class::Mint(Job {
        id: reminder.id.clone(), agent: reminder.agent_id.clone(), target: reminder.chat_id.clone(), creator,
        on_behalf_of: reminder.on_behalf_of.clone(), created_by: None,
    }))
}

/// Every WebChat cron row. A missing store has none; any other error aborts.
fn webchat_cron_rows(home: &Path) -> Result<Vec<CronRow>, String> {
    let path = home.join("cron_tasks.db");
    match std::fs::metadata(&path) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    }
    duduclaw_mcp_boundary::io_authority::verify_before_io().map_err(|e| e.to_string())?;
    let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|e| e.to_string())?;
    conn.busy_timeout(std::time::Duration::from_secs(5)).map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT id, agent_id, notify_chat_id, notify_origin, notify_origin_proof, on_behalf_of, created_by_user_id
             FROM cron_tasks WHERE notify_channel = ?1",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![crate::scheduled_webchat::CHANNEL], |r| Ok(CronRow {
            id: r.get(0)?, agent: r.get(1)?, target: r.get(2)?, origin: r.get(3)?, proof: r.get(4)?,
            on_behalf_of: r.get(5)?, created_by: r.get(6)?,
        }))
        .map_err(|e| e.to_string())?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(|e| e.to_string())
}

/// Write `proof` onto the cron row, only while it is still the legacy row it
/// was minted for. `Ok(false)` when the row changed or is gone.
fn attach_cron(home: &Path, job: &Job, proof: &str) -> Result<bool, String> {
    duduclaw_mcp_boundary::io_authority::verify_before_io().map_err(|e| e.to_string())?;
    let mut conn = Connection::open_with_flags(
        home.join("cron_tasks.db"),
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| e.to_string())?;
    conn.busy_timeout(std::time::Duration::from_secs(5)).map_err(|e| e.to_string())?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
    let changed = tx
        .execute(
            "UPDATE cron_tasks SET notify_origin_proof = ?2
             WHERE id = ?1 AND agent_id = ?3 AND notify_channel = ?4 AND notify_chat_id = ?5
               AND (notify_origin IS NULL OR notify_origin = '' OR notify_origin = ?5)
               AND (notify_origin_proof IS NULL OR notify_origin_proof = '')
               AND on_behalf_of IS ?6 AND created_by_user_id IS ?7",
            params![job.id, proof, job.agent, crate::scheduled_webchat::CHANNEL, job.target,
                job.on_behalf_of, job.created_by],
        )
        .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(changed == 1)
}

/// Outcome of one mint attempt.
enum Minted {
    Proof(String),
    Refused,
}

/// [`super::try_mint_webchat`] for a legacy row, with the backfill's
/// fail-closed mapping: unreadable state or an unwritable store stops the
/// pass; every other refusal leaves the row without a proof.
fn mint(home: &Path, job: &Job) -> Result<Minted, Abort> {
    match super::try_mint_webchat(home, &job.agent, &job.target, Some(&job.creator)) {
        Ok(id) => Ok(Minted::Proof(id)),
        Err(MintRefusal::Denied(OriginDenial::Unreadable)) => Err(Abort::State),
        Err(MintRefusal::Unwritable) => Err(Abort::Write),
        Err(MintRefusal::Denied(_) | MintRefusal::Full) => Ok(Minted::Refused),
    }
}

/// `true` when [`MARKER`] is recorded. Unreadable ⇒ the pass stops.
fn marker_written(home: &Path) -> Result<bool, Abort> {
    duduclaw_mcp_boundary::io_authority::verify_before_io().map_err(|_| Abort::State)?;
    let conn = super::open_read(home).map_err(|_| Abort::State)?;
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM session_migrations WHERE name = ?1)",
        params![MARKER],
        |r| r.get(0),
    )
    .map_err(|_| Abort::State)
}

/// Record [`MARKER`] (one IMMEDIATE transaction in `sessions.db`).
fn write_marker(home: &Path) -> Result<(), String> {
    duduclaw_mcp_boundary::io_authority::verify_before_io().map_err(|e| e.to_string())?;
    let mut conn = super::open_write(home)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT OR IGNORE INTO session_migrations (name, applied_at) VALUES (?1, ?2)",
        params![MARKER, chrono::Utc::now().to_rfc3339()],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

/// The migration: `Ok(None)` when [`MARKER`] is already recorded (nothing is
/// read or minted), otherwise one full [`run`] followed by the marker. The
/// marker is the last write: a pass that stopped, or a marker that could not
/// be written, leaves no marker and the next start runs the pass again.
fn run_once(home: &Path) -> Result<Option<Summary>, Abort> {
    // This migration covers schedules created by the released version, where
    // WebChat schedules wrote back to their conversation with no proof at all,
    // so it grants nothing that version did not already allow. It must not run
    // again: a row without a proof created after it completed is not legacy.
    if marker_written(home)? {
        return Ok(None);
    }
    let summary = run(home)?;
    write_marker(home).map_err(|_| Abort::Write)?;
    Ok(Some(summary))
}

/// One full pass (no lock or marker check; see [`run_once`]).
fn run(home: &Path) -> Result<Summary, Abort> {
    // Read everything first: an unreadable store mints nothing at all.
    let crons = webchat_cron_rows(home).map_err(|_| Abort::Schedules)?;
    let reminders = crate::reminder_scheduler::load_reminders_strict_sync(home).map_err(|_| Abort::Reminders)?;
    let mut summary = Summary::default();
    let mut tally = |class: Class, jobs: &mut Vec<Job>| match class {
        Class::Skip => summary.skipped += 1,
        Class::Refuse => summary.refused += 1,
        Class::Mint(job) => jobs.push(job),
    };
    let mut cron_jobs = Vec::new();
    for row in crons {
        tally(classify_cron(row), &mut cron_jobs);
    }
    let mut ids: HashMap<&str, usize> = HashMap::new();
    for reminder in &reminders {
        *ids.entry(reminder.id.as_str()).or_default() += 1;
    }
    let mut reminder_jobs = Vec::new();
    for reminder in &reminders {
        let duplicate = ids.get(reminder.id.as_str()).is_some_and(|count| *count > 1);
        if let Some(class) = classify_reminder(reminder, duplicate) {
            tally(class, &mut reminder_jobs);
        }
    }
    if cron_jobs.is_empty() && reminder_jobs.is_empty() {
        return Ok(summary);
    }
    // Live state must be readable before anything is minted.
    super::open_read(home).map_err(|_| Abort::State)?;
    match crate::decision_notify::open_user_db(home) {
        Ok(Some(_)) => {}
        _ => return Err(Abort::Accounts),
    }

    for job in &cron_jobs {
        let Minted::Proof(id) = mint(home, job)? else {
            summary.refused += 1;
            continue;
        };
        match attach_cron(home, job, &id) {
            Ok(true) => summary.minted += 1,
            Ok(false) => {
                super::forget(home, std::slice::from_ref(&id));
                summary.skipped += 1;
            }
            Err(_) => {
                super::forget(home, std::slice::from_ref(&id));
                return Err(Abort::Write);
            }
        }
    }

    let mut minted: Vec<LegacyWebchatProof> = Vec::new();
    let forget_all = |minted: &[LegacyWebchatProof]| {
        let ids: Vec<String> = minted.iter().map(|proof| proof.proof.clone()).collect();
        super::forget(home, &ids);
    };
    for job in reminder_jobs {
        match mint(home, &job) {
            Ok(Minted::Proof(proof)) => minted.push(LegacyWebchatProof {
                reminder_id: job.id, agent_id: job.agent, chat_id: job.target,
                on_behalf_of: job.on_behalf_of, proof,
            }),
            Ok(Minted::Refused) => summary.refused += 1,
            Err(abort) => {
                forget_all(&minted);
                return Err(abort);
            }
        }
    }
    let attached = match crate::reminder_scheduler::attach_legacy_webchat_proofs_sync(home, &minted) {
        Ok(attached) => attached,
        Err(_) => {
            forget_all(&minted);
            return Err(Abort::Write);
        }
    };
    let (kept, dropped): (Vec<_>, Vec<_>) =
        minted.into_iter().partition(|proof| attached.contains(&proof.reminder_id));
    summary.minted += kept.len();
    summary.skipped += dropped.len();
    forget_all(&dropped);
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cron_store::{CronStore, CronTaskRow};
    use crate::scheduled_webchat::tests::{Fixture, fixture, messages};
    use duduclaw_auth::{AccessLevel, UserDb, UserRole, UserStatus};

    fn proof_count(home: &Path) -> i64 {
        Connection::open(home.join("sessions.db")).unwrap()
            .query_row("SELECT COUNT(*) FROM schedule_origin_proofs", [], |r| r.get(0)).unwrap()
    }

    fn principal(user: &str) -> Option<String> {
        Some(format!("user:{user}"))
    }

    /// A v1.81 WebChat schedule: stamped target, no `notify_origin`, no proof.
    async fn legacy_cron(f: &Fixture, id: &str, on_behalf_of: Option<String>, created_by: Option<String>) {
        let store = CronStore::open(f.home.path()).unwrap();
        let mut row = CronTaskRow::new(id.into(), "每日摘要".into(), "sales".into(), "0 9 * * *".into(), "t".into());
        row.notify_channel = Some(crate::scheduled_webchat::CHANNEL.into());
        row.notify_chat_id = Some(f.target.clone());
        row.on_behalf_of = on_behalf_of;
        row.created_by_user_id = created_by;
        store.insert(&row).await.unwrap();
    }

    async fn cron(home: &Path, id: &str) -> CronTaskRow {
        CronStore::open(home).unwrap().get(id).await.unwrap().unwrap()
    }

    /// A v1.81 WebChat reminder: stamped target, no `notify_origin`, no proof.
    fn legacy_reminder(f: &Fixture, id: &str, on_behalf_of: Option<&str>) -> Reminder {
        serde_json::from_value(serde_json::json!({
            "id": id, "agent_id": "sales", "trigger_at": chrono::Utc::now(), "channel": "webchat",
            "chat_id": f.target, "mode": "direct", "message": "記得開會", "on_behalf_of": on_behalf_of,
        })).unwrap()
    }

    fn write_reminders(home: &Path, reminders: &[Reminder]) {
        let content: String = reminders.iter().map(|r| format!("{}\n", serde_json::to_string(r).unwrap())).collect();
        std::fs::write(home.join("reminders.jsonl"), content).unwrap();
    }

    fn reminder(home: &Path, id: &str) -> Reminder {
        crate::reminder_scheduler::load_reminders_strict_sync(home).unwrap().into_iter().find(|r| r.id == id).unwrap()
    }

    #[tokio::test]
    async fn legacy_webchat_schedule_and_reminder_get_a_proof_and_deliver() {
        let f = fixture("sales").await;
        let h = f.home.path();
        legacy_cron(&f, "legacy", principal(&f.user), None).await;
        write_reminders(h, &[legacy_reminder(&f, "r-legacy", principal(&f.user).as_deref())]);
        // A v1.82 row that already carries its proof is left alone.
        let store = CronStore::open(h).unwrap();
        let mut proven = CronTaskRow::new("proven".into(), "已證明".into(), "sales".into(), "0 9 * * *".into(), "t".into());
        proven.notify_channel = Some(crate::scheduled_webchat::CHANNEL.into());
        proven.notify_chat_id = Some(f.target.clone());
        proven.notify_origin = Some(f.target.clone());
        proven.notify_origin_proof = Some(f.proof.clone());
        store.insert(&proven).await.unwrap();
        // Without the backfill a released schedule would stop delivering.
        assert_eq!(crate::cron_scheduler::deliver_cron_result_for_test(h, &cron(h, "legacy").await, "x").await,
            Err("origin_unproven".to_string()));

        assert_eq!(run(h).unwrap(), Summary { minted: 2, skipped: 1, refused: 0 });
        let row = cron(h, "legacy").await;
        let proof = row.notify_origin_proof.clone().expect("legacy schedule proven");
        assert_eq!(row.notify_origin, None, "only the proof id is written");
        let record = super::super::verify(h, "sales", &f.target, Some(&proof)).unwrap();
        assert_eq!((record.user_id.as_str(), record.session.as_str()), (f.user.as_str(), f.session.as_str()));
        crate::cron_scheduler::deliver_cron_result_for_test(h, &row, "今天沒有待辦").await.unwrap();
        let stored = reminder(h, "r-legacy");
        assert!(stored.notify_origin_proof.is_some() && stored.notify_origin.is_none());
        crate::reminder_scheduler::deliver_for_test(h, &stored).await.unwrap();
        assert_eq!(messages(&f).await, vec!["⏰ [每日摘要] 今天沒有待辦".to_string(), "⏰ 記得開會".to_string()]);
        assert_eq!(cron(h, "proven").await.notify_origin_proof.as_deref(), Some(f.proof.as_str()));

        // Idempotent: a second start mints nothing and changes no id.
        let before = proof_count(h);
        assert_eq!(run(h).unwrap(), Summary { minted: 0, skipped: 3, refused: 0 });
        assert_eq!(proof_count(h), before);
        assert_eq!(cron(h, "legacy").await.notify_origin_proof, Some(proof));
        assert_eq!(reminder(h, "r-legacy").notify_origin_proof, stored.notify_origin_proof);
    }

    #[tokio::test]
    async fn suspended_creator_or_cleared_conversation_gets_no_proof_and_is_refused() {
        let f = fixture("sales").await;
        let h = f.home.path();
        legacy_cron(&f, "legacy", principal(&f.user), None).await;
        write_reminders(h, &[legacy_reminder(&f, "r-legacy", principal(&f.user).as_deref())]);
        let before = proof_count(h);
        let db = UserDb::new(&h.join("users.db")).unwrap();
        db.set_user_status(&f.user, UserStatus::Suspended).unwrap();
        assert_eq!(run(h).unwrap(), Summary { minted: 0, skipped: 0, refused: 2 });
        assert_eq!(proof_count(h), before);
        assert_refused_at_fire_time(h).await;
        // Active again, but the conversation was cleared: still nothing.
        db.set_user_status(&f.user, UserStatus::Active).unwrap();
        crate::session::SessionManager::new(&h.join("sessions.db")).unwrap().delete_session(&f.session).await.unwrap();
        assert_eq!(run(h).unwrap(), Summary { minted: 0, skipped: 0, refused: 2 });
        assert_eq!(proof_count(h), before);
        assert!(cron(h, "legacy").await.notify_origin_proof.is_none());
        assert!(reminder(h, "r-legacy").notify_origin_proof.is_none());
        assert_refused_at_fire_time(h).await;
        assert!(messages(&f).await.is_empty());
    }

    /// The unproven legacy cron row and reminder are refused `origin_unproven`.
    async fn assert_refused_at_fire_time(h: &Path) {
        assert_eq!(crate::cron_scheduler::deliver_cron_result_for_test(h, &cron(h, "legacy").await, "x").await,
            Err("origin_unproven".to_string()));
        let error = crate::reminder_scheduler::deliver_for_test(h, &reminder(h, "r-legacy")).await.unwrap_err();
        assert!(error.starts_with("origin_unproven"), "{error}");
    }

    #[tokio::test]
    async fn creator_must_be_recorded_once_and_own_the_conversation() {
        let f = fixture("sales").await;
        let h = f.home.path();
        let db = UserDb::new(&h.join("users.db")).unwrap();
        let other = db.create_user("other@test.invalid", "Other", "fixture-password", UserRole::Employee).unwrap();
        db.bind_agent(&other.id, "sales", AccessLevel::Operator).unwrap();
        legacy_cron(&f, "no-creator", None, None).await;
        legacy_cron(&f, "not-the-owner", principal(&other.id), None).await;
        legacy_cron(&f, "two-creators", principal(&f.user), Some(other.id.clone())).await;
        let mut default_agent = legacy_reminder(&f, "r-default", principal(&f.user).as_deref());
        default_agent.agent_id = "default".into();
        write_reminders(h, &[
            legacy_reminder(&f, "r-no-creator", None),
            legacy_reminder(&f, "r-unmapped", Some("unmapped:line:0123456789abcdef")),
            legacy_reminder(&f, "r-not-the-owner", principal(&other.id).as_deref()),
            default_agent,
        ]);
        let before = proof_count(h);
        assert_eq!(run(h).unwrap(), Summary { minted: 0, skipped: 0, refused: 7 });
        assert_eq!(proof_count(h), before);
        for id in ["no-creator", "not-the-owner", "two-creators"] {
            assert!(cron(h, id).await.notify_origin_proof.is_none(), "{id}");
        }
        for id in ["r-no-creator", "r-unmapped", "r-not-the-owner", "r-default"] {
            assert!(reminder(h, id).notify_origin_proof.is_none(), "{id}");
        }
        // The dashboard creator alone identifies the creator too.
        legacy_cron(&f, "dashboard-creator", None, Some(f.user.clone())).await;
        assert_eq!(run(h).unwrap(), Summary { minted: 1, skipped: 0, refused: 7 });
        assert!(cron(h, "dashboard-creator").await.notify_origin_proof.is_some());
    }

    #[tokio::test]
    async fn channel_origin_rows_are_never_backfilled() {
        use super::super::tests::{RECIPIENT, fixture as channel_fixture};
        let f = channel_fixture("sales").await;
        let h = f.home.path();
        let store = CronStore::open(h).unwrap();
        let mut row = CronTaskRow::new("channel".into(), "Daily".into(), "sales".into(), "0 9 * * *".into(), "t".into());
        row.notify_channel = Some("googlechat".into());
        row.notify_chat_id = Some("spaces/ORIGIN".into());
        row.notify_origin = Some(RECIPIENT.into());
        row.on_behalf_of = principal(&f.user);
        store.insert(&row).await.unwrap();
        let channel_reminder: Reminder = serde_json::from_value(serde_json::json!({
            "id": "r-channel", "agent_id": "sales", "trigger_at": chrono::Utc::now(), "channel": "googlechat",
            "chat_id": "spaces/ORIGIN", "mode": "direct", "message": "m", "notify_origin": RECIPIENT,
            "on_behalf_of": principal(&f.user),
        })).unwrap();
        write_reminders(h, &[channel_reminder]);
        let before = proof_count(h);
        assert_eq!(run(h).unwrap(), Summary::default());
        assert_eq!(proof_count(h), before);
        assert!(cron(h, "channel").await.notify_origin_proof.is_none());
        assert!(reminder(h, "r-channel").notify_origin_proof.is_none());
    }

    #[tokio::test]
    async fn unreadable_store_mints_nothing_and_the_next_start_retries() {
        let f = fixture("sales").await;
        let h = f.home.path();
        legacy_cron(&f, "legacy", principal(&f.user), None).await;
        let good = legacy_reminder(&f, "r-legacy", principal(&f.user).as_deref());
        write_reminders(h, std::slice::from_ref(&good));
        let before = proof_count(h);
        // A malformed reminders file: nothing is minted, not even for cron.
        std::fs::write(h.join("reminders.jsonl"),
            format!("{}\n{{not json\n", serde_json::to_string(&good).unwrap())).unwrap();
        assert_eq!(run(h), Err(Abort::Reminders));
        assert_eq!(proof_count(h), before);
        assert!(cron(h, "legacy").await.notify_origin_proof.is_none());
        // Missing conversation store: same.
        write_reminders(h, std::slice::from_ref(&good));
        std::fs::rename(h.join("sessions.db"), h.join("sessions.moved")).unwrap();
        assert_eq!(run(h), Err(Abort::State));
        std::fs::rename(h.join("sessions.moved"), h.join("sessions.db")).unwrap();
        assert_eq!(proof_count(h), before);
        assert!(cron(h, "legacy").await.notify_origin_proof.is_none());
        assert!(reminder(h, "r-legacy").notify_origin_proof.is_none());
        // Readable again: the next start backfills.
        assert_eq!(run(h).unwrap(), Summary { minted: 2, skipped: 0, refused: 0 });
    }

    #[tokio::test]
    async fn only_the_gateway_lock_holder_backfills() {
        let f = fixture("sales").await;
        let h = f.home.path();
        legacy_cron(&f, "legacy", principal(&f.user), None).await;
        // Not the lock holder (an MCP process never is): nothing happens.
        startup_backfill(h).await;
        assert!(cron(h, "legacy").await.notify_origin_proof.is_none());
        duduclaw_core::gateway_instance::acquire(h).unwrap();
        startup_backfill(h).await;
        assert!(cron(h, "legacy").await.notify_origin_proof.is_some());
    }

    fn marked(home: &Path) -> bool {
        Connection::open(home.join("sessions.db")).unwrap()
            .query_row("SELECT EXISTS(SELECT 1 FROM session_migrations WHERE name = ?1)", params![MARKER], |r| r.get(0))
            .unwrap()
    }

    #[tokio::test]
    async fn completed_migration_never_mints_for_a_later_row_without_a_proof() {
        let f = fixture("sales").await;
        let h = f.home.path();
        legacy_cron(&f, "legacy", principal(&f.user), None).await;
        write_reminders(h, &[legacy_reminder(&f, "r-legacy", principal(&f.user).as_deref())]);
        assert!(!marked(h));
        assert_eq!(run_once(h).unwrap(), Some(Summary { minted: 2, skipped: 0, refused: 0 }));
        assert!(marked(h), "a completed pass records the marker");
        assert!(cron(h, "legacy").await.notify_origin_proof.is_some());

        // A proof-less row that appears after the migration (same shape as a
        // released one, recorded creator and all) gets nothing.
        legacy_cron(&f, "planted", principal(&f.user), None).await;
        write_reminders(h, &[
            reminder(h, "r-legacy"),
            legacy_reminder(&f, "r-planted", principal(&f.user).as_deref()),
        ]);
        let before = proof_count(h);
        assert_eq!(run_once(h).unwrap(), None);
        assert_eq!(proof_count(h), before);
        assert!(cron(h, "planted").await.notify_origin_proof.is_none());
        assert!(reminder(h, "r-planted").notify_origin_proof.is_none());
        assert_eq!(crate::cron_scheduler::deliver_cron_result_for_test(h, &cron(h, "planted").await, "x").await,
            Err("origin_unproven".to_string()));
        // The lock holder's startup entry point is gated the same way.
        duduclaw_core::gateway_instance::acquire(h).unwrap();
        startup_backfill(h).await;
        assert_eq!(proof_count(h), before);
        assert!(cron(h, "planted").await.notify_origin_proof.is_none());
    }

    #[tokio::test]
    async fn stopped_pass_writes_no_marker_and_the_next_start_completes() {
        let f = fixture("sales").await;
        let h = f.home.path();
        legacy_cron(&f, "legacy", principal(&f.user), None).await;
        let good = legacy_reminder(&f, "r-legacy", principal(&f.user).as_deref());
        // Unreadable reminders file: the scan stops, no marker.
        std::fs::write(h.join("reminders.jsonl"),
            format!("{}\n{{not json\n", serde_json::to_string(&good).unwrap())).unwrap();
        assert_eq!(run_once(h), Err(Abort::Reminders));
        assert!(!marked(h));
        // Unreadable account store after the scan found work: no marker.
        write_reminders(h, std::slice::from_ref(&good));
        std::fs::rename(h.join("users.db"), h.join("users.moved")).unwrap();
        assert_eq!(run_once(h), Err(Abort::Accounts));
        std::fs::rename(h.join("users.moved"), h.join("users.db")).unwrap();
        assert!(!marked(h));
        // Marker store itself unreadable: nothing is scanned or minted.
        std::fs::rename(h.join("sessions.db"), h.join("sessions.moved")).unwrap();
        assert_eq!(run_once(h), Err(Abort::State));
        std::fs::rename(h.join("sessions.moved"), h.join("sessions.db")).unwrap();
        assert!(!marked(h));
        assert!(cron(h, "legacy").await.notify_origin_proof.is_none());
        assert!(reminder(h, "r-legacy").notify_origin_proof.is_none());
        // Readable again: the next start completes and records the marker.
        assert_eq!(run_once(h).unwrap(), Some(Summary { minted: 2, skipped: 0, refused: 0 }));
        assert!(marked(h));
    }

    #[tokio::test]
    async fn empty_first_run_still_writes_the_marker() {
        let f = fixture("sales").await;
        let h = f.home.path();
        assert_eq!(run_once(h).unwrap(), Some(Summary::default()));
        assert!(marked(h));
        legacy_cron(&f, "later", principal(&f.user), None).await;
        let before = proof_count(h);
        assert_eq!(run_once(h).unwrap(), None);
        assert_eq!(proof_count(h), before);
        assert!(cron(h, "later").await.notify_origin_proof.is_none());
    }

    #[test]
    fn recorded_creator_needs_exactly_one_account() {
        assert_eq!(recorded_creator(Some("user:u1"), None).as_deref(), Some("u1"));
        assert_eq!(recorded_creator(None, Some("u1")).as_deref(), Some("u1"));
        assert_eq!(recorded_creator(Some("user:u1"), Some("u1")).as_deref(), Some("u1"));
        assert_eq!(recorded_creator(Some("user:u1"), Some("u2")), None);
        assert_eq!(recorded_creator(None, None), None);
        assert_eq!(recorded_creator(None, Some("")), None);
        assert_eq!(recorded_creator(Some(""), Some("u1")), None, "a present but invalid principal is not absent");
        assert_eq!(recorded_creator(Some("unknown:rejected-principal"), None), None);
        assert_eq!(recorded_creator(Some("unmapped:line:0123456789abcdef"), None), None);
    }
}
