//! WP-CD4a-COMP codrive — `activate_window` (B-line CD-4a, multi-window
//! targeting). Task brief: "依 xdg-shell app_id（優先）/title 前綴比對 space
//! 中 toplevel（命中→複用 A1 的 `DuduclawComp::focus_window` helper…；查無→
//! 誠實錯誤 ack（非靜默 no-op）；凍結/takeover 中拒絕（照既有 op 閘）)".
//!
//! ## Relationship to the WP-A1 multi-window round
//! This file adds no new focus/activation mechanics at all — it's a
//! *lookup* layer on top of the WP-A1 round's `DuduclawComp::focus_window`
//! (`state.rs`), the same shared helper the human click-to-focus arm
//! (`input.rs`), the agent click-to-focus arm (`codrive/mod.rs`'s
//! `InjectCmd::Button` handling), Super+Tab cycling
//! (`state.rs::cycle_focus`), and close-time focus handoff
//! (`state.rs::reassign_focus_on_window_removed`) all already share. The
//! only new work here is resolving a caller-supplied `app_id` string (the
//! wire query) to one of the `Window`s already mapped in `self.space` —
//! everything downstream of that lookup is the already-VM-verified
//! raise+focus+activate path.
//!
//! ## Matching policy, and why it's split into a pure function
//! [`match_window_query`] takes plain `(index, app_id, title)` tuples, not
//! real `Window`/`Space` objects — same "pure logic unit-tested, live
//! Seat/Space state live-run-tested" split `shadow.rs`'s
//! `freeze_bypass_decision` (pure) / `is_freeze_bypass_eligible` (thin real-
//! state wrapper) pair already established for this crate (this crate has
//! never constructed a real smithay `Window`/`Space`/`Seat` in a unit test —
//! see BUILD.md's "A1 multi-window round" section, "none of the existing 70
//! tests construct real smithay objects either"). [`find_target_window`] is
//! the thin wrapper that extracts live identity facts from `comp.space` and
//! defers to the pure function for the actual decision.
//!
//! Priority order (task brief: "app_id（優先）/title 前綴"): every mapped
//! window is checked for an EXACT `app_id` match first; only if none match
//! at all does a second pass look for a window whose `title` STARTS WITH
//! the query. Both passes iterate `Space::elements()`'s own order (bottom-
//! to-top, same order `focus_window`/`cycle_focus` already rely on), so a
//! query that matches more than one window (e.g. two `foot` instances with
//! the same app_id) deterministically resolves to the LOWEST one in z-order
//! — an honest MVP assumption (DESIGN doesn't specify tie-breaking here);
//! `activate_window` can always be re-sent with a more specific query (or
//! the wire's own `app_id` need only be unique enough for the caller's
//! actual use case, e.g. `foot -a foot-A` vs. `foot -a foot-B` in this
//! crate's own live-run scripts — see BUILD.md's "A1 multi-window round").
//!
//! ## Why never shadow-bypass-eligible
//! `shadow.rs`'s `freeze_bypass_decision` has an explicit
//! `InjectCmd::ActivateWindow { .. } => false` arm (task brief: "凍結/
//! takeover 中拒絕（照既有 op 閘）" — reuse the standard gate, no new bypass
//! carve-out). Unlike `Move`/`Button`/`Highlight`, this op carries no
//! target coordinate to confirm against `SHADOW_ORIGIN`'s bounds — the
//! query is resolved to a window only INSIDE this file's own logic, which
//! never runs while frozen in the first place (`handle_agent_inject`'s
//! frozen gate short-circuits before this file's functions are called).
//!
//! ## WP-comp-shell-ipc reuse (2026-08-22)
//! `find_target_window`/`window_identity`/[`WindowMatch`] widened from
//! module-private to `pub(crate)` this round — `crate::shell_control`
//! (the human-operated dock↔comp IPC, a SEPARATE socket/trust-boundary
//! from this agent-injection channel — see that module's doc comment)
//! reuses this exact same lookup+matching policy for its own
//! `focus_window` op instead of re-deriving a second copy. No logic
//! changed here; only visibility. `codrive::mod`'s `pub(crate) mod
//! window_target;` (was module-private `mod window_target;`) is the other
//! half of this same widening.

use smithay::{
    desktop::Window,
    utils::SERIAL_COUNTER,
    wayland::{compositor::with_states, shell::xdg::XdgToplevelSurfaceData},
};

use crate::state::DuduclawComp;

/// Which criterion matched a query against a candidate window's identity —
/// carried through to the success audit line's `detail` so a reader can
/// tell an exact app_id hit apart from a title-prefix fallback, not just
/// that "something" matched.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum WindowMatch {
    AppId(String),
    TitlePrefix(String),
}

/// Pure matching policy (this file's module doc) — no `Window`/`Space`, so
/// it's unit-testable without a real Wayland display. `candidates` carries
/// each window's z-order index (NOT a `Window` — the real wrapper below
/// looks the winning element back up by index) alongside its current
/// app_id/title, in the exact order `Space::elements()` yields them.
fn match_window_query<'a>(
    query: &str,
    candidates: impl Iterator<Item = (usize, Option<&'a str>, Option<&'a str>)>,
) -> Option<(usize, WindowMatch)> {
    let candidates: Vec<_> = candidates.collect();

    // Priority 1: exact app_id match.
    for &(idx, app_id, _title) in &candidates {
        if app_id == Some(query) {
            return Some((idx, WindowMatch::AppId(query.to_string())));
        }
    }

    // Priority 2 (fallback): title prefix match.
    for &(idx, _app_id, title) in &candidates {
        if let Some(title) = title {
            if title.starts_with(query) {
                return Some((idx, WindowMatch::TitlePrefix(title.to_string())));
            }
        }
    }

    None
}

/// Reads a mapped window's current identity — `app_id`/`title` for an xdg
/// toplevel, `class`/`title` for an X11 window (A4, CP-1 XWayland: X11's
/// `WM_CLASS` "class" is the closest analogue to an xdg `app_id` — both are
/// the stable, machine-facing name an app registers, as opposed to its
/// human-facing, often-changing window title) — in one lock acquisition for
/// the xdg case. Mirrors `handlers/xdg_shell.rs::handle_commit`'s existing
/// `with_states` + `XdgToplevelSurfaceData` access pattern for that branch.
///
/// Before A4, this crate had no X11 window support at all, so `window.
/// toplevel().unwrap()` was safe here — every `Window` this crate ever
/// mapped came from `Window::new_wayland_window`. That assumption no longer
/// holds; every OTHER `w.toplevel().unwrap()` call site in this codebase that
/// predates A4 relied on the same one and has been fixed alongside this one.
pub(crate) fn window_identity(window: &Window) -> (Option<String>, Option<String>) {
    if let Some(x11) = window.x11_surface() {
        // `X11Surface::class()`/`title()` return `String`, defaulting to
        // empty (never absent) when the client set no `WM_CLASS`/`WM_NAME` —
        // normalised to `None` here so callers see the exact same "unset"
        // shape they already handle for the xdg case.
        let class = x11.class();
        let title = x11.title();
        return (
            (!class.is_empty()).then_some(class),
            (!title.is_empty()).then_some(title),
        );
    }
    let toplevel = window
        .toplevel()
        .expect("a Window is either xdg-toplevel-backed or X11-backed (see WindowSurface)");
    with_states(toplevel.wl_surface(), |states| {
        let attrs = states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .expect("mapped xdg toplevels always carry XdgToplevelSurfaceData")
            .lock()
            .unwrap();
        (attrs.app_id.clone(), attrs.title.clone())
    })
}

/// Thin wrapper (this file's module doc): extracts live identity facts from
/// every window currently in `space` and defers to [`match_window_query`]
/// for the actual decision. `pub(crate)` (was module-private) — this
/// file's own `codrive_activate_window` is still one caller;
/// `crate::shell_control`'s `focus_window` op is the other (WP-comp-
/// shell-ipc, see this file's module doc).
/// **WM-3** replaced the previous `find_target_window(&Space, query)` with this
/// explicit-candidate-list form.
///
/// A **minimized** window is not in `Space` at all (`crate::minimize`), so
/// resolving against the space alone would answer "no toplevel matched" for
/// exactly the windows a dock (or an agent) most needs to bring back — the
/// failure `crate::minimize`'s recoverability invariant exists to prevent. Both
/// callers now pass `DuduclawComp::all_windows()`, and the matching POLICY
/// below is unchanged and still lives in one place.
pub(crate) fn find_target_in(windows: &[Window], query: &str) -> Option<(Window, WindowMatch)> {
    let identities: Vec<(Option<String>, Option<String>)> = windows.iter().map(window_identity).collect();
    let candidates = identities
        .iter()
        .enumerate()
        .map(|(i, (app_id, title))| (i, app_id.as_deref(), title.as_deref()));
    let (idx, matched) = match_window_query(query, candidates)?;
    Some((windows[idx].clone(), matched))
}

impl DuduclawComp {
    /// `{"op":"activate_window","app_id":"..."}` (this file's module doc).
    /// Runs on the calloop main thread only, reached via
    /// `codrive::handle_agent_inject`'s `InjectCmd::ActivateWindow` arm —
    /// the same thread that owns `self.space`/`self.agent_seat`. A query
    /// that matches nothing is answered honestly (`activate_window_failed`
    /// audit line) — never a silent no-op, per the task brief.
    pub fn codrive_activate_window(&mut self, query: String) {
        // Debug-level diagnostic (same "make blind live-run verification
        // possible" motive as the A1 round's own geometry-in-commit-log
        // addition — see BUILD.md's "CSD title-bar/resize-hotspot
        // coordinates were found empirically" note): without this, a
        // caller has no way to discover what app_id/title a live client
        // actually registered short of guessing, since neither is logged
        // anywhere else in this crate.
        tracing::debug!(
            query = %query,
            known = ?self.all_windows().iter().map(window_identity).collect::<Vec<_>>(),
            "codrive: activate_window — resolving against every known window (mapped + minimized)"
        );
        // WM-3: resolved against mapped AND minimized windows, the same set
        // `shell_control`'s own `focus_window` op uses. Without this the agent
        // would get an honest-but-useless "no toplevel matched" for a window
        // that exists and is one restore away — the exact failure
        // `crate::minimize`'s recoverability invariant exists to prevent, and
        // there is no reason it should hold for the human but not the agent.
        let candidates = self.all_windows();
        match find_target_in(&candidates, &query) {
            Some((window, matched)) => {
                let restored = self.is_minimized(&window);
                if restored {
                    self.unminimize_window(&window);
                }
                let seat = self.agent_seat.clone();
                let serial = SERIAL_COUNTER.next_serial();
                self.focus_window(&seat, Some(&window), serial);

                let detail = match &matched {
                    WindowMatch::AppId(id) => {
                        format!("query={query:?} matched_app_id={id:?} restored={restored}")
                    }
                    WindowMatch::TitlePrefix(title) => {
                        format!(
                            "query={query:?} matched_via=title_prefix matched_title={title:?} restored={restored}"
                        )
                    }
                };
                tracing::info!(query = %query, detail = %detail, "codrive: activate_window — window focused");
                self.codrive.record("activate_window", Some("activate_window"), None, None, Some(detail));
            }
            None => {
                tracing::warn!(query = %query, "codrive: activate_window — no toplevel matched by app_id or title prefix");
                self.codrive.record(
                    "activate_window_failed",
                    Some("activate_window"),
                    None,
                    None,
                    Some(format!("query={query:?} — no toplevel matched by app_id (exact) or title (prefix)")),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Explicit single lifetime, not elided: `&[(Option<&str>, Option<&str>)]`
    // carries THREE distinct elided lifetime positions (the slice ref plus
    // each `&str`), so Rust's "exactly one input lifetime" elision rule
    // does not apply here — an elided signature would fail to compile with
    // a "missing lifetime specifier" error on the return type.
    fn candidates<'a>(
        items: &'a [(Option<&'a str>, Option<&'a str>)],
    ) -> Vec<(usize, Option<&'a str>, Option<&'a str>)> {
        items.iter().enumerate().map(|(i, (a, t))| (i, *a, *t)).collect()
    }

    /// 命中 — an exact app_id match wins outright.
    #[test]
    fn match_window_query_finds_exact_app_id_hit() {
        let items = [
            (Some("foot-A"), Some("foot")),
            (Some("gtk3-widget-factory"), Some("GTK3 Widget Factory")),
        ];
        let got = match_window_query("gtk3-widget-factory", candidates(&items).into_iter());
        assert_eq!(got, Some((1, WindowMatch::AppId("gtk3-widget-factory".to_string()))));
    }

    /// 查無 — nothing matches by app_id or title prefix: honest `None`, not
    /// a fallback to "the first window" or any other silent guess.
    #[test]
    fn match_window_query_returns_none_when_nothing_matches() {
        let items = [(Some("foot-A"), Some("foot")), (Some("foot-B"), Some("foot"))];
        let got = match_window_query("does-not-exist", candidates(&items).into_iter());
        assert_eq!(got, None);
    }

    /// title 前綴回退 — no app_id matches at all, so the second pass finds a
    /// title that STARTS WITH the query.
    #[test]
    fn match_window_query_falls_back_to_title_prefix() {
        let items = [(Some("org.foo.bar"), Some("Bar — Editor")), (None, Some("Settings"))];
        let got = match_window_query("Bar", candidates(&items).into_iter());
        assert_eq!(got, Some((0, WindowMatch::TitlePrefix("Bar — Editor".to_string()))));
    }

    /// Priority ordering: when a query happens to be both some OTHER
    /// window's exact app_id and this window's title prefix, the exact
    /// app_id match wins — proving the two passes are genuinely ordered,
    /// not just "whichever happens to be found first in one combined scan".
    #[test]
    fn match_window_query_prefers_app_id_over_title_prefix_when_both_could_match() {
        let items = [
            (Some("something-else"), Some("Foo the app")), // title starts with "Foo"
            (Some("Foo"), Some("unrelated title")),         // exact app_id == "Foo"
        ];
        let got = match_window_query("Foo", candidates(&items).into_iter());
        assert_eq!(got, Some((1, WindowMatch::AppId("Foo".to_string()))));
    }

    /// A title-prefix match requires an actual prefix, not merely a
    /// substring — `starts_with`, not `contains` (this crate's own
    /// coding-convention #2: no unanchored `contains` for routing
    /// decisions).
    #[test]
    fn match_window_query_title_prefix_is_anchored_not_substring() {
        let items = [(None, Some("My Editor — untitled"))];
        // "Editor" is a substring of the title but not a PREFIX of it.
        let got = match_window_query("Editor", candidates(&items).into_iter());
        assert_eq!(got, None, "a mid-string substring must not count as a title-prefix hit");
    }

    /// Z-order tie-break (module doc): with two windows sharing the same
    /// app_id, the lowest index (bottom of `Space::elements()`'s
    /// bottom-to-top order) wins deterministically.
    #[test]
    fn match_window_query_ties_resolve_to_the_lowest_z_order_index() {
        let items = [(Some("foot"), Some("shell A")), (Some("foot"), Some("shell B"))];
        let got = match_window_query("foot", candidates(&items).into_iter());
        assert_eq!(got, Some((0, WindowMatch::AppId("foot".to_string()))));
    }
}
