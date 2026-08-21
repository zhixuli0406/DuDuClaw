// WP-S5b2-E (2026-08-21) — "靈感畫廊" (`nav.rs` id `gallery`, wired into
// `AGENTS_ITEMS` by the parallel D package — see `nav.rs`'s S5b2-D header
// note). Module named `inspiration_gallery`, NOT `gallery` — that name is
// already taken by `screens::gallery`, the S3 component-library dogfood
// page (`screens/gallery.rs`'s own header comment); this file is the
// UNRELATED "inspiration gallery" product page `nav.rs` id `gallery` /
// `active_page == "gallery"` routes to, per the `web/src/pages/GalleryPage.
// tsx` naming this WP's brief points at.
//
// Visual authority: `commercial/design/duduclaw-s5-work-pages/Gallery.dc.
// html` — a category-grouped card wall, one card per team task example,
// each with a dual-state action button. Mirrors `web/src/pages/
// GalleryPage.tsx` (P2-b MVP): curated showcase cards fanned out from the
// same `team.toml` example data `experts.catalog` reads — no new storage,
// nothing user-submitted yet.
//
// ── RPC shape (read directly from `crates/duduclaw-gateway/src/handlers.
// rs`, never guessed; admin-scoped, `require_admin!()`) ──────────────────
//   `gallery.list {}` (`handle_gallery_list` ~L19099, built from
//   `expert_generate::gallery_cards`, `expert_generate.rs:356-398`) →
//   `{"deployed","unlocked","present_but_locked","cards":[{"id","industry",
//   "category","departments","team_slug","team_label","example",
//   "team_installed","lead_agent_name"}]}`.
//
// ── Card copy (reused verbatim from the already-shipped `gallery.card.*`
// web i18n keys — `web/src/i18n/{zh-TW,en,ja-JP}.json`, NOT invented here)
// and the exact selection rule `GalleryPage.tsx`'s `GalleryCardView` uses
// (read from its source, not guessed): `departments.length > 0` picks
// "適合需要{depts}人手的你" (`fitForDept`), an EMPTY `departments` picks
// "適合正在經營「{team}」的你" (`fitForGeneric`) — this is NOT keyed off
// `team_installed` (a card's title/dual-state button is; the "fit for you"
// line is a separate, independent field). `cardTitle()` below ports
// `GalleryPage.tsx`'s own pure helper (first zh-TW-delimited clause, capped
// at 24 codepoints, CJK-safe) so the card headline matches the web version.
//
// ── "雙態按鈕組裝" (task brief, verbatim) — BOTH states inert ───────────
// `team_installed` picks "做一個同款" (primary) vs "先加入這組 AI 團隊"
// (secondary+arrow) — assembled, neither wired. `GalleryPage.tsx` actually
// wires "做一個同款" to a real `useAssignStore` dialog and the join-team
// button to `navigate('/experts')`; this WP's brief calls both states out
// by name as "組裝" (assembled), so both stay inert here even though the
// web original wires the second one to a plain in-app navigation — a
// deliberate, documented scope trim, not an oversight.

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
pub struct GalleryCard {
    pub category: String,
    pub departments: Vec<String>,
    pub team_label: String,
    pub example: String,
    pub team_installed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GalleryData {
    pub deployed: bool,
    pub present_but_locked: bool,
    pub cards: Vec<GalleryCard>,
}

pub fn parse_gallery(v: &Value) -> GalleryData {
    let cards = v
        .get("cards")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|c| {
                    let example = c.get("example").and_then(Value::as_str)?.to_string();
                    if example.is_empty() {
                        return None;
                    }
                    Some(GalleryCard {
                        category: c.get("category").and_then(Value::as_str).unwrap_or("other").to_string(),
                        departments: c
                            .get("departments")
                            .and_then(Value::as_array)
                            .map(|a| a.iter().filter_map(|d| d.as_str().map(str::to_string)).collect())
                            .unwrap_or_default(),
                        team_label: c.get("team_label").and_then(Value::as_str).unwrap_or("").to_string(),
                        example,
                        team_installed: c.get("team_installed").and_then(Value::as_bool).unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    GalleryData {
        deployed: v.get("deployed").and_then(Value::as_bool).unwrap_or(false),
        present_but_locked: v.get("present_but_locked").and_then(Value::as_bool).unwrap_or(false),
        cards,
    }
}

/// Ports `GalleryPage.tsx`'s `cardTitle()` — first clause split on the
/// common zh-TW list separators, capped at 24 codepoints (CJK-safe: counts
/// `char`s, never UTF-8 bytes, matching that function's own `Array.from`
/// codepoint split — this crate's coding convention #1).
pub fn card_title(example: &str) -> String {
    let seg = example.split(['、', '，', '；', ';', ',']).next().map(str::trim).filter(|s| !s.is_empty()).unwrap_or_else(|| example.trim());
    let chars: Vec<char> = seg.chars().collect();
    if chars.len() > 24 {
        format!("{}…", chars[..24].iter().collect::<String>())
    } else {
        seg.to_string()
    }
}

// ── State ──────────────────────────────────────────────────────────────

pub struct GalleryState {
    requested: bool,
    pub data: Loadable<GalleryData>,
}

impl Default for GalleryState {
    fn default() -> Self {
        Self { requested: false, data: Loadable::Loading }
    }
}

impl Global for GalleryState {}

fn maybe_fetch(state: &RootView, cx: &mut Context<RootView>) {
    if state.ws_state != WsConnState::Authenticated {
        return;
    }
    if cx.default_global::<GalleryState>().requested {
        return;
    }
    cx.default_global::<GalleryState>().requested = true;
    let tx = state.session_tx.clone();
    cc::spawn_call(cx, tx, "gallery.list", json!({}), |cx, result| {
        cx.default_global::<GalleryState>().data = result.map(|v| parse_gallery(&v)).into();
    });
}

// ── Rendering ──────────────────────────────────────────────────────────

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    maybe_fetch(state, cx);
    let locale = state.locale;

    if state.ws_state != WsConnState::Authenticated {
        return div()
            .id("inspiration-gallery-page")
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

    let data = cx.default_global::<GalleryState>().data.clone();

    let body: Div = match &data {
        Loadable::Loading => grid_2col((0..3).map(|_| card_skeleton()).collect()),
        Loadable::Failed(msg) => empty_state("⚠️", i18n::t1(locale, "native.presets.loadError", "message", msg), None, None::<Div>),
        Loadable::Ready(d) if d.present_but_locked => empty_state("🔒", i18n::t(locale, "native.inspirationGallery.locked"), None, None::<Div>),
        Loadable::Ready(d) if !d.deployed => empty_state("🖼️", i18n::t(locale, "native.inspirationGallery.empty.title"), Some(i18n::t(locale, "native.inspirationGallery.notDeployed")), None::<Div>),
        Loadable::Ready(d) if d.cards.is_empty() => empty_state("🖼️", i18n::t(locale, "native.inspirationGallery.empty.title"), Some(i18n::t(locale, "native.inspirationGallery.empty.desc")), None::<Div>),
        Loadable::Ready(d) => {
            let groups = cc::group_by_category(&d.cards, |c| c.category.as_str());
            let mut col = div().flex().flex_col().gap_4();
            for (category, cards) in groups {
                col = col.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(cc::category_group_header(cc::category_label(locale, category)))
                        .child(grid_2col(cards.iter().map(|c| gallery_card(locale, c)).collect())),
                );
            }
            col
        }
    };

    div()
        .id("inspiration-gallery-page")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_4()
        .p_6()
        .child(cc::breadcrumb(i18n::t(locale, "navArea.agents"), i18n::t(locale, "native.inspirationGallery.title")))
        .child(cc::page_header(i18n::t(locale, "native.inspirationGallery.title"), i18n::t(locale, "native.inspirationGallery.subtitle"), None))
        .child(body)
}

fn grid_2col(cards: Vec<Div>) -> Div {
    div().flex().flex_wrap().gap_3().children(cards.into_iter().map(|c| c.flex_1().min_w(px(320.))))
}

fn card_skeleton() -> Div {
    cc::catalog_card().child(skeleton(px(180.), px(14.))).child(skeleton(px(220.), px(12.))).child(skeleton(px(140.), px(10.)))
}

fn gallery_card(locale: Locale, c: &GalleryCard) -> Div {
    let title = div()
        .flex()
        .items_center()
        .gap_1p5()
        .child(div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::BRAND, 1.0)).child("✨"))
        .child(div().text_size(px(theme::TEXT_SM)).font_weight(gpui::FontWeight::MEDIUM).text_color(theme::alpha(theme::FOREGROUND, 1.0)).truncate().child(card_title(&c.example)));

    let team_badge = div().child(badge(c.team_label.clone(), BadgeVariant::Outline));

    let outcome = div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(i18n::t1(locale, "native.inspirationGallery.card.outcome", "example", &c.example));

    let fit_for = if !c.departments.is_empty() {
        i18n::t1(locale, "native.inspirationGallery.card.fitForDept", "depts", &c.departments.join("、"))
    } else {
        i18n::t1(locale, "native.inspirationGallery.card.fitForGeneric", "team", &c.team_label)
    };
    let fit_row = div()
        .flex()
        .items_start()
        .gap_1p5()
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child("👥")
        .child(fit_for);

    // Dual-state, both assembled/inert — see module doc comment.
    let action = if c.team_installed {
        button("gallery-remake", i18n::t(locale, "native.inspirationGallery.card.remake"), ButtonVariant::Primary, false, None, |_ev, _window, _app| {})
    } else {
        button("gallery-join-first", i18n::t(locale, "native.inspirationGallery.card.joinTeamFirst"), ButtonVariant::Secondary, false, None, |_ev, _window, _app| {})
    };

    cc::catalog_card().child(title).child(team_badge).child(outcome).child(fit_row).child(div().flex().justify_end().child(action))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_gallery_reads_real_gallery_cards_shape() {
        let v = json!({
            "deployed": true, "unlocked": true, "present_but_locked": false,
            "cards": [{
                "id": "cs-team-0", "industry": "cs", "category": "professional",
                "departments": [], "team_slug": "cs-team", "team_label": "客服團隊",
                "example": "整理本週客訴摘要", "team_installed": true, "lead_agent_name": "cs_fd",
            }],
        });
        let d = parse_gallery(&v);
        assert!(d.deployed);
        assert_eq!(d.cards.len(), 1);
        assert_eq!(d.cards[0].team_label, "客服團隊");
        assert!(d.cards[0].team_installed);
        assert!(d.cards[0].departments.is_empty());
    }

    #[test]
    fn parse_gallery_absent_premium_tree_is_not_deployed() {
        let d = parse_gallery(&json!({ "deployed": false, "cards": [] }));
        assert!(!d.deployed);
        assert!(d.cards.is_empty());
    }

    #[test]
    fn parse_gallery_skips_cards_missing_example() {
        let v = json!({ "deployed": true, "cards": [{ "team_label": "x" }] });
        assert!(parse_gallery(&v).cards.is_empty());
    }

    #[test]
    fn card_title_takes_first_delimited_clause() {
        assert_eq!(card_title("整理本週客訴摘要、產出報表"), "整理本週客訴摘要");
        assert_eq!(card_title("no delimiter here"), "no delimiter here");
    }

    #[test]
    fn card_title_caps_at_24_codepoints_cjk_safe() {
        let long = "一".repeat(30);
        let title = card_title(&long);
        // 24 chars + the ellipsis mark, never a byte-boundary panic on
        // multi-byte CJK characters (coding convention #1).
        assert_eq!(title.chars().count(), 25);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn card_title_trims_and_never_panics_on_empty_or_pathological_input() {
        assert_eq!(card_title(""), "");
        assert_eq!(card_title("   "), "");
        // An all-delimiter string's first clause is empty, so this falls
        // back to the (delimiter-preserving) whole-string trim — matching
        // `GalleryPage.tsx`'s own `seg || example.trim()` fallback exactly
        // (JS's `"" || x` behaves the same way this `filter` does).
        assert_eq!(card_title("、、、"), "、、、");
    }
}
