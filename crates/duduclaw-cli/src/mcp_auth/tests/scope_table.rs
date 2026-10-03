use super::*;

// ── P0-S3 (2026-08 audit): lock `Scope` ↔ the shared canonical list in
// duduclaw-core bidirectionally. The gateway's `mcp_keys.create` scope
// validator and the frontend's scope picker both read
// `duduclaw_core::mcp_scopes::MCP_SCOPE_STRINGS` (gateway can't depend on
// this crate to read `Scope` directly — see that module's doc comment).
// If a future scope is added to the enum but not the shared list (or vice
// versa), this test goes red instead of the drift silently reappearing as
// "Unknown scope" in the dashboard for the new scope.
#[test]
fn scope_enum_matches_canonical_list() {
    use duduclaw_core::mcp_scopes::MCP_SCOPE_STRINGS;

    // Every enum variant, listed explicitly (Scope has no EnumIter — this
    // hardcoded list IS the trip-wire: forgetting to add a new variant
    // here is caught by the length assertion below).
    let all_variants = [
        Scope::MemoryRead,
        Scope::MemoryWrite,
        Scope::WikiRead,
        Scope::WikiWrite,
        Scope::MessagingSend,
        Scope::IdentityRead,
        Scope::OdooRead,
        Scope::OdooWrite,
        Scope::OdooExecute,
        Scope::GoogleRead,
        Scope::GoogleWrite,
        Scope::NotionRead,
        Scope::NotionWrite,
        Scope::GithubRead,
        Scope::GithubWrite,
        Scope::ForkExecute,
        Scope::OsNative,
        Scope::SkillExecute,
        Scope::Recording,
        Scope::MailRead,
        Scope::MailSend,
        Scope::DbRead,
        Scope::FilesRead,
        Scope::TeamHandoff,
        Scope::DiscoveryExecute,
        Scope::Admin,
    ];

    assert_eq!(
        all_variants.len(),
        MCP_SCOPE_STRINGS.len(),
        "Scope enum and duduclaw_core::mcp_scopes::MCP_SCOPE_STRINGS have \
         drifted in size — add the new variant to BOTH lists"
    );

    // Direction 1: every enum variant's Display string is in the shared
    // list, and parses back to the same variant.
    for variant in &all_variants {
        let wire = variant.to_string();
        assert!(
            MCP_SCOPE_STRINGS.contains(&wire.as_str()),
            "Scope::{variant:?} ({wire}) missing from \
             duduclaw_core::mcp_scopes::MCP_SCOPE_STRINGS"
        );
        let mut parsed = parse_scopes(&wire).expect("must parse its own Display string");
        assert_eq!(parsed.len(), 1);
        assert!(
            parsed.remove(variant),
            "parse_scopes({wire}) did not round-trip to Scope::{variant:?}"
        );
    }

    // Direction 2: every string in the shared list parses successfully
    // (no orphan entries the enum doesn't back).
    for s in MCP_SCOPE_STRINGS {
        assert!(
            parse_scopes(s).is_ok(),
            "canonical scope string '{s}' does not parse via Scope::from_str"
        );
    }
}

// ── OS-native Phase 1: os:native scope round-trips ───────────────────────
#[test]
fn test_os_native_scope_parse_and_display() {
    let scopes = parse_scopes("os:native").expect("should parse");
    assert!(scopes.contains(&Scope::OsNative));
    assert_eq!(scopes.len(), 1);
    assert_eq!(Scope::OsNative.to_string(), "os:native");
}

// ── Test 8: tool_requires_scope memory_store → MemoryWrite ───────────────
#[test]
fn test_tool_requires_scope_memory_store() {
    assert_eq!(
        tool_requires_scope("memory_store"),
        Some(Scope::MemoryWrite)
    );
}

// ── Test 9: tool_requires_scope memory_search → MemoryRead ───────────────
#[test]
fn test_tool_requires_scope_memory_search() {
    assert_eq!(
        tool_requires_scope("memory_search"),
        Some(Scope::MemoryRead)
    );
}

// ── Test 10: tool_requires_scope totally_unknown → None ──────────────────
#[test]
fn test_recording_scope_parses_and_maps() {
    // WP3.3: the recording scope round-trips through parse/Display and
    // every recording tool maps to it (never falls through to Admin).
    let scopes = parse_scopes("recording").expect("should parse");
    assert!(scopes.contains(&Scope::Recording));
    assert_eq!(Scope::Recording.to_string(), "recording");
    for tool in [
        "browser_record_start",
        "browser_record_stop",
        "desktop_record_start",
        "desktop_record_stop",
        "skill_from_recording",
    ] {
        assert_eq!(
            tool_requires_scope(tool),
            Some(Scope::Recording),
            "tool {tool} must require the recording scope"
        );
    }
}

#[test]
fn test_tool_requires_scope_unknown_tool() {
    // C2: fail-closed — unknown tools require Admin, not None.
    assert_eq!(tool_requires_scope("totally_unknown"), Some(Scope::Admin));
}

// ── O-0: system-operator tool face (DESIGN-agent-os-native-apps-2026-08.md
//    §6.3) — every os_* system tool maps to Admin, and is therefore never
//    reachable by an external MCP client (Admin is not in
//    EXTERNALLY_GRANTABLE_SCOPES). ──────────────────────────────────────

#[test]
fn os_ops_tools_require_admin_scope() {
    for tool in [
        "os_device_status",
        "os_system_status",
        "os_check_update",
        "os_backup_list",
        "os_network_info",
        "os_wifi_status",
        "os_wifi_scan",
        "os_wifi_connect",
        "os_apply_update",
        "os_boot_assessment",
        "os_update_rollback",
        "os_backup_create",
        "os_power",
        "os_factory_reset",
        "os_doctor_repair",
    ] {
        assert_eq!(
            tool_requires_scope(tool),
            Some(Scope::Admin),
            "tool {tool} must require Admin scope"
        );
    }
}

#[test]
fn os_ops_tools_never_reachable_by_external_clients() {
    // Even an external principal that explicitly claims Admin cannot
    // reach these — `external_tool_allowed` only substitutes within
    // `EXTERNALLY_GRANTABLE_SCOPES`, which does not include Admin.
    let principal = Principal {
        client_id: "external-client".to_string(),
        scopes: [Scope::Admin].into_iter().collect(),
        is_external: true,
        created_at: chrono::Utc::now(),
    };
    for tool in [
        "os_device_status",
        "os_factory_reset",
        "os_power",
        "os_apply_update",
        "os_boot_assessment",
        "os_update_rollback",
    ] {
        assert!(
            !external_tool_allowed(tool, &principal),
            "tool {tool} must never be reachable by an external client"
        );
    }
}

// ── Drift guard: dashboard tool catalog ↔ security gate ──────────────────
// The dashboard "add from built-in tools" picker is fed by
// `duduclaw_core::tool_catalog::builtin_tool_catalog()`, which lives in
// `duduclaw-core` because the gateway cannot depend on this crate (cli →
// gateway → core; a gateway → cli dep would be a cycle). This test is the
// mechanical guard that keeps the catalog's advertised scope byte-identical
// to what `tool_requires_scope` actually enforces. If someone changes a
// tool's scope in the gate but not the catalog (or vice versa), this fails.
#[test]
fn test_catalog_scopes_match_tool_requires_scope() {
    for entry in duduclaw_core::tool_catalog::builtin_tool_catalog() {
        if entry.kind != "mcp" {
            continue; // native Claude tools have no MCP scope
        }
        let enforced = tool_requires_scope(entry.name)
            .expect("enumerated MCP tool must resolve to a scope")
            .to_string();
        assert_eq!(
            enforced, entry.scope,
            "catalog scope for `{}` ({}) drifted from tool_requires_scope ({})",
            entry.name, entry.scope, enforced
        );
    }
}

/// A2: both co-drive tool faces resolve to Admin through their OWN
/// enumerated arm, never the Admin fall-through — so a future scope
/// split for co-drive stays a one-line diff instead of a silent
/// behavior change. The read-only `codrive_status` is deliberately held
/// to the same tier as `codrive_run`: knowing whether a human is
/// currently at the shared desktop is not a harmless read.
#[test]
fn test_both_codrive_tools_require_admin_and_are_internal_only() {
    for tool in ["codrive_run", "codrive_status"] {
        assert_eq!(
            tool_requires_scope(tool),
            Some(Scope::Admin),
            "tool {tool} must require Admin"
        );
        let external = Principal {
            client_id: "external-client".to_string(),
            scopes: [Scope::Admin].into_iter().collect(),
            is_external: true,
            created_at: chrono::Utc::now(),
        };
        assert!(
            !external_tool_allowed(tool, &external),
            "tool {tool} must never be reachable by an external client"
        );
    }
}

#[test]
fn test_dangerous_tools_require_admin() {
    // C2 regression: these previously returned None (no scope), letting a
    // narrowly-scoped key invoke them.
    for tool in [
        "execute_program",
        "agent_update_soul",
        "agent_remove",
        "agent_update",
        "spawn_agent",
        "create_agent",
        "send_to_agent",
        "evolution_toggle",
        "delete_cron_task",
        "run_cron_task",
        "wiki_write",
        "shared_wiki_delete",
    ] {
        let req = tool_requires_scope(tool);
        assert!(
            matches!(req, Some(Scope::Admin) | Some(Scope::WikiWrite)),
            "tool {tool} must require a real scope, got {req:?}"
        );
        // A memory:read-only principal must NOT satisfy it.
        assert_ne!(req, Some(Scope::MemoryRead));
    }
}

#[test]
fn test_new_odoo_read_tools_require_odoo_read() {
    // The safe customer search + schema introspection tools must sit in the
    // read scope class — never fall through to Admin or (worse) None.
    for tool in ["odoo_partner_search", "odoo_schema_fields"] {
        assert_eq!(
            tool_requires_scope(tool),
            Some(Scope::OdooRead),
            "tool {tool} must require odoo:read"
        );
    }
}

#[test]
fn test_explicitly_enumerated_admin_tools_2026_07() {
    // Scope-table consistency (2026-07): these previously relied on the
    // Admin fall-through; now enumerated explicitly with the same
    // effective scope.
    for tool in ["spawn_ephemeral", "cost_multi_vs_single"] {
        assert_eq!(
            tool_requires_scope(tool),
            Some(Scope::Admin),
            "tool {tool} must be explicitly Admin"
        );
    }
}

#[test]
fn test_read_tools_keep_narrow_scope() {
    // Narrow read keys must keep working (not forced to Admin).
    assert_eq!(tool_requires_scope("wiki_ls"), Some(Scope::WikiRead));
    assert_eq!(
        tool_requires_scope("memory_search_by_layer"),
        Some(Scope::MemoryRead)
    );
    assert_eq!(
        tool_requires_scope("send_photo"),
        Some(Scope::MessagingSend)
    );
}

// ── D3.2: entity-alias tools sit in the memory scope family ──────────────
#[test]
fn test_entity_alias_tools_scope() {
    // Adding an alias mutates the knowledge graph → write tier.
    assert_eq!(
        tool_requires_scope("memory_alias_add"),
        Some(Scope::MemoryWrite),
        "memory_alias_add must require memory:write"
    );
    // Listing aliases is read-only.
    assert_eq!(
        tool_requires_scope("memory_alias_list"),
        Some(Scope::MemoryRead),
        "memory_alias_list must require memory:read"
    );
    // A read-only key must NOT satisfy the write tool.
    assert_ne!(
        tool_requires_scope("memory_alias_add"),
        Some(Scope::MemoryRead)
    );
}

#[test]
fn test_google_scopes_parse_and_display() {
    let scopes = parse_scopes("google:read,google:write").expect("should parse");
    assert!(scopes.contains(&Scope::GoogleRead));
    assert!(scopes.contains(&Scope::GoogleWrite));
    assert_eq!(Scope::GoogleRead.to_string(), "google:read");
    assert_eq!(Scope::GoogleWrite.to_string(), "google:write");
}

#[test]
fn test_google_tools_scope_split() {
    // Read class.
    for tool in [
        "google_status",
        "gmail_search",
        "gmail_read",
        "calendar_list_events",
    ] {
        assert_eq!(
            tool_requires_scope(tool),
            Some(Scope::GoogleRead),
            "tool {tool} must require google:read"
        );
    }
    // Write class — a read-only key must NOT satisfy it.
    for tool in ["gmail_create_draft", "calendar_create_event"] {
        assert_eq!(
            tool_requires_scope(tool),
            Some(Scope::GoogleWrite),
            "tool {tool} must require google:write"
        );
        assert_ne!(tool_requires_scope(tool), Some(Scope::GoogleRead));
    }
}

#[test]
fn test_notion_scopes_parse_and_display() {
    let scopes = parse_scopes("notion:read,notion:write").expect("should parse");
    assert!(scopes.contains(&Scope::NotionRead));
    assert!(scopes.contains(&Scope::NotionWrite));
    assert_eq!(Scope::NotionRead.to_string(), "notion:read");
    assert_eq!(Scope::NotionWrite.to_string(), "notion:write");
}

#[test]
fn test_github_scopes_parse_and_display() {
    let scopes = parse_scopes("github:read,github:write").expect("should parse");
    assert!(scopes.contains(&Scope::GithubRead));
    assert!(scopes.contains(&Scope::GithubWrite));
    assert_eq!(Scope::GithubRead.to_string(), "github:read");
    assert_eq!(Scope::GithubWrite.to_string(), "github:write");
}

#[test]
fn test_notion_tools_scope_split() {
    for tool in ["notion_status", "notion_search", "notion_page_read"] {
        assert_eq!(
            tool_requires_scope(tool),
            Some(Scope::NotionRead),
            "tool {tool} must require notion:read"
        );
    }
    assert_eq!(
        tool_requires_scope("notion_page_append"),
        Some(Scope::NotionWrite)
    );
    // A read-only key must NOT satisfy the write tool.
    assert_ne!(
        tool_requires_scope("notion_page_append"),
        Some(Scope::NotionRead)
    );
}

#[test]
fn test_github_tools_scope_split() {
    for tool in [
        "github_status",
        "github_search_issues",
        "github_issue_read",
        "github_pr_read",
    ] {
        assert_eq!(
            tool_requires_scope(tool),
            Some(Scope::GithubRead),
            "tool {tool} must require github:read"
        );
    }
    assert_eq!(
        tool_requires_scope("github_issue_comment"),
        Some(Scope::GithubWrite)
    );
    assert_ne!(
        tool_requires_scope("github_issue_comment"),
        Some(Scope::GithubRead)
    );
}

#[test]
fn test_sheets_tools_join_google_scope_split() {
    assert_eq!(tool_requires_scope("sheets_read"), Some(Scope::GoogleRead));
    assert_eq!(
        tool_requires_scope("sheets_append"),
        Some(Scope::GoogleWrite)
    );
    assert_ne!(
        tool_requires_scope("sheets_append"),
        Some(Scope::GoogleRead)
    );
}

#[test]
fn test_user_code_profile_is_memory_read() {
    // UaC profile compilation is a read-only memory view — same scope as
    // memory_search / user_profile_get.
    assert_eq!(
        tool_requires_scope("user_code_profile"),
        Some(Scope::MemoryRead)
    );
}

// ── M6: fail-closed when nothing is configured ────────────────────────────
#[test]
fn test_unconfigured_is_fail_closed_by_default() {
    let _guard = ENV_LOCK.lock().unwrap();
    let dir = TempDir::new().unwrap(); // no config.toml ⇒ empty registry
    // SAFETY: protected by ENV_LOCK.
    unsafe {
        std::env::remove_var("DUDUCLAW_MCP_API_KEY");
        std::env::remove_var("DUDUCLAW_MCP_ALLOW_UNAUTHENTICATED");
    }
    let result = authenticate_from_env(dir.path());
    assert_eq!(
        result.unwrap_err(),
        AuthError::MissingKey,
        "unauthenticated peer must NOT be granted the default Admin principal"
    );
}

#[test]
fn test_unconfigured_grants_default_only_with_explicit_optin() {
    let _guard = ENV_LOCK.lock().unwrap();
    let dir = TempDir::new().unwrap();
    // SAFETY: protected by ENV_LOCK.
    unsafe {
        std::env::remove_var("DUDUCLAW_MCP_API_KEY");
        std::env::set_var("DUDUCLAW_MCP_ALLOW_UNAUTHENTICATED", "1");
    }
    let result = authenticate_from_env(dir.path());
    unsafe { std::env::remove_var("DUDUCLAW_MCP_ALLOW_UNAUTHENTICATED") };
    let principal = result.expect("explicit opt-in should grant default principal");
    assert_eq!(principal.client_id, "default");
    assert!(principal.scopes.contains(&Scope::Admin));
    assert!(!principal.is_external);
}

// ── L12: future-dated key must not be treated as ancient ──────────────────
#[test]
fn test_future_dated_key_is_not_expired() {
    let _guard = ENV_LOCK.lock().unwrap();
    let key = fresh_key("prod");
    // created_at 10 days in the FUTURE (clock skew / mis-set system time).
    let future = (Utc::now() + chrono::Duration::days(10)).to_rfc3339();
    let dir = make_config_dir_with_key(&key, "client-future", &["memory:read"], false, &future);
    // SAFETY: protected by ENV_LOCK.
    unsafe { std::env::set_var("DUDUCLAW_MCP_API_KEY", &key) };
    let result = authenticate_from_env(dir.path());
    unsafe { std::env::remove_var("DUDUCLAW_MCP_API_KEY") };
    // Before the L12 fix, num_days() was negative and `as u64` wrapped to a
    // huge value ⇒ KeyExpired. Now age clamps to 0 ⇒ authenticates.
    let principal = result.expect("future-dated key must authenticate, not falsely expire");
    assert_eq!(principal.client_id, "client-future");
}

// ── Test 11: constant-time lookup — valid key matching different entries ──
// Verifies that the constant-time scan selects the correct entry even when
// multiple keys share the same prefix (tests that the full 48-char comparison
// is completed, not short-circuited).
#[test]
fn test_constant_time_lookup_selects_correct_entry() {
    let _guard = ENV_LOCK.lock().unwrap();
    // Two keys that share the same env prefix (prod) but differ only in the
    // hex body — simulates a timing-attack scenario where a partial match
    // could be detected via early-exit.
    let key_a = "ddc_prod_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"; // 32 × 'a'
    let key_b = "ddc_prod_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"; // 32 × 'b'
    let dir = TempDir::new().unwrap();
    let today = fresh_today_rfc3339();
    let content = format!(
        r#"
[mcp_keys."{key_a}"]
client_id = "client-a"
scopes = ["memory:read"]
created_at = "{today}"
is_external = false

[mcp_keys."{key_b}"]
client_id = "client-b"
scopes = ["wiki:read"]
created_at = "{today}"
is_external = true
"#
    );
    std::fs::write(dir.path().join("config.toml"), &content).unwrap();

    // Authenticate with key_b — must resolve to client-b, not client-a.
    // SAFETY: protected by ENV_LOCK.
    unsafe { std::env::set_var("DUDUCLAW_MCP_API_KEY", key_b) };
    let result = authenticate_from_env(dir.path());
    unsafe { std::env::remove_var("DUDUCLAW_MCP_API_KEY") };

    let principal = result.expect("key_b should authenticate");
    assert_eq!(principal.client_id, "client-b");
    assert!(principal.is_external);
    assert!(principal.scopes.contains(&Scope::WikiRead));
    assert!(!principal.scopes.contains(&Scope::MemoryRead));
}

// ── Gap (a), WP-H2 §1.3: KeyRegistryCache / authenticate_from_env_cached ──

/// Explicitly set a file's mtime forward so tests never depend on
/// filesystem mtime resolution (HFS+ can be 1s-granular) or need a real
/// sleep to observe a "changed" mtime.
fn bump_mtime(path: &std::path::Path, seconds_forward: u64) {
    let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    let current = f.metadata().unwrap().modified().unwrap();
    f.set_modified(current + std::time::Duration::from_secs(seconds_forward))
        .unwrap();
}

/// Key rotation (scopes changed in `[mcp_keys]`) takes effect on the very
/// next call once the file's mtime has moved — no restart, no waiting for
/// a TTL. This is the direct regression test for Gap (a): before the fix,
/// only a fresh `authenticate_from_env` call (i.e. process restart) would
/// observe a scope change.
#[test]
fn cached_auth_observes_scope_rotation_after_mtime_change() {
    let _guard = ENV_LOCK.lock().unwrap();
    let key = fresh_key("prod");
    let today = fresh_today_rfc3339();
    let dir =
        make_config_dir_with_key(&key, "rotating-client", &["memory:read"], false, &today);
    let cache = KeyRegistryCache::new();

    unsafe { std::env::set_var("DUDUCLAW_MCP_API_KEY", &key) };

    let first =
        authenticate_from_env_cached(dir.path(), &cache).expect("first call authenticates");
    assert!(first.scopes.contains(&Scope::MemoryRead));
    assert!(!first.scopes.contains(&Scope::WikiWrite));

    // Operator rotates the key's scopes in place (same key string, wider
    // grant) and the file's mtime visibly advances.
    let dir2 = make_config_dir_with_key(
        &key,
        "rotating-client",
        &["memory:read", "wiki:write"],
        false,
        &today,
    );
    std::fs::copy(
        dir2.path().join("config.toml"),
        dir.path().join("config.toml"),
    )
    .unwrap();
    bump_mtime(&dir.path().join("config.toml"), 5);

    let second =
        authenticate_from_env_cached(dir.path(), &cache).expect("second call authenticates");
    unsafe { std::env::remove_var("DUDUCLAW_MCP_API_KEY") };

    assert!(
        second.scopes.contains(&Scope::WikiWrite),
        "rotated scope must be visible on the very next call, not just after a restart"
    );
}

/// Key revocation (entry removed from `[mcp_keys]`) takes effect on the
/// very next call once the file's mtime has moved.
#[test]
fn cached_auth_observes_revocation_after_mtime_change() {
    let _guard = ENV_LOCK.lock().unwrap();
    let key = fresh_key("prod");
    let today = fresh_today_rfc3339();
    let dir = make_config_dir_with_key(&key, "revoked-client", &["memory:read"], false, &today);
    let cache = KeyRegistryCache::new();

    unsafe { std::env::set_var("DUDUCLAW_MCP_API_KEY", &key) };

    let first =
        authenticate_from_env_cached(dir.path(), &cache).expect("first call authenticates");
    assert_eq!(first.client_id, "revoked-client");

    // Operator revokes the key: the `[mcp_keys]` table loses the entry.
    std::fs::write(dir.path().join("config.toml"), "[settings]\nfoo = 1\n").unwrap();
    bump_mtime(&dir.path().join("config.toml"), 5);

    let second = authenticate_from_env_cached(dir.path(), &cache);
    unsafe { std::env::remove_var("DUDUCLAW_MCP_API_KEY") };

    assert_eq!(
        second.unwrap_err(),
        AuthError::UnknownKey,
        "a revoked key must be denied on the very next call, not just after a restart"
    );
}

/// When `config.toml`'s mtime has NOT changed, the cache must be reused
/// rather than re-read from disk. Proven by making the file unreadable
/// (permission-denied) WITHOUT touching its mtime: a naive
/// "re-parse every call" implementation would start failing immediately,
/// while the cached implementation keeps succeeding because it never
/// re-opens the file.
#[cfg(unix)]
#[test]
fn cached_auth_reuses_registry_when_mtime_unchanged() {
    use std::os::unix::fs::PermissionsExt;

    let _guard = ENV_LOCK.lock().unwrap();
    let key = fresh_key("prod");
    let today = fresh_today_rfc3339();
    let dir = make_config_dir_with_key(&key, "cached-client", &["memory:read"], false, &today);
    let cache = KeyRegistryCache::new();
    let config_path = dir.path().join("config.toml");

    unsafe { std::env::set_var("DUDUCLAW_MCP_API_KEY", &key) };

    let first =
        authenticate_from_env_cached(dir.path(), &cache).expect("first call authenticates");
    assert_eq!(first.client_id, "cached-client");

    // Make the file unreadable WITHOUT changing its mtime.
    let original_perms = std::fs::metadata(&config_path).unwrap().permissions();
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o000)).unwrap();

    let second = authenticate_from_env_cached(dir.path(), &cache);

    // Restore permissions before any panic/cleanup can trip over it.
    std::fs::set_permissions(&config_path, original_perms).unwrap();
    unsafe { std::env::remove_var("DUDUCLAW_MCP_API_KEY") };

    let second = second.expect(
        "an unchanged mtime must serve the cached registry, not attempt (and fail) a fresh read",
    );
    assert_eq!(second.client_id, "cached-client");
}

/// Fail-closed reload: once a reload is triggered (mtime changed) and the
/// new content is malformed TOML, the call must be DENIED — never fall
/// back to whatever principal was cached from the last good load.
#[test]
fn cached_auth_fails_closed_when_reload_hits_malformed_toml() {
    let _guard = ENV_LOCK.lock().unwrap();
    let key = fresh_key("prod");
    let today = fresh_today_rfc3339();
    let dir = make_config_dir_with_key(&key, "good-client", &["memory:read"], false, &today);
    let cache = KeyRegistryCache::new();
    let config_path = dir.path().join("config.toml");

    unsafe { std::env::set_var("DUDUCLAW_MCP_API_KEY", &key) };

    let first =
        authenticate_from_env_cached(dir.path(), &cache).expect("first call authenticates");
    assert_eq!(first.client_id, "good-client");

    // Corrupt the file (unterminated table header) and bump its mtime so
    // a reload is triggered.
    std::fs::write(
        &config_path,
        "[mcp_keys.\"broken\n\nnot valid toml at all {{{{",
    )
    .unwrap();
    bump_mtime(&config_path, 5);

    let second = authenticate_from_env_cached(dir.path(), &cache);
    unsafe { std::env::remove_var("DUDUCLAW_MCP_API_KEY") };

    assert_eq!(
        second.unwrap_err(),
        AuthError::ReloadFailed,
        "a broken reload must deny outright, never reuse the previously-cached good principal"
    );
}

/// Fail-closed reload with the file becoming genuinely unreadable
/// (permission denied) after a mtime change — same contract as the
/// malformed-TOML case above, exercised via a real I/O error instead of a
/// parse error.
#[cfg(unix)]
#[test]
fn cached_auth_fails_closed_when_reload_hits_io_error() {
    use std::os::unix::fs::PermissionsExt;

    let _guard = ENV_LOCK.lock().unwrap();
    let key = fresh_key("prod");
    let today = fresh_today_rfc3339();
    let dir = make_config_dir_with_key(&key, "good-client", &["memory:read"], false, &today);
    let cache = KeyRegistryCache::new();
    let config_path = dir.path().join("config.toml");

    unsafe { std::env::set_var("DUDUCLAW_MCP_API_KEY", &key) };

    let first =
        authenticate_from_env_cached(dir.path(), &cache).expect("first call authenticates");
    assert_eq!(first.client_id, "good-client");

    // Touch the file (new content, so mtime genuinely changes) then
    // revoke read permission entirely.
    std::fs::write(&config_path, "# still readable for a moment\n").unwrap();
    bump_mtime(&config_path, 5);
    let original_perms = std::fs::metadata(&config_path).unwrap().permissions();
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o000)).unwrap();

    let second = authenticate_from_env_cached(dir.path(), &cache);

    std::fs::set_permissions(&config_path, original_perms).unwrap();
    unsafe { std::env::remove_var("DUDUCLAW_MCP_API_KEY") };

    assert_eq!(
        second.unwrap_err(),
        AuthError::ReloadFailed,
        "an I/O error on reload must deny outright, never reuse the previously-cached principal"
    );
}
