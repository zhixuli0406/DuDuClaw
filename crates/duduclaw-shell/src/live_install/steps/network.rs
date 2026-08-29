// Installer-settings-integration WP3 (2026-08-29,
// `commercial/docs/DESIGN-installer-settings-integration-2026-08.md` §5) —
// the live-installer's own Wi-Fi step. Visually mirrors `steps::account`
// (title/subtitle, two labeled `OobeTextField`s, an unconditionally-present
// status slot) — same reasoning: a thin "UI glue" layer over
// `LiveInstallFlow`'s own Network fields, no I/O of any kind.
//
// ── Why plan (b) — collect, don't connect (read before "fixing" it) ───────
// The design doc's §5 lays out two roads for live-install Wi-Fi:
//   (a) Real live connectivity: add `iwd`/wireless-regdb-static/duduclaw-
//       network-config back into the live image and scan/connect for real,
//       the same way OOBE's own `Network` step (`oobe::steps::network`)
//       does post-boot.
//   (b) Pure collection: this step only ever holds a typed SSID + passphrase
//       in `LiveInstallState`; `install_runner` (a later round, not this
//       one) serializes them into a THIRD scratch file the TARGET system's
//       own first-boot `iwd` setup consumes — the exact "pending file +
//       first-boot landing" shape §4 already established for the `Account`
//       step's password.
// (a) is explicitly rejected by the design doc: the live image "刻意裁掉"
// (deliberately strips) the entire `iwd`/D-Bus stack
// (`duduclaw-image-live.bb:84-92`), and rebuilding that stack inside a
// squashfs live session is unverified, high-risk work whose only payoff is
// letting the OPERATOR watch a live connection succeed during install — a
// nice-to-have, not a requirement, since the install media itself already
// carries every package it needs (no download ever happens during a live
// install). (b) delivers the actual requirement — "the target machine wakes
// up already on Wi-Fi" — without touching any of that stack. So THIS file
// has, and must never grow:
//   - NO `lsblk`-style scan, no nearby-network list, no signal bars.
//   - NO connect attempt, no `Connecting`/`Connected`/`Failed` states.
//   - NO gateway round trip, no `iwd`/D-Bus call, no background thread at
//     all — every write this step's collaborators make to `LiveInstallFlow`
//     is a synchronous, local-only state write, same as `steps::account`.
//
// ── The one thing this step must get right that `Account` doesn't: OPTIONAL
// submission ────────────────────────────────────────────────────────────
// `Account` is a required gate — `LiveInstallFlow::can_advance` for that
// step is `false` until BOTH fields are set. `Network` is the opposite: an
// operator with nothing to type (wired network, or "I'll connect later from
// Settings") must be able to move on with both fields left empty — see
// `LiveInstallStep::Network`'s own doc comment in `state.rs`. So this step's
// own status slot (`status_line` below) is never a validation floor the way
// `Account`'s `AccountValidationError` is; it only ever fires for a
// PARTIALLY-typed, genuinely inconsistent submission (`NetworkError`'s own
// doc comment in `state.rs` enumerates the three cases) — click-time
// validation for all three lives in `render.rs`'s own
// `validate_and_set_wifi`, not here, same "validate at Continue, not here"
// split `steps::account`'s own header comment documents for `Account`.
//
// A muted, always-present "留空可跳過" (this step can be skipped) hint line
// sits below the two fields so an operator who has never used this UI
// pattern before does not have to guess whether Continue with empty fields
// is a mistake waiting to happen — the ONE piece of copy this step has that
// `Account`'s equivalent screen doesn't need, precisely because `Account`
// has no legal empty outcome to reassure the operator about.

use gpui::{div, prelude::*, px, Context, Div, Entity};

use duduclaw_native_gui::theme;

use crate::i18n::{t, Key, Locale};
use crate::oobe::widgets::{self, LiveWifiFields, OobeTextField};
use crate::palette::ShellPalette;
use crate::ShellView;

use super::super::{LiveInstallFlow, NetworkError};

pub(super) fn render(flow: &LiveInstallFlow, fields: &LiveWifiFields, _cx: &mut Context<ShellView>) -> Div {
    let locale = flow.locale();
    let palette = flow.palette();

    let body = div()
        .flex()
        .flex_col()
        .gap(px(14.))
        .child(labeled_field(t(locale, Key::LiveWifiSsidLabel), fields.ssid.clone(), palette))
        .child(labeled_field(t(locale, Key::LiveWifiPskLabel), fields.psk.clone(), palette))
        // Always present, regardless of `network_error` — this is a plain
        // reassurance line, not a status slot that would otherwise collapse
        // to zero height (contrast `status_line` below, which reserves its
        // own line the W7-2 way for the SAME reason).
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(palette.muted_foreground, 1.0)).child(t(locale, Key::LiveWifiOptionalHint)))
        // W7-2 discipline (see `steps::account::status_line`'s own doc
        // comment, and this file's own header comment above): appended
        // unconditionally, as a top-level statement, never inside a
        // conditional — an omitted-when-empty status line would shift the
        // shared bottom-nav Continue button's Y position the instant an
        // error first appears.
        .child(status_line(flow.network_error(), locale, palette));

    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(20.))
        .child(widgets::title(t(locale, Key::LiveWifiTitle), palette))
        .child(widgets::subtitle(t(locale, Key::LiveWifiSubtitle), palette))
        .child(widgets::card(body, palette))
}

/// Same label-over-field layout `steps::account::labeled_field` establishes
/// (re-derived, not shared — that fn is private to `steps::account`).
fn labeled_field(label: &'static str, field: Entity<OobeTextField>, palette: ShellPalette) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(palette.muted_foreground, 1.0)).child(label))
        .child(field)
}

/// The step's status slot — unconditionally present in `render`'s `body`
/// above, same W7-2 (`OOBE-acct-stuck`, 2026-08-24) layout-stability
/// discipline `steps::account::status_line`'s own doc comment documents.
/// Re-derived locally rather than shared with that fn because this step's
/// error vocabulary is a completely different enum (`NetworkError`, not
/// `AccountError`) with no case in common.
fn status_line(error: Option<NetworkError>, locale: Locale, palette: ShellPalette) -> Div {
    let (text, alpha) = match error {
        Some(NetworkError::SsidMissingWithPsk) => (t(locale, Key::LiveWifiErrSsidMissing), 1.0),
        Some(NetworkError::SsidTooLong) => (t(locale, Key::LiveWifiErrSsidTooLong), 1.0),
        Some(NetworkError::PskLengthInvalid) => (t(locale, Key::LiveWifiErrPskLength), 1.0),
        // No error yet — a non-breaking space placeholder reserves the SAME
        // line height a real message would (an empty string child can
        // collapse a line's height to zero, defeating the whole point of
        // reserving space).
        None => ("\u{a0}", 0.0),
    };
    div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(palette.destructive, alpha)).child(text)
}
