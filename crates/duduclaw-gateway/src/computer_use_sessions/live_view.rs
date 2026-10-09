//! Live view and takeover for tool-driven sessions (P8): the state a
//! session carries for dashboard viewers, the container access the viewer
//! path needs, who may watch or take over, and one-time viewer tickets.
//!
//! ## Transport
//!
//! `x11vnc` runs inside the session container, started on demand by the
//! gateway (`docker exec … duduclaw-vnc start <viewonly|control>`, a fresh
//! random password on stdin) and listening **only on a unix socket in the
//! root-only `/tmp/duduclaw-root`** (`-rfbport 0 -rfbportv6 -1 -unixsock`,
//! plus `-localhost`). `-rfbport` stays 0: Debian's libvncserver 0.9.15
//! still binds `::1:5900` unless `-rfbportv6 -1` is set, and `-rfbport -1`
//! makes x11vnc delete the unix socket. Neither the host network nor the
//! browser's unprivileged `sandbox` user can reach it. No port is published: a viewer's bytes go
//! dashboard → the gateway's authenticated WebSocket
//! (`/ws/computer-view`, [`super::view_ws`]) → `docker exec -i … duduclaw-vnc-relay`
//! (a unix-socket ↔ stdio pipe) → x11vnc. The rejected alternative, a port
//! published on 127.0.0.1 with the gateway relaying, would still let any
//! process of any user on the host connect to the VNC server.
//!
//! ## Rules
//!
//! - Watching: an Admin, or a dashboard account bound to the employee at
//!   Operator level or above, or a Manager bound at any level
//!   ([`authorize`]); the role and bindings are re-read from `users.db` on
//!   every request and every viewer connection.
//! - Taking over: an Admin, or a Manager bound at Operator level or above.
//!   One lease per session; while it is active the employee's actions are
//!   refused (`human_has_control`) and only the holder's input reaches the
//!   display. The lease expires after `takeover_idle_minutes` without viewer
//!   input, or on hand back; either way a handoff note tells the employee
//!   that a human acted.
//! - Viewer input is filtered by the gateway ([`super::rfb`]) on top of
//!   x11vnc's own `-viewonly`: a connection that does not hold the lease
//!   never gets key, pointer or clipboard events through.

use std::collections::HashMap;
use std::sync::atomic::AtomicU32;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use duduclaw_auth::{AccessLevel, UserContext, UserRole};

use crate::computer_use::ComputerUseError;
use crate::computer_use_orchestrator::docker_output_with_stdin;

/// Viewer connections per session at most.
pub const MAX_VIEWERS_PER_SESSION: u32 = 4;
/// How long a viewer ticket is valid.
pub const TICKET_TTL: Duration = Duration::from_secs(30);
/// Upper bound on starting or stopping the in-container VNC server.
const VNC_EXEC_TIMEOUT: Duration = Duration::from_secs(20);

/// The VNC server's input mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VncMode {
    /// `-viewonly`: the server ignores all viewer input.
    ViewOnly,
    /// Input accepted (the gateway still forwards only the lease holder's).
    Control,
}

impl VncMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ViewOnly => "viewonly",
            Self::Control => "control",
        }
    }
}

/// A byte stream to the in-container VNC server.
pub trait RelayStream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> RelayStream for T {}

/// What the viewer path needs from a session's container, reachable without
/// the session lock (a viewer must not wait behind a long confirmation).
#[async_trait]
pub(crate) trait LiveViewAccess: Send + Sync {
    /// (Re)start the VNC server in `mode` with `password` (8 characters of
    /// `[A-Za-z0-9]`). Existing viewer connections drop.
    async fn start_vnc(&self, mode: VncMode, password: &str) -> Result<(), ComputerUseError>;
    /// Stop the VNC server (best effort).
    async fn stop_vnc(&self);
    /// One new connection to the VNC server.
    async fn open_relay(&self) -> Result<Box<dyn RelayStream>, ComputerUseError>;
}

/// Production [`LiveViewAccess`]: `docker exec` into the container.
pub(crate) struct DockerLiveView {
    pub(crate) container: String,
}

/// `docker exec -i … duduclaw-vnc-relay`'s stdin + stdout as one stream; the
/// child is killed when this is dropped.
struct ChildIo {
    _child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    stdout: tokio::process::ChildStdout,
}

impl tokio::io::AsyncRead for ChildIo {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.stdout).poll_read(cx, buf)
    }
}

impl tokio::io::AsyncWrite for ChildIo {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.stdin).poll_write(cx, buf)
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.stdin).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.stdin).poll_shutdown(cx)
    }
}

/// The `docker exec` argv that starts the VNC server (password on stdin,
/// never in argv). Pure, for tests.
pub(crate) fn vnc_start_args(container: &str, mode: VncMode) -> Vec<String> {
    ["exec", "-i", container, "duduclaw-vnc", "start", mode.as_str()]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// The `docker exec` argv of one relay connection. Pure, for tests.
pub(crate) fn relay_args(container: &str) -> Vec<String> {
    ["exec", "-i", container, "duduclaw-vnc-relay"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

#[async_trait]
impl LiveViewAccess for DockerLiveView {
    async fn start_vnc(&self, mode: VncMode, password: &str) -> Result<(), ComputerUseError> {
        if !valid_password(password) {
            return Err(ComputerUseError::ApiError("invalid VNC password".into()));
        }
        let args = vnc_start_args(&self.container, mode);
        let out = docker_output_with_stdin(
            &args,
            format!("{password}\n").as_bytes(),
            VNC_EXEC_TIMEOUT,
            "VNC start",
        )
        .await?;
        if !out.status.success() {
            return Err(ComputerUseError::ApiError("VNC server did not start".into()));
        }
        Ok(())
    }

    async fn stop_vnc(&self) {
        let _ = crate::computer_use_orchestrator::docker_output(
            &["exec", self.container.as_str(), "duduclaw-vnc", "stop"],
            VNC_EXEC_TIMEOUT,
            "VNC stop",
        )
        .await;
    }

    async fn open_relay(&self) -> Result<Box<dyn RelayStream>, ComputerUseError> {
        let mut child = tokio::process::Command::new("docker")
            .args(relay_args(&self.container))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| ComputerUseError::ApiError(format!("relay spawn failed: {e}")))?;
        let stdin = child.stdin.take().ok_or_else(|| ComputerUseError::ApiError("relay stdin".into()))?;
        let stdout = child.stdout.take().ok_or_else(|| ComputerUseError::ApiError("relay stdout".into()))?;
        Ok(Box::new(ChildIo { _child: child, stdin, stdout }))
    }
}

/// A random per-start VNC password: 8 ASCII alphanumerics (RFB VNC
/// authentication uses at most 8 characters).
pub(crate) fn new_password() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
    let bytes = uuid::Uuid::new_v4().into_bytes();
    bytes
        .iter()
        .take(8)
        .map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char)
        .collect()
}

pub(crate) fn valid_password(p: &str) -> bool {
    p.len() == 8 && p.bytes().all(|b| b.is_ascii_alphanumeric())
}

// ---------------------------------------------------------------------------
// Per-session state
// ---------------------------------------------------------------------------

/// A human's takeover of one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TakeoverLease {
    /// `users.db` user id (or `system` for the admin token).
    pub holder_id: String,
    /// Email or id, for audit rows and the status card (≤64 chars).
    pub holder_label: String,
    pub since: Instant,
    pub last_input: Instant,
    pub idle_limit: Duration,
    /// Viewer input messages forwarded while held.
    pub inputs: u64,
}

impl TakeoverLease {
    pub fn new(holder_id: &str, holder_label: &str, now: Instant, idle_limit: Duration) -> Self {
        Self {
            holder_id: holder_id.to_string(),
            holder_label: duduclaw_core::truncate_chars(holder_label, 64),
            since: now,
            last_input: now,
            idle_limit,
            inputs: 0,
        }
    }

    /// Whether the lease still holds at `now`.
    pub fn active(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.last_input) < self.idle_limit
    }

    pub fn idle_left(&self, now: Instant) -> Duration {
        self.idle_limit
            .saturating_sub(now.saturating_duration_since(self.last_input))
    }
}

/// Why the employee's actions are held until a human resumes the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hold {
    /// `input_guard` rule categories that matched (names only, never text).
    pub categories: Vec<String>,
    pub since_unix: i64,
}

/// What a session's container VNC server is running as.
#[derive(Default)]
pub(crate) struct VncServer {
    pub(crate) mode: Option<VncMode>,
    pub(crate) password: String,
}

/// Facts fixed at insert, readable without the session lock.
#[derive(Debug, Clone)]
pub(crate) struct SessionMeta {
    pub(crate) max_actions: u32,
    pub(crate) deadline: Instant,
    pub(crate) keep_alive: Duration,
    pub(crate) takeover_idle: Duration,
}

/// Live-view state of one session (inside [`super::SessionShared`]).
#[derive(Default)]
pub(crate) struct LiveState {
    pub(crate) takeover: Mutex<Option<TakeoverLease>>,
    pub(crate) hold: Mutex<Option<Hold>>,
    pub(crate) viewers: AtomicU32,
    /// When the container was paused (keep-alive), if it is.
    pub(crate) frozen_since: Mutex<Option<Instant>>,
    /// Mirror of `ManagedSession::actions_used` for the status card.
    pub(crate) actions_used: AtomicU32,
    pub(crate) vnc: tokio::sync::Mutex<VncServer>,
    pub(crate) access: Mutex<Option<Arc<dyn LiveViewAccess>>>,
    pub(crate) meta: OnceLock<SessionMeta>,
}

impl LiveState {
    /// The active lease at `now`, if any (an expired one counts as none).
    pub(crate) fn active_takeover(&self, now: Instant) -> Option<TakeoverLease> {
        self.takeover
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .filter(|l| l.active(now))
            .cloned()
    }

    pub(crate) fn hold(&self) -> Option<Hold> {
        self.hold.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub(crate) fn frozen_since(&self) -> Option<Instant> {
        *self.frozen_since.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn set_frozen(&self, since: Option<Instant>) {
        *self.frozen_since.lock().unwrap_or_else(|p| p.into_inner()) = since;
    }

    pub(crate) fn access(&self) -> Option<Arc<dyn LiveViewAccess>> {
        self.access.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Record viewer input for `user_id`: `true` (and the lease's idle clock
    /// restarts) only when that user holds an active lease.
    pub(crate) fn note_input(&self, user_id: &str, now: Instant) -> bool {
        let mut slot = self.takeover.lock().unwrap_or_else(|p| p.into_inner());
        match slot.as_mut() {
            Some(lease) if lease.holder_id == user_id && lease.active(now) => {
                lease.last_input = now;
                lease.inputs += 1;
                true
            }
            _ => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Authorization
// ---------------------------------------------------------------------------

/// What a dashboard account asks to do with an employee's session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveAction {
    /// Status, watch, stop.
    Watch,
    /// Take over, hand back, resume after an injection pause.
    Control,
}

/// Pure decision over an already re-read identity. Admin: everything.
/// Watch: a Manager bound to the employee, or anyone bound at Operator level
/// or above. Control: a Manager bound at Operator level or above.
pub fn authorize(live: &UserContext, agent_id: &str, action: LiveAction) -> bool {
    if live.is_admin() {
        return true;
    }
    if live.requires_password_change() {
        return false;
    }
    let operator = live.has_agent_access(agent_id, AccessLevel::Operator);
    match action {
        LiveAction::Watch => {
            operator || (live.role == UserRole::Manager && live.can_access_agent(agent_id))
        }
        LiveAction::Control => live.role == UserRole::Manager && operator,
    }
}

/// The label an operator is recorded under (email, else id; ≤64 chars).
pub fn operator_label(ctx: &UserContext) -> String {
    let raw = if ctx.email.is_empty() { &ctx.user_id } else { &ctx.email };
    duduclaw_core::truncate_chars(raw, 64)
}

// ---------------------------------------------------------------------------
// Viewer tickets
// ---------------------------------------------------------------------------

/// A one-time permission to open one viewer connection.
#[derive(Debug, Clone)]
pub struct ViewTicket {
    pub agent_id: String,
    pub session_id: String,
    /// The identity the ticket was issued to; re-read again on connect.
    pub ctx: UserContext,
    pub expires: Instant,
}

/// Single-use, short-lived tickets (a leaf lock, never held across await).
#[derive(Default)]
pub(crate) struct TicketBook {
    tickets: Mutex<HashMap<String, ViewTicket>>,
}

impl TicketBook {
    /// Issue a ticket (32 hex). Expired tickets are dropped first; at most
    /// 256 are kept (the oldest goes).
    pub(crate) fn issue(&self, ticket: ViewTicket, now: Instant) -> String {
        let token = uuid::Uuid::new_v4().as_simple().to_string();
        let mut book = self.tickets.lock().unwrap_or_else(|p| p.into_inner());
        book.retain(|_, t| t.expires > now);
        if book.len() >= 256
            && let Some(oldest) = book
                .iter()
                .min_by_key(|(_, t)| t.expires)
                .map(|(k, _)| k.clone())
        {
            book.remove(&oldest);
        }
        book.insert(token.clone(), ticket);
        token
    }

    /// Take a ticket (whatever happens next, it is gone). `None` when
    /// unknown, malformed or expired.
    pub(crate) fn consume(&self, token: &str, now: Instant) -> Option<ViewTicket> {
        if token.len() != 32 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let ticket = self
            .tickets
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(token)?;
        (ticket.expires > now).then_some(ticket)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(role: UserRole, access: &[(&str, AccessLevel)]) -> UserContext {
        UserContext {
            user_id: "u1".into(),
            email: "u1@example.com".into(),
            role,
            agent_access: access.iter().map(|(a, l)| (a.to_string(), *l)).collect(),
            must_change_password: false,
        }
    }

    #[test]
    fn authorization_matrix() {
        use AccessLevel::*;
        use LiveAction::*;
        use UserRole::*;
        let admin = ctx(Admin, &[]);
        assert!(authorize(&admin, "alice", Watch) && authorize(&admin, "alice", Control));
        let mgr_op = ctx(Manager, &[("alice", Operator)]);
        assert!(authorize(&mgr_op, "alice", Watch) && authorize(&mgr_op, "alice", Control));
        assert!(!authorize(&mgr_op, "bob", Watch), "bound to alice only");
        let mgr_viewer = ctx(Manager, &[("alice", Viewer)]);
        assert!(authorize(&mgr_viewer, "alice", Watch));
        assert!(!authorize(&mgr_viewer, "alice", Control));
        let emp_owner = ctx(Employee, &[("alice", Owner)]);
        assert!(authorize(&emp_owner, "alice", Watch));
        assert!(!authorize(&emp_owner, "alice", Control), "takeover needs a Manager");
        let emp_viewer = ctx(Employee, &[("alice", Viewer)]);
        assert!(!authorize(&emp_viewer, "alice", Watch));
        let unbound_mgr = ctx(Manager, &[]);
        assert!(!authorize(&unbound_mgr, "alice", Watch));
        let mut must_change = mgr_op.clone();
        must_change.must_change_password = true;
        assert!(!authorize(&must_change, "alice", Watch));
    }

    #[test]
    fn lease_expires_without_input_and_only_the_holder_counts() {
        let now = Instant::now();
        let state = LiveState::default();
        *state.takeover.lock().unwrap() =
            Some(TakeoverLease::new("u1", "u1@example.com", now, Duration::from_secs(600)));
        assert!(state.active_takeover(now + Duration::from_secs(599)).is_some());
        assert!(!state.note_input("u2", now + Duration::from_secs(10)), "not the holder");
        assert!(state.note_input("u1", now + Duration::from_secs(500)));
        // The idle clock restarted at +500 s.
        assert!(state.active_takeover(now + Duration::from_secs(1000)).is_some());
        assert!(state.active_takeover(now + Duration::from_secs(1100)).is_none());
        assert!(!state.note_input("u1", now + Duration::from_secs(1100)), "expired lease takes no input");
        let lease = state.takeover.lock().unwrap().clone().unwrap();
        assert_eq!(lease.inputs, 1);
    }

    #[test]
    fn tickets_are_single_use_and_expire() {
        let book = TicketBook::default();
        let now = Instant::now();
        let t = ViewTicket {
            agent_id: "alice".into(),
            session_id: "cu-1".into(),
            ctx: ctx(UserRole::Admin, &[]),
            expires: now + TICKET_TTL,
        };
        let token = book.issue(t.clone(), now);
        assert!(book.consume(&token, now).is_some());
        assert!(book.consume(&token, now).is_none(), "single use");
        let token = book.issue(t, now);
        assert!(book.consume(&token, now + TICKET_TTL).is_none(), "expired");
        assert!(book.consume("../../etc", now).is_none());
    }

    #[test]
    fn passwords_are_eight_alphanumerics_and_differ() {
        let a = new_password();
        let b = new_password();
        assert!(valid_password(&a) && valid_password(&b));
        assert_ne!(a, b);
        assert!(!valid_password("short"));
        assert!(!valid_password("abc;rm -"));
    }

    #[test]
    fn exec_argv_never_carries_the_password_or_a_port() {
        let start = vnc_start_args("duduclaw-cu-0123", VncMode::ViewOnly);
        assert_eq!(start, ["exec", "-i", "duduclaw-cu-0123", "duduclaw-vnc", "start", "viewonly"]);
        assert_eq!(vnc_start_args("c", VncMode::Control).last().unwrap(), "control");
        let relay = relay_args("duduclaw-cu-0123");
        assert_eq!(relay, ["exec", "-i", "duduclaw-cu-0123", "duduclaw-vnc-relay"]);
        for a in start.iter().chain(relay.iter()) {
            assert!(!a.contains("5900") && !a.starts_with("-p"), "{a}");
        }
    }
}
