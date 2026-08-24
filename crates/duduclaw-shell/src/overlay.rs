// Overlay shared shell — Shell-S0 round 2.
//
// Round 1 gave `Launcher` / `Notifications` / `ControlCenter` a single
// generic "半透明遮罩＋置中面板顯示 surface 名稱" stub. This round replaces
// that with each overlay's REAL content, lifted from its own `.dc.html`
// artboard — `overlay::launcher` / `overlay::notifications` /
// `overlay::controlcenter` (and, since ICON-3, `overlay::pointer_settings`),
// one file each (`src/overlay/*.rs` — same
// "big screen, own directory" convention `home.rs` + `home/home_dock.rs`
// already established, chosen here over a separate top-level `overlays/`
// directory so this crate doesn't end up with two near-identically-named
// top-level modules). See each submodule's own header comment for its
// board citation and layout notes.
//
// ── Backdrop / panel are SIBLINGS, not parent/child ─────────────────────
// Round 1's stub nested the panel INSIDE the backdrop div and accepted
// "clicking the panel also closes the overlay" as an honest gpui
// limitation (no `stopPropagation` — click events bubble to every ancestor
// `.on_click`, same as `mds_gpui::dialog`'s own `dialog_overlay` doc
// comment calls out). That stopped being acceptable once panels have REAL
// buttons inside them (Notifications' approve/reject, ControlCenter's
// toggles) — a click on "核准" must NOT also close the overlay it just
// updated. Making backdrop and panel SIBLINGS under one relative wrapper
// (backdrop painted first, panel painted after — gpui, like CSS, hit-tests
// absolutely-positioned siblings in paint order, topmost last) means a
// click on the panel or anything inside it never reaches the backdrop's
// `.on_click` at all, since the panel isn't a DESCENDANT of the backdrop —
// there is nothing for the click to bubble through. This sidesteps the
// missing-`stopPropagation` gap entirely rather than working around it.
//
// ── Dimming ───────────────────────────────────────────────────────────
// Only Launcher's board (`Launcher.dc.html`) shows a dimmed full-screen
// backdrop (`rgba(15,23,42,.28)` + a `backdrop-filter: blur` gpui can't
// reproduce — same limitation `home.rs`'s header comment already documents
// for this crate). Notifications/ControlCenter's own boards render as an
// UNDIMMED floating panel over the live Home surface (the macOS
// Notification-Center / Control-Center convention, not a modal) — their
// backdrop is still present, so a click outside the panel still closes the
// overlay (consistent behavior across every overlay, PointerSettings
// included — its board is a settings window, which has no dimming of its own
// either), just fully transparent
// rather than omitted: an explicit zero-alpha `.bg(...)` keeps the click
// target real instead of betting on an unset background still being
// hit-testable.
//
// ── Menu bar divergence (accepted, not fixed this round) ────────────────
// `Notifications.dc.html` / `ControlCenter.dc.html` each render a full
// 1440×900 mock that includes an overlay-specific menu-bar VARIANT (e.g.
// ControlCenter's board swaps the approval ticker for "AI 團隊安靜工作中"
// and the ⌘K hint for a "控制中心" badge). This shell composes `home::
// render()` (which owns the ONE menu bar) underneath the active overlay
// unchanged — coupling the menu bar's content to which overlay is open
// would mean `home.rs` reaching into overlay state, out of scope for this
// round (task brief: "native-gui 這輪不要動", and nothing in the brief asks
// for this specific cross-surface wiring either). Home's menu bar keeps
// showing the default approval ticker + ⌘K hint no matter which overlay,
// if any, is open on top of it.

use gpui::{div, prelude::*, rgb, App, ClickEvent, Context, Div, Stateful, Window};

use crate::palette::ShellPalette;
use crate::surface::Overlay;
use crate::ShellView;

/// A2 (2026-08-23): the 共駕 row inside ControlCenter's 「AI 團隊」 card —
/// see its own header comment. `pub(crate)`, not private, for the same
/// reason `pointer_settings` below is: `main.rs` and `chrome::windows` both
/// call `CodriveUiState::reset` from their overlay-close paths.
pub(crate) mod codrive_row;
mod controlcenter;
/// WP-A4-4 (2026-08-22): the Launcher's flatpak install confirmation gate —
/// a pure state machine, see its own header comment.
pub(crate) mod install_gate;
/// A1 result-loopback (2026-08-24): `pub(crate)`, not private — `main.rs`
/// (a sibling module of this one) calls `launcher::try_submit_delegate`
/// directly from `ShellView::on_oobe_next`, same "sibling caller needs the
/// path" reasoning `install_gate`/`notifications` above already state for
/// theirs.
pub(crate) mod launcher;
// `pub(crate)`, not private: `home.rs` (a sibling module of this one, not a
// descendant) calls `notifications::open_and_refresh` directly from its two
// "open Notifications" click sites — see that fn's own doc comment.
pub(crate) mod notifications;
/// D6 (2026-08-23): the SAME panel's third-party app notification section
/// (`org.freedesktop.Notifications`, see `crate::notifyd`). Its own file
/// rather than more lines in `notifications.rs` — that file was already at
/// this crate's 800-line ceiling; see `notifications_apps`'s own header.
mod notifications_apps;
/// WP-A4-4 (2026-08-22): retry spacing + log denoise for the feed's gateway
/// poll. Its own module rather than more methods on `notifications_feed`
/// because it is a self-contained, clock-injected policy with a test suite
/// of its own — same "many small files, low coupling" convention this
/// crate's `Cargo.toml`/`gateway_client` comments already state.
mod notifications_backoff;
pub mod notifications_feed;
/// A4 (2026-08-24): the SAME panel's "進行中任務" section (real
/// `tasks.list(status="in_progress")` rows) — its own file for the same
/// "already at the line-count ceiling" reason `notifications_apps`'s own
/// header comment gives.
mod notifications_tasks;
/// ICON-3 (2026-08-23): 「協助工具 › 指向與點按」 — see its own header
/// comment for why the board's settings PAGE lands as an overlay here.
/// `pub(crate)`, not private: `main.rs` owns its `PointerUiState` field on
/// `ShellView`, same as it owns `audio::AudioUiState`.
pub(crate) mod pointer_settings;
/// A4 (2026-08-24): the in-progress task-board feed backing the dock badge
/// and the Notifications panel's "進行中任務" section — see that struct's
/// own header comment. `pub`, not private, for the same reason
/// `notifications_feed` is: `home_dock.rs` (a sibling module, not a
/// descendant) reads it for the dock badge count.
pub mod task_progress_feed;
/// D4a-6 (2026-08-24): the ControlCenter Wi-Fi quick tile's real backend —
/// see its own header comment. `pub(crate)`, not private, for the same
/// reason `codrive_row` above is: `main.rs` and `chrome::windows` both call
/// `WifiTileState::reset` from their overlay-close paths.
pub(crate) mod wifi_tile;

/// Runtime-mutable state backing the two overlays that have actual
/// interactive controls this round (round 1's `SurfaceState` only tracked
/// WHICH overlay is open, never what's inside one). Lives on `ShellView`
/// itself (see `main.rs`), not re-created per render call, so a
/// decision/toggle survives closing and reopening the overlay.
#[derive(Debug, Clone, PartialEq)]
pub struct OverlayUiState {
    /// Shell-S4 (2026-08-22, WP-S4-notif): the Notifications overlay's real
    /// approval-card feed, replacing the old fake index-aligned
    /// `approval_decisions: Vec<ApprovalDecision>` (see this struct's git
    /// history before this round). `pub`, not private, for the same reason
    /// `overlay_ui` itself is `pub(crate)` on `ShellView` — both
    /// `overlay::notifications` (the panel) and `home.rs` (the menu-bar
    /// ticker) read/mutate it, and both are different modules from this
    /// one — see `notifications_feed`'s own header comment for why they
    /// share ONE model rather than each keeping their own.
    pub notifications: notifications_feed::NotificationsFeed,
    /// A4 (2026-08-24): the in-progress task-board feed — same cross-module
    /// sharing reason `notifications` above states: `home_dock.rs` (the
    /// dock badge count) and this panel's own "進行中任務" section
    /// (`notifications_tasks::task_progress_section`) both read it. See
    /// `task_progress_feed::TaskProgressFeed`'s own header comment.
    pub task_progress: task_progress_feed::TaskProgressFeed,
    /// A2 (2026-08-23): the 共駕 row's compositor-backed state.
    ///
    /// It lives HERE rather than as a sibling field on `ShellView` (the shape
    /// `audio_ui`/`pointer_ui` use) for one concrete reason: this row renders
    /// INSIDE `controlcenter::ai_team_card`, which already receives
    /// `&OverlayUiState`. A sibling field would mean widening
    /// `overlay::render`, `controlcenter::render` and `ai_team_card`'s
    /// signatures plus their call sites — four files' worth of churn through
    /// code other work packages are editing this round, to deliver one row.
    /// See `codrive_row::CodriveUiState`'s own doc comment for what it holds.
    pub(crate) codrive: codrive_row::CodriveUiState,
    /// D4a-6 (2026-08-24): the Wi-Fi quick tile's real-status read. Lives
    /// here for the identical reason `codrive` does — it renders INSIDE
    /// `controlcenter::quick_tiles_row`, which already receives
    /// `&OverlayUiState`. See `wifi_tile::WifiTileState`'s own doc comment.
    pub(crate) wifi_tile: wifi_tile::WifiTileState,
    automation_on: bool,
    proactive_on: bool,
    pause_all_on: bool,
    /// WP-A4-4 (2026-08-22): `Some` while the Launcher is showing the
    /// flatpak install confirmation sheet. `None` — including after a
    /// cancel — means no install is pending or running; see
    /// `install_gate::InstallGate`'s own header comment for why "cancel ＝
    /// drop the gate" is enough to guarantee nothing runs.
    pub(crate) install_gate: Option<install_gate::InstallGate>,
}

impl Default for OverlayUiState {
    fn default() -> Self {
        Self {
            notifications: notifications_feed::NotificationsFeed::default(),
            // A4: nothing has been fetched yet — the same honest "no data
            // yet" starting point `notifications` above uses.
            task_progress: task_progress_feed::TaskProgressFeed::default(),
            // A2: nothing has been asked of the compositor yet — the row's
            // own `NotLoaded` default, which is what arms its first read.
            codrive: codrive_row::CodriveUiState::default(),
            // D4a-6: nothing has been read from `network.status` yet — the
            // tile's own `NotLoaded` default, which is what arms its first
            // read.
            wifi_tile: wifi_tile::WifiTileState::default(),
            // ControlCenter.dc.html: 自動化/主動行為 both render as an ON
            // (blue) toggle, 全部暫停 renders OFF (gray) — the design
            // board's actual snapshot state, kept verbatim as the boot
            // default rather than inventing a different one.
            automation_on: true,
            proactive_on: true,
            pause_all_on: false,
            install_gate: None,
        }
    }
}

impl OverlayUiState {
    pub fn automation_on(&self) -> bool {
        self.automation_on
    }

    pub fn proactive_on(&self) -> bool {
        self.proactive_on
    }

    pub fn pause_all_on(&self) -> bool {
        self.pause_all_on
    }

    pub fn toggle_automation(&mut self) {
        self.automation_on = !self.automation_on;
    }

    pub fn toggle_proactive(&mut self) {
        self.proactive_on = !self.proactive_on;
    }

    pub fn toggle_pause_all(&mut self) {
        self.pause_all_on = !self.pause_all_on;
    }

    /// Drops the state an overlay close must not leave behind.
    ///
    /// D3-b (2026-08-23): the Launcher's typed search no longer lives here —
    /// it is a real `Entity<OobeTextField>` on `ShellView` (see
    /// `oobe::LauncherQueryField`), because a plain `String` fed by a root
    /// key listener can never receive IME composition. Clearing it is
    /// `ShellView::settle_launcher_query`'s job, which calls this too; the
    /// name is kept so the three existing close paths read unchanged.
    pub(crate) fn close_launcher_query(&mut self) {
        // WP-A4-4: closing the Launcher also dismisses any pending install
        // confirmation. Dropping the gate is exactly what "取消" does — an
        // unconfirmed gate never handed out an install command (see
        // `install_gate::InstallGate`'s own doc comments), so this can not
        // abandon a running install; it only discards a question that was
        // never answered. A gate that IS already installing is likewise
        // dropped from view, because the child process is flatpak's now,
        // not this shell's — pretending the sheet still controls it would
        // be the dishonest option.
        self.install_gate = None;
    }
}

/// Dispatches to the active overlay's own content module and wraps it with
/// the shared backdrop — see this module's header comment for why backdrop
/// and panel are siblings, not nested. `on_close` fires on a backdrop click
/// only; a click anywhere inside the panel (including its buttons) never
/// reaches it. `palette` is resolved once per render pass by the caller
/// (`ShellView::render` in `main.rs`) — same convention `home::render`
/// establishes (see that fn's own doc comment).
///
/// The parameter list is long and gets one longer with each overlay that
/// owns real state (ICON-3's `pointer_ui` is the fourth). Bundling them into
/// a struct was considered and rejected: every field is an INDEPENDENT
/// immutable borrow of a different `ShellView` field, which is exactly what
/// lets the caller take them all at once alongside `&mut Context`; a wrapper
/// struct would have to be built from those same borrows anyway, buying a
/// type and a construction site and no safety.
#[allow(clippy::too_many_arguments)]
pub fn render(
    overlay: Overlay,
    ui: &OverlayUiState,
    // Shell-S4 (2026-08-22): ControlCenter's volume slider needs its own
    // real-backend UI state (`crate::audio::AudioUiState`, lives on
    // `ShellView` as `audio_ui` — see that field's own doc comment in
    // `main.rs`), threaded straight through to `controlcenter::render` the
    // same way `ui` already is. Launcher/Notifications ignore it.
    audio_ui: &crate::audio::AudioUiState,
    // APP-1 (2026-08-22): the Launcher's 「應用程式」section renders the REAL
    // installed-app list (`crate::apps::feed::InstalledAppsFeed`, threaded
    // straight through the same way `audio_ui` already is). Notifications/
    // ControlCenter ignore it. The feed's background scan is dispatched from
    // Home's dock, which renders underneath every overlay — this surface
    // deliberately does not schedule a second one.
    installed_apps: &crate::apps::feed::InstalledAppsFeed,
    // ICON-3 (2026-08-23): the pointer surface's own compositor-backed state
    // (`crate::overlay::pointer_settings::PointerUiState`, lives on
    // `ShellView` as `pointer_ui`), threaded through exactly the way
    // `audio_ui` already is. Every other overlay ignores it.
    pointer_ui: &pointer_settings::PointerUiState,
    // D3-b (2026-08-23): the Launcher's search box is a real IME-capable
    // text-input entity now (`oobe::LauncherQueryField`, lives on `ShellView`
    // as `launcher_query_field`), not a `String` on `ui` — threaded through
    // exactly the way `audio_ui`/`installed_apps` already are. Every other
    // overlay ignores it.
    launcher_query: &crate::oobe::LauncherQueryField,
    // D4b (2026-08-23): the 系統設定 app's own state and its eight text
    // fields (`crate::settings::SettingsUiState` / `crate::oobe::
    // SettingsFields`, both living on `ShellView`), threaded through exactly
    // the way `audio_ui`/`installed_apps`/`pointer_ui` already are. Every
    // other overlay ignores them; the settings surface also reads `audio_ui`
    // for its 聲音 page, which is why that parameter now has two consumers.
    settings_ui: &crate::settings::SettingsUiState,
    settings_fields: &crate::oobe::SettingsFields,
    // D6 (2026-08-23): third-party app notifications delivered over
    // `org.freedesktop.Notifications` (see `crate::notifyd`), rendered by the
    // Notifications panel BELOW the gateway approval cards. Threaded through
    // exactly the way `audio_ui`/`installed_apps` already are; every other
    // overlay ignores it.
    notify_center: &crate::notifyd::center::NotificationCenter,
    palette: ShellPalette,
    on_close: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &mut Context<ShellView>,
) -> Div {
    let dim = matches!(overlay, Overlay::Launcher);

    // Launcher.dc.html: `rgba(15,23,42,0.28)` light / `rgba(0,0,0,0.45)`
    // dark — the ONLY overlay with a dimmed backdrop (see this module's
    // header comment); Notifications/ControlCenter stay fully transparent
    // in both themes, unaffected by this branch.
    let dim_opacity_base = if palette.is_dark() { 0x000000 } else { 0x0f172a };
    let dim_opacity = if palette.is_dark() { 0.45 } else { 0.28 };

    let backdrop: Stateful<Div> = div()
        .id("shell-overlay-backdrop")
        .absolute()
        .inset_0()
        .bg(rgb(dim_opacity_base).opacity(if dim { dim_opacity } else { 0.0 }))
        .on_click(on_close);

    let panel: Stateful<Div> = match overlay {
        Overlay::Launcher => launcher::render(ui, installed_apps, launcher_query, palette, cx),
        Overlay::Notifications => notifications::render(ui, notify_center, palette, cx),
        Overlay::ControlCenter => controlcenter::render(ui, audio_ui, palette, cx),
        Overlay::PointerSettings => pointer_settings::render(pointer_ui, palette, cx),
        // Lives at the crate root (`crate::settings`), not under `overlay/`:
        // it is an application with its own seven-page directory, not a
        // panel, and putting a nine-file module inside `overlay/` would
        // misfile it. This arm is the only thing that makes it an overlay.
        Overlay::Settings => crate::settings::render(settings_ui, settings_fields, audio_ui, palette, cx),
    };

    // The wrapper MUST be absolutely positioned (`absolute().inset_0()`),
    // not a normal flow child: the shell root stacks its flow children, so
    // a `.relative().size_full()` wrapper was laid out BELOW the full-height
    // home surface — origin (0px, 900px), one full window-height offscreen.
    // Every overlay rendered there: invisible, unhittable, clicks passing
    // straight through to home (root-caused 2026-08-20 via bounds_probe
    // after three "screen never changes" user reports; the state machine,
    // key dispatch, and render loop were all verified working the whole
    // time). `absolute` takes it out of flow and pins it to the window, and
    // the panels' own `.absolute()` top/left/right offsets anchor to it.
    // `panel.occlude()` keeps clicks inside the panel from falling through
    // to the backdrop sibling below it (which would close the overlay) —
    // the panel's own child buttons still receive their events.
    div()
        .absolute()
        .inset_0()
        .child(crate::bounds_probe("overlay-wrapper"))
        .child(backdrop.child(crate::bounds_probe("backdrop")))
        .child(panel.occlude())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_state_matches_the_design_boards_snapshot() {
        let ui = OverlayUiState::default();
        // Shell-S4: the approval-card feed's own default state
        // (Idle/no-session/empty) is `notifications_feed`'s own concern —
        // covered by that module's tests, not re-asserted here.
        assert_eq!(ui.notifications.status, notifications_feed::FeedStatus::Idle);
        assert!(ui.automation_on());
        assert!(ui.proactive_on());
        assert!(!ui.pause_all_on());
    }

    /// WP-A4-4: closing the Launcher by ANY route (Escape, cmd-k, a
    /// backdrop click — all three go through `close_launcher_query`) must
    /// also dismiss a pending install confirmation, so an unanswered "are
    /// you sure" can never survive to be answered by accident later.
    #[test]
    fn closing_the_launcher_also_drops_a_pending_install_confirmation() {
        // APP-1: the gate is armed from the installable CATALOG now, not
        // from the (deleted) canned `fake_data::DOCK_APPS` array — see
        // `crate::apps::catalog`'s own header comment.
        let entry = crate::apps::catalog::INSTALL_CATALOG.first().expect("one installable catalog entry must exist");
        let mut ui = OverlayUiState::default();
        assert!(ui.install_gate.is_none(), "no install may be pending on a fresh state");

        ui.install_gate = Some(install_gate::InstallGate::open(entry, crate::apps::INSTALL_DESTINATION_LABEL.to_string()));
        assert!(ui.install_gate.is_some());

        ui.close_launcher_query();
        assert!(ui.install_gate.is_none(), "the question must not outlive the panel that asked it");
    }

    #[test]
    fn toggle_automation_flips_and_flips_back() {
        let mut ui = OverlayUiState::default();
        ui.toggle_automation();
        assert!(!ui.automation_on());
        ui.toggle_automation();
        assert!(ui.automation_on());
    }

    #[test]
    fn toggle_proactive_flips_and_flips_back() {
        let mut ui = OverlayUiState::default();
        ui.toggle_proactive();
        assert!(!ui.proactive_on());
        ui.toggle_proactive();
        assert!(ui.proactive_on());
    }

    #[test]
    fn toggle_pause_all_flips_and_flips_back() {
        let mut ui = OverlayUiState::default();
        ui.toggle_pause_all();
        assert!(ui.pause_all_on());
        ui.toggle_pause_all();
        assert!(!ui.pause_all_on());
    }
}
