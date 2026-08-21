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
// WP-S6b1-J (2026-08-21) — "帳務" (`BillingPage.dc.html`, B17), a "進階設定"
// drill-down leaf reached via `active_page == "billing"`. See the module's
// own doc comment for the `billing.usage`/`accounts.budget_summary`/
// `budget.incidents` RPC shapes and canvas deviations.
pub mod billing;
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
// WP-S5b3-I (2026-08-21) — 畫布 (`Canvas.dc.html`, B12). Single-file page —
// see the module's own doc comment for RPC shapes and the HTML-rendering
// honesty boundary (gpui has no browser/HTML-sandbox capability; see also
// `panzoom` below, its pan/zoom-able content frame's shared primitive).
pub mod canvas;
// S5b (first wave) — 通道 (`Channels.dc.html`). Single-file page (no
// split-out data/rows sibling needed at its current size, unlike
// `goals`/`inbox`/`tasks`) — see the module's own doc comment for the RPC
// shape and canvas fidelity notes.
pub mod channels;
pub mod chat;
pub mod console;
// WP-S6b2-N (S6b 第二波, 2026-08-21) — "新增員工" (CreateAgent, `CreateAgent.
// dc.html`). No `nav.rs` entry — reached from the "AI 員工" list's own
// "新員工" button (`agents_list.rs`, this pass flips it from an honest
// disabled stub to a real `active_page = "createAgent"` click). `create_
// agent_data` (types + pure parsing) is a sibling, split off for the same
// file-size reason `agents_data.rs`/`goals_data.rs` are split from their own
// page modules — see `create_agent.rs`'s own module doc comment for RPC
// shapes and the assembled-vs-wired boundary.
pub mod create_agent;
mod create_agent_data;
mod create_agent_sections;
pub mod dashboard;
mod dashboard_cards;
// WP-S6b3-P (S6b 第三波, 2026-08-22) — "部門" (`Departments.dc.html`, B1
// 降規二欄 左清單/右詳情). A "進階設定" drill-down leaf reached via
// `active_page == "departments"` — wired from `manage_advanced.rs`'s 部門
// row by this same pass. See the module's own doc comment for the
// `departments.list` RPC shape and the description/role-title canvas
// deviations (no such fields exist on `DepartmentInfo`).
pub mod departments;
// S5b1-A (2026-08-21) — the "裝置" page. `device_backup` (backup/restore +
// danger-zone sections) is a sibling of `device`, split off for the same
// file-size reason `dashboard`/`dashboard_cards` are split (see `device.rs`'s
// own doc comment).
pub mod device;
mod device_backup;
// WP-S6b1-L (S6b 第一波, 2026-08-21) — "經銷" (`DistributorsPage.dc.html`,
// B19). Single-file page — see the module's own doc comment for the
// `distributor.list` RPC shape, why `distributor.status` isn't ALSO called,
// and the tier-badge/fingerprint-mask canvas deviations.
pub mod distributors;
// WP-S6b2-N (S6b 第二波, 2026-08-21) — "編輯員工" (EditAgent, `EditAgent.dc.
// html`). `edit_agent_data` (types + pure parsing), `edit_agent_tabs_a`
// (技能/工具/整合/一般), `edit_agent_tabs_b` (大腦/預算/自動化/進階) are
// siblings of `edit_agent`, split off for the same file-size reason
// `tasks`/`tasks_data`/`tasks_detail`/`tasks_detail_data`/`tasks_quickview`
// are split (see `edit_agent.rs`'s own module doc comment for the RPC
// shapes, the read-only-this-round scope decision, and the per-tab "誠實
// 偏差" field citations). No `nav.rs` entry — reached via `screens::agents_
// detail`'s own "編輯" button, or `DUDUCLAW_NATIVE_GUI_DEBUG_PAGE=editAgent`.
pub mod edit_agent;
mod edit_agent_data;
mod edit_agent_tabs_a;
mod edit_agent_tabs_b;
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
// WP-S5b3-G (S5b 第三波, 2026-08-21) — "預測與驗證" (`Foresight.dc.html`,
// B9). Single-file page — see the module's own doc comment for the
// `forward.summary`/`forward.states`/`forward.recent`/`forward.calibration`
// RPC shapes and the scope cut vs. `web/src/pages/ForesightPage.tsx` (no
// belief tab, no chain drill-down, deterministic top-agent auto-pick for
// calibration instead of a manual agent `<Select>`).
pub mod foresight;
// WP-S5b3-H (S5b 第三波, 2026-08-21) — "分支決戰" (`Forks.dc.html`, B13),
// RFC-26 Live Run Forking. `forks_data` (types + pure parsing) is a sibling
// of `forks`, split off for the same file-size reason `runs`/`runs_data` are
// split — see `forks.rs`'s own module doc comment for the `fork.list`/
// `fork.inspect` RPC shapes and the left-list title deviation.
pub mod forks;
mod forks_data;
pub mod gallery;
// WP-S6b3-R (S6b 第三波, 2026-08-22) — "GatewayPicker" (`GatewayPicker.dc.
// html`, B23 獨立小視窗). No `nav.rs` entry — self-attached in `screens/
// shell.rs` only, `DUDUCLAW_NATIVE_GUI_DEBUG_PAGE=gatewayPicker`. See the
// module's own doc comment for the execution-time attribution (Tauri-only
// `gateway_*` IPC commands, structurally unreachable from this crate — same
// shape `pet_studio.rs` documents) and the one real value this crate DOES
// have (`api::GATEWAY_BASE_URL` + live `WsConnState`), rendered as a
// centered card inside the normal shell rather than a root-swap.
pub mod gateway_picker;
// WP-S5b1-C (2026-08-21) — "Google 工作區" (`GoogleIntegration.dc.html`), an
// "整合" drill-down leaf reached via `RootView::active_page == "googleIntegration"`
// (the id `screens::integrations`'s own drill-down navigation contract
// names). See the module's own doc comment for RPC shapes and canvas
// deviations; shares `settings_common`'s boxed-list/breadcrumb primitives
// with its three sibling drill-down pages (`mcp`/`odoo`/`identity`).
pub mod google_integration;
// WP-S6b1-K (S6b 第一波, 2026-08-21) — "治理規則" (`GovernancePage.dc.html`,
// B18) + the shared `GovernanceShell` tabs/breadcrumb/spawn_call primitives
// `wiki_trust.rs` (below) also imports — see the module's own doc comment
// for RPC shapes and canvas fidelity deviations.
pub mod governance;
// WP-S5b3-G (S5b 第三波, 2026-08-21) — "成長" (`Growth.dc.html`, B9). Single-
// file page — see the module's own doc comment for the `growth.snapshot`/
// `growth.daily_report` RPC shapes and the scope cut vs. `web/src/pages/
// GrowthPage.tsx` (no 7-day archive picker — `growth.daily_report` is
// called with no `date`, which reports yesterday only).
pub mod growth;
// WP-S5b1-C (2026-08-21) — "身分解析" (`Identity.dc.html`), an "整合"
// drill-down leaf reached via `active_page == "identity"`. See the module's
// own doc comment — the one page in this batch with a genuinely live
// action (test-resolve).
pub mod identity;
// WP-S5b3-I (2026-08-21) — 信箱 (`Mail.dc.html`, B15, Agent Mail / P2-d).
// Single-file page — see the module's own doc comment for the six `mail.*`
// RPC shapes and why 確認寄出/不要寄/撰寫 are assembled but not wired (mail
// leaving the building is an ApprovalBroker decision, per this WP's own
// product-rule brief).
pub mod mail;
pub mod manage_advanced;
// WP-S6b1-J (2026-08-21) — shared breadcrumb + LicenseShell tab-strip
// primitives for this batch's 3 "進階設定" drill-down pages (`billing`/
// `license`/`partner_portal`, all in this file). See the module's own doc
// comment for why this is its own module rather than widening
// `settings_common` (a different batch's "整合" breadcrumb root).
mod manage_advanced_common;
// WP-S6b3-Q (S6b 第三波, 2026-08-22) — "市集" (`Marketplace.dc.html`, B2),
// an "整合" (Integrations) conceptual drill-down. See the module's own doc
// comment for the `marketplace.list` RPC shape, the real 10-entry catalog
// vs. the canvas's illustrative mockup names, and why the search box is
// decorative while the 5 category chips are real.
pub mod marketplace;
// WP-S6b3-R (S6b 第三波, 2026-08-22) — "桌寵浮層" (`MascotOverlay.dc.html`,
// B24 透明浮層). No `nav.rs` entry — self-attached in `screens/shell.rs`
// only, `DUDUCLAW_NATIVE_GUI_DEBUG_PAGE=mascotOverlay`. See the module's own
// doc comment for the execution-time attribution (a Tauri transparent/
// borderless/always-on-top second window this crate cannot open — `main.rs`
// opens exactly one window) and the one thing this page DOES fetch for
// real, unlike `pet_studio.rs`'s zero-RPC page: the pending-approvals badge
// count (`approvals.list`, the same RPC `dashboard.rs`/`console.rs`/
// `inbox.rs` already call).
pub mod mascot_overlay;
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
// WP-S6b2-M (S6b 第二波, 2026-08-21) — "資料搬家" (`Migrate.dc.html`, B16
// full-screen wizard). `migrate_views` (its own `screens/migrate/`
// subdirectory, `mod migrate_views;` declared inside `migrate.rs` itself —
// same page-private-sibling shape `plans.rs`/`routines.rs` establish) holds
// the three per-step content panels. See `migrate.rs`'s own module doc
// comment for the `migrate.scan`/`migrate.apply` RPC shapes, why this page
// bypasses the normal sidebar/content-list/content_shell composition
// entirely (a `shell.rs`-level root swap, not an overlay), and the canvas-
// fidelity deviations (outer window-mockup chrome dropped).
pub mod migrate;
// WP-S5b3-I (2026-08-21) — 記憶 (`Memory.dc.html`, B14). `memory_rows`
// (category rail + list column) / `memory_detail` (right detail column) are
// nested submodules declared inside `memory.rs` itself (files at
// `screens/memory/*.rs`) — same page-private-sibling shape `plans.rs`/
// `routines.rs` establish. See `memory.rs`'s own module doc comment for RPC
// shapes and canvas fidelity notes (only the 記憶 tab is wired to real data;
// the other three tabs render an honest stub).
pub mod memory;
// WP-S5b1-C (2026-08-21) — "Odoo ERP" (`Odoo.dc.html`), an "整合" drill-down
// leaf reached via `active_page == "odoo"`. See the module's own doc
// comment for RPC shapes and canvas deviations.
pub mod odoo;
// WP-S5b3-H (S5b 第三波, 2026-08-21) — "組織架構" (`OrgIndented.dc.html`,
// 方案 A 縮排階層清單 — the user's own 2026-08-21 拍板; B/C alternatives not
// built). `org_data` (types + `reports_to` tree flattening) is a sibling of
// `org`, split off for the same file-size reason `runs`/`runs_data` are
// split — see `org.rs`'s own module doc comment for the `agents.list` RPC
// shape and why this page can't reuse `agents_data::AgentListItem`.
pub mod org;
mod org_data;
// WP-S5b3-G (S5b 第三波, 2026-08-21) — "OS" (`Os.dc.html`, B9, KDE 兩層式).
// Single-file page — see the module's own doc comment for the `agents.list`/
// `os.status`/`os.gate.recent` RPC shapes and the scope cut vs. `web/src/
// pages/OSPage.tsx` (no live event tail, no environment doctor, read-only).
pub mod os;
// WP-S5b3-I (2026-08-21) — shared pan/zoom primitive for `canvas.rs`/
// `world.rs`'s content frames — see the module's own doc comment for why
// this is re-layout zoom (plain `Div` sizes recomputed per render), not a
// GPU transform (this crate's pinned gpui rev has no `Styled`-trait scale
// method reachable from element-building code). Not `pub`: same "private
// mod, `pub fn`/`pub struct` items reachable via `crate::screens::panzoom::…`"
// shape `agents_data`/`goals_data`/`catalog_common` already establish.
mod panzoom;
// WP-S6b2-O (S6b 第二波, 2026-08-21) — "桌寵工作室" (`PetStudio.dc.html`, B21
// 獨立工具視窗). No `nav.rs` entry (the canvas frames it as reached from the
// mascot's right-click menu / menu bar, not the sidebar) — self-attached in
// `screens/shell.rs` only, `DUDUCLAW_NATIVE_GUI_DEBUG_PAGE=petStudio`. See
// the module's own doc comment for why this is the one page in the batch
// with NO reachable RPC/bridge at all (Tauri-only `pet_*` commands, a
// different runtime than this crate).
pub mod pet_studio;
// WP-S6b1-J (2026-08-21) — "經銷夥伴入口" (`PartnerPortalPage.dc.html`, B19),
// the LicenseShell's second tab (see `screens::license`). See the module's
// own doc comment for the `partner.*` RPC shapes and canvas deviations.
pub mod partner_portal;
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
// WP-S6b3-P (S6b 第三波, 2026-08-22) — "可靠性" (`Reliability.dc.html`, B9
// Tabs 切資源 + 單一大圖表). A "進階設定" drill-down leaf reached via
// `active_page == "reliability"` — wired from `manage_advanced.rs`'s 可靠性
// 列 by this same pass. See the module's own doc comment for why `audit.
// unified_log`'s real `channel_failure` source is used instead of the
// per-agent `audit.reliability_summary`/`audit.evolution_query` family
// `web/src/pages/ReliabilityPage.tsx` calls, and the "one real tab, three
// honest stubs" scope cut.
pub mod reliability;
// WP-S5b3-G (S5b 第三波, 2026-08-21) — "分析報表" (`Reports.dc.html`, B9,
// 總覽→深潛). Single-file page — see the module's own doc comment for the
// `analytics.summary`/`analytics.conversations`/`analytics.cost_savings`/
// `cost.summary`/`cost.agents` RPC shapes; the only page this wave whose
// charts are gpui-canvas-drawn (line chart + bar chart) rather than
// width-percentage divs, per this task's brief.
pub mod reports;
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
// WP-S6b3-P (S6b 第三波, 2026-08-22) — "模型用量" (`Inference.dc.html`, B9
// Tabs 切模型供應商 + KPI 磚 + 單一大圖表). A "進階設定" drill-down leaf
// reached via `active_page == "inference"` — wired from `manage_advanced.
// rs`'s 模型用量 row by this same pass. See the module's own doc comment for
// the `cost.summary`/`cost.recent`/`inference.get` RPC shapes and why the 4
// provider tabs are derived from `cost.recent`'s real `model` field rather
// than 4 separate RPC calls.
pub mod inference;
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
// WP-S6b3-Q (S6b 第三波, 2026-08-22) — shared primitives for this pass's
// three "知識中樞" pages (`knowledge_hub`/`knowledge_curation`/
// `shared_wiki`, all below) — see the module's own doc comment for the
// shared `WikiPageMeta` parser, `namespace_of` folder-grouping key, and the
// 5-tab `KnowledgeView` switcher + how WP-S6b3-fix (2026-08-22) resolved the
// side-nav highlight this task's brief asked for (a real `nav.rs` id plus a
// drill-down mapping — see that module's own doc comment). Not `pub`: same
// "private
// `mod`, `pub(super) fn`/`pub(super) struct` items reachable via `crate::
// screens::knowledge_common::…`" shape `catalog_common`/`settings_common`
// already establish.
mod knowledge_common;
// WP-S6b3-Q (S6b 第三波, 2026-08-22) — "知識審核" (`KnowledgeCuration.dc.
// html`, B25+頁型3). See the module's own doc comment for the `wiki.
// auto_pages`/`wiki.read` RPC shapes and the assembled-not-wired 核准歸檔/
// 退回 buttons.
pub mod knowledge_curation;
// WP-S6b3-Q (S6b 第三波, 2026-08-22) — "知識中樞" (`KnowledgeHub.dc.html`,
// B25). See the module's own doc comment for the `wiki.pages`/`wiki.read`/
// `wiki.stats`/`wiki.lint` RPC shapes and the 5-tab scope cut (only 瀏覽/
// 健康度 wired to real data; 搜尋 decorative, 圖譜 an explicit placeholder,
// 審核 navigates to `knowledge_curation`).
pub mod knowledge_hub;
pub mod language_picker;
// WP-S6b3-R (S6b 第三波, 2026-08-22) — "Launcher" (`Launcher.dc.html`, B22
// APPS 網格). No `nav.rs` entry — self-attached in `screens/shell.rs` only
// as a full-bleed root swap (same shape `migrate.rs` establishes),
// `DUDUCLAW_NATIVE_GUI_DEBUG_PAGE=launcher`. See the module's own doc
// comment for the two OTHER "launcher" surfaces in this codebase this page
// is neither of (web's `/launcher` route, `duduclaw-shell`'s Super-K
// overlay), the static APPS-registry mirror (no backend RPC exists), and
// the real client-side search filter + real in-shell navigation wiring.
pub mod launcher;
// WP-S6b1-J (2026-08-21) — "授權" (`LicensePage.dc.html`, B16+B18), a
// "進階設定" drill-down leaf; also hosts the LicenseShell tab strip shared
// with `screens::partner_portal`. See the module's own doc comment for the
// `license.status` RPC shape, the verified tier-string wire values, and the
// canvas deviations.
pub mod license;
// WP-S6b3-P (S6b 第三波, 2026-08-22) — "本地模型市集" (`LocalModels.dc.html`,
// B2 已安裝/可下載雙態型錄卡). A "進階設定" drill-down leaf reached via
// `active_page == "localModels"` — wired from `manage_advanced.rs`'s 本地
// 模型市集 row by this same pass. See the module's own doc comment for the
// `localmodels.installed`/`.search` RPC shapes and the derived (never
// fabricated) market-card description.
pub mod local_models;
// WP-S6b3-P (S6b 第三波, 2026-08-22) — "日誌" (`Logs.dc.html`, B4 flat
// table + filter row). A "進階設定" drill-down leaf reached via
// `active_page == "logs"` — wired from `manage_advanced.rs`'s 日誌 row by
// this same pass. See the module's own doc comment for the `audit.
// unified_log` RPC shape and the real `Entity<TextField>` search box.
pub mod logs;
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
// WP-S6b3-P (S6b 第三波, 2026-08-22) — "安全審計" (`Secaudit.dc.html`, B4
// flat findings table + filter row). A "進階設定" drill-down leaf reached
// via `active_page == "secaudit"` — wired from `manage_advanced.rs`'s 安全
// 審計列 by this same pass. See the module's own doc comment for the
// `secaudit.reports`/`secaudit.report` RPC shapes and why this page shows
// only the single newest report's findings rather than a cross-scan
// aggregation.
pub mod secaudit;
// WP-S6b1-K (S6b 第一波, 2026-08-21) — "安全" (`SecurityPage.dc.html`, B5+
// B18 合併版). Single-file page — see the module's own doc comment for the
// `security.status` RPC shape and canvas fidelity deviations (緊急控制 has
// no backing RPC field at all; RBAC "最後變更" column has none either).
pub mod security;
// WP-S6b3-Q (S6b 第三波, 2026-08-22) — "共享知識庫" (`SharedWiki.dc.html`,
// B25). See the module's own doc comment for the `shared_wiki.pages`/
// `shared_wiki.read`/`wiki_scope.get` RPC shapes and the real (not
// mockup-copied) folder-grouping + policy-mode subtitle.
pub mod shared_wiki;
// WP-S6b3-P (S6b 第三波, 2026-08-22) — "系統設定" (`Settings.dc.html`, B5
// Tabs 索引 5 個 + boxed-list, only 通用 has real content). A "進階設定"
// drill-down leaf reached via `active_page == "settings"` — wired from
// `manage_advanced.rs`'s 系統設定列 by this same pass. See the module's own
// doc comment for the `system.config`/`system.version` RPC shapes and the
// three dropped rows (時區/啟動時檢查健康狀態/需要人工決策時通知 — no
// backing field in either RPC).
pub mod settings;
pub mod shell;
// WP-S5b2-E (2026-08-21) — "技能庫" (`Skills.dc.html`, "市場" tab only —
// the other three tabs render as an honest stub per this WP's brief).
// Single-file page — see the module's own doc comment for RPC shapes and
// the real 8-token category list vs the canvas's illustrative zh-TW chips.
pub mod skills;
// WP-S6b2-N (S6b 第二波, 2026-08-21) — 自建技能詳情 (SkillCustomDetail,
// `SkillCustomDetail.dc.html`). No `nav.rs` entry of its own — a
// `skills.rs` drill-down conceptually, same shape `skill_new.rs` right
// below already establishes; reachable only via
// `DUDUCLAW_NATIVE_GUI_DEBUG_PAGE=skillCustom` this round (no "我的技能"
// list row exists yet to click into it from). See the module's own doc
// comment for the `skills.custom_list`/`skills.custom_retire` RPC shapes,
// the client-side "find by id" reason (no dedicated get-by-id RPC exists),
// and the assembled-not-wired "封存這個技能" button.
pub mod skill_custom_detail;
// WP-S6b2-M (S6b 第二波, 2026-08-21) — "新增技能" (`SkillNew.dc.html`, B16
// in-page wizard, sidebar retained). No `nav.rs` entry of its own — a
// `skills.rs` drill-down conceptually (see that module's own doc comment
// for the full disambiguation). See `skill_new.rs`'s own module doc comment
// for the six `skills.custom_*` RPC line numbers cited (none wired this
// pass — a local-only step wizard, per this task's own brief) and the
// canvas fidelity notes (only the "生成" step is drawn; describe/form/
// review are honest minimal stubs).
pub mod skill_new;
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
// WP-S6b3-Q (S6b 第三波, 2026-08-22) — "系統" (`SystemHome.dc.html`, 頁型2
// 分區索引卡). See the module's own doc comment for the six-card → real-page
// mapping and why the side-nav highlight is DELIBERATELY zero (QA #3
// ruling, not a gap — quoted verbatim from the canvas's own leading HTML
// comment).
pub mod system_home;
pub mod system_updates;
// WP-S5b3-H (S5b 第三波, 2026-08-21) — "工作時間軸" (`Timeline.dc.html`,
// B10). `timeline_data` (types + lane-packing/kind-color layout) is a
// sibling of `timeline`, split off for the same file-size reason
// `runs`/`runs_data` are split — see `timeline.rs`'s own module doc comment
// for the `timeline.list` RPC shape and the spike_t7_timeline.rs painting
// recipe this page follows.
pub mod timeline;
mod timeline_data;
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
// WP-S6b1-L (S6b 第一波, 2026-08-21) — "成員" (`UsersPage.dc.html`, B19).
// Single-file page — see the module's own doc comment for the `users.list`
// RPC shape and the 通道身分-column/status-label canvas deviations.
pub mod users;
// WP-S6b2-O (S6b 第二波, 2026-08-21) — "新增 Widget" (`WidgetComposer.dc.
// html`, B21 複合編輯器桶), a creation-only leaf of `screens::widgets`. See
// the module's own doc comment for the `cost.agents` RPC shape feeding the
// real live preview and why the 儲存/產生/依回饋重新生成 decision-class
// actions are assembled but not wired.
pub mod widget_composer;
// WP-S5b2-E (2026-08-21) — "Widget 工坊" (`Widgets.dc.html`). Single-file
// page — see the module's own doc comment for RPC shapes, the `RootView::
// user_id` addition it needed for the 我的/團隊分享 split, and the
// decorative-thumbnail deviation (no HTML-sandbox rendering in gpui).
pub mod widgets;
// WP-S6b1-K (S6b 第一波, 2026-08-21) — "Wiki 信任層級" (`WikiTrustPage.dc.
// html`, B18) — the second tab of the `GovernanceShell` `governance.rs`
// owns (see that module's own doc comment). Single-file page — see this
// module's own doc comment for the `agents.list`/`wiki.trust_audit` RPC
// shapes and canvas fidelity deviations.
pub mod wiki_trust;
// WP-S5b3-I (2026-08-21) — 世界 (`World.dc.html`, a B12 variant). Single-file
// page — see the module's own doc comment for why it reuses `agents.list`
// (no dedicated "world state" RPC exists), the deterministic token-scatter
// hash, and the canvas fidelity deviations (no fabricated speech-bubble
// status text; scene switcher is real but client-local UI state, same as
// the web `WorldStage`'s own `localStorage`-only scene choice).
pub mod world;
// Column 1/Column 2/shared-row internals of `shell.rs` — see that file's
// header comment for why the three-column app shell is split across
// multiple files (this crate's own <300-line-per-file convention). Not
// `pub`: nothing outside `screens` needs these directly, only `shell.rs`
// itself (`pub(super)` on each module's own entry point).
mod shell_content_list;
mod shell_row;
mod shell_sidebar;
