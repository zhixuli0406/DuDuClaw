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
//   管理 (manage)      — about only. See "Two settle-related decisions"
//                        below for why `manage`/`componentLibrary` are NOT
//                        here.
//
// ── Two settle-related decisions worth stating explicitly ───────────────
// 1. `manage` (nav.manage, "管理" — its own i18n desc is literally "整合、
//    帳務、安全、設定") is pinned to `FOOTER_ITEMS`, not put inside the new
//    "管理" AREA. Its description already IS the HIG "Settings" concept
//    (macOS Cmd-,/Windows `IsSettingsVisible`) — this crate has no separate
//    "Preferences" id to invent, and `manage` already covers exactly that
//    ground. Reusing it avoids adding a synthetic id nothing else points at.
// 2. That leaves the "管理" AREA (a distinct 6th top-level area, per the
//    task brief's "channels/integrations/accounts/updates/advanced"
//    grouping) with nothing but `about` to hold — this Phase-1a subset has
//    no separate ids for those sub-pages yet (the full `web` app's
//    `manageNav`/`manageAdvancedNav` — 15+ rows — isn't ported here; see the
//    old file's header comment on "representative SUBSET" scope). `about`
//    is the one id that doesn't fit any of the other 5 areas and isn't
//    itself a settings-hub id, so it becomes this area's sole occupant.
//    `about` is ALSO reachable from the macOS menu bar (App ▸ About
//    DuDuClaw, see `main.rs`'s `ShowAbout` action) — belt-and-suspenders,
//    not exclusive, so non-macOS builds (no menu bar this pass) don't lose
//    the only way to reach it.
//
// `componentLibrary` is pinned to `FOOTER_ITEMS` per the task brief
// verbatim ("「設定」與「元件庫」釘在側欄 footer…不進分組").

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

const AGENTS_ITEMS: &[NavItem] = &[
    item("agents", "nav.agents", "nav.agents.desc", 'E', theme::WARNING),
    item("experts", "nav.experts", "nav.experts.desc", 'X', theme::WARNING),
    item("world", "nav.world", "nav.world.desc", 'W', theme::WARNING),
    item("growth", "nav.growth", "nav.growth.desc", 'G', theme::WARNING),
];

const TASKS_ITEMS: &[NavItem] = &[
    item("routines", "nav.routines", "nav.routines.desc", 'R', theme::SUCCESS),
    item("plans", "nav.plans", "nav.plans.desc", 'P', theme::SUCCESS),
    item("goals", "nav.goals", "nav.goals.desc", 'G', theme::SUCCESS),
    item("runs", "nav.runs", "nav.runs.desc", 'L', theme::SUCCESS),
    item("tasks", "nav.tasks", "nav.tasks.desc", 'T', theme::SUCCESS),
];

const KNOWLEDGE_ITEMS: &[NavItem] = &[
    item("files", "nav.files", "nav.files.desc", 'F', theme::INFO),
    item("skills", "nav.skills", "nav.skills.desc", 'S', theme::INFO),
    item("memory", "nav.memory", "nav.memory.desc", 'M', theme::INFO),
];

const MONITOR_ITEMS: &[NavItem] = &[
    item("home", "nav.home", "nav.home.desc", 'D', theme::CHART_2),
    item("reports", "nav.reports", "nav.reports.desc", 'A', theme::CHART_2),
];

const MANAGE_ITEMS: &[NavItem] =
    &[item("about", "nav.about", "nav.about.desc", 'I', theme::MUTED_FOREGROUND)];

/// The 6 top-level areas, in the sidebar's rendered order — see this
/// module's header comment for the full id→area mapping and rationale.
pub const AREAS: &[NavArea] = &[
    area("areaChat", "navArea.chat", 'C', theme::BRAND, CHAT_ITEMS),
    area("areaAgents", "navArea.agents", 'A', theme::WARNING, AGENTS_ITEMS),
    area("areaTasks", "navArea.tasks", 'T', theme::SUCCESS, TASKS_ITEMS),
    area("areaKnowledge", "navArea.knowledge", 'K', theme::INFO, KNOWLEDGE_ITEMS),
    area("areaMonitor", "navArea.monitor", 'M', theme::CHART_2, MONITOR_ITEMS),
    // Badge deliberately mirrors `about`'s own (see header comment §2): this
    // area is a single-item pass-through, not a real second-level grouping.
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
    const LEGACY_IDS: &[&str] = &[
        "newChat", "home", "console", "routines", "plans", "goals", "runs", "files", "tasks",
        "reports", "skills", "memory", "agents", "world", "growth", "experts", "manage", "about",
        "componentLibrary",
    ];

    #[test]
    fn every_legacy_id_still_resolves() {
        for id in LEGACY_IDS {
            assert!(find(id).is_some(), "legacy nav id {id:?} no longer resolves via find()");
        }
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
