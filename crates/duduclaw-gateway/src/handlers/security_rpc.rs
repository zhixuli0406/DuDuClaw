//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    /// Live security system status — replaces static placeholder panels.
    pub(crate) async fn handle_security_status(&self) -> WsFrame {
        let reg = self.registry.read().await;
        let agents = reg.list();

        // RBAC: derive from agent roles
        let rbac_entries: Vec<Value> = agents.iter().map(|a| {
            let cfg = &a.config;
            json!({
                "agent_id": cfg.agent.name,
                "role": cfg.agent.role,
                "tool_use": true,
                "web_access": cfg.capabilities.browser_via_bash,
                "file_write": true,
                "shell_exec": !cfg.capabilities.denied_tools.iter().any(|t| t == "Bash"),
                "delegate": cfg.capabilities.allowed_tools.iter().any(|t| t.contains("delegate") || t.contains("spawn")),
            })
        }).collect();

        // Rate limiter: read from config
        let config_path = self.home_dir.join("config").join("duduclaw.toml");
        let rate_limit = if config_path.exists() {
            let content = tokio::fs::read_to_string(&config_path)
                .await
                .unwrap_or_default();
            // Parse basic rate limit values from config
            let rpm = content
                .lines()
                .find(|l| l.contains("rate_limit_rpm"))
                .and_then(|l| l.split('=').nth(1))
                .and_then(|v| v.trim().parse::<u32>().ok())
                .unwrap_or(60);
            let concurrent = content
                .lines()
                .find(|l| l.contains("max_concurrent"))
                .and_then(|l| l.split('=').nth(1))
                .and_then(|v| v.trim().parse::<u32>().ok())
                .unwrap_or(5);
            json!({
                "requests_per_minute": rpm,
                "concurrent_requests": concurrent,
            })
        } else {
            json!({
                "requests_per_minute": 60,
                "concurrent_requests": 5,
            })
        };

        // SOUL.md drift detection status
        let soul_status: Vec<Value> = agents
            .iter()
            .map(|a| {
                let soul_path = self
                    .home_dir
                    .join("agents")
                    .join(&a.config.agent.name)
                    .join("SOUL.md");
                let exists = soul_path.exists();
                json!({
                    "agent_id": a.config.agent.name,
                    "soul_exists": exists,
                    "gvu_enabled": a.config.evolution.gvu_enabled,
                })
            })
            .collect();

        WsFrame::ok_response(
            "",
            json!({
                // G2 (2026-09 feature audit): `credential_proxy` and
                // `mount_guard` were removed. Their Rust modules
                // (`duduclaw-security::{credential_proxy, mount_guard}`) had
                // zero callers and were deleted, and the values this RPC
                // returned measured something else than their names claimed:
                // "injected_secrets" counted the *gateway process's own* env
                // vars matching API_KEY/TOKEN/SECRET — which the v1.61
                // spawn-env allowlist deliberately scrubs out of agent spawns
                // — and "rules" was the first agent's container mount list
                // rendered as if it were a global policy.
                "rbac": rbac_entries,
                "rate_limiter": rate_limit,
                "soul_drift": soul_status,
            }),
        )
    }

    /// `security.credential_hygiene` (WP-K) — read-only scan of `config.toml`
    /// for plaintext credentials, built on
    /// `security_posture::find_plaintext_secrets`. Never returns a value or a
    /// masked fragment of one — only TOML paths, twin status and severity
    /// (coding convention #4: this endpoint's entire reason to exist is to be
    /// safe to render on the dashboard).
    ///
    /// Fail-closed: an existing-but-unparsable `config.toml` is reported as an
    /// ERROR, never silently downgraded to "clean" — a corrupt file must not
    /// hide real plaintext secrets behind a false-green card.
    pub(crate) async fn handle_security_credential_hygiene(&self) -> WsFrame {
        let config_path = self.home_dir.join("config.toml");
        let table = match self.read_config_table_strict(&config_path).await {
            Ok(t) => t,
            Err(e) => return WsFrame::error_response("", &format!("憑證衛生偵測失敗:{e}")),
        };
        let findings = crate::security_posture::find_plaintext_secrets(&table);
        let findings_json: Vec<Value> = findings
            .iter()
            .map(|f| {
                json!({
                    "path": f.path,
                    "has_enc_twin": f.has_enc_twin,
                    "severity": f.severity,
                })
            })
            .collect();
        WsFrame::ok_response(
            "",
            json!({
                "clean": findings.is_empty(),
                "count": findings.len(),
                "findings": findings_json,
            }),
        )
    }

    /// `security.credential_inventory` (WP-H1 P1) — the structured credential
    /// list: every credential field in `config.toml` plus every per-agent
    /// channel token, each with the `describe()` verdict (configured / source /
    /// source_label / writable / residue).
    ///
    /// This is the answer to "which settings use `secret://`, which are still
    /// plaintext or `_enc`" without rendering the config file — the design's
    /// §2.3 replacement for the five masking dialects. `describe()` never
    /// resolves, so listing forty fields costs zero backend round-trips, and it
    /// never holds a value, so there is nothing here to mask in the first
    /// place.
    ///
    /// Fail-closed like its hygiene sibling: an unparsable `config.toml` is an
    /// ERROR, never an empty (falsely reassuring) list.
    pub(crate) async fn handle_security_credential_inventory(&self) -> WsFrame {
        let config_path = self.home_dir.join("config.toml");
        let table = match self.read_config_table_strict(&config_path).await {
            Ok(t) => t,
            Err(e) => return WsFrame::error_response("", &format!("憑證清單讀取失敗:{e}")),
        };
        let mut entries = crate::security_posture::credential_inventory(&table);

        // Per-agent channel tokens live in each `agent.toml`, not in
        // `config.toml`, and they are exactly the fields the 2026-08-13 OTP
        // incident moved a credential *into*. A list that stopped at
        // `config.toml` would show the global token as unset with no hint that
        // an agent now owns it.
        let agent_ids: Vec<String> = {
            let reg = self.registry.read().await;
            reg.list()
                .iter()
                .map(|a| a.config.agent.name.clone())
                .collect()
        };
        for agent_id in agent_ids {
            for channel in crate::channel_settings::VALID_CHANNEL_TYPES {
                let Some(status) = crate::config_crypto::agent_channel_token_ref(
                    &self.home_dir,
                    &agent_id,
                    channel,
                )
                .map(|r| r.describe()) else {
                    continue;
                };
                if !status.configured && !status.residue {
                    continue;
                }
                entries.push(crate::security_posture::CredentialEntry {
                    path: format!("agents.{agent_id}.channels.{channel}.bot_token"),
                    configured: status.configured,
                    source: status.source,
                    source_label: status.source_label,
                    writable: status.writable,
                    residue: status.residue,
                });
            }
        }

        let configured = entries.iter().filter(|e| e.configured).count();
        let referenced = entries
            .iter()
            .filter(|e| {
                !matches!(
                    e.source,
                    duduclaw_security::secret_ref::SourceKind::Unset
                        | duduclaw_security::secret_ref::SourceKind::Inline
                        | duduclaw_security::secret_ref::SourceKind::Legacy
                        | duduclaw_security::secret_ref::SourceKind::Ambiguous
                )
            })
            .count();
        WsFrame::ok_response(
            "",
            json!({
                "entries": entries,
                "total": entries.len(),
                "configured": configured,
                "referenced": referenced,
                "residue": entries.iter().filter(|e| e.residue).count(),
                "plaintext": entries
                    .iter()
                    .filter(|e| e.source == duduclaw_security::secret_ref::SourceKind::Legacy)
                    .count(),
            }),
        )
    }

    /// `security.credential_cleanup` (WP-K) — removes ONLY plaintext
    /// credential fields that already have a confirmed `_enc` twin (the
    /// residue pattern from the 2026-08-15 `[[accounts]] oauth_token`
    /// incident — see `commercial/docs/DESIGN-credentials-doctrine-2026-08.md`
    /// §1.5 / §3 P1). Fields with no twin are left completely untouched by
    /// this pass — encrypting an unfamiliar field in place risks writing a
    /// corrupt `config.toml`; those are surfaced by
    /// `security.credential_hygiene` for manual handling instead.
    ///
    /// Safety: backs up `config.toml` (timestamped filename, 0600) BEFORE
    /// mutating, writes back through a cross-process advisory lock + atomic
    /// temp/rename (coding convention #3 — config.toml is touched by many
    /// other RPC handlers concurrently, and this one deletes user secrets so
    /// it earns the strictest discipline in the file), and audits the
    /// removed paths only — never values. Idempotent: nothing to clean
    /// returns `cleaned: false` with an explanatory message rather than an
    /// error, and creates no backup (a true no-op should leave no trace).
    pub(crate) async fn handle_security_credential_cleanup(&self, ctx: &UserContext) -> WsFrame {
        let config_path = self.home_dir.join("config.toml");
        let table = match self.read_config_table_strict(&config_path).await {
            Ok(t) => t,
            Err(e) => return WsFrame::error_response("", &format!("憑證清理失敗:{e}")),
        };

        let cleanable = crate::security_posture::find_plaintext_secrets(&table)
            .iter()
            .filter(|f| f.has_enc_twin)
            .count();
        if cleanable == 0 {
            return WsFrame::ok_response(
                "",
                json!({
                    "cleaned": false,
                    "removed_paths": Vec::<String>::new(),
                    "message": "沒有可自動清理的明文憑證殘留。",
                }),
            );
        }

        // Backup BEFORE mutating — timestamped filename, best-effort 0600.
        let ts = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
        let backup_path = self.home_dir.join(format!("config.toml.bak.{ts}"));
        let raw = match tokio::fs::read(&config_path).await {
            Ok(b) => b,
            Err(e) => return WsFrame::error_response("", &format!("備份設定檔失敗:{e}")),
        };
        if let Err(e) = tokio::fs::write(&backup_path, &raw).await {
            return WsFrame::error_response("", &format!("備份設定檔失敗:{e}"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) =
                tokio::fs::set_permissions(&backup_path, std::fs::Permissions::from_mode(0o600))
                    .await
            {
                warn!("failed to tighten permissions on credential backup: {e}");
            }
        }

        let mut table = table;
        let removed_paths = crate::security_posture::strip_twin_residue(&mut table);

        // Atomic write under a cross-process advisory lock: config.toml is a
        // hot file touched by many other RPC handlers, and this write
        // deletes user secrets, so it does not reuse the ordinary
        // read/mutate/rename-only pattern used elsewhere in this file.
        let write_result = {
            let path_for_lock = config_path.clone();
            let table_for_write = table.clone();
            tokio::task::spawn_blocking(move || {
                duduclaw_core::with_file_lock(&path_for_lock, || {
                    let content = toml::to_string_pretty(&table_for_write).map_err(|e| {
                        std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
                    })?;
                    let tmp_path = path_for_lock.with_extension("toml.tmp");
                    std::fs::write(&tmp_path, content)?;
                    std::fs::rename(&tmp_path, &path_for_lock)?;
                    Ok::<(), std::io::Error>(())
                })
            })
            .await
        };
        match write_result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return WsFrame::error_response("", &format!("寫回設定檔失敗:{e}")),
            Err(e) => return WsFrame::error_response("", &format!("清理作業失敗:{e}")),
        }

        crate::security_autopilot::audit_and_emit(
            &self.home_dir,
            &duduclaw_security::audit::AuditEvent::new(
                "credential_hygiene_cleanup",
                "dashboard",
                duduclaw_security::audit::Severity::Warning,
                json!({
                    "actor_user_id": ctx.user_id,
                    "actor_email": ctx.email,
                    "removed_paths": removed_paths,
                    "backup_path": backup_path.display().to_string(),
                }),
            ),
        );

        WsFrame::ok_response(
            "",
            json!({
                "cleaned": true,
                "removed_paths": removed_paths,
                "backup_path": backup_path.display().to_string(),
            }),
        )
    }

    // ── Security Audit (secaudit dashboard) ───────────────────
    //
    // Reads/reviews reports written by `duduclaw secaudit --save` to
    // `<home>/secaudit/reports/<UTC ISO8601 basic>.json`
    // (DESIGN-code-security-audit-2026-08 §3.1). `secaudit_reports.rs`
    // owns all the file I/O (containment, size cap, atomic write); these
    // three handlers are thin RPC adapters — params in, `WsFrame` out.

    /// `secaudit.reports` — shallow-summary listing, newest first. A missing
    /// `secaudit/reports/` directory (nobody has run `--save` yet) is an
    /// empty list, not an error.
    pub(crate) async fn handle_secaudit_reports(&self) -> WsFrame {
        let home_dir = self.home_dir.clone();
        let rows = tokio::task::spawn_blocking(move || secaudit_reports::list_reports(&home_dir))
            .await
            .unwrap_or_default();
        WsFrame::ok_response("", json!({ "reports": rows }))
    }

    /// `secaudit.report` — full `AuditReport` JSON for one file.
    /// Params: `{ file: <basename> }` (no separators, no `..` — validated in
    /// `secaudit_reports::read_report`).
    pub(crate) async fn handle_secaudit_report(&self, params: Value) -> WsFrame {
        let Some(file) = params
            .get("file")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return WsFrame::error_response("", "file is required");
        };
        let home_dir = self.home_dir.clone();
        let file = file.to_string();
        let result =
            tokio::task::spawn_blocking(move || secaudit_reports::read_report(&home_dir, &file))
                .await;
        match result {
            Ok(Ok(report)) => WsFrame::ok_response("", json!({ "report": report })),
            Ok(Err(e)) => WsFrame::error_response("", &e),
            Err(e) => WsFrame::error_response("", &format!("secaudit.report: {e}")),
        }
    }

    /// `secaudit.finding_status` — operator confirm/suppress/refute action on
    /// one finding (design §3.1's "operator 確認 finding" entry point).
    /// Params: `{ file, finding_id, status }`, `status` ∈
    /// `confirmed|suppressed|refuted`. Read-modify-write, locked + atomic;
    /// every successful mutation is also written to the security audit log.
    pub(crate) async fn handle_secaudit_finding_status(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let Some(file) = params
            .get("file")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return WsFrame::error_response("", "file is required");
        };
        let Some(finding_id) = params
            .get("finding_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return WsFrame::error_response("", "finding_id is required");
        };
        let Some(status) = params
            .get("status")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return WsFrame::error_response("", "status is required");
        };
        let home_dir = self.home_dir.clone();
        let file = file.to_string();
        let finding_id = finding_id.to_string();
        let status = status.to_string();
        let result = tokio::task::spawn_blocking({
            let file = file.clone();
            let finding_id = finding_id.clone();
            let status = status.clone();
            move || secaudit_reports::set_finding_status(&home_dir, &file, &finding_id, &status)
        })
        .await;
        match result {
            Ok(Ok(finding)) => {
                crate::security_autopilot::audit_and_emit(
                    &self.home_dir,
                    &duduclaw_security::audit::AuditEvent::new(
                        "secaudit_finding_status_changed",
                        &finding_id,
                        duduclaw_security::audit::Severity::Info,
                        json!({
                            "actor": ctx.user_id,
                            "file": file,
                            "finding_id": finding_id,
                            "status": status,
                            "source": "dashboard",
                        }),
                    ),
                );
                WsFrame::ok_response(
                    "",
                    json!({ "success": true, "file": file, "finding": finding }),
                )
            }
            Ok(Err(e)) => WsFrame::error_response("", &e),
            Err(e) => WsFrame::error_response("", &format!("secaudit.finding_status: {e}")),
        }
    }
}
