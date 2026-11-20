//! R8d D1: the root host entitlement a protected employee needs, derived from
//! its APPROVED settings (the confirmation item's `settings`, the full
//! `AgentConfig` JSON; never the live `agent.toml`) and the signed runtimes
//! of the fresh gateway projection.
//!
//! The rule (the exact set of profiles the gateway may later ask spawnd to
//! derive for this employee):
//! 1. the PRIMARY runtime, `runtime.provider` resolved exactly as
//!    `runtime_config::load_runtime_settings` resolves it (absent or unknown ⇒
//!    the default runtime): the one signed profile of its isolation kind
//!    (`RootPolicy::derive` refuses an ambiguous kind, so a kind with two
//!    signed profiles counts as missing). None ⇒ [`DesiredError::RuntimeNotInIsolation`];
//! 2. the `runtime.fallback` runtime, likewise; without a signed profile it
//!    is skipped (reported in [`Desired::skipped_fallbacks`]), not fatal;
//! 3. every helper the gateway's producers ask spawnd to derive for a run of
//!    this employee: the in-run helpers (`RootPolicy::derive_helper`, the
//!    `isolation_execution_program` kinds: program, code map, recording,
//!    office scripts) and the fork snapshot helper (`RootPolicy::derive` of
//!    `fork_helper::RUNTIME`), each kind only when exactly one signed profile
//!    has it. The gateway projection carries no launcher records, so the
//!    helpers are not narrowed per parent here; root's `helper_table` still
//!    intersects them with the parent's signed launcher.
//!
//! `max_turns` is always `None`: the only per-employee step setting in the
//! approved settings, `runtime.max_turns`, is the gateway's own wish that
//! `claude_max_turns` narrows into the signed range per run, not a root
//! ceiling (R6 never fed the entitlement ceiling from it).
// Only the Linux gateway allocates protected employees.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]
use duduclaw_isolation::execution::RuntimeKind;
use duduclaw_migration::allocation::Entitlement;

/// Stable reason code: the primary runtime has no signed isolation profile.
pub(crate) const RUNTIME_NOT_IN_ISOLATION: &str = "runtime_not_available_in_isolation";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Desired {
    pub(crate) entitlement: Entitlement,
    /// Fallback runtime ids without a signed profile (skipped, not fatal).
    pub(crate) skipped_fallbacks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DesiredError {
    /// [`RUNTIME_NOT_IN_ISOLATION`]: `runtime` is the primary runtime id.
    RuntimeNotInIsolation { runtime: String },
    /// The signed runtimes could not be read (retry later).
    Unavailable,
}

/// The isolation runtime kind the gateway launches for `runtime` (the same
/// mapping as its protected launch paths: Claude, the native Codex/Grok
/// adapters, the API worker, and `generic-cli-v1.<id>` for every other CLI).
pub(crate) fn isolation_kind(runtime: duduclaw_core::types::RuntimeType) -> Option<RuntimeKind> {
    use duduclaw_core::types::RuntimeType;
    Some(match runtime {
        RuntimeType::Claude => RuntimeKind::Claude,
        RuntimeType::Codex => RuntimeKind::Codex,
        RuntimeType::Grok => RuntimeKind::Grok,
        RuntimeType::OpenAiCompat => RuntimeKind::OpenAiCompat,
        other => RuntimeKind::Registered(duduclaw_isolation::bounded::Text::new(format!("generic-cli-v1.{}", other.as_str())).ok()?),
    })
}

/// The in-run helper kinds the gateway's `execute_program` / `code_map` /
/// `office_script` / browser recording producers request
/// (`isolation_execution_program::helper_runtime` delegates here).
pub(crate) fn in_run_helper_kind(runtime: &RuntimeKind) -> bool {
    use duduclaw_execution::{code_map_helper, office_helper, program_helper::PROGRAM_RUNTIME, recording_helper};
    match runtime {
        RuntimeKind::Registered(name) => name.as_str() == PROGRAM_RUNTIME || name.as_str() == code_map_helper::RUNTIME
            || name.as_str() == recording_helper::RUNTIME
            || name.as_str().strip_prefix("duduclaw-office-").and_then(|v| v.strip_suffix("-v1"))
                .and_then(|v| v.split_once('-')).is_some_and(|(skill, script)| office_helper::script_id(skill, script).is_ok()),
        _ => false,
    }
}

/// Every helper kind (rule 3). R8n-1: every other signed runtime is a main
/// runtime (the readiness probe's "main runtime kinds").
pub(crate) fn helper_kind(runtime: &RuntimeKind) -> bool {
    in_run_helper_kind(runtime)
        || matches!(runtime, RuntimeKind::Registered(name) if name.as_str() == duduclaw_execution::fork_helper::RUNTIME)
}

#[cfg(target_os = "linux")]
use duduclaw_spawnd::policy::PublishedRuntime;

/// The one signed profile of `kind` (two or more: ambiguous, none).
#[cfg(target_os = "linux")]
fn single(runtimes: &[PublishedRuntime], kind: &RuntimeKind) -> Option<duduclaw_isolation::egress::ProfileId> {
    let mut matching = runtimes.iter().filter(|runtime| runtime.runtime == *kind);
    let first = matching.next()?;
    if matching.next().is_some() { return None; }
    Some(first.profile)
}

/// The desired entitlement of `settings` (rules 1–3, in that order; no
/// duplicates; helpers in signed order).
#[cfg(target_os = "linux")]
pub(crate) fn desired(runtimes: &[PublishedRuntime], settings: &serde_json::Value) -> Result<Desired, DesiredError> {
    use duduclaw_core::types::RuntimeType;
    let field = |key: &str| settings.get("runtime").and_then(|section| section.get(key)).and_then(serde_json::Value::as_str);
    let primary = field("provider").and_then(RuntimeType::parse).unwrap_or_default();
    let fallback = field("fallback").and_then(RuntimeType::parse).filter(|fallback| *fallback != primary);
    let mut profiles: Vec<duduclaw_isolation::egress::ProfileId> = Vec::new();
    let mut add = |profile: duduclaw_isolation::egress::ProfileId| { if !profiles.contains(&profile) { profiles.push(profile); } };
    let first = isolation_kind(primary).and_then(|kind| single(runtimes, &kind))
        .ok_or_else(|| DesiredError::RuntimeNotInIsolation { runtime: primary.as_str().to_owned() })?;
    add(first);
    let mut skipped_fallbacks = Vec::new();
    if let Some(fallback) = fallback {
        match isolation_kind(fallback).and_then(|kind| single(runtimes, &kind)) {
            Some(profile) => add(profile),
            None => skipped_fallbacks.push(fallback.as_str().to_owned()),
        }
    }
    for runtime in runtimes {
        if helper_kind(&runtime.runtime) && single(runtimes, &runtime.runtime) == Some(runtime.profile) { add(runtime.profile); }
    }
    Ok(Desired { entitlement: Entitlement { profiles, max_turns: None }, skipped_fallbacks })
}

#[cfg(all(test, target_os = "linux"))]
pub(crate) mod tests {
    use super::*;
    use duduclaw_isolation::{bounded::Text, egress::ProfileId, execution::{DiskQuota, NetworkPolicy, ResourceLimits, RunTimeouts},
        ids::Sha256Digest, ipc::QueueClass};
    use duduclaw_spawnd::policy::Executable;
    pub(crate) fn profile(byte: u8) -> ProfileId { ProfileId(Sha256Digest([byte; 32])) }
    pub(crate) fn signed(byte: u8, kind: RuntimeKind) -> PublishedRuntime {
        PublishedRuntime { runtime: kind, profile: profile(byte),
            executable: Executable::Elf { entry: Text::new("runtime".into()).unwrap(), digest: Sha256Digest([1; 32]) },
            pinned_version: Text::new("1.0.0".into()).unwrap(), fixed_args: vec![],
            resources: ResourceLimits { memory_bytes: 1 << 20, cpu_quota_percent: 100, tasks: 16, open_files: 64, ptys: 0,
                disk: DiskQuota::LoopFilesystem { bytes: 1024, inodes: 32 }, io_read_bytes_per_sec: 1024, io_write_bytes_per_sec: 1024,
                io_read_ops_per_sec: 10, io_write_ops_per_sec: 10, output_buffer_bytes: 4096, output_total_bytes: 4096,
                memfd_total_bytes: 4096 },
            timeouts: RunTimeouts { queue_ms: 1, startup_ms: 1, runtime_ms: 1, pty_idle_ms: None, termination_grace_ms: 1 },
            network: NetworkPolicy::Offline, mcp: true, pty_allowed: false, queue_class: QueueClass::Background, turn_limit: None }
    }
    pub(crate) fn registered(name: &str) -> RuntimeKind { RuntimeKind::Registered(Text::new(name.into()).unwrap()) }
    /// Claude 1, Codex 2, generic Qwen 3, program helper 10, code map 11, fork 12.
    pub(crate) fn host() -> Vec<PublishedRuntime> {
        vec![signed(1, RuntimeKind::Claude), signed(2, RuntimeKind::Codex), signed(3, registered("generic-cli-v1.qwen")),
            signed(10, registered(duduclaw_execution::program_helper::PROGRAM_RUNTIME)),
            signed(11, registered(duduclaw_execution::code_map_helper::RUNTIME)),
            signed(12, registered(duduclaw_execution::fork_helper::RUNTIME))]
    }
    fn settings(provider: Option<&str>, fallback: Option<&str>) -> serde_json::Value {
        let mut runtime = serde_json::Map::new();
        if let Some(provider) = provider { runtime.insert("provider".into(), provider.into()); }
        if let Some(fallback) = fallback { runtime.insert("fallback".into(), fallback.into()); }
        serde_json::json!({ "agent": { "name": "sales" }, "runtime": runtime })
    }
    fn profiles(bytes: &[u8]) -> Vec<ProfileId> { bytes.iter().map(|byte| profile(*byte)).collect() }

    #[test]
    fn desired_set_derivation_table() {
        let helpers = [10, 11, 12];
        let with = |first: &[u8]| -> Vec<ProfileId> { profiles(&[first, &helpers[..]].concat()) };
        // (provider, fallback) -> (profiles, skipped fallbacks)
        let rows: Vec<(Option<&str>, Option<&str>, Vec<ProfileId>, Vec<&str>)> = vec![
            (Some("claude"), None, with(&[1]), vec![]),
            // Absent provider: the default runtime, as the gateway runs it.
            (None, None, with(&[1]), vec![]),
            // An unknown provider falls back to the default exactly like `load_runtime_settings`.
            (Some("no-such-cli"), None, with(&[1]), vec![]),
            (Some("codex"), Some("claude"), with(&[2, 1]), vec![]),
            (Some("qwen"), None, with(&[3]), vec![]),
            // One fallback without a signed profile: skipped, not fatal.
            (Some("claude"), Some("grok"), with(&[1]), vec!["grok"]),
            // A fallback equal to the primary adds nothing.
            (Some("claude"), Some("claude"), with(&[1]), vec![]),
        ];
        for (provider, fallback, expected, skipped) in rows {
            let desired = desired(&host(), &settings(provider, fallback)).unwrap();
            assert_eq!(desired.entitlement.profiles, expected, "{provider:?} {fallback:?}");
            assert_eq!(desired.entitlement.max_turns, None);
            assert_eq!(desired.skipped_fallbacks, skipped, "{provider:?} {fallback:?}");
        }
    }

    #[test]
    fn primary_runtime_without_a_signed_profile_is_refused() {
        for provider in ["grok", "openai_compat", "kimi"] {
            assert_eq!(desired(&host(), &settings(Some(provider), Some("claude"))).unwrap_err(),
                DesiredError::RuntimeNotInIsolation { runtime: provider.into() }, "{provider}");
        }
        // Two signed profiles of the primary kind: ambiguous, so root could not derive it either.
        let mut ambiguous = host(); ambiguous.push(signed(20, RuntimeKind::Claude));
        assert!(matches!(desired(&ambiguous, &settings(Some("claude"), None)), Err(DesiredError::RuntimeNotInIsolation { .. })));
    }

    #[test]
    fn helper_profiles_follow_the_published_helper_kinds() {
        // No helper published: the main runtimes only.
        let bare = vec![signed(1, RuntimeKind::Claude)];
        assert_eq!(desired(&bare, &settings(None, None)).unwrap().entitlement.profiles, profiles(&[1]));
        // A non-helper registered runtime is never added as a helper.
        let mut other = bare.clone(); other.push(signed(4, registered("generic-cli-v1.kimi")));
        assert_eq!(desired(&other, &settings(None, None)).unwrap().entitlement.profiles, profiles(&[1]));
        // An ambiguous helper kind is left out (root's derive_helper refuses ambiguity).
        let mut doubled = host(); doubled.push(signed(13, registered(duduclaw_execution::code_map_helper::RUNTIME)));
        assert_eq!(desired(&doubled, &settings(None, None)).unwrap().entitlement.profiles, profiles(&[1, 10, 12]));
        assert!(in_run_helper_kind(&registered(duduclaw_execution::recording_helper::RUNTIME)));
        assert!(!in_run_helper_kind(&registered(duduclaw_execution::fork_helper::RUNTIME)));
        assert!(!in_run_helper_kind(&RuntimeKind::Claude));
    }

    /// R8n-1 1.5: a signed policy with zero helper kinds (the isolated layout
    /// without client-registered helpers) yields exactly the main profile(s),
    /// primary and fallback, and never fails because helpers are absent.
    #[test]
    fn r8n_zero_helpers_yield_exactly_the_main_profiles() {
        let mains = vec![signed(1, RuntimeKind::Claude), signed(2, RuntimeKind::Codex), signed(3, registered("generic-cli-v1.qwen"))];
        assert!(mains.iter().all(|runtime| !helper_kind(&runtime.runtime)));
        for (provider, fallback, expected) in [(None, None, vec![1u8]), (Some("codex"), Some("claude"), vec![2, 1]),
            (Some("qwen"), Some("codex"), vec![3, 2]), (Some("claude"), Some("grok"), vec![1])] {
            let desired = desired(&mains, &settings(provider, fallback)).unwrap();
            assert_eq!(desired.entitlement.profiles, profiles(&expected), "{provider:?} {fallback:?}");
            assert_eq!(desired.entitlement.max_turns, None);
        }
        // The primary still has to be signed: absent helpers never mask that.
        assert!(matches!(desired(&mains, &settings(Some("grok"), None)), Err(DesiredError::RuntimeNotInIsolation { .. })));
    }
}
