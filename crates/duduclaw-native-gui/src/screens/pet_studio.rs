// WP-S6b2-O (S6b 第二波, 2026-08-21) — "桌寵工作室" (`commercial/design/
// duduclaw-s6-form-pages/PetStudio.dc.html`, B21). The canvas's own header
// comment already frames this as "a standalone utility window — deliberately
// smaller than the main app frame; reached from the mascot's right-click
// menu or the menu bar, not the 35-item app sidebar" — matching this task's
// own "無側欄（畫布拍板豁免）" instruction: no `nav.rs` entry, self-attached
// in `screens/shell.rs` only, reachable via
// `DUDUCLAW_NATIVE_GUI_DEBUG_PAGE=petStudio`.
//
// ── There is NO RPC or bridge of any kind this crate can reach for pet
// features — grep-verified, not assumed ──────────────────────────────────
// `web/src/pages/PetStudioPage.tsx` never calls a gateway WS method at all;
// every action goes through `web/src/lib/pet.ts`'s `invoke(cmd, args)`
// wrapper over `window.__TAURI__.core.invoke`, i.e. Tauri IPC commands
// (`pet_list` / `pet_generate` / `pet_activate` / `pet_remove` / `pet_model_
// status` / `pet_model_download` / …, registered in `src-tauri/src/main.rs`
// and implemented in `src-tauri/src/pet_gen.rs`). `duduclaw-native-gui` is a
// completely separate binary/runtime — a gpui app, not a Tauri webview shell
// — with no `window.__TAURI__` global, no IPC channel to `src-tauri`, and no
// access to its local pet-pack filesystem state. A crate-wide grep for
// `"pet."` inside `crates/duduclaw-gateway/src/handlers.rs`'s dispatch table
// (the same table every other page in this batch cites line numbers from)
// returns zero matches: no `pet.*` WS RPC method exists anywhere the
// gateway could expose to this crate even in principle. This is therefore
// the one page in this whole S6 pass with genuinely NOTHING to call — not a
// scope cut, a structural gap between two different app runtimes.
//
// ── What this page renders instead ────────────────────────────────────
// Per this task's own brief ("預覽用 canvas 繪製示意...原生版誠實靜態示
// 意＋註明"), both canvas regions are still built — "建立新桌寵" (editor) and
// "我的桌寵" (gallery) — but every interactive element is either a genuinely
// harmless local-only control (the pet-name `TextField`, real typing, zero
// side effects — same "an input with no write consequence is safe to make
// live" reasoning `screens::widget_composer.rs`/`screens::identity.rs`
// already establish) or a disabled/static stand-in (upload box, style
// toggles, 生成/啟用 buttons — this crate has no native file-picker
// dependency at all, so even the "choose a photo" step could not be wired
// even if the generation step were). The 桌面即時預覽/去背預覽 illustrations
// are `gpui::canvas`-painted generic pet silhouettes (a rounded body + face
// + two eye dots, no photo data of any kind), explicitly an illustration of
// "what a pet would look like here", never a claim that a real cutout ran.
// The "我的桌寵" gallery — unlike the editor's admittedly-fake preview
// shapes — does NOT reproduce the canvas's three named example rows (小杜/
// 綠豆/胖橘) as if they were real pets: showing specific named entities with
// an "使用中" status when no such data could ever exist here would be
// exactly the fabricated-record problem this project's data conventions
// forbid (`screens::identity.rs`'s "no invented 18 位 count" precedent,
// `screens::world.rs`'s "no fabricated speech-bubble text"). It renders an
// honest empty state inside the same card shell instead.

use gpui::{div, prelude::*, px, Context, Div, Entity, Global, Stateful};

use crate::i18n::{self, Locale};
use crate::mds_gpui::{button, empty_state, ButtonVariant};
use crate::text_field::TextField;
use crate::theme;
use crate::RootView;

// ── State — one live field (pet name), nothing else has anywhere to write
// to. No `maybe_fetch`/RPC plumbing at all: there is nothing to fetch. ────

pub struct PetStudioState {
    name: Entity<TextField>,
}

impl PetStudioState {
    /// See `screens::widget_composer.rs::WidgetComposerState::new`'s own doc
    /// comment for why `locale`-at-construction-time (not re-read later) is
    /// an accepted, pre-existing limitation in this crate, not a new one.
    fn new(cx: &mut gpui::App, locale: Locale) -> Self {
        Self { name: TextField::new(cx, i18n::t(locale, "petStudio.namePlaceholder"), false, "") }
    }
}

impl Global for PetStudioState {}

fn ensure_state(locale: Locale, cx: &mut Context<RootView>) {
    if !cx.has_global::<PetStudioState>() {
        let state = PetStudioState::new(cx, locale);
        cx.set_global(state);
    }
}

// ── Card shell (same recipe `screens::widget_composer.rs::settings_card`
// uses — a per-page-local copy, not a cross-module import, per this crate's
// established "each settings-panel page keeps its own tiny card shell"
// precedent). ──────────────────────────────────────────────────────────

fn card() -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2p5()
        .p_4()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .shadow(theme::surface_shadow())
}

fn region_label(locale: Locale, key: &str) -> Div {
    div().text_size(px(11.)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, key))
}

// ── Static toggle pill — a per-page-local copy of `screens::governance.rs::
// static_toggle` (same visual recipe: a non-interactive knob for a
// decision-class control this pass assembles but does not wire). NOT
// imported from `governance.rs` — `governance.rs`'s own doc comment already
// documents `security.rs` keeping its own copy rather than importing across
// unrelated design-authority batches, and this page is a third, equally
// unrelated batch (B21, not GovernanceShell). ─────────────────────────────
fn static_toggle(knob_right: bool) -> Div {
    div()
        .relative()
        .w(px(32.))
        .h(px(19.))
        .flex_shrink_0()
        .rounded_full()
        .bg(if knob_right { theme::alpha(theme::BRAND, 1.0) } else { theme::alpha(theme::MUTED, 0.6) })
        .child(div().absolute().top(px(2.)).left(if knob_right { px(15.) } else { px(2.) }).size(px(15.)).rounded_full().bg(theme::alpha(0xffffff, 0.95)))
}

fn toggle_row(locale: Locale, title_key: &str, desc_key: &str, knob_right: bool) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            div()
                .flex()
                .flex_col()
                .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, title_key)))
                .child(div().text_size(px(10.5)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, desc_key))),
        )
        .child(static_toggle(knob_right))
}

// ── Illustrative pet silhouette — `gpui::canvas` + `window.paint_quad`
// (same "spike 配方" `screens/reports.rs::cost_bar_chart` /
// `screens/widget_composer.rs::preview_bar` already establish: solid
// rounded quads, no `PathBuilder` needed). A rounded body + a lighter face
// oval + two dark eye dots — generic and explicitly decorative, never a
// stand-in for a real cutout (this page has no photo data of any kind). ───

fn pet_silhouette(width: f32, height: f32) -> Div {
    div().w(px(width)).h(px(height)).child(
        gpui::canvas(
            move |_bounds, _window, _cx| {},
            move |bounds, _prepaint, window, _cx| {
                let body_w = width * 0.62;
                let body_h = height * 0.82;
                let body_x = (width - body_w) / 2.0;
                let body_y = height - body_h;
                let body_bounds = gpui::Bounds::new(bounds.origin + gpui::point(px(body_x), px(body_y)), gpui::size(px(body_w), px(body_h)));
                window.paint_quad(gpui::quad(body_bounds, px((body_w.min(body_h)) * 0.22), theme::alpha(0xe3966a, 1.0), px(0.), gpui::transparent_black(), gpui::BorderStyle::default()));

                let face_w = body_w * 0.46;
                let face_h = body_h * 0.4;
                let face_x = body_x + (body_w - face_w) / 2.0;
                let face_y = body_y + body_h * 0.14;
                let face_bounds = gpui::Bounds::new(bounds.origin + gpui::point(px(face_x), px(face_y)), gpui::size(px(face_w), px(face_h)));
                window.paint_quad(gpui::quad(face_bounds, px(face_h * 0.5), theme::alpha(0xfbe3c4, 1.0), px(0.), gpui::transparent_black(), gpui::BorderStyle::default()));

                let eye = (face_w * 0.14).max(2.0);
                let eye_y = face_y + face_h * 0.28;
                for dx in [face_w * 0.22, face_w * 0.64] {
                    let eye_bounds = gpui::Bounds::new(bounds.origin + gpui::point(px(face_x + dx), px(eye_y)), gpui::size(px(eye), px(eye)));
                    window.paint_quad(gpui::quad(eye_bounds, px(eye / 2.0), theme::alpha(0x2b1c14, 1.0), px(0.), gpui::transparent_black(), gpui::BorderStyle::default()));
                }
            },
        )
        .size_full(),
    )
}

// ── Editor region ("建立新桌寵") ───────────────────────────────────────────

// Fixed square boxes, not a CSS `aspect-ratio` (no precedent for that
// `Styled` method existing on this crate's pinned gpui rev — every other
// square swatch in this crate uses a fixed `.size(px(N.))` instead, e.g.
// `catalog_common::initial_avatar`).
const PHOTO_BOX: f32 = 120.0;

fn upload_box() -> Div {
    div()
        .size(px(PHOTO_BOX))
        .rounded(px(theme::RADIUS_LG))
        .border_1()
        .border_color(theme::surface_border())
        .bg(theme::alpha(theme::MUTED, 0.3))
        .flex()
        .items_center()
        .justify_center()
        .child(div().text_size(px(10.5)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child("+"))
}

fn cutout_box() -> Div {
    div()
        .size(px(PHOTO_BOX))
        .rounded(px(theme::RADIUS_LG))
        .border_1()
        .border_color(theme::surface_border())
        .bg(theme::alpha(theme::MUTED, 0.15))
        .flex()
        .items_center()
        .justify_center()
        .child(pet_silhouette(96., 96.))
}

fn editor_left(locale: Locale, name_entity: Entity<TextField>) -> Div {
    card()
        .child(div().flex().gap_2p5().child(upload_box()).child(cutout_box()))
        .child(div().flex().flex_col().gap_2().child(name_entity).child(toggle_row(locale, "petStudio.pixelLabel", "petStudio.pixelHint", true)).child(toggle_row(locale, "petStudio.externalLabel", "petStudio.externalHint", false)))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .pt_2p5()
                .border_t_1()
                .border_color(theme::surface_border())
                .child(div().text_size(px(10.5)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "petStudio.engineLabel")))
                .child(button("pet-studio-generate", i18n::t(locale, "petStudio.generate"), ButtonVariant::Primary, true, None, |_ev, _window, _app| {})),
        )
}

fn editor_right(locale: Locale) -> Div {
    let frame = div()
        .flex_1()
        .min_h(px(180.))
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(0xc9d7ee, 1.0))
        .relative()
        .child(div().absolute().top(px(10.)).left(px(10.)).w(px(90.)).h(px(58.)).rounded(px(6.)).bg(theme::alpha(0xffffff, 0.5)))
        .child(div().absolute().bottom(px(14.)).right(px(18.)).child(pet_silhouette(60., 62.)));
    card()
        .child(div().text_size(px(11.)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "petStudio.livePreviewLabel")))
        .child(frame)
        .child(div().text_size(px(10.5)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "petStudio.livePreviewHint")))
        .child(
            div()
                .flex()
                .justify_end()
                .child(button("pet-studio-activate", i18n::t(locale, "petStudio.activateThis"), ButtonVariant::Secondary, true, None, |_ev, _window, _app| {})),
        )
}

// ── Gallery region ("我的桌寵") — honest empty state, see module doc
// comment for why the canvas's three named example rows are NOT
// reproduced. ──────────────────────────────────────────────────────────

fn gallery_region(locale: Locale) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(region_label(locale, "petStudio.myPetsTitle"))
        .child(card().child(empty_state("🐾", i18n::t(locale, "petStudio.noBridgeTitle"), Some(i18n::t(locale, "petStudio.noBridgeBody")), None::<Div>)))
}

// ── Top-level render ───────────────────────────────────────────────────
// No `WsConnState` gate — this page has no RPC of any kind to wait on, same
// "zero-RPC pages skip the auth gate entirely" precedent `screens::gallery.
// rs` (component library) and `screens::prototypes::mod.rs` (design gallery)
// already establish.

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    let locale = state.locale;
    ensure_state(locale, cx);

    let name_entity = cx.global::<PetStudioState>().name.clone();

    let header = div()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(div().text_size(px(theme::TEXT_XL)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "petStudio.title")))
        .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "petStudio.subtitle")));

    let editor_region = div()
        .flex()
        .flex_col()
        .gap_2()
        .child(region_label(locale, "petStudio.createTitle"))
        .child(div().flex().gap_4().items_start().child(div().flex_1().min_w(px(320.)).child(editor_left(locale, name_entity))).child(div().flex_1().min_w(px(320.)).child(editor_right(locale))));

    div()
        .id("pet-studio-page")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_4()
        .p_6()
        .child(header)
        .child(editor_region)
        .child(gallery_region(locale))
}
