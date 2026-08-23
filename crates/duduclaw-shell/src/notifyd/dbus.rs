// D6 (2026-08-23) — the actual `org.freedesktop.Notifications` service.
// `#[cfg(target_os = "linux")]`; this is the ONLY file in `notifyd/` that
// knows zbus exists.
//
// ## Library choice: zbus, and why there was no real decision to make
//
// `zbus = "5"` is ALREADY a Linux-only dependency of this crate (added in
// Shell-S3 for `oobe/network/nm.rs`'s NetworkManager client) and is already
// in this crate's `Cargo.lock` at 5.19.0. So the honest framing is not "zbus
// vs. a minimal hand-rolled D-Bus implementation" — it is "reuse a dependency
// already compiled into this binary, or hand-roll a *server*". A hand-rolled
// client (which `nm.rs`'s header comment argues for, at the level of using
// the generic `Proxy` instead of the `#[proxy]` macro) is a different
// proposition from a hand-rolled server: a server has to do SASL EXTERNAL
// auth, `Hello`, message *dispatch*, `RequestName`, introspection, and
// correct error replies — hundreds of lines of protocol whose failure mode is
// "third-party apps silently cannot notify", which is the exact bug D6 is
// fixing. Adding zero new crates to reuse a battle-tested implementation is
// not a close call.
//
// The `#[interface]` MACRO is used here, unlike `nm.rs`'s deliberate choice of
// the generic `Proxy` over `#[proxy]`. That is not an inconsistency: `nm.rs`
// avoided codegen because a client's whole job is issuing calls whose exact
// wire shape you want visible at the call site. A server's job is *dispatch* —
// matching an incoming member name and signature to a function, replying with
// the right signature, answering `Introspect` — and that is boilerplate with
// one correct answer, not a place where hand-writing buys visibility.
//
// ## Threading (see also `notifyd/mod.rs`'s diagram)
//
// `spawn` starts ONE thread. It builds the connection (blocking: socket
// connect + SASL + `Hello` + `RequestName`) and then spends its life blocked
// in `rx.recv()`, waking only to turn a UI decision into a signal. It is NOT
// the thread that serves method calls — zbus builds its connection with
// `internal_executor: true` (the builder default, verified against the pinned
// zbus 5.19 source: `connection/builder.rs`'s `Default` impl plus
// `start_internal_executor`), which spawns zbus's own driver. So a `Notify`
// call is answered even while this thread is parked, and a wedged UI can
// never make a caller's `Notify` hang.
//
// gpui's main thread is never involved: it only ever touches `SharedInbox`
// (a mutex and a `VecDeque`) and `DaemonHandle::emit` (an `mpsc::send`).
//
// ## Signals: declared for introspection, emitted by hand
//
// `NotificationClosed`/`ActionInvoked` are declared with `#[zbus(signal)]` so
// they appear in this interface's `Introspect` XML — a client that introspects
// (or an operator running `gdbus introspect`) sees a complete, honest
// interface. They are EMITTED with `Connection::emit_signal`, which produces
// a byte-identical message, because the macro-generated emitters are `async`
// and this thread is deliberately blocking-only (see `nm.rs`'s own reasoning
// for why this crate uses zbus's blocking API rather than dragging an async
// runtime into a gpui process).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use zbus::blocking::connection::Builder;
use zbus::blocking::Connection;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedValue, Value};

use super::center::DaemonState;
use super::inbox::SharedInbox;
use super::{EmitCommand, NotifyRequest, BUS_NAME, CAPABILITIES, INTERFACE, OBJECT_PATH, SERVER_NAME, SERVER_VENDOR, SPEC_VERSION};

/// The served object. Holds nothing but the shared queue — every method is
/// `&self` (so zbus takes only a read lock on the interface) and every method
/// finishes in microseconds.
struct NotificationsService {
    inbox: SharedInbox,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl NotificationsService {
    /// `UINT32 Notify(STRING app_name, UINT32 replaces_id, STRING app_icon,
    /// STRING summary, STRING body, ARRAY actions, DICT hints,
    /// INT32 expire_timeout)` — Desktop Notifications Specification 1.2.
    ///
    /// `app_icon` is accepted and ignored: this shell renders a derived
    /// initial avatar, and `GetCapabilities` correspondingly does NOT
    /// advertise `icon-static`/`icon-multi`, so a client is never told
    /// otherwise.
    #[allow(clippy::too_many_arguments)]
    async fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        _app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        let req = NotifyRequest {
            app_name,
            replaces_id,
            summary,
            body,
            actions,
            urgency: super::urgency_from_hint(hint_u64(&hints, "urgency")),
            transient: hint_true(&hints, "transient"),
            expire_timeout,
        };
        // The ONLY clock read on this path — kept here, at the boundary, so
        // everything downstream stays deterministically testable.
        self.inbox.post(&req, Instant::now())
    }

    /// `void CloseNotification(UINT32 id)`.
    ///
    /// Always succeeds, including for an id that no longer exists — see
    /// `SharedInbox::close`'s own doc comment for why an error reply is not
    /// worth blocking the caller on the UI thread for.
    async fn close_notification(&self, id: u32) {
        self.inbox.close(id);
    }

    /// `ARRAY GetCapabilities(void)` — see `notifyd::CAPABILITIES` for the
    /// declared/not-declared list and the implementation behind each entry.
    async fn get_capabilities(&self) -> Vec<String> {
        CAPABILITIES.iter().map(|c| (*c).to_string()).collect()
    }

    /// `void GetServerInformation(out STRING name, out STRING vendor,
    /// out STRING version, out STRING spec_version)`.
    #[zbus(out_args("name", "vendor", "version", "spec_version"))]
    async fn get_server_information(&self) -> (String, String, String, String) {
        (SERVER_NAME.to_string(), SERVER_VENDOR.to_string(), env!("CARGO_PKG_VERSION").to_string(), SPEC_VERSION.to_string())
    }

    /// `NotificationClosed(UINT32 id, UINT32 reason)`. Declared for
    /// introspection; emitted by `emit_one` below — see this module's header.
    #[zbus(signal)]
    async fn notification_closed(emitter: &SignalEmitter<'_>, id: u32, reason: u32) -> zbus::Result<()>;

    /// `ActionInvoked(UINT32 id, STRING action_key)`. Same arrangement as
    /// `NotificationClosed` above.
    #[zbus(signal)]
    async fn action_invoked(emitter: &SignalEmitter<'_>, id: u32, action_key: &str) -> zbus::Result<()>;
}

/// Reads an integer hint, tolerating the wrong integer width.
///
/// The spec says `urgency` is a byte, and most senders comply — but a sender
/// that ships it as `u32` would otherwise have a genuinely critical alert
/// silently demoted to Normal, so the decode is deliberately permissive about
/// width while staying strict about *type* (a string "2" is not a number and
/// is not treated as one).
fn hint_u64(hints: &HashMap<String, OwnedValue>, key: &str) -> Option<u64> {
    match unwrap_variant(hints.get(key)?) {
        Value::U8(n) => Some(u64::from(*n)),
        Value::U16(n) => Some(u64::from(*n)),
        Value::U32(n) => Some(u64::from(*n)),
        Value::U64(n) => Some(*n),
        Value::I16(n) => u64::try_from(*n).ok(),
        Value::I32(n) => u64::try_from(*n).ok(),
        Value::I64(n) => u64::try_from(*n).ok(),
        _ => None,
    }
}

/// Reads a boolean hint. A missing key, a wrong-typed value and an explicit
/// `false` are all `false` — there is no state where "unreadable" should mean
/// "yes" (fail-closed, CLAUDE.md coding convention 4).
fn hint_true(hints: &HashMap<String, OwnedValue>, key: &str) -> bool {
    matches!(hints.get(key).map(unwrap_variant), Some(Value::Bool(true)))
}

/// Hints arrive as `a{sv}`, so every value is already a variant; some senders
/// nest a second one. Unwrap exactly one level rather than looping — an
/// arbitrarily deep nest is malformed input, not something to chase.
fn unwrap_variant(value: &OwnedValue) -> &Value<'_> {
    match &**value {
        Value::Value(inner) => inner,
        other => other,
    }
}

/// Handle held by `ShellView`. Dropping it ends the daemon thread (the
/// `Receiver` hangs up), which drops the `Connection` and releases the bus
/// name — the correct behavior on shell shutdown.
#[derive(Debug)]
pub(crate) struct DaemonHandle {
    tx: Sender<EmitCommand>,
    status: Arc<Mutex<DaemonState>>,
    /// Set once the first send fails, so a dead daemon produces one log line
    /// rather than one per click for the rest of the session.
    send_failure_logged: AtomicBool,
}

impl DaemonHandle {
    /// Current daemon status, for the panel's honest banner.
    pub(crate) fn status(&self) -> DaemonState {
        self.status.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Queues signals for the daemon thread to emit. Never blocks: an
    /// unbounded `mpsc::send` on a live channel is a lock-free push.
    pub(crate) fn emit(&self, commands: Vec<EmitCommand>) {
        for cmd in commands {
            if self.tx.send(cmd).is_err() && !self.send_failure_logged.swap(true, Ordering::Relaxed) {
                eprintln!("[notifyd] the notification daemon thread is gone — ActionInvoked/NotificationClosed signals are no longer being delivered");
            }
        }
    }
}

/// Starts the daemon. Returns immediately — the blocking bus connect happens
/// on the new thread, so this is safe to call from a gpui render pass.
///
/// The returned handle's `status()` starts at `Starting` and settles to
/// `Running` / `NameTaken` / `Failed` once the thread gets an answer.
pub(crate) fn spawn(inbox: SharedInbox) -> DaemonHandle {
    let (tx, rx) = mpsc::channel::<EmitCommand>();
    let status = Arc::new(Mutex::new(DaemonState::Starting));
    let thread_status = Arc::clone(&status);

    let spawned = std::thread::Builder::new().name("duduclaw-notifyd".to_string()).spawn(move || run(inbox, rx, thread_status));

    if let Err(e) = spawned {
        // Thread spawn failing is a resource-exhaustion situation, not
        // something to paper over: report it as the failure it is so the
        // panel can say third-party notifications are not working.
        eprintln!("[notifyd] could not start the notification daemon thread: {e}");
        set_status(&status, DaemonState::Failed(format!("thread spawn failed: {e}")));
    }

    DaemonHandle { tx, status, send_failure_logged: AtomicBool::new(false) }
}

fn run(inbox: SharedInbox, rx: Receiver<EmitCommand>, status: Arc<Mutex<DaemonState>>) {
    let connection = match connect(NotificationsService { inbox }) {
        Ok(conn) => conn,
        Err(zbus::Error::NameTaken) => {
            eprintln!("[notifyd] {BUS_NAME} is already owned by another daemon on this session bus — third-party notifications will go there, not to the DuDuClaw notification centre");
            set_status(&status, DaemonState::NameTaken);
            return;
        }
        Err(e) => {
            eprintln!("[notifyd] could not serve {BUS_NAME}: {e}");
            set_status(&status, DaemonState::Failed(format!("{e}")));
            return;
        }
    };

    eprintln!("[notifyd] serving {BUS_NAME} at {OBJECT_PATH} (spec {SPEC_VERSION}, capabilities: {})", CAPABILITIES.join(", "));
    set_status(&status, DaemonState::Running);

    // Parks here for the life of the shell. `recv` returns `Err` only when
    // every `Sender` is dropped, i.e. `ShellView` (and therefore the whole
    // shell) is going away — at which point returning drops `connection`,
    // which releases the bus name.
    while let Ok(cmd) = rx.recv() {
        emit_one(&connection, cmd);
    }
}

fn connect(service: NotificationsService) -> zbus::Result<Connection> {
    Builder::session()?.name(BUS_NAME)?.serve_at(OBJECT_PATH, service)?.build()
}

fn emit_one(connection: &Connection, cmd: EmitCommand) {
    let result = match &cmd {
        EmitCommand::Closed { id, reason } => {
            connection.emit_signal(None::<&str>, OBJECT_PATH, INTERFACE, "NotificationClosed", &(*id, reason.as_wire()))
        }
        EmitCommand::ActionInvoked { id, action_key } => {
            connection.emit_signal(None::<&str>, OBJECT_PATH, INTERFACE, "ActionInvoked", &(*id, action_key.as_str()))
        }
    };
    if let Err(e) = result {
        // Never silent: a lost signal means a client waiting forever for an
        // answer the operator already gave.
        eprintln!("[notifyd] failed to emit signal for {cmd:?}: {e}");
    }
}

fn set_status(status: &Arc<Mutex<DaemonState>>, state: DaemonState) {
    *status.lock().unwrap_or_else(PoisonError::into_inner) = state;
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Value as ZValue;

    fn hints(pairs: Vec<(&str, ZValue<'static>)>) -> HashMap<String, OwnedValue> {
        pairs.into_iter().filter_map(|(k, v)| OwnedValue::try_from(v).ok().map(|v| (k.to_string(), v))).collect()
    }

    #[test]
    fn urgency_reads_the_spec_byte() {
        let h = hints(vec![("urgency", ZValue::U8(2))]);
        assert_eq!(hint_u64(&h, "urgency"), Some(2));
    }

    #[test]
    fn urgency_tolerates_a_wrong_width_sender() {
        let h = hints(vec![("urgency", ZValue::U32(2))]);
        assert_eq!(hint_u64(&h, "urgency"), Some(2), "a critical alert must not be silently demoted");
    }

    #[test]
    fn a_non_numeric_urgency_is_not_guessed_at() {
        let h = hints(vec![("urgency", ZValue::Str("2".into()))]);
        assert_eq!(hint_u64(&h, "urgency"), None);
        assert_eq!(super::super::urgency_from_hint(None), super::super::Urgency::Normal);
    }

    #[test]
    fn a_missing_hint_is_none() {
        let h = hints(vec![]);
        assert_eq!(hint_u64(&h, "urgency"), None);
        assert!(!hint_true(&h, "transient"));
    }

    #[test]
    fn transient_is_read_and_fails_closed_on_junk() {
        assert!(hint_true(&hints(vec![("transient", ZValue::Bool(true))]), "transient"));
        assert!(!hint_true(&hints(vec![("transient", ZValue::Bool(false))]), "transient"));
        assert!(!hint_true(&hints(vec![("transient", ZValue::U32(1))]), "transient"), "an unreadable hint must not mean yes");
    }

    #[test]
    fn a_doubly_nested_variant_is_still_read() {
        let h = hints(vec![("urgency", ZValue::Value(Box::new(ZValue::U8(0))))]);
        assert_eq!(hint_u64(&h, "urgency"), Some(0));
    }
}
