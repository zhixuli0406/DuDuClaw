//! Activated workflow runs: read the server state, cancel one run, and clear
//! an activation's consecutive-failure breaker. Access follows the source
//! draft (task access plus audience), re-read on every call.
use super::*;
use crate::workflow::WorkflowRun;

impl MethodHandler {
    /// The run, after checking the caller may see its source draft.
    pub(super) async fn accessible_run(
        &self,
        params: &Value,
        ctx: &UserContext,
    ) -> Result<WorkflowRun, WsFrame> {
        self.accessible_run_at(params, ctx, AccessLevel::Viewer)
            .await
    }

    /// The run at `level` on its source draft (Operator for cancel/reset).
    pub(super) async fn accessible_run_at(
        &self,
        params: &Value,
        ctx: &UserContext,
        level: AccessLevel,
    ) -> Result<WorkflowRun, WsFrame> {
        let id = params.get("run_id").and_then(Value::as_str).unwrap_or("");
        let store = self
            .workflow_store()
            .await
            .map_err(|e| WsFrame::error_response("", &e))?;
        let run = store
            .get_run(id)
            .await
            .map_err(|e| WsFrame::error_response("", &e))?
            .filter(|r| r.activation_id.is_some())
            .ok_or_else(|| WsFrame::error_response("", "run not found"))?;
        let draft = store
            .draft_for_workflow(&run.workflow_id, run.revision)
            .await
            .map_err(|e| WsFrame::error_response("", &e))?
            .ok_or_else(|| WsFrame::error_response("", "run not found"))?;
        self.accessible_draft_at(
            &json!({"draft_id": draft.draft_id, "revision": draft.revision}),
            ctx,
            level,
        )
        .await?;
        Ok(run)
    }

    fn fresh_context(&self, ctx: &UserContext) -> Result<UserContext, WsFrame> {
        crate::review_evidence::audience::fresh_dashboard_context(&self.home_dir, ctx)
            .map_err(|_| WsFrame::error_response("", "permission denied"))
    }

    pub(crate) async fn handle_workflow_runs_get(
        &self,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        let ctx = match self.fresh_context(ctx) {
            Ok(c) => c,
            Err(f) => return f,
        };
        let run = match self.accessible_run(&params, &ctx).await {
            Ok(r) => r,
            Err(f) => return f,
        };
        let result = async { self.workflow_service().await?.run_view(&run).await }.await;
        match result {
            Ok(view) => WsFrame::ok_response("", view),
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    pub(crate) async fn handle_workflow_runs_list(
        &self,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        let ctx = match self.fresh_context(ctx) {
            Ok(c) => c,
            Err(f) => return f,
        };
        let draft = match self.accessible_draft(&params, &ctx).await {
            Ok(d) => d,
            Err(f) => return f,
        };
        let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(20) as usize;
        let result = async {
            let service = self.workflow_service().await?;
            let runs = service
                .store
                .list_runs(&draft.definition.workflow_id, draft.revision, limit)
                .await?;
            let mut views = Vec::with_capacity(runs.len());
            for run in &runs {
                views.push(service.run_view(run).await?);
            }
            Ok::<_, String>(json!({ "runs": views }))
        }
        .await;
        match result {
            Ok(v) => WsFrame::ok_response("", v),
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    /// Manager or above with an Operator binding on the source draft.
    pub(crate) async fn handle_workflow_runs_cancel(
        &self,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        let ctx = match self.fresh_context(ctx) {
            Ok(c) => c,
            Err(f) => return f,
        };
        if !ctx.has_role(UserRole::Manager) {
            return WsFrame::error_response("", "manager role required");
        }
        let run = match self.accessible_run_at(&params, &ctx, AccessLevel::Operator).await {
            Ok(r) => r,
            Err(f) => return f,
        };
        let result = async {
            let service = self.workflow_service().await?;
            let run = service
                .cancel_run(&run.run_id, &format!("dashboard:{}", ctx.user_id))
                .await?;
            service.run_view(&run).await
        }
        .await;
        match result {
            Ok(v) => WsFrame::ok_response("", v),
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    /// Admin only: lifts the consecutive-failure lock without re-activation.
    pub(crate) async fn handle_workflow_runs_reset_failures(
        &self,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        let ctx = match self.fresh_context(ctx) {
            Ok(c) => c,
            Err(f) => return f,
        };
        if !ctx.is_admin() {
            return WsFrame::error_response("", "admin role required");
        }
        let activation = params
            .get("activation_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        let reason = params.get("reason").and_then(Value::as_str).unwrap_or("");
        let result = async {
            self.workflow_service()
                .await?
                .reset_consecutive_failures(
                    activation,
                    &format!("dashboard:{}", ctx.user_id),
                    reason,
                )
                .await
        }
        .await;
        match result {
            Ok(id) => {
                WsFrame::ok_response("", json!({ "reset_id": id, "activation_id": activation }))
            }
            Err(e) => WsFrame::error_response("", &e),
        }
    }
}
