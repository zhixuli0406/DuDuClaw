//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

/// Machine-readable code the dashboard OS page matches on for the OS-native
/// quota rejection. Stable string — the frontend keys UI copy off it.
pub const OS_NATIVE_QUOTA_ERROR_CODE: &str = "os_native_quota_exceeded";

/// Build the structured error frame returned when a write would push the number
/// of OS-native ("OS 原生員工") agents past the edition quota. The message is
/// end-user zh-TW copy — no internal terms (capability keys, dir names) leak
/// into it. Pure so the code + message are unit-testable.
pub(crate) fn os_native_quota_reject_frame(limit: u32) -> WsFrame {
    let message = format!(
        "個人版僅能將 {limit} 個 AI 員工註冊為 OS 原生員工，請先停用其他員工的 OS 能力，\
         或升級方案以解鎖更多名額。"
    );
    WsFrame::Response {
        id: String::new(),
        ok: false,
        payload: None,
        error: Some(json!({
            "code": OS_NATIVE_QUOTA_ERROR_CODE,
            "message": message,
        })),
    }
}

/// Machine-readable code returned when a Personal-edition caller reaches an
/// RPC that only exists for the multi-person (Enterprise) form factor.
/// Stable string — the dashboard may key upgrade copy off it.
pub const ENTERPRISE_ONLY_ERROR_CODE: &str = "enterprise_edition_required";

/// Is `method` part of the Enterprise-only RPC surface?
///
/// # Why this exists (G2)
///
/// The dashboard's `EditionGuard` / `nav-visibility` flags are **UX only** —
/// the frontend says so itself. A Personal-edition user who edits the URL, hits
/// a legacy route alias, or speaks WebSocket JSON-RPC directly still reached
/// every enterprise management surface. This function is the server-side twin,
/// enforced once at the [`MethodHandler::dispatch`] chokepoint.
///
/// # Source of truth
///
/// `commercial/docs/ux-redesign-2026-08/09-edition-split-features.md` §1 — the
/// 89-unit inventory. Exactly the units marked ➖ (Personal-hidden) that have a
/// dashboard RPC of their own are listed here. The dividing axis is **"needs a
/// second natural person" / "sells to someone else's customers"**, never
/// complexity: approvals, audit logs, autopilot, kill switch, delegation
/// policy, shared wiki and the org chart are all cross-*agent*, not cross-
/// *person*, and stay fully open in Personal.
///
/// # Matching discipline
///
/// Whole-family entries compare the **first dot-separated segment for exact
/// equality** (never `starts_with`, per coding convention #2 — an unanchored
/// prefix would also swallow a hypothetical `users_export.run`). Exact-equality
/// on the segment is also fail-closed for *future* members: a new
/// `departments.rename` is Enterprise-gated the day it is added, without anyone
/// remembering to update this table.
///
/// Mixed families (`wiki.*`, `audit.*` — most of whose members are Personal-open)
/// cannot use that rule, so their Enterprise members are enumerated one by one.
/// **Any new `wiki.trust_*` or fleet-reliability RPC must be added below.**
///
/// # Deliberate exclusions (documented so they are not "fixed" back in)
///
/// - `users.me` / `users.change_password` — self-service only (both resolve the
///   target from `ctx.user_id`, never from params). §1.6 keeps the 帳號與密碼
///   tab open in Personal, and it is the *only* password path there.
/// - `license.*` — 授權管理 is `personalHidden` in the UI, but `license.activate`
///   / `license.redeem` are the upgrade path itself. Blocking them in Personal
///   would make Enterprise unreachable (edition is derived from license tier).
/// - `delegation.get` / `delegation.set` — §1.6 marks 委派權限 ✅ Personal:
///   it governs which *agent* may hand work to which agent, not which person.
/// - `billing.usage` — one RPC serves both the Personal 預算與超支 view (✅) and
///   the Enterprise 方案用量 KPI (➖); the split is a section-level render
///   decision inside `BillingPage`, not an RPC boundary.
/// - `security.*` / `killswitch.*` / `audit.unified_log` / `audit.evolution_query`
///   — D9 keeps kill switch + logs in Personal; the RBAC / vault / mount-guard
///   cards are hidden by conditional rendering, not by RPC.
/// - `topology.*` (組織架構) — D6 turned it into progressive disclosure
///   (`requiresData`), visible in **both** editions once ≥3 agents exist.
pub(crate) fn is_enterprise_only_method(method: &str) -> bool {
    // ── Self-service carve-outs inside an otherwise Enterprise family ──────
    // Checked first so the family rule below cannot swallow them.
    if matches!(method, "users.me" | "users.change_password") {
        return false;
    }

    // ── Whole Enterprise families (exact first-segment equality) ───────────
    let family = method.split('.').next().unwrap_or("");
    if matches!(
        family,
        // 成員（使用者管理）— §1.5「跨人第一名」
        "users"
        // 部門 — 人的組織分群
        | "departments"
        // 經銷商管理／發授權／白牌品牌 — 賣給別人的客戶
        | "distributor"
        // 夥伴入口（經銷 CRM）— 客戶數／抽成
        | "partner"
        // 身分解析 — 把不同通道上的「人」對到同一身分
        | "identity"
    ) {
        return true;
    }

    // ── Individual members of mixed families ───────────────────────────────
    matches!(
        method,
        // Wiki Trust 稽核 — 治理級知識稽核（`wiki.*` 其餘成員皆為個人版開放）
        "wiki.trust_audit" | "wiki.trust_history" | "wiki.trust_override"
        // 可靠性報告 — SRE 式機隊指標（`audit.unified_log` / `audit.evolution_query`
        // 為個人版開放的日誌與演化查詢，不在此列）
        | "audit.reliability_summary"
    )
}

/// Structured refusal for a Personal-edition caller hitting an Enterprise-only
/// RPC. End-user zh-TW copy — no method names, route paths or other internal
/// terms leak into it. Pure so code + message stay unit-testable.
pub(crate) fn enterprise_only_reject_frame() -> WsFrame {
    WsFrame::Response {
        id: String::new(),
        ok: false,
        payload: None,
        error: Some(json!({
            "code": ENTERPRISE_ONLY_ERROR_CODE,
            "message": "此功能屬多人團隊版，個人版沒有開放。\
                        升級後可解鎖成員、部門、治理政策與身分解析等多人協作功能：\
                        https://duduclaw.dudustudio.monster#pricing",
        })),
    }
}

/// Machine-readable code returned to a caller whose account still carries the
/// DB-level `must_change_password` flag and is trying to reach an RPC outside
/// the self-service change-password allowlist. Stable string — a frontend can
/// key a "go change your password" redirect off it instead of treating this
/// like an ordinary permission failure.
pub const MUST_CHANGE_PASSWORD_ERROR_CODE: &str = "must_change_password_required";

/// Is `method` reachable by a caller flagged `must_change_password` (the
/// bootstrap `admin@local`, or any account an operator has reset)?
///
/// # Why this exists
///
/// Before this fix, `server.rs::authenticate_jwt` refused the WS handshake
/// outright for such an account — the frontend could not distinguish that
/// refusal from a hung connection, and the one screen that could clear the
/// flag (account settings) lived behind the very gate that was blocking it
/// (`wiki/reports/resolved-todos/TODO-bootstrap-admin-ws-deadlock.md`). The handshake now
/// always succeeds for an Active account; this allowlist is the layer that
/// keeps the account otherwise unusable — C1's original "block all
/// operations" intent, enforced one level up so recovery stays reachable.
///
/// Kept to the bare self-service minimum: read one's own profile, set a new
/// password, and the handshake bookkeeping methods that already run before
/// any `UserContext` exists. Everything else — including every method the
/// Enterprise-only gate above would otherwise allow — is refused.
pub(crate) fn is_password_change_allowlisted(method: &str) -> bool {
    matches!(
        method,
        "users.me" | "users.change_password" | "connect" | "connect.challenge" | "ping"
    )
}

/// Structured refusal for a caller who must change their password before
/// doing anything else. End-user zh-TW copy, no method names or other
/// internal terms — same discipline as `enterprise_only_reject_frame`.
pub(crate) fn must_change_password_reject_frame() -> WsFrame {
    WsFrame::Response {
        id: String::new(),
        ok: false,
        payload: None,
        error: Some(json!({
            "code": MUST_CHANGE_PASSWORD_ERROR_CODE,
            "message": "此帳號尚未設定密碼，請先前往帳號設定完成密碼變更後再繼續使用。",
        })),
    }
}

/// Machine-readable code returned to a caller on an unauthenticated
/// (lock-screen) WebSocket connection that asked for anything outside
/// `power_local::PRE_AUTH_ALLOWED_METHOD`. Stable string — same pattern as
/// `MUST_CHANGE_PASSWORD_ERROR_CODE`, so a client can branch on it rather
/// than on prose.
pub const LOGIN_REQUIRED_ERROR_CODE: &str = "login_required";

/// Structured refusal for a pre-auth connection reaching a non-allowlisted
/// RPC. End-user zh-TW copy — names no method, module or internal term, same
/// discipline as `must_change_password_reject_frame`.
pub(crate) fn login_required_reject_frame() -> WsFrame {
    WsFrame::Response {
        id: String::new(),
        ok: false,
        payload: None,
        error: Some(json!({
            "code": LOGIN_REQUIRED_ERROR_CODE,
            "message": "請先登入後再使用這項功能。",
        })),
    }
}

/// Machine-readable code returned to a caller reaching a `device.*` RPC on a
/// non-appliance install. Stable string — mirrors
/// `MUST_CHANGE_PASSWORD_ERROR_CODE`/`ENTERPRISE_ONLY_ERROR_CODE`'s pattern.
pub const DEVICE_NOT_APPLIANCE_ERROR_CODE: &str = "not_appliance";

/// Structured refusal for `device.*` RPCs outside appliance mode
/// (`require_appliance!()` in `dispatch`). zh-TW copy, no internal method
/// names, same discipline as `enterprise_only_reject_frame`.
pub(crate) fn device_not_appliance_frame() -> WsFrame {
    WsFrame::Response {
        id: String::new(),
        ok: false,
        payload: None,
        error: Some(json!({
            "code": DEVICE_NOT_APPLIANCE_ERROR_CODE,
            "message": "此功能僅限 DuDuClaw 裝置版（appliance image）使用。",
        })),
    }
}

/// Structured error frame for the `accounts.setup_token_*` wizard
/// (WP-D) — `code` is one of [`crate::setup_token_wizard::SetupTokenErrorCode`]'s
/// stable strings; `message` is a zh-TW fallback, never the raw CLI
/// transcript (which may carry the captured token).
pub(crate) fn setup_token_error_frame(
    code: crate::setup_token_wizard::SetupTokenErrorCode,
    message: &str,
) -> WsFrame {
    WsFrame::Response {
        id: String::new(),
        ok: false,
        payload: None,
        error: Some(json!({
            "code": code.as_str(),
            "message": message,
        })),
    }
}

/// Structured error frame for the `finetune.*` family (WP-E).
///
/// `code` is [`crate::finetune::FinetuneError::code`]'s stable string. The
/// dashboard branches on `data_leaves_device_not_acknowledged` to open the
/// "this leaves your machine" consent dialog instead of showing a red toast,
/// so this MUST stay structured rather than collapsing to a bare message.
pub(crate) fn finetune_error_frame(e: &crate::finetune::FinetuneError) -> WsFrame {
    WsFrame::Response {
        id: String::new(),
        ok: false,
        payload: None,
        error: Some(json!({
            "code": e.code(),
            "message": e.message(),
        })),
    }
}

/// `Result<Value, FinetuneError>` → frame, so each `finetune.*` arm stays one
/// expression.
pub(crate) fn finetune_frame(r: Result<Value, crate::finetune::FinetuneError>) -> WsFrame {
    match r {
        Ok(v) => WsFrame::ok_response("", v),
        Err(e) => finetune_error_frame(&e),
    }
}

/// Structured refusal for a destructive `device.*` RPC missing
/// `"confirm": true` (`require_confirm!()` in `dispatch`).
pub(crate) fn device_confirm_required_frame() -> WsFrame {
    WsFrame::Response {
        id: String::new(),
        ok: false,
        payload: None,
        error: Some(json!({
            "code": "confirm_required",
            "message": "這是不可逆的操作，請在請求參數帶上 confirm: true 再次確認執行。",
        })),
    }
}

/// Turn a `device_ops::OpResult` into a `WsFrame` — the one place that maps
/// [`crate::device_ops::DeviceOpError`] onto the dashboard's error-frame
/// shape, so every `device.*` handler renders failures the same way.
pub(crate) fn device_op_result_frame(result: crate::device_ops::OpResult) -> WsFrame {
    match result {
        Ok(out) => WsFrame::ok_response(
            "",
            json!({
                "success": out.success,
                "stdout": out.stdout,
                "stderr": out.stderr,
            }),
        ),
        Err(crate::device_ops::DeviceOpError::Unsupported(msg)) => WsFrame::Response {
            id: String::new(),
            ok: false,
            payload: None,
            error: Some(json!({ "code": "unsupported", "message": msg })),
        },
        Err(crate::device_ops::DeviceOpError::Io(msg)) => WsFrame::Response {
            id: String::new(),
            ok: false,
            payload: None,
            error: Some(json!({ "code": "io_error", "message": msg })),
        },
    }
}

/// D4a: render a [`crate::network::WifiError`] as the standard error-frame
/// envelope, via [`crate::network::error_to_json`] — `detail` never reaches
/// this (see that function's own doc). Callers that know the attempted SSID
/// (`network.wifi_connect`) use [`crate::network::error_to_json_with_ssid`]
/// directly instead, so the `no_ip`/`portal` messages can name it.
pub(crate) fn network_error_frame(err: &crate::network::WifiError) -> WsFrame {
    WsFrame::Response {
        id: String::new(),
        ok: false,
        payload: None,
        error: Some(crate::network::error_to_json(err)),
    }
}

/// O16: render a [`crate::os_ops::OsOpError`] as this surface's error frame.
///
/// `serialize_what` is the already-shipped prefix for the (unreachable)
/// serialization arm — the one place the three `os_*` front doors word a
/// failure differently, so it is supplied per call site instead of being
/// flattened into the shared error type.
pub(crate) fn os_op_error_frame(err: &crate::os_ops::OsOpError, serialize_what: &str) -> WsFrame {
    use crate::os_ops::OsOpError as E;
    match err {
        E::NotAppliance => device_not_appliance_frame(),
        E::ConfirmRequired => device_confirm_required_frame(),
        E::InvalidParams(msg) | E::Message(msg) => WsFrame::error_response("", msg),
        E::Coded { code, message } => WsFrame::Response {
            id: String::new(),
            ok: false,
            payload: None,
            error: Some(json!({ "code": code, "message": message })),
        },
        E::DeviceOp(e) => device_op_result_frame(Err(e.clone())),
        E::Wifi(e) => network_error_frame(e),
        E::Serialize(detail) => {
            WsFrame::error_response("", &format!("{serialize_what} serialize failed: {detail}"))
        }
    }
}

/// O16: `Result<Value, OsOpError>` → frame, so every `device.*`/`network.*`
/// handler that routes through [`crate::os_ops`] stays one expression.
pub(crate) fn os_op_frame(result: Result<Value, crate::os_ops::OsOpError>, serialize_what: &str) -> WsFrame {
    match result {
        Ok(v) => WsFrame::ok_response("", v),
        Err(e) => os_op_error_frame(&e, serialize_what),
    }
}

/// System-settings app: `device.timedate_set`'s closed 3-code taxonomy
/// (`invalid_timezone` / `backend_unavailable` / `apply_failed`) — kept as
/// a simple code+message pair rather than a full enum type (unlike
/// `WifiErrorCode`/`crate::network::wired::WiredConfigErrorCode`, nothing
/// outside this one handler needs to match on it).
pub(crate) fn timedate_set_error_frame(code: &str, message: &str) -> WsFrame {
    WsFrame::Response {
        id: String::new(),
        ok: false,
        payload: None,
        error: Some(json!({ "code": code, "message": message })),
    }
}

/// System-settings app: render a
/// [`crate::network::wired::WiredConfigErrorCode`] as the standard
/// error-frame envelope — `network.wired_config`'s twin of
/// [`network_error_frame`].
pub(crate) fn network_wired_config_error_frame(code: crate::network::wired::WiredConfigErrorCode) -> WsFrame {
    WsFrame::Response {
        id: String::new(),
        ok: false,
        payload: None,
        error: Some(json!({ "code": code.code(), "message": code.message() })),
    }
}

/// `device.power_local`'s own result-frame mapping. UNLIKE the generic
/// [`device_op_result_frame`] (whose dashboard callers render the
/// stdout/stderr payload, so `ok:true` + `success:false` is legible there),
/// the lock screen branches on `ok` alone and deliberately renders nothing
/// on success — a power action that RAN but FAILED (e.g. `systemctl reboot`
/// under a polkit `Access denied`) must therefore answer `ok:false`, or the
/// operator watches "正在送出…" forever while nothing happens. That exact
/// laundering — shell-out fails, `OpOutput::success:false` rides an
/// `ok:true` frame, lock screen reads success — happened live on the
/// appliance VM (2026-08-23), alongside the sysd socket-permission defect
/// that forced the `SystemDeviceOps` fallback in the first place.
pub(crate) fn power_local_result_frame(result: crate::device_ops::OpResult) -> WsFrame {
    match result {
        Ok(out) if !out.success => {
            warn!(
                stderr = %duduclaw_core::truncate_chars(&out.stderr, 200),
                "lock-screen power action executed but the command failed"
            );
            WsFrame::Response {
                id: String::new(),
                ok: false,
                payload: None,
                error: Some(json!({
                    "code": "exec_failed",
                    "message": "電源指令執行失敗，請稍後再試。",
                })),
            }
        }
        other => device_op_result_frame(other),
    }
}
