//! Ensure each agent directory has a `.claude/settings.json` with the
//! `agent-file-guard` PreToolUse hook registered.
//!
//! The hook delegates to `duduclaw hook agent-file-guard`, which exits 2
//! (blocks the tool call) if the agent tries to Write / Edit / MultiEdit
//! an agent-structure file (`agent.toml` / `SOUL.md` / etc.) outside the
//! canonical `<home>/agents/<name>/` tree.
//!
//! WP1.1 (SOUL.md 唯讀化, `DESIGN-evolution-v3-aee.md` §1.9.2 C3): the same
//! hook additionally refuses `SOUL.md` writes *inside* the canonical tree
//! when the target is the caller's own agent directory
//! (`duduclaw_core::check_own_soul_write`) — personality is
//! operator-managed (dashboard), not self-writable even in the right
//! location. See `duduclaw_core::GuardDecision::BlockedOwnSoulWrite`.
//!
//! This is the enforcement layer for Option 3 of the "missing agents on
//! dashboard" bug: agents must use the `create_agent` MCP tool to scaffold
//! new agents — the raw Write tool is hard-gated.
//!
//! # Merge semantics
//!
//! If `<agent_dir>/.claude/settings.json` already exists, the installer
//! merges the hook entry in without clobbering unrelated settings
//! (user-custom `permissions`, `env`, other hooks, etc.). The merge is
//! idempotent — running twice produces the same output.
//!
//! Identity of "our" hook entry is tracked by the `"_duduclaw_hook"` tag
//! on the hook descriptor, so the installer can update the command path
//! without leaving stale duplicates behind.
//!
//! # Second hook: `data-file-guard` (RFC-23 §14.4)
//!
//! The same installer also registers `duduclaw hook data-file-guard` as a
//! PreToolUse hook for `Read` and `Bash`. Where `agent-file-guard` protects
//! DuDuClaw's own structure files, this one protects the *customer's* data:
//! `Read`/`Bash` are built-in tools, so a CSV read through them never passes
//! the MCP redaction choke point. The hook is inert unless the gateway sets
//! `DUDUCLAW_DATA_FILE_GUARD` at spawn time, which it only does when redaction
//! is actually active for that agent — so an install on a gateway with
//! redaction off changes no behavior at all.
//!
//! Two tagged entries now share `hooks.PreToolUse`, distinguished by the
//! `_duduclaw_hook` value ([`HOOK_ID`] vs [`DATA_FILE_HOOK_ID`]); the merge is
//! per-entry, so a user's own hooks and the other tagged entry are never
//! disturbed.
//!
//! **H10 (2026-09 feature audit)**: this used to be a POSIX shell script
//! dropped at `<agent_dir>/.claude/hooks/data-file-guard.sh`, which was inert
//! on a Windows host with no bash on PATH — the hook command failed and Claude
//! Code treats a non-2 exit as allow, so the guard was missing exactly where
//! nobody would notice. It is now the same kind of Rust subcommand
//! `agent-file-guard` has always been, and the installer deletes any leftover
//! script on upgrade so a stale copy cannot be mistaken for the live guard.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tracing::{debug, warn};

/// Tag embedded in the hook descriptor so we can find + update our own
/// entry on subsequent runs without touching user-added hooks.
const HOOK_TAG: &str = "_duduclaw_hook";

/// Sentinel value identifying the agent-file-guard hook specifically.
const HOOK_ID: &str = "agent-file-guard";

/// Tools the agent-file-guard hook sees. `NotebookEdit` joined in G1 round 2:
/// it writes any `.ipynb` path (including through a link), and the hook judges
/// its `notebook_path` like a Write.
const AGENT_FILE_GUARD_MATCHER: &str = "Write|Edit|MultiEdit|NotebookEdit|Read|Bash";

/// Sentinel for the RFC-23 §14.4 data-file guard entry.
const DATA_FILE_HOOK_ID: &str = "data-file-guard";
const EGRESS_HOOK_ID: &str = "egress-guard";

/// The pre-H10 shell script's filename, kept only so an upgrade can delete
/// the leftover file. Nothing writes it any more.
const LEGACY_DATA_FILE_HOOK_SCRIPT: &str = "data-file-guard.sh";

/// Ensure `<agent_dir>/.claude/settings.json` contains the agent-file-guard
/// PreToolUse hook pointing at `duduclaw_bin`.
///
/// Returns installation errors to the caller. Runtime spawns use
/// [`ensure_spawn_hook_settings`] to apply the effective egress mode.
///
/// # Idempotency
///
/// - No existing settings.json → writes a fresh minimal file.
/// - Existing settings.json without `hooks` → adds a `hooks` object.
/// - Existing `hooks.PreToolUse` array with our tagged entry → updates
///   the command in place if `duduclaw_bin` has changed.
/// - Existing `hooks.PreToolUse` array without our tagged entry → appends.
/// - Existing user-added hooks (without our tag) → left untouched.
pub async fn ensure_agent_hook_settings(
    agent_dir: &Path,
    duduclaw_bin: &Path,
) -> std::io::Result<()> {
    let settings_path = agent_dir.join(".claude").join("settings.json");

    // Load existing settings (or start fresh).
    let mut root: Value = match tokio::fs::read_to_string(&settings_path).await {
        Ok(content) if !content.trim().is_empty() => {
            match serde_json::from_str::<Value>(&content) {
                Ok(v) if v.is_object() => v,
                Ok(_) => {
                    warn!(
                        path = %settings_path.display(),
                        "settings.json is not a JSON object — refusing to overwrite"
                    );
                    return Err(std::io::Error::other("settings.json is not an object"));
                }
                Err(e) => {
                    warn!(
                        path = %settings_path.display(),
                        error = %e,
                        "settings.json has invalid JSON — skipping hook install"
                    );
                    return Err(std::io::Error::other(e));
                }
            }
        }
        Ok(_) => json!({}),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(e) => return Err(e),
    };
    if root.get("disableAllHooks").is_some_and(|value| value != &json!(false)) {
        return Err(std::io::Error::other("settings.json disables hooks"));
    }
    let hooks_enabled = root.get("disableAllHooks") != Some(&json!(false));
    root["disableAllHooks"] = json!(false);

    // WP22 T2 fix — the hook subprocess does not inherit `DUDUCLAW_AGENT_ID`
    // (that env is only injected into the MCP server child process), so the
    // caller-scope rule in `check_caller_scope` was inert in production. The
    // agent directory's basename is embedded directly into the installed
    // command instead: `agent_dir` is always `<home>/agents/<id>/` (or the
    // `.ephemeral/<eph-id>/` scaffold), so its file name IS the identity the
    // hook should claim — no env round-trip required, and the agent cannot
    // forge it without rewriting its own `.claude/settings.json`, which is
    // itself frozen by `ProtectedSurface::HookSettings`.
    let agent_id = agent_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();

    // H10 — remove the pre-H10 shell script if this agent still carries one.
    // It is no longer referenced by any hook entry, and leaving it on disk
    // invites someone to "fix" the guard by editing a file that does nothing.
    remove_legacy_data_file_guard_script(agent_dir).await;

    // Merge our hook descriptors into hooks.PreToolUse. Both run: a `false`
    // from either only means "already up to date".
    // G1 round 2 — the agent's CLI runs with a scrubbed environment that has
    // no `DUDUCLAW_HOME`, so the hook would fall back to `$HOME/.duduclaw`.
    // The home is derived from the agent directory and baked in as `--home`.
    let home = home_of_agent_dir(agent_dir);
    let mut updated = hooks_enabled;
    updated |= merge_agent_file_guard_hook(&mut root, duduclaw_bin, agent_id, home.as_deref());
    updated |= merge_data_file_guard_hook(&mut root, duduclaw_bin);
    updated |= merge_egress_guard_hook(&mut root, duduclaw_bin, agent_id, home.as_deref());
    if !updated {
        debug!(path = %settings_path.display(), "Hooks already up to date");
        return Ok(());
    }

    // Write back atomically: temp file + rename.
    if let Some(parent) = settings_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let tmp = settings_path.with_extension("json.tmp");
    let pretty = serde_json::to_string_pretty(&root).map_err(std::io::Error::other)?;
    tokio::fs::write(&tmp, pretty).await?;
    tokio::fs::rename(&tmp, &settings_path).await?;

    debug!(
        path = %settings_path.display(),
        bin = %duduclaw_bin.display(),
        "Installed agent-file-guard + data-file-guard + egress-guard PreToolUse hooks"
    );
    Ok(())
}

/// Installation is mandatory in ask/block; a failure must not rotate to a
/// different account or runtime. Shadow retains its observe-only behavior.
pub(crate) async fn ensure_spawn_hook_settings(
    home: &Path,
    agent_dir: &Path,
    duduclaw_bin: &Path,
) -> Result<(), String> {
    if let Err(error) = validate_hook_identity(agent_dir) {
        return hook_install_result(home, agent_dir, error);
    }
    if let Err(error) = ensure_agent_hook_settings(agent_dir, duduclaw_bin).await {
        return hook_install_result(home, agent_dir, error);
    }
    Ok(())
}

fn validate_hook_identity(agent_dir: &Path) -> std::io::Result<()> {
    let valid_agent = agent_dir.file_name().and_then(|s| s.to_str())
        .is_some_and(duduclaw_core::is_valid_agent_id);
    let valid_home = home_of_agent_dir(agent_dir)
        .is_some_and(|home| home.is_absolute() && home.to_str().is_some());
    if !valid_agent || !valid_home {
        return Err(std::io::Error::other("hook requires a valid employee directory and absolute home"));
    }
    Ok(())
}

/// Bare mode skips hooks. Borrowed workspaces must load the identity owner's
/// settings explicitly; cwd auto-discovery would guard the wrong employee.
pub(crate) fn claude_hook_args(home: &Path, agent_dir: Option<&Path>, bare: bool) -> Result<Vec<String>, String> {
    use duduclaw_core::egress_guard::{effective_mode, Mode};
    let mode = effective_mode(home, agent_dir);
    if bare {
        if mode >= Mode::Ask {
            return Err(duduclaw_agent::mcp_spawn_gate::spawn_gate_error(
                "外送防護需要啟用工具 hook；請關閉員工的 cli_bare_mode 後重試。",
            ));
        }
        warn!("Claude bare mode skips egress hooks; shadow/off is not enforced");
        return Ok(Vec::new());
    }
    let Some(dir) = agent_dir else {
        if mode >= Mode::Ask {
            return Err(duduclaw_agent::mcp_spawn_gate::spawn_gate_error(
                "外送防護找不到員工設定目錄，已停止啟動；請管理員檢查員工設定後重試。",
            ));
        }
        return Ok(Vec::new());
    };
    let path = dir.join(".claude/settings.json");
    if mode < Mode::Ask { return Ok(Vec::new()); }
    // Explicit settings are the only source in enforcing modes. Otherwise
    // cwd hooks can merge with these and consume the same ticket twice.
    Ok(vec!["--setting-sources".into(), String::new(), "--settings".into(), path.to_string_lossy().into_owned()])
}

pub(crate) fn hook_install_result(home: &Path, agent_dir: &Path, error: std::io::Error) -> Result<(), String> {
    use duduclaw_core::egress_guard::{effective_mode, Mode};
    if effective_mode(home, Some(agent_dir)) >= Mode::Ask {
        return Err(duduclaw_agent::mcp_spawn_gate::spawn_gate_error(&format!(
            "外送防護無法安裝，已停止啟動員工；請修復員工的 hook 設定與檔案權限後重試：{error}"
        )));
    }
    warn!(agent_dir = %agent_dir.display(), %error, "egress hook install failed; shadow/off spawn continues");
    Ok(())
}

/// Grok project hook file holding DuDuClaw's guards, for an agent directory
/// whose name is not a valid agent id (its command carries no `--agent`).
pub(crate) const GROK_GUARD_HOOKS_FILE: &str = "duduclaw-guards.json";

/// Distinguishes the temp files of concurrent writers in one process.
static GROK_HOOKS_TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// `duduclaw-guards-<agent id>.json`: one file per employee, so employees
/// sharing one working root never replace each other's identity. Grok loads
/// every file there, and a deny from any guard wins, so a member also running
/// another member's guard is only ever stricter.
pub(crate) fn grok_guard_hooks_file_name(agent_dir: &Path) -> String {
    match agent_dir.file_name().and_then(|n| n.to_str()) {
        Some(id) if duduclaw_core::is_valid_agent_id(id) => format!("duduclaw-guards-{id}.json"),
        _ => GROK_GUARD_HOOKS_FILE.to_string(),
    }
}

/// The agent-file-guard and data-file-guard entries as a standalone hook
/// document (`{"hooks": {"PreToolUse": [...]}}`) for the employee at
/// `agent_dir`: the same descriptors [`ensure_agent_hook_settings`] merges
/// into `.claude/settings.json`, identity and `--home` included.
pub(crate) fn guard_hooks_document(agent_dir: &Path, duduclaw_bin: &Path) -> Value {
    let agent_id = agent_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let home = home_of_agent_dir(agent_dir);
    let mut root = json!({"disableAllHooks": false});
    merge_agent_file_guard_hook(&mut root, duduclaw_bin, agent_id, home.as_deref());
    merge_data_file_guard_hook(&mut root, duduclaw_bin);
    merge_egress_guard_hook(&mut root, duduclaw_bin, agent_id, home.as_deref());
    root
}

/// Write [`guard_hooks_document`] to `<work_root>/.grok/hooks/`
/// ([`grok_guard_hooks_file_name`]), Grok's own project hook directory
/// (2026-10-20). Grok spawns turn its Claude-compat hook scanning off, which
/// also stops it reading `<agent_dir>/.claude/settings.json`, so the guards
/// are handed over in Grok's native format instead. Grok maps the Claude
/// tool names in the matchers to its own and sends hooks the Claude
/// `tool_name` / `tool_input` keys, so the subcommands read the same input.
///
/// `work_root` is the spawn's working directory (where Grok looks for project
/// hooks); `agent_dir` decides the identity baked into the command. An
/// up-to-date file is left alone; otherwise it is replaced through a temp
/// file unique to this writer, so concurrent spawns never read half a file.
pub(crate) async fn write_grok_guard_hooks(
    work_root: &Path,
    agent_dir: &Path,
    duduclaw_bin: &Path,
) -> std::io::Result<()> {
    validate_hook_identity(agent_dir)?;
    let dir = work_root.join(".grok").join("hooks");
    let file_name = grok_guard_hooks_file_name(agent_dir);
    let path = dir.join(&file_name);
    let body = serde_json::to_string_pretty(&guard_hooks_document(agent_dir, duduclaw_bin))
        .map_err(std::io::Error::other)?;
    if tokio::fs::read_to_string(&path).await.ok().as_deref() == Some(body.as_str()) {
        return Ok(());
    }
    tokio::fs::create_dir_all(&dir).await?;
    let seq = GROK_HOOKS_TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!(".{file_name}.{}.{seq}.tmp", std::process::id()));
    tokio::fs::write(&tmp, &body).await?;
    // Windows refuses to replace a file another process holds open (Grok
    // reading it at startup); that clears within moments, so retry briefly.
    let mut attempt = 0;
    loop {
        match tokio::fs::rename(&tmp, &path).await {
            Ok(()) => return Ok(()),
            Err(_) if attempt < 4 => {
                attempt += 1;
                tokio::time::sleep(std::time::Duration::from_millis(25 * attempt)).await;
            }
            Err(e) => {
                let _ = tokio::fs::remove_file(&tmp).await;
                return Err(e);
            }
        }
    }
}

/// Delete a leftover pre-H10 `data-file-guard.sh` (and the now-empty
/// `.claude/hooks/` directory, if we emptied it).
///
/// Best-effort and silent on absence — the overwhelmingly common case is a
/// fresh agent that never had one.
async fn remove_legacy_data_file_guard_script(agent_dir: &Path) {
    let hooks_dir = agent_dir.join(".claude").join("hooks");
    let script = hooks_dir.join(LEGACY_DATA_FILE_HOOK_SCRIPT);
    if tokio::fs::try_exists(&script).await.unwrap_or(false) {
        match tokio::fs::remove_file(&script).await {
            Ok(()) => debug!(
                path = %script.display(),
                "Removed the legacy data-file-guard shell script (superseded by `duduclaw hook data-file-guard`)"
            ),
            Err(e) => warn!(
                path = %script.display(),
                error = %e,
                "Could not remove the legacy data-file-guard script — it is unused, but should be deleted by hand"
            ),
        }
    }
    // Only succeeds when the directory is empty, which is exactly the
    // condition under which removing it is safe.
    let _ = tokio::fs::remove_dir(&hooks_dir).await;
}

/// Merge the RFC-23 §14.4 data-file-guard descriptor into
/// `root["hooks"]["PreToolUse"]`, matched on `Read|Bash`.
///
/// Structurally identical to [`merge_agent_file_guard_hook`] but keyed on its
/// own [`DATA_FILE_HOOK_ID`], so the two entries coexist and each upgrades
/// independently. Since H10 both point at the same `duduclaw` binary — the
/// difference is only which subcommand runs.
fn merge_data_file_guard_hook(root: &mut Value, duduclaw_bin: &Path) -> bool {
    let Some(arr) = pre_tool_use_array(root) else {
        return false;
    };
    let desired_entry = json!({
        HOOK_TAG: DATA_FILE_HOOK_ID,
        // Read is the direct route to a data file; Bash is the way around it
        // (`head customers.csv`). Write/Edit are deliberately absent — this
        // guard is about reading customer data, not about writing files.
        "matcher": "Read|Bash",
        "hooks": [{
            "type": "command",
            "command": format!("\"{}\" hook data-file-guard", duduclaw_bin.display()),
        }]
    });
    for item in arr.iter_mut() {
        if item.get(HOOK_TAG).and_then(|v| v.as_str()) == Some(DATA_FILE_HOOK_ID) {
            if item == &desired_entry {
                return false;
            }
            *item = desired_entry;
            return true;
        }
    }
    arr.push(desired_entry);
    true
}

fn merge_egress_guard_hook(root: &mut Value, bin: &Path, agent: &str, home: Option<&Path>) -> bool {
    let command = build_named_hook_command(bin, EGRESS_HOOK_ID, agent, home);
    let desired = json!({
        HOOK_TAG: EGRESS_HOOK_ID,
        "matcher": "WebFetch|WebSearch|Bash|Fetch|Shell|web_fetch|web_search|run_terminal_command",
        "hooks": [{"type": "command", "command": command}]
    });
    let Some(entries) = pre_tool_use_array(root) else { return false; };
    for entry in entries.iter_mut() {
        if entry.get(HOOK_TAG).and_then(Value::as_str) == Some(EGRESS_HOOK_ID) {
            if entry == &desired { return false; }
            *entry = desired;
            return true;
        }
    }
    entries.push(desired);
    true
}

/// Get `root["hooks"]["PreToolUse"]` as an array, creating (and repairing) the
/// containers on the way down. `None` only when `root` is not an object.
fn pre_tool_use_array(root: &mut Value) -> Option<&mut Vec<Value>> {
    let hooks = root
        .as_object_mut()?
        .entry("hooks")
        .or_insert_with(|| json!({}));
    if !hooks.is_object() {
        *hooks = json!({});
    }
    let pre_tool_use = hooks
        .as_object_mut()
        .expect("just normalized to an object")
        .entry("PreToolUse")
        .or_insert_with(|| json!([]));
    if !pre_tool_use.is_array() {
        *pre_tool_use = json!([]);
    }
    pre_tool_use.as_array_mut()
}

/// Merge the hook descriptor into `root["hooks"]["PreToolUse"]`.
///
/// Returns `true` if anything changed (caller should persist), `false`
/// if the hook was already present and up to date.
///
/// # Upgrading a stale entry (WP22 T2)
///
/// No special-case code is needed to migrate a pre-WP22-T2 entry (installed
/// with the old `"<bin>" hook agent-file-guard` command, no `--agent`): the
/// lookup below identifies "our" entry purely by [`HOOK_TAG`], then compares
/// it against `desired_entry` for byte-for-byte equality. An old-format
/// command never equals the new `--agent`-suffixed one, so the existing
/// "found but different → overwrite in place" branch upgrades it for free.
fn merge_agent_file_guard_hook(
    root: &mut Value,
    duduclaw_bin: &Path,
    agent_id: &str,
    home: Option<&Path>,
) -> bool {
    let desired_command = build_hook_command(duduclaw_bin, agent_id, home);
    // Bash is included so the guard can catch agents that bypass Write/Edit
    // by running `mkdir -p /project/.claude/agents/foo` or `cat > .../agent.toml`
    // via the shell. The CLI handler dispatches on tool name internally.
    let desired_entry = json!({
        HOOK_TAG: HOOK_ID,
        "matcher": AGENT_FILE_GUARD_MATCHER,
        "hooks": [{
            "type": "command",
            "command": desired_command,
        }]
    });

    let Some(arr) = pre_tool_use_array(root) else {
        return false;
    };
    for item in arr.iter_mut() {
        if item.get(HOOK_TAG).and_then(|v| v.as_str()) == Some(HOOK_ID) {
            if item == &desired_entry {
                return false; // already up to date
            }
            *item = desired_entry;
            return true;
        }
    }

    // Not found — append.
    arr.push(desired_entry);
    true
}

/// Build the shell command string that Claude Code will execute.
///
/// Uses the absolute path to the `duduclaw` binary so it works regardless
/// of the agent's PATH or cwd. No shell metacharacters are injected.
///
/// # WP22 T2 — self-identifying hook command
///
/// `DUDUCLAW_AGENT_ID` never reaches this subprocess in production (only the
/// MCP server child process gets it), which left `check_caller_scope`
/// permanently inert. The fix bakes the identity into the installed command
/// itself as `--agent <agent_id>`: `resolve_hook_caller` (CLI side) reads it
/// arg-first, env-second. `agent_id` is only appended when it already has the
/// canonical agent-id shape ([`duduclaw_core::is_valid_agent_id`]) — an empty
/// or malformed directory name (should not happen for a real agent directory)
/// falls back to the pre-T2 command rather than injecting an unquoted-looking
/// oddity, which is no worse than the previous inert state.
///
/// # G1 round 2 — `--home`
///
/// `home` (when known and absolute) is appended as `--home "<path>"`, or
/// single-quoted when it holds a character that is special inside double
/// quotes. A non-UTF-8 home is left out; the hook then infers the home from
/// its working directory or refuses an employee caller.
/// An upgrade needs no special case: the existing "found but different →
/// overwrite in place" branch rewrites an entry without it on the next
/// `ensure_agent_hook_settings` call (every spawn and every boot).
fn build_hook_command(duduclaw_bin: &Path, agent_id: &str, home: Option<&Path>) -> String {
    build_named_hook_command(duduclaw_bin, HOOK_ID, agent_id, home)
}

fn build_named_hook_command(duduclaw_bin: &Path, hook: &str, agent_id: &str, home: Option<&Path>) -> String {
    // Claude Code hook commands are executed via the user's shell, so
    // quote both the binary path and the agent id defensively — the path
    // may contain spaces, and the id, while already validated, gets the
    // same treatment on general principle (defense in depth, not because
    // `is_valid_agent_id` currently allows anything shell-special).
    let mut cmd = format!("\"{}\" hook {hook}", duduclaw_bin.display());
    if duduclaw_core::is_valid_agent_id(agent_id) {
        cmd.push_str(&format!(" --agent \"{agent_id}\""));
    }
    if let Some(h) = home.filter(|h| h.is_absolute()).and_then(|h| h.to_str()) {
        if h.chars().any(|c| matches!(c, '"' | '$' | '`' | '\\' | '\n' | '\r' | '\'')) {
            // POSIX single quotes take everything literally; a `'` inside is
            // closed, escaped and reopened. Never omitted (round 3): without
            // `--home` the hook would have to infer the home.
            cmd.push_str(&format!(" --home '{}'", h.replace('\'', "'\\''")));
        } else {
            cmd.push_str(&format!(" --home \"{h}\""));
        }
    }
    cmd
}

/// `<home>` for `<home>/agents/<id>` or `<home>/agents/.ephemeral/<id>`;
/// `None` for any other shape.
fn home_of_agent_dir(agent_dir: &Path) -> Option<PathBuf> {
    let parent = agent_dir.parent()?;
    let agents = if parent
        .file_name()
        .is_some_and(|n| n.eq_ignore_ascii_case(".ephemeral"))
    {
        parent.parent()?
    } else {
        parent
    };
    if !agents
        .file_name()
        .is_some_and(|n| n.eq_ignore_ascii_case("agents"))
    {
        return None;
    }
    agents.parent().map(Path::to_path_buf)
}

/// Resolve the absolute path to the currently running `duduclaw` binary.
///
/// Preference order:
/// 1. `std::env::current_exe()` — the actual binary path
/// 2. `DUDUCLAW_BIN` env var (test / override hook)
/// 3. Fallback to "duduclaw" (relies on PATH at hook invocation time —
///    less robust, used only if current_exe fails)
pub fn resolve_duduclaw_bin() -> PathBuf {
    duduclaw_core::resolve_duduclaw_bin()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_bin() -> PathBuf {
        PathBuf::from("/usr/local/bin/duduclaw")
    }

    #[tokio::test]
    async fn egress_install_failures_refuse_only_ask_and_block() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let dir = home.join("agents/egress-test");
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        for mode in ["off", "shadow", "ask", "block"] {
            std::fs::write(home.join("config.toml"), format!("[security]\negress_guard='{mode}'\n")).unwrap();
            for invalid in ["[", "[]", "{\"disableAllHooks\":true}"] {
                std::fs::write(dir.join(".claude/settings.json"), invalid).unwrap();
                let result = ensure_spawn_hook_settings(home, &dir, &fake_bin()).await;
                assert_eq!(result.is_err(), matches!(mode, "ask" | "block"));
                if let Err(error) = result {
                    assert!(duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(&error));
                }
                assert_eq!(std::fs::read_to_string(dir.join(".claude/settings.json")).unwrap(), invalid);
            }
        }
    }

    #[test]
    fn egress_grok_document_names_native_web_and_shell_tools_and_bakes_identity() {
        #[cfg(unix)]
        let home = PathBuf::from("/srv/dudu");
        #[cfg(windows)]
        let home = PathBuf::from("C:/dudu-hook-home");
        assert!(home.is_absolute());
        let doc = guard_hooks_document(&home.join("agents/tester"), &fake_bin());
        let entries = doc["hooks"]["PreToolUse"].as_array().unwrap();
        let entry = entries.iter().find(|entry| entry[HOOK_TAG] == EGRESS_HOOK_ID).unwrap();
        let matcher: Vec<_> = entry["matcher"].as_str().unwrap().split('|').collect();
        for name in ["WebFetch", "WebSearch", "Bash", "Fetch", "Shell", "web_fetch", "web_search", "run_terminal_command"] {
            assert!(matcher.iter().any(|part| part == &name));
        }
        assert_eq!(entry["hooks"][0]["command"], format!(
            "\"/usr/local/bin/duduclaw\" hook egress-guard --agent \"tester\" --home \"{}\"", home.display(),
        ));
        assert!(entry["hooks"][0].get("timeout").is_none());
    }

    #[test]
    fn claude_explicit_identity_settings_and_bare_mode_gate() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("agents/.ephemeral/worker");
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        std::fs::write(dir.join(".claude/settings.json"), "{}").unwrap();
        for mode in ["off", "shadow", "ask", "block"] {
            std::fs::write(temp.path().join("config.toml"), format!("[security]\negress_guard='{mode}'")).unwrap();
            let args = claude_hook_args(temp.path(), Some(&dir), false).unwrap();
            if matches!(mode, "ask" | "block") {
                assert_eq!(args, vec!["--setting-sources".to_string(), String::new(), "--settings".to_string(), dir.join(".claude/settings.json").to_string_lossy().into_owned()]);
            } else {
                assert!(args.is_empty());
            }
            let bare = claude_hook_args(temp.path(), Some(&dir), true);
            assert_eq!(bare.is_err(), matches!(mode, "ask" | "block"));
            assert_eq!(claude_hook_args(temp.path(), None, false).is_err(), matches!(mode, "ask" | "block"));
            if let Err(error) = bare {
                assert!(duduclaw_agent::mcp_spawn_gate::is_spawn_gate_error(&error));
            }
        }
    }

    #[tokio::test]
    async fn creates_settings_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_dir = tmp.path().join("myagent");
        ensure_agent_hook_settings(&agent_dir, &fake_bin())
            .await
            .unwrap();

        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(agent_dir.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();

        let arr = settings
            .pointer("/hooks/PreToolUse")
            .and_then(|v| v.as_array())
            .expect("PreToolUse must be array");
        // Two tagged entries since RFC-23 §14.4: agent-file-guard first,
        // data-file-guard appended after it.
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0][HOOK_TAG], HOOK_ID);
        assert_eq!(arr[0]["matcher"], "Write|Edit|MultiEdit|NotebookEdit|Read|Bash");
        assert_eq!(arr[1][HOOK_TAG], DATA_FILE_HOOK_ID);
        assert_eq!(arr[1]["matcher"], "Read|Bash");
        let cmd = arr[0]["hooks"][0]["command"].as_str().unwrap();
        assert!(cmd.contains("hook agent-file-guard"));
        // WP22 T2 — the agent directory's own basename is baked into the
        // command so the hook can self-identify without relying on env.
        assert!(cmd.contains("--agent \"myagent\""), "command: {cmd}");
    }

    #[tokio::test]
    async fn merges_into_existing_settings_without_clobbering() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_dir = tmp.path().join("myagent");
        std::fs::create_dir_all(agent_dir.join(".claude")).unwrap();

        // Pretend the user has a custom permissions block and a hook they added themselves.
        let existing = json!({
            "permissions": { "allow": ["Bash"] },
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "Bash",
                        "hooks": [{ "type": "command", "command": "echo user-hook" }]
                    }
                ]
            }
        });
        std::fs::write(
            agent_dir.join(".claude/settings.json"),
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .unwrap();

        ensure_agent_hook_settings(&agent_dir, &fake_bin())
            .await
            .unwrap();

        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(agent_dir.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();

        // User's permissions preserved.
        assert_eq!(settings["permissions"]["allow"][0], "Bash");

        // Both hooks present.
        let arr = settings["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 4, "user hook + our three");
        assert_eq!(arr[0]["matcher"], "Bash", "user hook must come first");
        assert_eq!(arr[1][HOOK_TAG], HOOK_ID, "our hook must be appended");
        assert_eq!(arr[2][HOOK_TAG], DATA_FILE_HOOK_ID);
    }

    #[tokio::test]
    async fn is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_dir = tmp.path().join("myagent");
        ensure_agent_hook_settings(&agent_dir, &fake_bin())
            .await
            .unwrap();
        let first = std::fs::read_to_string(agent_dir.join(".claude/settings.json")).unwrap();

        ensure_agent_hook_settings(&agent_dir, &fake_bin())
            .await
            .unwrap();
        let second = std::fs::read_to_string(agent_dir.join(".claude/settings.json")).unwrap();

        assert_eq!(first, second, "second run must not mutate the file");
    }

    #[tokio::test]
    async fn updates_stale_command_path() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_dir = tmp.path().join("myagent");

        // First install with one bin path.
        ensure_agent_hook_settings(&agent_dir, &PathBuf::from("/old/path/duduclaw"))
            .await
            .unwrap();

        // Install again with a new bin path — should update in place, not append.
        ensure_agent_hook_settings(&agent_dir, &PathBuf::from("/new/path/duduclaw"))
            .await
            .unwrap();

        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(agent_dir.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();

        let arr = settings["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 3, "must not duplicate our tagged entries");
        let cmd = arr[0]["hooks"][0]["command"].as_str().unwrap();
        assert!(cmd.contains("/new/path/duduclaw"));
        assert!(!cmd.contains("/old/path/duduclaw"));
    }

    #[tokio::test]
    async fn gracefully_handles_non_object_root() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_dir = tmp.path().join("myagent");
        std::fs::create_dir_all(agent_dir.join(".claude")).unwrap();
        std::fs::write(
            agent_dir.join(".claude/settings.json"),
            "[1, 2, 3]", // valid JSON but not an object
        )
        .unwrap();

        // Must not panic, must not overwrite with `{}`.
        ensure_agent_hook_settings(&agent_dir, &fake_bin())
            .await
            .unwrap_err();

        let content = std::fs::read_to_string(agent_dir.join(".claude/settings.json")).unwrap();
        assert_eq!(content.trim(), "[1, 2, 3]", "corrupt file left untouched");
    }

    #[tokio::test]
    async fn gracefully_handles_invalid_json() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_dir = tmp.path().join("myagent");
        std::fs::create_dir_all(agent_dir.join(".claude")).unwrap();
        std::fs::write(agent_dir.join(".claude/settings.json"), "{ not valid json").unwrap();

        // Must not panic.
        ensure_agent_hook_settings(&agent_dir, &fake_bin())
            .await
            .unwrap_err();

        let content = std::fs::read_to_string(agent_dir.join(".claude/settings.json")).unwrap();
        assert_eq!(content, "{ not valid json", "invalid file left untouched");
    }

    #[test]
    fn build_hook_command_quotes_path() {
        let cmd = build_hook_command(Path::new("/path with spaces/duduclaw"), "myagent", None);
        assert!(cmd.starts_with('"'));
        assert!(cmd.contains("/path with spaces/duduclaw"));
        assert!(cmd.contains("hook agent-file-guard"));
    }

    #[test]
    fn build_hook_command_quotes_the_agent_id_too() {
        let cmd = build_hook_command(Path::new("/usr/local/bin/duduclaw"), "sales-rep", None);
        assert!(cmd.contains("--agent \"sales-rep\""), "command: {cmd}");
    }

    #[test]
    fn build_hook_command_falls_back_when_agent_id_is_invalid() {
        // Empty / malformed directory basenames (should not happen for a real
        // agent directory, but the installer must not inject garbage into a
        // shell command) fall back to the pre-T2 shape — no worse than the
        // inert state this fix replaces.
        for bad_id in ["", "has spaces", "has/slash", "semi;colon"] {
            let cmd = build_hook_command(Path::new("/usr/local/bin/duduclaw"), bad_id, None);
            assert!(!cmd.contains("--agent"), "id={bad_id:?} cmd={cmd}");
            assert!(cmd.contains("hook agent-file-guard"));
        }
    }

    #[tokio::test]
    async fn upgrades_a_pre_wp22_t2_entry_missing_the_agent_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_dir = tmp.path().join("myagent");
        std::fs::create_dir_all(agent_dir.join(".claude")).unwrap();

        // Simulate a settings.json installed by the old installer: our tag
        // is present, but the command has no `--agent` suffix.
        let stale = json!({
            "hooks": {
                "PreToolUse": [{
                    HOOK_TAG: HOOK_ID,
                    "matcher": "Write|Edit|MultiEdit|Bash",
                    "hooks": [{
                        "type": "command",
                        "command": "\"/usr/local/bin/duduclaw\" hook agent-file-guard",
                    }]
                }]
            }
        });
        std::fs::write(
            agent_dir.join(".claude/settings.json"),
            serde_json::to_string_pretty(&stale).unwrap(),
        )
        .unwrap();

        ensure_agent_hook_settings(&agent_dir, &fake_bin())
            .await
            .unwrap();

        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(agent_dir.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        let arr = settings["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(
            arr.len(),
            3,
            "must upgrade in place with the data and egress guards"
        );
        let cmd = arr[0]["hooks"][0]["command"].as_str().unwrap();
        assert!(cmd.contains("--agent \"myagent\""), "command: {cmd}");
    }

    #[tokio::test]
    async fn installed_hook_command_carries_the_agent_flag_and_the_settings_file_is_a_protected_surface()
     {
        // Integration assertion tying WP22 T2's two halves together: the
        // installed command self-identifies, AND the file that carries it
        // cannot be rewritten by the agent it names (frozen outright by
        // `ProtectedSurface::HookSettings`).
        let home = tempfile::tempdir().unwrap();
        let agent_dir = home.path().join("agents").join("sales-rep");
        ensure_agent_hook_settings(&agent_dir, &fake_bin())
            .await
            .unwrap();

        let settings_path = agent_dir.join(".claude/settings.json");
        let settings: Value =
            serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        let cmd = settings["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(cmd.contains("--agent \"sales-rep\""), "command: {cmd}");

        assert_eq!(
            duduclaw_core::classify_identity_surface(&settings_path, home.path()),
            Some(duduclaw_core::ProtectedSurface::HookSettings),
        );
    }

    // ── G1 round 2: `--home` and NotebookEdit ─────────────────────────────

    #[tokio::test]
    async fn installed_command_carries_the_home_and_upgrades_an_entry_without_it() {
        let home = tempfile::tempdir().unwrap();
        let agent_dir = home.path().join("agents").join("sales-rep");
        std::fs::create_dir_all(agent_dir.join(".claude")).unwrap();
        // An entry from the previous release: `--agent`, no `--home`, old matcher.
        let stale = json!({"hooks": {"PreToolUse": [{
            HOOK_TAG: HOOK_ID,
            "matcher": "Write|Edit|MultiEdit|Bash",
            "hooks": [{"type": "command",
                "command": "\"/usr/local/bin/duduclaw\" hook agent-file-guard --agent \"sales-rep\""}]
        }]}});
        std::fs::write(
            agent_dir.join(".claude/settings.json"),
            serde_json::to_string_pretty(&stale).unwrap(),
        )
        .unwrap();
        ensure_agent_hook_settings(&agent_dir, &fake_bin()).await.unwrap();
        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(agent_dir.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        let arr = settings["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 3, "upgraded in place");
        assert_eq!(arr[0]["matcher"], AGENT_FILE_GUARD_MATCHER);
        let cmd = arr[0]["hooks"][0]["command"].as_str().unwrap();
        // A Windows temp dir (`C:\Users\…\Temp\…`) holds backslashes, which
        // `build_hook_command` single-quotes; a Unix temp dir has none of the
        // special characters and is double-quoted.
        let h = home.path().to_str().unwrap();
        let expected = if h.contains('\\') {
            format!("--home '{h}'")
        } else {
            format!("--home \"{h}\"")
        };
        assert!(cmd.contains(&expected), "command: {cmd}");
        assert!(cmd.contains("--agent \"sales-rep\""), "command: {cmd}");
    }

    #[test]
    fn home_is_derived_for_regular_and_ephemeral_agent_dirs_only() {
        assert_eq!(
            home_of_agent_dir(Path::new("/h/.duduclaw/agents/x")),
            Some(PathBuf::from("/h/.duduclaw"))
        );
        assert_eq!(
            home_of_agent_dir(Path::new("/h/.duduclaw/agents/.ephemeral/eph-1")),
            Some(PathBuf::from("/h/.duduclaw"))
        );
        assert_eq!(home_of_agent_dir(Path::new("/tmp/myagent")), None);
    }

    #[test]
    fn home_with_shell_special_characters_is_quoted_not_left_out() {
        // Only an absolute home is written, and `/h/…` has no drive on
        // Windows, so the root is `C:/h` there (forward slashes, so no
        // backslash adds a quoting reason the case does not name).
        let root = if cfg!(windows) { "C:/h" } else { "/h" };
        for (bad, quoted) in [
            ("a\"b", "a\"b'"),
            ("$HOME", "$HOME'"),
            ("`x`", "`x`'"),
            ("it's", "it'\\''s'"),
            // The separator of a Windows path.
            ("a\\b", "a\\b'"),
        ] {
            let bad = format!("{root}/{bad}");
            let quoted = format!("'{root}/{quoted}");
            let cmd = build_hook_command(Path::new("/bin/duduclaw"), "x", Some(Path::new(&bad)));
            assert!(cmd.contains(&format!("--home {quoted}")), "{bad}: {cmd}");
        }
        let cmd = build_hook_command(Path::new("/bin/duduclaw"), "x", Some(Path::new("rel/home")));
        assert!(!cmd.contains("--home"), "relative home must be ignored: {cmd}");
    }

    // ── RFC-23 §14.4 data-file guard (H10: Rust subcommand, not a script) ───

    #[tokio::test]
    async fn registers_the_data_file_guard_as_a_rust_subcommand() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_dir = tmp.path().join("myagent");
        ensure_agent_hook_settings(&agent_dir, &fake_bin())
            .await
            .unwrap();

        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(agent_dir.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        let arr = settings["hooks"]["PreToolUse"].as_array().unwrap();
        let entry = arr
            .iter()
            .find(|e| e[HOOK_TAG] == DATA_FILE_HOOK_ID)
            .expect("data-file-guard entry");
        assert_eq!(entry["matcher"], "Read|Bash");
        let cmd = entry["hooks"][0]["command"].as_str().unwrap();
        assert!(
            cmd.contains("hook data-file-guard"),
            "must invoke the Rust subcommand: {cmd}"
        );
        assert!(
            !cmd.contains("bash "),
            "H10 regression: no shell wrapper may remain (Windows has no bash): {cmd}"
        );
        assert!(cmd.starts_with('"'), "binary path must be quoted: {cmd}");

        // No script is written any more.
        assert!(
            !agent_dir.join(".claude/hooks/data-file-guard.sh").exists(),
            "the shell script must not be installed"
        );
    }

    #[tokio::test]
    async fn upgrade_deletes_a_leftover_shell_script() {
        // An agent installed before H10 still has the script on disk. Leaving
        // it there invites someone to "fix" the guard by editing a file that
        // is no longer referenced by anything.
        let tmp = tempfile::tempdir().unwrap();
        let agent_dir = tmp.path().join("myagent");
        let hooks_dir = agent_dir.join(".claude/hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        let stale = hooks_dir.join("data-file-guard.sh");
        std::fs::write(&stale, "#!/usr/bin/env bash\nexit 0\n").unwrap();

        ensure_agent_hook_settings(&agent_dir, &fake_bin())
            .await
            .unwrap();
        assert!(!stale.exists(), "stale script must be removed on upgrade");
    }

    #[tokio::test]
    async fn data_file_guard_registration_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_dir = tmp.path().join("myagent");
        ensure_agent_hook_settings(&agent_dir, &fake_bin())
            .await
            .unwrap();
        let settings_path = agent_dir.join(".claude/settings.json");
        let before = std::fs::read_to_string(&settings_path).unwrap();

        ensure_agent_hook_settings(&agent_dir, &fake_bin())
            .await
            .unwrap();
        let after = std::fs::read_to_string(&settings_path).unwrap();
        assert_eq!(before, after, "a second install must change nothing");

        let settings: Value = serde_json::from_str(&after).unwrap();
        let arr = settings["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(
            arr.iter()
                .filter(|e| e[HOOK_TAG] == DATA_FILE_HOOK_ID)
                .count(),
            1,
            "exactly one tagged entry, never a duplicate"
        );
    }

    // The guard's own behaviour (modes, extensions, CJK names, fail-open on a
    // malformed envelope) lives with the decision function it now shares with
    // the CLI subcommand: `duduclaw_core::data_file_guard`.

    // ── Grok project hooks ──────────────────────────────────────

    #[tokio::test]
    async fn grok_guard_hooks_carry_the_same_entries_as_the_claude_settings() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_dir = tmp.path().join("agents").join("trader");
        tokio::fs::create_dir_all(&agent_dir).await.unwrap();
        ensure_agent_hook_settings(&agent_dir, &fake_bin()).await.unwrap();
        let claude: Value = serde_json::from_str(
            &tokio::fs::read_to_string(agent_dir.join(".claude/settings.json"))
                .await
                .unwrap(),
        )
        .unwrap();

        write_grok_guard_hooks(&agent_dir, &agent_dir, &fake_bin())
            .await
            .unwrap();
        let path = agent_dir.join(".grok/hooks/duduclaw-guards-trader.json");
        let grok: Value =
            serde_json::from_str(&tokio::fs::read_to_string(&path).await.unwrap()).unwrap();
        assert_eq!(grok["hooks"], claude["hooks"]);
        let cmd = grok["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(cmd.contains("--agent \"trader\""), "{cmd}");
        // Quoting of the home differs per platform (`\\` in a Windows path).
        assert!(cmd.contains(" --home "), "{cmd}");
    }

    #[tokio::test]
    async fn grok_guard_hooks_in_another_work_root_keep_the_employee_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_dir = tmp.path().join("agents").join("trader");
        let root = tmp.path().join("shared-workspace");
        tokio::fs::create_dir_all(&agent_dir).await.unwrap();
        tokio::fs::create_dir_all(&root).await.unwrap();
        write_grok_guard_hooks(&root, &agent_dir, &fake_bin())
            .await
            .unwrap();
        let body = tokio::fs::read_to_string(root.join(".grok/hooks/duduclaw-guards-trader.json"))
            .await
            .unwrap();
        assert!(body.contains("--agent \\\"trader\\\""), "{body}");
        assert!(!agent_dir.join(".grok").exists());
    }

    #[tokio::test]
    async fn grok_guard_hooks_rewrite_a_stale_file_and_leave_no_temp_files() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_dir = tmp.path().join("agents").join("trader");
        let hooks_dir = agent_dir.join(".grok/hooks");
        tokio::fs::create_dir_all(&hooks_dir).await.unwrap();
        let path = hooks_dir.join("duduclaw-guards-trader.json");
        tokio::fs::write(&path, "{}").await.unwrap();
        for _ in 0..2 {
            write_grok_guard_hooks(&agent_dir, &agent_dir, &fake_bin())
                .await
                .unwrap();
        }
        let expected =
            serde_json::to_string_pretty(&guard_hooks_document(&agent_dir, &fake_bin())).unwrap();
        assert_eq!(tokio::fs::read_to_string(&path).await.unwrap(), expected);
        let mut names = Vec::new();
        let mut rd = tokio::fs::read_dir(&hooks_dir).await.unwrap();
        while let Some(e) = rd.next_entry().await.unwrap() {
            names.push(e.file_name().to_string_lossy().to_string());
        }
        assert_eq!(names, vec!["duduclaw-guards-trader.json".to_string()]);
    }

    #[tokio::test]
    async fn grok_guard_hooks_of_employees_sharing_a_root_live_side_by_side() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("shared-workspace");
        tokio::fs::create_dir_all(&root).await.unwrap();
        for id in ["planner", "executor"] {
            let agent_dir = tmp.path().join("agents").join(id);
            tokio::fs::create_dir_all(&agent_dir).await.unwrap();
            write_grok_guard_hooks(&root, &agent_dir, &fake_bin())
                .await
                .unwrap();
        }
        for id in ["planner", "executor"] {
            let body =
                tokio::fs::read_to_string(root.join(format!(".grok/hooks/duduclaw-guards-{id}.json")))
                    .await
                    .unwrap();
            assert!(body.contains(&format!("--agent \\\"{id}\\\"")), "{body}");
        }
        assert_eq!(
            grok_guard_hooks_file_name(Path::new("/x/agents/bad name")),
            GROK_GUARD_HOOKS_FILE
        );
    }

    #[tokio::test]
    async fn grok_guard_hooks_report_a_write_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_dir = tmp.path().join("agents").join("trader");
        tokio::fs::create_dir_all(&agent_dir).await.unwrap();
        // `.grok` is a file, so the hook directory cannot be created.
        tokio::fs::write(agent_dir.join(".grok"), "not a directory")
            .await
            .unwrap();
        assert!(
            write_grok_guard_hooks(&agent_dir, &agent_dir, &fake_bin())
                .await
                .is_err()
        );
    }
}
