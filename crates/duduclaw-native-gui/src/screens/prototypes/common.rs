// Shared helpers for the S4a design-gallery prototype pages
// (`p01_login.rs` … `p13_about.rs`, see `mod.rs`'s module doc comment for
// the overall gallery design). These pages are intentionally hand-drawn
// `div()` compositions with fabricated sample data, not wired to any real
// `RootView` state or MCP/dashboard RPC — the whole point of this gallery
// is a page-by-page visual sign-off BEFORE any of these layouts get built
// for real, so nothing here should look "more finished" (real data, real
// click handlers) than what it actually is.
//
// Text inside every prototype page is hardcoded Traditional Chinese on
// purpose — this is a design-review artifact, not a shipped product
// surface, so it deliberately does NOT go through `i18n::t(...)` (the
// gallery's own nav label, "設計稿", IS localized — see `nav.rs` — being the
// one exception: that label is real product chrome, these 13 pages are not).

use gpui::{div, prelude::*, px, App, ClickEvent, Div, IntoElement, SharedString, Window};

use crate::theme;

/// Default fixed "artboard" height most prototype pages render inside — the
/// design-gallery's own preview pane (`mod.rs::render`) is a scrollable
/// region with no fixed height of its own to hand down, but most of these 13
/// layouts (3-column master-detail, sidebar+list+inspector) need a concrete
/// height before "fills the remaining space" means anything. Matches how a
/// design tool presents fixed-size artboards rather than relying on ambient
/// document flow. `p13_about.rs` passes a smaller height directly to
/// `stage()` (an about panel is deliberately small, per its own brief).
pub const STAGE_HEIGHT: f32 = 640.0;

/// Semantic status-dot colors — the "行首狀態圈" vocabulary `p08_goals.rs` /
/// `p09_tasks.rs` / `p10_task_detail.rs` all share (task brief: "紅橘=
/// needs_human、藍=running、灰=done"). Centralized here so the three pages
/// can't silently drift to different color choices for the same state.
pub const STATUS_NEEDS_HUMAN: u32 = theme::WARNING;
pub const STATUS_RUNNING: u32 = theme::INFO;
pub const STATUS_DONE: u32 = theme::MUTED_FOREGROUND;
pub const STATUS_BLOCKED: u32 = theme::DESTRUCTIVE;

/// A no-op click handler — every clickable-looking element in these static
/// mockups draws hover/press states but doesn't mutate `RootView` (task
/// brief: "按鈕畫出來但不接事件，或接空事件"). Matches `screens/gallery.rs`'s
/// own `noop` (that page can't reuse this one directly — private to its own
/// module — so this is a second, deliberately identical, copy for this
/// sibling "static swatch page" module).
pub fn noop(_ev: &ClickEvent, _window: &mut Window, _cx: &mut App) {}

/// The fixed-height "stage" every prototype's outer container sits in —
/// full available width (the design-gallery preview pane, not the prototype
/// itself, owns horizontal sizing), a distinct background standing in for
/// "the whole app window", rounded + hairline border so it reads as one
/// contained artboard against the gallery chrome around it.
pub fn stage(bg: u32, height: f32) -> Div {
    div()
        .w_full()
        .h(px(height))
        .rounded(px(theme::RADIUS_XL))
        .overflow_hidden()
        .border_1()
        .border_color(theme::surface_border())
        .bg(theme::alpha(bg, 1.0))
}

/// A small colored status circle — the "行首狀態圈" itself. `diameter`
/// lets callers match row height (10px inline in a list row, larger in a
/// detail header).
pub fn status_dot(color: u32, diameter: f32) -> Div {
    div().size(px(diameter)).flex_shrink_0().rounded_full().bg(theme::alpha(color, 1.0))
}

/// Small circular avatar with a single initial — this crate has no icon/
/// photo asset pipeline yet (`nav.rs`'s badge-letter placeholder strategy
/// applies here too), used by the AI-employee pages (p11/p12) and the
/// chat/inbox list rows (p04/p05/p07).
pub fn avatar(letter: char, color: u32, diameter: f32) -> Div {
    div()
        .size(px(diameter))
        .flex_shrink_0()
        .rounded_full()
        .bg(theme::alpha(color, 1.0))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px((diameter * 0.42).max(9.0)))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme::alpha(theme::APP_SHELL, 1.0))
        .child(letter.to_string())
}

/// Section/group meta label — MDS "meta/label" bucket, same scale
/// `screens/gallery.rs`'s own `section()` helper and `screens/login.rs`'s
/// `field_label()` both already use.
pub fn meta_label(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(theme::TEXT_XS))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(text.into())
}

/// One "boxed-list" row — label left, value/control right (research doc's
/// GNOME "boxed lists" pattern: `label 左值右`) — used by `p12_agent_detail.
/// rs`'s 模型/能力/通道 groups and `p10_task_detail.rs`'s attribute
/// inspector.
pub fn kv_row(label: impl Into<SharedString>, value: impl IntoElement) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .h(px(38.))
        .px_3p5()
        .text_size(px(theme::TEXT_SM))
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(label.into()),
        )
        .child(value)
}

/// Wraps a `Vec<Div>` of `kv_row(...)`s (or any row shape) into one rounded,
/// hairline-divided group — the GNOME "boxed list" container itself.
pub fn boxed_group(rows: Vec<Div>) -> Div {
    let n = rows.len();
    let mut container = div()
        .flex()
        .flex_col()
        .rounded(px(theme::RADIUS_XL))
        .overflow_hidden()
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border());
    for (i, row) in rows.into_iter().enumerate() {
        container = container.child(if i + 1 < n {
            row.border_b_1().border_color(theme::border())
        } else {
            row
        });
    }
    container
}
