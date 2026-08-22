//! WP-CD4b-fix (B3) codrive — `window_geometry`, a READ-ONLY query that
//! answers "where is this application's window, in compositor global
//! coordinates?".
//!
//! ## Why this op has to exist at all
//!
//! The gateway's C-L3 AT-SPI2 locator (`duduclaw-gateway/src/codrive/
//! atspi_locate.rs`) used to ask GTK for a control's position with
//! `Component.GetExtents(CoordType::Screen)`. GTK4 ≥ 4.12 **hard-codes that
//! to `x=0, y=0`** — first-hand source, not a guess:
//!
//! - `gtk/a11y/gtkatspiutils.c`, `gtk_at_spi_translate_coordinates_from_
//!   accessible()` (line 427 in 4.18.x): the very first statement is
//!   `if (coordtype == ATSPI_COORD_TYPE_SCREEN) { *xo = 0; *yo = 0; return; }`
//!   (line 437-442).
//! - `gtk/a11y/gtkatspicomponent.c` line 140-152: `GetExtents` fills
//!   `x,y,width,height` from `gtk_accessible_get_bounds()` and then calls
//!   the translator above with `xi=0, yi=0`, which OVERWRITES `x`/`y`.
//! - Backend-independent (X11 gets zeros too); upstream closed it WONTFIX
//!   (GTK issues #5896, #6343). Debian trixie ships gtk4 4.18.6, so the
//!   appliance image always hits it.
//!
//! `CoordType::Window` (value 1) is the coordinate space that DOES work —
//! the same one Orca uses exclusively (`src/orca/ax_component.py`: six
//! `CoordType.WINDOW` call sites, zero `SCREEN`). It is computed by walking
//! each ancestor's parent-relative bounds up the tree
//! (`gtkatspiutils.c:459-475`) until `gtk_accessible_get_bounds()` stops
//! producing a parent offset, which happens at the toplevel `GtkWindow`
//! (`gtk/gtkwidget.c`'s `gtk_widget_accessible_get_bounds`, line 8914-8950:
//! bounds are computed relative to the widget's PARENT, and a parentless
//! widget — the window — is measured against itself, i.e. contributes
//! `(0,0)`). So AT-SPI WINDOW coordinates are relative to the **visible
//! window's top-left, decoration/shadow excluded** — exactly what
//! at-spi2-core#232 states verbatim: "(0,0) in the accessibility tree
//! corresponds to the top left of the application, not the top left of the
//! decoration."
//!
//! That leaves exactly one missing half — "where is that visible window on
//! screen?" — and *we are the compositor*, so we already know. This op
//! hands that half back, and deliberately keeps ALL of the smithay-specific
//! coordinate reasoning on this side of the socket (see the next section):
//! the gateway only ever does `global = origin + atspi_window_offset`.
//!
//! ## smithay coordinate semantics — verified against smithay 0.7.0's source
//!
//! Read from the pinned `smithay 0.7.0` crate source, not from memory:
//!
//! - `Space::element_location(elem)` (`src/desktop/space/mod.rs:226-232`)
//!   returns `InnerElement::location`, the value passed to `map_element`.
//! - `InnerElement::geometry()` (`:495-500`) is
//!   `{ let mut geo = self.element.geometry(); geo.loc = self.location; geo }`
//!   — i.e. **`element_location` IS the element's window-GEOMETRY origin in
//!   space (global logical) coordinates**, not its surface origin.
//! - The surface origin is a different point:
//!   `InnerElement::render_location() = location - element.geometry().loc`
//!   (`:509-511`) — and that, not `element_location`, is what
//!   `Space::element_under` returns (`:185-200`). Conflating the two is the
//!   whole trap this comment exists to close.
//! - `Window::geometry()` (`src/desktop/wayland/window.rs:154-170`) reads
//!   `SurfaceCachedState::geometry` — the client's own
//!   `xdg_surface.set_window_geometry` — clamped to the bbox, in
//!   SURFACE-local coordinates. On Wayland GTK4 always sets it to
//!   `(shadow_left, shadow_top, w - shadow_left - shadow_right, h - shadow_top
//!   - shadow_bottom)` (`gdk/wayland/gdksurface-wayland.c:138-143`, applied
//!   at `:579`), so its `.loc` is precisely the CSD shadow inset and its
//!   `.size` is the visible window size.
//!
//! Putting those together: `element_location` already equals "top-left of
//! the visible window in global coordinates" — the *same* point AT-SPI's
//! WINDOW space is relative to. No shadow arithmetic is needed by the
//! caller; the shadow inset is reported only as `shadow_dx`/`shadow_dy` for
//! diagnostics (and it is legitimately `(0,0)` when maximized/fullscreen —
//! `gtk/gtkwindow.c:4186-4188` zeroes the shadow there — so nobody may
//! assume it is non-zero).
//!
//! ## Fail-closed matching
//!
//! [`resolve_window`] never falls back to "the first/lowest-z window" the
//! way `window_target::match_window_query` deliberately does for
//! `activate_window`. Picking a plausible-but-wrong window here would hand
//! the agent a plausible-but-wrong coordinate, and clicking the wrong place
//! is worse than not clicking — so an unresolvable query answers
//! `ambiguous_window`/`window_not_found` and the gateway degrades to C-L1.
//!
//! ## Not an action
//!
//! Like `status`/`rotate_token`, this op is answered outside the
//! frozen/terminated gates (`listener.rs`): it mutates nothing, moves no
//! window, touches no seat. Denying it under freeze would be actively
//! *worse* for safety — the gateway's fallback on a failed locate is the
//! step's literal C-L1 coordinate, i.e. denying the query pushes the agent
//! toward the less-informed click, while the click itself stays gated
//! exactly as before. It therefore also writes no audit row (matching
//! `status`; the gateway audits every locate outcome with the resolved
//! origin in `tool_calls.jsonl` already) — only `tracing` diagnostics.

use serde::Serialize;
use smithay::{
    desktop::Window,
    reexports::wayland_server::{DisplayHandle, Resource},
};

use crate::state::DuduclawComp;

use super::window_target::window_identity;

/// One `window_geometry` request in flight, bridged from the socket thread
/// to the calloop main thread. Same oneshot-reply shape
/// `crate::shell_control::ShellControlMsg` uses (see that module's doc for
/// why a request/response round trip is needed at all, unlike the ordinary
/// fire-and-forget `InjectCmd` channel).
pub struct CodriveQuery {
    pub req: WindowGeometryRequest,
    pub reply_tx: std::sync::mpsc::Sender<WindowGeometryReply>,
}

/// `{"op":"window_geometry","pid":1234,"app_id":"…"}` — at least one of the
/// two must be present (enforced in `listener::validate`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowGeometryRequest {
    /// The client's process id, as the gateway learned it from the a11y bus
    /// (`org.freedesktop.DBus.GetConnectionUnixProcessID`). The strong
    /// identity signal: matched against each mapped toplevel's own
    /// `SO_PEERCRED`-derived Wayland client credentials.
    pub pid: Option<u32>,
    /// Optional tiebreaker, used ONLY to disambiguate when `pid` matched
    /// more than one toplevel (or as the sole key when no pid was given).
    /// Same exact-app_id-then-title-prefix vocabulary `activate_window`
    /// uses, minus its lowest-z fallback.
    pub app_id: Option<String>,
}

/// The visible window's placement, in the SAME global logical coordinate
/// space `InjectCmd::Move`/`Button` already operate in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct WindowGeometryInfo {
    /// `Space::element_location` — top-left of the visible window (CSD
    /// shadow excluded), global logical coordinates. This is the origin
    /// AT-SPI `CoordType::Window` offsets are relative to.
    pub origin_x: i32,
    pub origin_y: i32,
    /// `Window::geometry().size` — the visible window's size. The gateway
    /// uses it to bound-check a converted point before trusting it.
    pub width: i32,
    pub height: i32,
    /// `Window::geometry().loc` — the CSD shadow inset in surface-local
    /// coordinates. Diagnostics only; legitimately `(0,0)` for a maximized/
    /// fullscreen/server-decorated window (module doc).
    pub shadow_dx: i32,
    pub shadow_dy: i32,
    /// Which criterion resolved the query — `pid` / `pid+app_id` /
    /// `pid+title_prefix` / `app_id` / `title_prefix`.
    pub matched_via: &'static str,
}

/// One JSON line back to the caller. Every field but `ok` is omitted when
/// absent so the shapes stay exactly as documented in the gateway's
/// hand-mirrored `client.rs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WindowGeometryReply {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowGeometryInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<&'static str>,
    /// How many toplevels the query matched — present only on
    /// `ambiguous_window`, so the failure is diagnosable from the ack alone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidates: Option<usize>,
}

impl WindowGeometryReply {
    pub fn found(window: WindowGeometryInfo) -> Self {
        Self { ok: true, window: Some(window), error: None, candidates: None }
    }

    pub fn err(error: &'static str) -> Self {
        Self { ok: false, window: None, error: Some(error), candidates: None }
    }

    pub fn ambiguous(candidates: usize) -> Self {
        Self { ok: false, window: None, error: Some("ambiguous_window"), candidates: Some(candidates) }
    }
}

/// One mapped toplevel's identity facts, in `Space::elements()` order.
/// Plain data (no `Window`/`Space`) so [`resolve_window`] is unit-testable
/// without a real Wayland display — the same pure-logic/thin-wrapper split
/// `window_target.rs` and `shadow.rs` already established for this crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WindowCandidate<'a> {
    pub idx: usize,
    pub pid: Option<u32>,
    pub app_id: Option<&'a str>,
    pub title: Option<&'a str>,
}

/// Outcome of the pure matching policy.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum GeometryMatch {
    /// Exactly one toplevel matched; carries its index and the criterion.
    Unique(usize, &'static str),
    /// Nothing matched — honest miss.
    NotFound,
    /// More than one toplevel matched and nothing could separate them.
    /// Deliberately NOT resolved to "the first one" (module doc).
    Ambiguous(usize),
}

/// Pure matching policy (module doc's fail-closed section).
///
/// 1. `pid` given → keep only toplevels owned by that pid.
///    - 0 left ⇒ `NotFound`.
///    - exactly 1 left ⇒ `Unique` (the app_id, if any, is not even
///      consulted: pid already identified the process uniquely, and the
///      Wayland app_id need not equal the AT-SPI application name).
///    - `>1` left ⇒ narrow with `app_id` (exact), then `title` (prefix),
///      inside that pid pool only; anything short of exactly one survivor
///      is `Ambiguous`.
/// 2. `pid` absent → `app_id` exact over all toplevels, then `title`
///    prefix; exactly one survivor or bust.
pub(crate) fn resolve_window(
    pid: Option<u32>,
    app_id: Option<&str>,
    candidates: &[WindowCandidate<'_>],
) -> GeometryMatch {
    let query = app_id.map(str::trim).filter(|q| !q.is_empty());

    let Some(pid) = pid else {
        // No pid: the app_id/title query is the ONLY identity we have, so
        // it must resolve on its own. (`listener::validate` guarantees at
        // least one of the two is present, so `query` being `None` here
        // can only mean "app_id was whitespace" — refuse rather than
        // matching every window.)
        let Some(query) = query else {
            return GeometryMatch::NotFound;
        };
        return narrow(query, candidates.iter(), "app_id", "title_prefix");
    };

    let pool: Vec<&WindowCandidate<'_>> = candidates.iter().filter(|c| c.pid == Some(pid)).collect();
    match pool.len() {
        0 => GeometryMatch::NotFound,
        1 => GeometryMatch::Unique(pool[0].idx, "pid"),
        n => match query {
            // The pid matched several toplevels of the same process (a
            // multi-window app). Only the caller-supplied app_id/title can
            // separate them; if it can't, refuse.
            Some(query) => match narrow(query, pool.into_iter(), "pid+app_id", "pid+title_prefix") {
                GeometryMatch::Unique(idx, via) => GeometryMatch::Unique(idx, via),
                _ => GeometryMatch::Ambiguous(n),
            },
            None => GeometryMatch::Ambiguous(n),
        },
    }
}

/// Exact-`app_id` pass, then anchored `title`-prefix pass (this crate's
/// coding convention #2: `starts_with`, never an unanchored `contains`).
/// Each pass must leave EXACTLY one survivor; more than one is `Ambiguous`,
/// none falls through to the next pass and finally to `NotFound`.
fn narrow<'a, 'b, I>(query: &str, candidates: I, app_via: &'static str, title_via: &'static str) -> GeometryMatch
where
    I: Iterator<Item = &'b WindowCandidate<'a>> + Clone,
    'a: 'b,
{
    let by_app: Vec<&WindowCandidate<'_>> = candidates.clone().filter(|c| c.app_id == Some(query)).collect();
    match by_app.len() {
        1 => return GeometryMatch::Unique(by_app[0].idx, app_via),
        0 => {}
        n => return GeometryMatch::Ambiguous(n),
    }

    let by_title: Vec<&WindowCandidate<'_>> = candidates
        .filter(|c| c.title.is_some_and(|t| t.starts_with(query)))
        .collect();
    match by_title.len() {
        1 => GeometryMatch::Unique(by_title[0].idx, title_via),
        0 => GeometryMatch::NotFound,
        n => GeometryMatch::Ambiguous(n),
    }
}

/// The connecting client's pid, straight from the Wayland socket's
/// `SO_PEERCRED` credentials (`wayland_server::Client::get_credentials`).
/// `None` for anything unresolvable (dead client, X11 window — this crate
/// has no XWayland support, so `toplevel()` is `Some` for every window it
/// maps, but this stays an `Option` rather than the `unwrap()`
/// `window_target::window_identity` uses: a `None` here must degrade into
/// "this candidate has no pid", never a panic on a raced client teardown).
fn window_pid(window: &Window, dh: &DisplayHandle) -> Option<u32> {
    let toplevel = window.toplevel()?;
    let client = toplevel.wl_surface().client()?;
    let creds = client.get_credentials(dh).ok()?;
    u32::try_from(creds.pid).ok()
}

impl DuduclawComp {
    /// Thin live wrapper: collects identity facts off `self.space`, defers
    /// to [`resolve_window`] for the decision, and converts the winning
    /// window's smithay geometry into the wire answer (module doc's
    /// "smithay coordinate semantics" section for why `element_location` is
    /// the right origin and `element_under`'s render location is not).
    ///
    /// Runs on the calloop main thread only (reached via `codrive::init`'s
    /// query-channel source), the same thread that owns `self.space`.
    pub fn codrive_window_geometry(&self, req: &WindowGeometryRequest) -> WindowGeometryReply {
        let windows: Vec<Window> = self.space.elements().cloned().collect();
        let identities: Vec<(Option<String>, Option<String>)> = windows.iter().map(window_identity).collect();
        let pids: Vec<Option<u32>> = windows.iter().map(|w| window_pid(w, &self.display_handle)).collect();
        let candidates: Vec<WindowCandidate<'_>> = identities
            .iter()
            .enumerate()
            .map(|(idx, (app_id, title))| WindowCandidate {
                idx,
                pid: pids[idx],
                app_id: app_id.as_deref(),
                title: title.as_deref(),
            })
            .collect();

        tracing::debug!(
            pid = ?req.pid,
            app_id = ?req.app_id,
            known = ?candidates,
            "codrive: window_geometry — resolving against currently mapped windows"
        );

        match resolve_window(req.pid, req.app_id.as_deref(), &candidates) {
            GeometryMatch::Unique(idx, matched_via) => {
                let window = &windows[idx];
                // `element_location` is `Option` because the element may
                // have been unmapped between `elements()` and here; that is
                // an honest failure, not a reason to guess an origin.
                let Some(origin) = self.space.element_location(window) else {
                    return WindowGeometryReply::err("window_unmapped");
                };
                let geo = window.geometry();
                if geo.size.w <= 0 || geo.size.h <= 0 {
                    // A zero-size geometry means the client has not
                    // committed a usable window geometry yet — every
                    // coordinate derived from it would be meaningless.
                    return WindowGeometryReply::err("window_zero_size");
                }
                let info = WindowGeometryInfo {
                    origin_x: origin.x,
                    origin_y: origin.y,
                    width: geo.size.w,
                    height: geo.size.h,
                    shadow_dx: geo.loc.x,
                    shadow_dy: geo.loc.y,
                    matched_via,
                };
                tracing::debug!(?info, "codrive: window_geometry — resolved");
                WindowGeometryReply::found(info)
            }
            GeometryMatch::NotFound => {
                tracing::warn!(
                    pid = ?req.pid,
                    app_id = ?req.app_id,
                    "codrive: window_geometry — no toplevel matched (locate will fail closed)"
                );
                WindowGeometryReply::err("window_not_found")
            }
            GeometryMatch::Ambiguous(n) => {
                tracing::warn!(
                    pid = ?req.pid,
                    app_id = ?req.app_id,
                    candidates = n,
                    "codrive: window_geometry — more than one toplevel matched and nothing separated them; refusing (locate will fail closed)"
                );
                WindowGeometryReply::ambiguous(n)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand<'a>(idx: usize, pid: Option<u32>, app_id: Option<&'a str>, title: Option<&'a str>) -> WindowCandidate<'a> {
        WindowCandidate { idx, pid, app_id, title }
    }

    // ── pid path ────────────────────────────────────────────────────────

    #[test]
    fn pid_matching_exactly_one_window_wins_without_consulting_app_id() {
        let c = [
            cand(0, Some(11), Some("foot"), Some("shell")),
            cand(1, Some(22), Some("org.gnome.TextEditor"), Some("Untitled")),
        ];
        // Deliberately passes an app_id that matches NEITHER window: the
        // AT-SPI application name is frequently not the Wayland app_id, so
        // a unique pid must not be thrown away because of it.
        assert_eq!(resolve_window(Some(22), Some("gnome-text-editor"), &c), GeometryMatch::Unique(1, "pid"));
    }

    #[test]
    fn pid_matching_nothing_is_not_found() {
        let c = [cand(0, Some(11), Some("foot"), Some("shell"))];
        assert_eq!(resolve_window(Some(99), None, &c), GeometryMatch::NotFound);
    }

    #[test]
    fn pid_matching_two_windows_without_a_tiebreaker_is_ambiguous() {
        let c = [
            cand(0, Some(11), Some("foot"), Some("shell A")),
            cand(1, Some(11), Some("foot"), Some("shell B")),
        ];
        assert_eq!(resolve_window(Some(11), None, &c), GeometryMatch::Ambiguous(2));
    }

    #[test]
    fn pid_pool_is_narrowed_by_exact_app_id() {
        let c = [
            cand(0, Some(11), Some("foot-A"), Some("shell A")),
            cand(1, Some(11), Some("foot-B"), Some("shell B")),
        ];
        assert_eq!(resolve_window(Some(11), Some("foot-B"), &c), GeometryMatch::Unique(1, "pid+app_id"));
    }

    #[test]
    fn pid_pool_is_narrowed_by_title_prefix_when_no_app_id_matches() {
        let c = [
            cand(0, Some(11), Some("foot"), Some("Alpha — one")),
            cand(1, Some(11), Some("foot"), Some("Beta — two")),
        ];
        assert_eq!(resolve_window(Some(11), Some("Beta"), &c), GeometryMatch::Unique(1, "pid+title_prefix"));
    }

    #[test]
    fn pid_pool_with_a_tiebreaker_that_separates_nothing_stays_ambiguous() {
        let c = [
            cand(0, Some(11), Some("foot"), Some("shell A")),
            cand(1, Some(11), Some("foot"), Some("shell B")),
        ];
        // "foot" is both windows' app_id — narrowing can't split them, and
        // the answer must be a refusal, never "take index 0".
        assert_eq!(resolve_window(Some(11), Some("foot"), &c), GeometryMatch::Ambiguous(2));
    }

    // ── app_id-only path ────────────────────────────────────────────────

    #[test]
    fn app_id_only_exact_match_wins() {
        let c = [
            cand(0, Some(11), Some("foot"), Some("shell")),
            cand(1, Some(22), Some("org.gnome.TextEditor"), Some("Untitled")),
        ];
        assert_eq!(resolve_window(None, Some("org.gnome.TextEditor"), &c), GeometryMatch::Unique(1, "app_id"));
    }

    #[test]
    fn app_id_only_falls_back_to_title_prefix() {
        let c = [cand(0, Some(11), Some("org.foo.bar"), Some("Bar — Editor"))];
        assert_eq!(resolve_window(None, Some("Bar"), &c), GeometryMatch::Unique(0, "title_prefix"));
    }

    #[test]
    fn app_id_only_title_prefix_is_anchored_not_substring() {
        let c = [cand(0, None, None, Some("My Editor — untitled"))];
        assert_eq!(resolve_window(None, Some("Editor"), &c), GeometryMatch::NotFound);
    }

    #[test]
    fn app_id_only_duplicate_app_ids_are_ambiguous_never_lowest_z() {
        // The exact case `window_target::match_window_query` deliberately
        // resolves to index 0 for `activate_window`. For a COORDINATE this
        // must refuse instead — that is the whole point of not reusing that
        // function here.
        let c = [
            cand(0, Some(11), Some("foot"), Some("shell A")),
            cand(1, Some(22), Some("foot"), Some("shell B")),
        ];
        assert_eq!(resolve_window(None, Some("foot"), &c), GeometryMatch::Ambiguous(2));
    }

    #[test]
    fn app_id_only_no_match_is_not_found() {
        let c = [cand(0, Some(11), Some("foot"), Some("shell"))];
        assert_eq!(resolve_window(None, Some("does-not-exist"), &c), GeometryMatch::NotFound);
    }

    #[test]
    fn whitespace_only_app_id_without_pid_never_matches_everything() {
        let c = [cand(0, Some(11), Some("foot"), Some("shell"))];
        assert_eq!(resolve_window(None, Some("   "), &c), GeometryMatch::NotFound);
    }

    #[test]
    fn empty_window_list_is_not_found_on_both_paths() {
        let c: [WindowCandidate<'_>; 0] = [];
        assert_eq!(resolve_window(Some(11), None, &c), GeometryMatch::NotFound);
        assert_eq!(resolve_window(None, Some("foot"), &c), GeometryMatch::NotFound);
    }

    #[test]
    fn a_window_whose_pid_is_unreadable_never_matches_a_pid_query() {
        let c = [cand(0, None, Some("foot"), Some("shell"))];
        assert_eq!(resolve_window(Some(11), None, &c), GeometryMatch::NotFound);
    }

    // ── wire shapes ─────────────────────────────────────────────────────

    #[test]
    fn reply_wire_shape_success_omits_error_fields() {
        let reply = WindowGeometryReply::found(WindowGeometryInfo {
            origin_x: 10,
            origin_y: 20,
            width: 800,
            height: 600,
            shadow_dx: 26,
            shadow_dy: 23,
            matched_via: "pid",
        });
        let v = serde_json::to_value(&reply).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "ok": true,
                "window": {
                    "origin_x": 10, "origin_y": 20,
                    "width": 800, "height": 600,
                    "shadow_dx": 26, "shadow_dy": 23,
                    "matched_via": "pid"
                }
            })
        );
    }

    #[test]
    fn reply_wire_shape_errors() {
        assert_eq!(
            serde_json::to_value(WindowGeometryReply::err("window_not_found")).unwrap(),
            serde_json::json!({"ok": false, "error": "window_not_found"})
        );
        assert_eq!(
            serde_json::to_value(WindowGeometryReply::ambiguous(3)).unwrap(),
            serde_json::json!({"ok": false, "error": "ambiguous_window", "candidates": 3})
        );
    }
}
