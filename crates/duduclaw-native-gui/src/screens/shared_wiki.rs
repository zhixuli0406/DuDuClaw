// WP-S6b3-Q (S6b 第三波, 2026-08-22) — "共享知識庫" (`SharedWiki.dc.html`,
// B25). Still no `nav.rs` id of its own — self-attached in `screens/
// shell.rs`, reachable via `DUDUCLAW_NATIVE_GUI_DEBUG_PAGE=sharedWiki`.
// WP-S6b3-fix (2026-08-22) resolved "active 高亮「知識中樞」（QA 裁定）" by
// adding a real `knowledgeHub` id to `nav.rs` and mapping this page's
// sidebar highlight onto that row — see `knowledge_common.rs`'s module doc
// comment for the fix and `nav::sidebar_active_id`'s own doc comment for
// the mapping.
//
// Visual authority: `SharedWiki.dc.html` — "同骨架" (this task's own words)
// as `KnowledgeHub.dc.html`'s 瀏覽 view: 190px folder rail / 250px page list
// / flex content column, MINUS the 5-tab strip and "新增頁面" button (the
// canvas draws neither on this page — cross-agent shared knowledge is
// curated by the operator, not authored inline here) PLUS a "跨 agent 共享"
// pill on the content column's title row. Functional reference only (per
// this task's "版面禁抄 web"): `web/src/pages/SharedWikiPage.tsx`.
//
// ── RPC shapes (read directly from `crates/duduclaw-gateway/src/handlers.
// rs`, never guessed) ──────────────────────────────────────────────────
//   `shared_wiki.pages` (dispatch ~L5883, handler `handle_shared_wiki_pages`
//   ~L14628) → params `{}` (no agent scope — this store is global) →
//   `{"pages":[{"path","title","updated","tags"}],"exists"}`, the SAME
//   shape `wiki.pages` returns — parsed via `knowledge_common::
//   parse_wiki_pages` (shared with `knowledge_hub.rs`).
//   `shared_wiki.read` (~L5884, ~L14656) → params `{page_path}` →
//   `{"content","path"}`.
//   `wiki_scope.get` (~L5587, `handle_wiki_scope_get` ~L10729) → params `{}`
//   → `{"namespaces":[{"namespace","mode","synced_from"}]}` — `mode` ∈
//   `{"agent_writable","read_only","operator_only"}`. Fetched once on
//   mount, purely to back the content column's real policy-mode subtitle
//   (see below) — this page renders no write UI at all regardless of mode
//   (this task's own brief: "唯讀"), so a namespace missing from this list
//   never blocks reading, only changes which label the subtitle shows.
//
// ── Honest deviations from the design canvas ─────────────────────────────
// 1. The canvas's own three folder rows ("全公司"/"客服組"/"行銷組"/"工程組")
//    and its content column's "唯讀（operator_only）" caption are
//    illustrative mockup values, not backed by any fixture this pass has
//    access to. This page groups by REAL `knowledge_common::namespace_of`
//    (first path segment of whatever `shared_wiki.pages` actually returns —
//    likely `sources/`, `departments/`, or root-level pages depending on
//    what has actually been shared) with REAL counts, and derives the
//    policy-mode subtitle from a REAL `wiki_scope.get` lookup — defaulting
//    to `agent_writable` for a namespace absent from that list, mirroring
//    `handle_wiki_scope_update`'s own documented default ("remove reverts to
//    agent_writable default"). "全公司" (canvas's root-namespace label) is
//    kept as this page's own label for the empty-namespace bucket (root-
//    level shared pages with no directory), matching the canvas's evident
//    intent for that one bucket without fabricating the other three.
// 2. "跨 agent 共享" badge is unconditional on every page here (unlike a
//    per-page fabricated claim) — every row `shared_wiki.pages` returns is,
//    by construction, a page living in the cross-agent shared store; the
//    badge states a structural fact about the data source, not a guessed
//    per-page attribute.
// 3. No department-membership filtering. The real store has no per-page
//    "which department can see this" field surfaced by any of the three
//    RPCs above (that access-control layer is `.scope.toml`'s namespace
//    MODE, already rendered honestly per §1 above, not a per-department
//    visibility list) — the folder sidebar narrows by namespace only, same
//    as `knowledge_hub.rs`'s own folder sidebar.

use gpui::{div, prelude::*, px, Context, Div, Global, SharedString, Stateful};
use serde_json::{json, Value};

use crate::i18n::{self, Locale};
use crate::mds_gpui::{badge, empty_state, skeleton, BadgeVariant};
use crate::screens::catalog_common::spawn_call;
use crate::screens::dashboard::{error_row, Loadable};
use crate::screens::knowledge_common::{self as kc, WikiPageMeta};
use crate::theme;
use crate::ws_status::WsConnState;
use crate::RootView;

// ── Namespace policy (`wiki_scope.get`) ─────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeNamespace {
    pub namespace: String,
    pub mode: String,
}

pub fn parse_wiki_scope(v: &Value) -> Vec<ScopeNamespace> {
    v.get("namespaces")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|n| {
                    Some(ScopeNamespace {
                        namespace: n.get("namespace")?.as_str()?.to_string(),
                        mode: n.get("mode").and_then(Value::as_str).unwrap_or("agent_writable").to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Absent from the list ⇒ `agent_writable` — mirrors the server's own
/// documented "remove reverts to agent_writable default" semantics
/// (`handle_wiki_scope_update`'s doc comment, handlers.rs:10741).
fn mode_for_namespace<'a>(scope: &'a [ScopeNamespace], ns: &str) -> &'a str {
    scope.iter().find(|n| n.namespace == ns).map(|n| n.mode.as_str()).unwrap_or("agent_writable")
}

fn mode_label_key(mode: &str) -> &'static str {
    match mode {
        "read_only" => "sharedWiki.mode.readOnly",
        "operator_only" => "sharedWiki.mode.operatorOnly",
        _ => "sharedWiki.mode.agentWritable",
    }
}

// ── State ──────────────────────────────────────────────────────────────

pub struct SharedWikiState {
    requested: bool,
    pub pages: Loadable<Vec<WikiPageMeta>>,
    pub scope: Loadable<Vec<ScopeNamespace>>,
    pub selected_folder: Option<String>,
    pub selected_path: Option<String>,
    pub content: Loadable<String>,
    fetched_content_for: Option<String>,
}

impl SharedWikiState {
    fn new() -> Self {
        Self {
            requested: false,
            pages: Loadable::Loading,
            scope: Loadable::Loading,
            selected_folder: None,
            selected_path: None,
            content: Loadable::Loading,
            fetched_content_for: None,
        }
    }
}

impl Global for SharedWikiState {}

fn ensure_state(cx: &mut Context<RootView>) {
    if !cx.has_global::<SharedWikiState>() {
        cx.set_global(SharedWikiState::new());
    }
}

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if cx.global::<SharedWikiState>().requested {
        return;
    }
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    cx.global_mut::<SharedWikiState>().requested = true;
    let tx = state.session_tx.clone();
    spawn_call(cx, tx.clone(), "shared_wiki.pages", json!({}), |cx, result| {
        cx.global_mut::<SharedWikiState>().pages = result.map(|v| kc::parse_wiki_pages(&v).0).into();
    });
    spawn_call(cx, tx, "wiki_scope.get", json!({}), |cx, result| {
        cx.global_mut::<SharedWikiState>().scope = result.map(|v| parse_wiki_scope(&v)).into();
    });
}

fn maybe_fetch_content(state: &RootView, cx: &mut Context<RootView>) {
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    let (path, fetched_for) = {
        let st = cx.global::<SharedWikiState>();
        (st.selected_path.clone(), st.fetched_content_for.clone())
    };
    let Some(path) = path else { return };
    if fetched_for.as_ref() == Some(&path) {
        return;
    }
    cx.global_mut::<SharedWikiState>().fetched_content_for = Some(path.clone());
    cx.global_mut::<SharedWikiState>().content = Loadable::Loading;
    let tx = state.session_tx.clone();
    spawn_call(cx, tx, "shared_wiki.read", json!({ "page_path": path }), |cx, result| {
        cx.global_mut::<SharedWikiState>().content =
            result.map(|v| v.get("content").and_then(Value::as_str).unwrap_or_default().to_string()).into();
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
            cx.global_mut::<SharedWikiState>().selected_folder = folder.clone();
            cx.notify();
        }))
}

fn folder_sidebar(locale: Locale, pages: &[WikiPageMeta], selected_folder: &Option<String>, cx: &mut Context<RootView>) -> Stateful<Div> {
    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for p in pages {
        counts.entry(kc::namespace_of(&p.path)).and_modify(|c| *c += 1).or_insert(1);
    }

    let mut col = div()
        .id("sw-folder-sidebar")
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
        .child(folder_row(i18n::t(locale, "sharedWiki.folder.all"), pages.len(), selected_folder.is_none(), "sw-folder-all".into(), None, cx));

    for (ns, count) in counts {
        // Empty-namespace bucket (root-level shared pages) — folded under the
        // canvas's "全公司" label rather than an inert blank row (see module
        // doc comment §1).
        let (label, folder_key): (SharedString, String) =
            if ns.is_empty() { (i18n::t(locale, "sharedWiki.folder.companyWide"), String::new()) } else { (ns.to_string().into(), ns.to_string()) };
        let selected = selected_folder.as_deref() == Some(folder_key.as_str());
        let id: SharedString = format!("sw-folder-{}", if ns.is_empty() { "root" } else { ns }).into();
        col = col.child(folder_row(label, count, selected, id, Some(folder_key), cx));
    }
    col
}

// ── Page list column ───────────────────────────────────────────────────

fn page_list_row(p: &WikiPageMeta, selected: bool, cx: &mut Context<RootView>) -> Stateful<Div> {
    let path = p.path.clone();
    let title: SharedString = if p.title.is_empty() { p.path.clone().into() } else { p.title.clone().into() };
    div()
        .id(SharedString::from(format!("sw-page-{}", p.path)))
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
            cx.global_mut::<SharedWikiState>().selected_path = Some(path.clone());
            cx.notify();
        }))
}

// ── Top-level render ───────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    ensure_state(cx);
    maybe_fetch(state, cx);
    maybe_fetch_content(state, cx);

    let locale = state.locale;
    let (pages, scope, selected_folder, selected_path, content) = {
        let st = cx.global::<SharedWikiState>();
        (st.pages.clone(), st.scope.clone(), st.selected_folder.clone(), st.selected_path.clone(), st.content.clone())
    };

    let header = div()
        .text_size(px(theme::TEXT_XL))
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
        .child(i18n::t(locale, "sharedWiki.title"));

    let body: Div = match &pages {
        Loadable::Loading => div().flex().flex_col().gap_2().p_3().child(skeleton(px(600.), px(40.))).child(skeleton(px(600.), px(40.))),
        Loadable::Failed(e) => div().p_3().child(error_row(locale, e)),
        Loadable::Ready(list) if list.is_empty() => div().flex_1().child(empty_state("🌐", i18n::t(locale, "sharedWiki.empty"), None, None::<Div>)),
        Loadable::Ready(list) => {
            let filtered: Vec<&WikiPageMeta> = list
                .iter()
                .filter(|p| match &selected_folder {
                    Some(ns) => kc::namespace_of(&p.path) == ns,
                    None => true,
                })
                .collect();

            let mut list_col = div().id("sw-page-list").w(px(250.)).flex_shrink_0().h_full().overflow_y_scroll().border_r_1().border_color(theme::border());
            for p in &filtered {
                list_col = list_col.child(page_list_row(p, selected_path.as_deref() == Some(p.path.as_str()), cx));
            }

            let content_col: Stateful<Div> = match &selected_path {
                None => div().id("sw-content-empty").flex_1().h_full().flex().items_center().justify_center().child(
                    div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t(locale, "sharedWiki.selectHint")),
                ),
                Some(path) => {
                    let page = list.iter().find(|p| &p.path == path);
                    let title: SharedString = page.map(|p| if p.title.is_empty() { p.path.clone() } else { p.title.clone() }).unwrap_or_else(|| path.clone()).into();
                    let ns = kc::namespace_of(path);
                    let ns_label: String = if ns.is_empty() { i18n::t(locale, "sharedWiki.folder.companyWide").to_string() } else { ns.to_string() };
                    let mode = match &scope {
                        Loadable::Ready(list) => mode_for_namespace(list, ns),
                        _ => "agent_writable",
                    };
                    let mode_label = i18n::t(locale, mode_label_key(mode)).to_string();
                    let subtitle = i18n::tn(locale, "sharedWiki.pageMeta", &[("namespace", &ns_label), ("mode", &mode_label)]);

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
                        .id("sw-content-scroll")
                        .flex_1()
                        .h_full()
                        .overflow_y_scroll()
                        .p(px(28.))
                        .child(
                            div()
                                .max_w(px(620.))
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(div().text_size(px(19.)).font_weight(gpui::FontWeight::BOLD).text_color(theme::alpha(theme::FOREGROUND, 1.0)).child(title))
                                .child(badge(i18n::t(locale, "sharedWiki.crossAgentBadge"), BadgeVariant::Info)),
                        )
                        .child(div().max_w(px(620.)).text_size(px(11.5)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(subtitle))
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
                .child(folder_sidebar(locale, list, &selected_folder, cx))
                .child(list_col)
                .child(content_col)
        }
    };

    div().id("sharedwiki-page").size_full().flex().flex_col().gap_3().p_3().child(header).child(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_wiki_scope_reads_real_handler_shape() {
        let v = json!({ "namespaces": [
            { "namespace": "identity", "mode": "read_only", "synced_from": "identity:read" },
            { "namespace": "policies", "mode": "operator_only", "synced_from": null },
        ]});
        let ns = parse_wiki_scope(&v);
        assert_eq!(ns.len(), 2);
        assert_eq!(ns[0].mode, "read_only");
        assert_eq!(ns[1].mode, "operator_only");
    }

    #[test]
    fn mode_for_namespace_defaults_to_agent_writable_when_absent() {
        let scope = vec![ScopeNamespace { namespace: "identity".into(), mode: "read_only".into() }];
        assert_eq!(mode_for_namespace(&scope, "identity"), "read_only");
        assert_eq!(mode_for_namespace(&scope, "sources"), "agent_writable");
    }

    #[test]
    fn parse_wiki_scope_missing_fields_is_empty_not_panicking() {
        assert!(parse_wiki_scope(&json!({})).is_empty());
        assert!(parse_wiki_scope(&json!(null)).is_empty());
    }
}
