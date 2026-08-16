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
];

/// macOS only: Cocoa `NSString`/Keychain Services initialization reads this
/// before the OS keychain can be queried — without it, OS-keychain OAuth
/// accounts (the default `claude auth login` session, i.e. anything that
/// isn't an explicit API key or `setup-token`) can fail to authenticate.
/// Carried over from `worker_supervisor.rs`'s Round 3 fix.
#[cfg(target_os = "macos")]
pub const AGENT_CLI_ENV_ALLOWLIST_MACOS: &[&str] = &["__CF_USER_TEXT_ENCODING"];

/// Windows only: `claude` CLI locates its OAuth credentials + settings via
/// `%APPDATA%\claude\`. Carried over from `worker_supervisor.rs`'s Round 4
/// fix (MED-C3) — without these, the spawned CLI fails to authenticate and
/// (for the PTY-pool path) the pool churns rebuilding doomed sessions.
#[cfg(windows)]
pub const AGENT_CLI_ENV_ALLOWLIST_WINDOWS: &[&str] =
    &["USERPROFILE", "APPDATA", "LOCALAPPDATA", "COMPUTERNAME"];

/// Snapshot the current process's allowlisted env vars as `(name, value)`
/// pairs — present + non-empty only. Pure data; callers decide HOW to apply
/// it: `tokio::process::Command::env_clear()` + a loop (see
/// [`apply_agent_cli_env_allowlist`]), or seeding a `HashMap` for the
/// portable-pty path (`duduclaw-cli-runtime`'s `PtyCommand::clear_env`).
pub fn agent_cli_spawn_env_pairs() -> Vec<(&'static str, String)> {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The one property that matters most: nothing here can leak a secret,
    /// even if a future edit adds an entry without reading the module docs.
    #[test]
    fn allowlist_never_carries_a_secret_shaped_name() {
        let secret_suffixes = ["_API_KEY", "_TOKEN", "_SECRET", "_PASSWORD", "_KEY"];
        for name in AGENT_CLI_ENV_ALLOWLIST {
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
}
