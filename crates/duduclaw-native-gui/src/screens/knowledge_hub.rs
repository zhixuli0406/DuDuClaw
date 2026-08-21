// WP-S6b3-Q (S6b 第三波, 2026-08-22) — "知識中樞" (`KnowledgeHub.dc.html`,
// B25). Originally shipped with no `nav.rs` id — self-attached in `screens/
// shell.rs` per this wave's own "D 先掛好分支就直接可達" precedent — but
// WP-S6b3-fix (2026-08-22) added a real `nav.rs` id (`knowledgeHub`, in
// `KNOWLEDGE_ITEMS` between `memory` and `widgets`; see `knowledge_common.
// rs`'s module doc comment for the full side-nav-highlight fix this
// implies). Still also reachable directly via
// `DUDUCLAW_NATIVE_GUI_DEBUG_PAGE=knowledgeHub`.
//
// Visual authority: `KnowledgeHub.dc.html` — 5-tab segmented switcher (瀏覽/
// 搜尋/圖譜/健康度/審核, `knowledge_common::shell_tabs`) → "新增頁面" header
// button → three-column browser (190px folder rail / 250px page list / flex
// content column). Functional reference only (per this task's "版面禁抄
// web"): `web/src/pages/KnowledgeHubPage.tsx`.
//
// ── RPC shapes (read directly from `crates/duduclaw-gateway/src/handlers.
// rs`, never guessed) ──────────────────────────────────────────────────
//   `agents.list {}` (~L5396) — reused via `crate::screens::agents_data::
//   parse_agents_list`, same "first agent in the list becomes the default
//   selection" fallback `wiki_trust.rs` already establishes (this page has
//   no visible agent picker either — the canvas draws none, matching).
//   `wiki.pages` (~L14050, `handle_wiki_pages`) — params `{agent_id}` →
//   `{"pages":[{"path","title","updated","tags"}],"exists"}`, parsed via
//   `knowledge_common::parse_wiki_pages` (shared with `shared_wiki.rs`).
//   `wiki.read` (~L14265, `handle_wiki_read`) — params `{agent_id,
//   page_path}` → `{"content","path"}`.
//   `wiki.stats` (~L14385, `handle_wiki_stats`) — params `{agent_id}` →
//   `{"exists","total_pages","by_directory":{dir:count},"most_recent"}`.
//   `wiki.lint` (~L14351, `handle_wiki_lint`) — params `{agent_id}` →
//   `{"total_pages","index_entries","orphan_pages":[...],"broken_links":
//   [[from,to]],"stale_pages":[...],"healthy"}`. Both fetched together for
//   the 健康度 tab (mirrors `HealthView`'s own `Promise.all` in the web
//   reference).
//
// ── Scope cut across the 5 tabs (documented, not silent) ─────────────────
// Only 瀏覽 and 健康度 are wired to real data this pass. 搜尋 renders the
// same "decorative search box, assembled not wired" honest stub `skills.
// rs`'s own market tab and `marketplace.rs` (this same wave) already
// establish — wiring a real free-text query needs `TextField`'s IME-capable
// `Entity`, out of scope for this page too. 圖譜 is an explicit placeholder
// per this task's own brief ("圖譜視圖誠實佔位——WikiGraph spike 稿留後") —
// the canvas's OWN embedded warning banner already states the same thing
// verbatim ("圖譜視圖（WikiGraph）為次要視圖，需要 gpui 節點+連線+縮放平移的
// spike 驗證...本頁僅示意瀏覽視圖為主要入口"), so this page's placeholder
// copy quotes that reasoning rather than inventing new wording. 審核
// navigates to the separate `knowledgeCuration` page (see `knowledge_common.
// rs`'s own doc comment on why that is NOT an inline tab panel here).
//
// ── Honest deviations from the design canvas ─────────────────────────────
// 1. "新增頁面" header button is assembled but not wired — creating a wiki
//    page from the dashboard has no backing RPC at all (`wiki.pages`/`wiki.
//    read` are read-only; page creation only happens through the agent's
//    own conversational/auto-filing paths) — same `mds_gpui::button(...,
//    disabled: true, ...)` idiom every other decision-class control in this
//    wave's five pages uses.
// 2. Folder sidebar counts are REAL (grouped from the live `wiki.pages`
//    response by `knowledge_common::namespace_of`), not the canvas's
//    illustrative "SOP 與流程 12 / 產品規格 9 / ..." mockup rows — an
//    agent's actual wiki namespaces are whatever directories its pages
//    happen to live under, which this page cannot predict.

use gpui::{div, prelude::*, px, Context, Div, Global, SharedString, Stateful};
use serde_json::json;

use crate::i18n::{self, Locale};
use crate::mds_gpui::{button, empty_state, skeleton, ButtonVariant};
use crate::screens::agents_data::{self, AgentListItem};
use crate::screens::catalog_common::spawn_call;
use crate::screens::dashboard::{error_row, Loadable};
use crate::screens::knowledge_common::{self as kc, KnowledgeView, WikiPageMeta};
use crate::theme;
use crate::ws_status::WsConnState;
use crate::RootView;

// ── Health data model ──────────────────────────────────────────────────

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WikiStatsSummary {
    pub exists: bool,
    pub total_pages: i64,
    pub directory_count: usize,
}

pub fn parse_wiki_stats(v: &serde_json::Value) -> WikiStatsSummary {
    WikiStatsSummary {
        exists: v.get("exists").and_then(serde_json::Value::as_bool).unwrap_or(false),
        total_pages: v.get("total_pages").and_then(serde_json::Value::as_i64).unwrap_or(0),
        directory_count: v.get("by_directory").and_then(serde_json::Value::as_object).map(|m| m.len()).unwrap_or(0),
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WikiLintSummary {
    pub orphan_count: usize,
    pub broken_link_count: usize,
    pub stale_count: usize,
    pub healthy: bool,
}

pub fn parse_wiki_lint(v: &serde_json::Value) -> WikiLintSummary {
    let arr_len = |key: &str| v.get(key).and_then(serde_json::Value::as_array).map(Vec::len).unwrap_or(0);
    WikiLintSummary {
        orphan_count: arr_len("orphan_pages"),
        broken_link_count: arr_len("broken_links"),
        stale_count: arr_len("stale_pages"),
        healthy: v.get("healthy").and_then(serde_json::Value::as_bool).unwrap_or(true),
    }
}

// ── State ──────────────────────────────────────────────────────────────

pub struct KnowledgeHubState {
    requested_agents: bool,
    pub agents: Loadable<Vec<AgentListItem>>,
    pub selected_agent: Option<String>,
    pub pages: Loadable<Vec<WikiPageMeta>>,
    fetched_pages_for: Option<String>,
    pub selected_folder: Option<String>,
    pub selected_path: Option<String>,
    pub page_content: Loadable<String>,
    fetched_content_for: Option<String>,
    pub stats: Loadable<WikiStatsSummary>,
    pub lint: Loadable<WikiLintSummary>,
    fetched_health_for: Option<String>,
}

impl KnowledgeHubState {
    fn new() -> Self {
        Self {
            requested_agents: false,
            agents: Loadable::Loading,
            selected_agent: None,
            pages: Loadable::Loading,
            fetched_pages_for: None,
            selected_folder: None,
            selected_path: None,
            page_content: Loadable::Loading,
            fetched_content_for: None,
            stats: Loadable::Loading,
            lint: Loadable::Loading,
            fetched_health_for: None,
        }
    }
}

impl Global for KnowledgeHubState {}

fn ensure_state(cx: &mut Context<RootView>) {
    if !cx.has_global::<KnowledgeHubState>() {
        cx.set_global(KnowledgeHubState::new());
    }
}

fn maybe_fetch_agents(state: &RootView, cx: &mut Context<RootView>) {
    if cx.global::<KnowledgeHubState>().requested_agents {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<KnowledgeHubState>().requested_agents = true;
    let tx = state.session_tx.clone();
    spawn_call(cx, tx, "agents.list", json!({}), |cx, result| match result {
        Ok(v) => {
            let list = agents_data::parse_agents_list(&v);
            if cx.global::<KnowledgeHubState>().selected_agent.is_none() {
                if let Some(first) = list.first() {
                    cx.global_mut::<KnowledgeHubState>().selected_agent = Some(first.id.clone());
                }
            }
            cx.global_mut::<KnowledgeHubState>().agents = Loadable::Ready(list);
        }
        Err(e) => cx.global_mut::<KnowledgeHubState>().agents = Loadable::Failed(e),
    });
}

fn maybe_fetch_pages(state: &RootView, cx: &mut Context<RootView>) {
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    let (agent, fetched_for) = {
        let st = cx.global::<KnowledgeHubState>();
        (st.selected_agent.clone(), st.fetched_pages_for.clone())
    };
    let Some(agent) = agent else { return };
    if fetched_for.as_ref() == Some(&agent) {
        return;
    }
    cx.global_mut::<KnowledgeHubState>().fetched_pages_for = Some(agent.clone());
    cx.global_mut::<KnowledgeHubState>().pages = Loadable::Loading;
    let tx = state.session_tx.clone();
    spawn_call(cx, tx, "wiki.pages", json!({ "agent_id": agent }), |cx, result| {
        cx.global_mut::<KnowledgeHubState>().pages = result.map(|v| kc::parse_wiki_pages(&v).0).into();
    });
}

fn maybe_fetch_content(state: &RootView, cx: &mut Context<RootView>) {
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    let (agent, path, fetched_for) = {
        let st = cx.global::<KnowledgeHubState>();
        (st.selected_agent.clone(), st.selected_path.clone(), st.fetched_content_for.clone())
    };
    let (Some(agent), Some(path)) = (agent, path) else { return };
    if fetched_for.as_ref() == Some(&path) {
        return;
    }
    cx.global_mut::<KnowledgeHubState>().fetched_content_for = Some(path.clone());
    cx.global_mut::<KnowledgeHubState>().page_content = Loadable::Loading;
    let tx = state.session_tx.clone();
    spawn_call(cx, tx, "wiki.read", json!({ "agent_id": agent, "page_path": path }), |cx, result| {
        cx.global_mut::<KnowledgeHubState>().page_content =
            result.map(|v| v.get("content").and_then(serde_json::Value::as_str).unwrap_or_default().to_string()).into();
    });
}

fn maybe_fetch_health(state: &RootView, cx: &mut Context<RootView>) {
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    let (agent, fetched_for) = {
        let st = cx.global::<KnowledgeHubState>();
        (st.selected_agent.clone(), st.fetched_health_for.clone())
    };
    let Some(agent) = agent else { return };
    if fetched_for.as_ref() == Some(&agent) {
        return;
    }
    cx.global_mut::<KnowledgeHubState>().fetched_health_for = Some(agent.clone());
    cx.global_mut::<KnowledgeHubState>().stats = Loadable::Loading;
    cx.global_mut::<KnowledgeHubState>().lint = Loadable::Loading;
    let tx = state.session_tx.clone();
    spawn_call(cx, tx.clone(), "wiki.stats", json!({ "agent_id": agent }), |cx, result| {
        cx.global_mut::<KnowledgeHubState>().stats = result.map(|v| parse_wiki_stats(&v)).into();
    });
    spawn_call(cx, tx, "wiki.lint", json!({ "agent_id": agent }), |cx, result| {
        cx.global_mut::<KnowledgeHubState>().lint = result.map(|v| parse_wiki_lint(&v)).into();
    });
}

// ── Folder sidebar ─────────────────────────────────────────────────────

fn folder_row(label: SharedString, count: usize, selected: bool, id: SharedString, folder: Option<String>, cx: &mut Context<RootView>) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_2()
        .px_2p5()
        .py_1p5()
        .rounded(px(theme::RADIUS_MD))
        .cursor_pointer()
        .when(selected, |el| el.bg(theme::alpha(theme::BRAND, 0.12)).text_color(theme::alpha(theme::BRAND, 1.0)).font_weight(gpui::FontWeight::MEDIUM))
        .when(!selected, |el| el.text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).hover(|s| s.bg(theme::alpha(theme::MUTED, 0.4))))
        .text_size(px(theme::TEXT_XS))
        .child(div().flex_1().min_w_0().overflow_hidden().child(label))
        .child(div().text_size(px(10.5)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.8)).child(count.to_string()))
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            cx.global_mut::<KnowledgeHubState>().selected_folder = folder.clone();
            cx.notify();
        }))
}

fn folder_sidebar(locale: Locale, pages: &[WikiPageMeta], selected_folder: &Option<String>, cx: &mut Context<RootView>) -> Stateful<Div> {
    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for p in pages {
        *counts.entry(kc::namespace_of(&p.path)).or_insert(0) += 1;
    }

    let mut col = div()
        .id("kh-folder-sidebar")
        .w(px(190.))
        .flex_shrink_0()
        .h_full()
        .overflow_y_scroll()
        .border_r_1()
        .border_color(theme::border())
        .p_2()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(folder_row(i18n::t(locale, "knowledgeHub.folder.all"), pages.len(), selected_folder.is_none(), "kh-folder-all".into(), None, cx));

    for (ns, count) in counts {
        if ns.is_empty() {
            continue;
        }
        let label: SharedString = ns.to_string().into();
        let selected = selected_folder.as_deref() == Some(ns);
        let id: SharedString = format!("kh-folder-{ns}").into();
        col = col.child(folder_row(label, count, selected, id, Some(ns.to_string()), cx));
    }
    col
}

// ── Page list column ───────────────────────────────────────────────────

fn page_list_row(p: &WikiPageMeta, selected: bool, cx: &mut Context<RootView>) -> Stateful<Div> {
    let path = p.path.clone();
    let title: SharedString = if p.title.is_empty() { p.path.clone().into() } else { p.title.clone().into() };
    div()
        .id(SharedString::from(format!("kh-page-{}", p.path)))
        .flex()
        .flex_col()
        .gap_0p5()
        .px_3()
        .py_2()
        .cursor_pointer()
        .border_b_1()
        .border_color(theme::border())
        .when(selected, |el| el.bg(theme::alpha(theme::BRAND, 0.08)).border_l_2().border_color(theme::alpha(theme::BRAND, 1.0)))
        .when(!selected, |el| el.hover(|s| s.bg(theme::alpha(theme::MUTED, 0.3))))
        .child(div().text_size(px(theme::TEXT_XS)).font_weight(gpui::FontWeight::MEDIUM).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(title))
        .child(div().text_size(px(10.5)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 0.8)).child(SharedString::from(p.path.clone())))
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            cx.global_mut::<KnowledgeHubState>().selected_path = Some(path.clone());
            cx.notify();
        }))
}

// ── Browse view ────────────────────────────────────────────────────────

fn browse_view(state: &RootView, cx: &mut Context<RootView>) -> Div {
    let locale = state.locale;
    let (pages, selected_folder, selected_path, content) = {
        let st = cx.global::<KnowledgeHubState>();
        (st.pages.clone(), st.selected_folder.clone(), st.selected_path.clone(), st.page_content.clone())
    };

    let pages_list: Vec<WikiPageMeta> = match &pages {
        Loadable::Ready(list) => list.clone(),
        _ => Vec::new(),
    };

    let body: Div = match &pages {
        Loadable::Loading => div().flex().flex_col().gap_2().p_3().child(skeleton(px(600.), px(40.))).child(skeleton(px(600.), px(40.))),
        Loadable::Failed(e) => div().p_3().child(error_row(locale, e)),
        Loadable::Ready(list) if list.is_empty() => div().flex_1().child(empty_state("📚", i18n::t(locale, "knowledgeHub.empty"), None, None::<Div>)),
        Loadable::Ready(_) => {
            let filtered: Vec<&WikiPageMeta> = pages_list
                .iter()
                .filter(|p| match &selected_folder {
                    Some(ns) => kc::namespace_of(&p.path) == ns,
                    None => true,
                })
                .collect();

            let mut list_col = div().id("kh-page-list").w(px(250.)).flex_shrink_0().h_full().overflow_y_scroll().border_r_1().border_color(theme::border());
            for p in &filtered {
                list_col = list_col.child(page_list_row(p, selected_path.as_deref() == Some(p.path.as_str()), cx));
            }

            let content_col: Stateful<Div> = match &selected_path {
                None => div().id("kh-content-empty").flex_1().h_full().flex().items_center().justify_center().child(
                    div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "knowledgeHub.selectHint")),
                ),
                Some(path) => {
                    let page = pages_list.iter().find(|p| &p.path == path);
                    let title: SharedString = page.map(|p| if p.title.is_empty() { p.path.clone() } else { p.title.clone() }).unwrap_or_else(|| path.clone()).into();
                    let updated = page.map(|p| p.updated.clone()).unwrap_or_default();
                    let body_text: Div = match &content {
                        Loadable::Loading => div().mt_4().flex().flex_col().gap_2().child(skeleton(px(400.), px(14.))).child(skeleton(px(500.), px(14.))),
                        Loadable::Failed(e) => div().mt_4().child(error_row(locale, e)),
                        Loadable::Ready(text) => div()
                            .mt_4()
                            .text_size(px(theme::TEXT_SM))
                            .text_color(theme::alpha(theme::FOREGROUND, 0.92))
                            .child(SharedString::from(text.clone())),
                    };
                    div()
                        .id("kh-content-scroll")
                        .flex_1()
                        .h_full()
                        .overflow_y_scroll()
                        .p(px(28.))
                        .child(div().max_w(px(620.)).child(div().text_size(px(19.)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(title)))
                        .child(div().max_w(px(620.)).text_size(px(11.5)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(SharedString::from(updated)))
                        .child(div().max_w(px(620.)).child(body_text))
                }
            };

            div()
                .flex_1()
                .flex()
                .overflow_hidden()
                .rounded(px(theme::RADIUS_XL))
                .bg(theme::alpha(theme::SURFACE, 1.0))
                .border_1()
                .border_color(theme::surface_border())
                .child(folder_sidebar(locale, &pages_list, &selected_folder, cx))
                .child(list_col)
                .child(content_col)
        }
    };

    div().flex_1().flex().flex_col().overflow_hidden().child(body)
}

// ── Health view ────────────────────────────────────────────────────────

fn stat_tile(label: SharedString, value: String, color: u32) -> Div {
    div()
        .flex_1()
        .min_w(px(140.))
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .px_4()
        .py_3()
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(label))
        .child(div().mt_1().text_size(px(19.)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(color, 1.0)).child(value))
}

fn health_view(state: &RootView, cx: &mut Context<RootView>) -> Div {
    let locale = state.locale;
    maybe_fetch_health(state, cx);
    let (stats, lint) = {
        let st = cx.global::<KnowledgeHubState>();
        (st.stats.clone(), st.lint.clone())
    };

    match (&stats, &lint) {
        (Loadable::Loading, _) | (_, Loadable::Loading) => {
            div().p_3().flex().gap_2().child(skeleton(px(160.), px(64.))).child(skeleton(px(160.), px(64.))).child(skeleton(px(160.), px(64.)))
        }
        (Loadable::Failed(e), _) => div().p_3().child(error_row(locale, e)),
        (_, Loadable::Failed(e)) => div().p_3().child(error_row(locale, e)),
        (Loadable::Ready(s), Loadable::Ready(l)) => div()
            .p_3()
            .flex()
            .flex_wrap()
            .gap_2p5()
            .child(stat_tile(i18n::t(locale, "knowledgeHub.stats.total"), s.total_pages.to_string(), theme::FOREGROUND))
            .child(stat_tile(i18n::t(locale, "knowledgeHub.stats.dirs"), s.directory_count.to_string(), theme::FOREGROUND))
            .child(stat_tile(
                i18n::t(locale, "knowledgeHub.stats.orphans"),
                l.orphan_count.to_string(),
                if l.orphan_count > 0 { theme::WARNING } else { theme::SUCCESS },
            ))
            .child(stat_tile(
                i18n::t(locale, "knowledgeHub.stats.health"),
                i18n::t(locale, if l.healthy { "knowledgeHub.stats.healthy" } else { "knowledgeHub.stats.unhealthy" }).to_string(),
                if l.healthy { theme::SUCCESS } else { theme::DESTRUCTIVE },
            )),
    }
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    ensure_state(cx);
    maybe_fetch_agents(state, cx);
    maybe_fetch_pages(state, cx);
    maybe_fetch_content(state, cx);

    let locale = state.locale;
    let active_tab = kc::current_view_tab_id(cx);
    let view = cx.global::<kc::KnowledgeViewState>().view;

    let header = div()
        .flex()
        .items_center()
        .justify_between()
        .child(div().text_size(px(theme::TEXT_XL)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(i18n::t(locale, "knowledgeHub.title")))
        .child(button(
            "knowledgehub-new-page",
            i18n::t(locale, "knowledgeHub.newPage"),
            ButtonVariant::Primary,
            true, // see module doc comment deviation #1 — no page-create RPC exists.
            None,
            |_ev, _window, _cx| {},
        ));

    let content: Div = match view {
        KnowledgeView::Browse => browse_view(state, cx),
        KnowledgeView::Health => health_view(state, cx),
        KnowledgeView::Search => div().flex_1().p_3().child(empty_state(
            "🔍",
            i18n::t(locale, "knowledgeHub.search.title"),
            Some(i18n::t(locale, "knowledgeHub.search.desc")),
            None::<Div>,
        )),
        KnowledgeView::Graph => div().flex_1().p_3().child(
            div()
                .rounded(px(theme::RADIUS_LG))
                .bg(theme::alpha(theme::WARNING, 0.10))
                .border_1()
                .border_color(theme::alpha(theme::WARNING, 0.3))
                .px_3p5()
                .py_2p5()
                .text_size(px(11.5))
                .text_color(theme::alpha(theme::WARNING, 1.0))
                .child(i18n::t(locale, "knowledgeHub.graph.placeholder")),
        ),
    };

    div()
        .id("knowledgehub-page")
        .size_full()
        .flex()
        .flex_col()
        .gap_3()
        .p_3()
        .child(kc::shell_tabs(locale, active_tab, cx))
        .child(header)
        .child(content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_wiki_stats_reads_real_handler_shape() {
        let v = json!({ "exists": true, "total_pages": 12, "by_directory": { "sop": 5, "spec": 7 } });
        let s = parse_wiki_stats(&v);
        assert!(s.exists);
        assert_eq!(s.total_pages, 12);
        assert_eq!(s.directory_count, 2);
    }

    #[test]
    fn parse_wiki_lint_reads_real_handler_shape() {
        let v = json!({
            "orphan_pages": ["a.md"], "broken_links": [["a.md","b.md"]], "stale_pages": [],
            "healthy": false,
        });
        let l = parse_wiki_lint(&v);
        assert_eq!(l.orphan_count, 1);
        assert_eq!(l.broken_link_count, 1);
        assert_eq!(l.stale_count, 0);
        assert!(!l.healthy);
    }

    #[test]
    fn parse_wiki_lint_missing_fields_is_empty_not_panicking() {
        let l = parse_wiki_lint(&json!({}));
        assert_eq!(l.orphan_count, 0);
        assert!(l.healthy);
    }
}
