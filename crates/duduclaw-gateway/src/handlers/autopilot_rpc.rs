//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    pub(crate) async fn handle_autopilot_breakers(&self) -> WsFrame {
        let unavailable = || WsFrame::error_response("", "Autopilot engine unavailable");
        let Some(reader) = self.autopilot_breakers.read().await.clone() else {
            return unavailable();
        };
        let store = match self.ap_store().await {
            Ok(store) => store,
            Err(frame) => return frame,
        };
        let rules = match store.list_rules().await {
            Ok(rules) => rules,
            Err(error) => return WsFrame::error_response("", &format!("list autopilot: {error}")),
        };
        let ids: Vec<String> = rules.into_iter().map(|rule| rule.id).collect();
        match reader.snapshot(&ids).await {
            Some(breakers) => WsFrame::ok_response("", json!({"breakers": breakers})),
            None => unavailable(),
        }
    }

    pub(crate) async fn handle_autopilot_list(&self) -> WsFrame {
        let store = match self.ap_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        match store.list_rules().await {
            Ok(rows) => {
                let rules: Vec<Value> = rows.iter().map(|r| autopilot_rule_to_json(r)).collect();
                WsFrame::ok_response("", json!({ "rules": rules }))
            }
            Err(e) => WsFrame::error_response("", &format!("list autopilot: {e}")),
        }
    }

    pub(crate) async fn handle_autopilot_create(&self, params: Value, ctx: &UserContext) -> WsFrame {
        if params.get("owner_department").is_some() {
            return WsFrame::error_response("", "owner_department is not supported yet");
        }
        let store = match self.ap_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let definition = (|| -> Result<AutopilotRuleRow, String> {
            let name = params.get("name").and_then(Value::as_str)
                .filter(|s| !s.is_empty()).ok_or("name is required")?.to_string();
            let trigger_event = match params.get("trigger_event") {
                None => "task_created",
                Some(v) => v.as_str().ok_or("trigger_event must be a string")?,
            }.to_string();
            validate_autopilot_trigger_event_for_create(&trigger_event)?;
            let conditions = params.get("conditions").cloned().unwrap_or(json!({}));
            validate_autopilot_conditions(&conditions)?;
            let mut row = AutopilotRuleRow {
                id: uuid::Uuid::new_v4().to_string(),
                name,
                enabled: true,
                trigger_event,
                conditions: conditions.to_string(),
                action: params.get("action").cloned().unwrap_or(json!({})).to_string(),
                sequence: optional_rule_object(&params, "sequence")?,
                active_window: optional_rule_object(&params, "active_window")?,
                run_policy: optional_rule_object(&params, "run_policy")?,
                owner_department: None,
                created_by_user_id: Some(ctx.user_id.clone()),
                updated_by_user_id: None,
                updated_at: None,
                created_at: Utc::now().to_rfc3339(),
                last_triggered_at: None,
                trigger_count: 0,
                metadata: None,
            };
            wrap_rule_action_if_needed(&mut row)?;
            validate_autopilot_rule_for_save(&self.home_dir, &row)?;
            Ok(row)
        })();
        let row = match definition {
            Ok(row) => row,
            Err(e) => return WsFrame::error_response("", &e),
        };
        if let Err(e) = store.insert_rule(&row).await {
            return WsFrame::error_response("", &format!("create autopilot rule: {e}"));
        }
        WsFrame::ok_response("", json!({ "rule": autopilot_rule_to_json(&row) }))
    }

    pub(crate) async fn handle_autopilot_update(&self, params: Value, ctx: &UserContext) -> WsFrame {
        if params.get("owner_department").is_some() {
            return WsFrame::error_response("", "owner_department is not supported yet");
        }
        let store = match self.ap_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let rule_id = params.get("rule_id").and_then(Value::as_str).unwrap_or("");
        if rule_id.is_empty() {
            return WsFrame::error_response("", "rule_id is required");
        }
        let stored = match store.get_rule(rule_id).await {
            Ok(Some(row)) => row,
            Ok(None) => return WsFrame::error_response("", &format!("Rule not found: {rule_id}")),
            Err(e) => return WsFrame::error_response("", &format!("update rule: {e}")),
        };
        let definition = (|| -> Result<AutopilotRuleRow, String> {
            let mut row = stored.clone();
            if let Some(v) = params.get("name") {
                row.name = v.as_str().filter(|s| !s.is_empty()).ok_or("name is required")?.to_string();
            }
            if let Some(v) = params.get("enabled") {
                row.enabled = v.as_bool().ok_or("enabled must be a boolean")?;
            }
            let changes_definition = [
                "trigger_event", "conditions", "action", "sequence", "active_window", "run_policy",
            ].iter().any(|key| params.get(*key).is_some());
            // Keep stored definitions intact for toggles and renames, even if
            // current settings or validators would reject them on a new save.
            if !changes_definition {
                return Ok(row);
            }
            if let Some(v) = params.get("trigger_event") {
                row.trigger_event = v.as_str().ok_or("trigger_event must be a string")?.to_string();
            }
            if let Some(v) = params.get("conditions") {
                row.conditions = v.to_string();
            }
            if let Some(v) = params.get("action") {
                row.action = v.to_string();
            }
            if params.get("sequence").is_some() {
                row.sequence = optional_rule_object(&params, "sequence")?;
            }
            if params.get("active_window").is_some() {
                row.active_window = optional_rule_object(&params, "active_window")?;
            }
            if params.get("run_policy").is_some() {
                row.run_policy = optional_rule_object(&params, "run_policy")?;
            }
            validate_autopilot_trigger_event_for_update(&row.trigger_event, &stored.trigger_event)?;
            let conditions: Value = serde_json::from_str(&row.conditions).map_err(|_| "invalid conditions JSON")?;
            validate_autopilot_conditions(&conditions)?;
            wrap_rule_action_if_needed(&mut row)?;
            validate_autopilot_rule_for_save(&self.home_dir, &row)?;
            Ok(row)
        })();
        let row = match definition {
            Ok(row) => row,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let now = Utc::now().to_rfc3339();
        match store.replace_rule_definition(rule_id, &row, Some(&ctx.user_id), &now).await {
            Ok(Some(row)) => WsFrame::ok_response("", json!({ "rule": autopilot_rule_to_json(&row) })),
            Ok(None) => WsFrame::error_response("", &format!("Rule not found: {rule_id}")),
            Err(e) => WsFrame::error_response("", &format!("update rule: {e}")),
        }
    }

    pub(crate) async fn handle_autopilot_template_fields(&self, params: Value) -> WsFrame {
        let Some(trigger) = params.get("trigger_event").and_then(Value::as_str) else {
            return WsFrame::error_response("", "trigger_event is required and must be a string");
        };
        let sequence = match params.get("sequence") {
            None => None,
            Some(v) if v.is_object() => Some(v.to_string()),
            Some(_) => return WsFrame::error_response("", "sequence must be an object"),
        };
        match with_autopilot_field_context(&self.home_dir, trigger, sequence.as_deref(), |event, ctx| {
            Ok(crate::autopilot_fields::placeholders_for(event, ctx))
        }) {
            Ok(fields) => WsFrame::ok_response("", json!({"fields": fields})),
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    pub(crate) async fn handle_autopilot_remove(&self, params: Value) -> WsFrame {
        let store = match self.ap_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let rule_id = params.get("rule_id").and_then(|v| v.as_str()).unwrap_or("");
        if rule_id.is_empty() {
            return WsFrame::error_response("", "rule_id is required");
        }
        match store.remove_rule(rule_id).await {
            Ok(true) => WsFrame::ok_response("", json!({ "success": true })),
            Ok(false) => WsFrame::error_response("", &format!("Rule not found: {rule_id}")),
            Err(e) => WsFrame::error_response("", &format!("remove rule: {e}")),
        }
    }

    pub(crate) async fn handle_autopilot_history(&self, params: Value) -> WsFrame {
        let store = match self.ap_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let rule_id = params.get("rule_id").and_then(|v| v.as_str());
        let limit = params.get("limit").and_then(|v| v.as_i64()).unwrap_or(20);
        match store.list_history(rule_id, limit).await {
            Ok(entries) => {
                let result: Vec<Value> = entries
                    .iter()
                    .map(|e| {
                        json!({
                            "id": e.id,
                            "rule_id": e.rule_id,
                            "rule_name": e.rule_name,
                            "triggered_at": e.triggered_at,
                            "result": e.result,
                            "details": e.details,
                        })
                    })
                    .collect();
                WsFrame::ok_response("", json!({ "entries": result }))
            }
            Err(e) => WsFrame::error_response("", &format!("autopilot history: {e}")),
        }
    }
}

/// Absent and explicit null both mean no extension on create; update calls
/// this only for keys actually present, preserving omitted stored values.
fn optional_rule_object(params: &Value, key: &str) -> Result<Option<String>, String> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) if v.is_object() => Ok(Some(v.to_string())),
        Some(_) => Err(format!("{key} must be an object or null")),
    }
}

fn wrap_rule_action_if_needed(row: &mut AutopilotRuleRow) -> Result<(), String> {
    let action: Value = serde_json::from_str(&row.action).map_err(|_| "invalid action JSON")?;
    let has_params = action.get("type").and_then(Value::as_str) == Some("run_skill")
        && action.get("params").is_some();
    if has_params || row.active_window.is_some() || row.run_policy.is_some() || row.owner_department.is_some() {
        row.action = wrap_in_steps_envelope(action).to_string();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn autopilot_breakers_require_admin_and_report_unavailable() {
        use crate::handlers::task_privacy_tests::{admin, fixture};
        let f = fixture().await;
        let root = admin(&f);
        for role in [UserRole::Employee, UserRole::Manager] {
            f.db.update_user(&f.bob.user_id, None, Some(role), None).unwrap();
            for method in ["autopilot.list", "autopilot.breakers"] {
                let frame = f.handler.handle(method, json!({}), &f.bob).await;
                assert!(matches!(frame, WsFrame::Response {ok:false, error:Some(ref e), ..}
                    if e.as_str().is_some_and(|s| s.contains("permission denied"))), "{frame:?}");
            }
        }
        for edition in [duduclaw_core::EditionProfile::Personal, duduclaw_core::EditionProfile::Enterprise] {
            f.handler.set_test_edition(Some(edition)).await;
            let frame = f.handler.handle("autopilot.breakers", json!({}), &root).await;
            assert!(matches!(frame, WsFrame::Response {ok:false, error:Some(ref e), ..}
                if e == "Autopilot engine unavailable"), "{frame:?}");
        }
        let store = Arc::new(AutopilotStore::open(&f.home).unwrap());
        f.handler.set_autopilot_store(store.clone()).await;
        let created = f.handler.handle_autopilot_create(json!({"name":"test", "action":{"type":"notify", "channel":"webchat", "chat_id":"test", "text":"test"}}), &root).await;
        let rule_id = match created {
            WsFrame::Response {ok:true, payload:Some(p), ..} => p["rule"]["id"].as_str().unwrap().to_string(),
            other => panic!("{other:?}"),
        };
        let tasks = Arc::new(TaskStore::open(&f.home).unwrap());
        let (_tx, rx) = tokio::sync::broadcast::channel(16);
        let engine = crate::autopilot_engine::AutopilotEngine::new(f.home.clone(), store, tasks, None, rx);
        let reader = engine.breaker_reader();
        f.handler.set_autopilot_breakers(reader.clone()).await;
        let handle = tokio::spawn(engine.run());
        // Wait only for run() to publish its availability, never for a fire.
        for _ in 0..100 {
            if reader.snapshot(&[]).await.is_some() { break; }
            tokio::task::yield_now().await;
        }
        let frame = f.handler.handle("autopilot.breakers", json!({}), &root).await;
        assert!(matches!(frame, WsFrame::Response {ok:true, payload:Some(ref p), ..}
            if p == &json!({"breakers":[{"rule_id":rule_id,"state":"closed","opened_at":null}]})), "{frame:?}");
        handle.abort();
        let _ = handle.await;
        let frame = f.handler.handle("autopilot.breakers", json!({}), &root).await;
        assert!(matches!(frame, WsFrame::Response {ok:false, error:Some(ref e), ..}
            if e == "Autopilot engine unavailable"));
    }

    async fn automation_fixture() -> (crate::handlers::task_privacy_tests::Fixture, Arc<AutopilotStore>, UserContext) {
        use crate::handlers::task_privacy_tests::{admin, fixture};
        let f = fixture().await;
        let root = admin(&f);
        let store = Arc::new(AutopilotStore::open(&f.home).unwrap());
        f.handler.set_autopilot_store(store.clone()).await;
        (f, store, root)
    }

    fn response_payload(frame: WsFrame) -> Value {
        match frame {
            WsFrame::Response { ok: true, payload: Some(p), .. } => p,
            other => panic!("{other:?}"),
        }
    }

    fn response_error(frame: WsFrame) -> String {
        match frame {
            WsFrame::Response { ok: false, error: Some(e), .. } => e.as_str().unwrap().to_string(),
            other => panic!("{other:?}"),
        }
    }

    fn notify_rule() -> Value {
        json!({"name":"test", "action":{"type":"notify", "channel":"webchat", "chat_id":"test", "text":"hello"}})
    }

    fn skill_rule(template: &str) -> Value {
        json!({"name":"test", "trigger_event":"channel_message", "action":{
            "type":"run_skill", "target_agent":"sales", "skill_name":"test", "params":{"note":template}
        }})
    }

    fn active_window() -> Value {
        json!({"days":[1,2,3,4,5], "start":"09:00", "end":"18:00", "timezone":null})
    }

    #[tokio::test]
    async fn placeholder_outside_allowlist_refused_on_write_and_run() {
        // The runtime half lives in autopilot_template; this pins the RPC write path.
        let (f, store, root) = automation_fixture().await;
        let mut params = skill_rule("{{task.lease_expires_at}}");
        params["trigger_event"] = json!("task_created");
        let error = response_error(f.handler.handle_autopilot_create(params, &root).await);
        assert!(error.contains("placeholder"), "{error}");
        assert!(store.list_rules().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn create_wraps_skill_params_and_records_creator() {
        let (f, store, root) = automation_fixture().await;
        let mut params = skill_rule("{{text}}");
        params["action"]["screen"] = json!({"mode":"local", "prompt":"p"});
        params["action"]["require_approval"] = json!(true);
        params["created_by_user_id"] = json!("spoofed");
        let payload = response_payload(f.handler.handle_autopilot_create(params, &root).await);
        let rule = store.get_rule(payload["rule"]["id"].as_str().unwrap()).await.unwrap().unwrap();
        assert_eq!(rule.created_by_user_id.as_deref(), Some(root.user_id.as_str()));
        assert!(rule.updated_by_user_id.is_none());
        let action: Value = serde_json::from_str(&rule.action).unwrap();
        assert_eq!(action["type"], "steps");
        assert_eq!(action["steps"][0]["params"]["note"], "{{text}}");
        assert_eq!(action["steps"][0]["require_approval"], true);
        assert_eq!(action["screen"]["mode"], "local");
        assert!(action["steps"][0].get("screen").is_none());
    }

    #[tokio::test]
    async fn cep_params_use_then_event_fields() {
        let (f, store, root) = automation_fixture().await;
        let mut params = skill_rule("{{text}}");
        params["trigger_event"] = json!("task_created");
        params["sequence"] = json!({"first":{"event":"task_created"}, "then":{"event":"channel_message"}, "within_secs":60});
        response_payload(f.handler.handle_autopilot_create(params.clone(), &root).await);
        params["action"]["params"]["note"] = json!("{{task.title}}");
        response_error(f.handler.handle_autopilot_create(params.clone(), &root).await);
        params["sequence"] = json!({"then":{"event":"channel_message"}});
        response_error(f.handler.handle_autopilot_create(params, &root).await);
        assert_eq!(store.list_rules().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn window_requires_explicit_zone() {
        let (f, store, root) = automation_fixture().await;
        let mut params = notify_rule();
        params["active_window"] = active_window();
        let error = response_error(f.handler.handle_autopilot_create(params.clone(), &root).await);
        assert_eq!(error, "請先在系統設定填寫公司所在時區，或在這條規則指定時區");
        assert!(store.list_rules().await.unwrap().is_empty());
        std::fs::write(f.home.join("config.toml"), "[general]\ncompany_timezone = \"Asia/Taipei\"\n").unwrap();
        let payload = response_payload(f.handler.handle_autopilot_create(params, &root).await);
        let rule = store.get_rule(payload["rule"]["id"].as_str().unwrap()).await.unwrap().unwrap();
        assert_eq!(rule.active_window, Some(active_window().to_string()));
        assert_eq!(payload["rule"]["action"]["type"], "steps");
    }

    #[tokio::test]
    async fn toggle_and_rename_preserve_rule_after_company_timezone_removed() {
        let (f, store, root) = automation_fixture().await;
        std::fs::write(f.home.join("config.toml"), "[general]\ncompany_timezone = \"Asia/Taipei\"\n").unwrap();
        let mut params = notify_rule();
        params["active_window"] = active_window();
        let payload = response_payload(f.handler.handle_autopilot_create(params, &root).await);
        let id = payload["rule"]["id"].as_str().unwrap();
        let original = store.get_rule(id).await.unwrap().unwrap();
        assert!(original.enabled);
        assert_eq!(original.active_window, Some(active_window().to_string()));
        std::fs::write(f.home.join("config.toml"), "[general]\n").unwrap();

        response_payload(f.handler.handle_autopilot_update(json!({"rule_id":id, "enabled":false}), &root).await);
        let disabled = store.get_rule(id).await.unwrap().unwrap();
        assert!(!disabled.enabled);
        assert_eq!(disabled.updated_by_user_id.as_deref(), Some(root.user_id.as_str()));
        chrono::DateTime::parse_from_rfc3339(disabled.updated_at.as_deref().unwrap()).unwrap();
        assert_eq!(disabled.action, original.action);
        assert_eq!(disabled.active_window, original.active_window);

        response_payload(f.handler.handle_autopilot_update(json!({"rule_id":id, "name":"x"}), &root).await);
        let renamed = store.get_rule(id).await.unwrap().unwrap();
        assert_eq!(renamed.name, "x");
        assert!(!renamed.enabled);
        assert_eq!(renamed.updated_by_user_id.as_deref(), Some(root.user_id.as_str()));
        chrono::DateTime::parse_from_rfc3339(renamed.updated_at.as_deref().unwrap()).unwrap();
        assert_eq!(renamed.action, original.action);
        assert_eq!(renamed.active_window, original.active_window);

        assert_eq!(response_error(f.handler.handle_autopilot_update(json!({"rule_id":id, "conditions":{}}), &root).await),
            "請先在系統設定填寫公司所在時區，或在這條規則指定時區");
        for invalid in [json!({"rule_id":id, "name":""}), json!({"rule_id":id, "name":null}),
            json!({"rule_id":id, "enabled":"false"}), json!({"rule_id":id, "enabled":null})] {
            response_error(f.handler.handle_autopilot_update(invalid, &root).await);
        }
        let after = store.get_rule(id).await.unwrap().unwrap();
        assert_eq!(serde_json::to_value(renamed).unwrap(), serde_json::to_value(after).unwrap());
    }

    #[tokio::test]
    async fn unreadable_company_timezone_refuses_window_save() {
        let (f, store, root) = automation_fixture().await;
        std::fs::write(f.home.join("config.toml"), "[broken").unwrap();
        let mut params = notify_rule();
        params["active_window"] = active_window();
        params["active_window"]["timezone"] = json!("Asia/Taipei");
        assert_eq!(response_error(f.handler.handle_autopilot_create(params, &root).await),
            "company timezone setting is unreadable");
        assert!(store.list_rules().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn invalid_run_policy_and_extension_shapes_refused() {
        let (f, store, root) = automation_fixture().await;
        let payload = response_payload(f.handler.handle_autopilot_create(notify_rule(), &root).await);
        let id = payload["rule"]["id"].as_str().unwrap();
        for (key, value) in [
            ("run_policy", json!({"outside_window":"defer"})),
            ("run_policy", json!({"outside_window":"skip", "while_breaker_open":"drop"})),
            ("run_policy", json!({})), ("run_policy", json!("skip")),
            ("active_window", json!([])), ("active_window", json!({"days":[]})),
        ] {
            let mut params = notify_rule();
            params[key] = value.clone();
            response_error(f.handler.handle_autopilot_create(params, &root).await);
            let mut update = json!({"rule_id":id});
            update[key] = value;
            response_error(f.handler.handle_autopilot_update(update, &root).await);
        }
        let rule = store.get_rule(id).await.unwrap().unwrap();
        assert!(rule.active_window.is_none() && rule.run_policy.is_none() && rule.updated_at.is_none());
        assert_eq!(store.list_rules().await.unwrap().len(), 1);
        let mut valid = notify_rule();
        valid["run_policy"] = json!({"outside_window":"skip"});
        let payload = response_payload(f.handler.handle_autopilot_create(valid, &root).await);
        assert_eq!(payload["rule"]["action"]["type"], "steps");
    }

    #[tokio::test]
    async fn update_window_wraps_and_clear_keeps_envelope_and_audit() {
        let (f, store, root) = automation_fixture().await;
        let payload = response_payload(f.handler.handle_autopilot_create(notify_rule(), &root).await);
        let id = payload["rule"]["id"].as_str().unwrap();
        let original = store.get_rule(id).await.unwrap().unwrap();
        let mut editor = root.clone();
        editor.user_id = "second-admin".into();
        let mut window = active_window();
        window["timezone"] = json!("Asia/Taipei");
        response_payload(f.handler.handle_autopilot_update(json!({"rule_id":id, "active_window":window,
            "created_by_user_id":"spoofed", "updated_by_user_id":"spoofed", "updated_at":"spoofed"}), &editor).await);
        let updated = store.get_rule(id).await.unwrap().unwrap();
        assert!(updated.active_window.is_some());
        assert_eq!(serde_json::from_str::<Value>(&updated.action).unwrap()["type"], "steps");
        assert_eq!(updated.created_by_user_id, original.created_by_user_id);
        assert_eq!(updated.updated_by_user_id.as_deref(), Some("second-admin"));
        chrono::DateTime::parse_from_rfc3339(updated.updated_at.as_deref().unwrap()).unwrap();
        assert_ne!(crate::autopilot_approval::rule_revision_hash(&original), crate::autopilot_approval::rule_revision_hash(&updated));
        response_payload(f.handler.handle_autopilot_update(json!({"rule_id":id, "active_window":null}), &editor).await);
        let cleared = store.get_rule(id).await.unwrap().unwrap();
        assert!(cleared.active_window.is_none());
        assert_eq!(cleared.action, updated.action);
        response_payload(f.handler.handle_autopilot_update(json!({"rule_id":id, "name":"renamed"}), &root).await);
        let renamed = store.get_rule(id).await.unwrap().unwrap();
        assert_eq!(renamed.name, "renamed");
        assert_eq!(renamed.updated_by_user_id.as_deref(), Some(root.user_id.as_str()));
        assert_eq!(renamed.created_by_user_id, original.created_by_user_id);
        assert_eq!(renamed.created_at, original.created_at);
        chrono::DateTime::parse_from_rfc3339(renamed.updated_at.as_deref().unwrap()).unwrap();
    }

    #[tokio::test]
    async fn owner_department_param_rejected_on_create_and_update() {
        let (f, store, root) = automation_fixture().await;
        let payload = response_payload(f.handler.handle_autopilot_create(notify_rule(), &root).await);
        let id = payload["rule"]["id"].as_str().unwrap();
        for department in [json!("sales"), Value::Null] {
            let mut params = notify_rule();
            params["owner_department"] = department.clone();
            assert!(response_error(f.handler.handle_autopilot_create(params, &root).await).contains("owner_department"));
            assert!(response_error(f.handler.handle_autopilot_update(json!({"rule_id":id, "owner_department":department}), &root).await)
                .contains("owner_department"));
        }
        let rule = store.get_rule(id).await.unwrap().unwrap();
        assert!(rule.owner_department.is_none() && rule.updated_at.is_none());
        assert_eq!(store.list_rules().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn update_validates_merged_trigger_and_params() {
        let (f, store, root) = automation_fixture().await;
        let payload = response_payload(f.handler.handle_autopilot_create(skill_rule("{{text}}"), &root).await);
        let id = payload["rule"]["id"].as_str().unwrap();
        let before = store.get_rule(id).await.unwrap().unwrap();
        let error = response_error(f.handler.handle_autopilot_update(json!({"rule_id":id, "trigger_event":"task_created"}), &root).await);
        assert!(error.contains("placeholder"), "{error}");
        let after = store.get_rule(id).await.unwrap().unwrap();
        assert_eq!(serde_json::to_value(before).unwrap(), serde_json::to_value(after).unwrap());
    }

    #[tokio::test]
    async fn template_fields_require_admin_and_use_event_context() {
        let (f, _, root) = automation_fixture().await;
        for role in [UserRole::Employee, UserRole::Manager] {
            f.db.update_user(&f.bob.user_id, None, Some(role), None).unwrap();
            let error = response_error(f.handler.handle("autopilot.template_fields", json!({"trigger_event":"channel_message"}), &f.bob).await);
            assert!(error.contains("permission denied"), "{error}");
        }
        let payload = response_payload(f.handler.handle("autopilot.template_fields", json!({"trigger_event":"channel_message"}), &root).await);
        assert_eq!(payload, json!({"fields":["channel", "agent_id", "text"]}));
        let payload = response_payload(f.handler.handle("autopilot.template_fields", json!({"trigger_event":"future_event"}), &root).await);
        assert_eq!(payload, json!({"fields":[]}));
        let payload = response_payload(f.handler.handle("autopilot.template_fields", json!({"trigger_event":"task_created",
            "sequence":{"first":{"event":"task_created"}, "then":{"event":"channel_message"}, "within_secs":60}}), &root).await);
        assert_eq!(payload, json!({"fields":["channel", "agent_id", "text"]}));
        for params in [json!({}), json!({"trigger_event":1}), json!({"trigger_event":"tick", "sequence":null}),
            json!({"trigger_event":"tick", "sequence":{}})] {
            response_error(f.handler.handle("autopilot.template_fields", params, &root).await);
        }
    }

    #[tokio::test]
    async fn tick_fields_union_active_sources_for_save_and_picker() {
        let (f, store, root) = automation_fixture().await;
        std::fs::write(f.home.join("config.toml"), r#"
[tick]
enabled = true
[[tick.sources]]
id = "first"
kind = "http_poll"
url = "https://example.com/first"
json_fields = { price = "/price" }
[[tick.sources]]
id = "second"
kind = "http_poll"
url = "https://example.com/second"
json_fields = { volume = "/volume", price = "/price" }
[[tick.sources]]
id = "disabled"
kind = "http_poll"
url = "https://example.com/disabled"
enabled = false
json_fields = { secret = "/secret" }
"#).unwrap();
        let payload = response_payload(f.handler.handle_autopilot_template_fields(json!({"trigger_event":"tick"})).await);
        assert_eq!(payload, json!({"fields":["source", "ts", "price", "volume"]}));
        let mut params = skill_rule("{{price}} / {{volume}}");
        params["trigger_event"] = json!("tick");
        response_payload(f.handler.handle_autopilot_create(params.clone(), &root).await);
        params["action"]["params"]["note"] = json!("{{secret}}");
        response_error(f.handler.handle_autopilot_create(params, &root).await);
        assert_eq!(store.list_rules().await.unwrap().len(), 1);
    }


    #[tokio::test]
    async fn update_preserves_stored_department_and_wraps_replacement_action() {
        let (f, store, root) = automation_fixture().await;
        let mut params = notify_rule();
        params["action"] = wrap_in_steps_envelope(params["action"].clone());
        let payload = response_payload(f.handler.handle_autopilot_create(params, &root).await);
        let id = payload["rule"]["id"].as_str().unwrap();
        store.set_rule_extensions(id, &crate::autopilot_store::RuleExtensionsPatch {
            active_window: None, run_policy: None, owner_department: Some(Some("sales".into())),
            updated_by_user_id: None, updated_at: "before".into(),
        }).await.unwrap();
        response_payload(f.handler.handle_autopilot_update(json!({"rule_id":id, "action":notify_rule()["action"]}), &root).await);
        let stored = store.get_rule(id).await.unwrap().unwrap();
        assert_eq!(stored.owner_department.as_deref(), Some("sales"));
        assert_eq!(serde_json::from_str::<Value>(&stored.action).unwrap()["type"], "steps");
        let mut invalid = stored.clone();
        invalid.action = notify_rule()["action"].to_string();
        assert!(validate_autopilot_rule_for_save(&f.home, &invalid).is_err());
        invalid = stored;
        invalid.sequence = Some("not JSON".into());
        assert!(validate_autopilot_rule_for_save(&f.home, &invalid).is_err());
    }

}
