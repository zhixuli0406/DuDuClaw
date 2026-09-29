//! Garbage collection of finished ephemerals and the admission-queue drain.
//! Moved verbatim out of `ephemeral.rs`.

use super::*;

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

        // WP-2 crash net: a *role member* has no grace window — its round is
        // over the instant it completes, and per-round accumulation overflows
        // `ephemeral_max_active` on arithmetic alone (design §3.8). The happy
        // path is `finish_role_member`, which removes it synchronously; this
        // branch only catches a member whose round died before calling it.
        // Ordinary ephemeral agents keep the unchanged grace/TTL policy.
        let role_member_completed = completed_at.is_some() && read_role_member(&path).is_some();
        if !role_member_completed && !is_due_for_gc(meta.as_ref(), completed_at, dir_modified, now)
        {
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
                // WP22 T1 — the scaffold's authoritative record dies with it,
                // after the directory is really gone (same ordering rationale
                // as MCP `agent_remove`).
                if let Err(e) = duduclaw_core::org_store::remove(home_dir, &name) {
                    tracing::warn!(
                        ephemeral = %name,
                        error = %e,
                        "org.toml removal failed during ephemeral GC"
                    );
                }
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
// H19 — admission queue drain (queue-vs-fail admission for ephemeral spawn)
// ---------------------------------------------------------------------------

/// Summary of one [`drain_admission_queue`] pass, for the caller to log.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionDrainSummary {
    /// Queued spawns admitted (scaffolded + bus-enqueued) this pass.
    pub admitted: u32,
    /// Queued tickets dropped because their TTL elapsed before capacity
    /// freed.
    pub expired: u32,
    /// Queued tickets dropped because replaying them failed (rare — a
    /// cross-process race lost to another spawn, or a malformed/stale
    /// payload). Never retried — see [`replay_queued_ephemeral`] doc.
    pub failed: u32,
}

/// Drain the H19 admission queue for ephemeral spawn requests (class
/// [`EPHEMERAL_ADMISSION_CLASS`]).
///
/// Called from the dispatcher's existing hourly maintenance tick, right
/// after [`sweep`] frees capacity — deliberately the SAME cadence as GC:
/// `ephemeral_max_active` slots are only ever freed by GC (a scaffold's
/// `.completed` marker starts a 1h grace, and the sweep itself only runs
/// hourly), so a tighter poll interval would just spin without finding new
/// room. Also unconditionally sweeps TTL-expired tickets first — even when
/// no capacity is free, a perpetually-full queue must still shed and log
/// dead entries (see `duduclaw_core::spawn_admission::sweep_expired`).
pub async fn drain_admission_queue(home_dir: &Path) -> AdmissionDrainSummary {
    let home = home_dir.to_path_buf();
    tokio::task::spawn_blocking(move || drain_admission_queue_blocking(&home))
        .await
        .unwrap_or_default()
}

fn log_admission_event(home_dir: &Path, event_type: &str, ticket_id: &str, extra: &str, ok: bool) {
    duduclaw_security::audit::append_tool_call_with_extras(
        home_dir,
        "system",
        event_type,
        &format!("ticket_id={ticket_id}{extra}"),
        ok,
        &[(
            "ticket_id",
            serde_json::Value::String(ticket_id.to_string()),
        )],
    );
}

fn drain_admission_queue_blocking(home_dir: &Path) -> AdmissionDrainSummary {
    let mut summary = AdmissionDrainSummary::default();

    // Unconditional expiry sweep — independent of current capacity, so a
    // queue that stays full forever still visibly sheds dead entries.
    for expired in
        duduclaw_core::spawn_admission::sweep_expired(home_dir, EPHEMERAL_ADMISSION_CLASS)
    {
        summary.expired += 1;
        tracing::info!(
            ticket = %expired.ticket_id,
            "H19: queued ephemeral spawn expired before capacity freed, discarded"
        );
        log_admission_event(
            home_dir,
            "ephemeral_admission_expired",
            &expired.ticket_id,
            "",
            true,
        );
    }

    let admission_cfg = duduclaw_core::spawn_admission::AdmissionConfig::from_home(home_dir);
    let cap = duduclaw_core::spawn_admission::clamp_min_one(
        admission_cfg.ephemeral_max_active,
        EPHEMERAL_ADMISSION_CLASS,
    ) as usize;
    let root = ephemeral_root(home_dir);

    loop {
        if active_count(&root) >= cap {
            break; // still at capacity — retry on the next hourly tick
        }
        let result =
            duduclaw_core::spawn_admission::dequeue_next(home_dir, EPHEMERAL_ADMISSION_CLASS);
        for expired in result.expired {
            summary.expired += 1;
            tracing::info!(ticket = %expired.ticket_id, "H19: queued ephemeral spawn expired, discarded");
            log_admission_event(
                home_dir,
                "ephemeral_admission_expired",
                &expired.ticket_id,
                "",
                true,
            );
        }
        let Some(ticket) = result.ticket else {
            break; // queue empty — nothing left to admit
        };
        match replay_queued_ephemeral(home_dir, &ticket) {
            Ok(eph_id) => {
                summary.admitted += 1;
                tracing::info!(
                    ticket = %ticket.ticket_id, ephemeral = %eph_id,
                    "H19: queued ephemeral spawn admitted (FIFO release)"
                );
                log_admission_event(
                    home_dir,
                    "ephemeral_admission_admitted",
                    &ticket.ticket_id,
                    &format!(" agent_id={eph_id}"),
                    true,
                );
            }
            Err(reason) => {
                summary.failed += 1;
                tracing::warn!(
                    ticket = %ticket.ticket_id, error = %reason,
                    "H19: queued ephemeral spawn replay failed — dropped, not retried"
                );
                log_admission_event(
                    home_dir,
                    "ephemeral_admission_failed",
                    &ticket.ticket_id,
                    &format!(" error={reason}"),
                    false,
                );
            }
        }
    }

    summary
}

/// Reconstruct and replay one queued ephemeral spawn — `scaffold` (under the
/// free capacity [`drain_admission_queue_blocking`]'s loop already confirmed)
/// followed by the same bus-queue enqueue `spawn_ephemeral_with_ctx` performs
/// at request time. Returns the newly-scaffolded ephemeral agent id.
///
/// The dispatch circuit breaker (`dispatch_guard`) is re-evaluated NOW, at
/// actual dispatch time, from the `incoming_hop` captured at the ORIGINAL
/// enqueue time — never at enqueue time itself — matching
/// `spawn_ephemeral_with_ctx`'s ordering (rate-limiting must gate when the
/// bus task is actually created, not when the request was merely queued).
/// A malformed payload or a lost cross-process race is a non-retryable
/// failure: this function is never called again for the same ticket (the
/// caller already popped it), which is intentional — retrying a request that
/// fails for a reason OTHER than capacity would just fail again and starve
/// FIFO order behind it.
fn replay_queued_ephemeral(
    home_dir: &Path,
    ticket: &duduclaw_core::spawn_admission::QueuedSpawn,
) -> Result<String, String> {
    let p = &ticket.payload;
    let get_str = |k: &str| p.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let parent = get_str("parent");
    let instruction = get_str("instruction");
    let tier = get_str("tier");
    let context = get_str("context");
    let origin = get_str("origin");
    let tools: Vec<String> = p
        .get("tools")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let outgoing_depth = p
        .get("outgoing_depth")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u8;
    let incoming_hop = p.get("incoming_hop").and_then(|v| v.as_u64()).unwrap_or(0) as u8;

    if parent.is_empty() || instruction.is_empty() || context.is_empty() || tools.is_empty() {
        return Err("queued payload malformed (missing a required field)".to_string());
    }

    let spec = EphemeralSpawnSpec {
        parent: parent.clone(),
        instruction,
        tools,
        tier,
    };
    let scaffolded = scaffold(home_dir, &spec)?;
    let eph_id = scaffolded.agent_id.clone();

    // ── P3 runaway guard (cascade hop-depth + dispatch circuit breaker) ──
    // Mirrors `check_dispatch_runaway` in duduclaw-cli's mcp.rs (that helper
    // lives one crate layer up and is not reachable from here); kept in sync
    // by the shared `duduclaw_core::dispatch_guard` primitives it wraps.
    let guard_cfg = duduclaw_core::DispatchGuardConfig::from_home(home_dir);
    let outgoing_hop = incoming_hop.saturating_add(1);
    if outgoing_hop > guard_cfg.max_hop_depth {
        let _ = std::fs::remove_dir_all(&scaffolded.dir);
        return Err(format!(
            "hop_depth {outgoing_hop} exceeds max {} on replay",
            guard_cfg.max_hop_depth
        ));
    }
    match duduclaw_core::dispatch_guard_check(home_dir, "ephemeral", &parent, &guard_cfg) {
        duduclaw_core::DispatchGuardDecision::Trip { reason, .. } => {
            let _ = std::fs::remove_dir_all(&scaffolded.dir);
            return Err(format!(
                "dispatch circuit breaker tripped on replay: {reason}"
            ));
        }
        duduclaw_core::DispatchGuardDecision::Allow => {}
    }

    // ── Enqueue on the bus — identical shape to `spawn_ephemeral_with_ctx` ──
    let task_id = uuid::Uuid::new_v4().to_string();
    let queue_path = home_dir.join("bus_queue.jsonl");
    let entry = serde_json::json!({
        "type": "agent_message",
        "message_id": &task_id,
        "agent_id": &eph_id,
        "payload": context,
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "session_key": &task_id,
        "persistent": true,
        "delegation_depth": outgoing_depth,
        "hop_depth": outgoing_hop,
        "origin_agent": origin,
        "sender_agent": parent,
    });
    let entry_str = entry.to_string();
    const MAX_QUEUE_SIZE: u64 = 10 * 1024 * 1024; // 10 MB (same cap as the live path)
    let write_result = duduclaw_core::with_file_lock(&queue_path, || {
        use std::io::Write;
        if let Ok(meta) = std::fs::metadata(&queue_path) {
            if meta.len() > MAX_QUEUE_SIZE {
                return Err(std::io::Error::other(format!(
                    "bus_queue.jsonl exceeds {}MB size limit",
                    MAX_QUEUE_SIZE / (1024 * 1024)
                )));
            }
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&queue_path)?;
        writeln!(f, "{entry_str}")?;
        Ok(())
    });
    if let Err(e) = write_result {
        // Queueing failed — tear the scaffold down (mirrors the live path's
        // same rollback), do NOT fabricate a reply on the ephemeral's behalf.
        let _ = std::fs::remove_dir_all(&scaffolded.dir);
        return Err(format!("bus enqueue failed on replay: {e}"));
    }

    Ok(eph_id)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
