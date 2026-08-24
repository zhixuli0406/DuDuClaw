// Launcher overlay content — Shell-S0 round 2, dark theme in Shell-S1.
//
// Visual spec: `commercial/design/duduclaw-os-desktop/Launcher.dc.html`
// (light) / `commercial/design/duduclaw-os-home-dark/Launcher.dc.html`
// (dark) — both 1440×900 (same fixed window this crate always renders at,
// see `home.rs`'s own `cat_hero()` comment for why no responsive centering
// math is needed here either). The board's panel is `top:170px; left:50%;
// margin-left:-330px; width:660px` — for a FIXED 1440px window that's
// `left = (1440-660)/2 = 390px` exactly, reproduced as `PANEL_LEFT` below
// rather than carrying gpui a CSS-style percentage+negative-margin
// centering trick that only this one screen would ever use.
//
// ── Interaction scope, WP-A3 (2026-08-22, A-line S5 "殼整合") ──────────────
// Round 2's "static predisplay" is gone for the query row and the app
// section: `render()` reads a live query out of a real text field. D3-b
// (2026-08-23) replaced WP-A3's original mechanism — a `String` on
// `OverlayUiState` appended to by a raw `on_key_down` listener on the shell
// root, which that round documented as an honest "no IME composition" gap —
// with `ShellView.launcher_query_field` (`oobe::LauncherQueryField`, an
// `EntityInputHandler`-backed widget shared with the OOBE/lockscreen
// fields). The gap is closed: a zh-TW operator can now search their apps by
// typing Chinese, instead of only by an app's ASCII id or `Keywords=`
// entry. The delegate
// card and the files section stay untouched static DEMO content (task
// brief: "既有交辦框並存、交辦優先序不變") — see `render()`'s own comment
// below for why they only show in the EMPTY-query state rather than trying
// to (dishonestly) react to arbitrary typed text with no real
// NL-delegation/file-search backend behind them yet.
//
// ── Two app sections, two different claims (APP-1, 2026-08-22) ────────────
// A3's app section searched `fake_data::DOCK_APPS` — six canned entries,
// five of which had no app behind them. A user testing the real appliance
// reported exactly what that looks like from the outside: a menu of
// software that is not there. Both halves changed:
//
//   * 「應用程式」is now the REAL inventory of this machine —
//     `crate::apps::search` over `crate::apps::feed::InstalledAppsFeed`,
//     itself a live enumeration of `flatpak list` plus the XDG `.desktop`
//     directories (`crate::apps::installed`). Every row is a real launch
//     button (`crate::apps::launch`). When it is empty it SAYS why, in one
//     of three distinct honest states (`empty_apps_line` below) — it never
//     falls back to canned rows.
//   * 「可安裝」is a separate, clearly-labelled section for apps this shell
//     knows how to FETCH but that are not installed here
//     (`crate::apps::catalog`). "Installed" and "installable" are different
//     claims, so they are different sections and never mixed.
//
// ── Install confirmation gate, WP-A4-4 (2026-08-22) ───────────────────────
// A row in the 「可安裝」section carries a small "安裝" chip. It does not
// install anything — it arms a confirmation sheet (`overlay::install_gate`,
// a pure state machine with its own tests) that REPLACES this panel's
// result list until the operator answers, naming the app, the remote, the
// download size (or an honest "無法取得"), and where it lands. Cancel drops
// the gate and nothing ever ran. See that module's header comment for the
// honesty rules the type itself enforces, and `crate::apps`'s "Install"
// section for why every install argv must carry `--installation=data`.
//
// Icon glyphs: real stroke icons since ICON-1 (2026-08-22) wherever the
// approved boards actually draw one — the search magnifier in the query
// row, the folder and spreadsheet in the 檔案 rows, and the globe on the
// 可安裝 catalog row (that entry is the same Chromium the board's browser
// tile always stood for). See `crate::icons` for the asset provenance and
// the alpha-mask tinting constraint.
//
// INSTALLED app rows are the deliberate exception and still render the
// app's own first character on the board's neutral white/outline tile: a
// real installed app has no board colour or board icon to lift, and its
// real icon is whatever its `.desktop` `Icon=` names, resolved against the
// system icon theme. `apps::installed` already parses and carries that
// value; rendering it is a SEPARATE work package going through the design
// flow, deliberately not started here — but ICON-1's `AssetSource` and
// `gpui::svg()` plumbing is the foundation it was blocked on.
//
// ── Dark theme (Shell-S1) ─────────────────────────────────────────
// Every color below now resolves through `palette: ShellPalette` — see
// `crate::palette`'s own header comment for the light/dark token mapping.
// One structural note: NEITHER theme's panel root originally set an
// explicit `.text_color(...)` (every text node inside either set its own
// color, or fell through to gpui's own default text color, `black()` —
// verified in `gpui::style::TextStyle`'s own `Default` impl). That default
// happens to read acceptably close to `FOREGROUND` in LIGHT (`#09090b` is
// near-black), which is why the light board never needed a fallback — but
// it is NOT acceptable in dark (pure black text on a dark panel is
// unreadable). `render()` below therefore adds `.text_color(palette.
// foreground)` at the panel root ONLY when `palette.is_dark()` — light's
// chain is completely untouched (no new call at all in that branch), so
// this crate's "light stays byte-identical" constraint holds exactly, while
// dark gets a real, correct default instead of inheriting pure black.

use std::sync::mpsc;
use std::time::Duration;

use gpui::{div, linear_color_stop, linear_gradient, prelude::*, px, rgb, BoxShadow, Context, Div, FontWeight, Stateful};

use duduclaw_native_gui::theme;

use super::install_gate::{GateStage, InstallGate, SizeProbe};
use super::OverlayUiState;
use crate::apps::catalog::{self, CatalogApp};
use crate::apps::feed::{AppsEmptyState, InstalledAppsFeed};
use crate::apps::icon_theme;
use crate::apps::installed::InstalledApp;
use crate::apps::VerifiedTier;
use crate::fake_data;
use crate::gateway_client;
use crate::i18n::{t, Key, Locale};
use crate::icons;
use crate::palette::ShellPalette;
use crate::ShellView;

/// Cadence for the background-thread <-> `cx.spawn` bridge the install
/// gate's two blocking `flatpak` calls use — same value `overlay/
/// notifications.rs::POLL_INTERVAL` and `home/home_dock.rs::
/// BRIDGE_POLL_INTERVAL` already use for theirs.
const BRIDGE_POLL_INTERVAL: Duration = Duration::from_millis(50);

const PANEL_WIDTH: f32 = 660.;
const PANEL_LEFT: f32 = (1440. - PANEL_WIDTH) / 2.; // 390 — see header comment
const PANEL_TOP: f32 = 170.;

pub(super) fn render(
    ui: &OverlayUiState,
    installed: &InstalledAppsFeed,
    query_field: &crate::oobe::LauncherQueryField,
    palette: ShellPalette,
    cx: &mut Context<ShellView>,
) -> Stateful<Div> {
    // Owned, not borrowed: reading it borrows `cx` immutably, and everything
    // below (`apps_section`, `maybe_catalog_section`) needs `&mut Context`.
    let query = query_field.field.read(cx).content(cx);
    let query = query.as_str();
    // Launcher.dc.html: bg `rgba(255,255,255,0.97)` light / `rgba(30,30,33,
    // 0.97)` dark — `surface_raised` in both (see `home.rs`'s
    // `menu_bar_ticker` comment for why not `surface`). Border: opaque
    // `border()` light / `rgba(255,255,255,0.12)` dark. Shadow: bespoke in
    // both themes (deeper than `floating_shadow()`'s own recipe — same
    // "kept literal to match the board's actual numbers" reasoning the
    // original light-only comment already gave).
    let border_color: gpui::Hsla = if palette.is_dark() { theme::alpha(0xffffff, 0.12).into() } else { palette.border() };
    let shadow = if palette.is_dark() {
        vec![
            BoxShadow::new(px(0.), px(24.), rgb(0x000000).opacity(0.55).into()).blur_radius(px(64.)),
            BoxShadow::new(px(0.), px(4.), rgb(0x000000).opacity(0.35).into()).blur_radius(px(14.)),
        ]
    } else {
        vec![
            BoxShadow::new(px(0.), px(24.), rgb(0x0f172a).opacity(0.28).into()).blur_radius(px(64.)),
            BoxShadow::new(px(0.), px(4.), rgb(0x0f172a).opacity(0.10).into()).blur_radius(px(14.)),
        ]
    };

    let mut panel = div()
        .id("overlay-launcher-panel")
        .absolute()
        .top(px(PANEL_TOP))
        .left(px(PANEL_LEFT))
        .w(px(PANEL_WIDTH))
        .flex()
        .flex_col()
        .overflow_hidden()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(palette.surface_raised, 0.97))
        .border_1()
        .border_color(border_color)
        .shadow(shadow);
    if palette.is_dark() {
        // See this file's header comment on why this is dark-only.
        panel = panel.text_color(theme::alpha(palette.foreground, 1.0));
    }
    panel = panel.child(query_row(query_field, palette));
    // WP-A4-4: an armed install confirmation REPLACES the result list. It is
    // a decision the operator has to answer before anything else in this
    // panel means much, and leaving the underlying rows clickable behind a
    // pending "are you sure" would let a second click start a launch (or
    // another gate) while the first question is still on screen. Same
    // "takes over its container" shape OOBE/lockscreen use at the window
    // level, applied here at the panel level.
    if let Some(gate) = &ui.install_gate {
        return panel.child(install_sheet(gate, palette, cx)).child(footer(palette));
    }
    // The delegate card and files section are still fixed DEMO content (see
    // this file's header comment) — showing them alongside a live-typed
    // query that has nothing to do with "請款單" would be actively
    // misleading, not just stale, so they only appear in the pre-typing
    // empty state. A non-empty query shows ONLY the (now real) app results —
    // "既有交辦框並存、交辦優先序不變" is satisfied by the delegate card
    // still being the visually FIRST thing a fresh Launcher open shows, not
    // by forcing it to survive every possible typed query.
    if query.trim().is_empty() {
        panel = panel.child(delegate_section(palette)).child(apps_section(query, installed, palette, cx));
        if let Some(section) = maybe_catalog_section(query, installed, palette, cx) {
            panel = panel.child(section);
        }
        panel = panel.child(files_section(palette));
    } else {
        panel = panel.child(apps_section(query, installed, palette, cx));
        if let Some(section) = maybe_catalog_section(query, installed, palette, cx) {
            panel = panel.child(section);
        }
    }
    panel.child(footer(palette))
}

/// D3-b (2026-08-23): the row now hosts the REAL search field entity. It
/// used to paint a `String` plus a hand-drawn 2px caret bar; both are gone —
/// the widget draws its own caret (and its own IME preedit underline), and
/// the placeholder is the field's, set at construction from the same
/// `Key::LauncherSearchPlaceholder` string this fn used to read.
fn query_row(query_field: &crate::oobe::LauncherQueryField, palette: ShellPalette) -> Div {
    // Launcher.dc.html: border-bottom `#f0f0f2` light / `rgba(255,255,255,
    // 0.08)` dark.
    let border_color = if palette.is_dark() { theme::alpha(0xffffff, 0.08) } else { theme::alpha(0xf0f0f2, 1.0) };
    div()
        .flex()
        .items_center()
        .gap(px(12.))
        .px(px(22.))
        .py(px(18.))
        .border_b_1()
        .border_color(border_color)
        // ICON-1: the board's leading magnifier, 19px, `#71717b` light /
        // `#9f9fa9` dark — exactly `palette.muted_foreground` in both. This
        // slot had NO placeholder before (there is no safe single character
        // for a magnifier, so the original round dropped it), which is why
        // it uses `icon_or_none`: a missing asset renders nothing rather
        // than an invented substitute.
        .children(icons::icon_or_none(&[(icons::SEARCH, palette.muted_foreground)], 19.))
        .child(query_field.field.clone())
}

fn section_label(label: &'static str, palette: ShellPalette) -> Div {
    // Launcher.dc.html: `#9f9fa9` light / `#71717b` dark — text ladder
    // rank 4, matches `text_faint` exactly (see `crate::palette`'s own
    // header comment).
    div()
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::alpha(palette.text_faint, 1.0))
        .px(px(10.))
        .py(px(4.))
        .child(label)
}

fn delegate_section(palette: ShellPalette) -> Div {
    // Launcher.dc.html: highlight card bg `#e8f1fb` / border `#cfe0f5`
    // light — brand-tinted `rgba(67,144,238,0.12)` bg / `rgba(67,144,238,
    // 0.35)` border dark. Hint pill: bg `#ffffff` / border `#cfe0f5` / text
    // `brand` light — bg `rgba(67,144,238,0.15)` / border `rgba(67,144,
    // 238,0.40)` / text `brand_bright` (`#59a6ff`) dark.
    let dark = palette.is_dark();
    let card_bg = if dark { theme::alpha(palette.brand, 0.12) } else { theme::alpha(0xe8f1fb, 1.0) };
    let card_border = if dark { theme::alpha(palette.brand, 0.35) } else { theme::alpha(0xcfe0f5, 1.0) };
    let plan_text = theme::alpha(palette.text_secondary, 1.0);
    let hint_bg = if dark { theme::alpha(palette.brand, 0.15) } else { theme::alpha(palette.surface_raised, 1.0) };
    let hint_border = if dark { theme::alpha(palette.brand, 0.40) } else { theme::alpha(0xcfe0f5, 1.0) };
    let hint_text = if dark { theme::alpha(palette.brand_bright, 1.0) } else { theme::alpha(palette.brand, 1.0) };

    div().pt(px(10.)).px(px(12.)).pb(px(4.)).flex().flex_col().child(section_label(fake_data::LAUNCHER_SECTION_DELEGATE, palette)).child(
        div()
            .flex()
            .gap(px(12.))
            .bg(card_bg)
            .border_1()
            .border_color(card_border)
            .rounded(px(12.))
            .px(px(14.))
            .py(px(13.))
            .child(
                div()
                    .w(px(38.))
                    .h(px(38.))
                    .rounded(px(19.))
                    .bg(theme::alpha(fake_data::LAUNCHER_DELEGATE_AGENT_BG_HEX, 1.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme::alpha(palette.brand_foreground, 1.0))
                            .child(fake_data::LAUNCHER_DELEGATE_AGENT_INITIAL),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(fake_data::LAUNCHER_DELEGATE_TITLE))
                    .child(div().mt(px(3.)).text_size(px(12.)).text_color(plan_text).child(fake_data::LAUNCHER_DELEGATE_PLAN)),
            )
            .child(
                div()
                    .bg(hint_bg)
                    .border_1()
                    .border_color(hint_border)
                    .rounded(px(6.))
                    .px(px(8.))
                    .py(px(2.))
                    .text_size(px(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(hint_text)
                    .child(fake_data::LAUNCHER_DELEGATE_HINT),
            ),
    )
}

/// The Launcher's 「應用程式」category — the REAL inventory of this machine
/// (`crate::apps::search` over the live `InstalledAppsFeed`), see this
/// file's header comment. `cx: &mut Context<ShellView>` threaded through via
/// a plain `for` loop, not `.iter().map(...)` — `&mut Context<V>` isn't
/// `Copy`, same wall `overlay/notifications.rs`'s own header comment
/// documents and works around identically for its approval cards.
fn apps_section(query: &str, installed: &InstalledAppsFeed, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    let results = crate::apps::search(installed.apps(), query);
    let mut section = div().pt(px(6.)).pb(px(4.)).flex().flex_col().child(section_label(fake_data::LAUNCHER_SECTION_APPS, palette));
    if results.is_empty() {
        section = section.child(
            div().px(px(22.)).py(px(8.)).text_size(px(12.5)).text_color(theme::alpha(palette.text_faint, 1.0)).child(empty_apps_line(query, installed)),
        );
        return section;
    }
    let mut rows = div().flex().flex_col().px(px(12.));
    for item in results {
        rows = rows.child(app_result_row(item, palette, cx));
    }
    section.child(rows)
}

/// Which honest sentence an empty 「應用程式」list gets. Four distinct
/// situations, four distinct answers — the point of this work package is
/// that the shell stops pretending, so "we have not looked yet", "we
/// looked and this machine has nothing", "we could not read the list" and
/// "your search matched nothing" must not collapse into one message.
fn empty_apps_line(query: &str, installed: &InstalledAppsFeed) -> &'static str {
    if !installed.apps().is_empty() {
        // There ARE apps; the query just matched none of them.
        return t(Locale::ZhTw, Key::LauncherNoAppResults);
    }
    let _ = query;
    match installed.empty_state() {
        AppsEmptyState::Scanning => t(Locale::ZhTw, Key::LauncherAppsScanning),
        AppsEmptyState::NoneInstalled => t(Locale::ZhTw, Key::LauncherAppsNoneInstalled),
        AppsEmptyState::ReadFailed => t(Locale::ZhTw, Key::LauncherAppsReadFailed),
    }
}

/// One installed-app row. `item` is borrowed from the feed (not `'static`
/// like the old canned array), so the click handler captures an owned clone
/// — a handful of small structs per render pass, which is what it costs to
/// have a list that is actually real.
fn app_result_row(item: &InstalledApp, palette: ShellPalette, cx: &mut Context<ShellView>) -> Stateful<Div> {
    let launch_target = item.clone();
    let row = div()
        .id(format!("launcher-app-{}", item.id))
        .flex()
        .items_center()
        .gap(px(12.))
        .rounded(px(10.))
        .px(px(10.))
        .py(px(8.))
        .child(installed_app_tile(item, palette))
        .child(div().flex_1().text_size(px(13.5)).child(item.name.clone()))
        .child(verified_badge(catalog::verified_tier(item.xdg_app_id()), palette));

    let on_click = cx.listener(move |_view, _ev, _window, _cx| {
        if crate::diag_enabled() {
            eprintln!("[hit] launcher app row '{}' -> launch", launch_target.id);
        }
        crate::apps::launch(&launch_target);
    });
    row.cursor_pointer().hover(|style| style.bg(theme::alpha(palette.surface_hover, 1.0))).on_click(on_click)
}

/// The result row's tile corner radius (`Launcher.dc.html`). Named for the
/// same reason `home/home_dock.rs::DOCK_TILE_RADIUS_PX` is: ICON-2 needs it
/// twice, once on the tile and once on the clip a full-bleed square raster
/// is drawn into.
const ROW_TILE_RADIUS_PX: f32 = 8.;

/// The 30px neutral app tile every result row uses, without its child.
/// Reproduces the design board's own white/outline tile treatment (the one
/// it uses for Files and Browser) in both themes — a real installed app has
/// no board colour to lift, and inventing a per-app palette would be
/// decoration masquerading as identity.
///
/// Split out by ICON-2 so the CONTAINER is written once and the two things
/// that go inside it stay separate: the catalog's board artwork
/// (`app_tile`, `gpui::svg()`) and an installed app's real icon
/// (`installed_app_tile`, `gpui::img()`). See
/// `crate::icons::app_icon_element` for why those cannot be one path.
fn app_tile_shell(palette: ShellPalette) -> Div {
    let dark = palette.is_dark();
    let (gradient_top, gradient_bottom) =
        if dark { (palette.neutral_tile_top, palette.neutral_tile_bottom) } else { (0xffffff, 0xedeff4) };
    let border_color = if dark { palette.neutral_tile_border } else { theme::alpha(theme::light::SURFACE_BORDER, 1.0) };
    div()
        .w(px(icon_theme::TILE_ROW_PX))
        .h(px(icon_theme::TILE_ROW_PX))
        .rounded(px(ROW_TILE_RADIUS_PX))
        .bg(linear_gradient(180.0, linear_color_stop(rgb(gradient_top), 0.0), linear_color_stop(rgb(gradient_bottom), 1.0)))
        .border_1()
        .border_color(border_color)
        .flex()
        .items_center()
        .justify_center()
        // The pre-existing light-only `0.14` already approximates the
        // board's own two distinct per-icon values (`.12` neutral / `.16`
        // colored, averaging to `.14`) as one flat number — kept exactly
        // (light must stay byte-identical). Dark mirrors the same
        // averaging strategy over ITS OWN two board values (`.30`/`.35`
        // -> `.32`).
        .shadow(palette.icon_shadow(0.14, 0.32))
}

/// The stroke/text color a tile's contents use when this shell is drawing
/// them: light `SURFACE_FOREGROUND` at 0.55 alpha, dark `#b0b0b8`. Same
/// pair `home/home_dock.rs::dock_app` resolves for its own tile, unchanged
/// by ICON-2 — it now tints the generic application icon where it used to
/// tint a letter.
fn app_tile_content_color(palette: ShellPalette) -> gpui::Rgba {
    if palette.is_dark() {
        theme::alpha(0xb0b0b8, 1.0)
    } else {
        theme::alpha(palette.foreground, 0.55)
    }
}

/// The CATALOG tile: this shell's own board artwork (today the Chromium
/// globe), with the entry's text glyph as the fallback. `gpui::svg()`
/// path — the assets are `include_bytes!`-embedded and tinted from the
/// palette.
fn app_tile(glyph: &str, layers: &[icons::Layer], palette: ShellPalette) -> Div {
    app_tile_shell(palette)
        .text_size(px(13.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(app_tile_content_color(palette))
        .child(match icons::icon_or_none(layers, 17.) {
            Some(icon) => icon,
            None => glyph.to_string().into_any_element(),
        })
}

/// The INSTALLED-APP tile: the app's own icon, resolved from its `.desktop`
/// `Icon=` at scan time (`crate::apps::icon_resolve`) and drawn full colour
/// through `gpui::img()`, uncropped, at 80% of this container. ICON-2
/// (2026-08-22) — before it, this slot drew the app name's first character,
/// a convention the research sweep found at no desktop OS at all.
fn installed_app_tile(app: &InstalledApp, palette: ShellPalette) -> Div {
    app_tile_shell(palette).child(icons::app_icon_element(
        app.resolved_icon.as_ref().and_then(|icon| icon.for_container(icon_theme::TILE_ROW_PX)),
        icon_theme::TILE_ROW_PX,
        ROW_TILE_RADIUS_PX,
        app_tile_content_color(palette).into(),
    ))
}

/// The 「可安裝」section — apps this shell knows how to fetch that are NOT
/// already on this machine (`crate::apps::catalog`). Rendered as its own
/// labelled section, never mixed into the inventory above it: "installed"
/// and "installable" are different claims. Renders NOTHING at all when
/// every catalog entry matching the query is already installed, rather than
/// an empty heading.
fn maybe_catalog_section(query: &str, installed: &InstalledAppsFeed, palette: ShellPalette, cx: &mut Context<ShellView>) -> Option<Div> {
    let candidates: Vec<&'static CatalogApp> = catalog::search(query).into_iter().filter(|entry| !installed.contains_id(entry.flatpak_id)).collect();
    if candidates.is_empty() {
        return None;
    }
    let mut rows = div().flex().flex_col().px(px(12.));
    for entry in candidates {
        rows = rows.child(catalog_result_row(entry, palette, cx));
    }
    Some(
        div()
            .pt(px(6.))
            .pb(px(4.))
            .flex()
            .flex_col()
            .child(section_label(t(Locale::ZhTw, Key::LauncherSectionInstallable), palette))
            .child(rows),
    )
}

/// One installable-catalog row. Deliberately NOT clickable as a whole: the
/// row means "this could be fetched", and the only action it offers is the
/// explicit 安裝 chip, which arms the confirmation sheet. An installed-app
/// row's click means launch; conflating the two would make a single click
/// ambiguous about whether it downloads 2.4 GB.
fn catalog_result_row(entry: &'static CatalogApp, palette: ShellPalette, cx: &mut Context<ShellView>) -> Stateful<Div> {
    div()
        .id(entry.id)
        .flex()
        .items_center()
        .gap(px(12.))
        .rounded(px(10.))
        .px(px(10.))
        .py(px(8.))
        .child(app_tile(entry.glyph, &icons::catalog_layers(entry.id, palette).unwrap_or_default(), palette))
        .child(div().flex_1().text_size(px(13.5)).child(entry.label))
        .child(install_button(entry, palette, cx))
        .child(verified_badge(entry.verified, palette))
}

// ── Install confirmation gate (WP-A4-4, 2026-08-22) ─────────────────────

/// The secondary "安裝" control on an installable-catalog row. Arms the
/// confirmation sheet and kicks off the size probe — it does NOT install
/// anything. Styled as a quiet outlined chip (same neutral treatment
/// `overlay/notifications.rs`'s own secondary buttons use).
fn install_button(item: &'static CatalogApp, palette: ShellPalette, cx: &mut Context<ShellView>) -> Stateful<Div> {
    let bg = if palette.is_dark() { palette.surface_hover } else { palette.surface_raised };
    let border_color: gpui::Hsla = if palette.is_dark() { theme::alpha(0xffffff, 0.14).into() } else { palette.border() };
    let on_click = cx.listener(move |view, _ev, _window, cx| {
        if crate::diag_enabled() {
            eprintln!("[hit] launcher install chip '{}' -> arm confirmation gate", item.id);
        }
        open_install_gate(item, view, cx);
    });
    div()
        .id(format!("launcher-install-{}", item.id))
        .cursor_pointer()
        .bg(theme::alpha(bg, 1.0))
        .text_color(theme::alpha(palette.text_secondary, 1.0))
        .border_1()
        .border_color(border_color)
        .rounded(px(8.))
        .px(px(10.))
        .py(px(3.))
        .text_size(px(11.5))
        .child(t(Locale::ZhTw, Key::LauncherInstallButton).to_string())
        .on_click(on_click)
}

/// Arms the gate and dispatches the (blocking) size probe on a background
/// thread. Infallible since APP-1: a `CatalogApp` cannot exist without a
/// real ref and remote, so there is no "could not open a gate" case left
/// (see `overlay::install_gate`'s own APP-1 note).
fn open_install_gate(item: &'static CatalogApp, view: &mut ShellView, cx: &mut Context<ShellView>) {
    let gate = InstallGate::open(item, crate::apps::INSTALL_DESTINATION_LABEL.to_string());
    let (remote, flatpak_id, app_id) = (gate.remote, gate.flatpak_id, gate.app_id);
    view.overlay_ui.install_gate = Some(gate);
    cx.notify();

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(crate::apps::probe_download_size(remote, flatpak_id));
    });
    cx.spawn(async move |weak, cx| loop {
        match rx.try_recv() {
            Ok(size) => {
                let _ = weak.update(cx, |view, cx| {
                    if let Some(gate) = view.overlay_ui.install_gate.as_mut() {
                        gate.apply_size_probe(app_id, size);
                        cx.notify();
                    }
                });
                break;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => break,
        }
        cx.background_executor().timer(BRIDGE_POLL_INTERVAL).await;
    })
    .detach();
}

/// The confirmation sheet. Everything the operator needs to answer "should
/// this run" is on it: which app, from which remote, how big the download
/// is (or an honest "無法取得"), and where it lands.
fn install_sheet(gate: &InstallGate, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    let size_text = match &gate.size {
        SizeProbe::Probing => t(Locale::ZhTw, Key::InstallGateSizeProbing).to_string(),
        SizeProbe::Known(text) => text.clone(),
        SizeProbe::Unavailable => t(Locale::ZhTw, Key::InstallGateSizeUnknown).to_string(),
    };

    let mut sheet = div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .px(px(22.))
        .pt(px(16.))
        .pb(px(16.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                // ICON-1: the catalog entry's own board icon (the same one
                // its result row shows, so the sheet is visibly about the
                // app just clicked), falling back to that entry's `glyph`
                // character. No NEW pictogram is introduced here either
                // way — this sheet still never invents an icon of its own.
                .child(
                    div().text_size(px(20.)).text_color(theme::alpha(palette.text_secondary, 1.0)).child(icons::icon_or_glyph(
                        &icons::catalog_layers(gate.app_id, palette).unwrap_or_default(),
                        20.,
                        gate.glyph,
                    )),
                )
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).child(t(Locale::ZhTw, Key::InstallGateTitle).to_string()))
                        // The app's own identity, one line, prominent: the
                        // display name the operator recognizes plus the
                        // exact flatpak id that will actually be fetched —
                        // a confirmation sheet that only showed the pretty
                        // name would be asking them to approve something
                        // they cannot verify.
                        .child(
                            div()
                                .mt(px(2.))
                                .text_size(px(12.5))
                                .text_color(theme::alpha(palette.text_secondary, 1.0))
                                .child(format!("{} · {}", gate.label, gate.flatpak_id)),
                        ),
                ),
        )
        .child(div().text_size(px(12.5)).text_color(theme::alpha(palette.text_secondary, 1.0)).child(t(Locale::ZhTw, Key::InstallGateNotice).to_string()))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .mt(px(2.))
                .child(detail_row(t(Locale::ZhTw, Key::InstallGateSourceLabel), gate.remote.to_string(), palette))
                .child(detail_row(t(Locale::ZhTw, Key::InstallGateSizeLabel), size_text, palette))
                .child(detail_row(t(Locale::ZhTw, Key::InstallGateLocationLabel), gate.install_root.clone(), palette)),
        );

    sheet = sheet.child(match &gate.stage {
        GateStage::Confirming => decision_row(palette, cx),
        // No dismiss button while the spawn is in flight — there is nothing
        // useful to press, and offering one that cannot actually stop the
        // child process would be a lie.
        GateStage::Installing => status_line(t(Locale::ZhTw, Key::InstallGateInstallingLabel).to_string(), palette.text_secondary),
        GateStage::Started => settled_row(t(Locale::ZhTw, Key::InstallGateStartedLabel).to_string(), palette.text_secondary, palette, cx),
        // The raw reason (e.g. "No such file or directory") goes to stderr
        // only, same split `lockscreen::render::apply_unlock_result` draws:
        // the operator-facing line stays one honest sentence.
        GateStage::Failed(_) => settled_row(t(Locale::ZhTw, Key::InstallGateFailedLabel).to_string(), palette.destructive, palette, cx),
    });
    sheet
}

/// A settled sheet's status line plus a way back to the result list. Escape
/// and a backdrop click already dismiss the whole Launcher (and with it the
/// gate, via `OverlayUiState::close_launcher_query`), but the footer never
/// advertises Escape — leaving a finished sheet with no visible exit would
/// read as stuck.
fn settled_row(text: String, text_hex: u32, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    let back_click = cx.listener(|view, _ev, _window, cx| {
        view.overlay_ui.install_gate = None;
        cx.notify();
    });
    let bg = if palette.is_dark() { palette.surface_hover } else { palette.surface_raised };
    let border_color: gpui::Hsla = if palette.is_dark() { theme::alpha(0xffffff, 0.14).into() } else { palette.border() };
    div()
        .mt(px(4.))
        .flex()
        .items_center()
        .gap(px(10.))
        .child(div().flex_1().text_size(px(12.)).text_color(theme::alpha(text_hex, 1.0)).child(text))
        .child(
            div()
                .id("install-gate-back")
                .cursor_pointer()
                .bg(theme::alpha(bg, 1.0))
                .text_color(theme::alpha(palette.text_secondary, 1.0))
                .border_1()
                .border_color(border_color)
                .rounded(px(8.))
                .px(px(12.))
                .py(px(4.))
                .text_size(px(12.))
                .child(t(Locale::ZhTw, Key::NavBack).to_string())
                .on_click(back_click),
        )
}

/// One label/value line of the sheet. `flex_1` on the value side so a long
/// flatpak id or path wraps rather than pushing the panel wide.
fn detail_row(label: &str, value: String, palette: ShellPalette) -> Div {
    div()
        .flex()
        .gap(px(10.))
        .text_size(px(12.5))
        .child(div().w(px(72.)).text_color(theme::alpha(palette.text_faint, 1.0)).child(label.to_string()))
        .child(div().flex_1().child(value))
}

/// One post-decision status line (installing / started / failed). Takes the
/// resolved hex rather than the whole palette because the three call sites
/// already pick their own token — there is no per-theme branch left here.
fn status_line(text: String, color_hex: u32) -> Div {
    div().mt(px(4.)).text_size(px(12.)).text_color(theme::alpha(color_hex, 1.0)).child(text)
}

/// 開始安裝 / 取消. Cancel simply drops the gate — see `install_gate::
/// InstallGate`'s own header comment for why that is enough to guarantee
/// nothing ran.
fn decision_row(palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    let confirm_click = cx.listener(|view, _ev, _window, cx| {
        let Some(gate) = view.overlay_ui.install_gate.as_mut() else {
            return;
        };
        let Some((remote, app_id)) = gate.confirm() else {
            return;
        };
        cx.notify();
        dispatch_install(remote, app_id, cx);
    });
    let cancel_click = cx.listener(|view, _ev, _window, cx| {
        if crate::diag_enabled() {
            eprintln!("[hit] install gate -> cancel (nothing runs)");
        }
        view.overlay_ui.install_gate = None;
        cx.notify();
    });

    let cancel_bg = if palette.is_dark() { palette.surface_hover } else { palette.surface_raised };
    let cancel_border: gpui::Hsla = if palette.is_dark() { theme::alpha(0xffffff, 0.14).into() } else { palette.border() };

    div()
        .mt(px(6.))
        .flex()
        .items_center()
        .gap(px(8.))
        .child(
            div()
                .id("install-gate-confirm")
                .cursor_pointer()
                .bg(theme::alpha(palette.brand, 1.0))
                .text_color(theme::alpha(palette.brand_foreground, 1.0))
                .rounded(px(8.))
                .px(px(14.))
                .py(px(6.))
                .text_size(px(12.5))
                .font_weight(FontWeight::SEMIBOLD)
                .child(t(Locale::ZhTw, Key::InstallGateConfirmButton).to_string())
                .on_click(confirm_click),
        )
        .child(
            div()
                .id("install-gate-cancel")
                .cursor_pointer()
                .bg(theme::alpha(cancel_bg, 1.0))
                .text_color(theme::alpha(palette.text_secondary, 1.0))
                .border_1()
                .border_color(cancel_border)
                .rounded(px(8.))
                .px(px(14.))
                .py(px(6.))
                .text_size(px(12.5))
                // Reuses the OOBE network step's own "取消" rather than
                // adding a second identical key — same sharing
                // `overlay/notifications.rs::cancel_button` already does.
                .child(t(Locale::ZhTw, Key::NetworkCancelButton).to_string())
                .on_click(cancel_click),
        )
}

/// Runs `apps::install` off the render thread and settles the gate with the
/// SPAWN result — same `std::thread::spawn` + `mpsc` + `cx.spawn` bridge
/// every other blocking call in this crate uses.
fn dispatch_install(remote: &'static str, app_id: &'static str, cx: &mut Context<ShellView>) {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = crate::apps::install(remote, app_id);
        if let Err(e) = &result {
            eprintln!("[apps] install {remote} {app_id} could not start: {e}");
        }
        let _ = tx.send(result);
    });
    cx.spawn(async move |weak, cx| loop {
        match rx.try_recv() {
            Ok(result) => {
                let _ = weak.update(cx, |view, cx| {
                    if let Some(gate) = view.overlay_ui.install_gate.as_mut() {
                        gate.apply_install_result(result);
                        cx.notify();
                    }
                });
                break;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => break,
        }
        cx.background_executor().timer(BRIDGE_POLL_INTERVAL).await;
    })
    .detach();
}

/// The D8 "DuDuClaw Verified" tier badge pill — see `crate::palette::
/// ShellPalette::verified_bg`/`verified_text`'s own doc comments for why
/// this resolves through the existing `BadgeKind`/`destructive` tokens
/// rather than a new color family. Labels are new chrome (no board
/// precedent — see this file's header comment), so they route through
/// `crate::i18n` like the placeholder/no-results strings above.
fn verified_badge(tier: VerifiedTier, palette: ShellPalette) -> Div {
    let label = match tier {
        VerifiedTier::Verified => Key::VerifiedTierVerified,
        VerifiedTier::Works => Key::VerifiedTierWorks,
        VerifiedTier::Partial => Key::VerifiedTierPartial,
        VerifiedTier::Unsupported => Key::VerifiedTierUnsupported,
        VerifiedTier::Unrated => Key::VerifiedTierUnrated,
    };
    div()
        .text_size(px(11.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(palette.verified_text(tier))
        .bg(palette.verified_bg(tier))
        .rounded(px(999.))
        .px(px(8.))
        .py(px(2.))
        .child(t(Locale::ZhTw, label))
}

fn files_section(palette: ShellPalette) -> Div {
    let mut rows = div().flex().flex_col().px(px(12.));
    for item in fake_data::LAUNCHER_FILE_RESULTS {
        rows = rows.child(file_result_row(item, palette));
    }
    div().pt(px(6.)).pb(px(14.)).flex().flex_col().child(section_label(fake_data::LAUNCHER_SECTION_FILES, palette)).child(rows)
}

fn file_result_row(item: &fake_data::LauncherFileResult, palette: ShellPalette) -> Stateful<Div> {
    div()
        .id(item.id)
        .flex()
        .items_center()
        .gap(px(12.))
        .rounded(px(10.))
        .px(px(10.))
        .py(px(8.))
        .child(
            div()
                .w(px(30.))
                .h(px(30.))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(16.))
                .font_weight(FontWeight::MEDIUM)
                // File-type icon color is IDENTITY, not themed — same
                // convention `crate::palette`'s own header comment
                // documents for dock/app-tile colors (a folder is blue, an
                // xlsx is green, regardless of theme).
                .text_color(theme::alpha(item.glyph_hex, 1.0))
                // ICON-1: the board's own file-type icons — a filled blue
                // folder, and a green spreadsheet with white rules over it
                // (two stacked layers, since gpui paints one SVG in one
                // color; see `crate::icons`' header comment). Both keep the
                // row's IDENTITY `glyph_hex` rather than a theme token, per
                // the note just above. Falls back to the "夾"/"表"
                // placeholder if an asset goes missing.
                .child(icons::icon_or_glyph(
                    &icons::launcher_file_layers(item.id, item.glyph_hex, palette).unwrap_or_default(),
                    20.,
                    item.glyph,
                )),
        )
        .child(div().flex_1().text_size(px(13.5)).child(item.label))
        .child(div().text_size(px(11.)).text_color(theme::alpha(palette.text_faint, 1.0)).child(item.meta))
}

fn footer(palette: ShellPalette) -> Div {
    // Launcher.dc.html: border-top `#f0f0f2` light / `rgba(255,255,255,
    // 0.08)` dark. Bg: `#fbfbfb` light (bespoke, no matching token) /
    // `#18181b` dark — the exact `surface` token (an asymmetry the board
    // itself draws: light picked an off-white one shade past `SURFACE`,
    // dark reused the token verbatim).
    let border_color = if palette.is_dark() { theme::alpha(0xffffff, 0.08) } else { theme::alpha(0xf0f0f2, 1.0) };
    let bg = if palette.is_dark() { theme::alpha(palette.surface, 1.0) } else { theme::alpha(0xfbfbfb, 1.0) };
    div()
        .flex()
        .items_center()
        .justify_between()
        .px(px(20.))
        .py(px(10.))
        .border_t_1()
        .border_color(border_color)
        .bg(bg)
        .text_size(px(11.))
        .text_color(theme::alpha(palette.text_faint, 1.0))
        .child(div().child(fake_data::LAUNCHER_FOOTER_LEFT))
        .child(div().child(fake_data::LAUNCHER_FOOTER_RIGHT))
}

// ── A1 result-loopback (2026-08-24): "Enter 交辦" submits a real task ─────
//
// `fake_data::LAUNCHER_DELEGATE_HINT` has said "Enter 交辦" since Shell-S0
// round 2, and it lied: pressing Enter here did nothing (`ShellView::
// on_oobe_next`'s own doc comment, before this round: "A no-op outside OOBE
// — Home has no Enter binding of its own"). This is the other end of the
// TODO line A1 named: "「結果回流」（交辦結果推回通道）整塊未動" started
// with nothing to loop back FROM either — a typed delegation had no path to
// the gateway at all. `crate::main::ShellView::on_oobe_next` now calls
// `try_submit_delegate` before falling through to its OOBE-only logic (see
// that fn's own updated doc comment).
//
// The delegate CARD above (`delegate_section`) is still the fixed demo
// preview — this only wires the text FIELD's Enter key, not a live
// "here is what I'm about to delegate" plan preview (that needs an
// NL-understanding backend this round does not build; see this file's
// header comment on the card staying demo content). What IS real: the
// typed sentence becomes the `description` of an actual `goal_mode` task
// (`gateway_client::create_goal`, the exact `tasks.goal_create` RPC the
// dashboard's own AssignSheet uses), assigned to this session's resolved
// default agent (`gateway_client::pick_default_agent` — the org's
// `role: "main"` agent, or the first one reachable), and handed to
// `ShellView.task_results` (`crate::task_result::TaskResultTracker`) to
// watch for a terminal state. `overlay::notifications_apps`'s A4 section
// already established "no picker, no plan preview, just submit the text" —
// wait, no, that section reads existing data; the precedent this borrows is
// `overlay/notifications.rs::trigger_refresh_if_stale`'s own thread + mpsc +
// `cx.spawn` bridge shape, reused verbatim below.

/// `overlay/notifications.rs::POLL_INTERVAL`'s exact value and reasoning —
/// the "check the mpsc channel" tick for this bridge, not a fetch cadence.
const SUBMIT_BRIDGE_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Attempts to submit the Launcher's typed query as a delegation. Returns
/// `true` when Enter's press should be considered HANDLED (the caller must
/// not fall through to any other Enter meaning) — which is also `true`
/// while a previous submit is still in flight, so a second Enter cannot
/// fire a duplicate `tasks.goal_create` while quietly doing nothing else
/// either. `false` means "this wasn't a delegate-submit situation at all"
/// (Launcher isn't open, the install-confirmation sheet is armed and owns
/// Enter's meaning instead, or the query is empty) — the caller's cue to
/// keep evaluating its other Enter branches.
pub(crate) fn try_submit_delegate(view: &mut ShellView, window: &mut gpui::Window, cx: &mut Context<ShellView>) -> bool {
    if view.surface.overlay() != Some(crate::surface::Overlay::Launcher) {
        return false;
    }
    // The install-confirmation sheet REPLACES this panel while armed (see
    // `render`'s own comment) — Enter must not race a "start installing
    // 2.4GB" decision the operator hasn't made against a delegate submit
    // they also haven't made.
    if view.overlay_ui.install_gate.is_some() {
        return false;
    }
    let query = view.launcher_query_field.field.read(cx).content(cx);
    let description = query.trim().to_string();
    if description.is_empty() {
        return false;
    }
    if !view.task_results.begin_submit() {
        // Already submitting — swallow this Enter (still "handled": no
        // other Enter meaning should fire either) rather than starting a
        // second `tasks.goal_create` for the same typed sentence.
        return true;
    }

    // Optimistic close: same UX `install_gate`'s confirm click already
    // gives (the panel dismisses immediately, the RESULT arrives later,
    // asynchronously, as a notification card — see `apply_submit_outcome`'s
    // failure branch for what happens if the gateway actually says no).
    view.surface.close();
    view.settle_launcher_query(window, cx);

    let existing_jwt = view.task_results.session_jwt().map(str::to_string);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(submit_once(existing_jwt, description));
    });
    cx.spawn(async move |weak, cx| loop {
        match rx.try_recv() {
            Ok(outcome) => {
                let _ = weak.update(cx, |view, cx| {
                    apply_submit_outcome(view, outcome);
                    cx.notify();
                });
                break;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => break,
        }
        cx.background_executor().timer(SUBMIT_BRIDGE_POLL_INTERVAL).await;
    })
    .detach();
    true
}

/// What one background-thread submit attempt settled as.
enum SubmitOutcome {
    Created { agent_id: String, task_id: String, title: String },
    /// `agents.list` succeeded but came back empty — a deployment with no
    /// reachable agent at all. Distinct from a plain RPC failure so the
    /// posted card can say the honest, specific thing rather than a generic
    /// "try again" that would never stop failing.
    NoAgent,
    Failed(gateway_client::GatewayError),
}

/// Runs entirely on a background `std::thread` — never called from gpui's
/// own executor, same contract every other blocking call in this crate
/// documents. `new_jwt` mirrors `overlay/notifications.rs::fetch_once`'s own
/// `Some` only when THIS call bootstrapped a fresh session.
fn submit_once(existing_jwt: Option<String>, description: String) -> (Option<String>, SubmitOutcome) {
    let (jwt, new_jwt) = match existing_jwt {
        Some(jwt) => (jwt, None),
        None => match gateway_client::bootstrap_local_session() {
            Ok(jwt) => (jwt.clone(), Some(jwt)),
            Err(e) => return (None, SubmitOutcome::Failed(e.into())),
        },
    };
    let agents = match gateway_client::list_agents(&jwt) {
        Ok(a) => a,
        Err(e) => return (new_jwt, SubmitOutcome::Failed(e.into())),
    };
    let Some(agent) = gateway_client::pick_default_agent(&agents) else {
        return (new_jwt, SubmitOutcome::NoAgent);
    };
    let agent_id = agent.id.clone();
    match gateway_client::create_goal(&jwt, &agent_id, &description) {
        Ok(created) => {
            // `tasks.goal_create` truncates the title from the description
            // at 60 chars server-side; an empty title would only happen for
            // a description that is somehow all-whitespace after the
            // gateway's own `trim()` — already excluded by the `is_empty()`
            // guard in `try_submit_delegate` for the CLIENT-typed text, but
            // kept here as a defensive fallback rather than ever showing an
            // untitled card.
            let title = if created.title.is_empty() { description } else { created.title };
            (new_jwt, SubmitOutcome::Created { agent_id, task_id: created.task_id, title })
        }
        Err(e) => (new_jwt, SubmitOutcome::Failed(e.into())),
    }
}

/// Applies one settled submit attempt: persists a freshly-bootstrapped
/// session, arms the watch on success, or posts an honest failure card —
/// the operator pressed Enter and the panel already closed, so a silent
/// failure here would be indistinguishable from "it worked", exactly the
/// failure mode 5.誠實回報 forbids.
fn apply_submit_outcome(view: &mut ShellView, outcome: (Option<String>, SubmitOutcome)) {
    let (new_jwt, result) = outcome;
    if let Some(jwt) = new_jwt {
        view.task_results.apply_session(jwt);
    }
    match result {
        SubmitOutcome::Created { agent_id, task_id, title } => {
            if crate::diag_enabled() {
                eprintln!("[launcher] delegate submitted: agent={agent_id} task={task_id} title={title:?}");
            }
            view.task_results.apply_submit_ok(agent_id, task_id, title);
        }
        SubmitOutcome::NoAgent => {
            view.task_results.apply_submit_err();
            eprintln!("[launcher] delegate submit refused: no agent is reachable from this session");
            post_submit_failure_card(view, t(Locale::ZhTw, Key::LauncherDelegateNoAgent));
        }
        SubmitOutcome::Failed(e) => {
            view.task_results.apply_submit_err();
            eprintln!("[launcher] delegate submit failed: {e:?}");
            post_submit_failure_card(view, t(Locale::ZhTw, Key::LauncherDelegateSubmitFailed));
        }
    }
}

/// Posts a shell-originated "couldn't even submit" card through the SAME
/// D6 notification centre a terminal task result uses
/// (`crate::task_result`'s own consumer in `main.rs`) — the operator has
/// already watched the Launcher close, so this is the only honest way left
/// to tell them Enter did not actually work. No `system_task` id: there is
/// no task to retry FROM (it was never created), so no action button is
/// offered — dismissing it is the whole interaction.
fn post_submit_failure_card(view: &mut ShellView, message: &str) {
    view.notify_center.post_system(
        crate::task_result::NOTIFY_APP_NAME,
        t(Locale::ZhTw, Key::LauncherDelegateSubmitFailedTitle),
        message,
        crate::notifyd::Urgency::Normal,
        Vec::new(),
        None,
    );
}
