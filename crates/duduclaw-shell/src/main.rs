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
// `Launcher` / `Notifications` / `ControlCenter` / `PointerSettings` are
// overlays (`overlay.rs` + its `overlay/{launcher,notifications,
// controlcenter,pointer_settings}.rs` content modules) that render on top of
// it, at most one at a time, driven by the pure state machine in
// `surface.rs`. cmd-k toggles the Launcher
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

mod apps;
mod audio;
// WM-3 layer-shell migration (2026-08-23) — see this module's own header
// comment for the whole design; `chrome::windows` (Linux-only) is what
// `main()` calls into below instead of opening a single window directly.
mod chrome;
/// A2 (2026-08-23): the co-driving (共駕) half of comp's shell-control
/// socket. Its own module rather than more lines in `comp_client` (already
/// over this crate's file ceiling) — the SOCKET is still shared, see that
/// module's `call_raw`.
mod codrive_client;
mod comp_client;
mod fake_data;
mod gateway_client;
/// A1 (2026-08-23) — the Super+K global-task-bar trigger feed. Pure state
/// machine; the compositor owns the hotkey and this polls for what it saw.
mod global_task;
mod home;
mod i18n;
mod icons;
mod lockscreen;
/// D6 (2026-08-23): the shell's own `org.freedesktop.Notifications` daemon —
/// see its module doc for why the shell serves this itself rather than the
/// image shipping mako/dunst.
mod notifyd;
mod oobe;
mod overlay;
mod palette;
/// D4b (2026-08-23) — 系統設定, the settings application. A crate-root
/// module with its own directory (one file per page) rather than a fifth
/// file under `overlay/`: it is an app, not a panel. It renders AS an
/// overlay (`surface::Overlay::Settings`), which is the whole of its
/// relationship to that module.
mod settings;
/// Q1 (2026-08-24) — the compile-time gate that keeps this crate's debug
/// affordances out of a shipping binary. See its own header comment for the
/// `/etc/duduclaw/kiosk.env` hole it closes and for what is deliberately
/// left ungated.
mod shipping;
mod surface;
/// A1 result-loopback (2026-08-24): the terminal-state watch behind "Super
/// +K 交辦一個任務 → 結果推回殼" — see its own header comment for the full
/// design and for why it is a separate module from `notifyd` (the SINK the
/// events it produces are turned into cards on) rather than folded into it.
mod task_result;

use gpui::{
    actions, div, prelude::*, App, Context, FocusHandle, Focusable, KeyBinding, KeyDownEvent, Keystroke, MouseButton, MouseDownEvent, Render,
    Window,
};
// WM-3: `px`/`size`/`Bounds`/`WindowBounds`/`WindowOptions` are now used
// ONLY by the `ChromeMode::SingleFullscreen` fallback window (`fn main`'s
// `#[cfg(not(target_os = "linux"))]` block) — every layer-shell window's
// own `WindowOptions`/bounds are built in `chrome::gpui_bridge` instead.
// Split into their own `#[cfg]`-gated `use` so a Linux build (which never
// compiles that block) doesn't warn on five unused imports.
#[cfg(not(target_os = "linux"))]
use gpui::{px, size, Bounds, WindowBounds, WindowOptions};
use gpui_platform::application;

use duduclaw_native_gui::theme;

use surface::{Overlay, SurfaceState};

// `FocusNext`/`FocusPrev` (WP-oobe-tab, 2026-08-23): Tab/Shift-Tab focus
// cycling between an OOBE step's own text fields (see `oobe/focus_order.rs`'s
// own header comment for the pure decision behind them) — bound globally,
// same shape `OobeNext` already establishes, with the OOBE-only guard living
// in the handler body (`on_focus_next`/`on_focus_prev` below), not in the
// binding itself. Harmless outside OOBE: nothing else in this crate's
// `ImeTextInput::on_key_down` match arms does anything with a raw "tab"
// keystroke today (see that file's own match list), so claiming the
// keybinding globally removes no existing behavior.
actions!(duduclaw_shell, [ToggleLauncher, CloseOverlay, OobeNext, LockScreenNow, FocusNext, FocusPrev]);

/// Diagnostic gate (`DUDUCLAW_SHELL_DIAG=1`). Kept permanently: this
/// layer-splitting toolkit (in-app keystroke dispatch, raw OS input probes,
/// bounds probes, hit/action/render logs) is what root-caused the
/// "overlay laid out one window-height offscreen" bug after three
/// screen-never-changes reports — cheap to keep, expensive to rebuild.
/// A1 (2026-08-23): the ONE long-lived loop that drains comp's global-hotkey
/// intent queue. Started exactly once per process, from `run`.
///
/// ## Why not on the render pass (the P0 this replaced)
///
/// The first cut called this from `ShellView::render_root`, following the
/// convention `home_dock.rs` established for the dock's own comp poll. That
/// works there and does NOT work here, and the difference is the whole
/// point of the chrome migration: before it, ONE window drew everything, and
/// the menu bar's live clock guaranteed a repaint every second, so a
/// render-gated poll effectively ran on a timer. Now the clock lives in its
/// OWN window — an idle Home surface can go arbitrarily long without a
/// single render pass, so the poll simply stopped running.
///
/// Measured on the appliance VM: two Super+K presses left
/// `{"ok":true,"intents":["global_task_bar","global_task_bar"]}` sitting
/// unread in comp's queue. Compositor side perfect, shell side never asked.
///
/// This loop is started once and re-arms itself, which is exactly the shape
/// `home_dock.rs` warns against — but that warning is about spawning a timer
/// *per render pass* (the WP-A4-4 CPU-burn incident), where the count grows
/// without bound. One loop for the process's lifetime is the intended
/// alternative, not a violation of it.
///
/// Acting on an intent still happens on the render pass
/// (`settle_global_task_intents`), because opening the task bar needs a real
/// `&mut Window`. This loop only fills the queue and calls `cx.notify()`,
/// which wakes every chrome window — including Home — so the settle runs.
fn spawn_global_task_poll_loop(shared: gpui::Entity<ShellView>, cx: &mut App) {
    cx.spawn(async move |cx| {
        loop {
            // `read_with`/`update` on an `AsyncApp` return the closure's value
            // directly at this gpui rev (they are infallible here — `shared`
            // is the process-lifetime shell entity, created in `run` and
            // never dropped while this loop exists).
            let wait = shared.read_with(cx, |view, _| view.global_task.interval());
            cx.background_executor().timer(wait).await;

            // Not due yet / already in flight — neither can happen with a
            // single loop, but `begin_poll` is the single-flight authority
            // and this defers to it rather than assuming.
            if !shared.update(cx, |view, _| view.global_task.begin_poll()) {
                continue;
            }

            let outcome = cx
                .background_executor()
                .spawn(async move { comp_client::take_shell_intents() })
                .await;

            let updated = shared.update(cx, |view, view_cx| match outcome {
                Ok(intents) => {
                    let got = !intents.is_empty();
                    view.global_task.apply_ok(intents);
                    if got {
                        // Wake every chrome window so Home's render pass runs
                        // `settle_global_task_intents`. Deliberately NOT
                        // called on the (overwhelmingly common) empty answer:
                        // nothing drawn changed, and notifying on every poll
                        // would repaint the whole shell five times a second.
                        view_cx.notify();
                    }
                    true
                }
                // `Comp(_)` means the call REACHED comp and comp refused it —
                // a build without this op. Asking again cannot change that,
                // so stop asking for the rest of the session. Every other
                // error is "comp isn't reachable right now", which is
                // recoverable and only backs the cadence off.
                Err(comp_client::CompClientError::Comp(e)) => {
                    eprintln!("[global_task] comp has no take_shell_intents op ({e}) — Super+K disabled for this session");
                    view.global_task.give_up();
                    false
                }
                Err(e) => {
                    if diag_enabled() {
                        eprintln!("[global_task] poll failed: {e}");
                    }
                    view.global_task.apply_err();
                    true
                }
            });

            // `give_up` fired: this loop has nothing left to do and must not
            // spin for the rest of the session.
            if !updated {
                return;
            }
        }
    })
    .detach();
}

/// Q1 (2026-08-24): now behind the compile-time shipping gate
/// (`shipping::debug_env_is_one`), so a shipping binary answers `false` no
/// matter what `/etc/duduclaw/kiosk.env` puts in the environment. See
/// `crate::shipping`'s header comment. The `OnceLock` is kept: this is read
/// on every render pass, and in a gated-out build the whole body folds to a
/// constant anyway.
pub(crate) fn diag_enabled() -> bool {
    static DIAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DIAG.get_or_init(|| shipping::debug_env_is_one("DUDUCLAW_SHELL_DIAG"))
}

/// D2 (2026-08-23): tell the compositor which palette to draw its
/// server-side window decorations in.
///
/// Fire-and-forget on a detached thread, for the reason `comp_client`'s own
/// module doc spells out: every call in that client is PLAIN BLOCKING, and
/// this one is invoked from gpui's main thread at boot and at OOBE
/// completion. A 3-second socket timeout on the UI thread would be a
/// visible stall; a theme that fails to propagate is a cosmetic mismatch on
/// window frames. The failure is logged, never surfaced — the operator
/// cannot act on "the compositor isn't listening", and on the macOS dev
/// loop there is no compositor by design.
///
/// Deliberately NOT retried: comp keeps the value once it lands, and the
/// next theme change (or the next shell start) sends it again anyway.
pub(crate) fn notify_comp_theme(theme: oobe::ThemeChoice) {
    let wire = match theme {
        oobe::ThemeChoice::Dark => comp_client::THEME_DARK,
        oobe::ThemeChoice::Light => comp_client::THEME_LIGHT,
    };
    std::thread::spawn(move || match comp_client::set_theme(wire) {
        Ok(()) => {
            if diag_enabled() {
                eprintln!("[theme] comp accepted set_theme({wire})");
            }
        }
        Err(e) => eprintln!("[theme] comp set_theme({wire}) failed (decorations keep their previous palette): {e}"),
    });
}

/// D9-bug3/D9-bug4 (2026-08-24): tell the compositor whether this shell's
/// lock screen is up.
///
/// Same fire-and-forget-on-a-detached-thread shape (and the same reasoning)
/// as [`notify_comp_theme`] directly above: every `comp_client` call is plain
/// blocking with a 3 s socket timeout, and this fires from gpui's main thread
/// at boot, on every lock and on every unlock. Blocking the UI thread for
/// three seconds *while locking the screen* would be the worst possible place
/// to stall.
///
/// The failure is logged and never surfaced — but note that the two
/// directions fail differently, which is why the log line says which one it
/// was. A failed `locked=true` means comp keeps painting application windows
/// behind the lock screen (the D9-bug4 symptom, i.e. no worse than before
/// this round, but the operator should be able to find it in the journal); a
/// failed `locked=false` means comp keeps hiding them after the operator has
/// unlocked, which looks like "my windows are gone". Neither can be retried
/// usefully from here — comp is either listening or it is not — and the next
/// lock/unlock announces again anyway.
///
/// On the macOS dev loop there is never a real compositor, so this is
/// expected to log `NotAvailable` on every call and is deliberately quiet
/// about it unless `DUDUCLAW_SHELL_DIAG=1` is set.
pub(crate) fn notify_comp_session_locked(locked: bool) {
    std::thread::spawn(move || match comp_client::set_session_locked(locked) {
        Ok(()) => {
            if diag_enabled() {
                eprintln!("[lock] comp accepted set_session_locked({locked})");
            }
        }
        Err(comp_client::CompClientError::NotAvailable(_)) if !cfg!(target_os = "linux") => {
            if diag_enabled() {
                eprintln!("[lock] no compositor on this platform — set_session_locked({locked}) skipped");
            }
        }
        Err(e) => eprintln!(
            "[lock] comp set_session_locked({locked}) failed: {e} — the compositor is still {} application windows",
            if locked { "painting" } else { "hiding" }
        ),
    });
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
    /// ICON-3 (2026-08-23) — the pointer-settings overlay's compositor-backed
    /// state. A sibling field rather than part of `overlay_ui`, for exactly
    /// the reason `audio_ui` above already is one: it is this round's own
    /// state, and keeping it out of `OverlayUiState`'s body keeps the diff
    /// away from fields other work packages own. See
    /// `overlay::pointer_settings::PointerUiState`'s own doc comment.
    pub(crate) pointer_ui: overlay::pointer_settings::PointerUiState,
    /// D4b (2026-08-23) — the 系統設定 app's seven pages' state (selected
    /// category plus each page's own `Load`/in-flight machinery). A sibling
    /// field for exactly the reason `audio_ui`/`pointer_ui` above are: it is
    /// this work package's own state, and keeping it out of
    /// `OverlayUiState`'s body keeps the diff away from fields other
    /// packages own. See `settings::SettingsUiState`'s doc comment.
    pub(crate) settings_ui: settings::SettingsUiState,
    /// D4b — the settings app's eight `Entity<OobeTextField>`s, created once
    /// at window-open time, same precedent `oobe_account_fields`/
    /// `lockscreen_password_field` establish. Reached through
    /// `oobe::SettingsFields` purely because `OobeTextField::new` is private
    /// to that module; nothing here is part of the OOBE flow.
    pub(crate) settings_fields: oobe::SettingsFields,
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
    /// WP-comp-shell-ipc (2026-08-22) — the dock's real "執行中視窗" feed,
    /// polled from `duduclaw-comp`'s shell-control socket
    /// (`comp_client::list_windows`). Same "one model, read by whatever
    /// surfaces need it" shape `overlay_ui.notifications` already
    /// establishes for approvals — see `home::running_windows::
    /// RunningWindowsFeed`'s own header comment.
    pub(crate) running_windows: home::running_windows::RunningWindowsFeed,
    /// A1 (2026-08-23) — Super+K's trigger queue, polled from comp.
    ///
    /// Not rendered by anything: this feed's entire output is "did the
    /// operator ask for the task bar", consumed and cleared in the same
    /// render pass that observes it. See `global_task` for why the trigger
    /// is a poll rather than a push, and for the latency that costs.
    pub(crate) global_task: global_task::GlobalTaskIntentFeed,
    /// APP-1 (2026-08-22) — the REAL list of applications installed on this
    /// machine (`flatpak list` + the XDG `.desktop` directories, merged),
    /// scanned on a cadence off the render thread. Read by BOTH the dock
    /// (`home::home_dock::dock`) and the Launcher's app-search section
    /// (`overlay::launcher`), same "one model, read by whatever surfaces
    /// need it" shape `running_windows`/`overlay_ui.notifications` already
    /// establish — see `apps::feed::InstalledAppsFeed`'s own header comment.
    /// Replaces `fake_data::DOCK_APPS`, the canned six-entry array both
    /// surfaces used to render (five of whose entries had no app behind
    /// them at all).
    pub(crate) installed_apps: apps::feed::InstalledAppsFeed,
    /// D6 (2026-08-23) — the transport half of this shell's own
    /// `org.freedesktop.Notifications` daemon: the shared inbox the D-Bus
    /// handlers write into, plus the handle to the thread that owns the bus
    /// name. See `notifyd`'s module doc for the whole design, and
    /// `notifyd::NotifyRuntime` for why this field carries no `#[cfg]`
    /// despite being a no-op on the macOS dev loop.
    pub(crate) notify_runtime: notifyd::NotifyRuntime,
    /// D6 (2026-08-23) — the notification-center store the 通知中心 panel
    /// renders (third-party app notifications, as opposed to `overlay_ui.
    /// notifications`, which is the gateway's approval feed). Deliberately a
    /// SEPARATE field from `notify_runtime` rather than a member of it:
    /// draining takes `&mut` on both at once, and disjoint fields is what
    /// makes that a plain borrow instead of a dance.
    pub(crate) notify_center: notifyd::center::NotificationCenter,
    /// A1 result-loopback (2026-08-24) — every goal task this shell itself
    /// delegated (`overlay::launcher::try_submit_delegate`), watched until
    /// it reaches a terminal state. `schedule_task_result_poll` drains its
    /// events into `notify_center` above — see `task_result`'s own module
    /// doc for the full design.
    pub(crate) task_results: task_result::TaskResultTracker,
    /// WP-lock-pw (2026-08-22) — the lockscreen's real password-entry
    /// `Entity<OobeTextField>`, same "created once, unconditionally, at
    /// window-open time" precedent `oobe_account_fields`/`oobe_network_fields`
    /// below already establish (see either field's own doc comment). Reached
    /// through `oobe::LockPasswordField` (re-exported from `oobe::widgets`,
    /// see that type's own doc comment) purely because the underlying
    /// `OobeTextField::new` constructor is private to that module — this
    /// field has nothing conceptually to do with OOBE.
    pub(crate) lockscreen_password_field: oobe::LockPasswordField,
    /// D3-b (2026-08-23) — the Launcher's real search-box entity, same
    /// "created once at window-open time, reached through a bundle type
    /// because `OobeTextField::new` is private to `oobe::widgets`" shape
    /// `lockscreen_password_field` just above already establishes. Before
    /// D3-b the Launcher's query was a `String` on `overlay_ui` appended to
    /// by a raw key listener on this very root element, which meant gpui
    /// never had an `EntityInputHandler` to deliver IME commits to and an
    /// operator could not search their apps in Chinese at all.
    pub(crate) launcher_query_field: oobe::LauncherQueryField,
    /// ICON-3 (2026-08-23) — the lockscreen identity row's display name.
    /// Read ONCE at window-open time from the persisted OOBE state
    /// (`oobe::boot_operator_name`), exactly like `theme` just below and for
    /// the same reason: the lockscreen only ever renders on a boot path
    /// where `initial_oobe` resolved to `None`, so it cannot read the name
    /// out of a live `OobeFlow`. Not re-read per render — `load_state()` is
    /// a blocking disk read, and this value cannot change while the shell is
    /// running (the only writer is the OOBE flow, which by definition is not
    /// running when the lock screen is).
    ///
    /// `None` means no name is on file; see `lockscreen::render::name_row`
    /// for what that draws.
    pub(crate) operator_name: Option<String>,
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
    /// more at the moment OOBE completes — so a THEME step pick made during
    /// THIS run reaches Home on its very first frame, not just on the next
    /// restart. `ShellView::render` resolves this into a
    /// `palette::ShellPalette` fresh every render pass (same "recompute,
    /// never cache" convention `OobeFlow::palette()` already established)
    /// and threads it into `home::render`/`overlay::render`.
    ///
    /// D2-b (2026-08-24): OOBE has THREE completion sites — this file's
    /// own `handle_enter_key` (`EnterOutcome::Advance` arm, keyboard Enter)
    /// and `oobe/render.rs`'s `button_row` (`continue_click`/`skip_click`,
    /// the 完成/略過 mouse buttons everyone actually uses). All three MUST
    /// copy `flow.state().selections.theme` here and call
    /// `notify_comp_theme` before dropping `self.oobe` — the button paths
    /// were missing this (they only called `oobe::save_state`, which is why
    /// a shell RESTART after OOBE always picked the theme up correctly but
    /// the same-process OOBE→Home transition did not).
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
    /// Re-settles everything that has to follow a Launcher open/close
    /// transition, in ONE place so the four call sites (cmd-k toggle, Escape
    /// close, backdrop click, Home's composer/dock click) cannot drift apart.
    ///
    /// D3-b (2026-08-23): the search box is a focusable entity now, so
    /// "close the overlay" is no longer complete without handing keyboard
    /// focus BACK to the shell root — leave it on a field that is no longer
    /// rendered and the next keystroke has nowhere to go. Opening does the
    /// mirror image: focus the field so the operator can just start typing,
    /// which is also what installs its `EntityInputHandler` (gpui only
    /// registers the handler of the FOCUSED element — `Window::handle_input`).
    ///
    /// The field is cleared on every transition, open included: reopening
    /// the Launcher must never show the previous search, which is exactly
    /// what `OverlayUiState::close_launcher_query` used to guarantee for the
    /// old `String`.
    pub(crate) fn settle_launcher_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.launcher_query_field.field.update(cx, |field, cx| field.clear(cx));
        self.overlay_ui.close_launcher_query();
        let opening_launcher = self.surface.overlay() == Some(Overlay::Launcher);
        // WM-3: in `ChromeMode::LayerSurfaces` the Launcher's search box
        // renders inside a SEPARATE `duduclaw-shell-overlay` window that
        // `chrome::windows::SurfaceView::reconcile_overlay_window` creates
        // ASYNCHRONOUSLY, after this method returns (see that fn's own doc
        // comment) — so on the OPEN path, `window` here is whichever window
        // the click/keystroke that opened it actually arrived on (e.g.
        // Home), not the not-yet-existing overlay window. Focusing the
        // search field's handle against the WRONG window would silently do
        // nothing (no matching dispatch node there), so this method leaves
        // that half to the overlay window's own construction-time focus
        // call in that case (`chrome::windows::open_overlay_window`) and
        // only handles it directly — as it always has — in
        // `SingleFullscreen` mode, where there is only ever one window. The
        // CLOSE path (`opening_launcher == false`) is unaffected either
        // way: refocusing `self.focus_handle` on the window this call
        // actually ran in is correct in both modes.
        if opening_launcher && chrome::active_mode() == chrome::ChromeMode::LayerSurfaces {
            return;
        }
        if opening_launcher {
            let handle = self.launcher_query_field.field.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        } else {
            window.focus(&self.focus_handle, cx);
        }
    }

    /// `cmd-k`'s action handler — see this file's header comment for why
    /// this lives on the root element (`.on_action(cx.listener(...))` in
    /// `Render::render`) rather than as an App-global `cx.on_action`
    /// closure in `main()` like round 2 had it.
    fn on_toggle_launcher(&mut self, _action: &ToggleLauncher, window: &mut Window, cx: &mut Context<Self>) {
        if diag_enabled() {
            eprintln!("[action] ToggleLauncher fired");
        }
        // Shell-S4-lock: `cmd-k` is a BOUND key, so a keystroke matching it
        // never reaches the root's raw `on_key_down` catch-all at all (gpui
        // consumes it entirely via action dispatch — see `lockscreen::
        // render::note_input_or_reveal`'s own doc comment for why that
        // catch-all can't cover this key) — this handler carries its own
        // identical lock-check for that reason, not relying on the
        // catch-all to reveal the password prompt on cmd-k. WP-lock-pw:
        // cmd-k has no meaning while locked beyond that same reveal — it
        // does NOT unlock (see `lockscreen::render::note_input_or_reveal`'s
        // own doc comment for the reversal).
        if self.lockscreen.is_locked() {
            lockscreen::render::note_input_or_reveal(self, window, cx);
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
        // WP-A3 / D3-b: clearing is unconditional (a no-op on the open path,
        // the correct behavior on the close path); the focus half DOES read
        // `self.surface.overlay()`'s new value, because open and close move
        // focus in opposite directions. See `settle_launcher_query`.
        self.settle_launcher_query(window, cx);
        // ICON-3 (2026-08-23): closing ANY overlay also forgets the
        // pointer surface's compositor snapshot, so the next open re-reads
        // it — the cursor can have been changed by something else in
        // between, and showing a stale selection would be a claim this
        // surface never verified. Cheap: a no-op when it was never loaded.
        self.pointer_ui.reset();
        // A2 (2026-08-23): and the 共駕 row's own snapshot, for a sharper
        // version of the same reason — driving state changes on its own,
        // without anyone touching this panel, so a stale 「AI 正在操作這台
        // 電腦」 would be an outright false statement rather than a merely
        // out-of-date one. See `overlay::codrive_row::CodriveUiState::reset`.
        self.overlay_ui.codrive.reset();
        // D4a-6 (2026-08-24): and the Wi-Fi quick tile's own snapshot — the
        // link can drop or reconnect without anyone touching this panel.
        // See `overlay::wifi_tile::WifiTileState::reset`.
        self.overlay_ui.wifi_tile.reset();
        // D4b (2026-08-23): same reasoning, one surface further — closing
        // ANY overlay drops the settings app's cached backend reads AND
        // every one of its typed fields, two of which hold passwords. See
        // `SettingsUiState::reset` / `SettingsFields::clear_all`.
        self.settings_ui.reset();
        self.settings_fields.clear_all(cx);
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
    fn on_close_overlay(&mut self, _action: &CloseOverlay, window: &mut Window, cx: &mut Context<Self>) {
        if diag_enabled() {
            eprintln!("[action] CloseOverlay fired");
        }
        // WP-lock-pw: same reveal-only behavior as `on_toggle_launcher`
        // above — Escape has no other meaning while locked, it does NOT
        // unlock.
        if self.lockscreen.is_locked() {
            lockscreen::render::note_input_or_reveal(self, window, cx);
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
        self.settle_launcher_query(window, cx);
        // ICON-3 (2026-08-23): closing ANY overlay also forgets the
        // pointer surface's compositor snapshot, so the next open re-reads
        // it — the cursor can have been changed by something else in
        // between, and showing a stale selection would be a claim this
        // surface never verified. Cheap: a no-op when it was never loaded.
        self.pointer_ui.reset();
        // A2 (2026-08-23): and the 共駕 row's own snapshot, for a sharper
        // version of the same reason — driving state changes on its own,
        // without anyone touching this panel, so a stale 「AI 正在操作這台
        // 電腦」 would be an outright false statement rather than a merely
        // out-of-date one. See `overlay::codrive_row::CodriveUiState::reset`.
        self.overlay_ui.codrive.reset();
        // D4a-6 (2026-08-24): and the Wi-Fi quick tile's own snapshot — the
        // link can drop or reconnect without anyone touching this panel.
        // See `overlay::wifi_tile::WifiTileState::reset`.
        self.overlay_ui.wifi_tile.reset();
        // D4b (2026-08-23): same reasoning, one surface further — closing
        // ANY overlay drops the settings app's cached backend reads AND
        // every one of its typed fields, two of which hold passwords. See
        // `SettingsUiState::reset` / `SettingsFields::clear_all`.
        self.settings_ui.reset();
        self.settings_fields.clear_all(cx);
        cx.notify();
    }

    /// `enter`'s action handler — OOBE's keyboard "continue" binding (task
    /// brief: "Enter=繼續"), and (A1 result-loopback, 2026-08-24) the
    /// Launcher's own "Enter 交辦" outside OOBE — see
    /// `overlay::launcher::try_submit_delegate`'s own doc comment for
    /// exactly which states that covers. A no-op in every other situation
    /// (Home itself still has no Enter binding of its own). WP-lock-pw
    /// (2026-08-22): while locked, Enter is instead this surface's SUBMIT
    /// trigger — `lockscreen::render::submit_or_reveal` reveals the prompt
    /// on a first press (same as any other key) or, once the password field
    /// is already visible and focused, dispatches a real verify attempt
    /// against whatever the operator just typed into it (see that fn's own
    /// doc comment for why Enter reliably reaches the currently-focused
    /// field BEFORE bubbling up to this global binding).
    fn on_oobe_next(&mut self, _action: &OobeNext, window: &mut Window, cx: &mut Context<Self>) {
        if diag_enabled() {
            eprintln!("[action] OobeNext fired");
        }
        if self.lockscreen.is_locked() {
            lockscreen::render::submit_or_reveal(self, window, cx);
            return;
        }
        self.lockscreen.note_input();
        // A1 result-loopback (2026-08-24): outside OOBE, Enter's other real
        // meaning is the Launcher's "Enter 交辦" hint
        // (`fake_data::LAUNCHER_DELEGATE_HINT`) — checked BEFORE the
        // OOBE-only logic below so a delegate submit can never be shadowed
        // by it (`self.oobe` is already `None` here in every case that
        // matters, since the lockscreen/OOBE-active paths already returned
        // or reduce to a no-op below, but the explicit guard documents the
        // intent rather than relying on that incidentally).
        if self.oobe.is_none() && overlay::launcher::try_submit_delegate(self, window, cx) {
            cx.notify();
            return;
        }
        // WP-oobe-enter (2026-08-23): the three signals `OobeFlow::enter_
        // outcome` needs, read BEFORE the mutable borrow of `self.oobe`
        // below — a disjoint-field borrow of `self.oobe_ui`, not a
        // conflict (same shape the pre-existing `wired_online` read already
        // used here). See `OobeFlow::enter_outcome`'s own doc comment
        // (`oobe/state.rs`) for why Enter can no longer just call `next_
        // with_wired` unconditionally: `AccountCreate`/`Network` gate their
        // own precondition behind an async submit (帳號建立 / Wi-Fi 連線)
        // that ONLY their own button used to trigger — Enter reaching just
        // the generic advance was a silent, indefinite no-op on both steps
        // whenever that submit hadn't happened yet.
        let wired_online = self.oobe_ui.wired_online();
        let account_claim_in_flight = self.oobe_ui.account_claim == oobe::AccountClaimState::InFlight;
        let net_connect_submittable = self.oobe_ui.net_connect.submittable();
        let Some(outcome) =
            self.oobe.as_ref().map(|flow| flow.enter_outcome(wired_online, account_claim_in_flight, net_connect_submittable))
        else {
            return;
        };
        match outcome {
            oobe::EnterOutcome::Advance => {
                let Some(flow) = self.oobe.as_mut() else {
                    return;
                };
                flow.next_with_wired(wired_online);
                oobe::save_state(flow.state());
                if flow.completed() {
                    // Carry the Theme step's pick (if any was made) onto
                    // Home in this SAME process — see `ShellView.theme`'s
                    // own doc comment. Reading `flow.state().selections.
                    // theme` here (not `oobe::boot_theme` again) is
                    // deliberate: `boot_theme` is specifically about the
                    // PERSISTED file at boot, whereas this is reading the
                    // in-memory flow's live selection at the exact moment
                    // it transitions to completed — same source `save_
                    // state` just wrote to disk two lines up, so the two
                    // never disagree.
                    self.theme = flow.state().selections.theme;
                    // D2 (2026-08-23): the compositor draws the SERVER-SIDE
                    // decorations around application windows (title bars,
                    // borders, shadows, the Alt-Tab switcher). It has no way
                    // to learn the operator's theme pick on its own, so the
                    // shell — which is the half that persists it — tells it.
                    // Fire-and-forget: comp not running (macOS dev loop, or
                    // a shell started before the compositor) must never
                    // block or fail OOBE completion.
                    notify_comp_theme(self.theme);
                    self.oobe = None;
                }
            }
            // `AccountCreate`/`Network`: trigger that step's own submit
            // instead — mirrors its button's click exactly (`oobe::
            // handle_enter_submit` dispatches to `steps::account::
            // try_submit`/`steps::network::try_submit`, the SAME functions
            // each button's own `on_click` now calls).
            oobe::EnterOutcome::SubmitAccount | oobe::EnterOutcome::SubmitNetworkConnect => {
                oobe::handle_enter_submit(outcome, self, cx);
            }
            // Precondition unmet and nothing to submit yet (e.g. no Wi-Fi
            // row picked) — a legitimate no-op, not a bug.
            oobe::EnterOutcome::Blocked => {}
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

    /// `tab`'s action handler — see `FocusNext`'s own doc comment (next to
    /// its `actions!` declaration) for why this is bound globally with the
    /// OOBE guard living HERE, in the handler, rather than in the binding
    /// itself — same "always-registered action, self-contained guard" shape
    /// `on_toggle_launcher`'s own lockscreen check already establishes.
    /// WP-oobe-tab (2026-08-23): task brief "OOBE 完成後（Home/overlay 狀
    /// 態）Tab 不該被殼搶走" — `cycle_oobe_focus` below no-ops immediately
    /// whenever `self.oobe` is `None`.
    ///
    /// Wired in **two** places, and both are required: `render_root`'s
    /// `.on_action(...)` chain (the `SingleFullscreen` chrome mode, and the
    /// macOS dev loop) and `chrome/windows.rs`'s overlay root (the
    /// `LayerSurfaces` mode). Binding the keymap in `bind_keys` is
    /// independent of registering the action LISTENER on the focused root —
    /// miss the listener and the keystroke is swallowed with no handler,
    /// which is exactly the failure shape this file's header comment
    /// documents from an earlier round.
    fn on_focus_next(&mut self, _action: &FocusNext, window: &mut Window, cx: &mut Context<Self>) {
        if diag_enabled() {
            eprintln!("[action] FocusNext fired");
        }
        self.cycle_oobe_focus(window, cx, oobe::focus_next);
    }

    /// `shift-tab`'s action handler — the mirror image of `on_focus_next`.
    fn on_focus_prev(&mut self, _action: &FocusPrev, window: &mut Window, cx: &mut Context<Self>) {
        if diag_enabled() {
            eprintln!("[action] FocusPrev fired");
        }
        self.cycle_oobe_focus(window, cx, oobe::focus_prev);
    }

    /// Shared body for `on_focus_next`/`on_focus_prev` — `step_fn` is
    /// `oobe::focus_next`/`oobe::focus_prev`, the one pure difference
    /// between the two directions (see `oobe/focus_order.rs`'s own header
    /// comment for that pure logic, independently unit-tested there without
    /// any gpui window). This method is the gpui-facing half: it reads the
    /// CURRENT step's own focus order, figures out which (if any) of its
    /// fields presently has keyboard focus by checking each candidate's own
    /// `FocusHandle::is_focused(window)` directly — no separate "which
    /// field is focused" bookkeeping on `ShellView` to keep in sync, gpui's
    /// own focus state IS the source of truth, same principle `render.rs`'s
    /// own `focused = handle.is_focused(window)` reads already rely on for
    /// painting the focus ring — then moves focus to whatever `step_fn`
    /// returns. A step with no focusable field (`oobe::focus_order` empty,
    /// or `self.oobe` is `None` entirely) is a no-op.
    fn cycle_oobe_focus(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        step_fn: fn(oobe::OobeStep, Option<oobe::OobeFocusTarget>) -> Option<oobe::OobeFocusTarget>,
    ) {
        let Some(step) = self.oobe.as_ref().map(|flow| flow.current()) else {
            return;
        };
        let order = oobe::focus_order(step);
        if order.is_empty() {
            return;
        }
        let mut current = None;
        for target in order {
            if self.oobe_focus_handle(*target, cx).is_focused(window) {
                current = Some(*target);
                break;
            }
        }
        let Some(next) = step_fn(step, current) else {
            return;
        };
        let handle = self.oobe_focus_handle(next, cx);
        window.focus(&handle, cx);
        cx.notify();
    }

    /// Resolves one `OobeFocusTarget` to the real `FocusHandle` behind it —
    /// the ONE place that knows which `ShellView` field owns which target,
    /// so `cycle_oobe_focus` above never has to. `AccountName`/
    /// `AccountPassword` come from `self.oobe_account_fields` (created once
    /// at window-open time, see that field's own doc comment), `NetworkPsk`
    /// from `self.oobe_network_fields` the same way.
    fn oobe_focus_handle(&self, target: oobe::OobeFocusTarget, cx: &App) -> FocusHandle {
        match target {
            oobe::OobeFocusTarget::AccountName => self.oobe_account_fields.name.read(cx).focus_handle(cx),
            oobe::OobeFocusTarget::AccountPassword => self.oobe_account_fields.password.read(cx).focus_handle(cx),
            oobe::OobeFocusTarget::NetworkPsk => self.oobe_network_fields.psk.read(cx).focus_handle(cx),
        }
    }
}

/// The "check the mpsc channel" tick for `ShellView::
/// schedule_task_result_poll`'s own thread + `mpsc` + `cx.spawn` bridge —
/// same value (and reasoning) `overlay/notifications.rs::POLL_INTERVAL`/
/// `overlay/launcher.rs::SUBMIT_BRIDGE_POLL_INTERVAL` already use for
/// theirs; kept as its own module-level constant here (not a cross-module
/// import of either) since neither is `pub` and both are private
/// implementation details of files this one has no other reason to depend
/// on.
const TASK_RESULT_BRIDGE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// Runs entirely on a background `std::thread` — never called from gpui's
/// own executor, same contract every other blocking call in this crate
/// documents. `new_jwt` mirrors `overlay/notifications.rs::fetch_once`'s own
/// `Some` only when THIS call bootstrapped a fresh session.
fn task_result_poll_once(existing_jwt: Option<String>, agent_id: String) -> (Option<String>, Result<Vec<gateway_client::TaskSnapshot>, gateway_client::GatewayError>) {
    let (jwt, new_jwt) = match existing_jwt {
        Some(jwt) => (jwt, None),
        None => match gateway_client::bootstrap_local_session() {
            Ok(jwt) => (jwt.clone(), Some(jwt)),
            Err(e) => return (None, Err(e.into())),
        },
    };
    let result = gateway_client::list_tasks(&jwt, &agent_id).map_err(gateway_client::GatewayError::from);
    (new_jwt, result)
}

/// Turns one terminal-state transition into a card on `notify_center` — the
/// "結果推回殼" half of A1's TODO line. Three deliberate choices, all
/// stated here rather than scattered:
///
/// 1. **User-facing text, zero internal vocabulary** (task brief: "結果文
///    字使用者視角，零內部術語") — the summary names the task by its own
///    title, never its id; the three outcomes get three distinct honest
///    sentences (`Key::TaskResult{Done,Failed,NeedsHuman}Summary`), never a
///    generic "任務更新".
/// 2. **`needs_human` alone gets decision buttons** — `retry`/`abort`
///    dispatch straight to `gateway_client::decide_goal_task`
///    (`tasks.goal_decide`, the SAME RPC the dashboard's needs_human board
///    already uses — task brief: "沿用既有審批卡/決策管道，別重造"). See
///    `overlay/notifications_apps.rs`'s click-handler doc comment for why
///    `done`/`takeover` are deliberately left off a notification card.
/// 3. **The body is what `TaskSnapshot` actually said, truncated by
///    `post_system`'s own boundary caps** (task brief: "長結果截斷帶「查
///    看完整」路徑") — `MAX_BODY_CHARS` (1200 codepoints) covers the
///    overwhelming majority of a real `result_summary`/`judge_feedback`
///    without truncating at all, and the card stays in the persistent 通知
///    中心 panel (not a toast) until dismissed, so there is somewhere to
///    keep reading it. A dedicated "open full task detail" surface is
///    explicitly OUT of scope this round — no such view exists yet in this
///    shell (tracked separately) — so this deliberately does not invent a
///    click target that would go nowhere; see this module's own A1 report
///    for the honest state of that gap.
fn post_task_result_card(view: &mut ShellView, event: &task_result::TaskResultEvent) {
    use task_result::GoalOutcome;
    let locale = i18n::Locale::ZhTw;
    let (summary_key, urgency, needs_decision) = match event.outcome {
        GoalOutcome::Done => (i18n::Key::TaskResultDoneSummary, notifyd::Urgency::Normal, false),
        GoalOutcome::Failed => (i18n::Key::TaskResultFailedSummary, notifyd::Urgency::Normal, false),
        GoalOutcome::NeedsHuman => (i18n::Key::TaskResultNeedsHumanSummary, notifyd::Urgency::Critical, true),
    };
    let summary = i18n::t1(locale, summary_key, &event.title);
    let body = event.detail.clone().unwrap_or_else(|| i18n::t(locale, i18n::Key::TaskResultNoDetail).to_string());
    let actions = if needs_decision {
        vec![
            notifyd::NotificationAction { key: task_result::ACTION_RETRY.to_string(), label: i18n::t(locale, i18n::Key::TaskResultRetryButton).to_string() },
            notifyd::NotificationAction { key: task_result::ACTION_ABORT.to_string(), label: i18n::t(locale, i18n::Key::TaskResultAbortButton).to_string() },
        ]
    } else {
        Vec::new()
    };
    view.notify_center.post_system(task_result::NOTIFY_APP_NAME, &summary, &body, urgency, actions, Some(event.task_id.clone()));
}

impl ShellView {
    /// Builds the root element for whichever window is showing OOBE / the
    /// lock screen / the Home desktop — this is `Render::render`'s ENTIRE
    /// pre-WM-3 body, extracted so `ChromeMode::SingleFullscreen` (`Render::
    /// render` below, `single_window: true`) and `ChromeMode::LayerSurfaces`'
    /// dedicated `duduclaw-shell-home` window (`chrome::windows::
    /// render_surface_content`'s `ChromeSurface::Home` arm, `single_window:
    /// false`) share EXACTLY one implementation — see `crate::chrome`'s
    /// module doc for why visual/behavioral drift between the two modes is
    /// the one outcome this migration cannot afford.
    ///
    /// `single_window: false` skips exactly two things: `home::render`'s
    /// menu-bar/dock children (separate `duduclaw-shell-menubar`/`-dock`
    /// layer surfaces in that mode instead — see `home::render_desktop`'s
    /// own doc comment) and the trailing overlay-render block (the active
    /// overlay, if any, is a separate `duduclaw-shell-overlay` layer
    /// surface window instead — see `chrome::windows::SurfaceView::
    /// reconcile_overlay_window`). Everything else — the diag probes, the
    /// OOBE branch, the lock-screen branch, every action/key/mouse
    /// listener, focus tracking, the `theme::app_font()` call — is
    /// byte-identical in both modes.
    /// A1 (2026-08-23): act on whatever comp has queued for us.
    ///
    /// Reuses `on_toggle_launcher` wholesale rather than re-implementing
    /// "open the task bar": that handler already carries the lockscreen
    /// reveal, the OOBE no-op, the query-field focus move and the three
    /// overlay-state resets, and a second copy of those guards would be a
    /// place for the two paths to drift. Super+K and ⌘K therefore do
    /// exactly the same thing by construction — which is also what
    /// `docs/features/51-os-keyboard-shortcuts.md` tells the operator.
    fn settle_global_task_intents(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.global_task.take_task_bar_request() {
            return;
        }
        if diag_enabled() {
            eprintln!("[global_task] comp reported Super+K — raising the task bar");
        }
        self.on_toggle_launcher(&ToggleLauncher, window, cx);
    }

    /// D6 (2026-08-23): the ONE task that starts the notification daemon and
    /// then keeps the notification centre in step with it.
    ///
    /// ## Why a self-re-arming loop and not a per-render one-shot
    ///
    /// This crate has already paid for the other shape once. WP-A4-4's
    /// appliance-VM incident (`overlay::notifications::schedule_stale_check`
    /// carries the full post-mortem) was a timer pile-up: every render armed
    /// a fresh timer, each firing timer caused a repaint, each repaint armed
    /// another — pending timers grew for the machine's whole uptime until
    /// `cage` sat at ~100% CPU on a static screen. So this follows the shape
    /// that fixed it: claim a single slot (`NotificationCenter::
    /// try_arm_drain`), then loop internally. Callers may call this from a
    /// render body as often as they like; every call after the first fails
    /// the claim and returns immediately.
    ///
    /// ## Why it is armed from `render_root` and not from the panel
    ///
    /// A notification that only arrives while the operator happens to have
    /// the panel open is not a notification. `render_root` is the one place
    /// that renders in every chrome mode and every surface state, so arming
    /// here means the daemon is up (and the centre is being fed) from the
    /// first frame, whether or not anything is looking at it.
    ///
    /// ## Cost when nothing is happening
    ///
    /// One uncontended mutex acquire per `DRAIN_INTERVAL` (250ms) and an
    /// `Instant` comparison. `cx.notify()` is called ONLY when the data
    /// actually moved, so an idle machine does zero extra repaints — the
    /// exact rule WP-A4-4 established ("無變化不 notify").
    fn schedule_notification_drain(&mut self, cx: &mut Context<Self>) {
        if !self.notify_center.try_arm_drain() {
            return;
        }
        cx.spawn(async move |weak, cx| {
            loop {
                let keep_going = weak.update(cx, |view, cx| {
                    // Starting is idempotent (see `NotifyRuntime::start`) but
                    // the first call is what actually spawns the thread —
                    // deliberately done INSIDE the task, not at construction,
                    // so a hung session bus can never delay window creation.
                    let mut changed = if view.notify_runtime.is_started() {
                        view.notify_center.set_daemon(view.notify_runtime.status())
                    } else {
                        let state = view.notify_runtime.start();
                        view.notify_center.set_daemon(state)
                    };

                    let now = std::time::Instant::now();
                    let drained = view.notify_runtime.inbox.drain();
                    changed |= view.notify_center.apply(drained, now);
                    changed |= view.notify_center.expire_due(now);

                    // Hand back whatever the last tick's operator actions —
                    // and this tick's expiries/evictions — owe the bus.
                    // Emitting is an `mpsc::send`, never a socket write: the
                    // notifyd thread does the I/O.
                    let emits = view.notify_center.take_emits();
                    if !emits.is_empty() {
                        view.notify_runtime.emit(emits);
                    }

                    if changed {
                        // DIAG-gated, because this prints third-party
                        // content into the operator's own journal: it is the
                        // one probe that proves a `Notify` call reached the
                        // UI data model (the live-fire evidence D6's brief
                        // asks for), and it costs nothing when DIAG is off.
                        if diag_enabled() {
                            let cards: Vec<String> = view
                                .notify_center
                                .items()
                                .iter()
                                .map(|c| {
                                    let merged = if c.merged > 0 { format!(" (+{} merged)", c.merged) } else { String::new() };
                                    let acts = if c.actions.is_empty() {
                                        String::new()
                                    } else {
                                        format!(" [{}]", c.actions.iter().map(|a| a.key.as_str()).collect::<Vec<_>>().join(","))
                                    };
                                    format!("#{} {}: {}{merged}{acts}", c.id, c.app_name, c.summary)
                                })
                                .collect();
                            eprintln!("[notifyd] centre now holds {} card(s): {}", view.notify_center.len(), cards.join(" | "));
                        }
                        cx.notify();
                    }
                    true
                });
                if keep_going.is_err() {
                    // The view is gone (window closed) — the slot went with
                    // it, and dropping `ShellView` drops the daemon handle,
                    // which releases the bus name.
                    return;
                }
                cx.background_executor().timer(notifyd::center::DRAIN_INTERVAL).await;
            }
        })
        .detach();
    }

    /// A1 result-loopback (2026-08-24): the poll loop behind
    /// `task_results` — see that field's own doc comment and `task_result`'s
    /// module doc for the full design.
    ///
    /// Same "claim the single slot once, loop internally" shape
    /// `schedule_notification_drain` above establishes, armed from the same
    /// call site for the same reason (a task that finishes while nobody has
    /// any overlay open must still produce a notification). Unlike that
    /// loop, this one performs real network I/O each tick it actually polls
    /// (`gateway_client::list_tasks`), so it follows `overlay/
    /// notifications.rs::trigger_refresh_if_stale`'s established
    /// thread + `mpsc` + `cx.spawn` bridge for the I/O itself, nested inside
    /// the same self-re-arming sleep this fn's outer loop already needs.
    /// `TaskResultTracker::begin_poll` itself refuses (cheaply, no I/O) when
    /// nothing is watched, so an idle machine that has never delegated
    /// anything pays only the sleep — no socket is ever opened.
    fn schedule_task_result_poll(&mut self, cx: &mut Context<Self>) {
        if !self.task_results.try_arm_poll() {
            return;
        }
        cx.spawn(async move |weak, cx| {
            loop {
                let delay = match weak.update(cx, |view, _cx| view.task_results.next_check_delay()) {
                    Ok(d) => d,
                    Err(_) => return, // view gone
                };
                cx.background_executor().timer(delay).await;

                let poll_input = weak.update(cx, |view, _cx| {
                    if !view.task_results.begin_poll() {
                        return None;
                    }
                    Some((view.task_results.session_jwt().map(str::to_string), view.task_results.watch_agent_id().unwrap_or_default().to_string()))
                });
                let Ok(Some((existing_jwt, agent_id))) = poll_input else {
                    if poll_input.is_err() {
                        return; // view gone
                    }
                    continue; // nothing due yet — sleep again
                };

                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    let _ = tx.send(task_result_poll_once(existing_jwt, agent_id));
                });
                let outcome = loop {
                    match rx.try_recv() {
                        Ok(v) => break Some(v),
                        Err(std::sync::mpsc::TryRecvError::Empty) => {}
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => break None,
                    }
                    cx.background_executor().timer(TASK_RESULT_BRIDGE_POLL_INTERVAL).await;
                };
                let Some((new_jwt, result)) = outcome else {
                    continue; // the worker thread vanished without sending — treat as a lost tick, try again next cadence
                };

                let updated = weak.update(cx, |view, cx| {
                    if let Some(jwt) = new_jwt {
                        view.task_results.apply_session(jwt);
                    }
                    match result {
                        Ok(snapshots) => {
                            let events = view.task_results.apply_poll_ok(snapshots);
                            if !events.is_empty() {
                                for event in events {
                                    post_task_result_card(view, &event);
                                }
                                cx.notify();
                            }
                        }
                        Err(e) => {
                            if diag_enabled() {
                                eprintln!("[task_result] poll failed: {e:?}");
                            }
                            view.task_results.apply_poll_err();
                        }
                    }
                });
                if updated.is_err() {
                    return; // view gone
                }
            }
        })
        .detach();
    }

    pub(crate) fn render_root(&mut self, window: &mut Window, cx: &mut Context<Self>, single_window: bool) -> impl IntoElement {
        if self.diag {
            eprintln!("[render] overlay={:?}", self.surface.overlay());
        }
        // A1 (2026-08-23): act on anything comp has queued for us. The
        // POLLING that fills that queue is NOT driven from here — see
        // `spawn_global_task_poll_loop` for why a render-pass-gated poll was
        // wrong. This half stays on the render pass because opening the task
        // bar needs a real `&mut Window`, which only a render pass has.
        self.settle_global_task_intents(window, cx);
        // D6 (2026-08-23): starts the `org.freedesktop.Notifications` daemon
        // on the first pass and keeps the notification centre fed thereafter.
        // Idempotent by construction — see its own doc comment.
        self.schedule_notification_drain(cx);
        // A1 result-loopback (2026-08-24): the terminal-state watch behind
        // "Super+K 交辦一個任務 → 結果推回殼". Same idempotent single-arm
        // shape as the call directly above.
        self.schedule_task_result_poll(cx);
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
            // ICON-1 (2026-08-22): the other half of the font fix. Loading
            // the faces into the text system (see `main`'s `add_fonts`
            // call) only makes them AVAILABLE — gpui still lays every glyph
            // out in its platform default family until something asks for
            // one, and until this round nothing in this crate ever did (no
            // `.font(...)`/`.font_family(...)` call existed anywhere in
            // `src/`). Set once on the root, where it cascades to OOBE, the
            // lockscreen, Home and every overlay alike — the same single
            // call site `duduclaw-native-gui/src/main.rs:392` uses for its
            // own root. `theme::app_font()` resolves to the "Inter
            // Variable" family with "Noto Sans TC" as its per-glyph
            // fallback (see that function's own doc comment for how the
            // family names were verified against the TTFs' name tables).
            .font(theme::app_font())
            .track_focus(&self.focus_handle)
            .key_context("Shell")
            .on_action(cx.listener(Self::on_toggle_launcher))
            .on_action(cx.listener(Self::on_close_overlay))
            .on_action(cx.listener(Self::on_oobe_next))
            .on_action(cx.listener(Self::on_lock_now))
            // D9 (2026-08-23): Tab/Shift-Tab field traversal. Registered on
            // the SAME root element as the four above, for the same reason
            // this file's header comment gives — and registered in BOTH
            // chrome modes, so the shortcut does not silently depend on
            // whether layer surfaces came up (`chrome/windows.rs` carries
            // the identical pair for the `LayerSurfaces` overlay root).
            // The handlers self-guard on "is OOBE active", so having them
            // bound outside OOBE costs nothing and steals no Tab.
            .on_action(cx.listener(Self::on_focus_next))
            .on_action(cx.listener(Self::on_focus_prev))
            // Shell-S4-lock: always-on (NOT `self.diag`-gated, unlike the
            // pre-existing raw-input PROBE pair further down — these are
            // two SEPARATE listener registrations on the same element;
            // gpui's `key_down_listeners`/`mouse_down_listeners`/`mouse_
            // move_listeners` are `Vec`s that accumulate rather than
            // overwrite, confirmed against the pinned gpui rev's own
            // `elements/div.rs` before relying on this, so adding these
            // does not disturb the diagnostic pair below). WP-lock-pw
            // (2026-08-22): any key or click REVEALS the password prompt
            // while locked — it no longer unlocks by itself (reversed from
            // the original Shell-S4-lock MVP); while unlocked, they just
            // refresh the idle-auto-lock clock. See `lockscreen::render::
            // note_input_or_reveal`'s own doc comment for why `cmd-k`/
            // `escape`/`enter` are NOT covered by this catch-all (those
            // three go through `on_toggle_launcher`/`on_close_overlay`/
            // `on_oobe_next`'s own lock-checks instead — a bound key never
            // reaches a raw key listener on the same element).
            .on_key_down(cx.listener(|view, _ev, window, cx| {
                lockscreen::render::note_input_or_reveal(view, window, cx);
            }))
            // D3-b (2026-08-23): the Launcher's live search typing used to be a
            // SECOND raw `.on_key_down` registration right here, appending
            // `keystroke.key_char` into a `String`. It is gone: the search box
            // is a real `EntityInputHandler`-backed field now
            // (`launcher_query_field`, focused by `settle_launcher_query`
            // whenever the Launcher opens), which is the only shape that can
            // receive an IME commit at all — the old one could not compose
            // Chinese by construction, and keeping it would additionally have
            // DOUBLE-inserted every character, since gpui's platform layers
            // hand an un-consumed printable key to the focused input handler
            // themselves (see `duduclaw-native-gui/src/ime_input/
            // input_state.rs`'s header comment for the two exact call sites).
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, _ev, window, cx| {
                    lockscreen::render::note_input_or_reveal(view, window, cx);
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
        // D3-b (2026-08-23): publish it as the ambient `ShellPalette` global
        // BEFORE any surface renders. `oobe::widgets::OobeTextField` (the
        // shared text field used by OOBE, the lockscreen password prompt and
        // the Launcher search box alike) reads its colors from this global
        // rather than from a render parameter — see that type's own doc
        // comment. Until now only `oobe::render::render` ever set it, so the
        // lockscreen field was painted with whatever palette OOBE happened to
        // leave behind (or the light default on a boot that skipped OOBE
        // entirely). OOBE still overwrites this with the flow's own palette
        // on the branch below, so its behavior is unchanged.
        cx.set_global(home_palette);
        root = if let Some(flow) = &self.oobe {
            root.child(oobe::render(flow, &self.oobe_ui, &self.oobe_account_fields, &self.oobe_network_fields, cx))
        } else if self.lockscreen.is_locked() {
            // Shell-S4-lock: same "takes over the root's ENTIRE child, no
            // app chrome underneath" shape OOBE establishes above — Home
            // isn't rendered at all while locked, so there is nothing for a
            // Home overlay to sit on top of (mirrors the `self.oobe.is_
            // none()` guard on the overlay-render block further down).
            root.child(lockscreen::render::render(
                &self.lockscreen,
                &self.overlay_ui.notifications,
                &self.lockscreen_password_field,
                self.operator_name.as_deref(),
                cx,
            ))
        } else if single_window {
            // WP-comp-shell-ipc: `&self.running_windows` threaded down the
            // same way `&self.overlay_ui.notifications` already is just
            // above — an immutable borrow of one `self` field alongside
            // `cx` (a separate parameter, not a second borrow of `self`),
            // same shape, no conflict.
            root.child(home::render(
                home_palette,
                &self.overlay_ui.notifications,
                &self.running_windows,
                &self.installed_apps,
                &self.overlay_ui.task_progress,
                cx,
            ))
        } else {
            // WM-3, `ChromeMode::LayerSurfaces`: the menu bar and dock are
            // separate `duduclaw-shell-menubar`/`-dock` layer surfaces (see
            // `chrome::windows::render_surface_content`), so this window's
            // OWN content is everything else — `home::render_desktop`, not
            // `home::render`. See `home::render_desktop`'s own doc comment.
            // The function is `home::desktop_content` (its doc comment is
            // the one this note points at); the call site said
            // `render_desktop`, which never existed. Corrected 2026-08-23
            // while wiring D4b — the crate did not compile until it was.
            root.child(home::desktop_content(home_palette, cx))
        };
        if self.diag {
            root = root
                .on_key_down(cx.listener(|_, ev: &KeyDownEvent, _, _| {
                    // Q1 (2026-08-24): the typed CHARACTER is never printed.
                    //
                    // This listener sits on the window ROOT, and the lock
                    // screen renders as a child of that same root — so a
                    // keystroke typed into the password field bubbles past the
                    // field (whose own `OobeTextField::on_key_down` does not
                    // stop propagation) and reaches here. The previous version
                    // `{:?}`-formatted the whole `Keystroke`, whose `Debug`
                    // includes `key_char: Some("a")`. That was not theoretical:
                    // `lockscreen/render.rs`'s own `reveal_and_focus` comment
                    // records three such lines being MEASURED on the appliance
                    // while typing a password, and under the kiosk unit stderr
                    // goes to journald on persistent storage.
                    //
                    // Everything this probe was actually built to answer —
                    // "did a key arrive at all, and with which modifiers" — is
                    // kept. Only the identity of the key is dropped. Note the
                    // shipping gate on `diag` above is a SECOND line of
                    // defence, not the fix: a password must not be written to
                    // a log in any build.
                    //
                    // Only the modifiers are printed. Not even the character's
                    // LENGTH is reported: reading that field here would trip
                    // this crate's own `no_shell_surface_hand_rolls_raw_
                    // character_text_entry` guard, and the byte count adds
                    // nothing to "did a key arrive" while leaking a little
                    // about what was typed.
                    eprintln!(
                        "[probe] os key_down: modifiers={:?} (key identity withheld)",
                        ev.keystroke.modifiers
                    );
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
        //
        // WM-3: also guarded on `single_window` — in `ChromeMode::
        // LayerSurfaces` the active overlay (if any) is a separate
        // `duduclaw-shell-overlay` layer surface window, reconciled by
        // `chrome::windows::SurfaceView::reconcile_overlay_window`, not a
        // child appended here.
        if single_window && self.oobe.is_none() && !self.lockscreen.is_locked() {
            if let Some(active) = self.surface.overlay() {
                // Backdrop click-to-close — now a real `cx.listener` (round
                // 1's stub only logged, see that commit's own doc comment
                // for why: a plain closure can't reach `&mut ShellView`).
                // Building it via `cx.listener` first, THEN passing `cx`
                // again into `overlay::render`, keeps the two borrows of
                // `cx` sequential rather than interleaved in one
                // expression.
                let on_close = cx.listener(|view, _ev, window, cx| {
                    if diag_enabled() {
                        eprintln!("[hit] backdrop -> close overlay");
                    }
                    view.surface.close();
                    view.settle_launcher_query(window, cx);
                    // See `on_toggle_launcher`'s own note on these five.
                    view.pointer_ui.reset();
                    view.overlay_ui.codrive.reset();
                    view.overlay_ui.wifi_tile.reset();
                    view.settings_ui.reset();
                    view.settings_fields.clear_all(cx);
                    cx.notify();
                });
                root = root.child(overlay::render(
                    active,
                    &self.overlay_ui,
                    &self.audio_ui,
                    &self.installed_apps,
                    &self.pointer_ui,
                    &self.launcher_query_field,
                    &self.settings_ui,
                    &self.settings_fields,
                    // D6 (2026-08-23): third-party app notifications, drawn
                    // by the Notifications panel alongside the gateway's
                    // approval cards. Threaded through exactly the way
                    // `audio_ui`/`installed_apps` already are.
                    &self.notify_center,
                    home_palette,
                    on_close,
                    cx,
                ));
            }
        }
        root
    }
}

/// WM-3: the `SingleFullscreen` entry point — `ChromeMode::SingleFullscreen`
/// (macOS always; Linux on `DUDUCLAW_SHELL_NO_LAYER_SHELL=1` or when the
/// `LayerSurfaces` attempt fails at runtime, see `chrome::windows::
/// boot_windows`) opens exactly one window with `Entity<ShellView>` as its
/// root view directly, which is what makes THIS impl the one gpui actually
/// calls — see `render_root`'s own doc comment for what it does.
impl Render for ShellView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_root(window, cx, true)
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
// One more as of WP-lock-pw (2026-08-22, lockscreen PASSWORD unlock —
// reverses the any-key-unlock MVP above, see `lockscreen/mod.rs`'s own
// header comment for the full writeup):
// One more as of D3-b (2026-08-23, IME wiring) — owned by
// `duduclaw-native-gui`'s shared text widget rather than by this crate, but
// listed here because it is read by THIS binary's process:
//   - `DUDUCLAW_IME_TRACE=1` — logs one line per `EntityInputHandler`
//     callback (preedit set/replaced, commit, unmark, backspace), so an
//     fcitx5 bring-up can tell "the compositor never delivered the commit"
//     apart from "the widget mishandled it". MASKED fields log lengths and
//     cluster counts only, never their text. Default off; read live in
//     `duduclaw_native_gui::ime_input`'s `trace_enabled()`.
//   - `DUDUCLAW_SHELL_LOCK_NO_PASSWORD=1` — dev/headless escape hatch:
//     reproduces the ORIGINAL any-key-unlock behavior verbatim, no password
//     prompt, no gateway round trip. Any other value (including unset)
//     means "password required" — the safe default. Read live in
//     `lockscreen::password_required_from_env()`.
fn main() {
    eprintln!("[main] starting duduclaw-shell S0");
    // Q1 (2026-08-24): a build with the debug affordances compiled in says so
    // on its first line of stderr, unconditionally — so "why does this
    // machine behave oddly" is answerable from the journal alone. Silent in a
    // shipping build. See `crate::shipping`.
    shipping::announce_build_flavour();

    // ICON-1 (2026-08-22): the shell's embedded SVG asset source. This is
    // the ONLY point where it can be installed — `Application::with_assets`
    // (`gpui/src/app.rs:200`) consumes the pre-launch `Application` value
    // and rebuilds the `SvgRenderer` around the new source; there is no
    // post-`run()` equivalent. Without it gpui's default `impl AssetSource
    // for ()` answers `Ok(None)` to every path, which is why `gpui::svg()`
    // could never have drawn anything in this crate before now.
    application().with_assets(icons::ShellAssets).run(move |cx: &mut App| {
        // ICON-1 (2026-08-22): the bundled font stack. Two facts made this
        // necessary and neither was true of the comment it replaces
        // (`home.rs` used to record "duduclaw-native-gui's main.rs loads
        // these"):
        //   1. On the appliance the kiosk unit launches THIS binary
        //      directly (`appliance/mkosi.extra/usr/local/sbin/
        //      duduclaw-kiosk-launch.sh`); `duduclaw-native-gui`'s `main.rs`
        //      never runs there, so nothing it registers reaches the shell.
        //   2. The appliance image ships neither face — `fc-match
        //      sans-serif` there resolves to DejaVu Sans, and its CJK
        //      coverage is Noto Sans CJK TC, not the Noto Sans TC the
        //      boards specify. Every screen the operator has seen so far
        //      rendered in those substitutes.
        //
        // Placed FIRST inside `run`, ahead of `open_window` — the same
        // ordering (and the same reason) `duduclaw-native-gui/src/main.rs`
        // documents at its own `add_fonts` call: text laid out before
        // `add_fonts` returns resolves against gpui's system-font default
        // and never re-flows onto the bundled faces afterwards.
        //
        // The bytes come from `duduclaw-native-gui`'s lib rather than a
        // second copy checked in here: they are the SAME two faces
        // `theme::app_font()` names, that function already lives in that
        // module, and a duplicated 12.8 MB TTF in a second `assets/` tree
        // is exactly the kind of divergence `duduclaw-native-gui/src/lib.rs`
        // was created to prevent ("not a bin-local copy plus a lib-local
        // copy silently diverging"). Loading them is only half the fix —
        // `Render::render` also has to ASK for the family, see the
        // `.font(theme::app_font())` call on the root element there.
        //
        // Fail-open, same shape as that crate's own call site: a font
        // failure degrades this run to the system fallback (exactly today's
        // behavior) and is logged loudly; it must never stop the window
        // from opening.
        match cx.text_system().add_fonts(theme::bundled_font_bytes()) {
            Ok(()) => eprintln!("[main] add_fonts ok: InterVariable + NotoSansTC-Variable loaded"),
            Err(e) => eprintln!("[main] add_fonts FAILED (falling back to system font): {e}"),
        }

        // WM-3: this crate's fixed dev-mode window size used to be computed
        // here unconditionally; it is now computed only where it's actually
        // used (`fallback_window`'s `#[cfg(not(target_os = "linux"))]`
        // block, further down) — on Linux in `ChromeMode::LayerSurfaces`
        // there is no single "the window bounds" to speak of (each layer
        // surface has its own, computed in `chrome::gpui_bridge`), so
        // keeping this binding unconditional would be a dead, unused `let`
        // on that path.

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
        // ICON-3 (2026-08-23): same read-before-`resolve_boot_flow`-consumes-
        // it shape as `initial_theme` just above — see `ShellView.
        // operator_name`'s own doc comment. Unlike `ThemeChoice` this one
        // is not `Copy`, so it clones the string out rather than the state.
        let initial_operator_name = oobe::boot_operator_name(&persisted_oobe_state);
        // Q1 (2026-08-24): all three read through the shipping gate. Forcing,
        // skipping or jumping the first-run flow decides whether a machine
        // ever performs its device claim, which is not something an operator
        // env file on a duty appliance should be able to change. See
        // `crate::shipping`.
        let force_oobe = shipping::debug_env("DUDUCLAW_SHELL_FORCE_OOBE");
        let skip_oobe = shipping::debug_env("DUDUCLAW_SHELL_SKIP_OOBE");
        let debug_oobe_step = shipping::debug_env("DUDUCLAW_SHELL_DEBUG_OOBE_STEP");
        let initial_oobe =
            oobe::resolve_boot_flow(force_oobe.as_deref(), skip_oobe.as_deref(), debug_oobe_step.as_deref(), persisted_oobe_state);
        match &initial_oobe {
            Some(flow) => eprintln!("[main] OOBE boot resolution: OOBE at {:?}", flow.current()),
            None => eprintln!("[main] OOBE boot resolution: Home (OOBE already completed or skipped)"),
        }
        eprintln!("[main] Home/overlay boot theme: {initial_theme:?}");
        // D2 (2026-08-23): push the boot theme to comp too, so an
        // application window mapped before the operator ever opens a
        // settings surface already gets a matching frame. Comp defaults to
        // `light` and `ThemeChoice` defaults to `Light`, so this is a no-op
        // in the common case — it matters for a machine whose persisted
        // pick is Dark, where without it every window frame would stay
        // light until the next theme change. Non-blocking; see
        // `notify_comp_theme`.
        notify_comp_theme(initial_theme);

        // D9-bug3/D9-bug4 (2026-08-24): and announce that this session is
        // NOT locked. comp's own default is already `false`, so this is a
        // no-op against a compositor that started with (or after) this
        // shell — it matters when comp OUTLIVES a shell that died while
        // locked. comp deliberately keeps the lock in that case (windows
        // stay hidden, the safe direction — see comp's `session_lock.rs`
        // "Fail-closed choices"), and this line is what lets the
        // supervisor-restarted shell take the screen back. Non-blocking;
        // see `notify_comp_session_locked`.
        notify_comp_session_locked(false);

        // WM-3 (2026-08-23): `shared_state` replaces the single window's
        // root-view construction that used to happen inline inside `cx.
        // open_window`'s builder closure — see `crate::chrome`'s module doc
        // for why this now happens BEFORE any window opens. The SAME
        // `Entity<ShellView>` is reused EITHER as `ChromeMode::
        // SingleFullscreen`'s one window's root view directly, OR wrapped by
        // N `chrome::windows::SurfaceView`s in `ChromeMode::LayerSurfaces` —
        // never duplicated. Every `::new(cx)` call below only ever needed
        // `&mut App` (never a live `Window` — confirmed by reading each
        // one's own signature in `oobe/widgets.rs`), so moving them to here,
        // before any window exists, changes nothing about what they do.
        let oobe_account_fields = oobe::AccountFields::new(cx);
        let oobe_network_fields = oobe::NetworkFields::new(cx);
        let lockscreen_password_field = oobe::LockPasswordField::new(cx);
        let launcher_query_field = oobe::LauncherQueryField::new(cx);
        // D4b: the settings app's eight fields, same call site and same
        // "create once, unconditionally" reasoning as every bundle above it.
        let settings_fields = oobe::SettingsFields::new(cx);
        let shared_state = cx.new(|cx| ShellView {
            surface: SurfaceState::default(),
            overlay_ui: overlay::OverlayUiState::default(),
            audio_ui: audio::AudioUiState::default(),
            pointer_ui: overlay::pointer_settings::PointerUiState::default(),
            settings_ui: settings::SettingsUiState::default(),
            settings_fields,
            lockscreen: lockscreen::LockScreenState::default(),
            running_windows: home::running_windows::RunningWindowsFeed::default(),
            global_task: global_task::GlobalTaskIntentFeed::default(),
            installed_apps: apps::feed::InstalledAppsFeed::default(),
            // D6: constructed idle — the bus connection is not made here.
            // `ShellView::schedule_notification_drain` starts the daemon
            // thread from the first render pass, so a failing/absent session
            // bus can never delay or break window creation.
            notify_runtime: notifyd::NotifyRuntime::default(),
            notify_center: notifyd::center::NotificationCenter::default(),
            // A1 result-loopback: nothing delegated yet this session.
            task_results: task_result::TaskResultTracker::default(),
            lockscreen_password_field,
            launcher_query_field,
            operator_name: initial_operator_name,
            oobe: initial_oobe,
            oobe_ui: oobe::OobeUiState::default(),
            oobe_account_fields,
            oobe_network_fields,
            theme: initial_theme,
            focus_handle: cx.focus_handle(),
            // Q1 (2026-08-24): through the shipping gate, exactly like the
            // free `diag_enabled()` — NOT a raw `std::env::var`. This field
            // drives the boot-time auto-`cmd-k` dispatch and the keystroke
            // probe below; reading the env directly here was the miss that
            // let `DUDUCLAW_SHELL_DIAG=1` from `/etc/duduclaw/kiosk.env`
            // re-enable both in a shipping binary. `debug_env_is_one` folds to
            // `false` in a shipping build. See `crate::shipping`.
            diag: shipping::debug_env_is_one("DUDUCLAW_SHELL_DIAG"),
            diag_scheduled: false,
        });

        // WM-1 (2026-08-23): `app_id` is how `duduclaw-comp` tells the
        // session shell apart from an ordinary application window — see
        // `chrome::SHELL_APP_ID`'s own doc comment (now the one place this
        // string is spelled; every window this crate opens, layer-shell or
        // fallback alike, declares it). Comp has a first-mapped-toplevel
        // fallback for the boot path — gpui sends `set_app_id` after its
        // first commit, so the initial configure necessarily arrives
        // without it — but this is the authoritative signal, and it is also
        // what makes the shell identifiable to `list_windows` /
        // `activate_window` / `window_geometry`. Keep it in sync with
        // `crates/duduclaw-comp/src/window_policy.rs`'s `SHELL_APP_ID`.
        //
        // `ChromeMode::LayerSurfaces` (Linux, the default) opens the menu
        // bar / dock / desktop as three separate wlr-layer-shell windows
        // (plus a fourth, on-demand, for whichever overlay is open) — see
        // `crate::chrome`'s module doc for the whole design and
        // `chrome::windows::boot_windows` for the fallback-on-failure
        // dance. Every other platform, and Linux with
        // `DUDUCLAW_SHELL_NO_LAYER_SHELL=1` set, keeps this crate's
        // original single fullscreen window — byte-identical to before this
        // round — via the `#[cfg(not(target_os = "linux"))]` arm below.
        // `fallback_window` is `Some` only in that single-window case; the
        // one remaining call site below that needs it (the `DUDUCLAW_SHELL_
        // DEBUG_SURFACE` overlay hook, further down) branches on it.
        #[cfg(target_os = "linux")]
        chrome::windows::boot_windows(cx, shared_state.clone());
        #[cfg(target_os = "linux")]
        let fallback_window: Option<gpui::WindowHandle<ShellView>> = None;
        // A1 (2026-08-23): one process-wide loop that drains comp's global
        // hotkey queue. Started here — once — rather than from a render
        // pass; see `spawn_global_task_poll_loop` for the P0 that taught us
        // the difference. Harmless on a host with no compositor (macOS dev
        // loop): the first poll fails, the loop backs off to its slow
        // cadence, and `give_up` retires it entirely against a comp that
        // does not implement the op.
        spawn_global_task_poll_loop(shared_state.clone(), cx);
        #[cfg(not(target_os = "linux"))]
        let fallback_window: Option<gpui::WindowHandle<ShellView>> = {
            let bounds = Bounds::centered(None, size(px(1440.0), px(900.0)), cx);
            let window = cx
                .open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(bounds)),
                        app_id: Some(chrome::SHELL_APP_ID.to_string()),
                        ..Default::default()
                    },
                    {
                        let shared_state = shared_state.clone();
                        move |_window, _cx| shared_state.clone()
                    },
                )
                .expect("failed to open window");
            eprintln!("[main] window opened");
            // Give the root element real keyboard focus — see this file's
            // header comment ("Keyboard dispatch needs a focused element,
            // full stop.") for why this call is not optional. Same call
            // site as zed's own `crates/gpui/examples/input.rs`
            // `run_example()`: right after the window+view are created,
            // before `cx.activate(true)`.
            let _ = window.update(cx, |view, window, cx| {
                window.focus(&view.focus_handle, cx);
            });
            Some(window)
        };

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
            // WP-oobe-tab (2026-08-23): OOBE-only field-to-field navigation
            // — see `FocusNext`/`FocusPrev`'s own doc comment just above
            // their `actions!` declaration for why these are bound globally
            // with the OOBE guard inside the handler instead.
            KeyBinding::new("tab", FocusNext, None),
            KeyBinding::new("shift-tab", FocusPrev, None),
        ]);

        // Shell-S4-lock: the idle-auto-lock watchdog — started exactly ONCE
        // here, not from `render_root` (unlike this surface's own
        // clock-tick/stale-check timers, which self-re-arm only while
        // ALREADY locked — see `lockscreen::render::spawn_idle_watchdog`'s
        // own doc comment for why THIS one has to run continuously from
        // boot instead). WM-3: routed through `shared_state.update(...)`
        // rather than a specific window's `WindowHandle::update` — `spawn_
        // idle_watchdog` only ever needed a `Context<ShellView>`, never a
        // `Window`, so this is identical in both chrome modes (there may be
        // zero, one, or four windows open by this point depending on mode;
        // none of that matters here).
        shared_state.update(cx, |_view, cx| {
            lockscreen::render::spawn_idle_watchdog(cx);
        });

        // Debug-only boot override for headless smoke runs — this crate has
        // no scriptable UI-click automation (same gap
        // `duduclaw-native-gui/src/main.rs`'s own `DUDUCLAW_NATIVE_GUI_
        // DEBUG_PAGE` hook works around for that crate). Unset by default;
        // `DUDUCLAW_SHELL_DEBUG_SURFACE=launcher|notifications|
        // controlcenter|pointer|lockscreen` opens that surface immediately after
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
        //
        // WM-3: the "lockscreen" arm and `spawn_idle_watchdog` above route
        // through `shared_state.update(...)` (no `Window` ever needed —
        // both `lockscreen::render::lock_and_refresh` and `::spawn_idle_
        // watchdog` only take `&mut ShellView`/`&mut Context<ShellView>`),
        // identical in both chrome modes. The overlay-opening arm is the ONE
        // place here that genuinely differs: in `SingleFullscreen` mode
        // (`fallback_window: Some(_)`) it calls `settle_launcher_query`
        // directly through that window, exactly as before this round; in
        // `LayerSurfaces` mode (`fallback_window: None`) `settle_launcher_
        // query` cannot correctly focus the search field from here anyway
        // (see that method's own doc comment), so this arm just opens the
        // overlay and lets `chrome::windows::SurfaceView::
        // reconcile_overlay_window` pick it up — and focus it correctly
        // itself — on Home's next render pass.
        // Q1 (2026-08-24): behind the shipping gate — see `crate::shipping`.
        // `Ok(_)`/`Err(_)` become `Some(_)`/`None`, and a shipping build takes
        // the `None` arm unconditionally.
        match shipping::debug_env("DUDUCLAW_SHELL_DEBUG_SURFACE").ok_or(()) {
            Ok(raw) if raw.is_empty() => {}
            Ok(raw) if raw == "lockscreen" => {
                shared_state.update(cx, |view, cx| {
                    lockscreen::render::lock_and_refresh(view, cx);
                });
                eprintln!("[main] DUDUCLAW_SHELL_DEBUG_SURFACE=lockscreen -> locked");
            }
            Ok(raw) => match Overlay::from_debug_env(&raw) {
                Some(overlay) => {
                    if let Some(window) = fallback_window {
                        let _ = window.update(cx, |view, window, cx| {
                            view.surface.open(overlay);
                            // D3-b: `DUDUCLAW_SHELL_DEBUG_SURFACE=launcher`
                            // must land in the same focused state a real
                            // cmd-k does, or the headless smoke run would
                            // exercise a state the operator can never reach.
                            view.settle_launcher_query(window, cx);
                            cx.notify();
                        });
                    } else {
                        shared_state.update(cx, |view, cx| {
                            view.surface.open(overlay);
                            view.overlay_ui.close_launcher_query();
                            view.launcher_query_field.field.update(cx, |field, cx| field.clear(cx));
                            cx.notify();
                        });
                    }
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

#[cfg(test)]
mod tests {
    /// ICON-1 (2026-08-22) — the second half of the font fix, guarded.
    ///
    /// Registering the bundled faces with `add_fonts` and ASKING for them
    /// with `.font(...)` are independent, and getting only the first right
    /// is silent: `add_fonts` logs "ok", every screen still renders in the
    /// platform default, and nothing anywhere reports a problem. That was
    /// literally this crate's state until this round — `grep -r 'font_family\|\.font('
    /// src/` returned nothing at all.
    ///
    /// A source-text assertion is a crude instrument and is not pretending
    /// otherwise: it cannot prove the element reaches the screen (that is
    /// what the live visual check is for). What it CAN do is fail loudly if
    /// the one call this crate depends on is deleted or renamed, which is
    /// the specific regression that would otherwise be invisible. gpui has
    /// no way to interrogate a rendered element tree's resolved font from a
    /// plain unit test, and standing up a `TestAppContext` to open a window
    /// for one assertion buys no more certainty than the visual check
    /// already gives.
    /// D3-b (2026-08-23) — guards the regression that would silently kill
    /// Chinese input on DuDuClaw OS.
    ///
    /// Every shell text surface must route typed characters through the
    /// shared `EntityInputHandler` widget, NOT through a hand-rolled
    /// `keystroke.key_char` append. Reintroducing such an append is silent
    /// twice over: on a machine with no IME it looks like it works, and on a
    /// machine WITH one it both fails to compose and double-inserts (gpui's
    /// macOS and Wayland layers hand un-consumed printable keys to the
    /// focused input handler themselves — see `duduclaw-native-gui/src/
    /// ime_input/input_state.rs`'s header comment for the two call sites).
    ///
    /// A source-text assertion is a crude instrument, same caveat the font
    /// test below states: it cannot prove composition works end to end (that
    /// is what the fcitx5 live test is for). What it CAN do is fail loudly
    /// the moment someone re-adds the shape that was just removed.
    #[test]
    fn no_shell_surface_hand_rolls_raw_character_text_entry() {
        // Assembled at compile time so this test's own source does not
        // contain the literal it searches for (it scans itself).
        let needle = concat!("key", "_char");
        for (name, source) in
            [("main.rs", include_str!("main.rs")), ("oobe/widgets.rs", include_str!("oobe/widgets.rs"))]
        {
            for line in source.lines() {
                let code = line.trim_start();
                if code.starts_with("//") {
                    continue; // the removal is DESCRIBED in comments on purpose
                }
                assert!(
                    !code.contains(needle),
                    "{name} reintroduced a hand-rolled per-keystroke text path: {line}"
                );
            }
        }
    }

    /// The Launcher's search box must stay a real focusable field entity.
    /// If it ever regresses to a plain `String` on `OverlayUiState`, IME
    /// commits have nowhere to land again.
    #[test]
    fn the_launcher_search_box_is_a_focusable_field_entity() {
        let source = include_str!("main.rs");
        assert!(
            source.contains("launcher_query_field: oobe::LauncherQueryField"),
            "ShellView no longer owns the Launcher's search-field entity"
        );
        assert!(
            source.contains("fn settle_launcher_query"),
            "the open/close focus hand-off for the Launcher search field is gone"
        );
    }

    /// D9-bug9 (2026-08-24), M1 round: `lock_and_refresh` must clear BOTH
    /// text fields it dismisses on every lock — the Launcher's search box
    /// (already fixed, D3-b) and the lockscreen's own password field (this
    /// round's fix). Same "source-scan, not a live gpui test" convention
    /// this module's own tests already establish (`lockscreen/render.rs`
    /// itself deliberately carries no `#[cfg(test)] mod tests` — see that
    /// file's own trailing comment) — cannot prove the field is empty on
    /// screen (that's the VM check), but fails loudly if either `.clear(cx)`
    /// call is removed.
    #[test]
    fn lock_and_refresh_clears_both_the_launcher_query_and_the_password_field() {
        let source = include_str!("lockscreen/render.rs");
        let start = source.find("pub(crate) fn lock_and_refresh").expect("lock_and_refresh not found");
        let end = source[start..].find("\n}\n").map(|i| start + i).unwrap_or(source.len());
        let body = &source[start..end];
        assert!(
            body.contains("view.launcher_query_field.field.update(cx, |field, cx| field.clear(cx))"),
            "lock_and_refresh no longer clears the Launcher's search field"
        );
        assert!(
            body.contains("view.lockscreen_password_field.field.update(cx, |field, cx| field.clear(cx))"),
            "lock_and_refresh no longer clears the lockscreen password field — a lock cycle that \
             never reached a clean submit (throttled, in-flight, or a synthetic repeat flood) would \
             leak its leftover content into the VERY NEXT lock's freshly-revealed prompt"
        );
    }

    #[test]
    fn the_root_element_applies_the_bundled_font() {
        let source = include_str!("main.rs");
        assert!(
            source.contains(".font(theme::app_font())"),
            "the shell root no longer applies theme::app_font() — every screen would silently \
             fall back to the platform default family (see this test's own doc comment)"
        );
        assert!(
            source.contains("add_fonts(theme::bundled_font_bytes())"),
            "the shell no longer registers the bundled faces — .font(app_font()) would then \
             resolve to nothing and fall back to the platform default"
        );
    }
}
