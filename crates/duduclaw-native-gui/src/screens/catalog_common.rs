// WP-S5b2-E (2026-08-21) — shared primitives for the five "型錄卡牆"
// (catalog wall) pages this wave adds: `presets` / `skills` / `experts` /
// `inspiration_gallery` / `widgets`. All five render from ONE canvas batch
// (`commercial/design/duduclaw-s5-work-pages/{Presets,Skills,Experts,
// Gallery,Widgets}.dc.html`) sharing the identical page-header row,
// breadcrumb grammar, category-grouped-section header, and rounded-card
// shell — the same "one shared module for one design batch with zero
// divergence pressure" precedent `settings_common.rs` already establishes
// for the S5b1-C integrations batch (see that file's own header comment for
// why sharing is right here and NOT the general rule this crate follows —
// `about.rs`/`agents_detail.rs` still duplicate their own `kv_row`).
//
// `spawn_call`/`describe_call_error` are ALSO shared here rather than
// duplicated per file — this crate has BOTH precedents on record:
// `channels.rs`/`inbox.rs`/`console.rs`/`dashboard.rs` each keep a private
// copy of this ~15-line RPC round-trip shape (their own doc comments call
// that out as the DEFAULT), but `agents.rs` instead reuses `goals::
// spawn_goal_call` via `pub(super)` when >1 file in the SAME wave shares the
// identical shape — this wave's five pages are exactly that case.

use gpui::{div, prelude::*, px, Context, Div, Pixels, SharedString};
use serde_json::Value;
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::rpc::CallError;
use crate::theme;
use crate::ws_status::{self, Command as SessionCommand};
use crate::RootView;

// ── RPC round-trip boilerplate ────────────────────────────────────────────

pub(super) fn spawn_call(
    cx: &mut Context<RootView>,
    session_tx: tokio_mpsc::UnboundedSender<SessionCommand>,
    method: &'static str,
    params: Value,
    apply: impl FnOnce(&mut Context<RootView>, Result<Value, String>) + 'static,
) {
    cx.spawn(async move |weak, cx| {
        let rx = ws_status::call(&session_tx, method, params);
        let outcome = match rx.await {
            Ok(Ok(payload)) => Ok(payload),
            Ok(Err(err)) => Err(describe_call_error(&err)),
            Err(_) => Err("背景連線執行緒已結束".to_string()),
        };
        let _ = weak.update(cx, |_view, cx| {
            apply(cx, outcome);
            cx.notify();
        });
    })
    .detach();
}

pub(super) fn describe_call_error(e: &CallError) -> String {
    match e {
        CallError::NotConnected => "尚未連線到伺服器".to_string(),
        CallError::Timeout => "請求逾時".to_string(),
        CallError::Disconnected => "連線已中斷".to_string(),
        CallError::Rejected(v) => v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()),
    }
}

// ── Breadcrumb ─────────────────────────────────────────────────────────────

/// "{area_label} › {page_label}" — static text, deliberately NOT clickable.
/// A real deviation from `settings_common::breadcrumb`'s root segment (which
/// jumps to a real single-page hub, `active_page = "integrations"`): the
/// root these five canvases name ("AI 員工" / "知識與記憶") is a `nav.rs`
/// AREA label (`navArea.agents` / `navArea.knowledge`), not a navigable page
/// id — an area only expands a Column-2 list in `shell.rs`'s three-column
/// layout, it has no page of its own to jump to. Rendering it as inert text
/// is honest about that missing target, not a dropped feature.
pub(super) fn breadcrumb(area_label: impl Into<SharedString>, page_label: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .items_center()
        .gap_1p5()
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(area_label.into())
        .child(SharedString::from("›"))
        .child(div().overflow_hidden().child(page_label.into()))
}

// ── Page header ────────────────────────────────────────────────────────────

/// Title + subtitle (left) and an optional action-button row (right) — the
/// identical header shape opening every one of the five canvases (each
/// canvas's `<div style="display:flex;align-items:center;justify-content:
/// space-between">` block).
pub(super) fn page_header(
    title: impl Into<SharedString>,
    subtitle: impl Into<SharedString>,
    actions: Option<Div>,
) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .text_size(px(theme::TEXT_XL))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                        .child(title.into()),
                )
                .child(
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child(subtitle.into()),
                ),
        )
        .children(actions)
}

// ── Category grouping — `experts.rs` / `inspiration_gallery.rs` share the
// exact `industry_category` 6-bucket taxonomy `duduclaw_core::org::
// CATALOG_CATEGORIES` defines. This crate has no dependency on
// `duduclaw-core` (same constraint `channels.rs`'s module doc comment
// documents for `word_contains_ci`), so the 6 ids + their labels are
// duplicated here as plain data — sourced from the already-shipped
// `web/src/i18n/{zh-TW,en,ja-JP}.json`'s `experts.category.*` keys
// (`crates/duduclaw-core/src/org.rs:89-103` for the id list itself), not
// invented. ──────────────────────────────────────────────────────────────

pub(super) const CATEGORY_ORDER: [&str; 6] =
    ["health", "professional", "retail", "lifestyle", "education", "other"];

/// i18n key for one category id — `native.catalog.category.<id>`. Any id
/// outside `CATEGORY_ORDER` falls back to `other`'s label (defensive: the
/// server-side taxonomy could grow a 7th bucket this crate hasn't caught up
/// to yet — never panics, never shows a raw wire token).
pub(super) fn category_label(locale: Locale, category: &str) -> SharedString {
    let id = if CATEGORY_ORDER.contains(&category) { category } else { "other" };
    i18n::t(locale, &format!("native.catalog.category.{id}"))
}

/// Groups `items` into `CATEGORY_ORDER`'s fixed order via `category_of`; an
/// id outside that list collapses into the trailing "other" bucket rather
/// than being dropped. Empty buckets are omitted entirely — mirrors
/// `ExpertsPage.tsx`'s own `if (entries.length === 0) return null` per
/// category section.
pub(super) fn group_by_category<'a, T>(
    items: &'a [T],
    category_of: impl Fn(&'a T) -> &'a str,
) -> Vec<(&'static str, Vec<&'a T>)> {
    let mut buckets: Vec<(&'static str, Vec<&'a T>)> =
        CATEGORY_ORDER.iter().map(|c| (*c, Vec::new())).collect();
    for item in items {
        let cat = category_of(item);
        let idx = CATEGORY_ORDER.iter().position(|c| *c == cat).unwrap_or(CATEGORY_ORDER.len() - 1);
        buckets[idx].1.push(item);
    }
    buckets.into_iter().filter(|(_, v)| !v.is_empty()).collect()
}

pub(super) fn category_group_header(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(theme::TEXT_XS))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(text.into())
}

// ── Catalog card wrapper ────────────────────────────────────────────────────

/// The rounded/bordered surface card shell every catalog item (preset
/// summary / skill / expert pack / gallery example / widget thumbnail)
/// shares — canvas's `background:#fff;border:1px solid #ececef;border-
/// radius:12px` translated onto this crate's dark theme tokens (this crate
/// renders dark-only, see `theme.rs`'s module doc comment).
pub(super) fn catalog_card() -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .rounded(px(theme::RADIUS_XL))
        .p_3()
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
}

// ── Deterministic initial-avatar (Presets' AI-staff binding table) — this
// crate's `chat::agents_picker::avatar_color`/`avatar_glyph` do the same
// job but are private to the `chat` module (`chat.rs` declares `mod
// agents_picker;` with no `pub`, so it is not reachable from a `screens`
// sibling module — see that file's own module list), hence a fresh, small,
// self-contained reimplementation here rather than a visibility change to
// an unrelated page's internals. ─────────────────────────────────────────

const AVATAR_HUES: [u32; 6] = [0x2171cc, 0x0f766e, 0x8b5cf6, 0xb0578d, 0xc2620a, 0x4aa651];

/// A deterministic hue index from `seed` (agent id / member name) — the same
/// input always picks the same color across renders and re-renders, without
/// a hash-crate dependency (a plain byte-sum is enough entropy for a 6-way
/// pick — same "good enough" reasoning `chat::agents_picker::avatar_color`
/// documents for its own modulo-based pick).
fn avatar_hue(seed: &str) -> u32 {
    let sum: u32 = seed.bytes().map(u32::from).sum();
    AVATAR_HUES[(sum as usize) % AVATAR_HUES.len()]
}

/// First character of `label` (CJK-safe — `chars().next()`, never a raw
/// byte slice; this crate's coding convention #1) on a colored circle.
pub(super) fn initial_avatar(label: &str, size: Pixels) -> Div {
    let glyph: SharedString = label.chars().next().map(|c| c.to_string()).unwrap_or_default().into();
    let hue = avatar_hue(label);
    div()
        .size(size)
        .flex_shrink_0()
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(theme::alpha(hue, 1.0))
        .text_color(theme::alpha(theme::SURFACE_FOREGROUND, 1.0))
        .text_size(size * 0.4)
        .font_weight(gpui::FontWeight::BOLD)
        .child(glyph)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_label_falls_back_to_other_for_unknown_id() {
        assert_eq!(category_label(Locale::ZhTw, "carrier_pigeon"), category_label(Locale::ZhTw, "other"));
        assert_eq!(category_label(Locale::ZhTw, "health"), "醫療健康");
    }

    #[test]
    fn group_by_category_preserves_category_order_and_drops_empty_buckets() {
        let items = vec![("a", "retail"), ("b", "health"), ("c", "retail")];
        let groups = group_by_category(&items, |i| i.1);
        let ids: Vec<&str> = groups.iter().map(|(c, _)| *c).collect();
        // CATEGORY_ORDER = [health, professional, retail, lifestyle,
        // education, other] — health sorts before retail even though the
        // input listed a retail item first.
        assert_eq!(ids, vec!["health", "retail"]);
        assert_eq!(groups[1].1.len(), 2);
    }

    #[test]
    fn group_by_category_collapses_unknown_ids_into_other() {
        let items = vec![("a", "florist")];
        let groups = group_by_category(&items, |i| i.1);
        assert_eq!(groups, vec![("other", vec![&("a", "florist")])]);
    }

    #[test]
    fn avatar_hue_is_deterministic_and_in_range() {
        let h1 = avatar_hue("agent-alpha");
        let h2 = avatar_hue("agent-alpha");
        assert_eq!(h1, h2);
        assert!(AVATAR_HUES.contains(&h1));
    }

    #[test]
    fn initial_avatar_glyph_is_cjk_safe() {
        // Never panics on a multi-byte leading char (coding convention #1);
        // real assertion is just "doesn't panic building the element".
        let _ = initial_avatar("小杜", px(22.));
        let _ = initial_avatar("", px(22.));
    }
}
