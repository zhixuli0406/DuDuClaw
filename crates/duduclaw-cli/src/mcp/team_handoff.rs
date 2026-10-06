use super::*;

/// Keys that must never appear anywhere inside a packet.
///
/// `TaskPacket` is `deny_unknown_fields` all the way down, so serde already
/// refuses every one of these. This pre-scan exists to **name** the offending
/// key in the refusal — "unknown field `thinking`" buried in a serde error is
/// a much worse teacher than "packets never carry `thinking`" — and to find
/// one nested inside an otherwise plausible object.
pub(crate) const PACKET_FORBIDDEN_KEYS: &[&str] = &[
    "transcript",
    "messages",
    "tool_use",
    "tool_calls",
    "function_call",
    "functionCall",
    "thinking",
    "reasoning",
    "encrypted_content",
];

/// Depth ceiling for [`find_forbidden_packet_key`]. A packet is a flat-ish
/// record; anything deeper is not a packet and serde will reject it anyway.
pub(crate) const PACKET_SCAN_MAX_DEPTH: usize = 32;

/// First [`PACKET_FORBIDDEN_KEYS`] entry appearing anywhere in `value`.
///
/// Exact key equality, never a substring test (coding convention 2) — a field
/// legitimately named `reasoning_notes` must not be mistaken for `reasoning`.
pub(crate) fn find_forbidden_packet_key(value: &Value, depth: usize) -> Option<&'static str> {
    if depth > PACKET_SCAN_MAX_DEPTH {
        return None;
    }
    match value {
        Value::Object(map) => {
            for key in map.keys() {
                if let Some(hit) = PACKET_FORBIDDEN_KEYS.iter().find(|f| key == *f) {
                    return Some(hit);
                }
            }
            map.values()
                .find_map(|v| find_forbidden_packet_key(v, depth + 1))
        }
        Value::Array(items) => items
            .iter()
            .find_map(|v| find_forbidden_packet_key(v, depth + 1)),
        _ => None,
    }
}

/// Read the caller's team identity from its agent directory.
///
/// **One source only: `[team_member]`** in the caller's own `agent.toml` — the
/// scaffold's record of `(role, task_id, round)`. Present-but-unreadable is a
/// refusal, not a degrade: dropping to a weaker source would drop the
/// task/round pinning with it.
///
/// Anything else ⇒ `Err("not_a_team_member")`, which is also what a missing or
/// malformed `agent.toml` produces: membership must be demonstrated, never
/// assumed (coding convention 4). Every real role member has this section —
/// `ephemeral::scaffold_with` writes all four of role/task_id/round/parent.
///
/// ## Why there is no `[agent] role` fallback any more (review finding 2)
///
/// There used to be a second source: `[agent] role` parsed as a team
/// [`duduclaw_core::types::Role`], documented as safe because "an ordinary
/// employee's `role` is an *org* role which does not parse". **That was
/// false.** [`duduclaw_core::types::AgentRole`] has a `Planner` variant whose
/// canonical string is literally `"planner"`, and `create_agent` /
/// `agent_update` both accept it — so any long-lived employee configured
/// `[agent] role = "planner"` passed the membership check, with
/// `task_id: None` and `round: None`. Both pins below are
/// `if let Some(pinned)`, so an unpinned identity skipped them entirely and
/// could file a forged `planner → executor` packet against **any** task id
/// (enumerable via `tasks_list`) at any round. `read_packets` validates a
/// packet's self-declared lineage, not its author, and reads the canonical
/// slot before `.01`, so the forgery would have been handed to another
/// employee's executor as its sub-task instruction.
///
/// `ephemeral.rs`'s own `ROLE_MEMBER_SECTION` doc already explains that team
/// identity lives in `[team_member]` *because* `[agent] role = "planner"`
/// collides with the org concept. This fallback re-introduced exactly that
/// collision.
///
/// Fold `[team_member]` into `agent_toml::AgentTomlSections` once WP-2's
/// section shape is final.
pub(crate) fn read_team_member_identity(
    agent_dir: &Path,
) -> std::result::Result<TeamMemberIdentity, &'static str> {
    let Ok(text) = std::fs::read_to_string(agent_dir.join("agent.toml")) else {
        return Err("not_a_team_member");
    };
    let Ok(doc) = text.parse::<toml::Table>() else {
        return Err("not_a_team_member");
    };
    let Some(raw) = doc.get("team_member") else {
        return Err("not_a_team_member");
    };
    let section: TeamMemberSection = raw
        .clone()
        .try_into()
        .map_err(|_| "team_member_unreadable")?;
    let role = section
        .role
        .as_deref()
        .and_then(|r| r.parse::<duduclaw_core::types::Role>().ok())
        .ok_or("team_member_unreadable")?;
    Ok(TeamMemberIdentity {
        role,
        task_id: section
            .task_id
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty()),
        round: section.round,
    })
}

/// The caller's own agent directory, `.ephemeral/` layout included.
///
/// Team role members are scaffolded at `<home>/agents/.ephemeral/<eph-id>`
/// (WP-2), **not** at `<home>/agents/<id>`. Reading the plain path made every
/// live role member's `[team_member]` section invisible: the round-2 planner
/// that finally shaped a packet the validator accepted was then refused
/// `not_a_team_member`, gave up, and the round ended `planner_no_packets`.
///
/// [`duduclaw_gateway::ephemeral::resolve_agent_dir`] is the existing resolver
/// — it proves containment by canonicalizing both the `.ephemeral/` root and
/// the candidate — and returns `None` for anything that is not an `eph-` id,
/// which is exactly the ordinary-agent case that falls through to the plain
/// path.
///
/// `pub(crate)` because the same question is asked by sibling tool modules
/// (`mcp_db`, `mcp_planner`, `mcp_recording_distill`), which each used to
/// re-spell the bare join and so inherited the same blind spot.
pub(crate) fn caller_agent_dir(home_dir: &Path, caller: &str) -> std::path::PathBuf {
    agent_dir_for_id(home_dir, caller)
}

/// Any agent id → its on-disk directory, `.ephemeral/` layout included.
///
/// The general form of [`caller_agent_dir`]: same resolution, used where the
/// id names a *target* (a delegation peer, a wiki owner, the agent a
/// department is being read for) rather than the caller itself. Both spellings
/// exist so the W3-3b audit table can say, per call site, which question the
/// path answers.
///
/// Resolution order (`ephemeral::resolve_agent_dir`): a non-`eph-` id returns
/// `None` immediately and falls through to the plain registry path, so every
/// ordinary agent is byte-identical to a bare
/// `home_dir.join("agents").join(id)`. An `eph-` id is canonicalized and
/// proven contained inside `<home>/agents/.ephemeral/` before it is returned.
///
/// **Not** used for registry management (`create_agent` / `agent_update` /
/// `agent_remove` / `agent_update_soul` / `evolution_*` / `reports_to`
/// validation): `.ephemeral/` is deliberately outside the registry, and
/// resolving there would extend the management surface onto scaffolds.
pub(crate) fn agent_dir_for_id(home_dir: &Path, agent_id: &str) -> std::path::PathBuf {
    duduclaw_gateway::ephemeral::resolve_agent_dir(home_dir, agent_id)
        .unwrap_or_else(|| home_dir.join("agents").join(agent_id))
}

/// The pipeline's legal handoff edges: plan → do → check, plus the one
/// backward edge for repair. Everything else — including any edge touching
/// `utility`, which does not occupy a stage — is refused.
pub(crate) fn is_legal_handoff_edge(from: duduclaw_core::types::Role, to: duduclaw_core::types::Role) -> bool {
    use duduclaw_core::types::Role::{Executor, Planner, Verifier};
    matches!(
        (from, to),
        (Planner, Executor) | (Executor, Verifier) | (Verifier, Executor)
    )
}

/// The single role a given role may hand to, when there is one.
///
/// Each of the three pipeline roles has exactly one legal outgoing edge (see
/// [`is_legal_handoff_edge`], and `sole_target_agrees_with_the_edge_table`),
/// which is why `to_role` can be *derived* from the caller's identity instead
/// of demanded from it. `utility` occupies no stage and so has no target.
pub(crate) fn sole_legal_handoff_target(
    from: duduclaw_core::types::Role,
) -> Option<duduclaw_core::types::Role> {
    use duduclaw_core::types::Role::{Executor, Planner, Utility, Verifier};
    match from {
        Planner => Some(Executor),
        Executor => Some(Verifier),
        Verifier => Some(Executor),
        Utility => None,
    }
}

/// What "right" looks like, appended to every shape refusal: serde's message
/// says what is wrong, this says what to send instead.
///
/// Built at runtime rather than `concat!`-ed because the example is a `const`
/// in `duduclaw_core` (the tool description holds the same text as a literal,
/// and `team_handoff_description_shows_the_minimal_example` pins the two
/// together).
pub(crate) fn packet_shape_hint() -> String {
    format!(
        " · send this shape — these seven keys are enough: {} · optional keys, all defaulting to empty: {}",
        duduclaw_core::task_packet::MINIMAL_PACKET_EXAMPLE,
        duduclaw_core::task_packet::OPTIONAL_PACKET_KEYS.join(", "),
    )
}

/// How many numbered siblings one leg may hold before the call is refused.
pub(crate) const PACKET_FANOUT_MAX: u32 = 99;

/// `packet_id` of the packet already at `path`, if any is readable.
pub(crate) fn existing_packet_id(path: &Path) -> Option<String> {
    let raw = std::fs::read(path).ok()?;
    let value: Value = serde_json::from_slice(&raw).ok()?;
    value
        .get("packet_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// Decide which file this packet occupies on its leg.
///
/// The canonical [`duduclaw_core::task_packet::packet_path`] is used whenever
/// it is free, or already holds *this* `packet_id` — so re-filing after a
/// timeout is idempotent rather than duplicating. A **different** packet on
/// the same leg gets a numbered sibling in the same directory
/// (`planner-to-executor.01.json`, …), because one round can legitimately
/// carry several packets on one leg: a planner fanning a goal out into
/// independent sub-tasks writes one packet per sub-task. The composer
/// enumerates that leg by slot number (canonical file first, then `.01` …
/// `.99`; a missing slot ends the scan) and validates `from_role`/`to_role`
/// per file rather than trusting a filename, so siblings are found;
/// overwriting the canonical path instead would silently lose every sub-task
/// but the last.
///
/// Deterministic: the lowest free two-digit index, so filename order is also
/// arrival order. Must be called while holding the canonical path's lock.
pub(crate) fn resolve_packet_slot(canonical: &Path, packet_id: &str) -> std::io::Result<std::path::PathBuf> {
    if !canonical.exists() || existing_packet_id(canonical).as_deref() == Some(packet_id) {
        return Ok(canonical.to_path_buf());
    }
    for n in 1..=PACKET_FANOUT_MAX {
        let sibling = canonical.with_extension(format!("{n:02}.json"));
        if !sibling.exists() || existing_packet_id(&sibling).as_deref() == Some(packet_id) {
            return Ok(sibling);
        }
    }
    Err(std::io::Error::other(format!(
        "this handoff leg already holds {PACKET_FANOUT_MAX} packets"
    )))
}

/// File `bytes` on this leg atomically: pick the slot, write a temp file,
/// fsync, rename — all under the canonical path's cross-process advisory lock
/// (coding convention 3 — the gateway composer reads these files while members
/// write them, and the lock is taken on the canonical path so every sibling of
/// one leg serializes against the same guard). Returns the path written.
pub(crate) fn write_packet_atomic(
    canonical: &Path,
    packet_id: &str,
    bytes: &[u8],
) -> std::io::Result<std::path::PathBuf> {
    if let Some(parent) = canonical.parent() {
        std::fs::create_dir_all(parent)?;
    }
    duduclaw_core::with_file_lock(canonical, || {
        use std::io::Write as _;
        let path = resolve_packet_slot(canonical, packet_id)?;
        let tmp = path.with_extension("json.tmp");
        {
            let mut opts = std::fs::OpenOptions::new();
            opts.create(true).write(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            let mut file = opts.open(&tmp)?;
            file.write_all(bytes)?;
            file.sync_all()?;
        }
        std::fs::rename(&tmp, &path)?;
        Ok(path)
    })
}

pub(crate) async fn handle_team_handoff(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let caller = resolve_audit_agent(|| default_agent.to_string());
    // Every refusal gets an audited row carrying the stable code; the generic
    // state-changing audit in `handle_tools_call` records the call itself but
    // not *why* it failed (same shape as `codrive_run`).
    let refuse = |code: &str, msg: &str| -> Value {
        duduclaw_security::audit::append_tool_call_denied(
            home_dir,
            &caller,
            "team_handoff",
            code,
            msg,
            Some(args),
        );
        tool_error(&format!("{code}: {msg}"))
    };

    if !duduclaw_core::is_valid_agent_id(&caller) {
        return refuse(
            "invalid_agent_id",
            "caller identity is not a usable agent id",
        );
    }

    // Accept a native object or a JSON-encoded string: CLI runtimes serialize
    // nested objects either way (same tolerance as `codrive_run`).
    let mut raw = match args.get("packet") {
        Some(Value::String(s)) => match serde_json::from_str::<Value>(s) {
            Ok(v) => v,
            Err(e) => {
                return refuse(
                    "invalid_packet",
                    &format!("packet is not valid JSON: {e}{}", packet_shape_hint()),
                );
            }
        },
        Some(v) => v.clone(),
        None => return refuse("missing_packet", "packet is required"),
    };
    if !raw.is_object() {
        return refuse(
            "invalid_packet",
            &format!("packet must be a JSON object{}", packet_shape_hint()),
        );
    }
    if let Some(key) = find_forbidden_packet_key(&raw, 0) {
        return refuse(
            "forbidden_provider_field",
            &format!(
                "packets never carry `{key}` — hand over decisions and references, \
                 not a transcript (see docs/spec/task-packet.md)"
            ),
        );
    }

    // ── Identity: the packet describes the work, never the author ──────
    //
    // Read BEFORE the packet is parsed, because it supplies the identity
    // fields a caller may omit (below). It is also the refusal that teaches
    // the most, so a non-member learns it on its first call instead of after
    // twelve rounds of fighting the schema.
    let identity_dir = caller_agent_dir(home_dir, &caller);
    let member = match read_team_member_identity(&identity_dir) {
        Ok(m) => m,
        Err("team_member_unreadable") => {
            return refuse(
                "team_member_unreadable",
                "your agent.toml has a [team_member] section that cannot be read",
            );
        }
        Err(_) => {
            return refuse(
                "not_a_team_member",
                "only a role member of a team may file a TaskPacket",
            );
        }
    };

    // Review finding 2, second half: an identity with no `task_id` / `round`
    // pin has nothing for the two cross-checks further down to check against
    // (both are `if let Some(pinned)`, so both are skipped), which let such a
    // caller file a forged packet at ANY task id — enumerable through
    // `tasks_list` — and any round. Every scaffolded role member IS pinned
    // (`ephemeral::scaffold_with` writes role/task_id/round/parent together),
    // so the only way to arrive here is a hand-authored `[team_member]`
    // section: not a membership claim this tool may honour. Fail closed
    // (coding convention 4).
    if member.task_id.is_none() || member.round.is_none() {
        return refuse(
            "unpinned_team_member",
            "your [team_member] section names no task_id/round, so this call cannot be bound to \
             a round — a scaffolded role member always carries both",
        );
    }

    // The member's own record is authoritative for `(role, task, round)`, so a
    // caller may simply omit those keys — restating what the tool already
    // knows is pure opportunity for an `invalid_packet`, and twelve of the
    // round-2 planner's thirteen calls failed on shape rather than on work.
    // A value the caller DID write is left untouched and cross-checked below,
    // so a packet claiming someone else's role is still refused.
    if let Some(obj) = raw.as_object_mut() {
        obj.entry("from_role")
            .or_insert_with(|| Value::String(member.role.as_str().to_string()));
        if let Some(to) = sole_legal_handoff_target(member.role) {
            obj.entry("to_role")
                .or_insert_with(|| Value::String(to.as_str().to_string()));
        }
        if let Some(task) = member.task_id.as_deref() {
            obj.entry("goal_id")
                .or_insert_with(|| Value::String(task.to_string()));
        }
        if let Some(round) = member.round {
            obj.entry("round")
                .or_insert_with(|| Value::Number(round.into()));
        }
    }

    let packet: duduclaw_core::task_packet::TaskPacket = match serde_json::from_value(raw) {
        Ok(p) => p,
        // Serde's own message, verbatim and first: "missing field `objective`"
        // / "unknown field `objectives`, expected one of …" is the whole
        // repair instruction. The old wrapper prose buried it.
        Err(e) => return refuse("invalid_packet", &format!("{e}{}", packet_shape_hint())),
    };
    if let Err(e) = packet.validate() {
        // Already actionable and indexed (`constraints[3].text is 240 chars
        // (max 200)`) — no hint needed, and each cap keeps its own code.
        return refuse(e.code(), &e.to_string());
    }
    if packet.from_role != member.role {
        return refuse(
            "from_role_mismatch",
            &format!(
                "you are `{}`, but the packet says from_role `{}`",
                member.role, packet.from_role
            ),
        );
    }
    if let Some(pinned) = member.task_id.as_deref()
        && pinned != packet.goal_id
    {
        return refuse(
            "task_id_mismatch",
            "the packet's goal_id is not the task you were assigned",
        );
    }
    if let Some(pinned) = member.round
        && pinned != packet.round
    {
        return refuse(
            "round_mismatch",
            &format!(
                "you are on round {pinned}, but the packet says round {}",
                packet.round
            ),
        );
    }
    if !is_legal_handoff_edge(packet.from_role, packet.to_role) {
        return refuse(
            "invalid_handoff_edge",
            &format!(
                "`{}` → `{}` is not a stage transition (legal: planner→executor, \
                 executor→verifier, verifier→executor)",
                packet.from_role, packet.to_role
            ),
        );
    }

    let canonical = match duduclaw_core::task_packet::packet_path(
        home_dir,
        &packet.goal_id,
        packet.round,
        packet.from_role,
        packet.to_role,
    ) {
        Ok(p) => p,
        Err(e) => return refuse(e.code(), &e.to_string()),
    };
    // Serialized once, here: the bytes that land on disk are the bytes the
    // cap was measured against.
    let bytes = match serde_json::to_vec(&packet) {
        Ok(b) => b,
        Err(e) => {
            return refuse(
                "invalid_packet",
                &format!("packet could not be serialized: {e}"),
            );
        }
    };

    // Who may see the task before this packet (F3 follow-up): a packet that
    // first limits people is recorded on the Activity Feed and audit log.
    let audience_before =
        duduclaw_gateway::review_evidence::audience::task_audience(home_dir, &packet.goal_id);
    let write_canonical = canonical.clone();
    let write_id = packet.packet_id.clone();
    let write_bytes = bytes.clone();
    let path = match tokio::task::spawn_blocking(move || {
        write_packet_atomic(&write_canonical, &write_id, &write_bytes)
    })
    .await
    .unwrap_or_else(|e| Err(std::io::Error::other(format!("join error: {e}"))))
    {
        Ok(p) => p,
        Err(e) => return refuse("write_failed", &format!("could not file the packet: {e}")),
    };

    // Provenance: the packet is something this role produced for this task,
    // and the row carries the task id, so the task detail page attributes it
    // exactly rather than inferring it from a time window.
    let relative = path
        .strip_prefix(home_dir)
        .unwrap_or(&path)
        .to_string_lossy()
        .to_string();
    // `record_saved` reads its first argument only to derive `(home, agent)`
    // from the `<home>/agents/<id>` shape, so it gets that canonical spelling
    // even for a role member whose real directory is one level deeper under
    // `.ephemeral/` — passing the deeper path would file the row into the
    // scaffold's own ledger under an empty agent id, where the task detail
    // page never looks.
    let provenance_base = home_dir.join("agents").join(&caller);
    duduclaw_gateway::artifacts::record_saved(
        &provenance_base,
        &path,
        &format!(
            "handoff {}→{} r{}",
            packet.from_role, packet.to_role, packet.round
        ),
        bytes.len() as u64,
        &duduclaw_gateway::artifacts::SaveContext {
            origin: duduclaw_gateway::artifacts::ArtifactOrigin::Produced,
            task_id: Some(&packet.goal_id),
            round: Some(packet.round),
            channel: None,
            source_path: Some(&path),
        },
    );

    // `constraints` is a count (bodies belong in the packet, not the security
    // log); `audience` is recorded verbatim because *who may see this* is the
    // control the allowlist exists to make auditable.
    duduclaw_security::audit::append_audit_event(
        home_dir,
        &duduclaw_security::audit::AuditEvent::new(
            "team_handoff",
            &caller,
            duduclaw_security::audit::Severity::Info,
            serde_json::json!({
                "task_id": packet.goal_id,
                "round": packet.round,
                "from_role": packet.from_role,
                "to_role": packet.to_role,
                "bytes": bytes.len(),
                "constraints": packet.constraints.len(),
                "audience": packet.audience,
                "path": relative,
            }),
        ),
    );

    duduclaw_gateway::review_evidence::audience::record_audience_restriction(
        home_dir,
        &packet.goal_id,
        &audience_before,
        packet.round,
        &packet.from_role.to_string(),
        &caller,
    )
    .await;

    tool_text(
        &serde_json::json!({ "ok": true, "path": relative, "bytes": bytes.len() }).to_string(),
    )
}

// ── Belief Loop (design-market-belief-loop-2026-08.md WP2) ──────────────────
// Structured, programmatically-settled beliefs about an external subject.
// Agent identity is always `default_agent` (the caller's own), never a
// client-supplied field — `submit`/`settle`/`stats` in
// `duduclaw_gateway::prediction::belief` enforce ownership on top of
// that, but the MCP front door never even offers an `agent_id` param to
// spoof.
