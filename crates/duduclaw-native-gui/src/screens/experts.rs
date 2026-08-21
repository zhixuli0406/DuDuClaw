// WP-S5b2-E (2026-08-21) — "AI 團隊" (`nav.rs` id `experts`, already a real
// `AGENTS_ITEMS` sidebar entry per the S5b1-A pass — this file wires that
// existing id to a real page).
//
// Visual authority: `commercial/design/duduclaw-s5-work-pages/Experts.dc.
// html` — a horizontal row of "已安裝的 AI 團隊" cards above a category-
// grouped "可召喚的 AI 團隊" card wall. Mirrors `web/src/pages/
// ExpertsPage.tsx`'s two data sources (`experts.list` for installed packs,
// `experts.catalog` for the built-in industry catalog); per this WP's
// brief ("召喚=experts.install_builtin 決策類組裝不真按") the "召喚" action
// renders as an inert placeholder — no upload/generate/remove/hooks-apply
// flow either, all out of this 2-page-scope pass.
//
// ── RPC shapes (read directly from `crates/duduclaw-gateway/src/handlers.
// rs`, never guessed — both admin-scoped, `require_admin!()`) ───────────
//   `experts.list {}` (`handle_experts_list` ~L18918) → `{"packs":[{"slug",
//   "kind","display_name","version","description","agents":[...],
//   "skills_count","wiki_count","installed_at","hooks_status","hooks_
//   files"}]}`.
//   `experts.catalog {}` (`handle_experts_catalog` ~L19067, built from
//   `expert_generate::builtin_catalog`, `crates/duduclaw-gateway/src/
//   expert_generate.rs:178-264`) → `{"deployed","unlocked",
//   "present_but_locked","packs":[{"kind","industry"?,"category",
//   "departments","label","slug","description","agents_count","installed",
//   "members":[{"role","name","display_name","summary"}],
//   "humans":[{"title","summary"}],"excluded":[{"kit","reason"}],
//   "examples","lead_agent_name"}]}`.
//
// ── Deviation from the canvas (documented, not silent) ────────────────────
// The canvas's "可召喚的 AI 團隊" cards show "N 位員工／N 個技能／N 頁知識"
// stat pills for NOT-YET-installed catalog entries. `experts.catalog`'s real
// payload carries only `agents_count` for a catalog entry — `skills_count`/
// `wiki_count` exist ONLY on `experts.list`'s INSTALLED-pack rows (they come
// from `r.global_skills.len()`/`r.wiki_files.len()`, counts of files a pack
// actually wrote to disk — nothing to count before install). This page
// shows only the real `agents_count` pill on catalog cards, never a
// fabricated skill/wiki count. It also renders BOTH `humans[]` and
// `excluded[]` when present (the canvas's single example only had one of
// the two at a time) and shows an "已安裝" badge instead of "召喚" for a
// catalog entry that IS already installed — `ExpertsPage.tsx`'s own
// `BuiltinCatalogCard` does the same dual-state handling; the canvas simply
// didn't draw that second state.

use gpui::{div, prelude::*, px, Context, Div, Global, Stateful};
use serde_json::{json, Value};

use crate::i18n::{self, Locale};
use crate::mds_gpui::{badge, button, empty_state, skeleton, BadgeVariant, ButtonVariant};
use crate::screens::catalog_common as cc;
use crate::screens::dashboard::Loadable;
use crate::theme;
use crate::ws_status::WsConnState;
use crate::RootView;

// ── Data model ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledPack {
    pub slug: String,
    pub display_name: String,
    pub agents_count: usize,
    pub installed_at: Option<String>,
}

pub fn parse_installed_packs(v: &Value) -> Vec<InstalledPack> {
    v.get("packs")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|p| {
                    let slug = p.get("slug").and_then(Value::as_str)?.to_string();
                    if slug.is_empty() {
                        return None;
                    }
                    let display_name = p
                        .get("display_name")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .unwrap_or_else(|| slug.clone());
                    let agents_count = p.get("agents").and_then(Value::as_array).map(Vec::len).unwrap_or(0);
                    let installed_at = p.get("installed_at").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
                    Some(InstalledPack { slug, display_name, agents_count, installed_at })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogMember {
    pub display_name: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    pub kind: String,
    pub slug: String,
    pub label: String,
    pub description: String,
    pub category: String,
    pub agents_count: usize,
    pub installed: bool,
    pub members: Vec<CatalogMember>,
    pub human_titles: Vec<String>,
    pub excluded: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Catalog {
    pub deployed: bool,
    pub present_but_locked: bool,
    pub entries: Vec<CatalogEntry>,
}

pub fn parse_catalog(v: &Value) -> Catalog {
    let entries = v
        .get("packs")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|p| {
                    let slug = p.get("slug").and_then(Value::as_str)?.to_string();
                    if slug.is_empty() {
                        return None;
                    }
                    let members = p
                        .get("members")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(|m| {
                                    let name = m.get("name").and_then(Value::as_str)?.to_string();
                                    let display_name = m.get("display_name").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string).unwrap_or(name);
                                    let summary = m.get("summary").and_then(Value::as_str).unwrap_or("").to_string();
                                    Some(CatalogMember { display_name, summary })
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    let human_titles = p
                        .get("humans")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(|h| h.get("title").and_then(Value::as_str).map(str::to_string)).collect())
                        .unwrap_or_default();
                    let excluded = p
                        .get("excluded")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(|e| {
                                    let kit = e.get("kit").and_then(Value::as_str)?.to_string();
                                    let reason = e.get("reason").and_then(Value::as_str).unwrap_or("").to_string();
                                    Some((kit, reason))
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    Some(CatalogEntry {
                        kind: p.get("kind").and_then(Value::as_str).unwrap_or("team").to_string(),
                        slug,
                        label: p.get("label").and_then(Value::as_str).unwrap_or("").to_string(),
                        description: p.get("description").and_then(Value::as_str).unwrap_or("").to_string(),
                        category: p.get("category").and_then(Value::as_str).unwrap_or("other").to_string(),
                        agents_count: p.get("agents_count").and_then(Value::as_u64).unwrap_or(0) as usize,
                        installed: p.get("installed").and_then(Value::as_bool).unwrap_or(false),
                        members,
                        human_titles,
                        excluded,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Catalog {
        deployed: v.get("deployed").and_then(Value::as_bool).unwrap_or(false),
        present_but_locked: v.get("present_but_locked").and_then(Value::as_bool).unwrap_or(false),
        entries,
    }
}

// ── State ──────────────────────────────────────────────────────────────

pub struct ExpertsState {
    installed_requested: bool,
    pub installed: Loadable<Vec<InstalledPack>>,
    catalog_requested: bool,
    pub catalog: Loadable<Catalog>,
}

impl Default for ExpertsState {
    fn default() -> Self {
        Self { installed_requested: false, installed: Loadable::Loading, catalog_requested: false, catalog: Loadable::Loading }
    }
}

impl Global for ExpertsState {}

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    if !cx.default_global::<ExpertsState>().installed_requested {
        cx.default_global::<ExpertsState>().installed_requested = true;
        let tx = state.session_tx.clone();
        cc::spawn_call(cx, tx, "experts.list", json!({}), |cx, result| {
            cx.default_global::<ExpertsState>().installed = result.map(|v| parse_installed_packs(&v)).into();
        });
    }
    if !cx.default_global::<ExpertsState>().catalog_requested {
        cx.default_global::<ExpertsState>().catalog_requested = true;
        let tx = state.session_tx.clone();
        cc::spawn_call(cx, tx, "experts.catalog", json!({}), |cx, result| {
            cx.default_global::<ExpertsState>().catalog = result.map(|v| parse_catalog(&v)).into();
        });
    }
}

// ── Rendering ──────────────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    maybe_fetch(state, cx);
    let locale = state.locale;

    if state.ws_state != WsConnState::Authenticated {
        return div()
            .id("experts-page")
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

    let g = cx.default_global::<ExpertsState>();
    let installed = g.installed.clone();
    let catalog = g.catalog.clone();

    // Button copy reuses the real, already-shipped `experts.upload`/
    // `experts.generate.open` web i18n strings ("上傳安裝"/"自製 AI 團隊")
    // rather than the canvas's illustrative literal text ("上傳自訂 AI 團隊"/
    // "用 AI 生成") — product-copy consistency wins over pixel-matching
    // decorative canvas text, same call `skills.rs`'s category-chip
    // deviation makes.
    let actions = div()
        .flex()
        .gap_2()
        .child(button("experts-upload", i18n::t(locale, "native.experts.upload"), ButtonVariant::Secondary, false, None, |_ev, _window, _app| {}))
        .child(button("experts-generate", i18n::t(locale, "native.experts.generateOpen"), ButtonVariant::Secondary, false, None, |_ev, _window, _app| {}));

    div()
        .id("experts-page")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_4()
        .p_6()
        .child(cc::breadcrumb(i18n::t(locale, "navArea.agents"), i18n::t(locale, "native.experts.title")))
        .child(cc::page_header(i18n::t(locale, "native.experts.title"), i18n::t(locale, "native.experts.subtitle"), Some(actions)))
        .child(installed_section(locale, &installed))
        .child(catalog_section(locale, &catalog))
}

fn installed_section(locale: Locale, installed: &Loadable<Vec<InstalledPack>>) -> Div {
    let body: Div = match installed {
        Loadable::Loading => row_wrap((0..2).map(|_| installed_skeleton()).collect()),
        Loadable::Failed(msg) => empty_state("⚠️", i18n::t1(locale, "native.presets.loadError", "message", msg), None, None::<Div>),
        Loadable::Ready(rows) if rows.is_empty() => empty_state("📦", i18n::t(locale, "native.experts.empty.installed"), None, None::<Div>),
        Loadable::Ready(rows) => row_wrap(rows.iter().map(|p| installed_card(locale, p)).collect()),
    };
    div().flex().flex_col().gap_2().child(cc::category_group_header(i18n::t(locale, "native.experts.section.installed"))).child(body)
}

fn row_wrap(cards: Vec<Div>) -> Div {
    div().flex().flex_wrap().gap_2().children(cards)
}

fn installed_skeleton() -> Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .min_w(px(220.))
        .p_3()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .child(skeleton(px(34.), px(34.)))
        .child(skeleton(px(120.), px(12.)))
}

fn installed_card(locale: Locale, p: &InstalledPack) -> Div {
    let meta = match &p.installed_at {
        Some(when) => i18n::tn(locale, "native.experts.installedMeta", &[("count", &p.agents_count.to_string()), ("date", when)]),
        None => i18n::t1(locale, "native.experts.agentsCount", "count", &p.agents_count.to_string()),
    };
    div()
        .flex()
        .items_center()
        .gap_3()
        .min_w(px(220.))
        .p_3()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .child(
            div()
                .size(px(34.))
                .flex_shrink_0()
                .rounded(px(theme::RADIUS_MD))
                .flex()
                .items_center()
                .justify_center()
                .bg(theme::alpha(theme::BRAND, 0.14))
                .text_color(theme::alpha(theme::BRAND, 1.0))
                .child("◆"),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::MEDIUM).text_color(theme::alpha(theme::FOREGROUND, 1.0)).truncate().child(p.display_name.clone()))
                .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(meta)),
        )
}

fn catalog_section(locale: Locale, catalog: &Loadable<Catalog>) -> Div {
    let body: Div = match catalog {
        Loadable::Loading => grid_2col((0..2).map(|_| card_skeleton()).collect()),
        Loadable::Failed(msg) => empty_state("⚠️", i18n::t1(locale, "native.presets.loadError", "message", msg), None, None::<Div>),
        Loadable::Ready(cat) if cat.present_but_locked => empty_state("🔒", i18n::t(locale, "native.experts.locked"), None, None::<Div>),
        Loadable::Ready(cat) if !cat.deployed => empty_state("📭", i18n::t(locale, "native.experts.notDeployed"), None, None::<Div>),
        Loadable::Ready(cat) => {
            let groups = cc::group_by_category(&cat.entries, |e| e.category.as_str());
            let mut col = div().flex().flex_col().gap_4();
            for (category, entries) in groups {
                col = col.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(cc::category_group_header(cc::category_label(locale, category)))
                        .child(grid_2col(entries.iter().map(|e| catalog_card(locale, e)).collect())),
                );
            }
            col
        }
    };
    div().flex().flex_col().gap_2().child(cc::category_group_header(i18n::t(locale, "native.experts.section.summonable"))).child(body)
}

fn grid_2col(cards: Vec<Div>) -> Div {
    div().flex().flex_wrap().gap_3().children(cards.into_iter().map(|c| c.flex_1().min_w(px(320.))))
}

fn card_skeleton() -> Div {
    cc::catalog_card().child(skeleton(px(160.), px(14.))).child(skeleton(px(220.), px(12.))).child(skeleton(px(100.), px(10.)))
}

fn catalog_card(locale: Locale, e: &CatalogEntry) -> Div {
    let title = div().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::MEDIUM).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(e.label.clone());

    let mut card = cc::catalog_card().child(title);

    if !e.description.is_empty() {
        card = card.child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(e.description.clone()));
    }

    card = card.child(
        div()
            .text_size(px(theme::TEXT_XS))
            .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
            .child(i18n::t1(locale, "native.experts.agentsCount", "count", &e.agents_count.to_string())),
    );

    if !e.human_titles.is_empty() {
        card = card.child(
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(i18n::t1(locale, "native.experts.humans", "titles", &e.human_titles.join("、"))),
        );
    }
    if !e.excluded.is_empty() {
        let items = e.excluded.iter().map(|(kit, reason)| i18n::tn(locale, "native.experts.excludedItem", &[("kit", kit), ("reason", reason)]).to_string()).collect::<Vec<_>>().join("、");
        card = card.child(
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(i18n::t1(locale, "native.experts.excluded", "items", &items)),
        );
    }

    let action = if e.installed {
        div().flex().justify_end().child(badge(i18n::t(locale, "native.experts.installed"), BadgeVariant::Success))
    } else {
        let label = if e.kind == "expert" { i18n::t(locale, "native.experts.summonExpert") } else { i18n::t(locale, "native.experts.summon") };
        div().flex().justify_end().child(button(format!("experts-summon-{}", e.slug), label, ButtonVariant::Primary, false, None, |_ev, _window, _app| {}))
    };

    card.child(action)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_installed_packs_reads_real_handler_shape() {
        let v = json!({ "packs": [
            { "slug": "cs-team", "display_name": "客服作業組", "agents": ["a", "b", "c"], "installed_at": "2026-08-12T00:00:00Z" },
        ]});
        let rows = parse_installed_packs(&v);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].agents_count, 3);
        assert_eq!(rows[0].installed_at.as_deref(), Some("2026-08-12T00:00:00Z"));
    }

    #[test]
    fn parse_installed_packs_missing_display_name_falls_back_to_slug() {
        let v = json!({ "packs": [{ "slug": "hq", "agents": [] }] });
        let rows = parse_installed_packs(&v);
        assert_eq!(rows[0].display_name, "hq");
    }

    #[test]
    fn parse_catalog_reads_real_builtin_catalog_shape() {
        let v = json!({
            "deployed": true, "unlocked": true, "present_but_locked": false,
            "packs": [{
                "kind": "team", "industry": "clinic", "category": "health",
                "departments": ["front_desk"], "label": "牙醫診所前台團隊", "slug": "clinic-team",
                "description": "", "agents_count": 4, "installed": false,
                "members": [{ "role": "front_desk", "name": "clinic_fd", "display_name": "前台", "summary": "接待病患" }],
                "humans": [{ "title": "看診醫師", "summary": "" }, { "title": "麻醉師", "summary": "" }],
                "excluded": [], "examples": [], "lead_agent_name": null,
            }],
        });
        let cat = parse_catalog(&v);
        assert!(cat.deployed);
        assert!(!cat.present_but_locked);
        assert_eq!(cat.entries.len(), 1);
        let e = &cat.entries[0];
        assert_eq!(e.category, "health");
        assert_eq!(e.agents_count, 4);
        assert!(!e.installed);
        assert_eq!(e.members.len(), 1);
        assert_eq!(e.members[0].display_name, "前台");
        assert_eq!(e.human_titles, vec!["看診醫師".to_string(), "麻醉師".to_string()]);
    }

    #[test]
    fn parse_catalog_absent_premium_tree_is_not_deployed_not_an_error() {
        let cat = parse_catalog(&json!({ "deployed": false, "packs": [] }));
        assert!(!cat.deployed);
        assert!(cat.entries.is_empty());
    }

    #[test]
    fn parse_catalog_reads_excluded_kit_reason_pairs() {
        let v = json!({ "deployed": true, "packs": [{
            "slug": "ecom-team", "category": "retail", "label": "電商營運團隊", "agents_count": 6,
            "excluded": [{ "kit": "出貨包裝", "reason": "需要人工品管" }],
        }]});
        let cat = parse_catalog(&v);
        assert_eq!(cat.entries[0].excluded, vec![("出貨包裝".to_string(), "需要人工品管".to_string())]);
    }
}
