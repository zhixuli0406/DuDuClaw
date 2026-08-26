pub mod agent_guard;
pub mod agent_rename;
pub mod agent_toml;
pub mod appliance;
pub mod autostart;
pub mod concurrency_gate;
pub mod config;
pub mod cron_tz;
pub mod data_migrations;
pub mod delegation_policy;
pub mod department;
pub mod dispatch_guard;
pub mod error;
pub mod fs_lock;
pub mod grounding;
pub mod identity_token;
pub mod keychain;
pub mod lenient;
pub mod match_utils;
pub mod mcp_scopes;
pub mod org;
pub mod org_field_guard;
pub mod org_store;
pub mod platform;
pub mod preset;
pub mod provider_env;
pub mod relay_protocol;
pub mod sensitivity;
pub mod spawn_admission;
pub mod spawn_env;
pub mod takeover_state;
pub mod text_utils;
pub mod tool_catalog;
pub mod toml_merge;
pub mod traits;
pub mod types;
pub mod zh_variant;

pub use agent_guard::{check_agent_file_write, check_bash_command, GuardDecision, AGENT_STRUCTURE_FILES};
pub use agent_rename::{rename_in_markdown, synced_trigger};
pub use appliance::{appliance_default_bind, appliance_flag, is_appliance, pick_default_bind, APPLIANCE_ENV};
pub use concurrency_gate::{
    active_count as concurrency_active_count, effective_limit as concurrency_effective_limit,
    release as concurrency_release, renew as concurrency_renew, try_acquire as concurrency_try_acquire,
    AcquireOutcome as ConcurrencyAcquireOutcome, ConcurrencyGateConfig, Lease as ConcurrencyLease,
};
pub use config::{
    gateway_bind_for_home, gateway_port_for_home, read_gateway_raw_settings, resolve_gateway_bind,
    resolve_gateway_port, write_minimal_config, GatewaySettingSource,
};
pub use cron_tz::{parse_timezone, should_fire_in_tz};
pub use delegation_policy::{
    can_delegate, can_delegate_ext, can_delegate_rules, can_delegate_rules_ext,
    delegation_rules_from_home, is_ancestor as is_org_ancestor, is_reserved_agent_id,
    is_system_sender,
    policy_from_home as delegation_policy_from_home, require_identity_token_from_home,
    resolve_allow_pairs, DelegationConfig,
    DelegationDenied, DelegationPolicy, DelegationRules, DenyReason as DelegationDenyReason,
    MapOrgView, OrgNode, OrgView, ACP_CLIENT_SENDER, MAX_ANCESTOR_HOPS, SYSTEM_SENDERS,
};
pub use department::{
    department_of_page, department_page_visible, is_valid_department,
    namespace_department_visible, top_level_namespace as wiki_top_level_namespace,
    DepartmentVisibilityPolicy, DEPARTMENTS_NAMESPACE,
};
pub use dispatch_guard::{
    check_and_record as dispatch_guard_check, DispatchGuardConfig, DispatchGuardDecision,
};
pub use error::{DuDuClawError, Result};
pub use fs_lock::with_file_lock;
pub use grounding::{
    check_grounded, is_self_echo_tool, matching_result_texts, shares_contiguous_run,
    shares_contiguous_run_excluding_echo, tool_name_matches, GroundingOutcome,
    SELF_ECHO_TOOL_NAMES, ToolEvidence,
};
pub use identity_token::{
    agent_identity_env_vars, agent_identity_env_vars_default, ensure_key as ensure_identity_key,
    identity_key_path, load_key as load_identity_key, mint_token as mint_identity_token,
    verify_claim as verify_identity_claim, verify_env_identity, verify_token as verify_identity_token,
    IdentityVerdict, ENV_AGENT_TOKEN, IDENTITY_KEY_FILE, UNTRUSTED_AGENT_ID,
};
pub use keychain::{resolve_master_key, KeychainError, MasterKeySource};
pub use match_utils::{is_valid_discord_snowflake, is_valid_egress_host, origin_host_matches, word_contains_ci};
pub use org_field_guard::{
    check_bash_protected_write, check_caller_scope, check_identity_surface_write,
    check_own_soul_write, check_protected_toml_write, classify_identity_surface,
    classify_protected_toml, HookCaller, ProtectedSurface, ProtectedTomlKind, AGENT_ORG_FIELDS,
    CONFIG_PROTECTED_SECTIONS,
};
pub use org_store::{
    OrgDrift, OrgEntry, OrgStore, OrgSyncChange, ORG_SEEDED_FILE, ORG_STORE_FILE, ORG_STORE_SCHEMA,
};
pub use platform::{duduclaw_home, duduclaw_instance, expand_tilde, home_dir, mcp_server_key};
pub use provider_env::{
    provider_env_key_names, resolve_env_key as resolve_provider_env_key, KNOWN_PROVIDER_IDS,
};
pub use sensitivity::{is_private_session, perception_source_sensitivity, Sensitivity};
pub use spawn_admission::{
    clamp_min_one as spawn_admission_clamp_min_one, dequeue_next as spawn_admission_dequeue_next,
    enqueue as spawn_admission_enqueue, invalidate_owner as spawn_admission_invalidate_owner,
    queue_depth as spawn_admission_queue_depth, sweep_expired as spawn_admission_sweep_expired,
    AdmissionConfig as SpawnAdmissionConfig, AdmissionMode as SpawnAdmissionMode,
    DequeueResult as SpawnDequeueResult, EnqueueOutcome as SpawnEnqueueOutcome,
    QueuedSpawn,
};
pub use spawn_env::{
    agent_cli_spawn_env_pairs, agent_cli_spawn_env_pairs_for, apply_agent_cli_env_allowlist,
    apply_agent_cli_env_allowlist_for, git_credentials_env_pairs, git_credentials_granted_names,
    AGENT_CLI_ENV_ALLOWLIST, GIT_CREDENTIALS_ENV_ALLOWLIST,
};
pub use takeover_state::{
    BeginOutcome as TakeoverBeginOutcome, BeginRequest as TakeoverBeginRequest, TakeoverConfig,
    TakeoverRecord,
};
pub use text_utils::{truncate_bytes, truncate_chars};
pub use tool_catalog::{builtin_tool_catalog, ToolCatalogEntry};
pub use traits::{Channel, ContainerRuntime, MemoryEngine};
pub use types::*;
pub use zh_variant::{contains_simplified, dominant_variant, to_traditional, ChineseVariant};

// ── Delegation safety constants ──────────────────────────────

/// Maximum number of agent-to-agent delegation hops before messages are
/// dropped.  Shared across MCP tools (pre-check) and the bus dispatcher
/// (runtime guard).
pub const MAX_DELEGATION_DEPTH: u8 = 5;

/// Environment variable names used to inject delegation context into
/// Claude CLI subprocesses.  The MCP server reads these to track depth
/// without relying on (spoofable) tool parameters.
pub const ENV_DELEGATION_DEPTH: &str = "DUDUCLAW_DELEGATION_DEPTH";
pub const ENV_DELEGATION_ORIGIN: &str = "DUDUCLAW_DELEGATION_ORIGIN";
pub const ENV_DELEGATION_SENDER: &str = "DUDUCLAW_DELEGATION_SENDER";

/// Cascade **hop depth** for the goal-loop / feedback path (paper 2607.01641,
/// "Agent tool reentry"). Distinct from `delegation_depth`: that bounds direct
/// agent→agent delegation chains inside one MCP call; `hop_depth` rides the bus
/// task across the dispatcher's re-spawn boundary so a re-generating feedback
/// loop (dispatch → agent → spawn → dispatch …) inherits — never resets — its
/// depth. The dispatcher injects the current task's value via [`ENV_HOP_DEPTH`];
/// the MCP server reads it, writes `hop_depth = value + 1` onto the next bus
/// task, and rejects once it exceeds [`DEFAULT_MAX_HOP_DEPTH`]
/// (overridable via `config.toml [dispatch_guard] max_hop_depth`).
pub const ENV_HOP_DEPTH: &str = "DUDUCLAW_HOP_DEPTH";

/// Default cascade hop-depth ceiling (config-overridable). Exceeding it rejects
/// the delegating call with an explicit "委派鏈過深" error.
pub const DEFAULT_MAX_HOP_DEPTH: u8 = 5;

/// Agent identity injected into Claude CLI subprocesses via per-agent
/// `.mcp.json` so the MCP server knows *which* agent is the current
/// caller for supervisor-relation authorization.
///
/// Without this, the MCP server falls back to
/// `config.toml [general] default_agent` — which is the global default
/// and causes cross-agent delegations to be mis-authorized (e.g. a TL
/// sub-agent spawning its own sub-agent gets rejected because the MCP
/// thinks the caller is the top-level default agent, not TL).
///
/// Populated automatically at gateway startup; see
/// `duduclaw_agent::mcp_template::ensure_duduclaw_absolute_path`.
pub const ENV_AGENT_ID: &str = "DUDUCLAW_AGENT_ID";

/// MCP server authentication (M6 fail-closed): `duduclaw mcp-server` refuses
/// to start unless this env var carries a key registered in `config.toml
/// [mcp_keys]` (or the explicit unauthenticated opt-in below is set). The
/// gateway provisions an internal key at startup and every CLI-runtime MCP
/// config writer forwards it, because CLIs like Grok spawn MCP children with
/// ONLY the declared env block (no parent-env inheritance) — an env block
/// without this var means the duduclaw MCP server dies on boot and the agent
/// silently loses its whole tool surface.
pub const ENV_MCP_API_KEY: &str = "DUDUCLAW_MCP_API_KEY";

/// Explicit operator opt-in to run the MCP server without key auth (M6).
pub const ENV_MCP_ALLOW_UNAUTHENTICATED: &str = "DUDUCLAW_MCP_ALLOW_UNAUTHENTICATED";

/// Process-global override for the internal MCP API key, set once by the
/// gateway after provisioning it in `config.toml [mcp_keys]`. Used instead of
/// `std::env::set_var` (unsafe/UB-prone once the tokio runtime is
/// multi-threaded): every MCP env assembly point goes through
/// [`mcp_forward_env_vars`], which folds this override in, so mutating the
/// real process env is unnecessary. An operator-provided
/// `DUDUCLAW_MCP_API_KEY` env var always wins over this override.
static INTERNAL_MCP_API_KEY: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Record the gateway-provisioned internal MCP API key for this process.
/// First call wins; later calls are no-ops (the key never rotates mid-run).
pub fn set_internal_mcp_api_key(key: String) {
    let _ = INTERNAL_MCP_API_KEY.set(key);
}

/// The env vars every spawned `duduclaw mcp-server` child needs forwarded
/// from the current process, as `(name, value)` pairs (present + non-empty
/// only). Single source of truth for ALL MCP env assembly points — the
/// per-runtime config writers (grok/codex/gemini/antigravity), the Claude
/// `.mcp.json` template, and the direct tool-loop client. `DUDUCLAW_AGENT_ID`
/// is intentionally NOT included (it is per-call, not process-wide).
///
/// History: each assembly point used to hand-roll its own subset; all of them
/// missed `DUDUCLAW_MCP_API_KEY`, so every non-Claude runtime lost its MCP
/// tool surface after the M6 fail-closed auth change (v1.31).
pub fn mcp_forward_env_vars() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for var in [
        "DUDUCLAW_HOME",
        "DUDUCLAW_PORT",
        "DUDUCLAW_INSTANCE",
        ENV_MCP_API_KEY,
        ENV_MCP_ALLOW_UNAUTHENTICATED,
    ] {
        if let Ok(v) = std::env::var(var) {
            if !v.trim().is_empty() {
                out.push((var.to_string(), v));
                continue;
            }
        }
        // Env absent/empty: fall back to the gateway-provisioned internal key.
        if var == ENV_MCP_API_KEY {
            if let Some(k) = INTERNAL_MCP_API_KEY.get() {
                out.push((var.to_string(), k.clone()));
            }
        }
    }
    out
}

/// Channel context for delegation callback.
/// Format: `<channel_type>:<channel_id>[:<thread_id>]`
/// e.g. `telegram:12345` or `discord:thread:98765`
///
/// Set by channel handlers before spawning CLI sessions.
/// Read by `send_to_agent` MCP tool to record a callback so the
/// dispatcher can forward sub-agent responses back to the originating channel.
pub const ENV_REPLY_CHANNEL: &str = "DUDUCLAW_REPLY_CHANNEL";

/// Wiki RL Trust Feedback context for sub-agent dispatch (v1.10).
///
/// `DUDUCLAW_TURN_ID` carries the per-turn ULID used as the
/// `CitationTracker` drain key. `DUDUCLAW_SESSION_ID` carries the channel
/// session id used as the per-conversation cap budget key. Set by the
/// gateway when spawning Claude CLI; read by the MCP server when enqueueing
/// `send_to_agent` bus messages so sub-agent RAG citations attribute back
/// to the originating turn.
pub const ENV_TRUST_TURN_ID: &str = "DUDUCLAW_TURN_ID";
pub const ENV_TRUST_SESSION_ID: &str = "DUDUCLAW_SESSION_ID";

/// `working_state` key used for the agent-body update vertical slice's
/// cross-restart result report handshake (Y8-3, T1 —
/// `commercial/docs/DESIGN-agent-body-update-2026-08.md` §13). Shared here
/// (rather than hardcoded as the same string literal in two crates) because
/// both the writer (`duduclaw-cli`'s `mcp_os_ops.rs`, inside the per-session
/// `duduclaw mcp-server` subprocess) and the reader (`duduclaw-gateway`'s
/// `update_report_reconcile.rs`, inside the long-running gateway process)
/// already depend on `duduclaw-core` — a single string constant is the
/// simplest common ground two different crates' JSON-shaped `working_state`
/// values can share without inventing a cross-crate struct for a single key.
pub const WORKING_STATE_KEY_PENDING_UPDATE_REPORT: &str = "pending_update_report";

/// Channel types supported for delegation callback forwarding.
/// Used by both the MCP `send_to_agent` tool and the channel_reply session filter.
pub const SUPPORTED_CHANNEL_TYPES: &[&str] = &[
    "telegram", "line", "discord", "slack", "whatsapp", "feishu", "googlechat", "teams",
    "wecom", "dingtalk",
];

/// Resolve the absolute path to the current DuDuClaw binary.
///
/// Used to populate `.mcp.json` and hook commands so Claude CLI
/// subprocesses can find the MCP server without relying on PATH
/// inheritance (which is frequently incomplete when launched from
/// launchd, Finder, or Dock).
///
/// Preference order:
/// 1. `DUDUCLAW_BIN` env var (test / override hook)
/// 2. `std::env::current_exe()` when it IS the open-source `duduclaw` binary
/// 3. A sibling `duduclaw` next to `current_exe()` — the Enterprise fix
///    (LWM D4 incident): when the running process is `duduclaw-pro`, writing
///    `current_exe()` into `.mcp.json` pointed every agent's MCP server at a
///    binary whose `mcp-server` invocation boots a second gateway and dies
///    on the port bind — agents silently lost the entire duduclaw tool
///    surface (memory / wiki / tasks) for four days. Enterprise images ship
///    both binaries side by side, so prefer the sibling `duduclaw` when the
///    current exe isn't it.
/// 4. `current_exe()` as-is (single-binary installs), else `"duduclaw"`
///    (PATH-dependent, least robust)
pub fn resolve_duduclaw_bin() -> std::path::PathBuf {
    if let Ok(override_path) = std::env::var("DUDUCLAW_BIN")
        && !override_path.is_empty()
    {
        return std::path::PathBuf::from(override_path);
    }
    let Ok(exe) = std::env::current_exe() else {
        return std::path::PathBuf::from("duduclaw");
    };
    resolve_duduclaw_bin_from_exe(&exe)
}

/// Pure half of [`resolve_duduclaw_bin`] — sibling preference given the
/// current executable path (separated so the Enterprise-container behavior
/// is unit-testable without faking `current_exe`).
pub fn resolve_duduclaw_bin_from_exe(exe: &std::path::Path) -> std::path::PathBuf {
    let is_open_source = exe
        .file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("duduclaw"));
    if !is_open_source {
        if let Some(dir) = exe.parent() {
            #[cfg(windows)]
            let sibling = dir.join("duduclaw.exe");
            #[cfg(not(windows))]
            let sibling = dir.join("duduclaw");
            if sibling.exists() {
                return sibling;
            }
        }
    }
    exe.to_path_buf()
}

/// Validate that an agent ID is safe for filesystem and log use.
///
/// A valid agent ID contains only ASCII alphanumerics (either case —
/// `is_ascii_alphanumeric()` does not restrict to lowercase, despite this
/// comment's prior wording; kept case-insensitive deliberately so agents
/// created before the stricter [`is_valid_new_agent_id`] slug convention
/// existed, or ones with a mixed-case id, still validate for path safety),
/// hyphens, and underscores; is non-empty; and is at most 64 characters
/// long. This is the broad "safe to use as a path/log component" predicate —
/// use it to validate an id that may already exist. For minting a *new*
/// agent id, see [`is_valid_new_agent_id`], which additionally enforces the
/// lowercase-slug naming convention.
pub fn is_valid_agent_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Validate a *newly minted* agent id/slug — used when creating an agent via
/// the CLI `agent create` command, the dashboard `agents.create` RPC, and the
/// MCP `create_agent` tool.
///
/// Deliberately stricter than [`is_valid_agent_id`]: a fresh agent id is
/// meant to read as a clean URL/CLI-friendly slug, not merely "safe for a
/// filesystem path". Rules: lowercase ASCII letters, digits, and hyphens
/// only (no underscore, no uppercase — `is_valid_agent_id` allows both, for
/// backward compatibility with agents created before this convention
/// existed); non-empty; at most 64 characters; must not start or end with a
/// hyphen (a leading hyphen risks being misread as a flag by any downstream
/// command that forwards the id as a bare positional argument).
///
/// WP-4I (2026-08) consolidation: this rule used to be hand-rolled
/// independently in `duduclaw-gateway::handlers`, `duduclaw-cli::mcp`, and
/// `duduclaw-cli::lib` — three copies of the same intended rule that had
/// already drifted (the `mcp.rs` copy was missing the leading/trailing-hyphen
/// guard the other two had). All three now delegate here.
pub fn is_valid_new_agent_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

#[cfg(test)]
mod agent_id_tests {
    use super::{is_valid_agent_id, is_valid_new_agent_id};

    #[test]
    fn valid_agent_id_rejects_empty() {
        assert!(!is_valid_agent_id(""));
        assert!(!is_valid_new_agent_id(""));
    }

    #[test]
    fn valid_agent_id_rejects_path_traversal() {
        for bad in ["../etc", "..", "a/b", "a\\b", "./x", "a/../b"] {
            assert!(!is_valid_agent_id(bad), "{bad} must be rejected");
            assert!(!is_valid_new_agent_id(bad), "{bad} must be rejected");
        }
    }

    #[test]
    fn valid_agent_id_rejects_cjk_and_unicode() {
        for bad in ["嘟嘟", "agent-嘟", "café", "agent\u{0}x"] {
            assert!(!is_valid_agent_id(bad), "{bad} must be rejected");
            assert!(!is_valid_new_agent_id(bad), "{bad} must be rejected");
        }
    }

    #[test]
    fn valid_agent_id_rejects_over_length() {
        assert!(!is_valid_agent_id(&"a".repeat(65)));
        assert!(is_valid_agent_id(&"a".repeat(64)));
        assert!(!is_valid_new_agent_id(&"a".repeat(65)));
        assert!(is_valid_new_agent_id(&"a".repeat(64)));
    }

    #[test]
    fn valid_agent_id_allows_mixed_case_and_underscore() {
        // Broad predicate: safe for path use even if it predates the slug
        // convention (mixed case / underscore agent ids created historically).
        assert!(is_valid_agent_id("Agent-01"));
        assert!(is_valid_agent_id("agent_01"));
        assert!(is_valid_agent_id("AGENT"));
    }

    #[test]
    fn new_agent_id_rejects_uppercase_and_underscore() {
        // Narrow predicate: new ids must be a clean lowercase slug.
        assert!(!is_valid_new_agent_id("Agent-01"));
        assert!(!is_valid_new_agent_id("agent_01"));
        assert!(!is_valid_new_agent_id("AGENT"));
    }

    #[test]
    fn new_agent_id_rejects_leading_or_trailing_hyphen() {
        assert!(!is_valid_new_agent_id("-agent"));
        assert!(!is_valid_new_agent_id("agent-"));
        assert!(is_valid_new_agent_id("agent-01"));
    }

    #[test]
    fn both_accept_ordinary_slugs() {
        for good in ["bruno", "agent-01", "a", "z9"] {
            assert!(is_valid_agent_id(good));
            assert!(is_valid_new_agent_id(good));
        }
    }
}

/// Find the `claude` binary in PATH or common locations (BE-L1, BE-M1).
///
/// Discovery sources:
/// 1. `which claude` (Unix) / `where claude` (Windows) — respects current `PATH`
/// 2. Fixed absolute candidate paths covering Homebrew (Intel + Apple Silicon),
///    Bun, Volta, npm-global, user-local installs, asdf shims (Unix) and
///    npm / pnpm / Yarn / Bun / Volta / Scoop / Claude Code native installer
///    locations (Windows)
/// 3. NVM glob expansion (`$HOME/.nvm/versions/node/*/bin/claude`)
///
/// **Windows precedence (CRITICAL — fixes BatBadBut / CVE-2024-24576):**
///
/// Discoveries from sources 1 + 2 are pooled, then ranked **`.exe` ahead of
/// `.cmd`** regardless of source. Spawning a `.exe` is always safe; spawning
/// a `.cmd` triggers Rust's BatBadBut rejection when args contain newlines /
/// quotes / `&` (which user prompts and system prompts routinely do). So a
/// host with both `~/.local/bin/claude.exe` (clean) and a leftover
/// `%APPDATA%\npm\claude.cmd` (BatBadBut hazard) MUST resolve to the `.exe`
/// even when `where.exe claude` returns the `.cmd` first.
///
/// On Unix, the order is preserved (PATH first, then HOME).
///
/// When gateway is launched from launchd / Finder / Dock, `PATH` frequently
/// omits Homebrew and Node version-manager paths, so the fixed candidates
/// are critical for zero-config install discovery.
pub fn which_claude() -> Option<String> {
    // ── 1. Discover via PATH ─────────────────────────────────────
    let mut path_results: Vec<String> = Vec::new();
    let lookup_cmd = if cfg!(windows) { "where" } else { "which" };
    if let Ok(output) = std::process::Command::new(lookup_cmd)
        .arg("claude")
        .output()
        && output.status.success()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty() && std::path::Path::new(trimmed).exists() {
                path_results.push(trimmed.to_string());
            }
        }
    }

    // ── 2-3. Discover via HOME-rooted scan ──────────────────────
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default();
    let home_result = which_claude_in_home(std::path::Path::new(&home));

    // Combine in source order: PATH first (user's explicit env), then HOME.
    let mut all: Vec<String> = path_results;
    if let Some(h) = home_result {
        if !all.contains(&h) {
            all.push(h);
        }
    }

    if all.is_empty() {
        log_resolved_claude_path_once(None, &[]);
        return None;
    }

    // Unix: prefer the NEWEST binary when several installs coexist (a stale
    // /usr/local/bin/claude shadowing a current nvm install makes agents run
    // an outdated CLI); ties / unversioned candidates keep source order.
    // Windows: pick by .exe > .cmd > extensionless precedence (BatBadBut).
    #[cfg(not(windows))]
    let chosen: Option<String> = pick_newest_version(&all);
    #[cfg(windows)]
    let chosen: Option<String> = pick_windows_preferred(&all);

    log_resolved_claude_path_once(chosen.as_deref(), &all);
    chosen
}

/// Parse the first `MAJOR.MINOR.PATCH` triple out of a `--version` line.
fn parse_semver_triple(s: &str) -> Option<(u64, u64, u64)> {
    for token in s.split(|c: char| c.is_whitespace() || c == '(' || c == ')') {
        let mut parts = token.split('.');
        if let (Some(a), Some(b), Some(c)) = (parts.next(), parts.next(), parts.next())
            && let (Ok(a), Ok(b), Ok(c)) = (a.parse::<u64>(), b.parse::<u64>(), c.parse::<u64>())
        {
            return Some((a, b, c));
        }
    }
    None
}

/// Run `<path> --version` with a 2s poll-timeout; a wedged binary is killed
/// and treated as unversioned rather than hanging gateway startup.
fn probe_binary_version(path: &str) -> Option<(u64, u64, u64)> {
    let mut child = std::process::Command::new(path)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = String::new();
    use std::io::Read;
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    parse_semver_triple(&out)
}

/// Among candidate paths (source order), pick the highest `--version`;
/// versioned candidates beat unversioned ones; all-unversioned keeps the
/// first (legacy source-order behavior). Only runs when >1 candidate, so
/// the common single-install case pays zero probing cost.
#[cfg(any(unix, test))]
fn pick_newest_version(all: &[String]) -> Option<String> {
    if all.len() <= 1 {
        return all.first().cloned();
    }
    let mut best: Option<(u64, u64, u64)> = None;
    let mut best_path: Option<&String> = None;
    for path in all {
        if let Some(v) = probe_binary_version(path)
            && best.is_none_or(|b| v > b)
        {
            best = Some(v);
            best_path = Some(path);
        }
    }
    best_path.cloned().or_else(|| all.first().cloned())
}

/// Windows-only precedence: `.exe` STRICTLY > `.cmd` > extensionless.
///
/// Even if PATH discovery returned `.cmd` first, an `.exe` found anywhere in
/// the pool wins. This is the **BatBadBut mitigation hinge** — losing this
/// ordering puts every channel reply at risk because Rust 1.77+ rejects
/// spawning `.bat`/`.cmd` files when args contain newlines / quotes / `&`
/// (CVE-2024-24576), which user prompts and system prompts routinely do.
///
/// Compiled cross-platform under `#[cfg(any(windows, test))]` so the
/// precedence logic can be exercised by unit tests on macOS / Linux runners.
#[cfg(any(windows, test))]
fn pick_windows_preferred(all: &[String]) -> Option<String> {
    // Pass 1: any .exe wins (safe to spawn, no BatBadBut)
    all.iter()
        .find(|c| c.to_lowercase().ends_with(".exe"))
        .cloned()
        // Pass 2: .cmd (resolve_cmd_to_node parses to node + cli.js)
        .or_else(|| {
            all.iter()
                .find(|c| c.to_lowercase().ends_with(".cmd"))
                .cloned()
        })
        // Pass 3: extensionless — try appending .exe then .cmd
        .or_else(|| {
            all.iter().find_map(|c| {
                let exe_path = format!("{c}.exe");
                if std::path::Path::new(&exe_path).exists() {
                    return Some(exe_path);
                }
                let cmd_path = format!("{c}.cmd");
                if std::path::Path::new(&cmd_path).exists() {
                    return Some(cmd_path);
                }
                None
            })
        })
        // Last resort: first entry as-is
        .or_else(|| all.first().cloned())
}

/// Emit one INFO log on the first `which_claude` call so operators can see
/// which binary the gateway resolved without needing to enable trace-level
/// logging. Subsequent calls are silent — `which_claude` is invoked many
/// times per session and noisy logs would drown out real signals.
///
/// Logs the chosen path AND the full discovery pool so we can diagnose
/// "wrong .cmd was picked" reports without round-tripping with the user.
fn log_resolved_claude_path_once(chosen: Option<&str>, pool: &[String]) {
    static LOGGED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    LOGGED.get_or_init(|| {
        match chosen {
            Some(path) => {
                tracing::info!(
                    path = %path,
                    candidates = ?pool,
                    "Resolved claude binary"
                );
            }
            None => {
                tracing::warn!(
                    "claude binary not found — checked PATH and HOME candidates"
                );
            }
        }
    });
}

/// Scan fixed absolute paths and HOME-rooted candidates for the `claude` binary.
///
/// Extracted so tests can exercise candidate discovery deterministically
/// (without depending on the ambient `PATH`, which `which_claude` consults first).
/// Returns the first candidate that exists as a real filesystem entry.
pub fn which_claude_in_home(home: &std::path::Path) -> Option<String> {
    let home_str = home.to_string_lossy();

    // Platform-specific candidates
    #[cfg(not(windows))]
    let candidates = vec![
        // macOS Apple Silicon Homebrew
        "/opt/homebrew/bin/claude".to_string(),
        // macOS Intel / Linux Homebrew
        "/usr/local/bin/claude".to_string(),
        // Bun (increasingly common for Node CLIs)
        format!("{home_str}/.bun/bin/claude"),
        // Volta
        format!("{home_str}/.volta/bin/claude"),
        // npm global (default for many Node installs)
        format!("{home_str}/.npm-global/bin/claude"),
        // Claude Code native installer
        format!("{home_str}/.claude/bin/claude"),
        // User-local
        format!("{home_str}/.local/bin/claude"),
        // asdf shim
        format!("{home_str}/.asdf/shims/claude"),
    ];

    #[cfg(windows)]
    let candidates = {
        let appdata = std::env::var("APPDATA").unwrap_or_default();
        let localappdata = std::env::var("LOCALAPPDATA").unwrap_or_default();
        vec![
            // ── .exe candidates first ────────────────────────────
            // .exe spawns cleanly via std::process::Command — no
            // BatBadBut (CVE-2024-24576) hazard. When a host has both
            // a clean .exe install AND a leftover npm .cmd shim, we
            // MUST prefer the .exe to avoid Rust 1.77+'s rejection of
            // .cmd args containing newlines / quotes / `&` etc.
            //
            // Claude Code native installer (XDG-style on Windows):
            //   ~/.local/bin/claude.exe — the most common location
            //   on machines installed via the official installer.
            format!("{home_str}\\.local\\bin\\claude.exe"),
            // Claude Code legacy / desktop-installer locations
            format!("{home_str}\\.claude\\bin\\claude.exe"),
            format!("{localappdata}\\Programs\\claude\\claude.exe"),
            // Bun on Windows
            format!("{home_str}\\.bun\\bin\\claude.exe"),
            // Volta on Windows
            format!("{home_str}\\.volta\\bin\\claude.exe"),
            // Scoop
            format!("{home_str}\\scoop\\shims\\claude.exe"),
            // pnpm global (modern default)
            format!("{localappdata}\\pnpm\\claude.exe"),
            // Yarn classic global
            format!("{localappdata}\\Yarn\\bin\\claude.exe"),

            // ── .cmd candidates (rely on resolve_cmd_to_node) ────
            // Each .cmd is parsed into (node.exe, cli.js) at spawn
            // time so we never hand args directly to cmd.exe.
            // npm global (default Windows npm install location)
            format!("{appdata}\\npm\\claude.cmd"),
            format!("{appdata}\\npm\\claude"),
            // pnpm global .cmd shim
            format!("{localappdata}\\pnpm\\claude.cmd"),
            // Yarn classic global .cmd shim
            format!("{localappdata}\\Yarn\\bin\\claude.cmd"),
            // Bun on Windows (older versions ship .cmd shims)
            format!("{home_str}\\.bun\\bin\\claude.cmd"),
            // Volta .cmd (older releases)
            format!("{home_str}\\.volta\\bin\\claude.cmd"),
            // Scoop
            format!("{home_str}\\scoop\\shims\\claude.cmd"),
            // ~/.local/bin extensionless / .cmd fallback
            format!("{home_str}\\.local\\bin\\claude.cmd"),
            format!("{home_str}\\.local\\bin\\claude"),
        ]
    };

    for path in &candidates {
        if std::path::Path::new(path).exists() {
            return Some(path.clone());
        }
    }

    // NVM: scan all node versions for claude binary
    #[cfg(not(windows))]
    {
        let nvm_root = home.join(".nvm").join("versions").join("node");
        if let Ok(entries) = std::fs::read_dir(&nvm_root) {
            for entry in entries.flatten() {
                let candidate = entry.path().join("bin").join("claude");
                if candidate.exists() {
                    return Some(candidate.to_string_lossy().to_string());
                }
            }
        }
    }

    #[cfg(windows)]
    {
        // NVM for Windows: %APPDATA%\nvm\<version>\claude.cmd
        let nvm_root = std::path::Path::new(&std::env::var("APPDATA").unwrap_or_default()).join("nvm");
        if let Ok(entries) = std::fs::read_dir(&nvm_root) {
            for entry in entries.flatten() {
                for name in ["claude.cmd", "claude.exe"] {
                    let candidate = entry.path().join(name);
                    if candidate.exists() {
                        return Some(candidate.to_string_lossy().to_string());
                    }
                }
            }
        }
    }

    None
}

// ── Generic CLI discovery (Codex / Gemini / Antigravity) ──────────────
//
// `which_claude` carries Claude-specific baggage (native-installer paths,
// Windows `.exe` > `.cmd` BatBadBut precedence). The other multi-runtime CLIs
// install through the usual package managers, so a parameterized scan over the
// common locations is sufficient. PATH is consulted first (the user's explicit
// env), then a fixed candidate list — mirroring `which_claude`'s launchd-safe
// discovery so a Finder/launchd-launched gateway finds the binary without an
// interactive `PATH`.

/// Resolve a CLI `bin` (e.g. `"codex"`, `"gemini"`, `"agy"`) via PATH then a
/// HOME-rooted candidate scan. Returns the first match that exists on disk.
pub fn which_cli(bin: &str) -> Option<String> {
    let lookup_cmd = if cfg!(windows) { "where" } else { "which" };
    if let Ok(output) = std::process::Command::new(lookup_cmd).arg(bin).output()
        && output.status.success()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty() && std::path::Path::new(trimmed).exists() {
                return Some(trimmed.to_string());
            }
        }
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default();
    which_cli_in_home(std::path::Path::new(&home), bin)
}

/// HOME-rooted candidate scan for a CLI `bin`. Extracted so tests can drive it
/// deterministically without depending on the ambient `PATH`.
pub fn which_cli_in_home(home: &std::path::Path, bin: &str) -> Option<String> {
    let home_str = home.to_string_lossy();

    #[cfg(not(windows))]
    let candidates = vec![
        // Antigravity's official installer target (also a common user-local bin).
        format!("{home_str}/.local/bin/{bin}"),
        // Homebrew (Apple Silicon, then Intel / Linux).
        format!("/opt/homebrew/bin/{bin}"),
        format!("/usr/local/bin/{bin}"),
        // Node CLI managers.
        format!("{home_str}/.bun/bin/{bin}"),
        format!("{home_str}/.volta/bin/{bin}"),
        format!("{home_str}/.npm-global/bin/{bin}"),
        format!("{home_str}/.asdf/shims/{bin}"),
    ];

    #[cfg(windows)]
    let candidates = {
        let localappdata = std::env::var("LOCALAPPDATA").unwrap_or_default();
        let appdata = std::env::var("APPDATA").unwrap_or_default();
        vec![
            // Antigravity Windows installer target.
            format!("{localappdata}\\Antigravity\\{bin}.exe"),
            // User-local + package-manager .exe shims.
            format!("{home_str}\\.local\\bin\\{bin}.exe"),
            format!("{home_str}\\.bun\\bin\\{bin}.exe"),
            format!("{home_str}\\.volta\\bin\\{bin}.exe"),
            format!("{home_str}\\scoop\\shims\\{bin}.exe"),
            // npm / pnpm .cmd shims (resolved at spawn time).
            format!("{appdata}\\npm\\{bin}.cmd"),
            format!("{localappdata}\\pnpm\\{bin}.cmd"),
        ]
    };

    for path in &candidates {
        if std::path::Path::new(path).exists() {
            return Some(path.clone());
        }
    }
    None
}

/// Resolve the `codex` CLI binary. See [`which_cli`].
pub fn which_codex() -> Option<String> {
    which_cli("codex")
}

/// Resolve the `codex` CLI from a specific HOME. See [`which_cli_in_home`].
pub fn which_codex_in_home(home: &std::path::Path) -> Option<String> {
    which_cli_in_home(home, "codex")
}

/// Resolve the `gemini` CLI binary. See [`which_cli`].
pub fn which_gemini() -> Option<String> {
    which_cli("gemini")
}

/// Resolve the `gemini` CLI from a specific HOME. See [`which_cli_in_home`].
pub fn which_gemini_in_home(home: &std::path::Path) -> Option<String> {
    which_cli_in_home(home, "gemini")
}

/// Resolve the Antigravity `agy` CLI binary. See [`which_cli`].
pub fn which_agy() -> Option<String> {
    which_cli("agy")
}

/// Resolve the `agy` CLI from a specific HOME. See [`which_cli_in_home`].
pub fn which_agy_in_home(home: &std::path::Path) -> Option<String> {
    which_cli_in_home(home, "agy")
}

/// Resolve the xAI Grok CLI binary (R4). Prefers the official `grok` ("Grok
/// Build") binary — **verified** against docs.x.ai (2026-07-13): installed via
/// `curl -fsSL https://x.ai/cli/install.sh | bash`, invoked as `grok`. Falls
/// back to the unrelated third-party `grok-cli` (`superagent-ai/grok-cli`) only
/// so a user who happens to have that installed is still discovered. See
/// [`which_cli`].
pub fn which_grok() -> Option<String> {
    which_cli("grok").or_else(|| which_cli("grok-cli"))
}

/// Resolve the Grok CLI from a specific HOME. See [`which_cli_in_home`].
pub fn which_grok_in_home(home: &std::path::Path) -> Option<String> {
    which_cli_in_home(home, "grok").or_else(|| which_cli_in_home(home, "grok-cli"))
}

#[cfg(test)]
mod resolve_bin_tests {
    use super::resolve_duduclaw_bin_from_exe;

    /// LWM D4 regression: a `duduclaw-pro` process must resolve to the
    /// sibling open-source `duduclaw` for `.mcp.json` generation — pro's
    /// `mcp-server` boots a second gateway and dies on the port bind.
    #[test]
    fn pro_exe_prefers_sibling_open_source_binary() {
        let dir = std::env::temp_dir().join(format!("ddc-bin-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Platform-correct fixture names: the resolver probes for the
        // sibling as `duduclaw.exe` on Windows (matching how the binaries
        // are actually shipped there) — extensionless fixtures made this
        // test fail on the Windows CI matrix while production was correct.
        #[cfg(windows)]
        let (pro_name, oss_name) = ("duduclaw-pro.exe", "duduclaw.exe");
        #[cfg(not(windows))]
        let (pro_name, oss_name) = ("duduclaw-pro", "duduclaw");
        let pro = dir.join(pro_name);
        std::fs::write(&pro, b"x").unwrap();
        let oss = dir.join(oss_name);
        std::fs::write(&oss, b"x").unwrap();
        assert_eq!(resolve_duduclaw_bin_from_exe(&pro), oss);
        // Open-source exe keeps itself.
        assert_eq!(resolve_duduclaw_bin_from_exe(&oss), oss);
        // Pro without a sibling keeps itself (single-binary install).
        std::fs::remove_file(&oss).unwrap();
        assert_eq!(resolve_duduclaw_bin_from_exe(&pro), pro);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod which_cli_tests {
    use super::which_cli_in_home;

    #[test]
    fn finds_agy_in_local_bin() {
        let tmp = std::env::temp_dir().join("duduclaw-which-cli-test");
        let bin_dir = tmp.join(".local").join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let agy = bin_dir.join("agy");
        std::fs::write(&agy, b"#!/bin/sh\n").unwrap();
        let found = which_cli_in_home(&tmp, "agy");
        let _ = std::fs::remove_dir_all(&tmp);
        #[cfg(not(windows))]
        assert_eq!(found.as_deref(), Some(agy.to_string_lossy().as_ref()));
        #[cfg(windows)]
        let _ = found; // Windows scans .exe, not the extensionless stub
    }

    #[test]
    fn missing_binary_returns_none() {
        let tmp = std::env::temp_dir().join("duduclaw-which-cli-empty");
        std::fs::create_dir_all(&tmp).unwrap();
        let found = which_cli_in_home(&tmp, "definitely-not-a-cli-xyz");
        let _ = std::fs::remove_dir_all(&tmp);
        assert_eq!(found, None);
    }
}

#[cfg(test)]
mod which_claude_tests {
    use super::{pick_windows_preferred, which_claude_in_home};
    use std::fs;
    use std::path::Path;

    // ── pick_windows_preferred precedence (BatBadBut hinge) ──────
    //
    // These tests verify the v1.8.32 fix: even when PATH discovery
    // returns a `.cmd` first (e.g. `where.exe claude` finds an npm
    // shim), an `.exe` discovered anywhere in the candidate pool
    // MUST win. Losing this ordering = every channel reply on
    // Windows fails with "batch file arguments are invalid".

    #[test]
    fn windows_pref_exe_beats_cmd_even_when_cmd_listed_first() {
        let pool = vec![
            "C:\\Users\\X\\AppData\\Roaming\\npm\\claude.cmd".to_string(),
            "C:\\Users\\X\\.local\\bin\\claude.exe".to_string(),
        ];
        assert_eq!(
            pick_windows_preferred(&pool).as_deref(),
            Some("C:\\Users\\X\\.local\\bin\\claude.exe"),
        );
    }

    #[test]
    fn windows_pref_picks_cmd_when_no_exe_exists() {
        let pool = vec!["C:\\Users\\X\\AppData\\Roaming\\npm\\claude.cmd".to_string()];
        assert_eq!(
            pick_windows_preferred(&pool).as_deref(),
            Some("C:\\Users\\X\\AppData\\Roaming\\npm\\claude.cmd"),
        );
    }

    #[test]
    fn windows_pref_returns_none_for_empty_pool() {
        assert!(pick_windows_preferred(&[]).is_none());
    }

    #[test]
    fn windows_pref_first_exe_wins_among_multiple_exes() {
        let pool = vec![
            "C:\\a\\claude.exe".to_string(),
            "C:\\b\\claude.exe".to_string(),
        ];
        assert_eq!(
            pick_windows_preferred(&pool).as_deref(),
            Some("C:\\a\\claude.exe"),
        );
    }

    #[test]
    fn windows_pref_first_cmd_wins_among_multiple_cmds_when_no_exe() {
        let pool = vec![
            "C:\\a\\claude.cmd".to_string(),
            "C:\\b\\claude.cmd".to_string(),
        ];
        assert_eq!(
            pick_windows_preferred(&pool).as_deref(),
            Some("C:\\a\\claude.cmd"),
        );
    }

    #[test]
    fn windows_pref_extension_check_is_case_insensitive() {
        // Some installers / users have uppercase extensions in PATHEXT order.
        let pool = vec![
            "C:\\a\\claude.CMD".to_string(),
            "C:\\b\\claude.EXE".to_string(),
        ];
        assert_eq!(
            pick_windows_preferred(&pool).as_deref(),
            Some("C:\\b\\claude.EXE"),
        );
    }

    #[test]
    fn windows_pref_falls_back_to_first_for_extensionless_when_no_fs_match() {
        // Pass 3 (FS append) misses; Pass 4 returns first entry as-is.
        let pool = vec![
            "/nonexistent/claude".to_string(),
            "/another/claude".to_string(),
        ];
        assert_eq!(
            pick_windows_preferred(&pool).as_deref(),
            Some("/nonexistent/claude"),
        );
    }

    /// Create an executable shim at `path` so `.exists()` returns true.
    fn write_shim(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"#!/bin/sh\nexit 0\n").unwrap();
        crate::platform::set_executable(path).unwrap();
    }

    /// Guard: skip tests that rely on HOME-rooted candidates winning when the
    /// host already has a system-level claude install (which takes priority).
    fn host_has_system_claude() -> bool {
        Path::new("/opt/homebrew/bin/claude").exists()
            || Path::new("/usr/local/bin/claude").exists()
    }

    // HOME-rooted discovery fixtures use extensionless shims (Unix exec model);
    // Windows discovery is extension-aware and covered by the windows_pref_* tests.
    #[cfg_attr(windows, ignore = "extensionless HOME shim discovery is Unix-only; Windows covered by windows_pref_*")]
    #[test]
    fn discovers_bun_candidate() {
        if host_has_system_claude() {
            eprintln!("skipping: host has a system claude install");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let claude = tmp.path().join(".bun/bin/claude");
        write_shim(&claude);
        let found = which_claude_in_home(tmp.path());
        assert_eq!(found.as_deref(), Some(claude.to_string_lossy().as_ref()));
    }

    #[cfg_attr(windows, ignore = "extensionless HOME shim discovery is Unix-only")]
    #[test]
    fn discovers_volta_candidate() {
        if host_has_system_claude() {
            eprintln!("skipping: host has a system claude install");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let claude = tmp.path().join(".volta/bin/claude");
        write_shim(&claude);
        let found = which_claude_in_home(tmp.path());
        assert_eq!(found.as_deref(), Some(claude.to_string_lossy().as_ref()));
    }

    #[cfg_attr(windows, ignore = "extensionless HOME shim discovery is Unix-only")]
    #[test]
    fn discovers_asdf_shim() {
        if host_has_system_claude() {
            eprintln!("skipping: host has a system claude install");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let claude = tmp.path().join(".asdf/shims/claude");
        write_shim(&claude);
        let found = which_claude_in_home(tmp.path());
        assert_eq!(found.as_deref(), Some(claude.to_string_lossy().as_ref()));
    }

    #[cfg_attr(windows, ignore = "extensionless HOME shim discovery is Unix-only")]
    #[test]
    fn discovers_npm_global() {
        if host_has_system_claude() {
            eprintln!("skipping: host has a system claude install");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let claude = tmp.path().join(".npm-global/bin/claude");
        write_shim(&claude);
        let found = which_claude_in_home(tmp.path());
        assert_eq!(found.as_deref(), Some(claude.to_string_lossy().as_ref()));
    }

    #[cfg_attr(windows, ignore = "extensionless HOME shim discovery is Unix-only")]
    #[test]
    fn nvm_version_directory_is_scanned() {
        let tmp = tempfile::tempdir().unwrap();
        let claude = tmp.path().join(".nvm/versions/node/v20.10.0/bin/claude");
        write_shim(&claude);
        let found = which_claude_in_home(tmp.path());
        // Expect the nvm candidate since no fixed candidate matches in this tempdir
        // (and /opt/homebrew won't exist under a random tempdir HOME either, unless
        // the host happens to have it — which still satisfies the contract: a valid
        // absolute path to `claude` is returned).
        let found = found.expect("should find some claude candidate");
        let path = Path::new(&found);
        assert!(path.exists(), "returned path must exist: {found}");
        assert!(
            found.ends_with("bin/claude"),
            "returned path must end with bin/claude: {found}"
        );
    }

    #[test]
    fn no_candidates_returns_none_when_no_fixed_paths_present() {
        // Only valid if the host has none of /opt/homebrew/bin/claude or
        // /usr/local/bin/claude installed. Guarded accordingly so the test
        // remains deterministic on CI and dev machines alike.
        if Path::new("/opt/homebrew/bin/claude").exists()
            || Path::new("/usr/local/bin/claude").exists()
        {
            eprintln!("skipping: host has a system claude install");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let found = which_claude_in_home(tmp.path());
        assert!(found.is_none(), "empty HOME should return None, got {:?}", found);
    }

    #[cfg_attr(windows, ignore = "extensionless HOME shim discovery is Unix-only")]
    #[test]
    fn fixed_candidate_order_bun_beats_npm_global() {
        if host_has_system_claude() {
            eprintln!("skipping: host has a system claude install");
            return;
        }
        // When both .bun/bin/claude and .npm-global/bin/claude exist,
        // Bun should win because it's earlier in the candidate list.
        let tmp = tempfile::tempdir().unwrap();
        let bun = tmp.path().join(".bun/bin/claude");
        let npm = tmp.path().join(".npm-global/bin/claude");
        write_shim(&bun);
        write_shim(&npm);
        let found = which_claude_in_home(tmp.path()).unwrap();
        assert_eq!(found, bun.to_string_lossy());
    }
}

#[cfg(test)]
mod version_pick_tests {
    use super::*;

    #[test]
    fn semver_parse_from_claude_version_line() {
        assert_eq!(parse_semver_triple("2.1.173 (Claude Code)"), Some((2, 1, 173)));
        assert_eq!(parse_semver_triple("claude 2.1.104"), Some((2, 1, 104)));
        assert_eq!(parse_semver_triple("no version here"), None);
    }

    #[test]
    fn semver_ordering_prefers_newest() {
        assert!(parse_semver_triple("2.1.173").unwrap() > parse_semver_triple("2.1.104").unwrap());
        assert!(parse_semver_triple("3.0.0").unwrap() > parse_semver_triple("2.99.99").unwrap());
    }

    #[test]
    fn single_candidate_skips_probing() {
        let all = vec!["/nonexistent/claude".to_string()];
        assert_eq!(pick_newest_version(&all), Some("/nonexistent/claude".to_string()));
    }

    #[test]
    fn all_unversioned_falls_back_to_source_order() {
        let all = vec!["/nonexistent/a".to_string(), "/nonexistent/b".to_string()];
        assert_eq!(pick_newest_version(&all), Some("/nonexistent/a".to_string()));
    }
}

#[cfg(test)]
mod mcp_forward_env_tests {
    use super::*;

    /// The internal-key override must surface through `mcp_forward_env_vars`
    /// (this is the channel every MCP env assembly point reads). Regression
    /// for the v1.31 M6 gap where no assembly point carried the key and every
    /// non-Claude runtime lost its MCP tool surface.
    #[test]
    fn forward_env_vars_include_internal_key_override() {
        set_internal_mcp_api_key("ddc_prod_0123456789abcdef0123456789abcdef".to_string());
        let vars = mcp_forward_env_vars();
        assert!(
            vars.iter().any(|(k, v)| k == ENV_MCP_API_KEY && !v.trim().is_empty()),
            "override key missing from forward set: {vars:?}"
        );
    }
}
