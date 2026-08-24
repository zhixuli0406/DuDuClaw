// Linux-only: the actual gpui window orchestration for `ChromeMode::
// LayerSurfaces` — see `super`'s (this file's parent, `chrome/mod.rs`)
// header comment for the whole design. Everything pure/testable lives in
// `params.rs` instead (cross-platform); this file is where it turns into
// real `cx.open_window` calls, which is why the whole file is
// `#[cfg(target_os = "linux")]`-gated at its `chrome/mod.rs` declaration
// site rather than function-by-function in here.
//
// ── Sharing ONE `ShellView` across four windows ─────────────────────────
// `SurfaceView` below is the thin per-window root view. It holds only
// `kind` (which chrome surface this window IS) and `shared: Entity
// <ShellView>` (the SAME entity every other window also holds a clone of —
// `Entity<T>` is a cheap `Rc`-like handle, so `.clone()` never duplicates
// state). Its `Render::render` reads/mutates that shared entity via
// `Entity::update(cx, |shell, shell_cx| ...)`, which is the standard gpui
// pattern for one entity read by many views — the key fact that makes it
// SAFE here is that `Context<ShellView>::listener(...)` (used throughout
// `ShellView`'s existing click/action handlers, unchanged by this file)
// produces a closure of type `impl Fn(&Event, &mut Window, &mut App)` that
// captures only a WEAK reference to the `ShellView` entity — not any
// particular window. When gpui invokes it, it hands over whichever
// `Window`/`App` the triggering event actually arrived on (verified by
// reading `Context::listener`'s own definition, `gpui/src/app/context.rs`
// line 252) — so an action/click listener built while rendering the Overlay
// window works identically when it fires from a click inside the Overlay
// window, and one built while rendering the Home window works identically
// there. No listener needs to "know" which window it will run in.
//
// Every window here observes the shared entity
// (`cx.observe(&shared, |_, _, cx| cx.notify()).detach()`, done once at
// construction) so a `cx.notify()` anywhere in `ShellView`'s existing
// methods (locking, completing OOBE, opening/closing an overlay, toggling a
// ControlCenter switch, ...) schedules a re-render of EVERY open chrome
// window, not just whichever one happens to also be the window the
// triggering event arrived on.
//
// ── The on-demand overlay window ─────────────────────────────────────────
// `SurfaceState` (`surface.rs`) already tracks "which overlay, if any, is
// open" — untouched by this round, and its many call sites across
// `home.rs`/`home_dock.rs`/`overlay/*.rs` are untouched too (most of those
// files are out of this round's scope). Rather than teach every one of
// those call sites to also open/close a window, `SurfaceView::render`
// RECONCILES the overlay window reactively, but only from the Home
// instance's own render pass (`kind == ChromeSurface::Home` — chosen
// because Home is the one window guaranteed to exist for the whole
// `LayerSurfaces` session and to re-render on every shared-state change, so
// piggybacking the reconciliation there needs no extra machinery). This
// mirrors an existing convention in this crate: `home_dock::dock()` already
// dispatches a background poll as a side effect of its own render pass
// (`schedule_running_windows_poll`) — side-effecting work from inside
// `render()` is not new here.

// `Focusable` is imported explicitly (it is NOT in gpui's `prelude`):
// `open_overlay_window` moves keyboard focus onto the launcher's query
// field via `OobeTextField::focus_handle`, which is a trait method.
use gpui::{div, prelude::*, px, AnyElement, App, Context, Entity, Focusable, MouseButton, Render, Window, WindowHandle};

use duduclaw_native_gui::theme;

use crate::palette::ShellPalette;
use crate::surface::Overlay;
use crate::{home, lockscreen, overlay, ShellView};

use super::input_region::{self, BarRegion, RegionSlot};
use super::{gpui_bridge, ChromeSurface, DESKTOP_WIDTH, DOCK_HEIGHT, MENU_BAR_HEIGHT};

/// What a successful `try_open_layer_surfaces` produced: the desktop
/// surface, which is the one window that exists for the whole
/// `LayerSurfaces` session. The menu bar and dock are NOT here — they are
/// opened lazily by the Home instance itself and then kept for the whole
/// session (see `SurfaceView::chrome_bars`).
pub(crate) struct LayerSurfaceWindows {
    pub(crate) home: WindowHandle<SurfaceView>,
}

/// The per-window root view for `ChromeMode::LayerSurfaces` — see this
/// file's header comment.
pub(crate) struct SurfaceView {
    kind: ChromeSurface,
    shared: Entity<ShellView>,
    /// Only meaningfully used by the `kind == ChromeSurface::Home`
    /// instance's own render pass — see this file's header comment on why
    /// overlay open/close is reconciled from there. Always `None` for every
    /// other `kind`.
    overlay_window: Option<(Overlay, WindowHandle<SurfaceView>)>,
    /// Whether the menu bar and dock windows have been opened yet. They are
    /// opened ONCE (lazily, from the Home instance's first render) and then
    /// kept for the whole session — hidden by dropping their input region,
    /// never by being destroyed. See `reconcile_chrome_bars`
    /// for the two approaches that came before this one and exactly how each
    /// broke on real hardware, and `apply_bar_visibility` for what "hidden"
    /// means. Always `false` for every `kind` other than `Home`.
    chrome_bars_opened: bool,
}

impl SurfaceView {
    /// Compares `self.shared`'s live `SurfaceState::overlay()` against
    /// whichever overlay window (if any) THIS instance currently has open,
    /// and closes/opens windows to match. A no-op whenever nothing changed
    /// (the common case — most render passes are unrelated to the overlay).
    fn reconcile_overlay_window(&mut self, cx: &mut Context<Self>) {
        let wanted = self.shared.read(cx).surface.overlay();
        let current = self.overlay_window.as_ref().map(|(overlay, _)| *overlay);
        if wanted == current {
            return;
        }
        if let Some((_, handle)) = self.overlay_window.take() {
            // Ignore the `Result`: a window that is already gone (e.g. the
            // operator closed it some other way this crate doesn't yet
            // expose) is not an error worth surfacing here.
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
        if let Some(overlay) = wanted {
            match open_overlay_window(cx, self.shared.clone(), overlay) {
                Ok(handle) => self.overlay_window = Some((overlay, handle)),
                Err(e) => {
                    // Fail loud, not silent: `SurfaceState` now believes an
                    // overlay is open with no window to show it, which
                    // means the next cmd-k/click that TOGGLES it may look
                    // like a no-op to the operator. Not rolled back
                    // automatically this round (that would be a second
                    // mutation path into `SurfaceState` from outside its
                    // own call sites) — see `BUILD-LINUX.md`'s new section
                    // for this as a documented limitation.
                    eprintln!("[chrome] failed to open overlay layer surface for {overlay:?}: {e}");
                }
            }
        }
    }
}

impl SurfaceView {
    /// Opens the menu bar + dock windows once, on the first render that
    /// needs them. They are never closed again — visibility is
    /// `apply_bar_visibility`'s job, applied from each bar's own render pass.
    ///
    /// Opened as a PAIR: both are gated on the one
    /// `should_hide_chrome_bars` predicate, so a half-open state would only
    /// ever be a bug. If the dock fails to open, the menu bar is rolled back
    /// too, matching `try_open_layer_surfaces`' all-or-nothing contract.
    fn reconcile_chrome_bars(&mut self, cx: &mut Context<Self>) {
        // Open ONCE, then never close. Hiding is a size/input-region change
        // applied by each bar's own render pass (`apply_bar_visibility`), not
        // a teardown. This is the THIRD approach to "make the bars go away",
        // and the two before it are recorded here so neither gets
        // reintroduced — both were caught on the real appliance, not in
        // review:
        //
        //   1. **Full-size surface, render an empty `div`, zero the exclusive
        //      zone.** Broke *pointer* input: zeroing an exclusive zone only
        //      releases the reserved work area, it does nothing about input
        //      routing, so the invisible bars kept swallowing every click
        //      inside their bands. OOBE's 繼續 button sits at y≈762 on an
        //      800 px output — inside the dock's 710–800 band — so it was
        //      simply dead while clicks mid-screen worked fine.
        //
        //   2. **Destroy the windows (`remove_window`) while hidden.** Fixed
        //      the pointer problem and broke *keyboard* input instead:
        //      destroying ANY layer-shell window stops this gpui client
        //      dispatching key events at all, until something is clicked.
        //      Reproduced with no lock screen involved (open the Launcher,
        //      close it with Escape, keyboard is dead). See
        //      `BUILD-LINUX.md`'s "gpui `wl_keyboard::Event::Leave`" note for
        //      the upstream source reading behind that.
        //
        // So: the surfaces must keep existing (or the keyboard dies) AND must
        // stop accepting input (or they eat clicks). `apply_bar_visibility`
        // does exactly that, and `remove_window` is never called on a bar.
        if self.chrome_bars_opened {
            return;
        }
        let menu_bar = match open_surface_window(cx, self.shared.clone(), ChromeSurface::MenuBar) {
            Ok(handle) => handle,
            Err(e) => {
                eprintln!("[chrome] failed to open the menu-bar layer surface: {e}");
                return;
            }
        };
        let dock = match open_surface_window(cx, self.shared.clone(), ChromeSurface::Dock) {
            Ok(handle) => handle,
            Err(e) => {
                eprintln!("[chrome] failed to open the dock layer surface: {e}");
                let _ = menu_bar.update(cx, |_, window, _| window.remove_window());
                return;
            }
        };
        // Handles deliberately dropped: they exist to be torn down, and
        // tearing a bar down is precisely what must never happen here (it
        // kills this client's keyboard — see this fn's own doc comment).
        // Keeping them would be dead state inviting exactly that mistake.
        let _ = (menu_bar, dock);
        self.chrome_bars_opened = true;
    }
}

impl Render for SurfaceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.kind == ChromeSurface::Home {
            self.reconcile_chrome_bars(cx);
            self.reconcile_overlay_window(cx);
        }
        let kind = self.kind;
        let shared = self.shared.clone();
        shared.update(cx, move |shell, shell_cx| render_surface_content(kind, shell, window, shell_cx))
    }
}

/// `true` while OOBE owns the screen or the lock screen is up — the menu
/// bar and the dock must then be invisible AND must not accept input.
fn should_hide_chrome_bars(shell: &ShellView) -> bool {
    shell.oobe.is_some() || shell.lockscreen.is_locked()
}

/// Applies a chrome bar's visibility WITHOUT creating or destroying its
/// window — see `SurfaceView::reconcile_chrome_bars` for the two earlier
/// approaches this replaced and exactly how each one failed.
///
/// Hidden is two things together, and both are needed:
///
/// * **empty input region** — `set_input_region(Some(&[]))` builds a
///   `wl_region` with no rectangles in it, so the surface accepts no pointer
///   or touch input at all and clicks fall straight through to whatever is
///   beneath. This is the part that fixes approach 1, and it is exact: there
///   is no leftover 1 px of clickable area to reason about. (Keyboard needs
///   no equivalent — both bars are created with
///   `KeyboardInteractivity::None` and never take keyboard focus.)
/// * **zero exclusive zone** — releases the reserved work area so application
///   windows get the whole output while the bars are away.
///
/// Shown restores the exclusive zone, and restores an input region that
/// depends on the bar:
///
/// * The **menu bar** paints its full 30 px band edge to edge, so the whole
///   surface is exactly what it should claim — `BarRegion::Full`.
/// * The **dock** does NOT. Its surface spans the output's full width at
///   `DOCK_HEIGHT`, but it only paints a centred pill; the transparent
///   remainder of the band must fall through to the window underneath (D9-bug
///   2026-08-24 — Chromium's first-run ToS button sits inside that band and
///   was unclickable). So this deliberately leaves the dock's shown region
///   ALONE: it is owned by the dock's own prepaint listener, which reads the
///   pill's real laid-out rect (`home::render_dock` ->
///   `home_dock::dock_surface` -> `chrome::input_region::shown_region_for`)
///   and therefore tracks the pill widening/narrowing as apps come and go.
///   The hidden path above still applies here — `BarRegion::Empty` wins over
///   whatever rect was last applied, and the listener does not run at all
///   while the bar renders nothing.
///
/// Deliberately does **not** resize the surface. Shrinking the hidden bars to
/// 1×1 was tried and reverted: these surfaces are anchored to their edges, so
/// the compositor owns their width, and calling `Window::resize` with this
/// crate's nominal `DESKTOP_WIDTH` (1440) on the appliance's actual 1280-wide
/// output re-laid the chrome out for the wrong width — the menu bar's
/// right-hand status items landed off-screen and the dock shifted. The empty
/// input region already achieves everything the resize was there for.
fn apply_bar_visibility(window: &mut Window, slot: RegionSlot, hidden: bool, shown_height: f32) {
    if hidden {
        input_region::apply(window, slot, BarRegion::Empty);
        window.set_exclusive_zone(px(0.));
    } else {
        if slot != RegionSlot::Dock {
            input_region::apply(window, slot, BarRegion::Full);
        }
        window.set_exclusive_zone(px(shown_height));
    }
}

/// Renders whichever slice of `shell`'s state the window identified by
/// `kind` is responsible for. Called from inside `shared.update(...)`, so
/// `shell`/`cx` here are `&mut ShellView`/`&mut Context<ShellView>` — the
/// EXACT types `ShellView`'s own pre-existing methods (`render_root`,
/// `on_toggle_launcher`, ...) already expect, unchanged by this file.
fn render_surface_content(kind: ChromeSurface, shell: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) -> AnyElement {
    let palette = ShellPalette::for_choice(shell.theme);
    // See `main.rs`'s own `Render::render` for why this is set fresh on
    // every render pass rather than cached — same "recompute, never cache"
    // convention, just now repeated once per open window instead of once
    // per frame. `oobe::widgets::OobeTextField` (used by the lock screen's
    // password field and the Launcher's search box, both of which can now
    // render in either the Home or the Overlay window) reads this global,
    // so every window that might contain one has to publish it before
    // rendering.
    cx.set_global(palette);
    let hide_bars = should_hide_chrome_bars(shell);

    match kind {
        ChromeSurface::MenuBar => {
            apply_bar_visibility(window, RegionSlot::MenuBar, hide_bars, MENU_BAR_HEIGHT);
            if hide_bars {
                div().relative().size_full().into_any_element()
            } else {
                div()
                    .relative()
                    .size_full()
                    .font(theme::app_font())
                    .child(home::render_menu_bar(palette, &shell.overlay_ui.notifications, cx))
                    .into_any_element()
            }
        }
        ChromeSurface::Dock => {
            apply_bar_visibility(window, RegionSlot::Dock, hide_bars, DOCK_HEIGHT);
            if hide_bars {
                div().relative().size_full().into_any_element()
            } else {
                div()
                    .relative()
                    .size_full()
                    .font(theme::app_font())
                    .child(home::render_dock(
                        palette,
                        &shell.running_windows,
                        &shell.installed_apps,
                        &shell.overlay_ui.notifications,
                        &shell.overlay_ui.task_progress,
                        cx,
                    ))
                    .into_any_element()
            }
        }
        // `single_window: false` — see `ShellView::render_root`'s own doc
        // comment for exactly what that skips (menu-bar/dock children, the
        // trailing overlay-render block) and what it keeps (OOBE branch,
        // lock-screen branch, focus tracking, every action/key/mouse
        // listener).
        ChromeSurface::Home => {
            // D9-bug3 follow-up (2026-08-24): keep gpui keyboard focus on the
            // Home window while the screen is locked and the password prompt
            // has not been revealed yet, so the root's `on_key_down` catch-all
            // (`note_input_or_reveal`) actually fires on the first key — i.e.
            // "按任意鍵喚醒" works.
            //
            // Why it is needed only in `LayerSurfaces` mode, and only here:
            // when the lock is triggered from an OVERLAY (Super+K opens the
            // Launcher, then Super+L locks), the compositor correctly moves
            // wl_keyboard focus onto this Home surface as the Launcher overlay
            // is torn down (`duduclaw-comp`'s `session_lock` + layer-focus
            // settle) — but gpui focus is per-window, and the destroyed
            // Launcher window was the one that held it. Nothing re-claimed it
            // for the Home window, so keys reached the surface (verified: the
            // compositor delivers them) but landed on no focused element and
            // the catch-all never ran. Measured on the appliance VM: after a
            // Launcher-initiated lock, only a CLICK woke the field; a keypress
            // did nothing. A lock triggered directly from Home never hit this
            // because the Home window already held focus.
            //
            // Gated on `!prompt_visible`: once the field is shown,
            // `reveal_and_focus` owns focus (it moves it onto the password
            // field, which lives in THIS window), and re-claiming it for the
            // root here would fight that and swallow typed characters.
            // Idempotent when focus is already on this window's root.
            if shell.lockscreen.is_locked() && !shell.lockscreen.prompt_visible() {
                let handle = shell.focus_handle.clone();
                window.focus(&handle, cx);
            }
            shell.render_root(window, cx, false).into_any_element()
        }
        ChromeSurface::Overlay(_) => render_overlay_content(shell, window, cx, palette),
    }
}

/// The Overlay window's ENTIRE content — backdrop + panel for whichever
/// overlay `shell.surface.overlay()` currently names (the live source of
/// truth, not this window's own captured `kind`, though the two can never
/// disagree by construction — see `reconcile_overlay_window`). Mirrors the
/// action/key/mouse listener chain `ShellView::render_root` attaches to the
/// Home window's own root, MINUS the OOBE/lock-screen branching (an overlay
/// window is never open during either state — every UI element that opens
/// one only renders when neither is active) and minus the diag bounds-probe
/// pair (this window has no "screen laid out one window-height offscreen"
/// history to guard against).
fn render_overlay_content(shell: &mut ShellView, _window: &mut Window, cx: &mut Context<ShellView>, palette: ShellPalette) -> AnyElement {
    let Some(active) = shell.surface.overlay() else {
        // The reconciler is about to close this window on Home's next
        // render pass (or already has, and this is one last stale frame in
        // flight) — render nothing rather than stale/wrong content.
        return div().relative().size_full().into_any_element();
    };

    let on_close = cx.listener(|view, _ev, window, cx| {
        view.surface.close();
        view.settle_launcher_query(window, cx);
        view.pointer_ui.reset();
        // A2 (2026-08-23): the 共駕 row re-reads on its next open too — same
        // reasoning `main.rs`'s own overlay-close paths document, and this
        // chrome mode has to carry it or the refresh would work only on the
        // other one.
        view.overlay_ui.codrive.reset();
        // D4a-6 (2026-08-24): same reasoning, one row further — see
        // `overlay::wifi_tile::WifiTileState::reset`.
        view.overlay_ui.wifi_tile.reset();
        cx.notify();
    });

    div()
        .id("shell-overlay-root")
        .relative()
        .size_full()
        .font(theme::app_font())
        .track_focus(&shell.focus_handle)
        .key_context("Shell")
        .on_action(cx.listener(ShellView::on_toggle_launcher))
        .on_action(cx.listener(ShellView::on_close_overlay))
        .on_action(cx.listener(ShellView::on_oobe_next))
        .on_action(cx.listener(ShellView::on_lock_now))
        // D9 (2026-08-23): Tab/Shift-Tab field traversal — the layer-surface
        // mode's copy of the pair `main.rs`'s `render_root` also registers.
        // Both chrome modes must carry it or the shortcut would work only on
        // whichever path happened to come up.
        .on_action(cx.listener(ShellView::on_focus_next))
        .on_action(cx.listener(ShellView::on_focus_prev))
        .on_key_down(cx.listener(|view, _ev, window, cx| {
            lockscreen::render::note_input_or_reveal(view, window, cx);
        }))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|view, _ev, window, cx| {
                lockscreen::render::note_input_or_reveal(view, window, cx);
            }),
        )
        .on_mouse_move(cx.listener(|view, _ev, _window, _cx| {
            view.lockscreen.note_input();
        }))
        .child(overlay::render(
            active,
            &shell.overlay_ui,
            &shell.audio_ui,
            &shell.installed_apps,
            &shell.pointer_ui,
            &shell.launcher_query_field,
            // D4b (2026-08-23): the 系統設定 app's own state/fields —
            // `overlay::render` grew these two parameters after this
            // file's own header comment was written; kept in sync with
            // `main.rs`'s `render_root`'s own (`SingleFullscreen`-mode)
            // call to the same function.
            &shell.settings_ui,
            &shell.settings_fields,
            // D6 (2026-08-23): third-party app notifications — kept in sync
            // with `main.rs`'s `render_root` call to the same function, same
            // as the two `settings_*` parameters above it.
            &shell.notify_center,
            palette,
            on_close,
            cx,
        ))
        .into_any_element()
}

/// Opens one persistent (`MenuBar`/`Dock`/`Home`) or on-demand (`Overlay`)
/// chrome window. Every `SurfaceView` observes `shared` once at
/// construction — see this file's header comment.
fn open_surface_window(cx: &mut App, shared: Entity<ShellView>, kind: ChromeSurface) -> gpui::Result<WindowHandle<SurfaceView>> {
    let params = super::layer_params_for(kind);
    let options = gpui_bridge::to_window_options(&params, DESKTOP_WIDTH);
    cx.open_window(options, move |_window, cx| {
        cx.new(|cx| {
            let subscription = cx.observe(&shared, |_this, _shared, cx| cx.notify());
            subscription.detach();
            SurfaceView { kind, shared: shared.clone(), overlay_window: None, chrome_bars_opened: false }
        })
    })
}

/// Opens the on-demand overlay window and gives it real keyboard focus —
/// see this file's header comment on why the Launcher's search box can only
/// be correctly focused HERE (its own window, now actually open), not at
/// the `SurfaceState::open(Overlay::Launcher)` call site that triggered
/// this (that site's own `window` parameter names whichever OTHER window
/// the click/keystroke arrived on).
fn open_overlay_window(cx: &mut App, shared: Entity<ShellView>, overlay: Overlay) -> gpui::Result<WindowHandle<SurfaceView>> {
    let handle = open_surface_window(cx, shared.clone(), ChromeSurface::Overlay(overlay))?;
    let _ = handle.update(cx, move |_view, window, cx| {
        let focus_handle = if overlay == Overlay::Launcher {
            shared.read(cx).launcher_query_field.field.read(cx).focus_handle(cx)
        } else {
            shared.read(cx).focus_handle.clone()
        };
        window.focus(&focus_handle, cx);
    });
    Ok(handle)
}

/// Opens the menu bar / dock / desktop windows, all-or-nothing: if any of
/// the three fails (the realistic failure is the FIRST one, since a
/// compositor either supports `zwlr_layer_shell_v1` or it doesn't — but
/// every one is checked independently rather than assuming that), whatever
/// already opened is torn back down before returning the error, so a caller
/// falling back to `SingleFullscreen` never has to reason about a partial
/// `LayerSurfaces` state left behind — see `boot_windows`.
fn try_open_layer_surfaces(cx: &mut App, shared: Entity<ShellView>) -> gpui::Result<LayerSurfaceWindows> {
    // Only the desktop surface is opened here. The menu bar and dock are
    // opened by the Home instance's first render pass, via
    // `SurfaceView::reconcile_chrome_bars` — they must NOT exist while OOBE
    // or the lock screen owns the screen (see `SurfaceView::chrome_bars` for
    // the P0 that taught us), and at boot that is exactly the common case.
    //
    // This is still a complete layer-shell support probe: a compositor
    // either speaks `zwlr_layer_shell_v1` or it does not, so if the desktop
    // surface opens, the bars will too. Their own failures are handled
    // (and logged) where they are opened rather than by falling the whole
    // session back to `SingleFullscreen` after it has already started.
    let home = open_surface_window(cx, shared.clone(), ChromeSurface::Home)?;
    Ok(LayerSurfaceWindows { home })
}

/// The Linux-side `ChromeMode::SingleFullscreen` fallback — MUST stay
/// byte-identical to `main.rs`'s own `#[cfg(not(target_os = "linux"))]`
/// branch (macOS's only path). Duplicated rather than shared across the
/// `#[cfg]` boundary because the two live in genuinely different
/// compilation units (this whole module doesn't exist on macOS) and the
/// six lines involved are cheaper to keep in sync by comment convention
/// than to thread a shared helper across that boundary for.
fn open_single_fullscreen_fallback(cx: &mut App, shared: Entity<ShellView>) {
    let window = cx
        .open_window(
            gpui::WindowOptions {
                window_bounds: Some(gpui::WindowBounds::Windowed(gpui::Bounds::centered(
                    None,
                    gpui::size(px(DESKTOP_WIDTH), px(super::DESKTOP_HEIGHT)),
                    cx,
                ))),
                app_id: Some(super::SHELL_APP_ID.to_string()),
                ..Default::default()
            },
            move |_window, _cx| shared.clone(),
        )
        .expect("SingleFullscreen fallback must always be able to open a plain toplevel window");
    let _ = window.update(cx, |view, window, cx| {
        window.focus(&view.focus_handle, cx);
    });
}

/// The one entry point `main.rs` calls on Linux. Attempts `LayerSurfaces`
/// unless overridden (`DUDUCLAW_SHELL_NO_LAYER_SHELL=1`), falling back to
/// `SingleFullscreen` — with ZERO visual regression, since that fallback is
/// this crate's original, unmodified single-window path — if the
/// compositor doesn't support `zwlr_layer_shell_v1` (e.g. weston's headless
/// backend, per `BUILD-LINUX.md`'s B-② finding). `chrome::set_active_mode`
/// is called exactly once, at the end, with whichever mode this process
/// ACTUALLY ended up running — see that function's own doc comment for why
/// it is never called optimistically before a fallback might still
/// downgrade it.
pub(crate) fn boot_windows(cx: &mut App, shared: Entity<ShellView>) {
    let no_layer_shell = std::env::var("DUDUCLAW_SHELL_NO_LAYER_SHELL").ok();
    let desired = super::desired_chrome_mode(true, no_layer_shell.as_deref());

    let final_mode = if desired == super::ChromeMode::LayerSurfaces {
        match try_open_layer_surfaces(cx, shared.clone()) {
            Ok(windows) => {
                eprintln!("[chrome] layer-shell chrome surfaces opened (menubar/dock/home)");
                let _ = windows.home.update(cx, |_view, window, cx| {
                    let handle = shared.read(cx).focus_handle.clone();
                    window.focus(&handle, cx);
                });
                super::ChromeMode::LayerSurfaces
            }
            Err(e) => {
                eprintln!(
                    "[chrome] layer-shell unavailable ({e}); degrading to a single fullscreen \
                     window — this is expected on a compositor without zwlr_layer_shell_v1 \
                     (e.g. weston's headless backend), see BUILD-LINUX.md"
                );
                open_single_fullscreen_fallback(cx, shared.clone());
                super::ChromeMode::SingleFullscreen
            }
        }
    } else {
        open_single_fullscreen_fallback(cx, shared.clone());
        super::ChromeMode::SingleFullscreen
    };

    super::set_active_mode(final_mode);
    eprintln!("[chrome] active chrome mode: {final_mode:?}");
}
