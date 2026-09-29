//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

// ── WP-B: appliance device management ("device.*") ─────────────────────
//
// Dispatch (admin + `require_appliance!()` + `require_confirm!()` gates) is
// in `dispatch()`'s method match above; data gathering lives in
// `device.rs`, shell-outs in `device_ops.rs`. These handlers are thin glue
// — the pure/mockable logic is unit-tested in those two modules instead of
// here (see their own `#[cfg(test)]` blocks), so this module's own test
// coverage focuses on the dispatch-level gates (admin / appliance / confirm)
// that only exist at this layer.
impl MethodHandler {
    pub(crate) async fn handle_device_status(&self) -> WsFrame {
        os_op_frame(crate::os_ops::device_status(self.home_dir()), "device status")
    }

    /// `device.network` — read path returns the current interface list;
    /// any of the network-write-shaped keys in `params`
    /// (`crate::device::is_network_write_request`) refuses with a
    /// structured `not_implemented` error. Setting a static IP is real
    /// future work, not a placeholder that silently no-ops.
    pub(crate) async fn handle_device_network(&self, params: Value) -> WsFrame {
        if crate::device::is_network_write_request(&params) {
            return WsFrame::Response {
                id: String::new(),
                ok: false,
                payload: None,
                error: Some(json!({
                    "code": "not_implemented",
                    "message": "設定靜態 IP 尚未支援，本版僅提供網路介面讀取。",
                })),
            };
        }
        os_op_frame(
            crate::os_ops::network_interfaces().map(|v| json!({ "interfaces": v })),
            "network interfaces",
        )
    }

    // ── D4a: network settings (Wi-Fi over iwd D-Bus) ──────────────────
    // See `crate::network` for the type/error-taxonomy design and
    // `crate::network::iwd` for the D-Bus call sequence. Dispatch gating
    // (admin + appliance) lives with the other `device.*`/`network.*`
    // macros in `dispatch` above; these four handlers only translate
    // params <-> the module's own async facade.

    /// `network.wifi_scan` — `rescan` defaults to `true` (a fresh scan)
    /// when omitted, matching design §5.2's example payload.
    pub(crate) async fn handle_network_wifi_scan(&self, params: Value) -> WsFrame {
        let rescan = params
            .get("rescan")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        os_op_frame(crate::os_ops::wifi_scan(rescan).await, "wifi scan")
    }

    /// `network.wifi_connect` — success AND failure are audited (design
    /// §3.2); the psk itself, and even whether one was supplied, never
    /// reaches the audit payload (password-shaped information).
    ///
    /// T1 (`DESIGN-agent-body-network-2026-08.md` §12): an optional `source`
    /// param (read BEFORE `params` is consumed by `WifiConnectRequest`'s
    /// deserialization, which silently ignores unknown fields) lets the
    /// operator-console password card (`WifiPasswordRequestCard.tsx`)
    /// distinguish itself from an ordinary Settings-page connect in the
    /// audit trail — see [`wifi_connect_audit_source`] for the closed
    /// allowlist that keeps a caller from forging an arbitrary audit label.
    pub(crate) async fn handle_network_wifi_connect(&self, params: Value) -> WsFrame {
        let source = Self::wifi_connect_audit_source(params.get("source"));
        let req: crate::network::WifiConnectRequest = match serde_json::from_value(params) {
            Ok(r) => r,
            Err(e) => return WsFrame::error_response("", &format!("invalid params: {e}")),
        };
        // O16: the connect effect + its one audit row live in
        // `crate::os_ops::wifi_connect`; this handler keeps only the
        // dashboard's own param shape, its audit-source allowlist, and the
        // SSID-naming error projection (`error_to_json_with_ssid`).
        match crate::os_ops::wifi_connect(
            self.home_dir(),
            &req.ssid,
            req.psk.as_deref(),
            crate::os_ops::WifiAudit::AutopilotBus { source },
        )
        .await
        {
            Ok(v) => WsFrame::ok_response("", v),
            Err(crate::os_ops::OsOpError::Wifi(err)) => WsFrame::Response {
                id: String::new(),
                ok: false,
                payload: None,
                error: Some(crate::network::error_to_json_with_ssid(&err, &req.ssid)),
            },
            Err(e) => os_op_error_frame(&e, "wifi connect"),
        }
    }

    /// `network.wifi_forget` — success AND failure are audited.
    pub(crate) async fn handle_network_wifi_forget(&self, params: Value) -> WsFrame {
        let ssid = params
            .get("ssid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        os_op_frame(
            crate::os_ops::wifi_forget(
                self.home_dir(),
                &ssid,
                crate::os_ops::WifiAudit::AutopilotBus {
                    source: "dashboard",
                },
            )
            .await,
            "wifi forget",
        )
    }

    /// `network.status` — the underlying facade always succeeds (every
    /// sub-source degrades to an honest "unavailable"/"unknown" value
    /// rather than failing the call — see `network::status`'s own doc);
    /// the `Err` arm below only guards symmetry with the other three
    /// handlers and JSON serialization, which cannot itself fail for this
    /// type.
    pub(crate) async fn handle_network_status(&self) -> WsFrame {
        os_op_frame(crate::os_ops::wifi_status().await, "network status")
    }

    /// Design T1 (`DESIGN-agent-body-network-2026-08.md` §12): distinguish
    /// an operator-console-guided password submission (the
    /// `wifi_password_request` card, §5.2 — the browser calls this RPC
    /// directly after an agent's `os_wifi_connect` failed with
    /// `wrong_password`) from an ordinary dashboard Settings-page connect,
    /// in the audit trail's `source` field. A closed two-value allowlist:
    /// any other value, wrong type, or missing field degrades to the
    /// pre-existing `"dashboard"` — never let a client-supplied string
    /// reach the audit log verbatim (that would let a caller forge an audit
    /// attribution label). `&'static str` return keeps this a pure
    /// classification with no allocation.
    pub(crate) fn wifi_connect_audit_source(raw: Option<&Value>) -> &'static str {
        match raw.and_then(Value::as_str) {
            Some("operator_console_prompted") => "operator_console_prompted",
            _ => "dashboard",
        }
    }

    // O16: the one-row-per-attempt Wi-Fi audit write (design §3.2 —
    // `{ssid, ok, code, source}` only, never the psk nor a "psk_supplied"
    // flag) moved into `crate::os_ops::WifiAudit`, which both this surface
    // (audit row + autopilot bus event) and the out-of-process
    // `os_wifi_connect` MCP tool (audit row only) now share. The closed
    // `source` allowlist stays here — it is this surface's param shape.

    pub(crate) async fn handle_device_update_status(&self) -> WsFrame {
        os_op_frame(crate::os_ops::update_status().await, "update status")
    }

    /// `device.update_check` — H3d §11.5 (item 1): read the REAL update
    /// source (`config.toml [os_update] source_url`), not local staging.
    ///
    /// `device.update_status` above only ever reflects
    /// `systemd-sysupdate list`'s view of the LOCAL staging directory,
    /// which is empty until an `update_apply` call has actually downloaded
    /// something — so it can never answer "is there something new
    /// upstream". This calls [`crate::os_update::check_update`], which
    /// fetches and signature-verifies the same two small manifest files
    /// [`crate::os_update::stage_update`] does, but stops there (no payload
    /// download, no slot resolution) and reports an honest
    /// `{available, current_version, latest_version}` — or a distinct error
    /// code per failure mode, never a fabricated "up to date" on a network
    /// or verification failure.
    pub(crate) async fn handle_device_update_check(&self) -> WsFrame {
        os_op_frame(
            crate::os_ops::device_update_check(self.home_dir()).await,
            "update check",
        )
    }

    /// `device.update_apply` — stage a verified release, then install it.
    ///
    /// The staging half (H3d) is not optional and not a convenience: this
    /// appliance's `systemd-sysupdate` source is `Type=regular-file`, for
    /// which sysupdate does no integrity or authenticity checking at all
    /// (`sysupdate.d(5)`, v257 — there is no switch to enable it). So the
    /// only thing standing between a payload and the boot chain is
    /// [`crate::os_update::stage_update`], which verifies a signed manifest
    /// against a pinned key before a single byte reaches the staging
    /// directory.
    ///
    /// That is also why an unconfigured source is a **refusal, not a
    /// fall-through**: running sysupdate against whatever happens to be
    /// lying in the staging directory is precisely the hole this package
    /// closes. `UpToDate` is likewise reported honestly instead of being
    /// dressed up as an install.
    pub(crate) async fn handle_device_update_apply(&self) -> WsFrame {
        os_op_frame(
            crate::os_ops::device_update_apply(self.home_dir()).await,
            "update apply",
        )
    }

    pub(crate) async fn handle_device_update_rollback(&self) -> WsFrame {
        os_op_frame(crate::os_ops::update_rollback().await, "update rollback")
    }

    /// `device.boot_assessment` — read-only view of systemd's automatic
    /// boot assessment (`good` / `bad` / `indeterminate` / `clean`), so the
    /// dashboard can tell an operator whether the running version is still
    /// on probation without triggering anything.
    pub(crate) async fn handle_device_boot_assessment(&self) -> WsFrame {
        os_op_frame(crate::os_ops::boot_assessment().await, "boot assessment")
    }

    /// `device.factory_reset` — `params.clear_network` (D4a-8, optional,
    /// default `false`) additionally clears saved Wi-Fi credentials; see
    /// `DeviceOps::factory_reset`'s doc comment for the default-keep
    /// rationale.
    pub(crate) async fn handle_device_factory_reset(&self, params: Value) -> WsFrame {
        let clear_network = params
            .get("clear_network")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        os_op_frame(
            crate::os_ops::factory_reset(self.home_dir(), clear_network).await,
            "factory reset",
        )
    }

    pub(crate) async fn handle_device_power(&self, params: Value) -> WsFrame {
        let raw = params.get("action").and_then(|v| v.as_str()).unwrap_or("");
        match crate::os_ops::PowerAction::parse(raw) {
            Ok(action) => os_op_frame(crate::os_ops::power(action).await, "power"),
            Err(e) => os_op_error_frame(&e, "power"),
        }
    }

    /// `device.power_local` — the appliance lock screen's power menu
    /// (restart / shut down), reachable WITHOUT logging in.
    ///
    /// Gate order (all fail-closed, all in one place):
    /// 1. `power_local::evaluate` — appliance mode, loopback peer, closed
    ///    two-value action. See that function and its module header.
    /// 2. Rate limit — conservative, per source.
    /// 3. Audit **then** act: a reboot kills this process, so the row is
    ///    written before the call, never after.
    /// 4. Execute through the SAME `DeviceOps::reboot`/`poweroff` verbs the
    ///    admin-only `device.power` uses — on the appliance image that means
    ///    the privilege-separated `duduclaw-sysd` daemon, with one hardcoded
    ///    argv per verb and no shell anywhere.
    ///
    /// Failures are returned as the standard structured error envelope — a
    /// device that did not reboot never reports that it did.
    pub(crate) async fn handle_device_power_local(
        &self,
        params: Value,
        conn: crate::power_local::RpcConnInfo,
    ) -> WsFrame {
        use crate::power_local;

        // Missing/non-string `action` is treated as the empty string, which
        // `parse_action` refuses — one refusal path, no separate "absent"
        // branch that could drift from the "wrong value" branch.
        let raw_action = params.get("action").and_then(Value::as_str).unwrap_or("");

        let action = match power_local::evaluate(
            duduclaw_core::is_appliance(),
            conn.peer_is_loopback(),
            raw_action,
        ) {
            Ok(a) => a,
            Err(denial) => return self.reject_power_local(raw_action, denial, &conn),
        };

        if let Err(denial) =
            power_local::check_rate_limit(power_local::power_limiter(), &conn).await
        {
            return self.reject_power_local(raw_action, denial, &conn);
        }

        power_local::audit_accepted(self.home_dir(), action, &conn);
        info!(
            action = action.as_str(),
            source = %conn.peer_label(),
            pre_auth = conn.pre_auth,
            "lock-screen power action accepted"
        );

        power_local_result_frame(
            power_local::run_power_action(&*crate::device_ops::select_device_ops(), action).await,
        )
    }

    /// One refusal path for `device.power_local`: audit (only the refusals
    /// that already cleared the appliance + loopback fences — see
    /// `PowerLocalDenial::is_auditable`), log, and answer with the standard
    /// structured error envelope carrying end-user zh-TW copy.
    pub(crate) fn reject_power_local(
        &self,
        raw_action: &str,
        denial: crate::power_local::PowerLocalDenial,
        conn: &crate::power_local::RpcConnInfo,
    ) -> WsFrame {
        crate::power_local::audit_denied(self.home_dir(), raw_action, denial, conn);
        warn!(
            reason = denial.as_label(),
            source = %conn.peer_label(),
            "lock-screen power action refused"
        );
        WsFrame::Response {
            id: String::new(),
            ok: false,
            payload: None,
            error: Some(json!({ "code": denial.code(), "message": denial.message() })),
        }
    }

    /// `device.backup_create` — archives the writable data partition (the
    /// parent of `home_dir`; on the appliance image `home_dir` is
    /// `/data/duduclaw` so its parent is `/data`, matching the "整個 /data
    /// 打包匯出" requirement) and stages it under the shared attachments
    /// directory so the EXISTING file-download mechanism
    /// (`GET /api/files/download?name=<filename>`, `files_api.rs`) serves
    /// it — no new download endpoint.
    ///
    /// The archive is built at a staging path OUTSIDE the source tree
    /// first (`std::env::temp_dir()`) and only moved into the attachments
    /// dir on success: tar-ing `/data` while writing the output file
    /// *into* `/data` would make tar try to include the archive it is
    /// still writing.
    pub(crate) async fn handle_device_backup_create(&self) -> WsFrame {
        os_op_frame(
            crate::os_ops::backup_create(self.home_dir()).await,
            "backup create",
        )
    }

    // ── WP-G1: scheduled backups + device-migration restore ─────────────
    //
    // Scheduling config lives in `crate::backup_schedule`, the actual timer
    // is spawned once at gateway startup (`server.rs::start_gateway`) —
    // these handlers are the dashboard's read/write/list/delete surface
    // over it. Restore (`backup_restore`) only ever STAGES an uploaded
    // archive; the destructive swap runs exactly once, at the next boot,
    // in `crate::backup_restore::perform_pending_restore_swap`.

    pub(crate) async fn handle_device_backup_schedule_get(&self) -> WsFrame {
        let cfg = crate::backup_schedule::BackupScheduleConfig::from_home(self.home_dir());
        WsFrame::ok_response(
            "",
            json!({
                "schedule_enabled": cfg.enabled,
                "interval_hours": cfg.interval_hours,
                "retention_count": cfg.retention_count,
            }),
        )
    }

    pub(crate) async fn handle_device_backup_schedule_set(&self, params: Value) -> WsFrame {
        // Validate before touching the config file — fail-closed, no
        // partial writes.
        if let Some(v) = params.get("interval_hours").and_then(Value::as_u64)
            && !(1..=8760).contains(&v)
        {
            return WsFrame::error_response("", "interval_hours 必須介於 1 到 8760 小時之間");
        }
        if let Some(v) = params.get("retention_count").and_then(Value::as_u64)
            && !(1..=1000).contains(&v)
        {
            return WsFrame::error_response("", "retention_count 必須介於 1 到 1000 之間");
        }
        let has_any = ["schedule_enabled", "interval_hours", "retention_count"]
            .iter()
            .any(|k| params.get(*k).is_some());
        if !has_any {
            return WsFrame::error_response(
                "",
                "No valid fields to update. Supported: schedule_enabled, interval_hours, retention_count",
            );
        }

        let config_path = self.home_dir().join("config.toml");
        let mut table = self.read_config_table(&config_path).await;
        let mut changes: Vec<String> = Vec::new();
        {
            let backup = table
                .entry("backup")
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                .as_table_mut()
                .unwrap();
            if let Some(v) = params.get("schedule_enabled").and_then(Value::as_bool) {
                backup.insert("schedule_enabled".into(), toml::Value::Boolean(v));
                changes.push(format!("backup.schedule_enabled = {v}"));
            }
            if let Some(v) = params.get("interval_hours").and_then(Value::as_u64) {
                backup.insert("interval_hours".into(), toml::Value::Integer(v as i64));
                changes.push(format!("backup.interval_hours = {v}"));
            }
            if let Some(v) = params.get("retention_count").and_then(Value::as_u64) {
                backup.insert("retention_count".into(), toml::Value::Integer(v as i64));
                changes.push(format!("backup.retention_count = {v}"));
            }
        }

        let tmp_path = config_path.with_extension("toml.tmp");
        if let Err(e) = self.write_config_table(&tmp_path, &table).await {
            return WsFrame::error_response("", &format!("Failed to write config: {e}"));
        }
        if let Err(e) = tokio::fs::rename(&tmp_path, &config_path).await {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return WsFrame::error_response("", &format!("Failed to commit config: {e}"));
        }

        let cfg = crate::backup_schedule::BackupScheduleConfig::from_home(self.home_dir());
        info!(?changes, "device.backup_schedule_set completed");
        WsFrame::ok_response(
            "",
            json!({
                "schedule_enabled": cfg.enabled,
                "interval_hours": cfg.interval_hours,
                "retention_count": cfg.retention_count,
            }),
        )
    }

    /// `device.backup_list` — lists `<home>/backups/` (never `attachments/`
    /// — see `backup_schedule.rs`'s module doc for why the two stay
    /// separate). Reuses `files_api::list_files`, the same listing helper
    /// the task/channel attachments panel uses.
    pub(crate) async fn handle_device_backup_list(&self) -> WsFrame {
        os_op_frame(
            crate::os_ops::backup_list(self.home_dir()).map(|v| json!({ "files": v })),
            "backup list",
        )
    }

    /// `device.backup_delete` — reuses `files_api::resolve_download`'s
    /// allowlist + canonicalize-containment check to locate the file before
    /// removing it (the same fail-closed discipline as a download, just
    /// followed by a delete instead of a stream).
    pub(crate) async fn handle_device_backup_delete(&self, params: Value) -> WsFrame {
        let Some(name) = params.get("name").and_then(|v| v.as_str()) else {
            return WsFrame::error_response("", "name parameter is required");
        };
        let dir = crate::backup_schedule::backups_dir(self.home_dir());
        match crate::files_api::resolve_download(&dir, name) {
            Ok(path) => match std::fs::remove_file(&path) {
                Ok(()) => WsFrame::ok_response("", json!({ "deleted": true })),
                Err(e) => WsFrame::error_response("", &format!("刪除備份檔失敗: {e}")),
            },
            Err(crate::files_api::ResolveError::BadRequest) => {
                WsFrame::error_response("", "無效的檔名")
            }
            Err(crate::files_api::ResolveError::Denied) => {
                WsFrame::error_response("", "存取被拒絕")
            }
            Err(crate::files_api::ResolveError::NotFound) => {
                WsFrame::error_response("", "備份檔不存在")
            }
        }
    }

    /// `device.backup_restore` — WP-G1 device migration ("汰機搬家").
    /// `path` must be a file this gateway itself staged via
    /// `POST /api/device/backup-upload` (`server.rs`) —
    /// `is_within_upload_dir` fail-closed rejects anything else, so a
    /// caller cannot point this at an arbitrary filesystem path. Extracts
    /// into the restore staging dir under the shared `RestoreLimits` gate,
    /// then writes the pending-restore marker. Nothing destructive happens
    /// in this call — the actual swap runs once, at the next gateway boot
    /// (`crate::backup_restore::perform_pending_restore_swap`, wired into
    /// `server.rs::start_gateway`).
    pub(crate) async fn handle_device_backup_restore(&self, params: Value) -> WsFrame {
        let Some(path) = params.get("path").and_then(|v| v.as_str()) else {
            return WsFrame::error_response("", "path parameter is required");
        };
        let src = std::path::Path::new(path);
        if !crate::backup_restore::is_within_upload_dir(self.home_dir(), src) {
            return WsFrame::error_response("", "還原來源不存在（請重新上傳）");
        }

        let home = self.home_dir().to_path_buf();
        let staging = crate::backup_restore::staging_dir(&home);
        let limits = crate::backup_restore::RestoreLimits::from_home(&home);

        let extract_result = {
            let src = src.to_path_buf();
            let staging = staging.clone();
            tokio::task::spawn_blocking(move || {
                crate::backup_restore::extract_tar_gz_safely(&src, &staging, &limits)
            })
            .await
        };
        let report = match extract_result {
            Ok(Ok(report)) => report,
            Ok(Err(violation)) => {
                return WsFrame::Response {
                    id: String::new(),
                    ok: false,
                    payload: None,
                    error: Some(
                        json!({ "code": "restore_rejected", "message": violation.to_string() }),
                    ),
                };
            }
            Err(e) => {
                return WsFrame::error_response("", &format!("還原解壓時發生內部錯誤: {e}"));
            }
        };

        let source_filename = src
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let marker = crate::backup_restore::RestoreMarker {
            staged_at: Utc::now(),
            source_filename,
        };
        if let Err(e) = crate::backup_restore::write_marker(&home, &marker) {
            let _ = std::fs::remove_dir_all(&staging);
            return WsFrame::error_response("", &format!("寫入還原標記失敗: {e}"));
        }

        // Best-effort: the uploaded file's content is now safely inside
        // `staging`; keeping the original around only wastes disk.
        let _ = std::fs::remove_file(src);

        WsFrame::ok_response(
            "",
            json!({
                "staged": true,
                "files_written": report.files_written,
                "restart_required": true,
            }),
        )
    }
}
