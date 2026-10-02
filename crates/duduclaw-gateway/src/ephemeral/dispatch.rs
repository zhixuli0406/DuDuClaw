//! Resolving an ephemeral id to its directory and running one turn in it.
//! Moved verbatim out of `ephemeral.rs`.

use super::*;

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
///
/// **Role members: raw wins over tier.** A scaffold carrying a `[team_member]`
/// section had its `[model] preferred` written by the composer from a validated
/// `[team.roles.*]` assignment — that id *is* the answer, and the member's own
/// `agent.toml` is authoritative. Without this short-circuit a Claude-family
/// role member would have its explicit model replaced by whatever
/// [`tier_model`] resolves (the tier a role member carries is inert filler),
/// silently collapsing a four-vendor team back onto one lineup.
///
/// `preferred` is the caller's already-loaded `[model] preferred` for this same
/// directory ([`dispatch`] passes `loaded.config.model.preferred`), so returning
/// it verbatim returns the member's own model — not the parent's, which is what
/// the copied-then-overridden table used to leave behind on the non-Claude path.
pub fn resolve_tier_model_for_dir(dir: &Path, tier: ModelTier, preferred: &str) -> String {
    if read_role_member(dir).is_some() {
        return preferred.to_string();
    }
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
    dispatch_with(
        home_dir,
        registry,
        agent_id,
        prompt,
        crate::claude_runner::DispatchOverrides::default(),
    )
    .await
}

/// [`dispatch`] with caller-imposed
/// [`crate::claude_runner::DispatchOverrides`].
///
/// The team composer is the only caller that passes non-default overrides: a
/// role member runs with cwd = the employee's workspace (so its files outlive
/// the immediately-GC'd scaffold), with `--mcp-config` pinned to its own
/// `.mcp.json` (so moving the cwd does not move its identity), and with
/// cross-family failover refused (so a codex executor can never be silently
/// answered by Claude). Design `DESIGN-team-as-agent-2026-09.md` §4.3 E2/E3.
pub async fn dispatch_with(
    home_dir: &Path,
    registry: &std::sync::Arc<tokio::sync::RwLock<duduclaw_agent::registry::AgentRegistry>>,
    agent_id: &str,
    prompt: &str,
    overrides: crate::claude_runner::DispatchOverrides,
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

    // The scaffold copied the parent's `[container]` verbatim, so this is the
    // parent employee's sandbox flag. An ephemeral run is not sandboxed; say
    // so (once per parent per process) instead of running silently.
    crate::task_sandbox::note_not_applied(
        home_dir,
        &meta.parent,
        loaded.config.container.sandbox_enabled,
        crate::task_sandbox::HostPath::Ephemeral,
        crate::task_sandbox::HostAction::RanOnHost,
    );

    let result = crate::claude_runner::call_claude_for_agent_preloaded_with(
        home_dir,
        registry,
        &loaded,
        prompt,
        crate::cost_telemetry::RequestType::Dispatch,
        overrides,
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
