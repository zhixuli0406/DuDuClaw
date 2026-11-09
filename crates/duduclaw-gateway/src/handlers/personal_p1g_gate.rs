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
//!   rows), `users.list` / `users.me` (binding metadata), approval cards
//!   (metadata cards), and the lifecycle authorities with their own F6 rules
//!   — P1h: an explicit frozen list ([`LIFECYCLE_ALLOWLIST`]), no prefix
//!   match; `topology.list` left the allowlist (its rows are filtered).
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

/// P1h (N3): the lifecycle authority methods exempt from the default deny
/// and the hidden-row filter — an explicit, frozen list (no prefix match), so
/// a new `isolation.*` / `migration.*` / `me.isolation.*` method is NOT
/// exempt until it is reviewed and added here. Each entry: why its answer may
/// name another person's Personal employee, or how it hides that data.
/// `p1h_lifecycle_allowlist_equals_the_dispatch_table` keeps this equal to
/// the methods the dispatch table really routes in those namespaces.
pub(crate) const LIFECYCLE_ALLOWLIST: &[(&str, &str)] = &[
    ("isolation.connector_list", "connector routes name an owner user id (Personal) or nothing (Shared), never an \
        employee; no per-employee rows"),
    ("isolation.connector_step_up", "Admin step-up proof; returns `verified` only"),
    ("isolation.connector_provision", "Admin + step-up connector authority; returns the new account id only"),
    ("isolation.connector_revoke", "Admin + step-up connector authority; returns account / active only"),
    ("isolation.connector_import_legacy", "stages migration proposals; returns counts only"),
    ("isolation.recording_status", "D8 host control; `control_recording` (boot_recording.rs, HX6 store) \
        withholds a non-owned Personal employee's recording result (status only); audited either way"),
    ("isolation.recording_stop", "D8 host stop, audited; the result is withheld as for recording_status"),
    ("isolation.employee_list", "lifecycle read model by design: label / owner / status, no private data"),
    ("isolation.employee_propose", "Admin + step-up proposal; creates a confirmation card"),
    ("isolation.employee_tombstone", "Admin + step-up lifecycle removal (F6 rules)"),
    ("isolation.employee_purge_requests", "Admin + step-up purge of request records; counts only"),
    ("me.isolation.request_personal_employee", "self-service: files the caller's own request"),
    ("me.isolation.list_personal_employee_requests", "the caller's own requests only"),
    ("me.isolation.list_personal_delegations", "the caller's own delegation receipts only (owner)"),
    ("me.isolation.decide_personal_delegation", "the owner decides its own delegation receipts"),
    // Merge P1h × F6 (reviewed 2026-11-09): the F6 restore / adoption /
    // purge authorities. None takes an `EMPLOYEE_PARAM_KEYS` parameter (they
    // name a row by `employee_id` + `incarnation` or `request_id`); each
    // re-checks its own F6 rule (owner + strongest factor, or Admin + MFA +
    // step-up). Their answers are lifecycle metadata — label, employee id,
    // owner / requester / approver user ids, state, label-keyed item names
    // (`<store>:<id>`) — never employee content. They must not be filtered:
    // a restored or adopted Personal employee is live (hence hidden) again
    // when the reply is built, so filtering would withhold a decision that
    // already committed.
    ("me.isolation.list_restorable", "owner view: the caller's own tombstoned Personal rows only (exact owner id)"),
    ("me.isolation.restore_challenge", "owner (own row only, else NOT_FOUND): a purpose-bound factor challenge and \
        the open request's id / kind / state"),
    ("me.isolation.request_restore", "owner + strongest factor files its own restore; request id / state / label"),
    ("me.isolation.cancel_restore", "the owner cancels its own request (factor, or password for an Admin-filed \
        one); request id / state only"),
    ("me.isolation.request_adoption", "owner + strongest factor files its own adoption (F6g); request metadata"),
    ("isolation.employee_restore_decide", "Admin + MFA + fresh step-up decision (F6 rules); reply: label, \
        incarnation, self_approved and the label-keyed item names listed for the Admin (§3.3), no content"),
    ("isolation.employee_restore_cancel", "Admin + MFA + step-up cancel; request id / state only"),
    ("isolation.employee_restore_regrant", "Admin + MFA + step-up re-grant of one listed item, audited; \
        item / state / reason only"),
    ("isolation.employee_retire_propose", "Admin + MFA + step-up retire proposal (F6l); staged state only"),
    ("isolation.shared_adoption_request", "Admin + MFA + step-up adoption into a Shared employee (F6g); request \
        metadata"),
    ("isolation.employee_restore_request_for_owner", "Admin + MFA + fresh step-up, only for an owner with no \
        second factor at the tombstone (F6fa2); the owner is notified; request metadata"),
    ("isolation.adoption_request_for_owner", "as employee_restore_request_for_owner, for an adoption (F6fa2)"),
    ("isolation.shared_restore_options", "Admin + MFA: a Shared quarantine row's members (user id / access level), \
        tenant routes and open request — no Personal data"),
    ("isolation.shared_restore_request", "Admin + MFA + step-up Shared restore request (F6k); request metadata"),
    ("isolation.quarantine_purge", "Admin + MFA + step-up quarantine cleanup (F6i); entry counts / state, \
        audited"),
    ("migration.confirmations.list", "Admin migration review cards (F6 rules)"),
    ("migration.confirmations.step_up", "Admin step-up proof; returns `verified` only"),
    ("migration.confirmations.approve", "Admin + step-up decision on a migration card (F6 rules)"),
    ("migration.confirmations.retry", "Admin + step-up retry of a migration card (F6 rules)"),
    ("migration.confirmations.reject", "Admin + step-up rejection of a migration card"),
    ("migration.confirmations.rotate_secret", "Admin + step-up secret rotation of a migration card"),
];

/// The explicit Admin management / metadata allowlist (P1g §2). Methods
/// here keep reaching another person's Personal employee; each audits it in
/// its handler or is metadata by design. P1h (N6): `topology.list` is no
/// longer on it — its from / to rows go through the hidden-row filter.
pub(crate) fn metadata_allowlisted(method: &str) -> bool {
    matches!(
        method,
        "agents.status" | "presets.status" | "agents.avatar" | "agents.pause" | "agents.resume"
            | "autonomy.pause" | "autonomy.resume" | "autonomy.status" | "agents.list"
            | "users.list" | "users.me" | "approvals.list" | "approvals.get"
    ) || LIFECYCLE_ALLOWLIST.iter().any(|(listed, _)| *listed == method)
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
