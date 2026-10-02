use super::*;

/// Create a persistent sub-agent directory with agent.toml, SOUL.md, etc.
pub(crate) async fn handle_create_agent(params: &Value, home_dir: &Path, caller_agent: &str) -> Value {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let display_name = params
        .get("display_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if name.is_empty() || display_name.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: name and display_name are required"}],
            "isError": true
        });
    }

    // Validate name: safe for filesystem paths (no traversal)
    if !is_valid_agent_id(name) {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: name must be lowercase alphanumeric with hyphens, max 64 chars"}],
            "isError": true
        });
    }

    // WP21: the delegation system-sender ids (`cron`, `dashboard`, …) are
    // *not agents* (design doc §2.3). An agent that managed to claim one would
    // clear every delegation choke point unconditionally and could re-parent
    // any node in the org tree — self-service escalation by naming.
    if duduclaw_core::is_reserved_agent_id(name) {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "Error: 「{name}」是系統保留名稱,不能用來建立 AI 員工。\
                 保留名稱包含 dashboard / webhook / cron / heartbeat / autopilot / \
                 goal-loop-driver / a2a-client / default 以及任何以 __ 開頭的名稱,\
                 請換一個名稱。"
            )}],
            "isError": true
        });
    }

    let agent_dir = home_dir.join("agents").join(name);
    if agent_dir.exists() {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: agent '{name}' already exists at {}", agent_dir.display())}],
            "isError": true
        });
    }

    // WP22 T4 — reject a name collision against any *other* existing agent's
    // directory name or `[agent] name` field before creating anything. The
    // `agent_dir.exists()` check above only catches an exact directory-name
    // match; it misses an existing agent whose `[agent] name` equals `name`
    // while living under a differently-named directory (see
    // `collect_existing_agent_identifiers` for why that matters).
    if collect_existing_agent_identifiers(home_dir).contains(name) {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "Error: 已有同名的 AI 員工({name}),請換一個名稱"
            )}],
            "isError": true
        });
    }

    // Removed-name reservation. `agent_remove` moves an employee to `_trash`
    // instead of deleting it; recreating the same name here would hand the
    // seat (channel bindings, org position) to a fresh employee without the
    // operator's CONTRACT.toml / [capabilities] / sandbox settings. Every MCP
    // caller is treated as an AI caller for this rule: no supported operator
    // flow creates employees through MCP (the dashboard has `agents.create`,
    // the terminal has `duduclaw agent create`), and an "operator" signal on
    // this surface (absent identity env) is something an employee with a shell
    // can fake by relaunching the server. Fails closed on an unlistable trash.
    let reservation = duduclaw_core::agent_trash::check_name_reserved_for_ai(home_dir, name);
    if reservation.is_reserved() {
        duduclaw_security::audit::log_agent_name_reserved(
            home_dir,
            caller_agent,
            name,
            "mcp_create_agent",
            reservation.as_str(),
        );
        return serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "Error: {}",
                duduclaw_core::agent_trash::name_reserved_message(name, reservation)
            )}],
            "isError": true
        });
    }

    // Agent-count cap (edition / license quota). The dashboard enforces this
    // in `tier_limit_message`; without the same gate here, any agent could
    // `create_agent` its way past the Personal-edition cap or a signed
    // P-License `max_agents` quota. The MCP server is a separate process, so
    // the cap is resolved from disk (license + env), not the gateway global.
    let current_agents = count_existing_agents(home_dir);
    if let Some(msg) =
        duduclaw_gateway::license_runtime::agent_cap_message_from_disk(home_dir, current_agents)
            .await
    {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: {msg}")}],
            "isError": true
        });
    }

    let role = params
        .get("role")
        .and_then(|v| v.as_str())
        .unwrap_or("specialist");
    let reports_to = params
        .get("reports_to")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let soul = params.get("soul").and_then(|v| v.as_str()).unwrap_or("");
    let model = params
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("claude-sonnet-4-6");
    let trigger = params
        .get("trigger")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("@{display_name}"));
    let icon = params
        .get("icon")
        .and_then(|v| v.as_str())
        .unwrap_or("\u{1F916}");

    // Resolve reports_to when omitted. A system/human interface (dashboard,
    // webhook, ...) has no place of its own in the org tree, so it still
    // defaults to the main agent (pre-WP21 behaviour). A real agent caller
    // defaults to *itself*: defaulting every omitted `reports_to` to main used
    // to make a non-main caller's own placement check fail (it is not an
    // ancestor of main), so an agent that simply omitted the field could never
    // create a sub-agent under itself — the common case. Defaulting to the
    // caller keeps `check_org_placement_allowed` trivially satisfied (node ==
    // caller) while an explicit `reports_to` is still fully subject to that
    // check.
    let reports_to = if reports_to.is_empty() {
        if duduclaw_core::is_system_sender(caller_agent) {
            resolve_main_agent_name(home_dir).await
        } else {
            caller_agent.to_string()
        }
    } else {
        reports_to.to_string()
    };

    // Validate reports_to references an existing agent and won't create a cycle
    if let Err(reason) = validate_reports_to(home_dir, name, &reports_to).await {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: {reason}")}],
            "isError": true
        });
    }

    // WP21 C4: existence and acyclicity say the placement is *well-formed*;
    // they say nothing about whether this caller is entitled to it. Without the
    // gate below any agent could hang a new node under the CEO and thereby mint
    // itself a supervisor the delegation predicate would trust.
    if let Err(reason) =
        check_org_placement_allowed(home_dir, caller_agent, &reports_to, "建立 AI 員工").await
    {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error: {reason}")}],
            "isError": true
        });
    }

    // Create directory structure
    if let Err(e) = tokio::fs::create_dir_all(&agent_dir).await {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error creating agent directory: {e}")}],
            "isError": true
        });
    }
    let _ = tokio::fs::create_dir_all(agent_dir.join("SKILLS")).await;
    // RFC-26 P6.3: seed the deep-agents default skill set (code-review / refactor
    // / test-writer / git-workflow). Idempotent; never overwrites operator edits.
    match duduclaw_agent::builtin_skills::install_builtin_skills(&agent_dir.join("SKILLS")) {
        Ok(written) if !written.is_empty() => {
            info!(agent = name, skills = ?written, "seeded built-in skills");
        }
        Ok(_) => {}
        Err(e) => warn!("failed to seed built-in skills for {name}: {e}"),
    }

    // Write agent.toml — use toml crate to prevent injection via display_name/trigger/icon
    // Clone values that will be consumed by the toml! macro
    let reports_to_display = reports_to.clone();
    let agent_config = toml::toml! {
        [agent]
        name = name
        display_name = display_name
        role = role
        status = "active"
        trigger = trigger
        reports_to = reports_to
        icon = icon

        [model]
        preferred = model
        fallback = "claude-haiku-4-5"
        account_pool = []

        [container]
        timeout_ms = 1800000
        max_concurrent = 2
        readonly_project = true
        additional_mounts = []

        [heartbeat]
        enabled = false
        interval_seconds = 3600
        max_concurrent_runs = 1
        cron = ""

        [budget]
        monthly_limit_cents = 2000
        warn_threshold_percent = 80
        hard_stop = true

        [permissions]
        can_create_agents = false
        can_send_cross_agent = true
        can_modify_own_skills = true
        can_modify_own_soul = false
        can_schedule_tasks = false
        allowed_channels = []

        [evolution]
        skill_auto_activate = false
        skill_security_scan = true
        gvu_enabled = true
        strategy = "balanced"
        max_silence_hours = 12.0
    };
    let agent_toml = toml::to_string_pretty(&agent_config).unwrap_or_default();

    if let Err(e) = tokio::fs::write(agent_dir.join("agent.toml"), &agent_toml).await {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!("Error writing agent.toml: {e}")}],
            "isError": true
        });
    }

    // WP22 T1 — the agent now exists, so record its authoritative placement in
    // `<home>/org.toml`; `[agent] reports_to` above is a display mirror from
    // here on. Written *after* the commit, not before: a create that aborts
    // must not leave an entry behind for the id, or a later hand-created agent
    // of the same name would silently inherit this (stale) authority. If the
    // store write itself fails, the agent simply has no record and the
    // fallback rule keeps its `agent.toml` governing it — `duduclaw doctor`
    // stays quiet (no record ⇒ no drift) and the next gated edit records it.
    if let Err(e) = duduclaw_core::org_store::upsert(
        home_dir,
        name,
        duduclaw_core::OrgEntry::new(&reports_to_display, ""),
    ) {
        tracing::warn!(agent = %name, error = %e, "org.toml upsert failed on create_agent");
    }

    // Write SOUL.md if provided
    if !soul.is_empty() {
        let _ = tokio::fs::write(agent_dir.join("SOUL.md"), soul).await;
    }

    // Write empty MEMORY.md
    let _ = tokio::fs::write(agent_dir.join("MEMORY.md"), "").await;

    // Install agent-file-guard PreToolUse hook so the newly-created agent
    // immediately gets protected against out-of-tree Write/Edit.
    let bin = duduclaw_gateway::agent_hook_installer::resolve_duduclaw_bin();
    if let Err(e) =
        duduclaw_gateway::agent_hook_installer::ensure_agent_hook_settings(&agent_dir, &bin).await
    {
        tracing::warn!(
            agent = %name,
            error = %e,
            "Failed to install agent-file-guard hook via MCP create_agent"
        );
    }

    // Surface the creation in the dashboard activity feed (紀錄/即時動態).
    // Without this, spawning a whole team leaves zero visible trace until the
    // next agents.list poll. Best-effort — feed failure never fails the tool.
    {
        let actor = if is_valid_agent_id(caller_agent) {
            caller_agent
        } else {
            "system"
        };
        match duduclaw_gateway::task_store::TaskStore::open(home_dir) {
            Ok(store) => {
                let row = duduclaw_gateway::task_store::ActivityRow {
                    id: uuid::Uuid::new_v4().to_string(),
                    event_type: "agent_created".to_string(),
                    agent_id: actor.to_string(),
                    task_id: None,
                    summary: format!("建立新 AI 員工「{display_name}」({name})，角色:{role}"),
                    timestamp: chrono::Utc::now().to_rfc3339(),
                    metadata: Some(
                        serde_json::json!({ "created_agent": name, "role": role }).to_string(),
                    ),
                };
                if let Err(e) = store.append_activity(&row).await {
                    tracing::warn!(agent = %name, error = %e, "create_agent: activity append failed");
                } else {
                    append_bus_event(home_dir, "activity.new", &activity_row_to_json(&row)).await;
                }
            }
            Err(e) => {
                tracing::warn!(agent = %name, error = %e, "create_agent: open task store failed");
            }
        }
    }

    serde_json::json!({
        "content": [{"type": "text", "text": format!(
            "Agent '{display_name}' ({name}) created successfully.\n\
             Role: {role}\n\
             Reports to: {reports_to_display}\n\
             Model: {model}\n\
             Directory: {}\n\n\
             The agent is now available for delegation via send_to_agent or spawn_agent.",
            agent_dir.display()
        )}]
    })
}

/// List all registered agents with role, status, and hierarchy.
pub(crate) async fn handle_list_agents(params: &Value, home_dir: &Path, caller: &str) -> Value {
    // F2: accept both a bool and the string "true" (MCP args often arrive as
    // strings); default false so soft-deleted stay hidden and archived only
    // surface on explicit request.
    let include_archived = params
        .get("include_archived")
        .map(|v| v.as_bool().unwrap_or_else(|| v.as_str() == Some("true")))
        .unwrap_or(false);
    let agents_dir = home_dir.join("agents");
    let mut entries = match tokio::fs::read_dir(&agents_dir).await {
        Ok(e) => e,
        Err(e) => {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error reading agents directory: {e}")}],
                "isError": true
            });
        }
    };

    // WP21 T6 (design doc §2.5): `open` policy and system senders (dashboard /
    // heartbeat / ...) keep the pre-WP21 unrestricted listing.
    let rules = duduclaw_core::delegation_rules_from_home(home_dir);
    let unrestricted = rules.policy == duduclaw_core::DelegationPolicy::Open
        || duduclaw_core::is_system_sender(caller);

    let mut candidates: Vec<(String, Value)> = Vec::new();
    // Fed from *every* parsed agent.toml this scan sees, listable or not — an
    // archived/soft-deleted node can still sit on someone's ancestor chain,
    // and truncating the snapshot to only-listed agents would silently break
    // that chain. One pass over the directory instead of `org_snapshot`'s
    // per-pair reads, since list_agents already visits every agent anyway.
    //
    // WP22 T1: the *visibility* view is built from `<home>/org.toml` when that
    // agent has a record there, falling back to its `agent.toml` when it does
    // not — the same authority rule `org_snapshot` applies, so "who may see
    // whom" and "who may delegate to whom" can never disagree. The per-agent
    // JSON below still reports the mirror's `reports_to`, which is what the
    // operator sees in the file (drift is surfaced by `duduclaw doctor`, not
    // by silently showing a different value here).
    let org_store = duduclaw_core::org_store::load(home_dir);
    let mut org = duduclaw_core::MapOrgView::new();

    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let dir_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        if dir_name.starts_with('_') {
            continue;
        }

        let toml_path = path.join("agent.toml");
        if let Ok(content) = tokio::fs::read_to_string(&toml_path).await
            && let Ok(config) = toml::from_str::<duduclaw_core::types::AgentConfig>(&content)
        {
            match org_store.get(&dir_name) {
                Some(entry) => org.insert(
                    dir_name.as_str(),
                    entry.reports_to.as_str(),
                    entry.department.as_str(),
                ),
                None => org.insert(
                    dir_name.as_str(),
                    normalize_reports_to(config.agent.reports_to.trim()),
                    config.agent.department.trim(),
                ),
            }
            // F2: hide soft-deleted always; hide archived unless requested.
            if !config.agent.status.is_listable(include_archived) {
                continue;
            }
            candidates.push((
                dir_name.clone(),
                serde_json::json!({
                    "name": config.agent.name,
                    "display_name": config.agent.display_name,
                    "role": format!("{:?}", config.agent.role).to_lowercase(),
                    "status": format!("{:?}", config.agent.status).to_lowercase(),
                    "reports_to": config.agent.reports_to,
                    "icon": config.agent.icon,
                    "model": config.model.preferred,
                    "can_create_agents": config.permissions.can_create_agents,
                    "can_schedule_tasks": config.permissions.can_schedule_tasks,
                }),
            ));
        }
    }

    // WP21 T6: drop anything the caller may not see. `agents_dir` entries are
    // keyed by directory name, which is the agent id `org_visible` expects.
    let agents: Vec<Value> = candidates
        .into_iter()
        .filter(|(name, _)| unrestricted || org_visible(&rules, &org, caller, name))
        .map(|(_, v)| v)
        .collect();

    if agents.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "No agents found."}]
        });
    }

    // Build a readable text table
    let mut lines = vec![format!("Found {} agent(s):\n", agents.len())];
    for a in &agents {
        let name = a["name"].as_str().unwrap_or("?");
        let display = a["display_name"].as_str().unwrap_or("?");
        let role = a["role"].as_str().unwrap_or("?");
        let status = a["status"].as_str().unwrap_or("?");
        let reports_to = a["reports_to"].as_str().unwrap_or("");
        let icon = a["icon"].as_str().unwrap_or("");
        let hierarchy = if reports_to.is_empty() {
            "(root)".to_string()
        } else {
            format!("-> {reports_to}")
        };
        lines.push(format!(
            "{icon} {display} ({name}) [{role}/{status}] {hierarchy}"
        ));
    }

    serde_json::json!({
        "content": [{"type": "text", "text": lines.join("\n")}]
    })
}

/// Get detailed status of a specific agent.
pub(crate) async fn handle_agent_status(params: &Value, home_dir: &Path, caller: &str) -> Value {
    let agent_id = params
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if agent_id.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: agent_id is required"}],
            "isError": true
        });
    }
    if !is_valid_agent_id(agent_id) {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: agent_id must be lowercase alphanumeric with hyphens"}],
            "isError": true
        });
    }

    let agent_dir = home_dir.join("agents").join(agent_id);
    let toml_path = agent_dir.join("agent.toml");

    let content = match tokio::fs::read_to_string(&toml_path).await {
        Ok(c) => c,
        Err(_) => return agent_not_visible_error(agent_id),
    };

    let config: duduclaw_core::types::AgentConfig = match toml::from_str(&content) {
        Ok(c) => c,
        Err(e) => {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error parsing agent.toml: {e}")}],
                "isError": true
            });
        }
    };

    // WP21 T6 (design doc §2.5): read-side visibility gate. Same response
    // shape as "not found" (`agent_not_visible_error`) so the tool cannot be
    // used to probe which agent ids exist versus which are merely hidden.
    // `open` policy, system senders, and the caller looking up itself all
    // skip the org read entirely.
    let rules = duduclaw_core::delegation_rules_from_home(home_dir);
    if rules.policy != duduclaw_core::DelegationPolicy::Open
        && !duduclaw_core::is_system_sender(caller)
        && caller != agent_id
    {
        let org = org_snapshot(home_dir, &[caller, agent_id]).await;
        if !org_visible(&rules, &org, caller, agent_id) {
            return agent_not_visible_error(agent_id);
        }
    }

    // Check for SOUL.md, skills, memory
    let has_soul = agent_dir.join("SOUL.md").exists();
    let has_identity = agent_dir.join("IDENTITY.md").exists();
    let skill_count = match tokio::fs::read_dir(agent_dir.join("SKILLS")).await {
        Ok(mut entries) => {
            let mut count = 0u32;
            while let Ok(Some(_)) = entries.next_entry().await {
                count += 1;
            }
            count
        }
        Err(_) => 0,
    };

    // Check pending bus_queue messages for this agent
    let pending_tasks = count_pending_tasks(home_dir, agent_id).await;

    let info = format!(
        "Agent: {} ({})\n\
         Role: {:?} | Status: {:?}\n\
         Reports to: {}\n\
         Model: {} (fallback: {})\n\
         Icon: {}\n\
         Trigger: {}\n\
         \n\
         Files:\n\
         - SOUL.md: {}\n\
         - IDENTITY.md: {}\n\
         - Skills: {} file(s)\n\
         - Directory: {}\n\
         \n\
         Permissions:\n\
         - Create agents: {}\n\
         - Cross-agent messaging: {}\n\
         - Schedule tasks: {}\n\
         - Modify own skills: {}\n\
         - Allowed channels: {:?}\n\
         \n\
         Budget: {} cents/month (warn: {}%, hard stop: {})\n\
         Heartbeat: {} (interval: {}s)\n\
         Pending tasks in queue: {}",
        config.agent.display_name,
        config.agent.name,
        config.agent.role,
        config.agent.status,
        if config.agent.reports_to.is_empty() {
            "(root)"
        } else {
            &config.agent.reports_to
        },
        config.model.preferred,
        config.model.fallback,
        config.agent.icon,
        config.agent.trigger,
        if has_soul { "yes" } else { "no" },
        if has_identity { "yes" } else { "no" },
        skill_count,
        agent_dir.display(),
        config.permissions.can_create_agents,
        config.permissions.can_send_cross_agent,
        config.permissions.can_schedule_tasks,
        config.permissions.can_modify_own_skills,
        config.permissions.allowed_channels,
        config.budget.monthly_limit_cents,
        config.budget.warn_threshold_percent,
        config.budget.hard_stop,
        if config.heartbeat.enabled {
            "enabled"
        } else {
            "disabled"
        },
        config.heartbeat.interval_seconds,
        pending_tasks,
    );

    serde_json::json!({
        "content": [{"type": "text", "text": info}]
    })
}

/// Read an agent's lifecycle status from its `agent.toml` (`[agent].status`).
/// Robust to unrelated config fields — parses only the one field. Returns
/// `None` when the file is missing / unparseable / the field is absent, so
/// callers can decide the indeterminate policy (spawn treats indeterminate as
/// operational for backward-compat with pre-WP4 configs that lack the field).
pub(crate) fn agent_status_of(home_dir: &Path, agent_id: &str) -> Option<duduclaw_core::types::AgentStatus> {
    // Shared typed parse point (R2 unification). `status` stays a raw String
    // on the view so an unrecognised value keeps resolving to `None`
    // (indeterminate) here rather than becoming a hard deserialization error
    // that would take the whole `AgentConfig` — and the agent — with it.
    let status_str = duduclaw_core::agent_toml::load_for_agent(home_dir, agent_id)
        .agent
        .and_then(|a| a.status)?;
    // AgentStatus derives Deserialize with snake_case rename, so a bare status
    // string round-trips through serde.
    serde_json::from_value(serde_json::Value::String(status_str)).ok()
}
