//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Live Run Forking handlers (RFC-26) ──────────────────
    //
    // Forks execute in the MCP-server process and persist to the cross-process
    // `ForkStore` (`<home>/fork_store.db`); the dashboard reads it here.

    pub(crate) fn open_fork_store(&self) -> Result<duduclaw_fork::ForkStore, WsFrame> {
        let path = self.home_dir.join("fork_store.db");
        if !path.exists() {
            return Err(WsFrame::error_response(
                "",
                "no forks yet (fork store not created)",
            ));
        }
        duduclaw_fork::ForkStore::open(&path)
            .map_err(|e| WsFrame::error_response("", &format!("open fork store: {e}")))
    }

    /// `notify.stats` — per-type notification action rate over the last
    /// `days` days (default 30, clamped 1–365 by the data layer).
    ///
    /// The SRE 50% rule (P4-5) is applied server-side: `broken: true` means
    /// "this notification type has enough actionable samples and fewer than
    /// half of them made anyone do anything". Types with nothing to press
    /// (plain FYI lines) report `actionable: 0` and are never flagged —
    /// see [`crate::notify_stats`] for why that would be a tautology.
    pub(crate) fn handle_notify_stats(&self, params: Value) -> WsFrame {
        let days = params.get("days").and_then(|v| v.as_i64()).unwrap_or(30);
        let rows = crate::notify_stats::stats(&self.home_dir, days);
        let types: Vec<Value> = rows
            .iter()
            .map(|s| {
                json!({
                    "type": s.notify_type,
                    "pushed": s.pushed,
                    "actionable": s.actionable,
                    "acted": s.acted,
                    // Two decimals is all a percentage bar needs, and it
                    // keeps the payload from carrying float noise.
                    "action_rate": (s.action_rate * 100.0).round() / 100.0,
                    "broken": s.broken,
                })
            })
            .collect();
        WsFrame::ok_response(
            "",
            json!({
                "days": days.clamp(1, 365),
                "broken_threshold": crate::notify_stats::BROKEN_RATE,
                "min_sample": crate::notify_stats::MIN_SAMPLE,
                "types": types,
            }),
        )
    }

    pub(crate) fn handle_fork_list(&self, params: Value) -> WsFrame {
        // No fork has ever been created yet → the store file doesn't exist.
        // That's an empty list, not an error: return [] so the dashboard shows
        // its "no forks yet" empty state instead of a scary error banner.
        if !self.home_dir.join("fork_store.db").exists() {
            return WsFrame::ok_response("", json!({ "forks": [] }));
        }
        let store = match self.open_fork_store() {
            Ok(s) => s,
            Err(f) => return f,
        };
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(50)
            .min(500) as usize;
        match store.list_forks(limit) {
            Ok(forks) => {
                let rows: Vec<Value> = forks
                    .iter()
                    .map(|f| {
                        json!({
                            "fork_id": f.fork_id,
                            "agent_id": f.agent_id,
                            "merge_mode": f.merge_mode,
                            "resolved": f.resolved,
                            "winner": f.winner,
                            "promoted": f.promoted,
                            "aggregate_spent_usd": f.aggregate_spent_usd,
                            "created_at": f.created_at,
                        })
                    })
                    .collect();
                WsFrame::ok_response("", json!({ "forks": rows }))
            }
            Err(e) => WsFrame::error_response("", &format!("list forks: {e}")),
        }
    }

    pub(crate) fn handle_fork_inspect(&self, params: Value) -> WsFrame {
        let store = match self.open_fork_store() {
            Ok(s) => s,
            Err(f) => return f,
        };
        let fork_id = match params.get("fork_id").and_then(|v| v.as_str()) {
            Some(f) => f,
            None => return WsFrame::error_response("", "fork_id is required"),
        };
        let fork = match store.get_fork(fork_id) {
            Ok(Some(f)) => f,
            Ok(None) => return WsFrame::error_response("", "fork not found"),
            Err(e) => return WsFrame::error_response("", &format!("get fork: {e}")),
        };
        let branches = store.list_branches(fork_id).unwrap_or_default();
        let branch_json: Vec<Value> = branches
            .iter()
            .map(|b| {
                json!({
                    "branch_id": b.branch_id,
                    "steering": b.steering,
                    "state": b.state,
                    "budget_usd": b.budget_usd,
                    "spent_usd": b.spent_usd,
                    "test_exit_code": b.test_exit_code,
                    "output": duduclaw_core::truncate_bytes(&b.output, 8000),
                })
            })
            .collect();
        WsFrame::ok_response(
            "",
            json!({
                "fork_id": fork.fork_id,
                "agent_id": fork.agent_id,
                "prompt": duduclaw_core::truncate_bytes(&fork.prompt, 4000),
                "merge_mode": fork.merge_mode,
                "resolved": fork.resolved,
                "winner": fork.winner,
                "promoted": fork.promoted,
                "branches": branch_json,
            }),
        )
    }

    pub(crate) fn handle_fork_resolve(&self, params: Value) -> WsFrame {
        // MCP selection uses the same key. Hold the lock across the fresh
        // resolved check, file promotion, durable resolution and retention GC.
        match duduclaw_core::with_file_lock(&self.home_dir.join("fork_resolution.lock"), || {
            Ok(self.handle_fork_resolve_locked(params))
        }) {
            Ok(response) => response,
            Err(e) => WsFrame::error_response("", &format!("fork resolution lock unavailable: {e}")),
        }
    }

    fn handle_fork_resolve_locked(&self, params: Value) -> WsFrame {
        let store = match self.open_fork_store() {
            Ok(s) => s,
            Err(f) => return f,
        };
        let fork_id = match params.get("fork_id").and_then(|v| v.as_str()) {
            Some(f) => f,
            None => return WsFrame::error_response("", "fork_id is required"),
        };
        let branch_id = match params.get("branch_id").and_then(|v| v.as_str()) {
            Some(b) => b,
            None => return WsFrame::error_response("", "branch_id is required"),
        };
        for id in [fork_id, branch_id] {
            if let Err(e) = duduclaw_fork::retention::validate_id(id) {
                return WsFrame::error_response("", &e.to_string());
            }
        }
        let fork = match store.get_fork(fork_id) {
            Ok(Some(f)) => f,
            Ok(None) => return WsFrame::error_response("", "fork not found"),
            Err(e) => return WsFrame::error_response("", &format!("get fork: {e}")),
        };
        if fork.resolved {
            return WsFrame::error_response("", "fork already resolved");
        }
        let branches = store.list_branches(fork_id).unwrap_or_default();
        if !branches.iter().any(|b| b.branch_id == branch_id) {
            return WsFrame::error_response("", "branch not found in fork");
        }
        let (workspace, parent) = match retained_fork_promotion_paths(
            &self.home_dir, &store, fork_id, branch_id,
        ) {
            Ok(paths) => paths,
            Err(e) => return WsFrame::error_response("", &e),
        };
        match duduclaw_fork::with_parent_publication(&parent, |publication| {
            let report = match publication.promote(
                &workspace,
                // Same rule as the agent-side `merge_or_select`: an operator
                // picking a branch never carries agent-structure files (or
                // `.claude/`) back into an agent directory.
                &duduclaw_fork::CopyPolicy::promote_for_parent(&parent, &self.home_dir),
            ) {
                Ok(report) => report,
                Err(e) => return Ok(WsFrame::error_response("", &format!("promotion failed, fork left unresolved: {e}"))),
            };
            let aggregate = branches.iter().map(|b| b.spent_usd).sum();
            Ok(match store.set_resolution(fork_id, Some(branch_id), true, true, aggregate) {
                Ok(_) => {
                    if let Err(e) = duduclaw_fork::retention::remove_fork(
                        &duduclaw_fork::retention::retained_root(&self.home_dir), fork_id,
                    ) {
                        tracing::warn!("fork {fork_id}: removing retained copies failed: {e}");
                    }
                    let _ = store.clear_fork_workspaces(fork_id);
                    WsFrame::ok_response("", json!({
                        "fork_id": fork_id, "resolved": true, "promoted": true,
                        "winner": branch_id, "files_copied": report.files_copied,
                    }))
                }
                Err(e) => WsFrame::error_response("", &format!("files were promoted but the store update failed: {e}")),
            })
        }) {
            Ok(response) => response,
            Err(error) => WsFrame::error_response("", &format!("fork parent publication lock unavailable: {error}")),
        }
    }

    // ── Migrate-from handlers ───────────────────────────────

    /// `migrate.scan` — dry-run migration plan. Spawns `current_exe
    /// migrate from <platform> --json [--source <abs>]`, 60s timeout, returns
    /// the parsed JSON verbatim. Fail-closed: any spawn/timeout/parse failure
    /// is an error frame carrying the reason — never fabricated data.
    pub(crate) async fn handle_migrate_scan(&self, params: Value) -> WsFrame {
        self.run_migrate_cli(params, false).await
    }

    /// `migrate.apply` — actually write the imported data. Same as scan plus
    /// `--apply` (and `--rename` when `rename=true`), with a 300s timeout.
    pub(crate) async fn handle_migrate_apply(&self, params: Value) -> WsFrame {
        self.run_migrate_cli(params, true).await
    }

    /// Shared driver for both migrate RPCs: validate params, build the argv,
    /// spawn this binary, enforce a timeout, and parse the single stdout JSON.
    pub(crate) async fn run_migrate_cli(&self, params: Value, apply: bool) -> WsFrame {
        let platform = match params.get("platform").and_then(|v| v.as_str()) {
            Some(p) if migrate_platform_allowed(p) => p.to_string(),
            Some(p) => {
                return WsFrame::error_response(
                    "",
                    &format!(
                        "unsupported platform '{p}' (expected openclaw/hermes/paperclip/claude-code)"
                    ),
                );
            }
            None => return WsFrame::error_response("", "platform is required"),
        };

        // Optional source: when given it MUST be an absolute path (a relative
        // path would resolve against the gateway's cwd, which is ambiguous).
        let source = params.get("source").and_then(|v| v.as_str());
        if let Err(e) = validate_migrate_source(source) {
            return WsFrame::error_response("", &e);
        }
        let rename = apply
            && params
                .get("rename")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                return WsFrame::error_response("", &format!("cannot locate self binary: {e}"));
            }
        };

        let mut cmd = tokio::process::Command::new(&exe);
        cmd.arg("migrate").arg("from").arg(&platform).arg("--json");
        if let Some(src) = source {
            cmd.arg("--source").arg(src);
        }
        if apply {
            cmd.arg("--apply");
        }
        if rename {
            cmd.arg("--rename");
        }
        // Pin DUDUCLAW_HOME to the gateway's home so a launchd-spawned gateway
        // (which may not inherit the interactive env) migrates into the right
        // tree rather than a default `~/.duduclaw`.
        cmd.env("DUDUCLAW_HOME", &self.home_dir);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let dur = if apply {
            std::time::Duration::from_secs(300)
        } else {
            std::time::Duration::from_secs(60)
        };
        let output = match tokio::time::timeout(dur, cmd.output()).await {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => {
                return WsFrame::error_response("", &format!("spawn migrate from failed: {e}"));
            }
            Err(_) => {
                return WsFrame::error_response(
                    "",
                    &format!("migrate from timed out after {}s", dur.as_secs()),
                );
            }
        };

        if !output.status.success() {
            // Non-zero exit = fatal (bad args, paperclip w/o source, ...).
            // Surface a trimmed stderr tail so the operator sees the cause.
            let stderr = String::from_utf8_lossy(&output.stderr);
            let tail = duduclaw_core::truncate_bytes(stderr.trim(), 500);
            let msg = if tail.is_empty() {
                format!("migrate from exited with status {}", output.status)
            } else {
                format!("migrate from failed: {tail}")
            };
            return WsFrame::error_response("", &msg);
        }

        // stdout must be exactly one JSON object.
        let stdout = String::from_utf8_lossy(&output.stdout);
        match serde_json::from_str::<Value>(stdout.trim()) {
            Ok(v) => WsFrame::ok_response("", v),
            Err(e) => WsFrame::error_response(
                "",
                &format!("could not parse migrate from JSON output: {e}"),
            ),
        }
    }
}

fn retained_fork_promotion_paths(
    home: &std::path::Path, store: &duduclaw_fork::ForkStore,
    fork_id: &str, branch_id: &str,
) -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    let retained = duduclaw_fork::retention::fork_dir(
        &duduclaw_fork::retention::retained_root(home), fork_id,
    ).map_err(|e| e.to_string())?;
    let workspace = store.branch_workspace(branch_id).map_err(|e| e.to_string())?
        .map(std::path::PathBuf::from).ok_or("no retained workspace; nothing can be promoted")?;
    if !workspace.is_dir() || !duduclaw_fork::retention::is_contained(&retained, &workspace) {
        return Err("retained workspace is missing, expired, or outside this fork".into());
    }
    let parent = store.parent_workspace(fork_id).map_err(|e| e.to_string())?
        .map(std::path::PathBuf::from).ok_or("no parent workspace recorded")?;
    if !parent.is_dir() { return Err("parent workspace no longer exists; nothing was promoted".into()); }
    Ok((workspace, parent))
}

#[cfg(test)]
mod fork_promotion_tests {
    use super::*;

    fn fixture(home: &std::path::Path) -> (duduclaw_fork::ForkStore, std::path::PathBuf) {
        let store = duduclaw_fork::ForkStore::open(home.join("fork_store.db")).unwrap();
        store.insert_fork(&duduclaw_fork::ForkRow {
            fork_id: "fork-test".into(), agent_id: "test".into(), prompt: "fixture".into(),
            merge_mode: "manual".into(), resolved: false, winner: None, promoted: false,
            aggregate_spent_usd: 0.0, created_at: chrono::Utc::now().to_rfc3339(),
        }, &[duduclaw_fork::BranchRow {
            branch_id: "branch-test".into(), fork_id: "fork-test".into(), steering: None,
            budget_usd: 0.1, state: "finished".into(), spent_usd: 0.01,
            output: "fixture".into(), test_exit_code: Some(0),
        }, duduclaw_fork::BranchRow {
            branch_id: "branch-second".into(), fork_id: "fork-test".into(), steering: None,
            budget_usd: 0.1, state: "finished".into(), spent_usd: 0.01,
            output: "fixture".into(), test_exit_code: Some(0),
        }]).unwrap();
        let parent = home.join("parent");
        std::fs::create_dir(&parent).unwrap();
        store.set_parent_workspace("fork-test", Some(&parent.to_string_lossy())).unwrap();
        (store, parent)
    }

    #[tokio::test]
    async fn dashboard_resolution_promotes_real_files_and_cleans_retained_copies() {
        let home = tempfile::tempdir().unwrap();
        let (store, parent) = fixture(home.path());
        let workspace = home.path().join("fork_ws/fork-test/branch-test");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("artifact.txt"), "winner").unwrap();
        std::fs::write(workspace.join(".env"), "secret fixture").unwrap();
        store.set_branch_workspace("branch-test", Some(&workspace.to_string_lossy())).unwrap();
        let handler = MethodHandler::new(home.path().to_path_buf()).await;
        let response = handler.handle_fork_resolve(json!({"fork_id":"fork-test","branch_id":"branch-test"}));
        assert!(matches!(response, WsFrame::Response { ok: true, .. }));
        assert_eq!(std::fs::read_to_string(parent.join("artifact.txt")).unwrap(), "winner");
        assert!(!parent.join(".env").exists());
        assert!(store.get_fork("fork-test").unwrap().unwrap().promoted);
        assert!(!workspace.exists());
    }

    #[tokio::test]
    async fn dashboard_resolution_refuses_missing_or_escaping_workspace() {
        for escaping in [false, true] {
            let home = tempfile::tempdir().unwrap();
            let (store, parent) = fixture(home.path());
            if escaping {
                let outside = home.path().join("outside");
                std::fs::create_dir(&outside).unwrap();
                store.set_branch_workspace("branch-test", Some(&outside.to_string_lossy())).unwrap();
            }
            let handler = MethodHandler::new(home.path().to_path_buf()).await;
            let response = handler.handle_fork_resolve(json!({"fork_id":"fork-test","branch_id":"branch-test"}));
            assert!(matches!(response, WsFrame::Response { ok: false, .. }));
            let fork = store.get_fork("fork-test").unwrap().unwrap();
            assert!(!fork.promoted && !fork.resolved);
            assert_eq!(std::fs::read_dir(parent).unwrap().count(), 0);
        }
    }

    #[tokio::test]
    async fn competing_dashboard_selections_copy_only_one_winner() {
        let home = tempfile::tempdir().unwrap();
        let (store, parent) = fixture(home.path());
        for branch in ["branch-test", "branch-second"] {
            let workspace = home.path().join("fork_ws/fork-test").join(branch);
            std::fs::create_dir_all(&workspace).unwrap();
            std::fs::write(workspace.join("winner.txt"), branch).unwrap();
            std::fs::write(workspace.join(branch), "unique to winner").unwrap();
            store.set_branch_workspace(branch, Some(&workspace.to_string_lossy())).unwrap();
        }
        let handler = std::sync::Arc::new(MethodHandler::new(home.path().to_path_buf()).await);
        let first = handler.clone();
        let second = handler.clone();
        let (first, second) = tokio::join!(
            tokio::task::spawn_blocking(move || first.handle_fork_resolve(json!({"fork_id":"fork-test","branch_id":"branch-test"}))),
            tokio::task::spawn_blocking(move || second.handle_fork_resolve(json!({"fork_id":"fork-test","branch_id":"branch-second"}))),
        );
        let succeeded = [first.unwrap(), second.unwrap()].into_iter()
            .filter(|response| matches!(response, WsFrame::Response { ok: true, .. })).count();
        assert_eq!(succeeded, 1);
        let winner = store.get_fork("fork-test").unwrap().unwrap().winner.unwrap();
        assert_eq!(std::fs::read_to_string(parent.join("winner.txt")).unwrap(), winner);
        assert!(parent.join(&winner).exists());
        let loser = if winner == "branch-test" { "branch-second" } else { "branch-test" };
        assert!(!parent.join(loser).exists());
    }
}
