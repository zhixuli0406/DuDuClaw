//! Document knowledge base — the gateway side.
//!
//! `duduclaw-knowledge` holds storage, ACL and retrieval; this module owns
//! everything that needs the live gateway:
//!
//! - [`worker`]: the ingest queue (lock-holding gateway only) running
//!   [`pipeline`] jobs, which extract in a child process ([`extract_spawn`]).
//! - [`rpc`]: the `knowledge.*` dashboard RPCs (routed from
//!   `handlers/dispatch_kb.rs`).
//! - [`upload`]: `POST /api/knowledge/upload` and the original-file download.
//! - [`route`]: `POST /api/internal/knowledge`, the signed loopback route
//!   behind the read-only `knowledge_*` MCP tools.
//! - [`turn_requesters`]: who a live turn is for; [`requester_map`] maps a
//!   channel reply turn to that person.
//! - [`ledger`]: one citation numbering per live turn.
//! - [`auto_retrieve`]: passages and rules for a channel reply's prompt.
//! - [`cite_turn`]: citation parsing, `turn_citations` and per-channel
//!   source footers after the reply.
//! - [`add_source_approval`], [`chat_add`]: proposals to add a source (MCP
//!   tool, chat command), always decided in the dashboard.
//! - [`forget`]: forget-by-source plans for turns that cited a source.
//! - [`embedding`]: the embedder in effect (`[knowledge.embedding]`), embed
//!   job scheduling; [`embed_job`] runs one; [`embedding_rpc`] is
//!   `knowledge.embedding.*`; [`redaction_adapter`] de-identifies text
//!   before a cloud embedding call.
//! - [`principals`]: who a collection owner may grant to (the permission
//!   picker and the check before `knowledge.acl.set`).
//! - [`agent_binding`]: `agent.toml [knowledge] collections` from the
//!   employee edit page (`agents.update`).
//! - [`denied`]: the admin-wide list of refused reads.
//! - [`upload_limits`]: `knowledge.limits`, the upload limits the dashboard
//!   checks before sending a file.
//!
//! Writes happen only here. Every read goes through
//! `duduclaw_knowledge::acl::effective_collections`.

pub mod add_source_approval;
pub mod add_source_guard;
pub mod agent_binding;
pub mod auto_retrieve;
pub mod chat_add;
pub mod chat_attachments;
pub mod cite_turn;
pub mod denied;
pub mod embed_job;
pub mod embedding;
pub mod embedding_rpc;
pub mod embedding_select;
pub mod extract_spawn;
pub mod forget;
pub mod ledger;
pub mod pipeline;
pub mod principals;
pub mod redaction_adapter;
pub mod requester_map;
pub mod route;
mod route_add;
pub mod rpc;
pub mod rpc_channels;
pub mod turn_requesters;
#[cfg(test)]
pub(crate) mod test_fixtures;
pub mod upload;
pub mod upload_limits;
pub mod url_ingest;
pub mod worker;

use std::path::Path;
use std::sync::Arc;

use duduclaw_auth::UserContext;
use duduclaw_auth::db::UserDb;
use duduclaw_knowledge::acl::HumanCtx;
use duduclaw_knowledge::requester::{ResolvedUser, UserDirectory};

/// Account lookups against the dashboard user database. Only active
/// accounts resolve.
pub struct UserDbDirectory(pub Arc<UserDb>);

impl UserDirectory for UserDbDirectory {
    fn lookup(&self, user_id: &str) -> Result<Option<ResolvedUser>, String> {
        let user = self.0.get_user(user_id)?;
        Ok(user
            .filter(|u| u.status == duduclaw_auth::models::UserStatus::Active)
            .map(|u| ResolvedUser {
                is_admin: u.role == duduclaw_auth::models::UserRole::Admin,
                department: u.department.clone(),
                user_id: u.id,
            }))
    }
}

/// The account directory for `home`: `users` when the caller already holds
/// the database, else `users.db` opened read-through (never created). No
/// database, or one that cannot be opened, knows nobody (fail closed).
pub fn user_directory(
    home: &Path,
    users: Option<&Arc<UserDb>>,
) -> Box<dyn UserDirectory> {
    match users {
        Some(db) => Box::new(UserDbDirectory(db.clone())),
        None => match crate::decision_notify::open_user_db(home) {
            Ok(Some(db)) => Box::new(UserDbDirectory(Arc::new(db))),
            _ => Box::new(NoUsers),
        },
    }
}

/// A directory that knows nobody (no user database).
pub struct NoUsers;

impl UserDirectory for NoUsers {
    fn lookup(&self, _user_id: &str) -> Result<Option<ResolvedUser>, String> {
        Ok(None)
    }
}

/// The dashboard caller as a resolved user: role from the authenticated
/// context, department from the user database (none for the legacy
/// token / local `system` principal).
pub fn dashboard_user(ctx: &UserContext, users: Option<&Arc<UserDb>>) -> ResolvedUser {
    let department = users
        .and_then(|db| db.get_user(&ctx.user_id).ok().flatten())
        .and_then(|u| u.department)
        .filter(|d| duduclaw_core::department::is_valid_department(d));
    ResolvedUser {
        user_id: ctx.user_id.clone(),
        department,
        is_admin: ctx.is_admin(),
    }
}

/// Human side for a dashboard caller.
pub fn dashboard_human(user: &ResolvedUser) -> HumanCtx {
    HumanCtx::dashboard(&user.user_id, user.department.as_deref(), user.is_admin)
}

/// Whether the knowledge base is switched on for this home.
pub fn enabled(home: &Path) -> bool {
    duduclaw_knowledge::limits::KnowledgeConfig::from_home(home).enabled
}

/// Run blocking knowledge work off the async runtime.
pub(crate) async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> duduclaw_knowledge::KbResult<T> + Send + 'static,
) -> duduclaw_knowledge::KbResult<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| duduclaw_knowledge::KbError::Io(std::io::Error::other(e.to_string())))?
}

/// Record a management action in `security_audit.jsonl`.
pub(crate) fn audit_admin(home: &Path, event: &str, actor: &str, details: serde_json::Value) {
    let ev = duduclaw_security::audit::AuditEvent::new(
        event,
        "",
        duduclaw_security::audit::Severity::Info,
        serde_json::json!({ "actor": actor, "details": details }),
    );
    duduclaw_security::audit::append_audit_event(home, &ev);
}
