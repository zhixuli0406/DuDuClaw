// Home surface — Shell-S0's desktop first screen.
//
// Visual spec: `commercial/design/duduclaw-os-desktop/Main.dc.html` (light)
// / `commercial/design/duduclaw-os-home-dark/Main.dc.html` (dark) — both
// 1440×900, matching this crate's window size exactly, no scaling math
// needed. Shell-S1 (2026-08-20) added the dark board; every color
// below now resolves through the `palette: ShellPalette` parameter threaded
// into every fn in this file (see `crate::palette`'s own header comment for
// the full light/dark token mapping), deliberately NOT the crate's
// higher-level `mds_gpui` component facade: `mds_gpui::card()`/`button()`/
// `badge()` are hardwired to that crate's top-level RE-EXPORTED tokens
// (`theme::SURFACE` etc. = `theme::dark::SURFACE` unconditionally — see
// `theme.rs`'s own module doc comment, "Phase 1a renders dark only"), so
// calling them here would paint LIGHT-mode Home with dark colors always.
// Every element below is hand-built straight from `palette` fields instead —
// the same "consume theme tokens exclusively, no ad-hoc values" discipline
// `mds_gpui` itself established, just applied against a palette that
// actually switches. Values with no matching palette field are hardcoded
// PER THEME with a `// Main.dc.html:`/`duduclaw-os-home-dark/Main.dc.html:`
// comment naming their source (usually via `palette.is_dark()`), same
// citation convention this file already used for its light-only literals —
// see `crate::palette::ShellPalette::is_dark`'s own doc comment for why
// these one-off pairs stay inline rather than growing the palette struct
// further.
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
// Icon glyphs: REAL stroke icons since ICON-1 (2026-08-22). Every
// icon-shaped slot below used to render a single CJK character (and the
// menu bar's Wi-Fi arc, having no safe equivalent character, was dropped
// entirely) on the reasoning that this codebase had no `gpui::svg()` usage
// and no pictographic font coverage. That reasoning was right about the
// cause and wrong about the conclusion: the icons had been drawn as real
// inline SVG in the approved boards all along. `crate::icons` now embeds
// them as assets and `icon_or_glyph` swaps each placeholder for the board's
// own artwork, keeping the character as a fallback if an asset ever fails
// to resolve. See that module's header comment for the asset provenance,
// the alpha-mask/tinting constraint, and why the light and dark boards
// share ONE asset set.
//
// Font note: this crate's `main.rs` registers the bundled stack itself as
// of ICON-1 (`theme::bundled_font_bytes()` → `add_fonts`, plus
// `.font(theme::app_font())` on the root element in `Render::render`).
// Before that it registered nothing and asked for nothing, so every screen
// rendered in the platform's default family — the earlier note here, which
// claimed `duduclaw-native-gui`'s `main.rs` covered it, was wrong in the
// one place it mattered most: on the appliance the kiosk unit launches
// THIS binary directly and that crate's `main.rs` never runs at all.
//
// Split across two files, same convention `duduclaw-native-gui/src/
// screens/shell.rs` + its `shell_sidebar.rs`/`shell_content_list.rs`/
// `shell_row.rs` siblings already establish for one screen too big for one
// file: THIS file owns the wallpaper/menu-bar/AI-workspace top half,
// `home_dock.rs` (`pub(super)`, not `pub` — nothing outside this module
// needs its internals) owns the goal-cards/activity-shelf/dock bottom half.

mod home_dock;
pub(crate) mod running_windows;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use gpui::{div, img, linear_color_stop, linear_gradient, prelude::*, px, rgb, BoxShadow, Context, Div, FontWeight, Image, ImageFormat, Stateful};

use duduclaw_native_gui::theme;

use crate::fake_data;
use crate::i18n::{t, Key, Locale};
use crate::icons;
use crate::overlay::notifications_feed::{FeedStatus, NotificationsFeed};
use crate::overlay::task_progress_feed::TaskProgressFeed;
use crate::palette::ShellPalette;
use crate::surface::Overlay;
use crate::ShellView;
use running_windows::RunningWindowsFeed;

/// Branding PNGs, repo-relative to `appliance/branding/png/` (this crate's
/// task brief: "commercial/ 是 gitignored，資產一律用 appliance/branding 的
/// 版本"). `include_bytes!` embeds them directly into the binary — no
/// asset-loader / runtime file path involved, matching the task brief's
/// "可 include_bytes 內嵌". `pub(crate)` (not `pub(super)`/private, as this
/// used to be): Shell-S4's lockscreen surface (`lockscreen/render.rs`, a
/// SIBLING module of `home`, not a descendant) reuses the SAME embedded
/// bytes for its own mark icon + cat watermark (LockScreen.dc.html's own
/// board reuses the identical two assets) — widening visibility here avoids
/// a second `include_bytes!` of the same PNGs bloating the binary with a
/// duplicate copy.
pub(crate) const MARK_32: &[u8] = include_bytes!("../../../appliance/branding/png/mark-32.png");
pub(crate) const CAT_512: &[u8] = include_bytes!("../../../appliance/branding/png/cat-512.png");

/// WP-A4-4 (2026-08-22): memoized per asset. This is called from RENDER
/// bodies (Home's mascot + menu-bar mark, the lockscreen's watermark +
/// mark, OOBE's finish mark), so the previous `bytes.to_vec()` copied and
/// re-hashed the whole PNG on every single repaint — 14.5 KB for
/// `CAT_512` — purely to hand gpui a value it already had.
///
/// Correctness was never at risk (checked against the pinned gpui rev's own
/// `Image::from_bytes`, `crates/gpui/src/platform.rs`: `id: hash(&bytes)`
/// is CONTENT-derived, so a fresh `Arc` of identical bytes hits the same
/// image-cache entry and never triggers a re-decode). This is purely about
/// not doing the copy-and-hash again on a machine that is repainting
/// often — which the appliance VM demonstrably was.
pub(crate) fn png(bytes: &'static [u8]) -> Arc<Image> {
    static CACHE: OnceLock<Mutex<HashMap<usize, Arc<Image>>>> = OnceLock::new();
    // Keyed by the `&'static` slice's own address: every caller passes one
    // of this crate's `include_bytes!` consts, so the address IS the asset
    // identity and no hashing of the contents is needed to look it up.
    let key = bytes.as_ptr() as usize;
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    // A poisoned mutex here would mean a panic while cloning an `Arc` —
    // recovering the guard is strictly better than turning that into a
    // second panic on a UI thread.
    let mut guard = cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    Arc::clone(guard.entry(key).or_insert_with(|| Arc::new(Image::from_bytes(ImageFormat::Png, bytes.to_vec()))))
}

/// The whole Home surface: wallpaper, menu bar, AI workspace, goal cards,
/// activity shelf, and dock — absolutely positioned to match the design
/// board 1:1. WM-3 (2026-08-23) refactored this into `desktop_content(...)`
/// (everything except menu bar/dock) plus `.child(menu_bar(...))`/`.child
/// (home_dock::dock(...))` appended here, so `chrome::windows`' separate
/// `duduclaw-shell-menubar`/`-dock`/`-home` layer surfaces can reuse the
/// SAME three pieces without a second copy — see `desktop_content`'s own
/// doc comment. Paint order is no longer byte-identical to the design
/// board's own HTML source (the menu bar moved from 4th child to 2nd-to-
/// last) — deliberately accepted as safe, not missed: every one of these
/// children is `.absolute()`-positioned and the menu bar's own band (y: 0
/// -30px) never spatially overlaps `workspace`/`goal_cards_row`/
/// `activity_shelf` (which start no higher than y:150px), so paint order
/// between them cannot change a single visible pixel. `palette` is resolved
/// ONCE per render pass by
/// the caller (`ShellView::render` in `main.rs`, from `ShellView.theme`) and
/// threaded down through every fn in this module — same "recompute fresh,
/// never cache" convention `OobeFlow::palette()` already established for
/// OOBE, extended here to Home. `notifications` (Shell-S4, 2026-08-22,
/// WP-S4-notif) is `&self.overlay_ui.notifications` from the same caller —
/// the menu-bar approval ticker (`menu_bar_ticker` below) reads it, since
/// it's the SAME real feed the Notifications overlay panel itself renders
/// (see `overlay::notifications_feed`'s own header comment on why the two
/// surfaces share one model rather than each polling independently).
/// `running_windows` (WP-comp-shell-ipc, 2026-08-22) is `&self.running_
/// windows` from the same caller — the dock (`home_dock::dock`) reads it
/// for its running-indicator dots and reuses it to decide focus-vs-launch
/// on click (see `home::running_windows::RunningWindowsFeed`'s own header
/// comment).
/// `installed_apps` (APP-1, 2026-08-22) is `&self.installed_apps` from the
/// same caller — the dock renders the REAL apps on this machine from it,
/// and `home_dock::dock` also dispatches the background scan that keeps it
/// warm for the Launcher overlay too (see `crate::apps::feed::
/// InstalledAppsFeed`'s own header comment).
pub fn render(
    palette: ShellPalette,
    notifications: &NotificationsFeed,
    running_windows: &RunningWindowsFeed,
    installed_apps: &crate::apps::feed::InstalledAppsFeed,
    // A4 (2026-08-24): `&self.overlay_ui.task_progress` from the same
    // caller — the dock badge (`home_dock::dock`) reads it alongside
    // `notifications` for its combined pending-approvals/in-progress-tasks
    // count. See `overlay::task_progress_feed::TaskProgressFeed`'s own
    // header comment.
    task_progress: &TaskProgressFeed,
    cx: &mut Context<ShellView>,
) -> Stateful<Div> {
    desktop_content(palette, cx)
        .child(menu_bar(palette, notifications, cx))
        .child(home_dock::dock(palette, running_windows, installed_apps, notifications, task_progress, cx))
}

/// WM-3 layer-shell migration (`crate::chrome`, 2026-08-23): everything
/// `render` above draws EXCEPT the menu bar and the dock — those two are
/// now separate `duduclaw-shell-menubar`/`-dock` layer surfaces of their
/// own in `ChromeMode::LayerSurfaces` (see `render_menu_bar`/`render_dock`
/// below), so `chrome::windows`' dedicated `duduclaw-shell-home` background
/// surface renders THIS instead of `render` — same wallpaper/blobs/mascot/
/// workspace/goal-cards/activity-shelf, laid out identically (this fn's
/// root, like `render`'s own, is a full `size_full()` canvas starting at
/// this window's own (0,0), which for the Home surface IS the whole
/// 1440×900 output — see `chrome::params::LayerParams` for that surface's
/// `ChromeAnchor::FILL`). `render` itself is refactored to call this PLUS
/// its own menu_bar/dock children, so the two entry points can never
/// visually drift apart — see `ShellView::render_root`'s own doc comment
/// (`main.rs`) for the matching split on that side.
pub(crate) fn desktop_content(palette: ShellPalette, cx: &mut Context<ShellView>) -> Stateful<Div> {
    div()
        .id("shell-home")
        .relative()
        .size_full()
        // Main.dc.html: `linear-gradient(135deg, #eef1f6 0%, #e6ecf5 45%,
        // #dde6f3 100%)` light / `linear-gradient(135deg, #0c0c0e 0%,
        // #101016 45%, #12141c 100%)` dark — gpui's `linear_gradient` only
        // supports a 2-stop gradient (see this file's header comment), so
        // the middle stop is dropped in both themes; both endpoints are
        // kept verbatim via `palette.wallpaper_from`/`wallpaper_to`.
        .bg(linear_gradient(
            135.0,
            linear_color_stop(rgb(palette.wallpaper_from), 0.0),
            linear_color_stop(rgb(palette.wallpaper_to), 1.0),
        ))
        // Main.dc.html: two `radial-gradient` soft glow blobs — gpui has no
        // radial-gradient primitive, approximated as flat low-opacity
        // circles (a visible flat-opacity edge instead of a soft falloff).
        // Blob 1 is brand-hued in both themes (`rgba(33,113,204,.10)` light
        // / `rgba(67,144,238,.09)` dark — the hex IS `palette.brand`, only
        // the alpha differs); blob 2 keeps the SAME hex in both themes
        // (`#788cc8`, a decorative tint with no brand/semantic identity)
        // and only its alpha differs (`.12` light / `.08` dark) — see
        // `ShellPalette::is_dark`'s own doc comment for why this one-off
        // pair stays inline rather than becoming a palette field.
        .child(blob_top_left(-180., -120., 720., theme::alpha(palette.brand, if palette.is_dark() { 0.09 } else { 0.10 })))
        .child(blob_bottom_right(
            -220.,
            -100.,
            820.,
            theme::alpha(0x788cc8, if palette.is_dark() { 0.08 } else { 0.12 }),
        ))
        .child(cat_hero())
        .child(workspace(palette, cx))
        .child(home_dock::goal_cards_row(palette))
        .child(home_dock::activity_shelf(palette))
}

/// WM-3: the menu bar, wrapped for its own dedicated `duduclaw-shell-
/// menubar` layer surface — a thin `size_full()` root around the SAME
/// `menu_bar()` fn `render`/`desktop_content` above compose inline, so the
/// two chrome modes can never render a different menu bar. `.h(px(30.))`
/// on `menu_bar()` itself matches `chrome::MENU_BAR_HEIGHT` — see that
/// constant's own doc comment.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn render_menu_bar(palette: ShellPalette, notifications: &NotificationsFeed, cx: &mut Context<ShellView>) -> Stateful<Div> {
    div().id("shell-menu-bar-surface").relative().size_full().child(menu_bar(palette, notifications, cx))
}

/// WM-3: the dock, wrapped for its own dedicated `duduclaw-shell-dock`
/// layer surface — same "thin `size_full()` root around the SAME fn"
/// shape as `render_menu_bar` above, wrapping `home_dock::dock_surface`
/// instead.
///
/// D9-bug (2026-08-24): `dock_surface`, not `dock`. The two build the same
/// container; the surface variant additionally restricts this surface's
/// pointer input to the pill it actually paints, so the transparent rest of
/// the full-width dock band stops swallowing clicks meant for the window
/// underneath. See that fn's own doc comment.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn render_dock(
    palette: ShellPalette,
    running_windows: &RunningWindowsFeed,
    installed_apps: &crate::apps::feed::InstalledAppsFeed,
    // A4 (2026-08-24): same two feeds `render`'s own doc comment above adds
    // — the layer-shell dock surface must show the identical badge the
    // single-fullscreen path does.
    notifications: &NotificationsFeed,
    task_progress: &TaskProgressFeed,
    cx: &mut Context<ShellView>,
) -> Stateful<Div> {
    div()
        .id("shell-dock-surface")
        .relative()
        .size_full()
        .child(home_dock::dock_surface(palette, running_windows, installed_apps, notifications, task_progress, cx))
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

fn menu_bar(palette: ShellPalette, notifications: &NotificationsFeed, cx: &mut Context<ShellView>) -> Stateful<Div> {
    // Main.dc.html: bg `rgba(255,255,255,0.82)` light / `rgba(24,24,27,0.82)`
    // dark — the hex IS `palette.surface` in both (0xffffff / 0x18181b),
    // only alpha (0.82) is shared. Bottom border: light `rgba(228,228,231,
    // 0.8)` (=`SURFACE_BORDER` at 0.8, not the field's own stored 1.0 alpha)
    // / dark `rgba(255,255,255,0.10)` — a bespoke alpha in both themes, kept
    // inline per `ShellPalette::is_dark`'s own doc comment.
    let border_color =
        if palette.is_dark() { theme::alpha(0xffffff, 0.10) } else { theme::alpha(theme::light::SURFACE_BORDER, 0.8) };
    div()
        .id("shell-menu-bar")
        .absolute()
        .top(px(0.))
        .left(px(0.))
        .right(px(0.))
        // WM-3: `crate::chrome::MENU_BAR_HEIGHT` — kept as ONE literal
        // (formerly a bare `px(30.)`) so this band's height and the
        // `duduclaw-shell-menubar` layer surface's own `exclusive_zone` /
        // window height (`chrome::windows::render_surface_content`) can
        // never drift apart.
        .h(px(crate::chrome::MENU_BAR_HEIGHT))
        .flex()
        .items_center()
        .justify_between()
        .px(px(12.))
        .bg(theme::alpha(palette.surface, 0.82))
        .border_b_1()
        .border_color(border_color)
        .text_size(px(13.))
        .text_color(theme::alpha(palette.foreground, 1.0))
        .child(menu_bar_left())
        .child(menu_bar_ticker(palette, notifications, cx))
        .child(menu_bar_right(palette, cx))
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
/// brief. Round 3 wired the click to open Notifications; Shell-S4
/// (2026-08-22, WP-S4-notif) replaces the static `fake_data::
/// APPROVAL_TICKER` text with the REAL pending-approval feed (the same
/// `overlay::notifications_feed::NotificationsFeed` the panel itself reads
/// — see that struct's own header comment for why the two surfaces share
/// one model). The click now calls `overlay::notifications::
/// open_and_refresh`, not a bare `surface.open(...)` — opening the panel
/// this way also kicks off a fetch when the feed is stale (task brief:
/// "開面板即刷").
fn menu_bar_ticker(palette: ShellPalette, notifications: &NotificationsFeed, cx: &mut Context<ShellView>) -> Stateful<Div> {
    let on_click = cx.listener(|view, _ev, _window, cx| {
        if crate::diag_enabled() { eprintln!("[hit] ticker -> open Notifications"); }
        crate::overlay::notifications::open_and_refresh(view, cx);
    });

    // Main.dc.html: this pill's bg is `#ffffff` light / `#1e1e21` dark —
    // `surface_raised`, NOT `surface` (see `crate::palette`'s own header
    // comment: the two collapse to the same value in light, but differ in
    // dark). Border: opaque `SURFACE_BORDER` light / `rgba(255,255,255,
    // 0.12)` dark (a bespoke alpha, not the field's own stored 0.10).
    let border_color = if palette.is_dark() { theme::alpha(0xffffff, 0.12) } else { theme::alpha(theme::light::SURFACE_BORDER, 1.0) };
    let has_pending = notifications.pending_count() > 0;

    let mut el = div()
        .id("shell-approval-ticker")
        .cursor_pointer()
        .flex()
        .items_center()
        .gap(px(8.))
        .bg(theme::alpha(palette.surface_raised, 1.0))
        .border_1()
        .border_color(border_color)
        .rounded(px(999.))
        .px(px(12.))
        .py(px(3.))
        .shadow(palette.surface_shadow())
        .text_size(px(12.))
        .hover(|style| style.bg(theme::alpha(palette.surface_hover, 1.0)));
    if has_pending {
        // The dot uses `warning_dot`, not `warning` — see that field's own
        // doc comment on `ShellPalette` (a small circular alert dot uses a
        // brighter literal than the badge/ring token in dark). Only shown
        // when there is actually something waiting — an offline/loading/
        // empty ticker has nothing to alert about.
        el = el.child(div().w(px(7.)).h(px(7.)).rounded(px(7.)).bg(theme::alpha(palette.warning_dot, 1.0)));
    }
    el.child(div().font_weight(FontWeight::MEDIUM).child(ticker_text(notifications))).on_click(on_click)
}

/// Renders `notifications`' current state as the ticker's one line of text —
/// see `menu_bar_ticker`'s own doc comment. The "等你批准：" prefix on the
/// pending-summary case is a plain zh-TW literal (design CONTENT, same as
/// the old `fake_data::APPROVAL_TICKER` it replaces — Home/overlay's
/// established non-i18n boundary, see `crate::i18n`'s own header comment);
/// the three connectivity-state messages route through `crate::i18n`
/// because they're genuinely NEW process chrome, same reasoning
/// `overlay::notifications`'s own header comment gives for the panel.
fn ticker_text(feed: &NotificationsFeed) -> String {
    match feed.status {
        FeedStatus::Offline => t(Locale::ZhTw, Key::NotifOfflineBanner).to_string(),
        FeedStatus::Idle => t(Locale::ZhTw, Key::NotifLoadingLabel).to_string(),
        FeedStatus::Loading if feed.pending_count() == 0 => t(Locale::ZhTw, Key::NotifLoadingLabel).to_string(),
        FeedStatus::Loading | FeedStatus::Ready => match feed.rows().first() {
            None => t(Locale::ZhTw, Key::NotifEmptyLabel).to_string(),
            Some(first) => {
                let extra = feed.pending_count().saturating_sub(1);
                if extra > 0 {
                    format!("等你批准：{}（+{extra}）", first.summary)
                } else {
                    format!("等你批准：{}", first.summary)
                }
            }
        },
    }
}

/// Round 3: split into two independent click targets — the ⌘K pill opens
/// the Launcher (task brief: "「⌘K」膠囊點擊 → 開 Launcher"), the
/// battery/clock status group opens Notifications (task brief: "右側狀態
/// 區（agent 頭像/電量/時鐘一帶）點擊 → 開 Notifications" — this board has
/// no agent avatar in the menu bar at all, so the group is the Wi-Fi
/// indicator + battery + clock).
fn menu_bar_right(palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    // `.open(...)`, not `.toggle_launcher()` — see `composer()`'s own doc
    // comment for why every mouse click target here opens rather than
    // toggles (only the cmd-k keyboard binding itself toggles).
    let cmdk_click = cx.listener(|view, _ev, window, cx| {
        if crate::diag_enabled() { eprintln!("[hit] cmd-k pill -> open Launcher"); }
        view.surface.open(Overlay::Launcher);
        // D3-b: same focus hand-off the composer click does — see
        // `ShellView::settle_launcher_query`.
        view.settle_launcher_query(window, cx);
        cx.notify();
    });
    let status_click = cx.listener(|view, _ev, _window, cx| {
        if crate::diag_enabled() { eprintln!("[hit] status group -> open Notifications"); }
        crate::overlay::notifications::open_and_refresh(view, cx);
    });

    // Main.dc.html: `#f4f4f5`/`#71717b` light == `SECONDARY`/`MUTED_FOREGROUND`
    // exactly — `#27272a`/`#9f9fa9` dark ALSO equal `palette.secondary`/
    // `palette.muted_foreground` exactly, so both themes route through the
    // same two fields with no bespoke branch needed here.
    let cmdk_border = if palette.is_dark() { theme::alpha(0xffffff, 0.10) } else { theme::alpha(theme::light::SURFACE_BORDER, 1.0) };

    div()
        .flex()
        .items_center()
        .gap(px(12.))
        .child(
            div()
                .id("shell-menu-cmdk")
                .cursor_pointer()
                .bg(theme::alpha(palette.secondary, 1.0))
                .border_1()
                .border_color(cmdk_border)
                .rounded(px(6.))
                .px(px(6.))
                .py(px(1.))
                .text_size(px(11.))
                .text_color(theme::alpha(palette.muted_foreground, 1.0))
                .hover(|style| style.bg(theme::alpha(palette.surface_selected, 1.0)))
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
                .hover(|style| style.bg(theme::alpha(palette.surface_hover, 1.0)))
                // ICON-1: the Wi-Fi indicator between the ⌘K pill and the
                // battery reading. Main.dc.html renders it at 14px with the
                // menu bar's own foreground stroke (`#09090b` light /
                // `#fafafa` dark) — `palette.foreground` matches BOTH board
                // values exactly, no bespoke pair needed. It is inside the
                // status group's click target because the board draws it
                // there, grouped with battery/clock; it is not separately
                // interactive. No text fallback exists for it (there is no
                // safe single character for "Wi-Fi"), so a missing asset
                // renders nothing here rather than a wrong character —
                // which is why `icons`' registry tests exist.
                .children(icons::icon_or_none(&[(icons::WIFI, palette.foreground)], 14.))
                .child(div().text_size(px(12.)).child(fake_data::BATTERY_PCT))
                .child(div().text_size(px(12.)).font_weight(FontWeight::MEDIUM).child(fake_data::CLOCK))
                .on_click(status_click),
        )
}

// ── AI workspace (greeting + composer + suggestion chips) ────────────────

fn workspace(palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
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
                .text_color(theme::alpha(palette.foreground, 1.0))
                .child(fake_data::GREETING),
        )
        .child(composer(palette, cx))
        .child(suggestion_chips(palette))
}

/// Round 3: clicking the composer opens the Launcher — task brief's
/// "S0 的合理近似" for a not-yet-typeable input box (real typing is later-
/// round scope, see `overlay/launcher.rs`'s own header comment on its
/// still-static query row). Uses `.open(...)`, not `.toggle_launcher()` —
/// cmd-k is the one binding whose task-brief wording is explicitly "切換"
/// (toggle); every mouse click target's own wording is "開" (open), so a
/// second click can't accidentally CLOSE an already-open Launcher here.
fn composer(palette: ShellPalette, cx: &mut Context<ShellView>) -> Stateful<Div> {
    let on_click = cx.listener(|view, _ev, window, cx| {
        if crate::diag_enabled() { eprintln!("[hit] composer -> open Launcher"); }
        view.surface.open(Overlay::Launcher);
        // D3-b: focus the (now real) search field so typing goes straight
        // into it — see `ShellView::settle_launcher_query`.
        view.settle_launcher_query(window, cx);
        cx.notify();
    });

    // Main.dc.html: bg `#ffffff` light / `#1e1e21` dark — `surface_raised`
    // (see `menu_bar_ticker`'s own comment on why, not `surface`). Border:
    // opaque `border()` light (`#ececef`, matches the board exactly) /
    // `rgba(255,255,255,0.12)` dark (a bespoke alpha, not `border()`'s own
    // dark 0.06 — see `crate::palette`'s header comment on why this
    // specific canvas's borders don't reuse the generic MDS `--border`
    // alpha). Shadow: bespoke in BOTH themes (softer than `floating_shadow
    // ()`'s own recipe, kept literal to match the board's exact numbers —
    // same reasoning the original light-only comment already gave, now
    // extended with the dark board's own literal `0 16px 40px rgba(0,0,0,
    // .45), 0 3px 10px rgba(0,0,0,.30)`).
    // `palette.border()` returns `Hsla`; the dark branch needs `Rgba` to
    // match `theme::alpha`'s own return type, so light's value is spelled
    // out via the same `BORDER`/`BORDER_ALPHA` tokens `border()` itself
    // dispatches to, rather than calling that method here.
    let border_color =
        if palette.is_dark() { theme::alpha(0xffffff, 0.12) } else { theme::alpha(theme::light::BORDER, theme::light::BORDER_ALPHA) };
    let shadow = if palette.is_dark() {
        vec![
            BoxShadow::new(px(0.), px(16.), rgb(0x000000).opacity(0.45).into()).blur_radius(px(40.)),
            BoxShadow::new(px(0.), px(3.), rgb(0x000000).opacity(0.30).into()).blur_radius(px(10.)),
        ]
    } else {
        vec![
            BoxShadow::new(px(0.), px(16.), rgb(0x0f172a).opacity(0.10).into()).blur_radius(px(40.)),
            BoxShadow::new(px(0.), px(3.), rgb(0x0f172a).opacity(0.06).into()).blur_radius(px(10.)),
        ]
    };

    div()
        .id("shell-composer")
        .cursor_pointer()
        .w(px(620.))
        .flex()
        .items_center()
        .gap(px(10.))
        .bg(theme::alpha(palette.surface_raised, 1.0))
        .border_1()
        .border_color(border_color)
        .rounded(px(theme::RADIUS_XL))
        .px(px(16.))
        .py(px(14.))
        .shadow(shadow)
        .hover(|style| style.bg(theme::alpha(palette.surface_hover, 1.0)))
        .child(
            div()
                .text_size(px(15.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::alpha(palette.muted_foreground, 1.0))
                // ICON-1: the board's leading icon here is an arrow-out-of-
                // a-tray (upload/attach), 18px, `#71717b` light / `#9f9fa9`
                // dark — exactly `palette.muted_foreground` in both. "+" is
                // the pre-ICON-1 stand-in, kept as the fallback; the
                // wrapper's own `text_size`/`text_color` above are what
                // style it if it ever renders.
                .child(icons::icon_or_glyph(&[(icons::UPLOAD, palette.muted_foreground)], 18., "+")),
        )
        .child(
            div()
                .flex_1()
                .text_size(px(15.))
                // Main.dc.html: `#9f9fa9` light / `#71717b` dark — text
                // ladder rank 4, see `crate::palette`'s own header comment.
                .text_color(theme::alpha(palette.text_faint, 1.0))
                .child(fake_data::COMPOSER_PLACEHOLDER),
        )
        .child(
            div()
                .w(px(30.))
                .h(px(30.))
                .rounded(px(9.))
                .bg(theme::alpha(palette.brand, 1.0))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_size(px(13.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(theme::alpha(palette.brand_foreground, 1.0))
                        // ICON-1: the board's send button is an up-arrow,
                        // 14px, `#fafafa` on the brand fill in BOTH boards
                        // — `palette.brand_foreground` exactly, which is
                        // also what the "送" fallback already used.
                        .child(icons::icon_or_glyph(&[(icons::ARROW_UP, palette.brand_foreground)], 14., "送")),
                ),
        )
        .on_click(on_click)
}

fn suggestion_chips(palette: ShellPalette) -> Div {
    // Main.dc.html: bg `rgba(255,255,255,0.85)` light (`surface`) / `rgba
    // (39,39,42,0.85)` dark — `surface_hover` (`#27272a`), NOT
    // `surface_raised` — a genuine per-canvas choice (see `crate::palette`'s
    // own header comment: this crate deliberately does NOT try to reduce
    // every "raised surface" call site to one field, since the boards
    // themselves don't). Border: opaque `SURFACE_BORDER` light / `rgba(255,
    // 255,255,0.10)` dark.
    let bg_hex = if palette.is_dark() { palette.surface_hover } else { palette.surface };
    let border_color = if palette.is_dark() { theme::alpha(0xffffff, 0.10) } else { theme::alpha(theme::light::SURFACE_BORDER, 1.0) };

    div().flex().gap(px(8.)).children(fake_data::SUGGESTION_CHIPS.iter().map(move |label| {
        div()
            .bg(theme::alpha(bg_hex, 0.85))
            .border_1()
            .border_color(border_color)
            .rounded(px(999.))
            .px(px(12.))
            .py(px(4.))
            .text_size(px(12.))
            // Main.dc.html: `#52525c` light / `#d4d4d8` dark — text ladder
            // rank 2, see `crate::palette`'s own header comment.
            .text_color(theme::alpha(palette.text_secondary, 1.0))
            .child(*label)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WP-A4-4: `png` is called from render bodies, so a machine that
    /// repaints often must not pay for a fresh `Vec` copy plus a content
    /// hash of the whole PNG on every frame. Pointer equality of the
    /// returned `Arc` is the direct evidence the memo actually hit.
    #[test]
    fn png_returns_the_same_cached_image_for_a_repeated_asset() {
        let a = png(CAT_512);
        let b = png(CAT_512);
        assert!(Arc::ptr_eq(&a, &b), "a repeat render must reuse the decoded asset, not rebuild it");
    }

    #[test]
    fn png_keeps_distinct_assets_distinct() {
        let cat = png(CAT_512);
        let mark = png(MARK_32);
        assert!(!Arc::ptr_eq(&cat, &mark));
        assert_ne!(cat.id(), mark.id(), "two different assets must not collapse onto one cache entry");
    }
}
