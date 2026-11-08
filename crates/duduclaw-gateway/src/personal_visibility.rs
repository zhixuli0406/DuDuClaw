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
}

impl DataCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sessions => "sessions",
            Self::Memory => "memory",
            Self::AgentFiles => "agent_files",
            Self::Wiki => "wiki",
        }
    }
}

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
pub const PERSONAL_TARGET_REFUSED: &str = "這位 AI 員工是某位同仁的個人專屬員工，其他 AI 員工不能修改或移除它的設定與檔案。\
    如需變更，請由擁有者在控制台操作。 / This is a person's Personal employee; another AI employee cannot change or \
    remove it. The owner manages it from the dashboard.";
/// Shown when the Personal mirror cannot be read (fail closed).
pub const OWNERSHIP_UNAVAILABLE: &str = "無法確認這位 AI 員工的歸屬，已拒絕操作。 / Employee ownership could not be \
    verified; the operation was refused.";

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

/// Refusal for a cross-agent management MCP tool (`agent_update`,
/// `agent_update_soul`, `agent_remove`, …) whose `target` is a Personal
/// employee and whose acting employee is not that employee itself.
/// `None` = allowed (the tool's own checks still apply).
pub fn management_refusal(home: &Path, protected: bool, acting_agent: &str, target: &str)
    -> Option<&'static str> {
    if !protected {
        return None;
    }
    match personal_owner(home, target) {
        Ok(None) => None,
        Ok(Some(_)) if !acting_agent.is_empty() && acting_agent == target => None,
        Ok(Some(_)) => Some(PERSONAL_TARGET_REFUSED),
        Err(Unavailable) => Some(OWNERSHIP_UNAVAILABLE),
    }
}

/// Shared with the dashboard gate tests (`handlers/personal_data_gate_tests.rs`).
#[cfg(all(test, unix))]
#[path = "personal_visibility_tests.rs"]
pub(crate) mod tests;
