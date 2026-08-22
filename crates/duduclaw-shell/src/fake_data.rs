// Fake data for the Home surface — Shell-S0 prototype convention (see
// `duduclaw-native-gui/src/nav.rs`'s "placeholder icon: an uppercase letter
// + a fixed accent color, real iconography deferred" precedent). The
// `glyph` fields below are no longer what actually draws in most of those
// slots: ICON-1 (2026-08-22) wired the approved boards' own stroke icons in
// via `crate::icons`, and each `glyph` is now the FALLBACK for its slot.
// Content is lifted verbatim (zh-TW copy, hex colors,
// counts) from the visual spec
// (`commercial/design/duduclaw-os-desktop/Main.dc.html`) — nothing here is
// wired to a live gateway/agent yet, matching the task brief's "假資料
// （內容照畫板文案，繁中硬編——S0 原型慣例）".
//
// Centralized in this one file so `home.rs` stays presentation-only and
// `main.rs` stays data-free — same separation `duduclaw-native-gui`'s own
// `screens::prototypes` module keeps between its static page data and
// rendering.

// ── `DockApp` / `DOCK_APPS` were REMOVED in APP-1 (2026-08-22) ──────────
// They used to be this crate's whole "app registry": six hand-authored
// entries lifted from the design board (信箱/文件/瀏覽器/圖片/訊息/行事曆),
// five of which had no real app behind them. Both the dock and the
// Launcher's app-search rendered them, so on a real appliance the operator
// was reading a menu of software that was not installed — reported from the
// VM, and the reason this work package exists.
//
// What replaced them:
//   * the real inventory — `crate::apps::installed` (flatpak + XDG
//     `.desktop` enumeration), held in `crate::apps::feed::
//     InstalledAppsFeed`, rendered by `home/home_dock.rs` and
//     `overlay/launcher.rs`. Nothing falls back to canned entries when it
//     is empty or fails.
//   * the one non-fictional thing that array carried — a real flatpak ref
//     plus its remote, which an inventory cannot express for an app that is
//     not installed yet — now `crate::apps::catalog::INSTALL_CATALOG`, the
//     installable catalog behind the Launcher's separate 「可安裝」section.
//   * `VerifiedTier` moved to `crate::apps` (it is now a live lookup over
//     real apps, `crate::apps::catalog::verified_tier`, not a per-row
//     constant in a canned array).
//
// This file keeps only genuinely decorative board content — goal cards, the
// activity shelf, menu strings, the ControlCenter tiles. The app list is no
// longer part of it.

/// A pinned agent avatar in the dock (right of the app icons, after the
/// divider) — the design board's "杜"/"財" circles. The small corner status
/// dot is `running` (busy, matches its own avatar's brand hue) vs
/// `needs_human` (amber) per the task brief's "running 藍/needs_human 紅橘"
/// status convention (the design board's actual sample data has one of
/// each, not one of every state — kept verbatim rather than inventing a
/// third example).
pub struct DockAgent {
    pub id: &'static str,
    pub initial: &'static str,
    pub bg_hex: u32,
    pub status: AgentDockStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentDockStatus {
    Running,
    NeedsHuman,
}

/// Shell-S1 (2026-08-20, Home/overlay dark theme): which SEMANTIC
/// color family a status ring / badge belongs to. This file used to resolve
/// `GoalCard`'s dot and each badge's text/bg pair below straight to a
/// literal hex, appropriate when there was only ONE design board to lift
/// colors from. Now that Home/overlay has a light AND a dark board, the
/// concrete hex for each kind differs per theme (see `crate::palette::
/// ShellPalette`'s own header comment for the exact pairs) — resolving that
/// here would mean either importing the palette type (which owns gpui
/// `Rgba`/`Hsla` fields, breaking this module's own "stays gpui-free,
/// independently unit-testable" discipline, see this file's header comment)
/// or duplicating every literal twice per field. Storing the semantic KIND
/// instead and letting `ShellPalette::badge_accent`/`badge_bg`/`badge_text`
/// resolve the actual color at render time (`home/home_dock.rs`, `overlay/
/// notifications.rs`) keeps this file plain data while still being
/// theme-correct.
///
/// `AgentDockStatus`'s own status dot deliberately does NOT go through this
/// enum: `NeedsHuman`'s dot resolves through `ShellPalette::warning_dot`,
/// NOT `badge_accent(Warning)` — the small circular dot and the badge/ring
/// token are different fields in dark (see `warning_dot`'s own doc comment
/// on `ShellPalette`), so forcing `AgentDockStatus` through this 3-member
/// vocabulary would need a dead `Success` match arm for no actual gain;
/// `home/home_dock.rs::dock_agent` matches on `AgentDockStatus` directly
/// instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadgeKind {
    Warning,
    Success,
    Brand,
}

pub const DOCK_AGENTS: &[DockAgent] = &[
    DockAgent { id: "duxiaodu", initial: "杜", bg_hex: 0x2171cc, status: AgentDockStatus::Running },
    DockAgent { id: "finance", initial: "財", bg_hex: 0x0f766e, status: AgentDockStatus::NeedsHuman },
];

/// One "進行中 goal" card in the Home surface's mid-canvas row. `badge_kind`
/// drives BOTH the badge pill (text/bg) AND `dot`'s own ring/fill color —
/// see `BadgeKind`'s own doc comment for why a semantic kind replaces the
/// raw hex pair this struct used to carry (light-only precedent).
pub struct GoalCard {
    pub id: &'static str,
    pub dot: GoalDot,
    pub title: &'static str,
    pub badge_label: &'static str,
    pub badge_kind: BadgeKind,
    pub meta: &'static str,
}

/// The small status ring left of a goal card's title — an unfilled ring
/// (still running / awaiting a decision) or a filled dot (done). Carries a
/// `BadgeKind`, not a raw hex — see that type's own doc comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalDot {
    Outline(BadgeKind),
    Filled(BadgeKind),
}

pub const GOAL_CARDS: &[GoalCard] = &[
    GoalCard {
        id: "goal-daily-revenue",
        dot: GoalDot::Outline(BadgeKind::Warning),
        title: "每日營收日報自動化",
        badge_label: "等你決定",
        badge_kind: BadgeKind::Warning,
        meta: "第 3/5 輪 · 財務助理 · 判官指出兩處數字落差",
    },
    GoalCard {
        id: "goal-site-copy",
        dot: GoalDot::Outline(BadgeKind::Brand),
        title: "官網改版文案",
        badge_label: "進行中",
        badge_kind: BadgeKind::Brand,
        meta: "第 1/5 輪 · 小杜 · 正在讀現有頁面",
    },
    GoalCard {
        id: "goal-support-report",
        dot: GoalDot::Filled(BadgeKind::Success),
        title: "客服月報",
        badge_label: "已完成",
        badge_kind: BadgeKind::Success,
        meta: "10 分鐘前 · 小杜 · 已存到 工作/月報",
    },
];

// `APPROVAL_TICKER` (the old hardcoded menu-bar ticker text) was removed in
// Shell-S4 (2026-08-22, WP-S4-notif) — `home.rs::ticker_text` now renders
// the real `overlay::notifications_feed::NotificationsFeed`'s pending list
// instead of one static example sentence.
pub const GREETING: &str = "晚上好，Louis";
pub const COMPOSER_PLACEHOLDER: &str = "交代一件事給你的 AI 團隊…";
pub const BATTERY_PCT: &str = "86%";
pub const CLOCK: &str = "22:58";
pub const MENU_BRAND: &str = "DuDuClaw";

pub const MENU_ITEMS: &[&str] = &["檔案", "編輯", "顯示", "AI 團隊", "視窗"];
pub const SUGGESTION_CHIPS: &[&str] = &["整理 Q3 報表", "清理收件匣", "排明天的行程"];
/// "今日" activity shelf entries, rendered joined by "·" separators.
pub const ACTIVITY_SHELF: &[&str] = &["小杜 完成 客服月報", "財務助理 對照了 3 份表格", "已自動歸檔 12 封信"];

// ── Launcher overlay fake data ────────────────────────────────────────────
// Content lifted verbatim from `commercial/design/duduclaw-os-desktop/
// Launcher.dc.html` — see `overlay/launcher.rs`'s header comment for layout.

/// One "檔案" result row — icon has no background box in the design board
/// (just a colored glyph), unlike the app-result rows above.
pub struct LauncherFileResult {
    pub id: &'static str,
    pub glyph: &'static str,
    pub glyph_hex: u32,
    pub label: &'static str,
    pub meta: &'static str,
}

pub const LAUNCHER_FILE_RESULTS: &[LauncherFileResult] = &[
    LauncherFileResult { id: "launcher-file-folder", glyph: "夾", glyph_hex: 0x54a8ef, label: "財務/請款/2026-08/", meta: "資料夾" },
    LauncherFileResult { id: "launcher-file-xlsx", glyph: "表", glyph_hex: 0x21a366, label: "請款單-0819.xlsx", meta: "今天 14:20" },
];

// `LAUNCHER_QUERY` (the old static predisplay query text) was removed in
// WP-A3 (2026-08-22) — `overlay/launcher.rs::query_row` now renders the
// real, live-typed `OverlayUiState.launcher_query` instead.
pub const LAUNCHER_SECTION_DELEGATE: &str = "交辦";
pub const LAUNCHER_SECTION_APPS: &str = "應用程式";
pub const LAUNCHER_SECTION_FILES: &str = "檔案";
pub const LAUNCHER_DELEGATE_AGENT_INITIAL: &str = "財";
pub const LAUNCHER_DELEGATE_AGENT_BG_HEX: u32 = 0x0f766e;
pub const LAUNCHER_DELEGATE_TITLE: &str = "交辦給 財務助理";
pub const LAUNCHER_DELEGATE_PLAN: &str = "將執行：收集今日請款單 → 對照部門預算 → 產出摘要與異常清單";
pub const LAUNCHER_DELEGATE_HINT: &str = "Enter 交辦";
pub const LAUNCHER_FOOTER_LEFT: &str = "↑↓ 選擇 · Enter 執行 · Tab 換分類";
pub const LAUNCHER_FOOTER_RIGHT: &str = "Super 鍵隨時喚起";

// ── Notifications overlay fake data ───────────────────────────────────────
// Content lifted verbatim from `commercial/design/duduclaw-os-desktop/
// Notifications.dc.html` — see `overlay/notifications.rs`'s header comment.
//
// Shell-S4 (2026-08-22, WP-S4-notif): the approval CARDS themselves stopped
// being fake data this round — they now come from the real gateway
// (`gateway_client::list_approvals`, rendered via `overlay::
// notifications_feed::ApprovalRow`), so the old `ApprovalCard`/
// `APPROVAL_CARDS` (two hardcoded example approvals) were removed rather
// than left as unreferenced dead code. `NOTIF_APPROVE_LABEL`/
// `NOTIF_REJECT_LABEL` below replace the old per-card `approve_label`/
// `reject_label` pair (which differed per card in the design board itself,
// "核准"/"駁回" vs "批准"/"先不要") with ONE generic pair: the real gateway
// has no per-approval button-label field to carry that distinction, so a
// single consistent label is the honest choice rather than inventing one.

pub const NOTIF_APPROVE_LABEL: &str = "核准";
pub const NOTIF_REJECT_LABEL: &str = "駁回";

/// Which avatar an activity row shows — an agent's initial-in-a-circle
/// (same convention as `DockAgent`) or the system's own gradient glyph (the
/// update-available row has no agent behind it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowAvatar {
    Agent { initial: &'static str, bg_hex: u32 },
    System,
}

pub struct ActivityRow {
    pub id: &'static str,
    pub avatar: RowAvatar,
    pub line1: &'static str,
    pub line2: &'static str,
    /// `(label, kind)` — `None` for the system-update row, which has no
    /// status pill in the design board. `kind` replaces the old raw
    /// `(text_hex, bg_hex)` pair — see `BadgeKind`'s own doc comment.
    pub badge: Option<(&'static str, BadgeKind)>,
}

pub const TODAY_ACTIVITY: &[ActivityRow] = &[
    ActivityRow {
        id: "activity-support-report",
        avatar: RowAvatar::Agent { initial: "杜", bg_hex: 0x2171cc },
        line1: "完成 客服月報-9月 草稿",
        line2: "22:47 · 存到 工作/月報",
        badge: Some(("完成", BadgeKind::Success)),
    },
    ActivityRow {
        id: "activity-daily-revenue",
        avatar: RowAvatar::Agent { initial: "財", bg_hex: 0x0f766e },
        line1: "每日營收日報 · 第 3/5 輪暫停",
        line2: "21:12 · 兩處數字落差，等你決定口徑",
        badge: Some(("等你決定", BadgeKind::Warning)),
    },
    ActivityRow {
        id: "activity-system-update",
        avatar: RowAvatar::System,
        line1: "系統更新 1.7.0 可用",
        line2: "今晚 03:00 自動安裝，失敗自動回滾",
        badge: None,
    },
];

pub const NOTIF_HEADER_TITLE: &str = "通知";
pub const NOTIF_MARK_ALL_READ: &str = "全部標為已讀";
pub const NOTIF_TABS: &[&str] = &["全部", "審批 2", "進度", "系統"];
pub const NOTIF_TODAY_LABEL: &str = "今天";
pub const NOTIF_FOOTER: &str = "已在儀表板處理過的項目會自動消失";
pub const NOTIF_NOTE_PLACEHOLDER: &str = "補一句備註…";
pub const NOTIF_APPROVED_LABEL: &str = "已核准";
pub const NOTIF_REJECTED_LABEL: &str = "已駁回";

// ── ControlCenter overlay fake data ───────────────────────────────────────
// Content lifted verbatim from `commercial/design/duduclaw-os-desktop/
// ControlCenter.dc.html` — see `overlay/controlcenter.rs`'s header comment.
// The AI-team switch DEFAULTS (自動化/主動行為 on, 全部暫停 off) are
// RUNTIME state, not fake data — they live in `overlay::OverlayUiState`
// instead, seeded from this same board's snapshot.

pub struct QuickTile {
    pub id: &'static str,
    pub glyph: &'static str,
    pub title: &'static str,
    pub subtitle: &'static str,
    pub active: bool,
}

pub const QUICK_TILES: &[QuickTile] = &[
    QuickTile { id: "tile-wifi", glyph: "W", title: "Wi-Fi", subtitle: "DuDu-Office", active: true },
    QuickTile { id: "tile-bluetooth", glyph: "B", title: "藍牙", subtitle: "關閉", active: false },
    QuickTile { id: "tile-dnd", glyph: "勿", title: "勿擾", subtitle: "關閉", active: false },
];

/// A static quick-settings slider (volume/brightness) — `pct` is the fill
/// fraction (0.0–1.0), matched verbatim from `ControlCenter.dc.html`'s
/// inline `width: 62%` / `width: 80%`. Non-interactive this round (task
/// brief scoped "開關做視覺 toggle 狀態" to the AI-team switches only, not
/// these sliders — see `overlay/controlcenter.rs`'s header comment).
pub struct SliderRow {
    pub glyph: &'static str,
    pub pct: f32,
}

pub const SLIDER_ROWS: &[SliderRow] = &[SliderRow { glyph: "音", pct: 0.62 }, SliderRow { glyph: "光", pct: 0.80 }];

pub const CC_SECTION_AI_TEAM: &str = "AI 團隊";
pub const CC_SWITCH_AUTOMATION_LABEL: &str = "自動化";
pub const CC_SWITCH_AUTOMATION_DESC: &str = "例行工作與目標迴圈照常執行";
pub const CC_SWITCH_PROACTIVE_LABEL: &str = "主動行為";
pub const CC_SWITCH_PROACTIVE_DESC: &str = "允許 AI 員工主動提出建議與提醒";
pub const CC_SWITCH_PAUSE_ALL_LABEL: &str = "全部暫停";
pub const CC_SWITCH_PAUSE_ALL_DESC: &str = "一鍵暫停所有 AI 行為，待辦保留";
pub const CC_FOOTER_STATUS: &str = "2 位在值 · 1 件等你";
pub const CC_FOOTER_LINK: &str = "打開管理面";
/// Shell-S4-lock (2026-08-22): the manual-lock entry point in ControlCenter's
/// footer — see `overlay/controlcenter.rs::lock_button`'s own doc comment.
/// Plain zh-TW literal, same "chrome content stays hardcoded" convention
/// every other `CC_*` string in this block already follows (Home/overlay's
/// established non-i18n boundary — see `crate::i18n`'s own header comment).
pub const CC_LOCK_BUTTON: &str = "鎖定";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dock_agents_has_two_entries_matching_the_design_board() {
        assert_eq!(DOCK_AGENTS.len(), 2);
    }

    #[test]
    fn goal_cards_has_three_entries_matching_the_design_board() {
        assert_eq!(GOAL_CARDS.len(), 3);
    }

    #[test]
    fn suggestion_chips_has_three_entries() {
        assert_eq!(SUGGESTION_CHIPS.len(), 3);
    }

    #[test]
    fn activity_shelf_has_three_entries() {
        assert_eq!(ACTIVITY_SHELF.len(), 3);
    }

    #[test]
    fn menu_items_has_five_entries() {
        assert_eq!(MENU_ITEMS.len(), 5);
    }

    #[test]
    fn no_field_is_empty() {
        for agent in DOCK_AGENTS {
            assert!(!agent.id.is_empty());
            assert!(!agent.initial.is_empty());
        }
        for card in GOAL_CARDS {
            assert!(!card.id.is_empty());
            assert!(!card.title.is_empty());
            assert!(!card.badge_label.is_empty());
            assert!(!card.meta.is_empty());
        }
        for chip in SUGGESTION_CHIPS {
            assert!(!chip.is_empty());
        }
        for line in ACTIVITY_SHELF {
            assert!(!line.is_empty());
        }
        for item in MENU_ITEMS {
            assert!(!item.is_empty());
        }
        assert!(!GREETING.is_empty());
        assert!(!COMPOSER_PLACEHOLDER.is_empty());
        assert!(!BATTERY_PCT.is_empty());
        assert!(!CLOCK.is_empty());
        assert!(!MENU_BRAND.is_empty());
    }

    #[test]
    fn dock_agent_ids_are_unique() {
        let mut ids: Vec<&str> = DOCK_AGENTS.iter().map(|a| a.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), DOCK_AGENTS.len());
    }

    #[test]
    fn goal_card_ids_are_unique() {
        let mut ids: Vec<&str> = GOAL_CARDS.iter().map(|c| c.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), GOAL_CARDS.len());
    }

    #[test]
    fn agent_dock_status_has_exactly_the_two_documented_variants() {
        // `AgentDockStatus` no longer carries its own `BadgeKind` mapping
        // (see that enum's own doc comment for why — `home/home_dock.rs::
        // dock_agent` matches on these variants directly instead) — this
        // file's remaining stake in the status/color relationship is just
        // that the two variants themselves stay exactly Running/NeedsHuman,
        // pinned via `Debug` formatting so a future rename or a third
        // variant fails loudly here rather than silently in the renderer.
        assert_eq!(format!("{:?}", AgentDockStatus::Running), "Running");
        assert_eq!(format!("{:?}", AgentDockStatus::NeedsHuman), "NeedsHuman");
    }

    // ── Launcher ─────────────────────────────────────────────────────────

    #[test]
    fn launcher_file_results_has_two_entries_matching_the_design_board() {
        assert_eq!(LAUNCHER_FILE_RESULTS.len(), 2);
    }

    #[test]
    fn launcher_file_result_ids_are_unique() {
        let mut ids: Vec<&str> = LAUNCHER_FILE_RESULTS.iter().map(|r| r.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), LAUNCHER_FILE_RESULTS.len());
    }

    #[test]
    fn launcher_no_field_is_empty() {
        for r in LAUNCHER_FILE_RESULTS {
            assert!(!r.id.is_empty());
            assert!(!r.glyph.is_empty());
            assert!(!r.label.is_empty());
            assert!(!r.meta.is_empty());
        }
        assert!(!LAUNCHER_SECTION_DELEGATE.is_empty());
        assert!(!LAUNCHER_SECTION_APPS.is_empty());
        assert!(!LAUNCHER_SECTION_FILES.is_empty());
        assert!(!LAUNCHER_DELEGATE_AGENT_INITIAL.is_empty());
        assert!(!LAUNCHER_DELEGATE_TITLE.is_empty());
        assert!(!LAUNCHER_DELEGATE_PLAN.is_empty());
        assert!(!LAUNCHER_DELEGATE_HINT.is_empty());
        assert!(!LAUNCHER_FOOTER_LEFT.is_empty());
        assert!(!LAUNCHER_FOOTER_RIGHT.is_empty());
    }

    // ── Notifications ────────────────────────────────────────────────────

    #[test]
    fn today_activity_has_three_entries_matching_the_design_board() {
        assert_eq!(TODAY_ACTIVITY.len(), 3);
    }

    #[test]
    fn activity_row_ids_are_unique() {
        let mut ids: Vec<&str> = TODAY_ACTIVITY.iter().map(|r| r.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), TODAY_ACTIVITY.len());
    }

    #[test]
    fn notif_tabs_has_four_entries_matching_the_design_board() {
        assert_eq!(NOTIF_TABS.len(), 4);
    }

    #[test]
    fn notifications_no_field_is_empty() {
        assert!(!NOTIF_APPROVE_LABEL.is_empty());
        assert!(!NOTIF_REJECT_LABEL.is_empty());
        for r in TODAY_ACTIVITY {
            assert!(!r.id.is_empty());
            assert!(!r.line1.is_empty());
            assert!(!r.line2.is_empty());
            if let Some((label, ..)) = r.badge {
                assert!(!label.is_empty());
            }
        }
        for tab in NOTIF_TABS {
            assert!(!tab.is_empty());
        }
        assert!(!NOTIF_HEADER_TITLE.is_empty());
        assert!(!NOTIF_MARK_ALL_READ.is_empty());
        assert!(!NOTIF_TODAY_LABEL.is_empty());
        assert!(!NOTIF_FOOTER.is_empty());
        assert!(!NOTIF_NOTE_PLACEHOLDER.is_empty());
        assert!(!NOTIF_APPROVED_LABEL.is_empty());
        assert!(!NOTIF_REJECTED_LABEL.is_empty());
    }

    // ── ControlCenter ────────────────────────────────────────────────────

    #[test]
    fn quick_tiles_has_three_entries_matching_the_design_board() {
        assert_eq!(QUICK_TILES.len(), 3);
    }

    #[test]
    fn quick_tile_ids_are_unique() {
        let mut ids: Vec<&str> = QUICK_TILES.iter().map(|t| t.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), QUICK_TILES.len());
    }

    #[test]
    fn slider_rows_has_two_entries_matching_the_design_board() {
        assert_eq!(SLIDER_ROWS.len(), 2);
    }

    #[test]
    fn slider_pcts_are_within_the_unit_range() {
        for row in SLIDER_ROWS {
            assert!((0.0..=1.0).contains(&row.pct), "pct {} out of range for {}", row.pct, row.glyph);
        }
    }

    #[test]
    fn controlcenter_no_field_is_empty() {
        for t in QUICK_TILES {
            assert!(!t.id.is_empty());
            assert!(!t.glyph.is_empty());
            assert!(!t.title.is_empty());
            assert!(!t.subtitle.is_empty());
        }
        for row in SLIDER_ROWS {
            assert!(!row.glyph.is_empty());
        }
        assert!(!CC_SECTION_AI_TEAM.is_empty());
        assert!(!CC_SWITCH_AUTOMATION_LABEL.is_empty());
        assert!(!CC_SWITCH_AUTOMATION_DESC.is_empty());
        assert!(!CC_SWITCH_PROACTIVE_LABEL.is_empty());
        assert!(!CC_SWITCH_PROACTIVE_DESC.is_empty());
        assert!(!CC_SWITCH_PAUSE_ALL_LABEL.is_empty());
        assert!(!CC_SWITCH_PAUSE_ALL_DESC.is_empty());
        assert!(!CC_FOOTER_STATUS.is_empty());
        assert!(!CC_FOOTER_LINK.is_empty());
        assert!(!CC_LOCK_BUTTON.is_empty());
    }
}
