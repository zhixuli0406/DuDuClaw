//! Who a collection owner may grant access to.
//!
//! - `knowledge.principals.search`: the permission editor's picker. Admin,
//!   or the owner of the named collection (any owned collection when none
//!   is named). Returns display data only — never e-mail addresses. People
//!   are never listed in bulk: they come back only for a search of at
//!   least [`MIN_USER_QUERY_CHARS`] characters (at most
//!   [`MAX_USER_RESULTS`]), or by id through `resolve` for ids already on
//!   the collection's grant list (Admin: any id), so the editor can show
//!   names for existing grants.
//! - [`check_new_grants`]: runs before `knowledge.acl.set` so the rule the
//!   picker shows is also the rule the write enforces (hiding a principal
//!   is not authorization). Principals the request adds, and existing
//!   grants whose role goes up, are checked; unchanged grants are kept.
//!
//! What may be granted:
//!
//! | kind | rule |
//! |---|---|
//! | user | Enterprise edition only; an active dashboard account |
//! | department | Enterprise edition only; a known department |
//! | agent | an existing employee the caller OWNS (Admin: any) |
//! | channel | a known chat channel, optionally `<channel>:<chat id>` |

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use duduclaw_auth::db::UserDb;
use duduclaw_auth::models::UserStatus;
use duduclaw_auth::{AccessLevel, UserContext};
use duduclaw_knowledge::acl::{self, PrincipalKind, Role};
use duduclaw_knowledge::requester::ResolvedUser;
use duduclaw_knowledge::{KbError, KbResult, schema, store};
use rusqlite::Connection;
use serde::Serialize;
use serde_json::{Value, json};

/// Most rows returned per principal kind.
pub const MAX_RESULTS_PER_KIND: usize = 50;
/// People are returned only for a search at least this long (characters).
pub const MIN_USER_QUERY_CHARS: usize = 2;
/// Most people returned for one search.
pub const MAX_USER_RESULTS: usize = 20;
/// Most ids one `resolve` request may name.
pub const MAX_RESOLVE_IDS: usize = 500;

/// An AI employee as the picker shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentRef {
    pub id: String,
    pub display_name: String,
    pub department: Option<String>,
}

/// A dashboard account as the picker shows it (no e-mail).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UserRef {
    pub id: String,
    pub display_name: String,
    pub department: Option<String>,
}

/// Everything the grant rules need, gathered once per request by the
/// dispatcher (registry, user database, edition).
#[derive(Clone, Default)]
pub struct Directory {
    pub personal: bool,
    pub users: Option<Arc<UserDb>>,
    pub agents: Vec<AgentRef>,
    pub departments: Vec<String>,
}

/// Chat channels a `channel:` grant may name (WebChat callers are dashboard
/// users and are granted as users), plus the public-visitor channels
/// (website widget, Odoo website Live Chat). A visitor reads only the
/// `public` collections granted to its channel; the ACL query enforces that.
pub fn grantable_channels() -> Vec<&'static str> {
    crate::channel_capabilities::all_channels()
        .map(|c| c.channel)
        .filter(|c| *c != "webchat")
        .chain(duduclaw_knowledge::requester::PUBLIC_VISITOR_CHANNELS.iter().copied())
        .collect()
}

impl Directory {
    /// Active accounts (empty without a user database — fail closed).
    fn active_users(&self) -> Vec<UserRef> {
        let Some(db) = self.users.as_ref() else {
            return Vec::new();
        };
        match db.list_users() {
            Ok(list) => list
                .into_iter()
                .filter(|u| u.status == UserStatus::Active)
                .map(|u| UserRef {
                    id: u.id,
                    display_name: u.display_name,
                    department: u.department,
                })
                .collect(),
            Err(e) => {
                tracing::warn!(error = %e, "knowledge: user list unavailable for the permission picker");
                Vec::new()
            }
        }
    }

    /// Departments known from the registry and the accounts.
    fn known_departments(&self, users: &[UserRef]) -> Vec<String> {
        let mut set: BTreeSet<String> = self.departments.iter().cloned().collect();
        set.extend(users.iter().filter_map(|u| u.department.clone()));
        set.extend(self.agents.iter().filter_map(|a| a.department.clone()));
        set.into_iter()
            .filter(|d| duduclaw_core::department::is_valid_department(d))
            .collect()
    }

    fn agents_for(&self, ctx: &UserContext) -> Vec<AgentRef> {
        self.agents
            .iter()
            .filter(|a| ctx.has_agent_access(&a.id, AccessLevel::Owner))
            .cloned()
            .collect()
    }

    /// Whether `ctx` may add a grant for `(kind, id)`.
    pub fn may_grant(&self, ctx: &UserContext, kind: PrincipalKind, id: &str) -> bool {
        match kind {
            PrincipalKind::User => !self.personal && self.active_users().iter().any(|u| u.id == id),
            PrincipalKind::Department => {
                !self.personal && {
                    let users = self.active_users();
                    self.known_departments(&users).iter().any(|d| d == id)
                }
            }
            PrincipalKind::Agent => {
                self.agents.iter().any(|a| a.id == id)
                    && ctx.has_agent_access(id, AccessLevel::Owner)
            }
            PrincipalKind::Channel => {
                let channel = id.split_once(':').map_or(id, |(c, _)| c);
                grantable_channels().contains(&channel)
            }
        }
    }
}

/// Case-insensitive filter for the picker's search box. A display filter
/// only; nothing is authorized by it.
fn matches(query: &str, fields: &[&str]) -> bool {
    query.is_empty() || fields.iter().any(|f| f.to_lowercase().contains(query))
}

/// Admin, or the owner of `collection_id` (of any collection when `None`).
fn require_granting_owner(
    conn: &Connection,
    user: &ResolvedUser,
    collection_id: Option<&str>,
) -> KbResult<()> {
    if user.is_admin {
        return Ok(());
    }
    if let Some(id) = collection_id {
        acl::require_role(
            conn,
            id,
            &user.user_id,
            user.department.as_deref(),
            false,
            Role::Owner,
        )?;
        return Ok(());
    }
    let owned = store::list_for_user(conn, &user.user_id, user.department.as_deref(), false)?
        .iter()
        .any(|c| c.my_role == Some(Role::Owner));
    if owned {
        Ok(())
    } else {
        Err(KbError::PermissionDenied)
    }
}

/// `knowledge.principals.search {query?, collection_id?}`.
pub fn search(
    home: &Path,
    dir: &Directory,
    ctx: &UserContext,
    user: &ResolvedUser,
    params: &Value,
) -> KbResult<Value> {
    let conn = schema::open_home(home)?;
    let collection_id = params
        .get("collection_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    require_granting_owner(&conn, user, collection_id)?;
    let query = params
        .get("query")
        .and_then(Value::as_str)
        .map(|q| duduclaw_core::truncate_chars(q.trim(), 64).to_lowercase())
        .unwrap_or_default();

    let all_users = if dir.personal {
        Vec::new()
    } else {
        dir.active_users()
    };
    let departments: Vec<String> = if dir.personal {
        Vec::new()
    } else {
        dir.known_departments(&all_users)
            .into_iter()
            .filter(|d| matches(&query, &[d.as_str()]))
            .take(MAX_RESULTS_PER_KIND)
            .collect()
    };
    let resolved = resolve_users(&conn, &all_users, user, collection_id, params)?;
    // No bulk listing of people: only a real search returns them.
    let users: Vec<UserRef> = if query.chars().count() >= MIN_USER_QUERY_CHARS {
        all_users
            .into_iter()
            .filter(|u| matches(&query, &[&u.display_name, &u.id]))
            .take(MAX_USER_RESULTS)
            .collect()
    } else {
        Vec::new()
    };
    let agents: Vec<AgentRef> = dir
        .agents_for(ctx)
        .into_iter()
        .filter(|a| matches(&query, &[&a.display_name, &a.id]))
        .take(MAX_RESULTS_PER_KIND)
        .collect();
    let channels: Vec<&str> = grantable_channels()
        .into_iter()
        .filter(|c| matches(&query, &[c]))
        .collect();
    let kinds: Vec<&str> = if dir.personal {
        vec!["agent", "channel"]
    } else {
        vec!["user", "department", "agent", "channel"]
    };
    Ok(json!({
        "edition": if dir.personal { "personal" } else { "enterprise" },
        "kinds": kinds,
        "users": users,
        "resolved_users": resolved,
        "departments": departments,
        "agents": agents,
        "channels": channels,
    }))
}

/// Names for user ids the editor already shows: ids on the collection's
/// grant list (Admin: any id). Unknown or inactive ids are left out.
fn resolve_users(
    conn: &Connection,
    all_users: &[UserRef],
    user: &ResolvedUser,
    collection_id: Option<&str>,
    params: &Value,
) -> KbResult<Vec<UserRef>> {
    let Some(raw) = params.get("resolve").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    if raw.len() > MAX_RESOLVE_IDS {
        return Err(KbError::Invalid(format!(
            "一次最多查 {MAX_RESOLVE_IDS} 個名稱。"
        )));
    }
    let wanted: BTreeSet<&str> = raw.iter().filter_map(Value::as_str).collect();
    let on_list: BTreeSet<String> = match collection_id {
        Some(id) => acl::get_acl(conn, id)?
            .0
            .into_iter()
            .filter(|e| e.kind == PrincipalKind::User)
            .map(|e| e.id)
            .collect(),
        None => BTreeSet::new(),
    };
    Ok(all_users
        .iter()
        .filter(|u| wanted.contains(u.id.as_str()) && (user.is_admin || on_list.contains(&u.id)))
        .cloned()
        .collect())
}

/// Every requested grant, strictly: a row without a known kind, a
/// non-empty id and a known role is refused here rather than skipped.
fn requested_grants(params: &Value) -> KbResult<Vec<(PrincipalKind, String, Role)>> {
    let bad = || KbError::Invalid("授權項目格式不正確（需要 kind、id、role）。".to_string());
    let rows = params
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(bad)?;
    rows.iter()
        .map(|e| {
            let kind = e
                .get("kind")
                .and_then(Value::as_str)
                .and_then(PrincipalKind::parse);
            let id = e
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|i| !i.is_empty());
            let role = e.get("role").and_then(Value::as_str).and_then(Role::parse);
            match (kind, id, role) {
                (Some(k), Some(i), Some(r)) => Ok((k, i.to_string(), r)),
                _ => Err(bad()),
            }
        })
        .collect()
}

/// Refuse a `knowledge.acl.set` that adds a principal the caller may not
/// grant. A caller who is not the collection's owner gets the same
/// "permission denied" `acl.set` would give.
pub fn check_new_grants(
    home: &Path,
    dir: &Directory,
    ctx: &UserContext,
    user: &ResolvedUser,
    params: &Value,
) -> KbResult<()> {
    let Some(collection_id) = params
        .get("collection_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        return Ok(());
    };
    let conn = schema::open_home(home)?;
    acl::require_role(
        &conn,
        collection_id,
        &user.user_id,
        user.department.as_deref(),
        user.is_admin,
        Role::Owner,
    )?;
    // This check and the write use separate connections. The write is
    // bound to the caller's `expected_revision`: if the grants change after
    // this read, the revision moves and `acl.set` refuses the stale list, so
    // a check against an outdated list can never be the one that applies.
    let (current, _) = acl::get_acl(&conn, collection_id)?;
    for (kind, id, role) in requested_grants(params)? {
        // A grant is checked when it is new, or when its role goes up
        // (viewer → editor, anything → owner). Unchanged or lowered grants
        // are kept as they are.
        let existing = current
            .iter()
            .find(|e| e.kind == kind && e.id == id)
            .map(|e| e.role);
        let needs_check = existing.is_none_or(|r| role > r);
        if needs_check && !dir.may_grant(ctx, kind, &id) {
            return Err(KbError::Invalid(refusal_message(dir, kind, &id)));
        }
    }
    Ok(())
}

fn refusal_message(dir: &Directory, kind: PrincipalKind, id: &str) -> String {
    let id = duduclaw_core::truncate_chars(id, 64);
    match kind {
        PrincipalKind::User | PrincipalKind::Department if dir.personal => {
            "個人版只能授權給 AI 員工或通道。".to_string()
        }
        PrincipalKind::User => format!("找不到使用者「{id}」，或該帳號已停用。"),
        PrincipalKind::Department => format!("找不到部門「{id}」。"),
        PrincipalKind::Agent => format!("只能分享給你擁有的 AI 員工，「{id}」不是。"),
        PrincipalKind::Channel => format!("不認得的通道「{id}」。"),
    }
}

#[cfg(test)]
#[path = "principals_tests.rs"]
mod tests;
