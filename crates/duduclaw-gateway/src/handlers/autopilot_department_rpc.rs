//! Department-scoped write/read admission; company-wide Admin RPCs stay legacy.

use super::*;
use crate::dept_grants::{DeptGrantView, GrantAction, GrantGroup, record_grant_use};
use crate::dept_grants::rules::validate_department_rule;
use super::autopilot_rpc::{autopilot_create_definition, autopilot_update_definition};

const DENIED: &str = "permission denied";

fn owns_department(view: &DeptGrantView, row: &AutopilotRuleRow) -> bool {
    row.owner_department.as_ref().is_some_and(|department|
        view.departments_for(GrantGroup::Automation).contains(department))
}

fn optional_json(params: &Value, field: &str) -> Option<String> {
    params.get(field).filter(|v| !v.is_null()).map(Value::to_string)
}

impl MethodHandler {
    pub(crate) async fn handle_department_autopilot(
        &self, method: &str, params: Value, ctx: &UserContext,
    ) -> WsFrame {
        let admin = acl::require_role(ctx, UserRole::Admin).is_ok();
        if admin {
            match method {
                "autopilot.list" => return self.handle_autopilot_list().await,
                "autopilot.remove" => return self.handle_autopilot_remove(params).await,
                "autopilot.history" => return self.handle_autopilot_history(params).await,
                "autopilot.create" if params.get("owner_department").is_none_or(Value::is_null) => {
                    return self.handle_autopilot_create(params, ctx).await;
                }
                _ => {}
            }
        }
        let view = if admin { None } else {
            let view = DeptGrantView::load(self, ctx).await;
            if view.is_empty() || view.departments_for(GrantGroup::Automation).is_empty() {
                return WsFrame::error_response("", DENIED);
            }
            Some(view)
        };
        let store = match self.ap_store().await {
            Ok(store) => store,
            Err(frame) => return frame,
        };
        // Company-wide legacy updates retain their validator and patch semantics.
        // Department updates below read/authorize/validate/write under one fence.
        if admin && method == "autopilot.update" {
            let id = params.get("rule_id").and_then(Value::as_str).unwrap_or("");
            match store.get_rule(id).await {
                Ok(Some(row)) if row.owner_department.is_some() => {}
                Ok(_) => return self.handle_autopilot_update(params, ctx).await,
                Err(error) => return WsFrame::error_response("", &error),
            }
        }
        match self.department_autopilot_result(&store, method, &params, ctx, view.as_ref()).await {
            Ok(payload) => WsFrame::ok_response("", payload),
            Err(error) => WsFrame::error_response("", &error),
        }
    }

    async fn department_autopilot_result(
        &self, store: &AutopilotStore, method: &str, params: &Value,
        ctx: &UserContext, view: Option<&DeptGrantView>,
    ) -> Result<Value, String> {
        if method == "autopilot.list" {
            let view = view.ok_or(DENIED)?;
            let rows = store.list_rules().await?;
            let rules: Vec<Value> = rows.iter().filter(|row| owns_department(view, row))
                .map(autopilot_rule_to_json).collect();
            return Ok(json!({"rules": rules}));
        }
        if method == "autopilot.create" {
            let now = Utc::now().to_rfc3339();
            let department = params.get("owner_department").and_then(Value::as_str).ok_or(DENIED)?;
            let mut row = autopilot_create_definition(&self.home_dir, params, ctx, Some(department.to_owned()))?;
            row.metadata = optional_json(params, "metadata");
            row.updated_by_user_id = Some(ctx.user_id.clone());
            row.updated_at = Some(now);
            validate_department_rule(self, view, &row).await?;
            store.insert_rule(&row).await?;
            if view.is_some() {
                record_grant_use(self, ctx, department, GrantGroup::Automation, GrantAction::Create, method).await;
            }
            return Ok(json!({"rule": autopilot_rule_to_json(&row)}));
        }

        let id = params.get("rule_id").and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or(DENIED)?;
        let (fence, row) = store.fence_rule(id).await?;
        let mut row = row.ok_or(DENIED)?;
        let department = row.owner_department.clone().ok_or(DENIED)?;
        if view.is_some_and(|v| !owns_department(v, &row)) { return Err(DENIED.into()); }
        let (payload, audit_action) = match method {
            "autopilot.update" => {
                if params.get("owner_department").is_some_and(|v| v.as_str() != Some(department.as_str())) {
                    return Err(DENIED.into());
                }
                row = autopilot_update_definition(&self.home_dir, params, &row)?;
                if params.get("metadata").is_some() {
                    row.metadata = optional_json(params, "metadata");
                }
                row.updated_by_user_id = Some(ctx.user_id.clone());
                row.updated_at = Some(Utc::now().to_rfc3339());
                validate_department_rule(self, view, &row).await?;
                fence.write_department_rule(&row)?;
                (json!({"rule": autopilot_rule_to_json(&row)}), Some(GrantAction::Edit))
            }
            "autopilot.remove" => {
                fence.remove_department_rule(id)?;
                (json!({"success": true}), Some(GrantAction::Remove))
            }
            "autopilot.history" => {
                let limit = params.get("limit").and_then(Value::as_i64).unwrap_or(20);
                let entries = fence.department_history(id, limit)?;
                (json!({"entries": entries}), None)
            }
            _ => return Err(DENIED.into()),
        };
        fence.release()?;
        if let (Some(_), Some(action)) = (view, audit_action) {
            record_grant_use(self, ctx, &department, GrantGroup::Automation, action, method).await;
        }
        Ok(payload)
    }
}
