//! O2: Dynamic sub-agent synthesis — ephemeral (Instruction, Context, Tools,
//! Model) four-tuple agents (AOrchestra, arXiv:2602.03786).
//!
//! Instead of delegating only to predefined agents, an orchestrating agent can
//! synthesize a purpose-built *ephemeral* sub-agent: a transient agent
//! directory scaffolded under `<home>/agents/.ephemeral/<eph-id>` with
//! - **Instruction** → `SOUL.md` (the synthesized system-prompt fragment),
//! - **Context** → the task payload dispatched through the existing bus,
//! - **Tools** → `[capabilities] allowed_tools` restricted to the requested
//!   subset (deny-by-default; see [`check_tool_subset`] — requested tools not
//!   inside the *parent* agent's own capability envelope are rejected, so
//!   synthesis can never escalate privileges),
//! - **Model** → a *tier* (`cheap` / `standard` / `preferred`) resolved through
//!   [`crate::delegation_router::tier_model`] against the parent's configured
//!   models (copied verbatim into the scaffold) — callers never pass a raw
//!   model id (multi-model doctrine).
//!
//! The `.ephemeral/` container directory has no `agent.toml`, so the
//! [`duduclaw_agent::registry::AgentRegistry`] scan skips it entirely —
//! ephemeral agents never pollute the registry, the heartbeat scheduler, or
//! the "Your Team" roster. Dispatch happens through the normal bus →
//! `dispatcher::dispatch_to_agent` path, which detects the `eph-` id and
//! routes here ([`dispatch`]); responses flow back through the unchanged
//! delegation-callback path.
//!
//! **Garbage collection** ([`sweep`]): hooked into the dispatcher's existing
//! ~1 hour maintenance tick (the same `tick % 720` slot that runs
//! `cleanup_stale_delegation_callbacks`) — no new scheduler. A scaffold is
//! removed when (a) its `.completed` marker is older than 1 h (grace window
//! for SQLite-queue retries / response forwarding), or (b) it is older than
//! the 24 h TTL regardless of completion. Every deletion first re-verifies
//! that the canonicalized path is strictly contained under the canonicalized
//! `.ephemeral/` root (symlinks are never followed — a symlink entry is
//! unlinked itself, its target untouched), so the sweeper can never delete
//! anything outside its namespace.

use std::path::{Path, PathBuf};

use duduclaw_core::types::CapabilitiesConfig;

use crate::delegation_router::{ModelTier, tier_model};

/// Directory (under `<home>/agents/`) holding ephemeral agent scaffolds.
/// Leading dot + no `agent.toml` inside ⇒ invisible to the registry scan.
pub const EPHEMERAL_DIR_NAME: &str = ".ephemeral";

/// Every ephemeral agent id starts with this prefix.
pub const EPHEMERAL_ID_PREFIX: &str = "eph-";

/// Hard TTL: scaffolds older than this are swept even if never completed.
pub const EPHEMERAL_TTL_HOURS: i64 = 24;

/// Grace window after completion before the scaffold is removed (lets the
/// SQLite stale-message sweeper retry and response forwarding settle).
pub const COMPLETED_GRACE_SECS: i64 = 3600;

/// Circuit breaker: refuse to synthesize beyond this many live scaffolds.
pub const MAX_ACTIVE_EPHEMERAL: usize = 32;

/// Metadata sidecar (`ephemeral.toml`) written next to the scaffold's
/// `agent.toml`. Kept separate from `AgentConfig` so no core-schema change
/// is needed.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EphemeralMeta {
    /// The synthesizing (parent) agent id.
    pub parent: String,
    /// Requested model tier: "cheap" | "standard" | "preferred".
    pub tier: String,
    /// RFC 3339 creation time.
    pub created_at: String,
    /// RFC 3339 expiry (creation + TTL).
    pub expires_at: String,
}

/// Root directory for ephemeral scaffolds.
pub fn ephemeral_root(home_dir: &Path) -> PathBuf {
    home_dir.join("agents").join(EPHEMERAL_DIR_NAME)
}

/// Whether `id` is shaped like an ephemeral agent id (prefix + the same
/// charset rules as every other agent id — no dots, no slashes).
pub fn is_ephemeral_id(id: &str) -> bool {
    id.len() > EPHEMERAL_ID_PREFIX.len()
        && id.starts_with(EPHEMERAL_ID_PREFIX)
        && duduclaw_core::is_valid_agent_id(id)
}

/// Mint a fresh ephemeral agent id (`eph-` + 12 hex chars).
pub fn new_ephemeral_id() -> String {
    let hex = uuid::Uuid::new_v4().simple().to_string();
    // 12 ASCII hex chars — char == byte here, no multi-byte hazard.
    let short: String = hex.chars().take(12).collect();
    format!("{EPHEMERAL_ID_PREFIX}{short}")
}

/// Parse a caller-supplied tier keyword. Only the three tier names are
/// accepted — never a raw model id (multi-model doctrine).
pub fn parse_tier(s: &str) -> Option<ModelTier> {
    match s.trim().to_ascii_lowercase().as_str() {
        "cheap" => Some(ModelTier::Cheap),
        "standard" | "" => Some(ModelTier::Standard),
        "preferred" => Some(ModelTier::Preferred),
        _ => None,
    }
}

/// Charset guard for requested tool names (plain tool names or Claude-style
/// qualified patterns like `mcp__duduclaw__wiki_read` or `Bash(git:*)`).
fn is_valid_tool_name(t: &str) -> bool {
    !t.is_empty()
        && t.chars().count() <= 128
        && t.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '_' | '-' | '(' | ')' | ':' | '*' | ',' | '.' | ' ')
        })
}

/// Fail-closed capability subsetting: every requested tool must sit inside
/// the PARENT agent's own capability envelope.
///
/// Rules (deny wins, ambiguity rejects):
/// 1. Empty request → reject (an ephemeral agent must declare its tool
///    subset explicitly; an empty `allowed_tools` would mean *unrestricted*
///    under [`CapabilitiesConfig`] semantics, which is the opposite of
///    deny-by-default).
/// 2. Any tool in the parent's `denied_tools` → reject.
/// 3. Parent has a non-empty `allowed_tools` allowlist → every requested
///    tool must appear in it (case-insensitive, trimmed).
/// 4. Malformed tool names → reject.
pub fn check_tool_subset(parent: &CapabilitiesConfig, requested: &[String]) -> Result<(), String> {
    if requested.is_empty() {
        return Err("tools must list at least one tool (deny-by-default: an \
                    empty allowlist would mean unrestricted)"
            .to_string());
    }
    let eq_ci = |a: &str, b: &str| a.trim().eq_ignore_ascii_case(b.trim());
    for tool in requested {
        if !is_valid_tool_name(tool) {
            return Err(format!("invalid tool name: {tool:?}"));
        }
        if parent.denied_tools.iter().any(|d| eq_ci(d, tool)) {
            return Err(format!(
                "privilege escalation rejected: tool '{tool}' is in the \
                 parent agent's denied_tools"
            ));
        }
        if !parent.allowed_tools.is_empty() && !parent.allowed_tools.iter().any(|a| eq_ci(a, tool))
        {
            return Err(format!(
                "privilege escalation rejected: tool '{tool}' is not in the \
                 parent agent's allowed_tools"
            ));
        }
    }
    Ok(())
}

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
fn active_count(root: &Path) -> usize {
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
    let parent_dir = home_dir.join("agents").join(&spec.parent);
    let parent_toml_path = parent_dir.join("agent.toml");
    let parent_raw = std::fs::read_to_string(&parent_toml_path).map_err(|e| {
        format!(
            "cannot read parent agent '{}': {e} (fail-closed)",
            spec.parent
        )
    })?;
    let parent_config: duduclaw_core::types::AgentConfig =
        toml::from_str(&parent_raw).map_err(|e| {
            format!(
                "cannot parse parent agent '{}' config: {e} (fail-closed)",
                spec.parent
            )
        })?;
    check_tool_subset(&parent_config.capabilities, &spec.tools)?;

    // ── Circuit breaker ──────────────────────────────────────────────────
    // TOCTOU fix (2026-07 MED): count + create run under the cross-process
    // advisory lock (sidecar `.ephemeral.lock` next to the root, so the lock
    // file is never counted as a scaffold) — parallel spawns from the gateway
    // and MCP-server processes can no longer race past the cap.
    let root = ephemeral_root(home_dir);
    let agent_id = new_ephemeral_id();
    let dir = root.join(&agent_id);
    let created = duduclaw_core::with_file_lock(&root, || {
        if active_count(&root) >= MAX_ACTIVE_EPHEMERAL {
            return Ok(false);
        }
        std::fs::create_dir_all(&dir)?;
        Ok(true)
    })
    .map_err(|e| format!("create scaffold dir: {e}"))?;
    if !created {
        return Err(format!(
            "ephemeral agent limit reached ({MAX_ACTIVE_EPHEMERAL} live scaffolds) — \
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
        toml::Value::String(format!("Ephemeral ({agent_id})")),
    );
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

    // Heartbeat / evolution: hard OFF — an ephemeral agent is a one-task
    // worker, it must never heartbeat, self-evolve, or persist behavior.
    let mut hb_tbl = toml::Table::new();
    hb_tbl.insert("enabled".into(), toml::Value::Boolean(false));
    hb_tbl.insert("interval_seconds".into(), toml::Value::Integer(3600));
    hb_tbl.insert("max_concurrent_runs".into(), toml::Value::Integer(1));
    hb_tbl.insert("cron".into(), toml::Value::String(String::new()));
    table.insert("heartbeat".into(), toml::Value::Table(hb_tbl));

    let mut evo_tbl = toml::Table::new();
    evo_tbl.insert("skill_auto_activate".into(), toml::Value::Boolean(false));
    evo_tbl.insert("skill_security_scan".into(), toml::Value::Boolean(true));
    evo_tbl.insert("gvu_enabled".into(), toml::Value::Boolean(false));
    evo_tbl.insert("cognitive_memory".into(), toml::Value::Boolean(false));
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

    // ── SOUL.md (the Instruction) + metadata sidecar ─────────────────────
    std::fs::write(dir.join("SOUL.md"), &spec.instruction)
        .map_err(|e| format!("write SOUL.md: {e}"))?;

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

    // Audit: creation (existing tool_calls.jsonl convention).
    duduclaw_security::audit::append_tool_call_with_extras(
        home_dir,
        &spec.parent,
        "ephemeral_scaffold",
        &format!(
            "agent_id={agent_id} tier={} tools={}",
            tier.as_str(),
            spec.tools.len()
        ),
        true,
        &[("ephemeral_id", serde_json::Value::String(agent_id.clone()))],
    );
    tracing::info!(
        parent = %spec.parent,
        ephemeral = %agent_id,
        tier = tier.as_str(),
        tools = spec.tools.len(),
        "ephemeral agent scaffolded (O2)"
    );

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

/// Resolve an ephemeral agent id to its scaffold directory — or `None`.
///
/// Containment is proven by canonicalizing both the ephemeral root and the
/// candidate: the canonical candidate must be a strict child of the canonical
/// root (a symlinked scaffold pointing outside resolves outside the root and
/// is rejected). The id charset already forbids `.`/`/` so traversal cannot
/// be encoded in the id, but the canonicalize check makes the guarantee
/// independent of that.
pub fn resolve_agent_dir(home_dir: &Path, agent_id: &str) -> Option<PathBuf> {
    if !is_ephemeral_id(agent_id) {
        return None;
    }
    let root = ephemeral_root(home_dir).canonicalize().ok()?;
    let candidate = root.join(agent_id);
    let canonical = candidate.canonicalize().ok()?;
    if !canonical.starts_with(&root) || canonical == root {
        tracing::warn!(agent = %agent_id, "ephemeral dir escapes namespace — refused");
        return None;
    }
    if !canonical.join("agent.toml").is_file() {
        return None;
    }
    Some(canonical)
}

/// Read the metadata sidecar for a scaffold.
pub fn read_meta(dir: &Path) -> Option<EphemeralMeta> {
    let text = std::fs::read_to_string(dir.join("ephemeral.toml")).ok()?;
    toml::from_str(&text).ok()
}

/// Resolve the tier-appropriate model for a scaffold directory.
///
/// Reads the scaffold's `[model]` (copied verbatim from the parent) and
/// applies [`tier_model`]. Multi-model doctrine: when the resolved runtime
/// provider is NOT Claude, the tier is ignored and the preferred model is
/// returned unchanged (tier models are Claude ids; they must never leak into
/// a codex/gemini runtime).
pub fn resolve_tier_model_for_dir(dir: &Path, tier: ModelTier, preferred: &str) -> String {
    let settings = crate::runtime_config::load_runtime_settings(dir);
    if settings.non_claude_provider().is_some() {
        return preferred.to_string();
    }
    let standard = crate::runtime_config::agent_standard_model(dir);
    tier_model(
        tier,
        preferred,
        standard.as_deref(),
        &settings.utility_model,
    )
}

/// Dispatch a bus task to an ephemeral agent (called from
/// `dispatcher::dispatch_to_agent` when the target id matches the `eph-`
/// namespace). Loads the scaffold from disk (the registry never sees
/// ephemeral agents), resolves the tier model, and runs the normal Claude
/// delegation path via the preloaded-agent entry point.
pub async fn dispatch(
    home_dir: &Path,
    registry: &std::sync::Arc<tokio::sync::RwLock<duduclaw_agent::registry::AgentRegistry>>,
    agent_id: &str,
    prompt: &str,
) -> Result<String, String> {
    let dir = resolve_agent_dir(home_dir, agent_id)
        .ok_or_else(|| format!("ephemeral agent '{agent_id}' not found (expired or swept?)"))?;

    let meta = read_meta(&dir).ok_or_else(|| {
        format!("ephemeral agent '{agent_id}' has no readable metadata (fail-closed)")
    })?;
    if let Ok(expires) = chrono::DateTime::parse_from_rfc3339(&meta.expires_at) {
        if chrono::Utc::now() > expires {
            return Err(format!(
                "ephemeral agent '{agent_id}' expired at {}",
                meta.expires_at
            ));
        }
    }
    let tier = parse_tier(&meta.tier).ok_or_else(|| {
        format!(
            "ephemeral agent '{agent_id}' has invalid tier '{}'",
            meta.tier
        )
    })?;

    let mut loaded = duduclaw_agent::registry::AgentRegistry::load_agent(&dir)
        .await
        .map_err(|e| format!("load ephemeral agent '{agent_id}': {e}"))?;

    let model = resolve_tier_model_for_dir(&dir, tier, &loaded.config.model.preferred);
    tracing::info!(
        ephemeral = %agent_id,
        parent = %meta.parent,
        tier = tier.as_str(),
        model = %model,
        "dispatching ephemeral agent (O2)"
    );
    loaded.config.model.preferred = model;

    let result = crate::claude_runner::call_claude_for_agent_preloaded(
        home_dir,
        registry,
        &loaded,
        prompt,
        crate::cost_telemetry::RequestType::Dispatch,
    )
    .await;

    // Mark completed (success OR failure) so the GC grace clock starts.
    let marker = dir.join(".completed");
    if let Err(e) = std::fs::write(&marker, chrono::Utc::now().to_rfc3339()) {
        tracing::warn!(ephemeral = %agent_id, error = %e, "failed to write .completed marker");
    }

    result
}

/// Whether a scaffold directory is due for removal under the GC policy.
/// Pure decision function (testable without touching the filesystem clock):
/// `now` is injected.
pub fn is_due_for_gc(
    meta: Option<&EphemeralMeta>,
    completed_at: Option<chrono::DateTime<chrono::Utc>>,
    dir_modified: Option<std::time::SystemTime>,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    // (a) completed + grace elapsed
    if let Some(done) = completed_at {
        if now - done >= chrono::Duration::seconds(COMPLETED_GRACE_SECS) {
            return true;
        }
    }
    // (b) hard TTL from metadata created_at
    if let Some(m) = meta {
        if let Ok(created) = chrono::DateTime::parse_from_rfc3339(&m.created_at) {
            return now - created.with_timezone(&chrono::Utc)
                >= chrono::Duration::hours(EPHEMERAL_TTL_HOURS);
        }
    }
    // (c) metadata unreadable → fall back to directory mtime for the TTL.
    if let Some(mtime) = dir_modified {
        let mtime: chrono::DateTime<chrono::Utc> = mtime.into();
        return now - mtime >= chrono::Duration::hours(EPHEMERAL_TTL_HOURS);
    }
    false
}

/// GC sweep — called from the dispatcher's hourly maintenance tick.
///
/// Deletion safety (the containment invariant, tested below):
/// 1. Symlink entries are unlinked (`remove_file`) without following — their
///    targets are never touched.
/// 2. Real directories are canonicalized and must remain strict children of
///    the canonicalized ephemeral root before `remove_dir_all` runs.
///
/// Returns the number of scaffolds removed.
pub async fn sweep(home_dir: &Path) -> usize {
    let home = home_dir.to_path_buf();
    tokio::task::spawn_blocking(move || sweep_blocking(&home))
        .await
        .unwrap_or(0)
}

fn sweep_blocking(home_dir: &Path) -> usize {
    let root = ephemeral_root(home_dir);
    let Ok(canonical_root) = root.canonicalize() else {
        return 0; // no ephemeral namespace yet — nothing to do
    };
    let Ok(entries) = std::fs::read_dir(&canonical_root) else {
        return 0;
    };
    let now = chrono::Utc::now();
    let mut removed = 0usize;

    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };

        // Symlinks: never follow. An expired symlink entry is unlinked
        // itself; its target is out of our jurisdiction.
        let Ok(link_meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if link_meta.file_type().is_symlink() {
            // A symlink has no scaffold metadata — treat as foreign junk and
            // unlink only when older than TTL (by link mtime).
            let old_enough = link_meta
                .modified()
                .ok()
                .map(|m| {
                    let m: chrono::DateTime<chrono::Utc> = m.into();
                    now - m >= chrono::Duration::hours(EPHEMERAL_TTL_HOURS)
                })
                .unwrap_or(false);
            if old_enough && std::fs::remove_file(&path).is_ok() {
                tracing::warn!(entry = %name, "removed stale symlink from ephemeral namespace (target untouched)");
            }
            continue;
        }
        if !link_meta.is_dir() {
            continue; // stray files are left alone
        }

        let meta = read_meta(&path);
        let completed_at = std::fs::read_to_string(path.join(".completed"))
            .ok()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s.trim()).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc));
        let dir_modified = link_meta.modified().ok();

        if !is_due_for_gc(meta.as_ref(), completed_at, dir_modified, now) {
            continue;
        }

        // Containment re-verification immediately before deletion.
        let Ok(canonical) = path.canonicalize() else {
            continue;
        };
        if !canonical.starts_with(&canonical_root) || canonical == canonical_root {
            tracing::warn!(entry = %name, "GC candidate escapes ephemeral namespace — refused");
            continue;
        }

        match std::fs::remove_dir_all(&canonical) {
            Ok(()) => {
                removed += 1;
                let parent = meta
                    .as_ref()
                    .map(|m| m.parent.as_str())
                    .unwrap_or("unknown");
                duduclaw_security::audit::append_tool_call_with_extras(
                    home_dir,
                    parent,
                    "ephemeral_teardown",
                    &format!("agent_id={name}"),
                    true,
                    &[("ephemeral_id", serde_json::Value::String(name.clone()))],
                );
                tracing::info!(ephemeral = %name, "ephemeral agent scaffold garbage-collected (O2)");
            }
            Err(e) => {
                tracing::warn!(ephemeral = %name, error = %e, "ephemeral GC removal failed");
            }
        }
    }
    removed
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(allowed: &[&str], denied: &[&str]) -> CapabilitiesConfig {
        let mut c = CapabilitiesConfig::default();
        c.allowed_tools = allowed.iter().map(|s| s.to_string()).collect();
        c.denied_tools = denied.iter().map(|s| s.to_string()).collect();
        c
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn write_parent(home: &Path, name: &str, extra: &str) {
        let dir = home.join("agents").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("agent.toml"),
            format!(
                r#"[agent]
name = "{name}"
display_name = "{name}"
role = "specialist"
status = "active"
trigger = "@{name}"
reports_to = ""
icon = "X"

[model]
preferred = "parent-preferred-model"
fallback = ""
account_pool = []
utility = "parent-utility-model"
standard = "parent-standard-model"

[budget]
monthly_limit_cents = 1000
warn_threshold_percent = 80
hard_stop = false

[container]
sandbox_enabled = false
network_access = false
timeout_ms = 60000
max_concurrent = 2
readonly_project = false
additional_mounts = []

[heartbeat]
enabled = false
interval_seconds = 300
max_concurrent_runs = 1
cron = ""

[permissions]
can_create_agents = false
can_send_cross_agent = true
can_modify_own_skills = false
can_modify_own_soul = false
can_schedule_tasks = false
allowed_channels = []

[evolution]
skill_auto_activate = false
skill_security_scan = false

{extra}
"#
            ),
        )
        .unwrap();
    }

    // ── Privilege escalation (fail-closed subsetting) ────────────────────

    #[test]
    fn subset_rejects_tool_outside_parent_allowlist() {
        let parent = caps(&["Read", "Grep"], &[]);
        let err = check_tool_subset(&parent, &strs(&["Read", "Bash"])).unwrap_err();
        assert!(err.contains("privilege escalation"), "got: {err}");
    }

    #[test]
    fn subset_rejects_parent_denied_tool_even_when_parent_unrestricted() {
        let parent = caps(&[], &["Bash"]);
        let err = check_tool_subset(&parent, &strs(&["Bash"])).unwrap_err();
        assert!(err.contains("denied_tools"), "got: {err}");
    }

    #[test]
    fn subset_accepts_strict_subset_case_insensitive() {
        let parent = caps(&["Read", "Grep", "WebFetch"], &[]);
        assert!(check_tool_subset(&parent, &strs(&["read", "grep"])).is_ok());
    }

    #[test]
    fn subset_accepts_any_tool_when_parent_unrestricted() {
        let parent = caps(&[], &[]);
        assert!(check_tool_subset(&parent, &strs(&["Read", "Bash(git:*)"])).is_ok());
    }

    #[test]
    fn subset_rejects_empty_request_deny_by_default() {
        let parent = caps(&[], &[]);
        assert!(check_tool_subset(&parent, &[]).is_err());
    }

    #[test]
    fn subset_rejects_malformed_tool_name() {
        let parent = caps(&[], &[]);
        assert!(check_tool_subset(&parent, &strs(&["evil;rm -rf /"])).is_err());
        assert!(check_tool_subset(&parent, &strs(&["../escape"])).is_err());
    }

    // ── Scaffold ──────────────────────────────────────────────────────────

    #[test]
    fn scaffold_creates_contained_dir_with_valid_agent_config() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        write_parent(
            home,
            "boss",
            "[capabilities]\nallowed_tools = [\"Read\", \"Grep\"]\n",
        );

        let spec = EphemeralSpawnSpec {
            parent: "boss".into(),
            instruction: "You summarize logs. 只做摘要。".into(),
            tools: strs(&["Read"]),
            tier: "cheap".into(),
        };
        let result = scaffold(home, &spec).unwrap();

        assert!(is_ephemeral_id(&result.agent_id));
        // Dir is under the ephemeral root.
        assert!(result.dir.starts_with(ephemeral_root(home)));
        // agent.toml parses as a full AgentConfig with the restricted subset.
        let cfg: duduclaw_core::types::AgentConfig =
            toml::from_str(&std::fs::read_to_string(result.dir.join("agent.toml")).unwrap())
                .unwrap();
        assert_eq!(cfg.agent.reports_to, "boss");
        assert_eq!(cfg.capabilities.allowed_tools, vec!["Read".to_string()]);
        // Parent's model config copied verbatim — no hardcoded ids injected.
        assert_eq!(cfg.model.preferred, "parent-preferred-model");
        // SOUL.md carries the instruction (CJK-safe write).
        let soul = std::fs::read_to_string(result.dir.join("SOUL.md")).unwrap();
        assert!(soul.contains("只做摘要"));
        // Metadata sidecar records tier + parent.
        let meta = read_meta(&result.dir).unwrap();
        assert_eq!(meta.parent, "boss");
        assert_eq!(meta.tier, "cheap");
        // Resolvable through the containment-checked resolver.
        assert!(resolve_agent_dir(home, &result.agent_id).is_some());
    }

    #[test]
    fn scaffold_rejects_escalation_and_leaves_no_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        write_parent(home, "boss", "[capabilities]\nallowed_tools = [\"Read\"]\n");

        let spec = EphemeralSpawnSpec {
            parent: "boss".into(),
            instruction: "x".into(),
            tools: strs(&["Read", "Bash"]),
            tier: "standard".into(),
        };
        let err = scaffold(home, &spec).unwrap_err();
        assert!(err.contains("privilege escalation"), "got: {err}");
        // No scaffold left behind.
        let root = ephemeral_root(home);
        assert!(
            !root.exists() || std::fs::read_dir(&root).unwrap().next().is_none(),
            "escalation attempt must not scaffold anything"
        );
    }

    #[test]
    fn scaffold_fails_closed_when_parent_config_missing_or_malformed() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let spec = EphemeralSpawnSpec {
            parent: "ghost".into(),
            instruction: "x".into(),
            tools: strs(&["Read"]),
            tier: "standard".into(),
        };
        // Missing parent → reject.
        assert!(scaffold(home, &spec).is_err());
        // Malformed parent → reject.
        let dir = home.join("agents").join("ghost");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), "not [valid toml").unwrap();
        assert!(scaffold(home, &spec).is_err());
    }

    #[test]
    fn scaffold_cap_is_race_safe_under_parallel_spawns() {
        // 2026-07 MED: the count-then-create circuit breaker used to be a
        // TOCTOU race — N parallel spawns could all observe count < cap and
        // overshoot. Now count+create hold the advisory lock.
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        write_parent(&home, "boss", "");

        let attempts = MAX_ACTIVE_EPHEMERAL + 8;
        let handles: Vec<_> = (0..attempts)
            .map(|i| {
                let home = home.clone();
                std::thread::spawn(move || {
                    scaffold(
                        &home,
                        &EphemeralSpawnSpec {
                            parent: "boss".into(),
                            instruction: format!("worker {i}"),
                            tools: vec!["Read".to_string()],
                            tier: "standard".into(),
                        },
                    )
                    .is_ok()
                })
            })
            .collect();
        let succeeded = handles
            .into_iter()
            .filter_map(|h| h.join().ok())
            .filter(|ok| *ok)
            .count();

        assert_eq!(
            succeeded, MAX_ACTIVE_EPHEMERAL,
            "exactly the cap may succeed — no overshoot, no undershoot"
        );
        let live = std::fs::read_dir(ephemeral_root(&home))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .count();
        assert_eq!(live, MAX_ACTIVE_EPHEMERAL, "scaffold count must equal the cap");
    }

    #[test]
    fn scaffold_rejects_raw_model_id_as_tier() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        write_parent(home, "boss", "");
        let spec = EphemeralSpawnSpec {
            parent: "boss".into(),
            instruction: "x".into(),
            tools: strs(&["Read"]),
            tier: "claude-opus-4-5".into(), // a model id is NOT a tier
        };
        let err = scaffold(home, &spec).unwrap_err();
        assert!(err.contains("invalid tier"), "got: {err}");
    }

    // ── Tier → model resolution (no hardcoded ids) ────────────────────────

    #[test]
    fn tier_resolution_reads_models_from_scaffolded_config_only() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        write_parent(home, "boss", "");
        let spec = EphemeralSpawnSpec {
            parent: "boss".into(),
            instruction: "x".into(),
            tools: strs(&["Read"]),
            tier: "cheap".into(),
        };
        let result = scaffold(home, &spec).unwrap();

        // All three tiers resolve to the PARENT-configured strings — values
        // this test invented, proving nothing is hardcoded in the resolver.
        assert_eq!(
            resolve_tier_model_for_dir(&result.dir, ModelTier::Cheap, "parent-preferred-model"),
            "parent-utility-model"
        );
        assert_eq!(
            resolve_tier_model_for_dir(&result.dir, ModelTier::Standard, "parent-preferred-model"),
            "parent-standard-model"
        );
        assert_eq!(
            resolve_tier_model_for_dir(&result.dir, ModelTier::Preferred, "parent-preferred-model"),
            "parent-preferred-model"
        );
    }

    #[test]
    fn tier_resolution_ignores_tier_for_non_claude_provider() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        write_parent(home, "boss", "[runtime]\nprovider = \"codex\"\n");
        let spec = EphemeralSpawnSpec {
            parent: "boss".into(),
            instruction: "x".into(),
            tools: strs(&["Read"]),
            tier: "cheap".into(),
        };
        let result = scaffold(home, &spec).unwrap();
        // Multi-model doctrine: codex agent keeps its own preferred model —
        // the (Claude) utility tier must NOT leak in.
        assert_eq!(
            resolve_tier_model_for_dir(&result.dir, ModelTier::Cheap, "gpt-x-parent"),
            "gpt-x-parent"
        );
    }

    // ── GC policy + containment ───────────────────────────────────────────

    #[test]
    fn gc_decision_completed_grace_and_ttl() {
        let now = chrono::Utc::now();
        let meta = EphemeralMeta {
            parent: "boss".into(),
            tier: "standard".into(),
            created_at: (now - chrono::Duration::hours(2)).to_rfc3339(),
            expires_at: (now + chrono::Duration::hours(22)).to_rfc3339(),
        };
        // Fresh, not completed → keep.
        assert!(!is_due_for_gc(Some(&meta), None, None, now));
        // Completed 5 min ago → still in grace → keep.
        assert!(!is_due_for_gc(
            Some(&meta),
            Some(now - chrono::Duration::minutes(5)),
            None,
            now
        ));
        // Completed 2h ago → grace elapsed → remove.
        assert!(is_due_for_gc(
            Some(&meta),
            Some(now - chrono::Duration::hours(2)),
            None,
            now
        ));
        // Never completed but past 24h TTL → remove.
        let old_meta = EphemeralMeta {
            created_at: (now - chrono::Duration::hours(25)).to_rfc3339(),
            ..meta.clone()
        };
        assert!(is_due_for_gc(Some(&old_meta), None, None, now));
        // No metadata → dir mtime decides.
        let old_mtime = std::time::SystemTime::now() - std::time::Duration::from_secs(25 * 3600);
        assert!(is_due_for_gc(None, None, Some(old_mtime), now));
        assert!(!is_due_for_gc(
            None,
            None,
            Some(std::time::SystemTime::now()),
            now
        ));
    }

    #[tokio::test]
    async fn sweep_removes_expired_keeps_fresh_and_never_escapes() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        write_parent(home, "boss", "");

        // Fresh scaffold — must survive.
        let fresh = scaffold(
            home,
            &EphemeralSpawnSpec {
                parent: "boss".into(),
                instruction: "fresh".into(),
                tools: strs(&["Read"]),
                tier: "standard".into(),
            },
        )
        .unwrap();

        // Expired scaffold — completed 2h ago.
        let expired = scaffold(
            home,
            &EphemeralSpawnSpec {
                parent: "boss".into(),
                instruction: "old".into(),
                tools: strs(&["Read"]),
                tier: "standard".into(),
            },
        )
        .unwrap();
        std::fs::write(
            expired.dir.join(".completed"),
            (chrono::Utc::now() - chrono::Duration::hours(2)).to_rfc3339(),
        )
        .unwrap();

        // Outside directory that a malicious/buggy symlink points at.
        let outside = home.join("precious-data");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep.txt"), "do not delete").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, ephemeral_root(home).join("eph-evil-link")).unwrap();

        let removed = sweep(home).await;

        assert_eq!(removed, 1, "exactly the expired scaffold is removed");
        assert!(!expired.dir.exists(), "expired scaffold swept");
        assert!(fresh.dir.exists(), "fresh scaffold kept");
        // Containment: the symlink target must be untouched (the fresh link
        // itself is also kept — it only gets unlinked after TTL).
        assert!(
            outside.join("keep.txt").exists(),
            "sweep must NEVER delete outside the ephemeral namespace"
        );
    }

    #[test]
    fn resolver_rejects_symlinked_escape_and_foreign_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let root = ephemeral_root(home);
        std::fs::create_dir_all(&root).unwrap();

        // Non-ephemeral ids never resolve.
        assert!(resolve_agent_dir(home, "boss").is_none());
        assert!(resolve_agent_dir(home, "eph-../../etc").is_none()); // charset reject
        assert!(resolve_agent_dir(home, "eph-missing12345").is_none());

        // A symlink inside the namespace pointing outside must not resolve.
        #[cfg(unix)]
        {
            let outside = home.join("outside-agent");
            std::fs::create_dir_all(&outside).unwrap();
            std::fs::write(outside.join("agent.toml"), "").unwrap();
            std::os::unix::fs::symlink(&outside, root.join("eph-escape000000")).unwrap();
            assert!(
                resolve_agent_dir(home, "eph-escape000000").is_none(),
                "symlinked escape must not resolve"
            );
        }
    }

    #[test]
    fn ephemeral_ids_are_valid_agent_ids() {
        for _ in 0..8 {
            let id = new_ephemeral_id();
            assert!(is_ephemeral_id(&id), "{id}");
            assert!(duduclaw_core::is_valid_agent_id(&id), "{id}");
        }
        assert!(!is_ephemeral_id("worker"));
        assert!(!is_ephemeral_id("eph-"));
        assert!(is_ephemeral_id("eph-abc123"));
    }

    #[test]
    fn parse_tier_accepts_only_tier_keywords() {
        assert_eq!(parse_tier("cheap"), Some(ModelTier::Cheap));
        assert_eq!(parse_tier("Standard"), Some(ModelTier::Standard));
        assert_eq!(parse_tier("PREFERRED"), Some(ModelTier::Preferred));
        assert_eq!(parse_tier(""), Some(ModelTier::Standard));
        assert_eq!(parse_tier("claude-opus-4-5"), None);
        assert_eq!(parse_tier("gpt-5"), None);
    }
}
