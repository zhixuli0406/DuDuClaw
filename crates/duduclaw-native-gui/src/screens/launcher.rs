// WP-S6b3-R (S6b 第三波, 2026-08-22) — "Launcher" (`Launcher.dc.html`, B22
// APPS 網格). No `nav.rs` entry — the canvas's own header note says this is
// "shell 桌面的前身", not a sidebar destination — self-attached in
// `screens/shell.rs` only, `DUDUCLAW_NATIVE_GUI_DEBUG_PAGE=launcher`, same "D
// 先掛好分支就直接可達，未掛就自己掛" precedent every prior self-attached S5b/
// S6b page already establishes.
//
// ── Two DIFFERENT "launcher" surfaces exist in this codebase — this page is
// neither the web one nor the shell-compositor one ────────────────────────
// (1) `web/src/pages/LauncherPage.tsx` (`/launcher`, N-1): a React page in
//     the browser dashboard, driven by `@/apps/registry`'s `APPS` constant —
//     zero gateway RPC (grep-verified: the whole file never calls
//     `useWsCall`/`ws.send`, only `@/apps/registry` + `@/platform`'s auth
//     store for role filtering). This is the FUNCTIONAL reference this
//     page's data source is modeled on (see below), not a literal port.
// (2) `crates/duduclaw-shell/src/overlay/launcher.rs`: a Super-K natural-
//     language command overlay in the `duduclaw-shell` COMPOSITOR crate — a
//     completely separate binary/runtime (a smithay/gpui shell process, not
//     this app) that shows a typed-query + delegate-suggestion board, no
//     app grid at all. Confirmed by reading that file directly: its own
//     header comment describes "靜態預打字狀態", a query cursor, and an
//     "Enter 交辦" suggestion — conceptually unrelated to this page's "click
//     a tile to jump to an app area" grid. The task brief's own "先查證...
//     兩個不同介面——畫布批三已明辨" instruction is confirmed correct by this
//     read, not just taken on faith.
// This page (`duduclaw-native-gui`) is a THIRD thing: an in-shell full-bleed
// page reachable only via the debug-page boot override, modeled on (1)'s
// DATA shape but rendered with THIS crate's own gpui primitives.
//
// ── APPS registry: no backend RPC exists, so this is a static mirror
// (verified, not assumed) ──────────────────────────────────────────────────
// `crates/duduclaw-gateway/src/handlers.rs`'s dispatch table has no
// `apps.*` method anywhere (grep-verified over the same file every other
// page in this batch cites RPC line numbers from) — the "APPS registry" is
// a pure frontend constant on the web side too (`web/src/apps/registry.ts`'s
// own doc comment: "the single source of truth an app's identity is defined
// from"), never fetched from the gateway. `LAUNCHER_APPS` below is therefore
// a hand-written Rust mirror of that TypeScript constant's SEVEN entries —
// same `AppId` set (`system`/`workbench`/`staff`/`comms`/`memory`/`files`/
// `monitor`), same order — cross-checked against the canvas's own 7 cards,
// which render in this exact order and (once you discount MDS-token-vs-
// literal-hex, per this crate's universal convention) match the registry's
// 7 ids one-for-one. Card COPY (title/subtitle) follows the CANVAS's own
// text verbatim, not `web/src/i18n/zh-TW.json`'s shipped `app.<id>.name`/
// `app.<id>.desc` strings — those two sources disagree slightly (e.g. canvas
// "系統 / 設定、安全與帳戶" vs. web "系統設定 / 裝置、帳戶、安全與系統維運"),
// and this pass's own "canvas is the pixel authority for THIS page" rule
// (every other S6b module doc comment states the same priority) settles the
// tie in the canvas's favor. New `launcher.app.<id>.name`/`.desc` i18n keys
// are used rather than reusing `app.*` (which doesn't exist in this crate's
// catalogs at all — `app.name`/`app.subtitle` are the only two `app.*` keys
// present, and they mean something else: the whole application's own
// name/tagline).
//
// ── Real interactivity: client-side search filter + tile click = real
// in-shell navigation (a value-add beyond the task brief's minimum, safe
// because it's side-effect-free) ────────────────────────────────────────
// The search field is a real `TextField` (same "an input with no write
// consequence is safe to make live" bar `widget_composer.rs`/`identity.rs`
// already clear) — its `.content` is read fresh every render and filters
// `LAUNCHER_APPS` by a case-insensitive substring match against the
// LOCALIZED name+desc (not the raw i18n key), so it works in all three
// locales. A tile click sets `RootView::active_page` to that app's closest
// existing real page in THIS shell (`manageAdvanced`/`tasks`/`agents`/
// `channels`/`memory`/`files`/`runs` — all seven already have real
// `shell.rs` branches, wired by earlier waves) and `cx.notify()`s — a plain
// in-process page switch, zero side effects, the same shape `migrate.rs`'s
// own `exit_wizard` already establishes for "leave this full-bleed page,
// land on a real shell page". This is NOT "真桌面喚起" (spawning a separate
// OS-level app window) — that's `duduclaw-shell`/a future desktop-shell
// integration's job, out of scope here per the task brief's own "殼層整合
// （真桌面喚起）記為殼線 v2 欠帳不做" instruction. The mapping from "app
// category" to "one representative existing page" is this file's own
// judgment call (there is no 1:1 backend concept), documented per constant
// below rather than left implicit.
//
// ── Full-bleed root swap (not an overlay) — same shape, same reason, as
// `migrate.rs` ───────────────────────────────────────────────────────────
// `screens::shell::render` early-returns this page's own `render(...)`
// output for `active_page == "launcher"`, bypassing the normal three-column
// composition entirely — see `migrate.rs`'s own module doc comment for why
// this crate always does a root-level child swap for full-bleed pages
// rather than an absolutely-positioned overlay (a documented `duduclaw-
// shell` stacking-context incident, `DESIGN-native-gui-gpui-2026-08.md` §14
// P8f). The canvas's own "Esc" chip is a KEYBOARD hint in the design tool's
// mockup, not a real keybinding this crate has any global-keydown-outside-
// a-focused-field mechanism to honor — ported instead as a real CLICKABLE
// close affordance (→ `active_page = "home"`) so a person landing here via
// the debug-page override always has a way back out, same "no dead end"
// discipline `migrate.rs`'s own exit button establishes.
//
// ── Canvas deviations, documented ────────────────────────────────────────
// (1) Icon tiles: the canvas draws real per-app SVG icon paths (gear/kanban/
//     people/hub/book/inbox/chart). This crate has NO `gpui::svg()` usage
//     anywhere and `nav.rs`'s own doc comment establishes the crate-wide
//     fallback: "an uppercase letter + a fixed accent color (real
//     lucide-style iconography is still Phase 1b scope)" — followed here
//     verbatim rather than inventing a second icon convention for one page.
// (2) Background: the canvas layers a blurred, low-opacity `cat-512.png`
//     over a dark diagonal gradient + a `backdrop-filter: blur(18px)` glass
//     wash. `theme.rs`'s own header comment already documents that gpui (at
//     this crate's pinned rev) has no real backdrop-blur, and this crate has
//     no CSS-gradient div API in production use anywhere (only a throwaway
//     `spike_t7.rs` canvas-primitive demo references gradients at all) — so
//     the background here is a single opaque `theme::APP_SHELL` surface (the
//     darkest MDS token, the closest in-palette match to the canvas's dark
//     navy tone), no image, no blur. A real `cat-512.png` asset at that
//     resolution isn't bundled in this crate either (only `assets/mark-
//     256.png`, already used by `about.rs`) — blowing that up to the
//     canvas's 420px display width would be a visibly blurry upscale, not a
//     faithful decorative touch, so it's omitted rather than faked.

use gpui::{div, prelude::*, px, Context, Div, Global, Stateful};

use crate::i18n::{self, Locale};
use crate::mds_gpui::empty_state;
use crate::text_field::TextField;
use crate::theme;
use crate::RootView;

// ── APPS registry mirror ──────────────────────────────────────────────────

struct LauncherApp {
    /// Matches `web/src/apps/registry.ts`'s `AppId` string values.
    id: &'static str,
    name_key: &'static str,
    desc_key: &'static str,
    /// `nav.rs`-style placeholder icon glyph — see this file's header
    /// comment, deviation (1).
    glyph: char,
    /// Which real `RootView::active_page` id a click on this tile jumps to —
    /// this file's own judgment call, documented per entry below.
    target_page: &'static str,
}

const LAUNCHER_APPS: &[LauncherApp] = &[
    // 系統 → 進階設定殼 (`manageAdvanced`), which itself federates 治理/安全/
    // 帳務/授權/成員/經銷 — the closest single existing page to "設定、安全與
    // 帳戶".
    LauncherApp {
        id: "system",
        name_key: "launcher.app.system.name",
        desc_key: "launcher.app.system.desc",
        glyph: 'S',
        target_page: "manageAdvanced",
    },
    // 工作台 → 任務 (`tasks`) — the canvas's own subtitle names 任務/目標/
    // 例行工作; `tasks` is this app's most central existing single page.
    LauncherApp {
        id: "workbench",
        name_key: "launcher.app.workbench.name",
        desc_key: "launcher.app.workbench.desc",
        glyph: 'W',
        target_page: "tasks",
    },
    // 員工 → AI 員工列表 (`agents`) — a direct, unambiguous match.
    LauncherApp {
        id: "staff",
        name_key: "launcher.app.staff.name",
        desc_key: "launcher.app.staff.desc",
        glyph: 'E',
        target_page: "agents",
    },
    // 通訊 → 通道 (`channels`) — the canvas's subtitle names 通道/收件匣;
    // `channels` is the entry page for that area (`areaManage` in nav.rs).
    LauncherApp {
        id: "comms",
        name_key: "launcher.app.comms.name",
        desc_key: "launcher.app.comms.desc",
        glyph: 'C',
        target_page: "channels",
    },
    // 記憶 → 記憶 (`memory`) — a direct, unambiguous match.
    LauncherApp {
        id: "memory",
        name_key: "launcher.app.memory.name",
        desc_key: "launcher.app.memory.desc",
        glyph: 'M',
        target_page: "memory",
    },
    // 檔案 → 檔案 (`files`) — a direct, unambiguous match.
    LauncherApp {
        id: "files",
        name_key: "launcher.app.files.name",
        desc_key: "launcher.app.files.desc",
        glyph: 'F',
        target_page: "files",
    },
    // 監測 → 執行紀錄 (`runs`) — the canvas's own subtitle literally says
    // "執行紀錄與可靠性"; `runs` is the page that IS 執行紀錄 (rather than
    // `reports`, the more general 分析報表 page).
    LauncherApp {
        id: "monitor",
        name_key: "launcher.app.monitor.name",
        desc_key: "launcher.app.monitor.desc",
        glyph: 'R',
        target_page: "runs",
    },
];

// ── State — one live field (the search query), nothing else has anywhere
// to write to (same "genuinely nothing else to wire" shape `pet_studio.rs`
// establishes for its own zero-RPC page, minus the zero-RPC part: this page
// simply has no server round trip to make in the first place, registry data
// is a compile-time constant). ────────────────────────────────────────────

pub struct LauncherState {
    search: gpui::Entity<TextField>,
}

impl LauncherState {
    fn new(cx: &mut gpui::App, locale: Locale) -> Self {
        Self { search: TextField::new(cx, i18n::t(locale, "launcher.searchPlaceholder"), false, "") }
    }
}

impl Global for LauncherState {}

fn ensure_state(locale: Locale, cx: &mut Context<RootView>) {
    if !cx.has_global::<LauncherState>() {
        let state = LauncherState::new(cx, locale);
        cx.set_global(state);
    }
}

fn close_launcher(view: &mut RootView, cx: &mut Context<RootView>) {
    view.active_page = "home";
    cx.notify();
}

fn open_app(view: &mut RootView, cx: &mut Context<RootView>, target: &'static str) {
    view.active_page = target;
    cx.notify();
}

// ── Search bar ─────────────────────────────────────────────────────────

fn search_bar(locale: Locale, search_entity: gpui::Entity<TextField>, cx: &mut Context<RootView>) -> Stateful<Div> {
    div()
        .id("launcher-search")
        .flex()
        .items_center()
        .gap_2p5()
        .w(px(620.))
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 0.98))
        .px_5()
        .py_3p5()
        .shadow(theme::floating_shadow())
        .child(div().flex_1().child(search_entity))
        .child(
            div()
                .id("launcher-esc")
                .text_size(px(11.))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .border_1()
                .border_color(theme::surface_border())
                .rounded(px(theme::RADIUS_SM))
                .px_1p5()
                .py_0p5()
                .cursor_pointer()
                .hover(|s| s.text_color(theme::alpha(theme::FOREGROUND, 1.0)))
                .child(i18n::t(locale, "launcher.esc"))
                .on_click(cx.listener(|this, _ev, _window, cx| close_launcher(this, cx))),
        )
}

// ── App tile ───────────────────────────────────────────────────────────

fn app_tile(locale: Locale, app: &'static LauncherApp, cx: &mut Context<RootView>) -> Stateful<Div> {
    div()
        .id(gpui::SharedString::from(format!("launcher-app-{}", app.id)))
        .w(px(168.))
        .flex()
        .flex_col()
        .items_center()
        .gap_2p5()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 0.96))
        .border_1()
        .border_color(theme::surface_border())
        .px(px(16.))
        .py(px(18.))
        .cursor_pointer()
        .shadow(theme::surface_shadow())
        .hover(|s| s.bg(theme::alpha(theme::SURFACE_HOVER, 1.0)))
        .child(
            div()
                .size(px(46.))
                .rounded(px(13.))
                .bg(theme::alpha(theme::BRAND, 1.0))
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme::alpha(theme::BRAND_FOREGROUND, 1.0))
                .font_weight(gpui::FontWeight::BOLD)
                .text_size(px(theme::TEXT_BASE))
                .child(app.glyph.to_string()),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .text_center()
                .gap_0p5()
                .child(
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                        .child(i18n::t(locale, app.name_key)),
                )
                .child(
                    div()
                        .text_size(px(10.5))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child(i18n::t(locale, app.desc_key)),
                ),
        )
        .on_click(cx.listener(move |this, _ev, _window, cx| open_app(this, cx, app.target_page)))
}

// ── Query matching — pure fn, unit-tested below ──────────────────────────

/// Case-insensitive substring match against a tile's LOCALIZED name+desc
/// (not its i18n key) — an empty query matches everything.
fn matches_query(name: &str, desc: &str, query_lower: &str) -> bool {
    if query_lower.is_empty() {
        return true;
    }
    name.to_lowercase().contains(query_lower) || desc.to_lowercase().contains(query_lower)
}

// ── Top-level render ───────────────────────────────────────────────────
// No `WsConnState` gate — this page has no RPC of any kind to wait on, same
// "zero-RPC pages skip the auth gate entirely" precedent `pet_studio.rs`/
// `screens::gallery.rs`/`screens::prototypes::mod.rs` already establish.
// Returns a plain `Div` (not `Stateful<Div>`) — `screens::shell::render`
// hands this straight back as ITS OWN return value for the early-return
// bypass, same signature `migrate::render` uses for the identical reason.

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Div {
    let locale = state.locale;
    ensure_state(locale, cx);

    let search_entity = cx.global::<LauncherState>().search.clone();
    let query_lower = search_entity.read(cx).content.trim().to_lowercase();

    let visible: Vec<&'static LauncherApp> = LAUNCHER_APPS
        .iter()
        .filter(|app| {
            matches_query(&i18n::t(locale, app.name_key), &i18n::t(locale, app.desc_key), &query_lower)
        })
        .collect();

    let grid = if visible.is_empty() {
        div().mt(px(40.)).child(empty_state(
            "🔍",
            i18n::t(locale, "launcher.empty.title"),
            Some(i18n::t(locale, "launcher.empty.desc")),
            None::<Div>,
        ))
    } else {
        div()
            .mt(px(40.))
            .flex()
            .flex_wrap()
            .gap_4()
            .justify_center()
            .w(px(800.))
            .children(visible.into_iter().map(|app| app_tile(locale, app, cx)))
    };

    // The scrollable body needs its own `.id(...)` (`.overflow_y_scroll()`
    // requires `Stateful<Div>` — it tracks per-element scroll offset), but
    // the function's OWN return type must stay plain `Div` (see header
    // comment). So the id'd, scrollable element is nested ONE level in,
    // under a plain outer wrapper — same two-layer shape `migrate.rs`'s own
    // root (`div().size_full()...`, no id) uses around its own id'd,
    // independently-scrollable rail/content columns.
    let body = div()
        .id("launcher-scroll")
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .overflow_y_scroll()
        .pt(px(108.))
        .pb(px(48.))
        .child(search_bar(locale, search_entity, cx))
        .child(grid)
        .child(
            div()
                .mt(px(28.))
                .text_size(px(11.5))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.75))
                .child(i18n::t(locale, "launcher.hint")),
        );

    // Plain `Div`, no `.id(...)` — see this function's own header comment.
    div().size_full().bg(theme::alpha(theme::APP_SHELL, 1.0)).child(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_matches_everything() {
        assert!(matches_query("系統", "設定、安全與帳戶", ""));
    }

    #[test]
    fn query_matches_name_case_insensitively() {
        assert!(matches_query("Workbench", "任務、目標與例行工作", "work"));
    }

    #[test]
    fn query_matches_desc_substring() {
        assert!(matches_query("記憶", "知識庫與長期記憶", "知識庫"));
    }

    #[test]
    fn query_with_no_match_excludes() {
        assert!(!matches_query("檔案", "共同計畫的檔案", "zzz"));
    }

    #[test]
    fn launcher_apps_ids_match_web_registry_seven_entries() {
        let ids: Vec<&str> = LAUNCHER_APPS.iter().map(|a| a.id).collect();
        assert_eq!(ids, vec!["system", "workbench", "staff", "comms", "memory", "files", "monitor"]);
    }

    #[test]
    fn every_target_page_is_a_distinct_real_shell_id() {
        let mut targets: Vec<&str> = LAUNCHER_APPS.iter().map(|a| a.target_page).collect();
        targets.sort_unstable();
        targets.dedup();
        assert_eq!(targets.len(), LAUNCHER_APPS.len(), "every app should jump to its own distinct page");
    }
}
