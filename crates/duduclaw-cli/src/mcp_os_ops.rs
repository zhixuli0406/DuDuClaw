//! O-0: agent-facing bridge for the dashboard-only `device.*` (WP-B) and
//! `system.*` (check_update/apply_update/doctor/status) RPCs.
//!
//! Design authority: `commercial/docs/DESIGN-agent-os-native-apps-2026-08.md`
//! §6 (NL-OS) — §6.1 core principle, §6.3 O-0, §6.4 coupling/security. Core
//! rule: **same capability, two front doors, same gate set**. Every tool
//! here reuses the SAME admin / appliance / confirm / ApprovalBroker / sysd
//! checks the dashboard `device.*`/`system.*` RPCs already enforce
//! (`duduclaw_gateway::handlers::MethodHandler::dispatch`) — never a
//! separately-derived or looser copy.
//!
//! ## Cross-process constraint (read this before touching a handler here)
//!
//! `duduclaw mcp-server` runs as its own stdio subprocess per Claude Code
//! session — a SEPARATE OS process from the long-running `duduclaw-gateway`
//! binary that owns the dashboard WebSocket RPC. Nothing here can reach
//! `MethodHandler`'s in-memory state (the connected-agent registry, the
//! `system.apply_update` `pending_update` URL cache, the WS `event_tx`
//! progress bridge) directly — only on-disk state and pure/stateless
//! functions are reachable from this process. Concretely:
//!
//! - `device.*` (WP-B) is ENTIRELY pure/file-based (sysinfo reads, shell-outs
//!   behind the `DeviceOps` trait, plain file I/O) — every tool below calls
//!   the EXACT SAME free functions the dashboard RPC handlers call
//!   (`duduclaw_gateway::device`, `device_ops::select_device_ops()` — which
//!   already picks the privilege-separated `SysdDeviceOps` on a real
//!   appliance, `backup_schedule`, `files_api`), so there is zero drift risk
//!   and privileged ops still route through `duduclaw-sysd`.
//! - `system.status` needs the LIVE gateway's in-memory registry/uptime/
//!   connected-channel state, which this process cannot read. `os_system_status`
//!   below reports a reduced, honestly-labelled payload (fresh disk-derived
//!   agent count, the `channel_status.json` snapshot the gateway already
//!   maintains for exactly this kind of cross-process read, edition profile
//!   resolved from env only) rather than fabricating the live-only fields.
//! - `system.apply_update` normally caches a trusted download/checksum URL
//!   pair in `MethodHandler::pending_update` at `check_update` time so
//!   `apply_update` never has to trust a caller-supplied URL. This process
//!   cannot share that cache, so `os_apply_update`'s `system` target instead
//!   resolves a FRESH trusted URL pair via `updater::check_update()` and
//!   applies it atomically within the same call — same "never trust a
//!   caller-supplied URL" invariant, different (still safe) mechanism. It
//!   does not go through a gateway extension's custom update provider
//!   (white-label/distributor channel) — see the function doc comment.
//!
//! Every tool is admin-scope-gated at the shared MCP dispatch choke point
//! (`mcp_auth::tool_requires_scope` → `Scope::Admin`, enforced in
//! `mcp_dispatch.rs` before this module is ever reached) and, for the
//! `device.*`-backed tools, additionally fail-closed on
//! `duduclaw_core::is_appliance()` here — mirroring the dashboard's
//! `require_admin!()` + `require_appliance!()` macros exactly. Destructive
//! ops mirror `require_confirm!()`; `os_factory_reset` additionally routes
//! through `ApprovalBroker` (a NEW gate beyond the dashboard RPC's
//! confirm-only check — see its doc comment for why).

use std::path::Path;

use serde_json::{json, Value};

// ── Small local result helpers (mirrors mcp_recording.rs's rec_text/rec_error
//    convention — each MCP submodule owns its own, not a shared import) ────

fn os_ops_text(text: &str) -> Value {
    json!({ "content": [{"type": "text", "text": text}] })
}

fn os_ops_error(msg: &str) -> Value {
    json!({ "content": [{"type": "text", "text": msg}], "isError": true })
}

/// Fail-closed `not_appliance` refusal. SAME zh-TW copy as the dashboard
/// `device.*` RPC gate (`duduclaw_gateway::handlers::device_not_appliance_frame`,
/// code `DEVICE_NOT_APPLIANCE_ERROR_CODE = "not_appliance"`) so an operator
/// sees one consistent message regardless of which front door they used.
fn not_appliance_error() -> Value {
    debug_assert_eq!(
        duduclaw_gateway::handlers::DEVICE_NOT_APPLIANCE_ERROR_CODE,
        "not_appliance"
    );
    os_ops_error("此功能僅限 DuDuClaw 裝置版（appliance image）使用。")
}

/// Fail-closed refusal for a destructive op missing `"confirm": true` — SAME
/// zh-TW copy as the dashboard `device.*` RPC gate's `require_confirm!()`.
fn confirm_required_error() -> Value {
    os_ops_error("這是不可逆的操作，請在請求參數帶上 confirm: true 再次確認執行。")
}

fn confirm_flag(args: &Value) -> bool {
    args.get("confirm").and_then(Value::as_bool) == Some(true)
}

/// Render a `device_ops::OpResult` the same shape the dashboard's
/// `device_op_result_frame` uses (`success`/`stdout`/`stderr` on success;
/// `DeviceOpError`'s own `Display` — "unsupported: …" / "io error: …" — on
/// failure, byte-identical to the dashboard's error text).
fn device_op_result_text(result: duduclaw_gateway::device_ops::OpResult) -> Value {
    match result {
        Ok(out) => os_ops_text(
            &json!({ "success": out.success, "stdout": out.stdout, "stderr": out.stderr })
                .to_string(),
        ),
        Err(e) => os_ops_error(&e.to_string()),
    }
}

/// Count agents by scanning `<home>/agents/*/agent.toml` directly — a fresh,
/// honest recomputation of the same figure the live gateway's in-memory
/// registry would report (the registry itself is populated by scanning this
/// exact directory at startup), reachable without gateway IPC.
fn count_configured_agents(home_dir: &Path) -> usize {
    std::fs::read_dir(home_dir.join("agents"))
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().join("agent.toml").is_file())
                .count()
        })
        .unwrap_or(0)
}

// ── Read-only tools (admin scope only; device.*-backed ones also require
//    is_appliance()) ─────────────────────────────────────────────────────

/// `os_device_status` → `device.status`. Same collector
/// (`duduclaw_gateway::device::collect_status`), same appliance gate.
pub(crate) async fn handle_os_device_status(home_dir: &Path) -> Value {
    if !duduclaw_core::is_appliance() {
        return not_appliance_error();
    }
    let status = duduclaw_gateway::device::collect_status(home_dir);
    match serde_json::to_value(&status) {
        Ok(v) => os_ops_text(&v.to_string()),
        Err(e) => os_ops_error(&format!("device status serialize failed: {e}")),
    }
}

/// `os_network_info` → `device.network` (read path only — the RPC's
/// static-IP write path is `not_implemented` there too, so this tool never
/// exposes a write shape at all).
pub(crate) async fn handle_os_network_info() -> Value {
    if !duduclaw_core::is_appliance() {
        return not_appliance_error();
    }
    match serde_json::to_value(duduclaw_gateway::device::collect_network()) {
        Ok(v) => os_ops_text(&json!({ "interfaces": v }).to_string()),
        Err(e) => os_ops_error(&format!("network interfaces serialize failed: {e}")),
    }
}

/// `os_wifi_status` → `network.status` (D4a's rich link/IP/connectivity
/// facade — NOT `device.network`/`os_network_info`'s bare interface list;
/// see `duduclaw_gateway::network` module doc for why the two are
/// deliberately separate). Cross-process-safe the same way `os_network_info`
/// is: `network::status()` degrades every sub-source to an honest
/// "unavailable"/"unknown" value rather than reading any live-gateway
/// in-memory state, so it is always `Ok` and reachable from this out-of-
/// process tool exactly like the dashboard RPC (`network.status`, no
/// appliance gate on the dashboard side either — but this tool still applies
/// one here, matching the other four `device.*`/`network.*`-backed read
/// tools' `is_appliance()` gate for consistency, since Wi-Fi hardware only
/// exists on the appliance in practice).
///
/// Agent-body vertical slice (Y2-3): this is the "eye" — a caller asks
/// "what's my Wi-Fi doing right now" and gets link state / IP / captive-
/// portal verdict in one call. See
/// `commercial/docs/DESIGN-agent-body-network-2026-08.md` §4.
pub(crate) async fn handle_os_wifi_status() -> Value {
    if !duduclaw_core::is_appliance() {
        return not_appliance_error();
    }
    match duduclaw_gateway::network::status().await {
        Ok(status) => match serde_json::to_value(&status) {
            Ok(v) => os_ops_text(&v.to_string()),
            Err(e) => os_ops_error(&format!("wifi status serialize failed: {e}")),
        },
        // `network::status()` never constructs an `Err` today (see its own
        // doc), but the `Result` return type is real — degrade honestly
        // rather than unwrap.
        Err(err) => os_ops_error(&duduclaw_gateway::network::error_to_json(&err).to_string()),
    }
}

/// `os_wifi_scan` → `network.wifi_scan`. `rescan` (optional, default `true`
/// — same default the dashboard RPC uses) requests a fresh iwd scan before
/// reading results; `false` reads whatever iwd already knows without
/// triggering a new radio scan. Read-only: this tool can see networks, it
/// cannot join one — see the design doc's §5 for why `os_wifi_connect` is
/// deliberately NOT part of this tool face yet (the PSK never reaches an
/// agent's context; the write path needs a dedicated human-facing secure
/// channel, not a new MCP tool parameter).
///
/// Agent-body vertical slice (Y2-3): this is the other half of the "eye" —
/// "what networks can I see, and how strong are they" (`WifiNetwork.ssid`/
/// `signal_bars`/`security`/`known`), the exact data behind the "我看到 3
/// 個網路，DuDu-Office 訊號最強" line in the design's dialogue flow.
pub(crate) async fn handle_os_wifi_scan(args: &Value) -> Value {
    if !duduclaw_core::is_appliance() {
        return not_appliance_error();
    }
    let rescan = args.get("rescan").and_then(Value::as_bool).unwrap_or(true);
    match duduclaw_gateway::network::wifi_scan(rescan).await {
        Ok(result) => os_ops_text(&duduclaw_gateway::network::scan_result_to_json(&result).to_string()),
        Err(err) => os_ops_error(&duduclaw_gateway::network::error_to_json(&err).to_string()),
    }
}

/// `os_wifi_connect` → `network.wifi_connect`, **structurally without a
/// `psk` parameter** — there is no code path in this function's signature
/// that can accept, forward, log, or audit a plaintext Wi-Fi passphrase.
/// This is deliberate, not an oversight (see
/// `commercial/docs/DESIGN-agent-body-network-2026-08.md` §5): a secret
/// typed into a chat turn becomes part of the LLM's context, the transcript,
/// and potentially `tool_calls.jsonl` — none of which this platform treats
/// as a secret store. Calling `network::wifi_connect(ssid, None)` reuses
/// iwd's OWN semantics for a missing psk: succeeds for an open network or a
/// network iwd already holds a stored credential for (`WifiNetwork::known`
/// from a prior `os_wifi_scan`), fails with `wrong_password` for a new
/// secured network — that specific failure code is the intended signal for
/// the operator persona (O-4) to hand off to a human-facing password entry
/// surface instead of retrying with a guessed value.
///
/// Same `confirm: true` gate as `os_power` — connecting changes the box's
/// active network, a real-world side effect worth one explicit human
/// authorization even when no secret is involved (the design doc's dialogue
/// flow's step 3 "授權點" applies to every connect, not only the
/// password-required branch).
pub(crate) async fn handle_os_wifi_connect(args: &Value, home_dir: &Path) -> Value {
    if !duduclaw_core::is_appliance() {
        return not_appliance_error();
    }
    if !confirm_flag(args) {
        return confirm_required_error();
    }
    let ssid = match args.get("ssid").and_then(Value::as_str) {
        Some(s) if !s.trim().is_empty() => s.to_string(),
        _ => return os_ops_error("ssid 不可為空"),
    };

    let result = duduclaw_gateway::network::wifi_connect(&ssid, None).await;
    audit_agent_wifi_event(home_dir, "wifi_connect", &ssid, &result);
    match result {
        Ok(()) => os_ops_text(&json!({ "state": "connected", "ssid": ssid }).to_string()),
        Err(err) => os_ops_error(&duduclaw_gateway::network::error_to_json_with_ssid(&err, &ssid).to_string()),
    }
}

/// Same audit shape as the dashboard's own `audit_wifi_event`
/// (`{ssid, ok, code, source}`, no psk, no "was a psk even supplied" flag)
/// but `source: "agent_mcp"` instead of `"dashboard"` — the audit trail can
/// tell "an agent did this on a human's behalf" apart from "a human clicked
/// it in Settings" without inventing a second schema. `home_dir` is threaded
/// in by the caller rather than re-resolved here, matching every other
/// `mcp_os_ops.rs` handler's convention (and keeping it consistent with a
/// caller-supplied tempdir in tests).
fn audit_agent_wifi_event(
    home_dir: &Path,
    event_type: &str,
    ssid: &str,
    result: &Result<(), duduclaw_gateway::network::WifiError>,
) {
    // Deliberately best-effort: an audit-write failure must never surface as
    // a tool-call failure to the caller (the connect attempt itself already
    // succeeded or failed on its own terms).
    let (ok, code) = match result {
        Ok(()) => (true, None),
        Err(e) => (false, Some(e.code.code())),
    };
    duduclaw_security::audit::append_audit_event(
        home_dir,
        &duduclaw_security::audit::AuditEvent::new(
            event_type,
            ssid,
            duduclaw_security::audit::Severity::Info,
            json!({ "ssid": ssid, "ok": ok, "code": code, "source": "agent_mcp" }),
        ),
    );
}

/// `os_backup_list` → `device.backup_list`. Same two calls
/// (`backup_schedule::backups_dir` + `files_api::list_files`) as the
/// dashboard handler — that handler is already a 3-line wrapper over these,
/// so this is not a second implementation, it's the same one.
pub(crate) async fn handle_os_backup_list(home_dir: &Path) -> Value {
    if !duduclaw_core::is_appliance() {
        return not_appliance_error();
    }
    let dir = duduclaw_gateway::backup_schedule::backups_dir(home_dir);
    let files = duduclaw_gateway::files_api::list_files(&dir);
    match serde_json::to_value(&files) {
        Ok(v) => os_ops_text(&json!({ "files": v }).to_string()),
        Err(e) => os_ops_error(&format!("backup list serialize failed: {e}")),
    }
}

/// `os_system_status` — reduced, honestly-labelled counterpart to
/// `system.status`. See the module doc comment for exactly which fields are
/// unavailable cross-process and why (`uptime_seconds` / live connection
/// counts require the running gateway's in-memory state).
///
/// Unlike the four `device.*`-backed read tools, `system.status` itself has
/// NO appliance requirement in the dashboard RPC (it works on every
/// install) — this tool mirrors that: it always runs, `is_appliance()` is
/// reported as a plain field, not a gate.
pub(crate) async fn handle_os_system_status(home_dir: &Path) -> Value {
    let version = duduclaw_gateway::updater::current_version();
    let agents_count = count_configured_agents(home_dir);
    let is_appliance = duduclaw_core::is_appliance();
    // Edition profile: env-var override only (no `tier_key`) — the license
    // runtime tier lives in the live gateway process's in-memory cache and
    // is unreachable here. `config`-layer override is also unreachable (it
    // is the dashboard's own runtime toggle, not a file), so this can read
    // narrower than the dashboard's `system.status` in that one case —
    // documented, not silently approximated.
    let edition_profile = duduclaw_core::EditionProfile::resolve_from_env(None, None);

    // channels_connected: read the gateway-maintained `channel_status.json`
    // snapshot — the SAME cross-process bridge the existing `channel_status`
    // MCP tool already relies on (`handle_channel_status` in `mcp.rs`).
    // `None` (not `0`) when the gateway has never written a snapshot, so a
    // fresh/never-booted gateway is never misreported as "zero channels".
    let channels_connected: Option<usize> = std::fs::read_to_string(home_dir.join("channel_status.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.get("channels").and_then(|c| c.as_object()).map(|m| {
            m.values()
                .filter(|s| s.get("connected").and_then(Value::as_bool) == Some(true))
                .count()
        }));

    os_ops_text(
        &json!({
            "version": version,
            "agents_count": agents_count,
            "appliance": is_appliance,
            "edition_profile": edition_profile.as_str(),
            "channels_connected": channels_connected,
            "note": "跨程序限制：uptime_seconds 等僅存在於執行中 gateway 行程記憶體的欄位本工具無法回報；\
                     channels_connected 讀自 gateway 定期寫出的 channel_status.json 快照（可能有數秒延遲），\
                     null 代表 gateway 尚未寫出過快照。完整即時狀態請用 dashboard 的 system.status。",
        })
        .to_string(),
    )
}

/// `os_check_update` → `system.check_update` (always) + `device.update_status`
/// + `device.update_check` (both only when `is_appliance()` — omitted, not
/// hard-blocked, when not, since this tool intentionally combines three
/// independently-scoped RPCs into one read and the system half is
/// universally available).
///
/// Deliberately omits `download_url`/`checksum_url` from the `system` half
/// (present in the dashboard RPC's response): `os_apply_update`'s `system`
/// target re-resolves its own trusted URL pair rather than accepting one
/// from a caller (including this tool's own prior output) — see that
/// function's doc comment for the M2 "never trust a caller-supplied URL"
/// invariant this preserves.
///
/// `device_check` (Y5-3, agent-body update vertical slice): before this
/// field existed, an agent asking "有沒有新版本可以更新" only ever saw
/// `device.update_status` (`systemd-sysupdate list`'s view of the LOCAL
/// staging directory — empty until a `device.update_apply` call has already
/// downloaded something) and could not tell whether the CONFIGURED source
/// actually has something newer. This mirrors exactly what the dashboard's
/// own "check for update" button does (`DevicePage.tsx`'s `runUpdateCheck`
/// calls BOTH `device.update_status` and `device.update_check` in parallel) —
/// an agent asking the same question deserves the same answer, not a weaker
/// one. Additive only: `device` is unchanged, so
/// `os_operator::readonly_result_to_artifact`'s existing `update_status` card
/// (which only reads `device`/`system`) is untouched.
pub(crate) async fn handle_os_check_update(home_dir: &Path) -> Value {
    let system = match duduclaw_gateway::updater::check_update().await {
        Ok(info) => json!({
            "available": info.available,
            "current_version": info.current_version,
            "latest_version": info.latest_version,
            "release_notes": info.release_notes,
            "published_at": info.published_at,
            "install_method": info.install_method,
            "containerized": duduclaw_gateway::updater::is_containerized(),
        }),
        Err(e) => json!({ "error": e }),
    };
    let (device, device_check) = if duduclaw_core::is_appliance() {
        let device = match duduclaw_gateway::device_ops::select_device_ops()
            .update_status()
            .await
        {
            Ok(out) => json!({ "success": out.success, "stdout": out.stdout, "stderr": out.stderr }),
            Err(e) => json!({ "error": e.to_string() }),
        };
        let device_check = match duduclaw_gateway::os_update::check_update(home_dir).await {
            Ok(report) => json!({
                "available": report.available,
                "current_version": report.current_version,
                "latest_version": report.latest_version,
            }),
            Err(e) => json!({ "error": { "code": e.code(), "message": e.user_message() } }),
        };
        (device, device_check)
    } else {
        let note = json!({ "note": "非 appliance 安裝，無 OS image 更新可查（僅限 appliance）。" });
        (note.clone(), note)
    };
    os_ops_text(&json!({ "system": system, "device": device, "device_check": device_check }).to_string())
}

/// `os_doctor_repair` — reduced counterpart to `system.doctor_repair`.
/// Reuses the exact same three-source API-key check
/// (`duduclaw_gateway::handlers::has_api_key_configured`) and the exact same
/// MCP cold-start probe + status/message renderer + repair-hint text
/// (`duduclaw_gateway::doctor_probes`) as the dashboard RPC. Deliberately
/// omits the dashboard's `container_runtime` (docker) and `grok_cli` probes:
/// neither is gate-relevant, and their underlying checks
/// (`handlers::check_docker`) are private to the gateway crate and tied to
/// heavier live subprocess probing not worth duplicating for an agent tool.
/// Full diagnostics remain available via the dashboard or `duduclaw doctor`.
pub(crate) async fn handle_os_doctor_repair(home_dir: &Path) -> Value {
    let config_exists = home_dir.join("config.toml").exists();
    let has_agents = count_configured_agents(home_dir) > 0;
    let has_key = duduclaw_gateway::handlers::has_api_key_configured(home_dir).await;
    let mcp_report = duduclaw_gateway::doctor_probes::mcp_cold_start_probe(home_dir).await;
    let (mcp_status, mcp_message) = duduclaw_gateway::doctor_probes::mcp_cold_start_status_and_message(
        &mcp_report.outcome,
        mcp_report.provision_error.as_deref(),
    );

    let checks = vec![
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
        json!({ "name": "mcp_server", "status": mcp_status, "message": mcp_message }),
    ];
    let repair_hints: Vec<Value> = checks
        .iter()
        .filter(|c| c["status"] != "pass")
        .map(|c| {
            let name = c["name"].as_str().unwrap_or("unknown");
            json!({ "check": name, "hint": duduclaw_gateway::handlers::doctor_repair_hint(name) })
        })
        .collect();
    let pass = checks.iter().filter(|c| c["status"] == "pass").count();
    let warn = checks.iter().filter(|c| c["status"] == "warn").count();
    let fail = checks.iter().filter(|c| c["status"] == "fail").count();

    os_ops_text(
        &json!({
            "checks": checks,
            "summary": { "pass": pass, "warn": warn, "fail": fail },
            "repair_hints": repair_hints,
            "note": "省略 dashboard system.doctor 的 container_runtime/grok_cli 探測（非閘相關，需較重的子行程探測）。",
        })
        .to_string(),
    )
}

// ── Change / destructive tools ───────────────────────────────────────────

/// `os_backup_create` → `device.backup_create`. Calls the SAME free function
/// (`duduclaw_gateway::handlers::create_device_backup_archive`) the dashboard
/// handler itself was refactored to call — genuinely one implementation, two
/// callers, not a second copy.
pub(crate) async fn handle_os_backup_create(home_dir: &Path) -> Value {
    if !duduclaw_core::is_appliance() {
        return not_appliance_error();
    }
    use duduclaw_gateway::handlers::DeviceBackupOutcome;
    match duduclaw_gateway::handlers::create_device_backup_archive(home_dir).await {
        DeviceBackupOutcome::Created { filename, stdout, stderr } => os_ops_text(
            &json!({ "filename": filename, "stdout": stdout, "stderr": stderr }).to_string(),
        ),
        DeviceBackupOutcome::OpFailed(out) => os_ops_text(
            &json!({ "success": false, "stdout": out.stdout, "stderr": out.stderr }).to_string(),
        ),
        DeviceBackupOutcome::OpError(e) => os_ops_error(&e.to_string()),
        DeviceBackupOutcome::MoveFailed(msg) => os_ops_error(&msg),
    }
}

/// `os_power` → `device.power`. Same admin + appliance + confirm gate as
/// the dashboard RPC (`require_admin!()` + `require_appliance!()` +
/// `require_confirm!()`); NOT additionally approval-gated — restart/shutdown
/// is recoverable (the device comes back up), unlike `os_factory_reset`.
pub(crate) async fn handle_os_power(args: &Value) -> Value {
    if !duduclaw_core::is_appliance() {
        return not_appliance_error();
    }
    if !confirm_flag(args) {
        return confirm_required_error();
    }
    let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
    let result = match action {
        "restart" => duduclaw_gateway::device_ops::select_device_ops().reboot().await,
        "shutdown" => duduclaw_gateway::device_ops::select_device_ops().poweroff().await,
        _ => return os_ops_error("action 必須是 \"restart\" 或 \"shutdown\""),
    };
    device_op_result_text(result)
}

/// TTL for the `os_factory_reset` approval — mirrors `INSTALL_APPROVAL_TTL_SECONDS`
/// in `mcp.rs` (5 minutes: a realistic window for a human to see the push
/// notification and decide before it auto-denies).
const FACTORY_RESET_APPROVAL_TTL_SECONDS: i64 = 300;
/// Poll cadence while blocking on the decision — mirrors `INSTALL_APPROVAL_POLL`.
const FACTORY_RESET_APPROVAL_POLL: std::time::Duration = std::time::Duration::from_secs(2);

/// `os_factory_reset` → `device.factory_reset`. Same admin + appliance +
/// confirm gate as the dashboard RPC, PLUS a NEW `ApprovalBroker` gate not
/// present on the RPC itself.
///
/// Why the addition: the dashboard's `"confirm": true` param is meaningful
/// because a HUMAN is physically clicking through the dashboard's confirm
/// dialog — the human-in-the-loop already happened by the time the RPC
/// fires. An agent-issued `confirm: true` carries no such guarantee (the
/// agent decided it itself). Factory reset wipes device state and is
/// genuinely irreversible, so per design §6.1 ("不可逆走 ApprovalBroker")
/// this front door requires a live human decision via the SAME
/// `ApprovalBroker` + `run_install_approval` polling primitive
/// `crate::mcp::gate_install_approval` already uses for install-class tools
/// — fail-closed on broker-unavailable / denial / expiry, identical
/// semantics, not a re-derived approximation.
pub(crate) async fn handle_os_factory_reset(args: &Value, home_dir: &Path, caller_client_id: &str) -> Value {
    if !duduclaw_core::is_appliance() {
        return not_appliance_error();
    }
    if !confirm_flag(args) {
        return confirm_required_error();
    }
    if let Err(msg) = require_factory_reset_approval(home_dir, caller_client_id).await {
        return os_ops_error(&msg);
    }
    // D4a-8: same optional `clear_network` (default `false` — keep saved
    // Wi-Fi credentials) as the dashboard `device.factory_reset` RPC.
    let clear_network = args.get("clear_network").and_then(Value::as_bool).unwrap_or(false);
    device_op_result_text(
        duduclaw_gateway::device_ops::select_device_ops()
            .factory_reset(home_dir, clear_network)
            .await,
    )
}

async fn require_factory_reset_approval(home_dir: &Path, caller_client_id: &str) -> Result<(), String> {
    use duduclaw_gateway::approval::ApprovalBroker;

    let broker = match ApprovalBroker::open(home_dir) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "os_factory_reset: ApprovalBroker unavailable — denying (fail-closed)");
            return Err(
                "審批系統暫時無法使用，已拒絕 factory reset（fail-closed）。請稍後再試或由管理員經 dashboard 執行。"
                    .to_string(),
            );
        }
    };
    require_factory_reset_approval_via(&broker, caller_client_id).await
}

/// Broker-injected inner half of [`require_factory_reset_approval`] — split
/// out purely for testability (mirrors `mcp.rs`'s own
/// `gate_install_approval` → `run_install_approval` split): tests drive this
/// with an in-memory `ApprovalBroker` instead of a disk-backed one opened
/// twice from two independent connections (the production caller and a test
/// "decider" task each `ApprovalBroker::open`-ing the same path was found to
/// race under load during O-0 test development — using one shared,
/// in-process broker instance removes that cross-connection variable
/// entirely, exactly like the existing `approval_granted_proceeds`/
/// `approval_denied_blocks` tests in `mcp.rs` already do).
async fn require_factory_reset_approval_via(
    broker: &duduclaw_gateway::approval::ApprovalBroker,
    caller_client_id: &str,
) -> Result<(), String> {
    let agent_id = if caller_client_id.is_empty() { "unknown-agent" } else { caller_client_id };
    match crate::mcp::run_install_approval(
        broker,
        agent_id,
        "系統操作員 agent 要求執行 factory reset：清除裝置狀態並於下次開機重新佈建。",
        json!({ "tool": "os_factory_reset" }),
        FACTORY_RESET_APPROVAL_TTL_SECONDS,
        FACTORY_RESET_APPROVAL_POLL,
    )
    .await
    {
        crate::mcp::InstallApprovalOutcome::Proceed => Ok(()),
        crate::mcp::InstallApprovalOutcome::Denied(msg) => Err(msg),
    }
}

/// `os_apply_update` — bridges TWO independent RPCs behind one `target`
/// param (required — the two operations have very different blast radii, so
/// this tool never silently guesses which one was meant):
/// - `"device"` → the SAME verify→stage→backup→ESP-clear→install→
///   confirm-slot→cleanup pipeline `device.update_apply` runs
///   ([`duduclaw_gateway::handlers::stage_and_apply_device_update`] — see
///   that function's doc comment for the gap this closed: an earlier version
///   of this branch called the bare `device_ops::update_apply()` sysupdate
///   wrapper directly, skipping H3d's manifest signature verification
///   entirely). Same admin + appliance gate as the RPC.
/// - `"system"` → `system.apply_update` (duduclaw's own binary self-update).
///   Same admin-only gate as the RPC (no appliance requirement — this works
///   on every install). See [`apply_system_update`] for how it preserves the
///   RPC's "never trust a caller-supplied URL" invariant without the
///   cross-process `pending_update` cache.
///
/// **`confirm: true` required for BOTH targets** (Y5-3, agent-body update
/// vertical slice — a fix, not a pre-existing behavior): neither
/// `device.update_apply` nor `system.apply_update` requires a `confirm` param
/// at the dashboard-RPC layer, because a human already clicked a real button
/// to get there. An agent has no such button — the model calling this tool
/// IS the decision — so, exactly like `os_factory_reset`'s doc comment
/// argues for approval, this tool requires an explicit signal a model cannot
/// emit by accident. Recoverable (A/B rollback + boot assessment exists for
/// `device`; `os_check_update`/`os_apply_update` themselves are retriable for
/// `system`), so confirm-only — same tier as `os_power`, not
/// `os_factory_reset`'s ApprovalBroker. This was a real gap: unlike
/// `os_power`/`os_wifi_connect`/`os_factory_reset`, this handler previously
/// had NO `confirm_flag` check at all, even though `os_intent.rs`'s
/// `tool_gate` already classified `ApplyUpdate` as needing one — the router
/// only gates the FIRST message of an NL-matched turn, not every tool call
/// deeper in an autonomous/multi-step task (see
/// `commercial/docs/DESIGN-agent-body-network-2026-08.md` §6.2), so the
/// actual enforcement has to live here.
///
/// **Cross-restart result report handshake (Y8-3, T1 —
/// `commercial/docs/DESIGN-agent-body-update-2026-08.md` §3.4/§13)**: both
/// success branches below call [`record_pending_update_report`] before
/// returning. This is a deterministic, handler-side write — not a
/// system-prompt instruction hoping the model remembers to call
/// `working_state_set` itself — because no such "tool call auto-chains
/// another tool call" mechanism exists on this platform (`os_operator.rs`
/// never calls an MCP handler). See that function's doc comment for what
/// gets written and why; see `update_report_reconcile.rs` (`duduclaw-
/// gateway`) for who reads it back after the restart.
pub(crate) async fn handle_os_apply_update(args: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let target = args.get("target").and_then(|v| v.as_str()).unwrap_or("");
    match target {
        "device" => {
            if !duduclaw_core::is_appliance() {
                return not_appliance_error();
            }
            if !confirm_flag(args) {
                return confirm_required_error();
            }
            match duduclaw_gateway::handlers::stage_and_apply_device_update(home_dir).await {
                duduclaw_gateway::handlers::DeviceUpdateApplyOutcome::StageFailed(e) => {
                    os_ops_error(&json!({ "code": e.code(), "message": e.user_message() }).to_string())
                }
                duduclaw_gateway::handlers::DeviceUpdateApplyOutcome::EspPrepareFailed(message) => {
                    os_ops_error(&json!({ "code": "esp_prepare_failed", "message": message }).to_string())
                }
                duduclaw_gateway::handlers::DeviceUpdateApplyOutcome::SlotMismatch(message) => {
                    os_ops_error(&json!({ "code": "slot_mismatch", "message": message }).to_string())
                }
                duduclaw_gateway::handlers::DeviceUpdateApplyOutcome::Applied(applied) => {
                    record_pending_update_report(home_dir, default_agent, "device", None).await;
                    device_op_result_text(applied)
                }
            }
        }
        "system" => {
            if !confirm_flag(args) {
                return confirm_required_error();
            }
            apply_system_update(home_dir, default_agent).await
        }
        _ => os_ops_error(
            "target 必須是 \"device\"（appliance OS image 更新，經 duduclaw-sysd）或 \
             \"system\"（duduclaw 本體自我更新）。",
        ),
    }
}

/// Best-effort write of the cross-restart report handshake (Y8-3, T1) —
/// `pending_update_report` in `working_state`, read back by `duduclaw-
/// gateway`'s `update_report_reconcile::sweep` on a later `DispatchEngine`
/// tick (possibly after this very process, and the machine/gateway it ran
/// on top of, have both restarted).
///
/// Uses [`duduclaw_gateway::working_state::set_entry`] directly — the same
/// pure Rust API the `working_state_set` MCP tool wraps — rather than making
/// a second MCP round-trip, because this handler already knows everything
/// that write needs and a second tool call would just be indirection with
/// nowhere to indirect to (there is no MCP client on the other end of this
/// stdio connection that would relay a call back to `duduclaw mcp-server`
/// itself).
///
/// `report_channel`/`report_chat_id` are NOT accepted as arguments the model
/// could supply — they are read from `DUDUCLAW_REPLY_CHANNEL`
/// (`duduclaw_core::ENV_REPLY_CHANNEL`), the same env var
/// `channel_reply.rs` already threads down into this subprocess so
/// `send_to_agent`/install-approval flows can find their way back to the
/// originating chat (see `decision_notify::origin_target`'s doc comment on
/// the `duduclaw-gateway` side). A console/dashboard-triggered call (no
/// channel context) simply omits it — `update_report_reconcile.rs` falls
/// back to the agent's own default `[proactive]` notify destination.
///
/// Failure here (unknown agent id, key-cap exceeded, disk error) is logged
/// and swallowed: the update itself already succeeded by the time this
/// runs, and losing the bookkeeping write must never turn a successful
/// `os_apply_update` call into an error response.
async fn record_pending_update_report(
    home_dir: &Path,
    agent_id: &str,
    target: &str,
    expected_version: Option<&str>,
) {
    let reply_channel_raw = std::env::var(duduclaw_core::ENV_REPLY_CHANNEL)
        .ok()
        .filter(|s| !s.is_empty());
    let value = json!({
        "target": target,
        "expected_version": expected_version,
        "initiated_at": chrono::Utc::now().to_rfc3339(),
        "reply_channel_raw": reply_channel_raw,
        "restart_triggered": false,
        "restart_triggered_at": serde_json::Value::Null,
    })
    .to_string();
    let reason = format!("觸發 os_apply_update(target={target})，需要跨重啟回報結果");
    let home = home_dir.to_path_buf();
    let agent = agent_id.to_string();
    let key = duduclaw_core::WORKING_STATE_KEY_PENDING_UPDATE_REPORT;
    let result = tokio::task::spawn_blocking(move || {
        duduclaw_gateway::working_state::set_entry(&home, &agent, key, &value, &reason, Some(4.0), None)
    })
    .await;
    match result {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => {
            tracing::warn!(agent = agent_id, target, error = %e, "pending_update_report 寫入失敗（更新本身已成功，僅跨重啟回報這一步受影響）");
        }
        Err(e) => tracing::warn!(agent = agent_id, target, error = %e, "pending_update_report 寫入 join 失敗"),
    }
}

/// `system.apply_update`'s core effect (download + verify + swap the running
/// binary), reached without the dashboard RPC's `MethodHandler::pending_update`
/// cache: this call resolves a FRESH trusted `(download_url, checksum_url)`
/// pair via `updater::check_update()` — the same trusted resolution the
/// dashboard RPC's `check_update` step performs — and applies it
/// immediately, in the same call, so no untrusted URL is ever accepted from
/// an argument.
///
/// Known gap (documented, not silently papered over): this does NOT consult
/// `self.extension.update_provider()` — a gateway-extension-supplied custom
/// update channel some white-label/distributor builds configure. Those
/// deployments' `os_apply_update(target="system")` falls back to the OSS
/// default GitHub/control-plane channel instead. Reaching the extension's
/// provider would require exposing its resolution to a cross-process
/// reader, which is out of scope for the O-0 tool-face skeleton.
async fn apply_system_update(home_dir: &Path, default_agent: &str) -> Value {
    let info = match duduclaw_gateway::updater::check_update().await {
        Ok(info) => info,
        Err(e) => return os_ops_error(&format!("更新檢查失敗：{e}")),
    };
    if !info.available {
        return os_ops_text("目前已是最新版本，無需更新。");
    }

    // Same audit event kind/shape as the dashboard RPC ("system_update"/
    // "apply") so existing audit tooling picks up an agent-initiated update
    // identically to a human-initiated one.
    duduclaw_security::audit::append_audit_event(
        home_dir,
        &duduclaw_security::audit::AuditEvent::new(
            "system_update",
            "system",
            duduclaw_security::audit::Severity::Info,
            json!({ "action": "apply", "target_version": info.latest_version, "via": "os_apply_update" }),
        ),
    );

    let no_progress = |_p: duduclaw_gateway::updater::UpdateProgress| {};
    match duduclaw_gateway::updater::apply_update_with_progress(
        &info.download_url,
        &info.checksum_url,
        &no_progress,
    )
    .await
    {
        Ok(res) => {
            record_pending_update_report(home_dir, default_agent, "system", Some(&info.latest_version)).await;
            match serde_json::to_value(&res) {
                Ok(v) => os_ops_text(&json!({ "applied": true, "version": info.latest_version, "result": v }).to_string()),
                Err(e) => os_ops_text(&format!("更新已套用（version={}），但結果序列化失敗：{e}", info.latest_version)),
            }
        }
        Err(e) => os_ops_error(&format!("更新套用失敗：{e}")),
    }
}

/// `os_boot_assessment` → `device.boot_assessment`. Read-only view of
/// systemd's automatic boot assessment (`good`/`bad`/`indeterminate`/`clean`)
/// for the currently-running version — same call
/// (`device_ops::boot_assessment_status`) the dashboard RPC handler makes.
///
/// Agent-body update vertical slice (Y5-3): this is the tool the
/// cross-restart reporting design (`commercial/docs/DESIGN-agent-body-
/// update-2026-08.md` §4) depends on — after an OS-image update reboot, an
/// agent needs a way to answer "did the update I just applied actually take,
/// or did the box roll itself back" without a human having to ask. Before
/// this tool existed, `device.boot_assessment` was dashboard-only; an agent
/// had no way to read it at all, appliance or not.
pub(crate) async fn handle_os_boot_assessment() -> Value {
    if !duduclaw_core::is_appliance() {
        return not_appliance_error();
    }
    device_op_result_text(
        duduclaw_gateway::device_ops::select_device_ops()
            .boot_assessment_status()
            .await,
    )
}

/// `os_update_rollback` → `device.update_rollback`. Same admin + appliance +
/// confirm gate as the dashboard RPC (`require_admin!()` +
/// `require_appliance!()` + `require_confirm!()`) — same tier as `os_power`,
/// NOT `os_factory_reset`'s ApprovalBroker: rolling back to the previously-
/// installed A/B slot is the platform's own designed recovery path, not a
/// destructive action in the irreversible sense (the slot being rolled back
/// FROM is still on disk, not wiped).
///
/// Agent-body update vertical slice (Y5-3): completes the agent-reachable
/// A/B lifecycle (check → apply → boot-assess → roll back) — before this
/// tool existed, an agent that saw a bad boot assessment (via
/// `os_boot_assessment`) or a user reporting "更新後系統怪怪的" had no way to
/// help recover; the human had to be walked through the dashboard instead.
pub(crate) async fn handle_os_update_rollback(args: &Value) -> Value {
    if !duduclaw_core::is_appliance() {
        return not_appliance_error();
    }
    if !confirm_flag(args) {
        return confirm_required_error();
    }
    device_op_result_text(
        duduclaw_gateway::device_ops::select_device_ops()
            .update_rollback()
            .await,
    )
}

// ── A7c: agent→display bridge — `os_display_get`/`os_display_set` ────────
//
// Bridges A7a's `display` group (comp's `shell_control` socket: cursor
// size/source, comp's own decoration theme, output scale) to agents.
// Unlike every OTHER `device.*`-backed tool above, this does NOT call a
// pure/file-based function — `duduclaw_gateway::display_bridge` makes one
// real (but stateless, one-shot) Unix-socket round trip to comp's FIXED
// kiosk socket path each call, which is exactly why it is reachable from
// this out-of-process `mcp-server` subprocess at all (see that module's own
// doc for the full "why this works cross-process" reasoning, and
// `commercial/docs/DESIGN-os-self-drive-2026-08.md` for A7a's original
// uid-boundary finding this closes). `is_appliance()` is still checked here
// first, matching every other appliance-only tool's fast-fail shape — the
// bridge itself does not redundantly re-check it (see its own doc).
//
// requires_approval = false for BOTH tools (A7a design doc §5: appearance
// preferences are reversible, low-risk) — no ApprovalBroker gate, unlike
// `os_factory_reset`/`os_system_timezone_set`.

pub(crate) async fn handle_os_display_get() -> Value {
    if !duduclaw_core::is_appliance() {
        return not_appliance_error();
    }
    match duduclaw_gateway::display_bridge::display_get().await {
        Ok(v) => os_ops_text(&v.to_string()),
        Err(e) => os_ops_error(&e),
    }
}

pub(crate) async fn handle_os_display_set(args: &Value) -> Value {
    if !duduclaw_core::is_appliance() {
        return not_appliance_error();
    }
    let Some(field) = args.get("field").and_then(Value::as_str) else {
        return os_ops_error("缺少必要參數 field（合法值：cursor_size / cursor_source / theme / output_scale）。");
    };
    let Some(value) = args.get("value").and_then(Value::as_str) else {
        return os_ops_error("缺少必要參數 value（字串）。");
    };
    match duduclaw_gateway::display_bridge::display_set(field, value).await {
        Ok(v) => os_ops_text(&v.to_string()),
        Err(e) => os_ops_error(&e),
    }
}

// ── Y10-1: agent→audio bridge — `os_audio_get`/`os_audio_set` ────────────
//
// The audio twin of A7c's `os_display_get`/`os_display_set` pair directly
// above, built on the same "same capability, two front doors, same gate
// set" convention — but bridging `duduclaw_gateway::audio_bridge` (a plain
// `wpctl` subprocess call) instead of a comp socket round trip. See that
// module's own doc for why audio never goes through `duduclaw-comp` at all.
//
// requires_approval = false for BOTH tools — volume/mute/output-device are
// reversible, low-risk preferences, same tier as os_display_get/set (A7a
// design doc §5's spirit), not destructive machine operations. No
// ApprovalBroker gate.

pub(crate) async fn handle_os_audio_get() -> Value {
    if !duduclaw_core::is_appliance() {
        return not_appliance_error();
    }
    match duduclaw_gateway::audio_bridge::audio_get().await {
        Ok(v) => os_ops_text(&v.to_string()),
        Err(e) => os_ops_error(&e),
    }
}

pub(crate) async fn handle_os_audio_set(args: &Value) -> Value {
    if !duduclaw_core::is_appliance() {
        return not_appliance_error();
    }
    let Some(field) = args.get("field").and_then(Value::as_str) else {
        return os_ops_error("缺少必要參數 field（合法值：volume / mute / output）。");
    };
    let Some(value) = args.get("value").and_then(Value::as_str) else {
        return os_ops_error("缺少必要參數 value（字串）。");
    };
    match duduclaw_gateway::audio_bridge::audio_set(field, value).await {
        Ok(v) => os_ops_text(&v.to_string()),
        Err(e) => os_ops_error(&e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_home() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    // ── Pure helpers ─────────────────────────────────────────────────────

    #[test]
    fn confirm_flag_only_accepts_literal_true() {
        assert!(!confirm_flag(&json!({})));
        assert!(!confirm_flag(&json!({"confirm": false})));
        assert!(!confirm_flag(&json!({"confirm": "true"})), "a string \"true\" must not satisfy the gate");
        assert!(confirm_flag(&json!({"confirm": true})));
    }

    #[test]
    fn not_appliance_error_is_fail_closed_shape() {
        let v = not_appliance_error();
        assert_eq!(v["isError"], true);
        let text = v["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("appliance"), "unexpected message: {text}");
    }

    #[test]
    fn confirm_required_error_is_fail_closed_shape() {
        let v = confirm_required_error();
        assert_eq!(v["isError"], true);
        let text = v["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("confirm"), "unexpected message: {text}");
    }

    #[test]
    fn device_op_result_text_maps_success_and_error_variants() {
        use duduclaw_gateway::device_ops::{DeviceOpError, OpOutput};

        let ok = device_op_result_text(Ok(OpOutput {
            success: true,
            stdout: "done".to_string(),
            stderr: String::new(),
        }));
        assert_ne!(ok["isError"], true);

        let unsupported = device_op_result_text(Err(DeviceOpError::Unsupported("nope".to_string())));
        assert_eq!(unsupported["isError"], true);
        assert!(unsupported["content"][0]["text"].as_str().unwrap().contains("unsupported"));

        let io_err = device_op_result_text(Err(DeviceOpError::Io("disk full".to_string())));
        assert_eq!(io_err["isError"], true);
        assert!(io_err["content"][0]["text"].as_str().unwrap().contains("io error"));
    }

    #[test]
    fn count_configured_agents_counts_only_dirs_with_agent_toml() {
        let home = tmp_home();
        let agents = home.path().join("agents");
        std::fs::create_dir_all(agents.join("a")).unwrap();
        std::fs::write(agents.join("a").join("agent.toml"), "").unwrap();
        std::fs::create_dir_all(agents.join("b")).unwrap(); // no agent.toml — must not count
        assert_eq!(count_configured_agents(home.path()), 1);
    }

    // ── Appliance fail-closed gate ──────────────────────────────────────
    //
    // Mirrors `duduclaw_gateway::handlers`'s own
    // `all_device_methods_fail_closed_off_appliance` test: never flips
    // `DUDUCLAW_APPLIANCE` in-process (that test's doc comment explains why
    // — avoids any risk of racing other tests in the same process that read
    // it). CI/dev hosts are never appliances, so this path is exercised on
    // every run; the "on appliance" branches reuse the exact same
    // `device_ops`/`device` free functions the dashboard RPC handlers call,
    // whose own behavior is covered by `device_ops.rs`/`device.rs`'s tests.

    #[tokio::test]
    async fn appliance_gated_tools_fail_closed_off_appliance() {
        assert!(
            std::env::var(duduclaw_core::APPLIANCE_ENV).is_err(),
            "precondition: DUDUCLAW_APPLIANCE must be unset in the test process"
        );
        let home = tmp_home();

        let results = vec![
            handle_os_device_status(home.path()).await,
            handle_os_network_info().await,
            handle_os_wifi_status().await,
            handle_os_wifi_scan(&json!({})).await,
            handle_os_wifi_connect(&json!({"ssid": "DuDu-Office", "confirm": true}), home.path()).await,
            handle_os_backup_list(home.path()).await,
            handle_os_backup_create(home.path()).await,
            handle_os_power(&json!({"action": "restart", "confirm": true})).await,
            handle_os_factory_reset(&json!({"confirm": true}), home.path(), "sysop").await,
            handle_os_apply_update(&json!({"target": "device", "confirm": true}), home.path(), "test-agent").await,
            handle_os_boot_assessment().await,
            handle_os_update_rollback(&json!({"confirm": true})).await,
            handle_os_display_get().await,
            handle_os_display_set(&json!({"field": "output_scale", "value": "150"})).await,
            handle_os_audio_get().await,
            handle_os_audio_set(&json!({"field": "volume", "value": "70"})).await,
        ];
        for v in &results {
            assert_eq!(v["isError"], true, "{v:?}");
            let text = v["content"][0]["text"].as_str().unwrap();
            assert!(text.contains("appliance"), "unexpected message: {text}");
        }
    }

    // ── A7c: os_display_get / os_display_set ────────────────────────────
    //
    // The appliance gate is exercised above; these pin the tool-level
    // argument validation (missing field/value) that runs AFTER the gate —
    // covered here rather than off-appliance-only above because on a real
    // appliance a caller could still send a malformed request, and
    // `handle_os_display_set` must refuse it with a clear message rather
    // than reaching `display_bridge::display_set` with a `None`. Since the
    // test process is never an appliance, the gate fires first for BOTH
    // cases — so this also doubles as a second confirmation that the gate
    // really does run before argument parsing, matching every other O-0
    // tool's ordering.

    #[tokio::test]
    async fn display_set_missing_field_is_refused() {
        let v = handle_os_display_set(&json!({"value": "150"})).await;
        assert_eq!(v["isError"], true, "{v:?}");
        // Off-appliance in this test process, so the appliance gate fires
        // first — the missing-field message is only reachable on a real
        // appliance. This still proves the gate is fail-closed even for a
        // malformed request.
        assert!(v["content"][0]["text"].as_str().unwrap().contains("appliance"));
    }

    #[tokio::test]
    async fn display_set_missing_value_is_refused() {
        let v = handle_os_display_set(&json!({"field": "theme"})).await;
        assert_eq!(v["isError"], true, "{v:?}");
        assert!(v["content"][0]["text"].as_str().unwrap().contains("appliance"));
    }

    // ── Y10-1: os_audio_get / os_audio_set ──────────────────────────────
    //
    // Same ordering rationale as the display pair's own tests above: the
    // test process is never an appliance, so the gate fires before argument
    // parsing for every case here too — this still proves the gate really
    // does run first, matching every other O-0 tool.

    #[tokio::test]
    async fn audio_set_missing_field_is_refused() {
        let v = handle_os_audio_set(&json!({"value": "70"})).await;
        assert_eq!(v["isError"], true, "{v:?}");
        assert!(v["content"][0]["text"].as_str().unwrap().contains("appliance"));
    }

    #[tokio::test]
    async fn audio_set_missing_value_is_refused() {
        let v = handle_os_audio_set(&json!({"field": "volume"})).await;
        assert_eq!(v["isError"], true, "{v:?}");
        assert!(v["content"][0]["text"].as_str().unwrap().contains("appliance"));
    }

    /// `os_system_status` has NO appliance requirement (`system.status`
    /// doesn't either) — it must succeed off-appliance, not refuse.
    ///
    /// `os_doctor_repair` is deliberately NOT exercised end-to-end here: it
    /// calls the live `doctor_probes::mcp_cold_start_probe`, which spawns a
    /// real `duduclaw mcp-server` subprocess — `doctor_probes.rs`'s own test
    /// module only ever unit-tests the pure `classify_mcp_cold_start`
    /// classifier and never invokes the live probe either, for the same
    /// reason (needs the built binary, live subprocess spawn, not a fast/
    /// deterministic unit test). `handle_os_doctor_repair`'s other three
    /// checks (config_file/agents/api_key) and the repair-hint mapping are
    /// covered indirectly by `count_configured_agents`'s test above and by
    /// reusing `duduclaw_gateway::handlers::{has_api_key_configured,
    /// doctor_repair_hint}` — the SAME functions `duduclaw-gateway`'s own
    /// test suite covers.
    #[tokio::test]
    async fn system_status_works_off_appliance() {
        let home = tmp_home();
        std::fs::create_dir_all(home.path().join("agents")).unwrap();

        let status = handle_os_system_status(home.path()).await;
        assert_ne!(status["isError"], true, "{status:?}");
    }

    /// Agent-body network vertical slice (Y2-3): the `rescan` param is
    /// optional and defaults to `true` off-appliance too — the gate fires
    /// before the param is ever read, so `{}` and an explicit `false` must
    /// both refuse the same way, not diverge in error shape.
    #[tokio::test]
    async fn wifi_scan_rescan_param_is_optional_and_gate_fires_first() {
        let default_rescan = handle_os_wifi_scan(&json!({})).await;
        let explicit_no_rescan = handle_os_wifi_scan(&json!({"rescan": false})).await;
        for v in [&default_rescan, &explicit_no_rescan] {
            assert_eq!(v["isError"], true, "{v:?}");
            assert!(v["content"][0]["text"].as_str().unwrap().contains("appliance"));
        }
    }

    /// Agent-body network vertical slice (Y2-3): `os_wifi_connect` has no
    /// `psk` field in its schema at all — even a caller that supplies one
    /// must see it silently ignored, never echoed back or forwarded. Off-
    /// appliance the `is_appliance()` gate fires before any of that logic
    /// runs (matching `os_power`'s gate ordering), so this only pins the
    /// shape of the refusal, not the ssid/confirm logic — see
    /// `appliance_gated_tools_fail_closed_off_appliance` above for that.
    #[tokio::test]
    async fn wifi_connect_ignores_a_smuggled_psk_argument_shape() {
        let home = tmp_home();
        let v = handle_os_wifi_connect(
            &json!({"ssid": "DuDu-Office", "confirm": true, "psk": "should-be-ignored"}),
            home.path(),
        )
        .await;
        assert_eq!(v["isError"], true);
        let text = v["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("appliance"));
        assert!(!text.contains("should-be-ignored"), "a psk value must never round-trip into any response text");
    }

    #[tokio::test]
    async fn apply_update_rejects_unknown_or_missing_target() {
        let home = tmp_home();
        let unknown = handle_os_apply_update(&json!({"target": "nonsense"}), home.path(), "test-agent").await;
        assert_eq!(unknown["isError"], true);
        assert!(unknown["content"][0]["text"].as_str().unwrap().contains("target"));

        let missing = handle_os_apply_update(&json!({}), home.path(), "test-agent").await;
        assert_eq!(missing["isError"], true);
    }

    /// Y5-3 (agent-body update vertical slice) regression: `os_apply_update`
    /// previously had NO `confirm_flag` check at all for either target —
    /// the `"device"` branch's missing check was masked by the appliance
    /// gate firing first in every off-appliance test (see
    /// `appliance_gated_tools_fail_closed_off_appliance` above), but the
    /// `"system"` branch has NO appliance gate, so it is the one path that
    /// can prove the confirm check exists and fires BEFORE any network call
    /// (`apply_system_update` would otherwise reach a live
    /// `updater::check_update()` call, which this test must never trigger).
    #[tokio::test]
    async fn apply_update_system_target_requires_confirm_before_any_network_call() {
        let home = tmp_home();
        let v = handle_os_apply_update(&json!({"target": "system"}), home.path(), "test-agent").await;
        assert_eq!(v["isError"], true, "{v:?}");
        let text = v["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("confirm"), "unexpected message: {text}");
    }

    /// The `"device"` branch's appliance gate must run BEFORE its confirm
    /// gate (matching `os_power`/`os_wifi_connect`'s existing ordering) —
    /// confirm:true present must NOT change the off-appliance refusal shape.
    #[tokio::test]
    async fn apply_update_device_target_appliance_gate_fires_even_with_confirm() {
        let home = tmp_home();
        let v = handle_os_apply_update(&json!({"target": "device", "confirm": true}), home.path(), "test-agent").await;
        assert_eq!(v["isError"], true, "{v:?}");
        assert!(v["content"][0]["text"].as_str().unwrap().contains("appliance"));
    }

    // ── Y8-3 T1: cross-restart report handshake write ───────────────────
    //
    // `handle_os_apply_update`'s success branches are unreachable off-
    // appliance (device) / without live network (system) in this test
    // process — see `appliance_gated_tools_fail_closed_off_appliance` and
    // `apply_update_system_target_requires_confirm_before_any_network_call`'s
    // own doc comments for why those two paths are deliberately not
    // exercised end-to-end here. `record_pending_update_report` is exercised
    // directly instead — it is the one new piece of production logic this
    // ticket adds to this crate, and it needs no appliance/network access.

    #[tokio::test]
    async fn record_pending_update_report_writes_a_parseable_working_state_entry() {
        let home = tmp_home();
        std::fs::create_dir_all(home.path().join("agents").join("sysop")).unwrap();

        record_pending_update_report(home.path(), "sysop", "system", Some("1.63.0")).await;

        let full = duduclaw_gateway::working_state::read_full(home.path(), "sysop", 0).unwrap();
        let entry = &full["states"][duduclaw_core::WORKING_STATE_KEY_PENDING_UPDATE_REPORT];
        assert_ne!(*entry, serde_json::Value::Null, "{full:?}");
        let value: serde_json::Value =
            serde_json::from_str(entry["value"].as_str().unwrap()).unwrap();
        assert_eq!(value["target"], "system");
        assert_eq!(value["expected_version"], "1.63.0");
        assert_eq!(value["restart_triggered"], false);
        assert!(value["restart_triggered_at"].is_null());
    }

    #[tokio::test]
    async fn record_pending_update_report_omits_expected_version_for_device_target() {
        let home = tmp_home();
        std::fs::create_dir_all(home.path().join("agents").join("sysop")).unwrap();

        record_pending_update_report(home.path(), "sysop", "device", None).await;

        let full = duduclaw_gateway::working_state::read_full(home.path(), "sysop", 0).unwrap();
        let value: serde_json::Value = serde_json::from_str(
            full["states"][duduclaw_core::WORKING_STATE_KEY_PENDING_UPDATE_REPORT]["value"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(value["target"], "device");
        assert!(value["expected_version"].is_null());
    }

    /// An unresolvable agent id must not panic or propagate an error to the
    /// caller — the update already succeeded by the time this runs; the
    /// bookkeeping write is best-effort (see the function's own doc comment).
    #[tokio::test]
    async fn record_pending_update_report_unknown_agent_fails_open_silently() {
        let home = tmp_home();
        // No `agents/ghost` directory created — `working_state::set_entry`
        // will refuse with "unknown agent", which must be swallowed, not
        // panic this test.
        record_pending_update_report(home.path(), "ghost", "system", Some("1.63.0")).await;
    }

    /// Agent-body update vertical slice (Y5-3): `os_check_update`'s new
    /// `device_check` field must be present in every response shape (even
    /// off-appliance, where it degrades to the same honest `note` `device`
    /// already used) — the field is additive, so this also pins that
    /// `device`/`system` are untouched (no accidental shape drift for
    /// `os_operator::readonly_result_to_artifact`'s existing card logic).
    #[tokio::test]
    async fn check_update_includes_device_check_field_off_appliance() {
        let home = tmp_home();
        let v = handle_os_check_update(home.path()).await;
        let text = v["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert!(parsed.get("system").is_some());
        assert!(parsed.get("device").is_some());
        assert!(parsed.get("device_check").is_some(), "{parsed:?}");
        // Off-appliance: both `device` and `device_check` are the same
        // honest "note" shape, never a fabricated freshness answer.
        assert!(parsed["device_check"].get("note").is_some());
    }

    #[tokio::test]
    async fn boot_assessment_and_update_rollback_are_appliance_gated() {
        let boot = handle_os_boot_assessment().await;
        assert_eq!(boot["isError"], true);
        assert!(boot["content"][0]["text"].as_str().unwrap().contains("appliance"));

        let rollback_no_confirm = handle_os_update_rollback(&json!({})).await;
        assert_eq!(rollback_no_confirm["isError"], true);
        // Off-appliance: the appliance gate fires first, same ordering as
        // `os_power`/`os_wifi_connect`/`os_apply_update`'s device branch.
        assert!(rollback_no_confirm["content"][0]["text"].as_str().unwrap().contains("appliance"));
    }

    // ── ApprovalBroker gate for os_factory_reset ────────────────────────
    //
    // `require_factory_reset_approval_via` (the broker-injected inner half
    // of `require_factory_reset_approval`, split out for exactly this) is
    // driven with a SHARED in-memory `ApprovalBroker` — same technique
    // `mcp.rs`'s own `approval_granted_proceeds`/`approval_denied_blocks`
    // tests use for `run_install_approval`. An earlier version of these
    // tests opened a SECOND, independent disk-backed `ApprovalBroker` for
    // the decider task (pointed at the same tempdir path) and was observed
    // to hang for the full TTL on the "granted" case under load — sharing
    // one in-process broker instance (cloned, `Arc`-backed store) removes
    // that cross-connection variable entirely.

    fn in_mem_broker() -> duduclaw_gateway::approval::ApprovalBroker {
        duduclaw_gateway::approval::ApprovalBroker::new(std::sync::Arc::new(
            duduclaw_gateway::approval::ApprovalStore::open_in_memory().unwrap(),
        ))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn factory_reset_approval_granted_proceeds() {
        let broker = in_mem_broker();
        let decider = broker.clone();
        tokio::spawn(async move {
            for _ in 0..200 {
                if let Ok(pending) = decider.list_pending(Some("sysop")).await {
                    if let Some(rec) = pending.first() {
                        decider.decide(&rec.id, true, "dashboard:admin").await.unwrap();
                        return;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        });
        let result = require_factory_reset_approval_via(&broker, "sysop").await;
        assert!(result.is_ok(), "{result:?}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn factory_reset_approval_denied_blocks() {
        let broker = in_mem_broker();
        let decider = broker.clone();
        tokio::spawn(async move {
            for _ in 0..200 {
                if let Ok(pending) = decider.list_pending(Some("sysop")).await {
                    if let Some(rec) = pending.first() {
                        decider.decide(&rec.id, false, "dashboard:admin").await.unwrap();
                        return;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        });
        let result = require_factory_reset_approval_via(&broker, "sysop").await;
        match result {
            Err(msg) => assert!(msg.contains("拒絕"), "got: {msg}"),
            Ok(()) => panic!("denied approval must NOT proceed"),
        }
    }

    /// `require_factory_reset_approval` (the outer, `ApprovalBroker::open`
    /// wrapper) is exercised separately, once, purely for the fail-closed
    /// "broker unavailable" path — pointing it at a path that cannot hold a
    /// SQLite file (a plain file, not a directory) so `ApprovalBroker::open`
    /// itself errors, without waiting on any decision loop at all.
    #[tokio::test]
    async fn broker_unavailable_denies_fail_closed() {
        let home = tmp_home();
        let not_a_dir = home.path().join("not-a-directory");
        std::fs::write(&not_a_dir, b"x").unwrap();
        let result = require_factory_reset_approval(&not_a_dir, "sysop").await;
        match result {
            Err(msg) => assert!(msg.contains("拒絕"), "got: {msg}"),
            Ok(()) => panic!("broker-open failure must fail closed, not proceed"),
        }
    }
}
