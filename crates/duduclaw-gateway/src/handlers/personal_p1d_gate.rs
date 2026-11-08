//! Dashboard reads of a Personal employee's data named by id or listed
//! without a scope (native Linux isolation v1.81, task P1d). Protected mode
//! only; see `crate::personal_visibility` for the model.
//!
//! - **By id** ([`id_scoped`]). `plans.get` / `update` / `remove` /
//!   `add_step` (`plan_id`), `plans.update_step` / `remove_step`
//!   (`step_id` → its plan), `mail.read` / `mail.archive` (`mail_id`),
//!   `agent_files.pending_get` (`approval_id` → the card's employee) and
//!   `fork.inspect` / `fork.resolve` (`fork_id`) resolve the employee the
//!   item belongs to before the handler runs, then go through the P1 gate
//!   (`personal_data_gate`): the owner passes, an Admin who is not the owner
//!   needs the audited step-up, everybody else is refused. An unknown id is
//!   left to the handler; an unreadable store refuses. A non-Admin refused
//!   gets the handler's own not-found text ([`IdScope::not_found`]), so an
//!   id of another person's Personal employee reads like an unknown one.
//! - **Optional-scope lists** ([`MethodHandler::p1d_list_hidden`]).
//!   `plans.list`, `mail.list` and `mail.outbox` drop rows of Personal
//!   employees the caller does not own; scoped to one, an Admin needs the
//!   step-up (audited) and a non-Admin simply gets no rows (as for an unknown
//!   employee). `fork.list` drops the same rows in its handler.
//! - **Server-picked runners.** `tasks.skill_assign` never picks a Personal
//!   employee the caller does not own ([`retain_visible`]).
use super::*;
use super::personal_data_gate::{NOT_ALLOWED, STEP_UP_FIELD, Target, param_text};
use crate::personal_visibility::{self as visibility, DataCategory, Unavailable};
use std::collections::BTreeSet;

const UNAVAILABLE: &str = "無法確認這位員工的歸屬，已拒絕讀取。";

/// What an id parameter names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum IdKind {
    /// `tasks.db` `plans.id`.
    Plan,
    /// `tasks.db` `plan_steps.id` → its plan.
    PlanStep,
    /// An Agent Mail inbox item.
    Mail,
    /// An approval card (`approvals.db`) → `personal_decision::card_employee`.
    Approval,
    /// A Live Run Fork (`fork_store.db`).
    Fork,
}

/// One id-keyed method: the parameter, what it names, the data category for
/// the audit row and the text a non-Admin gets for an unknown id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct IdScope {
    pub key: &'static str,
    pub kind: IdKind,
    pub category: DataCategory,
    pub not_found: &'static str,
}

/// Dashboard methods that name one employee's item by id.
pub(super) fn id_scoped(method: &str) -> Option<IdScope> {
    let (key, kind, category, not_found) = match method {
        "plans.get" | "plans.update" | "plans.remove" | "plans.add_step" => {
            ("plan_id", IdKind::Plan, DataCategory::Work, NOT_ALLOWED)
        }
        "plans.update_step" | "plans.remove_step" => ("step_id", IdKind::PlanStep, DataCategory::Work, NOT_ALLOWED),
        "mail.read" | "mail.archive" => ("mail_id", IdKind::Mail, DataCategory::Mail, "mail not found"),
        "agent_files.pending_get" => ("approval_id", IdKind::Approval, DataCategory::AgentFiles, NOT_ALLOWED),
        "fork.inspect" | "fork.resolve" => ("fork_id", IdKind::Fork, DataCategory::Work, NOT_ALLOWED),
        _ => return None,
    };
    Some(IdScope { key, kind, category, not_found })
}

/// The employee the item `id` belongs to. `Ok(None)` = no such item (the
/// handler answers); `Err` = its store could not be read (refuse).
pub(super) fn id_employee(home: &Path, kind: IdKind, id: &str) -> Result<Option<String>, Unavailable> {
    use rusqlite::OptionalExtension;
    match kind {
        IdKind::Plan => visibility::work_item_employee(home, visibility::WorkItem::Plan, id),
        IdKind::PlanStep => {
            let conn = crate::review_evidence::audience::readonly_db(&home.join("tasks.db")).map_err(|_| Unavailable)?;
            conn.query_row(
                "SELECT p.agent_id FROM plan_steps s JOIN plans p ON p.id = s.plan_id WHERE s.id = ?1",
                [id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|_| Unavailable)
        }
        IdKind::Mail => Ok(crate::mail::get_inbox_item(home, id).map(|item| item.agent_id)),
        IdKind::Approval => {
            if !home.join("approvals.db").exists() {
                return Ok(None);
            }
            let record = crate::approval::ApprovalStore::get_readonly(home, &crate::approval::ApprovalId::from(id.to_owned()))
                .map_err(|_| Unavailable)?;
            match record {
                Some(record) => crate::personal_decision::card_employee(home, &record),
                None => Ok(None),
            }
        }
        IdKind::Fork => {
            let path = home.join("fork_store.db");
            if !path.exists() {
                return Ok(None);
            }
            let store = duduclaw_fork::ForkStore::open(&path).map_err(|_| Unavailable)?;
            store.get_fork(id).map(|fork| fork.map(|fork| fork.agent_id)).map_err(|_| Unavailable)
        }
    }
}

/// The text a refused caller sees: a non-Admin gets the handler's text for
/// an unknown id; an Admin (who sees metadata) the reason.
pub(super) fn id_refusal_text(is_admin: bool, scope: IdScope, refusal: &'static str) -> &'static str {
    if !is_admin && refusal == NOT_ALLOWED { scope.not_found } else { refusal }
}

/// Keep only the rows whose employee is not hidden from the caller.
pub(super) fn retain_visible<T>(rows: &mut Vec<T>, hidden: &BTreeSet<String>, employee: impl Fn(&T) -> &str) {
    if !hidden.is_empty() {
        rows.retain(|row| !hidden.contains(employee(row)));
    }
}

impl MethodHandler {
    /// [`id_scoped`] gate for one request (protected mode; called from
    /// `personal_gates_for_params`). `csrf` is the request's step-up token.
    pub(super) async fn personal_id_gate(
        &self,
        ctx: &UserContext,
        method: &str,
        scope: IdScope,
        params: &Value,
        csrf: Option<[u8; 32]>,
    ) -> Result<(), WsFrame> {
        let refuse = |message: &str| WsFrame::error_response("", message);
        let Some(id) = param_text(params, scope.key) else {
            return Ok(());
        };
        let home = self.home_dir.clone();
        let employee = tokio::task::spawn_blocking(move || id_employee(&home, scope.kind, &id))
            .await
            .map_err(|_| refuse(UNAVAILABLE))?
            .map_err(|_| refuse(UNAVAILABLE))?;
        let Some(agent) = employee else {
            return Ok(());
        };
        let admin = ctx.is_admin();
        self.personal_data_gate(ctx, Target { method, category: scope.category, agent: &agent }, csrf)
            .await
            .map_err(|message| refuse(id_refusal_text(admin, scope, message)))
    }

    /// Employees an optional-scope listing (`plans.list`, `mail.list`,
    /// `mail.outbox`) must leave out for `ctx`: Personal employees it does
    /// not own. A scope on one of them is admitted for an Admin only through
    /// the audited step-up (`step_up_csrf` in the request); for anyone else
    /// it stays hidden, so the list is empty exactly as for an unknown
    /// employee. Empty outside protected mode; unreadable ⇒ refused.
    pub(super) async fn p1d_list_hidden(
        &self,
        ctx: &UserContext,
        method: &str,
        category: DataCategory,
        params: &Value,
    ) -> Result<BTreeSet<String>, WsFrame> {
        let refuse = |message: &str| WsFrame::error_response("", message);
        let mut hidden = self.personal_hidden_agents(ctx).await.map_err(refuse)?;
        let Some(scope) = param_text(params, "agent_id").filter(|scope| hidden.contains(scope)) else {
            return Ok(hidden);
        };
        if !ctx.is_admin() {
            return Ok(hidden);
        }
        let csrf = params.get(STEP_UP_FIELD).and_then(|raw| serde_json::from_value::<[u8; 32]>(raw.clone()).ok());
        self.personal_data_gate(ctx, Target { method, category, agent: &scope }, csrf).await.map_err(refuse)?;
        hidden.remove(&scope);
        Ok(hidden)
    }
}

#[cfg(all(test, unix))]
#[path = "personal_p1d_tests.rs"]
mod tests;
