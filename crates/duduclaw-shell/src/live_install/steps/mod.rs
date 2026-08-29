// Y20-P2 (2026-08-29) — per-step content dispatcher, mirrors `oobe::steps`'s
// own dispatcher shape (see that module's own header comment) at this
// flow's much smaller scope (4 steps at P2, 6 as of installer-settings-
// integration WP1 below — still well under OOBE's own ten). `render.rs`'s
// frame owns the chrome (background, progress dots, bottom nav); each
// module below owns only the middle content area for one step.
//
// Y20-P3 (2026-08-29): `disk_select`/`confirm` now need `cx` too (a
// background-thread `lsblk` scan click, a checkbox toggle click — both real
// `cx.listener` closures, same reason `language::render` already needed
// it). `progress` stays `cx`-free — it is pure rendering of `LiveInstallFlow
// ::install()`, driven by `install_runner` rather than any click of its
// own (see that step's own header comment).
//
// Installer-settings-integration WP1 (2026-08-29): two new steps,
// `account`/`theme` (see each module's own header comment). This dispatcher
// now also threads a `fields: &AccountFields` parameter through — ONLY
// `account::render` reads it (the same two real text-input entities
// `main.rs`'s `ShellView::live_install_account_fields` owns, see that
// field's own doc comment for why it's a separate instance from OOBE's
// `oobe_account_fields`); every other arm ignores it, same "one extra
// parameter every step signature carries, most ignore" shape `oobe::steps::
// render`'s own dispatcher already established for `AccountFields`/
// `NetworkFields` there.
//
// Installer-settings-integration WP3 (2026-08-29): a third new step,
// `network` (see that module's own header comment). Rather than fold its
// field bundle into the existing `fields: &AccountFields` parameter (the two
// bundles have unrelated shapes — a name+password pair vs. an ssid+psk pair
// — and unrelated step ownership), this dispatcher grows a SECOND ignored-
// by-most parameter, `wifi_fields: &LiveWifiFields`, following the exact
// same "most arms ignore it" precedent `fields` itself set a moment earlier.

mod account;
mod confirm;
mod disk_select;
mod language;
mod network;
mod progress;
mod theme;

use gpui::{Context, Div};

use super::{LiveInstallFlow, LiveInstallStep};
use crate::oobe::widgets::{AccountFields, LiveWifiFields};
use crate::ShellView;

pub(super) fn render(
    step: LiveInstallStep,
    flow: &LiveInstallFlow,
    fields: &AccountFields,
    wifi_fields: &LiveWifiFields,
    cx: &mut Context<ShellView>,
) -> Div {
    match step {
        LiveInstallStep::Language => language::render(flow, cx),
        LiveInstallStep::Network => network::render(flow, wifi_fields, cx),
        LiveInstallStep::Account => account::render(flow, fields, cx),
        LiveInstallStep::Theme => theme::render(flow, cx),
        LiveInstallStep::DiskSelect => disk_select::render(flow, cx),
        LiveInstallStep::Confirm => confirm::render(flow, cx),
        LiveInstallStep::Progress => progress::render(flow),
    }
}
