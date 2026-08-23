//! D3-a: the three Wayland globals a Chinese input method needs, plus the
//! candidate window.
//!
//! ## The three globals, and why all three
//!
//! | global | who binds it | what it carries |
//! |---|---|---|
//! | `zwp_text_input_manager_v3` | every application (`duduclaw-shell`, Chromium, `foot`, GTK/Qt apps) | "I have a text field here, at this caret" ⇄ preedit / commit |
//! | `zwp_input_method_manager_v2` | fcitx5 | the composition itself, and the candidate popup surface |
//! | `zwp_virtual_keyboard_manager_v1` | fcitx5 | keys the input method decided not to consume, handed back |
//!
//! fcitx5 needs the **second and third together** or it does nothing at all,
//! with no error: `WaylandIMServerV2::init()` only sets `init_ = true` when it
//! has found both managers, so advertising the input-method global alone
//! yields a compositor where Chinese input silently never starts. That is the
//! easiest mistake to make here, which is why all three are created in one
//! place, in one call.
//!
//! Focus needs no code: smithay ties text-input focus to keyboard focus
//! automatically (`wayland/text_input/mod.rs`'s module doc), so
//! `DuduclawComp::focus_window` already drives it.
//!
//! ## The candidate window
//!
//! fcitx5's classicui draws its candidate list into a `zwp_input_popup_
//! surface_v2`. If a compositor advertises no popup path, fcitx5 logs "No
//! Panel surface available" and carries on **composing invisibly** — the
//! hardest failure in this whole chain to diagnose. So the popup is a
//! must-have, not a nicety: [`InputMethodHandler::new_popup`] records the
//! surface and [`DuduclawComp::ime_popup_elements`] draws it, positioned by
//! the pure geometry in [`popup`].
//!
//! It rides the `WaylandSurfaceRenderElement` path CUR-1 already added to
//! `crate::render::CodriveElement` for client cursor surfaces — an IME popup
//! is an ordinary surface tree, so no new element kind and no change to
//! `render_output`'s `custom_elements` interface were needed.
//!
//! ## Seat isolation
//!
//! See [`seat_filter`]'s module doc. It started here as D3-c (hide the agent
//! seat from input-method clients) and, since E1a-1, owns the compositor's
//! whole per-client `wl_seat` visibility policy — the agent seat is now
//! hidden from every client except the allow-listed session shell. That
//! module also documents what breaks when the filter cannot arm.

pub mod popup;
pub mod seat_filter;

use smithay::{
    backend::renderer::{
        element::{surface::render_elements_from_surface_tree, Kind},
        gles::GlesRenderer,
    },
    delegate_input_method_manager, delegate_text_input_manager, delegate_virtual_keyboard_manager,
    desktop::utils::bbox_from_surface_tree,
    reexports::wayland_server::{protocol::wl_surface::WlSurface, DisplayHandle},
    utils::{Logical, Physical, Point, Rectangle, Scale},
    wayland::{
        input_method::{InputMethodHandler, InputMethodManagerState, PopupSurface},
        text_input::TextInputManagerState,
        virtual_keyboard::VirtualKeyboardManagerState,
    },
};

use crate::{render::CodriveElement, state::DuduclawComp};

/// Everything D3-a keeps on [`DuduclawComp`].
pub struct ImeState {
    /// Held only to keep the globals alive for the process lifetime — every
    /// request they receive is dispatched by smithay straight into the
    /// [`InputMethodHandler`] impl below or into the per-seat handles in the
    /// seat's own `UserDataMap`. Same shape (and same reason) as
    /// `cursor_shape_manager_state` / `xdg_decoration_state` in `state.rs`.
    _text_input: TextInputManagerState,
    _input_method: InputMethodManagerState,
    _virtual_keyboard: VirtualKeyboardManagerState,

    /// The `(human, agent)` input-method keyboard-grab state as of the last
    /// observation, so [`DuduclawComp::ime_note_grab_state`] can log only the
    /// transitions rather than once per tick. `None` until the first look.
    last_grabs: Option<(bool, bool)>,

    /// The live candidate window, if the input method has one open.
    ///
    /// smithay already tracks this per seat inside `InputMethodHandle`, but
    /// exposes no way to read it back, so the render path keeps its own
    /// handle. Cleared on `dismiss_popup`, and defensively whenever the
    /// surface turns out to be dead at draw time — a client can die between
    /// the two.
    popup: Option<PopupSurface>,
}

impl std::fmt::Debug for ImeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImeState")
            .field("popup", &self.popup.is_some())
            .finish()
    }
}

impl ImeState {
    /// Creates the three globals.
    ///
    /// Must run **before** the Wayland socket starts accepting connections
    /// (`state::DuduclawComp::init_wayland_listener`) — a global that appears
    /// after a client's registry sweep is a global that client never sees.
    pub fn new(dh: &DisplayHandle) -> Self {
        let state = Self {
            _text_input: TextInputManagerState::new::<DuduclawComp>(dh),
            _input_method: InputMethodManagerState::new::<DuduclawComp, _>(
                dh,
                seat_filter::client_may_use_input_method,
            ),
            _virtual_keyboard: VirtualKeyboardManagerState::new::<DuduclawComp, _>(
                dh,
                seat_filter::client_may_use_input_method,
            ),
            last_grabs: None,
            popup: None,
        };
        tracing::info!(
            "comp/ime: advertising zwp_text_input_manager_v3, zwp_input_method_manager_v2 \
             and zwp_virtual_keyboard_manager_v1 (D3-a). fcitx5 needs the latter two \
             together or it silently never initialises"
        );
        state
    }
}

impl InputMethodHandler for DuduclawComp {
    fn new_popup(&mut self, surface: PopupSurface) {
        tracing::debug!(
            location = ?surface.location(),
            caret = ?surface.text_input_rectangle(),
            "comp/ime: candidate window opened"
        );
        self.ime.popup = Some(surface);
        self.queue_redraw();
    }

    fn dismiss_popup(&mut self, surface: PopupSurface) {
        // Only clear if this is still the popup we are holding: smithay
        // dismisses-then-recreates on every parent change
        // (`InputMethodHandle::activate_input_method`), and the dismiss for an
        // older surface can arrive after the new one has been recorded.
        if self.ime.popup.as_ref() == Some(&surface) {
            self.ime.popup = None;
            tracing::debug!("comp/ime: candidate window dismissed");
        }
        self.queue_redraw();
    }

    fn popup_repositioned(&mut self, surface: PopupSurface) {
        // The handle carries the caret rectangle, so replacing it wholesale is
        // what keeps the next frame's placement current.
        self.ime.popup = Some(surface);
        self.queue_redraw();
    }

    fn parent_geometry(&self, parent: &WlSurface) -> Rectangle<i32, Logical> {
        // Where the surface holding the text field sits on the space. The
        // caret rectangle the client reports is relative to this.
        //
        // Layer surfaces are deliberately not searched: nothing in this system
        // puts a text field on one yet (`crate::layer_shell`'s scope note), and
        // an unmatched surface already degrades to the origin rather than
        // failing. Revisit when the shell's dock/menu bar migrate.
        self.space
            .elements()
            .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == parent))
            .and_then(|w| self.space.element_geometry(w))
            .unwrap_or_default()
    }
}

delegate_text_input_manager!(DuduclawComp);
delegate_input_method_manager!(DuduclawComp);
delegate_virtual_keyboard_manager!(DuduclawComp);

impl DuduclawComp {
    /// Logs which seats an input method currently holds a keyboard grab on,
    /// whenever that changes.
    ///
    /// This is the one observable that says whether D3-c is doing its job:
    /// `human=true, agent=false` is a healthy Chinese-input session;
    /// `agent=true` means an input method reached the agent seat and codrive
    /// typing is dead (see `crate::ime::seat_filter`). Edge-triggered, so an
    /// idle desktop logs nothing.
    ///
    /// Called from `DuduclawComp::codrive_refresh_ime_pause`, which already
    /// runs on every housekeeping tick on both backends.
    pub fn ime_note_grab_state(&mut self, human: bool, agent: bool) {
        if self.ime.last_grabs == Some((human, agent)) {
            return;
        }
        self.ime.last_grabs = Some((human, agent));
        tracing::info!(
            human_seat = human,
            agent_seat = agent,
            "comp/ime: input-method keyboard grab changed (agent_seat=true means D3-c failed)"
        );
    }

    /// The candidate window's render elements for one output, front-most
    /// first, or empty when no composition is in flight.
    ///
    /// Called from `decor::paint::build_output_elements`, which owns the
    /// front-to-back assembly for the whole output.
    pub fn ime_popup_elements(
        &mut self,
        renderer: &mut GlesRenderer,
        output_geo: Rectangle<i32, Logical>,
        scale: Scale<f64>,
    ) -> Vec<CodriveElement> {
        let Some(surface) = self.ime.popup.clone() else {
            return Vec::new();
        };
        if !surface.alive() {
            // The input method went away between the dismiss we never got and
            // this frame.
            self.ime.popup = None;
            return Vec::new();
        }
        // No parent means the input method built the popup before any text
        // field had focus. smithay fills the parent in when the composition
        // activates; until then there is nothing to anchor to and nothing
        // worth drawing.
        let Some(parent) = surface.get_parent() else {
            return Vec::new();
        };

        let wl_surface = surface.wl_surface();
        let bbox = bbox_from_surface_tree(wl_surface, (0, 0));
        let at = popup::place(
            surface.text_input_rectangle(),
            parent.location,
            bbox.size,
            output_geo,
        );
        if !output_geo.overlaps(Rectangle::new(at, bbox.size)) {
            // The text field is on another output; that output's own pass
            // draws the popup.
            return Vec::new();
        }
        // Keep smithay's handle in step with where we actually drew it — it
        // documents `set_location` as "adjust the popup during rendering", and
        // the value it stores is parent-relative.
        surface.set_location(at - parent.location.loc);

        let phys: Point<i32, Physical> = (at - output_geo.loc).to_physical_precise_round(scale);
        render_elements_from_surface_tree::<GlesRenderer, CodriveElement>(
            renderer,
            wl_surface,
            phys,
            scale,
            1.0,
            Kind::Unspecified,
        )
    }
}
