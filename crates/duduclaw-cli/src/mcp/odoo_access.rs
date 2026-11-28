//! RM-141 MCP policy adapter. Local availability never selects a runtime mode.
use std::path::Path;
use serde_json::Value;
use duduclaw_odoo::{AgentOdooConfig, OdooConfig};
use duduclaw_odoo::access_policy::{self as policy, *};
use super::odoo_identity_client::{IdentityKind as DecisionKind, IdentityPermission, OdooIdentity};

const UNREADABLE: &str = "Odoo 權限設定讀不到或格式有誤，請管理員檢查設定後再試。";
const DENIED: &str = "這位 AI 員工目前沒有這項 Odoo 操作權限。";

/// Bound connectors always intersect Engine policy, even without task-local
/// isolation context (for example when called directly by a host adapter).
pub(crate) fn odoo_bound_preconnect(home: &Path, agent: &str, tool: &str, params: &Value)
    -> Result<(), String> {
    ToolPolicy::load(home, agent)?.data_preconnect_engine(tool, params)
}

#[derive(Clone)]
pub(super) struct ToolPolicy {
    pub person_key: bool,
    pub agent_id: String,
    pub agent: AgentOdooConfig,
    pub access: OdooAccessConfig,
    pub global: OdooConfig,
    pub identities: PossibleIdentities,
}

/// Metadata only, one read-only connection; never select credential ciphertext.
/// A damaged/missing database cannot create a possible personal identity.
pub(super) fn personal_facts(home: &Path, agent: &str) -> PossibleIdentities {
    personal_facts_checked(home, agent).unwrap_or_default()
}

fn personal_facts_checked(home: &Path, agent: &str) -> rusqlite::Result<PossibleIdentities> {
        let db = rusqlite::Connection::open_with_flags(home.join("users.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        db.query_row("SELECT
            (SELECT count(*) FROM users u WHERE u.status='active' AND u.avatar_agent_id=?1
                AND EXISTS(SELECT 1 FROM user_agent_bindings b WHERE b.user_id=u.id
                    AND b.agent_name=?1 AND b.access_level IN ('owner','operator'))),
            EXISTS(SELECT 1 FROM users u JOIN member_odoo_credentials c ON c.user_id=u.id
                WHERE u.status='active' AND u.avatar_agent_id=?1 AND c.state='ok'
                AND c.credential_epoch>=0 AND c.credential_epoch=u.credential_epoch
                AND EXISTS(SELECT 1 FROM user_agent_bindings b WHERE b.user_id=u.id
                    AND b.agent_name=?1 AND b.access_level IN ('owner','operator'))),
            EXISTS(SELECT 1 FROM users u JOIN member_odoo_credentials c ON c.user_id=u.id
                WHERE u.status='active' AND c.state='ok'
                AND c.credential_epoch>=0 AND c.credential_epoch=u.credential_epoch)", [agent], |row| {
            let owners: i64 = row.get(0)?;
            Ok(PossibleIdentities {
                avatar_has_owner: owners == 1,
                owner_has_personal_credential: owners == 1 && row.get::<_, bool>(1)?,
                // An avatar cannot borrow a different requester's identity.
                requester_has_personal_credential: owners == 0 && row.get::<_, bool>(2)?,
                ..Default::default()
            })
        })
}

/// Fixed methods must also be supplied during listing. Existing dedicated
/// business tools use Execute (ordinary grants), not generic-write opt-ins.
pub(super) fn operation<'a>(tool: &str, params: Option<&'a Value>) -> Option<(ToolClass, Option<&'a str>, Option<&'a str>)> {
    let model = params.and_then(|p| p.get("model")).and_then(Value::as_str);
    Some(match tool {
        "odoo_connect" | "odoo_status" => (ToolClass::Read, None, Some("read")),
        "odoo_search" => (ToolClass::Read, model, Some("search_read")),
        "odoo_schema_fields" | "odoo_schema_overview" => (ToolClass::Read, model.filter(|s| !s.is_empty()), Some("fields_get")),
        "odoo_create" => (ToolClass::GenericCreate, model.map(str::trim), Some("create")),
        "odoo_write" => (ToolClass::GenericWrite, model.map(str::trim), Some("write")),
        "odoo_activity_create" => (ToolClass::Activity, model, Some("activity_schedule")),
        "odoo_note_post" => (ToolClass::Note, model, Some("message_post")),
        "odoo_message_send" => (ToolClass::Message, model, Some("message_post")),
        "odoo_attachment_add" => (ToolClass::Attach, model, Some("create")),
        "odoo_execute" => (ToolClass::Execute, model, params.and_then(|p| p.get("method")).and_then(Value::as_str)),
        "odoo_crm_leads" => (ToolClass::Read, Some("crm.lead"), Some("search_read")),
        "odoo_crm_create_lead" => (ToolClass::Execute, Some("crm.lead"), Some("create")),
        "odoo_crm_update_stage" => (ToolClass::Execute, Some("crm.lead"), Some("write")),
        "odoo_sale_orders" => (ToolClass::Read, Some("sale.order"), Some("search_read")),
        "odoo_sale_create_quotation" => (ToolClass::Execute, Some("sale.order"), Some("create")),
        "odoo_sale_confirm" => (ToolClass::Execute, Some("sale.order"), Some("action_confirm")),
        "odoo_inventory_products" => (ToolClass::Read, Some("product.product"), Some("search_read")),
        "odoo_inventory_check" => (ToolClass::Read, Some("stock.quant"), Some("search_read")),
        "odoo_invoice_list" | "odoo_payment_status" => (ToolClass::Read, Some("account.move"), Some("search_read")),
        "odoo_partner_search" => (ToolClass::Read, Some("res.partner"), Some("search_read")),
        "odoo_report" => (ToolClass::Execute, Some("ir.actions.report"), Some("render_qweb_pdf")),
        _ => return None,
    })
}

impl ToolPolicy {
    pub fn load(home: &Path, agent_id: &str) -> Result<Self, String> {
        Self::load_inner(home, agent_id, false)
    }

    pub fn load_for_approval(home: &Path, agent_id: &str) -> Result<Self, String> {
        Self::load_inner(home, agent_id, true)
    }

    fn load_inner(home: &Path, agent_id: &str, strict_identity: bool) -> Result<Self, String> {
        let raw = std::fs::read_to_string(home.join("config.toml")).map_err(|_| UNREADABLE)?;
        let access = OdooAccessConfig::parse_config_toml(&raw)?;
        let table: toml::Table = raw.parse().map_err(|_| UNREADABLE)?;
        let global = table.get("odoo").cloned().ok_or(UNREADABLE)?
            .try_into::<OdooConfig>().map_err(|_| UNREADABLE)?;
        let agent = if strict_identity {
            // Unlike availability, an absent employee file or database cannot
            // turn an unknown approval mode into Engine.
            let raw = std::fs::read_to_string(super::caller_agent_dir(home, agent_id).join("agent.toml"))
                .map_err(|_| UNREADABLE)?;
            AgentOdooConfig::parse_agent_toml(&raw)?.unwrap_or_default()
        } else {
            super::odoo_generic_write::load_generic_write_override(home, agent_id)?.unwrap_or_default()
        };
        let mut identities = if strict_identity {
            personal_facts_checked(home, agent_id).map_err(|_| UNREADABLE)?
        } else { personal_facts(home, agent_id) };
        // The global member-credential switch is not an identity grant.
        if table.get("odoo").and_then(|v| v.get("member_credentials")).and_then(toml::Value::as_bool) != Some(true) {
            identities.owner_has_personal_credential = false;
            identities.requester_has_personal_credential = false;
        }
        let populated = |value: &Option<String>| value.as_ref().is_some_and(|s| !s.trim().is_empty());
        identities.has_agent_credential = populated(&agent.username)
            && (populated(&agent.api_key_enc) || populated(&agent.password_enc));
        let chain = duduclaw_core::autonomy_gate::effective_identities(home, agent_id,
            duduclaw_core::autonomy_gate::AutonomyPath::Dispatcher).map_err(|_| DENIED)?;
        let parent = chain.last().ok_or(DENIED)?.clone();
        Ok(Self { person_key: false, agent_id: parent, agent, access, global, identities })
    }

    /// Personal keys have no employee policy, autonomy chain or local identity facts.
    pub fn load_person(home: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(home.join("config.toml")).map_err(|_| UNREADABLE)?;
        let access = OdooAccessConfig::parse_config_toml(&raw)?;
        let mut table: toml::Table = raw.parse().map_err(|_| UNREADABLE)?;
        let mut config = table.remove("odoo").ok_or(UNREADABLE)?;
        let fields = config.as_table_mut().ok_or(UNREADABLE)?;
        // Keep endpoint and feature configuration, never a configured account.
        for key in ["username", "api_key_enc", "password_enc"] { fields.remove(key); }
        let global = config.try_into::<OdooConfig>().map_err(|_| UNREADABLE)?;
        Ok(Self { person_key: true, agent_id: String::new(), agent: AgentOdooConfig::default(),
            access, global, identities: PossibleIdentities::default() })
    }

    /// The gateway uses this same effective_mode (including the acknowledgement)
    /// for person keys. Availability never supplies a runtime identity.
    pub fn person_listed(&self, tool: &str, params: Option<&Value>) -> bool {
        use duduclaw_core::mcp_person_tools::{person_tool, PersonToolHost};
        if !person_tool(tool).is_some_and(|t| t.executes_in == PersonToolHost::McpWithPersonalOdoo) {
            return false;
        }
        let Some((class, model, method)) = operation(tool, params) else { return false };
        if class != ToolClass::Read && effective_mode(policy::IdentityKind::Personal,
            &self.access, PermissionOverride::Inherit) != EffectiveMode::Follow { return false; }
        let candidates: Vec<String> = model.map(|m| vec![m.to_owned()]).unwrap_or_else(||
            (0..=self.access.always_refuse.len()).map(|n| format!("policy_model_{n}")).collect());
        let Some(method) = method else { return false };
        candidates.iter().any(|model| self.common_tool(tool, model, method).is_ok()
            && (method != "search_read" || self.common(model, "fields_get").is_ok()))
            && self.auxiliary(tool).iter().all(|(m, v)|
                self.common(m, v).is_ok() && self.common(m, "fields_get").is_ok())
    }

    fn check_person_call(&self, tool: &str, params: &Value, decision: Option<&OdooIdentity>) -> Result<(), String> {
        use duduclaw_core::mcp_person_tools::{person_tool, PersonToolHost};
        if !person_tool(tool).is_some_and(|t| t.executes_in == PersonToolHost::McpWithPersonalOdoo) {
            return Err("本人金鑰不提供這項 Odoo 工具。".into());
        }
        let decision = decision.filter(|d| d.kind == DecisionKind::Personal
            && d.permission != IdentityPermission::Engine).ok_or("無法確認本人 Odoo 權限。")?;
        let (class, model, method) = operation(tool, Some(params)).ok_or(DENIED)?;
        if decision.permission == IdentityPermission::ReadOnly && class != ToolClass::Read {
            return Err("目前是唯讀模式，只能查詢 Odoo 資料，不能新增或修改。".into());
        }
        if let (Some(model), Some(method)) = (model, method) {
            self.common_tool(tool, model, method)?;
            if method == "search_read" { self.common(model, "fields_get")?; }
        }
        for (model, method) in self.auxiliary(tool) {
            self.common(model, method)?;
            self.common(model, "fields_get")?;
        }
        if !self.person_listed(tool, Some(params)) { return Err(DENIED.into()); }
        Ok(())
    }

    /// Preserve the old dedicated tools' search/execute grant vocabulary while
    /// the pure policy sees the actual RPC method (search_read/action_confirm).
    fn predicate_agent(&self, tool: &str) -> AgentOdooConfig {
        let mut agent = self.agent.clone();
        let alias = match tool {
            "odoo_search" | "odoo_crm_leads" | "odoo_sale_orders" | "odoo_inventory_products"
            | "odoo_inventory_check" | "odoo_invoice_list" | "odoo_payment_status" => Some(("search", "read")),
            "odoo_sale_confirm" => Some(("execute", "action")),
            _ => None,
        };
        if let Some((from, to)) = alias {
            agent.allowed_actions = agent.allowed_actions.iter().filter_map(|token| {
                let (verb, suffix) = token.split_once(':').map_or((token.as_str(), None), |(v,m)| (v,Some(m)));
                if verb == to { return None; }
                Some(if verb == from { suffix.map_or_else(|| to.to_owned(), |m| format!("{to}:{m}")) } else { token.clone() })
            }).collect();
            // Empty means unrestricted in the legacy engine, so preserve a
            // nonempty deny set when removing its only inapplicable token.
            if !self.agent.allowed_actions.is_empty() && agent.allowed_actions.is_empty() {
                agent.allowed_actions.push("no_permission".into());
            }
        }
        agent
    }

    fn pure(&self, tool: &str, class: ToolClass, model: Option<&str>, method: Option<&str>, engine: bool) -> bool {
        let agent = self.predicate_agent(tool);
        let mut access = self.access.clone();
        let mut identities = self.identities;
        if engine {
            // This is an intersection with a gateway Engine decision, never a
            // locally selected effective mode or an invented runtime account.
            access.permission_mode = PermissionMode::Engine;
            access.company_key_agents = vec![self.agent_id.clone()];
            identities = PossibleIdentities::default();
        }
        policy::odoo_tool_listable(&ListableInput { tool: class, model, method,
            agent_id: &self.agent_id, agent: &agent, access: &access, identities })
    }

    fn personal_available(&self) -> bool {
        (self.identities.avatar_has_owner && self.identities.owner_has_personal_credential)
            || (self.agent.act_as_requester && self.identities.requester_has_personal_credential)
    }

    /// Same personal candidates as listing/preconnect. Actual personal
    /// credentials precede employee/company fallbacks at the gateway.
    pub fn approval_may_follow(&self) -> bool {
        self.personal_available() && effective_mode(policy::IdentityKind::Personal,
            &self.access, self.agent.permission_override_mode()) == EffectiveMode::Follow
    }

    fn prospective_follow(&self) -> bool {
        let personal = self.personal_available();
        let disconnected = !personal && !self.identities.has_agent_credential
            && !self.access.company_key_agents.iter().any(|id| id == &self.agent_id);
        (personal || (disconnected && (self.identities.avatar_has_owner || self.agent.act_as_requester)))
            && effective_mode(policy::IdentityKind::Personal, &self.access, PermissionOverride::Inherit) == EffectiveMode::Follow
            && self.agent.permission_override_mode() != PermissionOverride::Engine
    }

    pub(super) fn common(&self, model: &str, method: &str) -> Result<(), String> {
        self.common_operation(ToolKind::Generic, method_verb(method), model, method)
    }

    fn common_tool(&self, tool: &str, model: &str, method: &str) -> Result<(), String> {
        let (kind, verb) = match tool {
            "odoo_activity_create" => (ToolKind::Activity, Verb::Activity),
            "odoo_note_post" => (ToolKind::Note, Verb::Note),
            "odoo_message_send" => (ToolKind::Message, Verb::Message),
            "odoo_attachment_add" => (ToolKind::Attachment, Verb::Attach),
            _ => (ToolKind::Generic, method_verb(method)),
        };
        self.common_operation(kind, verb, model, method)?;
        // The dedicated tools read the target before writing. This read is
        // intrinsic to the dedicated grant, but still obeys fixed/company gates.
        if matches!(kind, ToolKind::Activity | ToolKind::Note | ToolKind::Message | ToolKind::Attachment) {
            self.common(model, "search_read")?;
            self.common(model, "fields_get")?;
        }
        Ok(())
    }

    fn common_operation(&self, tool: ToolKind, verb: Verb, model: &str, method: &str) -> Result<(), String> {
        let op = OdooOp { tool, model, method, verb };
        if let Some(refusal) = fixed_refusal(&op) { return Err(fixed_refusal_message(refusal)); }
        if self.access.always_refuses(&op) {
            return Err(list_refusal_message(&format!("{}：{model}", match op.verb {
                Verb::Read => "讀取資料", Verb::Create => "新增資料", Verb::Write => "修改資料",
                Verb::Unlink => "刪除資料", _ => "執行操作",
            })));
        }
        self.global.feature_allows_model(model).map_err(|module| format!(
            "公司目前沒有開啟這項 Odoo 功能模組（{module}）；Odoo feature_gate: module '{module}' is switched off。"))
    }

    fn engine_model(&self, tool: &str, model: &str, method: &str) -> bool {
        let unblock = self.agent.unblock_models.as_ref().unwrap_or(&self.global.unblock_models);
        if let Some(verb) = super::odoo_generic_write::generic_write_verb(tool) {
            return super::odoo_generic_write::generic_write_model_gate(Some(&self.agent), &self.global, unblock, verb, model).is_ok();
        }
        if matches!(tool, "odoo_search" | "odoo_execute" | "odoo_schema_fields" | "odoo_schema_overview"
            | "odoo_activity_create" | "odoo_note_post" | "odoo_message_send" | "odoo_attachment_add") {
            let kind = if method_verb(method) == Verb::Read { duduclaw_odoo::AccessKind::Read } else { duduclaw_odoo::AccessKind::Mutate };
            if duduclaw_odoo::check_blocklist(model, kind, unblock).is_err() { return false; }
        }
        tool != "odoo_execute" || !super::odoo::blocked_odoo_method(method)
    }

    /// Isolation currently supplies a bound connector without a permission
    /// decision. Its listing must intersect the same conservative Engine
    /// policy as bound dispatch, rather than advertise Follow-only writes.
    pub fn listed_for_context(&self, tool: &str) -> bool {
        if crate::mcp_isolation::current().is_some() {
            let mut engine = self.clone();
            engine.access.permission_mode = PermissionMode::Engine;
            engine.listed(tool, None)
        } else {
            self.listed(tool, None)
        }
    }

    pub fn listed(&self, tool: &str, params: Option<&Value>) -> bool {
        let Some((class, model, method)) = operation(tool, params) else { return false };
        // Quantify over actual candidates so feature/block gates and the pure
        // predicate cannot authorize two different models independently.
        let mut candidates = self.agent.allowed_models.clone();
        candidates.extend(self.agent.allowed_actions.iter().filter_map(|s| s.split_once(':').map(|(_,m)| m.to_owned())));
        for n in 0..=self.access.always_refuse.len() {
            candidates.push(format!("policy_model_{n}"));
        }
        if let Some(model) = model { candidates = vec![model.to_owned()]; }
        let mut methods: Vec<String> = ["read", "search", "search_read", "search_count", "fields_get", "name_get",
            "name_search", "default_get", "read_group", "create", "copy", "write", "update", "unlink"]
            .into_iter().map(str::to_owned).collect();
        for n in 0..=self.access.always_refuse.len() {
            methods.push(format!("policy_execute_{n}"));
            methods.push(format!("action_policy_{n}"));
        }
        if let Some(method) = method { methods = vec![method.to_owned()]; }
        let allowed = candidates.iter().any(|model| methods.iter().any(|method| {
            self.common_tool(tool, model, method).is_ok()
                && (method != "search_read" || self.common(model, "fields_get").is_ok())
                && self.pure(tool, class, Some(model), Some(method), false)
                && (self.prospective_follow() || self.engine_model(tool, model, method))
        }));
        allowed && self.auxiliary(tool).iter().all(|(model, method)| {
            self.common(model, method).is_ok() && self.common(model, "fields_get").is_ok()
                && self.pure("odoo_search", ToolClass::Read, Some(model), Some(method), false)
        })
    }

    fn auxiliary(&self, tool: &str) -> Vec<(&'static str, &'static str)> {
        match tool {
            "odoo_inventory_check" => vec![("product.product", "search_read")],
            "odoo_crm_update_stage" => vec![("crm.stage", "search_read")],
            _ => vec![],
        }
    }

    /// Refuse locally before asking the gateway to select an identity. This
    /// does not select an account or authorize I/O; check_call repeats policy
    /// against the actual decision before connection.
    pub fn data_preconnect(&self, tool: &str, params: &Value,
        registered: Option<AgentOdooConfig>) -> Result<(), String> {
        self.data_preconnect_mode(tool, params, registered, crate::mcp_isolation::current().is_some())
    }

    pub fn data_preconnect_engine(&self, tool: &str, params: &Value) -> Result<(), String> {
        self.data_preconnect_mode(tool, params, None, true)
    }

    fn data_preconnect_mode(&self, tool: &str, params: &Value,
        registered: Option<AgentOdooConfig>, isolation: bool) -> Result<(), String> {
        let mut local = self.clone();
        if let Some(agent) = registered { local.agent = agent; }
        let (_, model, method) = operation(tool, Some(params)).ok_or(DENIED)?;
        if let (Some(model), Some(method)) = (model, method) {
            // Preserve the established diagnostic for the immutable metadata fence.
            if fixed_refusal(&OdooOp { tool: ToolKind::Generic, model, method,
                verb: method_verb(method) }) == Some(FixedRefusal::F2) {
                return Err(super::odoo::blocklist_denial_message(model, duduclaw_odoo::BlockDenial::SystemReadOnly));
            }
            local.common_tool(tool, model, method)?;
            if method == "search_read" { local.common(model, "fields_get")?; }
        }
        for (model, method) in local.auxiliary(tool) { local.common(model, method)?; }
        // A possible Follow identity must still be decided by the gateway.
        // Isolation has no personal handoff and always intersects Engine gates.
        if isolation || !local.prospective_follow() {
            if tool == "odoo_execute" && let Some(method) = method {
                if super::odoo::blocked_odoo_method(method) {
                    return Err(super::odoo::method_denial_message(method));
                }
            }
            if let Some((verb, model)) = super::odoo::classify_odoo_call(tool, params) {
                crate::odoo_pool::check_action_permission(Some(&local.agent), verb, &model)
                    .map_err(|reason| format!("Odoo permission denied: {reason}"))?;
                if matches!(tool, "odoo_search" | "odoo_execute" | "odoo_schema_fields" | "odoo_schema_overview") {
                    let kind = if matches!(verb, "read" | "search") {
                        duduclaw_odoo::AccessKind::Read
                    } else { duduclaw_odoo::AccessKind::Mutate };
                    let unblock = local.agent.unblock_models.as_ref().unwrap_or(&local.global.unblock_models);
                    duduclaw_odoo::check_blocklist(&model, kind, unblock)
                        .map_err(|denial| super::odoo::blocklist_denial_message(&model, denial))?;
                }
            }
            for (model, _) in local.auxiliary(tool) {
                crate::odoo_pool::check_action_permission(Some(&local.agent), "search", model)
                    .map_err(|reason| format!("Odoo permission denied: {reason}"))?;
            }
        }
        // Includes the pure Engine table/action predicate and dedicated tool
        // grants. Possible Follow credentials must not relax bound execution.
        if isolation { local.check_call(tool, params, None)?; }
        if !local.identities.has_agent_credential
            && !local.identities.owner_has_personal_credential
            && !(local.agent.act_as_requester && local.identities.requester_has_personal_credential)
            && !local.access.company_key_agents.iter().any(|id| id == &local.agent_id) {
            let person = local.identities.avatar_has_owner || local.agent.act_as_requester;
            if person && !local.global.member_credentials {
                return Err(MEMBER_CREDENTIALS_OFF.into());
            }
            return Err(format!("Odoo not connected. {}", not_connected_message(person)));
        }
        local.precheck(tool, params)
    }

    pub fn precheck(&self, tool: &str, params: &Value) -> Result<(), String> {
        let (_, model, method) = operation(tool, Some(params)).ok_or(DENIED)?;
        if let (Some(model), Some(method)) = (model, method) {
            self.common_tool(tool, model, method)?;
            if method == "search_read" { self.common(model, "fields_get")?; }
        }
        for (model, method) in self.auxiliary(tool) { self.common(model, method)?; }
        if self.listed(tool, Some(params)) { Ok(()) } else { Err(DENIED.into()) }
    }

    pub fn check_call(&self, tool: &str, params: &Value, decision: Option<&OdooIdentity>) -> Result<(), String> {
        if self.person_key { return self.check_person_call(tool, params, decision); }
        let (class, model, method) = operation(tool, Some(params)).ok_or(DENIED)?;
        if let (Some(model), Some(method)) = (model, method) {
            self.common_tool(tool, model, method)?;
            if method == "search_read" { self.common(model, "fields_get")?; }
        }
        let permission = decision.map_or(IdentityPermission::Engine, |d| d.permission);
        if permission == IdentityPermission::ReadOnly && class != ToolClass::Read {
            return Err("目前是唯讀模式，只能查詢 Odoo 資料，不能新增、修改或執行操作。".into());
        }
        if permission == IdentityPermission::Follow && decision.is_none_or(|d| d.kind != DecisionKind::Personal) {
            return Err(DENIED.into());
        }
        if permission == IdentityPermission::Engine {
            if tool == "odoo_activity_create" && params.get("user_id").is_some() {
                return Err("引擎模式不接受 user_id；活動只能指派給連線帳號本人。".into());
            }
            if let (Some(model), Some(method)) = (model, method) {
                if let Some(verb) = super::odoo_generic_write::generic_write_verb(tool) {
                    let unblock = self.agent.unblock_models.as_ref().unwrap_or(&self.global.unblock_models);
                    super::odoo_generic_write::generic_write_model_gate(Some(&self.agent), &self.global, unblock, verb, model)
                        .map_err(|denial| denial.message(tool, model))?;
                } else if let Some((verb, old_model)) = super::odoo::classify_odoo_call(tool, params) {
                    crate::odoo_pool::check_action_permission(Some(&self.agent), verb, &old_model)?;
                }
                if !self.engine_model(tool, model, method) { return Err(DENIED.into()); }
            }
            if !self.pure(tool, class, model, method, true) { return Err(DENIED.into()); }
            if let (Some(model), Some(method)) = (model, method) {
                if !self.engine_model(tool, model, method) { return Err(DENIED.into()); }
            }
        }
        self.precheck(tool, params)?;
        if permission == IdentityPermission::ReadOnly {
            let mut access = self.access.clone();
            access.company_key_agents.clear();
            let mut facts = PossibleIdentities::default();
            match decision.map(|d| d.kind) {
                Some(DecisionKind::Personal) => {
                    facts.avatar_has_owner = true;
                    facts.owner_has_personal_credential = true;
                }
                Some(DecisionKind::Agent) => facts.has_agent_credential = true,
                _ => access.company_key_agents.push(self.agent_id.clone()),
            }
            let mut agent = self.predicate_agent(tool);
            agent.permission_override = Some("read_only".into());
            if !odoo_tool_listable(&ListableInput { tool: class, model, method,
                agent_id: &self.agent_id, agent: &agent, access: &access, identities: facts }) {
                return Err(DENIED.into());
            }
            // ReadOnly narrows the selected identity's inherited read policy.
            // A possible personal account cannot relax a selected company or
            // agent account's sensitive-table gates.
            let inherited_follow = decision.is_some_and(|d| d.kind == DecisionKind::Personal)
                && effective_mode(policy::IdentityKind::Personal, &self.access, PermissionOverride::Inherit) == EffectiveMode::Follow;
            if !inherited_follow {
                if let (Some(model), Some(method)) = (model, method) {
                    if !self.engine_model(tool, model, method) { return Err(DENIED.into()); }
                }
            }
        }
        for (model, method) in self.auxiliary(tool) {
            self.common(model, method)?;
            self.common(model, "fields_get")?;
            if permission == IdentityPermission::Engine
                && !self.pure("odoo_search", ToolClass::Read, Some(model), Some(method), true) { return Err(DENIED.into()); }
        }
        Ok(())
    }

    pub fn schema_visible(&self, model: &str, decision: Option<&OdooIdentity>) -> bool {
        self.check_call("odoo_schema_fields", &serde_json::json!({"model":model}), decision).is_ok()
    }

}

pub(super) fn follows(decision: Option<&OdooIdentity>) -> bool {
    decision.is_some_and(|d| d.kind == DecisionKind::Personal && d.permission == IdentityPermission::Follow)
}

pub(super) fn identity_suffix(decision: Option<&OdooIdentity>, verb: &str) -> String {
    let action = match verb { "create" => "建立", "write" => "修改", _ => "執行" };
    let text = match decision.map(|d| d.kind) {
        Some(DecisionKind::Personal) => format!("以你的 Odoo 帳號 {} {action}", decision.and_then(|d| d.login.as_deref()).unwrap_or("本人")),
        Some(DecisionKind::Company) => format!("以公司帳號{action}"),
        _ => format!("以 AI 員工帳號{action}"),
    };
    duduclaw_security::audit::mask_sensitive_text(&text)
}

pub(super) fn error_text(error: &str, decision: Option<&OdooIdentity>) -> String {
    if follows(decision) {
        let classified = error.strip_prefix("Odoo field compatibility: fields_get failed for '")
            .and_then(|rest| rest.split_once("': ").map(|(_, error)| error)).unwrap_or(error);
        if let Some(summary) = classified.strip_prefix(duduclaw_odoo::rpc::PERSONAL_ACCESS_ERROR_PREFIX) {
            let summary = decision.and_then(|d| d.secret.as_deref()).filter(|s| !s.is_empty())
                .map_or_else(|| summary.to_owned(), |secret| summary.replace(secret.as_str(), "[REDACTED]"));
            let summary = duduclaw_core::truncate_chars(&summary, 200);
            let login = decision.and_then(|d| d.login.as_deref()).unwrap_or("本人");
            return duduclaw_security::audit::mask_sensitive_text(&format!(
                "你的 Odoo 帳號（{login}）沒有權限做這件事：{summary}。這是 Odoo 帳號本身的權限，要調整請找公司的 Odoo 管理員。"));
        }
    }
    format!("Odoo error: {error}")
}

pub(super) fn sanitize_for_session(mut result: Value, session: Option<&str>) -> Value {
    use duduclaw_gateway::session_origin::{origin_for_current_turn, OriginKind};
    let public = session.is_some_and(|s| {
        duduclaw_gateway::odoo_livechat::is_public_visitor_session(s)
            || matches!(origin_for_current_turn(s).kind(), OriginKind::PublicVisitor | OriginKind::WebchatVisitor | OriginKind::Channel)
            || s.split_once(':').is_some_and(|(prefix, tail)| prefix == "widget-visitor" && !tail.is_empty())
    });
    if public && result["isError"] == true {
        return super::mcp_error(duduclaw_gateway::odoo_livechat::VISITOR_FALLBACK_MESSAGE);
    }
    if public {
        // Audit retains attribution, but external conversations receive only
        // the business result, never a personal login or operator instruction.
        if let Some(items) = result.get_mut("content").and_then(Value::as_array_mut) {
            for item in items {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    if let Some((body, suffix)) = text.rsplit_once('\n') {
                        if ["以你的 Odoo 帳號 ", "以公司帳號", "以 AI 員工帳號"].iter().any(|prefix| suffix.starts_with(prefix)) {
                            item["text"] = Value::String(body.to_owned());
                        }
                    }
                }
            }
        }
    }
    result
}

pub(super) fn sanitize_result(result: Value) -> Value {
    let session = crate::mcp_isolation_identity::var(duduclaw_core::ENV_TRUST_SESSION_ID).ok()
        .or_else(|| crate::mcp_isolation_identity::var(duduclaw_core::ENV_DISPATCH_SESSION).ok());
    sanitize_for_session(result, session.as_deref())
}
