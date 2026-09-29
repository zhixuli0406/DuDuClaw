//! Team role members — the composer-only spawn variant: scaffold, marker
//! reads, admission and the bus payload codec.
//! Moved verbatim out of `ephemeral.rs`.

use super::*;

/// Scaffold one **team role member** — the composer-only entry point.
///
/// Reuses every gate [`scaffold`] applies (parent fail-closed parse,
/// [`check_tool_subset`] containment, instruction cap, the advisory-locked
/// count+create against `[dispatch] ephemeral_max_active`, verbatim copy of the
/// employee's `[model]/[runtime]/[container]/[budget]`), then overrides exactly
/// four things in the member's own `agent.toml`.
///
/// The one containment difference from [`scaffold`]:
/// [`TEAM_INTRINSIC_TOOLS`] are permitted even when the employee's
/// `allowed_tools` does not list them (see that constant for why the list is
/// one entry long). Every other tool, and the employee's `denied_tools` veto,
/// behave exactly as they do for an ordinary ephemeral.
///
/// The overrides:
///
/// | key | value |
/// |---|---|
/// | `[runtime] provider` | the canonical runtime id (alias resolved) |
/// | `[model] preferred` | the role's raw model id |
/// | `[model] effort` | the role's effort — **written only when `Some`** |
/// | `[team_member] role / task_id / round / parent` | the assignment record |
///
/// `[runtime] fallback` inherited from the employee is dropped: silently moving
/// a role to another vendor on first failure would erase the executor/verifier
/// decorrelation the team exists for.
///
/// **Not reachable from MCP.** The `spawn_ephemeral` tool keeps rejecting raw
/// model ids via [`parse_tier`], so an agent can never choose its own model;
/// only the gateway composer reaches this function. Over-capacity returns the
/// same hard [`EPHEMERAL_CAPACITY_ERROR_PREFIX`] error `scaffold` does — use
/// [`admit_role_member`] to get the queue-instead-of-fail behavior.
pub fn scaffold_role_member(
    home_dir: &Path,
    spec: &RoleMemberSpec,
) -> Result<ScaffoldResult, String> {
    // Validate BEFORE anything is created: a rejected assignment must leave no
    // directory, no queue ticket and no audit row claiming a scaffold exists.
    let runtime = validate_role_runtime_model(spec.role, &spec.runtime, &spec.model)?;
    if spec.task_id.trim().is_empty() {
        return Err("role member must carry a task_id".to_string());
    }
    let agent_id = new_role_member_id(&spec.parent_agent, spec.round, spec.role)?;

    // Canonicalised runtime + trimmed model are what get written — never the
    // caller's spelling.
    let normalized = RoleMemberSpec {
        runtime: runtime.to_string(),
        model: spec.model.trim().to_string(),
        ..spec.clone()
    };
    let eph_spec = EphemeralSpawnSpec {
        parent: spec.parent_agent.clone(),
        instruction: spec.instruction.clone(),
        tools: spec.tools.clone(),
        // Inert for a role member: `resolve_tier_model_for_dir` short-circuits
        // on the `[team_member]` section and returns the raw `[model] preferred`
        // instead of resolving a tier. "standard" is written only so `dispatch`
        // (which still parses the sidecar's tier) keeps working unchanged.
        tier: ModelTier::Standard.as_str().to_string(),
    };
    scaffold_with(
        home_dir,
        &eph_spec,
        ScaffoldPlan {
            agent_id: Some(agent_id),
            role: Some(&normalized),
        },
    )
}

/// Read a scaffold's `[team_member]` section — `None` for an ordinary
/// ephemeral agent (or any scaffold whose section is absent / malformed).
///
/// Lenient by design: a wrong-typed field yields `None` (treat it as an
/// ordinary ephemeral) rather than an error, matching
/// [`duduclaw_core::effort::read_agent_effort`]. The consequence of `None` is
/// always the *safer* branch — tier resolution instead of a raw model, and the
/// ordinary grace-window GC instead of immediate removal.
pub fn read_role_member(dir: &Path) -> Option<RoleMemberRecord> {
    let raw = std::fs::read_to_string(dir.join("agent.toml")).ok()?;
    let value: toml::Value = raw.parse().ok()?;
    let section = value.get(ROLE_MEMBER_SECTION)?;
    let role: Role = section.get("role")?.as_str()?.parse().ok()?;
    let task_id = section.get("task_id")?.as_str()?.to_string();
    let round = u32::try_from(section.get("round")?.as_integer()?).ok()?;
    let parent = section.get("parent")?.as_str()?.to_string();
    Some(RoleMemberRecord {
        role,
        task_id,
        round,
        parent,
    })
}

/// Does this scaffold directory carry the WP-2 `[team_member]` marker at all?
///
/// Deliberately weaker than [`read_role_member`]: only the section's *presence*
/// is checked, not that every field inside it parses. Callers use this to
/// decide whether to **withhold** a side effect from a throwaway scaffold
/// (memory distillation, for one), and for that question a malformed marker
/// must still read as "this is a role member" — fail-closed toward
/// withholding. `read_role_member` fails the other way (open, toward the
/// ordinary-ephemeral branch) because its consumers need a complete record
/// before they can act on it at all.
pub fn has_role_member_marker(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join("agent.toml"))
        .ok()
        .and_then(|raw| raw.parse::<toml::Value>().ok())
        .is_some_and(|v| v.get(ROLE_MEMBER_SECTION).is_some())
}

/// [`has_role_member_marker`] addressed by agent id.
///
/// Cheap for an ordinary agent: [`resolve_agent_dir`] rejects a non-ephemeral
/// id on the id shape alone, so no filesystem access happens at all.
pub fn is_role_member(home_dir: &Path, agent_id: &str) -> bool {
    resolve_agent_dir(home_dir, agent_id).is_some_and(|dir| has_role_member_marker(&dir))
}

/// Tear a role member down the moment its round reaches a terminal state:
/// writes the `.completed` marker **and** removes the scaffold immediately —
/// no 1 h grace, no waiting for the 24 h TTL.
///
/// Why immediate (design §3.8, fix ①): a team scaffolds one member per role per
/// round, so the default goal-loop shape (3 roles × 5 rounds × 3 concurrent
/// tasks) reaches 45 live scaffolds against a default `ephemeral_max_active` of
/// 32. Under the grace-window policy the overflow would queue and then expire,
/// costing rounds a role for reasons nothing in the logs would connect to GC.
///
/// The `.completed` marker is written *before* removal on purpose: if removal
/// then fails (a busy directory, a permission drift), the ordinary [`sweep`]
/// still collects the scaffold — degraded to "an hour late", never "never".
///
/// Safety:
/// * the id must be [`is_ephemeral_id`]-shaped, and the directory is resolved
///   through [`resolve_agent_dir`], which canonicalizes and requires strict
///   containment under `.ephemeral/` — a symlinked scaffold pointing outside
///   resolves outside the root and is refused;
/// * containment is re-verified immediately before `remove_dir_all` (same
///   discipline as [`sweep`]);
/// * the scaffold must actually carry a `[team_member]` section. This is the
///   load-bearing part: an ordinary ephemeral agent keeps its grace window, so
///   this entry can never be used to delete one early.
///
/// Idempotent: a member already gone (a second terminal event, a GC that beat
/// us to it) returns `Ok(())`. Cost rows already written stay — they live in
/// SQLite under the member id and fold onto the employee at report time.
pub fn finish_role_member(
    home_dir: &Path,
    member_id: &str,
    outcome: RoleMemberOutcome,
) -> Result<(), String> {
    if !is_ephemeral_id(member_id) {
        return Err(format!("not an ephemeral agent id: {member_id:?}"));
    }
    let Some(dir) = resolve_agent_dir(home_dir, member_id) else {
        // Already swept, never existed, or escapes the namespace —
        // `resolve_agent_dir` has already logged the escape case.
        return Ok(());
    };
    let Some(record) = read_role_member(&dir) else {
        return Err(format!(
            "'{member_id}' is not a team role member (no [{ROLE_MEMBER_SECTION}] section) — \
             refusing to bypass the ordinary ephemeral GC grace window"
        ));
    };

    // Marker first: if the removal below fails, `sweep` still collects it.
    let marker = dir.join(".completed");
    if let Err(e) = std::fs::write(&marker, chrono::Utc::now().to_rfc3339()) {
        tracing::warn!(member = %member_id, error = %e, "failed to write .completed marker");
    }

    // Containment re-verification immediately before deletion.
    let root = ephemeral_root(home_dir)
        .canonicalize()
        .map_err(|e| format!("ephemeral root unavailable: {e}"))?;
    let canonical = dir
        .canonicalize()
        .map_err(|e| format!("cannot canonicalize role member dir: {e}"))?;
    if !canonical.starts_with(&root) || canonical == root {
        return Err(format!(
            "role member '{member_id}' escapes the ephemeral namespace — refused"
        ));
    }
    std::fs::remove_dir_all(&canonical)
        .map_err(|e| format!("remove role member scaffold '{member_id}': {e}"))?;

    // Live round 3 (design §4.3 E3) saw torn-down member directories come
    // back, because members worked in their own scaffold and later writes
    // recreated the tree. Members now work in the employee's workspace, so a
    // reappearance means something is still writing into a dead scaffold —
    // worth a line in the log rather than a silent second directory that only
    // the 24 h TTL sweep will notice. Not an error: the scaffold IS gone as
    // far as this call is concerned, and a second `finish_role_member` for
    // the same member is a no-op by the `resolve_agent_dir` guard above.
    if canonical.exists() {
        tracing::warn!(
            member = %member_id,
            path = %canonical.display(),
            "role member scaffold reappeared immediately after teardown — something is still writing into a torn-down member directory"
        );
    }

    // The authoritative org record dies with the directory (same ordering as
    // `sweep` and MCP `agent_remove`).
    if let Err(e) = duduclaw_core::org_store::remove(home_dir, member_id) {
        tracing::warn!(
            member = %member_id,
            error = %e,
            "org.toml removal failed during role member teardown"
        );
    }
    duduclaw_security::audit::append_tool_call_with_extras(
        home_dir,
        &record.parent,
        "role_member_teardown",
        &format!(
            "agent_id={member_id} role={} task_id={} round={} outcome={outcome}",
            record.role.as_str(),
            record.task_id,
            record.round
        ),
        true,
        &[
            (
                "ephemeral_id",
                serde_json::Value::String(member_id.to_string()),
            ),
            (
                "role",
                serde_json::Value::String(record.role.as_str().into()),
            ),
            ("task_id", serde_json::Value::String(record.task_id.clone())),
            ("round", serde_json::Value::from(record.round)),
            (
                "outcome",
                serde_json::Value::String(outcome.as_str().into()),
            ),
        ],
    );
    tracing::info!(
        member = %member_id,
        parent = %record.parent,
        role = record.role.as_str(),
        task_id = %record.task_id,
        round = record.round,
        outcome = outcome.as_str(),
        "team role member torn down immediately (WP-2)"
    );
    Ok(())
}

/// Outcome of [`admit_role_member`].
#[derive(Debug, Clone)]
pub enum RoleMemberAdmitted {
    /// Scaffolded and ready to dispatch.
    Scaffolded(ScaffoldResult),
    /// Over `ephemeral_max_active` right now, durably queued instead.
    /// `position` is the 1-based FIFO rank. **The composer owns replay** — see
    /// [`duduclaw_core::spawn_admission::ROLE_TEAM_ADMISSION_CLASS`]; the hourly
    /// ephemeral drain deliberately does not touch this class, because a role
    /// member belongs to one live round and replaying it later as a plain
    /// ephemeral would silently drop its whole model assignment.
    Queued { ticket_id: String, position: u32 },
}

/// Admission-aware [`scaffold_role_member`]: queue instead of hard-failing when
/// the ephemeral cap is already reached.
///
/// This is the gateway-internal counterpart to the `spawn_ephemeral` MCP tool's
/// queue-vs-fail branch. `scaffold`'s own contract is untouched — it stays a
/// synchronous, TOCTOU-safe hard `Err` at the cap (see its circuit-breaker
/// comment); the queue decision lives here, one layer up, exactly as it does
/// for the MCP path.
///
/// `owner_key` should scope the ticket to the round that wants the member (e.g.
/// `"<task_id>#<round>"`) so
/// [`duduclaw_core::spawn_admission::invalidate_role_members`] can purge it the
/// moment that round reaches a terminal state — a queued member whose round is
/// gone has nobody left to consume its answer.
///
/// Validation errors (bad runtime/model pair, privilege escalation, malformed
/// parent) are returned as `Err` and are **never** queued: retrying them can
/// never succeed.
pub fn admit_role_member(
    home_dir: &Path,
    spec: &RoleMemberSpec,
    owner_key: Option<&str>,
) -> Result<RoleMemberAdmitted, String> {
    match scaffold_role_member(home_dir, spec) {
        Ok(result) => Ok(RoleMemberAdmitted::Scaffolded(result)),
        Err(e) if e.starts_with(EPHEMERAL_CAPACITY_ERROR_PREFIX) => {
            // Anchored prefix match (project convention #2), never a substring
            // scan of arbitrary error text: only "temporarily out of capacity"
            // may be deferred.
            let cfg = duduclaw_core::spawn_admission::AdmissionConfig::from_home(home_dir);
            let payload = role_member_payload(spec);
            match duduclaw_core::spawn_admission::enqueue_role_member(
                home_dir, &cfg, owner_key, payload,
            ) {
                Ok(duduclaw_core::spawn_admission::EnqueueOutcome::Queued {
                    ticket_id,
                    position,
                }) => {
                    tracing::info!(
                        parent = %spec.parent_agent,
                        role = spec.role.as_str(),
                        task_id = %spec.task_id,
                        round = spec.round,
                        ticket = %ticket_id,
                        position,
                        "WP-2: role member over capacity — durably queued"
                    );
                    Ok(RoleMemberAdmitted::Queued {
                        ticket_id,
                        position,
                    })
                }
                Ok(duduclaw_core::spawn_admission::EnqueueOutcome::Rejected { reason }) => {
                    Err(format!("{e}; and the admission queue refused it: {reason}"))
                }
                Err(queue_err) => Err(format!("{e}; and queuing failed: {queue_err}")),
            }
        }
        Err(e) => Err(e),
    }
}

/// Serialize a [`RoleMemberSpec`] into an admission-queue payload.
///
/// Hand-built rather than `derive(Serialize)`: `Effort` is deliberately not a
/// serde type (WP-3), and an explicit shape here is what a replaying composer
/// reads back — keeping the two sides of the queue in one visible place.
pub(super) fn role_member_payload(spec: &RoleMemberSpec) -> serde_json::Value {
    serde_json::json!({
        "parent_agent": spec.parent_agent,
        "task_id": spec.task_id,
        "round": spec.round,
        "role": spec.role.as_str(),
        "runtime": spec.runtime,
        "model": spec.model,
        "effort": spec.effort.map(|e| e.as_str()),
        "instruction": spec.instruction,
        "tools": spec.tools,
    })
}

/// Rebuild a [`RoleMemberSpec`] from an admission-queue payload written by
/// [`role_member_payload`]. `None` when any required field is missing or
/// wrong-typed — a malformed ticket is dropped and audited, never guessed into
/// a partially-correct role assignment.
pub fn role_member_from_payload(payload: &serde_json::Value) -> Option<RoleMemberSpec> {
    let s = |k: &str| payload.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let effort = match payload.get("effort") {
        None | Some(serde_json::Value::Null) => None,
        Some(v) => Some(v.as_str()?.parse::<Effort>().ok()?),
    };
    Some(RoleMemberSpec {
        parent_agent: s("parent_agent")?,
        task_id: s("task_id")?,
        round: u32::try_from(payload.get("round")?.as_u64()?).ok()?,
        role: s("role")?.parse().ok()?,
        runtime: s("runtime")?,
        model: s("model")?,
        effort,
        instruction: s("instruction")?,
        tools: payload
            .get("tools")?
            .as_array()?
            .iter()
            .map(|t| t.as_str().map(str::to_string))
            .collect::<Option<Vec<String>>>()?,
    })
}
