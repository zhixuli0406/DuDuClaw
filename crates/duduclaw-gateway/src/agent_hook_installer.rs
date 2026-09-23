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
//! The same installer also drops a shell script into
//! `<agent_dir>/.claude/hooks/data-file-guard.sh` and registers it as a
//! PreToolUse hook for `Read` and `Bash`. Where `agent-file-guard` protects
//! DuDuClaw's own structure files, this one protects the *customer's* data:
//! `Read`/`Bash` are built-in tools, so a CSV read through them never passes
//! the MCP redaction choke point. The script is inert unless the gateway sets
//! `DUDUCLAW_DATA_FILE_GUARD` at spawn time, which it only does when redaction
//! is actually active for that agent — so an install on a gateway with
//! redaction off changes no behavior at all.
//!
//! Two tagged entries now share `hooks.PreToolUse`, distinguished by the
//! `_duduclaw_hook` value ([`HOOK_ID`] vs [`DATA_FILE_HOOK_ID`]); the merge is
//! per-entry, so a user's own hooks and the other tagged entry are never
//! disturbed.
//!
//! Unlike `agent-file-guard` (a Rust subcommand, cross-platform by design),
//! the data-file guard is a POSIX shell script and is therefore inert on a
//! Windows host with no bash on PATH — the hook command fails and Claude Code
//! treats that as allow. Recorded here rather than left to be discovered: a
//! Windows deployment gets its protection from the MCP tool surface alone
//! until this is ported to `duduclaw hook data-file-guard`.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use tracing::{debug, warn};

/// Tag embedded in the hook descriptor so we can find + update our own
/// entry on subsequent runs without touching user-added hooks.
const HOOK_TAG: &str = "_duduclaw_hook";

/// Sentinel value identifying the agent-file-guard hook specifically.
const HOOK_ID: &str = "agent-file-guard";

/// Sentinel for the RFC-23 §14.4 data-file guard entry.
const DATA_FILE_HOOK_ID: &str = "data-file-guard";

/// Filename the guard script is installed under, inside `.claude/hooks/`.
const DATA_FILE_HOOK_SCRIPT: &str = "data-file-guard.sh";

/// The guard script itself, compiled into the binary so a fresh install has
/// nothing to fetch and a redeploy cannot leave a stale copy behind.
const DATA_FILE_HOOK_SOURCE: &str = include_str!("../hooks/data-file-guard.sh");

/// Ensure `<agent_dir>/.claude/settings.json` contains the agent-file-guard
/// PreToolUse hook pointing at `duduclaw_bin`.
///
/// Best-effort: on any filesystem or JSON error, this logs a warning and
/// returns `Ok(())` rather than blocking agent startup. Guard enforcement
/// is a defense-in-depth layer — the primary contract is still "use
/// `create_agent` MCP tool".
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
                    return Ok(());
                }
                Err(e) => {
                    warn!(
                        path = %settings_path.display(),
                        error = %e,
                        "settings.json has invalid JSON — skipping hook install"
                    );
                    return Ok(());
                }
            }
        }
        _ => json!({}),
    };

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

    // RFC-23 §14.4 — drop the data-file guard script next to the settings
    // file. Best-effort: a write failure means the entry below would point at
    // a missing script, so the registration is skipped too rather than left
    // dangling (a hook whose command does not exist is a per-tool-call error
    // in the agent's transcript, which is worse than no hook).
    let script_path = agent_dir
        .join(".claude")
        .join("hooks")
        .join(DATA_FILE_HOOK_SCRIPT);
    let script_ready = match install_data_file_guard_script(&script_path).await {
        Ok(()) => true,
        Err(e) => {
            warn!(
                path = %script_path.display(),
                error = %e,
                "Failed to install data-file-guard script — the hook will not be registered"
            );
            false
        }
    };

    // Merge our hook descriptors into hooks.PreToolUse. Both run: a `false`
    // from either only means "already up to date".
    let mut updated = merge_agent_file_guard_hook(&mut root, duduclaw_bin, agent_id);
    if script_ready {
        updated |= merge_data_file_guard_hook(&mut root, &script_path);
    }
    if !updated {
        debug!(path = %settings_path.display(), "Hooks already up to date");
        return Ok(());
    }

    // Write back atomically: temp file + rename.
    if let Some(parent) = settings_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let tmp = settings_path.with_extension("json.tmp");
    let pretty = serde_json::to_string_pretty(&root)
        .unwrap_or_else(|_| "{}".to_string());
    tokio::fs::write(&tmp, pretty).await?;
    tokio::fs::rename(&tmp, &settings_path).await?;

    debug!(
        path = %settings_path.display(),
        bin = %duduclaw_bin.display(),
        "Installed agent-file-guard + data-file-guard PreToolUse hooks"
    );
    Ok(())
}

/// Write the guard script to `path`, creating `.claude/hooks/` as needed.
///
/// Idempotent by content: an unchanged script is not rewritten, so the
/// installer running on every spawn does not churn the file's mtime. On Unix
/// the script is made executable — Claude Code runs a hook command through the
/// shell, and `bash script.sh` would work without the bit, but the installed
/// command names the script directly so the bit is load-bearing.
async fn install_data_file_guard_script(path: &Path) -> std::io::Result<()> {
    if let Ok(existing) = tokio::fs::read_to_string(path).await
        && existing == DATA_FILE_HOOK_SOURCE
    {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let tmp = path.with_extension("sh.tmp");
    tokio::fs::write(&tmp, DATA_FILE_HOOK_SOURCE).await?;
    tokio::fs::rename(&tmp, path).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).await?;
    }
    Ok(())
}

/// Merge the RFC-23 §14.4 data-file-guard descriptor into
/// `root["hooks"]["PreToolUse"]`, matched on `Read|Bash`.
///
/// Structurally identical to [`merge_agent_file_guard_hook`] but keyed on its
/// own [`DATA_FILE_HOOK_ID`], so the two entries coexist and each upgrades
/// independently.
fn merge_data_file_guard_hook(root: &mut Value, script_path: &Path) -> bool {
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
            "command": format!("bash \"{}\"", script_path.display()),
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
fn merge_agent_file_guard_hook(root: &mut Value, duduclaw_bin: &Path, agent_id: &str) -> bool {
    let desired_command = build_hook_command(duduclaw_bin, agent_id);
    // Bash is included so the guard can catch agents that bypass Write/Edit
    // by running `mkdir -p /project/.claude/agents/foo` or `cat > .../agent.toml`
    // via the shell. The CLI handler dispatches on tool name internally.
    let desired_entry = json!({
        HOOK_TAG: HOOK_ID,
        "matcher": "Write|Edit|MultiEdit|Bash",
        "hooks": [{
            "type": "command",
            "command": desired_command,
        }]
    });

    let Some(arr) = pre_tool_use_array(root) else {
        return false;
    };
    for item in arr.iter_mut() {
        if item
            .get(HOOK_TAG)
            .and_then(|v| v.as_str())
            == Some(HOOK_ID)
        {
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
fn build_hook_command(duduclaw_bin: &Path, agent_id: &str) -> String {
    // Claude Code hook commands are executed via the user's shell, so
    // quote both the binary path and the agent id defensively — the path
    // may contain spaces, and the id, while already validated, gets the
    // same treatment on general principle (defense in depth, not because
    // `is_valid_agent_id` currently allows anything shell-special).
    let base = format!("\"{}\" hook agent-file-guard", duduclaw_bin.display());
    if duduclaw_core::is_valid_agent_id(agent_id) {
        format!("{base} --agent \"{agent_id}\"")
    } else {
        base
    }
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
    async fn creates_settings_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_dir = tmp.path().join("myagent");
        ensure_agent_hook_settings(&agent_dir, &fake_bin()).await.unwrap();

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
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0][HOOK_TAG], HOOK_ID);
        assert_eq!(arr[0]["matcher"], "Write|Edit|MultiEdit|Bash");
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

        ensure_agent_hook_settings(&agent_dir, &fake_bin()).await.unwrap();

        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(agent_dir.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();

        // User's permissions preserved.
        assert_eq!(settings["permissions"]["allow"][0], "Bash");

        // Both hooks present.
        let arr = settings["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 3, "user hook + our two");
        assert_eq!(arr[0]["matcher"], "Bash", "user hook must come first");
        assert_eq!(arr[1][HOOK_TAG], HOOK_ID, "our hook must be appended");
        assert_eq!(arr[2][HOOK_TAG], DATA_FILE_HOOK_ID);
    }

    #[tokio::test]
    async fn is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_dir = tmp.path().join("myagent");
        ensure_agent_hook_settings(&agent_dir, &fake_bin()).await.unwrap();
        let first = std::fs::read_to_string(agent_dir.join(".claude/settings.json")).unwrap();

        ensure_agent_hook_settings(&agent_dir, &fake_bin()).await.unwrap();
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
        assert_eq!(arr.len(), 2, "must not duplicate our tagged entries");
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
        ensure_agent_hook_settings(&agent_dir, &fake_bin()).await.unwrap();

        let content = std::fs::read_to_string(agent_dir.join(".claude/settings.json")).unwrap();
        assert_eq!(content.trim(), "[1, 2, 3]", "corrupt file left untouched");
    }

    #[tokio::test]
    async fn gracefully_handles_invalid_json() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_dir = tmp.path().join("myagent");
        std::fs::create_dir_all(agent_dir.join(".claude")).unwrap();
        std::fs::write(
            agent_dir.join(".claude/settings.json"),
            "{ not valid json",
        )
        .unwrap();

        // Must not panic.
        ensure_agent_hook_settings(&agent_dir, &fake_bin()).await.unwrap();

        let content = std::fs::read_to_string(agent_dir.join(".claude/settings.json")).unwrap();
        assert_eq!(content, "{ not valid json", "invalid file left untouched");
    }

    #[test]
    fn build_hook_command_quotes_path() {
        let cmd = build_hook_command(Path::new("/path with spaces/duduclaw"), "myagent");
        assert!(cmd.starts_with('"'));
        assert!(cmd.contains("/path with spaces/duduclaw"));
        assert!(cmd.contains("hook agent-file-guard"));
    }

    #[test]
    fn build_hook_command_quotes_the_agent_id_too() {
        let cmd = build_hook_command(Path::new("/usr/local/bin/duduclaw"), "sales-rep");
        assert!(cmd.contains("--agent \"sales-rep\""), "command: {cmd}");
    }

    #[test]
    fn build_hook_command_falls_back_when_agent_id_is_invalid() {
        // Empty / malformed directory basenames (should not happen for a real
        // agent directory, but the installer must not inject garbage into a
        // shell command) fall back to the pre-T2 shape — no worse than the
        // inert state this fix replaces.
        for bad_id in ["", "has spaces", "has/slash", "semi;colon"] {
            let cmd = build_hook_command(Path::new("/usr/local/bin/duduclaw"), bad_id);
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

        ensure_agent_hook_settings(&agent_dir, &fake_bin()).await.unwrap();

        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(agent_dir.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        let arr = settings["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(
            arr.len(),
            2,
            "must upgrade in place (+ the §14.4 entry), not append a duplicate"
        );
        let cmd = arr[0]["hooks"][0]["command"].as_str().unwrap();
        assert!(cmd.contains("--agent \"myagent\""), "command: {cmd}");
    }

    #[tokio::test]
    async fn installed_hook_command_carries_the_agent_flag_and_the_settings_file_is_a_protected_surface(
    ) {
        // Integration assertion tying WP22 T2's two halves together: the
        // installed command self-identifies, AND the file that carries it
        // cannot be rewritten by the agent it names (frozen outright by
        // `ProtectedSurface::HookSettings`).
        let home = tempfile::tempdir().unwrap();
        let agent_dir = home.path().join("agents").join("sales-rep");
        ensure_agent_hook_settings(&agent_dir, &fake_bin()).await.unwrap();

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

    // ── RFC-23 §14.4 data-file guard ────────────────────────────────────────

    #[tokio::test]
    async fn installs_the_data_file_guard_script_and_registers_it() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_dir = tmp.path().join("myagent");
        ensure_agent_hook_settings(&agent_dir, &fake_bin()).await.unwrap();

        let script = agent_dir.join(".claude/hooks/data-file-guard.sh");
        assert!(script.is_file(), "guard script must be installed");
        let body = std::fs::read_to_string(&script).unwrap();
        assert_eq!(body, DATA_FILE_HOOK_SOURCE);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&script).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o755, "script must be executable");
        }

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
        assert!(cmd.contains("data-file-guard.sh"), "command: {cmd}");
        assert!(cmd.starts_with("bash \""), "command must be quoted: {cmd}");
    }

    #[tokio::test]
    async fn data_file_guard_install_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let agent_dir = tmp.path().join("myagent");
        ensure_agent_hook_settings(&agent_dir, &fake_bin()).await.unwrap();
        let script = agent_dir.join(".claude/hooks/data-file-guard.sh");
        let before = std::fs::metadata(&script).unwrap().modified().unwrap();

        ensure_agent_hook_settings(&agent_dir, &fake_bin()).await.unwrap();
        let after = std::fs::metadata(&script).unwrap().modified().unwrap();
        assert_eq!(before, after, "unchanged script must not be rewritten");
    }

    // ── The script's own behaviour, exercised by running it ──────────────────

    #[cfg(unix)]
    fn run_guard(mode: &str, payload: &str) -> (i32, String) {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("data-file-guard.sh");
        std::fs::write(&script, DATA_FILE_HOOK_SOURCE).unwrap();

        let mut child = Command::new("bash")
            .arg(&script)
            .env("DUDUCLAW_DATA_FILE_GUARD", mode)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("bash must be available");
        // The script may exit without ever reading stdin (`off`/unset mode
        // returns before the `cat`), so the write can race the child's exit
        // and fail with EPIPE on a loaded runner. That is the behaviour under
        // test, not a harness failure — tolerate exactly BrokenPipe.
        match child.stdin.as_mut().unwrap().write_all(payload.as_bytes()) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
            Err(e) => panic!("writing hook payload to stdin: {e}"),
        }
        let out = child.wait_with_output().unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    #[cfg(unix)]
    #[test]
    fn guard_blocks_reading_a_data_file_and_allows_a_markdown_one() {
        let (code, stderr) = run_guard(
            "on",
            r#"{"tool_name":"Read","tool_input":{"file_path":"/w/customers.csv"}}"#,
        );
        assert_eq!(code, 2, "Read of a .csv must be blocked");
        assert!(stderr.contains("csv_read"), "stderr: {stderr}");
        assert!(stderr.contains("去識別化"), "stderr: {stderr}");

        let (code, _) = run_guard(
            "on",
            r#"{"tool_name":"Read","tool_input":{"file_path":"/w/notes.md"}}"#,
        );
        assert_eq!(code, 0, "Read of a .md must be allowed");
    }

    #[cfg(unix)]
    #[test]
    fn guard_blocks_a_bash_command_naming_a_data_file() {
        let (code, _) = run_guard(
            "on",
            r#"{"tool_name":"Bash","tool_input":{"command":"head customers.csv"}}"#,
        );
        assert_eq!(code, 2);

        let (code, _) = run_guard(
            "on",
            r#"{"tool_name":"Bash","tool_input":{"command":"echo hello world"}}"#,
        );
        assert_eq!(code, 0, "an unrelated command must pass");
    }

    #[cfg(unix)]
    #[test]
    fn guard_read_only_mode_leaves_bash_alone() {
        let (code, _) = run_guard(
            "read_only",
            r#"{"tool_name":"Bash","tool_input":{"command":"head customers.csv"}}"#,
        );
        assert_eq!(code, 0, "read_only must not gate Bash");

        let (code, _) = run_guard(
            "read_only",
            r#"{"tool_name":"Read","tool_input":{"file_path":"/w/a.xlsx"}}"#,
        );
        assert_eq!(code, 2, "read_only still gates Read");
    }

    #[cfg(unix)]
    #[test]
    fn guard_is_inert_when_off_or_unset() {
        for mode in ["off", "", "nonsense"] {
            let (code, _) = run_guard(
                mode,
                r#"{"tool_name":"Read","tool_input":{"file_path":"/w/customers.csv"}}"#,
            );
            assert_eq!(code, 0, "mode {mode:?} must allow");
        }
    }

    #[cfg(unix)]
    #[test]
    fn guard_covers_every_spreadsheet_extension_including_cjk_names() {
        for ext in ["csv", "tsv", "xlsx", "xlsm", "xls", "ods", "XLSX"] {
            let payload = format!(
                r#"{{"tool_name":"Read","tool_input":{{"file_path":"/w/客戶清單.{ext}"}}}}"#
            );
            let (code, _) = run_guard("on", &payload);
            assert_eq!(code, 2, "extension {ext} must be blocked");
        }
    }

    #[cfg(unix)]
    #[test]
    fn guard_fails_open_on_a_malformed_envelope_or_another_tool() {
        let (code, _) = run_guard("on", "this is not json");
        assert_eq!(code, 0, "a malformed envelope must not brick the agent");

        let (code, _) = run_guard(
            "on",
            r#"{"tool_name":"Write","tool_input":{"file_path":"/w/out.csv"}}"#,
        );
        assert_eq!(code, 0, "Write is outside this guard's remit");
    }
}
