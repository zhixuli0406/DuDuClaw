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
    utils::{IsAlive, Logical, Point, Rectangle, SERIAL_COUNTER},
    wayland::{
        compositor::with_states,
        shell::{
            wlr_layer::{
                KeyboardInteractivity, Layer, LayerSurface as WlrLayerSurface, LayerSurfaceData,
                WlrLayerShellHandler, WlrLayerShellState,
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
            // D9-bug: the destroyed surface may have been the one holding
            // keyboard focus (the Launcher overlay is `Exclusive`), or its
            // teardown may otherwise have left the seat with no focus at all
            // — which is how "lock the screen, press a key, nothing happens"
            // was reachable.
            //
            // D9-bug2: the surface being destroyed is passed explicitly.
            // smithay leaves it sitting in `KeyboardHandle::focus` after it
            // dies, so without naming it here the settle below would conclude
            // "someone already holds focus" and leave the keyboard dead. See
            // `settle_layer_keyboard_focus`.
            self.settle_layer_keyboard_focus("layer_destroyed", Some(surface.wl_surface()));
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

    // Set when this commit is the one that maps the surface AND the surface
    // asked for keyboard input — see the `adopt_keyboard_focus` block below.
    // Computed inside the `map` scope because that is where the `LayerSurface`
    // is reachable, but acted on outside it: `focus_layer_surface` walks
    // `self.space`, and `layer_map_for_output` holds a lock this would
    // otherwise still be inside.
    let mut wants_keyboard = false;

    let zone_changed = {
        let mut map = layer_map_for_output(&output);
        let before = map.non_exclusive_zone();
        map.arrange();
        let after = map.non_exclusive_zone();
        if !initial_configure_sent {
            if let Some(layer) = map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL) {
                layer.layer_surface().send_configure();
                wants_keyboard = layer.can_receive_keyboard_focus();
                tracing::info!(
                    surface_id = ?surface.id(),
                    namespace = %layer.namespace(),
                    wants_keyboard,
                    "layer_shell: sending initial configure to layer surface"
                );
            }
        }
        before != after
    };

    // P0 fix (2026-08-23, real-hardware round): a freshly mapped layer
    // surface that wants keyboard input takes it **if nothing else holds
    // it**.
    //
    // Without this, an `on_demand` layer surface only ever gets keyboard
    // focus by being CLICKED (`input.rs`'s pointer-button arm). That is the
    // right rule once a session is running — it is what stops a panel from
    // stealing focus from the window you are typing into — but it is wrong
    // for the very first surface on an otherwise empty screen: the shell
    // would come up showing OOBE with a perfectly good keyboard that does
    // nothing until the operator happens to click somewhere. Measured on the
    // appliance VM: Enter on the OOBE language step did nothing until an
    // unrelated click landed first.
    //
    // Deliberately conditional on `current_focus().is_none()` rather than
    // unconditional: a layer surface mapping later in the session (a
    // notification panel, a third-party panel) must NOT yank focus away from
    // whatever the operator is using.
    //
    // D9-bug2 (2026-08-23): the ONE case that is not a policy choice is
    // `exclusive`, which the protocol says the top-most Top/Overlay claimant
    // always gets — `settle_layer_keyboard_focus` applies that first and only
    // then falls through to the conditional adopt above. The shell's ⌘K
    // palette is exactly such a surface, and without this it opened with the
    // keyboard still pointed at the desktop surface (a DIFFERENT gpui window),
    // so its search box could never receive a keystroke. See that function's
    // own doc comment.
    if wants_keyboard {
        state.settle_layer_keyboard_focus("layer_mapped", None);
    }

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

    /// Every layer-surface front-to-back, paired with what it asked for on the
    /// keyboard. The order is [`LAYERS_FRONT_TO_BACK`]'s, and within one layer
    /// it is smithay's own `layers_on` order — the same order
    /// [`DuduclawComp::layer_under_pointer`] hit-tests in, so "top-most" means
    /// the same thing to the keyboard rule and to the pointer router.
    ///
    /// Returns surfaces alongside their interactivity so the pure decision
    /// functions in [`geometry`] can work on plain values and this function
    /// stays the only place that holds a `LayerMap` guard.
    fn layer_focus_candidates(&self) -> Vec<(WlSurface, Layer, KeyboardInteractivity)> {
        let Some(output) = self.layout_output() else {
            return Vec::new();
        };
        let map = layer_map_for_output(&output);
        let mut out = Vec::new();
        for layer in LAYERS_FRONT_TO_BACK {
            for surface in map.layers_on(layer) {
                out.push((
                    surface.wl_surface().clone(),
                    layer,
                    surface.cached_state().keyboard_interactivity,
                ));
            }
        }
        out
    }

    /// Re-applies the whole layer-shell keyboard-focus rule after any
    /// layer-surface lifecycle change (map, unmap, destroy).
    ///
    /// Fixing this here rather than in the shell is deliberate: keyboard focus
    /// is the compositor's to own, a client cannot ask for it back, and any
    /// layer client (not just our shell) would hit the same holes.
    ///
    /// D9-bug (2026-08-23, root-caused on the appliance VM) is the first half:
    /// the session could end up with **no** keyboard focus at all — the shell
    /// tears surfaces down when the lock screen takes over, and again when the
    /// Launcher closes — and the keyboard was then simply dead until the
    /// operator happened to click something. Measured symptom: lock the screen,
    /// press a key, nothing happens, and a machine with no mouse looks bricked.
    /// D9-bug2 is the second half, below.
    ///
    /// `vacating` is the surface whose teardown triggered this call, if any. It
    /// is treated as "no longer holding focus" even when its `wl_surface` is
    /// still technically alive at this point in the teardown — the layer-shell
    /// role object is gone, the surface is already out of the layer map, and
    /// smithay never clears `KeyboardHandle::focus` by itself (read, not
    /// assumed: `input/keyboard/mod.rs` only checks liveness for an active
    /// GRAB, never for the focus itself). Without that, `current_focus()` keeps
    /// answering with a dead surface forever, every later repair sees "someone
    /// already holds focus", and the keyboard stays dead until the operator
    /// clicks something — which is exactly the reported "close the Launcher
    /// with Escape and the whole shell stops responding to keys" symptom.
    ///
    /// Three outcomes, in the protocol's own priority order:
    ///
    /// 1. a `Top`/`Overlay` surface asked for `exclusive` → it gets focus (see
    ///    [`geometry::topmost_exclusive`] for the spec sentence this is);
    /// 2. otherwise, whoever legitimately holds focus keeps it — this branch is
    ///    load-bearing: a panel mapping mid-session must never yank the
    ///    keyboard away from the window the operator is typing into;
    /// 3. otherwise (focus empty, dead, or just vacated) → hand it to the
    ///    top-most focusable LAYER surface, then (only if none wants it) the
    ///    top-most window, else clear it explicitly so the NEXT call is not
    ///    blocked by a corpse. The layer-before-window order is a safety
    ///    property — see the comment on that branch for the locked-screen
    ///    failure that taught it.
    ///
    /// Only the human seat: the agent seat's focus is co-drive's to manage
    /// (`codrive/`), and the shell's layer surfaces are human-facing by
    /// definition.
    pub(crate) fn settle_layer_keyboard_focus(
        &mut self,
        reason: &'static str,
        vacating: Option<&WlSurface>,
    ) {
        let Some(keyboard) = self.seat.get_keyboard() else {
            tracing::warn!(reason, "layer_shell: focus settle skipped — seat has no keyboard");
            return;
        };
        let held = keyboard
            .current_focus()
            .filter(|s| s.alive() && Some(s) != vacating);

        let candidates = self.layer_focus_candidates();
        let plain: Vec<(Layer, KeyboardInteractivity)> =
            candidates.iter().map(|(_, layer, k)| (*layer, *k)).collect();

        if let Some(index) = geometry::topmost_exclusive(&plain) {
            let surface = candidates[index].0.clone();
            if held.as_ref() == Some(&surface) {
                return;
            }
            tracing::info!(
                reason,
                surface_id = ?surface.id(),
                "layer_shell: exclusive keyboard interactivity — moving focus to the top-most claimant"
            );
            self.focus_layer_surface(&surface);
            return;
        }

        if held.is_some() {
            tracing::debug!(
                reason,
                held_id = ?held.as_ref().map(|s| s.id()),
                "layer_shell: focus settle — nothing exclusive, live holder keeps it"
            );
            return;
        }

        // Layer surfaces FIRST, ordinary windows only as a last resort. That
        // ordering is a safety property, not a preference, and this round's VM
        // run is why it is spelled out: the first draft handed focus to the
        // top-most WINDOW here ("closing the Launcher over a browser should
        // give the keyboard back to the browser", which is what a plain
        // desktop wants) — and locking the screen destroys the Launcher
        // overlay, so this settle also runs *while the shell is locked*. The
        // draft therefore handed the keyboard straight to Chromium on a
        // machine the operator had just locked, and it accepted typed text
        // (measured: "abc" landed in its URL bar).
        //
        // The compositor cannot ask the shell whether it is locked, and
        // inventing a protocol for that would be answering the wrong question:
        // the shell's own surfaces ARE layer surfaces, so preferring them is
        // both the safe answer and the pre-D9-bug2 behaviour (the adopt path
        // this replaced never considered windows at all). The cost is real and
        // accepted deliberately: after closing the Launcher over an
        // application window the keyboard goes to the desktop rather than back
        // to that application, and the operator clicks the window to resume
        // typing. Before this round, that same case left the keyboard dead
        // outright.
        if let Some(index) = geometry::topmost_focusable(&plain) {
            let surface = candidates[index].0.clone();
            tracing::info!(
                reason,
                surface_id = ?surface.id(),
                "layer_shell: keyboard focus vacated — adopting the top-most focusable layer surface"
            );
            self.focus_layer_surface(&surface);
            return;
        }

        // No layer surface wants the keyboard at all — a session with no shell
        // of ours running. `elements()` is bottom-to-top, so `next_back()` is
        // the top-most window: the same "Z 序次高者" rule
        // `DuduclawComp::reassign_focus_on_window_removed` applies when a
        // toplevel closes.
        //
        // Bound to a local first: holding the `elements()` iterator across the
        // `focus_window` call below would keep `self` immutably borrowed.
        let topmost_window = self.space.elements().next_back().cloned();
        if let Some(window) = topmost_window {
            tracing::info!(
                reason,
                "layer_shell: keyboard focus vacated with no focusable layer surface — handing it to the top-most window"
            );
            let seat = self.seat.clone();
            let serial = SERIAL_COUNTER.next_serial();
            self.focus_window(&seat, Some(&window), serial);
            return;
        }

        // Nothing at all can take it. Clearing is not cosmetic: leaving a dead
        // surface in `KeyboardHandle::focus` makes every future settle take the
        // "someone already holds it" branch above and the keyboard never comes
        // back.
        if keyboard.current_focus().is_some() {
            tracing::info!(
                reason,
                "layer_shell: keyboard focus vacated with nothing to hand it to — clearing"
            );
            let serial = SERIAL_COUNTER.next_serial();
            keyboard.set_focus(self, None, serial);
        }
    }

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
