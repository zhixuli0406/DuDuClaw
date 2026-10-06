//! P2-A C9 — dashboard RPCs for responsibilities, steering and stop.
//!
//! Every handler checks the caller at its entry: a responsibility is reached
//! through its owner employee, a task through its assignee
//! (`authorize_task_access`), and a non-admin account must be bound to that
//! employee at the required level (Viewer to read, Operator to change). Every
//! write appends one security-audit row (actor, target, before → after).

#[allow(unused_imports)]
use super::*;

use crate::responsibility::service::{self, ResponsibilityInput, ServiceError};
use crate::task_store::{RespCas, ResponsibilityRow};

fn err_frame(e: &ServiceError) -> WsFrame {
    WsFrame::error_response("", &format!("{}: {}", e.code, e.detail))
}

fn str_param<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
}

fn cas_frame(cas: RespCas) -> WsFrame {
    match cas {
        RespCas::Applied(r) => WsFrame::ok_response("", json!({ "responsibility": r })),
        RespCas::Conflict(r) => WsFrame::ok_response("", json!({ "conflict": true, "current": r })),
    }
}

fn actor(ctx: &UserContext) -> String {
    if ctx.user_id.is_empty() {
        "system".into()
    } else {
        ctx.user_id.clone()
    }
}

impl MethodHandler {
    fn responsibility_audit(&self, ctx: &UserContext, event: &str, agent: &str, detail: Value) {
        let mut detail = detail;
        detail["actor"] = json!(actor(ctx));
        duduclaw_security::audit::append_audit_event(
            &self.home_dir,
            &duduclaw_security::audit::AuditEvent::new(
                event,
                agent,
                duduclaw_security::audit::Severity::Info,
                detail,
            ),
        );
    }

    /// Load a responsibility and require `level` on its owner employee.
    async fn authorize_responsibility(
        &self,
        store: &TaskStore,
        ctx: &UserContext,
        id: &str,
        level: AccessLevel,
    ) -> Result<ResponsibilityRow, WsFrame> {
        match store.get_responsibility(id).await {
            Ok(Some(r)) => match acl::require_agent_access(ctx, &r.owner_agent_id, level) {
                Ok(()) => Ok(r),
                Err(e) => Err(WsFrame::error_response("", &e)),
            },
            Ok(None) if ctx.is_admin() => {
                Err(WsFrame::error_response("", "responsibility not found"))
            }
            Ok(None) => Err(WsFrame::error_response("", "permission denied")),
            Err(e) => Err(WsFrame::error_response("", &e)),
        }
    }

    pub(crate) async fn handle_responsibilities_rpc(
        &self,
        method: &str,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        let store = match self.task_store().await {
            Ok(s) => s,
            Err(f) => return f,
        };
        // S9 task privacy: every check below runs on the caller as the
        // account store has it now (role, status and bindings re-read), not
        // on the connection's cached context; a downgraded or unbound
        // account is refused with the shared `permission denied`.
        let live = match super::task_privacy::live_reader_context(&self.home_dir, ctx) {
            Ok(l) => l,
            Err(()) => {
                return WsFrame::error_response("", super::task_privacy::PERMISSION_DENIED);
            }
        };
        let ctx = &live;
        let now = Utc::now();
        match method {
            "responsibilities.create" => {
                let input: ResponsibilityInput = match serde_json::from_value(params.clone()) {
                    Ok(i) => i,
                    Err(e) => return WsFrame::error_response("", &format!("invalid input: {e}")),
                };
                if let Err(e) =
                    acl::require_agent_access(ctx, &input.owner_agent_id, AccessLevel::Operator)
                {
                    return WsFrame::error_response("", &e);
                }
                match service::create(&store, &self.home_dir, &input, &actor(ctx), now).await {
                    Ok(r) => {
                        self.responsibility_audit(
                            ctx,
                            "responsibility_created",
                            &r.owner_agent_id,
                            json!({"responsibility_id": r.responsibility_id, "after": "active"}),
                        );
                        // M3-2: runtimes whose usage cannot be relied on.
                        let usage_warnings = crate::responsibility::usage_hint::usage_warnings(
                            &self.home_dir,
                            &r.owner_agent_id,
                        );
                        WsFrame::ok_response(
                            "",
                            json!({ "responsibility": r, "usage_warnings": usage_warnings }),
                        )
                    }
                    Err(e) => err_frame(&e),
                }
            }
            "responsibilities.list" => {
                let agent = str_param(&params, "agent_id");
                if !ctx.is_admin() {
                    let Some(a) = agent else {
                        return WsFrame::error_response("", "agent_id parameter is required");
                    };
                    if let Err(e) = acl::require_agent_access(ctx, a, AccessLevel::Viewer) {
                        return WsFrame::error_response("", &e);
                    }
                }
                match store.list_responsibilities(agent).await {
                    Ok(rows) => WsFrame::ok_response("", json!({ "responsibilities": rows })),
                    Err(e) => WsFrame::error_response("", &e),
                }
            }
            "responsibilities.get" | "responsibilities.occurrences" | "responsibilities.fires" => {
                let Some(id) = str_param(&params, "responsibility_id") else {
                    return WsFrame::error_response("", "responsibility_id is required");
                };
                if let Err(f) = self
                    .authorize_responsibility(&store, ctx, id, AccessLevel::Viewer)
                    .await
                {
                    return f;
                }
                let out = match method {
                    "responsibilities.get" => {
                        let cost = crate::responsibility::TelemetryCostSource::new(&self.home_dir);
                        crate::responsibility::summary::summary(&store, &cost, id, now)
                            .await
                            .map(|s| json!({ "summary": s }))
                            .map_err(|e| format!("{}: {}", e.code, e.detail))
                    }
                    "responsibilities.occurrences" => store
                        .list_occurrences(id)
                        .await
                        .map(|o| json!({ "occurrences": o })),
                    _ => {
                        let limit =
                            params.get("limit").and_then(|v| v.as_u64()).unwrap_or(100) as usize;
                        store
                            .list_fires(id, limit)
                            .await
                            .map(|f| json!({ "fires": f }))
                    }
                };
                match out {
                    Ok(v) => WsFrame::ok_response("", v),
                    Err(e) => WsFrame::error_response("", &e),
                }
            }
            "responsibilities.update_contract"
            | "responsibilities.pause"
            | "responsibilities.resume"
            | "responsibilities.disable"
            | "responsibilities.enable"
            | "responsibilities.clear_failures" => {
                let Some(id) = str_param(&params, "responsibility_id") else {
                    return WsFrame::error_response("", "responsibility_id is required");
                };
                // S-L7: while the feature is off existing rows are read-only
                // except for the narrowing pause / disable.
                let narrowing = matches!(
                    method,
                    "responsibilities.pause" | "responsibilities.disable"
                );
                if !narrowing
                    && !crate::responsibility::ResponsibilityConfig::from_home(&self.home_dir)
                        .enabled
                {
                    return WsFrame::error_response(
                        "",
                        "持續任務功能目前關閉，只能暫停或停用，不接受其他變更。",
                    );
                }
                // L-1: clearing the failure streak undoes what a non-manager
                // stop counted (S-H1), so it needs a manager too.
                if method == "responsibilities.clear_failures"
                    && !ctx.has_role(duduclaw_auth::UserRole::Manager)
                {
                    return WsFrame::error_response("", "清除連續失敗需要管理者權限。");
                }
                let before = match self
                    .authorize_responsibility(&store, ctx, id, AccessLevel::Operator)
                    .await
                {
                    Ok(r) => r,
                    Err(f) => return f,
                };
                let who = actor(ctx);
                let reason = str_param(&params, "reason").unwrap_or("operator");
                let result = if method == "responsibilities.update_contract" {
                    let Some(rev) = params
                        .get("expected_contract_revision")
                        .and_then(|v| v.as_i64())
                    else {
                        return WsFrame::error_response(
                            "",
                            "expected_contract_revision is required",
                        );
                    };
                    let input: ResponsibilityInput =
                        match params.get("contract").cloned().map(serde_json::from_value) {
                            Some(Ok(i)) => i,
                            _ => return WsFrame::error_response("", "contract is required"),
                        };
                    service::update_contract(&store, &self.home_dir, id, rev, &input, &who, now)
                        .await
                } else {
                    let Some(epoch) = params
                        .get("expected_control_epoch")
                        .and_then(|v| v.as_i64())
                    else {
                        return WsFrame::error_response("", "expected_control_epoch is required");
                    };
                    match method {
                        "responsibilities.pause" => {
                            service::pause(&store, id, epoch, &who, reason, now).await
                        }
                        "responsibilities.resume" => {
                            service::resume(&store, id, epoch, &who, now).await
                        }
                        "responsibilities.disable" => {
                            service::disable(&store, id, epoch, &who, reason, now).await
                        }
                        "responsibilities.enable" => {
                            service::enable(&store, id, epoch, &who, now).await
                        }
                        _ => service::clear_failures(&store, id, epoch, &who, now).await,
                    }
                };
                match result {
                    Ok(cas) => {
                        if let RespCas::Applied(after) = &cas {
                            self.responsibility_audit(ctx, "responsibility_changed", &before.owner_agent_id, json!({
                                "responsibility_id": id, "method": method,
                                "before": {"state": before.state, "epoch": before.control_epoch, "revision": before.contract_revision},
                                "after": {"state": after.state, "epoch": after.control_epoch, "revision": after.contract_revision},
                            }));
                        }
                        cas_frame(cas)
                    }
                    Err(e) => err_frame(&e),
                }
            }
            "tasks.steer" | "tasks.steering" | "tasks.stop" | "tasks.stop_status" => {
                let Some(task_id) = str_param(&params, "task_id") else {
                    return WsFrame::error_response("", "task_id is required");
                };
                let level = if matches!(method, "tasks.steer" | "tasks.stop") {
                    AccessLevel::Operator
                } else {
                    AccessLevel::Viewer
                };
                // The shared task-content gate: agent ACL at `level` plus the
                // task's TaskPacket audience (directions and stop state are
                // task content).
                let task = match self
                    .authorize_private_task_read(&store, ctx, task_id, level)
                    .await
                {
                    Ok((t, _)) => t,
                    Err(f) => return f,
                };
                self.handle_task_control(method, &params, ctx, &store, &task, now)
                    .await
            }
            _ => WsFrame::error_response("", &format!("unknown method: {method}")),
        }
    }

    async fn handle_task_control(
        &self,
        method: &str,
        params: &Value,
        ctx: &UserContext,
        store: &Arc<TaskStore>,
        task: &TaskRow,
        now: DateTime<Utc>,
    ) -> WsFrame {
        let queue = self.message_queue.read().await.clone();
        let broker = crate::approval::ApprovalBroker::open(&self.home_dir).ok();
        match method {
            "tasks.steer" => {
                let body = params.get("body").and_then(|v| v.as_str()).unwrap_or("");
                let crid = params
                    .get("client_request_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                match crate::responsibility::steering::submit(
                    store,
                    &self.home_dir,
                    &task.id,
                    body,
                    &actor(ctx),
                    crid,
                    now,
                )
                .await
                {
                    Ok(out) => {
                        let (row, duplicate) = match out {
                            crate::task_store::SteeringSubmit::Created(r) => (r, false),
                            crate::task_store::SteeringSubmit::Duplicate(r) => (r, true),
                        };
                        if !duplicate {
                            self.responsibility_audit(
                                ctx,
                                "task_steering_submitted",
                                &task.assigned_to,
                                json!({"task_id": task.id, "seq": row.seq, "after": "pending"}),
                            );
                        }
                        WsFrame::ok_response("", json!({ "steering": row, "duplicate": duplicate }))
                    }
                    Err(e) => err_frame(&e),
                }
            }
            "tasks.steering" => {
                match crate::responsibility::steering::list(store, &task.id).await {
                    Ok(rows) => WsFrame::ok_response(
                        "",
                        json!({ "steering": rows, "authority_revision": task.authority_revision }),
                    ),
                    Err(e) => err_frame(&e),
                }
            }
            "tasks.stop" => {
                let Some(queue) = queue else {
                    return WsFrame::error_response("", "message queue not initialized");
                };
                let Some(rev) = params
                    .get("expected_authority_revision")
                    .and_then(|v| v.as_i64())
                else {
                    return WsFrame::error_response("", "expected_authority_revision is required");
                };
                match crate::responsibility::stop::stop_task(
                    store,
                    &queue,
                    broker.as_ref(),
                    None,
                    &self.home_dir,
                    &task.id,
                    rev,
                    &actor(ctx),
                    // S-H1: a stop decided below manager level counts as an
                    // unsuccessful occurrence, so an operator bound to the
                    // employee cannot reset its failure streak by stopping.
                    !ctx.has_role(duduclaw_auth::UserRole::Manager),
                    now,
                )
                .await
                {
                    Ok(status) => {
                        self.responsibility_audit(
                            ctx,
                            "task_stop_requested",
                            &task.assigned_to,
                            json!({
                                "task_id": task.id, "before": task.status, "after": status.state,
                                "affected": status.affected_task_ids.len(),
                            }),
                        );
                        WsFrame::ok_response("", json!({ "stop": status }))
                    }
                    Err(e) => err_frame(&e),
                }
            }
            _ => {
                // S-L4: a Viewer reads the stored state; only callers who may
                // stop the task also drive a reconciliation pass.
                if acl::require_agent_access(ctx, &task.assigned_to, AccessLevel::Operator).is_err()
                {
                    return match crate::responsibility::stop::stop_status_stored(store, &task.id)
                        .await
                    {
                        Ok(status) => WsFrame::ok_response(
                            "",
                            json!({ "stop": status, "authority_revision": task.authority_revision }),
                        ),
                        Err(e) => err_frame(&e),
                    };
                }
                let Some(queue) = queue else {
                    return WsFrame::error_response("", "message queue not initialized");
                };
                match crate::responsibility::stop::stop_status(
                    store,
                    &queue,
                    broker.as_ref(),
                    None,
                    &task.id,
                    now,
                )
                .await
                {
                    // `authority_revision` is what `tasks.stop` must echo back.
                    Ok(status) => WsFrame::ok_response(
                        "",
                        json!({ "stop": status, "authority_revision": task.authority_revision }),
                    ),
                    Err(e) => err_frame(&e),
                }
            }
        }
    }
}
