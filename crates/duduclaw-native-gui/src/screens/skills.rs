// WP-S5b2-E (2026-08-21) — "技能庫" (`nav.rs` id `skills`, already a real
// `AGENTS_ITEMS`→`KNOWLEDGE_ITEMS` sidebar entry per the S5b1-A pass — this
// file wires that existing id to a real page).
//
// Visual authority: `commercial/design/duduclaw-s5-work-pages/Skills.dc.
// html` — a Segmented tab strip (市場/我的技能/團隊技能/榮譽榜) over a search
// row + category-chip row + a 3-column skill card grid. Mirrors the "市場"
// (`MarketTab`) half of `web/src/pages/SkillMarketPage.tsx`; per this WP's
// 2-page-scope brief ("市場 tab 滿版＋其餘 tab 留頁籤") the other three tabs
// render as an honest "not in this wave" stub rather than their full
// install/share/leaderboard flows.
//
// ── RPC shape (read directly from `crates/duduclaw-gateway/src/handlers.
// rs`, never guessed) ──────────────────────────────────────────────────
//   `skills.search {"query"}` (open to all, `handle_skills_search`
//   ~L15136) → `{"skills":[{"name","description","tags","author","url",
//   "compatible","hub"?,"trust_tier"?,"install_count"?,"source_verdict"?}],
//   "source","total_indexed","hub_errors"}`. `query` MUST be non-empty —
//   an empty query is a hard `error_response` (L15138-15140), so this page
//   never auto-searches on mount; the category-browse grid (this crate's
//   own honest "not yet searched" state) is what a fresh page load shows,
//   same as `SkillMarketPage.tsx`'s own `runSearch`/`searched` gate.
//
// ── Deviations from the canvas (documented, not silent) ──────────────────
// 1. Category chips — the canvas's 7 chips (全部/客服/財務/行銷/資料分析/
//    自動化/其他) are zh-TW mockup text with no backend behind them. The
//    REAL category browse `SkillMarketPage.tsx`'s `MarketTab` drives is 8
//    literal, untranslated English tokens (`utility, communication, code,
//    data, security, ai, media, automation` — hardcoded client-side, no
//    i18n key, no "全部" chip). This page renders the REAL 8-token list,
//    wired to the real `skills.search` RPC — RPC-shape fidelity wins over
//    pixel-matching decorative canvas text (task brief: "RPC shape 零猜").
// 2. Free-text search box — renders decoratively (assembled, not wired).
//    Wiring a real text query needs `TextField`'s IME-capable `Entity`
//    plumbing (`text_field.rs`), which only `login.rs`/`chat.rs` currently
//    own; adding a second persistent-state text input is out of this WP's
//    effort budget. The category chips already exercise the identical
//    `skills.search` round trip for real, so the RPC path IS live — only
//    the free-text variant of triggering it is inert, consistent with this
//    WP's "decision-class controls are assembled not wired" convention.
// 3. "已審查 · 92" pill — the canvas's illustrative reviewed-score badge has
//    no backing field: `source_verdict` is `Option<String>` (a verdict
//    tag like `"clean"`, not a numeric score) and `install_count` is a
//    plain install tally, not a review score. This page shows an honest
//    two-state "已審查"/"未掃描" badge from `source_verdict.is_some()`,
//    plus a SEPARATE install-count line only when `install_count > 0` —
//    never a fabricated number pretending to be a score.
// 4. Hub badge — canvas shows "GitHub"/"Gist"; the real `HubRegistry` only
//    ever emits `github`/`clawhub`/`lobehub`/`skills-sh`/`anthropic-skills`
//    (`crates/duduclaw-agent/src/skill_hub.rs:42-47` — no "gist" hub
//    exists). `hub_display_label` below maps the real 5 ids.

use gpui::{div, prelude::*, px, Context, Div, Global, Stateful};
use serde_json::{json, Value};

use crate::i18n::{self, Locale};
use crate::mds_gpui::{badge, button, empty_state, skeleton, tabs, BadgeVariant, ButtonVariant, TabItem};
use crate::screens::catalog_common as cc;
use crate::screens::dashboard::Loadable;
use crate::theme;
use crate::ws_status::WsConnState;
use crate::RootView;

// ── Data model ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillEntry {
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    pub author: String,
    pub hub: Option<String>,
    pub reviewed: bool,
    pub install_count: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillSearchResult {
    pub skills: Vec<SkillEntry>,
    /// `0` ⇔ the market index itself never loaded (every hub unreachable) —
    /// a very different message than "your query matched nothing", see
    /// `handle_skills_search`'s own comment on this field.
    pub total_indexed: Option<i64>,
}

pub fn parse_skill_search(v: &Value) -> SkillSearchResult {
    let skills = v
        .get("skills")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|s| {
                    let name = s.get("name").and_then(Value::as_str)?.to_string();
                    if name.is_empty() {
                        return None;
                    }
                    Some(SkillEntry {
                        name,
                        description: s.get("description").and_then(Value::as_str).unwrap_or("").to_string(),
                        tags: s
                            .get("tags")
                            .and_then(Value::as_array)
                            .map(|a| a.iter().filter_map(|t| t.as_str().map(str::to_string)).collect())
                            .unwrap_or_default(),
                        author: s.get("author").and_then(Value::as_str).unwrap_or("").to_string(),
                        hub: s.get("hub").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string),
                        reviewed: s.get("source_verdict").and_then(|v| v.as_str()).is_some(),
                        install_count: s.get("install_count").and_then(Value::as_u64),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    SkillSearchResult { skills, total_indexed: v.get("total_indexed").and_then(Value::as_i64) }
}

/// The real, hardcoded 8-token category list `SkillMarketPage.tsx`'s
/// `MarketTab` browses — see module doc comment deviation #1.
pub const CATEGORIES: [&str; 8] =
    ["utility", "communication", "code", "data", "security", "ai", "media", "automation"];

fn hub_display_label(hub: &str) -> &str {
    match hub {
        "github" => "GitHub",
        "clawhub" => "ClawHub",
        "lobehub" => "LobeHub",
        "skills-sh" => "skills.sh",
        "anthropic-skills" => "Anthropic Skills",
        other => other,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SkillsTab {
    Market,
    Mine,
    Shared,
    Leaderboard,
}

impl SkillsTab {
    fn id(self) -> &'static str {
        match self {
            SkillsTab::Market => "market",
            SkillsTab::Mine => "mine",
            SkillsTab::Shared => "shared",
            SkillsTab::Leaderboard => "leaderboard",
        }
    }
}

// ── State ──────────────────────────────────────────────────────────────

pub struct SkillsState {
    tab: SkillsTab,
    active_category: Option<&'static str>,
    /// `None` = not yet searched (category-browse view), matching
    /// `SkillMarketPage.tsx`'s own `searched` boolean — this page's
    /// equivalent honest "haven't asked yet" state.
    pub results: Option<Loadable<SkillSearchResult>>,
}

impl Default for SkillsState {
    fn default() -> Self {
        Self { tab: SkillsTab::Market, active_category: None, results: None }
    }
}

impl Global for SkillsState {}

fn run_search(state: &RootView, cx: &mut Context<RootView>, category: &'static str) {
    cx.default_global::<SkillsState>().active_category = Some(category);
    cx.default_global::<SkillsState>().results = Some(Loadable::Loading);
    cx.notify();
    let tx = state.session_tx.clone();
    cc::spawn_call(cx, tx, "skills.search", json!({ "query": category }), |cx, result| {
        cx.default_global::<SkillsState>().results = Some(result.map(|v| parse_skill_search(&v)).into());
    });
}

// ── Rendering ──────────────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    let locale = state.locale;

    if state.ws_state != WsConnState::Authenticated {
        return div()
            .id("skills-page")
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(empty_state(
                "🔌",
                i18n::t(locale, "native.home.connError.title"),
                Some(i18n::t(locale, "native.home.connError.desc")),
                None::<Div>,
            ));
    }

    let tab = cx.default_global::<SkillsState>().tab;
    let active_category = cx.default_global::<SkillsState>().active_category;
    let results = cx.default_global::<SkillsState>().results.clone();

    let tab_items = vec![
        TabItem::new(SkillsTab::Market.id(), i18n::t(locale, "native.skills.tab.market"), cx.listener(|this, _ev, _window, cx| {
            select_tab(this, cx, SkillsTab::Market);
        })),
        TabItem::new(SkillsTab::Mine.id(), i18n::t(locale, "native.skills.tab.mine"), cx.listener(|this, _ev, _window, cx| {
            select_tab(this, cx, SkillsTab::Mine);
        })),
        TabItem::new(SkillsTab::Shared.id(), i18n::t(locale, "native.skills.tab.shared"), cx.listener(|this, _ev, _window, cx| {
            select_tab(this, cx, SkillsTab::Shared);
        })),
        TabItem::new(SkillsTab::Leaderboard.id(), i18n::t(locale, "native.skills.tab.leaderboard"), cx.listener(|this, _ev, _window, cx| {
            select_tab(this, cx, SkillsTab::Leaderboard);
        })),
    ];

    let body: Div = if tab == SkillsTab::Market {
        market_tab(state, cx, locale, active_category, &results)
    } else {
        div().child(empty_state("🧩", i18n::t(locale, "native.skills.tabStub"), None, None::<Div>))
    };

    div()
        .id("skills-page")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_3()
        .p_6()
        .child(cc::breadcrumb(i18n::t(locale, "navArea.agents"), i18n::t(locale, "native.skills.title")))
        .child(cc::page_header(
            i18n::t(locale, "native.skills.title"),
            i18n::t(locale, "native.skills.subtitle"),
            Some(
                div().child(button(
                    "skills-import-url",
                    i18n::t(locale, "native.skills.importFromUrl"),
                    ButtonVariant::Secondary,
                    false,
                    None,
                    |_ev, _window, _app| {},
                )),
            ),
        ))
        .child(div().w_full().child(tabs(tab_items, tab.id())))
        .child(body)
}

fn select_tab(_this: &mut RootView, cx: &mut Context<RootView>, tab: SkillsTab) {
    cx.default_global::<SkillsState>().tab = tab;
    cx.notify();
}

fn market_tab(
    state: &RootView,
    cx: &mut Context<RootView>,
    locale: Locale,
    active_category: Option<&'static str>,
    results: &Option<Loadable<SkillSearchResult>>,
) -> Div {
    let search_row = div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            // Decorative only — see module doc comment deviation #2.
            div()
                .flex_1()
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
                .child(i18n::t(locale, "native.skills.searchPlaceholder")),
        );

    let mut chip_row = div().flex().flex_wrap().gap_1p5();
    for cat in CATEGORIES {
        let selected = active_category == Some(cat);
        chip_row = chip_row.child(category_chip(state, cx, cat, selected));
    }

    let content: Div = match results {
        None => div()
            .flex()
            .flex_col()
            .gap_2()
            .child(cc::category_group_header(i18n::t(locale, "native.skills.categories")))
            .child(chip_row),
        Some(Loadable::Loading) => grid_3col((0..3).map(|_| card_skeleton()).collect()),
        Some(Loadable::Failed(msg)) => empty_state("⚠️", i18n::t1(locale, "native.presets.loadError", "message", msg), None, None::<Div>),
        Some(Loadable::Ready(r)) if r.skills.is_empty() => {
            let index_unavailable = r.total_indexed == Some(0);
            let title = if index_unavailable { "native.skills.market.indexEmpty" } else { "native.skills.market.noResults" };
            empty_state(if index_unavailable { "⚠️" } else { "🔍" }, i18n::t(locale, title), None, None::<Div>)
        }
        Some(Loadable::Ready(r)) => grid_3col(r.skills.iter().map(|s| skill_card(locale, s)).collect()),
    };

    div().flex().flex_col().gap_3().child(search_row).child(content)
}

fn category_chip(_state: &RootView, cx: &mut Context<RootView>, cat: &'static str, selected: bool) -> Stateful<Div> {
    div()
        .id(format!("skills-chip-{cat}"))
        .cursor_pointer()
        .px_3()
        .py_1p5()
        .rounded(px(theme::RADIUS_4XL))
        .text_size(px(theme::TEXT_XS))
        .font_weight(gpui::FontWeight::MEDIUM)
        .when(selected, |el| el.bg(theme::alpha(theme::BRAND, 1.0)).text_color(theme::alpha(theme::BRAND_FOREGROUND, 1.0)))
        .when(!selected, |el| {
            el.bg(theme::alpha(theme::MUTED, 1.0))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .hover(|s| s.text_color(theme::alpha(theme::FOREGROUND, 1.0)))
        })
        .child(cat)
        .on_click(cx.listener(move |this, _ev, _window, cx| {
            run_search(this, cx, cat);
        }))
}

fn grid_3col(cards: Vec<Div>) -> Div {
    div().flex().flex_wrap().gap_3().children(cards.into_iter().map(|c| c.flex_1().min_w(px(240.))))
}

fn card_skeleton() -> Div {
    cc::catalog_card().child(skeleton(px(140.), px(14.))).child(skeleton(px(200.), px(12.))).child(skeleton(px(80.), px(10.)))
}

fn skill_card(locale: Locale, s: &SkillEntry) -> Div {
    let title = div().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::MEDIUM).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(s.name.clone());

    let desc = div()
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(if s.description.is_empty() { i18n::t(locale, "native.skills.noDescription").to_string() } else { s.description.clone() });

    let mut meta = div().flex().items_center().gap_1p5().flex_wrap();
    if let Some(hub) = &s.hub {
        meta = meta.child(badge(hub_display_label(hub), BadgeVariant::Secondary));
    }
    meta = meta.child(if s.reviewed {
        badge(i18n::t(locale, "native.skills.reviewed"), BadgeVariant::Success)
    } else {
        badge(i18n::t(locale, "native.skills.unscanned"), BadgeVariant::Outline)
    });
    if let Some(n) = s.install_count.filter(|n| *n > 0) {
        meta = meta.child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t1(locale, "native.skills.installCount", "count", &n.to_string())));
    }

    let footer = div()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .child(meta)
        .child(button(format!("skills-install-{}", s.name), i18n::t(locale, "native.skills.install"), ButtonVariant::Primary, false, None, |_ev, _window, _app| {}));

    cc::catalog_card().child(title).child(desc).child(footer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_skill_search_reads_real_handler_shape() {
        let v = json!({
            "skills": [
                { "name": "客訴分類引擎", "description": "自動分類", "tags": ["客服"], "author": "sam",
                  "hub": "github", "source_verdict": "clean", "install_count": 92 },
                { "name": "發票自動核銷", "description": "", "tags": [], "author": "",
                  "hub": "clawhub", "install_count": 0 },
            ],
            "source": "hubs:github+clawhub",
            "total_indexed": 12,
        });
        let r = parse_skill_search(&v);
        assert_eq!(r.skills.len(), 2);
        assert_eq!(r.skills[0].name, "客訴分類引擎");
        assert!(r.skills[0].reviewed);
        assert_eq!(r.skills[0].install_count, Some(92));
        assert!(!r.skills[1].reviewed);
        assert_eq!(r.total_indexed, Some(12));
    }

    #[test]
    fn parse_skill_search_missing_array_is_empty_not_a_panic() {
        let r = parse_skill_search(&json!({}));
        assert!(r.skills.is_empty());
        assert_eq!(r.total_indexed, None);
    }

    #[test]
    fn parse_skill_search_skips_entries_missing_name() {
        let v = json!({ "skills": [{ "description": "x" }] });
        assert!(parse_skill_search(&v).skills.is_empty());
    }

    #[test]
    fn parse_skill_search_total_indexed_zero_means_index_unavailable() {
        let r = parse_skill_search(&json!({ "skills": [], "total_indexed": 0 }));
        assert_eq!(r.total_indexed, Some(0));
    }

    #[test]
    fn hub_display_label_maps_the_five_real_hub_ids() {
        assert_eq!(hub_display_label("github"), "GitHub");
        assert_eq!(hub_display_label("clawhub"), "ClawHub");
        assert_eq!(hub_display_label("lobehub"), "LobeHub");
        assert_eq!(hub_display_label("skills-sh"), "skills.sh");
        assert_eq!(hub_display_label("anthropic-skills"), "Anthropic Skills");
        // Unknown id renders verbatim, never blank.
        assert_eq!(hub_display_label("mystery-hub"), "mystery-hub");
    }

    #[test]
    fn categories_is_the_real_eight_token_list_not_the_canvas_zh_tw_chips() {
        assert_eq!(CATEGORIES.len(), 8);
        assert!(CATEGORIES.contains(&"utility"));
        assert!(CATEGORIES.contains(&"automation"));
    }
}
