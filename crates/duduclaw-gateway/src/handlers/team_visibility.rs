//! Read-only monitoring through the same capped grants as agents.callable.
//! Never use this gate for memory, settings, conversation content or writes.
use super::*;

pub(super) fn may_monitor(ctx: &UserContext, agent: &str) -> bool {
    ctx.has_role(UserRole::Manager)
        && ctx.team_access.get(agent).is_some_and(|grant| {
            duduclaw_auth::org::derived_team_level(grant.level)
                .at_least(AccessLevel::Viewer)
        })
}

impl MethodHandler {
    pub(super) fn monitoring_reader(
        &self,
        ctx: &UserContext,
        agent: Option<&str>,
    ) -> Result<TaskReader<'_>, WsFrame> {
        let reader = TaskReader::new(&self.home_dir, ctx)?;
        let live = reader.live();
        if !live.is_admin()
            && !agent.is_some_and(|a| {
                live.has_agent_access(a, AccessLevel::Viewer) || may_monitor(live, a)
            })
        {
            return Err(WsFrame::error_response("", PERMISSION_DENIED));
        }
        Ok(reader)
    }

    pub(super) fn team_run_metadata_response(&self, ctx: &UserContext, agent: &str, id: &str) -> WsFrame {
        let expected = if id.starts_with("dispatch:") {
            id.to_string()
        } else {
            match crate::history_privacy::handles::issue_run(&self.home_dir, &ctx.user_id, id) {
                Ok(id) => id,
                Err(e) => return WsFrame::error_response("", &e),
            }
        };
        match self.team_runs_metadata(ctx, agent, RUNS_SCAN_MSG_CAP) {
            Ok(runs) => match runs.into_iter().find(|run| run["id"].as_str() == Some(expected.as_str())) {
                Some(run) => WsFrame::ok_response("", json!({
                    "run": run, "events": [], "read_only": true,
                    "omitted": ["conversation_content", "tool_steps"],
                })),
                None => WsFrame::error_response("", "Run not found"),
            },
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    /// Team visibility exposes timing/status only. Existing direct-binding
    /// history projections and task audiences remain authoritative for content.
    pub(super) fn team_runs_metadata(
        &self,
        ctx: &UserContext,
        agent: &str,
        limit: usize,
    ) -> Result<Vec<Value>, String> {
        let live = self.bound_history_live(ctx)?;
        if !may_monitor(&live, agent) {
            return Err(PERMISSION_DENIED.into());
        }
        let mut out = Vec::new();
        let path = self.home_dir.join("sessions.db");
        if path.exists() {
            let conn = rusqlite::Connection::open_with_flags(
                path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            ).map_err(|_| "history unavailable")?;
            let mut rows = query_run_msg_rows(&conn, agent, RUNS_SCAN_MSG_CAP)
                .map_err(|_| "history unavailable")?;
            for row in &mut rows {
                row.preview.clear();
            }
            rows.sort_by(|a, b| (&a.session_id, a.rowid).cmp(&(&b.session_id, b.rowid)));
            for run in fold_session_runs(&rows, Utc::now()) {
                // Closed output shape: no conversation ids, prompts or steps.
                out.push(json!({
                    "id": crate::history_privacy::handles::issue_run(&self.home_dir, &live.user_id, &run.id)?,
                    "agent_id": run.agent_id,
                    "channel": run.channel,
                    "started_at": run.started_at,
                    "ended_at": run.ended_at,
                    "status": run.status,
                    "step_count": 0,
                    "preview": null,
                    "read_only": true,
                }));
            }
        }
        if let Some(store) = crate::run_steps::shared_store(&self.home_dir) {
            let runs = store.list_dispatch_runs(agent, limit)
                .map_err(|_| "history unavailable")?;
            out.extend(runs.into_iter().map(|run| json!({
                "id": format!("dispatch:{}", run.id),
                "agent_id": run.agent_id,
                "channel": run.source,
                "started_at": run.started_at,
                "ended_at": run.ended_at,
                "status": run.status,
                "step_count": 0,
                "preview": null,
                "read_only": true,
            })));
        }
        out.sort_by(|a, b| b["started_at"].as_str().cmp(&a["started_at"].as_str()));
        out.truncate(limit);
        let current = self.bound_history_live(&live)?;
        if !may_monitor(&current, agent) {
            return Err(PERMISSION_DENIED.into());
        }
        Ok(out)
    }
}
