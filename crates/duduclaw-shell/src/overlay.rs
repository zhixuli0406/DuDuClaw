// Overlay shared shell — Shell-S0 round 2.
//
// Round 1 gave `Launcher` / `Notifications` / `ControlCenter` a single
// generic "半透明遮罩＋置中面板顯示 surface 名稱" stub. This round replaces
// that with each overlay's REAL content, lifted from its own `.dc.html`
// artboard — `overlay::launcher` / `overlay::notifications` /
// `overlay::controlcenter`, one file each (`src/overlay/*.rs` — same
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
// overlay (consistent behavior across all three), just fully transparent
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

mod controlcenter;
/// WP-A4-4 (2026-08-22): the Launcher's flatpak install confirmation gate —
/// a pure state machine, see its own header comment.
pub(crate) mod install_gate;
mod launcher;
// `pub(crate)`, not private: `home.rs` (a sibling module of this one, not a
// descendant) calls `notifications::open_and_refresh` directly from its two
// "open Notifications" click sites — see that fn's own doc comment.
pub(crate) mod notifications;
/// WP-A4-4 (2026-08-22): retry spacing + log denoise for the feed's gateway
/// poll. Its own module rather than more methods on `notifications_feed`
/// because it is a self-contained, clock-injected policy with a test suite
/// of its own — same "many small files, low coupling" convention this
/// crate's `Cargo.toml`/`gateway_client` comments already state.
mod notifications_backoff;
pub mod notifications_feed;

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
    automation_on: bool,
    proactive_on: bool,
    pause_all_on: bool,
    /// WP-A3 (2026-08-22): the Launcher's live search box text — see
    /// `overlay/launcher.rs`'s header comment for why round 2's static
    /// predisplay became real typing this round. Typed via `main.rs`'s root
    /// `on_key_down` listener (gated on the Launcher actually being the
    /// open overlay), cleared by `close_launcher_query` below whenever the
    /// overlay closes so a fresh open never shows a stale search.
    pub(crate) launcher_query: String,
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
            // ControlCenter.dc.html: 自動化/主動行為 both render as an ON
            // (blue) toggle, 全部暫停 renders OFF (gray) — the design
            // board's actual snapshot state, kept verbatim as the boot
            // default rather than inventing a different one.
            automation_on: true,
            proactive_on: true,
            pause_all_on: false,
            launcher_query: String::new(),
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

    /// Clears the Launcher search box — called from every path that closes
    /// an overlay (`main.rs`'s `on_close_overlay`/`on_toggle_launcher`, and
    /// the backdrop-click listener in `render` below) so reopening the
    /// Launcher always starts from its empty/pre-typing state rather than
    /// showing whatever was typed last time. A no-op when it's already
    /// empty (closing Notifications/ControlCenter calls this too, same as
    /// every other overlay-close path — cheaper than threading an
    /// `Overlay`-specific branch through three call sites for a plain
    /// `String::clear()`).
    pub(crate) fn close_launcher_query(&mut self) {
        self.launcher_query.clear();
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
        Overlay::Launcher => launcher::render(ui, installed_apps, palette, cx),
        Overlay::Notifications => notifications::render(ui, palette, cx),
        Overlay::ControlCenter => controlcenter::render(ui, audio_ui, palette, cx),
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
        assert!(ui.launcher_query.is_empty());
    }

    #[test]
    fn close_launcher_query_clears_a_typed_search_and_is_a_noop_when_already_empty() {
        let mut ui = OverlayUiState::default();
        ui.launcher_query.push_str("chrome");
        ui.close_launcher_query();
        assert!(ui.launcher_query.is_empty());
        ui.close_launcher_query();
        assert!(ui.launcher_query.is_empty());
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
