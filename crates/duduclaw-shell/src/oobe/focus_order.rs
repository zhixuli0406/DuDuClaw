// Per-step Tab/Shift-Tab focus order — WP-oobe-tab (2026-08-23).
//
// Pure data + pure functions ONLY (no gpui types), same discipline
// `state.rs`'s own header comment establishes for `OobeFlow` — this is the
// part of "which field does Tab move to next" that can be unit-tested
// without a live window. The gpui-facing half (resolving an `OobeFocusTarget`
// to a real `FocusHandle` and calling `window.focus(...)`) lives in
// `main.rs`'s `on_focus_next`/`on_focus_prev` action handlers — the only two
// call sites, since Tab/Shift-Tab are bound there (see that file's `actions!`/
// `bind_keys` list).
//
// Scope: only the OOBE steps that actually have a real typed `OobeTextField`
// as of this round — `AccountCreate` (name/password) and `Network` (the PSK
// prompt, shown only once a secured row is selected). Every other step's
// order is empty; `Privacy`'s four toggle switches and `Templates`' cards are
// deliberately NOT focus targets here — the task that added this scoped it to
// "email/password 兩個 OobeTextField、網路步驟有 Wi-Fi 相關欄位" specifically,
// not every clickable control on every step.

use super::OobeStep;

/// One Tab-reachable field, across every OOBE step that has any. Flat (not
/// nested per-step) so `focus_next`/`focus_prev` can treat "which field is
/// currently focused" as a single `Option<OobeFocusTarget>` regardless of
/// which step it belongs to — the caller (`main.rs`) already knows the
/// current step separately and passes it to `focus_order`/`focus_next`/
/// `focus_prev` explicitly, so a target from the WRONG step never gets
/// looked up inside another step's order (`focus_order` for a step other
/// than the target's own simply won't contain it, and `position` returns
/// `None` — treated the same as "nothing focused yet", not a panic).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OobeFocusTarget {
    /// `AccountCreate`'s first field — the operator's display name.
    AccountName,
    /// `AccountCreate`'s second field.
    AccountPassword,
    /// `Network`'s PSK prompt — the only focusable field on that step, and
    /// only actually rendered once a secured row is selected (see `steps::
    /// network::connect_panel`). Listing it here regardless is harmless:
    /// `main.rs`'s resolver looks up its `FocusHandle` from `ShellView.
    /// oobe_network_fields` unconditionally either way (that entity exists
    /// for the whole OOBE lifetime, see `NetworkFields::new`'s own doc
    /// comment) — Tab simply has nothing else to cycle to on this step.
    NetworkPsk,
}

/// The ordered, Tab-reachable field list for one step. `&'static [_]` (not a
/// `Vec`) — this is fixed data, not something built per call.
pub(crate) fn focus_order(step: OobeStep) -> &'static [OobeFocusTarget] {
    match step {
        OobeStep::AccountCreate => &[OobeFocusTarget::AccountName, OobeFocusTarget::AccountPassword],
        OobeStep::Network => &[OobeFocusTarget::NetworkPsk],
        _ => &[],
    }
}

/// Tab: the field after `current` in this step's order, wrapping from the
/// last back to the first. `current = None` (nothing in THIS step's order
/// currently focused — including "focus is on some other step's field, or
/// on the shell root") starts at the first field. `None` back out means the
/// step has no focusable field at all — never a panic.
pub(crate) fn focus_next(step: OobeStep, current: Option<OobeFocusTarget>) -> Option<OobeFocusTarget> {
    step_relative(step, current, 1)
}

/// Shift-Tab: the mirror image of `focus_next` — the field BEFORE `current`,
/// wrapping from the first back to the last.
pub(crate) fn focus_prev(step: OobeStep, current: Option<OobeFocusTarget>) -> Option<OobeFocusTarget> {
    step_relative(step, current, -1)
}

/// Shared body for `focus_next`/`focus_prev` — `delta` is `1`/`-1`, wrapped
/// via `rem_euclid` so it never underflows on the `-1` case regardless of
/// `order.len()`.
fn step_relative(step: OobeStep, current: Option<OobeFocusTarget>, delta: i64) -> Option<OobeFocusTarget> {
    let order = focus_order(step);
    if order.is_empty() {
        return None;
    }
    let current_index = current.and_then(|c| order.iter().position(|t| *t == c));
    let next_index = match current_index {
        Some(i) => (i as i64 + delta).rem_euclid(order.len() as i64) as usize,
        // Nothing (recognizable) currently focused — Tab starts at the
        // first field either direction; there is no "before the start" to
        // distinguish from "after the end" when there was no start.
        None => 0,
    };
    Some(order[next_index])
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── focus_order: the static per-step list itself ──────────────────

    #[test]
    fn account_create_order_is_name_then_password() {
        assert_eq!(
            focus_order(OobeStep::AccountCreate),
            &[OobeFocusTarget::AccountName, OobeFocusTarget::AccountPassword]
        );
    }

    #[test]
    fn network_order_is_just_the_psk_field() {
        assert_eq!(focus_order(OobeStep::Network), &[OobeFocusTarget::NetworkPsk]);
    }

    #[test]
    fn every_other_step_has_no_focusable_field() {
        for step in OobeStep::ALL {
            if matches!(step, OobeStep::AccountCreate | OobeStep::Network) {
                continue;
            }
            assert!(focus_order(step).is_empty(), "{step:?} must have an empty focus order");
        }
    }

    // ── focus_next / focus_prev: two-field step (AccountCreate) ───────

    #[test]
    fn tab_from_name_goes_to_password() {
        assert_eq!(
            focus_next(OobeStep::AccountCreate, Some(OobeFocusTarget::AccountName)),
            Some(OobeFocusTarget::AccountPassword)
        );
    }

    #[test]
    fn tab_wraps_from_the_last_field_back_to_the_first() {
        assert_eq!(
            focus_next(OobeStep::AccountCreate, Some(OobeFocusTarget::AccountPassword)),
            Some(OobeFocusTarget::AccountName)
        );
    }

    #[test]
    fn shift_tab_from_password_goes_to_name() {
        assert_eq!(
            focus_prev(OobeStep::AccountCreate, Some(OobeFocusTarget::AccountPassword)),
            Some(OobeFocusTarget::AccountName)
        );
    }

    #[test]
    fn shift_tab_wraps_from_the_first_field_back_to_the_last() {
        assert_eq!(
            focus_prev(OobeStep::AccountCreate, Some(OobeFocusTarget::AccountName)),
            Some(OobeFocusTarget::AccountPassword)
        );
    }

    #[test]
    fn tab_with_nothing_focused_yet_starts_at_the_first_field() {
        assert_eq!(focus_next(OobeStep::AccountCreate, None), Some(OobeFocusTarget::AccountName));
    }

    #[test]
    fn shift_tab_with_nothing_focused_yet_starts_at_the_first_field_too() {
        // No "before the start" to distinguish from "after the end" when
        // there was no start — both directions land on the first field.
        assert_eq!(focus_prev(OobeStep::AccountCreate, None), Some(OobeFocusTarget::AccountName));
    }

    // ── focus_next / focus_prev: single-field step (Network) ──────────

    #[test]
    fn tab_on_a_single_field_step_is_a_noop() {
        assert_eq!(focus_next(OobeStep::Network, Some(OobeFocusTarget::NetworkPsk)), Some(OobeFocusTarget::NetworkPsk));
    }

    #[test]
    fn shift_tab_on_a_single_field_step_is_also_a_noop() {
        assert_eq!(focus_prev(OobeStep::Network, Some(OobeFocusTarget::NetworkPsk)), Some(OobeFocusTarget::NetworkPsk));
    }

    // ── focus_next / focus_prev: zero-field steps never panic ─────────

    #[test]
    fn tab_and_shift_tab_on_every_zero_field_step_return_none_without_panicking() {
        for step in OobeStep::ALL {
            if matches!(step, OobeStep::AccountCreate | OobeStep::Network) {
                continue;
            }
            assert_eq!(focus_next(step, None), None, "{step:?}");
            assert_eq!(focus_prev(step, None), None, "{step:?}");
            // A target that (by construction) can never belong to this
            // step's own order — same "unrecognized current" fallback the
            // `None` case above exercises, exercised here via `Some(_)`
            // instead to prove the `position()` miss path is equally panic
            // -free, not just the `current = None` path.
            assert_eq!(focus_next(step, Some(OobeFocusTarget::AccountName)), None, "{step:?}");
        }
    }

    // ── a target foreign to the current step's order degrades cleanly ──

    #[test]
    fn a_current_target_foreign_to_this_step_is_treated_like_nothing_focused() {
        // e.g. focus is still logically on the Network step's PSK field
        // while `focus_order` is asked about AccountCreate (should not
        // happen in practice — `main.rs` always passes the CURRENT step's
        // own order — but the fallback must still be sane, not a panic or
        // an out-of-bounds index).
        assert_eq!(
            focus_next(OobeStep::AccountCreate, Some(OobeFocusTarget::NetworkPsk)),
            Some(OobeFocusTarget::AccountName)
        );
    }
}
