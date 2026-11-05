//! Gateway-owned A2A operations. Exposure only narrows live dashboard ACLs.
use super::*;
use crate::a2a::{self, Config, Error};
use serde::{Deserialize, Serialize};

static CALLER_LIMIT: std::sync::LazyLock<duduclaw_security::rate_limiter::RateLimiter> =
    std::sync::LazyLock::new(|| duduclaw_security::rate_limiter::RateLimiter::new(60, std::time::Duration::from_secs(60)));

fn principal(ctx: &UserContext) -> String { format!("user:{}", ctx.user_id) }
fn owner(ctx: &UserContext) -> String { format!("a2a:{}", principal(ctx)) }

fn request_error(error: String) -> Error {
    if error == "A2A messageId already used with a different payload" {
        Error::new(-32602, "messageId already used with a different payload", "INVALID_ARGUMENT")
    } else if error == "A2A original task is no longer available" {
        Error::missing()
    } else { Error::denied() }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PageCursor {
    after: i64,
    snapshot: i64,
    query: String,
}

fn published_skill(id: &str, skill: &duduclaw_agent::registry::SkillFile) -> Option<Value> {
    let meta = duduclaw_agent::skill_loader::parse_skill_meta_from_content(&skill.content, &skill.name);
    if meta.name.trim().is_empty() || meta.description.trim().is_empty() { return None; }
    let mut tags = meta.tags;
    tags.retain(|tag| !tag.trim().is_empty());
    tags.push(format!("employee:{id}"));
    Some(json!({"id": format!("{}:{}", id, skill.name),
        "name": meta.name, "description": meta.description, "tags": tags,
        "inputModes": ["text/plain"], "outputModes": ["text/plain"]}))
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TaskParams {
    id: String,
    history_length: Option<u32>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListParams {
    context_id: Option<String>,
    status: Option<String>,
    page_size: Option<usize>,
    page_token: Option<String>,
    history_length: Option<u32>,
    include_artifacts: Option<bool>,
    status_timestamp_after: Option<String>,
}

impl MethodHandler {
    pub(crate) async fn a2a_card(&self, ctx: Option<&UserContext>) -> Result<Value, Error> {
        let config = Config::from_home(&self.home_dir)?;
        let mut visible = Vec::new();
        for id in &config.exposed_agents {
            if let Some(ctx) = ctx {
                if !ctx.has_agent_access(id, AccessLevel::Operator)
                    || self.deny_main_for_non_admin(ctx, id).await.is_err()
                    || crate::machine_employee::check_target(&self.home_dir, id, Some(&format!("user:{}", ctx.user_id))).is_err() {
                    continue;
                }
            }
            visible.push(id);
        }
        let registry = self.registry.read().await;
        let mut skills = Vec::new();
        for id in visible {
            let Some(agent) = registry.get(id) else { continue; };
            for skill in &agent.skills {
                if let Some(skill) = published_skill(id, skill) { skills.push(skill); }
            }
        }
        if skills.is_empty() {
            return Err(Error::new(-32007, "No agent card is configured for this caller", "EXTENDED_AGENT_CARD_NOT_CONFIGURED"));
        }
        skills.sort_by_key(|skill| skill["id"].as_str().unwrap_or("").to_string());
        skills.dedup_by(|a, b| a["id"] == b["id"]);
        let interfaces = vec![json!({"url": config.public_url, "protocolBinding": "JSONRPC", "protocolVersion": "1.0"})];
        Ok(json!({"name": "DuDuClaw", "description": "Operator-exposed employee skills and asynchronous tasks",
            "version": crate::updater::current_version(), "supportedInterfaces": interfaces,
            "capabilities": {"extendedAgentCard": true},
            "defaultInputModes": ["text/plain"], "defaultOutputModes": ["text/plain"], "skills": skills,
            "securitySchemes": {"dashboardBearer": {"httpAuthSecurityScheme": {
                "scheme": "Bearer", "bearerFormat": "JWT", "description": "Dashboard account access token"}}},
            "securityRequirements": [{"schemes": {"dashboardBearer": {}}}]}))
    }

    pub(crate) async fn handle_a2a_rpc(&self, method: &str, params: Value, ctx: &UserContext) -> WsFrame {
        let method = a2a::canonical_method(method);
        let result = if ctx.user_id == "system" || !ctx.has_role(a2a::minimum_role(method)) {
            Err(Error::denied())
        } else if !CALLER_LIMIT.check_and_record(&ctx.user_id).await {
            Err(Error::new(-32000, "Rate limit exceeded", "RESOURCE_EXHAUSTED"))
        } else if params.to_string().len() > 64 * 1024 {
            Err(Error::invalid())
        } else {
            match a2a::audit(&self.home_dir, &ctx.user_id, method, "requested") {
                Ok(()) => self.a2a_operation(method, params, ctx).await,
                Err(error) => Err(error),
            }
        };
        // The admission record above is durable before any effect. A failed
        // completion audit must not turn an already-created task into a retry.
        let _ = a2a::audit(&self.home_dir, &ctx.user_id, method,
            if result.is_ok() { "succeeded" } else { "refused" });
        match result {
            Ok(value) => WsFrame::ok_response("", value),
            Err(error) => WsFrame::Response { id: String::new(), ok: false, payload: None, error: Some(error.value()) },
        }
    }

    async fn a2a_operation(&self, method: &str, params: Value, ctx: &UserContext) -> Result<Value, Error> {
        let config = Config::from_home(&self.home_dir)?;
        match method {
            "GetExtendedAgentCard" => {
                if params != json!({}) { return Err(Error::invalid()); }
                self.a2a_card(Some(ctx)).await
            }
            "SendMessage" => self.a2a_send(params, ctx, &config).await,
            "GetTask" | "CancelTask" => {
                let params: TaskParams = serde_json::from_value(params).map_err(|_| Error::invalid())?;
                if params.id.is_empty() || params.id.len() > 128 { return Err(Error::invalid()); }
                if params.history_length.is_some_and(|n| n != 0) { return Err(Error::unsupported()); }
                let store = self.task_store().await.map_err(|_| Error::unavailable())?;
                let level = if method == "CancelTask" { AccessLevel::Operator } else { AccessLevel::Viewer };
                let (task, _) = self.authorize_private_task_read(&store, ctx, &params.id, level)
                    .await.map_err(|_| Error::missing())?;
                if task.a2a_owner.as_deref() != Some(principal(ctx).as_str()) || task.created_via.as_deref() != Some("a2a")
                    || !config.exposes(&task.assigned_to) {
                    return Err(Error::missing());
                }
                if method == "GetTask" { return self.a2a_task_view(&store, &task, true).await; }
                if matches!(task.status.as_str(), "done" | "failed" | "cancelled") {
                    return Err(Error::new(-32002, "Task not cancelable", "TASK_NOT_CANCELABLE"));
                }
                let queue = self.message_queue.read().await.clone().ok_or_else(Error::unavailable)?;
                let broker = crate::approval::ApprovalBroker::open(&self.home_dir).map_err(|_| Error::unavailable())?;
                let stop = crate::responsibility::stop::stop_task(&store, &queue, Some(&broker), None,
                    &self.home_dir, &task.id, task.authority_revision, &owner(ctx),
                    !ctx.has_role(UserRole::Manager), chrono::Utc::now()).await
                    .map_err(|_| Error::new(-32002, "Task not cancelable", "TASK_NOT_CANCELABLE"))?;
                let updated = store.get_task(&task.id).await.map_err(|_| Error::unavailable())?
                    .ok_or_else(Error::missing)?;
                let mut value = a2a::task(&updated, true);
                apply_stop(&mut value, &stop.state);
                Ok(value)
            }
            "ListTasks" => self.a2a_list(params, ctx, &config).await,
            "SendStreamingMessage" | "SubscribeToTask" => Err(Error::unsupported()),
            "CreateTaskPushNotificationConfig" | "GetTaskPushNotificationConfig"
            | "ListTaskPushNotificationConfigs" | "DeleteTaskPushNotificationConfig" =>
                Err(Error::new(-32003, "Push notifications not supported", "PUSH_NOTIFICATION_NOT_SUPPORTED")),
            _ => Err(Error::new(-32601, "Method not found", "METHOD_NOT_FOUND")),
        }
    }

    async fn a2a_send(&self, params: Value, ctx: &UserContext, config: &Config) -> Result<Value, Error> {
        let send = a2a::parse_send(params)?;
        if !config.exposes(&send.agent) || !ctx.has_agent_access(&send.agent, AccessLevel::Operator)
            || !self.registry.read().await.get(&send.agent).is_some_and(|agent|
                agent.skills.iter().any(|skill| published_skill(&send.agent, skill).is_some())) {
            return Err(Error::denied());
        }
        self.deny_main_for_non_admin(ctx, &send.agent).await.map_err(|_| Error::denied())?;
        let scan = duduclaw_security::input_guard::scan_input_with_audit(&send.text,
            duduclaw_security::input_guard::DEFAULT_BLOCK_THRESHOLD, &self.home_dir, &send.agent);
        if scan.blocked { return Err(Error::denied()); }
        let store = self.task_store().await.map_err(|_| Error::unavailable())?;
        if let Some(existing) = store.a2a_existing_request(&principal(ctx), &send.message_id, &send.payload_sha256)
            .await.map_err(request_error)? {
            if !config.exposes(&existing.assigned_to) { return Err(Error::missing()); }
            let (task, _) = self.authorize_private_task_read(&store, ctx, &existing.id, AccessLevel::Viewer)
                .await.map_err(|_| Error::missing())?;
            return Ok(json!({"task": self.a2a_task_view(&store, &task, true).await?}));
        }
        let home = self.home_dir.clone();
        let agent = send.agent.clone();
        let admission = Box::pin(async move {
            duduclaw_core::autonomy_gate::admit(&home, &agent,
                duduclaw_core::autonomy_gate::AutonomyPath::Background).await
                .map(Some).map_err(|_| "autonomous work is paused".to_string())
        });
        let result = crate::goal_create_core::create_goal_task_with_admission(&self.home_dir, &store,
            crate::goal_create_core::GoalCreateRequest {
                agent_id: send.agent, created_by: owner(ctx), description: send.text,
                a2a_owner: Some(principal(ctx)),
                a2a_request: Some((send.message_id, send.payload_sha256)),
                on_behalf_of: Some(format!("user:{}", ctx.user_id)), created_via: Some("a2a".into()),
                source_label: "A2A".into(), ..Default::default()
            }, admission).await.map_err(request_error)?;
        // A concurrent retry may have returned a task whose visibility changed.
        if !config.exposes(&result.task.assigned_to) { return Err(Error::missing()); }
        let (task, _) = self.authorize_private_task_read(&store, ctx, &result.task.id, AccessLevel::Viewer)
            .await.map_err(|_| Error::missing())?;
        Ok(json!({"task": self.a2a_task_view(&store, &task, true).await?}))
    }

    async fn a2a_task_view(&self, store: &TaskStore, task: &crate::task_store::TaskRow, artifacts: bool) -> Result<Value, Error> {
        let mut value = a2a::task(task, artifacts);
        if task.status == "cancelled" {
            let stop = crate::responsibility::stop::stop_status_stored(store, &task.id).await
                .map_err(|_| Error::unavailable())?;
            if let Some(stop) = stop { apply_stop(&mut value, &stop.state); }
        }
        Ok(value)
    }

    async fn a2a_list(&self, params: Value, ctx: &UserContext, config: &Config) -> Result<Value, Error> {
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
        let params: ListParams = serde_json::from_value(params).map_err(|_| Error::invalid())?;
        let size = params.page_size.unwrap_or(50);
        if !(1..=100).contains(&size) { return Err(Error::invalid()); }
        if params.history_length.is_some_and(|n| n != 0) { return Err(Error::unsupported()); }
        let after_time = params.status_timestamp_after.as_deref().map(chrono::DateTime::parse_from_rfc3339)
            .transpose().map_err(|_| Error::invalid())?;
        use sha2::{Digest, Sha256};
        let query = format!("{:x}", Sha256::digest(crate::a2a_signing::canonical_json(&json!({
            "owner": principal(ctx), "context": params.context_id, "status": params.status,
            "after": params.status_timestamp_after, "artifacts": params.include_artifacts.unwrap_or(false)
        })).as_bytes()));
        let cursor: Option<PageCursor> = match params.page_token.as_deref().filter(|s| !s.is_empty()) {
            Some(token) if token.len() <= 512 => Some(serde_json::from_slice(&URL_SAFE_NO_PAD.decode(token)
                .map_err(|_| Error::invalid())?).map_err(|_| Error::invalid())?),
            Some(_) => return Err(Error::invalid()),
            None => None,
        };
        if params.status.as_deref().is_some_and(|status| ![
            "TASK_STATE_SUBMITTED", "TASK_STATE_WORKING", "TASK_STATE_COMPLETED", "TASK_STATE_FAILED",
            "TASK_STATE_CANCELED", "TASK_STATE_INPUT_REQUIRED", "TASK_STATE_UNSPECIFIED",
            "TASK_STATE_REJECTED", "TASK_STATE_AUTH_REQUIRED"].contains(&status)) { return Err(Error::invalid()); }
        let store = self.task_store().await.map_err(|_| Error::unavailable())?;
        let mut tasks = Vec::new();
        let current = store.a2a_snapshot(&principal(ctx)).await.map_err(|_| Error::unavailable())?;
        let (after, snapshot) = match cursor {
            Some(cursor) if cursor.after >= 0 && cursor.after <= cursor.snapshot
                && cursor.snapshot <= current && cursor.query == query => (cursor.after, cursor.snapshot),
            Some(_) => return Err(Error::invalid()),
            None => (0, current),
        };
        let mut scan_after = 0;
        let mut last_returned = 0;
        let mut scanned = 0;
        let mut total = 0;
        let mut more = false;
        // Count only currently readable matches. Refuse an oversized inventory
        // rather than inventing totalSize or disclosing hidden task counts.
        loop {
            let rows = store.a2a_tasks_after(&principal(ctx), scan_after, snapshot, 1001).await.map_err(|_| Error::unavailable())?;
            let has_more = rows.len() > 1000;
            for (seq, row) in rows.iter().take(1000) {
                scanned += 1;
                if scanned > 10_000 { return Err(Error::new(-32000, "Task inventory exceeds listing limit", "RESOURCE_EXHAUSTED")); }
                scan_after = *seq;
                if row.created_via.as_deref() != Some("a2a") || !config.exposes(&row.assigned_to)
                    || !task_privacy::task_content_visible(&self.home_dir, ctx, &row.id, &row.assigned_to)
                    || params.context_id.as_ref().is_some_and(|id| id != &row.id) { continue; }
                if let Some(after_time) = after_time.as_ref() {
                    let updated = chrono::DateTime::parse_from_rfc3339(&row.updated_at).map_err(|_| Error::unavailable())?;
                    if updated <= *after_time { continue; }
                }
                let value = self.a2a_task_view(&store, row, params.include_artifacts.unwrap_or(false)).await?;
                if params.status.as_ref().is_some_and(|status| value["status"]["state"] != status.as_str()) { continue; }
                total += 1;
                if *seq <= after { continue; }
                if tasks.len() < size { last_returned = *seq; tasks.push(value); } else { more = true; }
            }
            if !has_more { break; }
        }
        let next = if more {
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&PageCursor { after: last_returned, snapshot, query })
                .map_err(|_| Error::unavailable())?)
        } else { String::new() };
        Ok(json!({"tasks": tasks, "pageSize": size, "totalSize": total, "nextPageToken": next}))
    }
}

fn apply_stop(value: &mut Value, stop: &str) {
    value["metadata"] = json!({"stopState": stop});
    match stop {
        "cancel_pending" => value["status"]["state"] = json!("TASK_STATE_WORKING"),
        "stopped_uncertain" => value["status"]["state"] = json!("TASK_STATE_INPUT_REQUIRED"),
        _ => {},
    }
}

#[cfg(test)]
mod tests;
