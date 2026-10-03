//! Caller ↔ record relationship check for MCP tools that change or trigger a
//! record owned by an AI employee (task board rows and their activity, cron
//! rows, reminders).
//!
//! Before this module, `tasks_update` / `tasks_claim` / `tasks_complete` /
//! `tasks_block`, the cron management tools, `create_reminder` and
//! `activity_post` never asked "is the caller allowed to touch a record that
//! belongs to agent T?": any employee could rewrite another department's task,
//! pause or fire another employee's routine, or file a reminder that wakes
//! another employee up. The creation paths already ran the WP21 delegation
//! predicate; the update paths did not, so an update was a way to launder what
//! creation refused.
//!
//! One rule, shared by every handler ([`check_record_change_allowed`]):
//!
//! - an operator (an admin key that maps to no AI employee) → allowed;
//! - a caller whose identity is a system-sender name (`dashboard`, `cron`, …)
//!   → refused: no MCP process the gateway starts carries such an identity
//!   (every injection point stamps an agent directory id, and those names are
//!   reserved at creation), so only a self-asserted identity can present one;
//! - the caller IS the owning agent → allowed;
//! - otherwise the WP21 delegation predicate decides;
//! - an owner that cannot be determined (empty) or an empty caller → refused
//!   (fail closed).
//!
//! Every refusal is audited.

use super::*;

/// Who is asking to change a record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RecordActor<'a> {
    /// A human operator: an MCP key that maps to no AI employee, in a process
    /// that is not running for one. Never restricted by this module. The
    /// string is the identity the handler stamps on what it writes (the
    /// process's default agent, as before this module existed).
    Operator(&'a str),
    /// An AI employee, or a caller that cannot prove it is not one (the shared
    /// internal key with no `DUDUCLAW_AGENT_ID` acts as the internal client id,
    /// which owns nothing — fail closed).
    Agent(&'a str),
}

impl<'a> RecordActor<'a> {
    /// Classify an MCP caller, built on the memory tools' fail-closed
    /// classification ([`crate::mcp_memory_handlers::ai_employee_caller`],
    /// pinned to agree with it by `record_authz_tests`), with two additions:
    ///
    /// - the employee id is the process's resolved `default_agent` rather than
    ///   the raw `DUDUCLAW_AGENT_ID` text, so an identity `get_default_agent`
    ///   refused (strict-mode rejection, a system-sender name) stays refused;
    /// - a process started for an employee (`DUDUCLAW_AGENT_ID` set) is an
    ///   employee whatever key it holds, so a per-agent `.mcp.json` that
    ///   carries a shared non-internal key does not turn it into an operator.
    pub(crate) fn resolve(
        caller_client_id: &'a str,
        default_agent: &'a str,
        env_agent_id: Option<&str>,
        client_is_agent: bool,
    ) -> Self {
        let env_set = env_agent_id.is_some_and(|a| !a.trim().is_empty());
        let internal_id = duduclaw_gateway::mcp_internal_key::INTERNAL_CLIENT_ID;
        match crate::mcp_memory_handlers::ai_employee_caller(
            caller_client_id,
            env_agent_id,
            client_is_agent,
        ) {
            // Internal key: the employee the process runs for, or nobody.
            Some(id) if id == internal_id => RecordActor::Agent(internal_id),
            Some(id) if id == caller_client_id => RecordActor::Agent(caller_client_id),
            Some(_) => RecordActor::Agent(default_agent),
            None if env_set => RecordActor::Agent(default_agent),
            None => RecordActor::Operator(default_agent),
        }
    }

    /// The identity this caller acts as: what a handler checks against and
    /// stamps (`created_by`, `claimed_by`, activity rows, reminder owner).
    pub(crate) fn id(self) -> &'a str {
        match self {
            RecordActor::Operator(id) | RecordActor::Agent(id) => id,
        }
    }

    /// The acting employee id, `None` for an operator — the callers this
    /// module restricts.
    pub(crate) fn agent(self) -> Option<&'a str> {
        match self {
            RecordActor::Operator(_) => None,
            RecordActor::Agent(id) => Some(id),
        }
    }
}

/// Resolve the [`RecordActor`] of a live `tools/call` from the process
/// environment. Production-only (reads `DUDUCLAW_AGENT_ID`); tests build the
/// actor directly.
pub(crate) fn record_actor_for<'a>(
    home_dir: &Path,
    caller_client_id: &'a str,
    default_agent: &'a str,
) -> RecordActor<'a> {
    let env_agent = std::env::var(duduclaw_core::ENV_AGENT_ID).ok();
    let client_is_agent = crate::mcp_namespace::client_is_agent(home_dir, caller_client_id);
    RecordActor::resolve(caller_client_id, default_agent, env_agent.as_deref(), client_is_agent)
}

/// What kind of record is being changed — only shapes the refusal text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RecordKind {
    Task,
    Cron,
    Reminder,
}

impl RecordKind {
    fn label_zh(self) -> &'static str {
        match self {
            RecordKind::Task => "任務",
            RecordKind::Cron => "例行工作",
            RecordKind::Reminder => "提醒",
        }
    }

    fn no_owner_hint(self) -> &'static str {
        match self {
            RecordKind::Task => "先用 tasks_claim 認領這個任務，再做變更；或請操作者在儀表板處理。",
            RecordKind::Cron => "這筆例行工作沒有可辨識的執行者，請操作者在儀表板的「例行工作」頁處理。",
            RecordKind::Reminder => "請在 agent_id 填上要收到提醒的 AI 員工 id。",
        }
    }
}

/// Audit a refusal this module decides. Same row shape as the
/// `goal_contract_frozen` refusal in `tasks_update`.
pub(crate) fn audit_record_refused(
    home_dir: &Path,
    caller: &str,
    tool: &str,
    target: &str,
    reason: &str,
    extras: &[(&str, serde_json::Value)],
) {
    let mut all: Vec<(&str, serde_json::Value)> = vec![
        ("target", serde_json::json!(target)),
        ("reason", serde_json::json!(reason)),
    ];
    all.extend(extras.iter().cloned());
    duduclaw_security::audit::append_tool_call_with_extras(
        home_dir,
        caller,
        tool,
        &format!(
            "denied: '{}' -> '{}' ({reason})",
            duduclaw_core::truncate_chars(caller, 64),
            duduclaw_core::truncate_chars(target, 64)
        ),
        false,
        &all,
    );
}

/// Refuse an employee caller whose identity cannot act at all: empty, or a
/// system-sender name (only a self-asserted identity can carry one — see the
/// module doc). Operators pass.
pub(crate) fn check_actor_identity(
    home_dir: &Path,
    actor: RecordActor<'_>,
    target: &str,
    tool: &str,
) -> std::result::Result<(), String> {
    let Some(caller) = actor.agent() else {
        return Ok(());
    };
    let caller = caller.trim();
    if caller.is_empty() {
        audit_record_refused(home_dir, caller, tool, target, "caller_unknown", &[]);
        return Err(format!("{tool} 遭拒：無法確認呼叫者身分，未做任何變更。"));
    }
    if duduclaw_core::is_system_sender(caller) {
        audit_record_refused(home_dir, caller, tool, target, "system_sender_identity", &[]);
        return Err(format!(
            "{tool} 遭拒：「{}」是系統保留名稱，不能當作 AI 員工身分使用，未做任何變更。",
            duduclaw_core::truncate_chars(caller, 64)
        ));
    }
    Ok(())
}

/// May `actor` change (or trigger) a `kind` record that belongs to agent
/// `target`? `tool` names the MCP tool for the audit row (`path_kind` on a
/// `delegation_denied` row). `Err` carries the text for the tool error.
pub(crate) async fn check_record_change_allowed(
    home_dir: &Path,
    actor: RecordActor<'_>,
    target: &str,
    tool: &str,
    kind: RecordKind,
) -> std::result::Result<(), String> {
    let Some(caller) = actor.agent() else {
        return Ok(()); // operator
    };
    let target = target.trim();
    check_actor_identity(home_dir, actor, target, tool)?;
    let caller = caller.trim();
    let caller_short = duduclaw_core::truncate_chars(caller, 64);
    if target.is_empty() {
        audit_record_refused(home_dir, caller, tool, target, "owner_unknown", &[]);
        return Err(format!(
            "{tool} 遭拒：這筆{}沒有所屬的 AI 員工，無法確認「{caller_short}」可以變更它，未做任何變更。{}",
            kind.label_zh(),
            kind.no_owner_hint()
        ));
    }
    if caller == target {
        return Ok(());
    }
    // The predicate audits its own refusal as `delegation_denied`; its text
    // is written for an assigner and suggests org edits an AI employee cannot
    // make, so the reply here is phrased for this situation instead.
    check_delegation_allowed(home_dir, caller, target, tool)
        .await
        .map_err(|_| {
            let target_short = duduclaw_core::truncate_chars(target, 64);
            format!(
                "{tool} 遭拒：這筆{label}屬於「{target_short}」，而「{caller_short}」與「{target_short}」\
                 之間沒有委派關係（同部門、上下級或白名單配對），未做任何變更。\
                 可行處理：① 用 send_to_agent 請「{target_short}」自己處理（若你們之間允許傳訊）；\
                 ② 請你的主管或操作者代為調整。",
                label = kind.label_zh()
            )
        })
}

// ── Reserved task tags ───────────────────────────────────────────────────────

/// Tag prefixes on task board rows that carry control meaning, each matched
/// against the start of one whole trimmed tag (never a substring of the tag
/// string) — exactly how their readers match:
///
/// - `outcome:` — deterministic acceptance contract
///   (`duduclaw_gateway::outcome_spec::OutcomeSpec::from_tags`, which takes the
///   FIRST `outcome:` tag; read by `goal_loop/tick.rs`,
///   `dispatch_engine/review.rs`, `dispatch_engine/settle.rs`);
/// - `grant:` — task-scoped capability grants minted at goal kickoff
///   (`goal_loop/kickoff.rs::grant_kickoff_tools`).
pub(crate) const RESERVED_TASK_TAG_PREFIXES: &[&str] = &["outcome:", "grant:"];

/// Whole tags with control meaning:
///
/// - `auto-research` — the self-study daily de-duplication marker
///   (`duduclaw_gateway::self_study::AUTO_RESEARCH_TAG`): adding it suppresses
///   that agent's auto-research task for the day, removing it lets a duplicate
///   be created.
pub(crate) const RESERVED_TASK_TAGS: &[&str] = &["auto-research"];

/// Does this single tag carry control meaning?
pub(crate) fn is_reserved_task_tag(tag: &str) -> bool {
    let tag = tag.trim();
    RESERVED_TASK_TAG_PREFIXES.iter().any(|p| tag.starts_with(p))
        || RESERVED_TASK_TAGS.iter().any(|t| tag == *t)
}

/// The reserved tags in a stored comma-separated tag string, in order of
/// appearance and with duplicates kept: a reader that takes the first match
/// (`OutcomeSpec::from_tags`) changes behaviour when two reserved tags swap
/// places, so order is part of what must not change.
pub(crate) fn reserved_task_tags(tags: &str) -> Vec<String> {
    tags.split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty() && is_reserved_task_tag(t))
        .map(str::to_string)
        .collect()
}

/// Refuse (and audit) an employee caller passing reserved tags where it may
/// not set them. Operators pass.
pub(crate) fn check_no_reserved_tags(
    home_dir: &Path,
    actor: RecordActor<'_>,
    tool: &str,
    target: &str,
    tags: &str,
) -> std::result::Result<(), Value> {
    let Some(me) = actor.agent() else {
        return Ok(());
    };
    let reserved = reserved_task_tags(tags);
    if reserved.is_empty() {
        return Ok(());
    }
    audit_record_refused(
        home_dir,
        me,
        tool,
        target,
        "reserved_tag_change",
        &[("after", serde_json::json!(reserved))],
    );
    Err(tool_error(RESERVED_TAG_REFUSAL))
}

pub(crate) const RESERVED_TAG_REFUSAL: &str =
    "tags refused: tags starting with `outcome:` or `grant:`, and the `auto-research` tag, \
     control how a task is accepted and what it may do; an AI employee cannot add, remove or \
     reorder them. Leave those tags exactly as they are and change only the others.";
