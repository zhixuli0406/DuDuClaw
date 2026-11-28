//! RM-141 pure Odoo authorization policy. No credential resolution or I/O.
//!
//! Parse errors must be propagated: callers must refuse all Odoo tools rather
//! than replacing an error with defaults. Identity facts come from trusted
//! callers. This module does not replace the gateway identity gate, MCP scopes,
//! feature switches, approval, engine schema ACL checks, or Odoo's own ACLs.

use std::collections::BTreeSet;

use chrono::DateTime;
use serde::Deserialize;

use crate::AgentOdooConfig;

pub const DEFAULT_ALWAYS_REFUSE: &[&str] = &[
    "unlink:*",
    "create:ir.*",
    "write:ir.*",
    "execute:ir.*",
    "create:res.users",
    "write:res.users",
    "create:res.groups",
    "write:res.groups",
    "method:execute_import",
    "method:signal_workflow",
];

const CONFIG_UNREADABLE: &str = "Odoo 權限設定讀不到或格式有誤，請管理員檢查設定後再試。";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    #[default]
    Engine,
    FollowOdooAccount,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionModeAck {
    pub by: String,
    pub at: String,
}

impl PermissionModeAck {
    fn valid(&self) -> bool {
        !self.by.trim().is_empty() && DateTime::parse_from_rfc3339(&self.at).is_ok()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OdooAccessConfig {
    pub permission_mode: PermissionMode,
    pub permission_mode_ack: Option<PermissionModeAck>,
    pub company_key_agents: Vec<String>,
    pub company_key_agents_migrated: String,
    pub upgrade_notice_pending: bool,
    pub always_refuse: Vec<String>,
    pub attachment_max_mb: u8,
}

impl Default for OdooAccessConfig {
    fn default() -> Self {
        Self {
            permission_mode: PermissionMode::Engine,
            permission_mode_ack: None,
            company_key_agents: Vec::new(),
            company_key_agents_migrated: String::new(),
            upgrade_notice_pending: false,
            always_refuse: DEFAULT_ALWAYS_REFUSE.iter().map(|s| (*s).to_owned()).collect(),
            attachment_max_mb: 10,
        }
    }
}

// Keep the acknowledgement untyped until validation: malformed acknowledgements
// are the one specified exception to parse errors (they disable Follow).
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawAccessConfig {
    permission_mode: PermissionMode,
    permission_mode_ack: Option<toml::Value>,
    company_key_agents: Vec<String>,
    company_key_agents_migrated: String,
    upgrade_notice_pending: bool,
    always_refuse: Vec<String>,
    attachment_max_mb: u8,
}

impl Default for RawAccessConfig {
    fn default() -> Self {
        let defaults = OdooAccessConfig::default();
        Self {
            permission_mode: defaults.permission_mode,
            permission_mode_ack: None,
            company_key_agents: defaults.company_key_agents,
            company_key_agents_migrated: defaults.company_key_agents_migrated,
            upgrade_notice_pending: defaults.upgrade_notice_pending,
            always_refuse: defaults.always_refuse,
            attachment_max_mb: defaults.attachment_max_mb,
        }
    }
}

impl OdooAccessConfig {
    /// Parse the whole config text, isolating the strict `[odoo_access]` section.
    /// Missing sections use defaults; invalid TOML or policy returns an error.
    pub fn parse_config_toml(raw: &str) -> Result<Self, String> {
        let table: toml::Table = raw.parse().map_err(|_| CONFIG_UNREADABLE.to_owned())?;
        Self::from_table(&table)
    }

    /// Parse a whole config table. Unknown acknowledgement keys are errors;
    /// missing/wrong-typed acknowledgement fields or an invalid timestamp
    /// discard the acknowledgement and normalize the mode to Engine.
    pub fn from_table(table: &toml::Table) -> Result<Self, String> {
        let Some(section) = table.get("odoo_access") else {
            return Ok(Self::default());
        };
        if !section.is_table() {
            return Err(CONFIG_UNREADABLE.to_owned());
        }
        let raw: RawAccessConfig = section.clone().try_into()
            .map_err(|_| CONFIG_UNREADABLE.to_owned())?;
        let ack = match raw.permission_mode_ack {
            Some(value) => {
                if let Some(table) = value.as_table() {
                    if table.keys().any(|key| key != "by" && key != "at") {
                        return Err(CONFIG_UNREADABLE.to_owned());
                    }
                }
                let parsed: Result<PermissionModeAck, _> = value.try_into();
                parsed.ok().filter(PermissionModeAck::valid)
            }
            None => None,
        };
        let config = Self {
            permission_mode: if ack.is_some() { raw.permission_mode } else { PermissionMode::Engine },
            permission_mode_ack: ack,
            company_key_agents: raw.company_key_agents,
            company_key_agents_migrated: raw.company_key_agents_migrated,
            upgrade_notice_pending: raw.upgrade_notice_pending,
            always_refuse: raw.always_refuse,
            attachment_max_mb: raw.attachment_max_mb,
        };
        config.validated_rules()?;
        Ok(config)
    }

    fn validated_rules(&self) -> Result<Vec<RefuseRule>, String> {
        if !(1..=25).contains(&self.attachment_max_mb)
            || self.company_key_agents.iter().any(|id| !duduclaw_core::is_valid_agent_id(id))
        {
            return Err(CONFIG_UNREADABLE.to_owned());
        }
        self.always_refuse.iter().map(|entry| RefuseRule::parse(entry)).collect()
    }

    /// Check the administrator's list. An invalid programmatically assembled
    /// config refuses as well; never silently skip an invalid list entry.
    pub fn always_refuses(&self, op: &OdooOp<'_>) -> bool {
        match self.validated_rules() {
            Ok(rules) => rules.iter().any(|rule| rule.matches(op)),
            Err(_) => true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Read,
    Create,
    Write,
    Unlink,
    Execute,
    Activity,
    Note,
    Message,
    Attach,
}

impl Verb {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Create => "create",
            Self::Write => "write",
            Self::Unlink => "unlink",
            Self::Execute => "execute",
            Self::Activity => "activity",
            Self::Note => "note",
            Self::Message => "message",
            Self::Attach => "attach",
        }
    }
}

/// CRUD aliases mirror the CLI. All non-CRUD methods, including workflow
/// buttons, use Execute for administrator refuse-list matching.
pub fn method_verb(method: &str) -> Verb {
    match method {
        "read" | "search" | "search_read" | "search_count" | "fields_get" | "name_get"
        | "name_search" | "default_get" | "read_group" => Verb::Read,
        "create" | "copy" => Verb::Create,
        "write" | "update" => Verb::Write,
        "unlink" => Verb::Unlink,
        _ => Verb::Execute,
    }
}

// The policy grammar intentionally differs from the bounded schema identifier
// validator: digit-leading segments are legal, empty segments are not.
pub(crate) fn valid_policy_model(model: &str) -> bool {
    model.split('.').all(|segment| {
        !segment.is_empty()
            && segment.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    })
}

fn valid_method(method: &str) -> bool {
    !method.is_empty() && method.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn descendant(model: &str, prefix: &str) -> bool {
    let mut parts = model.split('.');
    for segment in prefix.split('.') {
        if parts.next() != Some(segment) {
            return false;
        }
    }
    parts.next().is_some_and(|part| !part.is_empty())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ModelPattern {
    Any,
    Exact(String),
    Descendants(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RuleKind {
    Model { verb: Option<Verb>, model: ModelPattern },
    Method(String),
}

/// Validated administrator rule. Construction is only through the strict parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefuseRule(RuleKind);

impl RefuseRule {
    pub fn parse(entry: &str) -> Result<Self, String> {
        let invalid = || CONFIG_UNREADABLE.to_owned();
        let (verb, target) = entry.split_once(':').ok_or_else(invalid)?;
        if verb == "method" {
            return if valid_method(target) {
                Ok(Self(RuleKind::Method(target.to_owned())))
            } else {
                Err(invalid())
            };
        }
        let verb = match verb {
            "*" => None,
            "create" => Some(Verb::Create),
            "write" => Some(Verb::Write),
            "unlink" => Some(Verb::Unlink),
            "execute" => Some(Verb::Execute),
            "activity" => Some(Verb::Activity),
            "note" => Some(Verb::Note),
            "message" => Some(Verb::Message),
            "attach" => Some(Verb::Attach),
            _ => return Err(invalid()),
        };
        let model = if target == "*" {
            ModelPattern::Any
        } else if let Some(prefix) = target.strip_suffix(".*") {
            if !valid_policy_model(prefix) {
                return Err(invalid());
            }
            ModelPattern::Descendants(prefix.to_owned())
        } else if valid_policy_model(target) {
            ModelPattern::Exact(target.to_owned())
        } else {
            return Err(invalid());
        };
        Ok(Self(RuleKind::Model { verb, model }))
    }

    /// Dedicated operations use their semantic verb and target record's model
    /// (e.g. Attach on crm.lead), not the attachment storage table's Create.
    /// Method rules still match the underlying RPC method exactly.
    pub fn matches(&self, op: &OdooOp<'_>) -> bool {
        match &self.0 {
            RuleKind::Method(method) => method == op.method,
            RuleKind::Model { verb, model } => {
                if verb.is_some_and(|verb| verb != op.verb) {
                    return false;
                }
                match model {
                    ModelPattern::Any => true,
                    ModelPattern::Exact(model) => model == op.model,
                    ModelPattern::Descendants(prefix) => descendant(op.model, prefix),
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    Generic,
    Dedicated,
    Activity,
    Note,
    Message,
    Attachment,
}

/// A normalized operation. For generic tools, `verb` must be `method_verb`;
/// dedicated tools use their semantic verb and the target record's model.
/// Raw attachment storage operations may also be checked with Attachment kind.
#[derive(Debug, Clone, Copy)]
pub struct OdooOp<'a> {
    pub tool: ToolKind,
    pub model: &'a str,
    pub method: &'a str,
    pub verb: Verb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixedRefusal {
    F1,
    F2,
    F3,
    F4,
    F5,
}

impl FixedRefusal {
    pub fn code(self) -> &'static str {
        match self {
            Self::F1 => "F1", Self::F2 => "F2", Self::F3 => "F3", Self::F4 => "F4", Self::F5 => "F5",
        }
    }

    pub fn reason(self) -> &'static str {
        match self {
            Self::F1 => "附件只能透過新增附件功能從檔案建立，不能修改或刪除既有附件",
            Self::F2 => "不能修改 Odoo 的資料表、欄位定義或權限設定",
            Self::F3 => "不能替同仁建立、修改或刪除 Odoo 金鑰",
            Self::F4 => "訊息與通知必須透過專用功能處理，對外傳送前須經核准",
            Self::F5 => "這是 Odoo 內部運作使用的功能，不開放 AI 操作",
        }
    }
}

pub fn fixed_refusal(op: &OdooOp<'_>) -> Option<FixedRefusal> {
    let method = method_verb(op.method);
    let modifies = op.verb != Verb::Read || method != Verb::Read;
    if op.model == "ir.attachment"
        && (matches!(op.verb, Verb::Write | Verb::Unlink)
            || matches!(method, Verb::Write | Verb::Unlink)
            || ((op.verb == Verb::Create || method == Verb::Create) && op.tool != ToolKind::Attachment))
    {
        return Some(FixedRefusal::F1);
    }
    if modifies && (op.model == "ir.model" || descendant(op.model, "ir.model") || op.model == "ir.rule") {
        return Some(FixedRefusal::F2);
    }
    if modifies && (op.model == "res.users.apikeys" || descendant(op.model, "res.users.apikeys")) {
        return Some(FixedRefusal::F3);
    }
    if (modifies && (op.model == "mail" || descendant(op.model, "mail")))
        || (op.tool == ToolKind::Generic
            && matches!(op.method, "message_post" | "message_notify" | "message_subscribe" | "send_mail" | "action_send_mail"))
    {
        return Some(FixedRefusal::F4);
    }
    if op.method.starts_with('_') || matches!(op.method, "init" | "uninstall_hook") {
        return Some(FixedRefusal::F5);
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityKind { Personal, Agent, Company, NotConnected }

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PermissionOverride {
    #[default]
    Inherit,
    Engine,
    ReadOnly,
}

impl PermissionOverride {
    /// Higher ranks restrict more operations. Compare parsed values so unknown
    /// configuration strings retain their fail-closed read-only semantics.
    pub fn restriction_rank(self) -> u8 {
        match self {
            Self::Inherit => 0,
            Self::Engine => 1,
            Self::ReadOnly => 2,
        }
    }

    /// Unknown values are deliberately indistinguishable from read_only.
    pub fn from_config(value: Option<&str>) -> Self {
        match value {
            None | Some("inherit") => Self::Inherit,
            Some("engine") => Self::Engine,
            _ => Self::ReadOnly,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectiveMode { Follow, Engine, ReadOnly }

pub fn effective_mode(
    identity: IdentityKind, access: &OdooAccessConfig, override_: PermissionOverride,
) -> EffectiveMode {
    if override_ == PermissionOverride::ReadOnly {
        EffectiveMode::ReadOnly
    } else if identity == IdentityKind::Personal
        && access.permission_mode == PermissionMode::FollowOdooAccount
        && access.permission_mode_ack.as_ref().is_some_and(PermissionModeAck::valid)
        && override_ != PermissionOverride::Engine
    {
        EffectiveMode::Follow
    } else {
        EffectiveMode::Engine
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolClass {
    Read,
    GenericCreate,
    GenericWrite,
    Activity,
    Note,
    Message,
    Attach,
    Execute,
}

impl ToolClass {
    fn operation<'a>(self, model: &'a str, method: &'a str) -> Option<OdooOp<'a>> {
        let (tool, verb, compatible) = match self {
            Self::Read => (ToolKind::Generic, Verb::Read, method_verb(method) == Verb::Read),
            Self::GenericCreate => (ToolKind::Generic, Verb::Create, method_verb(method) == Verb::Create),
            Self::GenericWrite => (ToolKind::Generic, Verb::Write, method_verb(method) == Verb::Write),
            Self::Activity => (ToolKind::Activity, Verb::Activity, method == "activity_schedule"),
            Self::Note => (ToolKind::Note, Verb::Note, method == "message_post"),
            Self::Message => (ToolKind::Message, Verb::Message, method == "message_post"),
            Self::Attach => (ToolKind::Attachment, Verb::Attach, method == "create"),
            Self::Execute => (ToolKind::Generic, method_verb(method), true),
        };
        (compatible && valid_policy_model(model) && valid_method(method))
            .then_some(OdooOp { tool, model, method, verb })
    }
}

/// Trusted availability facts, not credentials or a caller-selected identity.
/// A requester credential only counts when `agent.act_as_requester` is enabled.
/// Company eligibility is derived from `agent_id` and the administrator list.
#[derive(Debug, Clone, Copy, Default)]
pub struct PossibleIdentities {
    pub has_agent_credential: bool,
    pub avatar_has_owner: bool,
    pub owner_has_personal_credential: bool,
    pub requester_has_personal_credential: bool,
}

pub struct ListableInput<'a> {
    pub tool: ToolClass,
    /// None asks whether any model is authorized. Some restricts the predicate
    /// to the actual call or a tool's fixed model.
    pub model: Option<&'a str>,
    /// None asks whether any method in the tool class is authorized. A tool
    /// with a fixed RPC method must supply Some(method), even during listing.
    pub method: Option<&'a str>,
    /// Use the parent agent id for a temporary employee, as resolved upstream.
    pub agent_id: &'a str,
    pub agent: &'a AgentOdooConfig,
    pub access: &'a OdooAccessConfig,
    pub identities: PossibleIdentities,
}

/// Shared policy predicate for listing and dispatch prechecks. An operation
/// must survive F1-F5 and the administrator list, then be authorized under at
/// least one available identity. Engine uses the agent's model/action grants;
/// Follow skips them. ReadOnly intersects the inherited policy with read tools
/// so narrowing an Engine agent never widens its read permissions.
///
/// With no credential, an avatar with an owner or an act_as_requester agent
/// may still list authorized read tools under its prospective Personal mode.
/// This allows the runtime to explain how to connect. Missing credentials
/// never expose writes; an agent with no possible identity lists nothing.
/// This exception applies only when no actual identity is available: it must
/// not bypass an available Agent/Company identity's engine restrictions.
/// Fixed refusals, administrator rules and engine restrictions still apply.
///
/// None model/method is existential over syntactically valid names, not live
/// Odoo schema availability. Exact grants plus a fresh name outside the finite
/// deny patterns suffice to decide that policy question without I/O. Dispatch
/// must pass actual names, resolve the actual identity and enforce all other
/// gates; a true result is not a credential or a substitute for those gates.
pub fn odoo_tool_listable(input: &ListableInput<'_>) -> bool {
    if !duduclaw_core::is_valid_agent_id(input.agent_id) {
        return false;
    }
    let Ok(rules) = input.access.validated_rules() else { return false };
    let facts = input.identities;
    let personal = (facts.avatar_has_owner && facts.owner_has_personal_credential)
        || (input.agent.act_as_requester && facts.requester_has_personal_credential);
    let company = input.access.company_key_agents.iter().any(|id| id == input.agent_id);
    let prospective = facts.avatar_has_owner || input.agent.act_as_requester;
    let disconnected = !personal && !facts.has_agent_credential && !company;
    let identities = [
        (IdentityKind::Personal, personal || (disconnected && prospective && input.tool == ToolClass::Read)),
        (IdentityKind::Agent, facts.has_agent_credential),
        (IdentityKind::Company, company),
    ];
    let models = candidate_models(input, &rules);
    let methods = candidate_methods(input, &rules);
    models.iter().any(|model| methods.iter().any(|method| {
        let Some(op) = input.tool.operation(model, method) else { return false };
        if fixed_refusal(&op).is_some() || rules.iter().any(|rule| rule.matches(&op)) {
            return false;
        }
        identities.iter().any(|&(identity, available)| {
            if !available {
                return false;
            }
            let mode = effective_mode(identity, input.access, input.agent.permission_override_mode());
            match mode {
                EffectiveMode::Follow => true,
                EffectiveMode::Engine => engine_grants(input.agent, input.tool, &op),
                EffectiveMode::ReadOnly => input.tool == ToolClass::Read
                    && (effective_mode(identity, input.access, PermissionOverride::Inherit) == EffectiveMode::Follow
                        || engine_grants(input.agent, input.tool, &op)),
            }
        })
    }))
}

fn engine_grants(agent: &AgentOdooConfig, tool: ToolClass, op: &OdooOp<'_>) -> bool {
    if !agent.permits_model(op.model) {
        return false;
    }
    match tool {
        ToolClass::GenericCreate | ToolClass::GenericWrite => {
            agent.grants_explicitly(op.verb.as_str(), op.model)
                && agent.generic_opt_in(op.verb.as_str(), op.model)
        }
        ToolClass::Activity | ToolClass::Note | ToolClass::Message | ToolClass::Attach => {
            agent.dedicated_opt_in(op.verb.as_str(), op.model)
        }
        ToolClass::Read | ToolClass::Execute => {
            // Preserve existing engine action grants, while administrator
            // lists consistently see Execute for all workflow buttons.
            let action = if op.method.starts_with("action_") || op.method.starts_with("button_")
                || op.method.starts_with("toggle_")
            {
                "action"
            } else {
                op.verb.as_str()
            };
            agent.permits(action, op.model)
        }
    }
}

fn candidate_models(input: &ListableInput<'_>, rules: &[RefuseRule]) -> Vec<String> {
    if let Some(model) = input.model {
        return vec![model.to_owned()];
    }
    let mut models: BTreeSet<String> = input.agent.allowed_models.iter().cloned().collect();
    for token in &input.agent.allowed_actions {
        if let Some((_, model)) = token.split_once(':') {
            models.insert(model.to_owned());
        }
    }
    // A fresh root cannot match any exact/prefix deny pattern or fixed table.
    let mut roots: BTreeSet<String> = ["ir", "res", "mail"].into_iter().map(str::to_owned).collect();
    for rule in rules {
        if let RuleKind::Model { model: ModelPattern::Exact(name) | ModelPattern::Descendants(name), .. } = &rule.0 {
            roots.insert(name.split('.').next().unwrap_or(name).to_owned());
        }
    }
    models.insert(fresh_name("policy_model_", &roots));
    models.into_iter().collect()
}

const READ_METHODS: &[&str] = &[
    "read", "search", "search_read", "search_count", "fields_get", "name_get",
    "name_search", "default_get", "read_group",
];

fn candidate_methods(input: &ListableInput<'_>, rules: &[RefuseRule]) -> Vec<String> {
    if let Some(method) = input.method {
        return vec![method.to_owned()];
    }
    let fixed = match input.tool {
        ToolClass::Read => READ_METHODS,
        ToolClass::GenericCreate => &["create", "copy"],
        ToolClass::GenericWrite => &["write", "update"],
        ToolClass::Activity => &["activity_schedule"],
        ToolClass::Note | ToolClass::Message => &["message_post"],
        ToolClass::Attach => &["create"],
        ToolClass::Execute => &[],
    };
    if input.tool != ToolClass::Execute {
        return fixed.iter().map(|s| (*s).to_owned()).collect();
    }
    let blocked: BTreeSet<String> = rules.iter().filter_map(|rule| match &rule.0 {
        RuleKind::Method(method) => Some(method.clone()),
        _ => None,
    }).collect();
    let mut methods: Vec<String> = READ_METHODS.iter().copied()
        .chain(["create", "copy", "write", "update", "unlink"])
        .map(str::to_owned).collect();
    methods.push(fresh_name("policy_execute_", &blocked));
    methods.push(fresh_name("action_policy_", &blocked));
    methods
}

fn fresh_name(prefix: &str, occupied: &BTreeSet<String>) -> String {
    // At most N names are occupied, so one of these N+1 names is free.
    for index in 0..=occupied.len() {
        let name = format!("{prefix}{index}");
        if !occupied.contains(&name) {
            return name;
        }
    }
    unreachable!("a finite set cannot occupy N+1 distinct names")
}

/// The caller supplies a human-readable item, never a raw policy token.
pub fn list_refusal_message(item: &str) -> String {
    format!("公司設定的『一律不允許』清單不允許 AI 做這件事（{item}）。")
}

pub fn fixed_refusal_message(refusal: FixedRefusal) -> String {
    format!("DuDuClaw 不讓 AI 做這件事：{}。", refusal.reason())
}

pub fn not_connected_message(avatar_or_requester: bool) -> &'static str {
    if avatar_or_requester {
        "你還沒有連接自己的 Odoo 帳號，請到『我的連線』連接。"
    } else {
        "這位 AI 員工沒有連接 Odoo，請管理員到 Odoo 設定指定帳號。"
    }
}

#[cfg(test)]
#[path = "access_policy_tests.rs"]
mod tests;
