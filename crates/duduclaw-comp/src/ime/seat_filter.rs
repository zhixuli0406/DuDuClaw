//! Per-client `wl_seat` visibility — who is allowed to see the **agent** seat.
//!
//! Despite living under `ime/` (where D3-c, the first rule below, was born),
//! this module is no longer input-method-specific: it owns the compositor's
//! whole answer to "which `wl_seat` globals does this client get to see".
//! Two rules run through it.
//!
//! ## Rule 1 (E1a-1, 2026-08-23): only the session shell sees the agent seat
//!
//! `duduclaw-comp` runs **two** `wl_seat`s — the human `"winit"` seat and the
//! agent `"duduclaw-agent"` seat codrive injects through (`crate::codrive`).
//! A Wayland client is free to bind both. Real ones frequently do not:
//!
//! * `duduclaw-shell` is a gpui client, and gpui keeps exactly one seat —
//!   the **last** one the registry advertised (`crate::seat_order`'s module
//!   doc has the line numbers). That is why the compositor advertises the
//!   agent seat first: "last wins" then lands on the human seat.
//! * Chromium keeps exactly one seat too — the **first** one. Measured on the
//!   real appliance VM (E1a, 2026-08-23, three reproductions): under the
//!   `AgentFirst` order Chromium binds the *agent* seat, and the human gets no
//!   pointer, no keyboard, no clicks. Nothing about the app is broken; it is
//!   simply listening to a seat the human never drives.
//!
//! The two behaviours are mutually exclusive under any single advertisement
//! order, which is what made this a ship-blocker. Ordering cannot fix it —
//! **visibility** can. Wayland already has the right primitive: a global can
//! be filtered per client, so each client can be handed exactly the seats it
//! should be reasoning about. Non-shell clients are handed the human seat and
//! nothing else, so there is no second seat for a single-seat client to pick
//! wrongly, whichever end of the list it picks from.
//!
//! ### What this costs, stated plainly
//!
//! **codrive cannot drive a client that cannot see the agent seat.** This is
//! not a guess — smithay routes seat events through the client's own
//! resources: `KeyboardTarget::key` for a `WlSurface` calls
//! `for_each_focused_kbds`, which walks `KeyboardHandle::known_kbds` (the
//! `wl_keyboard` objects created from *that* seat) and keeps the ones whose
//! client matches the focused surface (`smithay-0.7.0/src/wayland/seat/
//! keyboard.rs:143`); `PointerTarget` does the same through
//! `for_each_focused_pointer` / `known_pointers` (`pointer.rs:222`). A client
//! that never received the agent seat's `wl_registry.global` event never
//! created either object, so an injected key or click reaches **nobody**.
//!
//! There is no compositor-side synthesis path that routes around this today.
//! So the rule is deliberately paired with an explicit, audited failure in
//! `DuduclawComp::handle_agent_inject` — the same doctrine as the
//! `paused_by_ime` guard below: an injection that cannot land is *reported*,
//! never silently swallowed while the audit trail records `inject_applied`.
//!
//! [`AGENT_SEAT_PROCS_ENV`] is the escape hatch: add a process name to it and
//! that client sees the agent seat again (and can be co-driven again, at the
//! cost of re-exposing it to the single-seat hazard above). Note that a
//! single-seat client cannot have it both ways in any configuration — it can
//! only ever be driven by whichever one seat it picked.
//!
//! ## Rule 2 (D3-c): an input method never sees the agent seat
//!
//! fcitx5's `WaylandIMServerV2::refreshSeat()` walks **every** `wl_seat` the
//! registry advertises and creates one input-method context per seat, each of
//! which immediately calls `grab_keyboard()`. smithay's
//! `InputMethodKeyboardGrab::input()` never touches `KeyboardInnerHandle` at
//! all (`smithay-0.7.0/src/wayland/input_method/input_method_keyboard_grab.rs`),
//! so on a grabbed seat *every* key belongs to the input method and no client
//! sees one. Left alone, starting fcitx5 would silently kill codrive's
//! `type_text` — every injected key eaten into a composition nobody reads.
//!
//! Both facts were read out of the sources, not assumed; the full chain is in
//! `research/native-os-2026-08/ime-fcitx5-gpui-2026-08.md` §5.3.
//!
//! Rule 1 already denies fcitx5 the agent seat (it is not the shell), so rule
//! 2 is now defence in depth — and it is the half that must stay
//! **un-weakenable**: allow-listing a process via [`AGENT_SEAT_PROCS_ENV`]
//! grants the agent seat, but an input method is refused anyway. Getting rule
//! 2 wrong costs silent keystroke loss; getting rule 1 wrong costs a loudly
//! reported dropped injection.
//!
//! ## How the filter reaches `can_view`
//!
//! The D3-c probe proposed `create_global_with_filter`. **That literal route
//! is closed**: smithay's `SeatState::new_wl_seat` uses plain `create_global`,
//! and its `SeatGlobalData<D>` has a private `arc` field with no constructor —
//! so this crate cannot build the global data and therefore cannot create the
//! seat global itself. What *is* open, and is what this module does:
//!
//! 1. **`delegate_seat!` splits.** It is one `delegate_global_dispatch!` plus
//!    four `delegate_dispatch!` invocations over public types. Writing the
//!    four `Dispatch` delegations by hand and hand-rolling only the
//!    `GlobalDispatch` gives us `can_view` — and `bind` just forwards to
//!    smithay's own impl, so binding behaviour stays byte-identical.
//! 2. **The seat's name is observable through `Debug`.** `can_view` receives
//!    only `&SeatGlobalData<D>`, whose one field is private — but
//!    `SeatRc<D>`'s `Debug` impl prints `name` as its first field. This module
//!    sniffs it with a `fmt::Write` sink that **aborts the formatting run the
//!    moment the name is complete**, so `inner` (a `Mutex` holding pointer /
//!    keyboard handles and the known-seat list) is never formatted at all.
//!
//! Point 2 leans on a `Debug` rendering, which is not a stability guarantee.
//! Two things keep that honest rather than fragile:
//!
//! * [`parse_seat_name`] is a pure function with unit tests, and
//!   [`arm`] re-runs the whole extraction at startup against the two **real**
//!   `Seat` handles whose names this crate itself chose. A smithay upgrade
//!   that changes the rendering fails that check on the next boot, loudly.
//! * When the check fails the filter **disarms** — every seat stays visible to
//!   everyone, exactly as before this module existed. That restores the
//!   pre-E1a-1 state, which means the measured Chromium breakage comes back;
//!   the disarm log says so in as many words, because a compositor that
//!   quietly returns to a known-broken configuration is the worst outcome
//!   available. codrive's own backstop (`crate::codrive`'s `paused_by_ime`
//!   guard) still turns a resulting IME grab into a reported error instead of
//!   silently swallowed keystrokes. Degradation is visible, never silent.
//!
//! ## Identifying a client
//!
//! Classification happens **once per connection**, at accept time in
//! `state::DuduclawComp::init_wayland_listener`, from the socket's
//! `SO_PEERCRED` pid (via `Client::get_credentials`, the same route
//! `codrive::window_geometry::window_pid` already uses) → `/proc/<pid>/comm`,
//! and is cached on [`crate::state::ClientState`]. `can_view` then costs one
//! atomic read plus, for a client that is not allow-listed, one short bounded
//! `Debug` sniff per seat global.
//!
//! `/proc/<pid>/comm` is settable by the process itself, so it is not an
//! authentication mechanism — but note which way each failure leans. A client
//! that lies its way into "I am an input method" only loses sight of the agent
//! seat. A client that lies its way into "I am the shell" gains sight of a
//! seat whose injection socket is separately token-authenticated
//! (`crate::codrive`'s `write_token_file`), so there is still no authority to
//! steal here — only the single-seat hazard rule 1 exists to avoid, taken on
//! by a process that asked for it.

use std::fmt::{self, Write as _};
use std::sync::atomic::{AtomicBool, Ordering};

use smithay::{
    input::{Seat, SeatState},
    reexports::wayland_server::{
        protocol::{
            wl_keyboard::WlKeyboard, wl_pointer::WlPointer, wl_seat::WlSeat, wl_touch::WlTouch,
        },
        Client, DataInit, DisplayHandle, GlobalDispatch, New,
    },
    wayland::seat::{KeyboardUserData, PointerUserData, SeatGlobalData, SeatUserData, TouchUserData},
};

use crate::{codrive::AGENT_SEAT_NAME, state::ClientState, DuduclawComp};

/// Env override for the process names treated as input methods, comma
/// separated. Empty entries are ignored; an entirely empty value disables
/// input-method detection altogether (rule 1 still hides the agent seat from
/// them, because an input method is not the shell).
pub const IME_PROCS_ENV: &str = "DUDUCLAW_COMP_IME_PROCS";

/// Env flag: when set to `1`, only clients classified as input methods may
/// bind `zwp_input_method_manager_v2` / `zwp_virtual_keyboard_manager_v1`.
/// Default is off — see [`client_may_use_input_method`].
pub const IME_STRICT_ENV: &str = "DUDUCLAW_COMP_IME_STRICT";

/// E1a-1 env override for the process names allowed to see the **agent**
/// seat, comma separated. **Replaces** [`DEFAULT_AGENT_SEAT_PROCS`] rather
/// than extending it (same semantics as [`IME_PROCS_ENV`]), and the resolved
/// list is logged at startup so the effective value is never a guess.
///
/// Adding a process here restores codrive's reach into that client at the
/// cost of re-exposing it to the single-seat hazard in this module's doc.
/// An entirely empty value hides the agent seat from *every* client —
/// maximum isolation, and legal: the shell does not need the agent seat, it
/// only needs the human one.
pub const AGENT_SEAT_PROCS_ENV: &str = "DUDUCLAW_COMP_AGENT_SEAT_PROCS";

/// Env kill switch for the whole per-client filter: `0` / `off` / `false`
/// turns it off and every client sees every seat again (the pre-E1a-1
/// behaviour, i.e. the measured-broken one — for debugging only).
pub const SEAT_FILTER_ENV: &str = "DUDUCLAW_COMP_SEAT_FILTER";

/// Process names (as reported by `/proc/<pid>/comm`, which the kernel
/// truncates to 15 characters) treated as input methods when
/// [`IME_PROCS_ENV`] is unset.
const DEFAULT_IME_PROCS: &[&str] = &["fcitx5", "fcitx", "ibus-daemon", "kimpanel"];

/// Process names allowed to see the agent seat when [`AGENT_SEAT_PROCS_ENV`]
/// is unset.
///
/// Exactly one entry: the session shell. It is here — rather than being
/// filtered like everything else — because it is the one client whose
/// single-seat behaviour the `AgentFirst` advertisement order is built
/// around, and that pairing is the configuration Shell-S0…S3 verified on
/// real hardware. `"duduclaw-shell"` is 14 bytes, comfortably inside the
/// kernel's 15-byte `comm` truncation.
///
/// Spelled out rather than reusing `window_policy::SHELL_APP_ID`: that
/// constant is an `xdg_toplevel.app_id`, a different namespace that merely
/// happens to carry the same string today. Overriding one must not silently
/// move the other.
const DEFAULT_AGENT_SEAT_PROCS: &[&str] = &["duduclaw-shell"];

/// Upper bound on how much of a `Debug` rendering [`sniff_seat_name`] will
/// materialise. The prefix it needs is `SeatGlobalData { arc: SeatRc { name:
/// "…"` — about 40 bytes for our seat names — so this is generous headroom
/// that still stops well before `SeatRc`'s `inner` field.
const SNIFF_CAP: usize = 192;

/// Whether the per-client seat filter is live. Set exactly once, by [`arm`],
/// and only when the startup self-check passed. `can_view` is a static method
/// with no access to compositor state, which is why this is process-global
/// rather than a field on `DuduclawComp`.
static FILTER_ARMED: AtomicBool = AtomicBool::new(false);

/// What a connection's peer process was recognised as, decided once at accept
/// time. Both fields are questions about the *client*, deliberately kept
/// separate rather than collapsed into one enum: they gate different things
/// (`allow_listed` gates the agent seat, `is_input_method` gates both the
/// agent seat and — under [`IME_STRICT_ENV`] — the IME manager globals), and
/// a client can legitimately be neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClientClass {
    /// The peer's process name is on the input-method list ([`IME_PROCS_ENV`]).
    pub is_input_method: bool,
    /// The peer's process name is on the agent-seat allow list
    /// ([`AGENT_SEAT_PROCS_ENV`]).
    pub allow_listed: bool,
}

/// Outcome of the startup self-check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterStatus {
    /// The filter is live: only allow-listed, non-input-method clients see
    /// the agent seat.
    Armed,
    /// The filter could not be trusted and is off. Every client sees every
    /// seat — including the single-seat clients rule 1 exists to protect.
    Disarmed(&'static str),
    /// The filter was turned off deliberately via [`SEAT_FILTER_ENV`].
    Off,
}

impl FilterStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            FilterStatus::Armed => "armed",
            FilterStatus::Disarmed(_) => "disarmed",
            FilterStatus::Off => "off",
        }
    }
}

/// Runs the startup self-check and, if it passes, arms the filter.
///
/// The check is deliberately end-to-end: it drives the *same* extraction path
/// `can_view` will use, over the two real seats this process just created, and
/// insists on getting back the two names this crate itself asked for. Nothing
/// short of that is evidence that a `SeatGlobalData` will be classified
/// correctly later.
pub fn arm(human: &Seat<DuduclawComp>, agent: &Seat<DuduclawComp>) -> FilterStatus {
    if !filter_enabled() {
        tracing::warn!(
            "comp/seat: per-client seat filter turned OFF by {}. Every client now sees BOTH \
             seats, which is the configuration measured to leave first-seat-wins clients \
             (Chromium) with no human input at all (E1a). Debugging only",
            SEAT_FILTER_ENV
        );
        return FilterStatus::Off;
    }
    let status = evaluate(sniff_seat_name(human), sniff_seat_name(agent));
    match &status {
        FilterStatus::Armed => {
            FILTER_ARMED.store(true, Ordering::SeqCst);
            tracing::info!(
                agent_seat = AGENT_SEAT_NAME,
                agent_seat_procs = ?agent_seat_proc_names(),
                ime_procs = ?ime_proc_names(),
                strict = strict_mode(),
                "comp/seat: the agent seat is visible ONLY to the allow-listed process names \
                 above, and never to an input method (E1a-1 + D3-c). codrive cannot drive a \
                 client that cannot see it — injections at such a client are dropped and \
                 audited, never silently lost. Override the allow list with {}, the \
                 input-method list with {}, restrict who may bind the IME managers with \
                 {}=1, or turn the whole filter off with {}=off",
                AGENT_SEAT_PROCS_ENV,
                IME_PROCS_ENV,
                IME_STRICT_ENV,
                SEAT_FILTER_ENV
            );
        }
        FilterStatus::Disarmed(reason) => {
            tracing::error!(
                reason,
                "comp/seat: per-client seat filter DISARMED — every client can see the agent \
                 seat again. Two known consequences, both measured: a first-seat-wins client \
                 (Chromium) binds the agent seat and the human loses pointer/keyboard in it \
                 entirely (E1a), and an input method can grab the agent seat's keyboard, which \
                 stops codrive typing (D3-c; codrive reports `paused_by_ime` rather than losing \
                 keystrokes silently). This almost always means smithay's Seat Debug rendering \
                 changed; see `ime::seat_filter`'s module doc"
            );
        }
        // Unreachable: the `filter_enabled` early return above owns this
        // variant. Matched rather than `unreachable!()` so a future change
        // that starts producing it here fails quietly instead of panicking a
        // compositor at boot.
        FilterStatus::Off => {}
    }
    status
}

/// The self-check's decision, split out from [`arm`] so it is testable without
/// touching process-global state.
fn evaluate(human: Option<String>, agent: Option<String>) -> FilterStatus {
    let (Some(human), Some(agent)) = (human, agent) else {
        return FilterStatus::Disarmed("a seat name could not be read back out of Debug");
    };
    if agent != AGENT_SEAT_NAME {
        return FilterStatus::Disarmed("the agent seat's name did not read back as expected");
    }
    if human == agent {
        return FilterStatus::Disarmed("both seats read back with the same name");
    }
    FilterStatus::Armed
}

/// May a client of this class see the agent seat?
///
/// The whole policy, as one pure function. Allow-listing grants; being an
/// input method refuses regardless (rule 2 is not weakenable by rule 1's
/// knob — see this module's doc).
pub fn agent_seat_visible_to(class: ClientClass) -> bool {
    class.allow_listed && !class.is_input_method
}

/// Is this seat global visible to this client?
///
/// Everything is visible to everyone except one case: the agent seat, to a
/// client the policy above does not grant it to.
fn seat_visible(client: &Client, global_data: &SeatGlobalData<DuduclawComp>) -> bool {
    if !FILTER_ARMED.load(Ordering::SeqCst) {
        return true;
    }
    if agent_seat_visible_to(client_class(client)) {
        return true;
    }
    // Only pay for the Debug sniff once the cheap cached-flag check above has
    // established that this client is actually subject to the filter.
    match sniff_seat_name(global_data) {
        // Unreadable name: fail OPEN on this axis. Hiding a seat we cannot
        // identify could hide the *human* seat from every client, which would
        // leave the whole desktop input-dead — a far worse and much more
        // confusing failure than the ones this filter exists to prevent.
        None => true,
        Some(name) => name != AGENT_SEAT_NAME,
    }
}

/// E1a-1: is the agent seat hidden from this client by this filter?
///
/// The read `crate::codrive` uses to turn "this injection will reach nobody"
/// into a reported drop instead of a silent no-op. Deliberately phrased as
/// *hidden-by-us*, not *reachable*: it answers only for the half this
/// compositor controls. A client that can see the agent seat but simply never
/// bound it is not distinguishable here, and is not this predicate's claim.
pub fn agent_seat_hidden_from(client: &Client) -> bool {
    FILTER_ARMED.load(Ordering::SeqCst) && !agent_seat_visible_to(client_class(client))
}

/// May this client bind `zwp_input_method_manager_v2` /
/// `zwp_virtual_keyboard_manager_v1`?
///
/// Default is "anyone", matching anvil, because a false negative here is
/// fatal to Chinese input (fcitx5 needs **both** managers or it silently
/// never initialises — `WaylandIMServerV2::init()`), whereas the appliance's
/// client set is entirely ours. `DUDUCLAW_COMP_IME_STRICT=1` tightens it to
/// detected input methods only, for deployments that would rather lose the
/// IME than leave a key-injection protocol open to every client.
pub fn client_may_use_input_method(client: &Client) -> bool {
    !strict_mode() || client_class(client).is_input_method
}

fn strict_mode() -> bool {
    std::env::var(IME_STRICT_ENV).map(|v| v == "1").unwrap_or(false)
}

/// Pure half of [`filter_enabled`]. Anything that is not an explicit "off"
/// leaves the filter on, including a typo: the filter is what keeps
/// third-party apps usable, so an unrecognised value must never be the thing
/// that quietly ships a known-broken desktop.
pub fn filter_enabled_from_env_value(raw: Option<&str>) -> bool {
    !matches!(
        raw.map(str::trim),
        Some(v) if v.eq_ignore_ascii_case("off")
            || v.eq_ignore_ascii_case("0")
            || v.eq_ignore_ascii_case("false")
    )
}

fn filter_enabled() -> bool {
    filter_enabled_from_env_value(std::env::var(SEAT_FILTER_ENV).ok().as_deref())
}

/// Reads back the classification made at accept time.
fn client_class(client: &Client) -> ClientClass {
    client
        .get_data::<ClientState>()
        .map(ClientState::seat_class)
        .unwrap_or_default()
}

/// Classifies a freshly accepted connection. Called once per client, from
/// `state::DuduclawComp::init_wayland_listener`, immediately after
/// `insert_client` — which is the earliest moment a `Client` exists and still
/// strictly before any of its requests are dispatched, so nothing can read
/// the flags before they are written.
///
/// Fails **closed on the agent-seat axis**: a peer whose credentials or
/// `/proc/<pid>/comm` cannot be read is not the shell as far as we can prove,
/// so it gets the human seat only. That direction costs codrive's reach into
/// an unidentifiable client; the other direction would cost that client its
/// human input, which is the ship-blocker this rule exists to close.
pub fn classify_client(client: &Client, dh: &DisplayHandle) -> (ClientClass, Option<String>) {
    let Some(pid) = client.get_credentials(dh).ok().map(|c| c.pid) else {
        tracing::warn!(
            "comp/seat: could not read a new client's peer credentials — treating it as an \
             ordinary app (human seat only, no codrive reach)"
        );
        return (ClientClass::default(), None);
    };
    let Some(comm) = read_proc_comm(pid) else {
        tracing::warn!(
            pid,
            "comp/seat: could not read /proc/<pid>/comm for a new client — treating it as an \
             ordinary app (human seat only, no codrive reach)"
        );
        return (ClientClass::default(), None);
    };
    let class = ClientClass {
        is_input_method: proc_name_matches(&comm, &ime_proc_names()),
        allow_listed: proc_name_matches(&comm, &agent_seat_proc_names()),
    };
    tracing::info!(
        pid,
        comm = %comm,
        input_method = class.is_input_method,
        sees_agent_seat = agent_seat_visible_to(class),
        "comp/seat: client classified"
    );
    (class, Some(comm))
}

fn read_proc_comm(pid: i32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .ok()
        .map(|s| s.trim_end_matches('\n').to_string())
}

/// The configured input-method process names.
fn ime_proc_names() -> Vec<String> {
    match std::env::var(IME_PROCS_ENV) {
        Ok(raw) => parse_proc_list(&raw),
        Err(_) => DEFAULT_IME_PROCS.iter().map(|s| (*s).to_string()).collect(),
    }
}

/// The configured agent-seat allow list.
fn agent_seat_proc_names() -> Vec<String> {
    match std::env::var(AGENT_SEAT_PROCS_ENV) {
        Ok(raw) => parse_proc_list(&raw),
        Err(_) => DEFAULT_AGENT_SEAT_PROCS
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
    }
}

/// Splits a comma-separated process-name list. Whitespace is trimmed and
/// empty entries dropped, so `"fcitx5,,  ibus-daemon "` is two names and
/// `""` / `" , "` is none (an empty list matches nothing).
pub fn parse_proc_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Exact, case-sensitive match against a configured name list.
///
/// Deliberately NOT a substring test (repo coding convention 2): `contains`
/// would let a process called `not-fcitx5-at-all` pass, and process names are
/// an exact-match namespace to begin with.
pub fn proc_name_matches(comm: &str, names: &[String]) -> bool {
    names.iter().any(|n| n == comm)
}

/// A `fmt::Write` sink that stops the formatting run as soon as it has a
/// complete seat name — or once [`SNIFF_CAP`] bytes have gone by, whichever
/// comes first.
///
/// Returning `Err(fmt::Error)` from `write_str` is how a sink aborts a `Debug`
/// rendering; `std`'s `DebugStruct` propagates it and stops. That is the whole
/// point: it means `SeatRc`'s `inner` field — a `Mutex` whose contents are the
/// pointer/keyboard handles and every bound `wl_seat` — is never formatted,
/// so this stays a short, allocation-bounded string operation instead of
/// walking live input state on every registry advertisement.
#[derive(Default)]
struct NameSniffer {
    buf: String,
    done: bool,
}

impl fmt::Write for NameSniffer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if self.done {
            return Err(fmt::Error);
        }
        for ch in s.chars() {
            if self.buf.len() + ch.len_utf8() > SNIFF_CAP {
                self.done = true;
                return Err(fmt::Error);
            }
            self.buf.push(ch);
        }
        if parse_seat_name(&self.buf).is_some() {
            self.done = true;
            return Err(fmt::Error);
        }
        Ok(())
    }
}

/// Extracts a seat name from a `Seat` / `SeatGlobalData` `Debug` prefix.
///
/// Both render through `SeatRc`'s `Debug` impl, whose first field is
/// `name: "<the name>"` — which is what makes the startup self-check (over
/// real `Seat`s) meaningful evidence about what `can_view` will see later.
pub fn parse_seat_name(debug_prefix: &str) -> Option<String> {
    const KEY: &str = "name: \"";
    let start = debug_prefix.find(KEY)? + KEY.len();
    let mut out = String::new();
    let mut chars = debug_prefix[start..].chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                '\'' => out.push('\''),
                // `\u{…}` and anything else unexpected: refuse rather than
                // guess. An unparsed name disarms the filter at startup or
                // fails open at runtime, both of which are defined states.
                _ => return None,
            },
            _ => out.push(c),
        }
    }
    None
}

/// Formats `value` just far enough to read its seat name back out.
fn sniff_seat_name<T: fmt::Debug>(value: &T) -> Option<String> {
    let mut sink = NameSniffer::default();
    // The `Err` is expected — it is how the sink stops the rendering.
    let _ = write!(sink, "{value:?}");
    parse_seat_name(&sink.buf)
}

// ---------------------------------------------------------------------------
// The split `delegate_seat!`
// ---------------------------------------------------------------------------
//
// smithay's `delegate_seat!(DuduclawComp)` would expand to exactly the five
// impls below, with `SeatState` supplying `can_view`'s default (`true`) for
// every client. The four `Dispatch` halves are delegated verbatim; only the
// `GlobalDispatch` half is written out, and even that forwards its `bind` to
// smithay so no binding behaviour changes — the single difference from the
// macro is the `can_view` override.

smithay::reexports::wayland_server::delegate_dispatch!(DuduclawComp: [WlSeat: SeatUserData<DuduclawComp>] => SeatState<DuduclawComp>);
smithay::reexports::wayland_server::delegate_dispatch!(DuduclawComp: [WlPointer: PointerUserData<DuduclawComp>] => SeatState<DuduclawComp>);
smithay::reexports::wayland_server::delegate_dispatch!(DuduclawComp: [WlKeyboard: KeyboardUserData<DuduclawComp>] => SeatState<DuduclawComp>);
smithay::reexports::wayland_server::delegate_dispatch!(DuduclawComp: [WlTouch: TouchUserData<DuduclawComp>] => SeatState<DuduclawComp>);

impl GlobalDispatch<WlSeat, SeatGlobalData<DuduclawComp>, DuduclawComp> for DuduclawComp {
    fn bind(
        state: &mut DuduclawComp,
        dh: &DisplayHandle,
        client: &Client,
        resource: New<WlSeat>,
        global_data: &SeatGlobalData<DuduclawComp>,
        data_init: &mut DataInit<'_, DuduclawComp>,
    ) {
        <SeatState<DuduclawComp> as GlobalDispatch<
            WlSeat,
            SeatGlobalData<DuduclawComp>,
            DuduclawComp,
        >>::bind(state, dh, client, resource, global_data, data_init)
    }

    fn can_view(client: Client, global_data: &SeatGlobalData<DuduclawComp>) -> bool {
        seat_visible(&client, global_data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::input::SeatState;

    // -- the pure parser -----------------------------------------------------

    #[test]
    fn a_plain_name_is_extracted() {
        assert_eq!(
            parse_seat_name(r#"SeatGlobalData { arc: SeatRc { name: "duduclaw-agent", inner:"#),
            Some("duduclaw-agent".to_string())
        );
    }

    #[test]
    fn the_seat_handles_own_rendering_parses_the_same_way() {
        assert_eq!(
            parse_seat_name(r#"Seat { arc: SeatRc { name: "winit", inner: Mutex {"#),
            Some("winit".to_string())
        );
    }

    #[test]
    fn an_empty_name_is_a_name() {
        assert_eq!(parse_seat_name(r#"SeatRc { name: "", inner:"#), Some(String::new()));
    }

    #[test]
    fn escapes_inside_the_name_are_decoded() {
        assert_eq!(
            parse_seat_name(r#"SeatRc { name: "a\"b\\c", inner:"#),
            Some("a\"b\\c".to_string())
        );
    }

    #[test]
    fn an_unsupported_escape_refuses_rather_than_guesses() {
        assert_eq!(parse_seat_name(r#"SeatRc { name: "a\u{1F600}b", "#), None);
    }

    #[test]
    fn a_truncated_name_yields_nothing() {
        // Exactly the state the sink is in before the closing quote arrives.
        assert_eq!(parse_seat_name(r#"SeatRc { name: "dudu"#), None);
    }

    #[test]
    fn a_rendering_without_the_field_yields_nothing() {
        assert_eq!(parse_seat_name("SeatGlobalData { arc: <opaque> }"), None);
    }

    // -- the sniffer, against real smithay types -----------------------------

    /// The load-bearing probe: a REAL `Seat<DuduclawComp>` built by the same
    /// smithay version the binary links, formatted through the same sink
    /// `can_view` uses. If smithay ever changes `SeatRc`'s `Debug` rendering,
    /// this is what fails — in CI, not on a customer's desk.
    #[test]
    fn a_real_seats_name_survives_the_round_trip() {
        let mut seat_state = SeatState::<DuduclawComp>::new();
        let agent = seat_state.new_seat(AGENT_SEAT_NAME);
        let human = seat_state.new_seat("winit");
        assert_eq!(sniff_seat_name(&agent).as_deref(), Some(AGENT_SEAT_NAME));
        assert_eq!(sniff_seat_name(&human).as_deref(), Some("winit"));
    }

    #[test]
    fn the_sniffer_stops_long_before_it_has_formatted_the_whole_seat() {
        let mut seat_state = SeatState::<DuduclawComp>::new();
        let agent = seat_state.new_seat(AGENT_SEAT_NAME);
        let mut sink = NameSniffer::default();
        let _ = write!(sink, "{agent:?}");
        assert!(
            sink.buf.len() <= SNIFF_CAP,
            "sniffed {} bytes, cap is {SNIFF_CAP}",
            sink.buf.len()
        );
        assert!(
            !sink.buf.contains("inner"),
            "the sink formatted past the name into SeatRc::inner: {}",
            sink.buf
        );
    }

    #[test]
    fn a_debug_impl_without_a_name_field_does_not_hang_or_panic() {
        #[derive(Debug)]
        #[allow(dead_code)]
        struct NoName {
            a: u32,
            b: &'static str,
        }
        assert_eq!(sniff_seat_name(&NoName { a: 1, b: "x" }), None);
    }

    // -- the self-check decision --------------------------------------------

    #[test]
    fn two_distinct_readable_names_arm_the_filter() {
        assert_eq!(
            evaluate(Some("winit".into()), Some(AGENT_SEAT_NAME.into())),
            FilterStatus::Armed
        );
    }

    #[test]
    fn an_unreadable_name_disarms() {
        assert!(matches!(
            evaluate(None, Some(AGENT_SEAT_NAME.into())),
            FilterStatus::Disarmed(_)
        ));
        assert!(matches!(
            evaluate(Some("winit".into()), None),
            FilterStatus::Disarmed(_)
        ));
    }

    #[test]
    fn an_unexpected_agent_name_disarms() {
        assert!(matches!(
            evaluate(Some("winit".into()), Some("something-else".into())),
            FilterStatus::Disarmed(_)
        ));
    }

    #[test]
    fn two_identical_names_disarm() {
        assert!(matches!(
            evaluate(Some(AGENT_SEAT_NAME.into()), Some(AGENT_SEAT_NAME.into())),
            FilterStatus::Disarmed(_)
        ));
    }

    // -- the visibility policy ----------------------------------------------

    #[test]
    fn only_an_allow_listed_non_ime_client_sees_the_agent_seat() {
        // The whole of E1a-1 + D3-c, as a truth table.
        assert!(agent_seat_visible_to(ClientClass {
            allow_listed: true,
            is_input_method: false
        }));
        // An ordinary third-party app — the E1a ship-blocker case.
        assert!(!agent_seat_visible_to(ClientClass {
            allow_listed: false,
            is_input_method: false
        }));
        // D3-c, and the reason rule 2 is not weakenable by rule 1's knob:
        // even allow-listing an input method must not grant it the seat.
        assert!(!agent_seat_visible_to(ClientClass {
            allow_listed: true,
            is_input_method: true
        }));
        assert!(!agent_seat_visible_to(ClientClass {
            allow_listed: false,
            is_input_method: true
        }));
    }

    #[test]
    fn an_unclassifiable_client_defaults_to_human_seat_only() {
        // `ClientClass::default()` is what a client whose credentials or
        // /proc entry could not be read gets, and what `client_class` falls
        // back to when a connection somehow carries no `ClientState`. It must
        // fail closed on the agent-seat axis.
        assert!(!agent_seat_visible_to(ClientClass::default()));
    }

    #[test]
    fn the_shell_is_the_only_default_agent_seat_client() {
        assert_eq!(DEFAULT_AGENT_SEAT_PROCS, &["duduclaw-shell"]);
        // The kernel truncates /proc/<pid>/comm at 15 bytes; a default that
        // could never match would silently disable the shell's exemption.
        for name in DEFAULT_AGENT_SEAT_PROCS {
            assert!(name.len() <= 15, "{name} would be truncated in /proc/<pid>/comm");
        }
    }

    // -- process-name matching ----------------------------------------------

    #[test]
    fn the_proc_list_is_split_trimmed_and_compacted() {
        assert_eq!(parse_proc_list("fcitx5,,  ibus-daemon "), vec!["fcitx5", "ibus-daemon"]);
        assert!(parse_proc_list("").is_empty());
        assert!(parse_proc_list(" , ").is_empty());
    }

    #[test]
    fn proc_names_match_exactly_never_by_substring() {
        let names = vec!["fcitx5".to_string()];
        assert!(proc_name_matches("fcitx5", &names));
        assert!(!proc_name_matches("not-fcitx5-at-all", &names));
        assert!(!proc_name_matches("fcitx5-extra", &names));
        assert!(!proc_name_matches("FCITX5", &names));
        assert!(!proc_name_matches("", &names));
    }

    #[test]
    fn an_empty_name_list_matches_nothing() {
        assert!(!proc_name_matches("fcitx5", &[]));
        // Which is exactly what makes `DUDUCLAW_COMP_AGENT_SEAT_PROCS=""` the
        // documented "hide the agent seat from everyone" setting.
        assert!(!proc_name_matches("duduclaw-shell", &[]));
    }

    // -- the kill switch ----------------------------------------------------

    #[test]
    fn the_filter_is_on_unless_explicitly_turned_off() {
        for raw in [None, Some(""), Some("  "), Some("on"), Some("1"), Some("yes"), Some("🐾")] {
            assert!(
                filter_enabled_from_env_value(raw),
                "{raw:?} must leave the filter ON — an unrecognised value must never ship a \
                 known-broken desktop"
            );
        }
        for raw in ["off", "OFF", " Off ", "0", "false", "FALSE"] {
            assert!(
                !filter_enabled_from_env_value(Some(raw)),
                "{raw:?} should turn the filter off"
            );
        }
    }
}
