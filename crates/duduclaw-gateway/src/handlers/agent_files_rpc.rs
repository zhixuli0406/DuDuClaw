//! `agent_files.*`, the new `contract.update`, and the `agent_file_change`
//! approval decision (see `crate::agent_files`).
//!
//! Every method re-reads the caller from `users.db` (`fresh_dashboard_context`)
//! before acting: Viewer reads the files, Operator reads versions and diffs,
//! the employee's Owner (or an admin) saves, admins read and decide
//! approval requests.

#[allow(unused_imports)]
use super::*;

use crate::agent_files::flow::{self, Caller, SaveInput, require_live_agent_access};
use crate::agent_files::{FileKey, approval as fa, history};

fn str_param<'a>(params: &'a Value, key: &str) -> Result<&'a str, String> {
    params.get(key).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).ok_or_else(|| format!("`{key}` is required"))
}

fn file_param(params: &Value) -> Result<FileKey, String> {
    FileKey::parse(str_param(params, "file")?)
}

fn frame(r: Result<Value, String>) -> WsFrame {
    match r {
        Ok(v) => WsFrame::ok_response("", v),
        Err(e) => WsFrame::error_response("", &e),
    }
}

/// Text shown for a file: `agent.toml` is masked (secrets never leave).
fn display_text(key: &FileKey, text: &str) -> Result<(String, Vec<String>), String> {
    if *key != FileKey::AgentToml || text.is_empty() {
        return Ok((text.to_string(), Vec::new()));
    }
    let (masked, keys, _) = super::config_raw_rpc::mask_raw_toml(text)
        .map_err(|e| format!("agent.toml does not parse, cannot display it safely: {e}"))?;
    Ok((masked, keys))
}

fn drift_of(home: &Path, agent_id: &str, key: &FileKey) -> (&'static str, Option<String>) {
    let Some(doc) = key.protected_doc() else { return ("none", None) };
    let Ok(dir) = crate::agent_files::agent_dir(home, agent_id) else { return ("none", None) };
    let state = duduclaw_security::soul_guard::doc_drift(doc, &dir);
    let accepted = duduclaw_security::soul_guard::read_doc_hash(doc, &dir)
        .and_then(|h| history::find_by_sha(home, agent_id, key, &h));
    (state.as_str(), accepted)
}

impl MethodHandler {
    fn live(&self, ctx: &UserContext, agent_id: &str, min: AccessLevel) -> Result<UserContext, String> {
        require_live_agent_access(&self.home_dir, ctx, agent_id, min)
    }

    /// `agents.export_profile {agent_id, format, include_user?}`.
    pub(crate) async fn handle_agents_export_profile(&self, params: Value, ctx: &UserContext) -> WsFrame {
        frame((|| {
            let agent_id = str_param(&params, "agent_id")?;
            let live = self.live(ctx, agent_id, AccessLevel::Viewer)?;
            acl::require_role(&live, UserRole::Manager)?;
            let format_name = str_param(&params, "format")?;
            let format = format_name.parse::<duduclaw_core::agent_profile::ExportFormat>()
                .map_err(|e| e.to_string())?;
            let include_user = match params.get("include_user") {
                None => false,
                Some(v) => v.as_bool().ok_or("`include_user` must be a boolean")?,
            };
            let dir = crate::agent_files::agent_dir(&self.home_dir, agent_id)?;
            let profile = duduclaw_core::agent_profile::load(&dir, agent_id).map_err(|e| e.to_string())?;
            let files = duduclaw_security::agent_profile::export(&profile, format, include_user).map_err(|e| e.to_string())?;
            crate::security_autopilot::audit_and_emit(
                &self.home_dir,
                &duduclaw_security::audit::AuditEvent::new(
                    "agent_profile_exported", agent_id, duduclaw_security::audit::Severity::Info,
                    json!({ "user_id": live.user_id, "format": format_name, "include_user": include_user, "source": "dashboard" }),
                ),
            );
            Ok(json!({ "agent_id": agent_id, "format": format_name, "include_user": include_user, "files": files }))
        })())
    }

    /// `agent_files.list {agent_id}`.
    pub(crate) async fn handle_agent_files_list(&self, params: Value, ctx: &UserContext) -> WsFrame {
        frame(self.agent_files_list(&params, ctx).await)
    }

    async fn agent_files_list(&self, params: &Value, ctx: &UserContext) -> Result<Value, String> {
        let agent_id = str_param(params, "agent_id")?;
        let live = self.live(ctx, agent_id, AccessLevel::Viewer)?;
        let dir = crate::agent_files::agent_dir(&self.home_dir, agent_id)?;
        fa::sweep(&self.home_dir).await;
        let can_write = live.has_agent_access(agent_id, AccessLevel::Owner);
        let skills = crate::agent_files::skills::list(&dir);
        let mut files = Vec::new();
        let mut limits = serde_json::Map::new();
        for key in FileKey::ALL {
            files.push(self.list_entry(agent_id, &key, can_write).await?);
            limits.insert(key.as_str().into(), json!(key.max_bytes()));
        }
        // The skills folder is employee-writable: one odd file (gone, too
        // big, not text) shows as unreadable instead of failing the list.
        for p in skills {
            let key = FileKey::Skill(p.clone());
            let mut entry = match self.list_entry(agent_id, &key, can_write).await {
                Ok(e) => e,
                Err(_) => json!({
                    "file": key.as_str(), "label_key": key.label_key(), "exists": true, "size": 0, "hash": "",
                    "editable_by_me": false, "last_change": null, "drift": "none", "pending_approval_id": null,
                    "unreadable": true,
                }),
            };
            entry["skill_name"] = json!(p.skill_name());
            entry["path"] = json!(format!("{}/{}", crate::agent_files::skills::SKILLS_DIR, p.rel()));
            files.push(entry);
        }
        limits.insert("skill".into(), json!(crate::agent_files::MAX_MARKDOWN_BYTES));
        Ok(json!({ "agent_id": agent_id, "files": files, "limits": limits }))
    }

    /// One `agent_files.list` row. A file larger than twice its limit is not
    /// read (it cannot be saved here anyway).
    async fn list_entry(&self, agent_id: &str, key: &FileKey, can_write: bool) -> Result<Value, String> {
        let path = crate::agent_files::file_path(&self.home_dir, agent_id, key)?;
        if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > (key.max_bytes() as u64) * 2 {
            return Err(format!("{} is too large to show", key.file_name()));
        }
        let text = crate::agent_files::read_or_empty(&path)?;
        let last = history::last(&self.home_dir, agent_id, key);
        Ok(json!({
            "file": key.as_str(),
            "label_key": key.label_key(),
            "exists": path.exists(),
            "size": text.len(),
            "hash": crate::agent_files::hash_of(&text),
            "editable_by_me": can_write && key.text_editable(),
            "last_change": last.map(|v| json!({ "at": v.at, "user_id": v.user_id, "source": v.source })),
            "drift": drift_of(&self.home_dir, agent_id, key).0,
            "pending_approval_id": fa::pending_for(&self.home_dir, agent_id, key).await,
        }))
    }

    /// `agent_files.runtime_files {agent_id}`: the generated `CLAUDE.md` /
    /// `AGENTS.md` of the employee, read-only (Viewer). Same directory and
    /// symlink rules as the profile files; never writes.
    pub(crate) async fn handle_agent_files_runtime_files(&self, params: Value, ctx: &UserContext) -> WsFrame {
        frame(self.agent_files_runtime_files(&params, ctx))
    }

    fn agent_files_runtime_files(&self, params: &Value, ctx: &UserContext) -> Result<Value, String> {
        let agent_id = str_param(params, "agent_id")?;
        self.live(ctx, agent_id, AccessLevel::Viewer)?;
        let dir = crate::agent_files::agent_dir(&self.home_dir, agent_id)?;
        let files = crate::agent_files::runtime_context::read_all(&dir)?;
        Ok(json!({
            "agent_id": agent_id,
            "files": files.iter().map(|f| json!({
                "name": f.name, "content": f.content, "size": f.size, "truncated": f.truncated,
            })).collect::<Vec<_>>(),
        }))
    }

    /// `agent_files.get {agent_id, file, version_id?}`.
    pub(crate) async fn handle_agent_files_get(&self, params: Value, ctx: &UserContext) -> WsFrame {
        frame(self.agent_files_get(&params, ctx))
    }

    fn agent_files_get(&self, params: &Value, ctx: &UserContext) -> Result<Value, String> {
        let agent_id = str_param(params, "agent_id")?;
        let key = file_param(params)?;
        let version = params.get("version_id").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
        self.live(ctx, agent_id, if version.is_some() { AccessLevel::Operator } else { AccessLevel::Viewer })?;
        let path = crate::agent_files::file_path(&self.home_dir, agent_id, &key)?;
        if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > (key.max_bytes() as u64) * 2 {
            return Err(format!("{} is too large to show", key.file_name()));
        }
        let text = match version {
            Some(v) => history::read_version(&self.home_dir, agent_id, &key, v)?,
            None => crate::agent_files::read_or_empty(&path)?,
        };
        let (content, masked) = display_text(&key, &text)?;
        let (state, accepted) = drift_of(&self.home_dir, agent_id, &key);
        Ok(json!({
            "agent_id": agent_id,
            "file": key.as_str(),
            "content": content,
            "hash": crate::agent_files::hash_of(&text),
            "exists": path.exists(),
            "masked": masked,
            "drift": { "state": state, "accepted_version_id": accepted },
            "version_id": version,
        }))
    }

    /// `agent_files.diff {agent_id, file, content? | version_id?, against?}`:
    /// the proposed `content` (or a kept version) against `against` (a
    /// version) or the current file.
    pub(crate) async fn handle_agent_files_diff(&self, params: Value, ctx: &UserContext) -> WsFrame {
        frame(self.agent_files_diff(&params, ctx))
    }

    fn agent_files_diff(&self, params: &Value, ctx: &UserContext) -> Result<Value, String> {
        let agent_id = str_param(params, "agent_id")?;
        let key = file_param(params)?;
        self.live(ctx, agent_id, AccessLevel::Operator)?;
        let current = crate::agent_files::read_or_empty(&crate::agent_files::file_path(&self.home_dir, agent_id, &key)?)?;
        let version = |k: &str| params.get(k).and_then(|v| v.as_str()).filter(|s| !s.is_empty());
        let new = match (params.get("content").and_then(|v| v.as_str()), version("version_id")) {
            (Some(c), _) => c.to_string(),
            (None, Some(v)) => history::read_version(&self.home_dir, agent_id, &key, v)?,
            (None, None) => return Err("`content` or `version_id` is required".into()),
        };
        if new.len() > key.max_bytes() * 2 {
            return Err("content is too large".into());
        }
        let old = match version("against") {
            Some(v) => history::read_version(&self.home_dir, agent_id, &key, v)?,
            None => current,
        };
        let (old, _) = display_text(&key, &old)?;
        let (new, _) = display_text(&key, &new)?;
        Ok(crate::agent_files::diff::diff_json(&old, &new))
    }

    /// `agent_files.history {agent_id, file}`.
    pub(crate) async fn handle_agent_files_history(&self, params: Value, ctx: &UserContext) -> WsFrame {
        frame((|| {
            let agent_id = str_param(&params, "agent_id")?;
            let key = file_param(&params)?;
            self.live(ctx, agent_id, AccessLevel::Operator)?;
            crate::agent_files::file_path(&self.home_dir, agent_id, &key)?;
            let versions = history::list(&self.home_dir, agent_id, &key);
            let versions = with_editor_names(&self.home_dir, &versions);
            Ok(json!({ "agent_id": agent_id, "file": key.as_str(), "versions": versions }))
        })())
    }

    /// `agent_files.save {agent_id, file, content, base_hash, action?, analysis_id?, reason?, acknowledge_risk?}`.
    pub(crate) async fn handle_agent_files_save(&self, params: Value, ctx: &UserContext) -> WsFrame {
        frame(self.agent_files_save(&params, ctx, None).await)
    }

    /// `agent_files.rollback {agent_id, file, version_id, base_hash, action?, …}`:
    /// the kept version goes through the whole check, AI review included.
    pub(crate) async fn handle_agent_files_rollback(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let r = async {
            let version = str_param(&params, "version_id")?;
            let agent_id = str_param(&params, "agent_id")?;
            let key = file_param(&params)?;
            self.live(ctx, agent_id, AccessLevel::Owner)?;
            let content = history::read_version(&self.home_dir, agent_id, &key, version)?;
            self.agent_files_save(&params, ctx, Some(content)).await
        };
        frame(r.await)
    }

    async fn agent_files_save(&self, params: &Value, ctx: &UserContext, version: Option<String>) -> Result<Value, String> {
        let agent_id = str_param(params, "agent_id")?;
        let key = file_param(params)?;
        let live = self.live(ctx, agent_id, AccessLevel::Owner)?;
        let (content, source) = match version {
            Some(v) => (v, "rollback"),
            None => {
                let c = params.get("content").and_then(|v| v.as_str()).ok_or("`content` is required")?;
                if c.len() > key.max_bytes() * 2 {
                    return Err(format!("內容太長：最多 {} KB", key.max_bytes() / 1024));
                }
                (c.to_string(), "dashboard")
            }
        };
        let base_hash = params
            .get("base_hash")
            .and_then(|v| v.as_str())
            .ok_or("`base_hash` (the `hash` returned by agent_files.get) is required")?
            .to_string();
        let input = SaveInput::from_params(params, key, content, base_hash, source)?;
        fa::sweep(&self.home_dir).await;
        let caller = Caller { home: &self.home_dir, registry: &self.registry, live: &live, session: ctx };
        Box::pin(flow::save(&caller, input)).await
    }

    /// `agent_files.withdraw {approval_id}`: the requester or an admin.
    pub(crate) async fn handle_agent_files_withdraw(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let r = async {
            let id = str_param(&params, "approval_id")?;
            let live = crate::review_evidence::audience::fresh_dashboard_context(&self.home_dir, ctx)
                .map_err(|_| "permission denied".to_string())?;
            let broker = crate::approval::ApprovalBroker::open(&self.home_dir)?;
            let approval_id = crate::approval::ApprovalId::from(id.to_string());
            let rec = broker
                .get(&approval_id)
                .await?
                .filter(|r| r.action_kind == fa::ACTION_KIND)
                .ok_or("找不到這筆審核")?;
            let requester = rec.payload.get("requested_by").and_then(|v| v.as_str()) == Some(live.user_id.as_str());
            if !requester && !live.is_admin() {
                return Err("permission denied".into());
            }
            let withdrawn = broker.withdraw(&approval_id, &format!("dashboard:{}:withdrawn", live.user_id)).await?;
            fa::remove_sidecar(&self.home_dir, id);
            Ok(json!({ "withdrawn": withdrawn }))
        };
        frame(r.await)
    }

    /// `agent_files.pending_get {approval_id}` (admins): the diff, who asked,
    /// why, and the check result.
    pub(crate) async fn handle_agent_files_pending_get(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let r = async {
            let id = str_param(&params, "approval_id")?;
            let live = crate::review_evidence::audience::fresh_dashboard_context(&self.home_dir, ctx)
                .map_err(|_| "permission denied".to_string())?;
            if !live.is_admin() {
                return Err("permission denied".to_string());
            }
            let broker = crate::approval::ApprovalBroker::open(&self.home_dir)?;
            let rec = broker
                .get(&crate::approval::ApprovalId::from(id.to_string()))
                .await?
                .filter(|r| r.action_kind == fa::ACTION_KIND)
                .ok_or("找不到這筆審核")?;
            fa::pending_json(&self.home_dir, &rec)
        };
        frame(r.await)
    }

    /// `contract.update {agent_id, must_not[], must_always[], max_tool_calls_per_turn, action?, analysis_id?, reason?, acknowledge_risk?, base_hash?}`.
    /// The form's contract is rendered over the file as it is (comments
    /// kept) and runs the whole check. The response is the save envelope
    /// plus, once applied, the legacy contract fields.
    pub(crate) async fn handle_contract_update_live(&self, params: Value, ctx: &UserContext) -> WsFrame {
        frame(self.contract_update_live(&params, ctx).await)
    }

    async fn contract_update_live(&self, params: &Value, ctx: &UserContext) -> Result<Value, String> {
        let agent_id = str_param(params, "agent_id")?;
        let live = self.live(ctx, agent_id, AccessLevel::Owner)?;
        let path = crate::agent_files::file_path(&self.home_dir, agent_id, &FileKey::Contract)?;
        let current = crate::agent_files::read_or_empty(&path)?;
        let table = build_contract_table(params)?;
        let content = super::config_commit::render_preserving(&current, &table)?;
        let base_hash = params
            .get("base_hash")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| crate::agent_files::hash_of(&current));
        let input = SaveInput::from_params(params, FileKey::Contract, content, base_hash, "contract.update")?;
        let caller = Caller { home: &self.home_dir, registry: &self.registry, live: &live, session: ctx };
        let mut out = Box::pin(flow::save(&caller, input)).await?;
        if matches!(out["status"].as_str(), Some("applied" | "unchanged"))
            && let (Some(obj), Some(fields)) = (out.as_object_mut(), contract_table_to_response(&table).as_object())
        {
            for (k, v) in fields {
                obj.insert(k.clone(), v.clone());
            }
            obj.insert("success".into(), json!(true));
            obj.insert("message".into(), json!("Contract updated — applies on the agent's next turn"));
        }
        Ok(out)
    }

    /// Compatibility entry point; the service owns the decision lock and effects.
    pub(crate) async fn decide_agent_file_change(
        &self, broker: &crate::approval::ApprovalBroker,
        approval_id: &crate::approval::ApprovalId, approve: bool, ctx: &UserContext,
    ) -> WsFrame {
        let env = crate::approval::decision_service::DecisionEnv {
            home: self.home_dir.clone(), registry: Some(self.registry.clone()),
            broker: broker.clone(), users: self.user_db.read().await.clone(),
        };
        match Box::pin(crate::approval::decision_service::decide(
            &env, approval_id, approve, "", &crate::approval::decision_service::Decider::Dashboard(ctx.clone()),
        )).await {
            Ok(outcome) => WsFrame::ok_response("", outcome.value),
            Err(error) => error.dashboard_frame(),
        }
    }
}

/// Each kept version plus `user_name`: the editor's display name, resolved
/// when read from `users.db` (read-only, the same source the permission check
/// uses; never the email). `null` when the account no longer exists or the
/// database cannot be read; the UI then says "a deleted user". The `system`
/// marker and empty ids carry no name.
fn with_editor_names(home: &std::path::Path, versions: &[history::VersionEntry]) -> Vec<Value> {
    let conn = match crate::review_evidence::audience::readonly_db(&home.join("users.db")) {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::warn!(error = %e, "agent_files.history: editor names unavailable");
            None
        }
    };
    let mut cache: std::collections::HashMap<String, Option<String>> = std::collections::HashMap::new();
    versions
        .iter()
        .map(|v| {
            let mut row = serde_json::to_value(v).unwrap_or(Value::Null);
            let id = v.user_id.as_str();
            if !id.is_empty() && id != "system" {
                let name = cache
                    .entry(id.to_string())
                    .or_insert_with(|| {
                        let conn = conn.as_ref()?;
                        conn.query_row("SELECT display_name FROM users WHERE id = ?1", [id], |r| r.get::<_, String>(0))
                            .ok()
                            .filter(|n| !n.trim().is_empty())
                    })
                    .clone();
                row["user_name"] = json!(name);
            }
            row
        })
        .collect()
}
