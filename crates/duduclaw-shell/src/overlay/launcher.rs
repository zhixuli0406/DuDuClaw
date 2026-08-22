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
// section: `render()` now takes a live `query: &str` (owned by `ShellView.
// overlay_ui.launcher_query`, typed via `main.rs`'s root `on_key_down`
// listener — the exact same "plain `on_key_down`, printable `key_char`
// appends, `backspace` pops, no IME composition" pattern `duduclaw-native-
// gui/src/text_field.rs`'s own header comment already documents and
// accepts as an honest gap, not this file's own invention) and
// `apps_section` filters `crate::apps::search(query)` — real substring
// search over `fake_data::DOCK_APPS` (now doubling as this crate's whole
// app registry, see that const's own doc comment), not two hardcoded
// canned rows tied to one fixed demo scenario (the old `LauncherAppResult`/
// `LAUNCHER_APP_RESULTS`, removed). A result row with a known `flatpak_id`
// is a REAL click-to-launch button (`crate::apps::launch`, `flatpak run
// <id>`) — today that's exactly one entry (`browser`/Chromium, see
// `fake_data::DOCK_APPS`'s own header comment for why); every other row
// stays a non-interactive, honestly-labeled stub, same "only wire what's
// actually real" convention `home/home_dock.rs`'s Round 3 established for
// the dock. The delegate card and the files section stay untouched static
// DEMO content (task brief: "既有交辦框並存、交辦優先序不變") — see
// `render()`'s own comment below for why they only show in the EMPTY-query
// state rather than trying to (dishonestly) react to arbitrary typed text
// with no real NL-delegation/file-search backend behind them yet.
//
// ── Install confirmation gate, WP-A4-4 (2026-08-22) ───────────────────────
// A3 left INSTALL as stated debt: a result row could LAUNCH an app it
// already had, and that was all. A row whose registry entry names both a
// `flatpak_id` and a `flatpak_remote` now also carries a small secondary
// "安裝" chip. It does not install anything — it arms a confirmation sheet
// (`overlay::install_gate`, a pure state machine with its own tests) that
// REPLACES this panel's result list until the operator answers, naming the
// app, the remote, the download size (or an honest "無法取得"), and where it
// lands. Cancel drops the gate and nothing ever ran. See that module's
// header comment for the honesty rules the type itself enforces, and
// `crate::apps`'s "Install" section for why every install argv must carry
// `--installation=data`.
//
// Icon glyphs: same "single CJK character, no `gpui::svg()`" convention
// `home.rs`'s header comment establishes (this codebase has no
// `gpui::svg()` usage anywhere, and the bundled font stack has no
// guaranteed pictographic glyph coverage) — the board's own search-icon SVG
// is dropped rather than risk a tofu box, same as `home.rs`'s menu-bar bell.
// App-result rows reuse `fake_data::DOCK_APPS`' own glyph+gradient choices
// verbatim since the design board's Files/Mail rows use the identical icons.
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
use crate::fake_data::{self, DockApp, VerifiedTier};
use crate::i18n::{t, Key, Locale};
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

pub(super) fn render(ui: &OverlayUiState, palette: ShellPalette, cx: &mut Context<ShellView>) -> Stateful<Div> {
    let query = ui.launcher_query.as_str();
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
    panel = panel.child(query_row(query, palette));
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
        panel = panel.child(delegate_section(palette)).child(apps_section(query, palette, cx)).child(files_section(palette));
    } else {
        panel = panel.child(apps_section(query, palette, cx));
    }
    panel.child(footer(palette))
}

fn query_row(query: &str, palette: ShellPalette) -> Div {
    // Launcher.dc.html: border-bottom `#f0f0f2` light / `rgba(255,255,255,
    // 0.08)` dark.
    let border_color = if palette.is_dark() { theme::alpha(0xffffff, 0.08) } else { theme::alpha(0xf0f0f2, 1.0) };
    let text_color = if query.is_empty() { theme::alpha(palette.text_faint, 1.0) } else { theme::alpha(palette.foreground, 1.0) };
    // Empty query shows the placeholder (new chrome — see this file's
    // header comment on why it routes through `crate::i18n` rather than a
    // plain literal, same boundary `home.rs::ticker_text` already draws).
    // `Locale::ZhTw` is hardcoded, not read from a real preference — Home/
    // overlay locale selection is still a documented future step (see
    // `crate::i18n`'s own header comment), same as every other `t(Locale::
    // ZhTw, ...)` call site in this crate today.
    let display: String =
        if query.is_empty() { t(Locale::ZhTw, Key::LauncherSearchPlaceholder).to_string() } else { query.to_string() };
    div()
        .flex()
        .items_center()
        .gap(px(12.))
        .px(px(22.))
        .py(px(18.))
        .border_b_1()
        .border_color(border_color)
        .child(div().text_size(px(17.)).font_weight(FontWeight::MEDIUM).text_color(text_color).child(display))
        // The blinking text cursor — static bar, no actual blink animation
        // this round (unchanged limitation from before WP-A3; only the text
        // beside it went live).
        .child(div().w(px(2.)).h(px(22.)).bg(theme::alpha(palette.brand, 1.0)))
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

/// The Launcher's "app" result category — WP-A3: real `crate::apps::search`
/// over `fake_data::DOCK_APPS`, not a fixed two-row demo (see this file's
/// header comment). `cx: &mut Context<ShellView>` threaded through via a
/// plain `for` loop, not `.iter().map(...)` — `&mut Context<V>` isn't
/// `Copy`, same wall `overlay/notifications.rs`'s own header comment
/// documents and works around identically for its approval cards.
fn apps_section(query: &str, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    let results = crate::apps::search(query);
    let mut section = div().pt(px(6.)).pb(px(4.)).flex().flex_col().child(section_label(fake_data::LAUNCHER_SECTION_APPS, palette));
    if results.is_empty() {
        section = section.child(
            div()
                .px(px(22.))
                .py(px(8.))
                .text_size(px(12.5))
                .text_color(theme::alpha(palette.text_faint, 1.0))
                .child(t(Locale::ZhTw, Key::LauncherNoAppResults)),
        );
        return section;
    }
    let mut rows = div().flex().flex_col().px(px(12.));
    for item in results {
        rows = rows.child(app_result_row(item, palette, cx));
    }
    section.child(rows)
}

fn app_result_row(item: &'static DockApp, palette: ShellPalette, cx: &mut Context<ShellView>) -> Stateful<Div> {
    // Same "white/near-white bordered tile -> dark gradient" exception
    // `home/home_dock.rs::dock_app` applies — see `crate::palette`'s own
    // header comment. Non-bordered (colored) icons keep their identity
    // gradient in both themes.
    let neutral_dark = item.bordered && palette.is_dark();
    let (gradient_top, gradient_bottom) =
        if neutral_dark { (palette.neutral_tile_top, palette.neutral_tile_bottom) } else { (item.gradient_top, item.gradient_bottom) };

    let mut icon = div()
        .w(px(30.))
        .h(px(30.))
        .rounded(px(8.))
        .bg(linear_gradient(180.0, linear_color_stop(rgb(gradient_top), 0.0), linear_color_stop(rgb(gradient_bottom), 1.0)))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(13.))
        .font_weight(FontWeight::MEDIUM)
        // The pre-existing light-only `0.14` already approximates the
        // board's own two distinct per-icon values (`.12` neutral / `.16`
        // colored, averaging to `.14`) as one flat number — kept exactly
        // (light must stay byte-identical). Dark mirrors the same
        // averaging strategy over ITS OWN two board values (`.30`/`.35`
        // -> `.32`), rather than discriminating by `item.bordered` (which
        // light's own code never did either).
        .shadow(palette.icon_shadow(0.14, 0.32))
        .child(item.glyph);
    icon = if neutral_dark {
        icon.text_color(theme::alpha(0xb0b0b8, 1.0))
    } else {
        icon.text_color(theme::alpha(palette.foreground, if item.bordered { 0.55 } else { 1.0 }))
    };
    if item.bordered {
        let border_color = if palette.is_dark() { palette.neutral_tile_border } else { theme::alpha(theme::light::SURFACE_BORDER, 1.0) };
        icon = icon.border_1().border_color(border_color);
    }

    let mut row = div()
        .id(item.id)
        .flex()
        .items_center()
        .gap(px(12.))
        .rounded(px(10.))
        .px(px(10.))
        .py(px(8.))
        .child(icon)
        .child(div().flex_1().text_size(px(13.5)).child(item.label));

    // WP-A4-4: a row this shell could actually INSTALL gets a secondary
    // button next to its Verified badge. It is deliberately a separate,
    // explicitly-labelled control rather than a second meaning for the row
    // click — installing and launching are not interchangeable, and the row
    // click has meant "launch" since A3. The button only ARMS the
    // confirmation sheet; nothing is downloaded or written until the sheet
    // itself is confirmed (`overlay::install_gate`).
    if item.flatpak_id.is_some() && item.flatpak_remote.is_some() {
        row = row.child(install_button(item, palette, cx));
    }
    row = row.child(verified_badge(item.verified, palette));

    // Only a row with a real launch command is a real button — every other
    // entry stays an honest, non-interactive stub (see this file's header
    // comment). `item` is `&'static DockApp` (a reference into `fake_data::
    // DOCK_APPS`), `Copy`, so it captures into `move` with no lifetime/clone
    // ceremony.
    if item.flatpak_id.is_some() {
        let on_click = cx.listener(move |_view, _ev, _window, _cx| {
            if crate::diag_enabled() {
                eprintln!("[hit] launcher app row '{}' -> launch", item.id);
            }
            crate::apps::launch(item);
        });
        row = row.cursor_pointer().hover(|style| style.bg(theme::alpha(palette.surface_hover, 1.0))).on_click(on_click);
    }
    row
}

// ── Install confirmation gate (WP-A4-4, 2026-08-22) ─────────────────────

/// The secondary "安裝" control on an installable result row. Arms the
/// confirmation sheet and kicks off the size probe — it does NOT install
/// anything. Styled as a quiet outlined chip (same neutral treatment
/// `overlay/notifications.rs`'s own secondary buttons use) so it never
/// competes visually with the row itself, which still means "launch".
fn install_button(item: &'static DockApp, palette: ShellPalette, cx: &mut Context<ShellView>) -> Stateful<Div> {
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
/// thread. A `DockApp` with no `flatpak_id`/`flatpak_remote` produces no
/// gate at all (`InstallGate::open` returns `None`) — the button is only
/// rendered for entries that have both, so this is belt-and-suspenders.
fn open_install_gate(item: &'static DockApp, view: &mut ShellView, cx: &mut Context<ShellView>) {
    let Some(gate) = InstallGate::open(item, crate::apps::INSTALL_DESTINATION_LABEL.to_string()) else {
        return;
    };
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
                // The app's OWN existing glyph — no new pictogram is
                // introduced here; this crate has zero emoji and no
                // `gpui::svg()` usage to draw a stroke icon with (see
                // `home.rs`'s header comment on that gap).
                .child(div().text_size(px(20.)).text_color(theme::alpha(palette.text_secondary, 1.0)).child(gate.glyph))
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
                .child(item.glyph),
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
