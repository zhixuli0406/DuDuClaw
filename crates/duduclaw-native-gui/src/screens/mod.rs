pub mod about;
// S4b third wave — the "AI 員工" page (p11 list / p12 detail). `agents_data`
// (types + pure parsing), `agents_list` (p11 master list column),
// `agents_summary` (p11 right mini-summary), `agents_detail` (p12 full
// detail page) are all siblings of `agents`, split off for the same
// file-size reason `dashboard`/`dashboard_cards` are split (see `agents.rs`'s
// own doc comment).
pub mod agents;
mod agents_data;
mod agents_detail;
mod agents_list;
mod agents_summary;
// S5b1-A (2026-08-21) — 帳戶與登入 (`Accounts.dc.html`). Single-file page —
// see the module's own doc comment for the RPC shapes and the B6/B8-hybrid
// canvas fidelity notes.
pub mod accounts;
// WP-S5b2-E (2026-08-21) — shared page-header/breadcrumb/category-grouping/
// catalog-card primitives for this wave's five "型錄卡牆" pages (`presets`/
// `skills`/`experts`/`inspiration_gallery`/`widgets`, all below) — same "one
// shared module for one design batch with zero divergence pressure"
// precedent `settings_common.rs` establishes for the S5b1-C integrations
// batch (see that module's own doc comment). Not `pub`: same "private mod,
// `pub(super) fn` items reachable via `crate::screens::catalog_common::…`"
// shape `settings_common`/`agents_data`/`goals_data`/`tasks_data` already
// establish.
mod catalog_common;
// S5b (first wave) — 通道 (`Channels.dc.html`). Single-file page (no
// split-out data/rows sibling needed at its current size, unlike
// `goals`/`inbox`/`tasks`) — see the module's own doc comment for the RPC
// shape and canvas fidelity notes.
pub mod channels;
pub mod chat;
pub mod console;
pub mod dashboard;
mod dashboard_cards;
// S5b1-A (2026-08-21) — the "裝置" page. `device_backup` (backup/restore +
// danger-zone sections) is a sibling of `device`, split off for the same
// file-size reason `dashboard`/`dashboard_cards` are split (see `device.rs`'s
// own doc comment).
pub mod device;
mod device_backup;
// WP-S5b2-E (2026-08-21) — "AI 團隊" (`Experts.dc.html`). Single-file page —
// see the module's own doc comment for RPC shapes and canvas deviations
// (the catalog-card skill/wiki-count fabrication the canvas draws but the
// real `experts.catalog` RPC has no field for).
pub mod experts;
// WP-S5b2-F (2026-08-21) — 檔案 (`Files.dc.html`, B3 檔案總管). `files_data`
// (types + pure parsing/formatting/query-string building) is a sibling of
// `files`, split off for the same file-size reason `goals`/`goals_data` are
// split (see `files.rs`'s own doc comment for the RPC shapes — this is the
// one page in this crate whose data source is a REST route, not a WS RPC).
pub mod files;
mod files_data;
pub mod gallery;
// WP-S5b1-C (2026-08-21) — "Google 工作區" (`GoogleIntegration.dc.html`), an
// "整合" drill-down leaf reached via `RootView::active_page == "googleIntegration"`
// (the id `screens::integrations`'s own drill-down navigation contract
// names). See the module's own doc comment for RPC shapes and canvas
// deviations; shares `settings_common`'s boxed-list/breadcrumb primitives
// with its three sibling drill-down pages (`mcp`/`odoo`/`identity`).
pub mod google_integration;
// WP-S5b1-C (2026-08-21) — "身分解析" (`Identity.dc.html`), an "整合"
// drill-down leaf reached via `active_page == "identity"`. See the module's
// own doc comment — the one page in this batch with a genuinely live
// action (test-resolve).
pub mod identity;
pub mod manage_advanced;
// WP-S5b1-C (2026-08-21) — "工具伺服器（MCP）" (`Mcp.dc.html`), an "整合"
// drill-down leaf reached via `active_page == "mcp"`. See the module's own
// doc comment for RPC shapes and canvas deviations.
pub mod mcp;
// WP-S5b2-F (2026-08-21) — "存取金鑰" (McpKeysPage). No `nav.rs` entry of
// its own — a deeper "整合" drill-down leaf reached via `screens::mcp`'s
// "存取金鑰管理 →" row (wired this pass). See the module's own doc comment
// for RPC shapes and why it carries its own local breadcrumb instead of
// widening `settings_common::breadcrumb`.
pub mod mcp_keys;
// WP-S5b1-C (2026-08-21) — "Odoo ERP" (`Odoo.dc.html`), an "整合" drill-down
// leaf reached via `active_page == "odoo"`. See the module's own doc
// comment for RPC shapes and canvas deviations.
pub mod odoo;
// WP-S5b2-D (2026-08-21) — 共同計畫 (`Plans.dc.html`). `plans_rows`/
// `plans_detail` are nested submodules declared inside `plans.rs` itself —
// same page-private-sibling shape `routines.rs`/`routines_*.rs` use. See
// `plans.rs`'s own module doc comment for RPC shapes and canvas fidelity
// notes.
pub mod plans;
// WP-S5b2-E (2026-08-21) — "職務組合" (`Presets.dc.html`). Single-file page
// — see the module's own doc comment for RPC shapes (`presets.list` +
// per-agent `presets.status` fan-out) and canvas fidelity notes.
pub mod presets;
// WP-S5b1-C (2026-08-21) — shared boxed-list/breadcrumb/kv-row primitives
// for the four "整合" drill-down pages above (`mcp`/`odoo`/
// `google_integration`/`identity`) — see the module's own doc comment for
// why this batch shares one module rather than each page duplicating the
// grammar locally (`about.rs`/`agents_detail.rs`'s usual convention). Not
// `pub`: same "private mod, `pub fn` items reachable via `crate::screens::
// settings_common::…`" shape `agents_data`/`goals_data`/`tasks_data`
// already establish for this crate's other cross-sibling internal modules.
mod settings_common;
// S4b second wave — the "目標" page (p08). `goals_inspector` (right
// inspector) and `goals_data` (types + pure parsing/filtering) are both
// siblings of `goals`, split off for the same file-size reason
// `dashboard`/`dashboard_cards` are split (see `goals.rs`'s own doc
// comment).
pub mod goals;
mod goals_data;
mod goals_inspector;
pub mod inbox;
mod inbox_data;
mod inbox_rows;
// WP-S5b2-E (2026-08-21) — "靈感畫廊" (`Gallery.dc.html`, `nav.rs` id
// `gallery`). Module named `inspiration_gallery`, NOT `gallery` — that name
// is already taken by `screens::gallery` above (the S3 component-library
// dogfood page) — see this module's own doc comment for the full
// disambiguation and RPC shapes.
pub mod inspiration_gallery;
// S5b (first wave) — 整合總覽 (`Integrations.dc.html`). Single-file page
// (no split-out sibling needed at its current size) — see the module's own
// doc comment for the four RPC shapes and the C-package drill-down
// navigation contract.
pub mod integrations;
pub mod language_picker;
pub mod login;
pub mod prototypes;
// WP-S5b2-D (2026-08-21) — 例行工作 (`Routines.dc.html`). `routines_rows`
// (left list column) / `routines_detail` (right detail column) are nested
// submodules declared inside `routines.rs` itself (`mod routines_rows;` /
// `mod routines_detail;`, files at `screens/routines/*.rs`) — the same
// page-private-sibling shape `console.rs`/`console/*.rs` already establish
// (as opposed to `goals_data`/`goals_inspector`'s screens/mod.rs-level
// promotion, reserved for helpers shared ACROSS multiple top-level pages).
// See `routines.rs`'s own module doc comment for RPC shapes and canvas
// fidelity notes.
pub mod routines;
// WP-S5b2-F (2026-08-21) — 執行紀錄 (`Runs.dc.html`, B4). `runs_data`
// (types + pure parsing/formatting) is a sibling of `runs`, split off for
// the same file-size reason `files`/`files_data` are split — see `runs.rs`'s
// own doc comment for the `runs.list`/`runs.get` RPC shapes.
pub mod runs;
mod runs_data;
pub mod shell;
// WP-S5b2-E (2026-08-21) — "技能庫" (`Skills.dc.html`, "市場" tab only —
// the other three tabs render as an honest stub per this WP's brief).
// Single-file page — see the module's own doc comment for RPC shapes and
// the real 8-token category list vs the canvas's illustrative zh-TW chips.
pub mod skills;
// WP-gpui-spike-T7 (2026-08-21): debug-only Chromium-risk-page feasibility
// spike, NOT a real product page — see `spike_t7.rs`'s own module doc
// comment for the full rationale. Reachable only via
// `DUDUCLAW_NATIVE_GUI_DEBUG_PAGE=spike_t7` (`main.rs`'s debug-page boot
// override); no `nav.rs` entry, not part of any normal navigation flow.
// `spike_t7_timeline`/`spike_t7_panzoom` are siblings holding two of the
// spike's three primitive canvases, split out for the same file-size reason
// `goals`/`goals_data`/`goals_inspector` are split (see `spike_t7.rs`'s own
// doc comment).
pub mod spike_t7;
mod spike_t7_panzoom;
mod spike_t7_timeline;
pub mod system_updates;
// S4b third wave — the "任務" list page (p09) + its full detail page (p10).
// `tasks_data` (types + pure parsing/filtering), `tasks_quickview` (list
// page's right-column quick view), `tasks_detail` (the in-page full detail
// view `tasks_quickview` links to), and `tasks_detail_data` (that detail
// page's own data model + fetch/write orchestration, split out of
// `tasks_detail` to keep it under 800 lines too) are all siblings of
// `tasks`, same file-size-driven split `goals`/`goals_data`/
// `goals_inspector` establish — see `tasks.rs`'s own module doc comment.
pub mod tasks;
mod tasks_data;
mod tasks_detail;
mod tasks_detail_data;
mod tasks_quickview;
// WP-S5b2-E (2026-08-21) — "Widget 工坊" (`Widgets.dc.html`). Single-file
// page — see the module's own doc comment for RPC shapes, the `RootView::
// user_id` addition it needed for the 我的/團隊分享 split, and the
// decorative-thumbnail deviation (no HTML-sandbox rendering in gpui).
pub mod widgets;
// Column 1/Column 2/shared-row internals of `shell.rs` — see that file's
// header comment for why the three-column app shell is split across
// multiple files (this crate's own <300-line-per-file convention). Not
// `pub`: nothing outside `screens` needs these directly, only `shell.rs`
// itself (`pub(super)` on each module's own entry point).
mod shell_content_list;
mod shell_row;
mod shell_sidebar;
