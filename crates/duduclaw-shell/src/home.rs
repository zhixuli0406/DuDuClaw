// Home surface — Shell-S0's desktop first screen.
//
// Visual spec: `commercial/design/duduclaw-os-desktop/Main.dc.html`
// (1440×900 — matches this crate's window size exactly, no scaling math
// needed). That board is LIGHT-themed (white cards, `#09090b` foreground,
// blue `#2171cc` brand) — reproduced here via
// `duduclaw_native_gui::theme::light::*`, deliberately NOT the crate's
// higher-level `mds_gpui` component facade: `mds_gpui::card()`/`button()`/
// `badge()` are hardwired to that crate's top-level RE-EXPORTED tokens
// (`theme::SURFACE` etc. = `theme::dark::SURFACE` — see `theme.rs`'s own
// module doc comment, "Phase 1a renders dark only"), so calling them here
// would paint this board's light surfaces with dark colors. Every element
// below is hand-built straight from `theme::light::*` constants instead —
// the same "consume theme tokens exclusively, no ad-hoc values" discipline
// `mds_gpui` itself established, just applied against the other palette.
// Values with no matching token are hardcoded with a `// Main.dc.html:`
// comment naming their source, per the task brief's instruction.
//
// Round 3 (2026-08-20 keyboard/mouse bug fix): four elements are now real
// click targets, wired the same `cx.listener` way `overlay/notifications.rs`
// / `overlay/controlcenter.rs` already established — see this file's
// `render(cx)` call sites below. `cx: &mut Context<ShellView>` threads down
// through `menu_bar`/`menu_bar_ticker`/`menu_bar_right`/`workspace`/
// `composer` (and into `home_dock::dock`/`dock_settings`) the same
// SEQUENTIAL-not-simultaneous way `overlay.rs`'s own header comment
// documents for `main.rs`'s `on_close` + `overlay::render(..., cx)` — each
// function borrows `cx` for its own duration and returns before the next
// sibling call reborrows it, never two live borrows at once. Goal cards,
// the activity shelf, and every dock icon besides "設" stay static per the
// task brief ("goal 卡、shelf、其他 dock icon 維持靜態").
//
// Every clickable element also gets a `.hover(...)` bg tint, matching
// `duduclaw-native-gui`'s own established hover convention (e.g. `mds_gpui/
// button.rs`'s `.hover(move |style| style.bg(bg_hover))`) — `cursor_pointer()`
// alone signals clickability but gives no on-hover feedback.
//
// Icon glyphs: no `gpui::svg()` usage exists anywhere in this codebase yet
// (see `fake_data.rs`'s header comment), and the bundled font stack
// (`theme::app_font()`'s `InterVariable.ttf` + `NotoSansTC-Variable.ttf`,
// loaded by `duduclaw-native-gui`'s own `main.rs` — this crate does NOT
// re-load fonts, see this file's own font note below) has no guaranteed
// pictographic/emoji glyph coverage (bell, gear, arrow dingbats are NOT
// part of either font's core Latin/CJK charset). Every icon-shaped slot
// below is therefore either a single CJK character (guaranteed via Noto
// Sans TC, same "letter + color" placeholder convention
// `duduclaw-native-gui/src/nav.rs` already established) or plain ASCII, and
// purely decorative glyphs with no safe equivalent (the menu bar's
// notification-bell SVG) are dropped rather than risk a tofu box — an
// honest stub, not silently faked.
//
// Font note: unlike `duduclaw-native-gui`'s own `main.rs`, THIS crate's
// `main.rs` does not call `cx.text_system().add_fonts(...)` — Shell-S0 has
// no bundled font assets of its own yet, so text renders on gpui's system
// font fallback. CJK glyphs still render correctly (the OS provides a CJK
// system font), just not in the Inter/Noto Sans TC faces the design board
// specifies. Tracked as a stub, not fixed this round.
//
// Split across two files, same convention `duduclaw-native-gui/src/
// screens/shell.rs` + its `shell_sidebar.rs`/`shell_content_list.rs`/
// `shell_row.rs` siblings already establish for one screen too big for one
// file: THIS file owns the wallpaper/menu-bar/AI-workspace top half,
// `home_dock.rs` (`pub(super)`, not `pub` — nothing outside this module
// needs its internals) owns the goal-cards/activity-shelf/dock bottom half.

mod home_dock;

use std::sync::Arc;

use gpui::{div, img, linear_color_stop, linear_gradient, prelude::*, px, rgb, BoxShadow, Context, Div, FontWeight, Image, ImageFormat, Stateful};

use duduclaw_native_gui::theme;

use crate::fake_data;
use crate::surface::Overlay;
use crate::ShellView;

/// Branding PNGs, repo-relative to `appliance/branding/png/` (this crate's
/// task brief: "commercial/ 是 gitignored，資產一律用 appliance/branding 的
/// 版本"). `include_bytes!` embeds them directly into the binary — no
/// asset-loader / runtime file path involved, matching the task brief's
/// "可 include_bytes 內嵌".
const MARK_32: &[u8] = include_bytes!("../../../appliance/branding/png/mark-32.png");
const CAT_512: &[u8] = include_bytes!("../../../appliance/branding/png/cat-512.png");

pub(super) fn png(bytes: &'static [u8]) -> Arc<Image> {
    Arc::new(Image::from_bytes(ImageFormat::Png, bytes.to_vec()))
}

/// The whole Home surface: wallpaper, menu bar, AI workspace, goal cards,
/// activity shelf, and dock — absolutely positioned to match the design
/// board 1:1, same paint order as the board's own HTML source (background
/// layers first, chrome last).
pub fn render(cx: &mut Context<ShellView>) -> Stateful<Div> {
    div()
        .id("shell-home")
        .relative()
        .size_full()
        // Main.dc.html: `linear-gradient(135deg, #eef1f6 0%, #e6ecf5 45%,
        // #dde6f3 100%)` — gpui's `linear_gradient` only supports a 2-stop
        // gradient (see this file's header comment), so the middle stop is
        // dropped; both endpoints are kept verbatim.
        .bg(linear_gradient(
            135.0,
            linear_color_stop(rgb(0xeef1f6), 0.0),
            linear_color_stop(rgb(0xdde6f3), 1.0),
        ))
        // Main.dc.html: two `radial-gradient` soft glow blobs — gpui has no
        // radial-gradient primitive, approximated as flat low-opacity
        // circles (a visible flat-opacity edge instead of a soft falloff;
        // colors kept exact — `rgba(33,113,204,.10)` / `rgba(120,140,200,
        // .12)`).
        .child(blob_top_left(-180., -120., 720., theme::alpha(0x2171cc, 0.10)))
        .child(blob_bottom_right(-220., -100., 820., theme::alpha(0x788cc8, 0.12)))
        .child(cat_hero())
        .child(menu_bar(cx))
        .child(workspace(cx))
        .child(home_dock::goal_cards_row())
        .child(home_dock::activity_shelf())
        .child(home_dock::dock(cx))
}

fn blob_top_left(top: f32, left: f32, size: f32, color: gpui::Rgba) -> Div {
    div().absolute().top(px(top)).left(px(left)).w(px(size)).h(px(size)).rounded(px(size)).bg(color)
}

fn blob_bottom_right(bottom: f32, right: f32, size: f32, color: gpui::Rgba) -> Div {
    div().absolute().bottom(px(bottom)).right(px(right)).w(px(size)).h(px(size)).rounded(px(size)).bg(color)
}

/// The mascot — `cat-512.png` is actually 264×512 (not square), so the
/// design board's `height: 192px` implies `width: 99px` at the same aspect
/// ratio (264/512 × 192 = 99 exactly); centered for a FIXED 1440px window
/// (`left: (1440 - 99) / 2 = 670.5`) — this crate doesn't handle window
/// resize/responsive centering this round (task brief: dev-mode fixed
/// 1440×900 window).
fn cat_hero() -> gpui::Img {
    img(png(CAT_512))
        .absolute()
        .top(px(486.))
        .left(px(670.5))
        .w(px(99.))
        .h(px(192.))
        // Main.dc.html also applies `filter: drop-shadow(...)` — gpui has
        // no image drop-shadow filter (only a rectangular box-shadow on the
        // element's bounds, which would draw a visibly wrong-shaped box
        // behind this PNG's transparent margins) — skipped rather than
        // faked.
        .opacity(0.9)
}

// ── Menu bar ─────────────────────────────────────────────────────────────

fn menu_bar(cx: &mut Context<ShellView>) -> Stateful<Div> {
    div()
        .id("shell-menu-bar")
        .absolute()
        .top(px(0.))
        .left(px(0.))
        .right(px(0.))
        .h(px(30.))
        .flex()
        .items_center()
        .justify_between()
        .px(px(12.))
        .bg(theme::alpha(theme::light::SURFACE, 0.82))
        .border_b_1()
        .border_color(theme::alpha(theme::light::SURFACE_BORDER, 0.8))
        .text_size(px(13.))
        .text_color(theme::alpha(theme::light::FOREGROUND, 1.0))
        .child(menu_bar_left())
        .child(menu_bar_ticker(cx))
        .child(menu_bar_right(cx))
}

fn menu_bar_left() -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(16.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .font_weight(FontWeight::BOLD)
                .child(img(png(MARK_32)).w(px(16.)).h(px(16.)).rounded(px(8.)))
                .child(fake_data::MENU_BRAND),
        )
        .children(fake_data::MENU_ITEMS.iter().map(|label| div().child(*label)))
}

/// The approval ticker — "agent 審批 ticker 一條可點文字" per the task
/// brief. Round 3: a real `cx.listener` opening Notifications, same pattern
/// `overlay/notifications.rs`'s own approve/reject buttons already
/// established.
fn menu_bar_ticker(cx: &mut Context<ShellView>) -> Stateful<Div> {
    let on_click = cx.listener(|view, _ev, _window, cx| {
        if crate::diag_enabled() { eprintln!("[hit] ticker -> open Notifications"); }
        view.surface.open(Overlay::Notifications);
        cx.notify();
    });

    div()
        .id("shell-approval-ticker")
        .cursor_pointer()
        .flex()
        .items_center()
        .gap(px(8.))
        .bg(theme::alpha(theme::light::SURFACE, 1.0))
        .border_1()
        .border_color(theme::alpha(theme::light::SURFACE_BORDER, 1.0))
        .rounded(px(999.))
        .px(px(12.))
        .py(px(3.))
        .shadow(theme::light::surface_shadow())
        .text_size(px(12.))
        .hover(|style| style.bg(theme::alpha(theme::light::SURFACE_HOVER, 1.0)))
        .child(div().w(px(7.)).h(px(7.)).rounded(px(7.)).bg(theme::alpha(theme::light::WARNING, 1.0)))
        .child(div().font_weight(FontWeight::MEDIUM).child(fake_data::APPROVAL_TICKER))
        .on_click(on_click)
}

/// Round 3: split into two independent click targets — the ⌘K pill opens
/// the Launcher (task brief: "「⌘K」膠囊點擊 → 開 Launcher"), the
/// battery/clock status group opens Notifications (task brief: "右側狀態
/// 區（agent 頭像/電量/時鐘一帶）點擊 → 開 Notifications" — this board has
/// no agent-avatar glyph in the menu bar, see this file's header comment on
/// dropped pictographic glyphs, so the group is battery + clock only).
fn menu_bar_right(cx: &mut Context<ShellView>) -> Div {
    // `.open(...)`, not `.toggle_launcher()` — see `composer()`'s own doc
    // comment for why every mouse click target here opens rather than
    // toggles (only the cmd-k keyboard binding itself toggles).
    let cmdk_click = cx.listener(|view, _ev, _window, cx| {
        if crate::diag_enabled() { eprintln!("[hit] cmd-k pill -> open Launcher"); }
        view.surface.open(Overlay::Launcher);
        cx.notify();
    });
    let status_click = cx.listener(|view, _ev, _window, cx| {
        if crate::diag_enabled() { eprintln!("[hit] status group -> open Notifications"); }
        view.surface.open(Overlay::Notifications);
        cx.notify();
    });

    div()
        .flex()
        .items_center()
        .gap(px(12.))
        .child(
            div()
                .id("shell-menu-cmdk")
                .cursor_pointer()
                .bg(theme::alpha(theme::light::SECONDARY, 1.0))
                .border_1()
                .border_color(theme::alpha(theme::light::SURFACE_BORDER, 1.0))
                .rounded(px(6.))
                .px(px(6.))
                .py(px(1.))
                .text_size(px(11.))
                .text_color(theme::alpha(theme::light::MUTED_FOREGROUND, 1.0))
                .hover(|style| style.bg(theme::alpha(theme::light::SURFACE_SELECTED, 1.0)))
                // Main.dc.html shows a bell SVG to the left of this — see
                // this file's header comment on why pictographic glyphs are
                // dropped rather than risked; the ⌘K hint itself is kept
                // (design-load-bearing), accepting the small risk that "⌘"
                // alone renders as a tofu box on a font with no dedicated
                // glyph for it.
                .child("⌘K")
                .on_click(cmdk_click),
        )
        .child(
            div()
                .id("shell-menu-status")
                .cursor_pointer()
                .flex()
                .items_center()
                .gap(px(12.))
                .rounded(px(6.))
                .px(px(4.))
                .hover(|style| style.bg(theme::alpha(theme::light::SURFACE_HOVER, 1.0)))
                .child(div().text_size(px(12.)).child(fake_data::BATTERY_PCT))
                .child(div().text_size(px(12.)).font_weight(FontWeight::MEDIUM).child(fake_data::CLOCK))
                .on_click(status_click),
        )
}

// ── AI workspace (greeting + composer + suggestion chips) ────────────────

fn workspace(cx: &mut Context<ShellView>) -> Div {
    div()
        .absolute()
        .top(px(150.))
        .left(px(0.))
        .right(px(0.))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(14.))
        .child(
            div()
                .text_size(px(theme::TEXT_XL))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::alpha(theme::light::FOREGROUND, 1.0))
                .child(fake_data::GREETING),
        )
        .child(composer(cx))
        .child(suggestion_chips())
}

/// Round 3: clicking the composer opens the Launcher — task brief's
/// "S0 的合理近似" for a not-yet-typeable input box (real typing is later-
/// round scope, see `overlay/launcher.rs`'s own header comment on its
/// still-static query row). Uses `.open(...)`, not `.toggle_launcher()` —
/// cmd-k is the one binding whose task-brief wording is explicitly "切換"
/// (toggle); every mouse click target's own wording is "開" (open), so a
/// second click can't accidentally CLOSE an already-open Launcher here.
fn composer(cx: &mut Context<ShellView>) -> Stateful<Div> {
    let on_click = cx.listener(|view, _ev, _window, cx| {
        if crate::diag_enabled() { eprintln!("[hit] composer -> open Launcher"); }
        view.surface.open(Overlay::Launcher);
        cx.notify();
    });

    div()
        .id("shell-composer")
        .cursor_pointer()
        .w(px(620.))
        .flex()
        .items_center()
        .gap(px(10.))
        .bg(theme::alpha(theme::light::SURFACE, 1.0))
        .border_1()
        .border_color(theme::light::border())
        .rounded(px(theme::RADIUS_XL))
        .px(px(16.))
        .py(px(14.))
        // Main.dc.html: `0 16px 40px rgba(15,23,42,.10), 0 3px 10px
        // rgba(15,23,42,.06)` — a bespoke "floating" shadow, softer than
        // `theme::light::floating_shadow()`'s own recipe; kept as literal
        // `BoxShadow`s to match the board's actual numbers rather than
        // reusing that fn.
        .shadow(vec![
            BoxShadow::new(px(0.), px(16.), rgb(0x0f172a).opacity(0.10).into()).blur_radius(px(40.)),
            BoxShadow::new(px(0.), px(3.), rgb(0x0f172a).opacity(0.06).into()).blur_radius(px(10.)),
        ])
        .hover(|style| style.bg(theme::alpha(theme::light::SURFACE_HOVER, 1.0)))
        .child(
            div()
                .text_size(px(15.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::alpha(theme::light::MUTED_FOREGROUND, 1.0))
                .child("+"),
        )
        .child(
            div()
                .flex_1()
                .text_size(px(15.))
                // Main.dc.html: `#9f9fa9` placeholder gray — coincides with
                // `theme::dark::MUTED_FOREGROUND`, not any `light::` token;
                // kept as the literal hex the board specifies.
                .text_color(theme::alpha(0x9f9fa9, 1.0))
                .child(fake_data::COMPOSER_PLACEHOLDER),
        )
        .child(
            div()
                .w(px(30.))
                .h(px(30.))
                .rounded(px(9.))
                .bg(theme::alpha(theme::light::BRAND, 1.0))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_size(px(13.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(theme::alpha(theme::light::BRAND_FOREGROUND, 1.0))
                        .child("送"),
                ),
        )
        .on_click(on_click)
}

fn suggestion_chips() -> Div {
    div().flex().gap(px(8.)).children(fake_data::SUGGESTION_CHIPS.iter().map(|label| {
        div()
            .bg(theme::alpha(theme::light::SURFACE, 0.85))
            .border_1()
            .border_color(theme::alpha(theme::light::SURFACE_BORDER, 1.0))
            .rounded(px(999.))
            .px(px(12.))
            .py(px(4.))
            .text_size(px(12.))
            // Main.dc.html: `#52525c`, no matching `light::` token.
            .text_color(theme::alpha(0x52525c, 1.0))
            .child(*label)
    }))
}
