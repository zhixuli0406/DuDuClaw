//! The employee page's 「資料來源」 tab: what an AI employee can read from
//! the knowledge base, and its own collection.
//!
//! - `knowledge.agent.data_sources {agent_id}` (viewer access to the
//!   employee, re-read from `users.db`): the collections granted to the
//!   employee with counts, sensitivity, whether its settings use them and
//!   who else can read them (names for the collection's owners, counts for
//!   everyone else); its own collection with the files (only when the
//!   caller can read that collection); whether the caller may add or
//!   remove files there.
//! - `knowledge.agent.own.ensure {agent_id, sensitivity?, share_chat_users?}`
//!   (owner access to the employee, re-read from `users.db`): create the
//!   employee's own collection on first use, or return it. Files then go
//!   through the ordinary upload route and `knowledge.sources.delete`, which
//!   check the collection's own grants as for any collection.
//!
//! Default readers of a new own collection: the person creating it
//! (owner), the employee (viewer), Admins (always, as for every
//! collection) and, when `share_chat_users` is on (the dashboard ticks it
//! by default; an RPC call that leaves it out means off), every
//! active account bound to the employee: Owner bindings as collection
//! owners, Operator and Viewer bindings as viewers. These are the people an
//! Admin already chose to work with this employee; nobody else (team
//! managers, departments, channels, unbound senders) is added. Who reads a
//! passage in a reply is still the intersection of the employee's grant and
//! the person's own grant, so an upload never reaches a person who is not
//! on that list.

use std::path::Path;
use std::sync::Arc;

use duduclaw_auth::AccessLevel;
use duduclaw_auth::db::UserDb;
use duduclaw_auth::models::UserStatus;
use duduclaw_core::knowledge_agent::{KnowledgeScope, parse_scope};
use duduclaw_knowledge::acl::{self, AclEntry, PrincipalKind, Role};
use duduclaw_knowledge::agent_own::{self, Grant};
use duduclaw_knowledge::requester::ResolvedUser;
use duduclaw_knowledge::store::{self, CollectionSummary};
use duduclaw_knowledge::{KbError, KbResult, schema};
use rusqlite::Connection;
use serde_json::{Value, json};

use super::agent_binding::{Binding, MAX_BOUND_COLLECTIONS, apply_to_table};
use super::principals::AgentRef;

/// Default readers from the employee's account bindings (active accounts
/// only). Personal edition has no people to share with: none.
pub fn default_readers(users: Option<&Arc<UserDb>>, agent_id: &str, personal: bool) -> Vec<Grant> {
    if personal {
        return Vec::new();
    }
    let Some(db) = users else {
        return Vec::new();
    };
    let bindings = match db.get_agent_users(agent_id) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(agent = %agent_id, error = %e, "knowledge: employee bindings unavailable");
            return Vec::new();
        }
    };
    let mut out: Vec<Grant> = Vec::new();
    for b in bindings {
        let active = matches!(db.get_user(&b.user_id), Ok(Some(u)) if u.status == UserStatus::Active);
        if !active || out.iter().any(|(_, id, _)| *id == b.user_id) {
            continue;
        }
        let role = if b.access_level == AccessLevel::Owner {
            Role::Owner
        } else {
            Role::Viewer
        };
        out.push((PrincipalKind::User, b.user_id, role));
    }
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
}

/// Names for principals, from the accounts and the registry.
pub struct Names<'a> {
    pub users: Option<&'a Arc<UserDb>>,
    pub agents: &'a [AgentRef],
}

impl Names<'_> {
    fn user(&self, id: &str) -> String {
        self.users
            .and_then(|db| db.get_user(id).ok().flatten())
            .map(|u| u.display_name)
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| id.to_string())
    }

    fn agent(&self, id: &str) -> String {
        self.agents
            .iter()
            .find(|a| a.id == id)
            .map(|a| a.display_name.clone())
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| id.to_string())
    }
}

/// Plain-language reader rows for one collection, without the employee
/// itself. `you` flags the caller's own grant.
fn readers_json(entries: &[AclEntry], agent_id: &str, caller: &str, names: &Names<'_>) -> Vec<Value> {
    entries
        .iter()
        .filter(|e| !(e.kind == PrincipalKind::Agent && e.id == agent_id))
        .map(|e| {
            let label = match e.kind {
                PrincipalKind::User => names.user(&e.id),
                PrincipalKind::Agent => names.agent(&e.id),
                PrincipalKind::Department | PrincipalKind::Channel => e.id.clone(),
            };
            json!({
                "kind": e.kind.as_str(),
                "id": e.id,
                "label": label,
                "role": e.role.as_str(),
                "you": e.kind == PrincipalKind::User && e.id == caller,
            })
        })
        .collect()
}

/// How many principals of each kind (without the employee itself).
fn reader_counts(entries: &[AclEntry], agent_id: &str) -> Value {
    let n = |k: PrincipalKind| {
        entries
            .iter()
            .filter(|e| e.kind == k && !(k == PrincipalKind::Agent && e.id == agent_id))
            .count()
    };
    json!({
        "users": n(PrincipalKind::User),
        "departments": n(PrincipalKind::Department),
        "agents": n(PrincipalKind::Agent),
        "channels": n(PrincipalKind::Channel),
    })
}

fn binding_label(scope: &KnowledgeScope) -> &'static str {
    match scope {
        KnowledgeScope::All => "all",
        KnowledgeScope::Only(list) if list.is_empty() => "off",
        KnowledgeScope::Only(_) => "only",
        KnowledgeScope::Unreadable => "unreadable",
    }
}

fn in_use(scope: &KnowledgeScope, slug: &str) -> bool {
    match scope {
        KnowledgeScope::All => true,
        KnowledgeScope::Only(list) => list.iter().any(|s| s == slug),
        KnowledgeScope::Unreadable => false,
    }
}

/// One collection row for the tab. Who reads it is listed by name only to
/// the collection's owners (who see the grant list anyway; Admin: always);
/// everyone else gets counts per kind.
fn collection_json(
    conn: &Connection,
    c: &CollectionSummary,
    agent_id: &str,
    scope: &KnowledgeScope,
    caller: &ResolvedUser,
    names: &Names<'_>,
) -> KbResult<Value> {
    let my_role = acl::user_role(
        conn,
        &c.collection.id,
        &caller.user_id,
        caller.department.as_deref(),
        caller.is_admin,
    )?;
    let (entries, _) = acl::get_acl(conn, &c.collection.id)?;
    let readers = (my_role == Some(Role::Owner)).then(|| readers_json(&entries, agent_id, &caller.user_id, names));
    Ok(json!({
        "id": c.collection.id,
        "slug": c.collection.slug,
        "name": c.collection.name,
        "sensitivity": c.collection.sensitivity,
        "source_count": c.source_count,
        "ready_count": c.ready_count,
        "chunk_count": c.chunk_count,
        "in_use": in_use(scope, &c.collection.slug),
        "is_own": c.own_agent.as_ref().is_some_and(|o| o.agent_id == agent_id),
        "my_role": my_role.map(Role::as_str),
        "readers": readers,
        "reader_counts": reader_counts(&entries, agent_id),
    }))
}

/// Inputs of [`describe`].
pub struct DescribeInput<'a> {
    pub agent: &'a AgentRef,
    pub agent_dir: &'a Path,
    /// The employee is archived or deleted.
    pub inactive: bool,
    pub caller: &'a ResolvedUser,
    /// Live owner access to the employee (Admin: always).
    pub caller_owns_employee: bool,
    pub names: Names<'a>,
    pub default_readers: &'a [Grant],
}

/// The `knowledge.agent.data_sources` answer.
pub fn describe(home: &Path, input: &DescribeInput<'_>) -> KbResult<Value> {
    let conn = schema::open_home(home)?;
    let agent_id = input.agent.id.as_str();
    let scope = duduclaw_core::knowledge_agent::knowledge_scope(input.agent_dir);
    let granted = super::agent_binding::granted_collections(home, input.agent)?;
    let mut rows = Vec::with_capacity(granted.len());
    for c in &granted {
        rows.push(collection_json(&conn, c, agent_id, &scope, input.caller, &input.names)?);
    }
    let own = own_json(&conn, agent_id, &scope, input)?;
    let own_role = own
        .as_ref()
        .and_then(|o| o["collection"]["my_role"].as_str())
        .and_then(Role::parse);
    let blocked = manage_blocked(input, &scope, own.as_ref(), own_role);
    // Who a new collection would be shared with: only for the people who
    // may create it.
    let defaults: Vec<Value> = input
        .default_readers
        .iter()
        .filter(|_| input.caller_owns_employee)
        .filter(|(_, id, _)| *id != input.caller.user_id)
        .map(|(_, id, role)| json!({ "id": id, "label": input.names.user(id), "role": role.as_str() }))
        .collect();
    Ok(json!({
        "agent_id": agent_id,
        "binding": binding_label(&scope),
        "collections": rows,
        "own": own,
        "can_manage": blocked.is_none(),
        "manage_blocked": blocked,
        "default_readers": defaults,
    }))
}

/// Why the caller may not add or remove files here, or `None`.
fn manage_blocked(
    input: &DescribeInput<'_>,
    scope: &KnowledgeScope,
    own: Option<&Value>,
    own_role: Option<Role>,
) -> Option<&'static str> {
    if !input.caller_owns_employee {
        return Some("not_owner");
    }
    if input.inactive || own.is_some_and(|o| o["archived"] == true) {
        return Some("archived");
    }
    if matches!(scope, KnowledgeScope::Unreadable) {
        return Some("settings_unreadable");
    }
    if own.is_some() && own_role.is_none_or(|r| r < Role::Editor) {
        return Some("not_collection_editor");
    }
    None
}

fn own_json(
    conn: &Connection,
    agent_id: &str,
    scope: &KnowledgeScope,
    input: &DescribeInput<'_>,
) -> KbResult<Option<Value>> {
    let Some(own) = agent_own::get(conn, agent_id)? else {
        return Ok(None);
    };
    let Some(summary) = store::list_by_ids(conn, std::slice::from_ref(&own.collection_id))?.pop() else {
        return Ok(None);
    };
    let row = collection_json(conn, &summary, agent_id, scope, input.caller, &input.names)?;
    let readable = !row["my_role"].is_null();
    let sources = if readable {
        serde_json::to_value(duduclaw_knowledge::sources::list_sources(conn, &own.collection_id)?)
            .unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    Ok(Some(json!({
        "collection": row,
        "archived": own.archived_at.is_some(),
        "share_chat_users": own.share_chat_users,
        "sources": sources,
    })))
}

/// Put `slug` into an `agent.toml` table's `[knowledge] collections` when
/// that list exists and lacks it. No list ("every granted collection")
/// needs nothing. `Ok(true)` when the table changed; `Err` when the
/// section cannot be read (left for a person to repair) or is full.
pub fn bind_slug(table: &mut toml::Table, slug: &str) -> Result<bool, String> {
    let text = toml::to_string(table).map_err(|e| format!("agent.toml: {e}"))?;
    match parse_scope(&text) {
        KnowledgeScope::All => Ok(false),
        KnowledgeScope::Only(list) if list.iter().any(|s| s == slug) => Ok(false),
        KnowledgeScope::Only(mut list) => {
            if list.len() >= MAX_BOUND_COLLECTIONS {
                return Err(format!(
                    "這位 AI 員工已綁定 {MAX_BOUND_COLLECTIONS} 個集合，請先在員工設定移除一些。"
                ));
            }
            list.push(slug.to_string());
            apply_to_table(table, &Binding::Only(list))?;
            Ok(true)
        }
        KnowledgeScope::Unreadable => Err(
            "這位 AI 員工設定檔的 [knowledge] 段落無法解讀，請先修正後再新增檔案。".to_string(),
        ),
    }
}

/// What [`ensure_collection`] needs.
pub struct EnsureInput<'a> {
    pub agent: &'a AgentRef,
    pub caller: &'a ResolvedUser,
    pub sensitivity: Option<&'a str>,
    pub share_chat_users: bool,
    pub readers: Vec<Grant>,
}

/// [`ensure_collection`]'s answer.
pub struct EnsureOutcome {
    pub collection: CollectionSummary,
    pub created: bool,
    /// The grants a new collection got (empty for an existing one).
    pub grants: Vec<AclEntry>,
}

/// Create the employee's own collection, or return it when the caller may
/// add files to it (editor or owner of that collection; Admin always).
pub fn ensure_collection(home: &Path, input: &EnsureInput<'_>) -> KbResult<EnsureOutcome> {
    let cfg = duduclaw_knowledge::limits::KnowledgeConfig::from_home(home);
    let mut conn = schema::open_home(home)?;
    let readers: Vec<Grant> = if input.share_chat_users {
        input.readers.clone()
    } else {
        Vec::new()
    };
    let new = agent_own::NewOwn {
        agent_id: &input.agent.id,
        display_name: &input.agent.display_name,
        sensitivity: input.sensitivity,
        share_chat_users: input.share_chat_users,
        created_by: &input.caller.user_id,
        readers,
    };
    let ensured = agent_own::create(&mut conn, &new, &cfg)?;
    let created = matches!(ensured, agent_own::Ensured::Created(_));
    let id = ensured.collection().id.clone();
    let role = acl::user_role(
        &conn,
        &id,
        &input.caller.user_id,
        input.caller.department.as_deref(),
        input.caller.is_admin,
    )?;
    if role.is_none_or(|r| r < Role::Editor) {
        return Err(KbError::PermissionDenied);
    }
    let mut summary = store::list_by_ids(&conn, std::slice::from_ref(&id))?
        .pop()
        .ok_or(KbError::PermissionDenied)?;
    summary.my_role = role;
    let mut grants = Vec::new();
    if created {
        log_change(&conn, input.caller, &id, "collection_change")?;
        log_change(&conn, input.caller, &id, "acl_change")?;
        grants = acl::get_acl(&conn, &id)?.0;
    }
    Ok(EnsureOutcome {
        collection: summary,
        created,
        grants,
    })
}

fn log_change(conn: &Connection, caller: &ResolvedUser, collection_id: &str, op: &str) -> KbResult<()> {
    duduclaw_knowledge::audit::log_access(
        conn,
        &duduclaw_knowledge::audit::AccessEntry {
            op: op.to_string(),
            collection_id: Some(collection_id.to_string()),
            requester_kind: "dashboard_user".into(),
            requester_id: Some(caller.user_id.clone()),
            outcome: "ok".into(),
            ..Default::default()
        },
    )
}

fn grants_json(grants: &[Grant]) -> Vec<Value> {
    grants
        .iter()
        .map(|(k, i, r)| json!({"kind": k.as_str(), "id": i, "role": r.as_str()}))
        .collect()
}

/// The new collection's audit rows (`security_audit.jsonl`).
pub fn audit_created(home: &Path, actor: &str, agent_id: &str, c: &CollectionSummary, grants: &[AclEntry]) {
    super::audit_admin(
        home,
        "knowledge_collection_created",
        actor,
        json!({
            "collection_id": c.collection.id,
            "slug": c.collection.slug,
            "agent_id": agent_id,
            "own_collection": true,
            "sensitivity": c.collection.sensitivity,
        }),
    );
    let summary: Vec<Value> = grants
        .iter()
        .map(|e| json!({"kind": e.kind.as_str(), "id": e.id, "role": e.role.as_str()}))
        .collect();
    super::audit_admin(
        home,
        "knowledge_acl_changed",
        actor,
        json!({"collection_id": c.collection.id, "entries": summary, "reason": "own_collection_created"}),
    );
}

/// Archive the employee's own collection with the employee (archive,
/// soft delete, hand-off). Writes the audit row; an error is logged and
/// audited, never hidden.
pub fn offboard(home: &Path, agent_id: &str, actor: &str) {
    let result = schema::open_home(home).and_then(|mut conn| agent_own::archive(&mut conn, agent_id, actor));
    match result {
        Ok(Some(done)) => super::audit_admin(
            home,
            "knowledge_own_collection_archived",
            actor,
            json!({
                "agent_id": agent_id,
                "collection_id": done.collection_id,
                "removed": grants_json(&done.removed),
            }),
        ),
        Ok(None) => {}
        Err(e) => {
            tracing::error!(agent = %agent_id, error = %e, "knowledge: own collection not archived");
            super::audit_admin(
                home,
                "knowledge_own_collection_archive_failed",
                actor,
                json!({"agent_id": agent_id, "error": e.public_message()}),
            );
        }
    }
}

/// Restore the employee's own collection when the employee is restored:
/// the grants archiving removed come back.
pub fn restore(home: &Path, agent_id: &str, actor: &str) {
    let result = schema::open_home(home).and_then(|mut conn| agent_own::restore(&mut conn, agent_id, actor));
    match result {
        Ok(Some(added)) => super::audit_admin(
            home,
            "knowledge_own_collection_restored",
            actor,
            json!({"agent_id": agent_id, "added": grants_json(&added)}),
        ),
        Ok(None) => {}
        Err(e) => {
            tracing::error!(agent = %agent_id, error = %e, "knowledge: own collection not restored");
            super::audit_admin(
                home,
                "knowledge_own_collection_restore_failed",
                actor,
                json!({"agent_id": agent_id, "error": e.public_message()}),
            );
        }
    }
}

/// At gateway start, line own collections up with their employees:
/// archive those of employees that are archived or deleted, or whose
/// folder is gone (removed by hand); restore archived ones whose employee
/// is in the registry and not off-boarded (archived or restored outside
/// the dashboard, or a restore that failed). An employee merely missing
/// from the registry while its folder exists (a settings file that does
/// not parse) is left alone.
pub fn reconcile(home: &Path, inactive: &[String], known: &[String]) {
    let rows = match schema::open_home(home).and_then(|conn| agent_own::list(&conn)) {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "knowledge: own collections not reconciled");
            return;
        }
    };
    for row in rows {
        let off = inactive.contains(&row.agent_id);
        let present = known.contains(&row.agent_id);
        let gone = !present && !home.join("agents").join(&row.agent_id).exists();
        match row.archived_at {
            None if off || gone => offboard(home, &row.agent_id, "system"),
            Some(_) if present && !off => restore(home, &row.agent_id, "system"),
            _ => {}
        }
    }
}

#[cfg(test)]
#[path = "agent_own_tests.rs"]
mod tests;
