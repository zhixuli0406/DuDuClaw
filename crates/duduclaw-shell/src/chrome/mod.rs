// Chrome surface split — WM-3 layer-shell migration (2026-08-23).
//
// `duduclaw-comp`'s WM-3 round (`crates/duduclaw-comp/BUILD.md`) shipped a
// real `zwlr_layer_shell_v1` compositor implementation and explicitly left
// this crate's own migration as future work: "The shell still does not use
// layer-shell. Its dock and menu bar remain inside its own full-output
// toplevel and the reserved bands remain the contract. What the shell has
// to do later: create one `zwlr_layer_surface_v1` per chrome element on the
// `top` layer, anchor it, `set_exclusive_zone(30)` / `(90)`..." This module
// is that migration.
//
// ── Two hard constraints (spike-verified against the pinned gpui rev,
//    `~/.cargo/git/checkouts/zed-a70e2ad075855582/7a7c3e1`) ───────────────
//
// 1. `gpui::WindowKind::LayerShell(_)` is `#[cfg(all(target_os = "linux",
//    feature = "wayland"))]` — the variant does not exist at all on macOS.
//    Every piece of code that CONSTRUCTS one therefore lives behind
//    `#[cfg(target_os = "linux")]` in this crate too (`windows.rs` +
//    `gpui_bridge.rs`, both Linux-only files/modules). This file (`mod.rs`)
//    and `params.rs` are deliberately gpui-FREE pure data — see `params.rs`'s
//    own header comment — so they compile and are unit-testable on every
//    platform, including macOS, which is this crate's actual dev loop.
//
// 2. The host compositor might not support `zwlr_layer_shell_v1` at all —
//    `cx.open_window(...)` then returns
//    `Err(LayerShellNotSupportedError)`. This is not a theoretical risk:
//    `duduclaw-shell/BUILD-LINUX.md`'s own B-② round records this crate
//    panicking against `weston --backend=headless-backend.so` for an
//    UNRELATED reason (no `wl_seat`), and weston's headless backend
//    ADDITIONALLY does not implement wlr-layer-shell at all — so the
//    layer-shell attempt must degrade gracefully to the pre-WM-3 single
//    fullscreen window, with **zero visual regression** on that fallback
//    path (see `windows.rs`'s `boot_windows` for exactly how the fallback
//    is triggered, and `BUILD-LINUX.md`'s new "WM-3 shell migration"
//    section for how to reproduce/verify the degrade).
//
// ── Surface model ─────────────────────────────────────────────────────────
// In `ChromeMode::LayerSurfaces` (Linux, the default), the menu bar, the
// dock, and the desktop background are each their OWN gpui window (each its
// own `zwlr_layer_surface_v1`), opened once at boot and kept alive for the
// process lifetime; the currently-open overlay (Launcher/Notifications/
// ControlCenter/PointerSettings — at most one, per `crate::surface::
// SurfaceState`'s existing invariant) is a FOURTH window, created only while
// open and destroyed the moment it closes (see `windows.rs`'s
// `SurfaceView::reconcile_overlay_window`) — never left resident, since its
// `Exclusive` keyboard interactivity would otherwise permanently steal
// keyboard focus from every application window.
//
// All four window kinds share ONE underlying state entity
// (`gpui::Entity<ShellView>`, built once in `main.rs`'s `run()` before any
// window opens) — never a per-window copy. `chrome::windows::SurfaceView`
// (Linux-only) is the thin per-window root view that reads/renders a SLICE
// of that shared entity; see its own doc comment for the gpui mechanics
// (`Context<T>::listener`'s closures are window-agnostic by construction,
// which is what makes sharing one `ShellView` across N windows safe).
//
// In `ChromeMode::SingleFullscreen` (macOS always; Linux when
// `DUDUCLAW_SHELL_NO_LAYER_SHELL=1` is set, or when the layer-shell attempt
// above fails at runtime), the SAME `Entity<ShellView>` is instead used
// directly as the one window's root view — this is this crate's ORIGINAL
// Shell-S0..WM-2 behavior, untouched, still implemented by `ShellView`'s own
// `Render` impl in `main.rs`.
//
// ── OOBE / lock screen ───────────────────────────────────────────────────
// Both are full-screen-exclusive states with no app chrome (see `main.rs`'s
// own long-standing header comment on OOBE, and `lockscreen/mod.rs`'s). In
// `LayerSurfaces` mode they are NOT implemented by tearing down and
// recreating the menu-bar/dock windows — a cross-process teammate
// (independently spike-verified against this same pinned rev, see the
// message this round's implementer received mid-task) confirmed
// `gpui::Window::set_exclusive_zone` has a genuine runtime setter
// (`gpui/src/window.rs:2124`) whose `PlatformWindow` trait default is a
// no-op `{}` body (`gpui/src/platform.rs:903`) — safe to call
// unconditionally on every platform/window kind, never `#[cfg]`-gated,
// never a panic on a non-layer-shell window. So instead: the menu bar/dock
// windows STAY OPEN for the whole process lifetime, and on every render
// pass they ask the shared `ShellView` "is OOBE active or the screen
// locked?" (`windows::should_hide_chrome_bars`) — if so, they render
// nothing at all (fully transparent `window_background`, so the desktop
// surface underneath shows through unobstructed) AND zero their own
// exclusive zone (`window.set_exclusive_zone(px(0.))`), restoring
// `MENU_BAR_HEIGHT`/`DOCK_HEIGHT` the moment that state clears. This avoids
// an entire class of window-lifecycle race (destroy-then-recreate racing a
// second lock/unlock before the first pair settles) for a one-line runtime
// call. The desktop's own `Home` window is where OOBE/the lock screen
// actually render (unchanged from before this round — see `windows.rs`'s
// `render_home_content`), exactly the same branching `ShellView::
// render_root` already does for the `SingleFullscreen` path.

mod params;
// D9-bug (2026-08-24): pointer input regions for the chrome bars. NOT
// `#[cfg(target_os = "linux")]`, unlike `gpui_bridge`/`windows` below —
// `home/home_dock.rs` (cross-platform) calls into it from the dock's own
// prepaint listener, and `gpui::Window::set_input_region` is one of the
// runtime setters that exists on every platform with a no-op
// `PlatformWindow` default (see that module's own header comment), so
// nothing here needs gating. Its whole test suite runs on the macOS dev
// loop for exactly that reason.
pub(crate) mod input_region;
// `pub(crate)`, not private: `windows.rs` (a SIBLING module, not a
// descendant of `gpui_bridge` itself) calls `gpui_bridge::to_window_options`
// directly — kept simple/unambiguous rather than relying on the
// private-item-visible-to-descendants rule threading through two levels of
// module nesting.
#[cfg(target_os = "linux")]
pub(crate) mod gpui_bridge;
#[cfg(target_os = "linux")]
pub(crate) mod windows;

// `ChromeMode`/`MENU_BAR_HEIGHT` are read on every platform (mode selection
// and the menu bar's own height); everything else in this list exists to
// describe LAYER SURFACES, which only `gpui_bridge`/`windows` — both
// `#[cfg(target_os = "linux")]` — consume. Re-exporting them unconditionally
// keeps `params` testable as one unit on the macOS dev loop (its tests are
// not cfg-gated, deliberately: the whole point of the pure-data split is
// that the layer parameters can be asserted without a Wayland session), so
// the allow covers the macOS build rather than splitting the re-export in
// two and having the two halves drift.
#[cfg_attr(not(target_os = "linux"), allow(unused_imports))]
pub use params::{
    desired_chrome_mode, layer_params_for, ChromeAnchor, ChromeKeyboardInteractivity, ChromeLayer, ChromeMode, ChromeSurface, LayerParams,
    DESKTOP_HEIGHT, DESKTOP_WIDTH, DOCK_HEIGHT, MENU_BAR_HEIGHT,
};

/// The `app_id` every chrome window declares, layer-shell or fallback alike
/// — `duduclaw-comp`'s `window_policy.rs` (`SHELL_APP_ID`) uses this to tell
/// the session shell apart from an ordinary application window. Centralized
/// here (was a literal string at each `cx.open_window` call site) so the
/// `LayerSurfaces` and `SingleFullscreen` paths cannot drift on it.
pub const SHELL_APP_ID: &str = "duduclaw-shell";

/// Set exactly ONCE, at the end of `windows::boot_windows` (Linux only),
/// with whichever mode this process ACTUALLY ended up running — never
/// optimistically before a fallback attempt might still downgrade it (see
/// that function's own doc comment). Left unset on every other platform,
/// where `active_mode()`'s default (`SingleFullscreen`) is already correct.
static ACTIVE_MODE: std::sync::OnceLock<ChromeMode> = std::sync::OnceLock::new();

/// Linux-only: records the final, resolved chrome mode. `pub(crate)` since
/// only `windows::boot_windows` (this crate's one and only place that
/// decides the mode) ever calls it.
#[cfg(target_os = "linux")]
pub(crate) fn set_active_mode(mode: ChromeMode) {
    let _ = ACTIVE_MODE.set(mode);
}

/// The chrome mode this process is actually running under. Read by
/// `main.rs`'s `ShellView::settle_launcher_query` to decide whether it can
/// focus the Launcher's search field directly (`SingleFullscreen`, one
/// window) or must leave that to the overlay window's own construction-time
/// focus call (`LayerSurfaces` — see `windows::open_surface_window`'s doc
/// comment for why). Defaults to `SingleFullscreen` when never set, which
/// is exactly right on every platform that never calls `set_active_mode`
/// (macOS; and Linux before `boot_windows` has run, a window during which
/// nothing can call `settle_launcher_query` yet anyway).
pub fn active_mode() -> ChromeMode {
    ACTIVE_MODE.get().copied().unwrap_or(ChromeMode::SingleFullscreen)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_mode_defaults_to_single_fullscreen_before_anything_sets_it() {
        // Deliberately does NOT call `set_active_mode` — this crate's test
        // binary never runs `windows::boot_windows` (that whole module is
        // `#[cfg(target_os = "linux")]`, and even on Linux it is only
        // called from `main()`, never from a test), so this assertion holds
        // for the entire test run, on every platform.
        assert_eq!(active_mode(), ChromeMode::SingleFullscreen);
    }
}
