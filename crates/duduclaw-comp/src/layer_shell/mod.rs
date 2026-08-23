//! WM-3 (2026-08-23): **`zwlr_layer_shell_v1` server side**.
//!
//! ## Why comp needs this
//!
//! Up to WM-2 the session shell was one full-output `xdg_toplevel` that painted
//! its own menu bar and dock *inside* itself, and comp kept ordinary windows
//! clear of them with two hard-coded constants (`window_policy::ReservedBands`,
//! 30 px top / 90 px bottom). That works only as long as the shell is the
//! bottom-most window and nothing ever covers it — which is exactly what WM-1's
//! bug report was about, and what the constants are a patch for.
//!
//! Layer shell is the real mechanism every Wayland desktop uses instead:
//! a client declares which of four **layers** its surface belongs to
//! (background / bottom / top / overlay) and how much of the output it claims
//! exclusively; the compositor stacks accordingly and hands the leftover
//! rectangle to ordinary windows. It is also the prerequisite for A1's global
//! ⌘K palette, which has to draw above every window without being one.
//!
//! ## Scope of THIS work package — deliberately protocol-only
//!
//! `duduclaw-shell` is **not** changed in this round. Its dock and menu bar
//! stay inside its own toplevel, nothing claims an exclusive zone, and
//! [`geometry::effective_work_area`] therefore falls back to the reserved bands
//! — the layout every already-verified WM-2 behaviour was measured against
//! stays byte-identical. What lands here is the protocol, the z-order, the
//! pointer routing and the exclusive-zone plumbing, so that migrating the
//! shell later is a shell-side change with no compositor work left to do.
//!
//! What the shell will have to do when it migrates is listed in `BUILD.md`'s
//! WM-3 section under "what the shell has to do (later)".
//!
//! ## Where layer surfaces live
//!
//! Not in `Space`. smithay keeps one [`LayerMap`](smithay::desktop::LayerMap)
//! per [`Output`], stored in that output's own `UserDataMap` and reached with
//! `layer_map_for_output`. That map owns arrangement (anchors, margins,
//! exclusive zones) and hands back **output-local** geometry, which is why
//! every coordinate crossing in this module adds or subtracts
//! `output_geometry().loc` explicitly.
//!
//! > `layer_map_for_output` returns a `MutexGuard`. Holding two guards for the
//! > same output at once deadlocks (smithay documents this on the function).
//! > Every guard in this module is taken in the narrowest possible scope, and
//! > no function here calls another one that takes its own.

pub mod geometry;

use smithay::{
    delegate_layer_shell,
    desktop::{layer_map_for_output, LayerSurface, PopupKind, WindowSurfaceType},
    output::Output,
    reexports::wayland_server::{
        protocol::{wl_output::WlOutput, wl_surface::WlSurface},
        Resource,
    },
    utils::{Logical, Point, Rectangle, SERIAL_COUNTER},
    wayland::{
        compositor::with_states,
        shell::{
            wlr_layer::{
                Layer, LayerSurface as WlrLayerSurface, LayerSurfaceData, WlrLayerShellHandler,
                WlrLayerShellState,
            },
            xdg::PopupSurface,
        },
    },
};

use crate::state::DuduclawComp;

use geometry::{is_above_windows, LAYERS_FRONT_TO_BACK};

impl WlrLayerShellHandler for DuduclawComp {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    /// A client created a layer surface.
    ///
    /// `wl_output` is the client's request for a specific output; a client is
    /// allowed to leave it unset and let the compositor decide, which is what
    /// `duduclaw-shell` will do (it has one screen). Falling back to
    /// [`DuduclawComp::layout_output`] rather than `space.outputs().next()`
    /// matters: the first output on this space is the CD-2 shadow workspace's
    /// headless one, 100 000 px down (`state::primary_output`'s own bug note).
    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        wl_output: Option<WlOutput>,
        layer: Layer,
        namespace: String,
    ) {
        let output = wl_output
            .as_ref()
            .and_then(Output::from_resource)
            .or_else(|| self.layout_output());
        let Some(output) = output else {
            // No real output yet. Nothing can be arranged against a screen that
            // does not exist; the client is told so rather than being left with
            // a surface that will never be configured.
            tracing::warn!(
                namespace = %namespace,
                "layer_shell: new layer surface with no real output mapped yet — closing it"
            );
            surface.send_close();
            return;
        };

        // The CD-2 shadow workspace is advertised as a `wl_output` global
        // (`codrive::create_shadow_output`), so an output-aware layer client
        // targets it like any other monitor — the first WM-3 live run had both
        // `swaybg` and `waybar` creating a second surface on
        // `duduclaw-shadow-0`. That surface can never be composited (the shadow
        // output is only ever rendered offscreen for the PiP preview) and would
        // therefore never receive a frame callback, leaving that half of the
        // client stalled forever. Refusing it with the protocol's own `closed`
        // event is the standard output-hotplug path every layer client already
        // handles.
        //
        // Deliberately NOT "stop advertising the shadow output": clients rely
        // on `wl_surface.enter` to learn their scale, and revoking that global
        // would change CD-2's own verified behaviour — a separate decision with
        // its own verification, not a side effect of this work package.
        if output == self.shadow_output {
            tracing::info!(
                namespace = %namespace,
                layer = ?layer,
                "layer_shell: refusing a layer surface on the CD-2 shadow workspace — \
                 it can never be composited, so it would stall waiting for a frame callback"
            );
            surface.send_close();
            return;
        }

        tracing::info!(
            namespace = %namespace,
            layer = ?layer,
            output = %output.name(),
            surface_id = ?surface.wl_surface().id(),
            "layer_shell: new layer surface"
        );

        {
            let mut map = layer_map_for_output(&output);
            if let Err(e) = map.map_layer(&LayerSurface::new(surface, namespace)) {
                tracing::warn!(error = %e, "layer_shell: refusing to map a layer surface twice");
                return;
            }
        }

        // Mapping can change the exclusive zone, which changes the work area
        // every floating window is placed and clamped against.
        self.reapply_window_policy_all();
        self.queue_redraw();
    }

    /// A layer surface asked for a popup (a panel's own menu). Tracked in the
    /// same [`PopupManager`](smithay::desktop::PopupManager) toplevel popups
    /// use, so it renders through `LayerSurface`'s own
    /// `AsRenderElements` (which walks `popups_for_surface`) with no second
    /// mechanism.
    fn new_popup(&mut self, _parent: WlrLayerSurface, popup: PopupSurface) {
        self.queue_redraw();
        self.unconstrain_popup(&popup);
        let _ = self.popups.track_popup(PopupKind::Xdg(popup));
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        let mut found = false;
        let outputs: Vec<Output> = self.space.outputs().cloned().collect();
        for output in outputs {
            // Bound to a `let` inside the block (not returned as the block's
            // tail expression) so the `MutexGuard` is dropped before the block
            // ends — otherwise the iterator borrowing it outlives it.
            let layer = {
                let map = layer_map_for_output(&output);
                let found = map
                    .layers()
                    .find(|l| l.layer_surface() == &surface)
                    .cloned();
                found
            };
            if let Some(layer) = layer {
                layer_map_for_output(&output).unmap_layer(&layer);
                found = true;
                break;
            }
        }
        tracing::info!(
            surface_id = ?surface.wl_surface().id(),
            found,
            "layer_shell: layer surface destroyed"
        );

        // `found == false` is the normal path for a surface this compositor
        // refused above (shadow workspace, or no output at all): it was never in
        // any map, so it was never claiming anything and re-running the layout
        // would be pure churn — and, on the appliance, a burst of it every time
        // an output-aware layer client starts.
        if found {
            self.reapply_window_policy_all();
            self.queue_redraw();
        }
    }
}

delegate_layer_shell!(DuduclawComp);

/// Called from `CompositorHandler::commit` for every surface, exactly like
/// `xdg_shell::handle_commit`.
///
/// Two jobs, in this order (the order is the protocol's, not a preference):
/// `arrange()` first so the client's own requested size is honoured, then the
/// initial configure — *"the initial configure has to be sent in response to
/// the initial commit"*, and `LayerMap::arrange` deliberately refuses to send
/// one itself (smithay 0.7.0 `desktop/wayland/layer.rs`, read not remembered).
///
/// Returns `true` if `surface` was a layer surface, so the caller can skip the
/// toplevel path — a surface is never both.
pub fn handle_commit(state: &mut DuduclawComp, surface: &WlSurface) -> bool {
    let outputs: Vec<Output> = state.space.outputs().cloned().collect();
    let Some(output) = outputs.into_iter().find(|o| {
        layer_map_for_output(o)
            .layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
            .is_some()
    }) else {
        return false;
    };

    let initial_configure_sent = with_states(surface, |states| {
        states
            .data_map
            .get::<LayerSurfaceData>()
            .map(|d| d.lock().unwrap().initial_configure_sent)
            .unwrap_or(false)
    });

    let zone_changed = {
        let mut map = layer_map_for_output(&output);
        let before = map.non_exclusive_zone();
        map.arrange();
        let after = map.non_exclusive_zone();
        if !initial_configure_sent {
            if let Some(layer) = map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL) {
                layer.layer_surface().send_configure();
                tracing::info!(
                    surface_id = ?surface.id(),
                    namespace = %layer.namespace(),
                    "layer_shell: sending initial configure to layer surface"
                );
            }
        }
        before != after
    };

    if zone_changed {
        tracing::info!(
            surface_id = ?surface.id(),
            "layer_shell: exclusive zone changed — re-running the window layout policy"
        );
        state.reapply_window_policy_all();
    }
    state.queue_redraw();
    true
}

impl DuduclawComp {
    /// The layer map's leftover rectangle for the layout output, in
    /// **output-local** coordinates, or `None` when there is no real output.
    ///
    /// Handed straight to [`geometry::effective_work_area`], which owns the
    /// "did anyone actually claim anything" decision — this function
    /// deliberately does not pre-interpret it.
    pub(crate) fn layer_non_exclusive_zone(&self) -> Option<Rectangle<i32, Logical>> {
        let output = self.layout_output()?;
        // Same "drop the guard before the block ends" shape as
        // `layer_destroyed` above — a tail expression holding a `MutexGuard`
        // outlives the `Output` it borrows.
        let zone = layer_map_for_output(&output).non_exclusive_zone();
        Some(zone)
    }

    /// The topmost layer surface under `pos` (global coordinates) among the
    /// layers on the requested side of the window stack, together with the
    /// surface-local hit and its global origin.
    ///
    /// Coordinate chain copied from smithay's own reference compositor
    /// (`anvil/src/input_handler.rs::surface_under`, v0.7.0, MIT — same repo
    /// and licence as the `smallvil` this crate is adapted from): a layer's
    /// geometry is output-local, so the point is first moved into output-local
    /// space, then into layer-local space, and the resulting surface origin is
    /// walked all the way back out.
    pub(crate) fn layer_surface_under(
        &self,
        pos: Point<f64, Logical>,
        above_windows: bool,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        let output = self.layout_output()?;
        let output_geo = self.space.output_geometry(&output)?;
        let local = pos - output_geo.loc.to_f64();

        let map = layer_map_for_output(&output);
        for layer in LAYERS_FRONT_TO_BACK
            .into_iter()
            .filter(|l| is_above_windows(*l) == above_windows)
        {
            let Some(candidate) = map.layer_under(layer, local) else {
                continue;
            };
            let Some(geo) = map.layer_geometry(candidate) else {
                continue;
            };
            if let Some((surface, surface_loc)) =
                candidate.surface_under(local - geo.loc.to_f64(), WindowSurfaceType::ALL)
            {
                return Some((surface, (surface_loc + geo.loc + output_geo.loc).to_f64()));
            }
        }
        None
    }

    /// The layer surface itself (not its sub-surface) under `pos`, used by the
    /// pointer-button arm to decide keyboard focus.
    pub(crate) fn layer_under_pointer(
        &self,
        pos: Point<f64, Logical>,
        above_windows: bool,
    ) -> Option<LayerSurface> {
        let output = self.layout_output()?;
        let output_geo = self.space.output_geometry(&output)?;
        let local = pos - output_geo.loc.to_f64();
        let map = layer_map_for_output(&output);
        LAYERS_FRONT_TO_BACK
            .into_iter()
            .filter(|l| is_above_windows(*l) == above_windows)
            .find_map(|layer| map.layer_under(layer, local).cloned())
    }

    /// Gives keyboard focus to a layer surface and deactivates every window.
    ///
    /// Separate from [`DuduclawComp::focus_window`] rather than an extra arm on
    /// it, because the two differ in what they raise: a layer surface's
    /// stacking comes from its layer, never from click order, so there is
    /// nothing to raise here — only focus to move and windows to deactivate.
    pub(crate) fn focus_layer_surface(&mut self, surface: &WlSurface) {
        self.queue_redraw();
        for element in self.space.elements() {
            element.set_activated(false);
        }
        let seat = self.seat.clone();
        if let Some(keyboard) = seat.get_keyboard() {
            keyboard.set_focus(self, Some(surface.clone()), SERIAL_COUNTER.next_serial());
        }
        self.space.elements().for_each(|w| {
            w.toplevel().unwrap().send_pending_configure();
        });
    }

    /// Re-arranges every output's layer map. Called when an output's mode
    /// changes — the arrangement is computed against the mode, so a resize
    /// leaves every anchored surface at the old geometry otherwise.
    pub fn rearrange_layers(&mut self) {
        let outputs: Vec<Output> = self.space.outputs().cloned().collect();
        for output in outputs {
            layer_map_for_output(&output).arrange();
        }
    }

    /// Frame callbacks + dead-surface reaping for one output's layer surfaces.
    ///
    /// The window equivalent (`space.elements().for_each(send_frame)` +
    /// `space.refresh()`) is in both backends' render paths; layer surfaces are
    /// not in `Space`, so without this a panel that double-buffers stalls after
    /// its first commit and destroyed layers are never dropped from the map.
    pub fn send_layer_frames_and_cleanup(&mut self, output: &Output, time: std::time::Duration) {
        let mut map = layer_map_for_output(output);
        for layer in map.layers() {
            layer.send_frame(output, time, Some(std::time::Duration::ZERO), |_, _| {
                Some(output.clone())
            });
        }
        map.cleanup();
    }
}
