// Home surface — goal cards row, activity shelf, and dock. Split out of
// `home.rs` per that file's header comment (one screen, two files — same
// convention `duduclaw-native-gui/src/screens/shell.rs`'s own siblings
// establish). `pub(super)`, not `pub`: nothing outside `home.rs` needs
// these directly.
//
// Shell-S1 (2026-08-20): every fn here now takes a
// `palette: ShellPalette` parameter — see `home.rs`'s own header comment
// (unchanged reasoning, just extended to this sibling file) and `crate::
// palette`'s header comment for the Home/overlay light/dark token mapping.
//
// WP-comp-shell-ipc (2026-08-22): `dock()` and `dock_app()` now take a
// `running_windows: &RunningWindowsFeed` parameter — the real "執行中視窗"
// list this round wires in, replacing the "其他 dock icon 維持靜態" gap
// Round 3/WP-A3 left (a dock icon with a real `flatpak_id` used to only
// ever LAUNCH a fresh instance, with no concept of "is one already
// running"). `dock()` also dispatches the background poll that keeps that
// feed warm (`schedule_running_windows_poll`, same `std::thread::spawn` +
// `mpsc` + `cx.spawn` bridge `overlay/notifications.rs` already established
// for the gateway) — see that fn's own doc comment for why it checks
// staleness immediately on every render pass rather than waiting out one
// interval first, unlike that module's own `schedule_stale_check`.
//
// APP-1 (2026-08-22): the dock's icons are the REAL apps on this machine.
// They used to be `fake_data::DOCK_APPS` — six design-board icons, five of
// which had no app behind them at all, so on a real appliance the dock was
// a row of buttons that could not open anything. `dock()` now renders
// `crate::apps::feed::InstalledAppsFeed`'s first `DOCK_MAX_APPS` entries
// (deterministically ordered by `apps::installed::merge`, so the row does
// not reshuffle between refreshes) and dispatches that feed's own poll
// alongside the running-windows one. A machine with nothing installed gets
// a dock with no app tiles — honest, and never a fallback to canned icons.

use std::sync::mpsc;
use std::time::Duration;

use gpui::{div, linear_color_stop, linear_gradient, prelude::*, px, rgb, BoxShadow, Context, Div, FontWeight, Stateful};

use duduclaw_native_gui::theme;

use crate::apps::catalog;
use crate::apps::feed::InstalledAppsFeed;
use crate::apps::icon_theme;
use crate::apps::installed::InstalledApp;
use crate::comp_client;
use crate::fake_data::{self, AgentDockStatus, GoalDot};
use crate::home::running_windows::RunningWindowsFeed;
use crate::overlay::notifications_feed::NotificationsFeed;
use crate::overlay::task_progress_feed::TaskProgressFeed;
use crate::palette::ShellPalette;
use crate::surface::Overlay;
use crate::ShellView;

/// How many real apps the dock shows. The dock is a fixed-width row across
/// the bottom of a 1440px window, so it cannot grow with the machine's app
/// count — the Launcher (`cmd-k`) is the complete list, and this is the
/// quick row. Eight matches what the design board's own dock allotted to
/// apps before the divider (six app tiles plus headroom) without the row
/// running into the agent avatars.
const DOCK_MAX_APPS: usize = 8;

/// The dock app tile's corner radius (`Main.dc.html`, unchanged since the
/// board landed). Named rather than inlined only because ICON-2 needs the
/// same number twice: once on the tile itself, once on the clip a
/// full-bleed square raster is drawn into (`crate::icons::
/// app_icon_element`). The tile's SIZE lives in `apps::icon_theme` with the
/// rest of the ladder, since that is what decides which icon file gets
/// resolved for it.
const DOCK_TILE_RADIUS_PX: f32 = 10.;

// ── Goal cards row ─────────────────────────────────────────────────────

pub(super) fn goal_cards_row(palette: ShellPalette) -> Div {
    div()
        .absolute()
        .top(px(360.))
        .left(px(0.))
        .right(px(0.))
        .flex()
        .justify_center()
        .gap(px(14.))
        .children(fake_data::GOAL_CARDS.iter().map(move |card| goal_card(card, palette)))
}

fn goal_card(card: &fake_data::GoalCard, palette: ShellPalette) -> Stateful<Div> {
    let dot = match card.dot {
        GoalDot::Outline(kind) => {
            div().w(px(10.)).h(px(10.)).rounded(px(10.)).border(px(2.5)).border_color(theme::alpha(palette.badge_accent(kind), 1.0))
        }
        GoalDot::Filled(kind) => div().w(px(10.)).h(px(10.)).rounded(px(10.)).bg(theme::alpha(palette.badge_accent(kind), 1.0)),
    };

    // Main.dc.html: bg `#ffffff` light / `#1e1e21` dark — `surface_raised`
    // (see `home.rs`'s `menu_bar_ticker` comment for why, not `surface`).
    // Border: opaque `border()` light (matches the board's own `#ececef`
    // exactly) / `rgba(255,255,255,0.10)` dark (bespoke, not `border()`'s
    // own dark 0.06 alpha). Both arms are typed `Hsla` — the dark arm goes
    // through `.into()` (Rgba -> Hsla, the exact same conversion `border()`
    // itself already performs internally) so the LIGHT arm can stay
    // `palette.border()`'s own value completely untouched, with no
    // Rgba<->Hsla round trip anywhere near it (this crate's "light stays
    // byte-identical" constraint applies to that value, not to dark's).
    let border_color: gpui::Hsla = if palette.is_dark() { theme::alpha(0xffffff, 0.10).into() } else { palette.border() };
    // Shadow: light matches `palette.surface_shadow()` exactly (`0 1px 2px
    // rgba(15,23,42,.04), 0 1px 1px rgba(15,23,42,.03)`, byte-for-byte the
    // same recipe); dark's own literal (`rgba(0,0,0,.30)`/`rgba(0,0,0,.20)`)
    // is deeper than that token's calibrated `.2`/`.16` — accepted as the
    // same small approximation gap `ShellPalette::floating_shadow`'s own
    // doc comment already documents for the overlay panels, rather than
    // hand-tuning a bespoke shadow for this one card too.

    div()
        .id(card.id)
        .w(px(300.))
        .bg(theme::alpha(palette.surface_raised, 1.0))
        .border_1()
        .border_color(border_color)
        .rounded(px(theme::RADIUS_XL))
        .px(px(16.))
        .py(px(14.))
        .shadow(palette.surface_shadow())
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(dot)
                .child(
                    div()
                        .flex_1()
                        .text_size(px(14.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::alpha(palette.foreground, 1.0))
                        .child(card.title),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(palette.badge_text(card.badge_kind))
                        .bg(palette.badge_bg(card.badge_kind))
                        .rounded(px(999.))
                        .px(px(8.))
                        .py(px(2.))
                        .child(card.badge_label),
                ),
        )
        .child(
            div()
                .text_size(px(12.))
                .text_color(theme::alpha(palette.muted_foreground, 1.0))
                .child(card.meta),
        )
}

// ── Activity shelf ──────────────────────────────────────────────────────

pub(super) fn activity_shelf(palette: ShellPalette) -> Div {
    // Main.dc.html: bg `rgba(255,255,255,0.7)` light / `rgba(24,24,27,0.70)`
    // dark — plain `surface` (NOT `surface_raised` — this glass pill, unlike
    // the dock/composer/ticker, deliberately sits on the DARKER menu-bar-
    // level layer, see `crate::palette`'s own header comment on why the two
    // are not interchangeable). Border: light `SURFACE_BORDER` at `0.8` /
    // dark `rgba(255,255,255,0.10)`.
    let border_color = if palette.is_dark() { theme::alpha(0xffffff, 0.10) } else { theme::alpha(theme::light::SURFACE_BORDER, 0.8) };
    // Row text: light `#52525c` / dark `#d4d4d8` — text ladder rank 2,
    // matches `text_secondary` exactly (see `crate::palette`'s own header
    // comment for the ladder).
    let mut row = div()
        .id("shell-activity-shelf")
        .flex()
        .items_center()
        .gap(px(18.))
        .bg(theme::alpha(palette.surface, 0.7))
        .border_1()
        .border_color(border_color)
        .rounded(px(999.))
        .px(px(18.))
        .py(px(6.))
        .text_size(px(12.))
        .text_color(theme::alpha(palette.text_secondary, 1.0))
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::alpha(palette.foreground, 1.0))
                .child("今日"),
        );

    // Main.dc.html: the "·" separator is `#d4d4d8` in LIGHT and `#52525c`
    // in DARK — the opposite of `text_secondary`'s own light/dark values
    // (light board deliberately reuses dark's mid-gray here, and vice
    // versa, to keep this specific dot equally faint against either
    // background) — NOT the ladder inversion `crate::palette`'s header
    // comment documents for ranks 3/4 (that's a different, real semantic
    // token; this is a one-off "borrow the other theme's shade" choice for
    // exactly one decorative glyph), so it's kept as its own inline swap
    // rather than aliased to `text_secondary`.
    let separator_hex = if palette.is_dark() { 0x52525c } else { 0xd4d4d8 };

    // Plain imperative loop, not `.children(iter().map(...))`: needs to
    // interleave a "·" separator BETWEEN entries (not after the last one),
    // which a single `.map()` can't express as cleanly. No `Context`/`cx`
    // capture involved either way (this file is presentation-only, see
    // `home.rs`'s header comment) — `duduclaw-native-gui/src/main.rs`'s own
    // gotcha about `&mut Context<V>` not being `Copy` in a repeated closure
    // doesn't apply here.
    for (i, line) in fake_data::ACTIVITY_SHELF.iter().enumerate() {
        if i > 0 {
            row = row.child(div().text_color(theme::alpha(separator_hex, 1.0)).child("·"));
        }
        row = row.child(div().child(*line));
    }

    div().absolute().bottom(px(120.)).left(px(0.)).right(px(0.)).flex().justify_center().child(row)
}

// ── Dock ─────────────────────────────────────────────────────────────────

/// The dock as `ChromeMode::SingleFullscreen` composes it — one child of the
/// one full-output window (`home::render`). No input region is involved on
/// this path: the window it lives in paints the whole screen, so there are no
/// unpainted pixels for it to wrongly claim.
pub(super) fn dock(
    palette: ShellPalette,
    running_windows: &RunningWindowsFeed,
    installed: &InstalledAppsFeed,
    notifications: &NotificationsFeed,
    task_progress: &TaskProgressFeed,
    cx: &mut Context<ShellView>,
) -> Div {
    dock_container(palette, running_windows, installed, notifications, task_progress, cx)
}

/// D9-bug (2026-08-24): the dock as `ChromeMode::LayerSurfaces` composes it —
/// the SAME container, plus the one thing that only makes sense when the dock
/// owns a surface of its own: restricting that surface's pointer input to the
/// pill it actually paints.
///
/// The dock's layer surface is anchored `bottom+left+right`, so it is as wide
/// as the output (1280 px on the appliance) and `DOCK_HEIGHT` tall, while the
/// pill inside it is roughly a third that wide. Wayland's default input region
/// is the whole surface, so before this the dock silently ate every click that
/// landed in the transparent band either side of the pill — Chromium's
/// first-run ToS "Accept" button among them. `on_children_prepainted` hands
/// over the pill's REAL laid-out bounds (this container's only child) in
/// window coordinates, which is exactly what `Window::set_input_region` wants,
/// so the region tracks the pill widening and narrowing as apps come and go
/// with no second copy of the layout arithmetic to keep in sync. See
/// `crate::chrome::input_region`'s header comment for why this is applied from
/// prepaint and why it is cached.
///
/// Only ever called for the dedicated `duduclaw-shell-dock` surface
/// (`home::render_dock` is its one caller, and that is only reached from
/// `chrome::windows::render_surface_content`'s `ChromeSurface::Dock` arm), so
/// this can never narrow the input region of a window that paints more than
/// the pill.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(super) fn dock_surface(
    palette: ShellPalette,
    running_windows: &RunningWindowsFeed,
    installed: &InstalledAppsFeed,
    notifications: &NotificationsFeed,
    task_progress: &TaskProgressFeed,
    cx: &mut Context<ShellView>,
) -> Div {
    dock_container(palette, running_windows, installed, notifications, task_progress, cx).on_children_prepainted(|children, window, _cx| {
        let wanted = crate::chrome::input_region::shown_region_for(&children);
        crate::chrome::input_region::apply(window, crate::chrome::input_region::RegionSlot::Dock, wanted);
    })
}

fn dock_container(
    palette: ShellPalette,
    running_windows: &RunningWindowsFeed,
    installed: &InstalledAppsFeed,
    notifications: &NotificationsFeed,
    task_progress: &TaskProgressFeed,
    cx: &mut Context<ShellView>,
) -> Div {
    // WP-comp-shell-ipc: keeps `running_windows` warm for as long as Home
    // keeps rendering (which is continuously, unlike the Notifications
    // overlay's open/closed panel — see this file's own header comment).
    schedule_running_windows_poll(cx);
    // APP-1: the same one-shot-per-render-pass shape for the app scan. Home
    // renders continuously from window-open and the Launcher renders on top
    // of Home, so this ONE dispatch keeps the feed warm for both surfaces —
    // the Launcher deliberately does not schedule its own.
    schedule_installed_apps_poll(cx);
    // A4 (2026-08-24): arms `overlay::notifications`'s single-slot stale
    // timer from the dock too, not only from the Notifications panel /
    // lockscreen (that fn's own doc comment names this gap explicitly:
    // "Home 本身沒有 arm 它... 是後續範圍"). `try_arm_stale_timer` is a
    // single-claim guard (see that field's own doc comment) — a render pass
    // that finds it already armed by the panel or the lockscreen is a
    // one-comparison no-op, so calling this unconditionally on every dock
    // render costs nothing extra. This is what lets the dock badge below
    // show real numbers from first paint, on a machine that has never
    // opened Notifications or locked once.
    crate::overlay::notifications::schedule_stale_check(cx);

    // Main.dc.html: bg `rgba(255,255,255,0.78)` light / `rgba(30,30,33,
    // 0.78)` dark — `surface_raised` (matches `composer`'s own reasoning in
    // `home.rs`). Border: light `SURFACE_BORDER` at `0.9` / dark `rgba(255,
    // 255,255,0.12)`. Shadow: bespoke in both themes (`0 8px 24px rgba(15,
    // 23,42,.08)` light / `0 8px 24px rgba(0,0,0,.40)` dark).
    let border_color = if palette.is_dark() { theme::alpha(0xffffff, 0.12) } else { theme::alpha(theme::light::SURFACE_BORDER, 0.9) };
    let shadow = if palette.is_dark() {
        vec![BoxShadow::new(px(0.), px(8.), rgb(0x000000).opacity(0.40).into()).blur_radius(px(24.))]
    } else {
        vec![BoxShadow::new(px(0.), px(8.), rgb(0x0f172a).opacity(0.08).into()).blur_radius(px(24.))]
    };

    let mut row = div()
        .id("shell-dock")
        .flex()
        .items_center()
        .gap(px(10.))
        .bg(theme::alpha(palette.surface_raised, 0.78))
        .border_1()
        .border_color(border_color)
        .rounded(px(22.))
        .px(px(14.))
        .py(px(10.))
        .shadow(shadow);

    // APP-1: every tile here is a real, installed, launchable app. An empty
    // machine (or a scan that has not settled yet) simply contributes no
    // tiles — the divider, the agent avatars and the settings tile still
    // render, so the dock never looks broken, it just has nothing to launch
    // yet. That is the honest state, not a reason to draw canned icons.
    for app in installed.apps().iter().take(DOCK_MAX_APPS) {
        row = row.child(dock_app(app, palette, running_windows, cx));
    }
    row = row.child(dock_divider(palette));
    for agent in fake_data::DOCK_AGENTS {
        row = row.child(dock_agent(agent, palette));
    }
    row = row.child(dock_divider(palette));
    // A4 (2026-08-24): real gap fill — before this round the dock had no
    // representation at all of pending approvals or in-progress tasks
    // (`NotificationsFeed`/`TaskProgressFeed` were readable only from the
    // Notifications panel and the lockscreen). `pending_approvals`/
    // `in_progress_tasks` are cheap `usize` reads off feeds this fn already
    // keeps warm via `schedule_stale_check` above — no extra I/O.
    row = row.child(dock_task_badge(notifications.pending_count(), task_progress.count(), palette, cx));
    row = row.child(dock_settings(palette, cx));

    div().absolute().bottom(px(24.)).left(px(0.)).right(px(0.)).flex().justify_center().child(row)
}

fn dock_divider(palette: ShellPalette) -> Div {
    // Main.dc.html: opaque `SURFACE_BORDER` light / `rgba(255,255,255,0.12)`
    // dark.
    let color = if palette.is_dark() { theme::alpha(0xffffff, 0.12) } else { theme::alpha(theme::light::SURFACE_BORDER, 1.0) };
    div().w(px(1.)).h(px(34.)).bg(color)
}

fn dock_app(app: &InstalledApp, palette: ShellPalette, running_windows: &RunningWindowsFeed, cx: &mut Context<ShellView>) -> Stateful<Div> {
    // WP-comp-shell-ipc: the app's xdg-shell app_id is what's matched
    // against the live window list — `InstalledApp::xdg_app_id` (the
    // flatpak application id, or the desktop-file id) — see that fn's own
    // doc comment and `running_windows.rs`'s module doc for why this is a
    // reasonable freedesktop convention rather than a guarantee. An app
    // that sets some other app_id simply never shows a running dot; it
    // never shows a WRONG one.
    let is_running = running_windows.is_app_running(app.xdg_app_id());
    // APP-1: a real installed app has no design-board colour to lift, so
    // every dock tile renders in the neutral treatment the board already
    // uses for its own white/outline tiles (Files/Browser). Inventing a
    // per-app gradient would be decoration masquerading as identity.
    //
    // ICON-2 (2026-08-22): this tile is now the "soft mask" the research
    // prescribes (§2.4) — the container below is unchanged (same 44px, same
    // 10px radius, same `icon_shadow(0.20, 0.35)`, deliberately NOT a new
    // visual), and the app's REAL icon is drawn inside it, uncropped, at
    // 80% of the container. `crate::icons::app_icon_element` is a different
    // rendering path from every other icon in this file — see its own doc
    // comment for the `svg()` / `img()` split and why they must not merge.
    let dark = palette.is_dark();
    let (gradient_top, gradient_bottom) = if dark { (palette.neutral_tile_top, palette.neutral_tile_bottom) } else { (0xffffff, 0xedeff4) };

    // Fallback icon color: light is `SURFACE_FOREGROUND` at 0.55 alpha —
    // `SURFACE_FOREGROUND` and `FOREGROUND` share the identical value in
    // both themes (`theme.rs`'s own token table), so `palette.foreground`
    // reproduces it exactly. Dark swaps to a solid literal (`#b0b0b8`, the
    // approved canvas's own primary icon-stroke color) instead of a
    // translucent foreground tint — the translucency was only ever standing
    // in for "a lighter gray", which dark expresses with a genuinely
    // different (not just alpha-reduced) hex.
    //
    // ICON-2 keeps this pair EXACTLY as it was, now tinting the generic
    // application icon instead of the retired first-letter glyph, so a tile
    // that falls back reads at the same weight it always did. A resolved
    // third-party icon is never tinted at all — recolouring somebody's
    // brand is the one thing the research is unambiguous about.
    // Shadow: the pre-existing light-only code collapsed the board's own
    // per-family opacities to one flat `0.20` for every dock icon; kept
    // exactly (light must stay byte-identical). Dark mirrors the SAME
    // flat-approximation in the SAME direction light already picked.
    let icon_shadow = palette.icon_shadow(0.20, 0.35);
    let border_color = if dark { palette.neutral_tile_border } else { theme::alpha(theme::light::SURFACE_BORDER, 1.0) };
    let text_color = if dark { theme::alpha(0xb0b0b8, 1.0) } else { theme::alpha(palette.foreground, 0.55) };

    let mut el = div()
        .id(format!("dock-app-{}", app.id))
        .relative()
        .w(px(icon_theme::TILE_DOCK_PX))
        .h(px(icon_theme::TILE_DOCK_PX))
        .rounded(px(DOCK_TILE_RADIUS_PX))
        .bg(linear_gradient(180.0, linear_color_stop(rgb(gradient_top), 0.0), linear_color_stop(rgb(gradient_bottom), 1.0)))
        .border_1()
        .border_color(border_color)
        .flex()
        .items_center()
        .justify_center()
        .shadow(icon_shadow)
        // ICON-2: `text_size`/`font_weight`/`text_color` are gone from this
        // tile — nothing textual renders in it any more. The colour is
        // passed straight to the icon helper instead, which is the only
        // thing that ever consumed it.
        .child(crate::icons::app_icon_element(
            app.resolved_icon.as_ref().and_then(|icon| icon.for_container(icon_theme::TILE_DOCK_PX)),
            app.source,
            icon_theme::TILE_DOCK_PX,
            DOCK_TILE_RADIUS_PX,
            text_color.into(),
        ));

    // WP-A3: a small corner "Verified tier" indicator. APP-1 turned this
    // into a real lookup (`apps::catalog::verified_tier`) instead of a
    // per-row constant: an app this crate has rating evidence for gets its
    // dot, and everything else resolves to `Unrated`, which renders NO dot
    // at all — see `crate::palette::ShellPalette::verified_accent_dot`'s own
    // doc comment for why that beats drawing an empty/neutral one (the dock
    // has no room for the word "未評級" the way a Launcher row does).
    if let Some(hex) = palette.verified_accent_dot(catalog::verified_tier(app.xdg_app_id())) {
        el = el.child(
            div()
                .absolute()
                .bottom(px(1.))
                .right(px(1.))
                .w(px(9.))
                .h(px(9.))
                .rounded(px(9.))
                .bg(theme::alpha(hex, 1.0))
                .border_2()
                .border_color(theme::alpha(palette.surface_raised, 1.0)),
        );
    }

    // WP-comp-shell-ipc: macOS-dock-style "running" indicator — a small
    // dot centered BELOW the 44px icon (bottom-center, `left = (44-6)/2`),
    // deliberately not sharing a corner with the verified-tier dot above
    // (bottom-right) so the two never visually collide.
    if is_running {
        el = el.child(
            div()
                .absolute()
                .bottom(px(-4.))
                .left(px(19.))
                .w(px(6.))
                .h(px(6.))
                .rounded(px(6.))
                .bg(theme::alpha(palette.brand, 1.0)),
        );
    }

    // WP-comp-shell-ipc: a click on an ALREADY-RUNNING entry switches to it
    // (`comp_client::focus_window`) instead of blindly launching a second
    // instance — `is_running` is a snapshot from THIS render pass (same
    // "capture the feed's current read at render time" convention
    // `menu_bar_ticker`'s own `has_pending` already uses), freshened at most
    // `running_windows::POLL_INTERVAL` behind reality.
    //
    // APP-1: every dock tile is a real installed app, so every tile is a
    // real button — the "honest static stub" branch Round 3 needed is gone
    // along with the canned entries that made it necessary. `app` is
    // borrowed from the feed rather than `'static`, so the handler captures
    // owned clones.
    let launch_target = app.clone();
    let focus_query = app.xdg_app_id().to_string();
    let click_label = app.id.clone();
    let on_click = cx.listener(move |_view, _ev, _window, _cx| {
        if is_running {
            if crate::diag_enabled() {
                eprintln!("[hit] dock icon '{click_label}' -> focus_window({focus_query:?})");
            }
            // `comp_client::focus_window` is a blocking socket call (this
            // file's own header comment / `comp_client`'s own module doc) —
            // dispatched off a background thread so it never blocks gpui's
            // render thread, same "fire-and-forget from a click handler"
            // shape `crate::apps::launch` already uses for the spawn path,
            // just wrapped in a thread here because THIS call, unlike
            // `Command::spawn()`, actually waits on a reply.
            let query = focus_query.clone();
            std::thread::spawn(move || {
                if let Err(e) = comp_client::focus_window(&query) {
                    if crate::diag_enabled() {
                        eprintln!("[dock] focus_window({query:?}) failed: {e}");
                    }
                }
            });
        } else {
            if crate::diag_enabled() {
                eprintln!("[hit] dock icon '{click_label}' -> launch");
            }
            crate::apps::launch(&launch_target);
        }
    });
    el.cursor_pointer().hover(|style| style.opacity(0.85)).on_click(on_click)
}

// ── Running-windows poll (WP-comp-shell-ipc, 2026-08-22) ────────────────

/// Cadence for the background-thread <-> `cx.spawn` bridge — same value
/// `overlay/notifications.rs::POLL_INTERVAL` uses for its own gateway poll
/// bridge (this is the "check the mpsc channel" tick, NOT `running_windows::
/// POLL_INTERVAL`, which is the "how often do we actually re-fetch"
/// cadence).
const BRIDGE_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Called from `dock()` on every render pass. Unlike `overlay/
/// notifications.rs`'s `schedule_stale_check` (which waits out one full
/// `REFRESH_STALE_AFTER` before its first check, since it only exists as
/// belt-and-suspenders on top of a click-triggered `open_and_refresh` fast
/// path), Home's dock has no equivalent "just opened" event to pair a fast
/// first fetch against — Home renders continuously from window-open. So
/// this checks staleness IMMEDIATELY, every render pass; `trigger_refresh_
/// if_stale` below is cheap and no-ops instantly whenever the feed is
/// already fresh or a fetch is already in flight, so calling it every
/// render costs nothing beyond that one no-op check.
fn schedule_running_windows_poll(cx: &mut Context<ShellView>) {
    cx.spawn(async move |weak, cx| {
        let _ = weak.update(cx, |view, cx| {
            trigger_running_windows_refresh_if_stale(view, cx);
        });
    })
    .detach();
}

/// Same shape as `overlay/notifications.rs::trigger_refresh_if_stale`:
/// single-flight guarded (`RunningWindowsFeed::begin_refresh`), dispatches
/// the actual blocking `comp_client::list_windows()` call on a background
/// thread, bridges the result back via `mpsc` + a `cx.spawn` poll loop.
fn trigger_running_windows_refresh_if_stale(view: &mut ShellView, cx: &mut Context<ShellView>) {
    if !view.running_windows.is_stale() {
        return;
    }
    if !view.running_windows.begin_refresh() {
        // Already busy — the next render pass's check will catch it once
        // this one settles.
        return;
    }
    // WP-A4-4 (2026-08-22): NO `cx.notify()` here. Starting a poll changes
    // nothing the dock draws — `RunningWindowsFeed`'s only rendered output
    // is `is_app_running`, and that cannot have moved yet. The settle side
    // below now notifies only when the window list ACTUALLY changed, which
    // takes the appliance's `cage` session (no `duduclaw-comp` at all, so
    // every poll fails identically forever) from two full-window repaints
    // every three seconds down to zero. See `running_windows::
    // apply_list_ok`'s own doc comment.

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let outcome = comp_client::list_windows();
        let _ = tx.send(outcome);
    });

    cx.spawn(async move |weak, cx| loop {
        match rx.try_recv() {
            Ok(outcome) => {
                let _ = weak.update(cx, |view, cx| {
                    let changed = match outcome {
                        Ok(windows) => {
                            if crate::diag_enabled() {
                                eprintln!("[dock] list_windows ok: {} window(s): {windows:?}", windows.len());
                            }
                            view.running_windows.apply_list_ok(windows)
                        }
                        Err(e) => {
                            if crate::diag_enabled() {
                                eprintln!("[dock] list_windows failed: {e}");
                            }
                            // A failed poll leaves the last known window
                            // list in place on purpose (see `apply_list_err`
                            // there), so by definition nothing the dock
                            // draws moved — never a repaint.
                            view.running_windows.apply_list_err();
                            false
                        }
                    };
                    if changed {
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

// ── Installed-app scan poll (APP-1, 2026-08-22) ─────────────────────────

/// Called from `dock()` on every render pass, same shape as
/// `schedule_running_windows_poll` above and for the same reason: Home
/// renders continuously from window-open and has no "just opened" event to
/// pair a fast first fetch against, so the render-pass-gated immediate
/// check IS the fast-first-scan path.
///
/// **One-shot, never a self-re-arming timer.** This spawns a future that
/// does one staleness check and ends; it does NOT schedule the next one.
/// `trigger_installed_apps_refresh_if_stale` no-ops instantly whenever the
/// feed is fresh or a scan is already in flight, so calling it on every
/// render costs one comparison — while an `async loop { timer().await }`
/// per render pass would accumulate one live timer per repaint, which is
/// exactly the CPU-burn incident WP-A4-4 fixed in this crate. The same
/// discipline applies to the notify side: the settle below calls
/// `cx.notify()` only when `apply_scan` reports something visible actually
/// moved (see `apps::feed::InstalledAppsFeed::apply_scan`).
fn schedule_installed_apps_poll(cx: &mut Context<ShellView>) {
    cx.spawn(async move |weak, cx| {
        let _ = weak.update(cx, |view, cx| {
            trigger_installed_apps_refresh_if_stale(view, cx);
        });
    })
    .detach();
}

fn trigger_installed_apps_refresh_if_stale(view: &mut ShellView, cx: &mut Context<ShellView>) {
    if !view.installed_apps.is_stale() {
        return;
    }
    if !view.installed_apps.begin_refresh() {
        // Already scanning — the next render pass's check catches it once
        // this one settles.
        return;
    }
    // No `cx.notify()` here: starting a scan changes nothing that is drawn.

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        // BLOCKING: two `flatpak list` subprocesses plus a directory walk.
        // This is precisely why it does not happen on the render thread.
        let _ = tx.send(crate::apps::installed::scan());
    });

    cx.spawn(async move |weak, cx| loop {
        match rx.try_recv() {
            Ok(outcome) => {
                let _ = weak.update(cx, |view, cx| {
                    let (before_flatpak, before_desktop) = {
                        let (f, d) = view.installed_apps.source_statuses();
                        (f.cloned(), d.cloned())
                    };
                    let applied = view.installed_apps.apply_scan(outcome);
                    if applied.status_changed {
                        // Logged on CHANGE only, never every 60 seconds
                        // forever: a source that has been failing since boot
                        // is one log line, not one per refresh. Not
                        // DIAG-gated — a source that IS present and stops
                        // enumerating is something whoever reads this
                        // machine's journal needs to see.
                        let (after_flatpak, after_desktop) = view.installed_apps.source_statuses();
                        eprintln!(
                            "[apps] scan sources changed: flatpak {before_flatpak:?} -> {after_flatpak:?}, desktop {before_desktop:?} -> {after_desktop:?} ({} app(s))",
                            view.installed_apps.apps().len()
                        );
                    }
                    if applied.repaint {
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

fn dock_agent(agent: &fake_data::DockAgent, palette: ShellPalette) -> Stateful<Div> {
    // Status dot: `Running` uses the brand hue (matches its own avatar),
    // `NeedsHuman` uses `warning_dot` — NOT `badge_accent(Warning)`, see
    // `AgentDockStatus::badge_kind`'s own doc comment for why this small
    // circular dot is a different field from the badge/ring token in dark.
    let dot_hex = match agent.status {
        AgentDockStatus::Running => palette.brand,
        AgentDockStatus::NeedsHuman => palette.warning_dot,
    };

    div()
        .id(agent.id)
        .relative()
        .w(px(44.))
        .h(px(44.))
        .rounded(px(22.))
        .bg(theme::alpha(agent.bg_hex, 1.0))
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .text_size(px(15.))
                .font_weight(FontWeight::BOLD)
                .text_color(theme::alpha(palette.brand_foreground, 1.0))
                .child(agent.initial),
        )
        .child(
            div()
                .absolute()
                .bottom(px(1.))
                .right(px(1.))
                .w(px(11.))
                .h(px(11.))
                .rounded(px(11.))
                .bg(theme::alpha(dot_hex, 1.0))
                .border_2()
                // Main.dc.html: `#ffffff` light / `#1e1e21` dark —
                // `surface_raised` (the dot's border matches the DOCK
                // surface it visually sits against, not the bare `surface`
                // token).
                .border_color(theme::alpha(palette.surface_raised, 1.0)),
        )
}

/// A4 (2026-08-24) — real gap fill: "dock badge：待審批數/進行中任務數的即時
/// 徽章" (the task brief's own framing). Before this round the dock carried
/// no signal at all for either count — only `RunningWindowsFeed`'s per-app
/// running dot (comp windows, a different domain) and the two STATIC
/// `fake_data::DOCK_AGENTS` `Running`/`NeedsHuman` dots. One tile, same
/// 44px/10px-radius/neutral-gradient treatment `dock_settings` below
/// establishes (this crate has no design-board precedent for either of
/// these — the board never modeled a live task/approval count — so the
/// EXISTING settings-tile visual language is reused rather than inventing a
/// new one, same "no board precedent, reuse an existing token pair" call
/// `dock_app`'s own header comment makes for its running-indicator dot).
///
/// Clicking it opens the SAME Notifications panel the menu-bar approval
/// ticker opens (`overlay::notifications::open_and_refresh`) — this round
/// also adds that panel's "進行中任務" section (`notifications_tasks::
/// task_progress_section`), so the number on the badge and what appears
/// when you click it are the same underlying data, not two disconnected
/// surfaces.
fn dock_task_badge(pending_approvals: usize, in_progress_tasks: usize, palette: ShellPalette, cx: &mut Context<ShellView>) -> Stateful<Div> {
    let total = pending_approvals + in_progress_tasks;
    let on_click = cx.listener(|view, _ev, _window, cx| {
        if crate::diag_enabled() {
            eprintln!("[hit] dock task badge -> open Notifications");
        }
        crate::overlay::notifications::open_and_refresh(view, cx);
        cx.notify();
    });

    let glyph_color = if palette.is_dark() { palette.text_secondary } else { palette.brand_foreground };

    let mut tile = div()
        .id("shell-dock-task-badge")
        .relative()
        .cursor_pointer()
        .w(px(44.))
        .h(px(44.))
        .rounded(px(10.))
        .bg(linear_gradient(
            180.0,
            linear_color_stop(rgb(palette.settings_gradient_top), 0.0),
            linear_color_stop(rgb(palette.settings_gradient_bottom), 1.0),
        ))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(14.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::alpha(glyph_color, 1.0))
        .shadow(palette.icon_shadow(0.18, 0.30))
        .hover(|style| style.opacity(0.85))
        // "通" (a plain zh-TW glyph, not an emoji — this crate's own "零
        // emoji／手繪 SVG" convention) is the fallback if `icons::BELL`'s
        // asset ever fails to resolve; the real SVG is what renders in the
        // common case (`assets/icons/bell.svg` — declared in `icons.rs` but,
        // as of this round, otherwise unused anywhere in the shell).
        .child(crate::icons::icon_or_glyph(&[(crate::icons::BELL, palette.icon_on_neutral_gradient())], 20., "通"));

    if total > 0 {
        // Capped display, not a capped COUNT — `total` itself is never
        // clamped (a caller reading `pending_approvals`/`in_progress_tasks`
        // directly still gets the real numbers), only the two-character
        // badge glyph is, same "honest data, bounded rendering" split
        // `notifications_feed::MAX_DECIDED_HISTORY`'s own doc comment draws
        // for a different list.
        let label = if total > 9 { "9+".to_string() } else { total.to_string() };
        tile = tile.child(
            div()
                .absolute()
                .top(px(-3.))
                .right(px(-3.))
                .min_w(px(16.))
                .h(px(16.))
                .rounded(px(8.))
                .px(px(3.))
                .flex()
                .items_center()
                .justify_center()
                .bg(theme::alpha(palette.destructive, 1.0))
                .border_2()
                // Same "dot's border matches the surface it sits against"
                // rule `dock_agent`'s own status dot uses — here that
                // surface is the DOCK, not the menu bar.
                .border_color(theme::alpha(palette.surface_raised, 1.0))
                .text_size(px(9.))
                .font_weight(FontWeight::BOLD)
                .text_color(theme::alpha(0xffffff, 1.0))
                .child(label),
        );
    }

    tile.on_click(on_click)
}

/// Round 3: clicking the settings icon opens ControlCenter (task brief:
/// "dock 的「設」（設定）icon 點擊 → 開 ControlCenter").
fn dock_settings(palette: ShellPalette, cx: &mut Context<ShellView>) -> Stateful<Div> {
    let on_click = cx.listener(|view, _ev, _window, cx| {
        if crate::diag_enabled() { eprintln!("[hit] dock settings -> open ControlCenter"); }
        view.surface.open(Overlay::ControlCenter);
        cx.notify();
    });

    // ICON-1 (2026-08-22): the board's real gear icon replaces the "設"
    // placeholder. Its stroke is `#ffffff` light / `#d4d4d8` dark — that
    // is `palette.icon_on_neutral_gradient()`, the shared tint for icons
    // sitting on this neutral gray gradient (the Notifications
    // system-update row uses the same pair on the same gradient). The
    // fallback text keeps its ORIGINAL color pair (`brand_foreground` /
    // `text_secondary`), which was an approximation of the same board
    // values, so a degraded render is byte-identical to the pre-ICON-1
    // build rather than subtly restyled.
    let glyph_color = if palette.is_dark() { palette.text_secondary } else { palette.brand_foreground };

    div()
        .id("shell-dock-settings")
        .cursor_pointer()
        .w(px(44.))
        .h(px(44.))
        .rounded(px(10.))
        .bg(linear_gradient(
            180.0,
            linear_color_stop(rgb(palette.settings_gradient_top), 0.0),
            linear_color_stop(rgb(palette.settings_gradient_bottom), 1.0),
        ))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(14.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::alpha(glyph_color, 1.0))
        .shadow(palette.icon_shadow(0.18, 0.30))
        .hover(|style| style.opacity(0.85))
        .child(crate::icons::icon_or_glyph(&[(crate::icons::SETTINGS, palette.icon_on_neutral_gradient())], 22., "設"))
        .on_click(on_click)
}
