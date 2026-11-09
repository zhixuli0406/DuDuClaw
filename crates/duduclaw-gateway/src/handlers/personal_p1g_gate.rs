//! v1.81 P1g (owner decision 2026-11-09): the protected-mode default for
//! Personal employees is inverted — refused unless explicitly allowed.
//!
//! - **ACL.** `duduclaw_auth::acl` asks the installed restriction
//!   (`personal_visibility::AclRestriction`) before every predicate, so
//!   `has_agent_access` & co. return `false` for a Personal employee the
//!   caller does not own, Admins included. An audited Admin step-up of THIS
//!   request (`personal_data_gate`) admits that employee for the rest of the
//!   request only (`personal_visibility::record_break_glass`); delegation and
//!   other owner-only actions ignore it (`UserContext::owner_only_denied`).
//! - **Default deny at dispatch** ([`MethodHandler::personal_default_deny`]).
//!   Many Admin RPCs check only the role and never ask the ACL. After the
//!   P1–P1e gates, every request whose top-level employee parameters
//!   ([`EMPLOYEE_PARAM_KEYS`], [`EMPLOYEE_ARRAY_KEYS`]) name another
//!   person's Personal employee is refused, unless the method is on the
//!   explicit management / metadata allowlist ([`metadata_allowlisted`]) or
//!   this request's audited step-up admitted the employee. So every handler
//!   fails closed by default; there is no list of methods to keep current.
//! - **Hidden rows** ([`MethodHandler::personal_hide_rows`]). Unscoped
//!   listings drop another person's Personal employees from every response
//!   (array items whose employee field names one, object keys equal to one,
//!   bare names in arrays) — hidden, not redacted. The allowlisted metadata
//!   surfaces are exempt (they show label / owner / status by design).
//! - **Explicit allowlist** (each audited by its handler): `agents.status`,
//!   `presets.status`, `agents.avatar` (best-effort metadata audit),
//!   `agents.pause` / `agents.resume` / `autonomy.pause` / `autonomy.resume`
//!   (control audit, fail closed), `agents.list` (label / owner / status
//!   rows), `topology.list`, `users.list` / `users.me` (binding metadata),
//!   approval cards (metadata cards), and the lifecycle authorities
//!   (`isolation.*`, `migration.*`, `me.isolation.*`) with their own F6 rules.
use super::*;
use crate::personal_visibility as visibility;
use std::collections::BTreeSet;

/// Top-level request parameters that name one employee.
pub(crate) const EMPLOYEE_PARAM_KEYS: &[&str] = &[
    "agent_id", "agent", "agent_name", "from_agent", "to_agent", "assigned_to", "target_agent",
    "target_agent_id", "employee", "assignee", "owner_agent_id", "built_by_agent",
];
/// Top-level request parameters that name several employees.
pub(crate) const EMPLOYEE_ARRAY_KEYS: &[&str] = &["agent_ids", "agents"];
/// Fields of a response row that name its employee.
pub(crate) const ROW_EMPLOYEE_FIELDS: &[&str] = &[
    "agent_id", "agent", "agent_name", "from_agent", "to_agent", "assigned_to", "target_agent",
    "target_agent_id", "employee", "assignee", "owner_agent_id", "built_by_agent", "sender_agent",
    "worker", "actor_agent",
];

/// Shown to an Admin whose request names another person's Personal
/// employee outside the allowlist and without an audited step-up.
pub(crate) const PERSONAL_DEFAULT_DENIED: &str = "這位 AI 員工是某位同仁的個人專屬員工。除了名稱、狀態、暫停／恢復等管理動作，\
    其他操作只能由它的擁有者進行；私人資料與設定需管理員重新驗證身分（step-up）並留下稽核紀錄，交辦工作則僅限擁有者。 / This is \
    a person's Personal employee. Apart from name / status metadata and pause / resume, only its owner may act on it; \
    an Admin needs an audited step-up for its private data or configuration, and only the owner may hand it work.";

/// Shown when the raw log tail is refused in protected mode.
pub(crate) const LOG_TAIL_REFUSED: &str = "受保護模式下，有其他同仁的個人專屬員工時，即時系統紀錄無法逐列過濾，因此不提供。 / In \
    protected mode the live log tail cannot be filtered per row, so it is not available while another person's \
    Personal employees exist.";

/// The explicit Admin management / metadata allowlist (P1g §2). Methods
/// here keep reaching another person's Personal employee; each audits it in
/// its handler or is metadata by design.
pub(crate) fn metadata_allowlisted(method: &str) -> bool {
    matches!(
        method,
        "agents.status" | "presets.status" | "agents.avatar" | "agents.pause" | "agents.resume"
            | "autonomy.pause" | "autonomy.resume" | "autonomy.status" | "agents.list" | "topology.list"
            | "users.list" | "users.me" | "approvals.list" | "approvals.get"
    ) || method.starts_with("isolation.")
        || method.starts_with("migration.")
        || method.starts_with("me.isolation.")
}

/// Every employee the request's top-level parameters name (trimmed, so a
/// padded id is judged as the employee a handler would trim it to).
pub(crate) fn named_employees(params: &Value) -> BTreeSet<String> {
    let mut named = BTreeSet::new();
    let mut push = |raw: &str| {
        let agent = raw.trim();
        if !agent.is_empty() {
            named.insert(agent.to_owned());
        }
    };
    for key in EMPLOYEE_PARAM_KEYS {
        if let Some(raw) = params.get(*key).and_then(Value::as_str) {
            push(raw);
        }
    }
    for key in EMPLOYEE_ARRAY_KEYS {
        for raw in params.get(*key).and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
            push(raw);
        }
    }
    named
}

/// The first named employee in `hidden`, if any (pure; see
/// [`MethodHandler::personal_default_deny`]).
pub(crate) fn default_deny_target(method: &str, params: &Value, hidden: &BTreeSet<String>) -> Option<String> {
    if metadata_allowlisted(method) || hidden.is_empty() {
        return None;
    }
    named_employees(params).into_iter().find(|agent| hidden.contains(agent))
}

/// `true` when `value` — at any depth — has an object whose
/// [`ROW_EMPLOYEE_FIELDS`] name a hidden employee.
pub(crate) fn names_hidden(value: &Value, hidden: &BTreeSet<String>) -> bool {
    match value {
        Value::Object(map) => {
            ROW_EMPLOYEE_FIELDS
                .iter()
                .any(|field| map.get(*field).and_then(Value::as_str).is_some_and(|agent| hidden.contains(agent)))
                || map.values().any(|item| names_hidden(item, hidden))
        }
        Value::Array(items) => items.iter().any(|item| names_hidden(item, hidden)),
        _ => false,
    }
}

/// Drop every trace of `hidden` employees from a response payload: array
/// items that name one anywhere inside (a row and everything in it), bare
/// names in arrays, and object keys equal to one; recursively. Returns how
/// many entries were removed. The payload root itself is never removed —
/// [`MethodHandler::personal_hide_rows`] withholds a payload whose root
/// still names one ([`names_hidden`]).
pub(crate) fn drop_hidden_rows(value: &mut Value, hidden: &BTreeSet<String>) -> usize {
    if hidden.is_empty() {
        return 0;
    }
    let mut removed = 0;
    match value {
        Value::Array(items) => {
            let before = items.len();
            items.retain(|item| match item {
                Value::String(name) => !hidden.contains(name),
                other => !names_hidden(other, hidden),
            });
            removed += before - items.len();
            for item in items.iter_mut() {
                removed += drop_hidden_rows(item, hidden);
            }
        }
        Value::Object(map) => {
            let before = map.len();
            map.retain(|key, _| !hidden.contains(key));
            removed += before - map.len();
            for (_, item) in map.iter_mut() {
                removed += drop_hidden_rows(item, hidden);
            }
        }
        _ => {}
    }
    removed
}

impl MethodHandler {
    /// Personal employees hidden from `ctx` in THIS request: the users.db
    /// set minus the employees an audited step-up of this request admitted.
    async fn p1g_hidden_now(&self, ctx: &UserContext) -> Result<BTreeSet<String>, &'static str> {
        let mut hidden = self.personal_hidden_agents(ctx).await?;
        hidden.retain(|agent| !visibility::break_glass_granted(&ctx.user_id, agent));
        Ok(hidden)
    }

    /// P1g default deny (dispatch, after the P1–P1e gates). Protected mode
    /// only. Unreadable ownership refuses.
    pub(super) async fn personal_default_deny(&self, method: &str, params: &Value, ctx: &UserContext)
        -> Result<(), WsFrame> {
        if !crate::runtime_readiness::is_protected_linux() || metadata_allowlisted(method) {
            return Ok(());
        }
        if named_employees(params).is_empty() {
            return Ok(());
        }
        let hidden = self.p1g_hidden_now(ctx).await.map_err(|message| WsFrame::error_response("", message))?;
        match default_deny_target(method, params, &hidden) {
            None => Ok(()),
            // A non-Admin cannot tell another person's Personal employee from
            // an unknown one (P1d).
            Some(_) if !ctx.is_admin() => Err(WsFrame::error_response("", super::personal_data_gate::NOT_ALLOWED)),
            Some(_) => Err(WsFrame::error_response("", PERSONAL_DEFAULT_DENIED)),
        }
    }

    /// P1g: drop another person's Personal employees from a response
    /// (unscoped listings hide them). Protected mode only; the metadata
    /// allowlist is exempt; error frames pass unchanged. If ownership cannot
    /// be read, a successful payload is withheld (fail closed).
    pub(super) async fn personal_hide_rows(&self, method: &str, ctx: &UserContext, frame: WsFrame) -> WsFrame {
        if !crate::runtime_readiness::is_protected_linux() || metadata_allowlisted(method) {
            return frame;
        }
        let (id, mut payload, error) = match frame {
            WsFrame::Response { id, ok: true, payload: Some(payload), error } => (id, payload, error),
            other => return other,
        };
        match self.p1g_hidden_now(ctx).await {
            Ok(hidden) => {
                drop_hidden_rows(&mut payload, &hidden);
                // A single-object answer about another person's Personal
                // employee (e.g. `channel_ingress.inspect`'s event) that no
                // gate admitted is withheld, not trimmed.
                if names_hidden(&payload, &hidden) {
                    let text = if ctx.is_admin() { PERSONAL_DEFAULT_DENIED } else { super::personal_data_gate::NOT_ALLOWED };
                    return WsFrame::error_response(&id, text);
                }
                WsFrame::Response { id, ok: true, payload: Some(payload), error }
            }
            Err(message) => WsFrame::error_response(&id, message),
        }
    }

    /// P1g (a*): `logs.subscribe` in protected mode only for a caller with no
    /// hidden Personal employee (the raw tail cannot be filtered per row).
    pub(super) async fn p1g_log_tail_allowed(&self, ctx: &UserContext) -> Result<(), WsFrame> {
        if !crate::runtime_readiness::is_protected_linux() {
            return Ok(());
        }
        match self.personal_hidden_agents(ctx).await {
            Ok(hidden) if hidden.is_empty() => Ok(()),
            Ok(_) => Err(WsFrame::error_response("", LOG_TAIL_REFUSED)),
            Err(message) => Err(WsFrame::error_response("", message)),
        }
    }

    /// P1g: the Admin path of `cron.*` (which goes straight to the
    /// unscoped handlers) — every employee the request names and the
    /// employee of the row it acts on (by `id`, or legacy `name`) must not be
    /// another person's Personal employee (a routine makes it run work:
    /// owner only, no break-glass).
    pub(super) async fn p1g_admin_cron_guard(&self, method: &str, params: &Value, ctx: &UserContext)
        -> Result<(), WsFrame> {
        if !crate::runtime_readiness::is_protected_linux() || matches!(method, "cron.list" | "cron.templates") {
            return Ok(());
        }
        let mut agents = named_employees(params);
        let store = self.cron_store().await?;
        let id = params.get("id").and_then(Value::as_str).unwrap_or("");
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        let row = if id.is_empty() {
            None
        } else {
            store.get(id).await.map_err(|_| WsFrame::error_response("", visibility::OWNERSHIP_UNAVAILABLE))?
        };
        match row {
            Some(row) => {
                agents.insert(row.agent_id);
            }
            None if !name.is_empty() && method != "cron.add" => {
                let rows = store.list_all().await.map_err(|_| WsFrame::error_response("", visibility::OWNERSHIP_UNAVAILABLE))?;
                agents.extend(rows.into_iter().filter(|row| row.name == name).map(|row| row.agent_id));
            }
            None => {}
        }
        if agents.iter().any(|agent| ctx.owner_only_denied(agent)) {
            return Err(WsFrame::error_response("", PERSONAL_DEFAULT_DENIED));
        }
        Ok(())
    }
}

#[cfg(all(test, unix))]
#[path = "personal_p1g_tests.rs"]
mod tests;
