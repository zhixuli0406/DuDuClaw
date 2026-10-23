//! `knowledge.*` (document knowledge base) RPC routing. Runs before the
//! rest of the dispatch chain; every permission decision lives in
//! `crate::knowledge::rpc` and, for the methods that need the live
//! registry, in `crate::knowledge::{principals, agent_binding, denied}`.

use super::*;

use crate::knowledge::principals::{AgentRef, Directory};

/// Methods answered here rather than in `knowledge::rpc` (they need the
/// agent registry or the edition).
const REGISTRY_METHODS: &[&str] = &[
    "knowledge.principals.search",
    "knowledge.agent.collections",
    "knowledge.access_log.denied",
    "knowledge.limits",
];

impl MethodHandler {
    /// `Some(frame)` for a `knowledge.*` method, `None` otherwise.
    pub(crate) async fn dispatch_kb(
        &self,
        method: &str,
        params: &Value,
        ctx: &UserContext,
    ) -> Option<WsFrame> {
        if !crate::knowledge::rpc::is_knowledge_method(method) {
            return None;
        }
        let users = self.user_db.read().await.clone();
        if super::dispatch_kb_agent::AGENT_TAB_METHODS.contains(&method) {
            if !crate::knowledge::enabled(&self.home_dir) {
                return Some(WsFrame::error_response("", "文件知識庫已由管理員關閉。"));
            }
            let dir = self.knowledge_directory(users).await;
            return Some(match self.kb_agent_tab(method, params, ctx, dir).await {
                Ok(v) => WsFrame::ok_response("", v),
                Err(e) => kb_error_frame(method, e),
            });
        }
        if REGISTRY_METHODS.contains(&method) || method == "knowledge.acl.set" {
            if !crate::knowledge::enabled(&self.home_dir) {
                return Some(WsFrame::error_response("", "文件知識庫已由管理員關閉。"));
            }
            let dir = self.knowledge_directory(users.clone()).await;
            if let Some(frame) = self.dispatch_kb_registry(method, params, ctx, dir).await {
                return Some(frame);
            }
        }
        let result = crate::knowledge::rpc::handle(&self.home_dir, method, params, ctx, users).await?;
        Some(match result {
            Ok(v) => WsFrame::ok_response("", v),
            Err(e) => WsFrame::error_response("", &e),
        })
    }

    /// The grant rules' inputs: edition, accounts, employees, departments.
    async fn knowledge_directory(&self, users: Option<Arc<duduclaw_auth::db::UserDb>>) -> Directory {
        let personal = self.resolve_edition_profile().await.is_personal();
        let agents: Vec<AgentRef> = {
            let reg = self.registry.read().await;
            reg.list().iter().map(|a| agent_ref(a)).collect()
        };
        let pairs = self.agent_department_pairs().await;
        let departments = crate::departments::list_departments(&self.home_dir, &pairs)
            .into_iter()
            .map(|d| d.name)
            .collect();
        Directory {
            personal,
            users,
            agents,
            departments,
        }
    }

    /// `Some(frame)` when the method is fully answered here; `None` lets
    /// `knowledge.acl.set` continue to `knowledge::rpc` after its check.
    async fn dispatch_kb_registry(
        &self,
        method: &str,
        params: &Value,
        ctx: &UserContext,
        dir: Directory,
    ) -> Option<WsFrame> {
        let user = crate::knowledge::dashboard_user(ctx, dir.users.as_ref());
        let home = self.home_dir.clone();
        let p = params.clone();
        let c = ctx.clone();
        let result: duduclaw_knowledge::KbResult<Option<Value>> = match method {
            "knowledge.principals.search" => {
                crate::knowledge::blocking(move || {
                    crate::knowledge::principals::search(&home, &dir, &c, &user, &p).map(Some)
                })
                .await
            }
            "knowledge.acl.set" => {
                crate::knowledge::blocking(move || {
                    crate::knowledge::principals::check_new_grants(&home, &dir, &c, &user, &p).map(|_| None)
                })
                .await
            }
            "knowledge.agent.collections" => self.kb_agent_collections(params, ctx, &dir).await,
            // Signed in is enough: these are the route's public limits.
            "knowledge.limits" => Ok(Some(crate::knowledge::upload_limits::describe(&self.home_dir))),
            "knowledge.access_log.denied" => {
                if !ctx.is_admin() {
                    Err(duduclaw_knowledge::KbError::PermissionDenied)
                } else {
                    let before = params.get("before_id").and_then(Value::as_i64);
                    let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize;
                    crate::knowledge::blocking(move || {
                        crate::knowledge::denied::list(&home, before, limit).map(|rows| Some(json!({ "entries": rows })))
                    })
                    .await
                }
            }
            _ => Ok(None),
        };
        match result {
            Ok(Some(v)) => Some(WsFrame::ok_response("", v)),
            Ok(None) => None,
            Err(e) => Some(kb_error_frame(method, e)),
        }
    }

    /// `knowledge.agent.collections {agent_id}` — owner access to the
    /// employee required (Admin: any).
    async fn kb_agent_collections(
        &self,
        params: &Value,
        ctx: &UserContext,
        dir: &Directory,
    ) -> duduclaw_knowledge::KbResult<Option<Value>> {
        let agent_id = params.get("agent_id").and_then(Value::as_str).unwrap_or("");
        if !ctx.has_agent_access(agent_id, duduclaw_auth::AccessLevel::Owner) {
            return Err(duduclaw_knowledge::KbError::PermissionDenied);
        }
        let agent = dir
            .agents
            .iter()
            .find(|a| a.id == agent_id)
            .cloned()
            .ok_or(duduclaw_knowledge::KbError::PermissionDenied)?;
        let agent_dir = {
            let reg = self.registry.read().await;
            reg.get(agent_id).map(|a| a.dir.clone())
        }
        .ok_or(duduclaw_knowledge::KbError::PermissionDenied)?;
        let home = self.home_dir.clone();
        crate::knowledge::blocking(move || {
            crate::knowledge::agent_binding::describe(&home, &agent, &agent_dir).map(Some)
        })
        .await
    }

    /// The employee as the knowledge binding rules see it.
    pub(crate) async fn knowledge_agent_ref(&self, agent_id: &str) -> Option<AgentRef> {
        let reg = self.registry.read().await;
        reg.get(agent_id).map(agent_ref)
    }
}

/// An error answer; database and file errors are logged, never shown.
fn kb_error_frame(method: &str, e: duduclaw_knowledge::KbError) -> WsFrame {
    if matches!(e, duduclaw_knowledge::KbError::Db(_) | duduclaw_knowledge::KbError::Io(_)) {
        tracing::error!(error = %e, method, "knowledge RPC failed");
    }
    WsFrame::error_response("", &e.public_message())
}

fn agent_ref(a: &duduclaw_agent::registry::LoadedAgent) -> AgentRef {
    AgentRef {
        id: a.config.agent.name.clone(),
        display_name: a.config.agent.display_name.clone(),
        department: Some(a.config.agent.department.trim().to_string())
            .filter(|d| duduclaw_core::department::is_valid_department(d)),
    }
}
