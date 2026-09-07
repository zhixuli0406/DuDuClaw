// Y20-P3 (2026-08-29) — real Progress step, replacing the P2 static-0%
// placeholder. Renders whatever `LiveInstallFlow::install()` currently says
// — `install_runner::start_install` (kicked off from Confirm's own
// bottom-nav action, see that fn's own header comment) is the ONLY writer
// of this state; this file is pure rendering, no I/O of its own, same
// "state machine vs. the I/O that drives it" split `steps::disk_select`'s
// own scan already follows.
//
// `Running { percent: Some(_), .. }` paints the determinate fill-bar shape
// `overlay/controlcenter.rs`'s own volume track already establishes
// (`.relative()` on the track + `.absolute()` + `.w(relative(fraction))` on
// the fill — see that file's own `volume_slider_row`/`track` code for the
// pattern this borrows); `percent: None` renders the SAME bar shape at a
// fixed low fill with a "preparing" label instead of animating a fake sweep
// — an honest "we don't know the real fraction yet", not a fabricated
// animation.
//
// Y20-P5 (2026-09-04) bug fix: `percent: None` used to render "進度未知
// （無 pv）· Progress unknown (no pv) — {status}" — misleading on two
// counts, per a QEMU walkthrough (screens/07-installing-100.png captured the
// LATER 100% state; the early state was observed still at 1% ~90s in,
// i.e. genuinely still preparing, not stuck without `pv`). First,
// `duduclaw-os-install.sh`'s own image DOES ship `pv` (see that script's
// header comment on its `DUDUCLAW_PROGRESS` wire format) — the wording
// asserted a specific, usually-false diagnosis. Second, and more
// fundamentally: `install_runner.rs`'s `InstallEvent`/`parse_progress_line`
// (see that file's own header comment) has no THIRD event distinguishing
// "haven't seen a sample yet" from "this build's `pv` genuinely isn't
// there" — every non-`DUDUCLAW_PROGRESS:` line, including total silence so
// far, collapses to the exact same `percent: None`. Since the runner
// genuinely cannot tell the two apart (this file's own task brief: "若
// runner 分不出來，一律準備中，拿掉無 pv 措辭"), `running_label` below
// always renders a locale-appropriate "preparing" label for `None` and never
// mentions `pv` at all — honest about what this step actually knows (no
// sample has arrived) instead of asserting a cause it can't verify.
//
// Y20-P6 (2026-09-05): a review finding on the same round's disk-picker fix
// also flagged this label as one of two new strings still hardcoded as a
// bilingual "zh · en" literal rather than routed through `crate::i18n` like
// the sibling `steps::network` (`LiveWifi*`) step's own labels — so the
// `None` branch now reads `Key::LiveInstallProgressPreparing` via `t1`.
// Scoped to only THIS branch: the `Some(p)` branch's `"{p}% — {status}"` and
// every OTHER string in this file (`idle_body`/`done_body`/`failed_body`'s
// own bilingual literals) predate this round and are a separate, larger
// retrofit not in scope here.
//
// No separate "重新開機" button lives in THIS file's own card — that action
// is the shared bottom-nav slot, relabeled for this step (see `render.rs`'s
// own header comment for why: same "one shared action slot, not a second
// step-owned button" reasoning `confirm.rs`'s own header comment gives for
// "開始安裝"). This file's `Done` body only shows the completion TEXT.

use gpui::{div, prelude::*, px, relative, Div};

use duduclaw_native_gui::theme;

use crate::i18n::{t1, Key, Locale};
use crate::oobe::widgets;
use crate::palette::ShellPalette;

use super::super::{InstallState, LiveInstallFlow};

pub(super) fn render(flow: &LiveInstallFlow) -> Div {
    let palette = flow.palette();
    let locale = flow.locale();

    let (headline, body): (&'static str, Div) = match flow.install() {
        InstallState::Idle => ("準備安裝 · Preparing", idle_body(palette)),
        InstallState::Running { percent, status } => ("安裝進度 · Installing", running_body(locale, *percent, status, palette)),
        InstallState::Done => ("安裝完成 · Installation complete", done_body(palette)),
        InstallState::Failed(message) => ("安裝失敗 · Installation failed", failed_body(message, palette)),
    };

    div().flex().flex_col().items_center().gap(px(20.)).child(widgets::title(headline, palette)).child(widgets::card(body, palette))
}

/// The determinate/indeterminate fill-bar track. `percent: None` renders a
/// small fixed fill (never animated — this crate has no per-frame render
/// hook cheap enough to sweep a fake bar, and a static-but-honest "we don't
/// know yet" reads better than a bar that looks stuck at a fabricated
/// value).
fn progress_bar(percent: Option<u8>, palette: ShellPalette) -> Div {
    let fraction = percent.map(|p| f32::from(p.min(100)) / 100.0).unwrap_or(0.08);
    div()
        .relative()
        .w_full()
        .h(px(10.))
        .rounded(px(10.))
        .bg(theme::alpha(palette.muted, 1.0))
        .child(div().absolute().left(px(0.)).top(px(0.)).bottom(px(0.)).w(relative(fraction)).rounded(px(10.)).bg(theme::alpha(palette.brand, 1.0)))
}

fn status_line(text: &str, palette: ShellPalette) -> Div {
    // Bug fix (DESIGN-installer-settings-integration-2026-08.md §6): same
    // undefined-width flex_col-child overflow as `confirm.rs`'s
    // `warning_banner` — this div is a direct child of each body's flex_col
    // with no explicit width, and `running_body` feeds it an arbitrary
    // `duduclaw-os-install.sh` log line (the single most likely line in this
    // whole step to overflow). `.w_full()` gives it a real width to wrap
    // inside, matching `progress_bar` above (which already has `.w_full()`).
    div().w_full().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(palette.muted_foreground, 1.0)).child(text.to_string())
}

fn idle_body(palette: ShellPalette) -> Div {
    div().flex().flex_col().gap(px(8.)).child(progress_bar(None, palette)).child(status_line("等待開始… · Waiting to start…", palette))
}

fn running_body(locale: Locale, percent: Option<u8>, status: &str, palette: ShellPalette) -> Div {
    div().flex().flex_col().gap(px(8.)).child(progress_bar(percent, palette)).child(status_line(&running_label(locale, percent, status), palette))
}

/// Pure label builder — pulled out of `running_body` specifically so this
/// round's bug fix (see this file's own header comment, "Y20-P5") is
/// unit-testable without a live `gpui` window. `percent: Some(p)` renders
/// exactly as before ("`{p}% — {status}`", still a plain literal — out of
/// this round's i18n scope, see the header comment's "Y20-P6" section);
/// `percent: None` no longer asserts "no pv" (see the header comment for
/// why) — it renders `Key::LiveInstallProgressPreparing` via `t1`, which
/// mirrors the `Some` arm's own "prefix, em dash, status" shape in each
/// locale's own wording.
fn running_label(locale: Locale, percent: Option<u8>, status: &str) -> String {
    match percent {
        Some(p) => format!("{p}% — {status}"),
        None => t1(locale, Key::LiveInstallProgressPreparing, status),
    }
}

fn done_body(palette: ShellPalette) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(12.))
        .child(progress_bar(Some(100), palette))
        .child(status_line("安裝完成，請移除安裝媒介後重新開機 · Done — remove the install media, then reboot", palette))
}

fn failed_body(message: &str, palette: ShellPalette) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(
            // Bug fix (DESIGN-installer-settings-integration-2026-08.md §6): same
            // class as `status_line` above — `message` is an arbitrary install
            // failure reason with no length bound, direct flex_col child with no
            // explicit width otherwise.
            div()
                .w_full()
                .text_size(px(theme::TEXT_SM))
                .text_color(theme::alpha(palette.destructive, 1.0))
                .child(format!("安裝失敗 · Install failed：{message}")),
        )
        .child(status_line("請返回上一步重試 · Go back and try again", palette))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_label_with_a_percent_renders_the_percentage_and_status() {
        assert_eq!(running_label(Locale::ZhTw, Some(42), "寫入中"), "42% — 寫入中");
    }

    /// Y20-P5 bug fix: no percentage sample yet must render "準備中…" —
    /// NOT the old "無 pv" wording, which asserted a diagnosis
    /// (`install_runner.rs` can't actually tell "no sample yet" apart from
    /// "pv genuinely absent" — see this file's own header comment) that was
    /// usually false besides (the installer image does ship `pv`).
    #[test]
    fn running_label_with_no_percent_renders_preparing_not_no_pv() {
        let label = running_label(Locale::ZhTw, None, "準備安裝…");
        assert_eq!(label, "準備中… — 準備安裝…");
        assert!(!label.contains("pv"), "must never assert the no-pv diagnosis install_runner.rs cannot actually make");
    }

    #[test]
    fn running_label_with_no_percent_still_carries_the_live_status_line() {
        // The status line is the one thing that keeps changing while
        // `percent` is still `None` (log lines pumped from the installer's
        // stdout/stderr) — it must still show up verbatim, not get dropped
        // in favor of the static "準備中…" prefix alone.
        assert_eq!(running_label(Locale::ZhTw, None, "正在寫入映像到 /dev/vda..."), "準備中… — 正在寫入映像到 /dev/vda...");
    }

    /// Y20-P6: the label now genuinely varies by locale (it was a fixed
    /// bilingual literal before this round) — pin the English reading too,
    /// so a future edit that only updates the zh-TW catalog string can't
    /// silently regress the other two locales `i18n/tests.rs`'s own
    /// completeness test wouldn't catch (that test only checks
    /// non-emptiness, not wording).
    #[test]
    fn running_label_with_no_percent_reads_correctly_in_english() {
        assert_eq!(running_label(Locale::En, None, "Preparing to install…"), "Preparing… — Preparing to install…");
    }
}
