//! Write-side validation and live trigger/step department containment.

use serde_json::Value;

use super::*;
use crate::autopilot_store::AutopilotRuleRow;

const DENIED: &str = "permission denied";

/// `None` is the Admin path, selected only by the authenticated RPC context.
/// Apply containment to the shared builder's validated, normalized row,
/// including fields omitted by an update.
pub(crate) async fn validate_department_rule(
    h: &MethodHandler, view: Option<&DeptGrantView>, row: &AutopilotRuleRow,
) -> Result<(), String> {
    let department = row.owner_department.as_deref().ok_or(DENIED)?;
    if !duduclaw_core::is_valid_department(department)
        || view.is_some_and(|v| v.is_empty()
            || !v.departments_for(GrantGroup::Automation).iter().any(|d| d == department))
        || !matches!(row.trigger_event.as_str(), "task_created" | "task_updated"
            | "task_status_changed" | "activity_new" | "channel_message" | "agent_idle" | "run_at_risk")
        || row.sequence.is_some() || row.metadata.is_some()
    { return Err(DENIED.into()); }
    let admin_targets;
    let targets = if let Some(view) = view { view } else {
        admin_targets = DeptGrantView::load_targets(h).await.ok_or(DENIED)?;
        &admin_targets
    };
    // Unlike the caller-dependent ACL shortcut, even the personal employee's
    // own Admin/Manager cannot put it in a shared department rule.
    let owners = crate::personal_visibility::personal_owners(h.home_dir()).map_err(|_| DENIED)?;
    let target_in_department = |agent: &str| -> Result<bool, String> {
        Ok(!owners.contains_key(agent)
            && grant_target_department(targets, agent).map_err(|_| DENIED)?.as_deref() == Some(department))
    };
    if view.is_none() {
        let mut authoritative = false;
        for agent in targets.loaded_agents.keys() {
            authoritative |= target_in_department(agent)?;
        }
        if !authoritative { return Err(DENIED.into()); }
    }

    let action: Value = serde_json::from_str(&row.action).map_err(|_| DENIED)?;
    // The shared save validator has checked the normalized envelope, templates,
    // screen, window and run policy. Only department containment remains here.
    let steps = action.get("steps").and_then(Value::as_array).ok_or(DENIED)?;
    for step in steps {
        if step.get("type").and_then(Value::as_str) == Some("wait") { continue; }
        match step.get("type").and_then(Value::as_str) {
            Some("delegate" | "run_skill") => {
                let target = step.get("target_agent").and_then(Value::as_str).ok_or(DENIED)?;
                if !target_in_department(target)? || view.is_some_and(|v|
                    !department_grant_allows(v, target, GrantGroup::Automation, GrantAction::Create))
                { return Err(DENIED.into()); }
            }
            Some("notify" | "proactive_notify") => {
                let channel = step.get("channel").and_then(Value::as_str).ok_or(DENIED)?;
                let chat_id = step.get("chat_id").and_then(Value::as_str).ok_or(DENIED)?;
                let owner = row.created_by_user_id.as_deref().ok_or(DENIED)?;
                let db = h.user_db().await.ok_or(DENIED)?;
                let destinations = db.verified_dm_destinations_for_user(owner).map_err(|_| DENIED)?;
                let mut allowed = destinations.iter().any(|dest|
                    dest.channel == channel && dest.channel_user_id == chat_id);
                // Read the entire candidate set: a failed authority/config read
                // never becomes an absent destination or a fallback allow.
                for agent in targets.loaded_agents.keys() {
                    if !target_in_department(agent)? { continue; }
                    let target = crate::goal_notify::try_agent_notify_target(h.home_dir(), agent)
                        .map_err(|_| DENIED)?;
                    allowed |= target.is_some_and(|(ch, chat)| ch == channel && chat == chat_id);
                }
                if !allowed { return Err(DENIED.into()); }
            }
            _ => return Err(DENIED.into()),
        }
    }
    Ok(())
}


/// Fixed codes only: callers must never persist event data on a denial.
pub(crate) fn dept_rule_contained(
    home: &std::path::Path, rule: &AutopilotRuleRow, step: Option<&Value>,
    fields: &serde_json::Map<String, Value>,
) -> Result<(), String> {
    let department = rule.owner_department.as_deref().ok_or("department_missing")?;
    if !duduclaw_core::is_valid_department(department) {
        return Err("department_invalid".into());
    }
    if rule.sequence.is_some() || rule.metadata.is_some() {
        return Err("trigger_unsupported".into());
    }
    let subject = match rule.trigger_event.as_str() {
        "channel_message" | "agent_idle" | "run_at_risk" => fields.get("agent_id"),
        "task_created" | "task_updated" | "task_status_changed" =>
            fields.get("task").and_then(|v| v.get("assigned_to")),
        "activity_new" => fields.get("activity").and_then(|v| v.get("agent_id")),
        _ => return Err("trigger_unsupported".into()),
    }.and_then(Value::as_str).ok_or("subject_missing")?;
    if !duduclaw_core::is_valid_agent_id(subject) {
        return Err("subject_invalid".into());
    }
    if duduclaw_agent::skill_loader::try_authoritative_department(home, subject)
        .map_err(|_| "subject_unavailable")?.as_deref() != Some(department)
    {
        return Err("subject_outside_department".into());
    }
    if let Some(step) = step {
        return contained_step(home, rule, department, step);
    }
    let action: Value = serde_json::from_str(&rule.action).map_err(|_| "action_invalid")?;
    if action.get("type").and_then(Value::as_str) != Some("steps") {
        return Err("action_invalid".into());
    }
    let steps = action.get("steps").and_then(Value::as_array).ok_or("action_invalid")?;
    if steps.is_empty() || steps.len() > 5 { return Err("action_invalid".into()); }
    for step in steps { contained_step(home, rule, department, step)?; }
    Ok(())
}

fn contained_step(
    home: &std::path::Path, rule: &AutopilotRuleRow, department: &str, step: &Value,
) -> Result<(), String> {
    let kind = step.get("type").and_then(Value::as_str).ok_or("step_unsupported")?;
    if kind == "wait" { return Ok(()); }
    if !matches!(kind, "delegate" | "run_skill" | "notify" | "proactive_notify") {
        return Err("step_unsupported".into());
    }
    let owners = crate::personal_visibility::personal_owners(home).map_err(|_| "targets_unavailable")?;
    let raw = std::fs::read_to_string(home.join("config.toml")).map_err(|_| "targets_unavailable")?;
    let config: toml::Value = raw.parse().map_err(|_| "targets_unavailable")?;
    let general = config.get("general").map(|v| v.as_table().ok_or("targets_unavailable")).transpose()?;
    let default_id = general.and_then(|v| v.get("default_agent"))
        .map(|v| v.as_str().ok_or("targets_unavailable")).transpose()?.unwrap_or("");
    let target_in_department = |agent: &str| -> Result<bool, String> {
        if !duduclaw_core::is_valid_agent_id(agent) || agent == default_id || owners.contains_key(agent) {
            return Ok(false);
        }
        let raw = std::fs::read_to_string(home.join("agents").join(agent).join("agent.toml"))
            .map_err(|_| "targets_unavailable")?;
        let config: toml::Value = raw.parse().map_err(|_| "targets_unavailable")?;
        let info = config.get("agent").ok_or("targets_unavailable")?;
        let role: AgentRole = info.get("role").ok_or("targets_unavailable")?.clone()
            .try_into().map_err(|_| "targets_unavailable")?;
        let name = info.get("name").and_then(toml::Value::as_str).ok_or("targets_unavailable")?;
        if role == AgentRole::Main || name == default_id { return Ok(false); }
        Ok(duduclaw_agent::skill_loader::try_authoritative_department(home, agent)
            .map_err(|_| "targets_unavailable")?.as_deref() == Some(department))
    };
    if matches!(kind, "delegate" | "run_skill") {
        let target = step.get("target_agent").and_then(Value::as_str).ok_or("target_invalid")?;
        return if target_in_department(target)? { Ok(()) } else { Err("target_outside_department".into()) };
    }
    let channel = step.get("channel").and_then(Value::as_str).ok_or("destination_invalid")?;
    let chat = step.get("chat_id").and_then(Value::as_str).ok_or("destination_invalid")?;
    let owner = rule.created_by_user_id.as_deref().ok_or("owner_missing")?;
    let db = crate::decision_notify::open_user_db_read_only(home)
        .map_err(|_| "owner_unavailable")?.ok_or("owner_unavailable")?;
    let user = db.get_user(owner).map_err(|_| "owner_unavailable")?;
    let grants = db.dept_grants_for_user(owner).map_err(|_| "owner_unavailable")?;
    let destinations = db.verified_dm_destinations_for_user(owner).map_err(|_| "owner_unavailable")?;
    let eligible = user.is_some_and(|u| u.status == UserStatus::Active &&
        (u.role == UserRole::Admin || (u.role == UserRole::Manager && grants.iter()
            .any(|(dept, groups)| dept == department && groups.contains(&GrantGroup::Automation)))));
    let mut allowed = eligible && destinations.iter()
        .any(|d| d.channel == channel && d.channel_user_id == chat);
    // Enumerate live directory ids, never a cached registry. Read the entire
    // candidate set even after a match: unreadable authority fails closed.
    for entry in std::fs::read_dir(home.join("agents")).map_err(|_| "targets_unavailable")? {
        let entry = entry.map_err(|_| "targets_unavailable")?;
        let kind = entry.file_type().map_err(|_| "targets_unavailable")?;
        if !kind.is_dir() && !kind.is_symlink() { continue; }
        let Ok(agent) = entry.file_name().into_string() else { continue; };
        if duduclaw_core::is_internal_agent_dir(&agent) || !duduclaw_core::is_valid_agent_id(&agent) {
            continue;
        }
        if !entry.path().join("agent.toml").try_exists().map_err(|_| "targets_unavailable")? {
            continue;
        }
        if !target_in_department(&agent)? { continue; }
        let destination = crate::goal_notify::try_agent_notify_target(home, &agent)
            .map_err(|_| "targets_unavailable")?;
        allowed |= destination.is_some_and(|(ch, id)| ch == channel && id == chat);
    }
    if allowed { Ok(()) } else { Err("destination_outside_department".into()) }
}


#[cfg(test)]
pub(crate) mod runtime_tests {
    use super::*;
    use serde_json::json;

    pub(crate) fn employee(home: &std::path::Path, id: &str, department: &str) {
        let dir = home.join("agents").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        if !home.join("users.db").exists() {
            duduclaw_auth::UserDb::new(&home.join("users.db")).unwrap();
        }
        if !home.join("config.toml").exists() {
            std::fs::write(home.join("config.toml"), "[general]\ndefault_agent = 'main'\n").unwrap();
        }
        let path = dir.join("agent.toml");
        let mut config: toml::Value = std::fs::read_to_string(&path).unwrap_or_default().parse().unwrap();
        let info = config.as_table_mut().unwrap().entry("agent").or_insert_with(|| toml::Value::Table(Default::default()));
        let info = info.as_table_mut().unwrap();
        info.insert("name".into(), toml::Value::String(id.into()));
        info.insert("role".into(), toml::Value::String("specialist".into()));
        info.insert("status".into(), toml::Value::String("active".into()));
        info.insert("department".into(), toml::Value::String(department.into()));
        std::fs::write(path, toml::to_string(&config).unwrap()).unwrap();
        let path = home.join("org.toml");
        let mut org: toml::Value = std::fs::read_to_string(&path).unwrap_or_default().parse().unwrap();
        let agents = org.as_table_mut().unwrap().entry("agents").or_insert_with(|| toml::Value::Table(Default::default()));
        let mut entry = toml::map::Map::new();
        entry.insert("department".into(), toml::Value::String(department.into()));
        agents.as_table_mut().unwrap().insert(id.into(), toml::Value::Table(entry));
        std::fs::write(path, toml::to_string(&org).unwrap()).unwrap();
    }

    pub(crate) fn rule(step: Value) -> AutopilotRuleRow {
        serde_json::from_value(json!({
            "id":"dept", "name":"Department rule", "enabled":true,
            "trigger_event":"channel_message", "conditions":"{}",
            "action":json!({"type":"steps", "steps":[step]}).to_string(),
            "created_at":"", "trigger_count":0, "owner_department":"sales",
            "created_by_user_id":"mgrA"
        })).unwrap()
    }

    #[test]
    fn live_subject_and_step_checks_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        employee(home, "a1", "sales");
        employee(home, "a2", "sales");
        employee(home, "b1", "mfg");
        let fields = json!({"agent_id":"a1"}).as_object().unwrap().clone();
        let delegate = json!({"type":"delegate","target_agent":"a2","prompt":"test"});
        let mut row = rule(delegate.clone());
        assert!(dept_rule_contained(home, &row, None, &fields).is_ok());
        for (event, fields) in [
            ("channel_message", json!({"agent_id":"a1"})),
            ("agent_idle", json!({"agent_id":"a1"})),
            ("run_at_risk", json!({"agent_id":"a1"})),
            ("task_created", json!({"task":{"assigned_to":"a1"}})),
            ("task_updated", json!({"task":{"assigned_to":"a1"}})),
            ("task_status_changed", json!({"task":{"assigned_to":"a1"}})),
            ("activity_new", json!({"activity":{"agent_id":"a1"}})),
        ] {
            row.trigger_event = event.into();
            assert!(dept_rule_contained(home, &row, None, fields.as_object().unwrap()).is_ok());
            assert!(dept_rule_contained(home, &row, None, &Default::default()).is_err());
        }
        for trigger in ["tick", "task_deleted", "security_event", "cep_trigger"] {
            row.trigger_event = trigger.into();
            assert_eq!(dept_rule_contained(home, &row, None, &fields), Err("trigger_unsupported".into()));
        }
        row.trigger_event = "channel_message".into();
        row.sequence = Some("{}".into());
        assert_eq!(dept_rule_contained(home, &row, None, &fields), Err("trigger_unsupported".into()));
        row.sequence = None;
        row.owner_department = Some("sale".into());
        assert_eq!(dept_rule_contained(home, &row, None, &fields), Err("subject_outside_department".into()));
        row.owner_department = Some("sales".into());
        assert_eq!(dept_rule_contained(home, &row, None, json!({"agent_id":"b1"}).as_object().unwrap()),
            Err("subject_outside_department".into()));
        for step in [json!({"type":"run_skill","target_agent":"b1"}),
            json!({"type":"delegate","target_agent":"main"}), json!({"type":"unknown"})] {
            assert!(dept_rule_contained(home, &row, Some(&step), &fields).is_err());
        }
        employee(home, "a2", "mfg");
        assert_eq!(dept_rule_contained(home, &row, None, &fields), Err("target_outside_department".into()));
        assert!(dept_rule_contained(home, &row, Some(&json!({"type":"wait"})), &fields).is_ok());
        std::fs::write(home.join("org.toml"), "[broken").unwrap();
        assert_eq!(dept_rule_contained(home, &row, None, &fields), Err("subject_unavailable".into()));
    }

    #[tokio::test]
    async fn dept_rule_notify_skips_non_employee_directories() {
        let f = super::super::tests::fixture().await;
        f.db.bind_channel_identity("mgrA", "line", "owner-dm", true).unwrap();
        let step = json!({"type":"notify", "channel":"line", "chat_id":"owner-dm", "text":"test"});
        let row = rule(step.clone());
        let fields = json!({"agent_id":"a1"}).as_object().unwrap().clone();
        for name in ["_internal", "empty", "invalid name"] {
            std::fs::create_dir_all(f.home.join("agents").join(name)).unwrap();
        }
        // Internal and invalid ids are skipped even if they contain a config.
        for name in ["_internal", "invalid name"] {
            std::fs::write(f.home.join("agents").join(name).join("agent.toml"), "[broken").unwrap();
        }
        assert!(dept_rule_contained(&f.home, &row, Some(&step), &fields).is_ok());
        // A corrupt employee config must still fail closed after a destination match.
        std::fs::write(f.home.join("agents/empty/agent.toml"), "[broken").unwrap();
        assert_eq!(dept_rule_contained(&f.home, &row, Some(&step), &fields),
            Err("targets_unavailable".into()));
    }

    #[tokio::test]
    async fn dept_rule_notify_owner_revoked() {
        let f = super::super::tests::fixture().await;
        f.db.bind_channel_identity("mgrA", "line", "owner-dm", true).unwrap();
        let step = json!({"type":"notify", "channel":"line", "chat_id":"owner-dm", "text":"test"});
        let row = rule(step.clone());
        let fields = json!({"agent_id":"a1"}).as_object().unwrap().clone();
        assert!(dept_rule_contained(&f.home, &row, Some(&step), &fields).is_ok());
        f.db.set_dept_grants("admin", "mgrA", "sales", &[], true).unwrap();
        assert_eq!(dept_rule_contained(&f.home, &row, Some(&step), &fields),
            Err("destination_outside_department".into()));
        let mut admin_row = row.clone();
        admin_row.created_by_user_id = Some("admin".into());
        f.db.bind_channel_identity("admin", "line", "admin-dm", true).unwrap();
        let admin_step = json!({"type":"notify", "channel":"line", "chat_id":"admin-dm", "text":"test"});
        assert!(dept_rule_contained(&f.home, &admin_row, Some(&admin_step), &fields).is_ok());
        f.db.set_user_status("admin", UserStatus::Suspended).unwrap();
        assert!(dept_rule_contained(&f.home, &admin_row, Some(&admin_step), &fields).is_err());
        let path = f.home.join("agents/a2/agent.toml");
        let raw = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, format!("{raw}\n[proactive]\nnotify_channel = 'line'\nnotify_chat_id = 'owner-dm'\n")).unwrap();
        assert!(dept_rule_contained(&f.home, &row, Some(&step), &fields).is_ok());
        employee(&f.home, "a2", "mfg");
        assert!(dept_rule_contained(&f.home, &row, Some(&step), &fields).is_err());
    }
}
