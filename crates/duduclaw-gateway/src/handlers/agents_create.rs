//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    /// Test entry point without a caller identity. Production goes through
    /// [`Self::handle_agents_create_as`] so every audit row names the caller.
    #[cfg(test)]
    pub(crate) async fn handle_agents_create(&self, params: Value) -> WsFrame {
        self.handle_agents_create_as(params, None).await
    }

    /// `agents.create`. `caller` is the authenticated dashboard user; it is
    /// recorded on the `runtime_provider_deprecated` audit row (R1, 2026-10).
    pub(crate) async fn handle_agents_create_as(
        &self,
        params: Value,
        caller: Option<&UserContext>,
    ) -> WsFrame {
        let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let display_name = params
            .get("display_name")
            .and_then(|v| v.as_str())
            .unwrap_or(name);
        let role = params
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("specialist");
        let trigger = params.get("trigger").and_then(|v| v.as_str()).unwrap_or("");
        let trigger = if trigger.is_empty() {
            format!("@{display_name}")
        } else {
            trigger.to_string()
        };
        // Preferred model is now chosen by the user in the create form. The
        // hardcoded `claude-sonnet-4-6` remains ONLY as a last-resort fallback
        // for programmatic callers (MCP / API) that don't pass one — the
        // dashboard always sends the operator's explicit choice.
        let preferred_model = params
            .get("model_preferred")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("claude-sonnet-4-6")
            .to_string();

        if name.is_empty() {
            return WsFrame::error_response("", "Agent name is required");
        }
        if !is_valid_agent_id(name) {
            return WsFrame::error_response(
                "",
                "Agent name must be lowercase alphanumeric with hyphens, max 64 chars",
            );
        }
        // WP21 欠帳④ — the delegation system-sender ids (`cron`, `dashboard`,
        // …) are *not agents* (design doc §2.3). An agent that claimed one
        // would clear every delegation choke point unconditionally. MCP
        // `create_agent` already rejects these (mcp.rs); this dashboard path
        // creates agents too and must apply the same fail-closed check.
        if duduclaw_core::is_reserved_agent_id(name) {
            return WsFrame::error_response(
                "",
                &format!(
                    "「{name}」是系統保留名稱,不能用來建立 AI 員工。\
                     保留名稱包含 dashboard / webhook / cron / heartbeat / autopilot / \
                     goal-loop-driver / a2a-client / default 以及任何以 __ 開頭的名稱,\
                     請換一個名稱。"
                ),
            );
        }

        // WP22 T4 — reject a name collision against any *other* existing
        // agent's directory name or `[agent] name` field. The directory-claim
        // `create_dir` below only catches an exact directory-name match; it
        // misses an existing agent whose `[agent] name` equals `name` while
        // living under a differently-named directory — that gap is exactly
        // what lets the registry's `name → LoadedAgent` map (last-wins) or the
        // delegation `name → dir` resolver silently pick the wrong one.
        // Rescan first (bounded) so a just-created sibling agent is visible.
        if let Ok(mut reg) =
            tokio::time::timeout(std::time::Duration::from_millis(500), self.registry.write()).await
        {
            let _ = reg.scan().await;
        }
        let name_collides = self.registry.read().await.list().iter().any(|a| {
            a.config.agent.name == name || a.dir.file_name().and_then(|n| n.to_str()) == Some(name)
        });
        if name_collides {
            return WsFrame::error_response(
                "",
                &format!("已有同名的 AI 員工({name}),請換一個名稱"),
            );
        }

        // Optional org placement, validated BEFORE any filesystem effect.
        // `reports_to` must name an existing agent (the supervisor hierarchy
        // drives team rosters + channel-token cascade); `department` follows
        // the WP7 allowlist. Empty = none, matching the field defaults.
        let reports_to = params
            .get("reports_to")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if !reports_to.is_empty() {
            if reports_to == name {
                return WsFrame::error_response("", "上級不能是自己");
            }
            let exists = self
                .registry
                .read()
                .await
                .list()
                .iter()
                .any(|a| a.config.agent.name == reports_to);
            if !exists {
                return WsFrame::error_response("", &format!("上級 AI 員工「{reports_to}」不存在"));
            }
        }
        let department = params
            .get("department")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if !department.is_empty() && !duduclaw_core::is_valid_department(&department) {
            return WsFrame::error_response("", "部門名稱只能使用英數字、'-'、'_'（1–64 字元）");
        }

        // Cloud-tier agent cap (self-host is never capped — Apache 2.0).
        let agent_count = self.registry.read().await.list().len();
        if let Some(msg) = self.tier_limit_message("agent", agent_count).await {
            return WsFrame::error_response("", &msg);
        }

        // Create agent directory and files. Own the agents-dir path and release
        // the read guard immediately — the post-create registry rescan below
        // needs a write guard, and holding this read guard across it deadlocks.
        let agents_dir = self.registry.read().await.agents_dir().to_path_buf();
        let agent_dir = agents_dir.join(name);

        // Atomic directory claim: `create_dir` (not `create_dir_all`) fails on
        // AlreadyExists, so two concurrent creates can't both win the name.
        if let Err(e) = tokio::fs::create_dir_all(&agents_dir).await {
            return WsFrame::error_response("", &format!("Failed to create directory: {e}"));
        }
        if let Err(e) = tokio::fs::create_dir(&agent_dir).await {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                return WsFrame::error_response("", &format!("Agent '{name}' already exists"));
            }
            return WsFrame::error_response("", &format!("Failed to create directory: {e}"));
        }

        // If creating as main, demote the current main agent — only AFTER the
        // name claim succeeded, so a doomed create (name collision) can no
        // longer demote the live main as a side effect.
        if role == "main" {
            if let Err(e) = self.demote_current_main(name).await {
                let _ = tokio::fs::remove_dir_all(&agent_dir).await;
                return WsFrame::error_response("", &e);
            }
        }

        let skills_dir = agent_dir.join("SKILLS");
        if let Err(e) = tokio::fs::create_dir_all(&skills_dir).await {
            let _ = tokio::fs::remove_dir_all(&agent_dir).await;
            return WsFrame::error_response("", &format!("Failed to create directory: {e}"));
        }
        Self::seed_builtin_skills(&skills_dir);

        // WP22 T1 — kept for the post-commit `org.toml` write below; the
        // `toml!` macro consumes these by value.
        let org_entry = duduclaw_core::OrgEntry::new(&reports_to, &department);

        let mut agent_config = toml::toml! {
            [agent]
            name = name
            display_name = display_name
            role = role
            status = "active"
            trigger = trigger
            reports_to = reports_to
            department = department
            icon = "🤖"

            [model]
            preferred = preferred_model
            fallback = "claude-haiku-4-5"
            account_pool = []

            [container]
            timeout_ms = 1800000
            max_concurrent = 1
            readonly_project = true
            additional_mounts = []

            [heartbeat]
            enabled = false
            interval_seconds = 3600
            max_concurrent_runs = 1
            cron = ""

            [budget]
            monthly_limit_cents = 5000
            warn_threshold_percent = 80
            hard_stop = true

            [permissions]
            can_create_agents = false
            can_send_cross_agent = true
            can_modify_own_skills = true
            can_modify_own_soul = false
            can_schedule_tasks = false
            allowed_channels = ["*"]

            [evolution]
            micro_reflection = false
            meso_reflection = false
            macro_reflection = false
            skill_auto_activate = false
            skill_security_scan = true
        };

        // Optional `[runtime]` (provider/fallback) from the create params — lets
        // the dashboard onboarding pick a non-Claude backend at create time
        // instead of a follow-up update. No `runtime` key ⇒ no-op (existing
        // callers unaffected). Invalid provider ⇒ fail and clean up the dir.
        let runtime_outcome = match apply_runtime_to_table_reporting(&mut agent_config, &params) {
            Ok(o) => o,
            Err(e) => {
                let _ = tokio::fs::remove_dir_all(&agent_dir).await;
                return WsFrame::error_response("", &e);
            }
        };

        let agent_toml = toml::to_string_pretty(&agent_config).unwrap_or_default();

        // XC.2: atomic write (temp + rename) — mirror the per-agent update path.
        let agent_toml_path = agent_dir.join("agent.toml");
        let agent_toml_tmp = agent_toml_path.with_extension("toml.tmp");
        if let Err(e) = tokio::fs::write(&agent_toml_tmp, &agent_toml).await {
            return WsFrame::error_response("", &format!("Failed to write agent.toml.tmp: {e}"));
        }
        if let Err(e) = tokio::fs::rename(&agent_toml_tmp, &agent_toml_path).await {
            let _ = tokio::fs::remove_file(&agent_toml_tmp).await;
            return WsFrame::error_response("", &format!("Failed to commit agent.toml: {e}"));
        }

        // WP22 T1 — the agent is committed, so record its authoritative org
        // placement in `<home>/org.toml`. The `[agent] reports_to` /
        // `department` keys written above are a display mirror from here on;
        // delegation reads the store. Recorded *after* the commit so an
        // aborted create never leaves a record behind for the id.
        if let Err(e) = duduclaw_core::org_store::upsert(&self.home_dir, name, org_entry) {
            warn!(agent = %name, error = %e, "org.toml upsert failed on agents.create");
        }

        // R1 (2026-10): a deprecated runtime is still written, but the write
        // is audited with the caller so the migration is traceable.
        audit_deprecated_runtime_writes(
            &self.home_dir,
            name,
            "agents.create",
            caller.map(|c| c.user_id.as_str()).unwrap_or("unknown"),
            &runtime_outcome.deprecated,
        );

        // Honor an optional `soul` param (the agent's persona / system prompt).
        // Trim + cap defensively; fall back to a stock one-liner when absent.
        // (Previously this param was silently dropped — see api.ts agents.create.)
        let soul = params
            .get("soul")
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| {
                format!(
                    "# {display_name}\n\n{}\n",
                    duduclaw_core::truncate_chars(s, 8000)
                )
            })
            .unwrap_or_else(|| {
                format!("# {display_name}\n\nI am {display_name}, a specialist AI agent.\n")
            });
        let _ = tokio::fs::write(agent_dir.join("SOUL.md"), &soul).await;

        // Install the agent-file-guard PreToolUse hook so this newly-created
        // agent immediately gets protected against out-of-tree Write/Edit.
        let bin = crate::agent_hook_installer::resolve_duduclaw_bin();
        if let Err(e) =
            crate::agent_hook_installer::ensure_agent_hook_settings(&agent_dir, &bin).await
        {
            tracing::warn!(
                agent = %name,
                error = %e,
                "Failed to install agent-file-guard hook on agents.create"
            );
        }

        // Refresh the in-memory registry from disk so subsequent RPCs can see
        // the just-created agent. Previously create only wrote files and left
        // `self.registry` stale, so the onboarding wizard's immediate
        // `agents.update` (and any other same-session lookup) failed with
        // "Agent not found: <id>". Best-effort: a rescan failure is logged but
        // does not fail the create (the files are already committed on disk).
        {
            let mut reg = self.registry.write().await;
            if let Err(e) = reg.scan().await {
                tracing::warn!(name, error = %e, "agent created but registry rescan failed");
            }
        }

        info!(name, "Agent created");
        WsFrame::ok_response(
            "",
            json!({
                "success": true,
                "agent": { "name": name, "display_name": display_name, "role": role, "status": "active" }
            }),
        )
    }
}
