//! Admin template library. Resolved files never come from client-supplied paths.
use super::*;
use crate::company_templates as company;
use crate::premium_templates::{self as pt, AssembledRole, RoleKind};
use std::collections::BTreeMap;

const BUILTINS: &[(&str, &str, &str, &str)] = &[
    ("restaurant", include_str!("../../../../templates/restaurant/agent.toml"), include_str!("../../../../templates/restaurant/SOUL.md"), include_str!("../../../../templates/restaurant/CONTRACT.toml")),
    ("manufacturing", include_str!("../../../../templates/manufacturing/agent.toml"), include_str!("../../../../templates/manufacturing/SOUL.md"), include_str!("../../../../templates/manufacturing/CONTRACT.toml")),
    ("trading", include_str!("../../../../templates/trading/agent.toml"), include_str!("../../../../templates/trading/SOUL.md"), include_str!("../../../../templates/trading/CONTRACT.toml")),
    ("evaluator", include_str!("../../../../templates/evaluator/agent.toml"), include_str!("../../../../templates/evaluator/SOUL.md"), include_str!("../../../../templates/evaluator/CONTRACT.toml")),
];

struct LibraryRole {
    role: AssembledRole,
    skills: Option<BTreeMap<String, String>>,
    premium: bool,
    disabled_skills: Vec<String>,
    revision: Option<u64>,
}

fn role_from_files(id: &str, name: &str, description: &str, config: String, soul: String, contract: String) -> AssembledRole {
    AssembledRole {
        role_id: id.into(), kind: RoleKind::Worker, name: name.into(), display_name: name.into(),
        trigger: String::new(), reports_to: String::new(), summary: description.into(),
        soul_md: soul, contract_toml: contract, agent_toml: config, extras_dir: None,
    }
}

pub(super) fn bundled_skills() -> BTreeMap<String,String> {
    use duduclaw_agent::builtin_skills::{BUILTIN_SKILLS, BUILTIN_SKILL_FILES};
    let mut skills: BTreeMap<_,_> = BUILTIN_SKILLS.iter().map(|(name,body)|(name.to_string(),body.to_string())).collect();
    for (name, files) in BUILTIN_SKILL_FILES {
        if let Some((_,body)) = files.iter().find(|(path,_)|*path=="SKILL.md") {
            skills.insert(name.to_string(),body.to_string());
        }
    }
    skills
}

fn detail(id: &str, value: &LibraryRole) -> Result<Value, String> {
    let table: toml::Table = value.role.agent_toml.parse().map_err(|_| "Invalid template configuration")?;
    let field = |section: &str, key: &str| table.get(section).and_then(|v| v.get(key)).cloned();
    Ok(json!({
        "id":id, "kind":"agent", "source":if value.revision.is_some() {"company"} else {"builtin"},
        "name":value.role.display_name, "description":value.role.summary,
        "soul_md":value.role.soul_md,
        "contract_toml":value.role.contract_toml,
        "skill_contents":value.skills,
        "skills":value.skills.as_ref().map(|s|s.keys().cloned().collect::<Vec<_>>()).unwrap_or_default(),
        "allowed_tools":field("capabilities","allowed_tools").unwrap_or(toml::Value::Array(vec![])),
        "denied_tools":field("capabilities","denied_tools").unwrap_or(toml::Value::Array(vec![])),
        "permissions":table.get("permissions"),
        "model":field("model","preferred").and_then(|v|v.as_str().map(str::to_owned)).unwrap_or_default(),
        "runtime":field("runtime","provider").and_then(|v|v.as_str().map(str::to_owned)).unwrap_or_default(),
        "revision":value.revision, "premium":value.premium,
    }))
}

fn read_profile_file(dir: &Path, name: &str, optional: bool) -> Result<String, String> {
    let path = dir.join(name);
    match std::fs::symlink_metadata(&path) {
        Err(e) if optional && e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() && meta.len() <= 512 * 1024 =>
            company::read_portable_file(&path).map_err(|_| format!("{name} 無法安全讀取，已拒絕複製。")),
        _ => Err(format!("{name} 必須是可安全讀取且大小受限的一般檔案，已拒絕複製。")),
    }
}

impl MethodHandler {
    async fn library_role(&self, id: &str) -> Result<LibraryRole, String> {
        let parts: Vec<_> = id.split(':').collect();
        let result = match parts.as_slice() {
            ["builtin", slug] => {
                let (_, config, soul, contract) = BUILTINS.iter().find(|(key, ..)| key == slug).ok_or("Unknown template")?;
                let table: toml::Table = config.parse().map_err(|_| "Invalid template")?;
                let name = table.get("agent").and_then(|v|v.get("display_name")).and_then(|v|v.as_str()).unwrap_or(slug);
                let config = company::project(config)?;
                LibraryRole { role: role_from_files(id, name, &duduclaw_core::truncate_chars(soul, 180), config.clone(), soul.to_string(), contract.to_string()),
                    skills: Some(bundled_skills()), premium: false, disabled_skills: vec![], revision: None }
            }
            ["company", key] => {
                let item = company::get(&self.home_dir, key)?;
                if !item.members.is_empty() { return Err("Choose a team template".into()); }
                if item.premium && !self.premium_templates_unlocked().await { return Err("Premium templates are locked".into()); }
                LibraryRole { role: role_from_files(id, &item.name, &item.description, item.agent_toml, item.soul_md, item.contract_toml),
                    skills: Some(item.skills), premium: item.premium, disabled_skills: item.disabled_skills, revision: Some(item.revision) }
            }
            ["premium", industry, role_id] => {
                let dir = self.premium_dir_unlocked().await.map_err(|_| "Premium templates are locked")?;
                let role = self.assemble_template_role(&dir, &json!({"industry":industry}), role_id).await?;
                LibraryRole { role, skills: Some(bundled_skills()), premium: true, disabled_skills: vec![], revision: None }
            }
            _ => return Err("Unknown template".into()),
        };
        self.validate_template_policy(&result.role)?;
        Ok(result)
    }

    async fn library_team(&self, id: &str) -> Result<(String, Vec<LibraryRole>), String> {
        if let Some(key) = id.strip_prefix("company:") {
            let item = company::get(&self.home_dir,key)?;
            if item.members.is_empty() { return Err("Not a team template".into()); }
            if item.premium && !self.premium_templates_unlocked().await { return Err("Premium templates are locked".into()); }
            let mut roles = vec![];
            for member in &item.members {
                let mut role = Self::company_library_role(member.clone(),item.revision);
                role.role.reports_to = member.parent_index.map(|p|item.members[p].id.clone()).unwrap_or_default();
                self.validate_template_policy(&role.role)?;
                roles.push(role);
            }
            return Ok((item.name,roles));
        }
        let parts: Vec<_> = id.split(':').collect();
        let ["team", industry] = parts.as_slice() else { return Err("Unknown team template".into()); };
        let dir = self.premium_dir_unlocked().await.map_err(|_| "Premium templates are locked")?;
        let manifest = pt::load_team_manifest(&dir, industry)?;
        let mut roles = vec![];
        for role_id in std::iter::once(pt::FRONT_DESK_ROLE_ID).chain(manifest.workers.iter().map(|w|w.name.as_str())) {
            roles.push(self.library_role(&format!("premium:{industry}:{role_id}")).await?);
        }
        Ok((manifest.label, roles))
    }

    async fn library_catalog(&self) -> Result<Value, String> {
        let mut entries = vec![];
        // A failed policy read must not expose a catalog that cannot be used.
        crate::product_policy::load(&self.home_dir)?;
        for (key, ..) in BUILTINS {
            let id = format!("builtin:{key}");
            if let Ok(role) = self.library_role(&id).await { entries.push(detail(&id, &role)?); }
        }
        let unlocked = self.premium_templates_unlocked().await;
        for item in company::list(&self.home_dir)? {
            let policy = crate::product_policy::load(&self.home_dir)?;
            let table: toml::Table = item.agent_toml.parse().map_err(|_| "Invalid template")?;
            if crate::product_policy::validate_table(crate::product_policy::load(&self.home_dir)?, &table).is_err() { continue; }
            if item.members.iter().any(|m| m.agent_toml.parse::<toml::Table>().ok().is_none_or(|t|
                crate::product_policy::validate_table(policy, &t).is_err())) { continue; }
            entries.push(json!({"id":format!("company:{}",item.id),"kind":if item.members.is_empty() {"agent"} else {"team"},"source":"company",
                "name":item.name,"description":item.description,"locked":item.premium && !unlocked}));
        }
        if unlocked {
            if let Some(dir) = pt::find_premium_templates_dir() {
                if pt::ceo_available(&dir) {
                    let id = "premium:_:ceo";
                    if let Ok(role) = self.library_role(id).await { entries.push(detail(id,&role)?); }
                }
                for team in pt::list_team_industries(&dir) {
                    let id = format!("team:{}", team.industry);
                    if let Ok((name, roles)) = self.library_team(&id).await {
                        entries.push(json!({"id":id,"kind":"team","source":"builtin","name":name,"description":"","member_count":roles.len()}));
                        for role in roles {
                            let role_id = format!("premium:{}:{}", team.industry,role.role.role_id);
                            entries.push(detail(&role_id,&role)?);
                        }
                    }
                }
            }
        }
        Ok(json!({"templates":entries,"premium_locked":!unlocked}))
    }

    async fn library_get(&self, id: &str) -> Result<Value, String> {
        if id.split(':').next() == Some("team") || self.is_company_team(id)? {
            let company = id.strip_prefix("company:").map(|key|company::get(&self.home_dir,key)).transpose()?;
            let (name, roles) = if let Some(item) = &company {
                if item.premium && !self.premium_templates_unlocked().await { return Err("Premium templates are locked".into()); }
                let mut roles = vec![];
                for member in &item.members {
                    let role = Self::company_library_role(member.clone(),item.revision);
                    self.validate_template_policy(&role.role)?;
                    roles.push(role);
                }
                (item.name.clone(),roles)
            } else { self.library_team(id).await? };
            let members: Result<Vec<Value>,String> = roles.iter().enumerate().map(|(index,r)| {
                let mut value = detail(&r.role.role_id,r)?;
                value["parent_id"] = company.as_ref().and_then(|t|t.members[index].parent_index.map(|p|t.members[p].id.clone())).into();
                Ok(value)
            }).collect();
            return Ok(json!({"id":id,"kind":"team","source":if company.is_some() {"company"} else {"builtin"},
                "name":name,"description":company.as_ref().map(|t|t.description.as_str()).unwrap_or(""),
                "revision":company.as_ref().map(|t|t.revision),
                "soul_md":"","contract_toml":"","skill_contents":{},"skills":[],"allowed_tools":[],"denied_tools":[],"model":"","runtime":"","members":members?}));
        }
        detail(id, &self.library_role(id).await?)
    }

    async fn capture_library_employee(&self, agent: &str, name: &str, description: &str) -> Result<LibraryRole,String> {
        if !is_valid_agent_id(agent) { return Err("Invalid employee".into()); }
        let dir = self.registry.read().await.get(agent).ok_or("Employee not found")?.dir.clone();
        let meta = std::fs::symlink_metadata(&dir).map_err(|_|"Employee profile unavailable")?;
        if !meta.is_dir() || meta.file_type().is_symlink() { return Err("Invalid employee directory".into()); }
        let origin = read_profile_file(&dir,".template-origin.json",true)?;
        // Older employees have no provenance marker. They are already
        // installed, not a route for reading locked distribution assets.
        let premium = if origin.is_empty() { false } else {
            serde_json::from_str::<Value>(&origin).map_err(|_|"Invalid template origin")?
                .get("premium").and_then(Value::as_bool).ok_or("Invalid template origin")?
        };
        if premium && !self.premium_templates_unlocked().await { return Err("Premium templates are locked".into()); }
        let raw: toml::Table = read_profile_file(&dir,"agent.toml",false)?.parse().map_err(|_| "Invalid employee configuration")?;
        let bindings = duduclaw_core::preset::load_bindings_checked(&self.home_dir).map_err(|_| "Cannot read employee preset bindings")?;
        let directory_id = dir.file_name().and_then(|v|v.to_str()).ok_or("Invalid employee directory")?;
        let (mut effective, resolution) = duduclaw_core::preset::resolve_for_agent_with_bindings(&self.home_dir,directory_id,&raw,&bindings);
        if matches!(resolution,duduclaw_core::preset::PresetResolution::Unresolved{..}) {
            return Err("Employee preset could not be resolved".into());
        }
        effective.remove("preset"); // Display mirror; authority was resolved above.
        let config = company::project(&toml::to_string(&effective).map_err(|_| "Invalid effective configuration")?)?;
        let disabled = duduclaw_agent::skill_state::read_disabled(&self.home_dir,agent);
        if disabled.error().is_some() { return Err("Cannot read employee skill switches".into()); }
        let mut skills = company::capture_skills(&dir)?;
        skills.retain(|name,_|!disabled.is_disabled(name));
        Ok(LibraryRole { role: role_from_files(agent,name,description,config,
            read_profile_file(&dir,"SOUL.md",false)?,read_profile_file(&dir,"CONTRACT.toml",true)?),
            skills:Some(skills), premium, disabled_skills:disabled.names(), revision:None })
    }

    fn company_library_role(item: company::CompanyTemplate, revision: u64) -> LibraryRole {
        let mut role = role_from_files(&item.id, &item.name, &item.description, item.agent_toml, item.soul_md, item.contract_toml);
        role.name = item.id;

        LibraryRole { role, skills:Some(item.skills), premium:item.premium, disabled_skills:item.disabled_skills, revision:Some(revision) }
    }

    fn is_company_team(&self, id: &str) -> Result<bool,String> {
        match id.strip_prefix("company:") {
            Some(key) => Ok(!company::get(&self.home_dir,key)?.members.is_empty()),
            None => Ok(false),
        }
    }

    async fn library_copy(&self, params: &Value) -> Result<Value, String> {
        let name = params.get("name").and_then(Value::as_str).ok_or("Template name required")?;
        let description = params.get("description").and_then(Value::as_str).unwrap_or("");
        let source_count = ["agent","team_agent","template_id"].iter().filter(|k|params.get(**k).is_some()).count();
        if source_count != 1 { return Err("Choose one employee, team or template".into()); }
        let template_id = params.get("template_id").and_then(Value::as_str);
        let mut team_roles = None;
        if let Some(root) = params.get("team_agent").and_then(Value::as_str) {
            if !is_valid_agent_id(root) { return Err("Invalid team root".into()); }
            let registry = self.registry.read().await;
            if registry.get(root).is_none() { return Err("Team root not found".into()); }
            let mut names = vec![root.to_string()];
            let mut index = 0;
            while index < names.len() {
                let mut children = registry.list().into_iter().filter(|a| a.config.agent.reports_to == names[index])
                    .map(|a|a.config.agent.name.clone()).collect::<Vec<_>>();
                children.sort();
                for child in children {
                    if names.contains(&child) { return Err("Cyclic team hierarchy".into()); }
                    names.push(child);
                    if names.len() > 50 { return Err("Too many team members".into()); }
                }
                index += 1;
            }
            drop(registry);
            let mut roles = vec![];
            for employee in names {
                let mut role = self.capture_library_employee(&employee,&employee,"").await?;
                self.validate_template_policy(&role.role)?;
                // Preserve the employee's presentation without copying identity.
                let registry = self.registry.read().await;
                let source = registry.get(&employee).ok_or("Employee changed")?;
                role.role.display_name = if source.config.agent.display_name.trim().is_empty() { employee.clone() } else { source.config.agent.display_name.clone() };
                role.role.reports_to = source.config.agent.reports_to.clone();
                roles.push(role);
            }
            team_roles = Some(roles);
        } else if let Some(id) = template_id {
            if id.split(':').next() == Some("team") || self.is_company_team(id)? {
                team_roles = Some(self.library_team(id).await?.1);
            }
        }
        if let Some(roles) = team_roles {
            let mut members = vec![];
            let role_ids = roles.iter().map(|r|r.role.role_id.clone()).collect::<Vec<_>>();
            for (index, item) in roles.into_iter().enumerate() {
                let mut member = company::draft(&item.role.display_name,&item.role.summary,&item.role.agent_toml,
                    &item.role.soul_md,&item.role.contract_toml,item.skills.unwrap_or_default(),item.premium,item.disabled_skills)?;
                member.parent_index = if index == 0 {None} else {
                    // Company roles use stable member ids; installed employees use their ids.
                    Some(role_ids.iter().position(|id|id == &item.role.reports_to).unwrap_or(0))
                };
                members.push(member);
            }
            let draft = company::draft_team(name,description,members)?;
            return self.review_library_copy(params, draft);
        }
        let role = match (params.get("agent").and_then(Value::as_str), params.get("template_id").and_then(Value::as_str)) {
            (Some(agent), None) => {
                self.capture_library_employee(agent,name,description).await?
            }
            (None, Some(id)) => self.library_role(id).await?,
            _ => return Err("Choose one employee or template".into()),
        };
        self.validate_template_policy(&role.role)?;
        let draft = company::draft(name, description, &role.role.agent_toml,
            &role.role.soul_md, &role.role.contract_toml, role.skills.unwrap_or_default(), role.premium, role.disabled_skills)?;
        self.review_library_copy(params, draft)
    }

    fn review_library_copy(&self, params: &Value, draft: company::CompanyTemplate) -> Result<Value, String> {
        use sha2::{Digest, Sha256};
        // A newly generated id is not part of the reviewed content. Member ids
        // are deterministic, and all policy, content and hierarchy are bound.
        let mut normalized = draft.clone();
        normalized.id = "review".into();
        let bytes = serde_json::to_vec(&normalized).map_err(|_| "無法產生範本審閱摘要")?;
        let digest = format!("{:x}", Sha256::digest(bytes));
        if params.get("preview").and_then(Value::as_bool) == Some(true) {
            let mut preview = detail("preview", &Self::company_library_role(draft.clone(), 1))?;
            if !draft.members.is_empty() {
                preview["kind"] = json!("team");
                let members = draft.members.iter().map(|member| {
                    detail(&member.id, &Self::company_library_role(member.clone(), 1))
                }).collect::<Result<Vec<_>, _>>()?;
                preview["members"] = json!(members);
            }
            return Ok(json!({"preview":preview,"review_digest":digest}));
        }
        require_review(params)?;
        if params.get("review_digest").and_then(Value::as_str) != Some(digest.as_str()) {
            return Err("來源內容已變更或尚未審閱，請重新取得全文預覽並確認。".into());
        }
        company::save_new(&self.home_dir, &draft)?;
        Ok(json!({"id":format!("company:{}",draft.id)}))
    }

    /// Exact dispatch scope table: every method requires a live Admin. The
    /// outer dispatcher performs current identity and edition resolution.
    pub(crate) async fn handle_template_library(&self, method: &str, params: Value, caller: &UserContext) -> WsFrame {
        if !caller.is_admin() { return WsFrame::error_response("", "Administrator required"); }
        let id = params.get("id").and_then(Value::as_str).unwrap_or("");
        let result = match method {
            "templates.catalog" => self.library_catalog().await,
            "templates.get" => self.library_get(id).await,
            "templates.copy" => self.library_copy(&params).await,
            "templates.save" => self.library_save(&params).await,
            "templates.apply" => self.library_apply(&params,caller).await,
            _ => Err("Unknown template method".into()),
        };
        if matches!(method,"templates.copy"|"templates.save"|"templates.apply") {
            duduclaw_security::audit::append_audit_event(&self.home_dir,
                &duduclaw_security::audit::AuditEvent::new("template_change", &caller.user_id,
                    duduclaw_security::audit::Severity::Info,
                    json!({"method":method,"template":id,"success":result.as_ref().is_ok_and(|v|v.get("success").and_then(Value::as_bool)!=Some(false)),
                        "preview":params.get("preview").and_then(Value::as_bool)==Some(true),
                        "result_id":result.as_ref().ok().and_then(|v|v.get("id")),
                        "agents":result.as_ref().ok().and_then(|v|v.get("agents"))})));
        }
        match result { Ok(v) => WsFrame::ok_response("",v), Err(e) => WsFrame::error_response("",&e) }
    }

    async fn library_save(&self, params: &Value) -> Result<Value,String> {
        require_review(params)?;
        let id = params.get("id").and_then(Value::as_str).ok_or("Template required")?;
        let key = id.strip_prefix("company:").ok_or("Built-in templates are read-only; copy first")?;
        // Check current product/license restrictions before accepting an edit.
        if self.is_company_team(id)? {
            self.library_team(id).await?;
            let edits: Vec<company::MemberEdit> = serde_json::from_value(params.get("members").cloned().ok_or("Team members required")?)
                .map_err(|_|"Invalid team member edits")?;
            company::update_team(&self.home_dir,key,
                params.get("revision").and_then(Value::as_u64).ok_or("Template revision required")?,
                params.get("name").and_then(Value::as_str).ok_or("Name required")?,
                params.get("description").and_then(Value::as_str).ok_or("Description required")?,edits)?;
            return self.library_get(id).await;
        }
        self.library_role(id).await?;
        let revision = params.get("revision").and_then(Value::as_u64).ok_or("Template revision required")?;
        let strings = |key: &str| -> Result<Vec<String>,String> {
            serde_json::from_value(params.get(key).cloned().ok_or("Missing template field")?).map_err(|_|"Invalid template field".into())
        };
        company::update(&self.home_dir,key,revision,company::TemplateEdit {
            name:params.get("name").and_then(Value::as_str).ok_or("Name required")?.into(),
            description:params.get("description").and_then(Value::as_str).ok_or("Description required")?.into(),
            skills:strings("skills")?,allowed_tools:strings("allowed_tools")?,denied_tools:strings("denied_tools")?,
        })?;
        self.library_get(id).await
    }

    async fn library_apply(&self, params: &Value, caller: &UserContext) -> Result<Value,String> {
        self.library_apply_members(params, caller, |_| std::future::ready(())).await
    }

    async fn library_apply_members<F, Fut>(&self, params: &Value, caller: &UserContext, mut after_member: F) -> Result<Value,String>
    where F: FnMut(usize) -> Fut, Fut: std::future::Future<Output = ()> {
        require_review(params)?;
        let id = params.get("id").and_then(Value::as_str).ok_or("Template required")?;
        let name = params.get("name").and_then(Value::as_str).ok_or("Employee name required")?;
        let department = params.get("department").and_then(Value::as_str).unwrap_or("").trim();
        if !department.is_empty() && !duduclaw_core::is_valid_department(department) { return Err("Invalid department".into()); }
        let users: Vec<String> = serde_json::from_value(params.get("bound_users").cloned().unwrap_or(json!([]))).map_err(|_|"Invalid users")?;
        if users.len() > 100 { return Err("Too many bound users".into()); }
        if !users.is_empty() {
            if self.resolve_edition_profile().await.is_personal() { return Err("User bindings require the company edition".into()); }
            let db = self.user_db.read().await.clone().ok_or("User system unavailable")?;
            for user in &users {
                let account = db.get_user(user)?.ok_or("User not found")?;
                if account.status != duduclaw_auth::models::UserStatus::Active { return Err("User is inactive".into()); }
            }
        }
        let is_team = id.split(':').next() == Some("team") || self.is_company_team(id)?;
        let roles = if is_team { self.library_team(id).await?.1 } else { vec![self.library_role(id).await?] };
        if let Some(revision) = roles.first().and_then(|r|r.revision) {
            if params.get("revision").and_then(Value::as_u64) != Some(revision) {
                return Err("Template changed; reload before applying".into());
            }
        }
        let bindings = duduclaw_core::preset::load_bindings_checked(&self.home_dir).map_err(|_|"Cannot read employee preset bindings")?;
        let mut names = vec![];
        for item in &roles {
            let target = if is_team { format!("{name}-{}",item.role.name) } else { name.to_string() };
            if !is_valid_agent_id(&target) || duduclaw_core::is_reserved_agent_id(&target) { return Err("Invalid or reserved employee name".into()); }
            if bindings.contains(&target) { return Err("Employee name has an existing preset binding".into()); }
            if names.contains(&target) { return Err("Duplicate team member name".into()); }
            names.push(target);
        }
        let reg = self.registry.read().await;
        for name in &names {
            if reg.list().iter().any(|a|a.config.agent.name==*name || a.dir.file_name().and_then(|v|v.to_str())==Some(name.as_str()))
                || reg.agents_dir().join(name).exists() { return Err("Employee name already exists".into()); }
        }
        let count = reg.list().len();
        drop(reg);
        if let Some(error) = self.tier_limit_message("agent",count + names.len().saturating_sub(1)).await { return Err(error); }
        let premium_dir = if roles.iter().any(|r|r.premium) {
            self.premium_dir_unlocked().await.map_err(|_|"Premium templates are locked")?
        } else { self.home_dir.join("company_templates") };
        let role_targets: BTreeMap<String,String> = roles.iter().zip(&names).map(|(r,n)|(r.role.role_id.clone(),n.clone())).collect();
        let mut created = vec![];
        for (index, mut item) in roles.into_iter().enumerate() {
            // Reload and validate from disk before each employee creation. A
            // concurrent narrowing or unreadable/tampered template stops the batch.
            if let Some(key) = id.strip_prefix("company:") {
                let refusal = match company::get(&self.home_dir, key) {
                    Ok(current) if Some(current.revision) == item.revision => None,
                    Ok(_) => Some("範本版本已變更，請重新審閱。".to_string()),
                    Err(error) => Some(error),
                };
                if let Some(error) = refusal {
                    return Ok(json!({"success":false,"agents":created,"warning":"template_changed",
                        "error":format!("已停止建立其餘員工：{error}")}));
                }
            }
            // A new team has its own front desk; no accidental link back to
            // the original template's or another team's employee ids.
            {
                let mut table: toml::Table = item.role.agent_toml.parse().map_err(|_|"Invalid template")?;
                let agent = table.get_mut("agent").and_then(toml::Value::as_table_mut).ok_or("Invalid template identity")?;
                agent.insert("reports_to".into(),toml::Value::String(if !is_team || index==0 {String::new()} else {role_targets.get(&item.role.reports_to).cloned().unwrap_or_else(||names[0].clone())}));
                agent.insert("role".into(),toml::Value::String("specialist".into()));
                agent.insert("department".into(),toml::Value::String(department.into()));
                item.role.agent_toml=toml::to_string(&table).map_err(|_|"Invalid template")?;
            }
            let display_name = if is_team { format!("{} · {}",params.get("display_name").and_then(Value::as_str).unwrap_or(name),item.role.display_name) }
                else {params.get("display_name").and_then(Value::as_str).unwrap_or(name).to_string()};
            let create_params = json!({"name":names[index],"display_name":display_name,"department":department});
            let response = self.create_template_with_skills(&create_params,caller,&premium_dir,&item.role.role_id,&item.role,item.skills.as_ref(),item.premium,item.premium && item.revision.is_none(), &item.disabled_skills).await;
            if !matches!(response,WsFrame::Response{ok:true,..}) {
                let error = match response { WsFrame::Response { error, .. } => error, _ => None };
                return Ok(json!({"success":false,"agents":created,"warning":"creation_failed","error":error}));
            }
            created.push(names[index].clone());
            for user in &users {
                let response = self.handle_users_bind_agent(json!({"user_id":user,"agent_name":names[index],"access_level":"operator"}),caller).await;
                if !matches!(response,WsFrame::Response{ok:true,..}) {
                    return Ok(json!({"success":false,"agents":created,"warning":"binding_failed"}));
                }
            }
            after_member(index).await;
        }
        Ok(json!({"success":true,"agents":created}))
    }
}

fn require_review(params: &Value) -> Result<(), String> {
    if params.get("reviewed").and_then(Value::as_bool) != Some(true) {
        return Err("請先審閱完整 SOUL.md、CONTRACT.toml 與所有技能內容，確認無秘密或個資後勾選確認（reviewed: true）。".into());
    }
    Ok(())
}

#[cfg(test)]
mod revision_tests {
    use super::*;

    #[tokio::test]
    async fn templates_library_concurrent_update_between_members_stops_apply() {
        let home = tempfile::tempdir().unwrap();
        let config = include_str!("../../../../templates/evaluator/agent.toml");
        let member = company::draft("Member", "", config, "Helpful", "", BTreeMap::new(), false, vec![]).unwrap();
        let team = company::create_team(home.path(), "Team", "", vec![member.clone(), member]).unwrap();
        let handler = MethodHandler::new(home.path().into()).await;
        let params = json!({"id":format!("company:{}", team.id),"revision":1,"name":"concurrent","reviewed":true});
        let result = handler.library_apply_members(&params, &UserContext::admin_fallback(), |_| {
            let path = home.path().to_path_buf();
            let team = team.clone();
            async move {
                // Run an actual concurrent update at the boundary after the
                // first registry-visible commit, before the second member.
                tokio::task::spawn_blocking(move || {
                    let edits = team.members.iter().map(|m| company::MemberEdit {
                        id: m.id.clone(), edit: company::TemplateEdit {
                            name: m.name.clone(), description: m.description.clone(), skills: vec![],
                            allowed_tools: vec!["Read".into()], denied_tools: vec!["Bash".into()],
                        },
                    }).collect();
                    company::update_team(&path, &team.id, 1, "Narrowed", "", edits).unwrap();
                }).await.unwrap();
            }
        }).await.unwrap();
        assert_eq!(result["success"], false);
        assert_eq!(result["warning"], "template_changed");
        assert_eq!(result["agents"], json!(["concurrent-member-0"]));
        assert_eq!(company::get(home.path(), &team.id).unwrap().revision, 2);
        assert!(handler.registry.read().await.get("concurrent-member-0").is_some());
        assert!(!home.path().join("agents/concurrent-member-1").exists());
    }
}
