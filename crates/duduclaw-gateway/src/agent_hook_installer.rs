//! Ensure each agent directory has a `.claude/settings.json` with the
//! `agent-file-guard` PreToolUse hook registered.
//!
//! The hook delegates to `duduclaw hook agent-file-guard`, which exits 2
//! (blocks the tool call) if the agent tries to Write / Edit / MultiEdit
//! an agent-structure file (`agent.toml` / `SOUL.md` / etc.) outside the
//! canonical `<home>/agents/<name>/` tree.
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

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use tracing::{debug, warn};

/// Tag embedded in the hook descriptor so we can find + update our own
/// entry on subsequent runs without touching user-added hooks.
const HOOK_TAG: &str = "_duduclaw_hook";

/// Sentinel value identifying the agent-file-guard hook specifically.
const HOOK_ID: &str = "agent-file-guard";

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

    // Merge our hook descriptor into hooks.PreToolUse.
    let updated = merge_agent_file_guard_hook(&mut root, duduclaw_bin);
    if !updated {
        debug!(path = %settings_path.display(), "Hook already up to date");
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
        "Installed agent-file-guard PreToolUse hook"
    );
    Ok(())
}

/// Merge the hook descriptor into `root["hooks"]["PreToolUse"]`.
///
/// Returns `true` if anything changed (caller should persist), `false`
/// if the hook was already present and up to date.
fn merge_agent_file_guard_hook(root: &mut Value, duduclaw_bin: &Path) -> bool {
    let hooks = root
        .as_object_mut()
        .expect("root must be object — checked by caller")
        .entry("hooks")
        .or_insert_with(|| json!({}));

    if !hooks.is_object() {
        *hooks = json!({});
    }

    let pre_tool_use = hooks
        .as_object_mut()
        .unwrap()
        .entry("PreToolUse")
        .or_insert_with(|| json!([]));

    if !pre_tool_use.is_array() {
        *pre_tool_use = json!([]);
    }

    let desired_command = build_hook_command(duduclaw_bin);
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

    let arr = pre_tool_use.as_array_mut().unwrap();
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
fn build_hook_command(duduclaw_bin: &Path) -> String {
    // Claude Code hook commands are executed via the user's shell, so
    // quote the binary path defensively in case it contains spaces.
    format!(
        "\"{}\" hook agent-file-guard",
        duduclaw_bin.display()
    )
}

/// Resolve the absolute path to the currently running `duduclaw` binary.
///
/// Preference order:
/// 1. `std::env::current_exe()` — the actual binary path
/// 2. `DUDUCLAW_BIN` env var (test / override hook)
/// 3. Fallback to "duduclaw" (relies on PATH at hook invocation time —
///    less robust, used only if current_exe fails)
pub fn resolve_duduclaw_bin() -> PathBuf {
    if let Ok(override_path) = std::env::var("DUDUCLAW_BIN")
        && !override_path.is_empty()
    {
        return PathBuf::from(override_path);
    }
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("duduclaw"))
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
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0][HOOK_TAG], HOOK_ID);
        assert_eq!(arr[0]["matcher"], "Write|Edit|MultiEdit|Bash");
        assert!(
            arr[0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .contains("hook agent-file-guard")
        );
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
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["matcher"], "Bash", "user hook must come first");
        assert_eq!(arr[1][HOOK_TAG], HOOK_ID, "our hook must be appended");
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
        assert_eq!(arr.len(), 1, "must not duplicate our tagged entry");
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
        let cmd = build_hook_command(Path::new("/path with spaces/duduclaw"));
        assert!(cmd.starts_with('"'));
        assert!(cmd.contains("/path with spaces/duduclaw"));
        assert!(cmd.contains("hook agent-file-guard"));
    }
}
