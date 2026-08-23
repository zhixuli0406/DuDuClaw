//! WM-3: **minimize and restore**.
//!
//! ## What "minimized" means here
//!
//! Unmapped from [`Space`](smithay::desktop::Space), still alive, still
//! reachable. The `Window` handle moves into [`DuduclawComp::minimized`]; the
//! client is not told anything at all, because xdg-shell has no "you are
//! minimized" state to tell it (`xdg_toplevel` has `maximized`, `fullscreen`,
//! `resizing`, `activated` — and nothing else). What the client observes is
//! simply that its frame callbacks stop, which is exactly right: an off-screen
//! window should not be animating.
//!
//! ## The invariant that makes this safe
//!
//! **A minimized window is always recoverable.** There are exactly three ways
//! back — Alt-Tab (`crate::switcher`), `shell_control`'s `focus_window` op (the
//! dock), and the compositor restoring it when the client asks for attention —
//! and exactly two ways out of the list: [`DuduclawComp::unminimize_window`]
//! and destruction. Without that invariant, minimize is a way to lose a window
//! forever on a desktop that has no task bar of its own, which would be a worse
//! bug than not having minimize at all.
//!
//! Two windows are refused outright: the **session shell** (minimizing the
//! desktop leaves a black screen — the same refusal `close_window_politely`
//! already makes) and **shadow-workspace windows** (their geometry is owned by
//! `codrive/shadow.rs`, and unmapping one would strand the agent's own work).

use smithay::{
    desktop::Window,
    reexports::wayland_server::{protocol::wl_surface::WlSurface, Resource},
    utils::Rectangle,
};

use crate::state::DuduclawComp;

impl DuduclawComp {
    /// Is this window currently minimized?
    pub fn is_minimized(&self, window: &Window) -> bool {
        self.minimized.iter().any(|w| w == window)
    }

    /// Every window the compositor knows about, mapped or minimized, topmost
    /// first.
    ///
    /// The list `shell_control`'s dock and the Alt-Tab switcher both resolve
    /// against — a minimized window that vanished from those two would be
    /// unreachable, which is precisely what this module's invariant forbids.
    pub fn all_windows(&self) -> Vec<Window> {
        let mut out: Vec<Window> = self.space.elements().rev().cloned().collect();
        out.extend(self.minimized.iter().cloned());
        out
    }

    /// Minimizes `window`. Refused (with a log line, never silently) for the
    /// session shell, for a shadow-workspace window, and for a window that is
    /// already minimized.
    pub fn minimize_window(&mut self, window: &Window, reason: &'static str) {
        let Some(toplevel) = window.toplevel() else {
            return;
        };
        let surface = toplevel.wl_surface().clone();
        if self.is_session_shell_surface(&surface) {
            tracing::warn!(
                surface_id = ?surface.id(),
                reason,
                "minimize refused — that window is the session shell (minimizing it would leave a black screen)"
            );
            return;
        }
        if self.window_is_in_shadow_public(window) {
            tracing::warn!(
                surface_id = ?surface.id(),
                reason,
                "minimize refused — shadow-workspace windows are owned by codrive/shadow.rs"
            );
            return;
        }
        if self.is_minimized(window) {
            return;
        }

        // Snapshot where it is BEFORE unmapping: once it leaves the space,
        // `element_geometry` returns `None` and `decor_sync_frame` becomes a
        // no-op, so the restore rectangle would freeze at wherever the window
        // was last synced rather than where the user left it.
        self.decor_sync_frame(window);

        self.space.unmap_elem(window);
        self.minimized.push(window.clone());
        tracing::info!(
            surface_id = ?surface.id(),
            reason,
            minimized_count = self.minimized.len(),
            "minimize: window unmapped and parked"
        );

        // Reuses the close-time handoff: if this window held either seat's
        // keyboard focus, focus moves to the new topmost survivor; if it did
        // not, nothing is stolen from whatever the human was actually using.
        self.reassign_focus_on_window_removed(&surface);
        self.queue_redraw();
    }

    /// Restores a minimized window to the work area and maps it back on top.
    ///
    /// No-op for a window that is not minimized, so callers (the switcher, the
    /// dock's `focus_window`) can call it unconditionally.
    pub fn unminimize_window(&mut self, window: &Window) {
        let before = self.minimized.len();
        self.minimized.retain(|w| w != window);
        if self.minimized.len() == before {
            return;
        }

        let Some(toplevel) = window.toplevel() else {
            return;
        };
        let id = toplevel.wl_surface().id();
        let insets = self.window_insets(window);
        let content = match self.layout_work_area() {
            Some(work) => {
                let frame = if self.decor.maximized.contains(&id) {
                    // It was maximized when it was minimized; the work area may
                    // have changed in the meantime, so re-derive rather than
                    // restoring a stale rectangle.
                    work
                } else {
                    match self.decor.frames.get(&id).copied() {
                        Some(remembered) => crate::decor::refit_frame(remembered, work, insets),
                        None => {
                            let index = self.decor.cascade_next;
                            self.decor.cascade_next = self.decor.cascade_next.wrapping_add(1);
                            crate::decor::cascade_frame_rect(work, insets, index)
                        }
                    }
                };
                if !self.decor.maximized.contains(&id) {
                    self.decor.frames.insert(id.clone(), frame);
                }
                crate::decor::content_rect(frame, insets)
            }
            // No real output yet — map it back where it was and let the next
            // `reapply_window_policy_all` sort the geometry out. Better a
            // window in the wrong place than one that stays lost.
            None => Rectangle::new(
                self.decor
                    .frames
                    .get(&id)
                    .map(|f| f.loc)
                    .unwrap_or_default(),
                window.geometry().size,
            ),
        };

        toplevel.with_pending_state(|state| {
            state.size = Some(content.size);
        });
        self.space.map_element(window.clone(), content.loc, true);
        toplevel.send_pending_configure();

        tracing::info!(
            surface_id = ?id,
            content = ?(content.loc.x, content.loc.y, content.size.w, content.size.h),
            minimized_count = self.minimized.len(),
            "minimize: window restored"
        );
        self.queue_redraw();
    }

    /// Drops a destroyed toplevel from the minimized list. Called from
    /// `XdgShellHandler::toplevel_destroyed`, which is the only place a window
    /// may leave the list other than a real restore.
    pub(crate) fn forget_minimized(&mut self, destroyed: &WlSurface) {
        let before = self.minimized.len();
        self.minimized.retain(|w| {
            w.toplevel()
                .map(|t| t.wl_surface() != destroyed)
                .unwrap_or(false)
        });
        if self.minimized.len() != before {
            tracing::info!(
                surface_id = ?destroyed.id(),
                "minimize: a minimized window's client destroyed it"
            );
        }
    }
}
