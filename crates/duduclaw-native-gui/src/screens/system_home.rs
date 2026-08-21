// WP-S6b3-Q (S6b 第三波, 2026-08-22) — "系統" (`SystemHome.dc.html`, 頁型2
// 分區索引卡). A pure navigation hub: six clickable cards, each jumping
// `RootView::active_page` at a real already-implemented sibling page. No RPC
// of its own — the canvas draws only icon + title + one-line blurb per card,
// no live status data (unlike `screens::integrations`'s four status rows,
// this hub's own closest structural precedent, which DOES fetch per-card
// live state — this page deliberately does not, because the canvas gives it
// nothing to fetch).
//
// ── Side-nav highlight: intentionally zero (QA #3 ruling, verbatim in the
// canvas's own leading HTML comment) ─────────────────────────────────────
// `SystemHome.dc.html`'s first line states the ruling directly: "系統首頁刻意
// 零高亮——本頁是六群組 hub 索引頁，無單一側欄項目與其對應，強制歸屬任一項會
// 誤導使用者「這是哪個分類」". This page has no `nav.rs` id at all (not added
// to `AREAS`/`FOOTER_ITEMS` — out of this task's "nav.rs 不歸你動" boundary
// too), so `nav::area_for_page("systemHome")` naturally returns `None` and
// both Column 1's area row and Column 2's content list render with no
// selection — the "zero highlight" requirement falls out of simply never
// registering an id, no special-case code needed.
//
// ── Card → target mapping ─────────────────────────────────────────────────
// The canvas draws SIX cards: 安全 / 授權 / 帳戶與登入 / 系統更新 / 系統設定 /
// 裝置. This task's own brief names only FOUR as wired targets ("裝置/安全/
// 帳戶/更新總覽入口卡...真跳轉到已實作頁：device/security/accounts/
// systemUpdates"). Reconciled against what actually exists in this crate's
// `screens/shell.rs` Column-3 match:
//   - 安全 → `active_page = "security"` (`screens::security`, real, S6b1-K).
//   - 授權 → `active_page = "license"` (`screens::license`, real, S6b1-J) —
//     a FIFTH wired card beyond the brief's literal four-item list. Included
//     because the target page genuinely already exists and is reachable
//     from `shell.rs` today; leaving a real, working page's own entry card
//     inert would be a worse reading of "真跳轉到已實作頁" than the brief's
//     four named examples, not a scope violation of it.
//   - 帳戶與登入 → `active_page = "accounts"` (`screens::accounts`, S5b1-A).
//   - 系統更新 → `active_page = "systemUpdates"` (`screens::system_updates`,
//     S5b1-A).
//   - 裝置 → `active_page = "device"` (`screens::device`, S5b1-A).
//   - 系統設定 → `active_page = "settings"` (`screens::settings`, WP-S6b3-P).
//
// ── WP-S6b3-fix (2026-08-22): 系統設定 card wired, no longer inert ────────
// This card shipped with `target: None` not because no real target existed,
// but because `screens::settings` (WP-S6b3-P, "系統設定"／B5／
// `active_page == "settings"`／no `nav.rs` entry — see that module's own doc
// comment) is a SIBLING wave-3 package this file's original author hadn't
// seen land yet. The i18n text this card already displayed
// (`systemHome.card.settings.title`/`.desc` = "系統設定"／"通用選項與跨系統
// 整合") is the exact same copy `settings.rs`'s own module doc comment uses
// for itself ("跨系統整合設定總表") — confirming this IS the intended
// target, not a guess. `shell.rs` already has an `active_id == "settings"`
// branch (wired from `manage_advanced.rs`'s own 系統設定 row by the same
// WP-S6b3-P pass), so this card only needed its `target` filled in — no new
// routing code. `HubCard.target` is now a plain `&'static str` (the
// `Option` this file used to carry solely for this one inert case no longer
// has any `None` member to represent).

use gpui::{div, prelude::*, px, Context, Div, SharedString, Stateful};

use crate::i18n::{self, Locale};
use crate::theme;
use crate::RootView;

struct HubCard {
    /// Single-letter placeholder glyph — this crate has no icon set yet,
    /// same convention `nav.rs`'s `badge_letter` already establishes.
    glyph: &'static str,
    title_key: &'static str,
    desc_key: &'static str,
    /// `active_page` this card navigates to when clicked — every one of the
    /// six cards now has a real target (see WP-S6b3-fix note above).
    target: &'static str,
}

const CARDS: [HubCard; 6] = [
    HubCard { glyph: "S", title_key: "systemHome.card.security.title", desc_key: "systemHome.card.security.desc", target: "security" },
    HubCard { glyph: "K", title_key: "systemHome.card.license.title", desc_key: "systemHome.card.license.desc", target: "license" },
    HubCard { glyph: "A", title_key: "systemHome.card.accounts.title", desc_key: "systemHome.card.accounts.desc", target: "accounts" },
    HubCard { glyph: "U", title_key: "systemHome.card.updates.title", desc_key: "systemHome.card.updates.desc", target: "systemUpdates" },
    HubCard { glyph: "G", title_key: "systemHome.card.settings.title", desc_key: "systemHome.card.settings.desc", target: "settings" },
    HubCard { glyph: "D", title_key: "systemHome.card.device.title", desc_key: "systemHome.card.device.desc", target: "device" },
];

fn hub_card(card: &HubCard, locale: Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    let icon = div()
        .size(px(38.))
        .flex_shrink_0()
        .rounded(px(theme::RADIUS_LG))
        .flex()
        .items_center()
        .justify_center()
        .bg(theme::alpha(theme::BRAND, 0.12))
        .text_color(theme::alpha(theme::BRAND, 1.0))
        .text_size(px(15.))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .child(card.glyph);

    let body = div()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(
            div()
                .text_size(px(theme::TEXT_SM))
                .font_weight(gpui::FontWeight::MEDIUM)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(i18n::t(locale, card.title_key)),
        )
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(i18n::t(locale, card.desc_key)),
        );

    let id: SharedString = format!("systemhome-card-{}", card.target).into();
    let target = card.target;

    div()
        .id(id)
        .flex_1()
        .min_w(px(220.))
        .flex()
        .flex_col()
        .gap_2p5()
        .rounded(px(theme::RADIUS_XL))
        .p_4()
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .cursor_pointer()
        .hover(|s| s.bg(theme::alpha(theme::SURFACE_HOVER, 1.0)))
        .on_click(cx.listener(move |this, _ev, _window, cx| {
            this.active_page = target;
            cx.notify();
        }))
        .child(icon)
        .child(body)
}

pub fn render(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    let locale = state.locale;

    let header = div()
        .flex()
        .flex_col()
        .gap_0p5()
        .child(
            div()
                .text_size(px(theme::TEXT_XL))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                .child(i18n::t(locale, "systemHome.title")),
        )
        .child(
            div()
                .text_size(px(theme::TEXT_SM))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(i18n::t(locale, "systemHome.subtitle")),
        );

    let mut grid = div().flex().flex_wrap().gap_3();
    for card in &CARDS {
        grid = grid.child(hub_card(card, locale, cx));
    }

    div()
        .id("systemhome-page")
        .size_full()
        .overflow_y_scroll()
        .child(
            div()
                .max_w(px(860.))
                .mx_auto()
                .flex()
                .flex_col()
                .gap_4()
                .p_2()
                .child(header)
                .child(grid),
        )
}
