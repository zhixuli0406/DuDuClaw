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
    let content = if active_id == "home" {
        // S4b: the "總覽" dashboard — first real S4b page, see
        // `screens/dashboard.rs`'s module doc comment. Same "skip the
        // generic placeholder heading entirely" pattern as the two branches
        // below (this page draws its own greeting/prompt-bar/cards).
        content_shell.child(crate::screens::dashboard::render(state, cx))
    } else if active_id == "inbox" {
        // S4b (second wave): 收件匣 — see `screens/inbox.rs`'s module doc
        // comment. Same "skip the generic placeholder heading entirely"
        // pattern as every other real (non-stub) page in this match.
        content_shell.child(crate::screens::inbox::render(state, cx))
    } else if active_id == "console" {
        // S4b (second wave): 主控台 — see `screens/console.rs`'s module doc
        // comment. Same "skip the generic placeholder heading entirely"
        // pattern as every other real (non-stub) page in this match.
        content_shell.child(crate::screens::console::render(state, cx))
    } else if active_id == "goals" {
        // S4b (second wave): 目標 — see `screens/goals.rs`'s module doc
        // comment. Same "skip the generic placeholder heading entirely"
        // pattern as every other real (non-stub) page in this match.
        content_shell.child(crate::screens::goals::render(state, cx))
    } else if active_id == "tasks" {
        // S4b (third wave): 任務 — see `screens/tasks.rs`'s module doc
        // comment. Same "skip the generic placeholder heading entirely"
        // pattern as every other real (non-stub) page in this match.
        content_shell.child(crate::screens::tasks::render(state, cx))
    } else if active_id == "about" {
        // S4b (third wave): 關於 — see `screens/about.rs`'s module doc
        // comment. Same "skip the generic placeholder heading entirely"
        // pattern as every other real (non-stub) page in this match.
        content_shell.child(crate::screens::about::render(state, cx))
    } else if active_id == "agents" {
        // S4b (third wave): AI 員工 — see `screens/agents.rs`'s module doc
        // comment. Same "skip the generic placeholder heading entirely"
        // pattern as every other real (non-stub) page in this match.
        content_shell.child(crate::screens::agents::render(state, cx))
    } else if active_id == "device" {
        // S5b1-A: 裝置 — see `screens/device.rs`'s module doc comment. Same
        // "skip the generic placeholder heading entirely" pattern as every
        // other real (non-stub) page in this match.
        content_shell.child(crate::screens::device::render(state, cx))
    } else if active_id == "manageAdvanced" {
        // S5b1-A: 進階設定殼 — see `screens/manage_advanced.rs`'s module doc
        // comment. Same "skip the generic placeholder heading entirely"
        // pattern as every other real (non-stub) page in this match.
        content_shell.child(crate::screens::manage_advanced::render(state, cx))
    // ── WP-S6b1-K (S6b 第一波, 2026-08-21) — 治理/安全 drill-down leaves of
    // `manageAdvanced` (see `screens/governance.rs`'s module doc comment for
    // the GovernanceShell tabs `governance`/`wikiTrust` share). Self-
    // attached here per the "D 先掛好分支就直接可達，未掛就自己掛" precedent
    // every prior wave's own comments already establish — the actual
    // clickable row wiring inside `manage_advanced.rs` is a parallel WP's
    // territory (that file is off this pass's edit list). ─────────────────
    } else if active_id == "governance" {
        // WP-S6b1-K: 治理規則 — see `screens/governance.rs`'s module doc
        // comment. Same "skip the generic placeholder heading entirely"
        // pattern as every other real (non-stub) page in this match.
        content_shell.child(crate::screens::governance::render(state, cx))
    } else if active_id == "wikiTrust" {
        // WP-S6b1-K: Wiki 信任層級 — see `screens/wiki_trust.rs`'s module
        // doc comment. Same "skip the generic placeholder heading entirely"
        // pattern as every other real (non-stub) page in this match.
        content_shell.child(crate::screens::wiki_trust::render(state, cx))
    } else if active_id == "security" {
        // WP-S6b1-K: 安全 — see `screens/security.rs`'s module doc comment.
        // Same "skip the generic placeholder heading entirely" pattern as
        // every other real (non-stub) page in this match.
        content_shell.child(crate::screens::security::render(state, cx))
    // ── WP-S6b1-J (S6b 第一波, 2026-08-21) — 帳務/授權 drill-down leaves of
    // `manageAdvanced` (see `screens/manage_advanced_common.rs`'s module doc
    // comment for the LicenseShell tabs `license`/`partnerPortal` share).
    // Self-attached here per the same "D 先掛好分支就直接可達，未掛就自己掛"
    // precedent WP-S6b1-K's block right above already re-establishes — the
    // actual clickable row wiring inside `manage_advanced.rs` is a parallel
    // WP's territory (that file is off this pass's edit list too). ────────
    } else if active_id == "billing" {
        // WP-S6b1-J: 帳務 — see `screens/billing.rs`'s module doc comment.
        // Same "skip the generic placeholder heading entirely" pattern as
        // every other real (non-stub) page in this match.
        content_shell.child(crate::screens::billing::render(state, cx))
    } else if active_id == "license" {
        // WP-S6b1-J: 授權 — see `screens/license.rs`'s module doc comment.
        // Same "skip the generic placeholder heading entirely" pattern as
        // every other real (non-stub) page in this match.
        content_shell.child(crate::screens::license::render(state, cx))
    } else if active_id == "partnerPortal" {
        // WP-S6b1-J: 經銷夥伴入口 — see `screens/partner_portal.rs`'s module
        // doc comment. Same "skip the generic placeholder heading entirely"
        // pattern as every other real (non-stub) page in this match.
        content_shell.child(crate::screens::partner_portal::render(state, cx))
    } else if active_id == "systemUpdates" {
        // S5b1-A: 系統更新 — see `screens/system_updates.rs`'s module doc
        // comment. Same "skip the generic placeholder heading entirely"
        // pattern as every other real (non-stub) page in this match.
        content_shell.child(crate::screens::system_updates::render(state, cx))
    } else if active_id == "accounts" {
        // S5b1-A: 帳戶與登入 — see `screens/accounts.rs`'s module doc
        // comment. Same "skip the generic placeholder heading entirely"
        // pattern as every other real (non-stub) page in this match.
        content_shell.child(crate::screens::accounts::render(state, cx))
    } else if active_id == "channels" {
        // S5b (first wave): 通道 — see `screens/channels.rs`'s module doc
        // comment. Same "skip the generic placeholder heading entirely"
        // pattern as every other real (non-stub) page in this match.
        content_shell.child(crate::screens::channels::render(state, cx))
    } else if active_id == "integrations" {
        // S5b (first wave): 整合總覽 — see `screens/integrations.rs`'s
        // module doc comment. Same "skip the generic placeholder heading
        // entirely" pattern as every other real (non-stub) page in this
        // match. Its four drill-down targets (`mcp`/`odoo`/
        // `googleIntegration`/`identity`) are wired in the four branches
        // right below (WP-S5b1-C's own pages).
        content_shell.child(crate::screens::integrations::render(state, cx))
    } else if active_id == "mcp" {
        // WP-S5b1-C: 工具伺服器（MCP）— "整合" drill-down leaf, see
        // `screens/mcp.rs`'s module doc comment. Same "skip the generic
        // placeholder heading entirely" pattern as every other real
        // (non-stub) page in this match.
        content_shell.child(crate::screens::mcp::render(state, cx))
    } else if active_id == "odoo" {
        // WP-S5b1-C: Odoo ERP — "整合" drill-down leaf, see
        // `screens/odoo.rs`'s module doc comment. Same "skip the generic
        // placeholder heading entirely" pattern as every other real
        // (non-stub) page in this match.
        content_shell.child(crate::screens::odoo::render(state, cx))
    } else if active_id == "googleIntegration" {
        // WP-S5b1-C: Google 工作區 — "整合" drill-down leaf, see
        // `screens/google_integration.rs`'s module doc comment. Same "skip
        // the generic placeholder heading entirely" pattern as every other
        // real (non-stub) page in this match.
        content_shell.child(crate::screens::google_integration::render(state, cx))
    } else if active_id == "identity" {
        // WP-S5b1-C: 身分解析 — "整合" drill-down leaf, see
        // `screens/identity.rs`'s module doc comment. Same "skip the
        // generic placeholder heading entirely" pattern as every other real
        // (non-stub) page in this match.
        content_shell.child(crate::screens::identity::render(state, cx))
    } else if active_id == "routines" {
        // WP-S5b2-D: 例行工作 — see `screens/routines.rs`'s module doc
        // comment. Same "skip the generic placeholder heading entirely"
        // pattern as every other real (non-stub) page in this match.
        content_shell.child(crate::screens::routines::render(state, cx))
    } else if active_id == "plans" {
        // WP-S5b2-D: 共同計畫 — see `screens/plans.rs`'s module doc comment.
        // Same "skip the generic placeholder heading entirely" pattern as
        // every other real (non-stub) page in this match.
        content_shell.child(crate::screens::plans::render(state, cx))
    // ── WP-S5b2-E ("型錄/商店卡片牆", B2) — five pages added by this pass,
    // `nav.rs`/`shell.rs` wiring done here per the "D 先掛好分支就直接可達，
    // 未掛就自己掛" precedent `channels.rs`'s own first-S5b-wave doc comment
    // set (D's nav.rs S5b2-D header comment confirms these five ids are
    // this pass's own scope, not D's). ─────────────────────────────────
    } else if active_id == "presets" {
        // WP-S5b2-E: 職務組合 — see `screens/presets.rs`'s module doc
        // comment. Same "skip the generic placeholder heading entirely"
        // pattern as every other real (non-stub) page in this match.
        content_shell.child(crate::screens::presets::render(state, cx))
    } else if active_id == "skills" {
        // WP-S5b2-E: 技能庫 — see `screens/skills.rs`'s module doc comment.
        // Same "skip the generic placeholder heading entirely" pattern as
        // every other real (non-stub) page in this match.
        content_shell.child(crate::screens::skills::render(state, cx))
    } else if active_id == "experts" {
        // WP-S5b2-E: AI 團隊 — see `screens/experts.rs`'s module doc
        // comment. Same "skip the generic placeholder heading entirely"
        // pattern as every other real (non-stub) page in this match.
        content_shell.child(crate::screens::experts::render(state, cx))
    } else if active_id == "gallery" {
        // WP-S5b2-E: 靈感畫廊 — routes to `screens::inspiration_gallery`,
        // NOT `screens::gallery` (that module is the unrelated S3
        // component-library dogfood page reached via `componentLibrary`
        // below) — see `inspiration_gallery.rs`'s own doc comment for the
        // full disambiguation. Same "skip the generic placeholder heading
        // entirely" pattern as every other real (non-stub) page here.
        content_shell.child(crate::screens::inspiration_gallery::render(state, cx))
    } else if active_id == "widgets" {
        // WP-S5b2-E: Widget 工坊 — see `screens/widgets.rs`'s module doc
        // comment. Same "skip the generic placeholder heading entirely"
        // pattern as every other real (non-stub) page in this match.
        content_shell.child(crate::screens::widgets::render(state, cx))
    // ── WP-S5b2-F (2026-08-21), same wave — `files`/`runs`/`mcpKeys` wired
    // by this pass itself, per the "D 先掛好分支就直接可達，未掛就自己掛"
    // precedent WP-S5b2-E's own comment above already established (D had
    // not reached these three by the time this pass landed). ─────────────
    } else if active_id == "files" {
        // WP-S5b2-F: 檔案 (B3 檔案總管) — see `screens/files.rs`'s module
        // doc comment. Same "skip the generic placeholder heading entirely"
        // pattern as every other real (non-stub) page in this match.
        content_shell.child(crate::screens::files::render(state, cx))
    } else if active_id == "runs" {
        // WP-S5b2-F: 執行紀錄 (B4) — see `screens/runs.rs`'s module doc
        // comment. Same "skip the generic placeholder heading entirely"
        // pattern as every other real (non-stub) page in this match.
        content_shell.child(crate::screens::runs::render(state, cx))
    } else if active_id == "mcpKeys" {
        // WP-S5b2-F: 存取金鑰 (McpKeysPage) — no `nav.rs` entry of its own,
        // reached via `screens::mcp`'s "存取金鑰管理 →" row. See
        // `screens/mcp_keys.rs`'s module doc comment. Same "skip the
        // generic placeholder heading entirely" pattern as every other real
        // (non-stub) page in this match.
        content_shell.child(crate::screens::mcp_keys::render(state, cx))
    } else if active_id == "componentLibrary" {
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
    // ── WP-S5b3-H ("S5b 第三波" — three visualization pages, `Timeline.dc.
    // html`/`OrgIndented.dc.html`/`Forks.dc.html`) — no `nav.rs` entry yet
    // (this task's own "nav.rs 不歸你動" boundary; a later "G 包" pass wires
    // the sidebar and will replace this comment block), so each branch is
    // only reachable via `DUDUCLAW_NATIVE_GUI_DEBUG_PAGE=<id>` for now —
    // same "D 先掛好分支就直接可達，未掛就自己掛" precedent WP-S5b2-E's own
    // comment above already establishes for a page whose nav id lands in a
    // sibling pass. ───────────────────────────────────────────────────────
    } else if active_id == "timeline" {
        // WP-S5b3-H: 工作時間軸 — see `screens/timeline.rs`'s module doc
        // comment. Same "skip the generic placeholder heading entirely"
        // pattern as every other real (non-stub) page in this match.
        content_shell.child(crate::screens::timeline::render(state, cx))
    } else if active_id == "org" {
        // WP-S5b3-H: 組織架構（方案 A 縮排階層清單）— see `screens/org.rs`'s
        // module doc comment. Same "skip the generic placeholder heading
        // entirely" pattern as every other real (non-stub) page in this
        // match.
        content_shell.child(crate::screens::org::render(state, cx))
    } else if active_id == "forks" {
        // WP-S5b3-H: 分支決戰 — see `screens/forks.rs`'s module doc comment.
        // Same "skip the generic placeholder heading entirely" pattern as
        // every other real (non-stub) page in this match.
        content_shell.child(crate::screens::forks::render(state, cx))
    // ── WP-S5b3-G ("S5b 第三波" — the "監控" cluster's own 4 pages,
    // `Foresight.dc.html`/`Os.dc.html`/`Growth.dc.html`/`Reports.dc.html`)
    // AND this wave's sidebar/nav consolidation pass — see `nav.rs`'s own
    // S5b3-G header-comment update for the full membership reasoning. ─────
    } else if active_id == "foresight" {
        // WP-S5b3-G: 預測與驗證 — see `screens/foresight.rs`'s module doc
        // comment. Same "skip the generic placeholder heading entirely"
        // pattern as every other real (non-stub) page in this match.
        content_shell.child(crate::screens::foresight::render(state, cx))
    } else if active_id == "os" {
        // WP-S5b3-G: OS — see `screens/os.rs`'s module doc comment. Same
        // "skip the generic placeholder heading entirely" pattern as every
        // other real (non-stub) page in this match.
        content_shell.child(crate::screens::os::render(state, cx))
    } else if active_id == "growth" {
        // WP-S5b3-G: 成長 — see `screens/growth.rs`'s module doc comment.
        // Same "skip the generic placeholder heading entirely" pattern as
        // every other real (non-stub) page in this match.
        content_shell.child(crate::screens::growth::render(state, cx))
    } else if active_id == "reports" {
        // WP-S5b3-G: 分析報表 — see `screens/reports.rs`'s module doc
        // comment. Same "skip the generic placeholder heading entirely"
        // pattern as every other real (non-stub) page in this match.
        content_shell.child(crate::screens::reports::render(state, cx))
    // ── WP-S5b3-I ("S5b 第三波" — 畫布/世界/記憶/信箱) — this pass's own four
    // pages, self-attached here per the "D 先掛好分支就直接可達，未掛就自己掛"
    // precedent this file's own comment block above already establishes for
    // `timeline`/`org`/`forks`/`foresight`/`os`/`growth`/`reports`. ────────
    } else if active_id == "canvas" {
        // WP-S5b3-I: 畫布 — see `screens/canvas.rs`'s module doc comment.
        // Same "skip the generic placeholder heading entirely" pattern as
        // every other real (non-stub) page in this match.
        content_shell.child(crate::screens::canvas::render(state, cx))
    } else if active_id == "world" {
        // WP-S5b3-I: 世界 — see `screens/world.rs`'s module doc comment.
        // Same "skip the generic placeholder heading entirely" pattern as
        // every other real (non-stub) page in this match.
        content_shell.child(crate::screens::world::render(state, cx))
    } else if active_id == "memory" {
        // WP-S5b3-I: 記憶 — see `screens/memory.rs`'s module doc comment.
        // Same "skip the generic placeholder heading entirely" pattern as
        // every other real (non-stub) page in this match.
        content_shell.child(crate::screens::memory::render(state, cx))
    } else if active_id == "mail" {
        // WP-S5b3-I: 信箱 — see `screens/mail.rs`'s module doc comment. Same
        // "skip the generic placeholder heading entirely" pattern as every
        // other real (non-stub) page in this match.
        content_shell.child(crate::screens::mail::render(state, cx))
    // `timeline`/`org`/`forks` (WP-S5b3-H, above) and `canvas`/`world`/
    // `memory`/`mail` (WP-S5b3-I, just above) are this wave's viz-pages
    // batch, all now self-attached — each package wired its own branch per
    // the established "D 先掛好分支就直接可達，未掛就自己掛" precedent, so
    // none of the seven fall through to the generic placeholder below
    // anymore.
    // ── WP-S6b1-L (S6b 第一波, 2026-08-21) — 成員/經銷 (`UsersPage.dc.html`/
    // `DistributorsPage.dc.html`, B19). No `nav.rs` entry yet (reached via
    // `screens::manage_advanced`'s own wired rows, same pass, or the
    // debug-page override) — self-attached here per the "D 先掛好分支就直接
    // 可達，未掛就自己掛" precedent this file's own comment blocks above
    // already establish for every S5b3-wave page. ─────────────────────────
    } else if active_id == "users" {
        // WP-S6b1-L: 成員 — see `screens/users.rs`'s module doc comment. Same
        // "skip the generic placeholder heading entirely" pattern as every
        // other real (non-stub) page in this match.
        content_shell.child(crate::screens::users::render(state, cx))
    } else if active_id == "distributors" {
        // WP-S6b1-L: 經銷 — see `screens/distributors.rs`'s module doc
        // comment. Same "skip the generic placeholder heading entirely"
        // pattern as every other real (non-stub) page in this match.
        content_shell.child(crate::screens::distributors::render(state, cx))
    } else if active_id == "spike_t7" {
        // WP-gpui-spike-T7: debug-only Chromium-risk-page feasibility spike
        // (see `screens/spike_t7.rs`'s module doc comment). No `nav.rs`
        // entry — only reachable via `DUDUCLAW_NATIVE_GUI_DEBUG_PAGE=
        // spike_t7`, same "skip the generic placeholder heading" pattern.
        content_shell.child(crate::screens::spike_t7::render(state, cx))
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
