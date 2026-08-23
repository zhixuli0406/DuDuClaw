// Step 8 — 完成 + quiet period. §B-1 row 8: "完成屏（dashboard 網址/QR、通道
// 綁定提示）＋quiet period" — terminal step. See `OobeStep::Finish`'s own doc
// comment in `oobe/mod.rs`: continuing from here (the bottom-nav button,
// relabeled "開始使用" for this one step — see `render.rs`'s `button_row`)
// marks the whole flow `completed` and swaps straight to Home.
//
// Summary list (task brief: "摘要已完成項清單") is read directly off
// `flow.selections()` — no separate view-model, this step has no state of
// its own to add. `summary_rows` below is deliberately still six rows after
// the `Theme` step (2026-08-20) was added — the pixel board
// (`commercial/design/duduclaw-os-oobe/Theme.dc.html`) does not add a
// seventh "外觀" row here, so this file stays faithful to that board rather
// than inventing one; the pick still fully takes effect (it just isn't
// re-stated in this particular summary card). This screen itself renders
// through the palette the operator picked (`flow.palette()`, same as every
// other OOBE screen) even though its own content doesn't mention that pick.
//
// ── Quiet period (task brief) ────────────────────────────────────────
// "完成後 Home 不彈任何引導/通知" is satisfied by this file (and
// `render.rs`'s Finish-step Continue handler) doing NOTHING beyond
// `completed = true` + `save_state` + swapping `view.oobe` to `None` — the
// research doc's own citation for this concept (§2, Windows 11's "quiet
// period": "禁止任何 app 自動彈 UI") is honored by the ABSENCE of a
// post-completion toast/tour/notification call, not by adding one and then
// suppressing it. There is nothing in this codebase yet that WOULD fire on
// an OOBE→Home transition (no toast/tour system wired to that event), so
// this is a real, load-bearing design choice worth naming explicitly here
// rather than a gap that happens to look like one.

use gpui::{div, img, prelude::*, px, Div, FontWeight};

use duduclaw_native_gui::theme;

use crate::home;
use crate::i18n::{t, t1, Key};
use crate::palette::ShellPalette;
use crate::oobe::widgets;
use crate::oobe::{OobeFlow, PrivacyToggle, TemplateChoice};

/// ICON-3 (2026-08-23): this step's header art is the ORIGAMI CAT at 72px
/// tall, not the 40×40 circular paw mark it was between 2026-08-21 and this
/// round. `OOBE-KeySteps.dc.html`'s 完成 artboard draws `cat-256.png` at
/// `height: 72px`, and the operator's ruling ④ settled the one open question
/// that board left ("72px 維持還是放大？") in favour of keeping 72.
///
/// Reuses `home::CAT_512` rather than adding a `cat-256` const of its own:
/// it is the same artwork at 2× the pixels (264×512 vs 132×256, both 33:64),
/// already embedded in this binary for the lockscreen watermark, and
/// oversampling 512→72 costs nothing at render time while a second
/// `include_bytes!` of the same picture would cost a duplicate copy in the
/// binary. Width is derived from the PNG's real aspect (264/512 × 72 ≈
/// 37.1), the same derivation `home::cat_hero` and `lockscreen::render::
/// cat_watermark` already do — gpui sizes an `img()` by the box you give it,
/// so passing only a height would stretch it.
const CAT_HEIGHT: f32 = 72.;
const CAT_WIDTH: f32 = 264. / 512. * CAT_HEIGHT;

pub(super) fn render(flow: &OobeFlow) -> Div {
    let s = flow.selections();
    let locale = flow.locale();
    let palette = flow.palette();

    let privacy_on_count = PrivacyToggle::ALL.into_iter().filter(|toggle| flow.privacy_toggle_on(*toggle)).count();
    let runtime_summary = if s.runtime_authorized {
        t(locale, Key::FinishRuntimeAuthorized).to_string()
    } else if s.runtime_deferred {
        t(locale, Key::FinishRuntimeDeferred).to_string()
    } else {
        t(locale, Key::FinishRuntimeNotSet).to_string()
    };
    let template_summary = match &s.template_choice {
        Some(TemplateChoice::Express) => t(locale, Key::FinishTemplatesExpress).to_string(),
        // `id` (e.g. "retail") stays as the persisted slug, not looked up
        // against a translated display name — `oobe::fake_data`'s template
        // titles are their own knowingly-untranslated stub this round (see
        // `steps::templates`'s own comment on `template_card`), so there is
        // no translated string to substitute here yet either.
        Some(TemplateChoice::Custom(id)) => t1(locale, Key::FinishTemplatesCustom, id),
        Some(TemplateChoice::Skipped) => t(locale, Key::FinishTemplatesSkipped).to_string(),
        None => t(locale, Key::FinishTemplatesNotChosen).to_string(),
    };

    let summary_rows: [(&'static str, String); 6] = [
        (t(locale, Key::FinishSummaryLanguage), s.language.label().to_string()),
        (t(locale, Key::FinishSummaryNetwork), s.network_ssid.clone().unwrap_or_else(|| t(locale, Key::FinishNetworkNotConnected).to_string())),
        (
            t(locale, Key::FinishSummaryAccount),
            if s.account_created { t(locale, Key::FinishAccountCreated).to_string() } else { t(locale, Key::FinishAccountNotCreated).to_string() },
        ),
        (t(locale, Key::FinishSummaryRuntime), runtime_summary),
        (
            t(locale, Key::FinishSummaryPrivacy),
            if privacy_on_count == 0 { t(locale, Key::FinishPrivacyAllOff).to_string() } else { t1(locale, Key::FinishPrivacyOnCount, &privacy_on_count.to_string()) },
        ),
        (t(locale, Key::FinishSummaryTemplates), template_summary),
    ];

    let mut rows = div().flex().flex_col().gap(px(8.));
    for (label, value) in summary_rows {
        rows = rows.child(summary_row(label, value, palette));
    }

    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(20.))
        .child(img(home::png(home::CAT_512)).w(px(CAT_WIDTH)).h(px(CAT_HEIGHT)))
        .child(widgets::title(t(locale, Key::FinishTitle), palette))
        .child(widgets::subtitle(t(locale, Key::FinishSubtitle), palette))
        .child(widgets::card(rows, palette))
}

fn summary_row(label: &'static str, value: String, palette: ShellPalette) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(palette.muted_foreground, 1.0)).child(label))
        .child(div().text_size(px(theme::TEXT_SM)).font_weight(FontWeight::MEDIUM).text_color(theme::alpha(palette.foreground, 1.0)).child(value))
}
