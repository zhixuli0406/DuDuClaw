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
// WP-S5b1-C (2026-08-21) — "Odoo ERP" (`Odoo.dc.html`), an "整合" drill-down
// leaf reached via `active_page == "odoo"`. See the module's own doc
// comment for RPC shapes and canvas deviations.
pub mod odoo;
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
// S5b (first wave) — 整合總覽 (`Integrations.dc.html`). Single-file page
// (no split-out sibling needed at its current size) — see the module's own
// doc comment for the four RPC shapes and the C-package drill-down
// navigation contract.
pub mod integrations;
pub mod language_picker;
pub mod login;
pub mod prototypes;
pub mod shell;
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
// Column 1/Column 2/shared-row internals of `shell.rs` — see that file's
// header comment for why the three-column app shell is split across
// multiple files (this crate's own <300-line-per-file convention). Not
// `pub`: nothing outside `screens` needs these directly, only `shell.rs`
// itself (`pub(super)` on each module's own entry point).
mod shell_content_list;
mod shell_row;
mod shell_sidebar;
