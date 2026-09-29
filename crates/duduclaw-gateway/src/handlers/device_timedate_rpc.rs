//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── System-settings app: device.about / device.timedate* /
    // network.wired_* ────────────────────────────────────────────────
    // Dispatch (admin + `require_appliance!()`) is in `dispatch()`'s method
    // match above, alongside the rest of the `device.*`/`network.*` family.
    // Data gathering + pure parsing/validation live in `device_about.rs`
    // and `network/wired.rs`; these five handlers are thin glue, same
    // discipline as the WP-B/D4a handlers above.

    /// `device.about` — OS/kernel/hostname/gateway-version identity
    /// snapshot. Always succeeds (every field individually degrades to
    /// `null` off-Linux or when unreadable — see `device_about::
    /// collect_device_about`'s own doc); the `Err` arm below only guards
    /// JSON serialization, which cannot itself fail for this type.
    pub(crate) async fn handle_device_about(&self) -> WsFrame {
        os_op_frame(crate::os_ops::device_about(), "device about")
    }

    /// `device.timedate` — read-only timezone/clock/NTP snapshot. Always
    /// succeeds (see `device_about::collect_timedate`'s own doc: an
    /// unreachable `timedatectl` degrades to `available: false`, never a
    /// failed RPC).
    pub(crate) async fn handle_device_timedate(&self) -> WsFrame {
        let status = crate::os_ops::timedate().await;
        match serde_json::to_value(&status) {
            Ok(v) => WsFrame::ok_response("", v),
            Err(e) => {
                WsFrame::error_response("", &format!("timedate status serialize failed: {e}"))
            }
        }
    }

    /// `device.timedate_set` — `{timezone?, ntp?}`, at least one required.
    /// Each provided field is applied (and audited) independently via
    /// `duduclaw-sysd`'s `SetTimezone`/`SetNtp` verbs; a failure on either
    /// stops before attempting the next one and reports `apply_failed`
    /// (whatever already succeeded stays applied — and audited — even
    /// though the overall RPC reports failure).
    pub(crate) async fn handle_device_timedate_set(&self, params: Value) -> WsFrame {
        let timezone = params.get("timezone").and_then(Value::as_str);
        let ntp = params.get("ntp").and_then(Value::as_bool);
        if timezone.is_none() && ntp.is_none() {
            return timedate_set_error_frame(
                "invalid_timezone",
                "至少需要提供 timezone 或 ntp 其中一項。",
            );
        }
        // O16: shape validation + sysd selection + the actual verb call now
        // live once in `crate::os_ops::{set_timezone, set_ntp}` (shared with
        // the operator CLI's `duduclaw os system timezone-set`/`ntp-set`).
        // This surface keeps its own closed 3-code taxonomy, its own zh-TW
        // copy, and its own audit rows — all three differ from the CLI's on
        // purpose, so they stay at the front door.
        let mut applied_timezone: Option<String> = None;
        let mut applied_ntp: Option<bool> = None;

        if let Some(tz) = timezone {
            match crate::os_ops::set_timezone(tz).await {
                Ok(out) if out.success => {
                    self.audit_timedate_event(Some(tz), None, true, None);
                    applied_timezone = Some(tz.to_string());
                }
                Ok(out) => {
                    self.audit_timedate_event(Some(tz), None, false, Some("apply_failed"));
                    warn!(
                        stderr = %duduclaw_core::truncate_chars(&out.stderr, 200),
                        "device.timedate_set: set_timezone ran but reported failure"
                    );
                    return timedate_set_error_frame("apply_failed", "套用時區失敗，請稍後再試。");
                }
                Err(e) => {
                    return self.timedate_set_failure_frame(
                        Some(tz),
                        None,
                        &e,
                        "套用時區失敗，請稍後再試。",
                    );
                }
            }
        }

        if let Some(enabled) = ntp {
            match crate::os_ops::set_ntp(enabled).await {
                Ok(out) if out.success => {
                    self.audit_timedate_event(None, Some(enabled), true, None);
                    applied_ntp = Some(enabled);
                }
                Ok(out) => {
                    self.audit_timedate_event(None, Some(enabled), false, Some("apply_failed"));
                    warn!(
                        stderr = %duduclaw_core::truncate_chars(&out.stderr, 200),
                        "device.timedate_set: set_ntp ran but reported failure"
                    );
                    return timedate_set_error_frame(
                        "apply_failed",
                        "套用 NTP 設定失敗，請稍後再試。",
                    );
                }
                Err(e) => {
                    return self.timedate_set_failure_frame(
                        None,
                        Some(enabled),
                        &e,
                        "套用 NTP 設定失敗，請稍後再試。",
                    );
                }
            }
        }

        WsFrame::ok_response(
            "",
            json!({ "applied": true, "timezone": applied_timezone, "ntp": applied_ntp }),
        )
    }

    /// Map an [`crate::os_ops::OsOpError`] from `set_timezone`/`set_ntp` onto
    /// `device.timedate_set`'s own closed 3-code taxonomy and zh-TW copy.
    ///
    /// Pre-flight refusals (`invalid_timezone` / `backend_unavailable`) are
    /// deliberately NOT audited — nothing was attempted, which is exactly the
    /// behavior this handler already had. Only a real apply failure writes a
    /// row.
    pub(crate) fn timedate_set_failure_frame(
        &self,
        timezone: Option<&str>,
        ntp: Option<bool>,
        err: &crate::os_ops::OsOpError,
        apply_failed_message: &str,
    ) -> WsFrame {
        match err.code() {
            Some("invalid_timezone") => {
                timedate_set_error_frame("invalid_timezone", "timezone 格式不正確。")
            }
            Some("backend_unavailable") => timedate_set_error_frame(
                "backend_unavailable",
                "網路設定服務未啟動，請重新開機或聯絡支援。",
            ),
            _ => {
                self.audit_timedate_event(timezone, ntp, false, Some("apply_failed"));
                warn!(error = %err.message(), "device.timedate_set: sysd verb call failed");
                timedate_set_error_frame("apply_failed", apply_failed_message)
            }
        }
    }

    /// Append one audit row per `device.timedate_set` sub-change
    /// (timezone and/or ntp), success or failure — mirrors
    /// `audit_wifi_event`'s one-row-per-attempt shape. No secrets involved
    /// (timezone/ntp are not password-shaped), so unlike `audit_wifi_event`
    /// the actual values ARE included in the audit payload.
    pub(crate) fn audit_timedate_event(
        &self,
        timezone: Option<&str>,
        ntp: Option<bool>,
        ok: bool,
        code: Option<&str>,
    ) {
        crate::security_autopilot::audit_and_emit(
            &self.home_dir,
            &duduclaw_security::audit::AuditEvent::new(
                "timedate_set",
                "device",
                duduclaw_security::audit::Severity::Info,
                json!({ "timezone": timezone, "ntp": ntp, "ok": ok, "code": code }),
            ),
        );
    }

    /// `network.wired_status` — read-only. Always succeeds (every
    /// sub-source degrades honestly — see `network::wired::
    /// collect_wired_status`'s own doc); the `Err` arm below only guards
    /// JSON serialization.
    pub(crate) async fn handle_network_wired_status(&self) -> WsFrame {
        os_op_frame(crate::os_ops::wired_status(self.home_dir()), "wired status")
    }

    /// `network.wired_config` — `{interface?, mode, address?, gateway?,
    /// dns?}`. Validates gateway-side first (`network::wired::
    /// validate_wired_config_request` — defense in depth; `duduclaw-sysd`
    /// validates again independently), resolves the interface when omitted,
    /// calls the sysd verb, audits success AND failure, then — only on
    /// success — persists (or clears, for `mode: "dhcp"`) the desired
    /// config so [`crate::network::wired::reapply_wired_config_on_boot`]
    /// can restore it across a reboot (the sysd verb's effect lives on
    /// tmpfs, see that function's doc).
    pub(crate) async fn handle_network_wired_config(&self, params: Value) -> WsFrame {
        let mode = params.get("mode").and_then(Value::as_str).unwrap_or("");
        let address = params.get("address").and_then(Value::as_str);
        let gateway = params.get("gateway").and_then(Value::as_str);
        let dns: Vec<String> = params
            .get("dns")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();

        if let Err(code) =
            crate::network::wired::validate_wired_config_request(mode, address, gateway, &dns)
        {
            return network_wired_config_error_frame(code);
        }

        let interface = match params.get("interface").and_then(Value::as_str) {
            Some(s) if !s.trim().is_empty() => s.to_string(),
            _ => match crate::network::wired::detect_wired_interface() {
                Some(i) => i,
                None => {
                    return network_wired_config_error_frame(
                        crate::network::wired::WiredConfigErrorCode::NoInterface,
                    );
                }
            },
        };

        let Some(ops) = crate::device_ops::select_sysd_ops() else {
            return network_wired_config_error_frame(
                crate::network::wired::WiredConfigErrorCode::BackendUnavailable,
            );
        };

        let result = ops
            .network_wired_config(&interface, mode, address, gateway, &dns)
            .await;
        let ok = matches!(&result, Ok(out) if out.success);
        self.audit_wired_config_event(
            &interface,
            mode,
            address,
            gateway,
            &dns,
            ok,
            if ok { None } else { Some("apply_failed") },
        );

        match result {
            Ok(out) if out.success => {
                if mode == "dhcp" {
                    if let Err(e) = crate::network::wired::delete_wired_config(self.home_dir()) {
                        warn!(
                            error = %e,
                            "network.wired_config: failed to clear persisted desired config after switching to dhcp"
                        );
                    }
                } else {
                    let cfg = crate::network::wired::WiredConfig {
                        interface: interface.clone(),
                        mode: mode.to_string(),
                        address: address.map(str::to_string),
                        gateway: gateway.map(str::to_string),
                        dns: dns.clone(),
                        updated_at: Utc::now().to_rfc3339(),
                    };
                    if let Err(e) = crate::network::wired::save_wired_config(self.home_dir(), &cfg)
                    {
                        warn!(error = %e, "network.wired_config: failed to persist desired config");
                    }
                }
                WsFrame::ok_response(
                    "",
                    json!({ "applied": true, "interface": interface, "mode": mode }),
                )
            }
            Ok(out) => {
                warn!(
                    stderr = %duduclaw_core::truncate_chars(&out.stderr, 200),
                    "network.wired_config ran but reported failure"
                );
                network_wired_config_error_frame(
                    crate::network::wired::WiredConfigErrorCode::ApplyFailed,
                )
            }
            Err(e) => {
                warn!(error = %e, "network.wired_config call failed");
                network_wired_config_error_frame(
                    crate::network::wired::WiredConfigErrorCode::ApplyFailed,
                )
            }
        }
    }

    /// Append one audit row per `network.wired_config` attempt, success or
    /// failure — mirrors `audit_wifi_event`'s shape. Address/gateway/DNS
    /// are not secrets (unlike a Wi-Fi PSK), so they're included verbatim.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn audit_wired_config_event(
        &self,
        interface: &str,
        mode: &str,
        address: Option<&str>,
        gateway: Option<&str>,
        dns: &[String],
        ok: bool,
        code: Option<&str>,
    ) {
        crate::security_autopilot::audit_and_emit(
            &self.home_dir,
            &duduclaw_security::audit::AuditEvent::new(
                "network_wired_config",
                interface,
                duduclaw_security::audit::Severity::Info,
                json!({
                    "interface": interface,
                    "mode": mode,
                    "address": address,
                    "gateway": gateway,
                    "dns": dns,
                    "ok": ok,
                    "code": code,
                }),
            ),
        );
    }
}
