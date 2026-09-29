//! O16 — the single authority for the `os_*` device/system capability family.
//!
//! ## Why this module exists
//!
//! The 2026-09-29 feature audit (`wiki/reports/feature-audit-2026-09-29.md`
//! row O16) found the same set of OS capabilities implemented **three
//! times**: once as agent-facing MCP tools (`duduclaw-cli::mcp_os_ops`),
//! once as operator CLI leaf commands (`duduclaw-cli::os_drive`), and once
//! as dashboard WebSocket RPCs (`duduclaw-gateway::handlers`'s `device.*` /
//! `network.*` arms). Three copies of "what does `wifi connect` mean" is
//! three places to forget to fix a bug.
//!
//! This module owns **one** implementation per capability: parse-free,
//! wire-free, gate-free. Each `pub` function performs the effect and returns
//! the canonical JSON payload (or a typed [`OsOpError`]). The three front
//! doors keep exactly three responsibilities and nothing else:
//!
//! 1. **their own permission gate** — MCP's `Scope::OsNative` / `Scope::Admin`
//!    at the `mcp_dispatch` choke point plus `[capabilities] os_native`, the
//!    RPC's `require_admin!()` / `require_appliance!()` / `require_confirm!()`
//!    macros, the CLI's operator-terminal identity and
//!    `os_drive::approval::gate`. **Gates deliberately did NOT move here** —
//!    each front door's authorization model is different and moving them
//!    would mean re-deriving them, which is the exact failure mode this
//!    consolidation exists to prevent;
//! 2. **request parsing** from their own wire shape (JSON args / clap enum /
//!    RPC `params`);
//! 3. **response rendering** into their own wire shape (MCP tool-result
//!    envelope / `WsFrame` / stdout text).
//!
//! ## Byte-identical by construction
//!
//! Every payload built here is the payload the three surfaces already
//! produced, character for character. Where two surfaces genuinely differed
//! (the CLI's `序列化失敗：{e}` vs the RPC/MCP `"<what> serialize failed: {e}"`;
//! `os_check_update`'s verbose system half vs the CLI's brief one), the
//! divergence is expressed as an **explicit, named option at the call site**
//! ([`OsOpError::Serialize`] carrying only the serde detail,
//! [`CheckUpdateOptions`]) rather than a second implementation.
//!
//! ## What is deliberately NOT here
//!
//! - `os_notify` / `os_open` / `os_frontmost` / `os_spotlight_search` /
//!   `os_calendar_today` / `os_watch_status`: single front door (MCP only),
//!   and the `duduclaw-os` crate is already their one authority.
//! - `os.status` / `os.settings.update` / `os.gate.recent` /
//!   `os.events.recent`: dashboard-only, no second front door.
//! - `system.apply_update` (RPC) vs `os_apply_update(target="system")` (MCP):
//!   structurally different by design — the RPC trusts a URL pair cached in
//!   the live gateway's `MethodHandler::pending_update`, which a separate
//!   `duduclaw mcp-server` process cannot read, so the MCP tool re-resolves a
//!   fresh trusted pair instead. Two mechanisms, same invariant; not a
//!   duplicate to merge.
//! - `system.status` (RPC) vs `os_system_status` (MCP): the RPC reads the
//!   live in-memory registry/uptime; the MCP tool reports an honestly-reduced
//!   disk-derived subset. Different data, not a duplicate.

use std::path::Path;

use serde_json::{Value, json};

// ── Shared, front-door-visible copy ──────────────────────────────────────

/// Fail-closed off-appliance refusal text. All three front doors already
/// used this exact sentence; it lives here now so they cannot drift.
pub const NOT_APPLIANCE_MESSAGE: &str = "此功能僅限 DuDuClaw 裝置版（appliance image）使用。";

/// Missing-`confirm: true` refusal text for a destructive op.
pub const CONFIRM_REQUIRED_MESSAGE: &str =
    "這是不可逆的操作，請在請求參數帶上 confirm: true 再次確認執行。";

/// `device.power` / `os_power` invalid-action text.
pub const POWER_ACTION_MESSAGE: &str = "action 必須是 \"restart\" 或 \"shutdown\"";

/// Empty-SSID refusal text (`network.wifi_connect` / `os_wifi_connect`).
pub const SSID_REQUIRED_MESSAGE: &str = "ssid 不可為空";

/// `duduclaw-sysd` unreachable text (CLI `os system timezone-set`/`ntp-set`).
pub const SYSD_UNREACHABLE_MESSAGE: &str = "duduclaw-sysd 無法連線（appliance 檢查通過但 socket 不存在，或不是以能連上 sysd 的身分執行）。";

/// Timezone-shape rejection text used by the CLI front door.
pub const INVALID_TIMEZONE_SHAPE_MESSAGE: &str =
    "timezone 格式不正確（見 duduclaw_gateway::device_about::validate_timezone_shape）。";

// ── Unified error type ───────────────────────────────────────────────────

/// One error type for every `os_*` capability.
///
/// Variants deliberately carry the **original** domain error
/// ([`crate::device_ops::DeviceOpError`], [`crate::network::WifiError`])
/// rather than a pre-rendered string: the three front doors render the same
/// failure differently on purpose (MCP embeds the structured JSON as text,
/// the RPC returns it as a structured `error` object, the CLI prints a bare
/// message), and flattening to a string here would force one of them to
/// change shape.
#[derive(Debug, Clone)]
pub enum OsOpError {
    /// Off-appliance refusal — [`NOT_APPLIANCE_MESSAGE`], code `not_appliance`.
    NotAppliance,
    /// Destructive op without `confirm: true` — code `confirm_required`.
    ConfirmRequired,
    /// Caller-supplied parameters were invalid; carries the zh-TW text.
    InvalidParams(String),
    /// A backend failure that already has a stable machine code + zh-TW
    /// message pair (`os_update::StageError`, ESP/slot failures, …).
    Coded { code: String, message: String },
    /// A `device.*` op could not run.
    DeviceOp(crate::device_ops::DeviceOpError),
    /// A Wi-Fi op failed; the front door decides between
    /// [`crate::network::error_to_json`] and
    /// [`crate::network::error_to_json_with_ssid`].
    Wifi(crate::network::WifiError),
    /// Response serialization failed (unreachable for these concrete types —
    /// the `Result` is real, so it degrades honestly rather than unwrapping).
    /// Carries ONLY the serde detail: each front door prefixes it with its
    /// own already-shipped wording.
    Serialize(String),
    /// A plain, ready-to-display message with no machine code (bridge socket
    /// errors, archive-move failures, …).
    Message(String),
}

impl OsOpError {
    /// Stable machine code where one exists; `None` for message-only errors.
    pub fn code(&self) -> Option<&str> {
        match self {
            OsOpError::NotAppliance => Some(crate::handlers::DEVICE_NOT_APPLIANCE_ERROR_CODE),
            OsOpError::ConfirmRequired => Some("confirm_required"),
            OsOpError::Coded { code, .. } => Some(code),
            OsOpError::DeviceOp(crate::device_ops::DeviceOpError::Unsupported(_)) => {
                Some("unsupported")
            }
            OsOpError::DeviceOp(crate::device_ops::DeviceOpError::Io(_)) => Some("io_error"),
            OsOpError::Wifi(e) => Some(e.code.code()),
            OsOpError::InvalidParams(_) | OsOpError::Serialize(_) | OsOpError::Message(_) => None,
        }
    }

    /// Display text for a front door that renders a bare message.
    ///
    /// [`OsOpError::Serialize`] is NOT covered here — its wording differs per
    /// front door by design, so callers format it themselves.
    pub fn message(&self) -> String {
        match self {
            OsOpError::NotAppliance => NOT_APPLIANCE_MESSAGE.to_string(),
            OsOpError::ConfirmRequired => CONFIRM_REQUIRED_MESSAGE.to_string(),
            OsOpError::InvalidParams(m) | OsOpError::Message(m) => m.clone(),
            OsOpError::Coded { message, .. } => message.clone(),
            OsOpError::DeviceOp(e) => e.to_string(),
            OsOpError::Wifi(e) => crate::network::error_to_json(e).to_string(),
            OsOpError::Serialize(detail) => detail.clone(),
        }
    }

    /// The serde detail when this is a serialization failure, so a front door
    /// can wrap it in its own already-shipped prefix.
    pub fn serialize_detail(&self) -> Option<&str> {
        match self {
            OsOpError::Serialize(detail) => Some(detail),
            _ => None,
        }
    }
}

fn to_payload(value: &impl serde::Serialize) -> Result<Value, OsOpError> {
    serde_json::to_value(value).map_err(|e| OsOpError::Serialize(e.to_string()))
}

/// `{success, stdout, stderr}` — the one shape every `device.*` op result
/// takes on all three front doors.
fn device_op_payload(result: crate::device_ops::OpResult) -> Result<Value, OsOpError> {
    match result {
        Ok(out) => Ok(json!({
            "success": out.success,
            "stdout": out.stdout,
            "stderr": out.stderr,
        })),
        Err(e) => Err(OsOpError::DeviceOp(e)),
    }
}

// ── Typed requests ───────────────────────────────────────────────────────

/// Closed two-value power action (`device.power` / `os_power`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerAction {
    Restart,
    Shutdown,
}

impl PowerAction {
    /// Fail-closed parse — anything else is [`OsOpError::InvalidParams`] with
    /// the already-shipped [`POWER_ACTION_MESSAGE`].
    pub fn parse(raw: &str) -> Result<Self, OsOpError> {
        match raw {
            "restart" => Ok(PowerAction::Restart),
            "shutdown" => Ok(PowerAction::Shutdown),
            _ => Err(OsOpError::InvalidParams(POWER_ACTION_MESSAGE.to_string())),
        }
    }
}

/// Which audit sink a Wi-Fi write should use.
///
/// The dashboard RPC routes through [`crate::security_autopilot::audit_and_emit`]
/// (audit row **plus** an autopilot-bus event); the out-of-process MCP tool
/// writes the audit row only. `source` is a closed caller-vetted label —
/// never a client-supplied string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WifiAudit {
    /// Audit row only (`duduclaw_security::audit::append_audit_event`).
    Plain { source: &'static str },
    /// Audit row + autopilot bus event.
    AutopilotBus { source: &'static str },
}

impl WifiAudit {
    fn record(
        self,
        home_dir: &Path,
        event_type: &str,
        ssid: &str,
        result: &Result<(), crate::network::WifiError>,
    ) {
        let (ok, code) = match result {
            Ok(()) => (true, None),
            Err(e) => (false, Some(e.code.code())),
        };
        let source = match self {
            WifiAudit::Plain { source } | WifiAudit::AutopilotBus { source } => source,
        };
        let event = duduclaw_security::audit::AuditEvent::new(
            event_type,
            ssid,
            duduclaw_security::audit::Severity::Info,
            json!({ "ssid": ssid, "ok": ok, "code": code, "source": source }),
        );
        match self {
            WifiAudit::Plain { .. } => {
                duduclaw_security::audit::append_audit_event(home_dir, &event)
            }
            WifiAudit::AutopilotBus { .. } => {
                crate::security_autopilot::audit_and_emit(home_dir, &event)
            }
        }
    }
}

// ── Reads ────────────────────────────────────────────────────────────────

/// `device.status` / `os_device_status`.
pub fn device_status(home_dir: &Path) -> Result<Value, OsOpError> {
    to_payload(&crate::device::collect_status(home_dir))
}

/// The bare interface array behind `device.network` / `os_network_info` /
/// `duduclaw os network status`. The two JSON front doors wrap it in
/// `{"interfaces": …}`; the CLI prints the array itself — that wrap is
/// rendering, so it stays at the call site.
pub fn network_interfaces() -> Result<Value, OsOpError> {
    to_payload(&crate::device::collect_network())
}

/// `network.status` / `os_wifi_status` / `duduclaw os network wifi-status`.
pub async fn wifi_status() -> Result<Value, OsOpError> {
    match crate::network::status().await {
        Ok(status) => to_payload(&status),
        Err(e) => Err(OsOpError::Wifi(e)),
    }
}

/// `network.wifi_scan` / `os_wifi_scan`. `rescan` defaults to `true` at both
/// front doors (kept there — it is wire-level parsing).
pub async fn wifi_scan(rescan: bool) -> Result<Value, OsOpError> {
    match crate::network::wifi_scan(rescan).await {
        Ok(result) => Ok(crate::network::scan_result_to_json(&result)),
        Err(e) => Err(OsOpError::Wifi(e)),
    }
}

/// `network.wired_status` / `duduclaw os network wired-status`.
pub fn wired_status(home_dir: &Path) -> Result<Value, OsOpError> {
    to_payload(&crate::network::wired::collect_wired_status(home_dir))
}

/// `device.about` / `duduclaw os system about`.
pub fn device_about() -> Result<Value, OsOpError> {
    to_payload(&crate::device_about::collect_device_about(
        crate::updater::current_version(),
    ))
}

/// `device.timedate` — the full snapshot. The CLI's `timezone-get` /
/// `ntp-get` project two halves out of this one read; the RPC returns it
/// whole.
pub async fn timedate() -> crate::device_about::TimedateStatus {
    crate::device_about::collect_timedate().await
}

/// `device.backup_list` / `os_backup_list` — the bare file array (both JSON
/// front doors wrap it in `{"files": …}`).
pub fn backup_list(home_dir: &Path) -> Result<Value, OsOpError> {
    let dir = crate::backup_schedule::backups_dir(home_dir);
    to_payload(&crate::files_api::list_files(&dir))
}

/// `device.update_status` (local `systemd-sysupdate list` view).
pub async fn update_status() -> Result<Value, OsOpError> {
    device_op_payload(crate::device_ops::select_device_ops().update_status().await)
}

/// `device.update_check` — the real upstream freshness read (signed manifest
/// only, no download).
pub async fn device_update_check(home_dir: &Path) -> Result<Value, OsOpError> {
    match crate::os_update::check_update(home_dir).await {
        Ok(report) => Ok(json!({
            "available": report.available,
            "current_version": report.current_version,
            "latest_version": report.latest_version,
        })),
        Err(e) => Err(OsOpError::Coded {
            code: e.code().to_string(),
            message: e.user_message(),
        }),
    }
}

/// `device.boot_assessment` / `os_boot_assessment`.
pub async fn boot_assessment() -> Result<Value, OsOpError> {
    device_op_payload(
        crate::device_ops::select_device_ops()
            .boot_assessment_status()
            .await,
    )
}

// ── Writes ───────────────────────────────────────────────────────────────

/// `device.power` / `os_power`.
pub async fn power(action: PowerAction) -> Result<Value, OsOpError> {
    let ops = crate::device_ops::select_device_ops();
    let result = match action {
        PowerAction::Restart => ops.reboot().await,
        PowerAction::Shutdown => ops.poweroff().await,
    };
    device_op_payload(result)
}

/// `device.factory_reset` / `os_factory_reset`.
pub async fn factory_reset(home_dir: &Path, clear_network: bool) -> Result<Value, OsOpError> {
    device_op_payload(
        crate::device_ops::select_device_ops()
            .factory_reset(home_dir, clear_network)
            .await,
    )
}

/// `device.update_rollback` / `os_update_rollback`.
pub async fn update_rollback() -> Result<Value, OsOpError> {
    device_op_payload(
        crate::device_ops::select_device_ops()
            .update_rollback()
            .await,
    )
}

/// `device.backup_create` / `os_backup_create`.
pub async fn backup_create(home_dir: &Path) -> Result<Value, OsOpError> {
    match crate::handlers::create_device_backup_archive(home_dir).await {
        crate::handlers::DeviceBackupOutcome::Created {
            filename,
            stdout,
            stderr,
        } => Ok(json!({ "filename": filename, "stdout": stdout, "stderr": stderr })),
        crate::handlers::DeviceBackupOutcome::OpFailed(out) => device_op_payload(Ok(out)),
        crate::handlers::DeviceBackupOutcome::OpError(e) => Err(OsOpError::DeviceOp(e)),
        crate::handlers::DeviceBackupOutcome::MoveFailed(msg) => Err(OsOpError::Message(msg)),
    }
}

/// `device.update_apply` / `os_apply_update(target="device")` — stage a
/// signature-verified release, then install it.
pub async fn device_update_apply(home_dir: &Path) -> Result<Value, OsOpError> {
    match crate::handlers::stage_and_apply_device_update(home_dir).await {
        crate::handlers::DeviceUpdateApplyOutcome::StageFailed(e) => Err(OsOpError::Coded {
            code: e.code().to_string(),
            message: e.user_message(),
        }),
        crate::handlers::DeviceUpdateApplyOutcome::EspPrepareFailed(message) => {
            Err(OsOpError::Coded {
                code: "esp_prepare_failed".to_string(),
                message,
            })
        }
        crate::handlers::DeviceUpdateApplyOutcome::SlotMismatch(message) => Err(OsOpError::Coded {
            code: "slot_mismatch".to_string(),
            message,
        }),
        crate::handlers::DeviceUpdateApplyOutcome::Applied(applied) => device_op_payload(applied),
    }
}

/// `network.wifi_connect` / `os_wifi_connect`.
///
/// `psk` is `None` on the MCP path **structurally** — see that tool's doc for
/// why a passphrase must never reach an agent's context. Success AND failure
/// are audited here (one row per attempt), the psk never is.
pub async fn wifi_connect(
    home_dir: &Path,
    ssid: &str,
    psk: Option<&str>,
    audit: WifiAudit,
) -> Result<Value, OsOpError> {
    if ssid.trim().is_empty() {
        return Err(OsOpError::InvalidParams(SSID_REQUIRED_MESSAGE.to_string()));
    }
    let result = crate::network::wifi_connect(ssid, psk).await;
    audit.record(home_dir, "wifi_connect", ssid, &result);
    match result {
        Ok(()) => Ok(json!({ "state": "connected", "ssid": ssid })),
        Err(e) => Err(OsOpError::Wifi(e)),
    }
}

/// `network.wifi_forget`.
pub async fn wifi_forget(
    home_dir: &Path,
    ssid: &str,
    audit: WifiAudit,
) -> Result<Value, OsOpError> {
    if ssid.trim().is_empty() {
        return Err(OsOpError::InvalidParams(SSID_REQUIRED_MESSAGE.to_string()));
    }
    let result = crate::network::wifi_forget(ssid).await;
    audit.record(home_dir, "wifi_forget", ssid, &result);
    match result {
        Ok(()) => Ok(json!({ "forgotten": true, "ssid": ssid })),
        Err(e) => Err(OsOpError::Wifi(e)),
    }
}

/// Timezone write through `duduclaw-sysd`. Shape validation happens here
/// (one copy); the **audit row** stays at the front doors, whose event names
/// and payloads are deliberately different (`timedate_set` from the
/// dashboard, `os_drive_system` from the operator CLI) so the trail can tell
/// them apart.
pub async fn set_timezone(timezone: &str) -> Result<crate::device_ops::OpOutput, OsOpError> {
    if !crate::device_about::validate_timezone_shape(timezone) {
        return Err(OsOpError::Coded {
            code: "invalid_timezone".to_string(),
            message: INVALID_TIMEZONE_SHAPE_MESSAGE.to_string(),
        });
    }
    let Some(ops) = crate::device_ops::select_sysd_ops() else {
        return Err(OsOpError::Coded {
            code: "backend_unavailable".to_string(),
            message: SYSD_UNREACHABLE_MESSAGE.to_string(),
        });
    };
    ops.set_timezone(timezone)
        .await
        .map_err(OsOpError::DeviceOp)
}

/// NTP enable/disable through `duduclaw-sysd`. Same audit split as
/// [`set_timezone`].
pub async fn set_ntp(enabled: bool) -> Result<crate::device_ops::OpOutput, OsOpError> {
    let Some(ops) = crate::device_ops::select_sysd_ops() else {
        return Err(OsOpError::Coded {
            code: "backend_unavailable".to_string(),
            message: SYSD_UNREACHABLE_MESSAGE.to_string(),
        });
    };
    ops.set_ntp(enabled).await.map_err(OsOpError::DeviceOp)
}

// ── Compositor / audio bridges ───────────────────────────────────────────

/// `os_display_get` / `duduclaw os display …` (read side).
pub async fn display_get() -> Result<Value, OsOpError> {
    crate::display_bridge::display_get()
        .await
        .map_err(OsOpError::Message)
}

/// `os_display_set` / `duduclaw os display …-set`.
pub async fn display_set(field: &str, value: &str) -> Result<Value, OsOpError> {
    crate::display_bridge::display_set(field, value)
        .await
        .map_err(OsOpError::Message)
}

/// `os_audio_get` / `duduclaw os audio get`.
pub async fn audio_get() -> Result<Value, OsOpError> {
    crate::audio_bridge::audio_get()
        .await
        .map_err(OsOpError::Message)
}

/// `os_audio_set` / `duduclaw os audio volume-set|mute-toggle|output-set`.
pub async fn audio_set(field: &str, value: &str) -> Result<Value, OsOpError> {
    crate::audio_bridge::audio_set(field, value)
        .await
        .map_err(OsOpError::Message)
}

// ── Composite: combined update check ─────────────────────────────────────

/// Which projection of the `system` half [`check_update`] should build.
///
/// The MCP tool and the operator CLI shipped two different (already-public)
/// field sets. Rather than keep two bodies, the difference is this one named
/// option — the only thing that ever differed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckUpdateOptions {
    /// Include `release_notes` / `published_at` / `containerized` in the
    /// `system` half (the MCP tool's shape). The CLI omits them.
    pub include_release_metadata: bool,
    /// Text used for BOTH `device` and `device_check` when this install is
    /// not an appliance. The two front doors' wording differs by one clause.
    pub not_appliance_note: &'static str,
}

/// The `{system, device, device_check}` combined read behind
/// `os_check_update` and `duduclaw os system update-check`.
///
/// `system` is always present (duduclaw self-update works on every install);
/// `device` / `device_check` are the appliance OS-image halves and degrade to
/// an honest note off-appliance instead of being hard-blocked.
pub async fn check_update(home_dir: &Path, opts: CheckUpdateOptions) -> Value {
    let system = match crate::updater::check_update().await {
        Ok(info) => {
            let mut obj = json!({
                "available": info.available,
                "current_version": info.current_version,
                "latest_version": info.latest_version,
            });
            if opts.include_release_metadata {
                let map = obj.as_object_mut().expect("json! built an object");
                map.insert("release_notes".into(), json!(info.release_notes));
                map.insert("published_at".into(), json!(info.published_at));
                map.insert("install_method".into(), json!(info.install_method));
                map.insert(
                    "containerized".into(),
                    json!(crate::updater::is_containerized()),
                );
            } else {
                obj.as_object_mut()
                    .expect("json! built an object")
                    .insert("install_method".into(), json!(info.install_method));
            }
            obj
        }
        Err(e) => json!({ "error": e }),
    };

    let (device, device_check) = if duduclaw_core::is_appliance() {
        let device = match update_status().await {
            Ok(v) => v,
            Err(e) => json!({ "error": e.message() }),
        };
        let device_check = match device_update_check(home_dir).await {
            Ok(v) => v,
            Err(OsOpError::Coded { code, message }) => {
                json!({ "error": { "code": code, "message": message } })
            }
            Err(other) => json!({ "error": other.message() }),
        };
        (device, device_check)
    } else {
        let note = json!({ "note": opts.not_appliance_note });
        (note.clone(), note)
    };

    json!({ "system": system, "device": device, "device_check": device_check })
}

// ── Doctor: the check rows both front doors build ────────────────────────

/// Count agents by scanning `<home>/agents/*/agent.toml` — the same figure
/// the live gateway's in-memory registry reports, reachable without IPC.
pub fn count_configured_agents(home_dir: &Path) -> usize {
    std::fs::read_dir(home_dir.join("agents"))
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().join("agent.toml").is_file())
                .count()
        })
        .unwrap_or(0)
}

/// The three always-present doctor rows, in their shipped order.
///
/// `include_can_repair` adds the dashboard's `can_repair` field; the MCP
/// tool's reduced payload has never carried it.
pub fn doctor_base_checks(
    config_exists: bool,
    has_agents: bool,
    has_key: bool,
    include_can_repair: bool,
) -> Vec<Value> {
    let mut rows = vec![
        json!({
            "name": "config_file",
            "status": if config_exists { "pass" } else { "fail" },
            "message": if config_exists { "config.toml exists" } else { "config.toml not found" },
        }),
        json!({
            "name": "agents",
            "status": if has_agents { "pass" } else { "warn" },
            "message": if has_agents { "Agents found" } else { "No agents found" },
        }),
        json!({
            "name": "api_key",
            "status": if has_key { "pass" } else { "warn" },
            "message": if has_key { "ANTHROPIC_API_KEY is set" } else { "ANTHROPIC_API_KEY not set" },
        }),
    ];
    if include_can_repair {
        let repairable = [!config_exists, false, false];
        for (row, can_repair) in rows.iter_mut().zip(repairable) {
            row.as_object_mut()
                .expect("json! built an object")
                .insert("can_repair".into(), json!(can_repair));
        }
    }
    rows
}

/// The `mcp_server` doctor row.
pub fn doctor_mcp_check(status: &str, message: &str, include_can_repair: bool) -> Value {
    let mut row = json!({ "name": "mcp_server", "status": status, "message": message });
    if include_can_repair {
        row.as_object_mut()
            .expect("json! built an object")
            .insert("can_repair".into(), json!(false));
    }
    row
}

/// `{pass, warn, fail}` counts over a doctor check list.
pub fn doctor_summary(checks: &[Value]) -> Value {
    json!({
        "pass": checks.iter().filter(|c| c["status"] == "pass").count(),
        "warn": checks.iter().filter(|c| c["status"] == "warn").count(),
        "fail": checks.iter().filter(|c| c["status"] == "fail").count(),
    })
}

/// One `{check, hint}` row per non-passing check.
pub fn doctor_repair_hints(checks: &[Value]) -> Vec<Value> {
    checks
        .iter()
        .filter(|c| c["status"] != "pass")
        .map(|c| {
            let name = c["name"].as_str().unwrap_or("unknown");
            json!({ "check": name, "hint": crate::handlers::doctor_repair_hint(name) })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_are_the_already_shipped_stable_strings() {
        assert_eq!(
            OsOpError::NotAppliance.code(),
            Some(crate::handlers::DEVICE_NOT_APPLIANCE_ERROR_CODE)
        );
        assert_eq!(OsOpError::NotAppliance.code(), Some("not_appliance"));
        assert_eq!(OsOpError::ConfirmRequired.code(), Some("confirm_required"));
        assert_eq!(
            OsOpError::DeviceOp(crate::device_ops::DeviceOpError::Unsupported("x".into())).code(),
            Some("unsupported"),
        );
        assert_eq!(
            OsOpError::DeviceOp(crate::device_ops::DeviceOpError::Io("x".into())).code(),
            Some("io_error"),
        );
        assert_eq!(OsOpError::Message("m".into()).code(), None);
        assert_eq!(OsOpError::InvalidParams("m".into()).code(), None);
        assert_eq!(OsOpError::Serialize("m".into()).code(), None);
    }

    #[test]
    fn error_messages_match_the_already_shipped_copy() {
        assert_eq!(OsOpError::NotAppliance.message(), NOT_APPLIANCE_MESSAGE);
        assert_eq!(
            OsOpError::ConfirmRequired.message(),
            CONFIRM_REQUIRED_MESSAGE
        );
        assert_eq!(
            OsOpError::DeviceOp(crate::device_ops::DeviceOpError::Unsupported("boom".into()))
                .message(),
            "unsupported: boom",
        );
        assert_eq!(
            OsOpError::DeviceOp(crate::device_ops::DeviceOpError::Io("boom".into())).message(),
            "io error: boom",
        );
        assert_eq!(
            OsOpError::Serialize("bad".into()).serialize_detail(),
            Some("bad")
        );
    }

    #[test]
    fn power_action_parse_is_fail_closed() {
        assert_eq!(PowerAction::parse("restart").unwrap(), PowerAction::Restart);
        assert_eq!(
            PowerAction::parse("shutdown").unwrap(),
            PowerAction::Shutdown
        );
        for bad in ["", "Restart", "reboot", "poweroff", "restart "] {
            let err = PowerAction::parse(bad).unwrap_err();
            assert_eq!(err.message(), POWER_ACTION_MESSAGE, "accepted {bad:?}");
        }
    }

    #[test]
    fn device_op_payload_is_the_three_field_shape() {
        let v = device_op_payload(Ok(crate::device_ops::OpOutput {
            success: true,
            stdout: "out".into(),
            stderr: "err".into(),
        }))
        .unwrap();
        assert_eq!(
            v,
            json!({ "success": true, "stdout": "out", "stderr": "err" })
        );
    }

    #[test]
    fn doctor_base_checks_keep_their_shipped_order_and_optional_can_repair() {
        let lean = doctor_base_checks(true, true, true, false);
        let names: Vec<&str> = lean.iter().map(|c| c["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["config_file", "agents", "api_key"]);
        assert!(
            lean.iter().all(|c| c.get("can_repair").is_none()),
            "the MCP projection must not grow a can_repair field"
        );

        let rich = doctor_base_checks(false, false, false, true);
        assert_eq!(rich[0]["can_repair"], json!(true), "missing config repairs");
        assert_eq!(rich[1]["can_repair"], json!(false));
        assert_eq!(rich[2]["can_repair"], json!(false));
        assert_eq!(rich[0]["status"], "fail");
        assert_eq!(rich[1]["status"], "warn");
        assert_eq!(rich[2]["status"], "warn");
    }

    #[test]
    fn doctor_summary_and_hints_agree_with_the_rows() {
        let checks = vec![
            json!({ "name": "config_file", "status": "pass" }),
            json!({ "name": "agents", "status": "warn" }),
            json!({ "name": "api_key", "status": "fail" }),
        ];
        assert_eq!(
            doctor_summary(&checks),
            json!({ "pass": 1, "warn": 1, "fail": 1 })
        );
        let hints = doctor_repair_hints(&checks);
        assert_eq!(hints.len(), 2);
        assert_eq!(hints[0]["check"], "agents");
        assert_eq!(hints[1]["check"], "api_key");
    }

    #[tokio::test]
    async fn check_update_projections_differ_only_in_the_named_option() {
        // No appliance env on a dev/CI host — both halves must degrade to the
        // caller-supplied note rather than fabricating an answer.
        assert!(std::env::var(duduclaw_core::APPLIANCE_ENV).is_err());
        let home = tempfile::tempdir().unwrap();

        let verbose = check_update(
            home.path(),
            CheckUpdateOptions {
                include_release_metadata: true,
                not_appliance_note: "A",
            },
        )
        .await;
        let brief = check_update(
            home.path(),
            CheckUpdateOptions {
                include_release_metadata: false,
                not_appliance_note: "B",
            },
        )
        .await;

        assert_eq!(verbose["device"], json!({ "note": "A" }));
        assert_eq!(verbose["device_check"], json!({ "note": "A" }));
        assert_eq!(brief["device"], json!({ "note": "B" }));
        assert!(verbose.get("system").is_some());
        assert!(brief.get("system").is_some());
    }

    /// Golden equivalence: one authority output, two front-door renderings.
    ///
    /// The MCP tool embeds the payload as text, the dashboard RPC returns it
    /// as a `WsFrame` payload. Both must carry the SAME JSON — this pins that
    /// the two adapters project, never re-derive.
    #[test]
    fn one_authority_payload_renders_identically_through_both_json_front_doors() {
        let payload = device_op_payload(Ok(crate::device_ops::OpOutput {
            success: false,
            stdout: String::new(),
            stderr: "nope".into(),
        }))
        .unwrap();

        // MCP rendering: `{"content":[{"type":"text","text": <payload>}]}`.
        let mcp_text = payload.to_string();
        let reparsed: Value = serde_json::from_str(&mcp_text).unwrap();

        // RPC rendering: the payload IS the frame payload.
        assert_eq!(reparsed, payload);
    }
}
