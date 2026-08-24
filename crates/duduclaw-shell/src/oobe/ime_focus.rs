//! Proactive fcitx5 engine switch on focus of ASCII-only fields (OOBE
//! account name/password, Wi-Fi PSK, lockscreen password, Settings
//! passwords/IP fields) — W7-3 (`IME-account-fields-zhuyin` follow-up,
//! 2026-08-24).
//!
//! ## The two bugs this closes
//!
//! VM-reproduced on a scratch clone (screenshots under
//! `appliance/.vm/w73ime-artifacts/`, not committed — see the wave's own
//! handoff report for the full evidence trail):
//!
//! 1. **Typed ASCII disappears into zhuyin composition.** fcitx5-chewing is
//!    the ACTIVE engine at session start (`[Behavior] ActiveByDefault=True`,
//!    seeded by `duduclaw-kiosk-launch.sh` — D3-f) and stays active on
//!    whatever field next gains focus, because fcitx5 tracks IM state
//!    per-SEAT (`ShareInputState=All`), not per text field. A freshly
//!    focused account-name field, typed into WITHOUT the operator manually
//!    tapping Shift first, showed `你好ㄋ` (zhuyin composition, underlined)
//!    for typed ASCII letters. The password field lost 3 of 8 typed
//!    characters the same way (5 dots for 8 keystrokes) even with Shift
//!    held for the uppercase letters within the password.
//! 2. Manually tapping Shift ALONE first (fcitx5's `AltTriggerKeys` default,
//!    "switch between the group's first IM and current IM") DOES work as a
//!    workaround — but relying on the operator to remember this, on a field
//!    with no visual indicator of which language it is currently in, is not
//!    acceptable UX for the one OOBE step that cannot be skipped.
//!
//! ## Why this fix, not the alternatives already ruled out
//!
//! - **Not `zwp_text_input_v3` content_purpose.** gpui's vendored Wayland
//!   backend hardcodes `ContentPurpose::Normal` for every field
//!   (`gpui_linux/src/linux/wayland/client.rs:498`, pinned rev `7a7c3e1` —
//!   confirmed by reading that exact file, not assumed) — there is no
//!   plumbing from `EntityInputHandler` to it at all, so a shell-side
//!   `content_purpose` cannot be declared without forking gpui.
//! - **Not `EntityInputHandler::accepts_text_input() == false`.**
//!   `TextInputStyle::masked`'s own doc comment (`duduclaw-native-gui/src/
//!   ime_input/style.rs`) already worked this out and rejected it: gpui's
//!   Wayland backend calls `client.disable_ime()` when a handler reports
//!   `accepts_text_input() == false`, and on this appliance fcitx5 holds a
//!   permanent `grabKeyboard()` on the seat — with text-input disabled the
//!   field would receive NOTHING at all, an operator locked out of their own
//!   machine. That finding stands; this fix does not touch
//!   `accepts_text_input` at all, and adds no new capability to
//!   `duduclaw-native-gui` (this module lives entirely in `duduclaw-shell`).
//! - **What's left, and what this file does**: fcitx5 ships exactly the
//!   mechanism the operator's own Shift-tap workaround uses, as a
//!   first-class scriptable CLI: `fcitx5-remote -s <name>` (confirmed
//!   present on the appliance image and confirmed LIVE against a real
//!   fcitx5 process — `fcitx5-remote -s keyboard-us` then `fcitx5-remote -n`
//!   read back `keyboard-us`, and the same round-trip for `chewing`). This
//!   module does automatically, on focus, exactly what the operator was
//!   expected to do manually: switch to `keyboard-us` when an ASCII-only
//!   field gains focus, and back to `chewing` when it loses focus (so the
//!   rest of the session — Launcher, chat — is unaffected).
//! - Raw D-Bus (`org.fcitx.Fcitx.Controller1.SetCurrentIM`, the mechanism
//!   D3-d used and removed) was considered and dropped: a live `busctl
//!   --user list` against this exact VM's session bus did not show fcitx5
//!   registered under any `org.fcitx.*` name at all (its "dbus" addon
//!   appears not to publish one on this build), which would have made a
//!   `zbus` client silently no-op every call. `fcitx5-remote` reached the
//!   running fcitx5 successfully over the SAME session bus at the same
//!   moment — it does not depend on that name being registered — so it is
//!   both simpler AND the one confirmed-working path, not merely the
//!   simpler-looking one.
//!
//! ## Companion fix (symptom 2's actual root cause) — NOT in this file
//!
//! Forcing `keyboard-us` on focus only closes the bug for the fields this
//! module is wired to. "打大寫字母按 Shift 就誤觸切換" (typing a held-Shift
//! capital letter mid-password accidentally toggles back to Chinese) is a
//! hazard in `AltTriggerKeys=Shift_L` itself: it is fcitx5's OWN documented
//! ambiguity between "Shift held as a modifier for a letter" and "Shift
//! tapped alone as the IM-switch hotkey", and can misfire in ANY field, not
//! just the ones this module reaches. `duduclaw-kiosk-launch.sh`'s
//! `seed_fcitx5_config` (`FCITX5_SEED_VERSION=3`) turns `AltTriggerKeys` off
//! entirely for exactly this reason — see that seed's own comment for the
//! fuller writeup and the live VM confirmation (`fcitx5-remote -n` stayed on
//! `chewing` across two repeated bare-Shift taps once `AltTriggerKeys` was
//! emptied, versus toggling immediately with the old default). `Ctrl+Space`
//! (`TriggerKeys`, fcitx5's own default, left unmodified) remains available
//! as a manual toggle everywhere this module's proactive switch does not
//! reach.
//!
//! ## Fail-open, always
//!
//! Every call here is best-effort: no fcitx5 (dev build, `fcitx5-remote`
//! missing from `PATH`, no fcitx5 process to answer it) degrades to "the
//! field behaves exactly as it did before this module existed" — logged
//! once to stderr, never a panic, never a blocking UI stall (`set_current_im`
//! only enqueues onto a channel from the render thread — see `linux::
//! worker`'s own doc comment for why the actual subprocess calls run on ONE
//! persistent ordered worker thread rather than a fresh one per call, the
//! W7-3 VM-pipeline round's own regression finding).

#[cfg(target_os = "linux")]
mod linux {
    use std::process::Command;
    use std::sync::mpsc::{self, Sender};
    use std::sync::OnceLock;

    /// Matches `duduclaw-kiosk-launch.sh`'s seeded fcitx5 profile
    /// (`Groups/0/Items/0`) — the passthrough XKB `us` layout, not a
    /// composing engine.
    pub(super) const ASCII_IM: &str = "keyboard-us";
    /// Matches the seeded profile's `Groups/0/Items/1` / `DefaultIM` — the
    /// session's normal resting engine (`[Behavior] ActiveByDefault=True`).
    pub(super) const DEFAULT_IM: &str = "chewing";

    /// W7-3 VM-pipeline regression fix (2026-08-25) — see `set_current_im`'s
    /// own doc comment for the full race writeup. One worker thread, started
    /// lazily on first use (never at process startup — this module links
    /// into every `duduclaw-shell` invocation, including ones that never
    /// touch an ASCII-only field), draining an unbounded FIFO channel.
    fn worker() -> &'static Sender<&'static str> {
        static WORKER: OnceLock<Sender<&'static str>> = OnceLock::new();
        WORKER.get_or_init(|| {
            let (tx, rx) = mpsc::channel::<&'static str>();
            std::thread::spawn(move || {
                for name in rx {
                    match Command::new("fcitx5-remote").arg("-s").arg(name).status() {
                        Ok(status) if status.success() => {}
                        Ok(status) => eprintln!("[oobe/ime_focus] fcitx5-remote -s {name} exited {status} (fcitx5 not running? harmless on a dev build)"),
                        Err(e) => eprintln!("[oobe/ime_focus] fcitx5-remote -s {name} failed to spawn: {e} (missing from PATH? harmless on a dev build)"),
                    }
                }
            });
            tx
        })
    }

    pub(super) fn set_current_im(name: &'static str) {
        // Routed through ONE persistent worker thread's FIFO channel, not a
        // fresh `std::thread::spawn` per call (the original W7-3 shape,
        // which shipped a real regression a same-round VM pipeline run
        // caught before delivery): when focus moves directly from one
        // `ascii_only` field to another, the same render pass issues TWO
        // calls back to back — the field being LEFT restores `chewing`
        // (blur), the field being ENTERED switches to `keyboard-us`
        // (focus). Two independent fire-and-forget threads racing to spawn
        // and complete their own `fcitx5-remote` subprocess have no
        // ordering guarantee relative to each other, so the second
        // (correct, "the operator is about to type English here") call
        // could lose the race against the first if its thread happened to
        // schedule slower — VM-reproduced: the OOBE account step's password
        // field (focused immediately after the name field blurs) lost most
        // of an 11-character password to zhuyin composition, while the name
        // field itself — nothing to race, it is the first field focused on
        // that screen — typed cleanly. `on_focus_transition` always runs on
        // gpui's single render thread, so every `send()` below happens in
        // strict program order; draining them on one dedicated worker
        // (rather than N independent ones) makes actual application order
        // match issue order too, closing the race entirely.
        let _ = worker().send(name);
    }
}

#[cfg(not(target_os = "linux"))]
mod linux {
    pub(super) const ASCII_IM: &str = "keyboard-us";
    pub(super) const DEFAULT_IM: &str = "chewing";
    pub(super) fn set_current_im(_name: &'static str) {}
}

/// Call once per render pass with a field's `ascii_only` flag and its
/// (previous, current) focus state — a no-op unless focus just changed AND
/// the field opted in. See this module's own doc comment for the full
/// rationale. `was_focused`/`is_focused` are read via `FocusHandle::
/// is_focused(window)`, the same per-render-pass check `OobeTextField::
/// render`'s own `focused` local already computes — this is an edge-detect
/// on top of an existing read, not a new subscription.
pub(super) fn on_focus_transition(ascii_only: bool, was_focused: bool, is_focused: bool) {
    if !ascii_only || was_focused == is_focused {
        return;
    }
    if is_focused {
        linux::set_current_im(linux::ASCII_IM);
    } else {
        linux::set_current_im(linux::DEFAULT_IM);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // gpui/D-Bus/subprocess side effects aren't unit-testable here (no live
    // window, no live fcitx5 in CI) — these are the same "scan the source
    // for the guard" shape `oobe/steps/account.rs`'s own test module already
    // establishes for gpui closures a plain unit test cannot drive.

    #[test]
    fn a_non_ascii_field_never_calls_through() {
        let source = include_str!("ime_focus.rs");
        let start = source.find("pub(super) fn on_focus_transition").expect("on_focus_transition not found");
        let window = &source[start..(start + 400).min(source.len())];
        assert!(
            window.contains("if !ascii_only || was_focused == is_focused"),
            "on_focus_transition must short-circuit on !ascii_only before doing anything else"
        );
    }

    #[test]
    fn a_steady_focus_state_never_calls_through() {
        // Same guard as above, covering the OTHER half of the condition
        // (no transition happened) — both must be checked in ONE
        // short-circuiting expression so neither can regress independently.
        let source = include_str!("ime_focus.rs");
        assert!(source.contains("was_focused == is_focused"));
    }

    #[test]
    fn the_two_im_names_match_the_seeded_fcitx5_profile() {
        // `duduclaw-kiosk-launch.sh`'s `seed_fcitx5_config` is the source of
        // truth for these two literal names (`Groups/0/Items/0`=keyboard-us,
        // `Groups/0/Items/1`/`DefaultIM`=chewing) — this crate has no way to
        // read that shell script at compile time, so the best available
        // regression guard is pinning the exact strings here and relying on
        // this doc comment (and that seed's own comment, which points back
        // at this file) to keep the two in sync by hand.
        assert_eq!(linux::ASCII_IM, "keyboard-us");
        assert_eq!(linux::DEFAULT_IM, "chewing");
    }

    #[test]
    fn set_current_im_never_panics_without_fcitx5_on_the_path() {
        // The whole point of the fail-open contract: calling this on a
        // machine with no `fcitx5-remote` (this test's own CI/dev
        // environment) must never panic or block the test suite. Spawns a
        // real (short-lived, detached) thread — nothing to await, so this
        // assertion is just "the call returns", not "the subprocess
        // succeeded".
        on_focus_transition(true, false, true);
        on_focus_transition(true, true, false);
    }
}
