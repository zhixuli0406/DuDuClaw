//! The employee page's 「資料來源」 tab RPCs and the employee lifecycle
//! hooks for its own collection. The rules live in
//! `crate::knowledge::agent_own`; this file gathers the live inputs
//! (fresh account binding, registry, edition) and writes `agent.toml`.

use super::*;

use crate::knowledge::agent_own::{self as own, DescribeInput, EnsureInput, Names};
use crate::knowledge::principals::Directory;
use duduclaw_knowledge::{KbError, KbResult};

/// The tab's methods.
pub(super) const AGENT_TAB_METHODS: &[&str] =
    &["knowledge.agent.data_sources", "knowledge.agent.own.ensure"];

/// Statuses whose own collection is archived.
fn is_offboarded(status: &duduclaw_core::types::AgentStatus) -> bool {
    use duduclaw_core::types::AgentStatus;
    matches!(status, AgentStatus::Archived | AgentStatus::Deleted)
}

impl MethodHandler {
    /// `knowledge.agent.data_sources` / `knowledge.agent.own.ensure`.
    pub(super) async fn kb_agent_tab(
        &self,
        method: &str,
        params: &Value,
        ctx: &UserContext,
        dir: Directory,
    ) -> KbResult<Value> {
        let agent_id = params
            .get("agent_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if !is_valid_agent_id(&agent_id) {
            return Err(KbError::PermissionDenied);
        }
        let min = if method == "knowledge.agent.own.ensure" {
            AccessLevel::Owner
        } else {
            AccessLevel::Viewer
        };
        // The binding is re-read from users.db, never taken from the session.
        let fresh = crate::agent_files::flow::require_live_agent_access(
            &self.home_dir,
            ctx,
            &agent_id,
            min,
        )
        .map_err(|_| KbError::PermissionDenied)?;
        let (agent, agent_dir, offboarded) = {
            let reg = self.registry.read().await;
            let a = reg.get(&agent_id).ok_or(KbError::PermissionDenied)?;
            (
                dir.agents.iter().find(|r| r.id == agent_id).cloned(),
                a.dir.clone(),
                is_offboarded(&a.config.agent.status),
            )
        };
        let agent = agent.ok_or(KbError::PermissionDenied)?;
        let caller = crate::knowledge::dashboard_user(&fresh, dir.users.as_ref());
        let readers = own::default_readers(dir.users.as_ref(), &agent_id, dir.personal);
        if method == "knowledge.agent.data_sources" {
            let owns = fresh.has_agent_access(&agent_id, AccessLevel::Owner);
            let home = self.home_dir.clone();
            return boxed_frame(|| crate::knowledge::blocking(move || {
                let input = DescribeInput {
                    agent: &agent,
                    agent_dir: &agent_dir,
                    inactive: offboarded,
                    caller: &caller,
                    caller_owns_employee: owns,
                    names: Names {
                        users: dir.users.as_ref(),
                        agents: &dir.agents,
                    },
                    default_readers: &readers,
                };
                own::describe(&home, &input)
            }))
            .await;
        }
        if offboarded {
            return Err(KbError::Invalid(
                "這位 AI 員工已封存，資料來源暫停使用；請先恢復這位員工。".to_string(),
            ));
        }
        boxed_frame(|| self.kb_own_ensure(params, &fresh, caller, (agent, agent_dir), readers))
            .await
    }

    async fn kb_own_ensure(
        &self,
        params: &Value,
        fresh: &UserContext,
        caller: duduclaw_knowledge::requester::ResolvedUser,
        (agent, agent_dir): (crate::knowledge::principals::AgentRef, std::path::PathBuf),
        readers: Vec<duduclaw_knowledge::agent_own::Grant>,
    ) -> KbResult<Value> {
        // A settings file whose [knowledge] cannot be read is not changed,
        // and nothing is created for it.
        let scope = duduclaw_core::knowledge_agent::knowledge_scope(&agent_dir);
        if matches!(
            scope,
            duduclaw_core::knowledge_agent::KnowledgeScope::Unreadable
        ) {
            return Err(KbError::Invalid(
                "這位 AI 員工設定檔的 [knowledge] 段落無法解讀，請先修正後再新增檔案。".to_string(),
            ));
        }
        let sensitivity = params
            .get("sensitivity")
            .and_then(Value::as_str)
            .map(str::to_string);
        // Left out = not shared (the narrower choice); the dashboard sends it.
        let share = params
            .get("share_chat_users")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let home = self.home_dir.clone();
        let (agent_c, caller_c) = (agent.clone(), caller.clone());
        let outcome = boxed_frame(|| crate::knowledge::blocking(move || {
            own::ensure_collection(
                &home,
                &EnsureInput {
                    agent: &agent_c,
                    caller: &caller_c,
                    sensitivity: sensitivity.as_deref(),
                    share_chat_users: share,
                    readers,
                },
            )
        }))
        .await?;
        if outcome.created {
            own::audit_created(
                &self.home_dir,
                &caller.user_id,
                &agent.id,
                &outcome.collection,
                &outcome.grants,
            );
        }
        let slug = outcome.collection.collection.slug.clone();
        // Only a collection created just now joins the employee's list; an
        // existing one an owner took out of the list stays out.
        let needs_write = outcome.created
            && match &scope {
                duduclaw_core::knowledge_agent::KnowledgeScope::Only(list) => !list.contains(&slug),
                _ => false,
            };
        if !needs_write {
            return Ok(json!({
                "collection": outcome.collection,
                "created": outcome.created,
                "binding_changed": false,
            }));
        }
        let changed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = changed.clone();
        boxed_frame(|| self.update_agent_toml_as(&agent.id, "knowledge.agent.own.ensure", Some(fresh), Some(fresh), move |table| {
            let did = own::bind_slug(table, &slug)?;
            flag.store(did, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }))
        .await
        .map_err(|e| {
            tracing::warn!(agent = %agent.id, error = %e, "knowledge: own collection not bound");
            KbError::Invalid(format!(
                "已建立資料集合，但沒能加到這位 AI 員工的設定：{}。請到員工設定的「工具與權限 → 文件知識庫」勾選它。",
                duduclaw_core::truncate_chars(&e, 200)
            ))
        })?;
        let binding_changed = changed.load(std::sync::atomic::Ordering::SeqCst);
        if binding_changed {
            crate::knowledge::audit_admin(
                &self.home_dir,
                "knowledge_agent_binding_changed",
                &caller.user_id,
                json!({"agent_id": agent.id, "added": outcome.collection.collection.slug, "reason": "own_collection"}),
            );
        }
        Ok(json!({
            "collection": outcome.collection,
            "created": outcome.created,
            "binding_changed": binding_changed,
        }))
    }

    /// Archive the employee's own collection (archive, soft delete,
    /// hand-off). Never fails the off-boarding itself; errors are audited.
    pub(crate) async fn kb_own_offboard(&self, agent_id: &str) {
        let home = self.home_dir.clone();
        let id = agent_id.to_string();
        if let Err(e) =
            tokio::task::spawn_blocking(move || own::offboard(&home, &id, "dashboard")).await
        {
            tracing::error!(agent = %agent_id, error = %e, "knowledge: own collection archive task failed");
        }
    }

    /// Restore the employee's own collection with the employee.
    pub(crate) async fn kb_own_restore(&self, agent_id: &str) {
        let home = self.home_dir.clone();
        let id = agent_id.to_string();
        if let Err(e) =
            tokio::task::spawn_blocking(move || own::restore(&home, &id, "dashboard")).await
        {
            tracing::error!(agent = %agent_id, error = %e, "knowledge: own collection restore task failed");
        }
    }

    /// Gateway start: line own collections up with their employees.
    pub(crate) async fn kb_own_reconcile(&self) {
        let (inactive, known): (Vec<String>, Vec<String>) = {
            let reg = self.registry.read().await;
            let list = reg.list();
            (
                list.iter()
                    .filter(|a| is_offboarded(&a.config.agent.status))
                    .map(|a| a.config.agent.name.clone())
                    .collect(),
                list.iter().map(|a| a.config.agent.name.clone()).collect(),
            )
        };
        let home = self.home_dir.clone();
        if let Err(e) =
            tokio::task::spawn_blocking(move || own::reconcile(&home, &inactive, &known)).await
        {
            tracing::error!(error = %e, "knowledge: own collection reconcile task failed");
        }
    }
}
