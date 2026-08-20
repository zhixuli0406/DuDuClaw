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
mod launcher;
mod notifications;

/// Three-way outcome of an approval-card decision — Notifications' own
/// interactive core (task brief: "審批卡第一公民"). Pure enum, no gpui
/// types, same testable-without-a-window discipline `surface.rs`'s own
/// header comment establishes for `SurfaceState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    Pending,
    Approved,
    Rejected,
}

/// Runtime-mutable state backing the two overlays that have actual
/// interactive controls this round (round 1's `SurfaceState` only tracked
/// WHICH overlay is open, never what's inside one). Lives on `ShellView`
/// itself (see `main.rs`), not re-created per render call, so a
/// decision/toggle survives closing and reopening the overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayUiState {
    /// Index-aligned with `fake_data::APPROVAL_CARDS` (2 cards this round).
    /// A plain `Vec`, not a fixed-size array: `Default` seeds its length
    /// from `fake_data::APPROVAL_CARDS.len()` at runtime, so a hardcoded
    /// array length here can't silently drift out of sync with that list if
    /// a future round adds a third card.
    approval_decisions: Vec<ApprovalDecision>,
    automation_on: bool,
    proactive_on: bool,
    pause_all_on: bool,
}

impl Default for OverlayUiState {
    fn default() -> Self {
        Self {
            approval_decisions: vec![ApprovalDecision::Pending; crate::fake_data::APPROVAL_CARDS.len()],
            // ControlCenter.dc.html: 自動化/主動行為 both render as an ON
            // (blue) toggle, 全部暫停 renders OFF (gray) — the design
            // board's actual snapshot state, kept verbatim as the boot
            // default rather than inventing a different one.
            automation_on: true,
            proactive_on: true,
            pause_all_on: false,
        }
    }
}

impl OverlayUiState {
    pub fn approval_decision(&self, index: usize) -> ApprovalDecision {
        self.approval_decisions.get(index).copied().unwrap_or(ApprovalDecision::Pending)
    }

    pub fn approve(&mut self, index: usize) {
        if let Some(d) = self.approval_decisions.get_mut(index) {
            *d = ApprovalDecision::Approved;
        }
    }

    pub fn reject(&mut self, index: usize) {
        if let Some(d) = self.approval_decisions.get_mut(index) {
            *d = ApprovalDecision::Rejected;
        }
    }

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
        Overlay::Launcher => launcher::render(palette),
        Overlay::Notifications => notifications::render(ui, palette, cx),
        Overlay::ControlCenter => controlcenter::render(ui, palette, cx),
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
        assert_eq!(ui.approval_decision(0), ApprovalDecision::Pending);
        assert_eq!(ui.approval_decision(1), ApprovalDecision::Pending);
        assert!(ui.automation_on());
        assert!(ui.proactive_on());
        assert!(!ui.pause_all_on());
    }

    #[test]
    fn default_approval_decisions_length_matches_fake_data() {
        assert_eq!(OverlayUiState::default().approval_decisions.len(), crate::fake_data::APPROVAL_CARDS.len());
    }

    #[test]
    fn approve_only_changes_that_cards_decision() {
        let mut ui = OverlayUiState::default();
        ui.approve(0);
        assert_eq!(ui.approval_decision(0), ApprovalDecision::Approved);
        assert_eq!(ui.approval_decision(1), ApprovalDecision::Pending);
    }

    #[test]
    fn reject_only_changes_that_cards_decision() {
        let mut ui = OverlayUiState::default();
        ui.reject(1);
        assert_eq!(ui.approval_decision(1), ApprovalDecision::Rejected);
        assert_eq!(ui.approval_decision(0), ApprovalDecision::Pending);
    }

    #[test]
    fn approve_then_reject_the_same_card_overwrites_not_stacks() {
        let mut ui = OverlayUiState::default();
        ui.approve(0);
        ui.reject(0);
        assert_eq!(ui.approval_decision(0), ApprovalDecision::Rejected);
    }

    #[test]
    fn out_of_bounds_approve_reject_is_a_noop_not_a_panic() {
        let mut ui = OverlayUiState::default();
        ui.approve(99);
        ui.reject(99);
        assert_eq!(ui.approval_decision(99), ApprovalDecision::Pending);
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
