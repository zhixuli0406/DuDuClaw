// DuDuClaw OS gpui shell — Shell-S0 (2026-08-19).
//
// This is the desktop shell APP binary for DuDuClaw OS (see
// `commercial/docs/DESIGN-appliance-image-*.md` / the D13 "gpui 殼" design
// note for the wider plan) — distinct from `duduclaw-native-gui`, which is
// a plain chat/management client. This crate reuses that crate's MDS theme
// tokens + component facade (`duduclaw_native_gui::{theme, mds_gpui}`, see
// that crate's `lib.rs`) rather than forking either.
//
// Surface model: `Home` is the always-present base surface (`home.rs`);
// `Launcher` / `Notifications` / `ControlCenter` are overlays (`overlay.rs`
// + its `overlay/{launcher,notifications,controlcenter}.rs` content
// modules) that render on top of it, at most one at a time, driven by the
// pure state machine in `surface.rs`. cmd-k toggles the Launcher
// specifically; Escape closes whatever overlay is currently open; clicking
// the overlay backdrop (anywhere outside the panel) also closes it — see
// `overlay.rs`'s header comment for why that no longer conflicts with
// clicking a button INSIDE the panel now that the two overlays with real
// controls (Notifications' approve/reject, ControlCenter's toggles) exist.
//
// Shell-S1 (2026-08-20) adds `oobe/` — the system-level first-run flow
// (OOBE). It sits ABOVE this Home/overlay model, not inside it: when
// `ShellView.oobe` is `Some`, it is the root's ENTIRE child (Home isn't
// rendered at all underneath it — no app chrome during OOBE, see `oobe/
// render.rs`'s own header comment for why replacing the child outright was
// chosen over layering it as another overlay). See `oobe/mod.rs`'s header
// comment for the state machine + persistence design.
//
// gpui API notes NOT already covered by `duduclaw-native-gui/src/main.rs`'s
// own gotcha list (read that first — same pinned rev, same gotchas apply):
//   - Global keybindings (`actions!` + `KeyBinding` registered via `cx.
//     bind_keys`) are NOT macOS-menu-specific despite `duduclaw-native-gui`'s
//     only existing user of this API (`native_menu.rs`) being `#[cfg(target_os
//     = "macos")]`-gated — that gate is about menu-bar UX policy (GNOME wants
//     a different affordance, not a degraded copy of the macOS menu bar),
//     not about the underlying action-dispatch mechanism, which is
//     platform-generic. cmd-k/Escape here are wired unconditionally (see the
//     "Keyboard dispatch needs a focused element" note below for where the
//     actual handlers live now).
//   - `gpui::linear_gradient(angle, from, to)` only supports a 2-stop
//     gradient and gpui has no radial-gradient / backdrop-filter / image
//     drop-shadow primitives at all — see `home.rs`'s header comment for
//     how the design board's 3-stop wallpaper gradient and two radial glow
//     blobs are approximated.
//   - `gpui::Image::from_bytes(ImageFormat::Png, bytes.to_vec())` + `Arc`
//     is how a `img(...)` element gets bytes embedded via `include_bytes!`
//     (as opposed to a filesystem path or URL) — `id` is auto-derived by
//     hashing the bytes, no manual uniqueness bookkeeping needed.
//   - **Keyboard dispatch needs a focused element, full stop.** Round 2
//     bound `cmd-k`/`escape` via `cx.bind_keys` + App-global `cx.on_action`
//     and it LOOKED complete, but nothing in the tree ever called
//     `.track_focus(...)` or `window.focus(...)` — gpui walks key events
//     along the CURRENTLY FOCUSED element's dispatch path
//     (`Window::focus_node_id_in_rendered_frame`), and with no focus ever
//     set that path is unreliable across gpui's fallback/root-node
//     resolution, which is exactly the kind of implicit behavior real
//     zed example code never leans on. Every real gpui app keeps a root
//     `FocusHandle` and focuses it right after the window opens — see
//     zed's own `crates/gpui/examples/input.rs` (`InputExample` holds a
//     `focus_handle`, its root `div()` calls `.track_focus(&self.
//     focus_handle(cx))`, and `run_example()` calls `window.focus(&view.
//     text_input.focus_handle(cx), cx)` right after creating the view) and
//     this crate's own sibling `duduclaw-native-gui/src/text_field.rs`
//     (`TextField` holds `focus_handle: FocusHandle`, `.track_focus(&self.
//     focus_handle)` on its root div, `window.focus(&this.focus_handle,
//     cx)` on mouse-down). `ShellView` now follows the identical shape:
//     `focus_handle` field + `impl Focusable` + `.track_focus(&self.
//     focus_handle)` on the root element in `Render::render`, and `main()`
//     calls `window.focus(&view.focus_handle, cx)` right after the window
//     opens (same call site/order as `input.rs`'s `run_example()`). The two
//     action handlers moved from App-global `cx.on_action` closures onto
//     `.on_action(cx.listener(...))` directly on that SAME focused root
//     element — `input.rs`'s own `TextInput`/`InputExample` put their
//     action listeners on the focused element too (`.on_action(cx.listener
//     (Self::backspace))` etc. on `TextInput`'s own `track_focus`ed div) —
//     so the actions are guaranteed to sit on the dispatch path that gpui
//     actually walks for the focused node, rather than depending on a
//     global listener whose invocation is gated by that same walk having
//     found a matching binding in the first place. Overlay content
//     (`overlay.rs` + its `overlay/*.rs` submodules) never calls
//     `window.focus(...)` itself, so opening/closing an overlay never steals
//     this root focus — cmd-k/Escape keep working with any overlay open.

mod audio;
mod fake_data;
mod gateway_client;
mod home;
mod i18n;
mod lockscreen;
mod oobe;
mod overlay;
mod palette;
mod surface;

use gpui::{
    actions, div, prelude::*, px, size, App, Bounds, Context, FocusHandle, Focusable, KeyBinding, KeyDownEvent, Keystroke, MouseButton,
    MouseDownEvent, Render, Window, WindowBounds, WindowOptions,
};
use gpui_platform::application;

use surface::{Overlay, SurfaceState};

actions!(duduclaw_shell, [ToggleLauncher, CloseOverlay, OobeNext, LockScreenNow]);

/// Diagnostic gate (`DUDUCLAW_SHELL_DIAG=1`). Kept permanently: this
/// layer-splitting toolkit (in-app keystroke dispatch, raw OS input probes,
/// bounds probes, hit/action/render logs) is what root-caused the
/// "overlay laid out one window-height offscreen" bug after three
/// screen-never-changes reports — cheap to keep, expensive to rebuild.
pub(crate) fn diag_enabled() -> bool {
    static DIAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DIAG.get_or_init(|| std::env::var("DUDUCLAW_SHELL_DIAG").is_ok_and(|v| v == "1"))
}

/// DIAG-gated absolute full-size canvas that logs its laid-out bounds at
/// prepaint — ground truth for "is this subtree actually 0×0 / offscreen".
pub(crate) fn bounds_probe(tag: &'static str) -> impl IntoElement {
    let diag = diag_enabled();
    gpui::canvas(
        move |bounds, _, _| {
            if diag {
                eprintln!("[bounds] {tag}: {bounds:?}");
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .size_full()
}

pub struct ShellView {
    surface: SurfaceState,
    /// Interactive fake state for Notifications' approve/reject cards and
    /// ControlCenter's AI-team toggles — see `overlay.rs`'s `OverlayUiState`
    /// doc comment. `pub(crate)`, not private: the click listeners that
    /// mutate it are built inside `overlay::notifications` /
    /// `overlay::controlcenter` (different modules from this one), and each
    /// needs `view.overlay_ui.xxx()` to compile from there.
    pub(crate) overlay_ui: overlay::OverlayUiState,
    /// ControlCenter's volume-slider state — Shell-S4 (2026-08-22, real
    /// `crate::audio::AudioBackend` wiring). Separate from `overlay_ui`
    /// above rather than folded into `OverlayUiState`'s own fields:
    /// deliberately touching a NEW sibling field here, not that struct's
    /// body, keeps this round's diff away from `OverlayUiState`'s existing
    /// fields (`approval_decisions` etc., owned by Notifications' own
    /// interaction round) — see `crate::audio::AudioUiState`'s own doc
    /// comment for what it holds and why it's plain data, not a gpui
    /// `Entity`.
    pub(crate) audio_ui: audio::AudioUiState,
    /// Shell-S4-lock (2026-08-22) — the lock-screen surface's own runtime
    /// state (locked?/since-when/idle clock). `Some(&self.lockscreen)` never
    /// exists standalone the way `oobe: Option<...>` does: locking is a
    /// boolean flag on always-present state, not a separate flow object,
    /// since (unlike OOBE) there is no multi-step sequence to track — see
    /// `lockscreen::LockScreenState`'s own doc comment. Mutually exclusive
    /// with `oobe` being `Some` in PRACTICE (every path that can lock —
    /// `on_lock_now` below, `lockscreen::render::maybe_auto_lock` — refuses
    /// while `self.oobe.is_some()`), but not structurally enforced by the
    /// type itself, same "an invariant enforced by every call site, not by
    /// the type" tradeoff `surface: SurfaceState` already accepts alongside
    /// `oobe` (see this file's own `Render::render` for how the two
    /// combine).
    pub(crate) lockscreen: lockscreen::LockScreenState,
    /// `Some` while the system-level first-run flow (OOBE) owns the whole
    /// screen — see this file's header comment and `oobe/mod.rs`'s own for
    /// the design. `None` (the boot-resolved normal case once OOBE has
    /// been completed) means Home renders as usual; this field and
    /// `surface`/`overlay_ui` above are mutually exclusive presentation
    /// modes, never combined.
    pub(crate) oobe: Option<oobe::OobeFlow>,
    /// Ephemeral OOBE-only UI state (e.g. the language step's accessibility
    /// panel toggle) — see `oobe::OobeUiState`'s own doc comment for why
    /// this is separate from `oobe`'s persisted `OobeState`.
    pub(crate) oobe_ui: oobe::OobeUiState,
    /// The `AccountCreate` step's two real text-input entities — see
    /// `oobe::AccountFields`'s own doc comment (`oobe/widgets.rs`) for why
    /// these live here (not inside `oobe`/`oobe_ui`, both of which are
    /// plain data with no gpui types) and why they're created once,
    /// unconditionally, at window-open time rather than lazily.
    pub(crate) oobe_account_fields: oobe::AccountFields,
    /// The `Network` step's real PSK entry field (Shell-S3, 2026-08-21) —
    /// same reasoning as `oobe_account_fields` just above (created once,
    /// unconditionally, at window-open time — see `oobe::NetworkFields`'s
    /// own doc comment for why it isn't folded into that same field).
    pub(crate) oobe_network_fields: oobe::NetworkFields,
    /// Home/overlay's own theme choice — Shell-S1 (2026-08-20). Set
    /// once at window-open time from `oobe::boot_theme(&persisted_oobe_
    /// state)` (see that fn's own doc comment for why it's read independent
    /// of `initial_oobe`'s Home-vs-OOBE decision), and updated exactly once
    /// more, in `on_oobe_next`, at the moment OOBE completes — so a THEME
    /// step pick made during THIS run reaches Home on its very first frame,
    /// not just on the next restart. `ShellView::render` resolves this into
    /// a `palette::ShellPalette` fresh every render pass (same "recompute,
    /// never cache" convention `OobeFlow::palette()` already established)
    /// and threads it into `home::render`/`overlay::render`.
    theme: oobe::ThemeChoice,
    /// Root-level focus handle — see this file's header comment ("Keyboard
    /// dispatch needs a focused element, full stop."). Tracked on the root
    /// element in `Render::render`, focused once in `main()` right after the
    /// window opens; never touched again after that (nothing else in this
    /// crate calls `window.focus(...)`), so it stays the active focus for
    /// the lifetime of the window.
    focus_handle: FocusHandle,
    /// TEMP DIAGNOSTIC (`DUDUCLAW_SHELL_DIAG=1`): layer-splitting input
    /// probe for the "keyboard/mouse completely dead" user report. When on,
    /// the first render schedules an in-app `dispatch_keystroke("cmd-k")`
    /// (tests gpui's keymap/action dispatch WITHOUT macOS event delivery)
    /// and the root element logs raw key/mouse events (tests macOS event
    /// delivery WITHOUT our handler wiring). Remove once root-caused.
    diag: bool,
    diag_scheduled: bool,
}

impl Focusable for ShellView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ShellView {
    /// `cmd-k`'s action handler — see this file's header comment for why
    /// this lives on the root element (`.on_action(cx.listener(...))` in
    /// `Render::render`) rather than as an App-global `cx.on_action`
    /// closure in `main()` like round 2 had it.
    fn on_toggle_launcher(&mut self, _action: &ToggleLauncher, _window: &mut Window, cx: &mut Context<Self>) {
        if diag_enabled() {
            eprintln!("[action] ToggleLauncher fired");
        }
        // Shell-S4-lock: `cmd-k` is a BOUND key, so a keystroke matching it
        // never reaches the root's raw `on_key_down` catch-all at all (gpui
        // consumes it entirely via action dispatch — see `lockscreen::
        // render::note_input_or_unlock`'s own doc comment for why that
        // catch-all can't cover this key) — this handler carries its own
        // identical lock-check for that reason, not relying on the
        // catch-all to unlock on cmd-k.
        if self.lockscreen.is_locked() {
            lockscreen::render::unlock(self, cx);
            return;
        }
        self.lockscreen.note_input();
        if self.oobe.is_some() {
            // The Launcher has no meaning while OOBE owns the whole
            // screen (Home isn't even rendered underneath it) — a no-op,
            // not a panic or a stale-state overlay open.
            return;
        }
        self.surface.toggle_launcher();
        cx.notify();
    }

    /// `escape`'s action handler. While OOBE is active this is its
    /// keyboard "back" binding instead (task brief: "鍵盤：Enter=繼續、
    /// Escape=返回（第一步不可返回）") — `OobeFlow::back` already refuses
    /// to move past the first step, so no extra guard is needed here.
    /// Otherwise unchanged from Shell-S0: closes whatever Home overlay is
    /// currently open (a no-op when none is). Shell-S4-lock: same
    /// self-contained lock-check as `on_toggle_launcher` above — `escape`
    /// is also a bound key, same reasoning applies.
    fn on_close_overlay(&mut self, _action: &CloseOverlay, _window: &mut Window, cx: &mut Context<Self>) {
        if diag_enabled() {
            eprintln!("[action] CloseOverlay fired");
        }
        if self.lockscreen.is_locked() {
            lockscreen::render::unlock(self, cx);
            return;
        }
        self.lockscreen.note_input();
        if let Some(flow) = self.oobe.as_mut() {
            flow.back();
            oobe::save_state(flow.state());
            cx.notify();
            return;
        }
        self.surface.close();
        cx.notify();
    }

    /// `enter`'s action handler — OOBE's keyboard "continue" binding (task
    /// brief: "Enter=繼續"). A no-op outside OOBE (Home has no Enter
    /// binding of its own this round). Shell-S4-lock: same self-contained
    /// lock-check as `on_toggle_launcher`/`on_close_overlay` above — `enter`
    /// is also a bound key.
    fn on_oobe_next(&mut self, _action: &OobeNext, _window: &mut Window, cx: &mut Context<Self>) {
        if diag_enabled() {
            eprintln!("[action] OobeNext fired");
        }
        if self.lockscreen.is_locked() {
            lockscreen::render::unlock(self, cx);
            return;
        }
        self.lockscreen.note_input();
        let Some(flow) = self.oobe.as_mut() else {
            return;
        };
        flow.next();
        oobe::save_state(flow.state());
        if flow.completed() {
            // Carry the Theme step's pick (if any was made) onto Home in
            // this SAME process — see `ShellView.theme`'s own doc comment.
            // Reading `flow.state().selections.theme` here (not `oobe::
            // boot_theme` again) is deliberate: `boot_theme` is specifically
            // about the PERSISTED file at boot, whereas this is reading the
            // in-memory flow's live selection at the exact moment it
            // transitions to completed — same source `save_state` just
            // wrote to disk two lines up, so the two never disagree.
            self.theme = flow.state().selections.theme;
            self.oobe = None;
        }
        cx.notify();
    }

    /// `cmd-l`'s action handler — the manual-lock keyboard shortcut (task
    /// brief: "手動鎖...快捷鍵"), the keyboard twin of ControlCenter's own
    /// lock button (`overlay/controlcenter.rs`'s `lock_button`). A no-op
    /// during OOBE (locking a machine mid-first-run makes no sense) —
    /// `lockscreen::render::lock_and_refresh` itself is idempotent either
    /// way (re-locking an already-locked screen is a no-op per
    /// `LockScreenState::lock`'s own doc comment), so no separate
    /// already-locked guard is needed here.
    fn on_lock_now(&mut self, _action: &LockScreenNow, _window: &mut Window, cx: &mut Context<Self>) {
        if diag_enabled() {
            eprintln!("[action] LockScreenNow fired");
        }
        if self.oobe.is_some() {
            return;
        }
        lockscreen::render::lock_and_refresh(self, cx);
    }
}

impl Render for ShellView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.diag {
            eprintln!("[render] overlay={:?}", self.surface.overlay());
        }
        if self.diag && !self.diag_scheduled {
            self.diag_scheduled = true;
            let handle = self.focus_handle.clone();
            window.on_next_frame(move |window, cx| {
                eprintln!(
                    "[diag] after first frame: is_window_active={} focus_handle.is_focused={}",
                    window.is_window_active(),
                    handle.is_focused(window)
                );
                match Keystroke::parse("cmd-k") {
                    Ok(ks) => {
                        let handled = window.dispatch_keystroke(ks, cx);
                        eprintln!("[diag] in-app dispatch_keystroke(cmd-k) handled={handled}");
                    }
                    Err(e) => eprintln!("[diag] Keystroke::parse failed: {e:?}"),
                }
            });
        }
        let mut root = div()
            .id("shell-root")
            .track_focus(&self.focus_handle)
            .key_context("Shell")
            .on_action(cx.listener(Self::on_toggle_launcher))
            .on_action(cx.listener(Self::on_close_overlay))
            .on_action(cx.listener(Self::on_oobe_next))
            .on_action(cx.listener(Self::on_lock_now))
            // Shell-S4-lock: always-on (NOT `self.diag`-gated, unlike the
            // pre-existing raw-input PROBE pair further down — these are
            // two SEPARATE listener registrations on the same element;
            // gpui's `key_down_listeners`/`mouse_down_listeners`/`mouse_
            // move_listeners` are `Vec`s that accumulate rather than
            // overwrite, confirmed against the pinned gpui rev's own
            // `elements/div.rs` before relying on this, so adding these
            // does not disturb the diagnostic pair below). Any key or click
            // wakes the screen back up while locked; while unlocked, they
            // just refresh the idle-auto-lock clock. See `lockscreen::
            // render::note_input_or_unlock`'s own doc comment for why
            // `cmd-k`/`escape`/`enter` are NOT covered by this catch-all
            // (those three go through `on_toggle_launcher`/`on_close_
            // overlay`/`on_oobe_next`'s own lock-checks instead — a bound
            // key never reaches a raw key listener on the same element).
            .on_key_down(cx.listener(|view, _ev, _window, cx| {
                lockscreen::render::note_input_or_unlock(view, cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, _ev, _window, cx| {
                    lockscreen::render::note_input_or_unlock(view, cx);
                }),
            )
            .on_mouse_move(cx.listener(|view, _ev, _window, _cx| {
                // Mouse movement alone refreshes the idle clock but never
                // unlocks by itself — task brief's own wording is "任意
                // 鍵/點擊" (any KEY or CLICK), not "any input", so idly
                // passing the cursor over a locked screen must not wake it.
                view.lockscreen.note_input();
            }))
            .relative()
            .size_full();
        // OOBE, when active, is the root's ENTIRE child — see this file's
        // header comment and `oobe/render.rs`'s own for why this replaces
        // `home::render(cx)` outright rather than layering as another
        // overlay (no app chrome during first-run). `home_palette` is
        // resolved fresh here every render pass from `self.theme` — same
        // "recompute, never cache" convention `OobeFlow::palette()`
        // establishes for OOBE itself (see `ShellView.theme`'s own doc
        // comment) — and threaded into both `home::render` below and the
        // overlay-render call further down.
        let home_palette = palette::ShellPalette::for_choice(self.theme);
        root = if let Some(flow) = &self.oobe {
            root.child(oobe::render(flow, &self.oobe_ui, &self.oobe_account_fields, &self.oobe_network_fields, cx))
        } else if self.lockscreen.is_locked() {
            // Shell-S4-lock: same "takes over the root's ENTIRE child, no
            // app chrome underneath" shape OOBE establishes above — Home
            // isn't rendered at all while locked, so there is nothing for a
            // Home overlay to sit on top of (mirrors the `self.oobe.is_
            // none()` guard on the overlay-render block further down).
            root.child(lockscreen::render::render(&self.lockscreen, &self.overlay_ui.notifications, cx))
        } else {
            root.child(home::render(home_palette, &self.overlay_ui.notifications, cx))
        };
        if self.diag {
            root = root
                .on_key_down(cx.listener(|_, ev: &KeyDownEvent, _, _| {
                    eprintln!("[probe] os key_down: {:?}", ev.keystroke);
                }))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|_, ev: &MouseDownEvent, _, _| {
                        eprintln!("[probe] os mouse_down at {:?}", ev.position);
                    }),
                );
        }
        // Guarded by `self.oobe.is_none()`: Home's overlays (Launcher/
        // Notifications/ControlCenter) never render while OOBE owns the
        // screen — Home itself isn't rendered above, so there is nothing
        // for an overlay to sit on top of. In practice `self.surface`
        // can't hold an open overlay while OOBE is active anyway (`on_
        // toggle_launcher` no-ops during OOBE, and nothing else opens an
        // overlay), but the guard costs nothing and removes the
        // possibility entirely rather than relying on that invariant.
        // Shell-S4-lock adds the identical guard for `self.lockscreen.is_
        // locked()` — `lockscreen::render::lock_and_refresh` already calls
        // `self.surface.close()` on every lock, so in practice this is the
        // same belt-and-suspenders redundancy, not a load-bearing check.
        if self.oobe.is_none() && !self.lockscreen.is_locked() {
            if let Some(active) = self.surface.overlay() {
                // Backdrop click-to-close — now a real `cx.listener` (round
                // 1's stub only logged, see that commit's own doc comment
                // for why: a plain closure can't reach `&mut ShellView`).
                // Building it via `cx.listener` first, THEN passing `cx`
                // again into `overlay::render`, keeps the two borrows of
                // `cx` sequential rather than interleaved in one
                // expression.
                let on_close = cx.listener(|view, _ev, _window, cx| {
                    if diag_enabled() {
                        eprintln!("[hit] backdrop -> close overlay");
                    }
                    view.surface.close();
                    cx.notify();
                });
                root = root.child(overlay::render(active, &self.overlay_ui, &self.audio_ui, home_palette, on_close, cx));
            }
        }
        root
    }
}

// Two more `DUDUCLAW_SHELL_*` env vars exist as of Shell-S2 round 1
// (2026-08-20, real `/api/first-run/*` claim RPC — see `oobe/claim.rs`'s own
// header comment) but are read from THEIR OWN call sites rather than here in
// `main()`, since neither one is a boot-time decision like
// `FORCE_OOBE`/`SKIP_OOBE`/`DEBUG_OOBE_STEP` above:
//   - `DUDUCLAW_SHELL_GATEWAY_URL` — overrides the gateway base URL
//     `oobe::claim::create_account` dials (default `http://127.0.0.1:18789`).
//     Read in `oobe/claim.rs`'s `gateway_base_url()`.
//   - `DUDUCLAW_SHELL_OOBE_LOCAL_ACCOUNT=1` — DEV-ONLY escape hatch: skips
//     the network claim entirely and reproduces the `AccountCreate` step's
//     original (pre-round-1) local-only click behavior, for headless smoke
//     runs with no gateway reachable. Read in `oobe/steps/account.rs`'s
//     click handler — see that file's own header comment.
// One more as of Shell-S3 (2026-08-21, real Wi-Fi backend):
//   - `DUDUCLAW_SHELL_FAKE_NET=1` — forces the `Network` step's demo Wi-Fi
//     backend regardless of platform, same shape as
//     `DUDUCLAW_SHELL_OOBE_LOCAL_ACCOUNT` above. Read in
//     `oobe/network/mod.rs`'s `select_backend()` — see that fn's own doc
//     comment.
// One more as of Shell-S4 (2026-08-22, real ControlCenter volume backend):
//   - `DUDUCLAW_SHELL_FAKE_AUDIO=1` — forces ControlCenter's demo volume
//     backend regardless of platform, same shape as `DUDUCLAW_SHELL_FAKE_NET`
//     above. Read in `audio/mod.rs`'s `select_backend()` — see that fn's own
//     doc comment.
// Two more as of Shell-S4-lock (2026-08-22, lockscreen surface):
//   - `DUDUCLAW_SHELL_LOCK_PRIVACY=none|count|full` — which privacy tier the
//     lockscreen's duty-summary card renders at (default `count`). Read
//     live (not cached at boot) in `lockscreen::privacy_from_env()`.
//   - `DUDUCLAW_SHELL_LOCK_IDLE_MINS=<N>` — idle-to-auto-lock threshold in
//     minutes, `0` disables auto-lock entirely (default `10`). Read live in
//     `lockscreen::idle_after_from_env()`.
fn main() {
    eprintln!("[main] starting duduclaw-shell S0");

    application().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1440.0), px(900.0)), cx);

        // OOBE boot-entry resolution — see `oobe::resolve_boot_flow`'s own
        // doc comment for the exact priority rules (task brief: FORCE_OOBE
        // > SKIP_OOBE > a recognized DEBUG_OOBE_STEP > the persisted
        // state's own `completed` flag). Resolved BEFORE opening the
        // window so `ShellView` is constructed already in the right mode —
        // no post-open "flash of Home then swap to OOBE".
        let persisted_oobe_state = oobe::load_state();
        // Shell-S1: read the boot-time theme choice BEFORE
        // `resolve_boot_flow` (next) consumes `persisted_oobe_state` by
        // value — `oobe::boot_theme`'s own doc comment explains why this is
        // a SEPARATE read rather than derived from `initial_oobe` (the most
        // common boot path resolves that to `None`, which carries no
        // selections at all). `ThemeChoice` is `Copy`, so reading this
        // field first doesn't need to clone the state.
        let initial_theme = oobe::boot_theme(&persisted_oobe_state);
        let force_oobe = std::env::var("DUDUCLAW_SHELL_FORCE_OOBE").ok();
        let skip_oobe = std::env::var("DUDUCLAW_SHELL_SKIP_OOBE").ok();
        let debug_oobe_step = std::env::var("DUDUCLAW_SHELL_DEBUG_OOBE_STEP").ok();
        let initial_oobe =
            oobe::resolve_boot_flow(force_oobe.as_deref(), skip_oobe.as_deref(), debug_oobe_step.as_deref(), persisted_oobe_state);
        match &initial_oobe {
            Some(flow) => eprintln!("[main] OOBE boot resolution: OOBE at {:?}", flow.current()),
            None => eprintln!("[main] OOBE boot resolution: Home (OOBE already completed or skipped)"),
        }
        eprintln!("[main] Home/overlay boot theme: {initial_theme:?}");

        let window = cx
            .open_window(
                WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
                |_window, cx| {
                    // `AccountFields::new` needs `&mut App` (creating the two
                    // `OobeTextField` entities), available here — same call
                    // site `duduclaw-native-gui/src/main.rs` creates its own
                    // `email_field`/`password_field` at, right before the
                    // `cx.new(|cx| ...)` call below shadows `cx` with
                    // `&mut Context<ShellView>`.
                    let oobe_account_fields = oobe::AccountFields::new(cx);
                    let oobe_network_fields = oobe::NetworkFields::new(cx);
                    cx.new(|cx| ShellView {
                        surface: SurfaceState::default(),
                        overlay_ui: overlay::OverlayUiState::default(),
                        audio_ui: audio::AudioUiState::default(),
                        lockscreen: lockscreen::LockScreenState::default(),
                        oobe: initial_oobe,
                        oobe_ui: oobe::OobeUiState::default(),
                        oobe_account_fields,
                        oobe_network_fields,
                        theme: initial_theme,
                        focus_handle: cx.focus_handle(),
                        diag: std::env::var("DUDUCLAW_SHELL_DIAG").is_ok_and(|v| v == "1"),
                        diag_scheduled: false,
                    })
                },
            )
            .expect("failed to open window");
        eprintln!("[main] window opened");

        // `cmd-k`/`escape`/`enter` are dispatched via `ShellView::on_
        // toggle_launcher` / `::on_close_overlay` / `::on_oobe_next`, wired
        // as `.on_action(cx.listener(...))` on the root element in
        // `Render::render` — see this file's header comment for why that
        // replaced round 2's App-global `cx.on_action` closures. `cx.
        // bind_keys` is still the right place for the actual keymap
        // registration (App-level, independent of where the action HANDLER
        // lives); `None` context matches regardless of the dispatch path's
        // context stack, same as round 2. `enter` is new in Shell-S1 (OOBE
        // keyboard "continue" — task brief: "Enter=繼續").
        cx.bind_keys([
            KeyBinding::new("cmd-k", ToggleLauncher, None),
            KeyBinding::new("escape", CloseOverlay, None),
            KeyBinding::new("enter", OobeNext, None),
            // Shell-S4-lock: manual-lock shortcut (task brief: "手動鎖...快
            // 捷鍵"), the keyboard twin of ControlCenter's own lock button.
            // Not previously bound to anything in this crate.
            KeyBinding::new("cmd-l", LockScreenNow, None),
        ]);

        // Give the root element real keyboard focus — see this file's header
        // comment ("Keyboard dispatch needs a focused element, full stop.")
        // for why this call is not optional. Same call site as zed's own
        // `crates/gpui/examples/input.rs` `run_example()`: right after the
        // window+view are created, before `cx.activate(true)`.
        let _ = window.update(cx, |view, window, cx| {
            window.focus(&view.focus_handle, cx);
        });

        // Shell-S4-lock: the idle-auto-lock watchdog — started exactly ONCE
        // here, not from `Render::render` (unlike this surface's own
        // clock-tick/stale-check timers, which self-re-arm only while
        // ALREADY locked — see `lockscreen::render::spawn_idle_watchdog`'s
        // own doc comment for why THIS one has to run continuously from
        // boot instead). Needs a `Context<ShellView>`, hence the same
        // `window.update(cx, |view, _window, cx| ...)` call shape the
        // `DUDUCLAW_SHELL_DEBUG_SURFACE` override below already uses to get
        // one post-window-open.
        let _ = window.update(cx, |_view, _window, cx| {
            lockscreen::render::spawn_idle_watchdog(cx);
        });

        // Debug-only boot override for headless smoke runs — this crate has
        // no scriptable UI-click automation (same gap
        // `duduclaw-native-gui/src/main.rs`'s own `DUDUCLAW_NATIVE_GUI_
        // DEBUG_PAGE` hook works around for that crate). Unset by default;
        // `DUDUCLAW_SHELL_DEBUG_SURFACE=launcher|notifications|
        // controlcenter|lockscreen` opens that surface immediately after
        // boot so a real render pass over its code path is observable
        // without a manual cmd-k/click/idle-wait. An unrecognized value is
        // logged and ignored, never a panic — but an EMPTY value (`export
        // DUDUCLAW_SHELL_DEBUG_SURFACE=`, as opposed to leaving it unset
        // entirely) is treated the same as unset, silently: some launch
        // scripts `export VAR=` rather than omitting the var, and printing
        // "unrecognized, ignoring" for that case would wrongly suggest a
        // typo when none occurred — unset and empty both mean "no override
        // requested", not "a bad value was supplied". `lockscreen` is
        // handled as a SEPARATE arm before falling to `Overlay::
        // from_debug_env`, not added as a fourth `Overlay` variant — see
        // `ShellView.lockscreen`'s own doc comment for why locking is a
        // flag on always-present state, not another `SurfaceState` overlay.
        match std::env::var("DUDUCLAW_SHELL_DEBUG_SURFACE") {
            Ok(raw) if raw.is_empty() => {}
            Ok(raw) if raw == "lockscreen" => {
                let _ = window.update(cx, |view, _window, cx| {
                    lockscreen::render::lock_and_refresh(view, cx);
                });
                eprintln!("[main] DUDUCLAW_SHELL_DEBUG_SURFACE=lockscreen -> locked");
            }
            Ok(raw) => match Overlay::from_debug_env(&raw) {
                Some(overlay) => {
                    let _ = window.update(cx, |view, _window, cx| {
                        view.surface.open(overlay);
                        cx.notify();
                    });
                    eprintln!("[main] DUDUCLAW_SHELL_DEBUG_SURFACE={raw} -> opened {overlay:?}");
                }
                None => {
                    eprintln!("[main] DUDUCLAW_SHELL_DEBUG_SURFACE={raw} unrecognized, ignoring");
                }
            },
            Err(_) => {}
        }

        cx.activate(true);
    });
}
