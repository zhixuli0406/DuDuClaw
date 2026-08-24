// D6 (2026-08-23) — the shell's own `org.freedesktop.Notifications` daemon.
//
// ## Why this exists
//
// E1a's VM run proved the gap the hard way: LINE (and every other Chromium
// notification, and every future flatpak app) calls
// `org.freedesktop.Notifications.Notify` on the session bus, and on the
// DuDuClaw OS image **nobody answers** — `dbus-send` came back
// `ServiceUnknown`, the browser's own `onshow` handler fired, and the screen
// showed nothing. A notification that silently evaporates is worse than one
// that fails loudly: the operator has no way to even know a message arrived.
// So D6 is a hard prerequisite for the whole third-party-app line (E1/E2/E4),
// not a nicety.
//
// The拍板 was (b) — implement it in the shell rather than dropping a stock
// daemon (mako/dunst) into the image — for one reason that matters more than
// the implementation cost: a stock daemon paints its OWN popups, in its own
// visual language, with no relationship to the 通知中心 the operator already
// uses for approvals. Notifications from a browser tab and notifications from
// an AI agent would look like two unrelated operating systems. Owning the
// bus name ourselves puts every notification through ONE surface.
//
// ## Module layout (and what is deliberately NOT here)
//
// `mod.rs` (this file)  — the wire-shaped data model + the spec mapping
//                         (urgency, expire policy, close reasons,
//                         capabilities). Zero I/O, zero gpui, zero zbus, so
//                         it compiles and is unit-tested on the macOS dev
//                         loop where there is no D-Bus at all.
// `rate_limit.rs`       — the per-app flood guard. Pure, clock-injected.
// `inbox.rs`            — the shared hand-off point the D-Bus handlers write
//                         into and the UI drains. `Arc<Mutex<…>>`, no gpui.
// `center.rs`           — the notification-center store the UI renders:
//                         insert/replace/expire/dismiss/invoke, and the
//                         outbound signal commands those transitions owe the
//                         bus. Pure `&mut self`, no I/O.
// `dbus.rs`             — `#[cfg(target_os = "linux")]`. The ONLY file that
//                         knows zbus exists: owns the bus name, serves the
//                         four methods, emits the two signals.
//
// This split is the same discipline `global_task.rs` and `home/
// running_windows.rs` already state in their own headers ("pure `&mut self`
// mutation, no gpui types anywhere, no I/O performed here — the caller does
// the thread bridge"), applied to a service instead of a poll. It is what
// makes ~90% of this feature testable on a Mac that cannot run a single line
// of `dbus.rs`.
//
// ## Threading
//
// gpui's main thread never touches D-Bus. `dbus::spawn` starts ONE dedicated
// `std::thread`:
//
//   [3rd-party app] --D-Bus--> [zbus internal executor thread(s)]
//                                        |  (lock, push, unlock)
//                                        v
//                              SharedInbox (Arc<Mutex<VecDeque>>)
//                                        |  (drained on a 250ms gpui timer)
//                                        v
//                              NotificationCenter (on ShellView)
//                                        |  (user clicks / dismisses)
//                                        v
//                              mpsc::Sender<EmitCommand>
//                                        |
//                                        v
//                              [notifyd thread] --emit_signal--> bus
//
// The notifyd thread spends its whole life blocked in `rx.recv()`, so an
// idle machine pays nothing for it; it exists to (a) keep the `Connection`
// alive — dropping it would release the bus name — and (b) turn UI decisions
// back into `ActionInvoked`/`NotificationClosed` signals off the main thread.
// Method calls themselves are dispatched by zbus's own internal executor
// (`internal_executor: true` is the builder default, verified in the pinned
// zbus 5.19 source), never by that thread, so a slow UI can never stall a
// caller's `Notify`.
//
// ## Untrusted input
//
// Every string in a `Notify` call comes from an arbitrary local application.
// `sanitize_line`/`sanitize_block` + the length caps below are applied at the
// boundary (`NotifyRequest::sanitized`), before anything is stored or drawn:
// control characters are stripped, summaries are forced single-line, and
// nothing unbounded is ever kept. Same "validate at the boundary" rule
// `CLAUDE.md`'s own coding conventions state — and the truncation is
// codepoint-based (`chars().take(n)`), never a byte slice, per convention 1.

// Same reasoning as `inbox.rs`'s own copy of this attribute: several items
// here (the wire-shaped `NotifyRequest`, the hint decoders) have no production
// caller on a host with no D-Bus, and the macOS dev loop should not be noisy
// about code the ship target depends on.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::time::Duration;

pub(crate) mod center;
#[cfg(target_os = "linux")]
pub(crate) mod dbus;
pub(crate) mod inbox;
pub(crate) mod rate_limit;

/// The well-known bus name this shell owns. Losing the race for it (another
/// daemon got there first) is reported honestly rather than worked around —
/// see `center::DaemonState::NameTaken`.
pub(crate) const BUS_NAME: &str = "org.freedesktop.Notifications";
pub(crate) const OBJECT_PATH: &str = "/org/freedesktop/Notifications";
pub(crate) const INTERFACE: &str = "org.freedesktop.Notifications";

/// Reported by `GetServerInformation`. The spec version we implement against
/// is 1.2 (freedesktop Desktop Notifications Specification).
pub(crate) const SPEC_VERSION: &str = "1.2";
pub(crate) const SERVER_NAME: &str = "duduclaw-shell";
pub(crate) const SERVER_VENDOR: &str = "DuDuClaw";

/// The action key the spec reserves for "the user activated the notification
/// itself" (as opposed to one of its buttons). Clicking the card invokes
/// exactly this, and only when the sender actually declared it.
pub(crate) const DEFAULT_ACTION_KEY: &str = "default";

// ── Boundary caps (untrusted input) ──────────────────────────────────────
// `pub(crate)` (was private until A1 result-loopback, 2026-08-24):
// `center::NotificationCenter::post_system` applies the SAME caps to
// shell-originated cards (a task's `result_summary`/`judge_feedback` is
// agent-generated free text, not literally untrusted the way a third-party
// D-Bus call is, but a card with an unbounded body or an embedded newline
// in its summary looks exactly as broken either way — see that fn's own
// doc comment).
pub(crate) const MAX_APP_NAME_CHARS: usize = 48;
pub(crate) const MAX_SUMMARY_CHARS: usize = 160;
pub(crate) const MAX_BODY_CHARS: usize = 1200;
const MAX_ACTION_LABEL_CHARS: usize = 40;
const MAX_ACTION_KEY_CHARS: usize = 64;
/// Buttons a single notification may contribute to the panel. Anything past
/// this is dropped (with the count kept, see `NotifyRequest::sanitized`) —
/// a 390px-wide panel cannot honestly render twenty buttons, and an app that
/// sends twenty is not going to be helped by us trying.
const MAX_ACTIONS: usize = 4;

/// Ceiling on an app-supplied `expire_timeout`. A client asking for a
/// notification that lives for a year is asking for something we would rather
/// call "never" honestly than pretend to schedule.
const MAX_EXPIRE: Duration = Duration::from_secs(24 * 60 * 60);

/// What a transient notification (`hints["transient"] == true`) gets when it
/// asks for the server default (`expire_timeout == -1`). Transient is the
/// sender explicitly saying "this does not belong in a persistent list", so
/// honoring it is what makes the `persistence` capability below truthful for
/// everything else.
const TRANSIENT_DEFAULT_EXPIRE: Duration = Duration::from_secs(8);

/// Capabilities `GetCapabilities` reports — and NOTHING else.
///
/// Declared, because each is genuinely implemented:
/// - `body`        — the body text is rendered on the card.
/// - `actions`     — action buttons are rendered, and a click on the card
///   itself invokes `default` when the sender declared it
///   (`center::NotificationCenter::invoke_default`).
/// - `persistence` — notifications stay in the 通知中心 until dismissed
///   rather than vanishing with a popup. This is literally
///   what this shell does, and it is also what makes the
///   `expire_timeout` policy below defensible (see
///   `ExpirePolicy::resolve`).
///
/// Deliberately NOT declared, because they are not implemented — an
/// over-claimed capability makes a client send content we then silently
/// mangle, which is exactly the failure mode D6 exists to end:
/// - `body-markup` / `body-hyperlinks` / `body-images` — the body is drawn
///   as plain text; no markup is parsed, no links are clickable, no images
///   are decoded.
/// - `icon-static` / `icon-multi` — `app_icon` is accepted and ignored; the
///   card shows a derived initial avatar instead (see the follow-up list in
///   `commercial/docs/TODO-agent-first-os-2026-08.md`).
/// - `action-icons` — action buttons are text-only.
/// - `sound` — nothing is played.
pub(crate) const CAPABILITIES: &[&str] = &["body", "actions", "persistence"];

/// Spec urgency levels (`hints["urgency"]`, a byte).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Urgency {
    Low,
    #[default]
    Normal,
    Critical,
}

impl Urgency {
    /// Anything at or above 2 is Critical; anything unrecognized falls back
    /// to Normal, which is what the spec says the default is. Deliberately
    /// tolerant of the wrong integer width — the hint is specified as a byte
    /// but real senders have been seen using `u32`, and refusing those would
    /// silently downgrade a genuinely critical alert.
    pub(crate) fn from_raw(raw: u64) -> Self {
        match raw {
            0 => Urgency::Low,
            1 => Urgency::Normal,
            _ => Urgency::Critical,
        }
    }
}

/// One `(key, label)` pair out of `Notify`'s flat `actions` array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NotificationAction {
    pub(crate) key: String,
    pub(crate) label: String,
}

/// How long a notification stays in the center.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExpirePolicy {
    /// Stays until the operator dismisses it or the sender closes it.
    Never,
    After(Duration),
}

impl ExpirePolicy {
    /// Maps the spec's `expire_timeout` (milliseconds; `0` = never, `-1` =
    /// "server decides") onto what this shell actually does.
    ///
    /// Two deliberate policy decisions, both stated here rather than buried:
    ///
    /// 1. **Critical never expires.** The spec says critical notifications
    ///    should not automatically expire, and it is right: an alert the
    ///    operator was not present for is the one that most needs to still
    ///    be there when they come back.
    ///
    /// 2. **`-1` resolves to `Never`, not to a few seconds.** On a desktop
    ///    with a transient banner layer, "server default" conventionally
    ///    means "show the popup for ~5-10s, then move it into the tray".
    ///    This shell has NO banner layer yet (the 通知中心 panel is the only
    ///    surface — a toast/banner surface is a listed follow-up), so
    ///    resolving `-1` to a timeout would mean the notification is removed
    ///    before anything ever displayed it. That is the exact silent-loss
    ///    failure D6 exists to fix, so `-1` keeps the message instead. An
    ///    app that genuinely wants ephemeral behavior has two honest ways to
    ///    say so — a positive `expire_timeout`, or `hints["transient"]` —
    ///    and both are honored.
    pub(crate) fn resolve(expire_timeout: i32, urgency: Urgency, transient: bool) -> Self {
        if urgency == Urgency::Critical {
            return ExpirePolicy::Never;
        }
        match expire_timeout {
            0 => ExpirePolicy::Never,
            ms if ms > 0 => {
                let want = Duration::from_millis(ms as u64);
                ExpirePolicy::After(want.min(MAX_EXPIRE))
            }
            // Negative == "server decides" (the spec only defines -1, but any
            // negative is treated the same rather than rejected).
            _ if transient => ExpirePolicy::After(TRANSIENT_DEFAULT_EXPIRE),
            _ => ExpirePolicy::Never,
        }
    }
}

/// `NotificationClosed`'s `reason` argument, straight from the spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CloseReason {
    /// 1 — the notification expired.
    Expired,
    /// 2 — dismissed by the user.
    Dismissed,
    /// 3 — closed by a `CloseNotification` call.
    Requested,
    /// 4 — undefined/reserved. Used here for the one case the spec has no
    /// better code for: the center hit its hard capacity and evicted the
    /// oldest card. Emitting *something* is what lets a sender stop waiting.
    Undefined,
}

impl CloseReason {
    pub(crate) fn as_wire(self) -> u32 {
        match self {
            CloseReason::Expired => 1,
            CloseReason::Dismissed => 2,
            CloseReason::Requested => 3,
            CloseReason::Undefined => 4,
        }
    }
}

/// One `Notify` call, already sanitized and spec-mapped, on its way from the
/// D-Bus handler to the UI.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PostedNotification {
    pub(crate) id: u32,
    pub(crate) app_name: String,
    pub(crate) summary: String,
    pub(crate) body: String,
    pub(crate) actions: Vec<NotificationAction>,
    pub(crate) urgency: Urgency,
    pub(crate) expire: ExpirePolicy,
}

/// The raw, unsanitized shape of a `Notify` call. Built by `dbus.rs` from the
/// wire arguments; `sanitized` is the only way to turn it into something the
/// rest of the shell will touch.
#[derive(Debug, Clone, Default)]
pub(crate) struct NotifyRequest {
    pub(crate) app_name: String,
    pub(crate) replaces_id: u32,
    pub(crate) summary: String,
    pub(crate) body: String,
    /// Flat `[key1, label1, key2, label2, …]` exactly as it arrives.
    pub(crate) actions: Vec<String>,
    pub(crate) urgency: Urgency,
    pub(crate) transient: bool,
    pub(crate) expire_timeout: i32,
}

impl NotifyRequest {
    /// Applies every boundary rule in one place, and stamps the id the
    /// caller allocated.
    pub(crate) fn sanitized(&self, id: u32) -> PostedNotification {
        PostedNotification {
            id,
            app_name: sanitize_line(&self.app_name, MAX_APP_NAME_CHARS),
            summary: sanitize_line(&self.summary, MAX_SUMMARY_CHARS),
            body: sanitize_block(&self.body, MAX_BODY_CHARS),
            actions: parse_actions(&self.actions),
            urgency: self.urgency,
            expire: ExpirePolicy::resolve(self.expire_timeout, self.urgency, self.transient),
        }
    }
}

/// Splits the spec's flat `actions` array into pairs, dropping a trailing
/// unpaired key (a malformed sender) and anything past `MAX_ACTIONS`.
///
/// An action whose key is empty is dropped: the key is what goes back out on
/// `ActionInvoked`, and an empty one is unaddressable. An action with an
/// empty *label* keeps its key and falls back to showing the key, because
/// dropping it would silently remove a button the app is waiting on.
fn parse_actions(flat: &[String]) -> Vec<NotificationAction> {
    let mut out = Vec::new();
    for pair in flat.chunks_exact(2) {
        if out.len() >= MAX_ACTIONS {
            break;
        }
        let key = sanitize_line(&pair[0], MAX_ACTION_KEY_CHARS);
        if key.is_empty() {
            continue;
        }
        let label_raw = sanitize_line(&pair[1], MAX_ACTION_LABEL_CHARS);
        let label = if label_raw.is_empty() { key.clone() } else { label_raw };
        out.push(NotificationAction { key, label });
    }
    out
}

/// Single-line sanitizer: every control character (newlines included) becomes
/// a space, runs of whitespace collapse, and the result is trimmed and capped
/// by CODEPOINT count (never a byte slice — CLAUDE.md coding convention 1).
pub(crate) fn sanitize_line(raw: &str, max_chars: usize) -> String {
    let mut out = String::new();
    // Tracked explicitly rather than re-counting `out.chars()` per iteration:
    // that would be quadratic in the (attacker-chosen) input length.
    let mut kept = 0usize;
    let mut pending_space = false;
    for ch in raw.chars() {
        if kept >= max_chars {
            break;
        }
        if ch.is_control() || ch == '\u{feff}' || ch.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            kept += 1;
            pending_space = false;
            if kept >= max_chars {
                break;
            }
        }
        out.push(ch);
        kept += 1;
    }
    out
}

/// Block sanitizer: keeps newlines (a body legitimately has them), strips
/// every other control character, collapses runs of blank lines, and caps by
/// codepoint count.
pub(crate) fn sanitize_block(raw: &str, max_chars: usize) -> String {
    let mut out = String::new();
    let mut kept = 0usize;
    let mut newlines = 0usize;
    for ch in raw.chars() {
        if kept >= max_chars {
            break;
        }
        if ch == '\n' {
            // At most one blank line in a row; never lead with newlines.
            if out.is_empty() || newlines >= 2 {
                continue;
            }
            newlines += 1;
            out.push('\n');
            kept += 1;
            continue;
        }
        if ch.is_control() || ch == '\u{feff}' {
            continue;
        }
        newlines = 0;
        out.push(ch);
        kept += 1;
    }
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

/// Reads the two hints this daemon actually acts on out of a decoded hint
/// map, without `dbus.rs` having to know the mapping rules.
///
/// Lives here (not in `dbus.rs`) so it is testable on macOS: the caller
/// decodes `a{sv}` into plain integers/bools, this decides what they mean.
pub(crate) fn urgency_from_hint(raw: Option<u64>) -> Urgency {
    raw.map(Urgency::from_raw).unwrap_or_default()
}

/// What the UI hands back to the bus after the operator acts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EmitCommand {
    Closed { id: u32, reason: CloseReason },
    ActionInvoked { id: u32, action_key: String },
}

/// What the bus hands to the UI.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DaemonEvent {
    /// A new notification, or one replacing an existing id.
    Posted(Box<PostedNotification>),
    /// The flood guard folded this call onto an already-visible card
    /// (`onto`): its text replaces that card's and the card's "+N" counter
    /// goes up by one. See `rate_limit` for the policy.
    Merged { onto: u32, posted: Box<PostedNotification> },
    /// A sender called `CloseNotification(id)`.
    CloseRequested { id: u32 },
}

/// Everything `ShellView` needs to own to have a notification daemon, with
/// the platform split contained.
///
/// Exists so neither `main.rs` nor the overlay carries a single `#[cfg]`: on
/// Linux this holds a live `dbus::DaemonHandle`; everywhere else `start` is a
/// no-op that reports `DaemonState::Unsupported` and `emit` drops the
/// commands (there is no bus to send them to, and pretending otherwise would
/// be the dishonest option). The macOS dev loop therefore compiles and runs
/// the whole feature except the transport.
#[derive(Debug, Default)]
pub(crate) struct NotifyRuntime {
    /// Written by the D-Bus handlers, drained by the gpui timer.
    pub(crate) inbox: inbox::SharedInbox,
    #[cfg(target_os = "linux")]
    daemon: Option<dbus::DaemonHandle>,
}

impl NotifyRuntime {
    /// Starts the daemon exactly once. Returns the state to show right now
    /// (`Starting` on the first Linux call — the real answer arrives via
    /// `status()` once the bus has replied).
    ///
    /// Idempotent: a second call while a daemon is already running just
    /// reports its status, so an accidental double-arm cannot end up with two
    /// connections racing for one bus name.
    pub(crate) fn start(&mut self) -> center::DaemonState {
        #[cfg(target_os = "linux")]
        {
            if let Some(handle) = &self.daemon {
                return handle.status();
            }
            let handle = dbus::spawn(self.inbox.clone());
            let state = handle.status();
            self.daemon = Some(handle);
            state
        }
        #[cfg(not(target_os = "linux"))]
        {
            center::DaemonState::Unsupported
        }
    }

    /// Whether `start` has already been called on a platform that can serve.
    pub(crate) fn is_started(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.daemon.is_some()
        }
        #[cfg(not(target_os = "linux"))]
        {
            true
        }
    }

    pub(crate) fn status(&self) -> center::DaemonState {
        #[cfg(target_os = "linux")]
        {
            self.daemon.as_ref().map(dbus::DaemonHandle::status).unwrap_or_default()
        }
        #[cfg(not(target_os = "linux"))]
        {
            center::DaemonState::Unsupported
        }
    }

    /// Hands the operator's decisions back to the bus.
    pub(crate) fn emit(&self, commands: Vec<EmitCommand>) {
        #[cfg(target_os = "linux")]
        {
            if let Some(handle) = &self.daemon {
                handle.emit(commands);
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = commands;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn critical_never_expires_no_matter_what_the_sender_asked_for() {
        assert_eq!(ExpirePolicy::resolve(1500, Urgency::Critical, false), ExpirePolicy::Never);
        assert_eq!(ExpirePolicy::resolve(-1, Urgency::Critical, true), ExpirePolicy::Never);
    }

    #[test]
    fn zero_means_never_and_a_positive_timeout_is_honored() {
        assert_eq!(ExpirePolicy::resolve(0, Urgency::Normal, false), ExpirePolicy::Never);
        assert_eq!(ExpirePolicy::resolve(2500, Urgency::Normal, false), ExpirePolicy::After(Duration::from_millis(2500)));
    }

    #[test]
    fn server_default_keeps_the_message_unless_the_sender_says_transient() {
        // The whole point of D6: -1 must not silently drop a message on a
        // shell that has no banner surface to have shown it.
        assert_eq!(ExpirePolicy::resolve(-1, Urgency::Normal, false), ExpirePolicy::Never);
        assert_eq!(ExpirePolicy::resolve(-1, Urgency::Normal, true), ExpirePolicy::After(TRANSIENT_DEFAULT_EXPIRE));
    }

    #[test]
    fn an_absurd_timeout_is_clamped_rather_than_scheduled() {
        assert_eq!(ExpirePolicy::resolve(i32::MAX, Urgency::Low, false), ExpirePolicy::After(MAX_EXPIRE));
    }

    #[test]
    fn urgency_maps_the_spec_bytes_and_tolerates_junk() {
        assert_eq!(Urgency::from_raw(0), Urgency::Low);
        assert_eq!(Urgency::from_raw(1), Urgency::Normal);
        assert_eq!(Urgency::from_raw(2), Urgency::Critical);
        assert_eq!(Urgency::from_raw(99), Urgency::Critical);
        assert_eq!(urgency_from_hint(None), Urgency::Normal);
    }

    #[test]
    fn close_reasons_match_the_spec_wire_values() {
        assert_eq!(CloseReason::Expired.as_wire(), 1);
        assert_eq!(CloseReason::Dismissed.as_wire(), 2);
        assert_eq!(CloseReason::Requested.as_wire(), 3);
        assert_eq!(CloseReason::Undefined.as_wire(), 4);
    }

    #[test]
    fn a_summary_is_forced_onto_one_line_with_control_chars_stripped() {
        let got = sanitize_line("hello\nthere\u{0007}\tworld  ", 100);
        assert_eq!(got, "hello there world");
    }

    #[test]
    fn truncation_is_by_codepoint_and_never_splits_a_cjk_char() {
        // 8 CJK codepoints, 24 bytes — a byte-slice truncation at 10 would
        // panic; this must not.
        let got = sanitize_line("一二三四五六七八", 5);
        assert_eq!(got.chars().count(), 5);
        assert_eq!(got, "一二三四五");
    }

    #[test]
    fn a_body_keeps_newlines_but_collapses_blank_runs_and_trims() {
        let got = sanitize_block("line one\n\n\n\nline two\n\n", 100);
        assert_eq!(got, "line one\n\nline two");
    }

    #[test]
    fn actions_parse_into_pairs_and_drop_a_dangling_key() {
        let flat = vec!["default".into(), "Open".into(), "reply".into(), "Reply".into(), "orphan".into()];
        let got = parse_actions(&flat);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], NotificationAction { key: "default".into(), label: "Open".into() });
        assert_eq!(got[1].key, "reply");
    }

    #[test]
    fn an_action_with_an_empty_label_falls_back_to_its_key_and_an_empty_key_is_dropped() {
        let flat = vec!["".into(), "No key".into(), "act".into(), "".into()];
        let got = parse_actions(&flat);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0], NotificationAction { key: "act".into(), label: "act".into() });
    }

    #[test]
    fn a_flood_of_actions_is_capped() {
        let flat: Vec<String> = (0..40).map(|i| format!("a{i}")).collect();
        assert_eq!(parse_actions(&flat).len(), MAX_ACTIONS);
    }

    /// The `default` key survives sanitisation verbatim (it is what
    /// `center::CenterNotification::default_action` matches on, and what goes
    /// back out on the wire in `ActionInvoked`) — and is never synthesised
    /// for a sender that did not declare it. The behavioural half of this
    /// rule (a click on a card with no default action does nothing) is
    /// asserted in `center`'s own tests.
    #[test]
    fn the_default_action_key_is_preserved_and_never_invented() {
        let req = NotifyRequest { actions: vec!["reply".into(), "Reply".into()], ..Default::default() };
        assert!(!req.sanitized(1).actions.iter().any(|a| a.key == DEFAULT_ACTION_KEY));

        let req = NotifyRequest { actions: vec!["default".into(), "Open".into()], ..Default::default() };
        assert!(req.sanitized(1).actions.iter().any(|a| a.key == DEFAULT_ACTION_KEY));
    }

    #[test]
    fn capabilities_never_over_claim() {
        // Guard against a future edit adding a capability without adding the
        // behavior — every entry here has an implementation cited in
        // `CAPABILITIES`'s own doc comment.
        assert_eq!(CAPABILITIES, &["body", "actions", "persistence"]);
        for over_claimed in ["body-markup", "body-images", "body-hyperlinks", "icon-static", "icon-multi", "sound", "action-icons"] {
            assert!(!CAPABILITIES.contains(&over_claimed), "{over_claimed} is not implemented and must not be advertised");
        }
    }
}
