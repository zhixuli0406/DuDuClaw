// WP-S5b1-C — shared boxed-list / breadcrumb / kv-row primitives for the
// "整合" (Integrations) drill-down settings pages: `mcp.rs` / `odoo.rs` /
// `google_integration.rs` / `identity.rs`. All four are "整合" (nav id
// `integrations`, owned by a parallel workstream — not this task's to add)
// leaf pages: no own sidebar row, breadcrumb only (`整合 › 本頁`).
//
// Deliberate deviation from `about.rs`/`agents_detail.rs`'s "duplicate the
// kv_row/boxed_group pair locally rather than widen a sibling module's
// visibility" precedent (see those two files' own header comments): that
// rule exists to avoid coupling two UNRELATED design-authority pages through
// a module (`screens::prototypes::common`) that belongs to a third, internal
// tool. The four pages here are NOT unrelated — they are one design batch,
// same authority tree (`commercial/design/duduclaw-s5-settings-pages/`),
// rendering the byte-identical boxed-list/breadcrumb/kv-row grammar (same
// `border-radius: 12px`, same `#ececef` divider, same breadcrumb chevron)
// across all four `.dc.html` canvases. Four independent copies of one
// grammar with zero divergence pressure between them is the case the
// "duplicate, don't share" precedent was never arguing against — a real
// shared module for a real shared batch.

use gpui::{div, prelude::*, px, Context, Div, IntoElement, SharedString, Stateful};

use crate::i18n::{self, Locale};
use crate::theme;
use crate::RootView;

/// "整合 › {page_label}" breadcrumb. Clicking "整合" sets `active_page` to
/// `"integrations"` — the id the parallel Integrations-hub workstream is
/// wiring into `nav.rs`/`shell.rs` (out of this task's scope; confirmed
/// present as `nav.integrations` in this crate's own i18n catalogs as of
/// this pass). This is harmless even if that hub's own `shell.rs` match arm
/// hasn't landed yet: `shell.rs`'s fallback branch renders ANY unmatched
/// `active_page` id as a generic placeholder heading, never panics — the
/// same "unwired id degrades to a placeholder, not a crash" property
/// `spike_t7`'s debug-only routing already relies on.
///
/// `id_prefix` must be unique per call site (e.g. `"mcp-breadcrumb"`) so the
/// two child element ids this function mints (`{id_prefix}` / `{id_prefix}-
/// root`) never collide with a sibling page's own breadcrumb.
pub fn breadcrumb(
    id_prefix: &'static str,
    locale: Locale,
    page_label: SharedString,
    cx: &mut Context<RootView>,
) -> Stateful<Div> {
    div()
        .id(id_prefix)
        .flex()
        .items_center()
        .gap_1p5()
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(
            div()
                .id(format!("{id_prefix}-root"))
                .cursor_pointer()
                .hover(|s| s.text_color(theme::alpha(theme::FOREGROUND, 1.0)))
                .child(i18n::t(locale, "native.settings.breadcrumbIntegrations"))
                .on_click(cx.listener(|this, _ev, _window, cx| {
                    this.active_page = "integrations";
                    cx.notify();
                })),
        )
        .child(SharedString::from("›"))
        .child(div().overflow_hidden().child(page_label))
}

/// A boxed-list section header — "已連接的伺服器" / "連線設定" style, small
/// bold muted caption above a `boxed_group`.
pub fn section_label(text: impl Into<SharedString>) -> Div {
    div()
        .px_0p5()
        .pb_1p5()
        .text_size(px(theme::TEXT_XS))
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child(text.into())
}

/// One label/value row inside a `boxed_group` — GNOME "label 左值右" pattern,
/// same shape `about.rs::kv_row` / `agents_detail.rs::kv_row` already
/// establish. `value` is `impl IntoElement` (not just `SharedString`) so a
/// caller can put a badge, a masked-value + source-badge pair, or a plain
/// text child on the right without a second row primitive.
pub fn kv_row(label: impl Into<SharedString>, value: impl IntoElement, is_last: bool) -> Div {
    let row = div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .min_h(px(40.))
        .px_4()
        .py_2p5()
        .child(
            div()
                .flex_shrink_0()
                .text_size(px(theme::TEXT_SM))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(label.into()),
        )
        .child(div().flex().items_center().gap_2().child(value));
    if is_last {
        row
    } else {
        row.border_b_1().border_color(theme::border())
    }
}

/// The boxed-list card itself — rounded surface, 1px border, dividers
/// between rows handled by each row's own `is_last` flag (see `kv_row`).
pub fn boxed_group(rows: Vec<Div>) -> Div {
    div()
        .w_full()
        .flex()
        .flex_col()
        .rounded(px(theme::RADIUS_XL))
        .overflow_hidden()
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .children(rows)
}

/// Generic masked-value chip for a credential field this page never
/// receives cleartext for — literal `"••••••••"`, never a fabricated tail
/// (the design canvas's `sk-••••••••7f2a` implies the backend echoes a
/// partial cleartext suffix; the real RPCs behind every credential field on
/// these four pages return presence-only booleans, so inventing specific
/// tail characters would be exactly the kind of fabricated data this
/// project's conventions forbid — see each page's own header comment for
/// the RPC citation).
pub fn masked_value_chip() -> Div {
    div()
        .text_size(px(theme::TEXT_XS))
        .font_family("SF Mono")
        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
        .child("••••••••")
}
