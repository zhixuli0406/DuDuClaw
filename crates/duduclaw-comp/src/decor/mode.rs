//! WM-2: `zxdg_decoration_manager_v1` negotiation — **who draws the title
//! bar for this particular window**.
//!
//! ## Why this is a per-window decision and not a global switch
//!
//! WM-1 answered every request with `ClientSide` because comp had no
//! decoration renderer. It has one now, so the default flips to `ServerSide`.
//! But "always `ServerSide`" would be just as wrong as "always `ClientSide`,
//! for a first-hand reason recorded in this repo's own WM-1 notes: **Chromium
//! 151 draws its own CSD tab strip in normal (non-app) mode regardless of what
//! the protocol says**. A window that draws its own chrome *and* receives a
//! server title bar gets two of them stacked — visibly broken, and worse than
//! either extreme on its own.
//!
//! So the rule is the protocol's own rule, honoured literally:
//!
//! | client behaviour | result |
//! |---|---|
//! | never creates a `zxdg_toplevel_decoration_v1` | **client-side** — comp draws nothing |
//! | creates one, never calls `set_mode` | **server-side** (our preference) |
//! | `set_mode(ServerSide)` | **server-side** |
//! | `set_mode(ClientSide)` | **client-side** — honoured, no SSD drawn |
//! | `unset_mode` | back to **server-side** (the preference) |
//!
//! The first row is the conservative half and it is deliberate: a client that
//! never touches the protocol at all has, by xdg-decoration convention,
//! opted into drawing its own decorations, and there is no negotiation to
//! honour. Drawing a server title bar for it would be the double-title-bar
//! failure with no way for the client to object.
//!
//! ## Two hard overrides on top of the negotiation
//!
//! [`negotiated_ssd`] refuses SSD for the session shell and for
//! shadow-workspace windows no matter what was negotiated:
//!
//! * **the shell** paints the menu bar and the dock itself and is full-output;
//!   a title bar over it would cover the menu bar and offer a close button
//!   that ends the session (the exact failure `window_policy`'s Super+Q guard
//!   already refuses).
//! * **shadow-workspace windows** (CD-2, `codrive/shadow.rs`) are agent-side
//!   work isolated at `SHADOW_ORIGIN`; their geometry is owned by that module
//!   and is only ever seen through the PiP preview. Decorating them would
//!   change geometry `codrive/shadow.rs` computes for itself.

use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1;

/// The negotiated decoration owner for one toplevel.
///
/// Absence of an entry (`None` at the call site) means "this client has never
/// created a decoration object", which is a third state and NOT the same as
/// `ClientSide` — see the table in this module's doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecorMode {
    /// Comp draws the title bar, border and shadow.
    ServerSide,
    /// The client draws its own; comp draws nothing and adds no insets.
    ClientSide,
}

impl DecorMode {
    /// The wire value to send back in the `configure` event.
    pub fn wire(self) -> zxdg_toplevel_decoration_v1::Mode {
        match self {
            DecorMode::ServerSide => zxdg_toplevel_decoration_v1::Mode::ServerSide,
            DecorMode::ClientSide => zxdg_toplevel_decoration_v1::Mode::ClientSide,
        }
    }

    /// Short, log-safe name.
    pub fn as_str(self) -> &'static str {
        match self {
            DecorMode::ServerSide => "server-side",
            DecorMode::ClientSide => "client-side",
        }
    }
}

/// What comp answers when a client creates a decoration object without asking
/// for anything (`new_decoration`) or explicitly withdraws its request
/// (`unset_mode`).
pub const PREFERRED: DecorMode = DecorMode::ServerSide;

/// Maps a client's `set_mode` argument onto our answer.
///
/// The protocol allows a compositor to override the client's request; this
/// implementation deliberately does **not** override a `ClientSide` request,
/// because honouring it is the only defence against the double-title-bar
/// failure described in this module's doc. A `ServerSide` request is of course
/// granted — it is what we would have picked anyway.
pub fn answer_request(requested: zxdg_toplevel_decoration_v1::Mode) -> DecorMode {
    match requested {
        zxdg_toplevel_decoration_v1::Mode::ClientSide => DecorMode::ClientSide,
        zxdg_toplevel_decoration_v1::Mode::ServerSide => DecorMode::ServerSide,
        // The enum is `#[non_exhaustive]` on the wire; an unknown value is a
        // client sending garbage. Fail towards the conservative answer (draw
        // nothing) rather than towards drawing a second title bar.
        _ => DecorMode::ClientSide,
    }
}

/// The final "does comp draw a title bar for this window" decision.
///
/// `negotiated` is `None` when the client has never created a decoration
/// object at all.
pub fn negotiated_ssd(is_shell: bool, in_shadow: bool, negotiated: Option<DecorMode>) -> bool {
    if is_shell || in_shadow {
        return false;
    }
    matches!(negotiated, Some(DecorMode::ServerSide))
}

#[cfg(test)]
mod tests {
    use super::*;
    use zxdg_toplevel_decoration_v1::Mode as WireMode;

    #[test]
    fn our_preference_is_server_side() {
        assert_eq!(PREFERRED, DecorMode::ServerSide);
        assert_eq!(PREFERRED.wire(), WireMode::ServerSide);
    }

    #[test]
    fn a_client_side_request_is_honoured_not_overridden() {
        // This is the anti-double-title-bar rule. If it ever flips to
        // "override to ServerSide", Chromium in normal mode grows a second
        // title bar — see this module's doc for the first-hand observation.
        assert_eq!(answer_request(WireMode::ClientSide), DecorMode::ClientSide);
    }

    #[test]
    fn a_server_side_request_is_granted() {
        assert_eq!(answer_request(WireMode::ServerSide), DecorMode::ServerSide);
    }

    #[test]
    fn a_client_that_never_negotiated_gets_no_server_decoration() {
        assert!(!negotiated_ssd(false, false, None));
    }

    #[test]
    fn a_client_that_created_a_decoration_object_gets_server_decoration() {
        assert!(negotiated_ssd(false, false, Some(DecorMode::ServerSide)));
    }

    #[test]
    fn the_session_shell_is_never_server_decorated() {
        // Even if it asked for it. A title bar over the shell would cover its
        // own menu bar and offer a close button that ends the session.
        assert!(!negotiated_ssd(true, false, Some(DecorMode::ServerSide)));
        assert!(!negotiated_ssd(true, false, None));
    }

    #[test]
    fn shadow_workspace_windows_are_never_server_decorated() {
        assert!(!negotiated_ssd(false, true, Some(DecorMode::ServerSide)));
    }

    #[test]
    fn mode_names_are_stable_for_logs() {
        assert_eq!(DecorMode::ServerSide.as_str(), "server-side");
        assert_eq!(DecorMode::ClientSide.as_str(), "client-side");
    }
}
