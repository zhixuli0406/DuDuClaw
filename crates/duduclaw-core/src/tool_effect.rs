//! Tool effect classes, per-employee action rules and the read-only explore
//! lane.
//!
//! ## Why classify by effect
//!
//! Until 2026-10 every per-employee tool gate was keyed by tool NAME
//! (`[capabilities] approval_required_tools` / `irreversible_tools` /
//! `maybe_irreversible_tools` / `denied_tools` / `allowed_tools`). An operator
//! who wanted "ask me before anything leaves the building" had to enumerate
//! every sending tool by hand and keep the list current as tools were added.
//! [`ToolEffect`] gives each DuDuClaw MCP tool one side-effect class, and
//! [`ActionRules`] lets `agent.toml [capabilities] action_rules` say
//! `allow` / `ask` / `block` per class (or per tool).
//!
//! ## What a rule can and cannot do
//!
//! Rules only ever NARROW. `ask` adds an ApprovalBroker request, `block`
//! refuses; `allow` is the absence of friction from this layer and never
//! removes an approval or a denial that the name lists impose (the dispatch
//! gate combines the two layers take-the-stricter).
//!
//! ## Classification table
//!
//! [`effect_of_builtin`] classifies every DuDuClaw MCP tool by exact name
//! (coding convention 2: never a prefix or substring test). A tool whose
//! effect depends on its arguments (e.g. `wiki_write` with `scope`) carries
//! the stricter class. A name the table does not know resolves to
//! [`ToolEffect::Admin`] through [`effect_of`] (fail closed); `duduclaw-cli`
//! carries a test that walks every advertised `ToolDef` and requires an
//! explicit entry, so a new tool cannot ship unclassified.

use std::fmt;
use std::str::FromStr;

use serde::de::IgnoredAny;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The side effect one tool call has on the world.
///
/// There is no strictness order between classes: an operator writes one
/// verdict per class. [`Self::is_side_effecting`] separates the two classes
/// that leave nothing behind outside the reply (`read`, `draft`) from the
/// rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolEffect {
    /// Reads or computes; changes nothing.
    Read,
    /// Produces something for a person to review (a draft, a canvas, a
    /// staged proposal) without delivering it anywhere.
    Draft,
    /// Delivers a message to a person or another employee (chat, mail,
    /// desktop notification, delegation, calendar invitation).
    Send,
    /// Makes content visible beyond its author (shared wiki, public
    /// comment, shared skill pool).
    Publish,
    /// Commits money or a commercial obligation.
    Purchase,
    /// Removes or retires a record.
    Delete,
    /// Changes stored state the employee or the business relies on.
    Modify,
    /// Changes who can do what, installs capabilities, or operates the host.
    Admin,
}

impl ToolEffect {
    /// Every class, in display order.
    pub const ALL: [ToolEffect; 8] = [
        ToolEffect::Read,
        ToolEffect::Draft,
        ToolEffect::Send,
        ToolEffect::Publish,
        ToolEffect::Purchase,
        ToolEffect::Delete,
        ToolEffect::Modify,
        ToolEffect::Admin,
    ];

    /// The lowercase token used in `agent.toml` and on the wire.
    pub fn as_str(&self) -> &'static str {
        match self {
            ToolEffect::Read => "read",
            ToolEffect::Draft => "draft",
            ToolEffect::Send => "send",
            ToolEffect::Publish => "publish",
            ToolEffect::Purchase => "purchase",
            ToolEffect::Delete => "delete",
            ToolEffect::Modify => "modify",
            ToolEffect::Admin => "admin",
        }
    }

    /// Anything other than `read` and `draft`.
    pub fn is_side_effecting(&self) -> bool {
        !matches!(self, ToolEffect::Read | ToolEffect::Draft)
    }
}

impl fmt::Display for ToolEffect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ToolEffect {
    type Err = String;

    /// Exact token, ASCII case-insensitive after trimming.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let t = s.trim();
        ToolEffect::ALL
            .into_iter()
            .find(|e| e.as_str().eq_ignore_ascii_case(t))
            .ok_or_else(|| format!("unknown tool effect `{t}`"))
    }
}

/// The class of a DuDuClaw MCP tool, or `None` when the table has no entry.
///
/// Exact names only. Review notes for the less obvious rows:
/// - `mail_send` only files a draft a person must approve, but the effect it
///   leads to is a send, so it is `send`; `gmail_create_draft` never sends
///   and is `draft`.
/// - `wiki_write` is `modify` for both scopes; `wiki_share` (copies a page
///   into the shared wiki) is `publish`.
/// - `memory_invalidate_by_origin` expires rather than deletes, but it takes
///   facts out of use in bulk, so it is `delete`.
/// - `odoo_sale_confirm` turns a quotation into a binding order (`purchase`);
///   `odoo_execute` can call any model method and is `admin`.
/// - `execute_program` runs code in an offline sandbox; it changes nothing
///   outside the container but is arbitrary execution, so it is `admin`.
/// - `send_to_agent`, `spawn_agent`, `team_handoff` hand work to another
///   employee (`send`); `create_agent`, `spawn_ephemeral` create one (`admin`).
/// - `calendar_create_event` notifies attendees (`send`).
/// - `os_notify` shows a desktop notification to a person (`send`).
pub fn effect_of_builtin(tool_name: &str) -> Option<ToolEffect> {
    use ToolEffect::*;
    let effect = match tool_name {
        // ── read ────────────────────────────────────────────────────────
        "activity_list" | "agent_status" | "audit_trail_query" | "autopilot_list"
        | "belief_stats" | "channel_config_list" | "channel_status" | "check_responses"
        | "code_map" | "codrive_status" | "computer_screenshot" | "computer_workspace_list"
        | "computer_workspace_read" | "cost_agents" | "cost_multi_vs_single" | "cost_recent"
        | "cost_summary" | "cost_users" | "csv_read" | "db_query" | "db_select" | "db_sources"
        | "db_tables" | "decision_list" | "diff_branches" | "docs_read" | "drive_read"
        | "drive_search" | "evolution_status" | "file_read" | "fork_cost" | "forms_get"
        | "forms_list_responses" | "github_issue_read" | "github_pr_read"
        | "github_search_issues" | "github_status" | "gmail_read" | "gmail_search"
        | "goals_list" | "google_status" | "gtasks_list" | "gtasks_lists" | "hardware_info"
        | "identity_resolve" | "inference_mode" | "inference_status" | "inspect_branches"
        | "list_agents" | "list_cron_tasks" | "list_reminders" | "llamafile_list" | "mail_list"
        | "mail_read" | "memory_alias_list" | "memory_consolidation_status"
        | "memory_episodic_pressure" | "memory_fetch_batch" | "memory_get_at"
        | "memory_get_history" | "memory_improve" | "memory_read" | "memory_search"
        | "memory_search_by_layer" | "memory_successful_conversations" | "model_list"
        | "model_recommend" | "model_search" | "notion_page_read" | "notion_search"
        | "notion_status" | "odoo_connect" | "odoo_crm_leads" | "odoo_inventory_check"
        | "odoo_inventory_products" | "odoo_invoice_list" | "odoo_partner_search"
        | "odoo_payment_status" | "odoo_report" | "odoo_sale_orders" | "odoo_schema_fields"
        | "odoo_search" | "odoo_status" | "os_audio_get" | "os_backup_list"
        | "os_boot_assessment" | "os_calendar_today" | "os_check_update" | "os_device_status"
        | "os_display_get" | "os_doctor_repair" | "os_frontmost" | "os_network_info"
        | "os_spotlight_search" | "os_system_status" | "os_watch_status" | "os_wifi_scan"
        | "os_wifi_status" | "plan_get" | "reliability_summary" | "responsibility_get"
        | "route_query" | "session_restore_context" | "shared_skill_list" | "sheets_read"
        | "skill_gaps" | "skill_list" | "skill_search" | "skill_security_scan"
        | "skill_synthesis_status" | "slides_read" | "task_status" | "tasks_list"
        | "transcribe_audio" | "user_code_profile" | "user_profile_get" | "web_extract"
        | "web_fetch_cached" | "web_search" | "wiki_dedup" | "wiki_graph" | "wiki_lint"
        | "wiki_ls" | "wiki_namespace_status" | "wiki_read" | "wiki_search" | "wiki_stats"
        | "wiki_trust_audit" | "wiki_trust_history" | "working_state_get" | "xlsx_read"
        | "calendar_list_events" | "discovery_catalog" | "discovery_list" | "discovery_tree"
        | "discovery_artifact" => Read,

        // ── draft ───────────────────────────────────────────────────────
        "gmail_create_draft" | "canvas_push" | "plan_start" | "skill_from_recording"
        | "synthesize_speech" => Draft,

        // ── send ────────────────────────────────────────────────────────
        "send_message" | "send_photo" | "send_sticker" | "mail_send" | "send_to_agent"
        | "spawn_agent" | "team_handoff" | "create_reminder" | "responsibility_ask"
        | "calendar_create_event" | "os_notify" => Send,

        // ── publish ─────────────────────────────────────────────────────
        "wiki_share" | "github_issue_comment" | "shared_skill_share" => Publish,

        // ── purchase ────────────────────────────────────────────────────
        "odoo_sale_confirm" => Purchase,

        // ── delete ──────────────────────────────────────────────────────
        "agent_remove" | "delete_cron_task" | "cancel_reminder" | "shared_wiki_delete"
        | "memory_invalidate_by_origin" => Delete,

        // ── modify ──────────────────────────────────────────────────────
        "activity_post" | "belief_settle" | "belief_submit" | "browser_record_start"
        | "browser_record_stop" | "canvas_clear" | "computer_click" | "computer_key"
        | "computer_navigate" | "computer_scroll" | "computer_session_start"
        | "computer_session_stop" | "computer_type" | "computer_workspace_write"
        | "create_task" | "decision_resolve" | "desktop_record_start" | "desktop_record_stop"
        | "docs_append" | "fork_run" | "goals_create" | "gtasks_complete" | "gtasks_create"
        | "memory_alias_add" | "memory_store" | "merge_or_select" | "notion_page_append"
        | "odoo_crm_create_lead" | "odoo_crm_update_stage" | "odoo_sale_create_quotation"
        | "office_script" | "os_audio_set" | "os_backup_create" | "os_display_set" | "os_open"
        | "pause_cron_task" | "plan_update_step" | "responsibility_followup" | "run_cron_task"
        | "sheets_append" | "skill_bank_feedback" | "skill_curator_status" | "skill_extract"
        | "skill_pin" | "submit_feedback" | "tasks_block" | "tasks_claim" | "tasks_complete"
        | "tasks_create" | "tasks_renew" | "tasks_update" | "terminate_branch"
        | "update_cron_task" | "user_profile_record" | "wiki_export" | "wiki_rebuild_fts"
        | "wiki_write" | "working_state_clear" | "working_state_handoff"
        | "working_state_set" | "discovery_cancel" => Modify,

        // ── admin ───────────────────────────────────────────────────────
        "agent_update" | "agent_update_soul" | "capability_request" | "channel_config"
        | "codrive_run" | "create_agent" | "evolution_toggle" | "execute_program"
        | "llamafile_start" | "llamafile_stop" | "model_download" | "model_load"
        | "model_unload" | "odoo_execute" | "os_apply_update" | "os_factory_reset"
        | "os_power" | "os_update_rollback" | "os_wifi_connect" | "pairing_manage"
        | "shared_skill_adopt" | "skill_graduate" | "skill_hub_install"
        | "skill_synthesis_run" | "spawn_ephemeral" => Admin,

        _ => return None,
    };
    Some(effect)
}

/// The class of a native Claude Code tool by exact name, or `None` for a
/// name this table does not know (see [`effect_of_cli_builtin`]).
pub fn effect_of_native(tool_name: &str) -> Option<ToolEffect> {
    use ToolEffect::*;
    Some(match tool_name {
        "Read" | "Glob" | "Grep" | "WebFetch" | "WebSearch" => Read,
        "TodoWrite" => Draft,
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => Modify,
        "Bash" | "Task" | "Agent" => Admin,
        _ => return None,
    })
}

/// The Claude Code built-in tools [`ActionRules::builtins_refused`] judges
/// at spawn. Names outside [`effect_of_native`] are classed `admin`.
pub const CLAUDE_CLI_BUILTIN_TOOLS: &[&str] = &[
    "Read",
    "Glob",
    "Grep",
    "WebFetch",
    "WebSearch",
    "TodoWrite",
    "Write",
    "Edit",
    "MultiEdit",
    "NotebookEdit",
    "Bash",
    "BashOutput",
    "KillShell",
    "Task",
    "Agent",
    "SlashCommand",
    "ExitPlanMode",
];

/// The class of a Claude Code built-in tool: the [`effect_of_native`] entry,
/// or [`ToolEffect::Admin`] for any other name (fail closed).
pub fn effect_of_cli_builtin(tool_name: &str) -> ToolEffect {
    effect_of_native(tool_name).unwrap_or(ToolEffect::Admin)
}

/// The class a gate applies to `tool_name`: the table entry, or
/// [`ToolEffect::Admin`] for a name the table does not know (fail closed).
/// Accepts the bare name or the Claude CLI form `mcp__duduclaw__<name>`.
pub fn effect_of(tool_name: &str) -> ToolEffect {
    let bare = tool_name
        .strip_prefix("mcp__duduclaw__")
        .filter(|rest| !rest.is_empty())
        .unwrap_or(tool_name);
    effect_of_builtin(bare).unwrap_or(ToolEffect::Admin)
}

// ── Action rules ─────────────────────────────────────────────────────────

/// What one action rule decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActionVerdict {
    /// No friction from this layer.
    Allow,
    /// Ask a person (ApprovalBroker) before the call runs.
    Ask,
    /// Refuse the call.
    Block,
}

impl ActionVerdict {
    /// The lowercase token used in `agent.toml`.
    pub fn as_str(&self) -> &'static str {
        match self {
            ActionVerdict::Allow => "allow",
            ActionVerdict::Ask => "ask",
            ActionVerdict::Block => "block",
        }
    }
}

impl FromStr for ActionVerdict {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "allow" => Ok(ActionVerdict::Allow),
            "ask" => Ok(ActionVerdict::Ask),
            "block" => Ok(ActionVerdict::Block),
            other => Err(format!("unknown action verdict `{other}`")),
        }
    }
}

/// What a rule matches: one effect class, or one tool (a
/// `[capabilities]`-list style entry, see
/// [`crate::tool_catalog::tool_entry_matches`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionTarget {
    Effect(ToolEffect),
    Tool(String),
}

/// One valid `action_rules` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionRule {
    pub target: ActionTarget,
    pub verdict: ActionVerdict,
}

/// `agent.toml [capabilities] action_rules`.
///
/// Parsing never fails the surrounding config: a malformed list or entry is
/// kept verbatim (so a round-trip write does not drop it) and sets
/// [`Self::malformed`]. A malformed rule list is treated fail-closed:
/// every side-effecting call is at least `ask` (a valid `block` rule still
/// blocks). See [`Self::resolve`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ActionRules {
    /// The valid entries, in file order.
    pub rules: Vec<ActionRule>,
    /// The key was present but the value, or at least one entry, could not
    /// be read.
    pub malformed: bool,
    /// The value as written, re-emitted verbatim on serialization. `None`
    /// when the key was absent.
    raw: Option<serde_json::Value>,
}

impl ActionRules {
    /// No key in the file.
    pub fn is_absent(&self) -> bool {
        self.raw.is_none()
    }

    /// Rules read from an unreadable source: treated like a malformed list.
    pub fn unreadable() -> Self {
        Self {
            rules: Vec::new(),
            malformed: true,
            raw: None,
        }
    }

    /// Build from a JSON value (the shape the dashboard sends).
    pub fn from_value(value: serde_json::Value) -> Self {
        let mut out = Self {
            rules: Vec::new(),
            malformed: false,
            raw: Some(value.clone()),
        };
        let Some(items) = value.as_array() else {
            out.malformed = true;
            return out;
        };
        for item in items {
            match parse_rule(item) {
                Some(rule) => out.rules.push(rule),
                None => out.malformed = true,
            }
        }
        out
    }

    /// The verdict for one call, or `None` when no rule applies.
    ///
    /// A `tool` rule beats an `effect` rule; within one specificity the
    /// strictest verdict wins. A malformed list raises any side-effecting
    /// call to at least `ask`, whatever the valid rules say.
    pub fn resolve(&self, tool_name: &str, effect: ToolEffect) -> Option<ActionVerdict> {
        let strictest = |pred: &dyn Fn(&ActionTarget) -> bool| {
            self.rules
                .iter()
                .filter(|r| pred(&r.target))
                .map(|r| r.verdict)
                .max()
        };
        let by_tool = strictest(&|t| match t {
            ActionTarget::Tool(entry) => crate::tool_catalog::tool_entry_matches(entry, tool_name),
            ActionTarget::Effect(_) => false,
        });
        let verdict = by_tool.or_else(|| {
            strictest(&|t| matches!(t, ActionTarget::Effect(e) if *e == effect))
        });
        if self.malformed && effect.is_side_effecting() {
            return Some(verdict.map_or(ActionVerdict::Ask, |v| v.max(ActionVerdict::Ask)));
        }
        verdict
    }

    /// The strictest verdict among the `tool` rules naming `tool_name`
    /// (effect rules ignored). Used for a rule written for a removed tool
    /// name, which keeps gating the call that replaced it
    /// ([`crate::tool_catalog::removed_name_for_call`]).
    pub fn tool_rule_verdict(&self, tool_name: &str) -> Option<ActionVerdict> {
        self.rules
            .iter()
            .filter(|r| match &r.target {
                ActionTarget::Tool(entry) => {
                    crate::tool_catalog::tool_entry_matches(entry, tool_name)
                }
                ActionTarget::Effect(_) => false,
            })
            .map(|r| r.verdict)
            .max()
    }

    /// The verdict for one call with its arguments: [`Self::resolve`] for the
    /// tool, raised by any `tool` rule written for the removed name this
    /// call replaces (`wiki_write` with `scope = "shared"` is still
    /// `shared_wiki_write`). Only ever stricter than [`Self::resolve`].
    pub fn resolve_for_call(
        &self,
        tool_name: &str,
        effect: ToolEffect,
        args: &serde_json::Value,
    ) -> Option<ActionVerdict> {
        let own = self.resolve(tool_name, effect);
        let legacy = crate::tool_catalog::removed_name_for_call(tool_name, args)
            .and_then(|legacy| self.tool_rule_verdict(legacy));
        match (own, legacy) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
    }

    /// The Claude Code built-in tools these rules keep away from a spawned
    /// Claude CLI (`--disallowedTools`). A built-in call never reaches the
    /// ApprovalBroker, so `ask` is treated as `block` here (fail closed);
    /// built-ins are classed by [`effect_of_cli_builtin`] (unknown ⇒
    /// `admin`). Empty when the key is absent.
    pub fn builtins_refused(&self) -> Vec<String> {
        if self.is_absent() && !self.malformed {
            return Vec::new();
        }
        CLAUDE_CLI_BUILTIN_TOOLS
            .iter()
            .filter(|name| {
                self.resolve(name, effect_of_cli_builtin(name))
                    .is_some_and(|v| v >= ActionVerdict::Ask)
            })
            .map(|name| (*name).to_string())
            .collect()
    }
}

fn parse_rule(item: &serde_json::Value) -> Option<ActionRule> {
    let obj = item.as_object()?;
    // Only the three known keys; anything else is a typo we refuse to guess at.
    if obj.keys().any(|k| !matches!(k.as_str(), "effect" | "tool" | "verdict")) {
        return None;
    }
    let verdict: ActionVerdict = obj.get("verdict")?.as_str()?.parse().ok()?;
    let target = match (obj.get("effect"), obj.get("tool")) {
        (Some(e), None) => ActionTarget::Effect(e.as_str()?.parse().ok()?),
        (None, Some(t)) => {
            let t = t.as_str()?.trim();
            if t.is_empty() {
                return None;
            }
            ActionTarget::Tool(normalize_tool_rule(t))
        }
        _ => return None,
    };
    Some(ActionRule { target, verdict })
}

impl<'de> Deserialize<'de> for ActionRules {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Format-agnostic and infallible: anything that is not representable
        // as JSON (never the case for TOML) is kept as malformed.
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Any {
            Json(serde_json::Value),
            Other(IgnoredAny),
        }
        Ok(match Any::deserialize(d)? {
            Any::Json(v) => ActionRules::from_value(v),
            Any::Other(_) => ActionRules {
                rules: Vec::new(),
                malformed: true,
                raw: Some(serde_json::Value::Null),
            },
        })
    }
}

impl Serialize for ActionRules {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match &self.raw {
            Some(v) => v.serialize(s),
            None => serde_json::Value::Array(Vec::new()).serialize(s),
        }
    }
}

/// A tool rule may name a third-party tool as `<server>.<tool>` (the form
/// `duduclaw mcp-proxy` and the redaction rules use) or as the Claude CLI
/// form `mcp__<server>__<tool>`; the first is rewritten into the second so
/// one matcher serves both. DuDuClaw's own tool names never contain a dot, so
/// a dotted name can only mean a third-party tool. `<server>.*` names every
/// tool of that server.
fn normalize_tool_rule(t: &str) -> String {
    if t.starts_with("mcp__") {
        return t.to_string();
    }
    match t.split_once('.') {
        Some((server, tool)) if !server.is_empty() && !tool.is_empty() => {
            format!("mcp__{server}__{tool}")
        }
        _ => t.to_string(),
    }
}

// ── Third-party MCP tools (2026-10-08) ───────────────────────────────────

/// The `annotations` an MCP server declares for one tool in `tools/list`
/// (MCP 2025-03-26 and later). Every field is a *hint* the server makes
/// about itself: nothing checks it. A value that is not a JSON boolean
/// counts as absent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolAnnotations {
    #[serde(rename = "readOnlyHint", skip_serializing_if = "Option::is_none")]
    pub read_only: Option<bool>,
    #[serde(rename = "destructiveHint", skip_serializing_if = "Option::is_none")]
    pub destructive: Option<bool>,
    #[serde(rename = "idempotentHint", skip_serializing_if = "Option::is_none")]
    pub idempotent: Option<bool>,
    #[serde(rename = "openWorldHint", skip_serializing_if = "Option::is_none")]
    pub open_world: Option<bool>,
}

impl ToolAnnotations {
    /// Read the `annotations` object of one `tools/list` entry.
    pub fn from_tool(tool: &serde_json::Value) -> Self {
        let Some(a) = tool.get("annotations").and_then(|a| a.as_object()) else {
            return Self::default();
        };
        let flag = |k: &str| a.get(k).and_then(|v| v.as_bool());
        Self {
            read_only: flag("readOnlyHint"),
            destructive: flag("destructiveHint"),
            idempotent: flag("idempotentHint"),
            open_world: flag("openWorldHint"),
        }
    }
}

/// The name action rules see for a third-party tool:
/// `mcp__<server>__<tool>`, the Claude CLI's form, so `tool =
/// "mcp__github__*"`, `tool = "github.create_issue"` and a bare server rule
/// `tool = "mcp__github"` all match it (see [`ActionRules::resolve`]).
pub fn third_party_tool_ref(server: &str, tool: &str) -> String {
    format!("mcp__{server}__{tool}")
}

/// The class of a third-party tool from its annotations.
///
/// - `destructiveHint: true` ⇒ [`ToolEffect::Delete`] (believed from every
///   server: it can only make the tool stricter);
/// - `readOnlyHint: true` ⇒ [`ToolEffect::Read`] **only** when
///   `trust_read_hint` (the operator listed the server in
///   `[capabilities] trusted_read_hint_servers`), otherwise
///   [`ToolEffect::Modify`];
/// - anything else, including absent annotations ⇒ [`ToolEffect::Modify`].
///
/// Why `read` needs an opt-in: a malicious or careless server can declare
/// `readOnlyHint: true` on a tool that deletes things. Believed, that tool
/// would pass every `read` rule and the explore lane. Not believed, the
/// cost is that a genuinely read-only tool of an untrusted server is treated
/// like a write (asked, blocked or hidden wherever `modify` is).
pub fn effect_from_annotations(ann: &ToolAnnotations, trust_read_hint: bool) -> ToolEffect {
    if ann.destructive == Some(true) {
        return ToolEffect::Delete;
    }
    if ann.read_only == Some(true) && trust_read_hint {
        return ToolEffect::Read;
    }
    ToolEffect::Modify
}

/// Why a third-party tool is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThirdPartyBlock {
    /// An `action_rules` entry says `block`.
    Rule,
    /// The process runs in the explore lane and the tool is not `read`.
    Lane,
}

impl ThirdPartyBlock {
    /// Audit token.
    pub fn as_str(&self) -> &'static str {
        match self {
            ThirdPartyBlock::Rule => "action_rule",
            ThirdPartyBlock::Lane => "explore_lane",
        }
    }
}

/// What the proxy / bridge does with one third-party tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThirdPartyDecision {
    /// Listed and forwarded.
    Allow,
    /// Listed; each call waits for an ApprovalBroker decision.
    Ask,
    /// Hidden from `tools/list`; a call is answered with a JSON-RPC error.
    Block(ThirdPartyBlock),
}

/// One employee's policy for tools of servers other than DuDuClaw's own:
/// its `action_rules` plus the servers whose read-only hint it believes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ThirdPartyPolicy {
    pub rules: ActionRules,
    pub trusted_read_hint_servers: Vec<String>,
}

impl ThirdPartyPolicy {
    /// Does this employee believe `server`'s `readOnlyHint`? Exact name.
    pub fn trusts_read_hint(&self, server: &str) -> bool {
        self.trusted_read_hint_servers.iter().any(|s| s == server)
    }

    /// The class this employee gives `server`'s `tool`.
    pub fn effect(&self, server: &str, ann: &ToolAnnotations) -> ToolEffect {
        effect_from_annotations(ann, self.trusts_read_hint(server))
    }

    /// Is there anything to enforce outside the explore lane? `false` means
    /// the proxy would only pass frames through, which is the pre-2026-10-08
    /// behaviour (no rules ⇒ no friction).
    pub fn has_rules(&self) -> bool {
        !self.rules.is_absent() || self.rules.malformed
    }

    /// The decision for one tool in one lane.
    ///
    /// The explore lane admits only `read`, and a lane value other than
    /// `explore` refuses everything (as for DuDuClaw's own tools). Outside
    /// the lane the action rules decide; no rule ⇒ allow.
    pub fn decide(
        &self,
        server: &str,
        tool: &str,
        ann: &ToolAnnotations,
        lane: &ProcessLane,
    ) -> ThirdPartyDecision {
        let effect = self.effect(server, ann);
        let lane_ok = match lane {
            ProcessLane::Normal => true,
            ProcessLane::Explore => effect == ToolEffect::Read,
            ProcessLane::Invalid => false,
        };
        let verdict = self.rules.resolve(&third_party_tool_ref(server, tool), effect);
        match verdict {
            Some(ActionVerdict::Block) => ThirdPartyDecision::Block(ThirdPartyBlock::Rule),
            _ if !lane_ok => ThirdPartyDecision::Block(ThirdPartyBlock::Lane),
            Some(ActionVerdict::Ask) => ThirdPartyDecision::Ask,
            _ => ThirdPartyDecision::Allow,
        }
    }
}

// ── Explore lane ─────────────────────────────────────────────────────────

/// Environment variable naming the lane an MCP server process runs in. Set
/// by the gateway on spawns that must stay read-only (the heartbeat
/// proactive check). Only [`LANE_EXPLORE`] is defined.
pub const ENV_LANE: &str = "DUDUCLAW_LANE";

/// The read-only explore lane: only `read` and `draft` tools are listed and
/// callable.
pub const LANE_EXPLORE: &str = "explore";

/// The lane an MCP server process runs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessLane {
    /// [`ENV_LANE`] absent: no lane restriction.
    Normal,
    /// `explore`: read-only tools only.
    Explore,
    /// Present but empty, unknown or not UTF-8: every call is refused.
    Invalid,
}

impl ProcessLane {
    /// Decide from the raw environment value (`None` = absent).
    pub fn from_env_value(value: Option<&std::ffi::OsStr>) -> Self {
        match value {
            None => ProcessLane::Normal,
            Some(v) => match v.to_str() {
                Some(LANE_EXPLORE) => ProcessLane::Explore,
                _ => ProcessLane::Invalid,
            },
        }
    }

    /// This process's lane.
    pub fn current() -> Self {
        Self::from_env_value(std::env::var_os(ENV_LANE).as_deref())
    }

    /// May a tool of this class be listed and called in this lane?
    pub fn permits(&self, effect: ToolEffect) -> bool {
        match self {
            ProcessLane::Normal => true,
            ProcessLane::Explore => !effect.is_side_effecting(),
            ProcessLane::Invalid => false,
        }
    }
}

/// Claude Code built-in tools a read-only lane keeps (`--tools` list).
pub const EXPLORE_LANE_BUILTIN_TOOLS: &[&str] = &["Read", "Glob", "Grep", "WebFetch", "WebSearch"];

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(v: serde_json::Value) -> ActionRules {
        ActionRules::from_value(v)
    }

    #[test]
    fn cli_builtins_are_classified_and_unknown_names_are_admin() {
        assert_eq!(effect_of_cli_builtin("Read"), ToolEffect::Read);
        assert_eq!(effect_of_cli_builtin("WebSearch"), ToolEffect::Read);
        assert_eq!(effect_of_cli_builtin("TodoWrite"), ToolEffect::Draft);
        assert_eq!(effect_of_cli_builtin("MultiEdit"), ToolEffect::Modify);
        assert_eq!(effect_of_cli_builtin("Bash"), ToolEffect::Admin);
        assert_eq!(effect_of_cli_builtin("Task"), ToolEffect::Admin);
        assert_eq!(effect_of_cli_builtin("Agent"), ToolEffect::Admin);
        assert_eq!(effect_of_cli_builtin("SomethingNew"), ToolEffect::Admin);
        assert_eq!(effect_of_cli_builtin("read"), ToolEffect::Admin, "exact names only");
    }

    #[test]
    fn builtins_refused_treats_ask_as_block_and_never_touches_absent_rules() {
        assert!(ActionRules::default().builtins_refused().is_empty());
        let r = rules(serde_json::json!([
            { "effect": "modify", "verdict": "ask" },
            { "tool": "Bash", "verdict": "block" },
            { "effect": "read", "verdict": "allow" },
        ]));
        let refused = r.builtins_refused();
        for t in ["Write", "Edit", "MultiEdit", "NotebookEdit", "Bash"] {
            assert!(refused.contains(&t.to_string()), "{t}: {refused:?}");
        }
        for t in ["Read", "Grep", "Task", "TodoWrite"] {
            assert!(!refused.contains(&t.to_string()), "{t}: {refused:?}");
        }
        // A tool rule beats an effect rule, as for MCP tools.
        let r = rules(serde_json::json!([
            { "effect": "admin", "verdict": "block" },
            { "tool": "Task", "verdict": "allow" },
        ]));
        let refused = r.builtins_refused();
        assert!(refused.contains(&"Bash".to_string()));
        assert!(refused.contains(&"SlashCommand".to_string()), "unknown ⇒ admin");
        assert!(!refused.contains(&"Task".to_string()));
        // Malformed ⇒ every side-effecting built-in kept away; reads stay.
        let bad = rules(serde_json::json!([{ "effect": "send", "verdict": "maybe" }]));
        let refused = bad.builtins_refused();
        assert!(refused.contains(&"Write".to_string()) && refused.contains(&"Bash".to_string()));
        assert!(!refused.contains(&"Read".to_string()));
        assert!(!refused.contains(&"TodoWrite".to_string()));
        let unreadable = ActionRules::unreadable().builtins_refused();
        assert!(unreadable.contains(&"Edit".to_string()));
    }

    #[test]
    fn a_rule_for_a_removed_name_follows_its_replacement_call() {
        let r = rules(serde_json::json!([
            { "tool": "shared_wiki_write", "verdict": "block" },
            { "tool": "schedule_task", "verdict": "ask" },
        ]));
        let eff = |t: &str| crate::effect_of(t);
        let shared = serde_json::json!({"scope": "shared"});
        assert_eq!(
            r.resolve_for_call("wiki_write", eff("wiki_write"), &shared),
            Some(ActionVerdict::Block)
        );
        assert_eq!(
            r.resolve_for_call("wiki_write", eff("wiki_write"), &serde_json::json!({})),
            None
        );
        assert_eq!(
            r.resolve_for_call(
                "tasks_create",
                eff("tasks_create"),
                &serde_json::json!({"schedule": "0 9 * * *"})
            ),
            Some(ActionVerdict::Ask)
        );
        // Never looser than the call's own verdict.
        let r = rules(serde_json::json!([
            { "tool": "wiki_write", "verdict": "block" },
            { "tool": "shared_wiki_write", "verdict": "allow" },
        ]));
        assert_eq!(
            r.resolve_for_call("wiki_write", eff("wiki_write"), &shared),
            Some(ActionVerdict::Block)
        );
        // An effect rule does not match the removed name (it would class it
        // `admin`).
        let r = rules(serde_json::json!([{ "effect": "admin", "verdict": "block" }]));
        assert_eq!(r.tool_rule_verdict("shared_wiki_write"), None);
    }

    #[test]
    fn effect_round_trips_and_classifies_side_effects() {
        for e in ToolEffect::ALL {
            assert_eq!(e.as_str().parse::<ToolEffect>().unwrap(), e);
            assert_eq!(e.to_string(), e.as_str());
        }
        assert!("SEND".parse::<ToolEffect>().is_ok());
        assert!("sending".parse::<ToolEffect>().is_err());
        assert!(!ToolEffect::Read.is_side_effecting());
        assert!(!ToolEffect::Draft.is_side_effecting());
        assert!(ToolEffect::Send.is_side_effecting());
        assert!(ToolEffect::Admin.is_side_effecting());
    }

    #[test]
    fn table_is_exact_and_unknown_is_admin() {
        assert_eq!(effect_of_builtin("memory_search"), Some(ToolEffect::Read));
        assert_eq!(effect_of_builtin("wiki_write"), Some(ToolEffect::Modify));
        assert_eq!(effect_of_builtin("mail_send"), Some(ToolEffect::Send));
        assert_eq!(effect_of_builtin("wiki_share"), Some(ToolEffect::Publish));
        assert_eq!(effect_of_builtin("agent_remove"), Some(ToolEffect::Delete));
        assert_eq!(effect_of_builtin("memory_search_x"), None);
        assert_eq!(effect_of_builtin("memory"), None);
        assert_eq!(effect_of("totally_new_tool"), ToolEffect::Admin);
        assert_eq!(effect_of("mcp__duduclaw__send_message"), ToolEffect::Send);
        assert_eq!(effect_of("mcp__duduclaw__"), ToolEffect::Admin);
    }

    #[test]
    fn tool_rule_beats_effect_rule_and_strictest_wins_within_a_level() {
        let r = rules(serde_json::json!([
            { "effect": "send", "verdict": "block" },
            { "tool": "send_message", "verdict": "allow" },
            { "effect": "modify", "verdict": "allow" },
            { "effect": "modify", "verdict": "ask" },
        ]));
        assert!(!r.malformed);
        assert_eq!(r.resolve("send_message", ToolEffect::Send), Some(ActionVerdict::Allow));
        assert_eq!(r.resolve("mail_send", ToolEffect::Send), Some(ActionVerdict::Block));
        assert_eq!(r.resolve("wiki_write", ToolEffect::Modify), Some(ActionVerdict::Ask));
        assert_eq!(r.resolve("memory_search", ToolEffect::Read), None);
    }

    #[test]
    fn third_party_annotations_classify_and_read_needs_trust() {
        let ann = |v: serde_json::Value| ToolAnnotations::from_tool(&serde_json::json!({ "annotations": v }));
        let ro = ann(serde_json::json!({ "readOnlyHint": true }));
        assert_eq!(effect_from_annotations(&ro, false), ToolEffect::Modify);
        assert_eq!(effect_from_annotations(&ro, true), ToolEffect::Read);
        let del = ann(serde_json::json!({ "readOnlyHint": true, "destructiveHint": true }));
        assert_eq!(effect_from_annotations(&del, true), ToolEffect::Delete);
        // Non-boolean hints count as absent; absent never classifies as read.
        let junk = ann(serde_json::json!({ "readOnlyHint": "true" }));
        assert_eq!(junk.read_only, None);
        assert_eq!(effect_from_annotations(&junk, true), ToolEffect::Modify);
        assert_eq!(effect_from_annotations(&ToolAnnotations::default(), true), ToolEffect::Modify);
    }

    #[test]
    fn third_party_policy_applies_rules_and_lane() {
        let ro = ToolAnnotations { read_only: Some(true), ..Default::default() };
        let none = ToolAnnotations::default();
        let p = ThirdPartyPolicy {
            rules: rules(serde_json::json!([
                { "effect": "modify", "verdict": "ask" },
                { "tool": "github.delete_repo", "verdict": "block" },
                { "tool": "mcp__notion__*", "verdict": "allow" },
            ])),
            trusted_read_hint_servers: vec!["github".into()],
        };
        assert!(p.has_rules());
        let n = ProcessLane::Normal;
        assert_eq!(p.decide("github", "get_issue", &ro, &n), ThirdPartyDecision::Allow);
        assert_eq!(p.decide("github", "create_issue", &none, &n), ThirdPartyDecision::Ask);
        assert_eq!(p.decide("github", "delete_repo", &ro, &n), ThirdPartyDecision::Block(ThirdPartyBlock::Rule));
        // Untrusted server: its read-only claim is a modify.
        assert_eq!(p.decide("evil", "wipe", &ro, &n), ThirdPartyDecision::Ask);
        assert_eq!(p.decide("notion", "write_page", &none, &n), ThirdPartyDecision::Allow);
        let e = ProcessLane::Explore;
        assert_eq!(p.decide("github", "get_issue", &ro, &e), ThirdPartyDecision::Allow);
        assert_eq!(p.decide("evil", "wipe", &ro, &e), ThirdPartyDecision::Block(ThirdPartyBlock::Lane));
        assert_eq!(p.decide("notion", "write_page", &none, &e), ThirdPartyDecision::Block(ThirdPartyBlock::Lane));
        assert_eq!(p.decide("github", "get_issue", &ro, &ProcessLane::Invalid), ThirdPartyDecision::Block(ThirdPartyBlock::Lane));
        // No rules at all: nothing changes outside the lane.
        let empty = ThirdPartyPolicy::default();
        assert!(!empty.has_rules());
        assert_eq!(empty.decide("x", "y", &none, &n), ThirdPartyDecision::Allow);
        assert!(ThirdPartyPolicy { rules: ActionRules::unreadable(), ..Default::default() }.has_rules());
    }

    #[test]
    fn dotted_tool_rules_name_third_party_tools_only() {
        let r = rules(serde_json::json!([{ "tool": "crm.update", "verdict": "block" }]));
        assert_eq!(r.resolve("mcp__crm__update", ToolEffect::Modify), Some(ActionVerdict::Block));
        assert_eq!(r.resolve("update", ToolEffect::Modify), None);
        // Serialization keeps what the operator wrote.
        assert_eq!(serde_json::to_value(&r).unwrap()[0]["tool"], "crm.update");
    }

    #[test]
    fn tool_rules_use_the_anchored_list_matcher() {
        let r = rules(serde_json::json!([
            { "tool": "mcp__duduclaw__mail_send", "verdict": "block" },
            { "tool": "wiki_*", "verdict": "ask" },
        ]));
        assert_eq!(r.resolve("mail_send", ToolEffect::Send), Some(ActionVerdict::Block));
        assert_eq!(r.resolve("wiki_write", ToolEffect::Modify), Some(ActionVerdict::Ask));
        assert_eq!(r.resolve("shared_wiki_delete", ToolEffect::Delete), None);
    }

    #[test]
    fn malformed_rules_raise_side_effects_to_ask_and_keep_blocks() {
        for bad in [
            serde_json::json!("send=block"),
            serde_json::json!([{ "effect": "send", "verdict": "maybe" }]),
            serde_json::json!([{ "effect": "sending", "verdict": "block" }]),
            serde_json::json!([{ "effect": "send", "tool": "x", "verdict": "block" }]),
            serde_json::json!([{ "verdict": "block" }]),
            serde_json::json!([{ "effect": "send", "verdict": "allow", "note": 1 }]),
            serde_json::json!([42]),
        ] {
            let r = rules(bad.clone());
            assert!(r.malformed, "{bad}");
            assert_eq!(r.resolve("send_message", ToolEffect::Send), Some(ActionVerdict::Ask));
            assert_eq!(r.resolve("memory_search", ToolEffect::Read), None);
        }
        let mixed = rules(serde_json::json!([
            { "effect": "delete", "verdict": "block" },
            { "effect": "send", "verdict": "allow" },
            "junk",
        ]));
        assert!(mixed.malformed);
        assert_eq!(mixed.resolve("agent_remove", ToolEffect::Delete), Some(ActionVerdict::Block));
        assert_eq!(mixed.resolve("send_message", ToolEffect::Send), Some(ActionVerdict::Ask));
        assert_eq!(ActionRules::unreadable().resolve("x", ToolEffect::Admin), Some(ActionVerdict::Ask));
    }

    #[test]
    fn toml_parse_is_infallible_and_round_trips_verbatim() {
        #[derive(Deserialize, Serialize)]
        struct Caps {
            #[serde(default, skip_serializing_if = "ActionRules::is_absent")]
            action_rules: ActionRules,
            #[serde(default)]
            other: bool,
        }
        let ok: Caps = toml::from_str(
            "other = true\naction_rules = [{ effect = \"send\", verdict = \"ask\" }, { tool = \"mail_send\", verdict = \"block\" }]\n",
        )
        .unwrap();
        assert!(ok.other);
        assert_eq!(ok.action_rules.rules.len(), 2);
        assert!(!ok.action_rules.malformed);

        let bad: Caps = toml::from_str("other = true\naction_rules = 7\n").unwrap();
        assert!(bad.other, "a bad rule list must not fail the section");
        assert!(bad.action_rules.malformed);
        let written = toml::to_string(&bad).unwrap();
        assert!(written.contains("action_rules = 7"), "kept verbatim: {written}");

        let absent: Caps = toml::from_str("other = false\n").unwrap();
        assert!(absent.action_rules.is_absent());
        assert!(!toml::to_string(&absent).unwrap().contains("action_rules"));
    }

    #[test]
    fn lane_values() {
        use std::ffi::OsStr;
        assert_eq!(ProcessLane::from_env_value(None), ProcessLane::Normal);
        assert_eq!(ProcessLane::from_env_value(Some(OsStr::new("explore"))), ProcessLane::Explore);
        for bad in ["", "Explore", " explore", "read", "normal"] {
            assert_eq!(ProcessLane::from_env_value(Some(OsStr::new(bad))), ProcessLane::Invalid, "{bad:?}");
        }
        assert!(ProcessLane::Explore.permits(ToolEffect::Read));
        assert!(ProcessLane::Explore.permits(ToolEffect::Draft));
        assert!(!ProcessLane::Explore.permits(ToolEffect::Send));
        assert!(!ProcessLane::Invalid.permits(ToolEffect::Read));
        assert!(ProcessLane::Normal.permits(ToolEffect::Admin));
    }
}
