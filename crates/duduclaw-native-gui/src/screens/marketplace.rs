// WP-S6b3-Q (S6b 第三波, 2026-08-22) — "市集" (`Marketplace.dc.html`, B2 型錄
// 卡牆). An "整合" (Integrations) conceptual drill-down — no `nav.rs` id of
// its own (out of this task's "nav.rs 不歸你動" boundary), self-attached in
// `screens/shell.rs` per the "D 先掛好分支就直接可達，未掛就自己掛" precedent
// every prior S5b/S6b wave's own doc comments already establish. Reachable
// via `DUDUCLAW_NATIVE_GUI_DEBUG_PAGE=marketplace`.
//
// Visual authority: `Marketplace.dc.html` — header → 5 category chips (全部/
// 精選/瀏覽器/資料/通訊) → search input → 3-col MCP server card grid (name +
// description + 官方/社群 badge + 安裝/已安裝 button).
//
// ── RPC shape (read directly from `crates/duduclaw-gateway/src/handlers.
// rs`, never guessed) ──────────────────────────────────────────────────
//   `marketplace.list` (dispatch ~L6954, handler `handle_marketplace_list`
//   ~L26901) → params `{}` → `{ "servers": [ { "id", "name", "description",
//   "category", "author", "tags": [...], "featured": bool, "requires_oauth",
//   "default_def": {...}, "required_env": [...], "installed_by": [agent_id,
//   ...] } ] }`. `installed_by` is server-derived from scanning every
//   agent's real `.mcp.json` (`marketplace_installed_map`,
//   handlers.rs:26925) — reload-safe, not client-side UI state.
//
// The real catalog (`duduclaw_agent::mcp_template::marketplace_catalog()`,
// `crates/duduclaw-agent/src/mcp_template.rs:502`) has exactly 10 entries
// (playwright/browserbase/filesystem/github/slack/postgres/sqlite/memory/
// fetch/brave-search) plus whatever an operator has appended to
// `<home>/marketplace.json`. The canvas's own card grid shows illustrative
// names ("Notion", "Puppeteer") that do NOT exist in the real catalog at
// all — this page renders whatever `marketplace.list` actually returns, not
// the canvas's specific mockup names (per this task's "RPC shape 零猜" rule:
// the layout/chip-set/badge grammar comes from the canvas, the row DATA
// comes from the live RPC).
//
// ── Category chips (real, client-side) ────────────────────────────────────
// 全部/精選/瀏覽器/資料/通訊 mirror `MarketplacePage.tsx`'s own five-value
// `Category` union exactly (`web/src/pages/MarketplacePage.tsx:38`) — 精選
// filters on `featured`, the other three filter on `category` field equality
// (`"browser"|"data"|"communication"`, the real values `marketplace_catalog
// ()` uses). All filtering happens client-side against the one `marketplace.
// list` response already in memory — no per-chip round trip.
//
// ── 官方/社群 badge (real, derived from `author`) ─────────────────────────
// The canvas draws each card with an "官方"/"社群" pill this page's backing
// type (`MarketplaceServer` in `web/src/lib/api.ts`) does NOT carry as a
// distinct field — but `marketplace_catalog()`'s own `author` values are
// literally either `"community"` or a named publisher (`"Anthropic"` for
// every current built-in entry), so `author == "community"` → 社群, anything
// else → 官方 is a genuine data-derived badge, not a fabricated one (same
// "real inference from real data, not fabricated" bar `mcp.rs`'s own
// transport-badge deviation note sets).
//
// ── Honest deviations from the design canvas ─────────────────────────────
// 1. Search box renders decoratively (assembled, not wired) — same
//    documented choice `skills.rs`'s own market tab makes for its free-text
//    box (see that module's deviation #2): wiring a real text query needs
//    `TextField`'s IME-capable `Entity`, out of scope for a catalog page
//    this pass. The 5 category chips ARE real filters, matching `skills.rs`'s
//    own split (chips wired, free text decorative).
// 2. 安裝/已安裝 buttons render via `mds_gpui::button(..., disabled: true,
//    ...)` — assembled, deliberately not wired (this task's own brief:
//    "安裝決策類組裝不真按"). `MarketplacePage.tsx`'s real install flow opens
//    a target-agent picker dialog before calling `marketplace.install`; that
//    whole decision-class flow (an irreversible `.mcp.json` write to a real
//    agent) is out of this pass's scope, same "decision-class actions
//    assembled not wired" rule this wave's other four pages follow too. The
//    button's own `installed_by` state (真-已安裝 vs 未安裝 label + icon) IS
//    live data from the RPC — only the click is inert.
// 3. `active` sidebar highlight: the task brief says "active 高亮「整合」
//    （QA 裁定暫掛）" — this page has no `nav.rs` id, so `nav::area_for_page`
//    cannot literally highlight anything (same structural gap `mcp.rs`/
//    `odoo.rs`/`google_integration.rs`/`identity.rs` already accept for
//    every "整合" drill-down leaf — none of them light up a sidebar row
//    either). This page instead reuses `settings_common::breadcrumb`
//    (root label "整合", clicking jumps to `active_page = "integrations"`)
//    — the same non-sidebar navigation-context signal those four sibling
//    pages already rely on, pending the QA ruling this task flags as
//    "暫掛" (deferred, not resolved here).

use gpui::{div, prelude::*, px, Context, Div, Global, SharedString, Stateful};
use serde_json::Value;

use crate::i18n::{self, Locale};
use crate::mds_gpui::{badge, button, empty_state, skeleton, BadgeVariant, ButtonVariant};
use crate::screens::catalog_common::spawn_call;
use crate::screens::dashboard::{error_row, Loadable};
use crate::screens::settings_common::breadcrumb;
use crate::theme;
use crate::ws_status::WsConnState;
use crate::RootView;

// ── Data model ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketplaceServer {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub author: String,
    pub tags: Vec<String>,
    pub featured: bool,
    pub installed_by: Vec<String>,
}

pub fn parse_marketplace_list(v: &Value) -> Vec<MarketplaceServer> {
    v.get("servers")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|s| {
                    Some(MarketplaceServer {
                        id: s.get("id")?.as_str()?.to_string(),
                        name: s.get("name")?.as_str().unwrap_or_default().to_string(),
                        description: s.get("description").and_then(Value::as_str).unwrap_or_default().to_string(),
                        category: s.get("category").and_then(Value::as_str).unwrap_or_default().to_string(),
                        author: s.get("author").and_then(Value::as_str).unwrap_or_default().to_string(),
                        tags: s
                            .get("tags")
                            .and_then(Value::as_array)
                            .map(|t| t.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                            .unwrap_or_default(),
                        featured: s.get("featured").and_then(Value::as_bool).unwrap_or(false),
                        installed_by: s
                            .get("installed_by")
                            .and_then(Value::as_array)
                            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                            .unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Matches `MarketplacePage.tsx`'s own `Category` union verbatim.
const CATEGORIES: [&str; 5] = ["all", "featured", "browser", "data", "communication"];

fn category_label_key(cat: &str) -> &'static str {
    match cat {
        "featured" => "marketplace.category.featured",
        "browser" => "marketplace.category.browser",
        "data" => "marketplace.category.data",
        "communication" => "marketplace.category.communication",
        _ => "marketplace.category.all",
    }
}

fn matches_category(server: &MarketplaceServer, cat: &str) -> bool {
    match cat {
        "all" => true,
        "featured" => server.featured,
        other => server.category == other,
    }
}

// ── State ──────────────────────────────────────────────────────────────

pub struct MarketplaceState {
    requested: bool,
    pub servers: Loadable<Vec<MarketplaceServer>>,
    pub category: &'static str,
}

impl MarketplaceState {
    fn new() -> Self {
        Self { requested: false, servers: Loadable::Loading, category: "all" }
    }
}

impl Global for MarketplaceState {}

fn ensure_state(cx: &mut Context<RootView>) {
    if !cx.has_global::<MarketplaceState>() {
        cx.set_global(MarketplaceState::new());
    }
}

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if cx.global::<MarketplaceState>().requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<MarketplaceState>().requested = true;
    let tx = state.session_tx.clone();
    spawn_call(cx, tx, "marketplace.list", serde_json::json!({}), |cx, result| {
        cx.global_mut::<MarketplaceState>().servers = result.map(|v| parse_marketplace_list(&v)).into();
    });
}

// ── Card ───────────────────────────────────────────────────────────────

fn server_card(locale: Locale, s: &MarketplaceServer) -> Div {
    let author_badge = if s.author == "community" {
        badge(i18n::t(locale, "marketplace.author.community"), BadgeVariant::Secondary)
    } else {
        badge(i18n::t(locale, "marketplace.author.official"), BadgeVariant::Success)
    };

    let installed = !s.installed_by.is_empty();
    let install_button = button(
        SharedString::from(format!("marketplace-install-{}", s.id)),
        i18n::t(locale, if installed { "marketplace.installed" } else { "marketplace.install" }),
        if installed { ButtonVariant::Secondary } else { ButtonVariant::Primary },
        true, // see module doc comment deviation #2 — assembled, never wired.
        None,
        |_ev, _window, _cx| {},
    );

    let mut header_row = div()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .child(div().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::MEDIUM).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(SharedString::from(s.name.clone())));
    if s.featured {
        header_row = header_row.child(badge(i18n::t(locale, "marketplace.featured"), BadgeVariant::Info));
    }

    div()
        .flex()
        .flex_col()
        .gap_2()
        .rounded(px(theme::RADIUS_XL))
        .p_3p5()
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .child(header_row)
        .child(
            div()
                .flex_1()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(SharedString::from(s.description.clone())),
        )
        .child(div().flex().items_center().justify_between().gap_2().child(author_badge).child(install_button))
}

fn category_chip(locale: Locale, cat: &'static str, selected: bool, cx: &mut Context<RootView>) -> Stateful<Div> {
    let id: SharedString = format!("marketplace-cat-{cat}").into();
    div()
        .id(id)
        .h(px(26.))
        .px_2p5()
        .flex()
        .items_center()
        .rounded(px(theme::RADIUS_4XL))
        .cursor_pointer()
        .text_size(px(theme::TEXT_XS))
        .font_weight(gpui::FontWeight::MEDIUM)
        .when(selected, |el| el.bg(theme::alpha(theme::BRAND, 1.0)).text_color(theme::alpha(theme::BRAND_FOREGROUND, 1.0)))
        .when(!selected, |el| {
            el.bg(theme::alpha(theme::SURFACE, 1.0))
                .border_1()
                .border_color(theme::surface_border())
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .hover(|s| s.text_color(theme::alpha(theme::FOREGROUND, 1.0)))
        })
        .child(i18n::t(locale, category_label_key(cat)))
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            cx.global_mut::<MarketplaceState>().category = cat;
            cx.notify();
        }))
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    ensure_state(cx);
    maybe_fetch(state, cx);

    let locale = state.locale;
    let (servers, category) = {
        let st = cx.global::<MarketplaceState>();
        (st.servers.clone(), st.category)
    };

    let header = div()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(
            div()
                .text_size(px(theme::TEXT_XL))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(i18n::t(locale, "marketplace.title")),
        )
        .child(div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "marketplace.subtitle")));

    let mut chip_row = div().flex().flex_wrap().gap_1p5();
    for cat in CATEGORIES {
        chip_row = chip_row.child(category_chip(locale, cat, cat == category, cx));
    }

    // Decorative-only search box — see module doc comment deviation #1.
    let search_box = div()
        .h(px(32.))
        .px_3()
        .flex()
        .items_center()
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .text_size(px(theme::TEXT_SM))
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(i18n::t(locale, "marketplace.searchPlaceholder"));

    let body: Div = match &servers {
        Loadable::Loading => {
            let mut grid = div().flex().flex_wrap().gap_3();
            for _ in 0..6 {
                grid = grid.child(skeleton(px(280.), px(110.)).flex_1().min_w(px(240.)));
            }
            grid
        }
        Loadable::Failed(e) => div().child(error_row(locale, e)),
        Loadable::Ready(list) => {
            let filtered: Vec<&MarketplaceServer> = list.iter().filter(|s| matches_category(s, category)).collect();
            if filtered.is_empty() {
                div().child(empty_state("🧩", i18n::t(locale, "marketplace.empty"), None, None::<Div>))
            } else {
                let mut grid = div().flex().flex_wrap().gap_3();
                for s in filtered {
                    grid = grid.child(server_card(locale, s).flex_1().min_w(px(260.)));
                }
                grid
            }
        }
    };

    div()
        .id("marketplace-page")
        .size_full()
        .overflow_y_scroll()
        .child(
            div()
                .max_w(px(980.))
                .mx_auto()
                .flex()
                .flex_col()
                .gap_3p5()
                .p_2()
                .child(breadcrumb("marketplace-breadcrumb", locale, i18n::t(locale, "marketplace.title"), cx))
                .child(header)
                .child(chip_row)
                .child(search_box)
                .child(body),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_marketplace_list_reads_real_handler_shape() {
        let v = json!({ "servers": [
            { "id": "playwright", "name": "Playwright", "description": "Browser automation",
              "category": "browser", "author": "Anthropic", "tags": ["browser","automation"],
              "featured": true, "requires_oauth": false, "installed_by": ["cs-bot"] },
            { "id": "browserbase", "name": "Browserbase", "description": "Cloud browser",
              "category": "browser", "author": "community", "tags": [], "featured": false,
              "installed_by": [] },
        ]});
        let servers = parse_marketplace_list(&v);
        assert_eq!(servers.len(), 2);
        assert!(servers[0].featured);
        assert_eq!(servers[0].installed_by, vec!["cs-bot".to_string()]);
        assert_eq!(servers[1].author, "community");
        assert!(servers[1].installed_by.is_empty());
    }

    #[test]
    fn parse_marketplace_list_missing_fields_is_empty_not_panicking() {
        assert!(parse_marketplace_list(&json!({})).is_empty());
        assert!(parse_marketplace_list(&json!(null)).is_empty());
    }

    #[test]
    fn matches_category_all_passes_everything() {
        let s = MarketplaceServer {
            id: "x".into(), name: "X".into(), description: String::new(), category: "data".into(),
            author: "Anthropic".into(), tags: vec![], featured: false, installed_by: vec![],
        };
        assert!(matches_category(&s, "all"));
        assert!(matches_category(&s, "data"));
        assert!(!matches_category(&s, "browser"));
        assert!(!matches_category(&s, "featured"));
    }

    #[test]
    fn matches_category_featured_filters_on_bool_not_category_string() {
        let s = MarketplaceServer {
            id: "x".into(), name: "X".into(), description: String::new(), category: "browser".into(),
            author: "Anthropic".into(), tags: vec![], featured: true, installed_by: vec![],
        };
        assert!(matches_category(&s, "featured"));
        assert!(matches_category(&s, "browser"));
    }
}
