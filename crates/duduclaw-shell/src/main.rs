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

mod fake_data;
mod home;
mod i18n;
mod oobe;
mod overlay;
mod surface;

use gpui::{
    actions, div, prelude::*, px, size, App, Bounds, Context, FocusHandle, Focusable, KeyBinding, KeyDownEvent, Keystroke, MouseButton,
    MouseDownEvent, Render, Window, WindowBounds, WindowOptions,
};
use gpui_platform::application;

use surface::{Overlay, SurfaceState};

actions!(duduclaw_shell, [ToggleLauncher, CloseOverlay, OobeNext]);

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
    /// currently open (a no-op when none is).
    fn on_close_overlay(&mut self, _action: &CloseOverlay, _window: &mut Window, cx: &mut Context<Self>) {
        if diag_enabled() {
            eprintln!("[action] CloseOverlay fired");
        }
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
    /// binding of its own this round).
    fn on_oobe_next(&mut self, _action: &OobeNext, _window: &mut Window, cx: &mut Context<Self>) {
        if diag_enabled() {
            eprintln!("[action] OobeNext fired");
        }
        let Some(flow) = self.oobe.as_mut() else {
            return;
        };
        flow.next();
        oobe::save_state(flow.state());
        if flow.completed() {
            self.oobe = None;
        }
        cx.notify();
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
            .relative()
            .size_full();
        // OOBE, when active, is the root's ENTIRE child — see this file's
        // header comment and `oobe/render.rs`'s own for why this replaces
        // `home::render(cx)` outright rather than layering as another
        // overlay (no app chrome during first-run).
        root = if let Some(flow) = &self.oobe {
            root.child(oobe::render(flow, &self.oobe_ui, &self.oobe_account_fields, cx))
        } else {
            root.child(home::render(cx))
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
        if self.oobe.is_none() {
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
                root = root.child(overlay::render(active, &self.overlay_ui, on_close, cx));
            }
        }
        root
    }
}

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
        let force_oobe = std::env::var("DUDUCLAW_SHELL_FORCE_OOBE").ok();
        let skip_oobe = std::env::var("DUDUCLAW_SHELL_SKIP_OOBE").ok();
        let debug_oobe_step = std::env::var("DUDUCLAW_SHELL_DEBUG_OOBE_STEP").ok();
        let initial_oobe =
            oobe::resolve_boot_flow(force_oobe.as_deref(), skip_oobe.as_deref(), debug_oobe_step.as_deref(), persisted_oobe_state);
        match &initial_oobe {
            Some(flow) => eprintln!("[main] OOBE boot resolution: OOBE at {:?}", flow.current()),
            None => eprintln!("[main] OOBE boot resolution: Home (OOBE already completed or skipped)"),
        }

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
                    cx.new(|cx| ShellView {
                        surface: SurfaceState::default(),
                        overlay_ui: overlay::OverlayUiState::default(),
                        oobe: initial_oobe,
                        oobe_ui: oobe::OobeUiState::default(),
                        oobe_account_fields,
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
        ]);

        // Give the root element real keyboard focus — see this file's header
        // comment ("Keyboard dispatch needs a focused element, full stop.")
        // for why this call is not optional. Same call site as zed's own
        // `crates/gpui/examples/input.rs` `run_example()`: right after the
        // window+view are created, before `cx.activate(true)`.
        let _ = window.update(cx, |view, window, cx| {
            window.focus(&view.focus_handle, cx);
        });

        // Debug-only boot override for headless smoke runs — this crate has
        // no scriptable UI-click automation (same gap
        // `duduclaw-native-gui/src/main.rs`'s own `DUDUCLAW_NATIVE_GUI_
        // DEBUG_PAGE` hook works around for that crate). Unset by default;
        // `DUDUCLAW_SHELL_DEBUG_SURFACE=launcher|notifications|
        // controlcenter` opens that overlay immediately after boot so a
        // real render pass over its code path is observable without a
        // manual cmd-k/click. An unrecognized value is logged and ignored,
        // never a panic — but an EMPTY value (`export
        // DUDUCLAW_SHELL_DEBUG_SURFACE=`, as opposed to leaving it unset
        // entirely) is treated the same as unset, silently: some launch
        // scripts `export VAR=` rather than omitting the var, and printing
        // "unrecognized, ignoring" for that case would wrongly suggest a
        // typo when none occurred — unset and empty both mean "no override
        // requested", not "a bad value was supplied".
        match std::env::var("DUDUCLAW_SHELL_DEBUG_SURFACE") {
            Ok(raw) if raw.is_empty() => {}
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
