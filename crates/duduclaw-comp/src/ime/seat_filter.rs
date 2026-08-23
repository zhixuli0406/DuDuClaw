//! D3-c: keeping an input method off the **agent** seat.
//!
//! ## The problem this exists to solve
//!
//! `duduclaw-comp` runs two `wl_seat`s — the human `"winit"` seat and the
//! agent `"duduclaw-agent"` seat codrive injects through (`crate::codrive`).
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
//! ## The fix: the agent seat is invisible to input-method clients
//!
//! Wayland already has the right primitive — a global can be filtered
//! per-client, which is exactly the granularity we need: fcitx5 must not see
//! the agent seat, everyone else must. A client that never receives the
//! `wl_registry.global` event for a seat cannot bind it, so fcitx5's
//! `refreshSeat()` loop runs exactly once, over the human seat.
//!
//! ## What the D3-c probe found (2026-08-23)
//!
//! The spike report proposed reaching that filter through
//! `create_global_with_filter`. **That literal route is closed**: smithay's
//! `SeatState::new_wl_seat` uses plain `create_global`, and its
//! `SeatGlobalData<D>` has a private `arc` field with no constructor — so this
//! crate cannot build the global data and therefore cannot create the seat
//! global itself. What *is* open, and is what this module does:
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
//!   everyone, exactly as before this module existed — and codrive's own
//!   backstop (`crate::codrive`'s `paused_by_ime` guard) turns the resulting
//!   grab into a reported error instead of silently swallowed keystrokes.
//!   Degradation is visible, never silent.
//!
//! ## Identifying an input-method client
//!
//! Classification happens **once per connection**, at accept time in
//! `state::DuduclawComp::init_wayland_listener`, from the socket's
//! `SO_PEERCRED` pid (via `Client::get_credentials`, the same route
//! `codrive::window_geometry::window_pid` already uses) → `/proc/<pid>/comm`,
//! and is cached on [`crate::state::ClientState`]. `can_view` then costs one
//! atomic read.
//!
//! `/proc/<pid>/comm` is settable by the process itself, so it is not an
//! authentication mechanism — but note which way the failure leans: a client
//! that lies its way into "I am an input method" only loses sight of the agent
//! seat. There is no privilege on this side of the check to steal.

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
/// input-method detection (and therefore the filter) altogether.
pub const IME_PROCS_ENV: &str = "DUDUCLAW_COMP_IME_PROCS";

/// Env flag: when set to `1`, only clients classified as input methods may
/// bind `zwp_input_method_manager_v2` / `zwp_virtual_keyboard_manager_v1`.
/// Default is off — see [`client_may_use_input_method`].
pub const IME_STRICT_ENV: &str = "DUDUCLAW_COMP_IME_STRICT";

/// Process names (as reported by `/proc/<pid>/comm`, which the kernel
/// truncates to 15 characters) treated as input methods when
/// [`IME_PROCS_ENV`] is unset.
const DEFAULT_IME_PROCS: &[&str] = &["fcitx5", "fcitx", "ibus-daemon", "kimpanel"];

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

/// Outcome of the startup self-check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterStatus {
    /// The filter is live: input-method clients will not see the agent seat.
    Armed,
    /// The filter could not be trusted and is off. Every client sees every
    /// seat, and codrive's `paused_by_ime` backstop is the remaining defence.
    Disarmed(&'static str),
}

impl FilterStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            FilterStatus::Armed => "armed",
            FilterStatus::Disarmed(_) => "disarmed",
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
    let status = evaluate(sniff_seat_name(human), sniff_seat_name(agent));
    match &status {
        FilterStatus::Armed => {
            FILTER_ARMED.store(true, Ordering::SeqCst);
            tracing::info!(
                agent_seat = AGENT_SEAT_NAME,
                ime_procs = ?ime_proc_names(),
                strict = strict_mode(),
                "comp/ime: agent seat is hidden from input-method clients (D3-c). \
                 Override the process-name list with {}, restrict who may bind the IME \
                 managers with {}=1",
                IME_PROCS_ENV,
                IME_STRICT_ENV
            );
        }
        FilterStatus::Disarmed(reason) => {
            tracing::error!(
                reason,
                "comp/ime: agent-seat filter DISARMED — an input method will be able to \
                 grab the agent seat's keyboard, which stops codrive typing. codrive will \
                 report `paused_by_ime` instead of losing keystrokes silently. This almost \
                 always means smithay's Seat Debug rendering changed; see \
                 `ime::seat_filter`'s module doc"
            );
        }
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

/// Is this seat global visible to this client?
///
/// Everything is visible to everyone except one case: the agent seat, to a
/// client we identified as an input method.
fn seat_visible(client: &Client, global_data: &SeatGlobalData<DuduclawComp>) -> bool {
    if !FILTER_ARMED.load(Ordering::SeqCst) {
        return true;
    }
    if !client_is_input_method(client) {
        return true;
    }
    // Only pay for the Debug sniff for clients that could actually be
    // affected, i.e. after the cheap cached-flag check above.
    match sniff_seat_name(global_data) {
        // Unreadable name: fail OPEN on this axis. Hiding a seat we cannot
        // identify could hide the *human* seat from the input method, which
        // would break Chinese input outright — a worse and much more
        // confusing failure than the one codrive's backstop already reports.
        None => true,
        Some(name) => name != AGENT_SEAT_NAME,
    }
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
    !strict_mode() || client_is_input_method(client)
}

fn strict_mode() -> bool {
    std::env::var(IME_STRICT_ENV).map(|v| v == "1").unwrap_or(false)
}

/// Reads back the classification made at accept time.
fn client_is_input_method(client: &Client) -> bool {
    client
        .get_data::<ClientState>()
        .is_some_and(ClientState::is_input_method)
}

/// Classifies a freshly accepted connection. Called once per client, from
/// `state::DuduclawComp::init_wayland_listener`, immediately after
/// `insert_client` — which is the earliest moment a `Client` exists and still
/// strictly before any of its requests are dispatched, so nothing can read
/// the flag before it is written.
pub fn classify_client(client: &Client, dh: &DisplayHandle) -> bool {
    let names = ime_proc_names();
    if names.is_empty() {
        return false;
    }
    let Some(pid) = client.get_credentials(dh).ok().map(|c| c.pid) else {
        return false;
    };
    let Some(comm) = read_proc_comm(pid) else {
        return false;
    };
    let hit = proc_name_is_input_method(&comm, &names);
    if hit {
        tracing::info!(
            pid,
            comm = %comm,
            "comp/ime: client identified as an input method — the agent seat is hidden from it"
        );
    }
    hit
}

fn read_proc_comm(pid: i32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .ok()
        .map(|s| s.trim_end_matches('\n').to_string())
}

/// The configured input-method process names.
fn ime_proc_names() -> Vec<String> {
    match std::env::var(IME_PROCS_ENV) {
        Ok(raw) => parse_ime_procs(&raw),
        Err(_) => DEFAULT_IME_PROCS.iter().map(|s| (*s).to_string()).collect(),
    }
}

/// Splits the [`IME_PROCS_ENV`] value. Whitespace is trimmed and empty entries
/// dropped, so `"fcitx5,,  ibus-daemon "` is two names and `""` / `" , "` is
/// none (which turns detection, and therefore the filter, off).
pub fn parse_ime_procs(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Exact, case-sensitive match against the configured names.
///
/// Deliberately NOT a substring test (repo coding convention 2): `contains`
/// would let a process called `not-fcitx5-at-all` pass, and process names are
/// an exact-match namespace to begin with.
pub fn proc_name_is_input_method(comm: &str, names: &[String]) -> bool {
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

    // -- process-name matching ----------------------------------------------

    #[test]
    fn the_proc_list_is_split_trimmed_and_compacted() {
        assert_eq!(parse_ime_procs("fcitx5,,  ibus-daemon "), vec!["fcitx5", "ibus-daemon"]);
        assert!(parse_ime_procs("").is_empty());
        assert!(parse_ime_procs(" , ").is_empty());
    }

    #[test]
    fn proc_names_match_exactly_never_by_substring() {
        let names = vec!["fcitx5".to_string()];
        assert!(proc_name_is_input_method("fcitx5", &names));
        assert!(!proc_name_is_input_method("not-fcitx5-at-all", &names));
        assert!(!proc_name_is_input_method("fcitx5-extra", &names));
        assert!(!proc_name_is_input_method("FCITX5", &names));
        assert!(!proc_name_is_input_method("", &names));
    }

    #[test]
    fn an_empty_name_list_matches_nothing() {
        assert!(!proc_name_is_input_method("fcitx5", &[]));
    }
}
