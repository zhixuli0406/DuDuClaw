//! A4-5: in which order the compositor's two `wl_seat` globals are
//! advertised to clients.
//!
//! # Why this module exists
//!
//! `duduclaw-comp` deliberately runs **two** seats (see `state.rs`'s
//! `seat` / `agent_seat` fields and `codrive/mod.rs`'s module doc): the
//! human seat (`"winit"`) and the agent seat (`"duduclaw-agent"`). That
//! separation is the structural basis of the codrive safety model —
//! freeze, emergency stop and the audit trail all key off "the agent's
//! input travels through a seat that is a different object from the
//! human's". Two `wl_seat` globals is perfectly legal Wayland, and
//! multi-seat-aware clients handle it fine (verified on real hardware:
//! `foot` receives human keystrokes normally while both globals are up).
//!
//! # The client-side bug this works around
//!
//! `duduclaw-shell` is a gpui (zed) client, and gpui's Wayland backend
//! does **not** support multiple seats. At the rev this repo pins
//! (`7a7c3e1d2f03195c5fa19bc890da330ad7f3abef`), in
//! `crates/gpui_linux/src/linux/wayland/client.rs`:
//!
//! * the state struct holds a single seat — `wl_seat: wl_seat::WlSeat, //
//!   TODO: Multi seat support` (line 309);
//! * `WaylandClient::new` walks the registry and does `seat = Some(bind(…))`
//!   for **every** `wl_seat` it sees (lines ~717-725), so the local binding
//!   keeps only the **last** one;
//! * `Dispatch<wl_seat::WlSeat>` then runs for every bound seat and, on
//!   each `Capabilities` event, **releases the previously stored keyboard
//!   and pointer** before overwriting them (keyboard: `release()` at 1651,
//!   overwrite at 1654; pointer: `release()` at 1676, overwrite at 1679).
//! * `Dispatch<wl_registry::WlRegistry>` does the same for a `wl_seat`
//!   global that appears at runtime (releases both at 1325/1328, rebinds
//!   at 1331).
//!
//! Net effect against a two-seat compositor: the **last-advertised** seat
//! wins outright and the other seat's `wl_keyboard`/`wl_pointer` are
//! destroyed moments after being created. That is exactly the observed
//! failure — with the human seat advertised first, the shell ends up
//! holding only the *agent* seat's keyboard, so it never receives a single
//! `wl_keyboard.enter` or `wl_keyboard.key` for human typing, while the
//! compositor-side path is provably healthy (Super+Esc still reaches
//! comp's own key filter, and `foot` on the same compositor types fine).
//!
//! # What we do about it
//!
//! The real fix belongs upstream in gpui (make its seat state per-seat, or
//! at minimum stop clobbering). Until that lands, the cheapest correct-
//! enough lever the compositor has is **advertisement order**: advertise
//! the agent seat first so that gpui's "last one wins" lands on the human
//! seat. Nothing about the codrive model changes — both seats still exist,
//! still carry their own keyboard/pointer, and freeze / emergency stop /
//! audit are untouched. Agent input is *not* merged into the human seat.
//!
//! # The mirrored bug — measured, not theoretical (E1a, 2026-08-23)
//!
//! This module used to say that a client which naively keeps the **first**
//! seat (rather than the last) "would be" pushed onto the agent seat by
//! `AgentFirst`, and called that hazard *theoretical* on the grounds that
//! every appliance client except the shell is multi-seat-aware.
//!
//! **That was wrong, and it cost a ship-blocker.** On the appliance VM,
//! Chromium 151 keeps exactly one seat and keeps the **first** one: under
//! `AgentFirst` it binds the *agent* seat and the human gets no pointer, no
//! keyboard and no clicks in it at all — hamburger menu dead, text fields
//! never focus, Ctrl+T inert, reproduced three times, with fcitx5 excluded
//! as a cause. The decisive control: `DUDUCLAW_COMP_SEAT_ORDER=human-first`
//! made Chromium work completely and broke the shell's Enter key. The two
//! clients are mutually exclusive under **any** single advertisement order,
//! so no value of this enum is a fix.
//!
//! # What actually fixes it: per-client visibility, not order
//!
//! E1a-1 generalised D3-c's per-client global filter
//! (`crate::ime::seat_filter`): the agent seat's `wl_seat` global is now
//! advertised **only** to allow-listed processes — by default just
//! `duduclaw-shell` — and never to an input method. A single-seat client
//! that only ever sees one seat cannot pick the wrong one, whichever end of
//! the registry list it picks from.
//!
//! That leaves this module with a much smaller job than it had: ordering now
//! matters **only** for clients that see both seats, which by default is the
//! shell alone. `AgentFirst` stays the default because that is precisely the
//! client it was built for, and it is the configuration Shell-S0…S3 verified
//! on real hardware.
//!
//! Two consequences worth stating plainly:
//!
//! * If the filter **disarms** (its startup self-check failing — see that
//!   module's doc), every client sees both seats again and the Chromium
//!   blackout above comes straight back. The disarm log says so in as many
//!   words; this is not a silent degradation.
//! * The cost of `AgentFirst` is unchanged and still narrow: a gpui client
//!   cannot be *driven* by the agent (it releases the agent seat's
//!   keyboard). With the filter armed the shell is not merely un-driveable
//!   but structurally so, which is fine — human input to the shell is
//!   non-negotiable, agent-driving the shell's own UI is not a requirement.
//!
//! `DUDUCLAW_COMP_SEAT_ORDER=human-first` still restores the pre-A4-5 order
//! in one step (useful once gpui is fixed upstream and the workaround
//! becomes pointless), and would now be harmless to every non-shell client
//! for the same reason `AgentFirst` is: they only see one seat either way.

/// Which `wl_seat` global `DuduclawComp::new` creates — and therefore
/// advertises — first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SeatAdvertiseOrder {
    /// Agent seat global first, human seat global second. **Default.**
    /// Makes a last-seat-wins client (gpui) settle on the human seat.
    ///
    /// Since E1a-1 this is only observable by clients allow-listed to see
    /// both seats — by default the shell, the one client it exists for.
    #[default]
    AgentFirst,
    /// Human seat global first, agent seat second — the order this
    /// compositor used before A4-5, and the conventional one ("the first
    /// advertised seat is the primary seat"). Correct in the abstract,
    /// but leaves gpui clients keyboard-dead against this compositor
    /// (measured: the shell's Enter key stops working).
    HumanFirst,
}

/// Environment variable that overrides the default order.
pub const SEAT_ORDER_ENV: &str = "DUDUCLAW_COMP_SEAT_ORDER";

impl SeatAdvertiseOrder {
    /// Parses the `DUDUCLAW_COMP_SEAT_ORDER` value.
    ///
    /// Kept as a pure function taking the already-read value (rather than
    /// reading the environment itself) so it is unit-testable without
    /// mutating process-global state — `std::env::set_var` is unsound to
    /// call from tests that run concurrently with anything else.
    ///
    /// Unset, empty, whitespace-only, or unrecognised values all fall back
    /// to the default (`AgentFirst`). This deliberately does **not** fail
    /// closed by refusing to boot: the value only picks between two legal
    /// Wayland configurations, neither of which is a security boundary —
    /// the seats themselves, and every codrive gate on top of them, are
    /// identical either way. A typo costing the operator a bootable
    /// desktop would be a far worse failure mode than a typo silently
    /// getting the default.
    pub fn from_env_value(raw: Option<&str>) -> Self {
        match raw.map(str::trim) {
            Some(v) if v.eq_ignore_ascii_case("human-first") || v.eq_ignore_ascii_case("human") => {
                Self::HumanFirst
            }
            Some(v) if v.eq_ignore_ascii_case("agent-first") || v.eq_ignore_ascii_case("agent") => {
                Self::AgentFirst
            }
            _ => Self::AgentFirst,
        }
    }

    /// Reads `DUDUCLAW_COMP_SEAT_ORDER` from the real environment.
    pub fn from_env() -> Self {
        Self::from_env_value(std::env::var(SEAT_ORDER_ENV).ok().as_deref())
    }

    /// True when the agent seat's global is created first.
    pub fn agent_first(self) -> bool {
        matches!(self, Self::AgentFirst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_agent_first() {
        // The whole point of A4-5: out of the box, a last-seat-wins client
        // (gpui/duduclaw-shell) must land on the HUMAN seat.
        assert_eq!(SeatAdvertiseOrder::default(), SeatAdvertiseOrder::AgentFirst);
        assert!(SeatAdvertiseOrder::default().agent_first());
    }

    #[test]
    fn unset_value_is_agent_first() {
        assert_eq!(
            SeatAdvertiseOrder::from_env_value(None),
            SeatAdvertiseOrder::AgentFirst
        );
    }

    #[test]
    fn human_first_opt_out_is_honored() {
        for raw in ["human-first", "HUMAN-FIRST", "  human-first  ", "human"] {
            assert_eq!(
                SeatAdvertiseOrder::from_env_value(Some(raw)),
                SeatAdvertiseOrder::HumanFirst,
                "{raw:?} should select HumanFirst"
            );
        }
        assert!(!SeatAdvertiseOrder::HumanFirst.agent_first());
    }

    #[test]
    fn agent_first_can_be_named_explicitly() {
        for raw in ["agent-first", "AGENT-FIRST", " agent "] {
            assert_eq!(
                SeatAdvertiseOrder::from_env_value(Some(raw)),
                SeatAdvertiseOrder::AgentFirst,
                "{raw:?} should select AgentFirst"
            );
        }
    }

    #[test]
    fn garbage_falls_back_to_default_rather_than_refusing_to_boot() {
        // See `from_env_value`'s doc comment for why this is not
        // fail-closed: both values are legal Wayland configurations and
        // neither is a security boundary, so an unbootable desktop would
        // be the worse failure.
        for raw in ["", "   ", "yes", "1", "humanfirst", "agent_first", "🐾"] {
            assert_eq!(
                SeatAdvertiseOrder::from_env_value(Some(raw)),
                SeatAdvertiseOrder::AgentFirst,
                "{raw:?} should fall back to the default"
            );
        }
    }
}
