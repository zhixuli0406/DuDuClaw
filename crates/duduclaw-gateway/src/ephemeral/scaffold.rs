//! Scaffolding one ephemeral agent directory: the spawn spec, the parent
//! capability read and the whole `scaffold_with` writer.
//! Moved verbatim out of `ephemeral.rs`.

use super::*;

/// Specification for one ephemeral synthesis (the four-tuple minus context,
/// which travels on the bus as the task payload).
#[derive(Debug, Clone)]
pub struct EphemeralSpawnSpec {
    /// The synthesizing (parent) agent id — capability envelope source.
    pub parent: String,
    /// Instruction → SOUL.md.
    pub instruction: String,
    /// Requested tool subset → `[capabilities] allowed_tools`.
    pub tools: Vec<String>,
    /// Model tier keyword ("cheap" / "standard" / "preferred").
    pub tier: String,
}

/// Result of a successful scaffold.
#[derive(Debug, Clone)]
pub struct ScaffoldResult {
    pub agent_id: String,
    pub dir: PathBuf,
}

/// Count live (non-hidden) scaffold directories under the ephemeral root.
pub(super) fn active_count(root: &Path) -> usize {
    std::fs::read_dir(root)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .count()
        })
        .unwrap_or(0)
}

/// Scaffold an ephemeral agent directory. Fail-closed at every step:
/// - the parent's `agent.toml` must exist AND parse as a full `AgentConfig`
///   (an unreadable capability envelope means we cannot prove containment);
/// - the requested tools must pass [`check_tool_subset`];
/// - at most [`MAX_ACTIVE_EPHEMERAL`] live scaffolds (runaway-synthesis
///   circuit breaker).
///
/// The parent's raw `[model]` and `[runtime]` tables are copied verbatim so
/// tier→model resolution at dispatch time sees exactly the parent's model
/// lineup — no model ids are hardcoded or accepted from the caller.
pub fn scaffold(home_dir: &Path, spec: &EphemeralSpawnSpec) -> Result<ScaffoldResult, String> {
    scaffold_with(
        home_dir,
        spec,
        ScaffoldPlan {
            agent_id: None,
            role: None,
        },
    )
}

/// The parent employee's `agent.toml`, as raw text and as a parsed
/// `AgentConfig` — the single read [`scaffold_with`] bases both the verbatim
/// section copy and the capability containment check on.
///
/// Extracted so [`parent_capabilities`] can hand the *same* envelope to a
/// caller that needs to know what it may request **before** it requests it.
/// Two independent readers would be free to drift (a preset-resolved mirror
/// vs. the raw file, say), and a drift here reads as a mysterious
/// "privilege escalation rejected" on a tool list the caller derived from the
/// parent itself.
fn read_parent_config(
    home_dir: &Path,
    parent: &str,
) -> Result<(String, duduclaw_core::types::AgentConfig), String> {
    let path = home_dir.join("agents").join(parent).join("agent.toml");
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read parent agent '{parent}': {e} (fail-closed)"))?;
    let config: duduclaw_core::types::AgentConfig = toml::from_str(&raw)
        .map_err(|e| format!("cannot parse parent agent '{parent}' config: {e} (fail-closed)"))?;
    Ok((raw, config))
}

/// The parent employee's `[capabilities]` envelope, read exactly as
/// [`check_tool_subset`] will read it at scaffold time.
///
/// The team composer uses this to derive a role's tool list *from* the
/// employee's own envelope instead of from a fixed literal — an executor that
/// must edit files and run a build cannot be expressed as three read-only MCP
/// tools, and a hardcoded list is also how the member ended up in a
/// `read-only` codex sandbox in live round 5.
///
/// `None` when the parent's `agent.toml` is missing or unparseable. That is
/// the same condition [`scaffold_with`] refuses on, so a caller may fall back
/// to a default envelope and let the scaffold produce the authoritative,
/// audited refusal rather than inventing a second error path.
pub fn parent_capabilities(home_dir: &Path, parent: &str) -> Option<CapabilitiesConfig> {
    read_parent_config(home_dir, parent)
        .ok()
        .map(|(_, config)| config.capabilities)
}

/// Write the scaffold's `.mcp.json` (duduclaw server + identity env) and
/// verify the file actually landed.
///
/// [`duduclaw_agent::mcp_template::ensure_duduclaw_absolute_path`] is a
/// *migration* helper: it reports `Ok(false)` and writes nothing when the
/// duduclaw binary cannot be resolved to an absolute path, which is the right
/// silence for a boot sweep over existing agents but a mute agent here. The
/// existence re-check converts that silence into an error so the caller can
/// fail the scaffold closed.
fn ensure_scaffold_mcp_config(dir: &Path) -> Result<(), String> {
    duduclaw_agent::mcp_template::ensure_duduclaw_absolute_path(dir)?;
    let path = dir.join(".mcp.json");
    if path.is_file() {
        Ok(())
    } else {
        Err(format!(
            "{} was not created (duduclaw binary path did not resolve to an \
             absolute location)",
            path.display()
        ))
    }
}

/// Shared scaffold body behind [`scaffold`] and [`scaffold_role_member`].
///
/// Every gate is identical for both callers — parent-config fail-closed parse,
/// [`check_tool_subset`], the advisory-locked count+create against
/// `ephemeral_max_active`, the verbatim `[model]/[runtime]/[container]/[budget]`
/// copy, heartbeat/evolution/permissions hard-off. The role plan only *adds*
/// overrides on top of the copied tables, so a role member can never end up
/// with a weaker envelope than an ordinary ephemeral.
pub(super) fn scaffold_with(
    home_dir: &Path,
    spec: &EphemeralSpawnSpec,
    plan: ScaffoldPlan<'_>,
) -> Result<ScaffoldResult, String> {
    if !duduclaw_core::is_valid_agent_id(&spec.parent) {
        return Err("invalid parent agent id".to_string());
    }
    let tier = parse_tier(&spec.tier).ok_or_else(|| {
        format!(
            "invalid tier '{}' (valid: cheap, standard, preferred)",
            spec.tier
        )
    })?;
    if spec.instruction.trim().is_empty() {
        return Err("instruction must not be empty".to_string());
    }
    if spec.instruction.chars().count() > 16_000 {
        return Err("instruction too long (max 16000 chars)".to_string());
    }

    // ── Parent capability envelope (fail-closed) ────────────────────────
    let (parent_raw, parent_config) = read_parent_config(home_dir, &spec.parent)?;
    // Team role members carry the handoff channel itself; an ordinary
    // ephemeral (the MCP `spawn_ephemeral` path) gets no intrinsics at all.
    let intrinsics: &[&str] = if plan.role.is_some() {
        TEAM_INTRINSIC_TOOLS
    } else {
        &[]
    };
    check_tool_subset_with_intrinsics(&parent_config.capabilities, &spec.tools, intrinsics)?;

    // ── Circuit breaker ──────────────────────────────────────────────────
    // TOCTOU fix (2026-07 MED): count + create run under the cross-process
    // advisory lock (sidecar `.ephemeral.lock` next to the root, so the lock
    // file is never counted as a scaffold) — parallel spawns from the gateway
    // and MCP-server processes can no longer race past the cap.
    //
    // H19: the cap itself is now config-driven (`[dispatch]
    // ephemeral_max_active`, default `MAX_ACTIVE_EPHEMERAL`) and clamped to
    // at least 1 — a concurrency limit can be adjusted but never fully
    // disabled. `scaffold`'s own contract (hard `Err` at the cap) is
    // otherwise UNCHANGED: it stays a synchronous, TOCTOU-safe circuit
    // breaker. Queue-vs-fail admission handling lives one layer up, in the
    // `spawn_ephemeral` MCP tool, which recognizes this specific error via
    // [`EPHEMERAL_CAPACITY_ERROR_PREFIX`] and decides whether to durably
    // queue the request instead of forwarding the rejection.
    let admission_cfg = duduclaw_core::spawn_admission::AdmissionConfig::from_home(home_dir);
    let cap = duduclaw_core::spawn_admission::clamp_min_one(
        admission_cfg.ephemeral_max_active,
        EPHEMERAL_ADMISSION_CLASS,
    ) as usize;
    let root = ephemeral_root(home_dir);
    let agent_id = plan.agent_id.clone().unwrap_or_else(new_ephemeral_id);
    let dir = root.join(&agent_id);
    let created = duduclaw_core::with_file_lock(&root, || {
        if active_count(&root) >= cap {
            return Ok(false);
        }
        std::fs::create_dir_all(&dir)?;
        Ok(true)
    })
    .map_err(|e| format!("create scaffold dir: {e}"))?;
    if !created {
        return Err(format!(
            "{EPHEMERAL_CAPACITY_ERROR_PREFIX} ({cap} live scaffolds) — \
             wait for the hourly GC sweep or complete running tasks first"
        ));
    }

    // ── agent.toml — built as a toml::Table (injection-safe serializer) ──
    let parent_value: toml::Value = parent_raw
        .parse()
        .map_err(|e| format!("re-parse parent toml: {e}"))?;

    let mut table = toml::Table::new();

    let mut agent_tbl = toml::Table::new();
    agent_tbl.insert("name".into(), toml::Value::String(agent_id.clone()));
    agent_tbl.insert(
        "display_name".into(),
        toml::Value::String(match plan.role {
            Some(r) => format!("Team {} ({})", r.role.as_str(), r.parent_agent),
            None => format!("Ephemeral ({agent_id})"),
        }),
    );
    // Always `worker`: see [`ROLE_MEMBER_SECTION`] for why a team role is not
    // expressed as an `AgentRole`.
    agent_tbl.insert("role".into(), toml::Value::String("worker".into()));
    agent_tbl.insert("status".into(), toml::Value::String("active".into()));
    agent_tbl.insert(
        "trigger".into(),
        toml::Value::String(format!("@{agent_id}")),
    );
    agent_tbl.insert(
        "reports_to".into(),
        toml::Value::String(spec.parent.clone()),
    );
    agent_tbl.insert("icon".into(), toml::Value::String("\u{1F9EA}".into())); // 🧪
    table.insert("agent".into(), toml::Value::Table(agent_tbl));

    // Copy the parent's [model] / [runtime] tables verbatim so tier→model
    // resolution and the multi-model doctrine guard behave exactly as they
    // do for the parent — plus [container] / [budget], which `AgentConfig`
    // requires (the ephemeral agent inherits the parent's isolation and
    // budget envelope; it can only ever be *more* restricted, never less,
    // because capabilities below are the requested subset).
    for section in ["model", "runtime", "container", "budget"] {
        if let Some(v) = parent_value.get(section) {
            table.insert(section.into(), v.clone());
        }
    }

    // ── Team role member: override the copied model/runtime assignment ────
    // The copy above is the employee's lineup; a role member's whole purpose
    // is to run somewhere else. Overrides are applied *after* the copy so every
    // key the employee set (fallback, account_pool, utility, …) is inherited
    // and only the three the role owns are replaced. Validation happened before
    // the scaffold directory was created, in `scaffold_role_member`.
    if let Some(role_spec) = plan.role {
        let mut model_tbl = match table.get("model") {
            Some(toml::Value::Table(t)) => t.clone(),
            _ => toml::Table::new(),
        };
        model_tbl.insert(
            "preferred".into(),
            toml::Value::String(role_spec.model.clone()),
        );
        // `effort` is written ONLY when the role declares one: an absent key
        // means "pass no flag / provider default", which is not the same as any
        // of the five values. Writing a default here would silently change
        // every role member's cost and depth.
        match role_spec.effort {
            Some(effort) => {
                model_tbl.insert("effort".into(), toml::Value::String(effort.as_str().into()));
            }
            None => {
                model_tbl.remove("effort");
            }
        }
        table.insert("model".into(), toml::Value::Table(model_tbl));

        let mut runtime_tbl = match table.get("runtime") {
            Some(toml::Value::Table(t)) => t.clone(),
            _ => toml::Table::new(),
        };
        runtime_tbl.insert(
            "provider".into(),
            toml::Value::String(role_spec.runtime.clone()),
        );
        // A `fallback` inherited from the employee would silently move the role
        // to another vendor on the first failure — which is exactly the
        // decorrelation the verifier rule exists to guarantee. A role fails as
        // the role, or not at all.
        runtime_tbl.remove("fallback");
        table.insert("runtime".into(), toml::Value::Table(runtime_tbl));

        let mut member_tbl = toml::Table::new();
        member_tbl.insert(
            "role".into(),
            toml::Value::String(role_spec.role.as_str().into()),
        );
        member_tbl.insert(
            "task_id".into(),
            toml::Value::String(role_spec.task_id.clone()),
        );
        member_tbl.insert("round".into(), toml::Value::Integer(role_spec.round.into()));
        member_tbl.insert(
            "parent".into(),
            toml::Value::String(role_spec.parent_agent.clone()),
        );
        table.insert(ROLE_MEMBER_SECTION.into(), toml::Value::Table(member_tbl));
    }

    // Heartbeat / evolution: hard OFF — an ephemeral agent is a one-task
    // worker, it must never heartbeat, self-evolve, or persist behavior.
    let mut hb_tbl = toml::Table::new();
    hb_tbl.insert("enabled".into(), toml::Value::Boolean(false));
    hb_tbl.insert("interval_seconds".into(), toml::Value::Integer(3600));
    hb_tbl.insert("max_concurrent_runs".into(), toml::Value::Integer(1));
    hb_tbl.insert("cron".into(), toml::Value::String(String::new()));
    table.insert("heartbeat".into(), toml::Value::Table(hb_tbl));

    // D7 (2026-08-04): `cognitive_memory` is no longer written — the layer is
    // always on, so emitting the key would only bake a dead flag into every
    // ephemeral agent.toml. Heartbeat / GVU / skill activation stay hard OFF.
    let mut evo_tbl = toml::Table::new();
    evo_tbl.insert("skill_auto_activate".into(), toml::Value::Boolean(false));
    evo_tbl.insert("skill_security_scan".into(), toml::Value::Boolean(true));
    evo_tbl.insert("gvu_enabled".into(), toml::Value::Boolean(false));
    table.insert("evolution".into(), toml::Value::Table(evo_tbl));

    let mut caps_tbl = toml::Table::new();
    caps_tbl.insert(
        "allowed_tools".into(),
        toml::Value::Array(
            spec.tools
                .iter()
                .map(|t| toml::Value::String(t.trim().to_string()))
                .collect(),
        ),
    );
    // Inherit the parent's denies on top of the allowlist (deny wins).
    caps_tbl.insert(
        "denied_tools".into(),
        toml::Value::Array(
            parent_config
                .capabilities
                .denied_tools
                .iter()
                .map(|t| toml::Value::String(t.clone()))
                .collect(),
        ),
    );
    table.insert("capabilities".into(), toml::Value::Table(caps_tbl));

    let mut perm_tbl = toml::Table::new();
    perm_tbl.insert("can_create_agents".into(), toml::Value::Boolean(false));
    perm_tbl.insert("can_send_cross_agent".into(), toml::Value::Boolean(true));
    perm_tbl.insert("can_modify_own_skills".into(), toml::Value::Boolean(false));
    perm_tbl.insert("can_modify_own_soul".into(), toml::Value::Boolean(false));
    perm_tbl.insert("can_schedule_tasks".into(), toml::Value::Boolean(false));
    perm_tbl.insert("allowed_channels".into(), toml::Value::Array(vec![]));
    table.insert("permissions".into(), toml::Value::Table(perm_tbl));

    let agent_toml = toml::to_string_pretty(&toml::Value::Table(table))
        .map_err(|e| format!("serialize agent.toml: {e}"))?;
    std::fs::write(dir.join("agent.toml"), agent_toml)
        .map_err(|e| format!("write agent.toml: {e}"))?;

    // ── SOUL.md (stable role prefix or ordinary ephemeral instruction) ───
    // Team members receive `spec.instruction` again as their dispatch prompt.
    // Duplicating that task/packet text here made every member's system
    // prefix unique and defeated provider-side prompt caching.
    let soul = match plan.role {
        Some(r) => {
            let identity_path = home_dir.join("agents").join(&spec.parent).join("SOUL.md");
            let identity = match std::fs::read_to_string(&identity_path) {
                Ok(text) => text,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(e) => {
                    let _ = std::fs::remove_dir_all(&dir);
                    return Err(format!(
                        "read parent identity {}: {e}",
                        identity_path.display()
                    ));
                }
            };
            if identity.trim().is_empty() {
                stable_role_soul(r.role).to_string()
            } else {
                format!(
                    "{}\n\n{}\n{}",
                    stable_role_soul(r.role),
                    crate::direct_api::CACHE_SPLIT_MARKER,
                    identity.trim()
                )
            }
        }
        None => spec.instruction.clone(),
    };
    std::fs::write(dir.join("SOUL.md"), soul).map_err(|e| format!("write SOUL.md: {e}"))?;

    // ── .mcp.json — the scaffold's ONLY route to the duduclaw MCP server ──
    //
    // The Claude CLI auto-discovers `<work_dir>/.mcp.json` at spawn, and the
    // gateway's boot fixup (`ensure_mcp_absolute_paths_all`) only walks
    // `<home>/agents/*` — it never descends into `.ephemeral/`. Nothing else
    // in the codebase writes this file for a scaffold, so before this call an
    // ephemeral (O2) and a team role member (WP-2) both started with **zero**
    // duduclaw tools: no `team_handoff`, no memory, no tasks. Observed live as
    // a planner that could not hand anything back and a round that ended
    // `planner_no_packets`.
    //
    // The env block inside is also the ONLY channel by which the member's
    // mcp-server child learns the home/port: `duduclaw_core::spawn_env`'s
    // allowlist deliberately strips `DUDUCLAW_HOME` / `DUDUCLAW_PORT` from
    // the spawned CLI's environment, so an inherited env cannot stand in for
    // it. `derive_home_from_agent_dir` inside the helper already accounts for
    // the extra `.ephemeral/` path segment, so the signed identity token is
    // minted against this `home_dir`'s key, not `<home>/agents`'s.
    //
    // Deliberately duduclaw-only: the parent's other MCP servers (playwright,
    // browserbase, …) are NOT copied. A scaffold's capability envelope is the
    // requested subset of the parent's, and the per-spawn layers own that
    // decision (capability-filtered tool surface, redaction proxy rewrite) —
    // copying the parent's server list here would hand a role member browser
    // reach nobody granted it. Revisit only if a role genuinely needs one.
    //
    // Fail-closed: a member that cannot reach the MCP server cannot hand its
    // work back, so a failure here aborts the scaffold and removes the
    // directory instead of leaving a mute agent that still occupies a cap
    // slot.
    if let Err(e) = ensure_scaffold_mcp_config(&dir) {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(format!("scaffold '{agent_id}' MCP config: {e}"));
    }

    let now = chrono::Utc::now();
    let meta = EphemeralMeta {
        parent: spec.parent.clone(),
        tier: tier.as_str().to_string(),
        created_at: now.to_rfc3339(),
        expires_at: (now + chrono::Duration::hours(EPHEMERAL_TTL_HOURS)).to_rfc3339(),
    };
    let meta_toml =
        toml::to_string_pretty(&meta).map_err(|e| format!("serialize ephemeral.toml: {e}"))?;
    std::fs::write(dir.join("ephemeral.toml"), meta_toml)
        .map_err(|e| format!("write ephemeral.toml: {e}"))?;

    // WP22 T1 — record the ephemeral's placement in the authoritative store.
    // `reports_to = <parent>` is precisely the relation that authorises this
    // scaffold's dispatch, and `DispatchOrgView` now reads the store first; an
    // ephemeral with no record would fall back to the `agent.toml` written
    // above, i.e. to a file inside its own workspace. Department-less by
    // design (an ephemeral inherits isolation, not org membership).
    // Symmetric teardown lives in `sweep`.
    if let Err(e) = duduclaw_core::org_store::upsert(
        home_dir,
        &agent_id,
        duduclaw_core::OrgEntry::new(&spec.parent, ""),
    ) {
        tracing::warn!(
            ephemeral = %agent_id,
            error = %e,
            "org.toml upsert failed for ephemeral scaffold"
        );
    }

    // Audit: creation (existing tool_calls.jsonl convention). Role members get
    // their assignment recorded as extras — never colliding with the five
    // canonical field names (`append_tool_call_with_extras` drops those).
    let mut extras: Vec<(&str, serde_json::Value)> =
        vec![("ephemeral_id", serde_json::Value::String(agent_id.clone()))];
    if let Some(r) = plan.role {
        extras.push(("role", serde_json::Value::String(r.role.as_str().into())));
        extras.push(("task_id", serde_json::Value::String(r.task_id.clone())));
        extras.push(("round", serde_json::Value::from(r.round)));
        extras.push(("runtime", serde_json::Value::String(r.runtime.clone())));
        extras.push(("model", serde_json::Value::String(r.model.clone())));
        if let Some(e) = r.effort {
            extras.push(("effort", serde_json::Value::String(e.as_str().into())));
        }
    }
    let (tool_name, params_summary) = match plan.role {
        Some(r) => (
            "role_member_scaffold",
            format!(
                "agent_id={agent_id} role={} task_id={} round={} runtime={} model={} effort={} \
                 tools={}",
                r.role.as_str(),
                r.task_id,
                r.round,
                r.runtime,
                r.model,
                r.effort.map(|e| e.as_str()).unwrap_or("-"),
                spec.tools.len()
            ),
        ),
        None => (
            "ephemeral_scaffold",
            format!(
                "agent_id={agent_id} tier={} tools={}",
                tier.as_str(),
                spec.tools.len()
            ),
        ),
    };
    duduclaw_security::audit::append_tool_call_with_extras(
        home_dir,
        &spec.parent,
        tool_name,
        &params_summary,
        true,
        &extras,
    );
    match plan.role {
        Some(r) => tracing::info!(
            parent = %spec.parent,
            member = %agent_id,
            role = r.role.as_str(),
            task_id = %r.task_id,
            round = r.round,
            runtime = %r.runtime,
            model = %r.model,
            effort = r.effort.map(|e| e.as_str()).unwrap_or("-"),
            tools = spec.tools.len(),
            "team role member scaffolded (WP-2)"
        ),
        None => tracing::info!(
            parent = %spec.parent,
            ephemeral = %agent_id,
            tier = tier.as_str(),
            tools = spec.tools.len(),
            "ephemeral agent scaffolded (O2)"
        ),
    }

    // Cost attribution (2026-07): map this eph id to its parent so cost
    // reports (`all_agents_summary`, `multi_vs_single`) fold `eph-*` spend
    // into "<parent> (ephemeral)" instead of one meaningless row per
    // scaffold. Raw token_usage rows stay truthful under the eph id — the
    // mapping is applied at report time. Best-effort: a telemetry failure
    // must never fail the scaffold.
    if let Err(e) =
        crate::cost_telemetry::record_ephemeral_parent(home_dir, &agent_id, &spec.parent)
    {
        tracing::warn!(
            ephemeral = %agent_id,
            parent = %spec.parent,
            "could not record ephemeral cost-parent mapping: {e}"
        );
    }

    Ok(ScaffoldResult { agent_id, dir })
}

// ---------------------------------------------------------------------------
// Team role members (WP-2)
// ---------------------------------------------------------------------------

/// Validate a role's `(runtime, model)` pair and return the **canonical**
/// runtime id (so an alias like `agy` is written as `antigravity`).
///
/// The same two rules [`duduclaw_core::types::validate_team`] applies, on the
/// same catalog data, because the composer may build a spec from a source that
/// never went through `validate_team` (a per-task override, a replayed queue
/// ticket) and a model in the wrong family would otherwise be handed to the
/// wrong CLI — `gpt-5.4` into the Claude binary — and fail at the vendor API
/// with an error nobody can trace back to config.
///
/// 1. `runtime` must resolve through the catalog **and** be in
///    [`TEAM_ROLE_RUNTIME_ALLOWLIST`] (a runtime whose MCP registration is not
///    wired produces confident tool-free narration, which a verifier cannot
///    distinguish from work).
/// 2. `model`'s family must be one this runtime actually serves, checked
///    against the runtime's own `model_prefixes` (not
///    `runtime_for_model(model).id == runtime.id`: catalog order breaks the
///    `gemini-` prefix tie in favour of the Gemini CLI, which would reject the
///    perfectly legal `runtime = "antigravity", model = "gemini-3.7-flash"`).
///    An unknown family is an error, never a guess (goose#10731).
pub(super) fn validate_role_runtime_model(
    role: Role,
    runtime: &str,
    model: &str,
) -> Result<&'static str, String> {
    let runtime_raw = runtime.trim();
    let model_raw = model.trim();
    if model_raw.is_empty() {
        return Err(format!("role '{role}' must declare a model"));
    }
    let spec = duduclaw_core::runtime_catalog::spec_for(runtime_raw)
        .filter(|s| TEAM_ROLE_RUNTIME_ALLOWLIST.contains(&s.id))
        .ok_or_else(|| {
            format!(
                "role '{role}': runtime '{runtime_raw}' cannot back a team role \
                 (allowed: {})",
                TEAM_ROLE_RUNTIME_ALLOWLIST.join(", ")
            )
        })?;
    let lower = model_raw.to_ascii_lowercase();
    let bare = lower.rsplit('/').next().unwrap_or(&lower);
    if !spec.model_prefixes.iter().any(|p| bare.starts_with(*p)) {
        return Err(format!(
            "role '{role}': model '{model_raw}' does not belong to runtime '{}'",
            spec.id
        ));
    }
    Ok(spec.id)
}
