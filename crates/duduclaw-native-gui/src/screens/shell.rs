// Screen 3 — app shell. P0-1 of the "殼回訪修正" pass (2026-08-19, see
// `research/native-os-2026-08/desktop-app-conventions.md` §B) rebuilt this
// from a single flat/grouped sidebar into a native three-column
// master-detail skeleton — Apple HIG's own vocabulary (research doc §1.4/
// §2): **Column 1** (`shell_sidebar.rs`) is the area rail — the 6
// top-level areas from `nav.rs`'s `AREAS`, plus the pinned footer
// (`nav.rs::FOOTER_ITEMS` — 設定/元件庫, never inside an area, Windows
// `NavigationView.IsSettingsVisible` convention). **Column 2**
// (`shell_content_list.rs`) is the selected area's own page list — hidden
// entirely when that area holds only one page (nothing to disambiguate,
// HIG: "area 只有單頁時欄 2 可隱藏"). **Column 3** (`content`, built right
// here) is unchanged from before the P0-1 rebuild: the actual page
// content, still stub placeholders for every page except
// `newChat`/`componentLibrary`.
//
// The three columns are split across four files (this one plus
// `shell_sidebar.rs` / `shell_content_list.rs` / their shared
// `shell_row.rs` primitives) purely to keep each under this crate's own
// <300-line convention — before the split, one `shell.rs` covering all
// three columns ran to ~425 lines. No behavior differs from a
// hypothetical unsplit version; this file is pure composition.
//
// Column 1 is collapsible (`RootView::sidebar_collapsed`, toggled by the
// macOS menu bar's View ▸ Show/Hide Sidebar action — see `main.rs` /
// `native_menu.rs`). The FULL-HIDE choice (Column 1 vanishes entirely, vs.
// shrinking to an icon-only rail) is deliberate: an icon-only variant needs
// its own second layout (every row's label conditionally omitted, the
// CTA/search widgets need their own collapsed forms, the footer badge needs
// a narrower form) — roughly doubling `shell_sidebar.rs`'s row-rendering
// code for a Phase-1a first pass. Full-hide is one boolean gate over the
// existing column, and matches how several first-party macOS apps with a
// similarly slim sidebar (Notes, Reminders) treat "Hide Sidebar" — the
// whole list disappears, not just its labels. Revisit if a later pass
// actually needs the icon-only middle state.
//
// Still MDS §5.1-styled throughout — see `theme.rs`'s module doc comment
// for the surface-layering philosophy (no real backdrop-blur in gpui).

use gpui::{div, prelude::*, px, Context, Div, Stateful};

use crate::i18n;
use crate::nav;
use crate::screens::{shell_content_list, shell_sidebar};
use crate::theme;
use crate::RootView;

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Div {
    let locale = state.locale;
    let active_id = state.active_page;

    // ── Content-area placeholder heading ─────────────────────────────────
    let active_item = nav::find(active_id);
    let page_label = active_item.map(|i| i18n::t(locale, i.label_key)).unwrap_or_else(|| active_id.to_string().into());
    let page_desc = active_item.map(|i| i18n::t(locale, i.desc_key));
    let heading = i18n::t1(locale, "native.shell.pageStub", "page", &page_label);

    // MDS spec §5.1: `SidebarInset` — `rounded-xl bg-page-canvas shadow-
    // [var(--surface-shadow)] ring-1 ring-surface-border`.
    let content_shell = div()
        .id("content-area")
        .flex_1()
        .h_full()
        .flex()
        .flex_col()
        .gap_2()
        .p_6()
        .rounded(px(theme::RADIUS_XL))
        .overflow_hidden()
        .bg(theme::alpha(theme::PAGE_CANVAS, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .shadow(theme::surface_shadow());

    // S3: the `componentLibrary` nav item is the first real (non-stub) page
    // — it renders `screens::gallery::render(...)` in full instead of the
    // generic placeholder heading every other still-unwired nav id falls
    // back to. `gallery::render` draws its own title/subtitle internally
    // (see that file), so this branch skips the placeholder heading below
    // entirely rather than showing both.
    let content = if active_id == "componentLibrary" {
        content_shell.child(crate::screens::gallery::render(state, cx))
    } else if active_id == "newChat" {
        // S4: the chat page draws its own header/messages/composer (see
        // that file), same "skip the generic placeholder heading entirely"
        // pattern the component-library page above already established.
        content_shell.child(crate::screens::chat::render(state, cx))
    } else if active_id == "designGallery" {
        // S4a: the 13-page static design gallery draws its own list +
        // caption bar + preview (see `screens/prototypes/mod.rs`), same
        // "skip the generic placeholder heading" pattern.
        content_shell.child(crate::screens::prototypes::render(state, cx))
    } else {
        content_shell
            .child(
                // "詳情 hero 標題" / "Settings 分頁標題" scale: text-xl font-semibold.
                div()
                    .text_size(px(theme::TEXT_XL))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                    .child(heading),
            )
            .children(page_desc.map(|desc| {
                div().text_size(px(theme::TEXT_SM)).text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0)).child(desc)
            }))
    };

    let content_list_col = shell_content_list::render(state, cx);
    let sidebar_col: Option<Stateful<Div>> =
        if state.sidebar_collapsed { None } else { Some(shell_sidebar::render(state, cx)) };

    div()
        .size_full()
        .flex()
        .gap_2()
        .p_2()
        .bg(theme::alpha(theme::APP_SHELL, 1.0))
        .children(sidebar_col)
        .children(content_list_col)
        .child(content)
}
