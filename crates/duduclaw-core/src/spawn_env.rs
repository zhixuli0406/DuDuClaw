//! Allowlisted environment for spawned agent-CLI subprocesses.
//!
//! WP-8B (credentials doctrine P3, `commercial/docs/DESIGN-credentials-doctrine-2026-08.md`
//! §1.6 / §3 P3): the primary agent-spawn paths (`claude_runner.rs`'s
//! `prepare_claude_cmd`, `channel_reply.rs`'s `spawn_claude_cli_with_env` /
//! `spawn_claude_cli_pty_with_env`, and the external-judge spawn in
//! `judge_mode.rs`) used to hand the child process the gateway's FULL
//! environment (`tokio::process::Command` inherits by default unless
//! `env_clear()` is called), including every vendor `*_API_KEY` the operator
//! configured for OTHER agents/providers. Only `duduclaw-cli-worker`'s
//! supervisor (`worker_supervisor.rs`, "Round 3 security fix MED-M5") had
//! already closed this: `env_clear()` + an explicit allowlist. This module
//! generalizes that already-shipped, already-validated pattern into a single
//! shared list so every other spawn site can adopt it instead of growing its
//! own copy.
//!
//! Nothing here is CLI-specific — it is the generic "safe base environment
//! any subprocess needs to behave like a normal program on this OS" set
//! (locate binaries, resolve the home dir, render text correctly, find a
//! writable temp dir, reach the network through a configured proxy). It is
//! reused as-is by the external-judge spawn (`judge_mode.rs`), which runs an
//! arbitrary operator-configured command, not `claude` — none of these names
//! are Anthropic/Claude-specific.
//!
//! **Never add a name here that is itself a secret** (`*_API_KEY`, `*_TOKEN`,
//! `*_SECRET`, `*_PASSWORD`, ...). Every credential a spawned CLI needs is
//! injected explicitly by the caller from a resolved config value (the
//! `AccountRotator`-selected account's env, or an agent-scoped read) — never
//! ambiently inherited. `allowlist_never_carries_a_secret_shaped_name` below
//! enforces this as a compile-time-adjacent test guard.
//!
//! 2026-08 live-fire validation (macOS, real OAuth session): a `claude -p`
//! subprocess spawned with ONLY `PATH`/`HOME`/`USER`/`LOGNAME` (no ambient
//! `LANG`/`TERM`/`SHELL`/`TMPDIR`) successfully resolved OS-keychain OAuth
//! and completed a Bash-tool round-trip. The wider allowlist below is
//! deliberately more generous than that minimal proof — matching
//! `worker_supervisor.rs`'s already-shipped set plus a few defensive,
//! non-sensitive additions (XDG / proxy / timezone) for deployment shapes
//! (headless Linux, corporate proxy) this workstation can't exercise
//! directly. See the DESIGN doc's honesty note in the WP-8B report: this is
//! "cover the validated set generously", not "exhaustively proven for every
//! platform".
//!
//! ## WP-10A — per-agent git/SSH/GPG credential opt-in (2026-08)
//!
//! The WP-8B scrub above is unconditional: an agent that used to `git push`
//! over SSH, or produce a GPG-signed commit, from inside its spawned CLI lost
//! that ability the moment `SSH_AUTH_SOCK` / `GNUPGHOME` stopped being
//! ambiently inherited. Unlike the base allowlist (safe for every agent by
//! construction — locale, proxy, binary resolution), those two names hand the
//! agent the **operator's own** push/signing identity, so they cannot simply
//! be added to [`AGENT_CLI_ENV_ALLOWLIST`] without granting every agent on the
//! gateway that ability regardless of whether it asked for it.
//!
//! [`GIT_CREDENTIALS_ENV_ALLOWLIST`] is the separate, narrower list; it is
//! only ever added on top of the base allowlist for an agent whose
//! `agent.toml [capabilities] git_credentials = true` (see
//! [`crate::types::CapabilitiesConfig::git_credentials`], default `false`).
//! [`agent_cli_spawn_env_pairs_for`] / [`apply_agent_cli_env_allowlist_for`]
//! are the capability-aware siblings of the two functions below; every other
//! agent's spawn stays byte-identical to the WP-8B behavior.

/// Env var names an agent-CLI (or judge-CLI) subprocess is allowed to
/// inherit from the gateway process, on every platform.
pub const AGENT_CLI_ENV_ALLOWLIST: &[&str] = &[
    // Binary resolution + home dir — without these the CLI cannot find its
    // own dependencies (node/bun/ripgrep/git/...) or locate `~/.claude`.
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    // Bash-tool shell selection.
    "SHELL",
    // Locale / text rendering — CJK-safe output depends on these.
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    // Portable-pty sizing / terminfo lookups (PTY spawn path).
    "TERM",
    "TERMINFO",
    // Temp-file creation location.
    "TMPDIR",
    // Timestamp-sensitive tool output / logging consistency.
    "TZ",
    // Non-default claude config location. Only load-bearing on the
    // ambient-fallback path (no rotator account selected) — an
    // AccountRotator-selected account injects this explicitly afterward and
    // wins on top (see `build_account_env` in
    // `duduclaw-agent::account_rotator`).
    "CLAUDE_CONFIG_DIR",
    // XDG base-dir spec — headless Linux deployments (enterprise
    // Docker/OEM images) route Node-ecosystem config/cache through these.
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
    // Outbound network proxy. Without these, a corporate/enterprise
    // deployment behind a proxy cannot reach the vendor API at all — a
    // strictly worse failure than any leak this allowlist is closing.
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    // Feature flag (not a credential): semantic-vector memory re-ranking,
    // read by the spawned `duduclaw mcp-server` (`mcp.rs`). The v1.61.0
    // scrub silently kept this off even when the operator set `=1` — the
    // TODO-spawn-env-allowlist-fallout sweep's one silent in-child casualty.
    "DUDUCLAW_SEMANTIC_VECTORS",
];

/// macOS only: Cocoa `NSString`/Keychain Services initialization reads this
/// before the OS keychain can be queried — without it, OS-keychain OAuth
/// accounts (the default `claude auth login` session, i.e. anything that
/// isn't an explicit API key or `setup-token`) can fail to authenticate.
/// Carried over from `worker_supervisor.rs`'s Round 3 fix.
///
/// Declared unconditionally (only the usage loop in
/// [`agent_cli_spawn_env_pairs`] is `#[cfg]`-gated) so the secret-shape test
/// below guards it on every development platform.
pub const AGENT_CLI_ENV_ALLOWLIST_MACOS: &[&str] = &["__CF_USER_TEXT_ENCODING"];

/// Windows only. Declared unconditionally (only the usage loop is
/// `#[cfg(windows)]`-gated) so the secret-shape test guards it on every
/// development platform. Windows env lookups are case-insensitive
/// (`GetEnvironmentVariableW`), so the canonical casings below match however
/// the host spells them.
///
/// Two entry families:
/// - Credential/settings location (`worker_supervisor.rs` Round 4, MED-C3):
///   `claude` locates its OAuth credentials + settings via `%APPDATA%\claude\`;
///   without these the spawned CLI cannot authenticate.
/// - The Windows system set (2026-08-20 field incident): the v1.61.x scrub
///   omitted it entirely, and a Node-based CLI (`claude.exe`) fail-fasts at
///   startup without `SystemRoot` — exit 0xC0000409, no output, nothing to
///   debug from. These are machine-shape variables (paths, sizes, arch), not
///   operator data — the same class as `PATH`/`TMPDIR` on the base list.
pub const AGENT_CLI_ENV_ALLOWLIST_WINDOWS: &[&str] = &[
    // Credential + settings resolution.
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "COMPUTERNAME",
    // System roots — DLL/runtime initialization. `SystemRoot` is the one
    // whose absence instantly crashes Node (and therefore claude.exe).
    "SystemRoot",
    "windir",
    "SystemDrive",
    // Shell + executable resolution: `cmd.exe` location and the extension
    // search list (spawning `npm`/`claude` without `.cmd`/`.exe` needs it).
    "ComSpec",
    "PATHEXT",
    // Temp-file creation (Windows has no `TMPDIR`).
    "TEMP",
    "TMP",
    // Platform detection + pool sizing read by Node/libuv and build tools.
    "OS",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    // Home/profile resolution beyond USERPROFILE (tools composing
    // HOMEDRIVE+HOMEPATH; USERNAME is Windows' USER/LOGNAME).
    "HOMEDRIVE",
    "HOMEPATH",
    "USERNAME",
    // Install-location probing (git/node system config, Program Files
    // lookups by npm and installer-based CLIs).
    "ProgramFiles",
    "ProgramFiles(x86)",
    "ProgramData",
    "ALLUSERSPROFILE",
];

/// The `DUDUCLAW_*` names set in THIS process's environment that the
/// allowlist will drop from every spawn. Pure helper for
/// [`warn_scrubbed_duduclaw_vars_once`] and its test.
///
/// Scoped to our own namespace on purpose: a `DUDUCLAW_*` var on the gateway
/// process is almost certainly operator intent aimed at DuDuClaw components,
/// so silently dropping it is exactly the failure mode of the v1.61.0
/// incident (TODO-spawn-env-allowlist-fallout: a real-money agent fell back
/// to a mock broker because its env never arrived). Foreign names (`ML_*`,
/// vendor keys) stay unlisted — dropping those is the scrub working as
/// designed.
fn scrubbed_duduclaw_var_names() -> Vec<String> {
    std::env::vars()
        .filter(|(k, v)| {
            k.starts_with("DUDUCLAW_")
                && !v.is_empty()
                && !AGENT_CLI_ENV_ALLOWLIST.contains(&k.as_str())
        })
        .map(|(k, _)| k)
        .collect()
}

/// Warn ONCE per process (names only, never values) about `DUDUCLAW_*` env
/// vars the allowlist drops from spawned children. The v1.61.0 scrub's two
/// production failures each took hours to diagnose precisely because the
/// drop was perfectly silent — this converts the next one into a boot-time
/// signal. Delivery paths that survive the scrub (`.mcp.json` `env` blocks,
/// explicit caller injection) are unaffected; the warning documents them.
fn warn_scrubbed_duduclaw_vars_once() {
    static WARNED: std::sync::Once = std::sync::Once::new();
    WARNED.call_once(|| {
        let dropped = scrubbed_duduclaw_var_names();
        if !dropped.is_empty() {
            tracing::warn!(
                dropped = %dropped.join(", "),
                "spawn-env allowlist will NOT pass these DUDUCLAW_* vars to spawned \
                 agent CLIs / MCP servers — if a child needs one, declare it in the \
                 server's .mcp.json `env` block or add it to the allowlist \
                 (never for credentials)"
            );
        }
    });
}

/// Snapshot the current process's allowlisted env vars as `(name, value)`
/// pairs — present + non-empty only. Pure data; callers decide HOW to apply
/// it: `tokio::process::Command::env_clear()` + a loop (see
/// [`apply_agent_cli_env_allowlist`]), or seeding a `HashMap` for the
/// portable-pty path (`duduclaw-cli-runtime`'s `PtyCommand::clear_env`).
pub fn agent_cli_spawn_env_pairs() -> Vec<(&'static str, String)> {
    warn_scrubbed_duduclaw_vars_once();
    let mut out = Vec::new();
    for name in AGENT_CLI_ENV_ALLOWLIST {
        if let Ok(v) = std::env::var(name) {
            if !v.is_empty() {
                out.push((*name, v));
            }
        }
    }
    #[cfg(target_os = "macos")]
    for name in AGENT_CLI_ENV_ALLOWLIST_MACOS {
        if let Ok(v) = std::env::var(name) {
            if !v.is_empty() {
                out.push((*name, v));
            }
        }
    }
    #[cfg(windows)]
    for name in AGENT_CLI_ENV_ALLOWLIST_WINDOWS {
        if let Ok(v) = std::env::var(name) {
            if !v.is_empty() {
                out.push((*name, v));
            }
        }
    }
    out
}

/// Apply the allowlist to a [`tokio::process::Command`]: clears the fully
/// inherited environment first, then seeds only the allowlisted vars. Call
/// this BEFORE applying any further explicit overrides (rotator-resolved
/// credentials, per-agent context propagation env) so those always win —
/// mirrors the existing "explicit env wins" contract at every call site.
pub fn apply_agent_cli_env_allowlist(cmd: &mut tokio::process::Command) {
    cmd.env_clear();
    for (k, v) in agent_cli_spawn_env_pairs() {
        cmd.env(k, v);
    }
}

// ── WP-10A: per-agent git/SSH/GPG credential opt-in ──────────────────────

/// Env var names carrying the operator's SSH/GPG identity, additionally
/// allowlisted for a spawned agent-CLI subprocess ONLY when the owning
/// agent's `agent.toml [capabilities] git_credentials = true`.
///
/// Scoped to exactly what `git push` over an SSH remote and a GPG commit
/// signature (`git commit -S` / `commit.gpgsign = true`) need:
/// - `SSH_AUTH_SOCK` / `SSH_AGENT_PID` — talk to the operator's running
///   `ssh-agent` so `git push` over SSH can authenticate without a
///   passphrase prompt (a non-interactive `-p`/PTY spawn cannot answer one
///   anyway).
/// - `GPG_TTY` / `GNUPGHOME` — `gpg` needs `GPG_TTY` to locate a pinentry
///   and `GNUPGHOME` to find the operator's keyring on a non-default
///   install.
///
/// Deliberately does NOT include `GIT_*` variables (`GIT_AUTHOR_*` /
/// `GIT_SSH_COMMAND` / …): none of them are load-bearing for the SSH-push +
/// GPG-sign path this WP validated, and adding them widens the grant beyond
/// what was scoped. Extend this list only after confirming a concrete
/// missing capability, not defensively — see the module doc for why this is
/// a separate, narrower list from [`AGENT_CLI_ENV_ALLOWLIST`] rather than a
/// simple addition to it.
pub const GIT_CREDENTIALS_ENV_ALLOWLIST: &[&str] =
    &["SSH_AUTH_SOCK", "SSH_AGENT_PID", "GPG_TTY", "GNUPGHOME"];

/// Snapshot the present + non-empty [`GIT_CREDENTIALS_ENV_ALLOWLIST`] vars
/// from the gateway's own environment, unconditionally.
///
/// This is the raw building block — it does NOT consult any capability
/// flag. Callers that need the capability gate should use
/// [`agent_cli_spawn_env_pairs_for`] / [`git_credentials_granted_names`]
/// instead of calling this directly.
pub fn git_credentials_env_pairs() -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    for name in GIT_CREDENTIALS_ENV_ALLOWLIST {
        if let Ok(v) = std::env::var(name) {
            if !v.is_empty() {
                out.push((*name, v));
            }
        }
    }
    out
}

/// The base [`agent_cli_spawn_env_pairs`] set, plus
/// [`git_credentials_env_pairs`] when `capabilities.git_credentials` is
/// `true`. `capabilities = None` or `git_credentials = false` is
/// byte-identical to [`agent_cli_spawn_env_pairs`] alone — the WP-8B scrub
/// is unchanged for every agent that has not opted in.
pub fn agent_cli_spawn_env_pairs_for(
    capabilities: Option<&crate::types::CapabilitiesConfig>,
) -> Vec<(&'static str, String)> {
    let mut pairs = agent_cli_spawn_env_pairs();
    if capabilities.is_some_and(|c| c.git_credentials) {
        pairs.extend(git_credentials_env_pairs());
    }
    pairs
}

/// The [`GIT_CREDENTIALS_ENV_ALLOWLIST`] names that were actually carried
/// into a spawn built from [`agent_cli_spawn_env_pairs_for`] /
/// [`apply_agent_cli_env_allowlist_for`] for `capabilities` — i.e. the
/// capability is on AND the var is present + non-empty in this process's
/// environment.
///
/// Callers use this to audit-log which credential *names* (never values)
/// were handed to a subprocess (WP-10A rule: "every spawn that actually
/// adds them is audit-logged"). Empty when the capability is off, or when
/// none of the four vars happen to be set ambiently.
pub fn git_credentials_granted_names(
    capabilities: Option<&crate::types::CapabilitiesConfig>,
) -> Vec<&'static str> {
    if !capabilities.is_some_and(|c| c.git_credentials) {
        return Vec::new();
    }
    git_credentials_env_pairs()
        .into_iter()
        .map(|(k, _)| k)
        .collect()
}

/// [`apply_agent_cli_env_allowlist`], but additionally seeding
/// [`GIT_CREDENTIALS_ENV_ALLOWLIST`] when `capabilities.git_credentials` is
/// `true`. Returns the git-credential env var *names* that were actually
/// granted (see [`git_credentials_granted_names`]) so the caller can
/// audit-log the grant — a non-empty return MUST be logged by the caller,
/// never silently discarded.
pub fn apply_agent_cli_env_allowlist_for(
    cmd: &mut tokio::process::Command,
    capabilities: Option<&crate::types::CapabilitiesConfig>,
) -> Vec<&'static str> {
    cmd.env_clear();
    for (k, v) in agent_cli_spawn_env_pairs_for(capabilities) {
        cmd.env(k, v);
    }
    git_credentials_granted_names(capabilities)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one property that matters most: nothing here can leak a secret,
    /// even if a future edit adds an entry without reading the module docs.
    #[test]
    fn allowlist_never_carries_a_secret_shaped_name() {
        let secret_suffixes = ["_API_KEY", "_TOKEN", "_SECRET", "_PASSWORD", "_KEY"];
        // WP-10A: `GIT_CREDENTIALS_ENV_ALLOWLIST` is a *reference* list
        // (socket paths / TTY / directory paths that point AT credentials,
        // not the credentials themselves — matching e.g. `CLAUDE_CONFIG_DIR`
        // on the base list), so it must clear the same bar.
        for name in AGENT_CLI_ENV_ALLOWLIST
            .iter()
            .chain(GIT_CREDENTIALS_ENV_ALLOWLIST)
            .chain(AGENT_CLI_ENV_ALLOWLIST_MACOS)
            .chain(AGENT_CLI_ENV_ALLOWLIST_WINDOWS)
        {
            for suffix in secret_suffixes {
                assert!(
                    !name.ends_with(suffix),
                    "`{name}` looks like a credential (suffix `{suffix}`) — must not be in the spawn-env allowlist"
                );
            }
        }
    }

    #[test]
    fn known_vendor_keys_are_absent() {
        // Defence in depth: explicitly assert the exact leak this WP closes
        // is closed, not just structurally-suffix-shaped names.
        for leaked in [
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "GEMINI_API_KEY",
            "GOOGLE_API_KEY",
            "DEEPSEEK_API_KEY",
            "MINIMAX_API_KEY",
            "GROQ_API_KEY",
            "TOGETHER_API_KEY",
            "MISTRAL_API_KEY",
            "OPENROUTER_API_KEY",
            "XAI_API_KEY",
            "DASHSCOPE_API_KEY",
            "QWEN_API_KEY",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "DUDUCLAW_MCP_API_KEY",
        ] {
            assert!(
                !AGENT_CLI_ENV_ALLOWLIST.contains(&leaked),
                "{leaked} must never be ambiently inherited by a spawned agent CLI"
            );
        }
    }

    // Serialize the few env-mutating tests so they don't race each other
    // (matches the idiom in `platform.rs::home_tests`).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn pairs_include_present_allowlisted_vars_and_skip_others() {
        let _g = ENV_LOCK.lock().unwrap();
        let prev = std::env::var("TZ").ok();
        unsafe { std::env::set_var("TZ", "Asia/Taipei") };

        let pairs = agent_cli_spawn_env_pairs();
        assert!(
            pairs.iter().any(|(k, v)| *k == "TZ" && v == "Asia/Taipei"),
            "TZ should be carried through when present"
        );
        // A var not on the allowlist must never appear even if it's set in
        // the test process's real environment.
        assert!(
            !pairs.iter().any(|(k, _)| *k == "ANTHROPIC_API_KEY"),
            "ANTHROPIC_API_KEY must never appear in the allowlisted snapshot"
        );

        unsafe {
            match prev {
                Some(v) => std::env::set_var("TZ", v),
                None => std::env::remove_var("TZ"),
            }
        }
    }

    #[test]
    fn semantic_vectors_flag_is_carried_through() {
        let _g = ENV_LOCK.lock().unwrap();
        let prev = std::env::var("DUDUCLAW_SEMANTIC_VECTORS").ok();
        unsafe { std::env::set_var("DUDUCLAW_SEMANTIC_VECTORS", "1") };

        let pairs = agent_cli_spawn_env_pairs();
        assert!(
            pairs
                .iter()
                .any(|(k, v)| *k == "DUDUCLAW_SEMANTIC_VECTORS" && v == "1"),
            "the semantic-vectors feature flag must reach spawned children \
             (TODO-spawn-env-allowlist-fallout item 4)"
        );

        unsafe {
            match prev {
                Some(v) => std::env::set_var("DUDUCLAW_SEMANTIC_VECTORS", v),
                None => std::env::remove_var("DUDUCLAW_SEMANTIC_VECTORS"),
            }
        }
    }

    #[test]
    fn scrub_detector_lists_dropped_duduclaw_vars_only() {
        let _g = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("DUDUCLAW_SCRUB_TEST_PROBE", "x");
            std::env::set_var("ML_SCRUB_TEST_FOREIGN", "y");
        }

        let dropped = scrubbed_duduclaw_var_names();
        assert!(
            dropped.iter().any(|n| n == "DUDUCLAW_SCRUB_TEST_PROBE"),
            "a DUDUCLAW_* var off the allowlist must be reported as dropped"
        );
        assert!(
            !dropped.iter().any(|n| n == "ML_SCRUB_TEST_FOREIGN"),
            "foreign-namespace vars are dropped by design, not reported"
        );
        assert!(
            !dropped.iter().any(|n| n == "DUDUCLAW_SEMANTIC_VECTORS"),
            "an allowlisted DUDUCLAW_* var must not be reported as dropped"
        );

        unsafe {
            std::env::remove_var("DUDUCLAW_SCRUB_TEST_PROBE");
            std::env::remove_var("ML_SCRUB_TEST_FOREIGN");
        }
    }

    #[test]
    fn empty_value_is_treated_as_unset() {
        let _g = ENV_LOCK.lock().unwrap();
        let prev = std::env::var("TZ").ok();
        unsafe { std::env::set_var("TZ", "") };

        let pairs = agent_cli_spawn_env_pairs();
        assert!(
            !pairs.iter().any(|(k, _)| *k == "TZ"),
            "an empty-string env var must not be forwarded as if it were set"
        );

        unsafe {
            match prev {
                Some(v) => std::env::set_var("TZ", v),
                None => std::env::remove_var("TZ"),
            }
        }
    }

    #[test]
    fn apply_to_command_clears_then_seeds_allowlist_only() {
        let _g = ENV_LOCK.lock().unwrap();
        let prev = std::env::var("TZ").ok();
        unsafe {
            std::env::set_var("TZ", "Asia/Taipei");
            // A non-allowlisted var that MUST NOT survive env_clear().
            std::env::set_var("DUDUCLAW_SPAWN_ENV_TEST_LEAK_CANARY", "should-not-leak");
        }

        let mut cmd = tokio::process::Command::new("true");
        apply_agent_cli_env_allowlist(&mut cmd);
        let std_cmd: &std::process::Command = cmd.as_std();
        let envs: std::collections::HashMap<_, _> = std_cmd
            .get_envs()
            .filter_map(|(k, v)| v.map(|v| (k.to_owned(), v.to_owned())))
            .collect();

        assert_eq!(
            envs.get(std::ffi::OsStr::new("TZ")),
            Some(&std::ffi::OsString::from("Asia/Taipei"))
        );
        assert!(
            !envs.contains_key(std::ffi::OsStr::new("DUDUCLAW_SPAWN_ENV_TEST_LEAK_CANARY")),
            "env_clear() + allowlist must not let an arbitrary ambient var through"
        );

        unsafe {
            std::env::remove_var("DUDUCLAW_SPAWN_ENV_TEST_LEAK_CANARY");
            match prev {
                Some(v) => std::env::set_var("TZ", v),
                None => std::env::remove_var("TZ"),
            }
        }
    }

    // ── WP-10A: per-agent git/SSH/GPG credential opt-in ──────────────────

    fn caps_with_git_credentials(enabled: bool) -> crate::types::CapabilitiesConfig {
        crate::types::CapabilitiesConfig {
            git_credentials: enabled,
            ..Default::default()
        }
    }

    /// Guard against a probe env var colliding with something a parallel
    /// test thread also mutates — reused by every WP-10A test below.
    fn with_ssh_auth_sock<T>(value: Option<&str>, f: impl FnOnce() -> T) -> T {
        let prev = std::env::var("SSH_AUTH_SOCK").ok();
        unsafe {
            match value {
                Some(v) => std::env::set_var("SSH_AUTH_SOCK", v),
                None => std::env::remove_var("SSH_AUTH_SOCK"),
            }
        }
        let out = f();
        unsafe {
            match prev {
                Some(v) => std::env::set_var("SSH_AUTH_SOCK", v),
                None => std::env::remove_var("SSH_AUTH_SOCK"),
            }
        }
        out
    }

    #[test]
    fn capability_off_never_adds_git_credential_env_even_when_ambiently_present() {
        let _g = ENV_LOCK.lock().unwrap();
        with_ssh_auth_sock(Some("/tmp/agent.sock"), || {
            let off = caps_with_git_credentials(false);
            let pairs = agent_cli_spawn_env_pairs_for(Some(&off));
            assert!(
                !pairs.iter().any(|(k, _)| *k == "SSH_AUTH_SOCK"),
                "git_credentials=false must stay byte-identical to the WP-8B base allowlist"
            );
            assert!(
                git_credentials_granted_names(Some(&off)).is_empty(),
                "nothing should be reported as granted when the capability is off"
            );
            // `None` capabilities (agent-less system callers) must behave
            // identically to an explicit `false`.
            assert!(git_credentials_granted_names(None).is_empty());
            assert_eq!(
                agent_cli_spawn_env_pairs_for(None),
                agent_cli_spawn_env_pairs(),
                "None capabilities must be byte-identical to the base allowlist"
            );
        });
    }

    #[test]
    fn capability_on_adds_git_credential_env_only_when_ambiently_present() {
        let _g = ENV_LOCK.lock().unwrap();
        let on = caps_with_git_credentials(true);

        with_ssh_auth_sock(Some("/tmp/agent.sock"), || {
            let pairs = agent_cli_spawn_env_pairs_for(Some(&on));
            assert!(
                pairs
                    .iter()
                    .any(|(k, v)| *k == "SSH_AUTH_SOCK" && v == "/tmp/agent.sock"),
                "git_credentials=true must carry through an ambiently-present SSH_AUTH_SOCK"
            );
            let granted = git_credentials_granted_names(Some(&on));
            assert!(granted.contains(&"SSH_AUTH_SOCK"));
        });

        with_ssh_auth_sock(None, || {
            // Capability on, but nothing ambiently set — must not fabricate
            // a grant (and therefore must not over-claim in the audit log).
            let pairs = agent_cli_spawn_env_pairs_for(Some(&on));
            assert!(!pairs.iter().any(|(k, _)| *k == "SSH_AUTH_SOCK"));
            assert!(!git_credentials_granted_names(Some(&on)).contains(&"SSH_AUTH_SOCK"));
        });
    }

    #[test]
    fn apply_to_command_for_off_excludes_git_credential_env() {
        let _g = ENV_LOCK.lock().unwrap();
        with_ssh_auth_sock(Some("/tmp/agent.sock"), || {
            let off = caps_with_git_credentials(false);
            let mut cmd = tokio::process::Command::new("true");
            let granted = apply_agent_cli_env_allowlist_for(&mut cmd, Some(&off));
            assert!(granted.is_empty());

            let std_cmd: &std::process::Command = cmd.as_std();
            let has_ssh = std_cmd
                .get_envs()
                .any(|(k, _)| k == std::ffi::OsStr::new("SSH_AUTH_SOCK"));
            assert!(
                !has_ssh,
                "SSH_AUTH_SOCK must not reach the child command when git_credentials=false"
            );
        });
    }

    #[test]
    fn apply_to_command_for_on_includes_git_credential_env_and_reports_it() {
        let _g = ENV_LOCK.lock().unwrap();
        with_ssh_auth_sock(Some("/tmp/agent.sock"), || {
            let on = caps_with_git_credentials(true);
            let mut cmd = tokio::process::Command::new("true");
            let granted = apply_agent_cli_env_allowlist_for(&mut cmd, Some(&on));
            assert!(
                granted.contains(&"SSH_AUTH_SOCK"),
                "the audit-facing return must name every git-credential env var actually applied"
            );

            let std_cmd: &std::process::Command = cmd.as_std();
            let ssh_value = std_cmd
                .get_envs()
                .find(|(k, _)| *k == std::ffi::OsStr::new("SSH_AUTH_SOCK"))
                .and_then(|(_, v)| v)
                .map(|v| v.to_owned());
            assert_eq!(
                ssh_value,
                Some(std::ffi::OsString::from("/tmp/agent.sock")),
                "SSH_AUTH_SOCK must reach the child command when git_credentials=true"
            );
        });
    }

    /// Live-fire validation (not just `Command` introspection above): actually
    /// spawn `/bin/sh` and have it print `SSH_AUTH_SOCK` at runtime, matching
    /// the WP-8B `env -i` live-fire method referenced in the module doc.
    /// Confirms the real child process — not just the `Command` object we
    /// built — does/doesn't see the credential depending on the capability.
    #[cfg(unix)]
    #[tokio::test]
    async fn live_spawn_git_credentials_toggle_controls_real_child_env() {
        let _g = ENV_LOCK.lock().unwrap();
        let probe_value = "/tmp/duduclaw-wp10a-livefire.sock";
        let prev = std::env::var("SSH_AUTH_SOCK").ok();
        unsafe { std::env::set_var("SSH_AUTH_SOCK", probe_value) };

        async fn run_and_capture(
            capabilities: Option<&crate::types::CapabilitiesConfig>,
        ) -> String {
            let mut cmd = tokio::process::Command::new("/bin/sh");
            cmd.args(["-c", "printf 'SSH_AUTH_SOCK=[%s]' \"$SSH_AUTH_SOCK\""]);
            apply_agent_cli_env_allowlist_for(&mut cmd, capabilities);
            cmd.stdout(std::process::Stdio::piped());
            let out = cmd.output().await.expect("live spawn of /bin/sh failed");
            assert!(out.status.success(), "probe child exited non-zero: {out:?}");
            String::from_utf8_lossy(&out.stdout).into_owned()
        }

        let off = caps_with_git_credentials(false);
        let off_out = run_and_capture(Some(&off)).await;
        assert_eq!(
            off_out, "SSH_AUTH_SOCK=[]",
            "git_credentials=false: the REAL spawned child must not see SSH_AUTH_SOCK"
        );

        let on = caps_with_git_credentials(true);
        let on_out = run_and_capture(Some(&on)).await;
        assert_eq!(
            on_out,
            format!("SSH_AUTH_SOCK=[{probe_value}]"),
            "git_credentials=true: the REAL spawned child must see the exact SSH_AUTH_SOCK value"
        );

        unsafe {
            match prev {
                Some(v) => std::env::set_var("SSH_AUTH_SOCK", v),
                None => std::env::remove_var("SSH_AUTH_SOCK"),
            }
        }
    }
}
