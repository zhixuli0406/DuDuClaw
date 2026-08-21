// WP-S6b3-Q (S6b 第三波, 2026-08-22) — shared primitives for this pass's
// three "知識中樞" pages: `knowledge_hub.rs` (`KnowledgeHub.dc.html`, B25),
// `knowledge_curation.rs` (`KnowledgeCuration.dc.html`, B25+頁型3), and
// `shared_wiki.rs` (`SharedWiki.dc.html`, B25 — "同骨架" per this task's own
// brief). Same "one shared module for one design batch" precedent
// `catalog_common.rs`/`settings_common.rs`/`manage_advanced_common.rs`
// already establish for their own S5b/S6b batches — see those modules' own
// header comments for why sharing is right here and not this crate's
// general per-file-duplication default.
//
// Three things live here:
// 1. `WikiPageMeta` + `parse_wiki_pages` — `wiki.pages` (handlers.rs:14050)
//    and `shared_wiki.pages` (handlers.rs:14628) return the IDENTICAL shape
//    (`{ pages: [{path,title,updated,tags}], exists }`), so one parser
//    serves both `knowledge_hub.rs` (agent-scoped) and `shared_wiki.rs`
//    (no agent scope).
// 2. `namespace_of` — first path segment (`"sop/x.md"` → `"sop"`, `"x.md"`
//    → `""`), the same folder-grouping key `KnowledgeHubPage.tsx`'s own
//    `namespaceOf` helper and `SharedWikiPage.tsx`'s copy of it use. Powers
//    both pages' folder sidebar (canvas: 190px column, namespace + count).
// 3. `KnowledgeView` + `KnowledgeViewState` (`Global`) + `shell_tabs` — the
//    5-way 瀏覽/搜尋/圖譜/健康度/審核 segmented switcher both `KnowledgeHub.
//    dc.html` and `KnowledgeCuration.dc.html` draw as the identical pill
//    strip at the top of their content column. Rendered via `mds_gpui::tabs`
//    (underline style) — the same established substitution `wiki_trust.rs`/
//    `governance.rs`'s own `GovernanceShell::shell_tabs` already makes for a
//    canvas that draws a filled-pill segmented control (this crate's `tabs`
//    primitive is underline-only; reusing the shipped, reviewed component
//    beats hand-rolling a second segmented-control look for one batch).
//    Clicking 瀏覽/搜尋/圖譜/健康度 sets `KnowledgeViewState.view` AND
//    navigates `active_page = "knowledgeHub"` (so `KnowledgeCuration`'s own
//    copy of this tab strip can jump back into the hub on the exact view it
//    names, not just always defaulting to 瀏覽); clicking 審核 navigates to
//    `active_page = "knowledgeCuration"` — a separate, real, self-attached
//    page per this task's own item 4, not an inline tab panel (unlike
//    `KnowledgeHubPage.tsx`'s own `view === 'curate'` embedding).
//
// ── Side-nav highlight: RESOLVED by WP-S6b3-fix (2026-08-22) ─────────────
// The task brief asked for "active 高亮「知識中樞」" on all three pages. At
// the time this module was authored no `nav.rs` id named `knowledgeHub`/
// anything "知識中樞"-shaped existed anywhere in `AREAS`/`FOOTER_ITEMS` (this
// wave's own boundary was "nav.rs 不歸你動"), so the QA ruling was "暫掛"
// (deferred) — same structural gap `mcp.rs`/`odoo.rs`/`google_integration.
// rs`/`identity.rs`/`marketplace.rs` all still carry today for their own
// "整合" concept.
//
// `nav.rs`'s follow-up fix pass (WP-S6b3-fix) added a real `knowledgeHub`
// id to `KNOWLEDGE_ITEMS` and a `nav::sidebar_active_id()` mapping function
// so `knowledgeCuration`/`sharedWiki` — still self-attached leaves with no
// `nav.rs` id of their own — light up the `knowledgeHub` row in both
// Column 1 (area) and Column 2 (page list) while showing. `knowledgeHub`
// itself needs no special-casing: its `active_page` value now equals a real
// `NavItem.id`, so the existing `active_id == item.id` comparison in
// `shell_row::nav_row` lights it up exactly like any other real nav page.
// Documented here once rather than repeated in each of the three pages' own
// doc comments (their own headers still narrate the pre-fix "暫掛" history
// for context, but this is the module that actually implements the fix).

use gpui::{div, prelude::*, Context, Div, Global};
use serde_json::Value;

use crate::i18n::{self, Locale};
use crate::mds_gpui::{tabs, TabItem};
use crate::RootView;

// ── Page list (shared `wiki.pages` / `shared_wiki.pages` shape) ─────────

// `pub`, not `pub(super)` — this type is embedded in fully-`pub` state
// structs (`KnowledgeHubState::pages`/`SharedWikiState::pages`), and Rust's
// "private type in public interface" lint requires it to be at least as
// visible as those. Same "individual item widened to plain `pub` even
// though its containing `mod` is private" convention `agents_data.rs`'s own
// `AgentListItem` already establishes (that mod is `mod agents_data;`, not
// `pub mod`, but the type itself is fully `pub` for exactly this reason).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WikiPageMeta {
    pub path: String,
    pub title: String,
    pub updated: String,
    #[allow(dead_code)] // parsed for shape-fidelity; no page in this batch renders tags yet
    pub tags: Vec<String>,
}

/// Returns `(pages, exists)` — `exists: false` (no `wiki/` directory yet)
/// is a distinct state from "directory exists but is empty", mirrored from
/// both handlers' own `{"pages":[],"exists":false}` early-return shape.
pub(super) fn parse_wiki_pages(v: &Value) -> (Vec<WikiPageMeta>, bool) {
    let pages = v
        .get("pages")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|p| {
                    Some(WikiPageMeta {
                        path: p.get("path")?.as_str()?.to_string(),
                        title: p.get("title").and_then(Value::as_str).unwrap_or_default().to_string(),
                        updated: p.get("updated").and_then(Value::as_str).unwrap_or_default().to_string(),
                        tags: p
                            .get("tags")
                            .and_then(Value::as_array)
                            .map(|t| t.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                            .unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let exists = v.get("exists").and_then(Value::as_bool).unwrap_or(false);
    (pages, exists)
}

/// First path segment — `""` for a root-level page (no `/`). Matches
/// `KnowledgeHubPage.tsx`/`SharedWikiPage.tsx`'s own `namespaceOf`.
pub(super) fn namespace_of(path: &str) -> &str {
    match path.find('/') {
        Some(idx) if idx > 0 => &path[..idx],
        _ => "",
    }
}

// ── Shared 5-tab view switcher ────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum KnowledgeView {
    Browse,
    Search,
    Graph,
    Health,
}

impl KnowledgeView {
    fn tab_id(self) -> &'static str {
        match self {
            KnowledgeView::Browse => "browse",
            KnowledgeView::Search => "search",
            KnowledgeView::Graph => "graph",
            KnowledgeView::Health => "health",
        }
    }
}

/// Which of the 4 in-hub views is selected — read by `knowledge_hub.rs`,
/// written by both `knowledge_hub.rs` (clicking its own tabs) and
/// `knowledge_curation.rs` (clicking a tab that jumps back into the hub).
/// Deliberately its own `Global`, not a field on either page's own state
/// struct — both pages' tab strips need to WRITE it, and only
/// `knowledge_hub.rs` needs to READ it.
pub(super) struct KnowledgeViewState {
    pub view: KnowledgeView,
}

impl Global for KnowledgeViewState {}

pub(super) fn ensure_view_state(cx: &mut Context<RootView>) {
    if !cx.has_global::<KnowledgeViewState>() {
        cx.set_global(KnowledgeViewState { view: KnowledgeView::Browse });
    }
}

pub(super) fn current_view_tab_id(cx: &mut Context<RootView>) -> &'static str {
    ensure_view_state(cx);
    cx.global::<KnowledgeViewState>().view.tab_id()
}

/// The 5-tab strip itself. `active` is one of `"browse"/"search"/"graph"/
/// "health"/"curate"` — `knowledge_hub.rs` passes `current_view_tab_id(cx)`,
/// `knowledge_curation.rs` always passes `"curate"`.
pub(super) fn shell_tabs(locale: Locale, active: &'static str, cx: &mut Context<RootView>) -> Div {
    ensure_view_state(cx);
    let items = vec![
        TabItem::new(
            "browse",
            i18n::t(locale, "knowledgeHub.tab.browse"),
            cx.listener(|this, _ev, _window, cx| {
                cx.global_mut::<KnowledgeViewState>().view = KnowledgeView::Browse;
                this.active_page = "knowledgeHub";
                cx.notify();
            }),
        ),
        TabItem::new(
            "search",
            i18n::t(locale, "knowledgeHub.tab.search"),
            cx.listener(|this, _ev, _window, cx| {
                cx.global_mut::<KnowledgeViewState>().view = KnowledgeView::Search;
                this.active_page = "knowledgeHub";
                cx.notify();
            }),
        ),
        TabItem::new(
            "graph",
            i18n::t(locale, "knowledgeHub.tab.graph"),
            cx.listener(|this, _ev, _window, cx| {
                cx.global_mut::<KnowledgeViewState>().view = KnowledgeView::Graph;
                this.active_page = "knowledgeHub";
                cx.notify();
            }),
        ),
        TabItem::new(
            "health",
            i18n::t(locale, "knowledgeHub.tab.health"),
            cx.listener(|this, _ev, _window, cx| {
                cx.global_mut::<KnowledgeViewState>().view = KnowledgeView::Health;
                this.active_page = "knowledgeHub";
                cx.notify();
            }),
        ),
        TabItem::new(
            "curate",
            i18n::t(locale, "knowledgeHub.tab.curate"),
            cx.listener(|this, _ev, _window, cx| {
                this.active_page = "knowledgeCuration";
                cx.notify();
            }),
        ),
    ];
    div().w_full().child(tabs(items, active))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_wiki_pages_reads_shared_shape() {
        let v = json!({ "pages": [
            { "path": "sop/退換貨.md", "title": "退換貨標準流程", "updated": "2026-08-20T10:00:00Z", "tags": ["sop"] },
        ], "exists": true });
        let (pages, exists) = parse_wiki_pages(&v);
        assert!(exists);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].path, "sop/退換貨.md");
        assert_eq!(pages[0].title, "退換貨標準流程");
    }

    #[test]
    fn parse_wiki_pages_missing_exists_defaults_false_not_panicking() {
        let (pages, exists) = parse_wiki_pages(&json!({}));
        assert!(pages.is_empty());
        assert!(!exists);
        assert!(parse_wiki_pages(&json!(null)).0.is_empty());
    }

    #[test]
    fn namespace_of_first_segment_or_root() {
        assert_eq!(namespace_of("sop/退換貨.md"), "sop");
        assert_eq!(namespace_of("root.md"), "");
        assert_eq!(namespace_of("a/b/c.md"), "a");
    }
}
