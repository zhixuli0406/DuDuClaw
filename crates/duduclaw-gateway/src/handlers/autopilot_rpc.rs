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
        let definition = autopilot_create_definition(&self.home_dir, &params, ctx, None);
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
        let definition = autopilot_update_definition(&self.home_dir, &params, &stored);
        let row = match definition {
            Ok(row) => row,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let now = Utc::now().to_rfc3339();
        match store.replace_company_rule_definition(rule_id, &row, Some(&ctx.user_id), &now).await {
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

    pub(crate) async fn handle_autopilot_runs_list(&self, params: Value) -> WsFrame {
        let Some(rule_id) = params.get("rule_id").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
            return WsFrame::error_response("", "rule_id is required and must be a non-empty string");
        };
        let limit = match params.get("limit") {
            None => 20,
            Some(value) => match value.as_i64() {
                Some(limit) if (1..=100).contains(&limit) => limit,
                _ => return WsFrame::error_response("", "limit must be an integer between 1 and 100"),
            },
        };
        let store = match self.ap_store().await {
            Ok(store) => store,
            Err(frame) => return frame,
        };
        let runs = match store.list_runs_for_rule(rule_id, limit).await {
            Ok(runs) => runs,
            Err(error) => return WsFrame::error_response("", &format!("list autopilot runs: {error}")),
        };
        let mut result = Vec::with_capacity(runs.len());
        for run in runs {
            let steps = match store.list_run_steps(&run.run_id).await {
                Ok(steps) => steps,
                Err(error) => return WsFrame::error_response("", &format!("list autopilot run steps: {error}")),
            };
            // Explicit status projection: never serialize the durable rows, which
            // carry customer event data and internal execution identifiers.
            let steps: Vec<Value> = steps.iter().map(|step| json!({
                "step_index": step.step_index,
                "step_type": step.step_type,
                "state": step.state,
                "started_at": step.started_at,
                "finished_at": step.finished_at,
                "detail": step.detail,
            })).collect();
            result.push(json!({
                "run_id": run.run_id,
                "state": run.state,
                "step_index": run.step_index,
                "step_count": run.step_count,
                "next_at": run.next_at,
                "outcome": run.outcome,
                "detail": run.detail,
                "event_name": run.event_name,
                "created_at": run.created_at,
                "updated_at": run.updated_at,
                "steps": steps,
            }));
        }
        WsFrame::ok_response("", json!({"runs": result}))
    }

    pub(crate) async fn handle_autopilot_runs_cancel(&self, params: Value, ctx: &UserContext) -> WsFrame {
        use crate::autopilot_store::{RUN_PENDING, RUN_WAITING, RUN_AWAITING_APPROVAL};
        let Some(run_id) = params.get("run_id").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
            return WsFrame::error_response("", "run_id is required and must be a non-empty string");
        };
        let store = match self.ap_store().await {
            Ok(store) => store,
            Err(frame) => return frame,
        };
        let run = match store.get_run(run_id).await {
            Ok(Some(run)) => run,
            Ok(None) => return WsFrame::error_response("", &format!("Run not found: {run_id}")),
            Err(error) => return WsFrame::error_response("", &format!("get autopilot run: {error}")),
        };
        if !matches!(run.state.as_str(), RUN_PENDING | RUN_WAITING | RUN_AWAITING_APPROVAL) {
            return autopilot_run_cancel_refused(&run.state);
        }
        let detail = format!("由 {} 取消", ctx.user_id);
        // Match exactly the state observed above so the audit and approval cleanup
        // describe the state that was actually cancelled, even during a resume tick.
        match store.settle_unclaimed_run(run_id, &[run.state.as_str()], "cancelled_by_admin", Some(&detail), Utc::now()).await {
            Ok(true) => {}
            Ok(false) => return match store.get_run(run_id).await {
                Ok(Some(current)) => autopilot_run_cancel_refused(&current.state),
                Ok(None) => WsFrame::error_response("", &format!("Run not found: {run_id}")),
                Err(error) => WsFrame::error_response("", &format!("get autopilot run: {error}")),
            },
            Err(error) => return WsFrame::error_response("", &format!("cancel autopilot run: {error}")),
        }
        duduclaw_security::audit::append_audit_event(
            &self.home_dir,
            &duduclaw_security::audit::AuditEvent::new(
                "autopilot_run_cancelled", "autopilot", duduclaw_security::audit::Severity::Info,
                json!({"actor": ctx.user_id, "rule_id": run.rule_id, "run_id": run_id, "previous_state": run.state}),
            ),
        );
        if run.state == RUN_AWAITING_APPROVAL {
            // The executor's fenced run-state check is the execution guarantee.
            // Card withdrawal is best-effort and cannot undo the settled run.
            let cleanup = async {
                let steps = store.list_run_steps(run_id).await?;
                let dispatch_id = steps.iter().find(|step| step.step_index == run.step_index)
                    .and_then(|step| step.dispatch_id.as_deref())
                    .ok_or_else(|| "approval step has no dispatch binding".to_string())?;
                let dispatch = store.get_dispatch(dispatch_id).await?
                    .ok_or_else(|| "approval dispatch not found".to_string())?;
                let approval_id = dispatch.approval_id
                    .ok_or_else(|| "approval dispatch has no approval id".to_string())?;
                let broker = crate::approval::ApprovalBroker::open(&self.home_dir)?;
                let withdrawn = broker.withdraw(&crate::approval::ApprovalId::from(approval_id), "system:autopilot_run_cancelled").await?;
                if !withdrawn {
                    return Err("approval card was not pending or could not be withdrawn".to_string());
                }
                Ok::<(), String>(())
            }.await;
            if let Err(error) = cleanup {
                tracing::warn!(%run_id, %error, "autopilot: cancelled run approval card withdrawal failed");
            }
        }
        WsFrame::ok_response("", json!({"cancelled": true, "run_id": run_id}))
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

/// Shared normalization and validation for company and department creates.
pub(super) fn autopilot_create_definition(
    home: &Path, params: &Value, ctx: &UserContext, owner_department: Option<String>,
) -> Result<AutopilotRuleRow, String> {
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
        sequence: optional_rule_object(params, "sequence")?,
        active_window: optional_rule_object(params, "active_window")?,
        run_policy: optional_rule_object(params, "run_policy")?,
        owner_department,
        created_by_user_id: Some(ctx.user_id.clone()),
        updated_by_user_id: None,
        updated_at: None,
        created_at: Utc::now().to_rfc3339(),
        last_triggered_at: None,
        trigger_count: 0,
        metadata: None,
    };
    wrap_rule_action_if_needed(&mut row)?;
    validate_autopilot_rule_for_save(home, &row)?;
    Ok(row)
}

/// Department updates validate the entire resulting row, including toggles.
pub(super) fn autopilot_update_definition(
    home: &Path, params: &Value, stored: &AutopilotRuleRow,
) -> Result<AutopilotRuleRow, String> {
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
    // Preserve legacy company definitions for toggles and renames, even if
    // current validators reject them. Department writes always revalidate.
    if !changes_definition && row.owner_department.is_none() {
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
        row.sequence = optional_rule_object(params, "sequence")?;
    }
    if params.get("active_window").is_some() {
        row.active_window = optional_rule_object(params, "active_window")?;
    }
    if params.get("run_policy").is_some() {
        row.run_policy = optional_rule_object(params, "run_policy")?;
    }
    validate_autopilot_trigger_event_for_update(&row.trigger_event, &stored.trigger_event)?;
    let conditions: Value = serde_json::from_str(&row.conditions).map_err(|_| "invalid conditions JSON")?;
    validate_autopilot_conditions(&conditions)?;
    wrap_rule_action_if_needed(&mut row)?;
    validate_autopilot_rule_for_save(home, &row)?;
    Ok(row)
}

fn autopilot_run_cancel_refused(state: &str) -> WsFrame {
    use crate::autopilot_store::{RUN_RUNNING, RUN_SETTLED};
    let message = match state {
        RUN_RUNNING => "目前正在執行步驟，請稍後再試",
        RUN_SETTLED => "這次執行已結束，無法取消",
        _ => "執行狀態已變更或無法取消，請重新整理後再試",
    };
    WsFrame::error_response("", message)
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

    async fn insert_status_run(store: &AutopilotStore, id: &str, state: &str, now: chrono::DateTime<Utc>) {
        use crate::autopilot_store::{AutopilotRunRow, AutopilotRunStepRow, RunInsertOutcome, run_ts};
        let run = AutopilotRunRow {
            gateway_origin: false,
            run_id: id.into(), rule_id: "status-rule".into(), rule_name: "Status".into(),
            rule_hash: "private-hash".into(), event_name: "task_created".into(),
            event_ref: "private-event".into(), dedupe_key: format!("dedupe-{id}"),
            fields_json: r#"{"text":"customer secret"}"#.into(), state: state.into(),
            step_index: 1, step_count: 3, next_at: Some(run_ts(now)),
            lease_owner: Some("private-owner".into()), lease_until: Some(run_ts(now)),
            owner_department: None, outcome: None, detail: Some("status detail".into()),
            notice_pending: false, notice_push_state: None, notice_push_attempts: 0,
            notice_push_next_at: None, created_at: run_ts(now), updated_at: run_ts(now),
        };
        let steps: Vec<_> = (0..3).map(|index| AutopilotRunStepRow {
            run_id: id.into(), step_index: index,
            step_type: if index == 1 { "wait" } else { "delegate" }.into(),
            state: if index == 1 && matches!(state, "running" | "awaiting_approval") { "running" }
                else if index < 2 { "done" } else { "pending" }.into(),
            idempotency_key: format!("key-{id}-{index}"),
            dispatch_id: if matches!(state, "settled" | "awaiting_approval") { Some(format!("dispatch-{id}-{index}")) } else { None },
            queue_message_id: if state == "settled" { Some(format!("queue-{id}-{index}")) } else { None },
            started_at: None, finished_at: None, detail: None,
        }).collect();
        assert_eq!(store.insert_run(&run, &steps, now).await.unwrap(), RunInsertOutcome::Inserted);
    }

    fn assert_status_has_no_private_keys(value: &Value) {
        match value {
            Value::Object(object) => {
                for key in ["fields_json", "dedupe_key", "lease_owner", "idempotency_key", "dispatch_id", "queue_message_id"] {
                    assert!(!object.contains_key(key), "private key: {key}");
                }
                for value in object.values() { assert_status_has_no_private_keys(value); }
            }
            Value::Array(values) => for value in values { assert_status_has_no_private_keys(value); },
            _ => {}
        }
    }

    #[tokio::test]
    async fn runs_list_returns_newest_status_with_steps_and_no_private_fields() {
        let (f, store, root) = automation_fixture().await;
        let now = Utc::now();
        for i in 0..21 {
            insert_status_run(&store, &format!("run-{i}"), "settled", now + chrono::Duration::seconds(i)).await;
        }
        let payload = response_payload(f.handler.handle("autopilot.runs.list", json!({"rule_id":"status-rule"}), &root).await);
        let runs = payload["runs"].as_array().unwrap();
        assert_eq!(runs.len(), 20);
        assert_eq!(runs[0]["run_id"], "run-20");
        assert_eq!(runs[19]["run_id"], "run-1");
        let newest = store.get_run("run-20").await.unwrap().unwrap();
        assert_eq!(runs[0], json!({
            "run_id":"run-20", "state":"settled", "step_index":1, "step_count":3,
            "next_at":newest.next_at, "outcome":null, "detail":"status detail",
            "event_name":"task_created", "created_at":newest.created_at, "updated_at":newest.updated_at,
            "steps":[
                {"step_index":0,"step_type":"delegate","state":"done","started_at":null,"finished_at":null,"detail":null},
                {"step_index":1,"step_type":"wait","state":"done","started_at":null,"finished_at":null,"detail":null},
                {"step_index":2,"step_type":"delegate","state":"pending","started_at":null,"finished_at":null,"detail":null}
            ]
        }));
        assert_status_has_no_private_keys(&payload);
        assert!(!payload.to_string().contains("customer secret"));
        for (limit, count) in [(1, 1), (100, 21)] {
            let payload = response_payload(f.handler.handle("autopilot.runs.list", json!({"rule_id":"status-rule","limit":limit}), &root).await);
            assert_eq!(payload["runs"].as_array().unwrap().len(), count);
        }
        let payload = response_payload(f.handler.handle("autopilot.runs.list", json!({"rule_id":"other"}), &root).await);
        assert_eq!(payload, json!({"runs":[]}));
    }

    #[tokio::test]
    async fn runs_list_validates_required_rule_and_limit_bounds() {
        let (f, _, root) = automation_fixture().await;
        for params in [json!({}), json!({"rule_id":null}), json!({"rule_id":1}), json!({"rule_id":""})] {
            assert!(response_error(f.handler.handle("autopilot.runs.list", params, &root).await).contains("rule_id"));
        }
        for limit in [json!(0), json!(101), json!(-1), json!(1.5), json!("20"), Value::Null] {
            assert!(response_error(f.handler.handle("autopilot.runs.list", json!({"rule_id":"status-rule","limit":limit}), &root).await).contains("limit"));
        }
    }

    #[tokio::test]
    async fn runs_cancel_waiting_settles_and_resume_leaves_remaining_steps_pending() {
        use crate::autopilot_store::{RUN_SETTLED, STEP_PENDING};
        let (f, store, root) = automation_fixture().await;
        let now = Utc::now();
        insert_status_run(&store, "waiting", "waiting", now).await;
        let payload = response_payload(f.handler.handle("autopilot.runs.cancel", json!({"run_id":"waiting"}), &root).await);
        assert_eq!(payload, json!({"cancelled":true,"run_id":"waiting"}));
        let run = store.get_run("waiting").await.unwrap().unwrap();
        assert_eq!(run.state, RUN_SETTLED);
        assert_eq!(run.outcome.as_deref(), Some("cancelled_by_admin"));
        assert_eq!(run.detail, Some(format!("由 {} 取消", root.user_id)));
        assert!(run.notice_pending);
        let events = duduclaw_security::audit::read_recent_events(&f.home, 100);
        let events: Vec<_> = events.iter().filter(|e| e.event_type == "autopilot_run_cancelled").collect();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].details, json!({"actor":root.user_id,"rule_id":"status-rule","run_id":"waiting","previous_state":"waiting"}));
        duduclaw_core::gateway_instance::acquire(&f.home).unwrap();
        let mq = Arc::new(crate::message_queue::MessageQueue::open(&f.home).unwrap());
        let tasks = Arc::new(TaskStore::open(&f.home).unwrap());
        let (_tx, rx) = tokio::sync::broadcast::channel(16);
        let engine = crate::autopilot_engine::AutopilotEngine::new(f.home.clone(), store.clone(), tasks, Some(mq.clone()), rx);
        engine.resume_runs_once(now + chrono::Duration::minutes(1)).await.unwrap();
        assert!(!store.claim_run("waiting", "later-executor", now + chrono::Duration::minutes(1)).await.unwrap());
        assert_eq!(store.get_run("waiting").await.unwrap().unwrap().state, RUN_SETTLED);
        assert_eq!(store.list_run_steps("waiting").await.unwrap()[2].state, STEP_PENDING);
        assert!(mq.pending_messages(100).await.unwrap().is_empty());
        response_error(f.handler.handle("autopilot.runs.cancel", json!({"run_id":"waiting"}), &root).await);
        let events = duduclaw_security::audit::read_recent_events(&f.home, 100);
        assert_eq!(events.iter().filter(|e| e.event_type == "autopilot_run_cancelled").count(), 1);
    }

    #[tokio::test]
    async fn runs_cancel_accepts_pending_and_awaiting_approval() {
        let (f, store, root) = automation_fixture().await;
        for state in ["pending", "awaiting_approval"] {
            insert_status_run(&store, state, state, Utc::now()).await;
            // No ledger row: failed best-effort card cleanup must not undo cancellation.
            response_payload(f.handler.handle("autopilot.runs.cancel", json!({"run_id":state}), &root).await);
            let run = store.get_run(state).await.unwrap().unwrap();
            assert_eq!(run.state, "settled");
            assert_eq!(run.outcome.as_deref(), Some("cancelled_by_admin"));
        }
    }

    #[tokio::test]
    async fn runs_cancel_awaiting_approval_withdraws_bound_card() {
        use crate::approval::{ApprovalBroker, ApprovalStatus};
        use crate::autopilot_store::{AutopilotDispatchRow, StepApprovalBindMode, run_ts};
        let (f, store, root) = automation_fixture().await;
        let now = Utc::now();
        insert_status_run(&store, "gated", "running", now).await;
        let broker = ApprovalBroker::open(&f.home).unwrap();
        let approval_id = broker.request("autopilot", crate::autopilot_approval::ACTION_KIND,
            "Gated step", json!({}), 3600).await.unwrap();
        let row = AutopilotDispatchRow {
            dispatch_id: "gated-dispatch".into(), approval_id: None,
            rule_id: "status-rule".into(), rule_name: "Status".into(), rule_hash: "private-hash".into(),
            event_name: "task_created".into(), event_ref: "private-event".into(),
            dedupe_key: "gated-dispatch-dedupe".into(), frozen_json: "{}".into(), frozen_digest: "digest".into(),
            state: "requesting".into(), outcome: None, detail: None,
            notice_activity_pending: false, notice_push_state: None, notice_push_attempts: 0,
            notice_push_next_at: None, notice_pushed_at: None, created_at: run_ts(now), updated_at: run_ts(now),
            run_id: Some("gated".into()), step_index: Some(1),
        };
        assert!(store.insert_dispatch(&row).await.unwrap());
        assert!(store.bind_step_dispatch_awaiting("gated-dispatch", approval_id.as_str(), "gated", 1,
            "private-owner", now, StepApprovalBindMode::Filing).await.unwrap());
        response_payload(f.handler.handle("autopilot.runs.cancel", json!({"run_id":"gated"}), &root).await);
        let approval = broker.get(&approval_id).await.unwrap().unwrap();
        assert_eq!(approval.status, ApprovalStatus::Denied);
        assert_eq!(approval.decided_by.as_deref(), Some("system:autopilot_run_cancelled"));
        let run = store.get_run("gated").await.unwrap().unwrap();
        assert_eq!(run.state, "settled");
        assert_eq!(run.outcome.as_deref(), Some("cancelled_by_admin"));
        let steps = store.list_run_steps("gated").await.unwrap();
        assert_eq!(steps[1].state, "failed");
        assert_eq!(steps[2].state, "pending");
    }

    #[tokio::test]
    async fn runs_cancel_refuses_running_settled_unknown_and_invalid_ids() {
        let (f, store, root) = automation_fixture().await;
        for (state, message) in [("running", "正在執行步驟"), ("settled", "已結束")] {
            insert_status_run(&store, state, state, Utc::now()).await;
            let error = response_error(f.handler.handle("autopilot.runs.cancel", json!({"run_id":state}), &root).await);
            assert!(error.contains(message), "{error}");
            assert_eq!(store.get_run(state).await.unwrap().unwrap().state, state);
        }
        assert!(response_error(f.handler.handle("autopilot.runs.cancel", json!({"run_id":"unknown"}), &root).await).contains("not found"));
        for params in [json!({}), json!({"run_id":1}), json!({"run_id":""}), json!({"run_id":null})] {
            assert!(response_error(f.handler.handle("autopilot.runs.cancel", params, &root).await).contains("run_id"));
        }
        assert!(duduclaw_security::audit::read_recent_events(&f.home, 100).iter().all(|e| e.event_type != "autopilot_run_cancelled"));
    }

    #[tokio::test]
    async fn runs_routes_require_admin() {
        let (f, store, _) = automation_fixture().await;
        insert_status_run(&store, "waiting", "waiting", Utc::now()).await;
        for role in [UserRole::Employee, UserRole::Manager] {
            f.db.update_user(&f.bob.user_id, None, Some(role), None).unwrap();
            for (method, params) in [
                ("autopilot.runs.list", json!({"rule_id":"status-rule"})),
                ("autopilot.runs.cancel", json!({"run_id":"waiting"}))
            ] {
                assert!(response_error(f.handler.handle(method, params, &f.bob).await).contains("permission denied"));
            }
        }
        assert_eq!(store.get_run("waiting").await.unwrap().unwrap().state, "waiting");
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
    async fn legacy_update_refuses_stored_department_and_shared_builder_wraps_action() {
        let (f, store, root) = automation_fixture().await;
        let mut params = notify_rule();
        params["action"] = wrap_in_steps_envelope(params["action"].clone());
        let payload = response_payload(f.handler.handle_autopilot_create(params, &root).await);
        let id = payload["rule"]["id"].as_str().unwrap();
        store.set_rule_extensions(id, &crate::autopilot_store::RuleExtensionsPatch {
            active_window: None, run_policy: None, owner_department: Some(Some("sales".into())),
            updated_by_user_id: None, updated_at: "before".into(),
        }).await.unwrap();
        let before = store.get_rule(id).await.unwrap().unwrap();
        let patch = json!({"rule_id":id, "action":notify_rule()["action"]});
        let normalized = autopilot_update_definition(&f.home, &patch, &before).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&normalized.action).unwrap()["type"], "steps");
        assert!(response_error(f.handler.handle_autopilot_update(patch, &root).await).contains("permission denied"));
        let stored = store.get_rule(id).await.unwrap().unwrap();
        assert_eq!(serde_json::to_value(&stored).unwrap(), serde_json::to_value(&before).unwrap());
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
