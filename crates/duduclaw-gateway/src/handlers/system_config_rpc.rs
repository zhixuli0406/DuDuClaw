//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    pub(crate) async fn handle_system_config(&self) -> WsFrame {
        let config_path = self.home_dir.join("config.toml");

        // Current voice settings from inference.toml [voice] so the
        // dashboard Voice tab can show saved values instead of defaults.
        let voice = {
            let inf_table = self
                .read_config_table(&self.home_dir.join("inference.toml"))
                .await;
            inf_table
                .get("voice")
                .and_then(|v| serde_json::to_value(v.clone()).ok())
                .unwrap_or(Value::Null)
        };

        // Structured [gateway] allowed_origins array so the dashboard can render
        // the remote-access allowlist as editable chips (the masked TOML string
        // is display-only). Absent / malformed => empty (loopback-only).
        let allowed_origins: Vec<String> = {
            let table = self.read_config_table(&config_path).await;
            table
                .get("gateway")
                .and_then(|g| g.as_table())
                .and_then(|g| g.get("allowed_origins"))
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|item| item.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        };

        // Structured [skills] gap_digest_enabled so the dashboard Settings
        // toggle shows the saved value (absent / malformed ⇒ false, matching
        // the fail-closed default in skill_gap_digest.rs).
        let gap_digest_enabled: bool = {
            let table = self.read_config_table(&config_path).await;
            table
                .get("skills")
                .and_then(|s| s.as_table())
                .and_then(|s| s.get("gap_digest_enabled"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        };

        // Structured [memory] novelty_gate so the dashboard Settings toggle
        // shows the saved value (absent / malformed ⇒ true, matching the
        // fail-closed default in `mcp.rs::novelty_gate_enabled_from_config`).
        let novelty_gate_enabled: bool = {
            let table = self.read_config_table(&config_path).await;
            table
                .get("memory")
                .and_then(|s| s.as_table())
                .and_then(|s| s.get("novelty_gate"))
                .and_then(|v| v.as_bool())
                .unwrap_or(true)
        };

        // S20: structured `[miniapp] enabled` so the Settings toggle shows the
        // saved value. Reuses `miniapp::enabled`'s own parser rather than
        // re-implementing the "absent / malformed ⇒ false" posture.
        let miniapp_enabled: bool = crate::miniapp::enabled(&self.home_dir);

        // Structured [notify] daily_digest so the dashboard Settings toggle +
        // time field show the saved values (W2-8). Reuses `DigestConfig`'s
        // own fail-open parser rather than re-implementing it — same
        // "absent/malformed ⇒ default (off, 09:00)" posture every other
        // structured field on this response follows.
        let digest_cfg = crate::notify_digest::DigestConfig::from_home(&self.home_dir);

        match tokio::fs::read_to_string(&config_path).await {
            Ok(content) => {
                // Mask sensitive fields
                match content.parse::<toml::Table>() {
                    Ok(mut table) => {
                        // v1.68: whether a WebChat widget key is stored (never the key).
                        let webchat_widget_key_set = table
                            .get("webchat")
                            .and_then(|w| w.get("widget_key"))
                            .and_then(|v| v.as_str())
                            .is_some_and(|k| !k.is_empty());
                        Self::mask_sensitive_fields(&mut table);
                        Self::mask_keyed_secret_tables(&mut table);
                        let masked =
                            toml::to_string_pretty(&table).unwrap_or_else(|_| content.clone());
                        WsFrame::ok_response(
                            "",
                            json!({
                                "config": masked,
                                "voice": voice,
                                "allowed_origins": allowed_origins,
                                "gap_digest_enabled": gap_digest_enabled,
                                "novelty_gate_enabled": novelty_gate_enabled,
                                "miniapp_enabled": miniapp_enabled,
                                "daily_digest_enabled": digest_cfg.enabled,
                                "daily_digest_at": digest_cfg.at.format("%H:%M").to_string(),
                                "webchat_widget_key_set": webchat_widget_key_set,
                            }),
                        )
                    }
                    Err(_) => {
                        // Do NOT return raw content — it may contain unmasked tokens (MCP-H5)
                        WsFrame::error_response(
                            "",
                            "Failed to parse config.toml — cannot safely display",
                        )
                    }
                }
            }
            Err(e) => WsFrame::error_response("", &format!("Failed to read config.toml: {e}")),
        }
    }

    /// `system.autostart.status` — report the login/boot registration state.
    pub(crate) async fn handle_system_autostart_status(&self) -> WsFrame {
        let s = tokio::task::spawn_blocking(duduclaw_core::autostart::status)
            .await
            .unwrap_or_else(|_| duduclaw_core::autostart::AutostartStatus {
                supported: false,
                enabled: false,
                method: "unsupported",
                detail: String::new(),
            });
        WsFrame::ok_response(
            "",
            json!({
                "supported": s.supported,
                "enabled": s.enabled,
                "method": s.method,
                "detail": s.detail,
            }),
        )
    }

    /// `system.autostart.set { enabled: bool }` — write/remove the user-level
    /// autostart registration (launchd plist / systemd user unit / HKCU Run
    /// key). Registration only: the running gateway process is never touched,
    /// so disabling from the dashboard cannot kill the gateway serving this
    /// very request. Takes effect at the next login/boot.
    pub(crate) async fn handle_system_autostart_set(&self, params: Value) -> WsFrame {
        let Some(enabled) = params.get("enabled").and_then(|v| v.as_bool()) else {
            return WsFrame::error_response("", "missing boolean field: enabled");
        };
        let result = tokio::task::spawn_blocking(move || {
            if enabled {
                duduclaw_core::autostart::enable()
            } else {
                duduclaw_core::autostart::disable()
            }
        })
        .await;
        match result {
            Ok(Ok(s)) => {
                info!(
                    enabled = s.enabled,
                    method = s.method,
                    "system.autostart.set applied"
                );
                WsFrame::ok_response(
                    "",
                    json!({
                        "supported": s.supported,
                        "enabled": s.enabled,
                        "method": s.method,
                        "detail": s.detail,
                    }),
                )
            }
            Ok(Err(e)) => WsFrame::error_response("", &format!("autostart change failed: {e}")),
            Err(e) => WsFrame::error_response("", &format!("autostart task failed: {e}")),
        }
    }

    pub(crate) async fn handle_system_version(&self) -> WsFrame {
        // `edition` mirrors the active license tier so the dashboard can
        // gate Pro-only UI (e.g. the auto-update toggle). "community" when
        // no license runtime is installed.
        let edition = match crate::license_runtime::global() {
            Some(runtime) => {
                let snapshot = runtime.snapshot().await;
                match snapshot.tier {
                    duduclaw_license::LicenseTier::OpenSource => "community".to_string(),
                    tier => tier.to_string(),
                }
            }
            None => "community".to_string(),
        };
        let edition_profile = self.resolve_edition_profile().await;
        WsFrame::ok_response(
            "",
            json!({
                "version": crate::updater::current_version(),
                "auto_update": crate::updater::auto_update_enabled(&self.home_dir),
                "edition": edition,
                // Product form-factor (personal|enterprise); see system.status.
                "edition_profile": edition_profile.as_str(),
            }),
        )
    }

    pub(crate) async fn handle_system_check_update(&self) -> WsFrame {
        // Detect a binary already swapped on disk under this running process
        // (npm/brew/manual update without a restart): the dashboard would
        // otherwise keep showing the stale in-memory version with no hint.
        let on_disk = crate::updater::on_disk_version().await;
        let restart_pending_version = on_disk.filter(|v| v != crate::updater::current_version());
        // An extension-supplied provider owns both halves of the update flow.
        // Without one this is byte-for-byte the previous behavior.
        let provider = self.extension.update_provider();
        let update_channel = crate::updater::update_channel_label(provider.is_some());
        let check_result = match &provider {
            Some(p) => p.check().await,
            None => crate::updater::check_update().await,
        };
        match check_result {
            Ok(info) => {
                // [M2] Cache the download/checksum URLs server-side
                // so apply_update does not accept URLs from the client.
                *self.pending_update.write().await = if info.available {
                    Some(PendingUpdate {
                        download_url: info.download_url.clone(),
                        checksum_url: info.checksum_url.clone(),
                        version: info.latest_version.clone(),
                        info: provider.as_ref().map(|_| info.clone()),
                        cached_at: Instant::now(),
                    })
                } else {
                    None
                };
                WsFrame::ok_response(
                    "",
                    json!({
                        "available": info.available,
                        "current_version": info.current_version,
                        "latest_version": info.latest_version,
                        "release_notes": info.release_notes,
                        "published_at": info.published_at,
                        "download_url": info.download_url,
                        "checksum_url": info.checksum_url,
                        "install_method": info.install_method,
                        "brew_formula": crate::updater::brew_formula_name(),
                        "auto_update": crate::updater::auto_update_enabled(&self.home_dir),
                        // Non-null when the on-disk binary is already a
                        // different version than this running process —
                        // "installed, restart to apply".
                        "restart_pending_version": restart_pending_version,
                        // Which channel can actually install this update:
                        // "control_plane" | "github" | "none". The update page
                        // hides its install button on "none" instead of offering
                        // a click that is guaranteed to be refused.
                        "update_channel": update_channel,
                        // Containerized deployments never swap the running
                        // binary in-process (image is immutable — see
                        // `install_verified_binary`'s refusal); the dashboard
                        // uses this to show an `update.sh`/image-rebuild
                        // guidance card instead of an install button.
                        "containerized": crate::updater::is_containerized(),
                    }),
                )
            }
            Err(e) => WsFrame::error_response("", &format!("Update check failed: {e}")),
        }
    }

    pub(crate) async fn handle_system_apply_update(&self, _params: Value) -> WsFrame {
        // An extension-supplied provider installs through its own channel and
        // its own pinned key. Without one this is byte-for-byte the previous
        // behavior — including the `duduclaw-pro` refusal inside
        // `apply_update_with_progress`, which stays as the last line of defence.
        let provider = self.extension.update_provider();

        // [M2] Use server-side cached URL — never accept URL from client.
        // On the provider path the cached descriptor stands in for the URL pair
        // (the provider resolves its asset at apply time).
        let pending = self.pending_update.read().await.clone();
        let pending = match pending {
            Some(p) if provider.is_some() && p.info.is_some() => p,
            Some(p) if provider.is_none() && !p.download_url.is_empty() => p,
            _ => {
                return WsFrame::error_response(
                    "",
                    "No pending update. Call system.check_update first.",
                );
            }
        };

        // [R2:NM1] TTL check — reject stale cached URLs
        if pending.is_expired() {
            *self.pending_update.write().await = None;
            return WsFrame::error_response(
                "",
                "Pending update expired. Please call system.check_update again.",
            );
        }

        // [M5] Audit log
        crate::security_autopilot::audit_and_emit(
            &self.home_dir,
            &duduclaw_security::audit::AuditEvent::new(
                "system_update",
                "system",
                duduclaw_security::audit::Severity::Info,
                json!({ "action": "apply", "target_version": pending.version }),
            ),
        );

        // Progress bridge — the download stage retries transient failures
        // (2026-08-04 field report: first install click failed red, the retry
        // succeeded). Without this the dashboard would sit on a silent spinner
        // for up to 20s of back-off with no idea anything was being retried.
        let progress_tx = self.event_tx.read().await.clone();
        let progress_version = pending.version.clone();
        let on_progress = move |p: crate::updater::UpdateProgress| {
            let Some(tx) = progress_tx.as_ref() else {
                return;
            };
            let frame = WsFrame::Event {
                event: "system.update_progress".to_string(),
                payload: json!({
                    "version": progress_version,
                    "phase": p.phase,
                    "attempt": p.attempt,
                    "max_attempts": p.max_attempts,
                }),
                seq: None,
                state_version: None,
            };
            let _ = tx.send(serde_json::to_string(&frame).unwrap_or_default());
        };

        let apply_result = match (&provider, pending.info.as_ref()) {
            (Some(p), Some(info)) => p.apply(info, &on_progress).await,
            _ => {
                crate::updater::apply_update_with_progress(
                    &pending.download_url,
                    &pending.checksum_url,
                    &on_progress,
                )
                .await
            }
        };
        match apply_result {
            Ok(result) => {
                *self.pending_update.write().await = None;

                if result.needs_restart {
                    // Broadcast to ALL dashboard tabs (not just the RPC caller)
                    // so every client can wait out the restart and reload.
                    if let Some(tx) = self.event_tx.read().await.clone() {
                        let frame = WsFrame::Event {
                            event: "system.update_installed".to_string(),
                            payload: json!({
                                "version": pending.version,
                                "needs_restart": true,
                                "message": result.message,
                            }),
                            seq: None,
                            state_version: None,
                        };
                        let _ = tx.send(serde_json::to_string(&frame).unwrap_or_default());
                    }
                    tokio::spawn(async {
                        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                        tracing::info!(
                            "Shutting down for update — will re-exec new binary after graceful shutdown"
                        );
                        duduclaw_core::platform::request_restart_after_shutdown();
                        duduclaw_core::platform::self_interrupt();
                    });
                }

                crate::security_autopilot::audit_and_emit(
                    &self.home_dir,
                    &duduclaw_security::audit::AuditEvent::new(
                        "system_update_success",
                        "system",
                        duduclaw_security::audit::Severity::Info,
                        json!({ "version": pending.version, "needs_restart": result.needs_restart }),
                    ),
                );

                WsFrame::ok_response(
                    "",
                    json!({
                        "success": result.success,
                        "message": result.message,
                        "needs_restart": result.needs_restart,
                    }),
                )
            }
            Err(e) => {
                // [R2:NM5] Clear stale pending on failure so user must re-check
                *self.pending_update.write().await = None;

                // [R2:NM3] Sanitize error for audit log (strip ANSI/newlines)
                let sanitized = e.replace('\n', " ").replace('\r', "").replace('\x1b', "");
                crate::security_autopilot::audit_and_emit(
                    &self.home_dir,
                    &duduclaw_security::audit::AuditEvent::new(
                        "system_update_failed",
                        "system",
                        duduclaw_security::audit::Severity::Warning,
                        json!({ "error": sanitized }),
                    ),
                );
                WsFrame::error_response("", &format!("Update failed: {e}"))
            }
        }
    }
}
