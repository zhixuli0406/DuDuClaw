//! WP10 one-time migration — undo the dashboard's silent PTY-pool enablement.
//!
//! **Why this exists.** Between v1.44.0 and v1.49.x the agent edit page carried
//! an effect ("Change 3c", commit `53c5c91`, 2026-07-19) that, on detecting a
//! Claude Code OAuth login, wrote `pty_pool_enabled = true` +
//! `worker_managed = true` into `agent.toml`. The page also auto-saves, so the
//! write happened when a user merely *opened* the page — no toggle, no consent,
//! no notice. That contradicted the documented default (PTY pool off) and put
//! production installs on the interactive-REPL path.
//!
//! On a contended single OAuth account that path stalls (2026-08-04 field
//! incident: 4 stalls and 2 account exhaustions in 90 minutes). Removing the
//! effect stops NEW installs from being affected but does nothing for the
//! agents whose `agent.toml` already carries the value — hence this migration.
//!
//! **Precision.** Only the exact pair the effect wrote is reset:
//! `pty_pool_enabled = true` AND `worker_managed = true` AND the runtime
//! provider is Claude (explicitly, or absent ⇒ the `claude` default). An agent
//! with `pty_pool_enabled = true` alone was configured deliberately — by hand
//! or via the toggle — and is left strictly alone.
//!
//! **Safety.** Runs once (marker file), writes atomically (temp + rename), and
//! never blocks boot: every failure degrades to a `warn!`.

use std::path::{Path, PathBuf};

use tracing::{info, warn};

/// Marker filename under `<home>/migrations/`. Its presence — regardless of
/// contents — suppresses re-runs.
const MARKER_NAME: &str = "wp10-pty-default-reset.done";

/// Outcome of one migration pass (returned for tests + boot logging).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct MigrationReport {
    /// Agents whose runtime block was reset to the default.
    pub reset: Vec<String>,
    /// Agents inspected but deliberately left alone.
    pub skipped: usize,
    /// Agents that matched but could not be written (IO/parse failure).
    pub failed: Vec<String>,
    /// True when a marker already existed, so nothing was inspected.
    pub already_done: bool,
}

/// Does this `agent.toml` carry the exact signature the dashboard effect wrote?
///
/// Deliberately conservative — all three conditions must hold:
/// 1. `[runtime] pty_pool_enabled = true`
/// 2. `[runtime] worker_managed = true`
/// 3. `[runtime] provider` is absent (⇒ `claude`) or literally `claude`
///
/// `pty_pool_enabled = true` on its own reads as an intentional opt-in and
/// returns `false` here.
pub fn matches_auto_enabled_signature(table: &toml::Table) -> bool {
    let Some(rt) = table.get("runtime").and_then(|v| v.as_table()) else {
        return false;
    };
    let pty = rt
        .get("pty_pool_enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let worker = rt
        .get("worker_managed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !(pty && worker) {
        return false;
    }
    match rt.get("provider").and_then(|v| v.as_str()) {
        None => true, // absent ⇒ the `claude` default the effect assumed
        Some(p) => p.eq_ignore_ascii_case("claude"),
    }
}

/// Reset the two flags to the documented default. Returns true when the table
/// actually changed. Other `[runtime]` keys (provider, fallback, timeouts) are
/// preserved untouched.
///
/// The keys are written as explicit `false` rather than removed so the reset is
/// self-documenting in the file and cannot be re-materialised by any future
/// "key absent ⇒ enable" default.
pub fn reset_runtime_pty_defaults(table: &mut toml::Table) -> bool {
    let Some(rt) = table.get_mut("runtime").and_then(|v| v.as_table_mut()) else {
        return false;
    };
    let mut changed = false;
    for key in ["pty_pool_enabled", "worker_managed"] {
        if rt.get(key).and_then(|v| v.as_bool()) != Some(false) {
            rt.insert(key.to_string(), toml::Value::Boolean(false));
            changed = true;
        }
    }
    changed
}

fn marker_path(home_dir: &Path) -> PathBuf {
    home_dir.join("migrations").join(MARKER_NAME)
}

/// Rewrite one `agent.toml` atomically (temp + rename), mirroring
/// `channel_reply::update_agent_toml_with`'s durability contract. Standalone
/// because this runs at boot, before/independent of the agent registry, so
/// there is nothing to hot-reload.
fn rewrite_agent_toml(path: &Path, table: &toml::Table) -> Result<(), String> {
    let rendered =
        toml::to_string_pretty(table).map_err(|e| format!("serialise agent.toml: {e}"))?;
    let tmp = path.with_extension("toml.wp10tmp");
    std::fs::write(&tmp, rendered).map_err(|e| format!("write temp: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("commit rename: {e}")
    })
}

/// Run the migration if it has not run before.
///
/// Never returns an error: a wedged migration must not stop the gateway from
/// booting. Callers get a [`MigrationReport`] for logging/tests.
pub fn run(home_dir: &Path) -> MigrationReport {
    let mut report = MigrationReport::default();
    let marker = marker_path(home_dir);
    if marker.exists() {
        report.already_done = true;
        return report;
    }

    let agents_dir = home_dir.join("agents");
    let Ok(entries) = std::fs::read_dir(&agents_dir) else {
        // No agents directory yet (fresh install) — still drop the marker so a
        // later, deliberately-configured agent is never retroactively reset.
        write_marker(&marker, &report);
        return report;
    };

    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let toml_path = dir.join("agent.toml");
        let Ok(text) = std::fs::read_to_string(&toml_path) else {
            continue;
        };
        let Ok(mut table) = text.parse::<toml::Table>() else {
            continue;
        };
        let agent_id = dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("<unknown>")
            .to_string();

        if !matches_auto_enabled_signature(&table) {
            report.skipped += 1;
            continue;
        }
        if !reset_runtime_pty_defaults(&mut table) {
            report.skipped += 1;
            continue;
        }
        match rewrite_agent_toml(&toml_path, &table) {
            Ok(()) => {
                info!(
                    agent_id = %agent_id,
                    "WP10 migration: agent.toml had pty_pool_enabled + worker_managed set \
                     automatically by the dashboard (v1.44–v1.49) — reset to the default \
                     (PTY pool off). Re-enable it in the agent's runtime settings if it was wanted."
                );
                report.reset.push(agent_id);
            }
            Err(e) => {
                warn!(
                    agent_id = %agent_id,
                    error = %e,
                    "WP10 migration: could not reset agent.toml — leaving it as-is"
                );
                report.failed.push(agent_id);
            }
        }
    }

    if report.reset.is_empty() {
        info!(
            inspected = report.skipped,
            "WP10 migration: no agent carried the dashboard auto-enabled PTY signature"
        );
    } else {
        info!(
            reset = report.reset.len(),
            failed = report.failed.len(),
            agents = ?report.reset,
            "WP10 migration: reset dashboard-auto-enabled PTY pool settings to default"
        );
    }

    write_marker(&marker, &report);
    report
}

/// Best-effort marker write. The marker doubles as the audit record: it states
/// what was changed and why, in plain language, next to the timestamp.
///
/// A failure here only risks the migration running once more on the next boot,
/// which is harmless — the pass is idempotent (a reset agent no longer matches
/// the signature).
fn write_marker(marker: &Path, report: &MigrationReport) {
    if let Some(parent) = marker.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        warn!(error = %e, "WP10 migration: could not create migrations dir — marker not written");
        return;
    }
    let record = serde_json::json!({
        "migration": "wp10-pty-default-reset",
        "completed_at": chrono::Utc::now().to_rfc3339(),
        "reset_agents": report.reset,
        "failed_agents": report.failed,
        "inspected_unchanged": report.skipped,
        "reason": "DuDuClaw v1.44–v1.49 的儀表板 agent 編輯頁會在偵測到 Claude OAuth 時\
                   自動把 pty_pool_enabled 與 worker_managed 寫成 true（開啟頁面即觸發，\
                   使用者未同意）。該行為已移除，這些 agent 已回復預設（PTY pool 關閉）。\
                   刻意要用互動 REPL 的使用者可在 agent 的執行環境設定中重新開啟。",
    });
    if let Err(e) = std::fs::write(marker, format!("{record}\n")) {
        warn!(error = %e, "WP10 migration: could not write completion marker — may re-run next boot");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent_with(home: &Path, id: &str, runtime_toml: &str) -> PathBuf {
        let dir = home.join("agents").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agent.toml");
        std::fs::write(&path, format!("name = \"{id}\"\n{runtime_toml}")).unwrap();
        path
    }

    fn runtime_of(path: &Path) -> toml::Table {
        let text = std::fs::read_to_string(path).unwrap();
        let table: toml::Table = text.parse().unwrap();
        table
            .get("runtime")
            .and_then(|v| v.as_table())
            .cloned()
            .unwrap_or_default()
    }

    // ── signature matching ────────────────────────────────────────────

    #[test]
    fn full_signature_matches() {
        let t: toml::Table =
            "[runtime]\npty_pool_enabled = true\nworker_managed = true\n".parse().unwrap();
        assert!(matches_auto_enabled_signature(&t));
    }

    #[test]
    fn explicit_claude_provider_still_matches() {
        let t: toml::Table =
            "[runtime]\nprovider = \"claude\"\npty_pool_enabled = true\nworker_managed = true\n"
                .parse()
                .unwrap();
        assert!(matches_auto_enabled_signature(&t));
    }

    /// The effect only ever fired for Claude; a codex/gemini agent carrying
    /// both flags was configured by hand.
    #[test]
    fn non_claude_provider_does_not_match() {
        let t: toml::Table =
            "[runtime]\nprovider = \"codex\"\npty_pool_enabled = true\nworker_managed = true\n"
                .parse()
                .unwrap();
        assert!(!matches_auto_enabled_signature(&t));
    }

    /// The load-bearing carve-out: a lone `pty_pool_enabled` is a deliberate
    /// opt-in and must survive the migration untouched.
    #[test]
    fn pty_alone_is_deliberate_and_does_not_match() {
        let t: toml::Table = "[runtime]\npty_pool_enabled = true\n".parse().unwrap();
        assert!(!matches_auto_enabled_signature(&t));

        let t2: toml::Table =
            "[runtime]\npty_pool_enabled = true\nworker_managed = false\n".parse().unwrap();
        assert!(!matches_auto_enabled_signature(&t2));
    }

    #[test]
    fn absent_or_false_flags_do_not_match() {
        let t: toml::Table = "[runtime]\nprovider = \"claude\"\n".parse().unwrap();
        assert!(!matches_auto_enabled_signature(&t));
        let t2: toml::Table = "name = \"x\"\n".parse().unwrap();
        assert!(!matches_auto_enabled_signature(&t2));
    }

    // ── reset semantics ───────────────────────────────────────────────

    #[test]
    fn reset_preserves_other_runtime_keys() {
        let mut t: toml::Table = "[runtime]\nprovider = \"claude\"\nfallback = \"codex\"\n\
                                  pty_pool_enabled = true\nworker_managed = true\n"
            .parse()
            .unwrap();
        assert!(reset_runtime_pty_defaults(&mut t));
        let rt = t.get("runtime").unwrap().as_table().unwrap();
        assert_eq!(rt.get("pty_pool_enabled").unwrap().as_bool(), Some(false));
        assert_eq!(rt.get("worker_managed").unwrap().as_bool(), Some(false));
        assert_eq!(rt.get("provider").unwrap().as_str(), Some("claude"));
        assert_eq!(rt.get("fallback").unwrap().as_str(), Some("codex"));
    }

    #[test]
    fn reset_is_idempotent() {
        let mut t: toml::Table =
            "[runtime]\npty_pool_enabled = false\nworker_managed = false\n".parse().unwrap();
        assert!(!reset_runtime_pty_defaults(&mut t), "no change ⇒ no rewrite");
    }

    // ── end-to-end pass ───────────────────────────────────────────────

    #[test]
    fn run_resets_only_the_signature_match() {
        let home = tempfile::TempDir::new().unwrap();
        let victim = agent_with(
            home.path(),
            "auto-enabled",
            "[runtime]\npty_pool_enabled = true\nworker_managed = true\n",
        );
        let deliberate = agent_with(
            home.path(),
            "deliberate",
            "[runtime]\npty_pool_enabled = true\n",
        );
        let untouched = agent_with(home.path(), "plain", "[runtime]\nprovider = \"claude\"\n");

        let report = run(home.path());
        assert_eq!(report.reset, vec!["auto-enabled".to_string()]);
        assert!(report.failed.is_empty());

        assert_eq!(
            runtime_of(&victim).get("pty_pool_enabled").unwrap().as_bool(),
            Some(false)
        );
        assert_eq!(
            runtime_of(&deliberate).get("pty_pool_enabled").unwrap().as_bool(),
            Some(true),
            "a deliberate lone opt-in must NOT be reset"
        );
        assert!(runtime_of(&untouched).get("pty_pool_enabled").is_none());
    }

    #[test]
    fn marker_prevents_a_second_run() {
        let home = tempfile::TempDir::new().unwrap();
        let path = agent_with(
            home.path(),
            "a",
            "[runtime]\npty_pool_enabled = true\nworker_managed = true\n",
        );

        let first = run(home.path());
        assert_eq!(first.reset.len(), 1);
        assert!(marker_path(home.path()).exists());

        // A user deliberately re-enables it afterwards...
        std::fs::write(
            &path,
            "name = \"a\"\n[runtime]\npty_pool_enabled = true\nworker_managed = true\n",
        )
        .unwrap();

        // ...and the migration must never touch it again.
        let second = run(home.path());
        assert!(second.already_done);
        assert!(second.reset.is_empty());
        assert_eq!(
            runtime_of(&path).get("pty_pool_enabled").unwrap().as_bool(),
            Some(true),
            "re-enabling after the migration must stick"
        );
    }

    #[test]
    fn marker_is_written_even_when_nothing_matched() {
        let home = tempfile::TempDir::new().unwrap();
        agent_with(home.path(), "plain", "[runtime]\nprovider = \"claude\"\n");
        let report = run(home.path());
        assert!(report.reset.is_empty());
        assert!(
            marker_path(home.path()).exists(),
            "marker must exist so a later deliberate opt-in is never retro-reset"
        );
    }

    #[test]
    fn missing_agents_dir_does_not_panic() {
        let home = tempfile::TempDir::new().unwrap();
        let report = run(home.path());
        assert!(report.reset.is_empty());
        assert!(marker_path(home.path()).exists());
    }

    /// A read-only `agent.toml` must be reported as failed and must not panic
    /// or abort the boot sequence.
    #[cfg(unix)]
    #[test]
    fn write_failure_is_reported_not_panicked() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::TempDir::new().unwrap();
        agent_with(
            home.path(),
            "locked",
            "[runtime]\npty_pool_enabled = true\nworker_managed = true\n",
        );
        // Make the agent DIRECTORY read-only so the temp-file create fails
        // (the rename target itself is replaced, so file perms alone wouldn't).
        let dir = home.path().join("agents").join("locked");
        let mut perms = std::fs::metadata(&dir).unwrap().permissions();
        perms.set_mode(0o500);
        std::fs::set_permissions(&dir, perms).unwrap();

        let report = run(home.path());

        // Restore perms before assertions so TempDir cleanup works.
        let mut perms = std::fs::metadata(&dir).unwrap().permissions();
        perms.set_mode(0o700);
        std::fs::set_permissions(&dir, perms).unwrap();

        assert!(report.reset.is_empty());
        assert_eq!(report.failed, vec!["locked".to_string()]);
    }
}
