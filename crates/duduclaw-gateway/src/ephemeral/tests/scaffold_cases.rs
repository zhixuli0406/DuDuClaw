//! Unit tests for [`super`], moved verbatim out of the former `ephemeral.rs` — scaffold cases.

use super::*;

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
    assert_eq!(
        live, MAX_ACTIVE_EPHEMERAL,
        "scaffold count must equal the cap"
    );
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

// ── H19: admission-queue cap sourcing + drain ──────────────────────────

/// `[dispatch] ephemeral_max_active` now sources the cap instead of the
/// hardcoded constant, and `0` is clamped to `1` — a concurrency limit
/// can be adjusted but never fully disabled.
#[test]
fn scaffold_cap_is_configurable_and_zero_is_clamped_to_one() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_parent(home, "boss", "");
    std::fs::write(
        home.join("config.toml"),
        "[dispatch]\nephemeral_max_active = 0\n",
    )
    .unwrap();

    let spec = |n: usize| EphemeralSpawnSpec {
        parent: "boss".into(),
        instruction: format!("worker {n}"),
        tools: strs(&["Read"]),
        tier: "standard".into(),
    };
    // Clamped to 1: exactly one scaffold succeeds.
    assert!(scaffold(home, &spec(1)).is_ok());
    let err = scaffold(home, &spec(2)).unwrap_err();
    assert!(
        err.starts_with(EPHEMERAL_CAPACITY_ERROR_PREFIX),
        "got: {err}"
    );
    assert!(err.contains("(1 live scaffolds)"), "got: {err}");
}
