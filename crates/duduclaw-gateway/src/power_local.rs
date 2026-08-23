//! Lock-screen power surface — the gate layer behind the `device.power_local`
//! dashboard RPC (dispatch lives in `handlers.rs`, the actual reboot/poweroff
//! shell-out in `device_ops.rs`, which this module never duplicates).
//!
//! # Why a login-free RPC exists at all
//!
//! The appliance lock screen is a **pre-auth surface** — it is what the
//! DuDuClaw shell paints *before* a password is accepted. Every desktop OS
//! puts power controls there (macOS, Windows and GNOME lock screens all do),
//! for the plain reason that whoever is standing at the machine can already
//! hold the physical power button down. Refusing a graceful shutdown to the
//! person who can yank the plug buys no security and costs a corrupted
//! filesystem.
//!
//! What that reasoning does NOT license is a login-free power switch reachable
//! from the LAN. So the surface is fenced by four independent, fail-closed
//! conditions, all of which must hold:
//!
//! 1. **Appliance mode** — `duduclaw_core::is_appliance()`, the single
//!    authority (`duduclaw-core/src/appliance.rs`), never re-derived here. On
//!    every other install (desktop, container, dev machine) this RPC does not
//!    exist as far as a caller can tell.
//! 2. **Loopback peer** — decided from the WebSocket connection's own TCP peer
//!    address (`RpcConnInfo::peer`), never from a request header. A header is
//!    caller-controlled; `X-Forwarded-For` and friends are exactly how a
//!    "localhost only" check gets bypassed.
//! 3. **Closed two-value action** — `reboot` or `shutdown`, exact match.
//!    Anything else is refused rather than interpreted.
//! 4. **A conservative rate limit** — a power button does not need to be
//!    pressable in a loop.
//!
//! Authentication is deliberately NOT among them; instead, a connection that
//! completed the WS handshake *without* credentials
//! (`RpcConnInfo::pre_auth`) is restricted at the top of
//! `MethodHandler::dispatch` to exactly the one method named in
//! [`PRE_AUTH_ALLOWED_METHOD`] — the same "handshake succeeds, dispatch-top
//! allowlist restricts" shape the bootstrap-admin deadlock fix established for
//! `users.change_password` (`handlers.rs::is_password_change_allowlisted`).
//!
//! ## Known limitation: a same-box reverse proxy
//!
//! Condition 2 reads the TCP peer, so an operator who fronts the gateway with
//! nginx **on the appliance itself** would make every LAN request look
//! loopback — the same caveat `local_session.rs`'s own gate carries and
//! documents for `/api/session/local`. The shipped appliance image binds the
//! gateway directly (`appliance_default_bind()` ⇒ `0.0.0.0`) with no proxy in
//! front, so LAN callers arrive with their real address; this is a note about
//! a configuration the image does not ship, not a hole in the shipped one.

use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::LazyLock;
use std::time::Duration;

use duduclaw_security::audit::{append_audit_event, AuditEvent, Severity};
use duduclaw_security::rate_limiter::RateLimiter;

use crate::device_ops::{DeviceOps, OpResult};

// ── Per-connection transport facts ──────────────────────────────────────

/// The transport-level facts about ONE WebSocket connection that RPC dispatch
/// needs but `UserContext` (a credentials/ACL snapshot) has no business
/// carrying: where the caller actually connected from, and whether the
/// connection authenticated at all.
///
/// Threaded from `server.rs::handle_socket` into
/// `MethodHandler::handle_conn`. Every other caller of the long-standing
/// `MethodHandler::handle` (in-process callers and this crate's own tests)
/// gets [`RpcConnInfo::internal`], whose `peer: None` reads as **not
/// loopback** — fail-closed, so a caller that cannot prove where it came from
/// never satisfies condition 2 above.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RpcConnInfo {
    /// TCP peer address of the WebSocket connection, when the call arrived
    /// over one. `None` for in-process dispatch.
    pub peer: Option<SocketAddr>,
    /// `true` when this connection completed the WS handshake with NO
    /// credential at all (the appliance lock-screen path). Such a connection
    /// is restricted to [`is_pre_auth_allowlisted`] methods.
    pub pre_auth: bool,
}

impl RpcConnInfo {
    /// In-process dispatch — no socket, and not a pre-auth connection (the
    /// caller is code inside the gateway, not an anonymous peer). `peer:
    /// None` deliberately fails the loopback condition.
    pub fn internal() -> Self {
        Self { peer: None, pre_auth: false }
    }

    /// A real WebSocket connection.
    pub fn from_ws(peer: SocketAddr, pre_auth: bool) -> Self {
        Self { peer: Some(peer), pre_auth }
    }

    /// Did this call arrive from the machine itself? `None` peer ⇒ `false`.
    pub fn peer_is_loopback(&self) -> bool {
        self.peer.map(|a| ip_is_loopback(a.ip())).unwrap_or(false)
    }

    /// Stable, low-cardinality label for logs/audit rows. Never a header
    /// value — only the address the kernel reports.
    pub fn peer_label(&self) -> String {
        match self.peer {
            Some(a) => a.ip().to_string(),
            None => "in-process".to_string(),
        }
    }
}

/// Loopback test that also recognises the IPv4-mapped IPv6 form
/// (`::ffff:127.0.0.1`), which a dual-stack listener hands back for an
/// ordinary IPv4 loopback client and which `Ipv6Addr::is_loopback()` alone
/// reports as `false`. Missing this would silently refuse the very caller the
/// surface exists for.
pub fn ip_is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => {
            v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
    }
}

// ── Pre-auth dispatch allowlist ─────────────────────────────────────────

/// The ONE method an unauthenticated (lock-screen) WebSocket connection may
/// call. Deliberately a single constant rather than a list: widening this is
/// a security decision that should require editing this line and re-reading
/// this module's header, not appending to a growing array.
pub const PRE_AUTH_ALLOWED_METHOD: &str = "device.power_local";

/// Exact equality — never `starts_with`/`contains` (coding convention #2): an
/// unanchored prefix test would also admit a hypothetical
/// `device.power_local_and_wipe`.
pub fn is_pre_auth_allowlisted(method: &str) -> bool {
    method == PRE_AUTH_ALLOWED_METHOD
}

// ── The action itself ───────────────────────────────────────────────────

/// The closed two-value action set the lock screen's power menu offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerAction {
    Reboot,
    Shutdown,
}

impl PowerAction {
    /// Wire spelling — also what lands in the audit row.
    pub fn as_str(self) -> &'static str {
        match self {
            PowerAction::Reboot => "reboot",
            PowerAction::Shutdown => "shutdown",
        }
    }
}

/// Parse the `action` param. **Exact** match on the two contract values — no
/// trimming, no case folding, no aliases (`restart`, the spelling the
/// admin-only `device.power` RPC uses, is deliberately NOT accepted here).
/// A closed enum that quietly accepts near-misses is not a closed enum.
pub fn parse_action(raw: &str) -> Option<PowerAction> {
    match raw {
        "reboot" => Some(PowerAction::Reboot),
        "shutdown" => Some(PowerAction::Shutdown),
        _ => None,
    }
}

/// Why a `device.power_local` call was refused. Each variant maps to one
/// stable machine-readable `code` plus end-user zh-TW copy that names no
/// method, module, env var or other internal term.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerLocalDenial {
    /// Not running on the appliance image.
    NotAppliance,
    /// The call did not come from the machine itself.
    NotLocal,
    /// `action` was absent, or not one of the two contract values.
    UnknownAction,
    /// Too many power requests in the recent window.
    RateLimited,
}

impl PowerLocalDenial {
    pub fn code(self) -> &'static str {
        match self {
            // Same code the rest of the `device.*` family already returns
            // off-appliance, so a client branches on one string, not two.
            PowerLocalDenial::NotAppliance => "not_appliance",
            PowerLocalDenial::NotLocal => "not_local",
            PowerLocalDenial::UnknownAction => "invalid_action",
            PowerLocalDenial::RateLimited => "rate_limited",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            PowerLocalDenial::NotAppliance => "這台機器不是 DuDuClaw 值班機，沒有電源控制功能。",
            PowerLocalDenial::NotLocal => "電源操作只能在值班機本機的畫面上進行。",
            PowerLocalDenial::UnknownAction => "電源動作只支援「重新啟動」與「關機」兩種。",
            PowerLocalDenial::RateLimited => "剛剛已經送出電源指令，請稍候再試一次。",
        }
    }

    /// Short stable label for the local log/audit row.
    pub fn as_label(self) -> &'static str {
        match self {
            PowerLocalDenial::NotAppliance => "not_appliance",
            PowerLocalDenial::NotLocal => "off_loopback",
            PowerLocalDenial::UnknownAction => "unknown_action",
            PowerLocalDenial::RateLimited => "rate_limited",
        }
    }

    /// Should a refusal of this kind be written to the security audit log?
    ///
    /// Only refusals from a caller that already cleared the appliance AND
    /// loopback fences are recorded. Auditing the other two would let any
    /// unauthenticated LAN peer grow `security_audit.jsonl` without bound by
    /// spamming a method it can never reach — a log-flood vector traded for
    /// no information (the `warn!` tracing line already says it happened).
    pub fn is_auditable(self) -> bool {
        match self {
            PowerLocalDenial::NotAppliance | PowerLocalDenial::NotLocal => false,
            PowerLocalDenial::UnknownAction | PowerLocalDenial::RateLimited => true,
        }
    }
}

/// The whole non-rate-limit gate as one pure function — every input is a
/// plain value, so the full matrix is unit-testable without touching the
/// process-global `DUDUCLAW_APPLIANCE` env var or opening a socket.
///
/// Ordering is by blast radius, matching `local_session::evaluate`'s
/// convention: the fence that says "this surface does not exist here" first,
/// then "you are not standing at the machine", then the payload check.
pub fn evaluate(
    is_appliance: bool,
    peer_is_loopback: bool,
    action_raw: &str,
) -> Result<PowerAction, PowerLocalDenial> {
    if !is_appliance {
        return Err(PowerLocalDenial::NotAppliance);
    }
    if !peer_is_loopback {
        return Err(PowerLocalDenial::NotLocal);
    }
    parse_action(action_raw).ok_or(PowerLocalDenial::UnknownAction)
}

// ── Rate limit ──────────────────────────────────────────────────────────

/// Accepted power requests per [`POWER_LOCAL_WINDOW`] per source. Three is
/// "one press, plus room for a double-click and one honest retry" — a power
/// button has no legitimate high-frequency use.
pub const POWER_LOCAL_MAX_PER_WINDOW: u32 = 3;
/// Window the budget above is measured over.
pub const POWER_LOCAL_WINDOW: Duration = Duration::from_secs(60);

/// Process-wide limiter for the production path. Reuses the shared
/// `duduclaw_security::rate_limiter::RateLimiter` rather than hand-rolling a
/// fourth sliding window in this crate. (The MCP `OpType` buckets named in the
/// work order live in `duduclaw-cli`, which the gateway does not depend on —
/// see this constant's commit message; this is the equivalent mechanism that
/// IS reachable from here.)
static POWER_LIMITER: LazyLock<RateLimiter> =
    LazyLock::new(|| RateLimiter::new(POWER_LOCAL_MAX_PER_WINDOW, POWER_LOCAL_WINDOW));

/// The production limiter. Tests build their own `RateLimiter` and call
/// [`check_rate_limit`] with it, so they never race this static.
pub fn power_limiter() -> &'static RateLimiter {
    &POWER_LIMITER
}

/// Limiter key: the peer address. Per-source rather than global so one
/// misbehaving caller cannot lock the real operator out of their own power
/// button.
pub fn rate_limit_key(conn: &RpcConnInfo) -> String {
    format!("power_local:{}", conn.peer_label())
}

/// Atomic check-and-record. `Ok(())` means the request may proceed.
pub async fn check_rate_limit(
    limiter: &RateLimiter,
    conn: &RpcConnInfo,
) -> Result<(), PowerLocalDenial> {
    if limiter.check_and_record(&rate_limit_key(conn)).await {
        Ok(())
    } else {
        Err(PowerLocalDenial::RateLimited)
    }
}

// ── Execution ───────────────────────────────────────────────────────────

/// Map an accepted [`PowerAction`] onto the EXISTING `DeviceOps` verbs — the
/// same `reboot()` / `poweroff()` the admin-only `device.power` RPC already
/// drives, so on the appliance image both routes end at the privilege-
/// separated `duduclaw-sysd` daemon and there is exactly one argv per verb in
/// the whole codebase.
///
/// Takes `&dyn DeviceOps` rather than calling `select_device_ops()` itself so
/// tests can drive the accepted path through `device_ops::mock::MockDeviceOps`
/// without rebooting the developer's machine.
pub async fn run_power_action(ops: &dyn DeviceOps, action: PowerAction) -> OpResult {
    match action {
        PowerAction::Reboot => ops.reboot().await,
        PowerAction::Shutdown => ops.poweroff().await,
    }
}

// ── Audit ───────────────────────────────────────────────────────────────

/// Audit event type for an accepted, about-to-execute power request.
pub const AUDIT_EVENT_ACCEPTED: &str = "device_power_local";
/// Audit event type for a refused power request (only the auditable kinds —
/// see [`PowerLocalDenial::is_auditable`]).
pub const AUDIT_EVENT_DENIED: &str = "device_power_local_denied";

/// `agent_id` column value for these rows. This surface has no agent and no
/// authenticated user by construction, and inventing one would make the audit
/// trail lie; the lock screen is named for what it is.
const AUDIT_ACTOR: &str = "lockscreen";

/// Longest slice of a caller-supplied `action` string kept in an audit row.
/// Char-based (`duduclaw_core::truncate_chars`) — never a raw byte slice,
/// which panics mid-codepoint on CJK/emoji input (coding convention #1).
const AUDIT_RAW_ACTION_MAX_CHARS: usize = 32;

/// Record an accepted power request **before** it executes — a reboot kills
/// the process that would otherwise write the row afterwards, so "log then
/// act" is the only ordering that leaves evidence.
pub fn audit_accepted(home_dir: &Path, action: PowerAction, conn: &RpcConnInfo) {
    append_audit_event(
        home_dir,
        &AuditEvent::new(
            AUDIT_EVENT_ACCEPTED,
            AUDIT_ACTOR,
            Severity::Warning,
            serde_json::json!({
                "action": action.as_str(),
                "source": conn.peer_label(),
                "pre_auth": conn.pre_auth,
            }),
        ),
    );
}

/// Record a refused power request from a caller that already cleared the
/// appliance + loopback fences.
pub fn audit_denied(
    home_dir: &Path,
    raw_action: &str,
    denial: PowerLocalDenial,
    conn: &RpcConnInfo,
) {
    if !denial.is_auditable() {
        return;
    }
    append_audit_event(
        home_dir,
        &AuditEvent::new(
            AUDIT_EVENT_DENIED,
            AUDIT_ACTOR,
            Severity::Info,
            serde_json::json!({
                "reason": denial.as_label(),
                "action_raw": duduclaw_core::truncate_chars(raw_action, AUDIT_RAW_ACTION_MAX_CHARS),
                "source": conn.peer_label(),
                "pre_auth": conn.pre_auth,
            }),
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device_ops::mock::MockDeviceOps;
    use crate::device_ops::{DeviceOpError, OpOutput};
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn sock(ip: IpAddr) -> SocketAddr {
        SocketAddr::new(ip, 51234)
    }

    // ── loopback detection ───────────────────────────────────────────

    #[test]
    fn loopback_covers_v4_v6_and_the_v4_mapped_form() {
        assert!(ip_is_loopback(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))));
        assert!(ip_is_loopback(IpAddr::V4(Ipv4Addr::new(127, 9, 9, 9))));
        assert!(ip_is_loopback(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        // ::ffff:127.0.0.1 — what a dual-stack listener reports for an
        // ordinary IPv4 loopback client.
        assert!(ip_is_loopback(IpAddr::V6("::ffff:127.0.0.1".parse().unwrap())));
    }

    #[test]
    fn loopback_rejects_lan_and_public_addresses() {
        assert!(!ip_is_loopback(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10))));
        assert!(!ip_is_loopback(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2))));
        assert!(!ip_is_loopback(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        assert!(!ip_is_loopback(IpAddr::V6("fe80::1".parse().unwrap())));
        // The mapped form of a NON-loopback v4 address must stay refused.
        assert!(!ip_is_loopback(IpAddr::V6("::ffff:192.168.1.10".parse().unwrap())));
    }

    #[test]
    fn in_process_conn_is_not_loopback_fail_closed() {
        assert!(!RpcConnInfo::internal().peer_is_loopback());
        assert!(!RpcConnInfo::default().peer_is_loopback());
        assert!(!RpcConnInfo::internal().pre_auth);
        assert_eq!(RpcConnInfo::internal().peer_label(), "in-process");
    }

    #[test]
    fn ws_conn_reports_its_peer() {
        let c = RpcConnInfo::from_ws(sock(IpAddr::V4(Ipv4Addr::LOCALHOST)), true);
        assert!(c.peer_is_loopback());
        assert!(c.pre_auth);
        assert_eq!(c.peer_label(), "127.0.0.1");
    }

    // ── pre-auth allowlist ───────────────────────────────────────────

    #[test]
    fn only_the_power_method_is_pre_auth_reachable() {
        assert!(is_pre_auth_allowlisted("device.power_local"));
        assert_eq!(PRE_AUTH_ALLOWED_METHOD, "device.power_local");
    }

    /// The direction that actually matters: nothing else leaks through —
    /// including the admin-only `device.power` twin, the self-service
    /// methods the *password-change* allowlist opens, and ordinary
    /// daily-driver RPCs.
    #[test]
    fn everything_else_is_blocked_pre_auth() {
        for method in [
            "device.power",
            "device.factory_reset",
            "device.status",
            "device.backup_create",
            "users.me",
            "users.change_password",
            "users.list",
            "agents.list",
            "system.status",
            "tasks.list",
            "connect",
            "ping",
            // Unanchored prefix/substring matching would let these in.
            "device.power_local_and_wipe",
            "device.power_local.extra",
            "xdevice.power_local",
            " device.power_local",
            "device.power_local ",
            "DEVICE.POWER_LOCAL",
        ] {
            assert!(
                !is_pre_auth_allowlisted(method),
                "{method} must NOT be reachable without logging in"
            );
        }
    }

    // ── action parsing ───────────────────────────────────────────────

    #[test]
    fn parses_exactly_the_two_contract_values() {
        assert_eq!(parse_action("reboot"), Some(PowerAction::Reboot));
        assert_eq!(parse_action("shutdown"), Some(PowerAction::Shutdown));
        assert_eq!(PowerAction::Reboot.as_str(), "reboot");
        assert_eq!(PowerAction::Shutdown.as_str(), "shutdown");
    }

    #[test]
    fn rejects_everything_else_including_near_misses() {
        for raw in [
            "",
            " ",
            "Reboot",
            "REBOOT",
            " reboot",
            "reboot ",
            "reboot\n",
            // `device.power`'s spelling — a different RPC's vocabulary is
            // not this one's.
            "restart",
            "poweroff",
            "halt",
            "factory_reset",
            "重新啟動",
            "reboot;shutdown",
        ] {
            assert_eq!(parse_action(raw), None, "{raw:?} must not parse");
        }
    }

    // ── the gate matrix (pure) ───────────────────────────────────────

    #[test]
    fn accepted_only_when_appliance_and_loopback_and_known_action() {
        assert_eq!(evaluate(true, true, "reboot"), Ok(PowerAction::Reboot));
        assert_eq!(evaluate(true, true, "shutdown"), Ok(PowerAction::Shutdown));
    }

    #[test]
    fn non_appliance_is_refused_even_when_everything_else_is_perfect() {
        assert_eq!(evaluate(false, true, "reboot"), Err(PowerLocalDenial::NotAppliance));
        assert_eq!(evaluate(false, true, "shutdown"), Err(PowerLocalDenial::NotAppliance));
    }

    #[test]
    fn non_loopback_is_refused_on_a_real_appliance() {
        assert_eq!(evaluate(true, false, "reboot"), Err(PowerLocalDenial::NotLocal));
        assert_eq!(evaluate(true, false, "shutdown"), Err(PowerLocalDenial::NotLocal));
    }

    #[test]
    fn unknown_action_is_refused_even_from_a_local_appliance_caller() {
        for raw in ["", "restart", "Reboot", "wipe"] {
            assert_eq!(
                evaluate(true, true, raw),
                Err(PowerLocalDenial::UnknownAction),
                "{raw:?}"
            );
        }
    }

    /// Blast-radius ordering: the widest fence answers first, so a remote
    /// caller on a non-appliance never learns which of the later conditions
    /// it would also have failed.
    #[test]
    fn gate_order_is_appliance_then_loopback_then_action() {
        assert_eq!(evaluate(false, false, "nonsense"), Err(PowerLocalDenial::NotAppliance));
        assert_eq!(evaluate(true, false, "nonsense"), Err(PowerLocalDenial::NotLocal));
        assert_eq!(evaluate(true, true, "nonsense"), Err(PowerLocalDenial::UnknownAction));
    }

    // ── denial vocabulary ────────────────────────────────────────────

    #[test]
    fn denial_codes_are_stable_and_distinct() {
        let all = [
            PowerLocalDenial::NotAppliance,
            PowerLocalDenial::NotLocal,
            PowerLocalDenial::UnknownAction,
            PowerLocalDenial::RateLimited,
        ];
        let codes: Vec<_> = all.iter().map(|d| d.code()).collect();
        assert_eq!(codes, ["not_appliance", "not_local", "invalid_action", "rate_limited"]);
        let mut sorted = codes.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), codes.len(), "codes must be distinct");
    }

    #[test]
    fn denial_messages_are_end_user_copy_with_no_internal_terms() {
        for d in [
            PowerLocalDenial::NotAppliance,
            PowerLocalDenial::NotLocal,
            PowerLocalDenial::UnknownAction,
            PowerLocalDenial::RateLimited,
        ] {
            let msg = d.message();
            assert!(!msg.is_empty());
            for leak in [
                "device.power_local",
                "loopback",
                "RPC",
                "dispatch",
                "DUDUCLAW_APPLIANCE",
                "appliance",
                "sysd",
                "systemctl",
                "WsFrame",
            ] {
                assert!(!msg.contains(leak), "internal term {leak:?} leaked into: {msg}");
            }
        }
    }

    #[test]
    fn only_post_fence_denials_are_audited() {
        assert!(!PowerLocalDenial::NotAppliance.is_auditable());
        assert!(!PowerLocalDenial::NotLocal.is_auditable());
        assert!(PowerLocalDenial::UnknownAction.is_auditable());
        assert!(PowerLocalDenial::RateLimited.is_auditable());
    }

    // ── rate limit ───────────────────────────────────────────────────

    #[tokio::test]
    async fn rate_limit_allows_the_budget_then_refuses() {
        let limiter = RateLimiter::new(POWER_LOCAL_MAX_PER_WINDOW, POWER_LOCAL_WINDOW);
        let conn = RpcConnInfo::from_ws(sock(IpAddr::V4(Ipv4Addr::LOCALHOST)), true);
        for i in 0..POWER_LOCAL_MAX_PER_WINDOW {
            assert_eq!(check_rate_limit(&limiter, &conn).await, Ok(()), "request {i}");
        }
        assert_eq!(
            check_rate_limit(&limiter, &conn).await,
            Err(PowerLocalDenial::RateLimited)
        );
    }

    #[tokio::test]
    async fn rate_limit_is_per_source_so_one_peer_cannot_starve_another() {
        let limiter = RateLimiter::new(1, POWER_LOCAL_WINDOW);
        let a = RpcConnInfo::from_ws(sock(IpAddr::V4(Ipv4Addr::LOCALHOST)), true);
        let b = RpcConnInfo::from_ws(sock(IpAddr::V6(Ipv6Addr::LOCALHOST)), true);
        assert_eq!(check_rate_limit(&limiter, &a).await, Ok(()));
        assert_eq!(check_rate_limit(&limiter, &b).await, Ok(()));
        assert_eq!(check_rate_limit(&limiter, &a).await, Err(PowerLocalDenial::RateLimited));
    }

    // ── execution reaches the real device_ops verbs (mocked) ─────────

    #[tokio::test]
    async fn reboot_calls_device_ops_reboot_and_nothing_else() {
        let ops = MockDeviceOps::default();
        *ops.reboot_result.lock().unwrap() = Some(Ok(OpOutput {
            success: true,
            stdout: "ok".into(),
            stderr: String::new(),
        }));
        let out = run_power_action(&ops, PowerAction::Reboot).await.unwrap();
        assert!(out.success);
        assert_eq!(*ops.calls.lock().unwrap(), vec!["reboot".to_string()]);
    }

    #[tokio::test]
    async fn shutdown_calls_device_ops_poweroff_and_nothing_else() {
        let ops = MockDeviceOps::default();
        *ops.poweroff_result.lock().unwrap() = Some(Ok(OpOutput {
            success: true,
            stdout: String::new(),
            stderr: String::new(),
        }));
        let out = run_power_action(&ops, PowerAction::Shutdown).await.unwrap();
        assert!(out.success);
        assert_eq!(*ops.calls.lock().unwrap(), vec!["poweroff".to_string()]);
    }

    /// A failing device layer surfaces as an error — never silently as
    /// "success" (the mock's own default is `Unsupported`, so a forgotten
    /// arm fails loudly rather than passing).
    #[tokio::test]
    async fn device_layer_failure_is_propagated_not_swallowed() {
        let ops = MockDeviceOps::default();
        let result = run_power_action(&ops, PowerAction::Reboot).await;
        assert!(matches!(result, Err(DeviceOpError::Unsupported(_))), "{result:?}");
    }

    // ── audit rows ───────────────────────────────────────────────────

    fn audit_lines(home: &Path) -> Vec<serde_json::Value> {
        let path = home.join("security_audit.jsonl");
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    #[test]
    fn accepted_row_records_action_source_and_time() {
        let home = tempfile::tempdir().unwrap();
        let conn = RpcConnInfo::from_ws(sock(IpAddr::V4(Ipv4Addr::LOCALHOST)), true);
        audit_accepted(home.path(), PowerAction::Shutdown, &conn);

        let rows = audit_lines(home.path());
        assert_eq!(rows.len(), 1, "{rows:?}");
        let row = &rows[0];
        assert_eq!(row["event_type"], AUDIT_EVENT_ACCEPTED);
        assert_eq!(row["agent_id"], AUDIT_ACTOR);
        assert_eq!(row["severity"], "warning");
        assert_eq!(row["details"]["action"], "shutdown");
        assert_eq!(row["details"]["source"], "127.0.0.1");
        assert_eq!(row["details"]["pre_auth"], true);
        assert!(
            chrono::DateTime::parse_from_rfc3339(row["timestamp"].as_str().unwrap()).is_ok(),
            "timestamp must be RFC3339: {row:?}"
        );
    }

    #[test]
    fn denied_row_is_written_only_for_post_fence_refusals() {
        let home = tempfile::tempdir().unwrap();
        let conn = RpcConnInfo::from_ws(sock(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 9))), true);

        // Pre-fence refusals must leave no row (log-flood defence).
        audit_denied(home.path(), "reboot", PowerLocalDenial::NotAppliance, &conn);
        audit_denied(home.path(), "reboot", PowerLocalDenial::NotLocal, &conn);
        assert!(audit_lines(home.path()).is_empty());

        audit_denied(home.path(), "wipe", PowerLocalDenial::UnknownAction, &conn);
        let rows = audit_lines(home.path());
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0]["event_type"], AUDIT_EVENT_DENIED);
        assert_eq!(rows[0]["details"]["reason"], "unknown_action");
        assert_eq!(rows[0]["details"]["action_raw"], "wipe");
    }

    /// A caller-supplied `action` is untrusted text — it must be capped by
    /// CHARACTER count (never a byte slice, which would panic mid-codepoint
    /// on the CJK input below).
    #[test]
    fn denied_row_caps_caller_supplied_action_without_panicking_on_cjk() {
        let home = tempfile::tempdir().unwrap();
        let conn = RpcConnInfo::from_ws(sock(IpAddr::V4(Ipv4Addr::LOCALHOST)), false);
        let hostile = "關機關機關機關機關機關機關機關機關機關機關機關機關機關機關機關機關機";
        audit_denied(home.path(), hostile, PowerLocalDenial::UnknownAction, &conn);

        let rows = audit_lines(home.path());
        assert_eq!(rows.len(), 1);
        let recorded = rows[0]["details"]["action_raw"].as_str().unwrap();
        assert!(
            recorded.chars().count() <= AUDIT_RAW_ACTION_MAX_CHARS,
            "capped to {AUDIT_RAW_ACTION_MAX_CHARS} chars, got {}",
            recorded.chars().count()
        );
        assert!(hostile.starts_with(recorded));
    }
}
