//! P9 — dashboard RPCs for the employee digest and feedback on finished work.
//!
//! * `digest.latest` — the newest saved digest, cut to the employees the
//!   viewer may see (Viewer binding; Admin sees all) and, for non-admins, to
//!   the items whose task audience lets them read it, plus the latest
//!   verdict already given per item and the `[digest]` switch.
//! * `digest.feedback {task_id, verdict: up|down|changes, note?}` — Operator
//!   on the employee and the task's audience (the shared task-content gate);
//!   only a finished (`done`) task takes feedback.
//! * `digest.feedback {artifact: {agent_id, archived_name}, verdict, note?}`
//!   — a file the employee handed over (`artifacts.jsonl`, outbound only):
//!   one tied to a task needs what that task needs (minus the `done` rule),
//!   one without a task needs Operator on the employee. An unknown file and
//!   a refusal read the same.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    pub(crate) async fn handle_digest_rpc(
        &self,
        method: &str,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        let live = match super::task_privacy::live_reader_context(&self.home_dir, ctx) {
            Ok(l) => l,
            Err(()) => return WsFrame::error_response("", super::task_privacy::PERMISSION_DENIED),
        };
        match method {
            "digest.latest" => {
                let cfg = crate::digest::DigestConfig::from_home(&self.home_dir);
                let home = self.home_dir.clone();
                let (latest, verdicts) = tokio::task::spawn_blocking(move || {
                    (
                        crate::digest::latest(&home),
                        crate::digest::feedback_by_item(&home),
                    )
                })
                .await
                .unwrap_or((None, Default::default()));
                let store = self.task_store().await.ok();
                let digest = match latest {
                    None => None,
                    Some(mut d) => {
                        let mut agents = Vec::new();
                        for mut a in d.agents.into_iter() {
                            if acl::require_agent_access(&live, &a.agent_id, AccessLevel::Viewer)
                                .is_err()
                            {
                                continue;
                            }
                            if !live.is_admin() {
                                let mut kept = Vec::new();
                                for item in a.finished.into_iter() {
                                    let readable = match &store {
                                        Some(s) => self
                                            .authorize_private_task_read(
                                                s,
                                                &live,
                                                &item.id,
                                                AccessLevel::Viewer,
                                            )
                                            .await
                                            .is_ok(),
                                        None => false,
                                    };
                                    if readable {
                                        kept.push(item);
                                    }
                                }
                                a.finished = kept;
                                let mut kept_files = Vec::new();
                                for art in a.artifacts.into_iter() {
                                    let readable = match (&art.task_id, &store) {
                                        (None, _) => true,
                                        (Some(task_id), Some(s)) => self
                                            .authorize_private_task_read(
                                                s,
                                                &live,
                                                task_id,
                                                AccessLevel::Viewer,
                                            )
                                            .await
                                            .is_ok(),
                                        (Some(_), None) => false,
                                    };
                                    if readable {
                                        kept_files.push(art);
                                    }
                                }
                                a.artifacts = kept_files;
                            }
                            agents.push(a);
                        }
                        d.agents = agents;
                        Some(d)
                    }
                };
                let feedback: serde_json::Map<String, Value> = digest
                    .iter()
                    .flat_map(|d| d.agents.iter())
                    .flat_map(|a| {
                        a.finished.iter().map(|i| i.id.clone()).chain(a.artifacts.iter().map(
                            |x| crate::digest::artifact_item_id(&x.agent_id, &x.archived_name),
                        ))
                    })
                    .filter_map(|id| verdicts.get(&id).map(|v| (id.clone(), json!(v))))
                    .collect();
                WsFrame::ok_response(
                    "",
                    json!({
                        "enabled": cfg.enabled,
                        "digest": digest,
                        "feedback": feedback,
                    }),
                )
            }
            "digest.feedback" if params.get("artifact").is_some() => {
                // A delivered file: `{artifact: {agent_id, archived_name}}`.
                let art = &params["artifact"];
                let (Some(agent_id), Some(archived)) = (
                    art.get("agent_id").and_then(|v| v.as_str()).filter(|s| !s.is_empty()),
                    art.get("archived_name").and_then(|v| v.as_str()).filter(|s| !s.is_empty()),
                ) else {
                    return WsFrame::error_response("", "artifact.agent_id and artifact.archived_name are required");
                };
                let Some(verdict) = params
                    .get("verdict")
                    .and_then(|v| v.as_str())
                    .and_then(crate::digest::Verdict::parse)
                else {
                    return WsFrame::error_response("", "verdict must be up, down or changes");
                };
                let denied = || WsFrame::error_response("", super::task_privacy::PERMISSION_DENIED);
                let home = self.home_dir.clone();
                let (a, n) = (agent_id.to_string(), archived.to_string());
                let row = tokio::task::spawn_blocking(move || {
                    crate::artifacts::find_delivered(&home, &a, &n)
                })
                .await
                .ok()
                .flatten();
                // Unknown file and no access read the same.
                let Some(row) = row else { return denied() };
                match &row.task_id {
                    Some(task_id) => {
                        let store = match self.task_store().await {
                            Ok(s) => s,
                            Err(f) => return f,
                        };
                        match self
                            .authorize_private_task_read(&store, &live, task_id, AccessLevel::Operator)
                            .await
                        {
                            Ok((t, _)) if t.assigned_to == row.agent_id => {}
                            _ => return denied(),
                        }
                    }
                    None => {
                        if !live.has_agent_access(&row.agent_id, AccessLevel::Operator) {
                            return denied();
                        }
                    }
                }
                let user = if live.user_id.is_empty() {
                    "system".to_string()
                } else {
                    live.user_id.clone()
                };
                let item_id = crate::digest::artifact_item_id(&row.agent_id, &row.archived_name);
                let title = if row.display_name.is_empty() {
                    row.archived_name.clone()
                } else {
                    row.display_name.clone()
                };
                let note = params.get("note").and_then(|v| v.as_str()).map(str::to_string);
                let home = self.home_dir.clone();
                let (agent, id) = (row.agent_id.clone(), item_id.clone());
                let written = tokio::task::spawn_blocking(move || {
                    crate::digest::record_feedback(
                        &home,
                        &agent,
                        "artifact",
                        &id,
                        &title,
                        verdict,
                        note.as_deref(),
                        &user,
                        Utc::now(),
                    )
                })
                .await;
                match written {
                    Ok(Ok(_)) => WsFrame::ok_response(
                        "",
                        json!({ "item_id": item_id, "verdict": verdict.as_str() }),
                    ),
                    Ok(Err(e)) => WsFrame::error_response("", &format!("feedback not stored: {e}")),
                    Err(e) => WsFrame::error_response("", &format!("feedback not stored: {e}")),
                }
            }
            "digest.feedback" => {
                let Some(task_id) = params
                    .get("task_id")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.trim().is_empty())
                else {
                    return WsFrame::error_response("", "task_id is required");
                };
                let Some(verdict) = params
                    .get("verdict")
                    .and_then(|v| v.as_str())
                    .and_then(crate::digest::Verdict::parse)
                else {
                    return WsFrame::error_response("", "verdict must be up, down or changes");
                };
                let note = params.get("note").and_then(|v| v.as_str());
                let store = match self.task_store().await {
                    Ok(s) => s,
                    Err(f) => return f,
                };
                let task = match self
                    .authorize_private_task_read(&store, &live, task_id, AccessLevel::Operator)
                    .await
                {
                    Ok((t, _)) => t,
                    Err(f) => return f,
                };
                if task.status != "done" {
                    return WsFrame::error_response("", "only finished work takes feedback");
                }
                let kind = if task.goal_mode { "goal" } else { "task" };
                let user = if live.user_id.is_empty() {
                    "system".to_string()
                } else {
                    live.user_id.clone()
                };
                let home = self.home_dir.clone();
                let agent = task.assigned_to.clone();
                let id = task.id.clone();
                let title = task.title.clone();
                let note = note.map(str::to_string);
                let written = tokio::task::spawn_blocking(move || {
                    crate::digest::record_feedback(
                        &home,
                        &agent,
                        kind,
                        &id,
                        &title,
                        verdict,
                        note.as_deref(),
                        &user,
                        Utc::now(),
                    )
                })
                .await;
                match written {
                    Ok(Ok(_)) => WsFrame::ok_response(
                        "",
                        json!({ "task_id": task.id, "verdict": verdict.as_str() }),
                    ),
                    Ok(Err(e)) => WsFrame::error_response("", &format!("feedback not stored: {e}")),
                    Err(e) => WsFrame::error_response("", &format!("feedback not stored: {e}")),
                }
            }
            _ => WsFrame::error_response("", &format!("unknown method: {method}")),
        }
    }
}

#[cfg(test)]
mod digest_rpc_tests {
    use super::*;
    use duduclaw_auth::{UserDb, UserRole};

    fn is_ok(frame: &WsFrame) -> bool {
        matches!(frame, WsFrame::Response { ok: true, .. })
    }

    #[tokio::test]
    async fn feedback_needs_operator_and_a_finished_task() {
        let dir = tempfile::tempdir().unwrap();
        let agent = dir.path().join("agents").join("alice");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(agent.join("agent.toml"), "[agent]\nname = \"alice\"\n").unwrap();
        let handler = MethodHandler::new(dir.path().to_path_buf()).await;
        let store = Arc::new(TaskStore::open(dir.path()).unwrap());
        handler.set_task_store(Arc::clone(&store)).await;
        let mut t = TaskRow::new(
            "t-done".into(),
            "週報".into(),
            "".into(),
            "medium".into(),
            "alice".into(),
            "op".into(),
        );
        t.status = "done".into();
        store.insert_task(&t).await.unwrap();
        let mut open = t.clone();
        open.id = "t-open".into();
        open.status = "in_progress".into();
        store.insert_task(&open).await.unwrap();

        let db = UserDb::new(&dir.path().join("users.db")).unwrap();
        let mk = |level: AccessLevel| {
            let email = format!("{}@test.invalid", uuid::Uuid::new_v4());
            let u = db
                .create_user(&email, &email, "isolated-test-password", UserRole::Employee)
                .unwrap();
            db.bind_agent(&u.id, "alice", level).unwrap();
            let mut access = std::collections::HashMap::new();
            access.insert("alice".to_string(), level);
            UserContext {
                user_id: u.id,
                email,
                role: UserRole::Employee,
                agent_access: access,
                must_change_password: false,
            }
        };
        let viewer = mk(AccessLevel::Viewer);
        let operator = mk(AccessLevel::Operator);
        let up = json!({"task_id": "t-done", "verdict": "up"});
        assert!(!is_ok(
            &handler
                .handle_digest_rpc("digest.feedback", up.clone(), &viewer)
                .await
        ));
        assert!(is_ok(
            &handler
                .handle_digest_rpc("digest.feedback", up, &operator)
                .await
        ));
        let open_fb = json!({"task_id": "t-open", "verdict": "down"});
        assert!(!is_ok(
            &handler
                .handle_digest_rpc("digest.feedback", open_fb, &operator)
                .await
        ));
        let bad = json!({"task_id": "t-done", "verdict": "meh"});
        assert!(!is_ok(
            &handler
                .handle_digest_rpc("digest.feedback", bad, &operator)
                .await
        ));
        let rows = std::fs::read_to_string(dir.path().join("feedback.jsonl")).unwrap();
        assert_eq!(rows.lines().count(), 1);
        assert!(rows.contains("\"source\":\"deliverable\""));
    }
}
