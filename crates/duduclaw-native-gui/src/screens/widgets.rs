// WP-S5b2-E (2026-08-21) — "Widget 工坊" (`nav.rs` id `widgets`, wired into
// `KNOWLEDGE_ITEMS` by the parallel D package — see `nav.rs`'s S5b2-D
// header note).
//
// Visual authority: `commercial/design/duduclaw-s5-work-pages/Widgets.dc.
// html` — 我的/團隊分享 tabs over a 4-column thumbnail-card grid. Mirrors
// `web/src/pages/WidgetsPage.tsx`'s `mine`/`shared` split — this page reads
// the same `widgets.custom.list` RPC and applies the identical client-side
// filter, but authoring/sharing/import-export/add-to-board are all out of
// this WP's 2-page-scope pass (header buttons + card kebab menu render
// decoratively — assembled, not wired, per the task brief).
//
// UPDATE (WP-S6b2-O, 2026-08-21): the 新增 button now navigates for real —
// `active_page = "widgetComposer"` — reaching `screens/widget_composer.rs`
// (a creation-only "新增 Widget" leaf, see that module's own doc comment).
// HTML 自訂/匯入 stay exactly as decorative as this paragraph originally
// described; only 新增 changed.
//
// ── RPC shape (read directly from `crates/duduclaw-gateway/src/handlers.
// rs`, never guessed; open to any authenticated user — no `require_*!()`
// gate on `widgets.custom.list`) ─────────────────────────────────────────
//   `widgets.custom.list {}` (`handle_widgets_custom_list` ~L26111, via
//   `custom_widget_summary` ~L26097) → `{"widgets":[{"id","title",
//   "description","origin","created_by_user","shared","html_bytes",
//   "created_at","updated_at"}],"max_per_user"}`. `origin` is `"html"` or
//   `"ai"` (`WidgetOrigin::as_str`, `crates/duduclaw-gateway/src/
//   custom_widgets.rs:57-70`). `list_visible(&ctx.user_id)` already scopes
//   the server-side result to "my own rows (any `shared` value) + every
//   OTHER user's `shared: true` rows" — this page does the client-side
//   `created_by_user == my id` / `!= my id` split on top, exactly
//   `WidgetsPage.tsx`'s own `mine`/`shared` `useMemo` filters.
//
// ── Why this needed a `RootView` field (main.rs, NOT nav.rs/shell.rs) ────
// The mine/shared split needs "my own user id" — `RootView` only carried
// `display_name` before this pass (S4b), no id. `user_id: Option<String>`
// was added right next to `display_name` in `main.rs` (same two capture
// call sites, `SessionEvent::LocalSessionOk`/`LoginOk`) — see that field's
// own doc comment. This is a `RootView` field addition, not a
// `nav.rs`/`shell.rs` sidebar-wiring change, so it stays inside this WP's
// scope (the task brief only carves out sidebar/nav/shell wiring for the
// parallel D package).
//
// ── Deviations from the canvas (documented, not silent) ──────────────────
// 1. Thumbnails — the canvas shows a static bar-chart placeholder; the REAL
//    web page (`WidgetThumb`) mounts a LIVE sandboxed iframe rendering the
//    widget's actual HTML. This crate has no HTML-sandbox rendering
//    capability (gpui is not a browser engine) — every card shows the same
//    decorative placeholder swatch the canvas draws, honestly labeled by
//    this comment as chrome, not data.
// 2. Origin badge — added beyond the canvas (small, data-driven, real
//    `origin` field): each card also shows "AI 產生"/"HTML" so the two
//    authoring paths `WidgetsPage.tsx` distinguishes stay visible here too.

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
pub struct WidgetSummary {
    pub id: String,
    pub title: String,
    pub description: String,
    pub origin: String,
    pub created_by_user: String,
    pub shared: bool,
}

pub fn parse_widgets(v: &Value) -> Vec<WidgetSummary> {
    v.get("widgets")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|w| {
                    let id = w.get("id").and_then(Value::as_str)?.to_string();
                    if id.is_empty() {
                        return None;
                    }
                    Some(WidgetSummary {
                        id,
                        title: w.get("title").and_then(Value::as_str).unwrap_or("").to_string(),
                        description: w.get("description").and_then(Value::as_str).unwrap_or("").to_string(),
                        origin: w.get("origin").and_then(Value::as_str).unwrap_or("html").to_string(),
                        created_by_user: w.get("created_by_user").and_then(Value::as_str).unwrap_or("").to_string(),
                        shared: w.get("shared").and_then(Value::as_bool).unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WidgetsTab {
    Mine,
    Shared,
}

// ── State ──────────────────────────────────────────────────────────────

pub struct WidgetsState {
    requested: bool,
    pub widgets: Loadable<Vec<WidgetSummary>>,
    tab: WidgetsTab,
}

impl Default for WidgetsState {
    fn default() -> Self {
        Self { requested: false, widgets: Loadable::Loading, tab: WidgetsTab::Mine }
    }
}

impl Global for WidgetsState {}

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    if cx.default_global::<WidgetsState>().requested {
        return;
    }
    cx.default_global::<WidgetsState>().requested = true;
    let tx = state.session_tx.clone();
    cc::spawn_call(cx, tx, "widgets.custom.list", json!({}), |cx, result| {
        cx.default_global::<WidgetsState>().widgets = result.map(|v| parse_widgets(&v)).into();
    });
}

// ── Rendering ──────────────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    maybe_fetch(state, cx);
    let locale = state.locale;

    if state.ws_state != WsConnState::Authenticated {
        return div()
            .id("widgets-page")
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

    let widgets = cx.default_global::<WidgetsState>().widgets.clone();
    let tab = cx.default_global::<WidgetsState>().tab;
    let my_id = state.user_id.clone();

    let (mine_count, shared_count) = match &widgets {
        Loadable::Ready(rows) => {
            let mine = rows.iter().filter(|w| my_id.as_deref().is_some_and(|id| w.created_by_user == id)).count();
            let shared = rows.iter().filter(|w| w.shared && !my_id.as_deref().is_some_and(|id| w.created_by_user == id)).count();
            (mine, shared)
        }
        _ => (0, 0),
    };

    let tab_items = vec![
        TabItem::new("mine", i18n::t1(locale, "native.widgets.tab.mine", "count", &mine_count.to_string()), cx.listener(|_this, _ev, _window, cx| {
            cx.default_global::<WidgetsState>().tab = WidgetsTab::Mine;
            cx.notify();
        })),
        TabItem::new("shared", i18n::t1(locale, "native.widgets.tab.shared", "count", &shared_count.to_string()), cx.listener(|_this, _ev, _window, cx| {
            cx.default_global::<WidgetsState>().tab = WidgetsTab::Shared;
            cx.notify();
        })),
    ];

    let body: Div = match &widgets {
        Loadable::Loading => grid_4col((0..4).map(|_| card_skeleton()).collect()),
        Loadable::Failed(msg) => empty_state("⚠️", i18n::t1(locale, "native.presets.loadError", "message", msg), None, None::<Div>),
        Loadable::Ready(rows) => {
            let filtered: Vec<&WidgetSummary> = rows
                .iter()
                .filter(|w| {
                    let mine = my_id.as_deref().is_some_and(|id| w.created_by_user == id);
                    if tab == WidgetsTab::Mine { mine } else { w.shared && !mine }
                })
                .collect();
            if filtered.is_empty() {
                let key = if tab == WidgetsTab::Mine { "native.widgets.empty.mine" } else { "native.widgets.empty.shared" };
                empty_state("🧩", i18n::t(locale, key), None, None::<Div>)
            } else {
                grid_4col(filtered.iter().map(|w| widget_card(locale, w)).collect())
            }
        }
    };

    let actions = div()
        .flex()
        .gap_2()
        .child(button("widgets-new-html", i18n::t(locale, "native.widgets.newHtml"), ButtonVariant::Secondary, false, None, |_ev, _window, _app| {}))
        .child(button("widgets-import", i18n::t(locale, "native.widgets.import"), ButtonVariant::Secondary, false, None, |_ev, _window, _app| {}))
        .child(button(
            "widgets-new",
            i18n::t(locale, "native.widgets.new"),
            ButtonVariant::Primary,
            false,
            None,
            // WP-S6b2-O (2026-08-21): the one live navigation this page
            // wires — every other header button (HTML 自訂/匯入) stays
            // decorative, this one now actually reaches the new "新增
            // Widget" composer (`screens/widget_composer.rs`), same
            // "reaching page wires the row/button that gets you there"
            // precedent `screens/mcp.rs`'s own 存取金鑰管理 row establishes.
            cx.listener(|this, _ev, _window, cx| {
                this.active_page = "widgetComposer";
                cx.notify();
            }),
        ));

    div()
        .id("widgets-page")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_3()
        .p_6()
        .child(cc::breadcrumb(i18n::t(locale, "navArea.knowledge"), i18n::t(locale, "native.widgets.title")))
        .child(cc::page_header(i18n::t(locale, "native.widgets.title"), i18n::t(locale, "native.widgets.subtitle"), Some(actions)))
        .child(div().w_full().child(tabs(tab_items, if tab == WidgetsTab::Mine { "mine" } else { "shared" })))
        .child(body)
}

fn grid_4col(cards: Vec<Div>) -> Div {
    div().flex().flex_wrap().gap_3().children(cards.into_iter().map(|c| c.flex_1().min_w(px(190.))))
}

fn thumbnail_placeholder() -> Div {
    // Decorative-only chrome — see module doc comment deviation #1.
    div()
        .h(px(84.))
        .rounded(px(theme::RADIUS_MD))
        .bg(theme::alpha(theme::MUTED, 0.6))
        .border_1()
        .border_color(theme::surface_border())
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .w(px(120.))
                .flex()
                .flex_col()
                .gap_1()
                .child(div().h(px(6.)).w(px(64.)).rounded(px(3.)).bg(theme::alpha(theme::MUTED_FOREGROUND, 0.4)))
                .child(div().h(px(16.)).w(px(48.)).rounded(px(3.)).bg(theme::alpha(theme::MUTED_FOREGROUND, 0.6)))
                .child(div().h(px(5.)).w(px(90.)).rounded(px(3.)).bg(theme::alpha(theme::MUTED_FOREGROUND, 0.3))),
        )
}

fn card_skeleton() -> Div {
    cc::catalog_card().child(skeleton(px(190.), px(84.))).child(skeleton(px(120.), px(12.)))
}

fn widget_card(locale: Locale, w: &WidgetSummary) -> Div {
    let origin_badge = if w.origin == "ai" {
        badge(i18n::t(locale, "native.widgets.badge.ai"), BadgeVariant::Info)
    } else {
        badge(i18n::t(locale, "native.widgets.badge.html"), BadgeVariant::Secondary)
    };

    let header = div()
        .flex()
        .items_start()
        .justify_between()
        .gap_1p5()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(div().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::MEDIUM).text_color(theme::alpha(theme::FOREGROUND, 1.0)).truncate().child(w.title.clone()))
                .child(
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child(if w.description.is_empty() { i18n::t(locale, "native.widgets.noDescription").to_string() } else { w.description.clone() }),
                ),
        )
        .child(origin_badge);

    cc::catalog_card().child(thumbnail_placeholder()).child(header)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_widgets_reads_real_custom_widget_summary_shape() {
        let v = json!({ "widgets": [
            { "id": "w1", "title": "本週待審批數", "description": "顯示目前等待你決定的項目數",
              "origin": "ai", "created_by_user": "u1", "shared": false, "html_bytes": 512,
              "created_at": "2026-08-01T00:00:00Z", "updated_at": "2026-08-01T00:00:00Z" },
        ], "max_per_user": 20 });
        let rows = parse_widgets(&v);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].origin, "ai");
        assert_eq!(rows[0].created_by_user, "u1");
        assert!(!rows[0].shared);
    }

    #[test]
    fn parse_widgets_missing_array_is_empty_not_a_panic() {
        assert!(parse_widgets(&json!({})).is_empty());
    }

    #[test]
    fn parse_widgets_skips_entries_missing_id() {
        let v = json!({ "widgets": [{ "title": "x" }] });
        assert!(parse_widgets(&v).is_empty());
    }

    #[test]
    fn mine_vs_shared_split_matches_widgets_page_tsx_semantics() {
        let rows = vec![
            WidgetSummary { id: "a".into(), title: "".into(), description: "".into(), origin: "html".into(), created_by_user: "me".into(), shared: true },
            WidgetSummary { id: "b".into(), title: "".into(), description: "".into(), origin: "html".into(), created_by_user: "them".into(), shared: true },
            WidgetSummary { id: "c".into(), title: "".into(), description: "".into(), origin: "html".into(), created_by_user: "them".into(), shared: false },
        ];
        let my_id = Some("me".to_string());
        let mine: Vec<&WidgetSummary> = rows.iter().filter(|w| my_id.as_deref().is_some_and(|id| w.created_by_user == id)).collect();
        let shared: Vec<&WidgetSummary> = rows.iter().filter(|w| w.shared && !my_id.as_deref().is_some_and(|id| w.created_by_user == id)).collect();
        // "a" is mine (regardless of its own `shared` flag) — `shared` only
        // matters for OTHER users' rows, matching `WidgetsPage.tsx`'s
        // `w.created_by_user === user?.id` / `w.shared && w.created_by_user
        // !== user?.id` exactly.
        assert_eq!(mine.iter().map(|w| w.id.as_str()).collect::<Vec<_>>(), vec!["a"]);
        assert_eq!(shared.iter().map(|w| w.id.as_str()).collect::<Vec<_>>(), vec!["b"]);
        // "c" (not shared, not mine) appears in neither tab — server-side
        // `list_visible` would not even return such a row in practice, but
        // the client-side filter degrades safely either way.
    }
}
