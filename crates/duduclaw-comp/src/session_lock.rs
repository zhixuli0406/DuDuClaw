//! D9-bug3 / D9-bug4 (2026-08-24): **compositor-side session lock**.
//!
//! ## What this is, and why it lives in the compositor
//!
//! `duduclaw-shell` draws its lock screen on the `duduclaw-shell-home`
//! layer surface, which sits on the **`Background`** layer — under every
//! ordinary application window. Locking the screen with Chromium open
//! therefore left the operator looking at Chromium (`D9-bug4`), while the
//! keyboard had already been handed to the shell. The two faces of "locked"
//! disagreed, and the visible one said "not locked".
//!
//! A client cannot fix that: layer, keyboard interactivity and stacking are
//! the compositor's to decide, and a lock screen that depends on one client
//! painting over another is exactly the arrangement `ext-session-lock-v1`
//! exists to replace. So the shell now *tells* comp when the session is
//! locked (`shell_control` op `set_session_locked`), and comp enforces it:
//!
//! | face | while locked |
//! |---|---|
//! | **paint** | ordinary windows, the Alt-Tab panel and the IME candidate window are left out of the frame entirely (`decor::paint::build_output_elements`); only layer surfaces (the shell) and the cursor are drawn |
//! | **keyboard** | keys go **straight to the shell's own layer surface**, bypassing the input method's keyboard grab (see below); every system gesture except the Super+Esc emergency stop is swallowed |
//! | **pointer** | window surfaces, server-side decorations and the resize ring are unreachable — only layer surfaces can be entered, focused or clicked |
//!
//! ## Why the keyboard half is not "just focus"
//!
//! `D9-bug3`: with fcitx5 in 注音 mode, pressing a key on the locked screen
//! did nothing at all — but Super combos still arrived. That asymmetry is
//! the signature of an **input-method keyboard grab**, and reading smithay
//! 0.7.0 confirms it exactly:
//!
//! * `zwp_input_method_v2::Request::GrabKeyboard` calls
//!   `KeyboardHandle::set_grab` **once**, when fcitx5 creates its input
//!   method object (`wayland/input_method/input_method_handle.rs`). The grab
//!   is never released on text-input deactivation — it lives until the
//!   `zwp_input_method_keyboard_grab_v2` object is destroyed. So "no text
//!   field has focus" does **not** mean "fcitx5 is not holding the
//!   keyboard": it always is.
//! * `InputMethodKeyboardGrab::input` forwards the key to the input method
//!   and **never** calls `KeyboardInnerHandle`
//!   (`wayland/input_method/input_method_keyboard_grab.rs`), so no client
//!   ever sees a key directly. Keys reach applications only because fcitx5
//!   hands the ones it did not consume back through
//!   `zwp_virtual_keyboard_v1`. In 注音 mode, after the Launcher overlay was
//!   torn down, it stopped handing them back.
//! * `KeyboardHandle::input` runs the compositor's filter closure
//!   (`input_intercept`) **before** `input_forward` consults the grab
//!   (`input/keyboard/mod.rs`) — which is precisely why Super+Esc kept
//!   working while ordinary keys did not.
//!
//! That last fact is the fix. While locked, the filter closure short-circuits
//! and delivers the key itself, straight to the focused **layer** surface via
//! `KeyboardTarget` — the grab is never reached, so fcitx5 never sees a
//! keystroke typed on the lock screen. Two things fall out of that, both
//! wanted:
//!
//! 1. the "press any key to wake" gesture works in 注音 mode;
//! 2. a password is typed as literal ASCII rather than being fed into a
//!    Zhuyin composition — the same reason every mainstream lock screen
//!    disables its input method.
//!
//! Nothing about fcitx5's own state is touched (no grab is unset, no context
//! is deactivated), so composition on the desktop after unlocking is
//! byte-identical to before. The alternative — unsetting the grab for the
//! duration — was rejected because it is **not reversible from here**:
//! smithay's `InputMethodKeyboardGrab` is only reachable through
//! `InputMethodHandle`'s `pub(crate) inner`, so comp could unset the grab
//! and would then have no way to put it back.
//!
//! ## Fail-closed choices
//!
//! * Keys are delivered **only** when keyboard focus is on a layer surface.
//!   If it is on an ordinary window (which the layer-first focus rule in
//!   [`crate::layer_shell`] should already prevent), the key is dropped
//!   rather than typed into that application.
//! * The lock state is per-process, not per-connection: a shell that dies
//!   while locked leaves comp locked (windows stay hidden). That is the
//!   safe direction; the cost is that a shell crash shows a blank screen
//!   until the kiosk supervisor restarts it, and the restarted shell then
//!   announces `locked=false` at boot. Comp cannot persist a lock across a
//!   shell restart on its own — the credential check lives in the shell.
//! * The Super+Esc emergency stop is the one gesture that survives locking.
//!   A lock screen must never be able to trap the operator's only way to
//!   stop the agent.

use smithay::{
    backend::input::KeyState,
    desktop::WindowSurfaceType,
    input::keyboard::{keysyms, FilterResult, KeyboardTarget, KeysymHandle, Keysym, ModifiersState},
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{IsAlive, Serial},
};

use crate::state::DuduclawComp;

/// The compositor-level gestures `input.rs`'s human keyboard filter can
/// observe, named so [`gesture_allowed_while_locked`] can be a pure,
/// unit-testable decision instead of a chain of `if`s buried in the filter
/// closure (this crate's standing convention — see `input.rs`'s
/// `is_system_gesture_tail`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SystemGesture {
    /// Super+Esc — freeze the agent seat.
    EmergencyStop,
    /// Super+Enter — the human hands control back to the agent.
    HumanResume,
    /// Alt-Tab / Super+Tab — the MRU window switcher.
    Switcher,
    /// Super+Q — politely close the focused window.
    CloseWindow,
    /// Super+K — open the shell's global 交辦欄.
    TaskBar,
}

/// Which gestures still fire while the session is locked.
///
/// Only the emergency stop. The other four all act on, or reveal, the
/// session behind the lock screen: the switcher panel lists every open
/// window's title (disclosure), Super+Q closes an application, Super+K opens
/// the task bar, and Super+Enter resumes agent control — none of which a
/// person who has not authenticated should be able to do from the lock
/// screen. Super+Esc is deliberately exempt: it only ever *stops* something.
pub(crate) fn gesture_allowed_while_locked(gesture: SystemGesture) -> bool {
    matches!(gesture, SystemGesture::EmergencyStop)
}

/// D9-bug7 (2026-08-24, root-caused on the W5-1 VM round): whether an
/// UNRECOGNISED (not a [`SystemGesture`]) Logo/Alt-modified key event should
/// be swallowed by [`DuduclawComp::locked_key_filter`] rather than delivered
/// to the layer surface.
///
/// Pure and unit-tested on purpose — same "decision here, dispatch in the
/// caller" split [`classify_gesture`]/[`gesture_allowed_while_locked`]
/// already establish above. The one bit of behaviour this encodes is the
/// fix itself: **only a PRESS is swallowed on the strength of a held
/// modifier; a RELEASE never is**, and that asymmetry is load-bearing, not
/// an oversight — see [`DuduclawComp::locked_key_filter`]'s own doc comment
/// for the measured failure mode it closes.
pub(crate) fn should_swallow_unbound_locked_key(key_state: KeyState, modifiers: &ModifiersState) -> bool {
    key_state == KeyState::Pressed && (modifiers.logo || modifiers.alt)
}

/// Which [`SystemGesture`], if any, this key press is — mirroring exactly the
/// arms `input.rs`'s unlocked keyboard filter matches, using that module's own
/// keysym predicates so the two cannot drift apart.
///
/// Deliberately a separate classifier rather than a refactor of that filter:
/// the unlocked path also has to *act*, in a specific order, with an
/// `&mut DuduclawComp` in hand. This one only has to *name* the gesture, which
/// is what makes the locked policy a pure decision instead of a second copy of
/// the dispatch chain.
///
/// A release is never a gesture (every binding in `input.rs` fires on
/// `Pressed`), so callers pass presses only; `None` means "not one of the five"
/// — including an unrecognised Logo/Alt chord, which [`DuduclawComp::
/// locked_key_filter`] swallows anyway rather than guessing.
pub(crate) fn classify_gesture(modifiers: &ModifiersState, sym: Keysym) -> Option<SystemGesture> {
    use crate::input::{is_close_window_keysym, is_switcher_keysym, is_task_bar_keysym};

    if modifiers.logo && sym == Keysym::new(keysyms::KEY_Escape) {
        return Some(SystemGesture::EmergencyStop);
    }
    if modifiers.logo && sym == Keysym::new(keysyms::KEY_Return) {
        return Some(SystemGesture::HumanResume);
    }
    if (modifiers.logo || modifiers.alt) && is_switcher_keysym(sym) {
        return Some(SystemGesture::Switcher);
    }
    if modifiers.logo && is_close_window_keysym(sym) {
        return Some(SystemGesture::CloseWindow);
    }
    if modifiers.logo && is_task_bar_keysym(sym) {
        return Some(SystemGesture::TaskBar);
    }
    None
}

impl DuduclawComp {
    /// Whether the session shell has declared the screen locked.
    pub fn session_locked(&self) -> bool {
        self.session_locked
    }

    /// Records the new lock state and re-settles anything that depends on it.
    /// Returns `true` when the value actually changed.
    ///
    /// Two settles, both needed and both cheap:
    ///
    /// * **keyboard focus** — locking while an application window holds the
    ///   keyboard must move it to the shell, or the first key on the locked
    ///   screen would be dropped by [`Self::locked_key_target`]'s fail-closed
    ///   check. [`crate::layer_shell::DuduclawComp::settle_layer_keyboard_focus`]
    ///   already prefers layer surfaces, so this is the existing rule re-run,
    ///   not a second policy.
    /// * **pointer focus** — `PointerHandle` only learns about a new surface
    ///   from a `motion`, so without this a press that lands where the pointer
    ///   already sits would still be delivered to whatever window it had
    ///   entered *before* the lock. Re-entering at the same position with the
    ///   locked routing applied fixes that with no visible cursor movement.
    pub(crate) fn set_session_locked(&mut self, locked: bool) -> bool {
        if self.session_locked == locked {
            return false;
        }
        self.session_locked = locked;
        tracing::info!(locked, "session_lock: session lock state changed");
        // D9-bug9 (2026-08-24), M1 round: force a REAL wl_keyboard leave+
        // enter cycle on whoever currently holds keyboard focus, before
        // `settle_layer_keyboard_focus` below even runs. See
        // `Self::force_keyboard_leave_enter_cycle`'s own doc comment for the
        // full evidence chain (a source read of the pinned gpui rev) this is
        // built on — short version: gpui's Linux/Wayland backend runs its
        // OWN client-side software autorepeat timer, armed on every key
        // PRESS it receives and disarmed ONLY by a `wl_keyboard.leave`
        // event (its own source comment: "Prevent keyboard events from
        // repeating after opening e.g. a file chooser and closing it
        // quickly") — a matching RELEASE for the same keycode ALSO disarms
        // it, but `settle_layer_keyboard_focus` below never sends either:
        // the shell's layer surface almost always ALREADY holds keyboard
        // focus at the moment a lock gesture fires (that focus is
        // literally how the shell saw the keystroke that triggered the
        // lock in the first place), so its own "already-focused, no early
        // surface change" short-circuit (`held.as_ref() == Some(&surface)`)
        // means the real leave/enter pair this bug needs never gets sent.
        // Cycling focus through `None` first (this call) guarantees a
        // genuine focus transition on BOTH edges, independent of whether
        // the surface identity is about to change at all.
        self.force_keyboard_leave_enter_cycle();
        self.settle_layer_keyboard_focus(
            if locked { "session_locked" } else { "session_unlocked" },
            None,
        );
        self.resettle_pointer_focus();
        self.queue_redraw();
        true
    }

    /// D9-bug9 (2026-08-24): unconditionally drops keyboard focus to
    /// `None` (a genuine transition whenever anything is currently
    /// focused — Wayland's protocol never merges a leave+enter pair into a
    /// no-op the way re-targeting the SAME surface can), then relies on the
    /// caller's own immediate re-settle (`settle_layer_keyboard_focus` in
    /// [`Self::set_session_locked`]) to hand focus back out on the very
    /// next line.
    ///
    /// ── Why this exists — read the M1 evidence chain first ────────────────
    /// A source read of the pinned gpui rev
    /// (`gpui_linux/src/linux/wayland/client.rs`, `wl_keyboard::Event`
    /// handling) confirms gpui's Linux/Wayland platform backend runs its
    /// OWN client-side software key-repeat timer: any non-modifier key
    /// PRESS arms a `calloop` timer that keeps re-delivering a synthetic
    /// `KeyDown(is_held: true)` at the keyboard's reported rate, using the
    /// `Keystroke` (including whatever modifiers were held) captured AT ARM
    /// TIME — forever, until either (a) a RELEASE for the exact same
    /// keycode arrives, or (b) a `wl_keyboard::Event::Leave` fires, which
    /// unconditionally bumps an internal generation counter
    /// (`repeat.current_id`) that invalidates any in-flight timer
    /// regardless of keycode. Case (b) is not a guess — it is that file's
    /// own comment, verbatim: "Prevent keyboard events from repeating after
    /// opening e.g. a file chooser and closing it quickly." Gpui's authors
    /// already anticipated this exact class of bug and built the escape
    /// hatch; this compositor just wasn't pulling it at the one moment that
    /// needed it.
    ///
    /// `duduclaw-native-gui`'s own 128-byte flood-guard doc comment
    /// (`ime_input/input_state.rs`, D9-bug7/D9-bug8) already documents (a)
    /// as a known, ACCEPTED gap from the previous round — "a client-side
    /// mechanism this crate does not own (vendored dependency) and cannot
    /// patch here" — but that conclusion stopped one layer too early: this
    /// crate does not own gpui's timer, but it DOES own the Wayland
    /// `wl_keyboard` events gpui's timer listens to, and (b) is reachable
    /// entirely from here.
    ///
    /// A held-but-since-released `l` from a `cmd-l` chord that physically
    /// overlaps the lock transition (a completely ordinary human typing
    /// pattern — Cmd tends to lift a beat before the letter) leaves that
    /// timer armed with `is_held: true` copies of an `l` keystroke, which
    /// then flood straight into the just-revealed lock-screen password
    /// field once `locked_key_filter` starts forwarding plain (no longer
    /// Logo-chorded) keys to it — measured on the M1 VM round as an
    /// apparently-permanent leak that thousands of Backspaces could not
    /// visibly clear (the 128-byte insert cap in `insert_committed` only
    /// refuses GROWTH past the cap; it never stops the flood, so every
    /// Backspace's one byte of headroom was refilled by the very next
    /// synthetic repeat, pinning the field at the cap instead of emptying
    /// it).
    ///
    /// Idempotent by construction: if nothing is currently focused,
    /// `set_focus(self, None, ...)` is already a no-op inside smithay
    /// itself, so calling this on every lock AND unlock edge (not just
    /// lock) costs nothing extra on a quiet seat.
    fn force_keyboard_leave_enter_cycle(&mut self) {
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        if keyboard.current_focus().is_none() {
            return;
        }
        keyboard.set_focus(self, None, smithay::utils::SERIAL_COUNTER.next_serial());
    }

    /// Re-runs surface routing for the pointer at its current position.
    ///
    /// Split out of [`Self::set_session_locked`] only so the "why" fits in a
    /// doc comment; it has no other caller. Uses the compositor's own clock
    /// for the event timestamp, exactly like
    /// `input::DuduclawComp::seed_absolute_pointer_position` — there is no
    /// libinput event behind this motion.
    fn resettle_pointer_focus(&mut self) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let pos = pointer.current_location();
        let under = self.surface_under(pos);
        let serial = smithay::utils::SERIAL_COUNTER.next_serial();
        let time = self.start_time.elapsed().as_millis() as u32;
        self.update_close_hover(pos);
        pointer.motion(
            self,
            under,
            &smithay::input::pointer::MotionEvent {
                location: pos,
                serial,
                time,
            },
        );
        pointer.frame(self);
    }

    /// The surface a key typed on the locked screen may be delivered to — the
    /// currently focused surface, but **only** if it is a layer surface.
    ///
    /// Fail-closed by construction: an ordinary window (or no focus at all)
    /// answers `None` and the key is dropped. Locking the screen must never
    /// be the thing that types a password into a browser.
    pub(crate) fn locked_key_target(&self) -> Option<WlSurface> {
        let focused = self.seat.get_keyboard()?.current_focus()?;
        if !focused.alive() {
            return None;
        }
        self.is_layer_surface(&focused).then_some(focused)
    }

    /// The **entire** keyboard policy of a locked session, called from
    /// `input.rs`'s human keyboard filter closure before any other arm.
    ///
    /// Always intercepts: nothing typed on a locked screen is ever forwarded
    /// down smithay's ordinary path, because that path ends at the input
    /// method's keyboard grab (see this module's doc). Three outcomes:
    ///
    /// 1. Super+Esc still freezes the agent seat — [`gesture_allowed_while_locked`];
    /// 2. any other Logo/Alt chord's PRESS is swallowed, so the switcher,
    ///    Super+Q, Super+K and Super+Enter cannot be reached from the lock
    ///    screen — but see the D9-bug7 note below for why its RELEASE is not;
    /// 3. every remaining key (including every release) is delivered
    ///    **directly** to the focused layer surface, or dropped if focus is
    ///    not on one.
    ///
    /// `modifiers` is re-sent immediately before each key rather than only on
    /// change: this path bypasses `KeyboardHandle::input_forward`, which is
    /// what normally tracks `mods_changed`, and a lock screen that silently
    /// lost Shift would reject every password containing a capital letter.
    /// `wl_keyboard.modifiers` is idempotent, so re-sending costs one small
    /// event per keystroke and cannot desynchronise anything.
    ///
    /// ── D9-bug7 (2026-08-24) — the release/press asymmetry ────────────────
    /// Until this round, an unrecognised Logo/Alt chord swallowed its
    /// RELEASE the same way it swallowed its PRESS, on the reasoning (still
    /// correct for a chord that starts AND ends while already locked) that
    /// "the client never saw the press either, so nothing needs to be told
    /// it came back up." That reasoning breaks for exactly the chord that
    /// LOCKS the screen — `cmd-l` — because its PRESS is dispatched through
    /// the *unlocked* path (`session_locked()` was still `false` at that
    /// instant, before the shell's async `set_session_locked(true)` IPC call
    /// lands) and reaches the shell as an ordinary keystroke; that is what
    /// fires `LockScreenNow` in the first place. If the operator is still
    /// physically holding `l` (or Cmd) by the time the lock takes effect
    /// compositor-side, the RELEASE arrives here instead, on the *locked*
    /// path, with `modifiers.logo` still `true` — and the old code swallowed
    /// it. The shell never learns the key came back up, so its own
    /// client-side autorepeat timer (armed by the press it DID see) free-runs
    /// forever. Measured on the W5-1 VM round: a multi-thousand-character
    /// flood of `l`s into the freshly-revealed password field (`content`
    /// climbing past 1,100 masked clusters, unbounded) that eventually broke
    /// the Wayland connection outright (`duduclaw_comp::state: xdg client
    /// disconnected … reason=ConnectionClosed`), which the shell then
    /// surfaces as a clean `exit(0)` — invisible to `duduclaw-kiosk.service`'s
    /// `Restart=on-failure` (see that unit file's own D9-bug8 comment for the
    /// matching self-heal fix; the two rounds share one root symptom class:
    /// an unbounded key-repeat flood the client cannot recover from on its
    /// own).
    ///
    /// [`should_swallow_unbound_locked_key`] is where this is now decided:
    /// a PRESS still swallows on a held Logo/Alt (unchanged), a RELEASE
    /// never does. This is deliberately NOT conditioned on "was this
    /// specific release's press actually delivered" — that would need this
    /// compositor to keep a second, private copy of exactly the physical-key
    /// bookkeeping `KeyboardHandle` already keeps. Always forwarding the
    /// release is the safe simplification: a client that receives a
    /// `wl_keyboard.key` RELEASE for a keysym it has no memory of pressing
    /// (e.g. the release half of a gesture whose press WAS intercepted, like
    /// Super+Esc) is ordinary, expected Wayland-client behaviour — it is
    /// silently ignored, never inserts text and never arms a repeat timer on
    /// its own — whereas swallowing the WRONG release is what produces an
    /// unbounded flood.
    pub(crate) fn locked_key_filter(
        &mut self,
        modifiers: &ModifiersState,
        handle: KeysymHandle<'_>,
        key_state: KeyState,
        serial: Serial,
        time: u32,
    ) -> FilterResult<()> {
        if key_state == KeyState::Pressed {
            if let Some(gesture) = classify_gesture(modifiers, handle.modified_sym()) {
                if gesture_allowed_while_locked(gesture) {
                    match gesture {
                        SystemGesture::EmergencyStop => self.emergency_stop("super+esc"),
                        // Nothing else is on the allow list. Written as an
                        // explicit arm rather than `_ => {}` inside an `if`
                        // that already filtered: widening
                        // `gesture_allowed_while_locked` without deciding what
                        // the new gesture DOES here should be a compile error,
                        // not a silent no-op.
                        SystemGesture::HumanResume
                        | SystemGesture::Switcher
                        | SystemGesture::CloseWindow
                        | SystemGesture::TaskBar => {
                            tracing::warn!(
                                ?gesture,
                                "session_lock: gesture is on the locked allow list but has no \
                                 action here — suppressed"
                            );
                        }
                    }
                } else {
                    tracing::debug!(?gesture, "session_lock: system gesture suppressed by the lock");
                }
                return FilterResult::Intercept(());
            }
        }
        if should_swallow_unbound_locked_key(key_state, modifiers) {
            // An unrecognised Logo/Alt chord's PRESS. Swallowed rather than
            // delivered: a lock screen has no use for a modifier chord, and
            // guessing at one this compositor does not bind would be
            // inventing behaviour. See this fn's own D9-bug7 doc comment for
            // why the RELEASE half no longer takes this branch.
            return FilterResult::Intercept(());
        }
        let Some(surface) = self.locked_key_target() else {
            tracing::debug!(
                "session_lock: key dropped — keyboard focus is not on a layer surface"
            );
            return FilterResult::Intercept(());
        };
        // `Seat<D>` is a cheap `Arc`-backed handle (same reason
        // `state::focus_window` takes a caller-owned clone), which is what lets
        // this hold a seat and `&mut self` at the same time.
        let seat = self.seat.clone();
        KeyboardTarget::modifiers(&surface, &seat, self, *modifiers, serial);
        KeyboardTarget::key(&surface, &seat, self, handle, key_state, serial, time);
        FilterResult::Intercept(())
    }

    /// Is `surface` a mapped layer surface on any of this space's outputs?
    ///
    /// Walks every output rather than just [`Self::layout_output`]: the
    /// answer must not depend on which screen the shell happened to map its
    /// surface on. Each `layer_map_for_output` guard is taken and dropped
    /// inside one loop iteration — see `layer_shell`'s module doc on why two
    /// live guards for the same output deadlock.
    fn is_layer_surface(&self, surface: &WlSurface) -> bool {
        self.space.outputs().any(|output| {
            smithay::desktop::layer_map_for_output(output)
                .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
                .is_some()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{classify_gesture, gesture_allowed_while_locked, should_swallow_unbound_locked_key, SystemGesture};

    /// D9-bug9 (2026-08-24): `set_session_locked` must call
    /// `force_keyboard_leave_enter_cycle()` BEFORE `settle_layer_keyboard_
    /// focus` on every edge — reordering or dropping this call reintroduces
    /// the exact regression this round fixed. This can't be a live
    /// `DuduclawComp`/seat unit test (this crate has no such fixture — every
    /// other stateful method here is verified against a real VM instead,
    /// same convention this file's own `locked_key_filter` follows); a
    /// source-scan is the same "crude but load-bearing" instrument
    /// `duduclaw-shell`'s own test module already uses for gpui closures it
    /// cannot drive from a plain unit test — it cannot prove the leave/enter
    /// pair reaches the Wayland wire (that's the VM check), but it fails
    /// loudly the moment the call is removed or reordered.
    #[test]
    fn set_session_locked_forces_a_keyboard_refocus_cycle_before_settling_layer_focus() {
        let source = include_str!("session_lock.rs");
        let start = source
            .find("pub(crate) fn set_session_locked")
            .expect("set_session_locked not found in session_lock.rs");
        let body = &source[start..(start + 2200).min(source.len())];
        let refocus_at = body
            .find("self.force_keyboard_leave_enter_cycle();")
            .expect("set_session_locked no longer calls force_keyboard_leave_enter_cycle()");
        let settle_at = body
            .find("self.settle_layer_keyboard_focus(")
            .expect("set_session_locked no longer calls settle_layer_keyboard_focus");
        assert!(
            refocus_at < settle_at,
            "force_keyboard_leave_enter_cycle() must run BEFORE settle_layer_keyboard_focus — \
             settle_layer_keyboard_focus no-ops when the layer surface already holds focus (the \
             common case at lock time), so the forced None-focus edge must land first"
        );
    }
    use smithay::backend::input::KeyState;
    use smithay::input::keyboard::{keysyms, Keysym, ModifiersState};

    fn mods(logo: bool, alt: bool) -> ModifiersState {
        ModifiersState { logo, alt, ..Default::default() }
    }

    // ── D9-bug7 (2026-08-24): the release/press asymmetry ─────────────────
    // Root-caused on the W5-1 VM round: `cmd-l`'s own RELEASE, arriving here
    // after the lock takes effect while Logo is still physically held,
    // MUST reach the layer surface — swallowing it left the shell's
    // client-side autorepeat timer (armed by the press, which reached the
    // shell through the pre-lock path) with no way to learn the key came
    // back up, and it free-ran forever (measured: 1,100+ masked clusters
    // flooding the password field, ending in the Wayland connection itself
    // breaking). See `DuduclawComp::locked_key_filter`'s own doc comment for
    // the full write-up.

    #[test]
    fn an_unbound_logo_or_alt_press_is_swallowed() {
        assert!(should_swallow_unbound_locked_key(KeyState::Pressed, &mods(true, false)));
        assert!(should_swallow_unbound_locked_key(KeyState::Pressed, &mods(false, true)));
        assert!(should_swallow_unbound_locked_key(KeyState::Pressed, &mods(true, true)));
    }

    #[test]
    fn a_press_with_neither_modifier_held_is_never_swallowed_here() {
        // Not this predicate's job to decide plain keys — `locked_key_filter`
        // only reaches it after `classify_gesture` already found nothing.
        assert!(!should_swallow_unbound_locked_key(KeyState::Pressed, &mods(false, false)));
    }

    #[test]
    fn a_release_is_never_swallowed_on_the_strength_of_a_held_modifier_alone() {
        // The load-bearing regression: this is exactly the state a plain
        // `l` key-up arrives in while the operator is still holding Cmd
        // (Logo) down after the `cmd-l` chord that locked the screen.
        assert!(!should_swallow_unbound_locked_key(KeyState::Released, &mods(true, false)));
        assert!(!should_swallow_unbound_locked_key(KeyState::Released, &mods(false, true)));
        assert!(!should_swallow_unbound_locked_key(KeyState::Released, &mods(true, true)));
        assert!(!should_swallow_unbound_locked_key(KeyState::Released, &mods(false, false)));
    }

    #[test]
    fn every_bound_chord_this_compositor_has_is_classified() {
        let logo = mods(true, false);
        for (sym, want) in [
            (keysyms::KEY_Escape, SystemGesture::EmergencyStop),
            (keysyms::KEY_Return, SystemGesture::HumanResume),
            (keysyms::KEY_Tab, SystemGesture::Switcher),
            (keysyms::KEY_q, SystemGesture::CloseWindow),
            (keysyms::KEY_k, SystemGesture::TaskBar),
        ] {
            assert_eq!(
                classify_gesture(&logo, Keysym::new(sym)),
                Some(want),
                "keysym {sym:#x} with Logo held must classify as {want:?}"
            );
        }
        // Alt-Tab is the switcher too — `input.rs` widened that binding in
        // WM-3, and this classifier has to agree or Alt-Tab would fall through
        // to the "unrecognised chord" arm (still suppressed, but for the wrong
        // reason and with no log line naming it).
        assert_eq!(
            classify_gesture(&mods(false, true), Keysym::new(keysyms::KEY_Tab)),
            Some(SystemGesture::Switcher)
        );
        // xkb reports Shift+Tab as ISO_Left_Tab — the backwards direction.
        assert_eq!(
            classify_gesture(&logo, Keysym::new(keysyms::KEY_ISO_Left_Tab)),
            Some(SystemGesture::Switcher)
        );
        // Upper case, for a Caps Lock / Shift-holding operator.
        assert_eq!(classify_gesture(&logo, Keysym::new(keysyms::KEY_Q)), Some(SystemGesture::CloseWindow));
        assert_eq!(classify_gesture(&logo, Keysym::new(keysyms::KEY_K)), Some(SystemGesture::TaskBar));
    }

    #[test]
    fn an_ordinary_key_is_not_a_gesture_and_therefore_reaches_the_lock_screen() {
        let none = mods(false, false);
        for sym in [keysyms::KEY_a, keysyms::KEY_Escape, keysyms::KEY_Return, keysyms::KEY_Tab, keysyms::KEY_q, keysyms::KEY_k] {
            assert_eq!(
                classify_gesture(&none, Keysym::new(sym)),
                None,
                "keysym {sym:#x} with no modifier must be typed, not treated as a gesture"
            );
        }
    }

    /// Escape with no Logo is the ONE that matters most: it is the lock
    /// screen's own "close the power menu" key, and misclassifying it as the
    /// emergency stop would both freeze the agent seat and swallow the key.
    #[test]
    fn plain_escape_is_never_the_emergency_stop() {
        assert_eq!(classify_gesture(&mods(false, false), Keysym::new(keysyms::KEY_Escape)), None);
        assert_eq!(classify_gesture(&mods(false, true), Keysym::new(keysyms::KEY_Escape)), None);
    }

    #[test]
    fn the_emergency_stop_survives_a_locked_session() {
        assert!(gesture_allowed_while_locked(SystemGesture::EmergencyStop));
    }

    #[test]
    fn every_other_system_gesture_is_suppressed_while_locked() {
        for gesture in [
            SystemGesture::HumanResume,
            SystemGesture::Switcher,
            SystemGesture::CloseWindow,
            SystemGesture::TaskBar,
        ] {
            assert!(
                !gesture_allowed_while_locked(gesture),
                "{gesture:?} must not fire from a locked screen"
            );
        }
    }
}
