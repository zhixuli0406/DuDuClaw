//! Personal employee data visibility (native Linux isolation v1.81, task P1,
//! SR2 finding 5).
//!
//! A Personal employee belongs to exactly one person (the `users.db`
//! `isolation_personal_agents` mirror written by the employee apply path).
//! Surfaces built for Shared employees must not hand its private data —
//! conversations, memory, agent files (SOUL.md, CONTRACT.toml, working state)
//! and wiki pages — to anyone else:
//!
//! - **Team grants** (`duduclaw_auth::org::team_grants_on`,
//!   `overseen_reports_on`) skip Personal employees in SQL, so a manager gets
//!   no derived access and no team-history oversight of a subordinate's
//!   Personal employee.
//! - **Dashboard reads** go through [`classify_reader`]: the owner reads as
//!   before; an Admin who is not the owner gets [`Reader::BreakGlass`] and the
//!   dashboard gate (`handlers/personal_data_gate.rs`) requires a fresh
//!   step-up (the migration/isolation dashboard proof) and writes an audit
//!   row before anything is read; everybody else is refused. Cross-agent
//!   listings hide the Personal employees the caller does not own
//!   ([`hidden_from`]), so listing and access agree.
//! - **Cross-agent management MCP tools** call [`management_refusal`]: an AI
//!   caller never manages another person's Personal employee; human owner
//!   actions go through the dashboard.
//! - **P1b:** the same refusal covers delegation and cross-agent reads
//!   (`spawn_agent`, `send_to_agent`, `check_responses`, `task_status`, memory
//!   namespaces); `list_agents` hides ([`hidden_from_employee`]); an
//!   ephemeral sub-agent acts as its parent ([`acting_employee`]); task,
//!   activity, timeline, decision, canvas and approval reads are category
//!   [`DataCategory::Work`] on the dashboard.
//! - **P1c:** deciding a Personal employee's approval card is category
//!   [`DataCategory::Approval`] (owner, or an audited Admin step-up on the
//!   dashboard; owner only on channels — `crate::personal_decision`);
//!   handing work to one is owner-only on the dashboard
//!   ([`delegation_refusal_for_user`]) and, for AI callers, allowed only from
//!   the employee itself or another Personal employee of the same owner
//!   ([`delegation_refusal`]); tasks and plans named by id resolve their
//!   employee through [`work_item_employee`].
//! - **P1d:** an employee id must be canonical ([`canonical_agent`]: no
//!   surrounding whitespace) before it is compared or used, so the gates and
//!   the handlers always name the same employee; mail is category
//!   [`DataCategory::Mail`]; a goal of the goal hierarchy belongs to the
//!   employee that created it ([`WorkItem::Goal`], [`goal_creators`]).
//!
//! Every check here runs only in protected mode (`protected = true`, from
//! `runtime_readiness::is_protected_linux()`); otherwise nothing is read and
//! the existing behaviour is unchanged. The mirror is read fresh from
//! `users.db` on every call through the hardened authority-store opener; an
//! unreadable store is [`Unavailable`] and every caller refuses.
use std::collections::BTreeSet;
use std::path::Path;

use crate::fs_safe::sqlite::{Access, open_existing};

const USERS_DB: &str = "users.db";

/// The private data category named in the audit row of an Admin read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataCategory {
    /// Conversations / session history / transcripts and cross-source search.
    Sessions,
    /// The employee's memory store.
    Memory,
    /// Profile and working files (SOUL.md, CONTRACT.toml, agent.toml, …).
    AgentFiles,
    /// The employee's own wiki pages.
    Wiki,
    /// Its work record (P1b): tasks and their detail, activity, timeline,
    /// open decisions, canvas and approval payloads.
    Work,
    /// A decision on one of its approval cards (P1c): `approvals.decide`,
    /// `mail.decide`.
    Approval,
    /// Its received and drafted mail (P1d): `mail.list`, `mail.read`,
    /// `mail.archive`, `mail.outbox`.
    Mail,
}

impl DataCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sessions => "sessions",
            Self::Memory => "memory",
            Self::AgentFiles => "agent_files",
            Self::Wiki => "wiki",
            Self::Work => "work",
            Self::Approval => "approval",
            Self::Mail => "mail",
        }
    }
}

/// A work item named by id in a tool argument (P1c).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkItem {
    /// `tasks.db` `tasks.id` → `assigned_to`.
    Task,
    /// `tasks.db` `plans.id` → `agent_id`.
    Plan,
    /// `tasks.db` `goals.id` → the employee whose `goal_created` activity
    /// row records it (P1d). A goal no employee created (dashboard / legacy
    /// company goal) has none.
    Goal,
}

/// Shown when an employee id carries surrounding whitespace (P1d).
pub const NON_CANONICAL_AGENT: &str = "AI 員工代號前後不能有空白。 / An employee id must not have leading or \
    trailing whitespace.";

/// P1d: `raw` as a canonical employee id. Every gate and handler compares
/// and uses this exact value; an id with surrounding whitespace is refused
/// rather than trimmed, so a gate can never check one string while the
/// handler acts on another.
pub fn canonical_agent(raw: &str) -> Result<&str, &'static str> {
    if raw.trim() == raw { Ok(raw) } else { Err(NON_CANONICAL_AGENT) }
}

/// The creating employee of every goal, from its `goal_created` activity row
/// (`metadata.goal_id`; a legacy row without metadata matches by title).
const GOAL_CREATOR: &str = "(SELECT a.agent_id FROM activity a WHERE a.event_type = 'goal_created' AND \
    (CASE WHEN json_valid(a.metadata) THEN json_extract(a.metadata, '$.goal_id') END = g.id \
    OR (a.metadata IS NULL AND a.summary = 'Created goal: ' || g.title)) ORDER BY a.timestamp ASC LIMIT 1)";

/// What a dashboard caller may do with one employee's private data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reader {
    /// Not protected mode, or not a Personal employee: the existing checks
    /// decide alone.
    Unrestricted,
    /// The caller is the employee's recorded owner.
    Owner,
    /// An Admin who is not the owner: only with a fresh step-up, audited.
    BreakGlass { owner: String },
    /// Anyone else, including the legacy `system` admin context.
    Refused,
}

/// The `users.db` mirror could not be read; callers must refuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unavailable;

/// Shown to an AI caller that targets another person's Personal employee.
pub const PERSONAL_TARGET_REFUSED: &str = "這位 AI 員工是某位同仁的個人專屬員工，其他 AI 員工不能讀取、委派工作給它，\
    也不能修改或移除它的設定與檔案。如需變更，請由擁有者在控制台操作。 / This is a person's Personal employee; another \
    AI employee cannot read, delegate to, change or remove it. The owner manages it from the dashboard.";
/// Shown when the Personal mirror cannot be read (fail closed).
pub const OWNERSHIP_UNAVAILABLE: &str = "無法確認這位 AI 員工的歸屬，已拒絕操作。 / Employee ownership could not be \
    verified; the operation was refused.";
/// Shown when a dashboard user who is not the owner hands work to a
/// Personal employee (P1c).
pub const PERSONAL_DELEGATION_REFUSED: &str = "這位 AI 員工是某位同仁的個人專屬員工，只有它的擁有者能交辦工作給它。 / This is a \
    person's Personal employee; only its owner can hand work to it.";
/// Shown when someone who is not the owner decides a Personal employee's
/// approval card where no step-up is possible (channels, chat replies) or
/// is not an Admin (P1c).
pub const PERSONAL_DECISION_REFUSED: &str = "這張審核卡屬於某位同仁的個人專屬員工，只有它的擁有者能決定。 / This approval \
    belongs to a person's Personal employee; only its owner can decide it.";

fn open(home: &Path) -> Result<rusqlite::Connection, Unavailable> {
    open_existing(home, USERS_DB, Access::ReadOnly).map_err(|_| Unavailable)
}

/// The recorded Personal owner of `agent`, read fresh from `users.db`.
/// `Ok(None)` = Shared or unknown employee. Evidence only; the run-time
/// authority still checks the full lifecycle.
pub fn personal_owner(home: &Path, agent: &str) -> Result<Option<String>, Unavailable> {
    let conn = open(home)?;
    duduclaw_auth::db::personal_agent_owner(&conn, agent).map_err(|_| Unavailable)
}

/// Personal employees whose private data `user_id` does not own, for hiding
/// them from cross-agent listings. Empty outside protected mode.
pub fn hidden_from(home: &Path, protected: bool, user_id: &str) -> Result<BTreeSet<String>, Unavailable> {
    if !protected {
        return Ok(BTreeSet::new());
    }
    let conn = open(home)?;
    let mut statement = conn
        .prepare("SELECT agent_name FROM isolation_personal_agents WHERE owner_user_id <> ?1")
        .map_err(|_| Unavailable)?;
    let rows = statement
        .query_map([user_id], |row| row.get::<_, String>(0))
        .map_err(|_| Unavailable)?;
    rows.collect::<Result<BTreeSet<_>, _>>().map_err(|_| Unavailable)
}

/// Decide how a dashboard caller may read `agent`'s private data.
/// `user_id`/`is_admin` come from the live (users.db) caller context.
pub fn classify_reader(
    home: &Path,
    protected: bool,
    user_id: &str,
    is_admin: bool,
    agent: &str,
) -> Result<Reader, Unavailable> {
    if !protected {
        return Ok(Reader::Unrestricted);
    }
    // The legacy token / local-mode context has no person behind it.
    let person = !user_id.is_empty() && user_id != "system";
    Ok(match personal_owner(home, agent)? {
        None => Reader::Unrestricted,
        Some(owner) if person && owner == user_id => Reader::Owner,
        Some(owner) if person && is_admin => Reader::BreakGlass { owner },
        Some(_) => Reader::Refused,
    })
}

/// The regular employee an acting AI id works as (P1b). A regular id is
/// itself (no I/O). An ephemeral sub-agent (`eph-…`) is the root of its
/// host-recorded parent chain, `duduclaw_core::autonomy_gate::
/// effective_identities`: at every hop the scaffold's `ephemeral.toml`
/// `parent` must equal the host-owned org store's `reports_to`, so neither
/// the sub-agent's own files nor a tool argument can choose its parent.
/// `None` = blank id or an unresolvable chain (callers refuse).
pub fn acting_employee(home: &Path, acting: &str) -> Option<String> {
    if acting.is_empty() {
        return None;
    }
    if !crate::ephemeral::is_ephemeral_id(acting) {
        return Some(acting.to_owned());
    }
    duduclaw_core::autonomy_gate::effective_identities(
        home,
        acting,
        duduclaw_core::autonomy_gate::AutonomyPath::Background,
    )
    .ok()?
    .pop()
}

/// `acting` is `target` itself, or an ephemeral sub-agent of `target`
/// ([`acting_employee`]). Exact id equality; never reads `users.db`.
pub fn same_employee(home: &Path, acting: &str, target: &str) -> bool {
    if acting.is_empty() || target.is_empty() {
        return false;
    }
    acting == target
        || (crate::ephemeral::is_ephemeral_id(acting)
            && acting_employee(home, acting).as_deref() == Some(target))
}

/// Personal employees an AI caller must not see in a cross-agent listing
/// (MCP `list_agents`): every one except the acting employee itself (an
/// ephemeral sub-agent counts as its parent). Other AI employees get no
/// metadata at all — the row is hidden, not redacted. Empty outside
/// protected mode; an unreadable store is [`Unavailable`].
pub fn hidden_from_employee(home: &Path, protected: bool, acting: &str)
    -> Result<BTreeSet<String>, Unavailable> {
    if !protected {
        return Ok(BTreeSet::new());
    }
    let me = acting_employee(home, acting);
    let conn = open(home)?;
    let mut statement = conn
        .prepare("SELECT agent_name FROM isolation_personal_agents")
        .map_err(|_| Unavailable)?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|_| Unavailable)?;
    let mut hidden = rows.collect::<Result<BTreeSet<_>, _>>().map_err(|_| Unavailable)?;
    if let Some(me) = me {
        hidden.remove(&me);
    }
    Ok(hidden)
}

/// Refusal for a cross-agent MCP tool (`agent_update`, `agent_remove`,
/// `spawn_agent`, `check_responses`, …) whose `target` is a Personal
/// employee and whose acting employee is not that employee itself (an
/// ephemeral sub-agent of it counts as it, [`same_employee`]). A self-target
/// is decided before any `users.db` read. `None` = allowed (the tool's own
/// checks still apply).
pub fn management_refusal(home: &Path, protected: bool, acting_agent: &str, target: &str)
    -> Option<&'static str> {
    if !protected || same_employee(home, acting_agent, target) {
        return None;
    }
    match personal_owner(home, target) {
        Ok(None) => None,
        Ok(Some(_)) => Some(PERSONAL_TARGET_REFUSED),
        Err(Unavailable) => Some(OWNERSHIP_UNAVAILABLE),
    }
}

/// P1c: an AI caller handing work to `target` (MCP `spawn_agent`,
/// `send_to_agent`, `tasks_create`/`tasks_update` `assigned_to`, …). As
/// [`management_refusal`], except that an acting employee which is itself a
/// Personal employee of the same owner may hand work to it: that is the
/// owner's own work moving between the owner's own employees. Both owners
/// are read fresh; an unreadable store refuses. `None` = allowed.
pub fn delegation_refusal(home: &Path, protected: bool, acting_agent: &str, target: &str)
    -> Option<&'static str> {
    let refusal = management_refusal(home, protected, acting_agent, target)?;
    if refusal != PERSONAL_TARGET_REFUSED {
        return Some(refusal);
    }
    let Some(me) = acting_employee(home, acting_agent) else {
        return Some(refusal);
    };
    match (personal_owner(home, &me), personal_owner(home, target)) {
        (Ok(Some(mine)), Ok(Some(theirs))) if !mine.is_empty() && mine == theirs => None,
        (Err(Unavailable), _) | (_, Err(Unavailable)) => Some(OWNERSHIP_UNAVAILABLE),
        _ => Some(refusal),
    }
}

/// P1c: a dashboard user (`user_id` from the live users.db context) handing
/// work to `agent` (`tasks.create`/`tasks.update` `assigned_to`,
/// `tasks.goal_create`/`plans.create` `agent_id`). A Personal employee takes
/// work only from its owner — an Admin who is not the owner is refused too
/// (delegation has no break-glass). `None` = allowed (the existing checks
/// still apply). Empty outside protected mode.
pub fn delegation_refusal_for_user(home: &Path, protected: bool, user_id: &str, agent: &str)
    -> Option<&'static str> {
    if !protected {
        return None;
    }
    match classify_reader(home, true, user_id, false, agent) {
        Ok(Reader::Unrestricted | Reader::Owner) => None,
        Ok(Reader::BreakGlass { .. } | Reader::Refused) => Some(PERSONAL_DELEGATION_REFUSED),
        Err(Unavailable) => Some(OWNERSHIP_UNAVAILABLE),
    }
}

/// P1c: the employee a task or plan named by `id` belongs to, read from
/// `tasks.db` without opening a writable store. `Ok(None)` = no such item
/// (the tool answers); `Err` = the store could not be read (callers refuse).
pub fn work_item_employee(home: &Path, item: WorkItem, id: &str) -> Result<Option<String>, Unavailable> {
    use rusqlite::OptionalExtension;
    let goal_sql;
    let sql = match item {
        WorkItem::Task => "SELECT assigned_to FROM tasks WHERE id=?1",
        WorkItem::Plan => "SELECT agent_id FROM plans WHERE id=?1",
        WorkItem::Goal => {
            goal_sql = format!("SELECT {GOAL_CREATOR} FROM goals g WHERE g.id=?1");
            goal_sql.as_str()
        }
    };
    let conn = crate::review_evidence::audience::readonly_db(&home.join("tasks.db")).map_err(|_| Unavailable)?;
    conn.query_row(sql, [id], |row| row.get::<_, Option<String>>(0))
        .optional()
        .map(Option::flatten)
        .map_err(|_| Unavailable)
}

/// P1d: goal id → creating employee, for every goal that has one
/// ([`WorkItem::Goal`]). `Err` = `tasks.db` could not be read.
pub fn goal_creators(home: &Path) -> Result<std::collections::BTreeMap<String, String>, Unavailable> {
    let conn = crate::review_evidence::audience::readonly_db(&home.join("tasks.db")).map_err(|_| Unavailable)?;
    let mut statement = conn
        .prepare(&format!("SELECT g.id, {GOAL_CREATOR} FROM goals g"))
        .map_err(|_| Unavailable)?;
    let rows = statement
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)))
        .map_err(|_| Unavailable)?;
    let mut creators = std::collections::BTreeMap::new();
    for row in rows {
        if let (goal, Some(agent)) = row.map_err(|_| Unavailable)? {
            creators.insert(goal, agent);
        }
    }
    Ok(creators)
}

/// Shared with the dashboard gate tests (`handlers/personal_data_gate_tests.rs`).
#[cfg(all(test, unix))]
#[path = "personal_visibility_tests.rs"]
pub(crate) mod tests;
