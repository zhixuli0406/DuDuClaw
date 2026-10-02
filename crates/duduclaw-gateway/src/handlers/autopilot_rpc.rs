//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
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

    pub(crate) async fn handle_autopilot_create(&self, params: Value) -> WsFrame {
        let store = match self.ap_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if name.is_empty() {
            return WsFrame::error_response("", "name is required");
        }
        let trigger_event = params
            .get("trigger_event")
            .and_then(|v| v.as_str())
            .unwrap_or("task_created")
            .to_string();
        if let Err(e) = validate_autopilot_trigger_event_for_create(&trigger_event) {
            return WsFrame::error_response("", &e);
        }
        // Absent conditions mean "fire on every event of this trigger"; the
        // engine evaluates the stored `{}` as always-true.
        let conditions = params.get("conditions").cloned().unwrap_or(json!({}));
        if let Err(e) = validate_autopilot_conditions(&conditions) {
            return WsFrame::error_response("", &e);
        }
        let action = params.get("action").cloned().unwrap_or(json!({}));
        // Reject malformed rules at write time so the dashboard surfaces
        // the error immediately rather than silently in autopilot_history
        // the first time the rule would have fired.
        if let Err(e) = validate_autopilot_action(&action) {
            return WsFrame::error_response("", &e);
        }
        // P3-3: optional lightweight-CEP `sequence` spec — validated
        // structurally at write time (unknown event names / operators / an
        // out-of-range `within_secs` are all rejected here, not silently at
        // first-match time). Absent or explicit `null` → ordinary rule.
        let sequence: Option<String> = match params.get("sequence") {
            None | Some(Value::Null) => None,
            Some(v) => {
                if let Err(e) = crate::cep_matcher::validate_sequence_spec(v) {
                    return WsFrame::error_response("", &e);
                }
                Some(v.to_string())
            }
        };

        let row = AutopilotRuleRow {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            enabled: true,
            trigger_event,
            conditions: conditions.to_string(),
            action: action.to_string(),
            created_at: Utc::now().to_rfc3339(),
            last_triggered_at: None,
            trigger_count: 0,
            sequence,
            metadata: None,
        };
        if let Err(e) = store.insert_rule(&row).await {
            return WsFrame::error_response("", &format!("create autopilot rule: {e}"));
        }
        WsFrame::ok_response("", json!({ "rule": autopilot_rule_to_json(&row) }))
    }

    pub(crate) async fn handle_autopilot_update(&self, params: Value) -> WsFrame {
        let store = match self.ap_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        let rule_id = params.get("rule_id").and_then(|v| v.as_str()).unwrap_or("");
        if rule_id.is_empty() {
            return WsFrame::error_response("", "rule_id is required");
        }
        // Re-validate any provided trigger_event / action fields. A legacy
        // trigger (`cron_tick`) survives only on a rule that already had it.
        if let Some(t) = params.get("trigger_event").and_then(|v| v.as_str()) {
            let stored = match store.get_rule(rule_id).await {
                Ok(Some(row)) => row.trigger_event,
                Ok(None) => {
                    return WsFrame::error_response("", &format!("Rule not found: {rule_id}"));
                }
                Err(e) => return WsFrame::error_response("", &format!("update rule: {e}")),
            };
            if let Err(e) = validate_autopilot_trigger_event_for_update(t, &stored) {
                return WsFrame::error_response("", &e);
            }
        }
        if let Some(a) = params.get("action") {
            if let Err(e) = validate_autopilot_action(a) {
                return WsFrame::error_response("", &e);
            }
        }
        if let Some(c) = params.get("conditions") {
            if let Err(e) = validate_autopilot_conditions(c) {
                return WsFrame::error_response("", &e);
            }
        }
        // P3-3: `sequence: null` clears an existing rule back to ordinary
        // dispatch — only a non-null replacement needs structural validation.
        if let Some(s) = params.get("sequence") {
            if !s.is_null() {
                if let Err(e) = crate::cep_matcher::validate_sequence_spec(s) {
                    return WsFrame::error_response("", &e);
                }
            }
        }
        match store.update_rule(rule_id, &params).await {
            Ok(Some(row)) => {
                WsFrame::ok_response("", json!({ "rule": autopilot_rule_to_json(&row) }))
            }
            Ok(None) => WsFrame::error_response("", &format!("Rule not found: {rule_id}")),
            Err(e) => WsFrame::error_response("", &format!("update rule: {e}")),
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
