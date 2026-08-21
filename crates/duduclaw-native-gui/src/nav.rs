// Sidebar navigation data — P0-1 of the "殼回訪修正" pass (2026-08-19, see
// `research/native-os-2026-08/desktop-app-conventions.md` §B, P0 item 1).
//
// ── Why this file changed shape ──────────────────────────────────────────
// The PREVIOUS structure (kept in git history) mirrored `web/src/components/
// layout/nav-model.ts` almost verbatim: a flat "daily" rail up top, then
// `工作`/`公司`/`設定` groups — ~19 items poured flat into ONE scrollable
// list. That is web-app IA, not desktop IA. The research doc's HIG survey
// (Apple/Microsoft/GNOME, one-hand sources, §2 "導覽階層") converges on two
// hard numbers for a native app: **macOS sidebars top out at 5–8 top-level
// areas**, and going deeper than that means a THIRD column (sidebar → content
// list → detail), not a longer flat list or nested tree. This file now
// exports that three-tier shape: `AREAS` (6 top-level areas, rendered as
// Column 1 by `screens/shell.rs`), each area's `items` (Column 2, "content
// list"), and `FOOTER_ITEMS` (pinned outside every area, Column 1's own
// footer — the Windows `NavigationView.IsSettingsVisible` convention the
// research doc cites: "settings should be the last item…pinned to the
// bottom").
//
// ── The 6 areas and why each existing id landed where it did ────────────
// Every id from the previous flat/grouped structure is preserved — none was
// dropped, per the task brief's "既有頁 id 全部保留映射". The mapping:
//
//   對話 (chat)       — newChat, console. The two "operate right now"
//                        surfaces. `newChat` ALSO keeps its own dedicated
//                        CTA button in Column 1 (unchanged from before) —
//                        appearing both as the CTA and as this area's first
//                        content-list row is the same duplication the OLD
//                        `DAILY_ITEMS` block already had (a distinctly-
//                        styled "+" button AND a same-id flat nav row), not
//                        a new decision.
//   AI 員工 (agents)   — agents, experts, world, growth. Unchanged from the
//                        old `公司` group's WARNING (amber) hue.
//   任務與目標 (tasks) — routines, plans, goals, runs, tasks. Unchanged from
//                        the old `工作` group's SUCCESS (green) hue, minus
//                        `files`/`reports` which moved out (see below).
//   知識與記憶 (knowledge) — files, skills, memory. `files` used to live in
//                        `工作` (SUCCESS) and `skills`/`memory` in `公司`
//                        (WARNING) — splitting them left one area with two
//                        clashing hues. Recolored INFO as one area, one hue
//                        (`nav.rs`'s own established convention, see the old
//                        file's "One hue per GROUP" comment — same rule,
//                        reapplied to the new grouping).
//   監控 (monitor)     — home, reports. `home` (the old flat-rail dashboard)
//                        and `reports` (old tail of `工作`) are both
//                        "look, don't touch" surfaces — CHART_2 hue.
//   管理 (manage)      — originally `about` only (Phase 1a placeholder,
//                        since nothing else had an id yet); superseded by
//                        the S5b1-A update below, which is now this area's
//                        current, real membership. See "Two settle-related
//                        decisions" for why `manage`/`componentLibrary` are
//                        NOT here (footer-pinned instead).
//
// ── Two settle-related decisions worth stating explicitly ───────────────
// 1. `manage` (nav.manage, "管理" — its own i18n desc is literally "整合、
//    帳務、安全、設定") is pinned to `FOOTER_ITEMS`, not put inside the new
//    "管理" AREA. Its description already IS the HIG "Settings" concept
//    (macOS Cmd-,/Windows `IsSettingsVisible`) — this crate has no separate
//    "Preferences" id to invent, and `manage` already covers exactly that
//    ground. Reusing it avoids adding a synthetic id nothing else points at.
// 2. `componentLibrary` is pinned to `FOOTER_ITEMS` per the task brief
//    verbatim ("「設定」與「元件庫」釘在側欄 footer…不進分組").
//
// ── S5b1-A update (2026-08-21): the "管理" AREA gained real occupants ────
// The Phase-1a placeholder above ("管理" area holds only `about`, since
// nothing else had an id yet) is now stale — WP-S5b1-A wires four of the
// S5 settings pages (`device`/`manageAdvanced`/`systemUpdates`/`accounts`)
// plus two placeholder rows this same wave predefines for a sibling package
// to fill in (`channels`/`integrations`). `MANAGE_ITEMS` below is now the
// literal 6-item membership the S5 canvas's own "統一側欄組成" panel spells
// out (`commercial/design/duduclaw-s5-settings-pages/Main.dc.html`,
// approved 2026-08-21): 通道／整合／帳戶與登入／系統更新／裝置／進階設定, in
// that exact order.
//
// `about` is deliberately DROPPED from this area, not folded in as a 7th
// item — the approved canvas's own sidebar mockup (all 10 artboards share
// one literal sidebar HTML fragment) renders exactly those 6 rows, and the
// cover sheet's "與 web 版面的刻意差異" §4 states the group "從 S4a 的 2 項
// 擴充為 6 項（通道／整合／帳戶與登入／系統更新／裝置／進階設定）" — an
// enumerated list that does not include `about`. `about` stays fully
// reachable exactly as before via the macOS menu bar (App ▸ About DuDuClaw,
// `main.rs`'s `ShowAbout` action sets `active_page = "about"` directly, with
// no dependency on `nav::find`/`AREAS` membership) — this file's own
// `screens/about.rs` doc comment already documented that path as
// "belt-and-suspenders, not exclusive"; it is now the ONLY path on this
// build. Non-macOS builds (no menu bar yet) lose sidebar reachability for
// this one page until a future pass gives it one — an accepted, documented
// gap, not a silent regression (tracked by this comment, not hidden).
//
// ── S5b2-D update (2026-08-21): four areas reshuffled per the work-pages
// canvas ──────────────────────────────────────────────────────────────────
// WP-S5b2-D ("S5b 第二波" — this crate's sole `nav.rs`/`shell.rs` editor for
// the wave, per its own task brief) wires the S5a work-pages canvas
// (`commercial/design/duduclaw-s5-work-pages/`, approved 2026-08-21) into
// `AREAS`. Four constants change; see each constant's own comment above for
// the full per-area reasoning. Summary of WHERE ids moved (every id keeps a
// home — none dropped, same "既有頁 id 全部保留映射" rule §20 states):
//   `files`:  KNOWLEDGE_ITEMS → TASKS_ITEMS   (work-with-your-files, not
//             generic knowledge — matches the canvas's own §2 rationale)
//   `skills`: KNOWLEDGE_ITEMS → AGENTS_ITEMS  (an agent capability, grouped
//             with the AI-employee pages that configure agents)
//   `runs`:   TASKS_ITEMS → MONITOR_ITEMS     (this wave's explicit "「監控」
//             加 執行紀錄" instruction)
//   `presets`/`gallery`/`widgets`: brand-new ids this wave, landing directly
//             in AGENTS_ITEMS/AGENTS_ITEMS/KNOWLEDGE_ITEMS respectively.
// Recolored to match each NEW area's established "one hue per GROUP"
// convention (`files`→SUCCESS, `skills`→WARNING, `runs`→CHART_2) rather than
// keeping their old area's hue — the whole point of that convention (§37-43
// above) is that hue signals CURRENT group membership, so carrying a stale
// hue across a move would misrepresent it. `presets`/`skills`/`experts`/
// `gallery`'s pages are this wave's SIBLING packages' scope (E/B2), not this
// pass's own two pages (`routines`/`plans`, B1) — `screens/shell.rs`'s
// generic placeholder branch renders them honestly until each sibling lands
// its own page, same pattern the S5b1-A update above already established for
// `channels`/`integrations`.

use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NavItem {
    /// Stable identity used both as the gpui `ElementId` suffix and as the
    /// "currently selected page" key (`RootView::active_page`).
    pub id: &'static str,
    /// i18n key for the row label (`nav.<id>` in the catalogs).
    pub label_key: &'static str,
    /// i18n key for the one-line description shown in the content-area
    /// placeholder heading (`nav.<id>.desc`).
    pub desc_key: &'static str,
    /// Placeholder icon: an uppercase letter + a fixed accent color (real
    /// lucide-style iconography is still Phase 1b scope, see the previous
    /// version of this file's now-superseded note on that).
    pub badge_letter: char,
    pub badge_color: u32,
}

const fn item(
    id: &'static str,
    label_key: &'static str,
    desc_key: &'static str,
    badge_letter: char,
    badge_color: u32,
) -> NavItem {
    NavItem { id, label_key, desc_key, badge_letter, badge_color }
}

/// A top-level sidebar area (Column 1 row) plus the pages it expands to
/// (Column 2, "content list" — Apple HIG's own three-column vocabulary,
/// research doc §1.4/§2). `id` is this file's own internal namespace
/// (`area*`), deliberately distinct from every `NavItem::id` — `manage`
/// exists as BOTH a footer `NavItem` id and would otherwise be a tempting
/// area id, so every area id here is prefixed to rule out that collision
/// outright rather than rely on call sites never mixing the two up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NavArea {
    pub id: &'static str,
    /// i18n key for the area's own row label (`navArea.<name>`).
    pub label_key: &'static str,
    /// This area's own badge, shown on its Column-1 row — independent of
    /// any one item's badge (an area is not itself a page).
    pub badge_letter: char,
    pub badge_color: u32,
    pub items: &'static [NavItem],
}

const fn area(
    id: &'static str,
    label_key: &'static str,
    badge_letter: char,
    badge_color: u32,
    items: &'static [NavItem],
) -> NavArea {
    NavArea { id, label_key, badge_letter, badge_color, items }
}

const CHAT_ITEMS: &[NavItem] = &[
    item("newChat", "nav.newChat", "nav.newChat.desc", '+', theme::BRAND),
    item("console", "nav.console", "nav.console.desc", 'C', theme::CHART_2),
];

// S5b2-D (2026-08-21) update: this area is now the S5a work-pages canvas's
// 5-item "AI 員工" membership — 員工總覽/職務組合/技能庫/AI團隊/靈感畫廊
// (`agents`/`presets`/`skills`/`experts`/`gallery`, in that exact order —
// `commercial/design/duduclaw-s5-work-pages/Main.dc.html`'s "與 web 版面的
// 刻意差異" §1). `presets`/`gallery` are new ids this wave; `skills` MOVES IN
// from the old `KNOWLEDGE_ITEMS` (a skill is an agent capability, not a
// knowledge/memory artifact — see that constant's own note). `world`/
// `growth` are DELIBERATELY KEPT here too (appended, unchanged position)
// rather than dropped to hit the canvas's literal "5 項" count: the brief's
// 5-item list has no destination for them, and this crate's own established
// rule (`nav.rs`'s header comment, "既有頁 id 全部保留映射") is to never strand
// an existing id without an assigned home. The canvas's own FULL unified
// sidebar (embedded per-page, e.g. `Routines.dc.html`) in fact shows `world`/
// `growth` still living in this exact area (with a not-yet-built `組織架構`
// row between `gallery` and `world` — that page belongs to a LATER wave, not
// this one) — so keeping them here is the closer reading of the two
// documents, not just the safer one. A future wave that lands 組織架構 should
// insert it between `gallery` and `world` to match the canvas exactly.
const AGENTS_ITEMS: &[NavItem] = &[
    item("agents", "nav.agents", "nav.agents.desc", 'E', theme::WARNING),
    item("presets", "nav.presets", "nav.presets.desc", 'P', theme::WARNING),
    item("skills", "nav.skills", "nav.skills.desc", 'S', theme::WARNING),
    item("experts", "nav.experts", "nav.experts.desc", 'X', theme::WARNING),
    item("gallery", "nav.gallery", "nav.gallery.desc", 'I', theme::WARNING),
    item("world", "nav.world", "nav.world.desc", 'W', theme::WARNING),
    item("growth", "nav.growth", "nav.growth.desc", 'G', theme::WARNING),
];

// S5b2-D (2026-08-21) update: the S5a work-pages canvas's 5-item "任務與
// 目標" membership — 目標/任務/例行工作/共同計畫/檔案 (`goals`/`tasks`/
// `routines`/`plans`/`files`, in that exact order, matching both the task
// brief's own parenthetical list AND the canvas's embedded sidebar). `runs`
// MOVES OUT to `MONITOR_ITEMS` (this wave's "「監控」加 執行紀錄" instruction);
// `files` MOVES IN from the old `KNOWLEDGE_ITEMS`. The canvas's full unified
// sidebar shows a 6th row here, `分支決戰` (`forks`) — explicitly deferred to
// a later wave per the task brief ("分支決戰留波三"), so it is NOT added yet.
const TASKS_ITEMS: &[NavItem] = &[
    item("goals", "nav.goals", "nav.goals.desc", 'G', theme::SUCCESS),
    item("tasks", "nav.tasks", "nav.tasks.desc", 'T', theme::SUCCESS),
    item("routines", "nav.routines", "nav.routines.desc", 'R', theme::SUCCESS),
    item("plans", "nav.plans", "nav.plans.desc", 'P', theme::SUCCESS),
    item("files", "nav.files", "nav.files.desc", 'F', theme::SUCCESS),
];

// S5b2-D (2026-08-21) update: `files` and `skills` MOVE OUT (see
// `TASKS_ITEMS`/`AGENTS_ITEMS`'s own notes above — a shared file/skill is
// now grouped with the work it belongs to rather than staying in a generic
// "knowledge" bucket); `widgets` is new (this wave's "「知識與記憶」加 Widget
// 工坊" instruction). The canvas's full unified sidebar also shows a
// `知識中樞` ("Knowledge Hub"/WikiGraph) row here with no id in this crate
// yet — a later (viz-pages) wave's page, not this one's, so it is not added.
const KNOWLEDGE_ITEMS: &[NavItem] = &[
    item("memory", "nav.memory", "nav.memory.desc", 'M', theme::INFO),
    item("widgets", "nav.widgets", "nav.widgets.desc", 'W', theme::INFO),
];

// S5b2-D (2026-08-21) update: `runs` MOVES IN from `TASKS_ITEMS` (this
// wave's "「監控」加 執行紀錄" instruction) — the canvas's full unified
// "監控" group additionally has usage/foresight/OS/timeline/canvas rows (7
// total, "監控 7 項不拆" per the 2026-08-21 status-map decision log) and
// drops `home` to the "對話" group entirely; both of those are OUTSIDE this
// wave's brief (only "加 執行紀錄" was asked for) and belong to a later
// (viz-pages) wave, so `home`'s position is deliberately left untouched here.
const MONITOR_ITEMS: &[NavItem] = &[
    item("home", "nav.home", "nav.home.desc", 'D', theme::CHART_2),
    item("reports", "nav.reports", "nav.reports.desc", 'A', theme::CHART_2),
    item("runs", "nav.runs", "nav.runs.desc", 'L', theme::CHART_2),
];

// S5b1-A (2026-08-21): the real 6-item "管理" membership — order matches
// the approved S5 canvas's sidebar exactly (see this file's header comment
// on the S5b1-A update). `channels`/`integrations` are placeholder rows
// this wave predefines (their target screens are a sibling package's scope,
// landing in a later commit); until then `shell.rs`'s generic fallback
// branch renders their `label_key`/`desc_key` as an honest "here's what
// this page is, not yet wired" placeholder heading — never a blank page or
// a silent redirect.
const MANAGE_ITEMS: &[NavItem] = &[
    item("channels", "nav.channels", "nav.channels.desc", 'C', theme::MUTED_FOREGROUND),
    item("integrations", "nav.integrations", "nav.integrations.desc", 'I', theme::MUTED_FOREGROUND),
    item("accounts", "nav.accounts", "nav.accounts.desc", 'A', theme::MUTED_FOREGROUND),
    item("systemUpdates", "nav.systemUpdates", "nav.systemUpdates.desc", 'U', theme::MUTED_FOREGROUND),
    item("device", "nav.device", "nav.device.desc", 'D', theme::MUTED_FOREGROUND),
    item("manageAdvanced", "nav.manageAdvanced", "nav.manageAdvanced.desc", 'S', theme::MUTED_FOREGROUND),
];

/// The 6 top-level areas, in the sidebar's rendered order — see this
/// module's header comment for the full id→area mapping and rationale.
pub const AREAS: &[NavArea] = &[
    area("areaChat", "navArea.chat", 'C', theme::BRAND, CHAT_ITEMS),
    area("areaAgents", "navArea.agents", 'A', theme::WARNING, AGENTS_ITEMS),
    area("areaTasks", "navArea.tasks", 'T', theme::SUCCESS, TASKS_ITEMS),
    area("areaKnowledge", "navArea.knowledge", 'K', theme::INFO, KNOWLEDGE_ITEMS),
    area("areaMonitor", "navArea.monitor", 'M', theme::CHART_2, MONITOR_ITEMS),
    // Badge kept as the neutral `MUTED_FOREGROUND` "settings" hue — same
    // "one hue per group" convention every other area follows, and the same
    // tone `about`'s own former solo badge used before S5b1-A gave this
    // area 6 real occupants.
    area("areaManage", "navArea.manage", 'I', theme::MUTED_FOREGROUND, MANAGE_ITEMS),
];

/// Pinned to Column 1's footer, below every area — never inside `AREAS`.
/// Order matches the task brief verbatim: 設定 (`manage`) before 元件庫
/// (`componentLibrary`). `designGallery` (S4a, 2026-08-19) is appended last
/// — the 13-page static prototype gallery (`screens/prototypes/`) is an
/// internal design-review surface, same footer-not-area placement logic as
/// `componentLibrary`'s own dogfood page, so it lives right next to it
/// rather than inventing a 7th top-level area for one internal tool.
pub const FOOTER_ITEMS: &[NavItem] = &[
    item("manage", "nav.manage", "nav.manage.desc", 'M', theme::MUTED_FOREGROUND),
    item(
        "componentLibrary",
        "nav.componentLibrary",
        "nav.componentLibrary.desc",
        'K',
        theme::MUTED_FOREGROUND,
    ),
    item("designGallery", "nav.designGallery", "nav.designGallery.desc", 'G', theme::MUTED_FOREGROUND),
];

/// Look up a nav item (area content or footer) by its stable id — used by
/// the content area to resolve the currently-selected page's label/desc
/// keys, and by `shell.rs` to derive which area row should show persistent
/// selection highlight (Apple HIG, research doc §4: "persistently highlight
/// the current selection in each pane").
pub fn find(id: &str) -> Option<NavItem> {
    AREAS
        .iter()
        .flat_map(|a| a.items.iter())
        .chain(FOOTER_ITEMS.iter())
        .find(|i| i.id == id)
        .copied()
}

/// Which area (if any) owns the page currently showing in Column 3 — the
/// area row this page's selection should bubble up to in Column 1. Returns
/// `None` for a footer item's id (`manage`/`componentLibrary` aren't inside
/// any area, so no Column-1 row lights up for them — their OWN footer row
/// carries the selection highlight instead, handled directly in
/// `shell.rs`).
pub fn area_for_page(id: &str) -> Option<&'static NavArea> {
    AREAS.iter().find(|a| a.items.iter().any(|i| i.id == id))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every id the crate used to reference before the P0-1 restructuring
    /// must still resolve via `find()` — the task brief's "既有頁 id 全部
    /// 保留映射" requirement, enforced rather than just asserted in a
    /// comment. If a future edit drops an id from `AREAS`/`FOOTER_ITEMS`
    /// without updating this list too, this test is the trip-wire.
    ///
    /// `about` is deliberately NOT in this list as of S5b1-A (2026-08-21) —
    /// see this file's header comment on the S5b1-A update for why the
    /// approved S5 canvas drops it from the sidebar's "管理" area (it stays
    /// reachable via the macOS menu bar only). Removing a legacy id from
    /// this list is exactly the kind of change this test exists to catch if
    /// done silently; this is the one documented, approved exception.
    const LEGACY_IDS: &[&str] = &[
        "newChat", "home", "console", "routines", "plans", "goals", "runs", "files", "tasks",
        "reports", "skills", "memory", "agents", "world", "growth", "experts", "manage",
        "componentLibrary",
    ];

    /// S5b2-D (2026-08-21): the three brand-new ids this wave introduces —
    /// not "legacy" (nothing referenced them before), but resolvability is
    /// exactly as load-bearing, so they get their own explicit trip-wire
    /// rather than being folded into `LEGACY_IDS` (whose name and doc
    /// comment are specifically about pre-existing ids).
    const S5B2_D_NEW_IDS: &[&str] = &["presets", "gallery", "widgets"];

    #[test]
    fn every_s5b2_d_new_id_resolves() {
        for id in S5B2_D_NEW_IDS {
            assert!(find(id).is_some(), "new nav id {id:?} does not resolve via find()");
        }
    }

    #[test]
    fn every_legacy_id_still_resolves() {
        for id in LEGACY_IDS {
            assert!(find(id).is_some(), "legacy nav id {id:?} no longer resolves via find()");
        }
    }

    /// S5b1-A (2026-08-21): the "管理" area's 6-item membership, in the
    /// exact order the approved S5 canvas's sidebar renders them — a trip-
    /// wire against a future edit silently reordering or dropping one of
    /// these without updating the canvas-derived comment above.
    #[test]
    fn manage_area_has_the_six_s5_canvas_items_in_order() {
        let manage = AREAS.iter().find(|a| a.id == "areaManage").expect("areaManage must exist");
        let ids: Vec<&str> = manage.items.iter().map(|i| i.id).collect();
        assert_eq!(
            ids,
            vec!["channels", "integrations", "accounts", "systemUpdates", "device", "manageAdvanced"]
        );
    }

    /// S5b2-D (2026-08-21): the "AI 員工" area's 7-item membership — the
    /// canvas's own 5-item list (員工總覽/職務組合/技能庫/AI團隊/靈感畫廊) in
    /// exact order, followed by the pre-existing `world`/`growth` (kept, per
    /// this file's header comment on the S5b2-D update's reasoning).
    #[test]
    fn agents_area_has_the_s5b2_d_membership_in_order() {
        let agents = AREAS.iter().find(|a| a.id == "areaAgents").expect("areaAgents must exist");
        let ids: Vec<&str> = agents.items.iter().map(|i| i.id).collect();
        assert_eq!(ids, vec!["agents", "presets", "skills", "experts", "gallery", "world", "growth"]);
    }

    /// S5b2-D (2026-08-21): the "任務與目標" area's 5-item membership, in the
    /// exact order the task brief and the canvas's embedded sidebar both
    /// give (目標/任務/例行工作/共同計畫/檔案).
    #[test]
    fn tasks_area_has_the_s5b2_d_membership_in_order() {
        let tasks = AREAS.iter().find(|a| a.id == "areaTasks").expect("areaTasks must exist");
        let ids: Vec<&str> = tasks.items.iter().map(|i| i.id).collect();
        assert_eq!(ids, vec!["goals", "tasks", "routines", "plans", "files"]);
    }

    /// S5b2-D (2026-08-21): the "知識與記憶" area shrinks to `memory` +
    /// `widgets` — `files`/`skills` moved out (see `TASKS_ITEMS`/
    /// `AGENTS_ITEMS`'s own membership tests above).
    #[test]
    fn knowledge_area_has_the_s5b2_d_membership_in_order() {
        let knowledge = AREAS.iter().find(|a| a.id == "areaKnowledge").expect("areaKnowledge must exist");
        let ids: Vec<&str> = knowledge.items.iter().map(|i| i.id).collect();
        assert_eq!(ids, vec!["memory", "widgets"]);
    }

    /// S5b2-D (2026-08-21): `runs` joins `home`/`reports` in "監控" — this
    /// wave's only instructed change to this area ("home"'s position is
    /// deliberately left untouched, see `MONITOR_ITEMS`'s own comment).
    #[test]
    fn monitor_area_has_the_s5b2_d_membership_in_order() {
        let monitor = AREAS.iter().find(|a| a.id == "areaMonitor").expect("areaMonitor must exist");
        let ids: Vec<&str> = monitor.items.iter().map(|i| i.id).collect();
        assert_eq!(ids, vec!["home", "reports", "runs"]);
    }

    /// `about` is intentionally absent from every area and from the footer
    /// as of S5b1-A — it is reachable only via the macOS menu bar now (see
    /// `LEGACY_IDS`'s doc comment). This positive assertion makes that
    /// absence a deliberate, tested fact rather than something a future
    /// reader has to infer from a comment alone.
    #[test]
    fn about_is_no_longer_a_sidebar_nav_item() {
        assert!(find("about").is_none());
    }

    #[test]
    fn no_duplicate_ids_across_areas_and_footer() {
        let mut seen = std::collections::HashSet::new();
        for i in AREAS.iter().flat_map(|a| a.items.iter()).chain(FOOTER_ITEMS.iter()) {
            assert!(seen.insert(i.id), "duplicate nav item id: {}", i.id);
        }
    }

    #[test]
    fn no_duplicate_area_ids() {
        let mut seen = std::collections::HashSet::new();
        for a in AREAS {
            assert!(seen.insert(a.id), "duplicate nav area id: {}", a.id);
        }
    }

    #[test]
    fn every_area_has_at_least_one_item() {
        for a in AREAS {
            assert!(!a.items.is_empty(), "area {} has no items", a.id);
        }
    }

    #[test]
    fn area_for_page_matches_area_membership() {
        for a in AREAS {
            for i in a.items {
                assert_eq!(area_for_page(i.id).map(|found| found.id), Some(a.id));
            }
        }
        // Footer items belong to no area.
        for i in FOOTER_ITEMS {
            assert!(area_for_page(i.id).is_none());
        }
    }
}
